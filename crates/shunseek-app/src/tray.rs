//! 系统托盘：搜索窗平时隐藏，托盘是它的常驻入口与唯一的正常退出出口。
//! 专职线程 + 原生 Win32 消息窗口（HWND_MESSAGE）：
//! - 左键单击图标：winvis::toggle() 切换显隐；
//! - 右键菜单：「打开瞬觅 / 重新建立索引 / 立即刷新索引 / 以管理员身份重启 / 设置 / 关于 / 退出」；
//! - 隐藏态下不需要显示窗口的后台动作（重建/刷新/提权重启）直接走 `on_bg_action` 回调执行，
//!   不依赖 UI 线程是否正在跑帧；
//! - 监听 `TaskbarCreated` 广播，Explorer 重启后自动恢复托盘图标；
//! - `icon_rect()` 带 1 秒缓存节流，避免每帧同步 IPC 卡顿 UI。

use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayAction {
    Toggle,
    Show,
    RebuildIndex,
    RefreshIndex,
    AdminRestart,
    OpenSettings,
    About,
    Exit,
}

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayBgAction {
    RebuildIndex,
    RefreshIndex,
    AdminRestart,
}

pub struct TrayInit {
    pub rebuilding: Arc<AtomicBool>,
    pub data_dir: std::path::PathBuf,
    pub hotkey_label: String,
    pub on_bg_action: Option<Arc<dyn Fn(TrayBgAction) + Send + Sync>>,
}

pub struct Tray {
    actions: Arc<Mutex<VecDeque<TrayAction>>>,
    #[cfg(windows)]
    inner: Option<win::WinTray>,
}

impl Tray {
    pub fn new(ctx: egui::Context, init: TrayInit) -> Tray {
        let actions: Arc<Mutex<VecDeque<TrayAction>>> = Arc::new(Mutex::new(VecDeque::new()));
        #[cfg(windows)]
        let inner = win::WinTray::create(ctx, actions.clone(), init);
        #[cfg(not(windows))]
        let _ = (&ctx, &init);
        Tray {
            actions,
            #[cfg(windows)]
            inner,
        }
    }

    pub fn poll(&mut self) -> Option<TrayAction> {
        let mut q = self.actions.lock().unwrap_or_else(|e| e.into_inner());
        q.pop_front()
    }

    pub fn shutdown(&mut self) {
        #[cfg(windows)]
        if let Some(inner) = &self.inner {
            inner.shutdown();
        }
    }

    pub fn shutdown_sync(&mut self) {
        #[cfg(windows)]
        if let Some(inner) = &self.inner {
            inner.shutdown_sync();
        }
    }

    pub fn icon_rect(&self) -> Option<[i32; 4]> {
        #[cfg(windows)]
        {
            self.inner.as_ref().and_then(|inner| inner.icon_rect())
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    pub fn set_rebuild_enabled(&mut self, enabled: bool) {
        #[cfg(windows)]
        if let Some(inner) = &self.inner {
            inner.set_rebuilding(!enabled);
        }
        #[cfg(not(windows))]
        let _ = enabled;
    }
}

#[cfg(windows)]
mod win {
    use super::{TrayAction, TrayBgAction, TrayInit};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

    const WM_TRAY: u32 = 0x8000 + 1;
    const WM_SS_SHUTDOWN: u32 = 0x8000 + 2;
    const WM_CLOSE: u32 = 0x0010;
    const WM_DESTROY: u32 = 0x0002;
    const WM_CONTEXTMENU: u32 = 0x007B;
    const NIN_SELECT: u32 = 0x0400;

    const NIM_ADD: u32 = 0;
    const NIM_DELETE: u32 = 2;
    const NIM_SETVERSION: u32 = 4;

    const ID_SHOW: usize = 1;
    const ID_REBUILD: usize = 2;
    const ID_SETTINGS: usize = 3;
    const ID_ABOUT: usize = 4;
    const ID_EXIT: usize = 5;
    const ID_REFRESH: usize = 6;
    const ID_ADMIN: usize = 7;

    struct TrayShared {
        actions: Arc<Mutex<VecDeque<TrayAction>>>,
        ctx: egui::Context,
        rebuilding: Arc<AtomicBool>,
        hwnd_out: Arc<AtomicIsize>,
        on_bg_action: Option<Arc<dyn Fn(TrayBgAction) + Send + Sync>>,
    }

    static SHARED: OnceLock<Arc<TrayShared>> = OnceLock::new();
    static TASKBAR_CREATED_MSG: AtomicU32 = AtomicU32::new(0);

    fn shared() -> Option<&'static Arc<TrayShared>> {
        SHARED.get()
    }

    fn push_action(a: TrayAction) {
        if let Some(sh) = shared() {
            sh.actions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_back(a);
            sh.ctx.request_repaint();
        }
    }

    pub struct WinTray {
        hwnd: Arc<AtomicIsize>,
        rebuilding: Arc<AtomicBool>,
        rect_cache: Mutex<(Option<Instant>, Option<[i32; 4]>)>,
    }

    impl WinTray {
        pub fn create(
            ctx: egui::Context,
            actions: Arc<Mutex<VecDeque<TrayAction>>>,
            init: TrayInit,
        ) -> Option<WinTray> {
            let hwnd = Arc::new(AtomicIsize::new(0));
            let shared_state = Arc::new(TrayShared {
                actions,
                ctx,
                rebuilding: init.rebuilding.clone(),
                hwnd_out: hwnd.clone(),
                on_bg_action: init.on_bg_action,
            });
            if SHARED.set(shared_state).is_err() {
                return None;
            }
            let hwnd_thread = hwnd.clone();
            let data_dir = init.data_dir;
            let hotkey_label = init.hotkey_label;
            let spawned = std::thread::Builder::new()
                .name("shunseek-tray".into())
                .spawn(move || {
                    tray_thread_main(hwnd_thread, data_dir, hotkey_label);
                });
            match spawned {
                Ok(_) => Some(WinTray {
                    hwnd,
                    rebuilding: init.rebuilding,
                    rect_cache: Mutex::new((None, None)),
                }),
                Err(_) => None,
            }
        }

        pub fn icon_rect(&self) -> Option<[i32; 4]> {
            use windows_sys::Win32::Foundation::RECT;
            use windows_sys::Win32::UI::Shell::{
                Shell_NotifyIconGetRect, NOTIFYICONIDENTIFIER,
            };
            let hwnd = self.hwnd.load(Ordering::SeqCst);
            if hwnd == 0 {
                return None;
            }
            let mut cache = self.rect_cache.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            if let (Some(ts), rect) = *cache {
                if now.duration_since(ts) < Duration::from_secs(1) {
                    return rect;
                }
            }
            let got = unsafe {
                let mut id: NOTIFYICONIDENTIFIER = std::mem::zeroed();
                id.cbSize = std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32;
                id.hWnd = hwnd;
                id.uID = 1;
                let mut rc: RECT = std::mem::zeroed();
                if Shell_NotifyIconGetRect(&id, &mut rc) == 0
                    && rc.right > rc.left
                    && rc.bottom > rc.top
                {
                    Some([rc.left, rc.top, rc.right, rc.bottom])
                } else {
                    None
                }
            };
            *cache = (Some(now), got);
            got
        }

        pub fn set_rebuilding(&self, rebuilding: bool) {
            self.rebuilding.store(rebuilding, Ordering::SeqCst);
        }

        pub fn shutdown(&self) {
            let hwnd = self.hwnd.load(Ordering::SeqCst);
            if hwnd != 0 {
                unsafe {
                    PostMessageW(hwnd, WM_SS_SHUTDOWN, 0, 0);
                }
            }
        }

        pub fn shutdown_sync(&self) {
            self.shutdown();
            for _ in 0..20 {
                if self.hwnd.load(Ordering::SeqCst) == 0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn add_tray_icon(hwnd: HWND) -> bool {
        unsafe {
            let hicon = make_app_hicon();
            let ok = notify_icon(hwnd, NIM_ADD, hicon);
            if hicon != 0 {
                windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon(hicon);
            }
            if ok {
                notify_icon_version4(hwnd);
            }
            ok
        }
    }

    fn tray_thread_main(
        hwnd_out: Arc<AtomicIsize>,
        data_dir: std::path::PathBuf,
        hotkey_label: String,
    ) {
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, DispatchMessageW, GetMessageW, RegisterClassW,
            RegisterWindowMessageW, TranslateMessage, MSG, WNDCLASSW,
        };

        unsafe {
            let tb_msg = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
            TASKBAR_CREATED_MSG.store(tb_msg, Ordering::Relaxed);

            let hinstance = GetModuleHandleW(std::ptr::null());
            if hinstance == 0 {
                return;
            }
            let class_name = wide("ShunSeekTrayWindow");
            let wc = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(tray_wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance,
                hIcon: 0,
                hCursor: 0,
                hbrBackground: 0,
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name.as_ptr(),
            };
            let _ = RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                wide("ShunSeekTray").as_ptr(),
                0,
                0,
                0,
                0,
                0,
                -3isize as HWND,
                0,
                hinstance,
                std::ptr::null(),
            );
            if hwnd == 0 {
                return;
            }
            hwnd_out.store(hwnd, Ordering::SeqCst);

            if !add_tray_icon(hwnd) {
                DestroyWindow(hwnd);
                return;
            }
            maybe_show_balloon(hwnd, &data_dir, &hotkey_label);

            let mut msg: MSG = std::mem::zeroed();
            loop {
                let r = GetMessageW(&mut msg, 0, 0, 0);
                if r <= 0 {
                    break;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    unsafe fn notify_icon(hwnd: HWND, message: u32, hicon: isize) -> bool {
        use windows_sys::Win32::UI::Shell::{
            Shell_NotifyIconW, NOTIFYICONDATAW, NIF_ICON, NIF_MESSAGE, NIF_TIP,
        };
        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = 1;
        if message == NIM_ADD {
            data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
            data.uCallbackMessage = WM_TRAY;
            data.hIcon = hicon;
            let tip = "瞬觅 ShunSeek";
            for (i, u) in tip.encode_utf16().enumerate() {
                if i < data.szTip.len() - 1 {
                    data.szTip[i] = u;
                }
            }
        }
        Shell_NotifyIconW(message, &data) != 0
    }

    unsafe fn notify_icon_version4(hwnd: HWND) {
        use windows_sys::Win32::UI::Shell::{
            Shell_NotifyIconW, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
        };
        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = 1;
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
    }

    unsafe fn notify_icon_delete(hwnd: HWND) {
        notify_icon(hwnd, NIM_DELETE, 0);
    }

    unsafe fn maybe_show_balloon(
        hwnd: HWND,
        data_dir: &std::path::Path,
        hotkey_label: &str,
    ) {
        use windows_sys::Win32::UI::Shell::{
            Shell_NotifyIconW, NOTIFYICONDATAW, NIF_INFO,
        };
        const NIM_MODIFY: u32 = 1;
        const NIIF_INFO: u32 = 1;
        let flag = data_dir.join("tray-hint-shown");
        if flag.exists() {
            return;
        }
        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = 1;
        data.uFlags = NIF_INFO;
        data.dwInfoFlags = NIIF_INFO;
        data.Anonymous.uTimeout = 5000;
        let title = "瞬觅已在托盘";
        for (i, u) in title.encode_utf16().enumerate() {
            if i < data.szInfoTitle.len() - 1 {
                data.szInfoTitle[i] = u;
            }
        }
        let body = format!(
            "Win11 可能把它收进了 ^ 折叠区，拖出来固定一下就好。{hotkey_label} 呼出搜索。"
        );
        for (i, u) in body.encode_utf16().enumerate() {
            if i < data.szInfo.len() - 1 {
                data.szInfo[i] = u;
            }
        }
        if Shell_NotifyIconW(NIM_MODIFY, &data) != 0 {
            let _ = std::fs::write(&flag, b"");
        }
    }

    unsafe fn make_app_hicon() -> isize {
        use windows_sys::Win32::Graphics::Gdi::{
            CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER,
            BI_RGB, DIB_RGB_COLORS,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, ICONINFO};

        let rgba = include_bytes!("../assets/icon-64.rgba");
        if rgba.len() < 64 * 64 * 4 {
            return 0;
        }
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = 64;
        bmi.bmiHeader.biHeight = -64;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let hbm_color = CreateDIBSection(0, &bmi, DIB_RGB_COLORS, &mut bits, 0, 0);
        if hbm_color == 0 || bits.is_null() {
            return 0;
        }
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, 64 * 64 * 4);
        for (i, px) in rgba.chunks_exact(4).enumerate() {
            dst[i * 4] = px[2];
            dst[i * 4 + 1] = px[1];
            dst[i * 4 + 2] = px[0];
            dst[i * 4 + 3] = px[3];
        }
        let hbm_mask = CreateBitmap(64, 64, 1, 1, std::ptr::null());
        if hbm_mask == 0 {
            DeleteObject(hbm_color);
            return 0;
        }
        let ii = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: hbm_mask,
            hbmColor: hbm_color,
        };
        let hicon = CreateIconIndirect(&ii);
        DeleteObject(hbm_color);
        DeleteObject(hbm_mask);
        hicon
    }

    unsafe extern "system" fn tray_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DefWindowProcW, DestroyWindow, PostQuitMessage,
        };
        let tb = TASKBAR_CREATED_MSG.load(Ordering::Relaxed);
        if tb != 0 && msg == tb {
            add_tray_icon(hwnd);
            return 0;
        }
        match msg {
            WM_TRAY => {
                let event = (lparam as u32) & 0xFFFF;
                if event == WM_CONTEXTMENU {
                    show_tray_menu(hwnd);
                } else if event == NIN_SELECT {
                    crate::winvis::toggle();
                    push_action(TrayAction::Toggle);
                }
                0
            }
            WM_SS_SHUTDOWN => {
                notify_icon_delete(hwnd);
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                if let Some(sh) = shared() {
                    sh.hwnd_out.store(0, Ordering::SeqCst);
                }
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe fn show_tray_menu(hwnd: HWND) {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, DestroyMenu, DestroyWindow, GetCursorPos,
            PostMessageW, SetForegroundWindow, TrackPopupMenu, MF_GRAYED, MF_SEPARATOR,
            MF_STRING, TPM_RETURNCMD, TPM_RIGHTBUTTON,
        };

        let rebuilding = shared()
            .map(|s| s.rebuilding.load(Ordering::SeqCst))
            .unwrap_or(false);
        let menu = CreatePopupMenu();
        if menu == 0 {
            return;
        }
        let about_text = format!("关于瞬觅 v{}", env!("CARGO_PKG_VERSION"));
        let rebuild_text = if rebuilding {
            "正在建立索引…"
        } else {
            "重新建立索引"
        };
        let rebuild_flags = if rebuilding {
            MF_STRING | MF_GRAYED
        } else {
            MF_STRING
        };
        AppendMenuW(menu, MF_STRING, ID_SHOW, wide("打开瞬觅").as_ptr());
        AppendMenuW(menu, rebuild_flags, ID_REBUILD, wide(rebuild_text).as_ptr());
        AppendMenuW(menu, MF_STRING, ID_REFRESH, wide("立即刷新索引").as_ptr());
        AppendMenuW(menu, MF_STRING, ID_ADMIN, wide("以管理员身份重启").as_ptr());
        AppendMenuW(menu, MF_STRING, ID_SETTINGS, wide("设置").as_ptr());
        AppendMenuW(menu, MF_STRING, ID_ABOUT, wide(&about_text).as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, ID_EXIT, wide("退出").as_ptr());

        SetForegroundWindow(hwnd);
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        let cmd = {
            let _guard = crate::clickwatch::SuspendGuard::new();
            TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                pt.x,
                pt.y,
                0,
                hwnd,
                std::ptr::null(),
            )
        };
        PostMessageW(hwnd, 0, 0, 0); // WM_NULL
        DestroyMenu(menu);

        match cmd as usize {
            ID_SHOW => {
                crate::winvis::show();
                push_action(TrayAction::Show);
            }
            ID_REBUILD => {
                if !rebuilding {
                    if let Some(cb) = shared().and_then(|s| s.on_bg_action.as_ref()) {
                        cb(TrayBgAction::RebuildIndex);
                    } else {
                        push_action(TrayAction::RebuildIndex);
                    }
                }
            }
            ID_REFRESH => {
                if let Some(cb) = shared().and_then(|s| s.on_bg_action.as_ref()) {
                    cb(TrayBgAction::RefreshIndex);
                } else {
                    push_action(TrayAction::RefreshIndex);
                }
            }
            ID_ADMIN => {
                if let Some(cb) = shared().and_then(|s| s.on_bg_action.as_ref()) {
                    cb(TrayBgAction::AdminRestart);
                } else {
                    push_action(TrayAction::AdminRestart);
                }
            }
            ID_SETTINGS => {
                crate::winvis::show();
                push_action(TrayAction::OpenSettings);
            }
            ID_ABOUT => {
                crate::winvis::show();
                push_action(TrayAction::About);
            }
            ID_EXIT => {
                notify_icon_delete(hwnd);
                push_action(TrayAction::Exit);
                let main_h = crate::winvis::hwnd();
                if main_h != 0 {
                    PostMessageW(main_h as HWND, WM_CLOSE, 0, 0);
                }
                std::thread::Builder::new()
                    .name("shunseek-tray-exit-watchdog".into())
                    .spawn(|| {
                        std::thread::sleep(Duration::from_secs(2));
                        std::process::exit(0);
                    })
                    .ok();
                DestroyWindow(hwnd);
            }
            _ => {}
        }
    }
}
