//! 窗口可见性原生层（v0.1.12）：主窗口的显示 / 隐藏 / 最小化 /
//! 还原全部走 Win32 ShowWindow，由**发起线程当场执行**，不再
//! 排队等 UI 线程在 update() 里消费 ViewportCommand。
//!
//! 为什么整层换掉（v0.1.6–v0.1.11 连续六版真机唤回全灭的统一
//! 病根）：旧链是「托盘/热键/唤醒线程置标志 + request_repaint
//! → UI 线程醒来 → update() 消费队列 → 发 ViewportCommand::
//! Visible/Minimized/Focus」。真机上凡是需要「隐藏状态下的 UI
//! 线程被唤醒并处理」的动作全死（托盘打开/设置/热键呼出/单
//! 实例唤醒），凡是托盘线程自己独立完成的动作全活（退出、
//! 菜单弹出）——断点就在 repaint 唤醒这一环。原版 .NET 自始
//! 至终用 ShowWindow 直接操作 HWND，根本不依赖 UI 线程唤醒，
//! 本模块就是把可见性收敛到那条路上：
//!  - HWND 在 App 创建时从 eframe 的 window_handle 存入全局
//!    原子量（见 main.rs ShunSeekApp::new，另有首帧 Frame 兜底
//!    重取），托盘 / 热键 / 钩子 / 唤醒四个线程共享读。
//!  - 状态以 OS 为准：visible/minimized 的判定读
//!    IsWindowVisible / IsIconic，不读任何影子标志。UI 侧的
//!    影子字段只是每帧从这里同步的缓存（绘制与武装判定用），
//!    托盘/热键路径的决策一律现读本模块的 state()。
//!  - ShowWindow 跨线程调用合法：发起线程（托盘/热键/唤醒）
//!    直接调，窗口的显示状态当场改变，与 UI 线程是否活着、
//!    是否及时处理 repaint 完全无关。egui 侧的 repaint 从此
//!    只负责「窗口已显示后补一帧副作用」（重搜/全选/聚焦请求），
//!    唤不醒也只丢副作用，绝不丢可见性。
//!  - 钩子回调里用 ShowWindowAsync：它就是为「非属主线程发起
//!    显示变更」设计的，投递即返回，守住低级钩子的超时纪律
//!    （见 clickwatch 模块头）；效果与 ShowWindow 等价（属主
//!    线程的消息泵一直在转，请求立即被处理）。
//!
//! 非 Windows：无 Win32 可调，platform 退化为一对影子原子量
//! （演示环境口径），真实的窗口显隐由 main.rs 在同一调用点
//! 以 cfg 兜底的 ViewportCommand 完成——那是演示平台的机制，
//! 不进入 Windows 链。

use std::sync::atomic::{AtomicIsize, Ordering};

/// 主窗口 HWND（数值形式；0 = 尚未存入 / 取不到 / 非 Windows）。
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

/// 存入主窗口 HWND（UI 线程在 App 创建时调用，取自 eframe 的
/// window_handle；首帧若仍为 0 会再从 Frame 重取一次）。
pub fn store_hwnd(hwnd: isize) {
    if hwnd != 0 {
        MAIN_HWND.store(hwnd, Ordering::SeqCst);
    }
}

/// 当前已存入的主窗口 HWND（0 = 无）。
pub fn hwnd() -> isize {
    MAIN_HWND.load(Ordering::SeqCst)
}

/// OS 口径的窗口可见状态（判定唯一来源，见模块头）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VisState {
    /// 已隐藏（托盘驻留态）。
    Hidden,
    /// 最小化到任务栏（Win32 语义下仍属可见）。
    Minimized,
    /// 正常显示（含被其他窗口遮挡）。
    Shown,
}

/// 现读 OS 状态。HWND 未存入（0）时返回 Shown——证明不了隐藏
/// 就按显示处理，绝不凭空把窗口判死（此时可见性仍由 UI 侧的
/// 旧命令兜底，见 main.rs 的 cfg 落点）。
pub fn state() -> VisState {
    platform::state()
}

/// 呼出：最小化态 → SW_RESTORE 还原；其余 → SW_SHOW 显示，
/// 随后 SetForegroundWindow 尝试拉到前台（前台锁定可能拒绝
/// 聚焦，但显示本身不受影响——这正是与旧链的分界：可见性
/// 不再陪聚焦一起死）。任意线程可调。
pub fn show() {
    platform::show();
}

/// 隐藏到托盘（SW_HIDE）。任意线程可调。
pub fn hide() {
    platform::hide();
}

/// 钩子回调专用隐藏：ShowWindowAsync 投递即返回（见模块头）。
/// 非 Windows 无钩子、无调用点，故 allow(dead_code)。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn hide_from_hook() {
    platform::hide_from_hook();
}

/// 最小化（SW_MINIMIZE）。由 UI 线程内的最小化按钮调用。
pub fn minimize() {
    platform::minimize();
}

/// 显示/隐藏切换，决策现读 OS 状态：Shown → 隐藏；
/// Minimized / Hidden → 呼出。托盘左键与热键线程直接调，
/// 不经过 UI 队列。
pub fn toggle() {
    match state() {
        VisState::Shown => hide(),
        VisState::Minimized | VisState::Hidden => show(),
    }
}

#[cfg(windows)]
mod platform {
    use super::{hwnd, VisState};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IsIconic, IsWindowVisible, SetForegroundWindow, ShowWindow, ShowWindowAsync,
        SW_HIDE, SW_MINIMIZE, SW_RESTORE, SW_SHOW,
    };

    pub fn state() -> VisState {
        let h = hwnd();
        if h == 0 {
            return VisState::Shown;
        }
        unsafe {
            if IsWindowVisible(h) == 0 {
                VisState::Hidden
            } else if IsIconic(h) != 0 {
                VisState::Minimized
            } else {
                VisState::Shown
            }
        }
    }

    pub fn show() {
        let h = hwnd();
        if h == 0 {
            return;
        }
        unsafe {
            if IsIconic(h) != 0 {
                ShowWindow(h, SW_RESTORE);
            } else {
                ShowWindow(h, SW_SHOW);
            }
            // 拉前台：调用方是刚处理完用户输入的线程（托盘点击/
            // 热键/第二进程唤醒）时系统通常放行；被前台锁定拒绝
            // 也只是焦点没抢到，窗口已经显示，不回滚。
            SetForegroundWindow(h);
        }
    }

    pub fn hide() {
        let h = hwnd();
        if h == 0 {
            return;
        }
        unsafe {
            ShowWindow(h, SW_HIDE);
        }
    }

    pub fn hide_from_hook() {
        let h = hwnd();
        if h == 0 {
            return;
        }
        unsafe {
            ShowWindowAsync(h, SW_HIDE);
        }
    }

    pub fn minimize() {
        let h = hwnd();
        if h == 0 {
            return;
        }
        unsafe {
            ShowWindow(h, SW_MINIMIZE);
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::VisState;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// 演示平台的影子状态（见模块头）：与 main.rs 的 cfg 兜底
    /// ViewportCommand 保持同义，App 的每帧 OS 同步在非
    /// Windows 上就读这里。
    static VISIBLE: AtomicBool = AtomicBool::new(true);
    static MINIMIZED: AtomicBool = AtomicBool::new(false);

    pub fn state() -> VisState {
        if !VISIBLE.load(Ordering::SeqCst) {
            VisState::Hidden
        } else if MINIMIZED.load(Ordering::SeqCst) {
            VisState::Minimized
        } else {
            VisState::Shown
        }
    }

    pub fn show() {
        VISIBLE.store(true, Ordering::SeqCst);
        MINIMIZED.store(false, Ordering::SeqCst);
    }

    pub fn hide() {
        VISIBLE.store(false, Ordering::SeqCst);
    }

    pub fn hide_from_hook() {
        hide();
    }

    pub fn minimize() {
        MINIMIZED.store(true, Ordering::SeqCst);
    }
}
