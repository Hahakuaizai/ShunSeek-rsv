//! 系统托盘：搜索窗平时隐藏，托盘是它的常驻入口与（v0.1.6 起唯一的）
//! 正常退出出口——窗口 ✕ 只隐藏到托盘，进程只能从这里「退出」。
//!
//! 菜单只放快捷动作，不放任何开关——
//!   打开瞬觅 / 重新建立索引 / 设置 / 关于瞬觅 vX.Y.Z / 退出
//! 开关类（主题、宫格、自启动、热键等）一律在应用内「设置」窗口里。
//! 图标左键单击 = 显示/隐藏切换。「重新建立索引」在重建中置灰 +
//! 文案「正在建立索引…」，菜单每次打开时按共享原子量现拼，状态永远新鲜。
//!
//! 架构（v0.1.6 结构性重写，弃 tray-icon / muda）：
//! v0.1.5 的全局事件 handler 方案在真机失效——窗口隐藏后菜单动作到不了
//! update()，托盘退不出、任务栏又没有窗口（旧版 with_taskbar(false)），
//! 用户只能任务管理器强杀。新版改专职托盘线程 + 原生 Win32：
//!  - 托盘线程自建隐藏消息窗口（HWND_MESSAGE）、Shell_NotifyIconW 加图标、
//!    跑自己的 GetMessage 循环；右键菜单用 CreatePopupMenu/TrackPopupMenu
//!    （TPM_RETURNCMD 同步返回所选项），全部在托盘线程内处理，不依赖
//!    UI 线程的事件循环是否在转。
//!  - 与 UI 通信只走既有 TrayAction 队列 + request_repaint（唤醒 UI 这条
//!    已被热键线程在真机证明有效）。
//!  - **退出独立于 UI 线程**：托盘线程收到「退出」时先自己 NIM_DELETE
//!    撤图标，再入队 Exit + 唤醒，并起 2 秒看门狗线程——到时进程还活着
//!    就 std::process::exit(0)。UI 侧关窗/看门狗逻辑保留作第二道保险。
//!
//! 非 Windows：没有托盘（演示环境），保留骨架（接口一致，poll 恒空），
//! 应用其余部分不感知平台差异。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

// 非 Windows 骨架下多数变体无人构造（poll 恒空），故 allow(dead_code)
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayAction {
    /// 图标左键单击：显示/隐藏切换。
    Toggle,
    /// 菜单「打开瞬觅」：只显示（已显示则聚焦），不切换。
    Show,
    /// 菜单「重新建立索引」。
    RebuildIndex,
    /// 菜单「设置」：显示主窗口并打开设置窗口。
    OpenSettings,
    /// 菜单「关于瞬觅」。
    About,
    /// 菜单「退出」：进程唯一的正常退出路径。
    Exit,
}

/// 建托盘时 App 传入的初始状态（重建项置灰按真实状态起步）。
pub struct TrayInit {
    pub rebuilding: bool,
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
        let inner = win::WinTray::create(ctx, actions.clone(), &init);
        #[cfg(not(windows))]
        let _ = (&ctx, &init); // 骨架：不创建任何系统托盘对象
        Tray {
            actions,
            #[cfg(windows)]
            inner,
        }
    }

    /// 取走一个待处理动作（UI 每帧调用）。锁中毒时恢复内部值继续用，
    /// 绝不因一次 panic 让托盘动作从此全灭（与搜索冻结同类根治）。
    pub fn poll(&mut self) -> Option<TrayAction> {
        let mut q = self
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        q.pop_front()
    }

    /// UI 侧退出流程调用：请托盘线程撤图标并收尾（异步投递消息，
    /// 托盘线程自己的循环里执行 NIM_DELETE）。
    pub fn shutdown(&mut self) {
        #[cfg(windows)]
        if let Some(inner) = &self.inner {
            inner.shutdown();
        }
    }

    /// 重建进行中：菜单项置灰 + 文案换成「正在建立索引…」，完成恢复。
    /// 只写共享原子量；菜单下次打开时读取，无需跨线程同步句柄。
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
    use super::{TrayAction, TrayInit};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

    /// 托盘回调消息（Shell → 消息窗口）。
    const WM_TRAY: u32 = 0x8000 + 1; // WM_APP + 1
    /// UI 请求托盘线程收尾（撤图标 + 结束循环）。
    const WM_SS_SHUTDOWN: u32 = 0x8000 + 2; // WM_APP + 2
    const WM_DESTROY: u32 = 0x0002;
    const WM_CONTEXTMENU: u32 = 0x007B;
    /// NOTIFYICON_VERSION_4 下左键激活事件。
    const NIN_SELECT: u32 = 0x0400;

    // Shell_NotifyIconW 消息码
    const NIM_ADD: u32 = 0;
    const NIM_DELETE: u32 = 2;
    const NIM_SETVERSION: u32 = 4;

    // 菜单项 ID
    const ID_SHOW: usize = 1;
    const ID_REBUILD: usize = 2;
    const ID_SETTINGS: usize = 3;
    const ID_ABOUT: usize = 4;
    const ID_EXIT: usize = 5;

    /// 托盘线程与 UI 的共享状态：动作队列 + 唤醒用的 Context + 重建标志。
    struct TrayShared {
        actions: Arc<Mutex<VecDeque<TrayAction>>>,
        ctx: egui::Context,
        rebuilding: AtomicBool,
    }

    /// wndproc 是 extern 回调拿不到捕获，只能走进程级静态共享。
    /// 托盘全程只建一次，OnceLock 正合用。
    static SHARED: OnceLock<Arc<TrayShared>> = OnceLock::new();

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
        /// 托盘消息窗口句柄（托盘线程写入；0 = 尚未建好/已销毁）。
        hwnd: Arc<AtomicIsize>,
        rebuilding: Arc<AtomicBool>,
    }

    impl WinTray {
        pub fn create(
            ctx: egui::Context,
            actions: Arc<Mutex<VecDeque<TrayAction>>>,
            init: &TrayInit,
        ) -> Option<WinTray> {
            let rebuilding = Arc::new(AtomicBool::new(init.rebuilding));
            let shared_state = Arc::new(TrayShared {
                actions,
                ctx,
                rebuilding: AtomicBool::new(init.rebuilding),
            });
            if SHARED.set(shared_state).is_err() {
                return None; // 只建一次；重复调用直接放弃
            }
            let hwnd = Arc::new(AtomicIsize::new(0));
            let hwnd_thread = hwnd.clone();
            let rebuilding_thread = rebuilding.clone();
            let spawned = std::thread::Builder::new()
                .name("shunseek-tray".into())
                .spawn(move || {
                    tray_thread_main(hwnd_thread, rebuilding_thread);
                });
            match spawned {
                Ok(_) => Some(WinTray { hwnd, rebuilding }),
                Err(_) => None,
            }
        }

        pub fn set_rebuilding(&self, rebuilding: bool) {
            self.rebuilding.store(rebuilding, Ordering::SeqCst);
            if let Some(sh) = shared() {
                sh.rebuilding.store(rebuilding, Ordering::SeqCst);
            }
        }

        pub fn shutdown(&self) {
            let hwnd = self.hwnd.load(Ordering::SeqCst);
            if hwnd != 0 {
                unsafe {
                    PostMessageW(hwnd, WM_SS_SHUTDOWN, 0, 0);
                }
            }
        }
    }

    /// UTF-16 NUL 结尾串。
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 托盘线程主体：建消息窗口 → 加图标 → 消息循环。
    /// 任何一步失败就结束线程（无托盘可用，但应用本体照常运行）。
    fn tray_thread_main(hwnd_out: Arc<AtomicIsize>, _rebuilding: Arc<AtomicBool>) {
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, DispatchMessageW, GetMessageW, RegisterClassW,
            TranslateMessage, MSG, WNDCLASSW,
        };

        unsafe {
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
            // 已注册过会返回 0，不视为致命错误（窗口仍能建）
            let _ = RegisterClassW(&wc);
            // HWND_MESSAGE (-3) 作父窗口 = 消息专用窗口（不可见、无任务栏）
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

            let hicon = make_app_hicon();
            if !notify_icon(hwnd, NIM_ADD, hicon) {
                DestroyWindow(hwnd);
                return;
            }
            // 版本 4 语义：右键 WM_CONTEXTMENU、左键 NIN_SELECT，坐标可靠
            notify_icon_version4(hwnd);

            let mut msg: MSG = std::mem::zeroed();
            loop {
                let r = GetMessageW(&mut msg, 0, 0, 0);
                if r <= 0 {
                    break; // WM_QUIT 或出错
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    /// 填充 NOTIFYICONDATAW 并调用 Shell_NotifyIconW。
    /// hicon 仅 NIM_ADD 时需要，其余传 0。
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

    /// assets/icon-64.rgba（64×64 原始 RGBA）→ HICON：
    /// 32bpp 自上而下 DIB（BGRA）作颜色位图 + 单色掩码，CreateIconIndirect。
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
        bmi.bmiHeader.biHeight = -64; // 负高 = 自上而下，与 RGBA 字节序一致
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let hbm_color = CreateDIBSection(
            0,
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            0,
            0,
        );
        if hbm_color == 0 || bits.is_null() {
            return 0;
        }
        // RGBA → BGRA 逐像素写入 DIB
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

    /// 托盘窗口过程：只处理托盘回调与收尾消息，其余走默认过程。
    unsafe extern "system" fn tray_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DefWindowProcW, DestroyWindow, PostQuitMessage,
        };
        match msg {
            WM_TRAY => {
                let event = (lparam as u32) & 0xFFFF;
                if event == WM_CONTEXTMENU {
                    show_tray_menu(hwnd);
                } else if event == NIN_SELECT {
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
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// 右键菜单：每次打开现拼（重建项状态读共享原子量），
    /// TPM_RETURNCMD 在本线程同步返回所选项，就地分发。
    unsafe fn show_tray_menu(hwnd: HWND) {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, DestroyMenu, DestroyWindow, GetCursorPos,
            PostMessageW, SetForegroundWindow, TrackPopupMenu, MF_GRAYED, MF_SEPARATOR,
            MF_STRING, TPM_RETURNCMD, TPM_RIGHTBUTTON,
        };

        let rebuilding = shared().map(|s| s.rebuilding.load(Ordering::SeqCst)).unwrap_or(false);
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
        AppendMenuW(menu, MF_STRING, ID_SETTINGS, wide("设置").as_ptr());
        AppendMenuW(menu, MF_STRING, ID_ABOUT, wide(&about_text).as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, ID_EXIT, wide("退出").as_ptr());

        // 经典要求：跟踪前先把宿主窗口设为前台，菜单才会在点外时正常收起
        SetForegroundWindow(hwnd);
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            hwnd,
            std::ptr::null(),
        );
        // 配合 SetForegroundWindow 的收尾消息，保证菜单状态机干净关闭
        PostMessageW(hwnd, 0, 0, 0); // WM_NULL
        DestroyMenu(menu);

        match cmd as usize {
            ID_SHOW => push_action(TrayAction::Show),
            ID_REBUILD => {
                if !rebuilding {
                    push_action(TrayAction::RebuildIndex);
                }
            }
            ID_SETTINGS => push_action(TrayAction::OpenSettings),
            ID_ABOUT => push_action(TrayAction::About),
            ID_EXIT => {
                // 退出独立于 UI 线程：先自撤图标，再入队 + 唤醒，
                // 并起 2 秒看门狗——UI 若没能处理 Exit，进程也必退。
                notify_icon_delete(hwnd);
                push_action(TrayAction::Exit);
                std::thread::Builder::new()
                    .name("shunseek-tray-exit-watchdog".into())
                    .spawn(|| {
                        std::thread::sleep(std::time::Duration::from_secs(2));
                        std::process::exit(0);
                    })
                    .ok();
                DestroyWindow(hwnd);
            }
            _ => {}
        }
    }
}
