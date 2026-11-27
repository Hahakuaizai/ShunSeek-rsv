//! 窗口可见性原生层：UI 线程子类化 + 原生 SW_HIDE / SW_SHOW。
//!
//! ## 架构设计
//!
//! 1. 主窗口 HWND 与 egui::Context 在启动时存入全局原子量 / OnceLock。
//! 2. Windows 上通过 `SetWindowLongPtrW(GWLP_WNDPROC)` 安装轻量级子类过程
//!    `subclass_wndproc`，所有跨线程的显隐切换（托盘、全局热键、全局点击钩子、
//!    单实例唤醒）一律通过 `PostMessageW(hwnd, WM_SS_VIS, cmd, 0)` 异步投递给
//!    UI 线程在自己的消息循环里串行执行，彻底杜绝跨线程同步 SendMessage 死锁、
//!    低级鼠标钩子超时以及多线程原子状态竞态。
//! 3. 隐藏使用真正的 `ShowWindow(h, SW_HIDE)`，使窗口、任务栏按钮与 Alt+Tab
//!    条目同步干净消失；显示时先 `SW_RESTORE` / `SW_SHOW`，再通过
//!    `HWND_TOPMOST` -> `HWND_NOTOPMOST` + `AttachThreadInput` +
//!    `BringWindowToTop` + `SetForegroundWindow` 稳妥拉回前台并触发重绘。

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::OnceLock;

/// 主窗口 HWND（数值形式；0 = 尚未存入 / 取不到 / 非 Windows）。
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
static EGUI_CTX: OnceLock<egui::Context> = OnceLock::new();

/// 存入主窗口 HWND（必须在 UI 线程调用；仅首次非 0 调用会安装子类 WndProc）。
pub fn store_hwnd(hwnd: isize) {
    if hwnd != 0 {
        MAIN_HWND.store(hwnd, Ordering::SeqCst);
        platform::on_store_hwnd(hwnd);
    }
}

/// 存入 egui::Context，供窗口显示后主动触发一帧重绘以补齐 UI 副作用。
pub fn store_ctx(ctx: egui::Context) {
    let _ = EGUI_CTX.set(ctx);
}

/// 当前已存入的主窗口 HWND（0 = 无）。
pub fn hwnd() -> isize {
    MAIN_HWND.load(Ordering::SeqCst)
}

/// 窗口可见状态（判定唯一来源，直接查询 OS）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VisState {
    /// 已隐藏（托盘驻留态）。
    Hidden,
    /// 最小化到任务栏。
    Minimized,
    /// 正常显示（含被其他窗口遮挡）。
    Shown,
}

/// 现读当前状态。HWND 未存入（0）时返回 Shown。
pub fn state() -> VisState {
    platform::state()
}

/// 窗口是否处于系统最大化状态。
pub fn is_maximized() -> bool {
    platform::is_maximized()
}

/// 查询主窗口当前在屏幕上的物理像素矩形 `[left, top, right, bottom]`。
pub fn window_rect() -> Option<[i32; 4]> {
    platform::window_rect()
}

/// 呼出并拉到前台（最小化则还原）。任意线程可调。
pub fn show() {
    crate::clickwatch::note_shown();
    platform::show();
}

/// 隐藏到托盘（SW_HIDE）。任意线程可调。
pub fn hide() {
    crate::clickwatch::disarm();
    platform::hide();
}

/// 钩子回调专用隐藏：先立即撤防，再异步 PostMessage 给 UI 线程隐藏，微秒返回。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn hide_from_hook() {
    crate::clickwatch::disarm();
    platform::hide();
}

/// 最小化到任务栏（SW_MINIMIZE）。
pub fn minimize() {
    crate::clickwatch::disarm();
    platform::minimize();
}

/// 显示/隐藏切换：Shown -> 隐藏；Minimized / Hidden -> 呼出。任意线程可调。
pub fn toggle() {
    platform::toggle();
}

#[cfg(windows)]
mod platform {
    use super::{hwnd, VisState, EGUI_CTX};
    use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::InvalidateRect;
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, CallWindowProcW, GetForegroundWindow, GetWindowRect,
        GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed, PostMessageW,
        SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWLP_WNDPROC,
        SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, SW_MINIMIZE, SW_RESTORE, SW_SHOW,
        WNDPROC,
    };

    const WM_SS_VIS: u32 = 0x8000 + 0x41; // WM_APP + 0x41
    const CMD_HIDE: usize = 1;
    const CMD_SHOW: usize = 2;
    const CMD_TOGGLE: usize = 3;
    const CMD_MINIMIZE: usize = 4;

    static SUBCLASSED: AtomicBool = AtomicBool::new(false);
    static PREV_WNDPROC: AtomicIsize = AtomicIsize::new(0);
    static UI_THREAD_ID: AtomicU32 = AtomicU32::new(0);

    pub fn on_store_hwnd(h: isize) {
        if SUBCLASSED.swap(true, Ordering::SeqCst) {
            return;
        }
        unsafe {
            UI_THREAD_ID.store(GetCurrentThreadId(), Ordering::SeqCst);
            let prev = SetWindowLongPtrW(
                h as HWND,
                GWLP_WNDPROC,
                subclass_wndproc as *const () as isize,
            );
            PREV_WNDPROC.store(prev, Ordering::SeqCst);
        }
    }

    unsafe extern "system" fn subclass_wndproc(
        h: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_SS_VIS {
            exec_cmd(h, wparam);
            return 0;
        }
        let prev = PREV_WNDPROC.load(Ordering::SeqCst);
        if prev != 0 {
            let proc_fn: WNDPROC = Some(std::mem::transmute::<
                isize,
                unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
            >(prev));
            CallWindowProcW(proc_fn, h, msg, wparam, lparam)
        } else {
            0
        }
    }

    fn dispatch_cmd(cmd: usize) {
        let h = hwnd();
        if h == 0 {
            return;
        }
        let ui_tid = UI_THREAD_ID.load(Ordering::SeqCst);
        let cur_tid = unsafe { GetCurrentThreadId() };
        if ui_tid != 0 && cur_tid == ui_tid {
            unsafe { exec_cmd(h as HWND, cmd) };
        } else {
            unsafe {
                PostMessageW(h as HWND, WM_SS_VIS, cmd, 0);
            }
        }
    }

    unsafe fn exec_cmd(h: HWND, cmd: usize) {
        match cmd {
            CMD_HIDE => {
                crate::clickwatch::disarm();
                if IsIconic(h) == 0 {
                    ShowWindow(h, SW_HIDE);
                }
            }
            CMD_SHOW => {
                crate::clickwatch::note_shown();
                if IsIconic(h) != 0 {
                    ShowWindow(h, SW_RESTORE);
                } else {
                    ShowWindow(h, SW_SHOW);
                }
                // 破除 Z 序沉底，同时不长期置顶
                SetWindowPos(
                    h,
                    -1isize as HWND, // HWND_TOPMOST
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
                );
                SetWindowPos(
                    h,
                    -2isize as HWND, // HWND_NOTOPMOST
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
                );
                let fg = GetForegroundWindow();
                let fg_tid = if fg != 0 {
                    GetWindowThreadProcessId(fg, std::ptr::null_mut())
                } else {
                    0
                };
                let cur_tid = GetCurrentThreadId();
                let attached = fg_tid != 0 && fg_tid != cur_tid
                    && AttachThreadInput(fg_tid, cur_tid, 1) != 0;
                BringWindowToTop(h);
                SetForegroundWindow(h);
                if attached {
                    AttachThreadInput(fg_tid, cur_tid, 0);
                }
                InvalidateRect(h, std::ptr::null(), 0);
                if let Some(ctx) = EGUI_CTX.get() {
                    ctx.request_repaint();
                }
            }
            CMD_TOGGLE => {
                if IsWindowVisible(h) == 0 || IsIconic(h) != 0 {
                    exec_cmd(h, CMD_SHOW);
                } else {
                    exec_cmd(h, CMD_HIDE);
                }
            }
            CMD_MINIMIZE => {
                crate::clickwatch::disarm();
                ShowWindow(h, SW_MINIMIZE);
            }
            _ => {}
        }
    }

    pub fn state() -> VisState {
        let h = hwnd();
        if h == 0 {
            return VisState::Shown;
        }
        unsafe {
            if IsWindowVisible(h as HWND) == 0 {
                VisState::Hidden
            } else if IsIconic(h as HWND) != 0 {
                VisState::Minimized
            } else {
                VisState::Shown
            }
        }
    }

    pub fn is_maximized() -> bool {
        let h = hwnd();
        if h == 0 {
            return false;
        }
        unsafe { IsZoomed(h as HWND) != 0 }
    }

    pub fn window_rect() -> Option<[i32; 4]> {
        let h = hwnd();
        if h == 0 {
            return None;
        }
        unsafe {
            let mut rc: RECT = std::mem::zeroed();
            if GetWindowRect(h as HWND, &mut rc) != 0 && rc.right > rc.left && rc.bottom > rc.top
            {
                Some([rc.left, rc.top, rc.right, rc.bottom])
            } else {
                None
            }
        }
    }

    pub fn show() {
        dispatch_cmd(CMD_SHOW);
    }

    pub fn hide() {
        dispatch_cmd(CMD_HIDE);
    }

    pub fn minimize() {
        dispatch_cmd(CMD_MINIMIZE);
    }

    pub fn toggle() {
        dispatch_cmd(CMD_TOGGLE);
    }
}

#[cfg(not(windows))]
mod platform {
    use super::VisState;
    use std::sync::atomic::{AtomicBool, Ordering};

    static VISIBLE: AtomicBool = AtomicBool::new(true);
    static MINIMIZED: AtomicBool = AtomicBool::new(false);

    pub fn on_store_hwnd(_h: isize) {}

    pub fn state() -> VisState {
        if !VISIBLE.load(Ordering::SeqCst) {
            VisState::Hidden
        } else if MINIMIZED.load(Ordering::SeqCst) {
            VisState::Minimized
        } else {
            VisState::Shown
        }
    }

    pub fn is_maximized() -> bool {
        false
    }

    pub fn window_rect() -> Option<[i32; 4]> {
        None
    }

    pub fn show() {
        VISIBLE.store(true, Ordering::SeqCst);
        MINIMIZED.store(false, Ordering::SeqCst);
    }

    pub fn hide() {
        VISIBLE.store(false, Ordering::SeqCst);
    }

    pub fn minimize() {
        MINIMIZED.store(true, Ordering::SeqCst);
    }

    pub fn toggle() {
        match state() {
            VisState::Shown => hide(),
            VisState::Minimized | VisState::Hidden => show(),
        }
    }
}
