//! 窗口可见性原生层（v0.1.13：隐藏改「移屏」让 UI 线程永不休眠）。
//!
//! ## 历史病根（v0.1.6–v0.1.11 连续六版真机唤回全灭）
//!
//! 旧链是「托盘/热键/唤醒线程置标志 + request_repaint → UI 线程醒来
//! → update() 消费队列 → 发 ViewportCommand::Visible/Minimized/Focus」。
//! 真机上凡是需要「隐藏状态下 UI 线程被唤醒并处理」的动作全死，凡是
//! 托盘线程自己独立完成的动作全活（退出、菜单弹出）——断点就在
//! repaint 唤醒这一环。
//!
//! ## v0.1.12 的修法与它的残留
//!
//! v0.1.12 把**可见性**收敛到发起线程直接调 ShowWindow：托盘线程点
//! 「打开」时自己 SW_SHOW，不再等 UI 醒来。这解决了「显示类」动作，
//! 但没解决「需要 UI 线程干活、又不需要显示窗口」的动作——托盘菜单
//! 的「重新建立索引」「立即刷新索引」「以管理员身份重启」三个动作
//! 只是 push_action 入队，窗口 SW_HIDE 时 UI 线程仍然停摆，队列照样
//! 没人消费，点了没反应。用户口径里的「托盘按键只有退出有效」，
//! 这三个就是残留部分。
//!
//! ## v0.1.13：隐藏不再让窗口不可见
//!
//! 根因是 SW_HIDE 本身：Windows 对**不可见窗口不再投递 WM_PAINT**，
//! winit 的 request_redraw 走 RedrawWindow(RDW_INTERNALPAINT) 对隐藏
//! 窗口同样无效 —— update() 整条停摆。所以只要还发 SW_HIDE，就永远
//! 有「某些队列动作没人消费」的洞要一个个打补丁。
//!
//! 改成**移到屏幕外 + 摘任务栏项**，WS_VISIBLE 自始至终为真：
//!  - WM_PAINT 照常投递 → update() 一直在跑 → 托盘/热键/单实例
//!    任何一次 push_action 都有人消费，不存在「点了没反应」。
//!  - 位置尺寸、GL surface 全程不动，不碰任何破坏 OpenGL 合成的
//!    扩展样式（WS_EX_LAYERED 会禁用 GL 渲染，不能用）。
//!  - 摘掉 WS_EX_TOOLWINDOW 任务栏项，用户看不到它，也 Alt+Tab 不到。
//!
//! 显示 = 移回原位 + 恢复任务栏项 + SW_RESTORE 解最小化 + 置前台。
//!
//! 隐藏时还必须**把前台让出去**：窗口只是挪到屏幕外，仍是活动窗口，
//! 不交前台的话用户在其他应用里打的字会进这个看不见的窗口。所以
//! hide() 末尾若自己是前台，就把前台交给桌面（GetShellWindow）。
//!
//! 最大化态要单独记：隐藏前若窗口是最大化，原位矩形是整屏，直接
//! 移回会得到一个「看起来最大化但其实不是」的窗口；hide 时记
//! WAS_MAX，show 时补一次 SW_MAXIMIZE 还原窗口状态。
//!
//! ## 状态口径
//!
//! state() 先读离屏标志（只有 hide/show 会写，不存在竞态歧义），
//! 再读 OS 的 IsIconic。UI 侧影子字段每帧同步它，只供绘制与武装
//! 判定，任何**决策**都在发起线程现读本模块。
//!
//! 非 Windows：无 Win32 可调，退化为影子原子量（演示环境口径），
//! 真实显隐由 main.rs 在同一调用点以 cfg 兜底的 ViewportCommand 完成。

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

/// 窗口可见状态（判定唯一来源，见模块头）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VisState {
    /// 已隐藏（托盘驻留态）。
    Hidden,
    /// 最小化到任务栏。
    Minimized,
    /// 正常显示（含被其他窗口遮挡）。
    Shown,
}

/// 现读当前状态。HWND 未存入（0）时返回 Shown——证明不了隐藏
/// 就按显示处理，绝不凭空把窗口判死。
pub fn state() -> VisState {
    platform::state()
}

/// 呼出：最小化态 → SW_RESTORE；离屏隐藏态 → 移回原位 + 恢复
/// 任务栏项（必要时补 SW_MAXIMIZE）；随后 SetForegroundWindow
/// 尝试拉到前台（前台锁定可能拒绝聚焦，但显示本身不受影响）。
/// 任意线程可调。
pub fn show() {
    platform::show();
}

/// 隐藏到托盘（移到屏幕外，见模块头）。任意线程可调。
pub fn hide() {
    platform::hide();
}

/// 钩子回调专用隐藏。与 hide() 同一路径：移屏用 SetWindowPos，
/// 是同步但极快的调用（远低于低级钩子的 300ms 超时预算），
/// 不需要 ShowWindowAsync 那套投递语义。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn hide_from_hook() {
    platform::hide();
}

/// 最小化（SW_MINIMIZE）。由 UI 线程内的最小化按钮调用。
pub fn minimize() {
    platform::minimize();
}

/// 显示/隐藏切换，决策现读当前状态：Shown → 隐藏；
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
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetShellWindow, GetWindowLongPtrW, GetWindowPlacement,
        GetWindowRect, IsIconic, IsZoomed, SetForegroundWindow, SetWindowLongPtrW,
        SetWindowPos,
        ShowWindow, GWL_EXSTYLE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_MAXIMIZE, SW_MINIMIZE,
        SW_RESTORE, SW_SHOWMAXIMIZED, WINDOWPLACEMENT,
        WS_EX_TOOLWINDOW,
    };

    /// 离屏坐标：远在任何多显示器布局之外（Win32 惯例值）。
    const OFFSCREEN_COORD: i32 = -32000;

    /// SW_SHOW 的裸值（windows-sys 的常量名在不同版本间变过，
    /// 直接用数值最稳：5 = SW_SHOW）。
    const SW_SHOW_PLAIN: i32 = 5;

    /// 是否处于「离屏隐藏」态。只有 hide/show 写它，读写都在
    /// 本模块内，语义单一：true = 用户眼里窗口是藏着的。
    static OFFSCREEN: AtomicBool = AtomicBool::new(false);
    /// 离屏前的屏幕位置（hide 首次进入时记录，show 移回）。
    static ORIG_X: AtomicI32 = AtomicI32::new(0);
    static ORIG_Y: AtomicI32 = AtomicI32::new(0);
    /// 隐藏前是否最大化（见模块头：移回后要补 SW_MAXIMIZE）。
    static WAS_MAX: AtomicBool = AtomicBool::new(false);

    pub fn state() -> VisState {
        let h = hwnd();
        if h == 0 {
            return VisState::Shown;
        }
        if OFFSCREEN.load(Ordering::SeqCst) {
            return VisState::Hidden;
        }
        unsafe {
            if IsIconic(h) != 0 {
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
        let was_offscreen = OFFSCREEN.swap(false, Ordering::SeqCst);
        let (x, y) = (
            ORIG_X.load(Ordering::SeqCst),
            ORIG_Y.load(Ordering::SeqCst),
        );
        let was_max = WAS_MAX.load(Ordering::SeqCst);
        unsafe {
            // 先解最小化：最小化态下 SetWindowPos 挪不动窗口。
            if IsIconic(h) != 0 {
                ShowWindow(h, SW_RESTORE);
            }
            if was_offscreen {
                // 恢复任务栏项（hide 时摘掉的）。
                let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
                SetWindowLongPtrW(h, GWL_EXSTYLE, ex & !(WS_EX_TOOLWINDOW as isize));
                SetWindowPos(
                    h,
                    0,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE
                        | SWP_FRAMECHANGED,
                );
                // 移回原位。窗口一直 WS_VISIBLE，这里只是挪回来。
                SetWindowPos(
                    h,
                    0,
                    x,
                    y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
                // 隐藏前是最大化：补一次 SW_MAXIMIZE，别留个
                // 「铺满屏幕但不是最大化态」的怪窗口。
                if was_max {
                    ShowWindow(h, SW_MAXIMIZE);
                }
            } else {
                ShowWindow(h, SW_SHOW_PLAIN);
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
            // 已最小化：屏幕上本来就没有它，状态由 state() 报
            // Minimized，托盘/热键 toggle 会 SW_RESTORE 呼出。这里
            // 若为了移屏先把它还原，用户会看到窗口闪一下再消失，
            // 所以直接不动——最小化本身就是「收起来了」。
            if IsIconic(h) != 0 {
                return;
            }
            // 只在第一次进入隐藏时记原位：重复 hide 不许把
            // -32000 存成「原位」，否则窗口再也回不来。
            if !OFFSCREEN.swap(true, Ordering::SeqCst) {
                // 用 GetWindowPlacement 的 rcNormalPosition 取「还原
                // 态矩形」：最大化时 GetWindowRect 给的是整屏，不是
                // 我们要还回去的位置。
                let mut wp: WINDOWPLACEMENT = std::mem::zeroed();
                wp.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
                let ok = GetWindowPlacement(h, &mut wp) != 0;
                WAS_MAX.store(ok && wp.showCmd == SW_SHOWMAXIMIZED as u32, Ordering::SeqCst);
                if ok {
                    ORIG_X.store(wp.rcNormalPosition.left, Ordering::SeqCst);
                    ORIG_Y.store(wp.rcNormalPosition.top, Ordering::SeqCst);
                } else {
                    // 取不到就退回当前矩形（还原态下两者一致）。
                    let mut rc: RECT = std::mem::zeroed();
                    if GetWindowRect(h, &mut rc) != 0 {
                        ORIG_X.store(rc.left, Ordering::SeqCst);
                        ORIG_Y.store(rc.top, Ordering::SeqCst);
                    }
                }
                // 最大化窗口的位置由系统管，SetWindowPos 挪不动，
                // 必须先回还原态。同步两步，紧接着移屏，中间态
                // 用户看不到。
                if IsZoomed(h) != 0 {
                    ShowWindow(h, SW_RESTORE);
                }
            }
            // WS_EX_TOOLWINDOW：任务栏与 Alt+Tab 都不再列它。
            // 扩展样式改动必须跟一次 SWP_FRAMECHANGED 才生效。
            let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
            SetWindowLongPtrW(h, GWL_EXSTYLE, ex | (WS_EX_TOOLWINDOW as isize));
            SetWindowPos(
                h,
                0,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE
                    | SWP_FRAMECHANGED,
            );
            // 移到屏幕外。SWP_SHOWWINDOW 保证 WS_VISIBLE 保持为真
            // ——这是整条修法的支点：窗口可见，WM_PAINT 才照发，
            // update() 才不会停摆（见模块头）。
            SetWindowPos(
                h,
                0,
                OFFSCREEN_COORD,
                OFFSCREEN_COORD,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            // 让出前台：窗口只是挪走，仍是活动窗口；不交出去的话
            // 用户在其他应用里打的字会进这个看不见的窗口。交给
            // 桌面窗口（GetShellWindow）是最不打扰的落点。
            if GetForegroundWindow() == h {
                let shell = GetShellWindow();
                if shell != 0 {
                    SetForegroundWindow(shell);
                }
            }
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

    pub fn minimize() {
        MINIMIZED.store(true, Ordering::SeqCst);
    }
}
