//! 紧凑平坦索引：这是 Rust 版内存远小于 .NET 版（411MB）的关键。
//!
//! .NET 版每条目存两个 string（全路径 + 显示名，UTF-16）+ char 池 + 7 个并行数组，
//! 200 万条目约 411MB。Rust 版的做法：
//!  - **目录驻留**：全路径拆成「目录 + 文件名」，目录串去重后只存一份
//!    （真实磁盘上目录数比文件数少一到两个数量级）。条目只记 4 字节 dir_id。
//!    这一项就砍掉了原版最大的一块——每条目一份全路径字符串。
//!  - **名字只存一份 UTF-8 原始字节**（不是 UTF-16，中文名直接省一半），拼进一个
//!    连续字节池；搜索时在池上做大小写折叠子串匹配，不再单独存小写池。
//!  - 拼音首字母池同理（纯 ASCII）。
//!  - 每条目定长数组：name_off 4 + init_off 4 + dir_id 4 + cat 1 + file_ref 8
//!    = 21 字节（偏移数组用 n+1 哨兵，长度由相邻偏移相减得到，不单存 len）。
//!  - 父条目下标只在建索引拼路径时用，冻结后丢弃（原版留在 store 里但 UI 从不读）。
//!
//! 落盘格式与内存布局完全一致（小端定长数组 + 字节池），所以加载 = mmap 文件后
//! 按 header 里的区段表直接切片，**零拷贝、零解析**：进程起来映射完就能搜，
//! 索引文件多大都不影响启动速度（页面由 OS 按需调入，冷数据不占 RSS）。

use std::collections::HashMap;
use std::io;
use std::path::Path;

use crate::category::Category;
use crate::pinyin;

/// 数据源产出的原始条目（对应 C# 版 RawEntry）。
/// full_path 为 None 表示 MFT 形态：靠 file_ref/parent_ref 在建索引时拼路径。
#[derive(Clone, Debug)]
pub struct RawEntry {
    pub name: String,
    pub full_path: Option<String>,
    pub category: Category,
    pub file_ref: u64,
    pub parent_ref: u64,
    pub is_dir: bool,
}

impl RawEntry {
    pub fn new(name: String, full_path: String, category: Category) -> RawEntry {
        RawEntry {
            name,
            full_path: Some(full_path),
            category,
            file_ref: 0,
            parent_ref: 0,
            is_dir: false,
        }
    }
}

// ---------------- 落盘格式 ----------------

pub(crate) const MAGIC: &[u8; 8] = b"SSKIDX01";
/// v2（v0.1.8）：尾部追加全拼池两区段（full_off, full_pool），旧
/// 区段下标不变；v1 缓存文件版本校验不过会自动重建（同原版口径）。
/// v3（v0.1.9）：缓存信任字段（对齐原版 IndexCache）——header
/// 偏移 20 存 built_unix（建表时刻，秒），尾部追加 scope 区段
/// （作用域串，如 "C:,D: · MFT 直读全盘"）；加载端据此判定
/// scope 是否还对得上、缓存是否超过 24h。v2 文件版本校验不过
/// 自动重建。
const VERSION: u32 = 3;
/// header 大小（字节），区段从此偏移开始，按 8 字节对齐排布。
const HEADER_SIZE: usize = 256;
/// 区段数：dirs_off, dirs_pool, name_off, name_pool, init_off, init_pool,
/// dir_ids, cats, refs, full_off, full_pool, scope_pool
const SECTIONS: usize = 12;

#[derive(Clone, Copy, Default)]
struct Section {
    off: u64,
    len: u64,
}

enum Backing {
    Owned(OwnedData),
    Mapped(MappedData),
}

struct OwnedData {
    dirs_off: Vec<u32>,
    dirs_pool: Vec<u8>,
    name_off: Vec<u32>,
    name_pool: Vec<u8>,
    init_off: Vec<u32>,
    init_pool: Vec<u8>,
    dir_ids: Vec<u32>,
    cats: Vec<u8>,
    refs: Vec<u64>,
    full_off: Vec<u32>,
    full_pool: Vec<u8>,
}

struct MappedData {
    mmap: memmap2::Mmap,
    sections: [Section; SECTIONS],
}

pub struct IndexStore {
    count: u32,
    dir_count: u32,
    backing: Backing,
}

fn align8(x: usize) -> usize {
    (x + 7) & !7
}

impl IndexStore {
    pub fn count(&self) -> usize {
        self.count as usize
    }

    pub fn dir_count(&self) -> usize {
        self.dir_count as usize
    }

    pub fn is_mapped(&self) -> bool {
        matches!(self.backing, Backing::Mapped(_))
    }

    /// 将当前存储（含 Mmap 态）深拷贝为独立的内存 OwnedData，从而立即解除对磁盘文件的映射锁定。
    pub fn to_owned_store(&self) -> IndexStore {
        IndexStore {
            count: self.count,
            dir_count: self.dir_count,
            backing: Backing::Owned(OwnedData {
                dirs_off: self.dirs_off().to_vec(),
                dirs_pool: self.dirs_pool().to_vec(),
                name_off: self.name_off().to_vec(),
                name_pool: self.name_pool().to_vec(),
                init_off: self.init_off().to_vec(),
                init_pool: self.init_pool().to_vec(),
                dir_ids: self.dir_ids().to_vec(),
                cats: self.cats().to_vec(),
                refs: self.refs().to_vec(),
                full_off: self.full_off().to_vec(),
                full_pool: self.full_pool().to_vec(),
            }),
        }
    }

    // ---- 区段切片（Owned 直接借 Vec；Mapped 按区段表切 mmap，u32/u64 用小端重组视图）----

    // ---- 区段切片（Owned 直接借 Vec；Mapped 按区段表切 mmap 重解释）----

    pub fn dirs_off(&self) -> &[u32] {
        match &self.backing {
            Backing::Owned(o) => &o.dirs_off,
            Backing::Mapped(m) => mapped_u32(m, 0),
        }
    }
    pub fn dirs_pool(&self) -> &[u8] {
        match &self.backing {
            Backing::Owned(o) => &o.dirs_pool,
            Backing::Mapped(m) => mapped_u8(m, 1),
        }
    }
    pub fn name_off(&self) -> &[u32] {
        match &self.backing {
            Backing::Owned(o) => &o.name_off,
            Backing::Mapped(m) => mapped_u32(m, 2),
        }
    }
    pub fn name_pool(&self) -> &[u8] {
        match &self.backing {
            Backing::Owned(o) => &o.name_pool,
            Backing::Mapped(m) => mapped_u8(m, 3),
        }
    }
    pub fn init_off(&self) -> &[u32] {
        match &self.backing {
            Backing::Owned(o) => &o.init_off,
            Backing::Mapped(m) => mapped_u32(m, 4),
        }
    }
    pub fn init_pool(&self) -> &[u8] {
        match &self.backing {
            Backing::Owned(o) => &o.init_pool,
            Backing::Mapped(m) => mapped_u8(m, 5),
        }
    }
    pub fn dir_ids(&self) -> &[u32] {
        match &self.backing {
            Backing::Owned(o) => &o.dir_ids,
            Backing::Mapped(m) => mapped_u32(m, 6),
        }
    }
    pub fn cats(&self) -> &[u8] {
        match &self.backing {
            Backing::Owned(o) => &o.cats,
            Backing::Mapped(m) => mapped_u8(m, 7),
        }
    }
    pub fn refs(&self) -> &[u64] {
        match &self.backing {
            Backing::Owned(o) => &o.refs,
            Backing::Mapped(m) => mapped_u64(m, 8),
        }
    }
    pub fn full_off(&self) -> &[u32] {
        match &self.backing {
            Backing::Owned(o) => &o.full_off,
            Backing::Mapped(m) => mapped_u32(m, 9),
        }
    }
    pub fn full_pool(&self) -> &[u8] {
        match &self.backing {
            Backing::Owned(o) => &o.full_pool,
            Backing::Mapped(m) => mapped_u8(m, 10),
        }
    }

    // ---- 条目访问（冷路径：命中后组装结果才碰）----

    pub fn name(&self, i: usize) -> &str {
        let off = self.name_off();
        let pool = self.name_pool();
        let (a, b) = (off[i] as usize, off[i + 1] as usize);
        // 构建时写入的都是合法 UTF-8 切片
        std::str::from_utf8(&pool[a..b]).unwrap_or("")
    }

    pub fn initials(&self, i: usize) -> &str {
        let off = self.init_off();
        let pool = self.init_pool();
        let (a, b) = (off[i] as usize, off[i + 1] as usize);
        std::str::from_utf8(&pool[a..b]).unwrap_or("")
    }

    /// 全拼串（v0.1.8 第三搜索通道用，如 "微信" → "weixin"）。
    pub fn full_pinyin(&self, i: usize) -> &str {
        let off = self.full_off();
        let pool = self.full_pool();
        let (a, b) = (off[i] as usize, off[i + 1] as usize);
        std::str::from_utf8(&pool[a..b]).unwrap_or("")
    }

    pub fn category(&self, i: usize) -> Category {
        Category::from_u8(self.cats()[i])
    }

    pub fn file_ref(&self, i: usize) -> u64 {
        self.refs()[i]
    }

    pub fn dir_path(&self, i: usize) -> &str {
        let d = self.dir_ids()[i] as usize;
        let off = self.dirs_off();
        let pool = self.dirs_pool();
        let (a, b) = (off[d] as usize, off[d + 1] as usize);
        std::str::from_utf8(&pool[a..b]).unwrap_or("")
    }

    /// 完整路径 = 目录 + '\\' + 名字（目录以 '\\' 结尾或为空时直接拼接）。
    pub fn full_path(&self, i: usize) -> String {
        let dir = self.dir_path(i);
        let name = self.name(i);
        if dir.is_empty() {
            name.to_string()
        } else if dir.ends_with('\\') || dir.ends_with('/') {
            format!("{dir}{name}")
        } else {
            format!("{dir}\\{name}")
        }
    }

    /// 索引实际占用字节数（全部数组+池；mmap 形态下是文件大小对应的数据量，
    /// RSS 只算被访问到的页，通常远小于这个数）。
    pub fn memory_bytes(&self) -> usize {
        let n = self.count();
        self.dirs_pool().len()
            + self.name_pool().len()
            + self.init_pool().len()
            + self.full_pool().len()
            + (self.dir_count() + 1) * 4
            + (n + 1) * 4 * 3
            + n * (4 + 1 + 8)
    }

    // ---- 落盘 / 加载 ----

    /// 把索引写成与内存布局一致的二进制文件（save 后可用 load_mmap 秒开）。
    /// 无信任字段的兼容入口（scope 空串、built 0），等价旧行为。
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        self.save_with_meta(path, "", 0)
    }

    /// 带缓存信任字段的保存（v0.1.9）：scope = 作用域串（模式 +
    /// 卷列表），built_unix = 建表时刻（Unix 秒）。加载端用
    /// read_cache_meta 读回判定可信度（见 IndexService 对齐口径：
    /// scope 对不上不用、超过 24h 先用后重扫）。
    pub fn save_with_meta(
        &self,
        path: &Path,
        scope: &str,
        built_unix: u64,
    ) -> io::Result<()> {
        use std::io::{Seek, Write};
        let sections_data: [&[u8]; SECTIONS] = [
            u32_bytes(self.dirs_off()),
            self.dirs_pool(),
            u32_bytes(self.name_off()),
            self.name_pool(),
            u32_bytes(self.init_off()),
            self.init_pool(),
            u32_bytes(self.dir_ids()),
            self.cats(),
            u64_bytes(self.refs()),
            u32_bytes(self.full_off()),
            self.full_pool(),
            scope.as_bytes(),
        ];
        let mut sections = [Section::default(); SECTIONS];
        let mut cursor = HEADER_SIZE;
        for (i, data) in sections_data.iter().enumerate() {
            cursor = align8(cursor);
            sections[i] = Section {
                off: cursor as u64,
                len: data.len() as u64,
            };
            cursor += data.len();
        }
        let mut header = vec![0u8; HEADER_SIZE];
        header[0..8].copy_from_slice(MAGIC);
        header[8..12].copy_from_slice(&VERSION.to_le_bytes());
        header[12..16].copy_from_slice(&self.count.to_le_bytes());
        header[16..20].copy_from_slice(&self.dir_count.to_le_bytes());
        header[20..28].copy_from_slice(&built_unix.to_le_bytes());
        for (i, s) in sections.iter().enumerate() {
            let base = 32 + i * 16;
            header[base..base + 8].copy_from_slice(&s.off.to_le_bytes());
            header[base + 8..base + 16].copy_from_slice(&s.len.to_le_bytes());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&header)?;
            for (i, data) in sections_data.iter().enumerate() {
                let pad = sections[i].off as usize - (HEADER_SIZE + sections_data[..i].iter().map(|d| d.len()).sum::<usize>() + total_pad(&sections, i));
                let _ = pad; // 对齐填充按区段 off 直接定位写
                let cur = f.stream_position()?;
                let want = sections[i].off;
                if cur < want {
                    f.write_all(&vec![0u8; (want - cur) as usize])?;
                }
                f.write_all(data)?;
            }
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// mmap 加载：校验 header 后直接映射，零拷贝零解析。
    pub fn load_mmap(path: &Path) -> io::Result<IndexStore> {
        let file = std::fs::File::open(path)?;
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        if mmap.len() < HEADER_SIZE || &mmap[0..8] != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "不是 ShunSeek 索引文件"));
        }
        let rd32 = |o: usize| u32::from_le_bytes(mmap[o..o + 4].try_into().unwrap());
        let rd64 = |o: usize| u64::from_le_bytes(mmap[o..o + 8].try_into().unwrap());
        if rd32(8) != VERSION {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "索引版本不匹配"));
        }
        let count = rd32(12);
        let dir_count = rd32(16);
        let mut sections = [Section::default(); SECTIONS];
        for i in 0..SECTIONS {
            let base = 32 + i * 16;
            let off = rd64(base);
            let len = rd64(base + 8);
            let end = off.checked_add(len).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "索引文件损坏（区段溢出）")
            })?;
            if end as usize > mmap.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "索引文件损坏（区段越界）"));
            }
            sections[i] = Section { off, len };
        }
        // 对齐与长度一致性校验，杜绝损坏缓存引发 UB / panic
        for idx in [0usize, 2, 4, 6, 9] {
            if sections[idx].off % 4 != 0 || sections[idx].len % 4 != 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "索引文件损坏（u32 对齐错误）"));
            }
        }
        if sections[8].off % 8 != 0 || sections[8].len % 8 != 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "索引文件损坏（u64 对齐错误）"));
        }
        let c = count as u64;
        let dc = dir_count as u64;
        if sections[0].len != (dc + 1) * 4
            || sections[2].len != (c + 1) * 4
            || sections[4].len != (c + 1) * 4
            || sections[6].len != c * 4
            || sections[7].len != c
            || sections[8].len != c * 8
            || sections[9].len != (c + 1) * 4
        {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "索引文件损坏（区段长度不匹配）"));
        }
        let store = IndexStore {
            count,
            dir_count,
            backing: Backing::Mapped(MappedData { mmap, sections }),
        };
        if store.dirs_off().last().copied().unwrap_or(0) as usize > store.dirs_pool().len()
            || store.name_off().last().copied().unwrap_or(0) as usize > store.name_pool().len()
            || store.init_off().last().copied().unwrap_or(0) as usize > store.init_pool().len()
            || store.full_off().last().copied().unwrap_or(0) as usize > store.full_pool().len()
        {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "索引文件损坏（偏移哨兵越界）"));
        }
        Ok(store)
    }

    /// 只读缓存信任字段（v0.1.9）：返回 (scope, built_unix)。
    /// 文件不存在 / 魔数版本不对 / 区段越界都返回 Err，调用方
    /// 按「无可信缓存」处理（不用或重建）。
    pub fn read_cache_meta(path: &Path) -> io::Result<(String, u64)> {
        let file = std::fs::File::open(path)?;
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        if mmap.len() < HEADER_SIZE || &mmap[0..8] != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "不是 ShunSeek 索引文件"));
        }
        let rd32 = |o: usize| u32::from_le_bytes(mmap[o..o + 4].try_into().unwrap());
        let rd64 = |o: usize| u64::from_le_bytes(mmap[o..o + 8].try_into().unwrap());
        if rd32(8) != VERSION {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "索引版本不匹配"));
        }
        let built_unix = rd64(20);
        let base = 32 + 11 * 16; // scope_pool 是第 12 区段（下标 11）
        let (off, len) = (rd64(base), rd64(base + 8));
        let end = off.checked_add(len).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "索引文件损坏（scope 溢出）")
        })?;
        if end as usize > mmap.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "索引文件损坏（scope 越界）"));
        }
        let scope = String::from_utf8_lossy(&mmap[off as usize..end as usize]).into_owned();
        Ok((scope, built_unix))
    }
}

fn total_pad(sections: &[Section; SECTIONS], upto: usize) -> usize {
    let mut pad = 0usize;
    let mut prev_end = HEADER_SIZE;
    for s in sections.iter().take(upto) {
        pad += s.off as usize - prev_end;
        prev_end = (s.off + s.len) as usize;
    }
    pad
}

fn mapped_u32(m: &MappedData, idx: usize) -> &[u32] {
    let s = m.sections[idx];
    let bytes = &m.mmap[s.off as usize..(s.off + s.len) as usize];
    // 文件由我们自己写、区段 8 字节对齐；小端机器上布局与内存一致。
    unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const u32, bytes.len() / 4) }
}
fn mapped_u64(m: &MappedData, idx: usize) -> &[u64] {
    let s = m.sections[idx];
    let bytes = &m.mmap[s.off as usize..(s.off + s.len) as usize];
    unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const u64, bytes.len() / 8) }
}
fn mapped_u8(m: &MappedData, idx: usize) -> &[u8] {
    let s = m.sections[idx];
    &m.mmap[s.off as usize..(s.off + s.len) as usize]
}

fn u32_bytes(v: &[u32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 4) }
}
fn u64_bytes(v: &[u64]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 8) }
}

// ---------------- 构建 ----------------

pub struct Builder {
    dirs_off: Vec<u32>,
    dirs_pool: Vec<u8>,
    dir_intern: HashMap<Box<str>, u32>,
    name_off: Vec<u32>,
    name_pool: Vec<u8>,
    init_off: Vec<u32>,
    init_pool: Vec<u8>,
    full_off: Vec<u32>,
    full_pool: Vec<u8>,
    dir_ids: Vec<u32>,
    cats: Vec<u8>,
    refs: Vec<u64>,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

impl Builder {
    pub fn new() -> Builder {
        Builder {
            dirs_off: vec![0],
            dirs_pool: Vec::new(),
            dir_intern: HashMap::new(),
            name_off: vec![0],
            name_pool: Vec::new(),
            init_off: vec![0],
            init_pool: Vec::new(),
            full_off: vec![0],
            full_pool: Vec::new(),
            dir_ids: Vec::new(),
            cats: Vec::new(),
            refs: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.cats.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cats.is_empty()
    }

    fn intern_dir(&mut self, dir: &str) -> u32 {
        if let Some(&id) = self.dir_intern.get(dir) {
            return id;
        }
        let id = (self.dirs_off.len() - 1) as u32;
        self.dirs_pool.extend_from_slice(dir.as_bytes());
        self.dirs_off.push(self.dirs_pool.len() as u32);
        self.dir_intern.insert(Box::from(dir), id);
        id
    }

    /// dir 是父目录（不含文件名）；name 是显示名。
    pub fn add(&mut self, name: &str, dir: &str, category: Category, file_ref: u64) {
        let dir_id = self.intern_dir(dir);
        self.name_pool.extend_from_slice(name.as_bytes());
        self.name_off.push(self.name_pool.len() as u32);
        pinyin::initials_into(name, &mut self.init_pool);
        self.init_off.push(self.init_pool.len() as u32);
        // 纯 ASCII 名字在 initials_pool 已完整保留，跳过全拼池写入可显著减小索引体积
        if !name.is_ascii() {
            pinyin::full_into(name, &mut self.full_pool);
        }
        self.full_off.push(self.full_pool.len() as u32);
        self.dir_ids.push(dir_id);
        self.cats.push(category as u8);
        self.refs.push(file_ref);
    }

    pub fn freeze(mut self) -> IndexStore {
        self.dirs_off.shrink_to_fit();
        self.dirs_pool.shrink_to_fit();
        self.name_off.shrink_to_fit();
        self.name_pool.shrink_to_fit();
        self.init_off.shrink_to_fit();
        self.init_pool.shrink_to_fit();
        self.full_off.shrink_to_fit();
        self.full_pool.shrink_to_fit();
        self.dir_ids.shrink_to_fit();
        self.cats.shrink_to_fit();
        self.refs.shrink_to_fit();
        let count = self.cats.len() as u32;
        let dir_count = (self.dirs_off.len() - 1) as u32;
        IndexStore {
            count,
            dir_count,
            backing: Backing::Owned(OwnedData {
                dirs_off: self.dirs_off,
                dirs_pool: self.dirs_pool,
                name_off: self.name_off,
                name_pool: self.name_pool,
                init_off: self.init_off,
                init_pool: self.init_pool,
                full_off: self.full_off,
                full_pool: self.full_pool,
                dir_ids: self.dir_ids,
                cats: self.cats,
                refs: self.refs,
            }),
        }
    }
}

/// 把全路径拆成 (目录, 文件名)。没有分隔符时目录为空。
pub fn split_path(full: &str) -> (&str, &str) {
    match full.rfind(['\\', '/']) {
        Some(p) => (&full[..p], &full[p + 1..]),
        None => ("", full),
    }
}
