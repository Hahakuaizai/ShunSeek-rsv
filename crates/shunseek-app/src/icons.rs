//! 结果行真实图标（v0.1.5）：Windows 上向 Shell 要系统图标，行里不再
//! 只有分类色块 + 首字。设计要点：
//!  - **不阻塞 UI**：取图标在专职后台线程做，UI 绘制时只查缓存；
//!    未命中先发请求（同 key 只发一次）、本帧只画空占位盘，图标到了
//!    （worker 完成时会 request_repaint）下一帧自动换上。
//!  - **图标总规（v0.1.8 用户钉死）**：文件类图标只许 Windows 11
//!    系统原版——本链三段（真实路径取图 → .lnk 目标解析取图 →
//!    USEFILEATTRIBUTES 按扩展名/属性问通用图标）全是 Shell 系统
//!    图标，最终兜底也是系统通用图标；绘制侧在 Windows 上永不画
//!    自绘分类字形冒充文件/文件夹/软件图标，未到达就空盘等。
//!  - **按 key 取，不逐个访问磁盘**：普通文件按扩展名取（SHGetFileInfoW
//!    + USEFILEATTRIBUTES，只问类型图标，不打开文件本体）；文件夹共用
//!    一个 "folder"；只有 .exe / .lnk / 应用条目按完整路径取真图标
//!    （每个程序各异，且 .lnk 经 Shell 解析会拿到目标的图标）。
//!  - **缓存有上限**：key 数到 MAX_ICON_KEYS 就不再取新的，免得长
//!    会话里纹理无限涨；取失败的 key 也记下来（None），不反复重试。
//!  - **全程判空判错**：FFI 任何一步失败都回退色块，绝不 panic。
//!  - **图标三规（v0.1.7 用户钉死）**：文件夹 = 系统文件夹图标、
//!    文件 = 对应类型图标、快捷方式 = 目标软件的 ico。前两条由
//!    key_for 的分流天然满足；第三条补强：`path:` 的 .lnk 直接
//!    SHGetFileInfo 取图失败时，走 IShellLink COM 解析目标路径与
//!    图标位置、再对目标取图（见 fetch_lnk_target_icon）。
//!
//! 非 Windows：没有 Shell 这回事，texture() 恒返回 None，绘制侧
//! 回退空占位盘（演示构建另在盘内保留类别字形，见 main.rs 的
//! cfg 分流；Windows 构建无此回退）。

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender};

use shunseek_core::Category;

/// 图标缓存上限（个 key）：一屏结果最多牵涉百来个 key，256 足够。
const MAX_ICON_KEYS: usize = 256;
/// 图标在结果行内的绘制边长（原版行图标 24×24，占位盘 IconPlate 同尺寸）。
pub const ICON_SIZE: f32 = 24.0;

/// 一个结果行该用什么 key 取图标：
///  - 文件夹 / 安装目录 → "folder"（全系统共用一个通用文件夹图标）
///  - 应用 / 快捷方式 / 散落 exe → "path:<完整路径>"（图标各异）
///  - 其余文件 → 扩展名（含点、小写，如 ".docx"）；无扩展名 → "file"
pub fn key_for(name: &str, path: &str, category: Category) -> String {
    match category {
        Category::Folder | Category::InstallDir => "folder".to_string(),
        Category::App | Category::Shortcut | Category::Executable => {
            format!("path:{path}")
        }
        _ => match name.rfind('.') {
            Some(p) if p > 0 => name[p..].to_lowercase(),
            _ => "file".to_string(),
        },
    }
}

/// 后台线程取回的一张图标：(宽, 高, RGBA 字节)。
type Pixels = (u32, u32, Vec<u8>);

pub struct IconCache {
    /// key → 纹理；None = 已取过但失败 / 无图标，恒回退色块。
    textures: HashMap<String, Option<egui::TextureHandle>>,
    /// 已发过请求的 key（防重复发）。非 Windows 下无人用，属预期闲置。
    #[allow(dead_code)]
    requested: HashSet<String>,
    tx: Option<Sender<String>>,
    rx: Option<Receiver<(String, Option<Pixels>)>>,
    /// 可观测性计数（v0.1.6）：收割结果时累计，设置窗口「索引」区展示
    /// 「图标：已加载 X · 失败 Y」，真机排障时一眼定位取图是否全灭。
    loaded_count: usize,
    failed_count: usize,
}

impl IconCache {
    pub fn new(ctx: &egui::Context) -> IconCache {
        let (tx, rx) = win::spawn_worker(ctx);
        IconCache {
            textures: HashMap::new(),
            requested: HashSet::new(),
            tx,
            rx,
            loaded_count: 0,
            failed_count: 0,
        }
    }

    /// 计数三元组：(已发出请求数, 已加载成功数, 已知失败数)。
    /// 设置窗口「索引」区用它显示图标管线健康度。
    pub fn stats(&self) -> (usize, usize, usize) {
        (self.requested.len(), self.loaded_count, self.failed_count)
    }

    /// 查 key 的图标纹理：命中（含已知失败）直接返回；未命中则发请求
    /// 并返回 None（本帧回退色块）。顺带收割后台线程已取回的结果。
    pub fn texture(&mut self, ctx: &egui::Context, key: &str) -> Option<egui::TextureHandle> {
        // 收割结果：RGBA → egui 纹理（只能在 UI 线程做，所以在这里建）
        if let Some(rx) = &self.rx {
            let mut arrived: Vec<(String, Option<Pixels>)> = Vec::new();
            while let Ok(item) = rx.try_recv() {
                arrived.push(item);
            }
            for (k, px) in arrived {
                match &px {
                    Some(_) => self.loaded_count += 1,
                    None => self.failed_count += 1,
                }
                let handle = px.map(|(w, h, rgba)| {
                    let img = egui::ColorImage::from_rgba_unmultiplied(
                        [w as usize, h as usize],
                        &rgba,
                    );
                    ctx.load_texture(
                        format!("ss-icon-{k}"),
                        img,
                        egui::TextureOptions::LINEAR,
                    )
                });
                self.textures.insert(k, handle);
            }
        }
        if let Some(h) = self.textures.get(key) {
            return h.clone();
        }
        // 未命中：发请求（同 key 只发一次；缓存 + 在途都算数，满了停增）
        if self.textures.len() + self.requested.len() < MAX_ICON_KEYS
            && !self.requested.contains(key)
        {
            if let Some(tx) = &self.tx {
                if tx.send(key.to_string()).is_ok() {
                    self.requested.insert(key.to_string());
                }
            }
        }
        None
    }
}

#[cfg(windows)]
mod win {
    use super::Pixels;
    use std::sync::mpsc::{channel, Receiver, Sender};

    /// 起图标 worker 线程（非 Windows 返回 (None, None)，缓存恒空）。
    /// worker 只做 FFI 取图 + 回传像素，不碰 egui 纹理（那是 UI 线程的事）；
    /// 每取回一张就 request_repaint，让正在显示的行尽快换上真图标。
    pub fn spawn_worker(
        ctx: &egui::Context,
    ) -> (
        Option<Sender<String>>,
        Option<Receiver<(String, Option<Pixels>)>>,
    ) {
        let (req_tx, req_rx) = channel::<String>();
        let (res_tx, res_rx) = channel::<(String, Option<Pixels>)>();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("shunseek-icons".into())
            .spawn(move || {
                // v0.1.6：worker 线程先初始化 COM（STA）。SHGetFileInfoW 对
                // .lnk 解析、部分图标提取走 Shell 命名空间对象，未初始化
                // COM 时会整批无声失败——v0.1.5 真机「图标全灭」的最可疑
                // 根因。初始化失败也不中断线程（取图本身仍可尝试）。
                unsafe {
                    use windows_sys::Win32::System::Com::{
                        CoInitializeEx, COINIT_APARTMENTTHREADED,
                    };
                    let _ = CoInitializeEx(
                        std::ptr::null(),
                        COINIT_APARTMENTTHREADED as u32,
                    );
                }
                while let Ok(key) = req_rx.recv() {
                    // FFI 全程已判空判错，这里再套一层 catch_unwind：
                    // 就算 Shell 那边出了意料外的状况，也只丢这一张图。
                    let px = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        fetch(&key)
                    }))
                    .unwrap_or(None);
                    if res_tx.send((key, px)).is_err() {
                        break; // UI 已退出，线程跟着收工
                    }
                    ctx.request_repaint();
                }
                unsafe {
                    windows_sys::Win32::System::Com::CoUninitialize();
                }
            });
        match spawned {
            Ok(_) => (Some(req_tx), Some(res_rx)),
            Err(_) => (None, None),
        }
    }

    /// 按 key 取系统图标并转成 RGBA。任何一步失败都返回 None。
    /// v0.1.6：`path:` 类 key 首次按真实路径取失败时，再带
    /// SHGFI_USEFILEATTRIBUTES 按文件属性重试一次（不解析 .lnk 目标、
    /// 不落盘打开文件），至少拿到该扩展名的通用图标而不是回退色块。
    fn fetch(key: &str) -> Option<Pixels> {
        const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
        const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

        // (要问的路径, 文件属性, 是否走 USEFILEATTRIBUTES 不落盘)
        let (path_str, attrs, use_attrs): (String, u32, bool) =
            if key == "folder" {
                ("folder".to_string(), FILE_ATTRIBUTE_DIRECTORY, true)
            } else if key == "file" {
                ("file".to_string(), FILE_ATTRIBUTE_NORMAL, true)
            } else if let Some(p) = key.strip_prefix("path:") {
                (p.to_string(), 0, false)
            } else {
                // ".docx" 之类的扩展名 key：拼个假文件名问类型图标
                (format!("file{key}"), FILE_ATTRIBUTE_NORMAL, true)
            };

        if let Some(px) = fetch_once(&path_str, attrs, use_attrs) {
            return Some(px);
        }
        if key.starts_with("path:") && !use_attrs {
            // v0.1.7 图标三规兜底：.lnk 直接取图失败 → IShellLink
            // 解析目标 / 图标位置后对目标取图（目标软件自己的 ico），
            // 成功同样计 loaded（worker 回传 Some 即计数）。
            if path_str.to_lowercase().ends_with(".lnk") {
                if let Some(px) = fetch_lnk_target_icon(&path_str) {
                    return Some(px);
                }
            }
            // 真实路径取图失败 → 按属性问法重试（通用图标兜底）
            return fetch_once(&path_str, FILE_ATTRIBUTE_NORMAL, true);
        }
        None
    }

    /// IShellLink / IPersistFile 的 vtbl 布局（windows-sys 0.52 只给
    /// 接口指针类型、不给 vtbl，这里按 shobjidl.h 的方法顺序手写；
    /// 只声明要用的槽，其余槽用指针占位，布局与真实 vtbl 一致）。
    #[repr(C)]
    struct IShellLinkWVtbl {
        query_interface: unsafe extern "system" fn(
            *mut std::ffi::c_void,
            *const windows_sys::core::GUID,
            *mut *mut std::ffi::c_void,
        ) -> i32,
        add_ref: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
        release: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
        get_path: unsafe extern "system" fn(
            *mut std::ffi::c_void,
            *mut u16,
            i32,
            *mut std::ffi::c_void,
            u32,
        ) -> i32,
        _get_id_list: *const std::ffi::c_void,
        _set_id_list: *const std::ffi::c_void,
        _get_description: *const std::ffi::c_void,
        _set_description: *const std::ffi::c_void,
        _get_working_directory: *const std::ffi::c_void,
        _set_working_directory: *const std::ffi::c_void,
        _get_arguments: *const std::ffi::c_void,
        _set_arguments: *const std::ffi::c_void,
        _get_hotkey: *const std::ffi::c_void,
        _set_hotkey: *const std::ffi::c_void,
        _get_show_cmd: *const std::ffi::c_void,
        _set_show_cmd: *const std::ffi::c_void,
        get_icon_location: unsafe extern "system" fn(
            *mut std::ffi::c_void,
            *mut u16,
            i32,
            *mut i32,
        ) -> i32,
        _set_icon_location: *const std::ffi::c_void,
        _set_relative_path: *const std::ffi::c_void,
        _resolve: *const std::ffi::c_void,
        _set_path: *const std::ffi::c_void,
    }

    #[repr(C)]
    struct IPersistFileVtbl {
        query_interface: unsafe extern "system" fn(
            *mut std::ffi::c_void,
            *const windows_sys::core::GUID,
            *mut *mut std::ffi::c_void,
        ) -> i32,
        add_ref: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
        release: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
        _get_class_id: *const std::ffi::c_void,
        _is_dirty: *const std::ffi::c_void,
        load: unsafe extern "system" fn(
            *mut std::ffi::c_void,
            *const u16,
            u32,
        ) -> i32,
        _save: *const std::ffi::c_void,
        _save_completed: *const std::ffi::c_void,
        _get_cur_file: *const std::ffi::c_void,
    }

    /// .lnk 兜底取图（v0.1.7 图标三规）：IShellLink 解析出目标路径
    /// 与图标位置后，对图标位置（快捷方式自带 ico 时就是它）或目标
    /// 本体走 fetch_once 真实路径取图——拿到的是目标软件自己的图标。
    /// 任何一步失败返回 None，外层再走通用图标兜底。
    fn fetch_lnk_target_icon(lnk_path: &str) -> Option<Pixels> {
        let (target, icon_loc) = unsafe { resolve_lnk(lnk_path) }?;
        if !icon_loc.is_empty() {
            if let Some(px) = fetch_once(&icon_loc, 0, false) {
                return Some(px);
            }
        }
        if !target.is_empty() && target != icon_loc {
            if let Some(px) = fetch_once(&target, 0, false) {
                return Some(px);
            }
        }
        None
    }

    /// 用 COM 解析 .lnk：CoCreateInstance(ShellLink) → QI 出
    /// IPersistFile → Load(.lnk) → GetPath + GetIconLocation。
    /// 返回 (目标路径, 图标位置路径)，任一可为空串；整体失败 None。
    /// 所有接口指针在返回前 Release。
    unsafe fn resolve_lnk(lnk_path: &str) -> Option<(String, String)> {
        use std::ffi::{c_void, OsStr};
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::core::GUID;
        use windows_sys::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

        const CLSID_SHELLLINK: GUID = GUID {
            data1: 0x0002_1401,
            data2: 0,
            data3: 0,
            data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
        };
        const IID_ISHELLLINKW: GUID = GUID {
            data1: 0x0002_14F9,
            data2: 0,
            data3: 0,
            data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
        };
        const IID_IPERSISTFILE: GUID = GUID {
            data1: 0x0000_010B,
            data2: 0,
            data3: 0,
            data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
        };
        const MAX_PATH_LEN: usize = 1024;

        let mut link: *mut c_void = std::ptr::null_mut();
        let hr = CoCreateInstance(
            &CLSID_SHELLLINK,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_ISHELLLINKW,
            &mut link,
        );
        if hr != 0 || link.is_null() {
            return None;
        }
        // 主体放闭包里执行，保证每条退出路径都 Release
        let work = |link: *mut c_void| -> Option<(String, String)> {
            let link_vt = *(link as *const *const IShellLinkWVtbl);
            let mut pf: *mut c_void = std::ptr::null_mut();
            let hr = ((*link_vt).query_interface)(
                link,
                &IID_IPERSISTFILE,
                &mut pf,
            );
            if hr != 0 || pf.is_null() {
                return None;
            }
            let result = (|| {
                let pf_vt = *(pf as *const *const IPersistFileVtbl);
                let wide: Vec<u16> = OsStr::new(lnk_path)
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect();
                // IPersistFile::Load(path, STGM_READ=0)
                if ((*pf_vt).load)(pf, wide.as_ptr(), 0) != 0 {
                    return None;
                }
                let mut target_buf = vec![0u16; MAX_PATH_LEN];
                let hr = ((*link_vt).get_path)(
                    link,
                    target_buf.as_mut_ptr(),
                    MAX_PATH_LEN as i32,
                    std::ptr::null_mut(),
                    0,
                );
                let target = if hr == 0 {
                    wide_to_string(&target_buf)
                } else {
                    String::new()
                };
                let mut icon_buf = vec![0u16; MAX_PATH_LEN];
                let mut icon_index: i32 = 0;
                let hr = ((*link_vt).get_icon_location)(
                    link,
                    icon_buf.as_mut_ptr(),
                    MAX_PATH_LEN as i32,
                    &mut icon_index,
                );
                let icon_loc = if hr == 0 {
                    wide_to_string(&icon_buf)
                } else {
                    String::new()
                };
                if target.is_empty() && icon_loc.is_empty() {
                    None
                } else {
                    Some((target, icon_loc))
                }
            })();
            let pf_vt = *(pf as *const *const IPersistFileVtbl);
            ((*pf_vt).release)(pf);
            result
        };
        let result = work(link);
        let link_vt = *(link as *const *const IShellLinkWVtbl);
        ((*link_vt).release)(link);
        result
    }

    /// NUL 结尾的 UTF-16 缓冲 → String（截到第一个 NUL 为止）。
    fn wide_to_string(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    /// 单次 SHGetFileInfoW 问图：给定路径/属性/问法，成功返回 RGBA。
    fn fetch_once(path_str: &str, attrs: u32, use_attrs: bool) -> Option<Pixels> {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::Shell::{
            SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_USEFILEATTRIBUTES,
        };

        let wide: Vec<u16> = OsStr::new(path_str)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut info: SHFILEINFOW = unsafe { std::mem::zeroed() };
        let mut flags = SHGFI_ICON; // 不设 SMALLICON 即大图标（一般 32×32）
        if use_attrs {
            flags |= SHGFI_USEFILEATTRIBUTES;
        }
        let ok = unsafe {
            SHGetFileInfoW(
                wide.as_ptr(),
                attrs,
                &mut info,
                std::mem::size_of::<SHFILEINFOW>() as u32,
                flags,
            )
        };
        if ok == 0 || info.hIcon == 0 {
            return None;
        }
        let px = unsafe { hicon_to_rgba(info.hIcon) };
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon(info.hIcon);
        }
        px
    }

    /// HICON → (宽, 高, RGBA)：GetIconInfo 拿颜色位图，GetDIBits 按
    /// 32bpp 自上而下读 BGRA，再转成 RGBA。位图句柄用完 DeleteObject。
    unsafe fn hicon_to_rgba(
        hicon: windows_sys::Win32::UI::WindowsAndMessaging::HICON,
    ) -> Option<Pixels> {
        use std::ffi::c_void;
        use windows_sys::Win32::Graphics::Gdi::{
            DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO,
            BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};

        let mut ii: ICONINFO = std::mem::zeroed();
        if GetIconInfo(hicon, &mut ii) == 0 {
            return None;
        }
        let result = (|| {
            if ii.hbmColor == 0 {
                return None;
            }
            let mut bm: BITMAP = std::mem::zeroed();
            let got = GetObjectW(
                ii.hbmColor,
                std::mem::size_of::<BITMAP>() as i32,
                &mut bm as *mut BITMAP as *mut c_void,
            );
            if got == 0 {
                return None;
            }
            let (w, h) = (bm.bmWidth, bm.bmHeight);
            if w <= 0 || h <= 0 || w > 256 || h > 256 {
                return None;
            }
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = w;
            bmi.bmiHeader.biHeight = -h; // 负高 = 自上而下，省一次翻转
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;
            let mut buf = vec![0u8; (w * h * 4) as usize];
            let hdc = GetDC(0);
            if hdc == 0 {
                return None;
            }
            let lines = GetDIBits(
                hdc,
                ii.hbmColor,
                0,
                h as u32,
                buf.as_mut_ptr() as *mut c_void,
                &mut bmi,
                DIB_RGB_COLORS,
            );
            ReleaseDC(0, hdc);
            if lines == 0 {
                return None;
            }
            // BGRA → RGBA
            for px in buf.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
            // 老图标没有 alpha 通道（全像素 alpha=0）：整体置不透明，
            // 否则整张图会因 alpha=0 而完全透明、什么都看不见。
            if buf.iter().skip(3).step_by(4).all(|&a| a == 0) {
                for px in buf.chunks_exact_mut(4) {
                    px[3] = 255;
                }
            }
            Some((w as u32, h as u32, buf))
        })();
        if ii.hbmColor != 0 {
            DeleteObject(ii.hbmColor);
        }
        if ii.hbmMask != 0 {
            DeleteObject(ii.hbmMask);
        }
        result
    }
}

#[cfg(not(windows))]
mod win {
    use super::Pixels;
    use std::sync::mpsc::{Receiver, Sender};

    /// 非 Windows 骨架：不起线程，缓存恒空，结果行回退分类色块。
    pub fn spawn_worker(
        _ctx: &egui::Context,
    ) -> (
        Option<Sender<String>>,
        Option<Receiver<(String, Option<Pixels>)>>,
    ) {
        (None, None)
    }
}
