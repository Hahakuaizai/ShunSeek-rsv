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
//!  - **取图链（v0.1.9 对齐原版 IconCache）**：`path:` 类 key 依次走
//!    ① IShellItemImageFactory 按路径取 64px 本项图标 → ② SHGetFileInfoW
//!    真实路径大图标 → ③ .lnk 目标解析取图 → ④ USEFILEATTRIBUTES
//!    通用图标兜底。
//!  - **失败重试（v0.1.9 对齐原版）**：失败不焊死——计数 <3 次时
//!    下次查询可重发请求（防启动瞬间 Shell 忙导致永久灰块），满
//!    3 次才缓存空结果。
//!  - **缓存有上限**：key 数到 MAX_ICON_KEYS 就不再取新的，免得长
//!    会话里纹理无限涨（64px RGBA 一张 16KB，1024 个约 16MB 封顶）。
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

/// 图标缓存上限（个 key）：64px 图源一张约 16KB，256 个封顶约 4MB，
/// 配合隐藏时清理，有效控制前台与后台内存占用。
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
            if path.is_empty() {
                "file".to_string()
            } else {
                format!("path:{path}")
            }
        }
        _ => match name.rfind('.') {
            Some(p) if p > 0 => name[p..].to_lowercase(),
            _ => "file".to_string(),
        },
    }
}

/// 后台线程取回的一张图标：(宽, 高, RGBA 字节, 是否预乘 alpha)。
type Pixels = (u32, u32, Vec<u8>, bool);

pub struct IconCache {
    /// key → 纹理；None = 已取过但失败 / 无图标，恒回退色块。
    textures: HashMap<String, Option<egui::TextureHandle>>,
    /// 正在途中的请求 key（收到结果后移除，避免上限被腰斩）。
    #[allow(dead_code)]
    requested: HashSet<String>,
    /// 累计发出的请求次数（供 stats 显示）。
    requested_total: usize,
    attempts: HashMap<String, u8>,
    tx: Option<Sender<String>>,
    rx: Option<Receiver<(String, Option<Pixels>)>>,
    loaded_count: usize,
    failed_count: usize,
}

impl IconCache {
    pub fn new(ctx: &egui::Context) -> IconCache {
        let (tx, rx) = win::spawn_worker(ctx);
        IconCache {
            textures: HashMap::new(),
            requested: HashSet::new(),
            requested_total: 0,
            attempts: HashMap::new(),
            tx,
            rx,
            loaded_count: 0,
            failed_count: 0,
        }
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        (self.requested_total, self.loaded_count, self.failed_count)
    }

    /// 隐藏到托盘时清空图标纹理与请求状态，释放内存。
    pub fn clear(&mut self) {
        if let Some(rx) = &self.rx {
            while rx.try_recv().is_ok() {}
        }
        self.textures.clear();
        self.textures.shrink_to_fit();
        self.requested.clear();
        self.requested.shrink_to_fit();
        self.attempts.clear();
        self.attempts.shrink_to_fit();
    }

    pub fn texture(&mut self, ctx: &egui::Context, key: &str) -> Option<egui::TextureHandle> {
        if let Some(rx) = &self.rx {
            let mut arrived: Vec<(String, Option<Pixels>)> = Vec::new();
            while let Ok(item) = rx.try_recv() {
                arrived.push(item);
            }
            for (k, px) in arrived {
                self.requested.remove(&k);
                match &px {
                    Some(_) => self.loaded_count += 1,
                    None => self.failed_count += 1,
                }
                if px.is_none() {
                    let n = {
                        let e = self.attempts.entry(k.clone()).or_insert(0);
                        *e += 1;
                        *e
                    };
                    if n >= 3 {
                        self.textures.insert(k, None);
                    }
                    continue;
                }
                let handle = px.map(|(w, h, rgba, premul)| {
                    let img = if premul {
                        egui::ColorImage::from_rgba_premultiplied(
                            [w as usize, h as usize],
                            &rgba,
                        )
                    } else {
                        egui::ColorImage::from_rgba_unmultiplied(
                            [w as usize, h as usize],
                            &rgba,
                        )
                    };
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
        if self.textures.len() + self.requested.len() < MAX_ICON_KEYS
            && !self.requested.contains(key)
        {
            if let Some(tx) = &self.tx {
                if tx.send(key.to_string()).is_ok() {
                    self.requested.insert(key.to_string());
                    self.requested_total += 1;
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
    /// `path:` 类 key 的取图链（v0.1.9 对齐原版 IconCache）：
    /// ① IShellItemImageFactory 按路径取 64px 本项图标 →
    /// ② SHGetFileInfoW 真实路径大图标 → ③ .lnk 目标解析取图
    /// （v0.1.7 图标三规）→ ④ USEFILEATTRIBUTES 通用图标兜底。
    /// 其余 key（folder / file / 扩展名）只问类型图标、不落盘。
    fn fetch(key: &str) -> Option<Pixels> {
        const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
        const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

        if key == "folder" {
            return fetch_once("folder", FILE_ATTRIBUTE_DIRECTORY, true);
        }
        if key == "file" {
            return fetch_once("file", FILE_ATTRIBUTE_NORMAL, true);
        }
        if let Some(p) = key.strip_prefix("path:") {
            if let Some(px) = fetch_factory(p) {
                return Some(px);
            }
            if let Some(px) = fetch_once(p, 0, false) {
                return Some(px);
            }
            if p.to_lowercase().ends_with(".lnk") {
                if let Some(px) = fetch_lnk_target_icon(p) {
                    return Some(px);
                }
            }
            return fetch_once(p, FILE_ATTRIBUTE_NORMAL, true);
        }
        // ".docx" 之类的扩展名 key：拼个假文件名问类型图标
        fetch_once(&format!("file{key}"), FILE_ATTRIBUTE_NORMAL, true)
    }

    /// IShellItemImageFactory 的 vtbl 布局（windows-sys 0.52 同样
    /// 只给指针类型；方法顺序按 shobjidl.h：QI/AddRef/Release 后
    /// 就是 GetImage）。
    #[repr(C)]
    struct IShellItemImageFactoryVtbl {
        query_interface: unsafe extern "system" fn(
            *mut std::ffi::c_void,
            *const windows_sys::core::GUID,
            *mut *mut std::ffi::c_void,
        ) -> i32,
        add_ref: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
        release: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
        get_image: unsafe extern "system" fn(
            *mut std::ffi::c_void,
            windows_sys::Win32::Foundation::SIZE,
            u32,
            *mut isize,
        ) -> i32,
    }

    /// 图标链首层（v0.1.9）：SHCreateItemFromParsingName 直接要
    /// IShellItemImageFactory，GetImage 取 64×64 本项图标
    /// （SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK：只要图标本体、
    /// 允许给大图）。返回的 HBITMAP 用完 DeleteObject，接口
    /// Release。
    fn fetch_factory(path_str: &str) -> Option<Pixels> {
        use std::ffi::{c_void, OsStr};
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::core::GUID;
        use windows_sys::Win32::Foundation::SIZE;
        use windows_sys::Win32::Graphics::Gdi::DeleteObject;
        use windows_sys::Win32::UI::Shell::SHCreateItemFromParsingName;

        const IID_SHELL_ITEM_IMAGE_FACTORY: GUID = GUID {
            data1: 0xBCC1_8B79,
            data2: 0xBA16,
            data3: 0x442F,
            data4: [0x80, 0xC4, 0x8A, 0x59, 0xC3, 0x0C, 0x46, 0x3B],
        };
        const SIIGBF_BIGGERSIZEOK: u32 = 0x1;
        const SIIGBF_ICONONLY: u32 = 0x4;

        let wide: Vec<u16> = OsStr::new(path_str)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let mut factory: *mut c_void = std::ptr::null_mut();
            let hr = SHCreateItemFromParsingName(
                wide.as_ptr(),
                std::ptr::null_mut(),
                &IID_SHELL_ITEM_IMAGE_FACTORY,
                &mut factory,
            );
            if hr != 0 || factory.is_null() {
                return None;
            }
            let result = (|| {
                let vt = *(factory as *const *const IShellItemImageFactoryVtbl);
                let mut hbm: isize = 0;
                let hr = ((*vt).get_image)(
                    factory,
                    SIZE { cx: 64, cy: 64 },
                    SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK,
                    &mut hbm,
                );
                if hr != 0 || hbm == 0 {
                    return None;
                }
                let px = hbitmap_to_rgba(hbm).map(|(w, h, buf)| (w, h, buf, true));
                DeleteObject(hbm);
                px
            })();
            let vt = *(factory as *const *const IShellItemImageFactoryVtbl);
            ((*vt).release)(factory);
            result
        }
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
        let px = unsafe { hicon_to_rgba(info.hIcon) }.map(|(w, h, buf)| (w, h, buf, false));
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon(info.hIcon);
        }
        px
    }

    /// HICON → (宽, 高, RGBA)：GetIconInfo 拿颜色位图后走
    /// hbitmap_to_rgba；位图句柄用完 DeleteObject。
    unsafe fn hicon_to_rgba(
        hicon: windows_sys::Win32::UI::WindowsAndMessaging::HICON,
    ) -> Option<(u32, u32, Vec<u8>)> {
        use windows_sys::Win32::Graphics::Gdi::DeleteObject;
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};

        let mut ii: ICONINFO = std::mem::zeroed();
        if GetIconInfo(hicon, &mut ii) == 0 {
            return None;
        }
        let px = if ii.hbmColor != 0 {
            hbitmap_to_rgba(ii.hbmColor)
        } else {
            None
        };
        if ii.hbmColor != 0 {
            DeleteObject(ii.hbmColor);
        }
        if ii.hbmMask != 0 {
            DeleteObject(ii.hbmMask);
        }
        px
    }

    /// HBITMAP → (宽, 高, RGBA)：GetObjectW 验尺寸，GetDIBits 按
    /// 32bpp 自上而下读 BGRA，再转成 RGBA。只读不删（句柄归
    /// 调用方）。HICON 路径与 IShellItemImageFactory 路径共用。
    unsafe fn hbitmap_to_rgba(
        hbm: windows_sys::Win32::Graphics::Gdi::HBITMAP,
    ) -> Option<(u32, u32, Vec<u8>)> {
        use std::ffi::c_void;
        use windows_sys::Win32::Graphics::Gdi::{
            GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO,
            BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        };
        let mut bm: BITMAP = std::mem::zeroed();
        let got = GetObjectW(
            hbm,
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
            hbm,
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
