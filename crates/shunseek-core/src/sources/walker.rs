//! 兜底遍历数据源（对应 C# 版 DirectoryWalker）：非 NTFS 卷、无管理员权限的
//! 便携模式、或只需索引少数目录时用。迭代式显式栈，不递归（深目录不爆栈）；
//! 单个目录无权限/被占用就跳过；跳过目录联接/符号链接防环。
//! 速度预期与 C# 版注释一致：这是「能用」的兜底，不是主力。

use std::path::PathBuf;

use crate::category::{classify_by_extension, Category};
use crate::store::RawEntry;

pub fn walk(roots: &[PathBuf], include_files: bool) -> Vec<RawEntry> {
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
            if ft.is_dir() {
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
