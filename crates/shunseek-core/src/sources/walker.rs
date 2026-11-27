//! 兜底遍历数据源（对应 C# 版 DirectoryWalker）：非 NTFS 卷、无管理员权限的
//! 便携模式、或只需索引少数目录时用。迭代式显式栈，不递归（深目录不爆栈）；
//! 单个目录无权限/被占用就跳过；跳过目录联接/符号链接防环。

use std::path::PathBuf;

use crate::category::{classify_by_extension, Category};
use crate::store::RawEntry;

pub const NOISE_DIR_NAMES: &[&str] = &[
    "cache",
    "caches",
    "gpucache",
    "code cache",
    "cachestorage",
    "cache data",
    "crashpad",
    "crashdumps",
    "logs",
    "log",
    "temp",
    "tmp",
    "node_modules",
    ".git",
    ".svn",
    ".vs",
    "__pycache__",
    ".pytest_cache",
    "shadercache",
    "scriptcache",
    "webcache",
    "backup",
    "backups.tmp",
];

fn is_noise_dir(name: &str) -> bool {
    let lower = name.to_lowercase();
    NOISE_DIR_NAMES.contains(&lower.as_str())
}

fn is_reparse_point(entry: &std::fs::DirEntry) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        if let Ok(meta) = entry.metadata() {
            return (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0;
        }
    }
    #[cfg(not(windows))]
    let _ = entry;
    false
}

pub fn walk(roots: &[PathBuf], include_files: bool) -> Vec<RawEntry> {
    walk_with_excludes(roots, include_files, &[])
}

pub fn walk_with_excludes(
    roots: &[PathBuf],
    include_files: bool,
    exclude_prefixes: &[String],
) -> Vec<RawEntry> {
    let prefixes: Vec<String> = exclude_prefixes
        .iter()
        .map(|p| {
            p.to_lowercase()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_string()
        })
        .filter(|p| !p.is_empty())
        .collect();
    let excluded = |path: &std::path::Path| -> bool {
        if prefixes.is_empty() {
            return false;
        }
        let s = path.to_string_lossy().to_lowercase().replace('/', "\\");
        prefixes.iter().any(|p| {
            s == *p || (s.starts_with(p.as_str()) && s.as_bytes().get(p.len()) == Some(&b'\\'))
        })
    };
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = roots
        .iter()
        .filter(|r| r.is_dir())
        .cloned()
        .collect();
    while let Some(dir) = stack.pop() {
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in read.flatten() {
            let path = entry.path();
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if excluded(&path) {
                continue;
            }
            if ft.is_symlink() || is_reparse_point(&entry) {
                if include_files && !ft.is_dir() {
                    out.push(RawEntry {
                        category: classify_by_extension(&name),
                        name,
                        full_path: Some(path.to_string_lossy().into_owned()),
                        file_ref: 0,
                        parent_ref: 0,
                        is_dir: false,
                    });
                }
                continue;
            }
            if ft.is_dir() {
                if is_noise_dir(&name) {
                    continue;
                }
                stack.push(path.clone());
                out.push(RawEntry {
                    name,
                    full_path: Some(path.to_string_lossy().into_owned()),
                    category: Category::Folder,
                    file_ref: 0,
                    parent_ref: 0,
                    is_dir: true,
                });
            } else if include_files {
                out.push(RawEntry {
                    category: classify_by_extension(&name),
                    name,
                    full_path: Some(path.to_string_lossy().into_owned()),
                    file_ref: 0,
                    parent_ref: 0,
                    is_dir: false,
                });
            }
        }
    }
    out
}
