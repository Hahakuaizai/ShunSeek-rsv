//! 「软件」类数据源（对应 C# 版 AppScanner）：
//!  1) 开始菜单（当前用户+全部用户）与桌面上的 .lnk → Shortcut（显示名去 .lnk 后缀）；
//!     指向程序的 .exe → App。
//!  2) 注册表 Uninstall 键（HKLM 64/32 位视图 + HKCU）：DisplayName → App，
//!     InstallLocation → InstallDir。SystemComponent=1 的系统组件跳过。
//! UWP/商店应用（shell:AppsFolder）与 .lnk 目标解析仍是接入点 TODO，与 C# 版相同，
//! 不假装已实现（见 C# 版 AppScanner 末尾注释）。

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
    // 当前用户开始菜单 / 桌面
    if let Some(appdata) = special_dir("APPDATA") {
        dirs.push(appdata.join(r"Microsoft\Windows\Start Menu"));
    }
    if let Some(userprofile) = special_dir("USERPROFILE") {
        dirs.push(userprofile.join("Desktop"));
    }
    // 全部用户开始菜单 / 公共桌面
    if let Some(program_data) = special_dir("ProgramData") {
        dirs.push(program_data.join(r"Microsoft\Windows\Start Menu"));
    }
    if let Some(public) = special_dir("PUBLIC") {
        dirs.push(public.join("Desktop"));
    }
    let mut out = Vec::new();
    for e in walker::walk(&dirs, true) {
        if e.is_dir {
            continue;
        }
        let lower = e.name.to_lowercase();
        if lower.ends_with(".lnk") {
            let display = e.name[..e.name.len() - 4].to_string();
            out.push(RawEntry {
                name: display,
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

/// 注册表 Uninstall 枚举已安装软件 + 安装目录。
pub fn scan_installed_apps() -> Vec<RawEntry> {
    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    const UNINSTALL_WOW: &str = r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall";
    let mut out = Vec::new();
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    for (hive, sub) in [
        (&hklm, UNINSTALL),
        (&hklm, UNINSTALL_WOW),
        (&hkcu, UNINSTALL),
    ] {
        let root = match hive.open_subkey_with_flags(sub, KEY_READ) {
            Ok(r) => r,
            Err(_) => continue, // 无权限读这个 hive 就跳过这一路
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
            if display.trim().is_empty() {
                continue;
            }
            let system_component: u32 = key.get_value("SystemComponent").unwrap_or(0);
            if system_component == 1 {
                continue;
            }
            let location: String = key.get_value("InstallLocation").unwrap_or_default();
            let has_location =
                !location.trim().is_empty() && PathBuf::from(&location).is_dir();
            out.push(RawEntry {
                name: display.trim().to_string(),
                full_path: Some(if has_location {
                    location.clone()
                } else {
                    String::new()
                }),
                category: Category::App,
                file_ref: 0,
                parent_ref: 0,
                is_dir: false,
            });
            if has_location {
                let folder_name = location
                    .trim_end_matches(['\\', '/'])
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or("")
                    .to_string();
                out.push(RawEntry {
                    name: folder_name,
                    full_path: Some(location),
                    category: Category::InstallDir,
                    file_ref: 0,
                    parent_ref: 0,
                    is_dir: true,
                });
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
