//! 把若干数据源汇总成 IndexStore（对应 C# 版 IndexBuilder）：
//!  1) 先把全部 RawEntry 攒成表，建 FileRef → 下标 的映射；
//!  2) 给没有 FullPath 的条目（来自 MFT）沿 ParentFileRef 链拼出完整路径；
//!  3) 分类兜底：数据源给了 Other 且不是文件夹时，按扩展名再分一次；
//!     是文件夹且 Other 时记为 Folder；
//!  4) 把全路径拆成「目录（驻留）+ 文件名」灌进 Builder 并 Freeze。

use std::collections::HashMap;

use crate::category::{classify_by_extension, Category};
use crate::store::{split_path, Builder, IndexStore, RawEntry};

/// volume_roots：MFT 条目拼路径用的卷根，如 'C' → "C:\\"。
pub fn build_index(
    entries: Vec<RawEntry>,
    volume_roots: Option<&HashMap<char, String>>,
) -> IndexStore {
    // FileRef → 下标（只有 MFT 条目带引用号）
    let mut ref_to_index: HashMap<u64, usize> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        if e.file_ref != 0 {
            ref_to_index.insert(e.file_ref, i);
        }
    }

    // .lnk 去重（v0.1.8，对齐原版构建期去重）：同一快捷方式既被
    // MFT 扫到又被 appscan（开始菜单/桌面）扫到时，只留带完整路径
    // 的 appscan 条目（它带正确的分类与图标路径），MFT 的跳过。
    let mut lnk_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &entries {
        if e.full_path.is_some() && e.name.to_lowercase().ends_with(".lnk") {
            if let Some(p) = &e.full_path {
                lnk_paths.insert(p.to_lowercase());
            }
        }
    }

    let mut builder = Builder::new();
    for e in &entries {
        let full_path = match &e.full_path {
            Some(p) => p.clone(),
            None => resolve_mft_path(e, &entries, &ref_to_index, volume_roots),
        };
        if e.full_path.is_none()
            && e.name.to_lowercase().ends_with(".lnk")
            && lnk_paths.contains(&full_path.to_lowercase())
        {
            continue;
        }
        let mut category = e.category;
        if category == Category::Other && !e.is_dir {
            category = classify_by_extension(&e.name);
        }
        if e.is_dir && category == Category::Other {
            category = Category::Folder;
        }
        // 目录部分驻留；文件名以条目名为准（MFT 拼出的路径尾段就是它）。
        let (dir, _tail) = split_path(&full_path);
        let dir = if e.full_path.is_none() && full_path.is_empty() {
            ""
        } else {
            dir
        };
        builder.add(&e.name, dir, category, e.file_ref);
    }
    builder.freeze()
}

/// 沿父引用链拼路径（与 C# 版 ResolveMftPath 相同：最多 64 层防环，
/// 父链断裂时停在已收集部分，最后冠上卷根）。
fn resolve_mft_path(
    entry: &RawEntry,
    list: &[RawEntry],
    ref_to_index: &HashMap<u64, usize>,
    volume_roots: Option<&HashMap<char, String>>,
) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let mut cur = entry;
    for _ in 0..64 {
        parts.push(&cur.name);
        if cur.parent_ref == 0 || cur.parent_ref == cur.file_ref {
            break;
        }
        match ref_to_index.get(&cur.parent_ref) {
            Some(&p) => cur = &list[p],
            None => break,
        }
    }
    parts.reverse();
    let joined = parts.join("\\");
    if let Some(roots) = volume_roots {
        if let Some(root) = roots.values().next() {
            return format!("{root}{joined}");
        }
    }
    joined
}
