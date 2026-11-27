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

const REBUILD_DEAD_THRESHOLD: usize = 20_000;
const REBUILD_DIR_DEAD_THRESHOLD: usize = 32;

#[derive(Clone, Default)]
pub struct Tombstones {
    pub dead_refs: HashSet<u64>,
    pub dead_paths: HashSet<String>,
    pub dead_dir_prefixes: Vec<String>,
}

impl Tombstones {
    pub fn is_empty(&self) -> bool {
        self.dead_refs.is_empty()
            && self.dead_paths.is_empty()
            && self.dead_dir_prefixes.is_empty()
    }

    pub fn dead_count(&self) -> usize {
        self.dead_refs.len() + self.dead_paths.len()
    }

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

#[derive(Default)]
pub struct DeltaIndex {
    add_by_ref: HashMap<u64, usize>,
    add_by_path: HashMap<String, usize>,
    add_rows: Vec<RawEntry>,
    dead_refs: HashSet<u64>,
    dead_paths: HashSet<String>,
    dead_dir_prefixes: Vec<String>,
    total_changes: usize,
    force_rebuild: bool,
}

impl DeltaIndex {
    pub fn new() -> DeltaIndex {
        DeltaIndex::default()
    }

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

    pub fn requires_rebuild(&self) -> bool {
        self.force_rebuild
            || self.dead_refs.len() + self.dead_paths.len() > REBUILD_DEAD_THRESHOLD
            || self.dead_dir_prefixes.len() > REBUILD_DIR_DEAD_THRESHOLD
    }

    pub fn request_rebuild(&mut self) {
        self.force_rebuild = true;
    }

    fn clear_row_indices(&mut self, row: usize) {
        let old = &self.add_rows[row];
        if old.file_ref != 0 && self.add_by_ref.get(&old.file_ref) == Some(&row) {
            self.add_by_ref.remove(&old.file_ref);
        }
        if let Some(p) = &old.full_path {
            let k = p.to_lowercase();
            if !k.is_empty() && self.add_by_path.get(&k) == Some(&row) {
                self.add_by_path.remove(&k);
            }
        }
    }

    /// 新增/更新一条（同引用号或同路径已有增量行 → 原地更新该行）。
    /// 同时给主索引加墓碑，确保主索引里的旧版本被屏蔽、不会出现双份结果。
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

        // 屏蔽主索引中可能存在的同 ref / 同路径旧条目，防止合并搜索出现重复
        if entry.file_ref != 0 {
            self.dead_refs.insert(entry.file_ref);
        }
        if !path_key.is_empty() {
            self.dead_paths.insert(path_key.clone());
        }

        match existing {
            Some(row) => {
                self.clear_row_indices(row);
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

    /// 删除一条：增量层里有该行就清空它并清理双向映射；同时给主索引侧加 ref/路径墓碑。
    pub fn remove(&mut self, file_ref: u64, path: &str) {
        let path_key = path.to_lowercase();
        let mut rows_to_clear = Vec::new();
        if file_ref != 0 {
            if let Some(r) = self.add_by_ref.remove(&file_ref) {
                rows_to_clear.push(r);
            }
        }
        if !path_key.is_empty() {
            if let Some(r) = self.add_by_path.remove(&path_key) {
                if !rows_to_clear.contains(&r) {
                    rows_to_clear.push(r);
                }
            }
        }
        for r in rows_to_clear {
            self.clear_row_indices(r);
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

    /// 删除整个目录：主索引侧加目录前缀墓碑 + 目录本身路径墓碑；
    /// 增量层里路径在该目录下（或目录本身）的行一并清空。
    pub fn remove_directory(&mut self, dir_path: &str, _is_dir_event: bool) {
        let mut prefix = dir_path.to_lowercase();
        if !prefix.ends_with('\\') {
            prefix.push('\\');
        }
        if !self.dead_dir_prefixes.contains(&prefix) {
            self.dead_dir_prefixes.push(prefix.clone());
        }
        let bare = prefix.trim_end_matches('\\').to_string();
        if !bare.is_empty() {
            self.dead_paths.insert(bare.clone());
        }
        let keys: Vec<String> = self
            .add_by_path
            .keys()
            .filter(|k| k.starts_with(&prefix) || **k == bare)
            .cloned()
            .collect();
        for k in keys {
            if let Some(r) = self.add_by_path.remove(&k) {
                self.clear_row_indices(r);
                self.add_rows[r].name.clear();
                self.add_rows[r].full_path = None;
                self.add_rows[r].file_ref = 0;
            }
        }
        self.total_changes += 1;
    }

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
        let (store, tombs) = d.freeze();
        assert_eq!(store.count(), 1);
        assert_eq!(store.name(0), "新文件.txt");
        assert!(tombs.dead_refs.contains(&42));
        assert!(tombs.dead_paths.contains("c:\\d\\新文件.txt"));
    }

    #[test]
    fn upsert_rename_clears_old_path_mapping() {
        let mut d = DeltaIndex::new();
        d.upsert(entry("a.txt", "C:\\d\\a.txt", 7));
        d.upsert(entry("b.txt", "C:\\d\\b.txt", 7));
        // 在原路径建新文件不能覆盖掉 b.txt
        d.upsert(entry("a.txt", "C:\\d\\a.txt", 8));
        let (store, _) = d.freeze();
        assert_eq!(store.count(), 2);
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
    fn remove_directory_tombstones_dir_itself_and_prefix() {
        let mut d = DeltaIndex::new();
        d.remove_directory("C:\\Gone", true);
        let (_, tombs) = d.freeze();
        assert_eq!(tombs.dead_dir_prefixes, vec!["c:\\gone\\".to_string()]);
        assert!(tombs.dead_paths.contains("c:\\gone"));
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
