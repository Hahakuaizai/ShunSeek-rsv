//! 显示名解析（v0.1.9，对齐原版 ShellNames.cs）：文件名 ≠ 给人
//! 看的名字——文件夹 Desktop 显示「桌面」、微信.lnk 显示「微信」
//! （不带扩展名）、Weixin.exe 的文件描述是「微信」。索引里只存
//! 路径，这些名字在渲染期向 Shell 问、按路径缓存、后台线程做，
//! UI 线程只查缓存（未命中本帧先用文件名，到了下一帧自动换上）。
//!
//! 口径与原版逐条一致：
//!  - 只解析三类：文件夹 → SHGetFileInfo 显示名；.lnk → Shell
//!    显示名（拿不到退回去后缀文件名）；.exe → 版本信息里的
//!    FileDescription。
//!  - 解析结果与文件名相同 / 空白 → 不替换（返回 None）。
//!  - 失败也记缓存（None），不反复重试（Shell 查询不便宜）。
//!
//! 非 Windows：恒返回 None，绘制侧用索引名。

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender};

pub struct NameService {
    /// path → 显示名；None = 已查过、无替换名（或不适用）。
    cache: HashMap<String, Option<String>>,
    /// 已发过请求的 path（防重复发）。
    requested: HashSet<String>,
    tx: Option<Sender<(String, bool)>>,
    rx: Option<Receiver<(String, Option<String>)>>,
}

impl NameService {
    pub fn new(ctx: &egui::Context) -> NameService {
        let (tx, rx) = win::spawn_worker(ctx);
        NameService {
            cache: HashMap::new(),
            requested: HashSet::new(),
            tx,
            rx,
        }
    }

    /// 该路径是否属于要解析的三类（文件夹 / .lnk / .exe）。
    fn qualifies(path: &str, is_dir: bool) -> bool {
        if is_dir {
            return true;
        }
        let lower = path.to_lowercase();
        lower.ends_with(".lnk") || lower.ends_with(".exe")
    }

    /// 查显示名：命中（含已查无替换）直接返回；未命中发请求、
    /// 本帧返回 None（绘制侧用索引名顶着）。顺带收割已完成的
    /// 解析结果。
    pub fn get(&mut self, path: &str, is_dir: bool) -> Option<String> {
        if let Some(rx) = &self.rx {
            while let Ok((p, name)) = rx.try_recv() {
                self.cache.insert(p, name);
            }
        }
        if let Some(hit) = self.cache.get(path) {
            return hit.clone();
        }
        if Self::qualifies(path, is_dir) && !self.requested.contains(path) {
            if let Some(tx) = &self.tx {
                if tx.send((path.to_string(), is_dir)).is_ok() {
                    self.requested.insert(path.to_string());
                }
            }
        }
        None
    }
}

#[cfg(windows)]
mod win {
    use std::sync::mpsc::{channel, Receiver, Sender};

    /// 起显示名 worker 线程（非 Windows 返回 (None, None)）。
    /// 只做 Shell 查询 + 回传，不碰 UI；每回一条就 request_repaint，
    /// 让正在显示的行尽快换上显示名。
    pub fn spawn_worker(
        ctx: &egui::Context,
    ) -> (
        Option<Sender<(String, bool)>>,
        Option<Receiver<(String, Option<String>)>>,
    ) {
        let (req_tx, req_rx) = channel::<(String, bool)>();
        let (res_tx, res_rx) = channel::<(String, Option<String>)>();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("shunseek-names".into())
            .spawn(move || {
                while let Ok((path, is_dir)) = req_rx.recv() {
                    let name = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        resolve(&path, is_dir)
                    }))
                    .unwrap_or(None);
                    if res_tx.send((path, name)).is_err() {
                        break; // UI 已退出，线程跟着收工
                    }
                    ctx.request_repaint();
                }
            });
        match spawned {
            Ok(_) => (Some(req_tx), Some(res_rx)),
            Err(_) => (None, None),
        }
    }

    /// 按原版 ShellNames.Resolve 的口径解析一个路径的显示名。
    fn resolve(path: &str, is_dir: bool) -> Option<String> {
        let lower = path.to_lowercase();
        let mut name: Option<String> = if is_dir {
            shell_display_name(path)
        } else if lower.ends_with(".lnk") {
            shell_display_name(path).or_else(|| {
                // Shell 显示名拿不到：退回去掉 .lnk 后缀的文件名
                let leaf = path.rsplit(['\\', '/']).next().unwrap_or(path);
                Some(leaf[..leaf.len().saturating_sub(4)].to_string())
            })
        } else if lower.ends_with(".exe") {
            file_description(path)
        } else {
            None
        };
        // 与文件名相同 / 空白 → 不替换（与原版一致）
        let leaf = path.rsplit(['\\', '/']).next().unwrap_or(path);
        if let Some(n) = &name {
            if n == leaf || n.trim().is_empty() {
                name = None;
            }
        }
        name
    }

    /// SHGetFileInfoW + SHGFI_DISPLAYNAME：Shell 给的本地化名。
    fn shell_display_name(path: &str) -> Option<String> {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::Shell::{
            SHGetFileInfoW, SHFILEINFOW, SHGFI_DISPLAYNAME,
        };
        let wide: Vec<u16> = OsStr::new(path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut info: SHFILEINFOW = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            SHGetFileInfoW(
                wide.as_ptr(),
                0,
                &mut info,
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_DISPLAYNAME,
            )
        };
        if ok == 0 {
            return None;
        }
        let buf: &[u16] = &info.szDisplayName;
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let text = String::from_utf16_lossy(&buf[..end]).trim().to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    /// .exe 的 FileDescription（版本信息）：读 Translation 表拿
    /// (语言, 代码页)，再查 StringFileInfo 对应块；Translation
    /// 读不到时按常见组合（简中/美英 + Unicode 代码页）兜底试。
    fn file_description(path: &str) -> Option<String> {
        use std::ffi::{c_void, OsStr};
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
        };
        let wide: Vec<u16> = OsStr::new(path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let mut handle: u32 = 0;
            let size = GetFileVersionInfoSizeW(wide.as_ptr(), &mut handle);
            if size == 0 {
                return None;
            }
            let mut buf = vec![0u8; size as usize];
            if GetFileVersionInfoW(
                wide.as_ptr(),
                0,
                size,
                buf.as_mut_ptr() as *mut c_void,
            ) == 0
            {
                return None;
            }
            // 候选 (lang, codepage)：先 Translation 表，再常见组合
            let mut pairs: Vec<(u16, u16)> = Vec::new();
            let trans_key: Vec<u16> = OsStr::new("\\VarFileInfo\\Translation")
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let mut ptr: *mut c_void = std::ptr::null_mut();
            let mut len: u32 = 0;
            if VerQueryValueW(
                buf.as_ptr() as *const c_void,
                trans_key.as_ptr(),
                &mut ptr,
                &mut len,
            ) != 0
                && !ptr.is_null()
                && len >= 4
            {
                let words = std::slice::from_raw_parts(ptr as *const u16, (len / 2) as usize);
                for pair in words.chunks_exact(2) {
                    pairs.push((pair[0], pair[1]));
                }
            }
            pairs.push((0x0804, 0x04B0)); // 简体中文 + Unicode
            pairs.push((0x0409, 0x04B0)); // 美式英语 + Unicode
            pairs.push((0x0804, 0x03A8)); // 简体中文 + GBK
            for (lang, cp) in pairs {
                let sub = format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\FileDescription");
                let sub_w: Vec<u16> = OsStr::new(&sub)
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect();
                let mut ptr: *mut c_void = std::ptr::null_mut();
                let mut len: u32 = 0;
                if VerQueryValueW(
                    buf.as_ptr() as *const c_void,
                    sub_w.as_ptr(),
                    &mut ptr,
                    &mut len,
                ) != 0
                    && !ptr.is_null()
                    && len > 0
                {
                    let chars =
                        std::slice::from_raw_parts(ptr as *const u16, len as usize);
                    let text = String::from_utf16_lossy(chars)
                        .trim_matches('\0')
                        .trim()
                        .to_string();
                    if !text.is_empty() {
                        return Some(text);
                    }
                }
            }
            None
        }
    }
}

#[cfg(not(windows))]
mod win {
    use std::sync::mpsc::{Receiver, Sender};

    /// 非 Windows 骨架：不起线程，显示名恒用索引名。
    pub fn spawn_worker(
        _ctx: &egui::Context,
    ) -> (
        Option<Sender<(String, bool)>>,
        Option<Receiver<(String, Option<String>)>>,
    ) {
        (None, None)
    }
}
