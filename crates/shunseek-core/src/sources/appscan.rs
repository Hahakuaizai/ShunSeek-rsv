//! 「软件」类数据源（对应 C# 版 AppScanner）：
//!  1) 开始菜单（当前用户+全部用户）与桌面上的 .lnk → Shortcut（保留 .lnk 后缀使 full_path 真实有效）；
//!     指向程序的 .exe → App。
//!  2) 注册表 Uninstall 键（HKLM 64/32 位视图 + HKCU）：DisplayIcon 指向的真实 .exe → App，
//!     InstallLocation → InstallDir。SystemComponent=1 的系统组件跳过。

use std::collections::HashSet;
use std::path::PathBuf;

use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};
use winreg::RegKey;

use crate::category::Category;
use crate::sources::walker;
use crate::store::RawEntry;

fn special_dir(env_key: &str) -> Option<PathBuf> {
    std::env::var_os(env_key).map(PathBuf::from)
}

/// 开始菜单 + 桌面的快捷方式（和散落的 .exe 入口）。
pub fn scan_shortcuts() -> Vec<RawEntry> {
    let mut dirs = Vec::new();
    if let Some(appdata) = special_dir("APPDATA") {
        dirs.push(appdata.join(r"Microsoft\Windows\Start Menu"));
    }
    if let Some(userprofile) = special_dir("USERPROFILE") {
        dirs.push(userprofile.join("Desktop"));
    }
    if let Some(program_data) = special_dir("ProgramData") {
        dirs.push(program_data.join(r"Microsoft\Windows\Start Menu"));
    }
    if let Some(public) = special_dir("PUBLIC") {
        dirs.push(public.join("Desktop"));
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for e in walker::walk(&dirs, true) {
        if e.is_dir {
            continue;
        }
        let lower_path = e
            .full_path
            .as_deref()
            .unwrap_or("")
            .to_lowercase();
        if !lower_path.is_empty() && !seen.insert(lower_path) {
            continue;
        }
        let lower = e.name.to_lowercase();
        if lower.ends_with(".lnk") {
            // 保留 .lnk 原文件名，确保 store.full_path() 还原真实磁盘路径；
            // 友好显示名由 UI 层 NameService 负责去掉 .lnk 后缀。
            out.push(RawEntry {
                category: Category::Shortcut,
                ..e
            });
        } else if lower.ends_with(".exe") {
            out.push(RawEntry {
                category: Category::App,
                ..e
            });
        }
    }
    out
}

fn normalize_path(path: &str) -> Option<String> {
    let p = path
        .trim()
        .trim_matches('"')
        .trim_end_matches(['\\', '/']);
    if p.is_empty() {
        None
    } else {
        Some(p.to_string())
    }
}

fn resolve_executable(display_icon: &str) -> Option<String> {
    let mut p = normalize_path(display_icon)?;
    if let Some(comma) = p.rfind(',') {
        if comma > 2 {
            p = p[..comma].trim().trim_matches('"').to_string();
        }
    }
    if !p.to_lowercase().ends_with(".exe") {
        return None;
    }
    if PathBuf::from(&p).is_file() {
        Some(p)
    } else {
        None
    }
}

/// 注册表 Uninstall 枚举已安装软件 + 安装目录。
pub fn scan_installed_apps() -> Vec<RawEntry> {
    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    const UNINSTALL_WOW: &str = r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall";
    let mut out = Vec::new();
    let mut seen_names: HashSet<String> = HashSet::new();
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    for (hive, sub) in [
        (&hklm, UNINSTALL),
        (&hklm, UNINSTALL_WOW),
        (&hkcu, UNINSTALL),
    ] {
        let root = match hive.open_subkey_with_flags(sub, KEY_READ) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for key_name in root.enum_keys().flatten() {
            let key = match root.open_subkey_with_flags(&key_name, KEY_READ) {
                Ok(k) => k,
                Err(_) => continue,
            };
            let display: String = match key.get_value("DisplayName") {
                Ok(v) => v,
                Err(_) => continue,
            };
            let display_trimmed = display.trim();
            if display_trimmed.is_empty() {
                continue;
            }
            let system_component: u32 = key.get_value("SystemComponent").unwrap_or(0);
            if system_component == 1 {
                continue;
            }
            if !seen_names.insert(display_trimmed.to_lowercase()) {
                continue;
            }

            let exe = key
                .get_value::<String, _>("DisplayIcon")
                .ok()
                .and_then(|s| resolve_executable(&s));
            if let Some(exe_path) = exe {
                let exe_name = exe_path
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or(display_trimmed)
                    .to_string();
                out.push(RawEntry {
                    name: exe_name,
                    full_path: Some(exe_path),
                    category: Category::App,
                    file_ref: 0,
                    parent_ref: 0,
                    is_dir: false,
                });
            }

            let location = key
                .get_value::<String, _>("InstallLocation")
                .ok()
                .and_then(|s| normalize_path(&s));
            if let Some(loc) = location {
                if PathBuf::from(&loc).is_dir() {
                    let folder_name = loc
                        .rsplit(['\\', '/'])
                        .next()
                        .unwrap_or("")
                        .to_string();
                    if !folder_name.is_empty() {
                        out.push(RawEntry {
                            name: folder_name,
                            full_path: Some(loc),
                            category: Category::InstallDir,
                            file_ref: 0,
                            parent_ref: 0,
                            is_dir: true,
                        });
                    }
                }
            }
        }
    }
    out
}

pub fn load_all() -> Vec<RawEntry> {
    let mut out = scan_shortcuts();
    out.extend(scan_installed_apps());
    out
}
