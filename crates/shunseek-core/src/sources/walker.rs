//! 兜底遍历数据源（对应 C# 版 DirectoryWalker）：非 NTFS 卷、无管理员权限的
//! 便携模式、或只需索引少数目录时用。迭代式显式栈，不递归（深目录不爆栈）；
//! 单个目录无权限/被占用就跳过；跳过目录联接/符号链接防环。
//! 速度预期与 C# 版注释一致：这是「能用」的兜底，不是主力。

use std::path::PathBuf;

use crate::category::{classify_by_extension, Category};
use crate::store::RawEntry;

/// 噪音目录名（v0.1.8，对齐原版 DirectoryWalker 的跳过名单，小写）：
/// 名字命中的子目录整棵树不进索引、目录本身也不产出条目。
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

pub fn walk(roots: &[PathBuf], include_files: bool) -> Vec<RawEntry> {
    walk_with_excludes(roots, include_files, &[])
}

/// 带排除的遍历（v0.1.8）：exclude_prefixes 是完整路径前缀（大小写
/// 不敏感，如 …\Tencent\xwechat\radium），命中的文件/目录整棵跳过；
/// 噪音目录名恒生效。roots 本身不查（调用方给的根就是想要的范围）。
pub fn walk_with_excludes(
    roots: &[PathBuf],
    include_files: bool,
    exclude_prefixes: &[String],
) -> Vec<RawEntry> {
    let prefixes: Vec<String> =
        exclude_prefixes.iter().map(|p| p.to_lowercase()).collect();
    let excluded = |path: &std::path::Path| -> bool {
        if prefixes.is_empty() {
            return false;
        }
        let s = path.to_string_lossy().to_lowercase();
        prefixes.iter().any(|p| s.starts_with(p.as_str()))
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
            Err(_) => continue, // 无权限/被占用：跳过这个目录
        };
        for entry in read.flatten() {
            let path = entry.path();
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue, // 枚举间隙文件被删/改名
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if excluded(&path) {
                continue;
            }
            if ft.is_dir() {
                if is_noise_dir(&name) {
                    continue; // 噪音目录：不进栈、不产出条目
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
            } else if ft.is_symlink() {
                // 符号链接不跟进（防环）；链接本身按普通文件收一条
                if include_files {
                    out.push(RawEntry {
                        category: classify_by_extension(&name),
                        name,
                        full_path: Some(path.to_string_lossy().into_owned()),
                        file_ref: 0,
                        parent_ref: 0,
                        is_dir: false,
                    });
                }
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
