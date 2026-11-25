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
#[derive(Clone, Copy, Debug)]
pub struct SearchHit {
    pub index: u32,
    pub score: f32,
    pub match_start: u32,
    pub via_initials: bool,
}

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
    pub fn search(
        &self,
        query: &str,
        category: Option<Category>,
        limit: usize,
        cancelled: &AtomicBool,
    ) -> Vec<SearchHit> {
        let q = query.trim().to_lowercase();
        if q.is_empty() || limit == 0 || self.store.count() == 0 {
            return Vec::new();
        }
        let initials_eligible = !q.is_empty() && q.bytes().all(|b| b.is_ascii_alphanumeric());
        let cat_byte = category.map(|c| c as u8);
        let count = self.store.count();
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(8);
        let partitions = threads.max(1);

        let locals: Vec<Vec<SearchHit>> = std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(partitions);
            for p in 0..partitions {
                let start = count * p / partitions;
                let end = count * (p + 1) / partitions;
                let q = q.as_str();
                handles.push(scope.spawn(move || {
                    self.scan_partition(start, end, q, initials_eligible, cat_byte, cancelled)
                }));
            }
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_default())
                .collect()
        });

        if cancelled.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let total: usize = locals.iter().map(|l| l.len()).sum();
        if total == 0 {
            return Vec::new();
        }
        let mut all: Vec<SearchHit> = Vec::with_capacity(total);
        for l in locals {
            all.extend(l);
        }
        all.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        all.truncate(limit);
        all
    }

    fn scan_partition(
        &self,
        start: usize,
        end: usize,
        q: &str,
        initials_eligible: bool,
        cat_byte: Option<u8>,
        cancelled: &AtomicBool,
    ) -> Vec<SearchHit> {
        let store = self.store;
        let name_off = store.name_off();
        let name_pool = store.name_pool();
        let cats = store.cats();
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
            if let Some(cb) = cat_byte {
                if cats[entry] != cb {
                    continue;
                }
            }
            let name_end = name_off[entry + 1] as usize;
            if pos + qb.len() > name_end {
                continue;
            }
            if !fold_eq(&name_pool[pos..pos + qb.len()], qb) {
                continue;
            }
            let match_start = pos - name_off[entry] as usize;
            let score = self.score_match(entry, match_start, false);
            matched[entry - start] = true;
            local.push(SearchHit {
                index: entry as u32,
                score,
                match_start: match_start as u32,
                via_initials: false,
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
                if let Some(cb) = cat_byte {
                    if cats[ientry] != cb {
                        continue;
                    }
                }
                let iend = init_off[ientry + 1] as usize;
                if pos + qb.len() > iend {
                    continue;
                }
                if &init_pool[pos..pos + qb.len()] != qb {
                    continue;
                }
                let match_start = pos - init_off[ientry] as usize;
                let score = self.score_match(ientry, match_start, true);
                matched[ientry - start] = true;
                local.push(SearchHit {
                    index: ientry as u32,
                    score,
                    match_start: match_start as u32,
                    via_initials: true,
                });
                if local.len() >= 16384 {
                    trim_local(&mut local);
                }
            }
        }
        local
    }

    fn score_match(&self, index: usize, match_start_byte: usize, via_initials: bool) -> f32 {
        let name = self.store.name(index);
        // 命中位置与名字长度按「字符数」计，与 C# 版的 char 口径一致
        let match_at = name[..match_start_byte.min(name.len())].chars().count() as f32;
        let name_len = name.chars().count() as f32;
        let mut score = if via_initials {
            if match_at == 0.0 {
                80.0
            } else {
                50.0
            }
        } else if match_at == 0.0 {
            100.0
        } else {
            let prev = name[..match_start_byte.min(name.len())]
                .chars()
                .last()
                .unwrap_or(' ');
            let cur_is_cjk = name[match_start_byte.min(name.len())..]
                .chars()
                .next()
                .map(is_cjk)
                .unwrap_or(false);
            let word_start = matches!(prev, ' ' | '-' | '_' | '·' | '(' | '（' | '.')
                || (is_cjk(prev) && !cur_is_cjk);
            if word_start {
                70.0
            } else {
                40.0
            }
        };
        score -= match_at * 0.1;
        score -= name_len * 0.01;
        score += match self.store.category(index) {
            Category::App | Category::Shortcut => 15.0,
            Category::InstallDir | Category::Folder => 5.0,
            _ => 0.0,
        };
        if let Some(u) = self.usage {
            score += u.boost(&self.store.full_path(index));
        }
        score
    }
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
