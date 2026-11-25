//! 增量索引（v0.1.8，对齐原版 DeltaIndex.cs + SearchIndex.cs 的墓碑
//! 机制）：主索引不动，变更先攒在这里——新增/修改进一张小索引
//! （add），删除/移动走墓碑（tombstones）屏蔽主索引里的旧条目。
//! 搜索时主引擎带墓碑过滤、增量引擎独立搜，两路合并排序。
//! 全量重建触发口径同原版：墓碑 ref/路径 > 20000 或目录前缀墓碑
//! > 32 或显式请求。

use std::collections::{HashMap, HashSet};

use crate::builder::build_index;
use crate::category::Category;
use crate::store::{IndexStore, RawEntry};

/// 重建触发阈值（原版 SearchIndex.RebuildDeadThreshold /
/// RebuildDirDeadThreshold）。
const REBUILD_DEAD_THRESHOLD: usize = 20_000;
const REBUILD_DIR_DEAD_THRESHOLD: usize = 32;

/// 墓碑集合：标记主索引中已失效的条目。搜索扫描命中时逐条过滤。
#[derive(Clone, Default)]
pub struct Tombstones {
    /// 按 NTFS 文件引用号杀（卷内唯一；引用号会复用，配合路径墓碑兜底）。
    pub dead_refs: HashSet<u64>,
    /// 按完整路径杀（小写）。
    pub dead_paths: HashSet<String>,
    /// 按目录前缀杀（小写、带尾随 '\'）：整棵子树失效。
    pub dead_dir_prefixes: Vec<String>,
}

impl Tombstones {
    pub fn is_empty(&self) -> bool {
        self.dead_refs.is_empty()
            && self.dead_paths.is_empty()
            && self.dead_dir_prefixes.is_empty()
    }

    /// 墓碑总量（不含目录前缀，原版 TotalChanges 的死条目部分）。
    pub fn dead_count(&self) -> usize {
        self.dead_refs.len() + self.dead_paths.len()
    }

    /// 索引 i 在主索引里是否已被墓碑杀掉。
    pub fn is_dead(&self, store: &IndexStore, i: usize) -> bool {
        if self.is_empty() {
            return false;
        }
        let r = store.file_ref(i);
        if r != 0 && self.dead_refs.contains(&r) {
            return true;
        }
        if !self.dead_paths.is_empty() || !self.dead_dir_prefixes.is_empty() {
            let p = store.full_path(i).to_lowercase();
            if self.dead_paths.contains(&p) {
                return true;
            }
            for pre in &self.dead_dir_prefixes {
                if p.starts_with(pre.as_str()) {
                    return true;
                }
            }
        }
        false
    }
}

/// 增量层：新增条目（按引用号/路径各一份映射，值 = add 索引行号）
/// + 墓碑。app 侧由 flush 线程独占持有，周期性 freeze 成
/// (IndexStore, Tombstones) 快照给搜索线程用。
#[derive(Default)]
pub struct DeltaIndex {
    add_by_ref: HashMap<u64, usize>,
    add_by_path: HashMap<String, usize>,
    add_rows: Vec<RawEntry>,
    dead_refs: HashSet<u64>,
    dead_paths: HashSet<String>,
    dead_dir_prefixes: Vec<String>,
    /// 累计变更计数（页脚「增量 M」的口径基础：当前增量层规模）。
    total_changes: usize,
    force_rebuild: bool,
}

impl DeltaIndex {
    pub fn new() -> DeltaIndex {
        DeltaIndex::default()
    }

    /// 增量层条目数（页脚「增量 M」显示值）。
    pub fn count(&self) -> usize {
        self.add_rows.len()
    }

    pub fn dead_count(&self) -> usize {
        self.dead_refs.len() + self.dead_paths.len()
    }

    pub fn total_changes(&self) -> usize {
        self.total_changes
    }

    pub fn is_empty(&self) -> bool {
        self.add_rows.is_empty()
            && self.dead_refs.is_empty()
            && self.dead_paths.is_empty()
            && self.dead_dir_prefixes.is_empty()
    }

    /// 是否已到必须全量重建的程度（原版 NeedsRebuild 口径）。
    pub fn requires_rebuild(&self) -> bool {
        self.force_rebuild
            || self.dead_refs.len() + self.dead_paths.len() > REBUILD_DEAD_THRESHOLD
            || self.dead_dir_prefixes.len() > REBUILD_DIR_DEAD_THRESHOLD
    }

    pub fn request_rebuild(&mut self) {
        self.force_rebuild = true;
    }

    /// 新增/更新一条（同引用号或同路径已有增量行 → 原地更新该行）。
    /// entry.category 若为 Other 则按扩展名/目录分类兜底（与
    /// store::Builder 对 RawEntry 的处理一致，freeze 时再走 Builder）。
    pub fn upsert(&mut self, mut entry: RawEntry) {
        if entry.category == Category::Other {
            entry.category = if entry.is_dir {
                Category::Folder
            } else {
                crate::category::classify_by_extension(&entry.name)
            };
        }
        let path_key = entry
            .full_path
            .as_deref()
            .unwrap_or("")
            .to_lowercase();
        let existing = if entry.file_ref != 0 {
            self.add_by_ref.get(&entry.file_ref).copied()
        } else {
            None
        }
        .or_else(|| {
            if path_key.is_empty() {
                None
            } else {
                self.add_by_path.get(&path_key).copied()
            }
        });
        // 复活：同一条目若此前被墓碑杀过（主索引侧），撤销对应墓碑
        if entry.file_ref != 0 {
            self.dead_refs.remove(&entry.file_ref);
        }
        if !path_key.is_empty() {
            self.dead_paths.remove(&path_key);
        }
        match existing {
            Some(row) => {
                self.add_rows[row] = entry.clone();
                if entry.file_ref != 0 {
                    self.add_by_ref.insert(entry.file_ref, row);
                }
                if !path_key.is_empty() {
                    self.add_by_path.insert(path_key, row);
                }
            }
            None => {
                let row = self.add_rows.len();
                if entry.file_ref != 0 {
                    self.add_by_ref.insert(entry.file_ref, row);
                }
                if !path_key.is_empty() {
                    self.add_by_path.insert(path_key, row);
                }
                self.add_rows.push(entry);
            }
        }
        self.total_changes += 1;
    }

    /// 删除一条：增量层里有该行就清空它（freeze 时空名行被跳过，
    /// 等效移除）；同时给主索引侧加 ref/路径墓碑杀旧条目。
    pub fn remove(&mut self, file_ref: u64, path: &str) {
        let path_key = path.to_lowercase();
        let mut row = None;
        if file_ref != 0 {
            row = self.add_by_ref.remove(&file_ref);
        }
        if row.is_none() && !path_key.is_empty() {
            row = self.add_by_path.remove(&path_key);
        }
        if let Some(r) = row {
            // 行内容清空（freeze 时空名行被跳过，等效移除）
            self.add_rows[r] = RawEntry {
                name: String::new(),
                full_path: None,
                category: Category::Other,
                file_ref: 0,
                parent_ref: 0,
                is_dir: false,
            };
        }
        if file_ref != 0 {
            self.dead_refs.insert(file_ref);
        }
        if !path_key.is_empty() {
            self.dead_paths.insert(path_key);
        }
        self.total_changes += 1;
    }

    /// 删除整个目录：主索引侧加目录前缀墓碑（整树失效）；增量层里
    /// 路径在该目录下的行一并清空。
    pub fn remove_directory(&mut self, dir_path: &str, _is_dir_event: bool) {
        let mut prefix = dir_path.to_lowercase();
        if !prefix.ends_with('\\') {
            prefix.push('\\');
        }
        if !self.dead_dir_prefixes.contains(&prefix) {
            self.dead_dir_prefixes.push(prefix.clone());
        }
        let bare = prefix.trim_end_matches('\\').to_string();
        let keys: Vec<String> = self
            .add_by_path
            .keys()
            .filter(|k| k.starts_with(&prefix) || **k == bare)
            .cloned()
            .collect();
        for k in keys {
            if let Some(r) = self.add_by_path.remove(&k) {
                self.add_rows[r].name.clear();
                self.add_rows[r].full_path = None;
            }
        }
        self.total_changes += 1;
    }

    /// 冻结成搜索可用的快照：增量小索引 + 墓碑集。
    pub fn freeze(&self) -> (IndexStore, Tombstones) {
        let rows: Vec<RawEntry> = self
            .add_rows
            .iter()
            .filter(|r| !r.name.is_empty())
            .cloned()
            .collect();
        let store = build_index(rows, None);
        let tombs = Tombstones {
            dead_refs: self.dead_refs.clone(),
            dead_paths: self.dead_paths.clone(),
            dead_dir_prefixes: self.dead_dir_prefixes.clone(),
        };
        (store, tombs)
    }

    pub fn clear(&mut self) {
        *self = DeltaIndex::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, path: &str, r: u64) -> RawEntry {
        RawEntry {
            name: name.into(),
            full_path: Some(path.into()),
            category: Category::Other,
            file_ref: r,
            parent_ref: 0,
            is_dir: false,
        }
    }

    #[test]
    fn upsert_freeze_searchable() {
        let mut d = DeltaIndex::new();
        d.upsert(entry("新文件.txt", "C:\\d\\新文件.txt", 42));
        assert_eq!(d.count(), 1);
        let (store, _) = d.freeze();
        assert_eq!(store.count(), 1);
        assert_eq!(store.name(0), "新文件.txt");
    }

    #[test]
    fn remove_kills_add_row_and_tombstones_base() {
        let mut d = DeltaIndex::new();
        d.upsert(entry("a.txt", "C:\\d\\a.txt", 7));
        d.remove(7, "C:\\d\\a.txt");
        let (store, tombs) = d.freeze();
        assert_eq!(store.count(), 0);
        assert!(tombs.dead_refs.contains(&7));
        assert!(tombs.dead_paths.contains("c:\\d\\a.txt"));
    }

    #[test]
    fn remove_directory_prefix_tombstone() {
        let mut d = DeltaIndex::new();
        d.remove_directory("C:\\Gone", true);
        let (_, tombs) = d.freeze();
        assert_eq!(tombs.dead_dir_prefixes, vec!["c:\\gone\\".to_string()]);
    }

    #[test]
    fn rebuild_thresholds() {
        let mut d = DeltaIndex::new();
        assert!(!d.requires_rebuild());
        d.request_rebuild();
        assert!(d.requires_rebuild());
        let mut d2 = DeltaIndex::new();
        for i in 0..33 {
            d2.remove_directory(&format!("C:\\d{i}"), true);
        }
        assert!(d2.requires_rebuild());
    }
}
