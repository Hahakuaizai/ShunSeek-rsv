//! 搜索引擎：对紧凑索引做并行子串扫描（对应 C# 版 SearchEngine，打分规则逐项一致）。
//!
//! 与 C# 版的差别在扫描方式（更快、更省）：
//!  - C# 版逐条目在 char 池上做 Span.IndexOf，并单独存了一份小写名字池。
//!  - Rust 版只存原始 UTF-8 名字池，用 memchr（SIMD）在**整个池**上找查询首字节的
//!    候选位置（ASCII 字母同时找大小写两种形态），再按偏移数组定位候选属于哪条目、
//!    做大小写折叠的子串校验。池是连续内存，memchr 的吞吐远高于逐条目调用。
//!  - 拼音首字母通道同理，在首字母池上做精确子串扫描（池是纯小写 ASCII）。
//!  - 条目区间按线程数分片并行（std::thread::scope，无额外运行时），每片独立收集，
//!    最后合并排序。单片命中过多时按 C# 版同样的 16384 → 4096 截断防爆。
//!  - 取消：调用方置位 AtomicBool，新输入到来时旧查询尽快退出并返回空。
//!
//! 打分（与 C# 版一致）：前缀 100 / 词首 70 / 子串 40；拼音通道 80/50；
//! 减去命中位置 ×0.1 与名字长度 ×0.01；App/Shortcut +15，InstallDir/Folder +5；
//! 再叠加 UsageStore 频次加分。

use std::sync::atomic::{AtomicBool, Ordering};

use crate::category::Category;
use crate::store::IndexStore;
use crate::usage::UsageStore;

/// 一条搜索命中。match_start 是命中点在名字中的**字节**偏移（供 UI 高亮切分用；
/// C# 版是 char 偏移，语义相同、单位不同，README 有注）。
/// 语义钉死（v0.1.6）：match_start 相对**原始 name 字符串**（store 里存的原名，
/// 未小写化、未拼首字母串）的字节偏移。UI 切片前必须先把偏移收敛到字符边界
/// （is_char_boundary 回退）再切——偏移理论上总落在字符边界上，但 UI 不得
/// 依赖这一点直接切片（v0.1.5 真机曾因此 panic 并毒化互斥锁）。
#[derive(Clone, Copy, Debug)]
pub struct SearchHit {
    pub index: u32,
    pub score: f32,
    pub match_start: u32,
    /// 高亮长度（字符数）：路径模式下 query 含路径片段，高亮只能按
    /// 名字段算——UI 用 match_start + match_len 切名字，别拿原始
    /// query 长度。通配 / 目录本身命中时为 0（不高亮）。
    pub match_len: u32,
    pub via_initials: bool,
    /// 是否被垃圾降权（哈希名 / 垃圾路径段任一命中）。UI 据此把结果
    /// 归类到尾部折叠区，重要结果优先展示。
    pub demoted: bool,
    /// 是否子序列模糊命中（非连续）：UI 不做连续段高亮。
    pub fuzzy: bool,
}

/// 搜索范围过滤（v0.1.5）：一组目录根，条目完整路径与任一根做
/// 不区分大小写的前缀匹配，且必须落在目录边界上——根
/// `C:\Users\me\Desktop` 能匹配 `...\Desktop\a.txt`，不能匹配
/// `...\Desktop2\a.txt`（前缀后须是 '\\' 或字符串恰好结束）。
/// 根在构造时归一化（小写、'/'→'\\'、去尾部 '\\'；盘根 "C:\"
/// 归一为 "c:" 后靠边界规则同样成立）。
#[derive(Clone, Debug)]
pub struct ScopeFilter {
    roots: Vec<String>,
}

impl ScopeFilter {
    pub fn new(roots: &[String]) -> ScopeFilter {
        let roots = roots
            .iter()
            .map(|r| {
                r.to_lowercase()
                    .replace('/', "\\")
                    .trim_end_matches('\\')
                    .to_string()
            })
            .filter(|r| !r.is_empty())
            .collect();
        ScopeFilter { roots }
    }

    pub fn matches(&self, full_path: &str) -> bool {
        if self.roots.is_empty() {
            return true;
        }
        let p = full_path.to_lowercase().replace('/', "\\");
        self.roots.iter().any(|r| {
            p == *r || (p.starts_with(r.as_str()) && p.as_bytes().get(r.len()) == Some(&b'\\'))
        })
    }

    /// 与 matches(dir + '\\' + name) 等价，但不拼字符串：把
    /// dir + 分隔符 + name 当作一条虚拟字符流逐字与根比（大小写
    /// 折叠、'/' 归一为 '\\'）。扫描热路径（子序列通道逐条目过滤）
    /// 用它免掉每条目一次字符串分配。
    pub fn matches_parts(&self, dir: &str, name: &str) -> bool {
        if self.roots.is_empty() {
            return true;
        }
        let sep_needed =
            !dir.is_empty() && !dir.ends_with('\\') && !dir.ends_with('/');
        self.roots.iter().any(|r| {
            let mut virt = dir
                .chars()
                .chain(if sep_needed { Some('\\') } else { None })
                .chain(name.chars());
            for rc in r.chars() {
                match virt.next() {
                    Some(c) if fold_scope_char(c) == rc => {}
                    _ => return false,
                }
            }
            // 边界：虚拟串恰好结束，或下一个字符是分隔符
            match virt.next() {
                None => true,
                Some(c) => fold_scope_char(c) == '\\',
            }
        })
    }
}

/// 范围匹配的单字折叠（与 ScopeFilter 构造时的整串 to_lowercase 对齐
/// 到单字粒度；'/' 先归一为 '\\'）。
fn fold_scope_char(c: char) -> char {
    let c = if c == '/' { '\\' } else { c };
    c.to_lowercase().next().unwrap_or(c)
}

// ---------------------------------------------------------------- 查询模式
//
// Windows 文件名不允许出现 \ / : * ? " < > |，所以 query 只要含这类
// 字符就必然不是纯文件名查询，不存在歧义——不做开关、不做转义：
//   含 \ / :（盘符段）→ 路径模式；名字段含 * ? → 通配符模式（可与
//   路径模式叠加，如 `3089\*.txt`）；" < > | 直接当噪声去掉。

/// 解析后的查询：名字段走既有通道（或通配），路径片段做目录对齐。
struct ParsedQuery {
    /// 名字段（已小写）；路径模式下是最后一段，目录模式下为空串。
    name_q: String,
    /// 路径片段段列（已小写、'/' 已统一为 '\\'）；空 = 非路径模式。
    path_segs: Vec<String>,
    /// 片段以盘符段（"c:"）开头 → 必须从目录路径开头对齐。
    anchored: bool,
    /// 名字段含 * 或 ? → 通配匹配（代替既有名字通道）。
    wildcard: bool,
    /// 名字段为空（query 以斜杠结尾 / 只有盘符）→ 结果是目录本身。
    dir_only: bool,
    path_mode: bool,
}

fn parse_query(raw: &str) -> ParsedQuery {
    // " < > | 是 Windows 保留字符、文件名里不可能出现：当噪声去掉，
    // 别让它们把搜索打空。
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| !"\"<>|".contains(*c))
        .collect();
    let lower = cleaned.replace('/', "\\").to_lowercase();
    // 只有盘符（"c:"）也是路径模式：名字段空、片段 = 盘符本身。
    let drive_only = {
        let b = lower.as_bytes();
        b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
    };
    if !lower.contains('\\') && !drive_only {
        return ParsedQuery {
            wildcard: lower.contains('*') || lower.contains('?'),
            name_q: lower,
            path_segs: Vec::new(),
            anchored: false,
            dir_only: false,
            path_mode: false,
        };
    }
    if drive_only {
        return ParsedQuery {
            name_q: String::new(),
            path_segs: vec![lower.clone()],
            anchored: true,
            wildcard: false,
            dir_only: true,
            path_mode: true,
        };
    }
    let parts: Vec<&str> = lower.split('\\').collect();
    let name_q = parts.last().copied().unwrap_or("").to_string();
    let path_segs: Vec<String> = parts[..parts.len() - 1]
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    let anchored = path_segs
        .first()
        .map(|s| s.ends_with(':'))
        .unwrap_or(false);
    ParsedQuery {
        wildcard: name_q.contains('*') || name_q.contains('?'),
        dir_only: name_q.is_empty(),
        name_q,
        path_segs,
        anchored,
        path_mode: true,
    }
}

/// 目录路径切段（小写）。
fn dir_segs_lower(dir: &str) -> Vec<String> {
    dir.split(['\\', '/'])
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

/// 路径片段对齐：片段每一段都是对应目录段的子串（不区分大小写，
/// 段已全小写）；非 anchored 时对齐窗口可在目录段列任意位置，
/// anchored（盘符开头）只能从第 0 段开始。
fn frag_aligns(dir_segs: &[String], frag: &[String], anchored: bool) -> bool {
    if frag.is_empty() {
        return true;
    }
    if frag.len() > dir_segs.len() {
        return false;
    }
    let last_start = dir_segs.len() - frag.len();
    let range = if anchored { 0..=0 } else { 0..=last_start };
    range.into_iter().any(|st| {
        frag.iter()
            .zip(&dir_segs[st..])
            .all(|(f, d)| d.contains(f.as_str()))
    })
}

/// 目录本身命中用：对齐窗口必须结束在完整路径的最后一段
///（片段描述的是这个目录自己，不是它的父级）。
fn frag_aligns_end(full_segs: &[String], frag: &[String], anchored: bool) -> bool {
    if frag.is_empty() || frag.len() > full_segs.len() {
        return false;
    }
    let st = full_segs.len() - frag.len();
    if anchored && st != 0 {
        return false;
    }
    frag.iter()
        .zip(&full_segs[st..])
        .all(|(f, d)| d.contains(f.as_str()))
}

/// 通配匹配：* = 任意串、? = 任意单字（字符已全小写）。贪心双指针
/// + 星号回溯，整串匹配。
fn wildcard_match(pat: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star_p, mut star_t) = (usize::MAX, 0usize);
    while t < text.len() {
        if p < pat.len() && (pat[p] == '?' || pat[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star_p = p;
            star_t = t;
            p += 1;
        } else if star_p != usize::MAX {
            p = star_p + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

/// 子序列匹配：q 的字符按序在 name 中出现（大小写折叠）即命中，
/// 返回 (首个命中字符的字节偏移, 跨度内多余字符数）。双指针单趟，
/// 任一字符找不到立即退出。
fn subsequence_match(name: &str, q: &str) -> Option<(usize, usize)> {
    let q_len = q.chars().count();
    let mut rest = q.chars();
    let mut want = rest.next()?;
    let mut first_byte = 0usize;
    let mut first_char = 0usize;
    let mut last_char = 0usize;
    let mut seen_first = false;
    for (ci, (bi, c)) in name.char_indices().enumerate() {
        let folded = if c.is_ascii() {
            c.to_ascii_lowercase()
        } else {
            c.to_lowercase().next().unwrap_or(c)
        };
        if folded == want {
            if !seen_first {
                first_byte = bi;
                first_char = ci;
                seen_first = true;
            }
            last_char = ci;
            match rest.next() {
                Some(n) => want = n,
                None => {
                    let span = last_char - first_char + 1;
                    return Some((first_byte, span.saturating_sub(q_len)));
                }
            }
        }
    }
    None
}

/// 子序列补齐的触发阈值：第一遍常规通道命中总数低于此数，才跑
/// 第二遍子序列扫描。子序列要逐条目全量扫名（2M 条目量级下单遍
/// 上百毫秒），而命中充足时用户要看的本来就在常规结果里，
/// 没必要为长尾兜底买单。
const FUZZY_FILL_THRESHOLD: usize = 30;

pub struct SearchEngine<'a> {
    store: &'a IndexStore,
    usage: Option<&'a UsageStore>,
}

impl<'a> SearchEngine<'a> {
    pub fn new(store: &'a IndexStore, usage: Option<&'a UsageStore>) -> SearchEngine<'a> {
        SearchEngine { store, usage }
    }

    pub fn store(&self) -> &'a IndexStore {
        self.store
    }

    /// category 为 None = 全部分类；limit 为最多返回条数（UI 传 100）。
    /// 单类查询是掩码查询的特例（一位掩码），见 search_scoped。
    pub fn search(
        &self,
        query: &str,
        category: Option<Category>,
        limit: usize,
        cancelled: &AtomicBool,
    ) -> Vec<SearchHit> {
        let mask = category.map(|c| 1u16 << (c as u8)).unwrap_or(0);
        self.search_scoped(query, mask, limit, cancelled, None)
    }

    /// 带分类掩码与搜索范围的入口。
    /// cat_mask：u16 位掩码，11 类各占一位（bit = Category as u8），
    /// 条目分类命中任一位即过；0 = 全部。
    /// scope：为 Some 时条目完整路径必须落在任一范围根内才参与
    /// 打分——过滤在扫描里做（先过范围再打分），app 侧拿到的
    /// top N 不会被范围外结果挤占（app 侧事后过滤会把 100 条筛空，
    /// 所以必须在这里做）。
    pub fn search_scoped(
        &self,
        query: &str,
        cat_mask: u16,
        limit: usize,
        cancelled: &AtomicBool,
        scope: Option<&ScopeFilter>,
    ) -> Vec<SearchHit> {
        if query.trim().is_empty() || limit == 0 || self.store.count() == 0 {
            return Vec::new();
        }
        // 查询模式解析（路径 / 通配 / 普通）：普通无斜杠 query 的
        // name_q 就是原 query 小写，行为与之前逐字一致。
        let pq = parse_query(query);
        if pq.path_mode && pq.dir_only && pq.path_segs.is_empty() {
            // 只剩一个斜杠：没有路径片段也没有名字，无从匹配
            return Vec::new();
        }
        if !pq.path_mode && !pq.wildcard && pq.name_q.is_empty() {
            // query 全是噪声字符（"<>|）→ 清洗后为空
            return Vec::new();
        }
        let initials_eligible = !pq.name_q.is_empty()
            && pq.name_q.bytes().all(|b| b.is_ascii_alphanumeric());
        let count = self.store.count();
        // 并行度封顶 4（v0.1.6）：搜索是前台交互，全核狂奔会和索引构建
        // 叠加抢满 CPU、拖垮整机流畅度；4 分区对当前索引规模已足够。
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(4);
        let partitions = threads.max(1);

        // 注意：thread::scope 的闭包参数也叫 scope，会遮蔽范围参数，先存别名
        let scope_filter = scope;
        // 一遍全分区扫描。enable_fuzzy 控制子序列通道是否开启，其余
        // 通道（名字 / 首字母 / 通配 / 路径）两遍行为完全一致。
        let run_scan = |enable_fuzzy: bool| -> Vec<Vec<SearchHit>> {
            std::thread::scope(|scope| {
                let mut handles = Vec::with_capacity(partitions);
                for p in 0..partitions {
                    let start = count * p / partitions;
                    let end = count * (p + 1) / partitions;
                    let pq = &pq;
                    handles.push(scope.spawn(move || {
                        self.scan_partition(
                            start,
                            end,
                            pq,
                            initials_eligible,
                            cat_mask,
                            cancelled,
                            scope_filter,
                            enable_fuzzy,
                        )
                    }));
                }
                handles
                    .into_iter()
                    .map(|h| h.join().unwrap_or_default())
                    .collect()
            })
        };

        // ---- 两段式填充 ----
        // 第一遍不开子序列：精确 / 前缀 / 分段 / 子串 / 拼音 / 通配 /
        // 路径通道全靠锚点扫描，本来就快。第二遍（开子序列重扫）只在
        // 第一遍命中总数 < FUZZY_FILL_THRESHOLD 时才跑——子序列是唯一
        // 要逐条目全量扫名的通道，把它的成本关进「命中稀少」这个笼子：
        // 命中充足的常见查询永远不付这笔钱，命中稀少时用户本来就在
        // 翻长尾，多等一遍也值。单字查询永不跑（其他通道已足够）。
        // 目录本身 / 通配模式没有子序列通道，天然只跑第一遍。
        let mut locals = run_scan(false);
        if cancelled.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let first_total: usize = locals.iter().map(|l| l.len()).sum();
        if first_total < FUZZY_FILL_THRESHOLD
            && pq.name_q.chars().count() >= 2
            && !pq.dir_only
            && !pq.wildcard
        {
            // 第二遍的名字 / 首字母通道会重演第一遍的命中，但子序列
            // 通道在 scan_partition 内部本就只对其余通道未命中者跑
            //（matched 判定，与第一遍同一套谓词），所以合并时只需
            // 取回第二遍的 fuzzy 命中，与第一遍天然不重叠，不另造
            // 去重结构。
            let second = run_scan(true);
            if cancelled.load(Ordering::Relaxed) {
                return Vec::new();
            }
            for (dst, extra) in locals.iter_mut().zip(second) {
                dst.extend(extra.into_iter().filter(|h| h.fuzzy));
            }
        }
        let total: usize = locals.iter().map(|l| l.len()).sum();
        if total == 0 {
            return Vec::new();
        }
        let mut all: Vec<SearchHit> = Vec::with_capacity(total);
        for l in locals {
            all.extend(l);
        }
        // 排序：分数降序为主键；同分时依次按路径深度升序（浅者在前）、
        // 名字长度升序（短者在前）、原索引序——v0.1.5 加的次级键，
        // 免得同分条目（尤其降权后并列的缓存文件）按任意顺序挤占榜面。
        // 次级键每条只算一次（别在比较器里反复拼路径 / 数深度）。
        let mut keyed: Vec<(SearchHit, usize, usize)> = all
            .into_iter()
            .map(|h| {
                let i = h.index as usize;
                let depth = self
                    .store
                    .dir_path(i)
                    .chars()
                    .filter(|c| *c == '\\' || *c == '/')
                    .count();
                let name_len = self.store.name(i).chars().count();
                (h, depth, name_len)
            })
            .collect();
        keyed.sort_by(|a, b| {
            b.0.score
                .partial_cmp(&a.0.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.cmp(&b.2))
                .then_with(|| a.0.index.cmp(&b.0.index))
        });
        let mut all: Vec<SearchHit> = keyed.into_iter().map(|(h, _, _)| h).collect();
        all.truncate(limit);
        all
    }

    fn scan_partition(
        &self,
        start: usize,
        end: usize,
        pq: &ParsedQuery,
        initials_eligible: bool,
        cat_mask: u16,
        cancelled: &AtomicBool,
        scope: Option<&ScopeFilter>,
        enable_fuzzy: bool,
    ) -> Vec<SearchHit> {
        let store = self.store;
        let name_off = store.name_off();
        let name_pool = store.name_pool();
        let cats = store.cats();
        // 谓词链顺序（全模式统一）：分类掩码 → 范围根 → 路径对齐 → 名字通道。
        // 路径对齐谓词：路径模式且名字段非空时，条目所在目录段列必须
        // 与 query 的路径片段对齐（见 frag_aligns）；目录本身模式在
        // 下面的专属分支里用末尾对齐，不走这个谓词。
        let path_ok = |entry: usize| -> bool {
            if !pq.path_mode || pq.dir_only {
                return true;
            }
            let segs = dir_segs_lower(store.dir_path(entry));
            frag_aligns(&segs, &pq.path_segs, pq.anchored)
        };

        // ---- 目录本身模式（query 以斜杠结尾）：只出对齐命中的目录
        // 本身（文件夹条目、片段末尾对齐到它自己的最后一段），绝不
        // 把目录下的内容倒出来。基础分取分段档 120，再走综合分。
        if pq.dir_only {
            let mut local: Vec<SearchHit> = Vec::new();
            for entry in start..end {
                if cat_mask != 0 && (cat_mask & (1u16 << cats[entry])) == 0 {
                    continue;
                }
                if cats[entry] != Category::Folder as u8 {
                    continue;
                }
                if let Some(s) = scope {
                    if !s.matches_parts(store.dir_path(entry), store.name(entry)) {
                        continue;
                    }
                }
                let mut segs = dir_segs_lower(store.dir_path(entry));
                segs.push(store.name(entry).to_lowercase());
                if !frag_aligns_end(&segs, &pq.path_segs, pq.anchored) {
                    continue;
                }
                let (score, demoted) = self.finish_score(entry, 120.0);
                local.push(SearchHit {
                    index: entry as u32,
                    score,
                    match_start: 0,
                    match_len: 0,
                    via_initials: false,
                    demoted,
                    fuzzy: false,
                });
                if local.len() >= 16384 {
                    trim_local(&mut local);
                }
            }
            return local;
        }

        // ---- 通配模式（名字段含 * / ?）：整名匹配住 → 前缀档 160；
        // 只有子串窗口匹配住（等价于模式两端加 *）→ 子串档 80。
        if pq.wildcard {
            let pat: Vec<char> = pq.name_q.chars().collect();
            let mut wrapped: Vec<char> = Vec::with_capacity(pat.len() + 2);
            wrapped.push('*');
            wrapped.extend_from_slice(&pat);
            wrapped.push('*');
            let mut local: Vec<SearchHit> = Vec::new();
            for entry in start..end {
                if cat_mask != 0 && (cat_mask & (1u16 << cats[entry])) == 0 {
                    continue;
                }
                if let Some(s) = scope {
                    if !s.matches_parts(store.dir_path(entry), store.name(entry)) {
                        continue;
                    }
                }
                if !path_ok(entry) {
                    continue;
                }
                let name_chars: Vec<char> =
                    store.name(entry).to_lowercase().chars().collect();
                let base = if wildcard_match(&pat, &name_chars) {
                    160.0
                } else if wildcard_match(&wrapped, &name_chars) {
                    80.0
                } else {
                    continue;
                };
                let (score, demoted) =
                    self.finish_score(entry, base - name_chars.len() as f32 * 0.01);
                local.push(SearchHit {
                    index: entry as u32,
                    score,
                    match_start: 0,
                    match_len: 0,
                    via_initials: false,
                    demoted,
                    fuzzy: false,
                });
                if local.len() >= 16384 {
                    trim_local(&mut local);
                }
            }
            return local;
        }

        let q = pq.name_q.as_str();
        let q_chars = q.chars().count() as u32;
        let qb = q.as_bytes();
        let seg_start = name_off[start] as usize;
        let seg_end = name_off[end] as usize;
        let segment = &name_pool[seg_start..seg_end];

        let mut local: Vec<SearchHit> = Vec::with_capacity(256);
        let mut matched = vec![false; end - start];

        // ---- 名字通道：两字节锚点找候选 ----
        // 只用首字节做锚时，'m'、'2' 这类高频字节会产生百万级候选（mp4 查询实测
        // 被拖到 50ms+）。改用查询前两字节的大小写变体（≤4 种）做 memmem 锚点，
        // 候选量降一到两个数量级，再逐个做全串折叠校验。
        let positions = anchor_positions(segment, qb);
        let mut entry = start;
        let mut checked = 0usize;
        for rel in positions {
            let pos = seg_start + rel;
            while entry + 1 < end && (name_off[entry + 1] as usize) <= pos {
                entry += 1;
            }
            // 候选可能落在 entry 的名字之外（偏移间隙不存在，池是紧挨的，所以 pos 必在某条目内；
            // 但 entry 推进只到 end-1 为止，越界候选丢弃）
            if pos >= name_off[entry + 1] as usize {
                continue;
            }
            if cat_mask != 0 && (cat_mask & (1u16 << cats[entry])) == 0 {
                continue;
            }
            if let Some(s) = scope {
                if !s.matches_parts(store.dir_path(entry), store.name(entry)) {
                    continue;
                }
            }
            if !path_ok(entry) {
                continue;
            }
            let name_end = name_off[entry + 1] as usize;
            if pos + qb.len() > name_end {
                continue;
            }
            if !fold_eq(&name_pool[pos..pos + qb.len()], qb) {
                continue;
            }
            let match_start = pos - name_off[entry] as usize;
            let (score, demoted) = self.score_match(entry, match_start, false, q);
            matched[entry - start] = true;
            local.push(SearchHit {
                index: entry as u32,
                score,
                match_start: match_start as u32,
                match_len: q_chars,
                via_initials: false,
                demoted,
                fuzzy: false,
            });
            checked += 1;
            if checked % 4096 == 0 && cancelled.load(Ordering::Relaxed) {
                return Vec::new();
            }
            if local.len() >= 16384 {
                trim_local(&mut local);
            }
        }

        // ---- 拼音首字母通道（仅纯 ASCII 字母数字查询）----
        if initials_eligible {
            let init_off = store.init_off();
            let init_pool = store.init_pool();
            let iseg_start = init_off[start] as usize;
            let iseg_end = init_off[end] as usize;
            let isegment = &init_pool[iseg_start..iseg_end];
            let mut ientry = start;
            for rel in memchr::memchr_iter(qb[0], isegment) {
                let pos = iseg_start + rel;
                while ientry + 1 < end && (init_off[ientry + 1] as usize) <= pos {
                    ientry += 1;
                }
                if pos >= init_off[ientry + 1] as usize || matched[ientry - start] {
                    continue;
                }
                if cat_mask != 0 && (cat_mask & (1u16 << cats[ientry])) == 0 {
                    continue;
                }
                if let Some(s) = scope {
                    if !s.matches_parts(store.dir_path(ientry), store.name(ientry)) {
                        continue;
                    }
                }
                if !path_ok(ientry) {
                    continue;
                }
                let iend = init_off[ientry + 1] as usize;
                if pos + qb.len() > iend {
                    continue;
                }
                if &init_pool[pos..pos + qb.len()] != qb {
                    continue;
                }
                let match_start = pos - init_off[ientry] as usize;
                let (score, demoted) = self.score_match(ientry, match_start, true, q);
                matched[ientry - start] = true;
                local.push(SearchHit {
                    index: ientry as u32,
                    score,
                    match_start: match_start as u32,
                    match_len: q_chars,
                    via_initials: true,
                    demoted,
                    fuzzy: false,
                });
                if local.len() >= 16384 {
                    trim_local(&mut local);
                }
            }
        }

        // ---- 子序列模糊通道（最低档）：其余通道全未命中的条目才跑 ----
        // query 字符按序出现在名字里即命中（不要求连续），按跨度
        // 紧凑度在档内给分。逐条目短路（matched 跳过）、类别/范围
        // 先过滤再扫名，双指针早退出；这是唯一的全量兜底通道，
        // 所以默认不开——由 search_scoped 的两段式填充决定：仅当
        // 第一遍（enable_fuzzy=false）命中总数不足时才开第二遍。
        if enable_fuzzy && q.chars().count() >= 2 {
            for entry in start..end {
                if matched[entry - start] {
                    continue;
                }
                if cat_mask != 0 && (cat_mask & (1u16 << cats[entry])) == 0 {
                    continue;
                }
                if let Some(s) = scope {
                    if !s.matches_parts(store.dir_path(entry), store.name(entry)) {
                        continue;
                    }
                }
                if !path_ok(entry) {
                    continue;
                }
                let name = store.name(entry);
                if let Some((first_byte, extra)) = subsequence_match(name, q) {
                    let base = 30.0 - (extra as f32 * 0.5).min(10.0);
                    let (score, demoted) = self.finish_score(entry, base);
                    matched[entry - start] = true;
                    local.push(SearchHit {
                        index: entry as u32,
                        score,
                        match_start: first_byte as u32,
                        match_len: 0,
                        via_initials: false,
                        demoted,
                        fuzzy: true,
                    });
                    if local.len() >= 16384 {
                        trim_local(&mut local);
                    }
                }
            }
        }
        local
    }

    /// 返回 (得分, 是否被垃圾降权)。
    /// 通道分档（v0.1.5 重定，档间距 ≥40，重要性加减不翻盘）：
    /// 完全一致 200 / 前缀 160 / 分段前缀 120 / 子串 80 /
    /// 拼音首字母 前缀 75 · 包含 60 / 子序列 30（在通道处给分）。
    /// 综合分 = 通道分 + 重要性（finish_score），排序仍由匹配度主导。
    fn score_match(
        &self,
        index: usize,
        match_start_byte: usize,
        via_initials: bool,
        q: &str,
    ) -> (f32, bool) {
        let name = self.store.name(index);
        let name_len = name.chars().count() as f32;
        // 命中位置按「字符数」计。注意 via_initials 时 match_start_byte 是首字母串
        // 里的偏移，绝不能拿去切名字（会对不齐甚至切碎多字节汉字而 panic，
        // v0.1.0 拼音搜不到结果就是这个原因）；首字母串是纯 ASCII，字节偏移即字符数。
        let (base, match_at) = if via_initials {
            (
                if match_start_byte == 0 { 75.0 } else { 60.0 },
                match_start_byte as f32,
            )
        } else {
            // 不切片：按 char_indices 数命中点前的字符数，prev/cur 取命中点前后字符。
            let mut cnt = 0usize;
            let mut prev = ' ';
            let mut cur = ' ';
            for (i, c) in name.char_indices() {
                if i >= match_start_byte {
                    cur = c;
                    break;
                }
                cnt += 1;
                prev = c;
            }
            let match_at = cnt as f32;
            let tier = if is_exact_name(name, q) {
                200.0
            } else if cnt == 0 {
                160.0
            } else if is_segment_start(prev, cur) {
                120.0
            } else {
                80.0
            };
            (tier, match_at)
        };
        let score = base - match_at * 0.1 - name_len * 0.01;
        self.finish_score(index, score)
    }

    /// 通道分之上的重要性调整：分类小额加分（App/快捷方式/安装目录/
    /// 文件夹 +5）、垃圾降权（哈希名 −60 / 垃圾路径 −40，可叠加，
    /// 见 junk_penalty）、使用频次加分（UsageStore 现有规则，上限
    /// 约 +12）。合计幅度小于档间距，保证综合分不翻盘通道大档。
    fn finish_score(&self, index: usize, mut score: f32) -> (f32, bool) {
        let name = self.store.name(index);
        score += match self.store.category(index) {
            Category::App | Category::Shortcut => 5.0,
            Category::InstallDir | Category::Folder => 5.0,
            _ => 0.0,
        };
        let penalty = junk_penalty(name, &self.store.full_path(index));
        score -= penalty;
        if let Some(u) = self.usage {
            score += u.boost(&self.store.full_path(index));
        }
        (score, penalty > 0.0)
    }
}

/// 完全一致：全名或去扩展名后的主干与 query 相等（大小写不论）。
fn is_exact_name(name: &str, q: &str) -> bool {
    let lower = name.to_lowercase();
    if lower == q {
        return true;
    }
    match lower.rfind('.') {
        Some(p) if p > 0 => &lower[..p] == q,
        _ => false,
    }
}

/// 分段前缀判定：命中点是否落在一段的开头。分段边界 = 分隔符
///（空格 / _ / - / . / · / 括号）、驼峰边界（小写→大写）、
/// 汉字→非汉字交界。如 ".claude" 的 claude 段、"ShunSeek" 的 Seek 段。
fn is_segment_start(prev: char, cur: char) -> bool {
    matches!(prev, ' ' | '-' | '_' | '·' | '.' | '．' | '(' | '（')
        || (prev.is_lowercase() && cur.is_uppercase())
        || (is_cjk(prev) && !is_cjk(cur))
}

/// 锚点候选位置：查询 ≥2 字节时用前两字节的大小写变体做 memmem 扫描后排序去重；
/// 单字节查询退回 memchr 首字节扫描。
fn anchor_positions(segment: &[u8], qb: &[u8]) -> Vec<usize> {
    if qb.len() >= 2 {
        let variants = |b: u8| -> Vec<u8> {
            if b.is_ascii_alphabetic() {
                vec![b.to_ascii_lowercase(), b.to_ascii_uppercase()]
            } else {
                vec![b]
            }
        };
        let mut positions = Vec::new();
        for a in variants(qb[0]) {
            for b in variants(qb[1]) {
                let pat = [a, b];
                positions.extend(memchr::memmem::find_iter(segment, &pat));
            }
        }
        positions.sort_unstable();
        positions.dedup();
        positions
    } else {
        let first = qb[0];
        if first.is_ascii_alphabetic() {
            let mut v: Vec<usize> = memchr::memchr2_iter(
                first.to_ascii_lowercase(),
                first.to_ascii_uppercase(),
                segment,
            )
            .collect();
            v.sort_unstable();
            v
        } else {
            memchr::memchr_iter(first, segment).collect()
        }
    }
}

fn trim_local(local: &mut Vec<SearchHit>) {
    local.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    local.truncate(4096);
}

/// 哈希样名字：去掉扩展名后长度 ≥12，且 ≥80% 的字符是十六进制字符
/// （0-9a-f，大小写不论）。典型如微信缓存里的 `wx915c6f684ab5b15f`
/// ——`wx` 前缀 + 16 位哈希，比例 16/18 ≈ 89%，命中。
fn looks_like_hash_name(name: &str) -> bool {
    let stem = match name.rfind('.') {
        Some(p) if p > 0 => &name[..p],
        _ => name,
    };
    let total = stem.chars().count();
    if total < 12 {
        return false;
    }
    let hex = stem.chars().filter(|c| c.is_ascii_hexdigit()).count();
    hex * 5 >= total * 4 // ≥80%，整数算，避免浮点
}

/// 垃圾路径段：命中任一即视为缓存 / 临时类目录。段带前后反斜杠，
/// 只匹配完整目录段，不误伤名字里恰好含这些词的文件。
const JUNK_PATH_SEGMENTS: &[&str] = &[
    "\\temp\\",
    "\\tmp\\",
    "\\cache\\",
    "\\caches\\",
    "\\code cache\\",
    "\\gpucache\\",
    "\\radium\\",
    "\\xwechat\\",
];

/// v0.1.5 降权：哈希名 −60，垃圾路径 −40，两项可叠加。
/// 这些条目不是搜不到，而是没有理由排在用户真正要找的东西前面。
fn junk_penalty(name: &str, full_path: &str) -> f32 {
    let mut penalty = 0.0;
    if looks_like_hash_name(name) {
        penalty += 60.0;
    }
    // 路径统一成小写 + 反斜杠再查段（索引里的路径以 '\\' 拼接，
    // 演示 / 兜底数据可能混 '/'，一并归一）。
    let lower = full_path.to_lowercase().replace('/', "\\");
    if JUNK_PATH_SEGMENTS.iter().any(|s| lower.contains(s)) {
        penalty += 40.0;
    }
    penalty
}

fn is_cjk(c: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&c)
}

/// 大小写折叠相等：查询已转小写，水池字节按 ASCII 折叠后逐字节比；
/// 非 ASCII 字节（UTF-8 多字节）原样比（汉字没有大小写）。
fn fold_eq(hay: &[u8], needle_lower: &[u8]) -> bool {
    if hay.len() != needle_lower.len() {
        return false;
    }
    hay.iter()
        .zip(needle_lower.iter())
        .all(|(a, b)| a.to_ascii_lowercase() == *b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Builder;

    fn store_of(entries: &[(&str, &str, Category)]) -> IndexStore {
        let mut b = Builder::new();
        for (name, dir, cat) in entries {
            b.add(name, dir, *cat, 0);
        }
        b.freeze()
    }

    fn names(store: &IndexStore, hits: &[SearchHit]) -> Vec<String> {
        hits.iter()
            .map(|h| store.name(h.index as usize).to_string())
            .collect()
    }

    #[test]
    fn hash_detection() {
        assert!(looks_like_hash_name("wx915c6f684ab5b15f")); // wx + 16 位哈希
        assert!(looks_like_hash_name("deadbeefcafe1234")); // 纯哈希
        assert!(looks_like_hash_name("wx915c6f684ab5b15f.tmp")); // 带扩展名看主干
        assert!(!looks_like_hash_name("wxwork2024")); // 太短（<12）
        assert!(!looks_like_hash_name("my-holiday-photos")); // 正常长名，十六进制占比低
        assert!(!looks_like_hash_name("WXQTY")); // 正常短名
    }

    #[test]
    fn wechat_cache_flood_ranks_below_normal() {
        // 复刻用户真机场景：搜 wx，正常文件夹与微信 radium 缓存里的
        // 哈希文件夹同为前缀命中——正常结果必须排在哈希结果之前。
        let store = store_of(&[
            (
                "wx915c6f684ab5b15f",
                "C:\\Users\\me\\AppData\\Roaming\\Tencent\\xwechat\\radium\\users\\629fb81d",
                Category::Folder,
            ),
            (
                "wx3f78b6f96e5992f4",
                "C:\\Users\\me\\AppData\\Roaming\\Tencent\\xwechat\\radium\\users\\629fb81d",
                Category::Folder,
            ),
            ("WXWork", "C:\\Users\\me\\Documents", Category::Folder),
            ("WXQTY", "C:\\Users\\me\\AppData\\Roaming\\os-launcher", Category::Folder),
        ]);
        let engine = SearchEngine::new(&store, None);
        let hits = engine.search("wx", None, 100, &AtomicBool::new(false));
        let got = names(&store, &hits);
        assert_eq!(got.len(), 4, "四条都应命中：{got:?}");
        // 前两名是正常名（WXWork / WXQTY，谁先都行），后两名是哈希文件夹
        assert!(
            got[..2].contains(&"WXWork".to_string()) && got[..2].contains(&"WXQTY".to_string()),
            "正常结果应在哈希文件夹之前：{got:?}"
        );
        assert!(
            got[2..].iter().all(|n| looks_like_hash_name(n)),
            "哈希文件夹应沉底：{got:?}"
        );
    }

    #[test]
    fn hash_name_alone_is_demoted_on_clean_path() {
        // 路径干净、只有名字是哈希：−60 也足以让它排在正常名之后
        let store = store_of(&[
            ("wx915c6f684ab5b15f", "D:\\Data", Category::Folder),
            ("wxnotes", "D:\\Data", Category::Folder),
        ]);
        let engine = SearchEngine::new(&store, None);
        let hits = engine.search("wx", None, 100, &AtomicBool::new(false));
        let got = names(&store, &hits);
        assert_eq!(got.first().map(|s| s.as_str()), Some("wxnotes"), "{got:?}");
    }

    #[test]
    fn demoted_flag_marks_junk_hits() {
        // demoted 标志：哈希名 / 垃圾路径任一命中即为 true，正常结果为 false
        //（UI 靠它把不重要结果归类到尾部折叠区）
        let store = store_of(&[
            (
                "wx915c6f684ab5b15f",
                "C:\\Users\\me\\AppData\\Roaming\\Tencent\\xwechat\\radium\\users\\629fb81d",
                Category::Folder,
            ),
            ("wxcachefile", "C:\\Users\\me\\AppData\\Local\\Temp", Category::Folder),
            ("WXWork", "C:\\Users\\me\\Documents", Category::Folder),
        ]);
        let engine = SearchEngine::new(&store, None);
        let hits = engine.search("wx", None, 100, &AtomicBool::new(false));
        assert_eq!(hits.len(), 3);
        for h in &hits {
            let name = store.name(h.index as usize);
            let expect = name != "WXWork";
            assert_eq!(h.demoted, expect, "{name} 的 demoted 应为 {expect}");
        }
    }

    #[test]
    fn scope_filter_limits_to_roots() {
        // 跨盘 + 桌面内外 + 目录边界：范围外的条目不参与打分
        let store = store_of(&[
            ("report", "C:\\Users\\me\\Desktop", Category::Document),
            ("report", "C:\\Users\\me\\Desktop2", Category::Document),
            ("report", "D:\\Docs", Category::Document),
            ("report", "C:\\Users\\me\\Documents", Category::Document),
        ]);
        let engine = SearchEngine::new(&store, None);
        // 只限桌面：Desktop 内命中；Desktop2 是边界误含用例，必须排除
        let scope = ScopeFilter::new(&["C:\\Users\\me\\Desktop".to_string()]);
        let hits = engine.search_scoped("report", 0, 100, &AtomicBool::new(false), Some(&scope));
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(store.dir_path(hits[0].index as usize), "C:\\Users\\me\\Desktop");
        // 盘根范围：D:\ 下全中
        let scope = ScopeFilter::new(&["D:\\".to_string()]);
        let hits = engine.search_scoped("report", 0, 100, &AtomicBool::new(false), Some(&scope));
        assert_eq!(hits.len(), 1);
        assert_eq!(store.dir_path(hits[0].index as usize), "D:\\Docs");
        // 多根并集
        let scope = ScopeFilter::new(&[
            "C:\\Users\\me\\Desktop".to_string(),
            "D:\\".to_string(),
        ]);
        let hits = engine.search_scoped("report", 0, 100, &AtomicBool::new(false), Some(&scope));
        assert_eq!(hits.len(), 2);
        // 旧入口（无范围）全中，现有调用语义不变
        let hits = engine.search("report", None, 100, &AtomicBool::new(false));
        assert_eq!(hits.len(), 4);
    }

    #[test]
    fn category_mask_selects_multiple() {
        // 类型多选：掩码选 文档+图片 时只出这两类；掩码 0 = 全部；
        // 旧单类入口（一位掩码的特例）语义不变。
        let store = store_of(&[
            ("alpha", "C:\\x", Category::Document),
            ("alpha", "C:\\x", Category::Image),
            ("alpha", "C:\\x", Category::Video),
        ]);
        let engine = SearchEngine::new(&store, None);
        let mask = (1u16 << Category::Document as u8) | (1u16 << Category::Image as u8);
        let hits = engine.search_scoped("alpha", mask, 100, &AtomicBool::new(false), None);
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert!(hits.iter().all(|h| matches!(
            store.category(h.index as usize),
            Category::Document | Category::Image
        )));
        let hits = engine.search_scoped("alpha", 0, 100, &AtomicBool::new(false), None);
        assert_eq!(hits.len(), 3);
        let hits = engine.search("alpha", Some(Category::Video), 100, &AtomicBool::new(false));
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn dot_prefixed_entries_are_searchable() {
        // 点开头条目（.claude 文件夹 / .gitignore 文件）必须能搜到：
        // 子串命中即可，且不被哈希降权误伤（去点后也不是十六进制串）
        let store = store_of(&[
            (".claude", "C:\\Users\\me", Category::Folder),
            (".gitignore", "C:\\Users\\me\\proj", Category::Other),
            ("claude-notes", "C:\\Users\\me", Category::Document),
        ]);
        let engine = SearchEngine::new(&store, None);
        let hits = engine.search("claude", None, 100, &AtomicBool::new(false));
        let got = names(&store, &hits);
        assert!(got.contains(&".claude".to_string()), "{got:?}");
        let h = hits
            .iter()
            .find(|h| store.name(h.index as usize) == ".claude")
            .unwrap();
        assert!(!h.demoted, ".claude 不应被降权");
        for q in ["gitignore", "git"] {
            let hits = engine.search(q, None, 100, &AtomicBool::new(false));
            let got = names(&store, &hits);
            assert!(got.contains(&".gitignore".to_string()), "搜 {q}：{got:?}");
            let h = hits
                .iter()
                .find(|h| store.name(h.index as usize) == ".gitignore")
                .unwrap();
            assert!(!h.demoted, ".gitignore 不应被降权");
        }
    }

    #[test]
    fn match_tiers_ordering() {
        // 同一 query 五档俱全时排序必须是：
        // 完全一致 > 前缀 > 分段前缀 > 子串 > 子序列
        let store = store_of(&[
            ("s-e-e-k", "C:\\x", Category::Other),
            ("aseekb", "C:\\x", Category::Other),
            ("ShunSeek", "C:\\x", Category::Other),
            ("seektool", "C:\\x", Category::Other),
            ("seek", "C:\\x", Category::Other),
        ]);
        let engine = SearchEngine::new(&store, None);
        let hits = engine.search("seek", None, 100, &AtomicBool::new(false));
        let got = names(&store, &hits);
        assert_eq!(
            got,
            vec!["seek", "seektool", "ShunSeek", "aseekb", "s-e-e-k"],
            "{got:?}"
        );
    }

    #[test]
    fn subsequence_hits() {
        // "cld" 子序列命中 "claude"（字符按序、不连续）
        let store = store_of(&[("claude", "C:\\x", Category::Folder)]);
        let engine = SearchEngine::new(&store, None);
        let hits = engine.search("cld", None, 100, &AtomicBool::new(false));
        assert_eq!(hits.len(), 1);
        assert!(hits[0].fuzzy);
    }

    #[test]
    fn fuzzy_fills_when_first_pass_is_scarce() {
        // 两段式填充，第一遍命中稀少（< 30）→ 第二遍子序列照常补齐：
        // "cld" 第一遍只有前缀命中的 cld-report（1 条），子序列把
        // claude 捞回来，且前缀档排在子序列档之前。
        // 注：claude 的首字母串就是 "claude"，不含子串 "cld"，
        // 名字也不含 "cld"——它只能被子序列命中，是干净的用例。
        let store = store_of(&[
            ("cld-report", "C:\\x", Category::Document),
            ("claude", "C:\\x", Category::Folder),
            ("unrelated", "C:\\x", Category::Other),
        ]);
        let engine = SearchEngine::new(&store, None);
        let hits = engine.search("cld", None, 100, &AtomicBool::new(false));
        assert_eq!(hits.len(), 2, "{:?}", names(&store, &hits));
        assert_eq!(store.name(hits[0].index as usize), "cld-report");
        assert!(!hits[0].fuzzy);
        assert_eq!(store.name(hits[1].index as usize), "claude");
        assert!(hits[1].fuzzy);
    }

    #[test]
    fn fuzzy_skipped_when_first_pass_has_enough_hits() {
        // 两段式填充，第一遍命中 ≥ 30 → 不跑子序列：35 条前缀命中
        // 照常全出（行为等价），而只能靠子序列命中的 sxeexk 不出现。
        // sxeexk 对 "seek"：名字不含子串 "seek"，首字母串（就是它
        // 本身）也不含——只有子序列（s-x-e-e-x-k 按序）能命中。
        let mut b = Builder::new();
        for i in 0..35 {
            b.add(&format!("seekfile{i:02}"), "C:\\x", Category::Document, 0);
        }
        b.add("sxeexk", "C:\\x", Category::Other, 0);
        let store = b.freeze();
        let engine = SearchEngine::new(&store, None);
        let hits = engine.search("seek", None, 100, &AtomicBool::new(false));
        assert_eq!(hits.len(), 35, "{:?}", names(&store, &hits));
        assert!(hits.iter().all(|h| !h.fuzzy));
        assert!(!names(&store, &hits).contains(&"sxeexk".to_string()));
    }

    #[test]
    fn usage_boost_cannot_overturn_tiers() {
        // 满频次的子串命中（80 + 12）也压不过裸前缀命中（160）
        let store = store_of(&[
            ("seekapp", "C:\\x", Category::Other),
            ("myseekfile", "C:\\x", Category::Other),
        ]);
        let mut usage = UsageStore::load(None);
        for _ in 0..60 {
            usage.record_use("C:\\x\\myseekfile");
        }
        let engine = SearchEngine::new(&store, Some(&usage));
        let hits = engine.search("seek", None, 100, &AtomicBool::new(false));
        let got = names(&store, &hits);
        assert_eq!(got.first().map(|s| s.as_str()), Some("seekapp"), "{got:?}");
    }

    #[test]
    fn category_mask_full_set_behaves_like_all() {
        // 全集掩码（12 位全 1）与不过滤等价；去掉一位即排除该类
        let store = store_of(&[
            ("alpha", "C:\\x", Category::Document),
            ("alpha", "C:\\x", Category::Image),
            ("alpha", "C:\\x", Category::Video),
        ]);
        let engine = SearchEngine::new(&store, None);
        let full: u16 = (1 << 12) - 1;
        let hits = engine.search_scoped("alpha", full, 100, &AtomicBool::new(false), None);
        assert_eq!(hits.len(), 3);
        let minus_video = full & !(1u16 << Category::Video as u8);
        let hits = engine.search_scoped("alpha", minus_video, 100, &AtomicBool::new(false), None);
        assert_eq!(hits.len(), 2);
        assert!(hits
            .iter()
            .all(|h| store.category(h.index as usize) != Category::Video));
    }

    #[test]
    fn shallower_path_wins_on_score_tie() {
        // 同名同分类同分时，路径浅的排前面（次级排序键）
        let store = store_of(&[
            ("wxfile", "C:\\a\\b\\c\\d", Category::Folder),
            ("wxfile", "C:\\a", Category::Folder),
        ]);
        let engine = SearchEngine::new(&store, None);
        let hits = engine.search("wxfile", None, 100, &AtomicBool::new(false));
        assert_eq!(hits.len(), 2);
        assert_eq!(store.dir_path(hits[0].index as usize), "C:\\a");
    }

    /// 路径模式钉死用例的索引：30894 主目录（其下 .claude / notes.txt /
    /// docs\报告.docx）、30894 目录本身、别的主目录下的同名 .claude、
    /// D 盘的报告与 txt 干扰项。
    fn path_mode_store() -> IndexStore {
        store_of(&[
            ("30894", "C:\\Users", Category::Folder),
            (".claude", "C:\\Users\\30894", Category::Folder),
            ("notes.txt", "C:\\Users\\30894", Category::Document),
            ("报告.docx", "C:\\Users\\30894\\docs", Category::Document),
            (".claude", "C:\\Users\\12345", Category::Folder),
            ("报告.docx", "D:\\Data", Category::Document),
            ("other.txt", "D:\\Data", Category::Document),
        ])
    }

    #[test]
    fn path_mode_variants_hit() {
        let store = path_mode_store();
        let engine = SearchEngine::new(&store, None);
        // 四种写法都必须命中 30894 下的 .claude（且不是 12345 下的）
        for q in [
            "3089\\.claude",
            "3089/claude",
            "users\\3089\\.cl",
            "C:\\Users\\30894\\.claude",
        ] {
            let hits = engine.search(q, None, 100, &AtomicBool::new(false));
            assert_eq!(hits.len(), 1, "query {q} 应只命中一条：{:?}", names(&store, &hits));
            assert_eq!(store.name(hits[0].index as usize), ".claude");
            assert_eq!(store.dir_path(hits[0].index as usize), "C:\\Users\\30894");
        }
        // 路径片段对不上 → 不命中
        let hits = engine.search("9999\\claude", None, 100, &AtomicBool::new(false));
        assert!(hits.is_empty(), "9999\\claude 不应命中：{:?}", names(&store, &hits));
    }

    #[test]
    fn path_mode_trailing_slash_lists_the_directory_itself() {
        let store = path_mode_store();
        let engine = SearchEngine::new(&store, None);
        // `3089\` → 只出 30894 目录本身，不倒出它下面的内容
        let hits = engine.search("3089\\", None, 100, &AtomicBool::new(false));
        assert_eq!(hits.len(), 1, "应只有目录本身：{:?}", names(&store, &hits));
        assert_eq!(store.name(hits[0].index as usize), "30894");
    }

    #[test]
    fn wildcard_mode_hits() {
        let store = path_mode_store();
        let engine = SearchEngine::new(&store, None);
        // *.txt 命中两个 txt（整名匹配 → 前缀档）
        let hits = engine.search("*.txt", None, 100, &AtomicBool::new(false));
        let got = names(&store, &hits);
        assert_eq!(got.len(), 2, "*.txt 应命中两个 txt：{got:?}");
        assert!(got.contains(&"notes.txt".to_string()) && got.contains(&"other.txt".to_string()));
        // 报?.docx 命中「报告.docx」（两份都在）
        let hits = engine.search("报?.docx", None, 100, &AtomicBool::new(false));
        assert_eq!(hits.len(), 2, "报?.docx 应命中两份报告：{:?}", names(&store, &hits));
        // 路径 + 通配叠加：3089\*.txt 只出 30894 下的 notes.txt
        let hits = engine.search("3089\\*.txt", None, 100, &AtomicBool::new(false));
        assert_eq!(hits.len(), 1, "叠加应只命中一条：{:?}", names(&store, &hits));
        assert_eq!(store.name(hits[0].index as usize), "notes.txt");
    }
}
