//! 开机自启动：HKCU\Software\Microsoft\Windows\CurrentVersion\Run
//! 下的值名 "ShunSeek"，值为带引号的当前 exe 完整路径 + ` --tray`
//!（v0.1.8：自启时静默启动、只进托盘，程序在 main 里解析该参数）。
//! is_enabled 除「值存在」外还核对值里含当前 exe 路径——旧版本写
//! 的无 --tray 值、或 exe 被移动过，都判未启用，设置里重勾一次
//! 即按新格式重写。
//!
//! 勾选状态以注册表实际有无为准：启动时读一次，每次切换后回读确认
//! （写失败就不勾，别让 UI 显示一个并不存在的自启动）。
//! 非 Windows：恒未启用、set_enabled 空操作（托盘本身只在 Windows
//! 目标存在，这个模块只是让调用方不感知平台差异）。

#[cfg(windows)]
mod win {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
    };

    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const VALUE_NAME: &str = "ShunSeek";

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    fn open(write: bool) -> Option<HKEY> {
        unsafe {
            let mut h: HKEY = 0;
            let access = if write {
                KEY_SET_VALUE | KEY_QUERY_VALUE
            } else {
                KEY_QUERY_VALUE
            };
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide(RUN_KEY).as_ptr(),
                0,
                access,
                &mut h,
            ) == ERROR_SUCCESS
            {
                Some(h)
            } else {
                None
            }
        }
    }

    pub fn is_enabled() -> bool {
        unsafe {
            let Some(h) = open(false) else { return false };
            let mut ty = 0u32;
            let mut len = 0u32;
            let ok = RegQueryValueExW(
                h,
                wide(VALUE_NAME).as_ptr(),
                std::ptr::null_mut(),
                &mut ty,
                std::ptr::null_mut(),
                &mut len,
            ) == ERROR_SUCCESS
                && len >= 2;
            if !ok {
                RegCloseKey(h);
                return false;
            }
            // 读出值本体，核对含当前 exe 路径（v0.1.8）
            let mut buf = vec![0u16; (len as usize / 2).max(1)];
            let mut len2 = len;
            let ok = RegQueryValueExW(
                h,
                wide(VALUE_NAME).as_ptr(),
                std::ptr::null_mut(),
                &mut ty,
                buf.as_mut_ptr() as *mut u8,
                &mut len2,
            ) == ERROR_SUCCESS;
            RegCloseKey(h);
            if !ok {
                return false;
            }
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let value = String::from_utf16_lossy(&buf[..end]);
            let exe = std::env::current_exe()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let val_lower = value.to_lowercase();
            !exe.is_empty()
                && val_lower.contains(&exe.to_lowercase())
                && val_lower.contains("--tray")
        }
    }

    pub fn set_enabled(on: bool) -> bool {
        unsafe {
            let Some(h) = open(true) else { return false };
            let name = wide(VALUE_NAME);
            let ok = if on {
                let exe = std::env::current_exe()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if exe.is_empty() {
                    RegCloseKey(h);
                    return false;
                }
                let val = wide(&format!("\"{exe}\" --tray"));
                RegSetValueExW(
                    h,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    val.as_ptr() as *const u8,
                    (val.len() * 2) as u32,
                ) == ERROR_SUCCESS
            } else {
                RegDeleteValueW(h, name.as_ptr()) == ERROR_SUCCESS
            };
            RegCloseKey(h);
            ok
        }
    }
}

#[cfg(windows)]
pub use win::{is_enabled, set_enabled};

#[cfg(not(windows))]
pub fn is_enabled() -> bool {
    false
}

#[cfg(not(windows))]
pub fn set_enabled(_on: bool) -> bool {
    false
}
