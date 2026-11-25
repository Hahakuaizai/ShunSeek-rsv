//! 全局点外隐藏（v0.1.10，照原版 .NET GlobalClickWatcher 的机制）：
//! WH_MOUSE_LL 低级鼠标钩子装在专职线程上（带自己的消息泵——
//! 低级钩子靠安装线程的消息队列投递），全局左/右/中键按下时，
//! 落点不在主窗口与设置窗口矩形内 → 置位「点外」标志并
//! request_repaint，UI 线程下一帧 hide()。点哪还是哪，钩子
//! 从不吞点击（永远 CallNextHookEx 放行）。
//!
//! 为什么不用失焦事件（v0.1.6–v0.1.9 的老路）：egui 的
//! viewport.focused 是 winit 焦点状态，点任务栏/托盘/开始菜单
//! 等系统外壳区域时焦点语义混乱，由此衍生出最小化互斥、恢复
//! 宽限期、指针守卫一摞补丁，真机仍然「点别处就消失且唤不回」。
//! 原版自始至终用的是几何判定：点击坐标 vs 窗口矩形，没有
//! 任何焦点推断——本模块就是把它搬过来，焦点从此不参与
//! 隐藏判定。
//!
//! 钩子纪律（低级钩子超时会被系统静默摘除）：
//!  - 回调里只做：读 MSLLHOOKSTRUCT 坐标 → 读几个原子量做
//!    矩形包含判断 → 置位 + request_repaint。不分配、不加锁、
//!    不做任何 I/O。
//!  - 矩形由 UI 线程每帧以「物理像素」发布（egui 的 outer_rect
//!    是监控器空间的点坐标 × native_pixels_per_point 换算），
//!    回调只读原子量，不回调 UI。
//!  - 主窗口矩形取不到（outer_rect 为 None）时 UI 侧直接把
//!    armed 发成 false——宁可不藏，不可乱藏。
//!  - 托盘菜单打开期间由 tray.rs 调 set_suspended(true) 挂起：
//!    菜单本身在两个窗口矩形之外，点菜单条目不该顺带把主窗藏掉。
//!  - 托盘图标矩形是第三个豁免区（v0.1.11）：点托盘图标本身
//!    （左键 Toggle、右键开菜单）永远不算「点外」——否则图标
//!    点击攒下的点外标志会在同一帧反杀托盘的显式呼出（真机
//!    实测：托盘只有「退出」起作用，「打开」永远打不开）。
//!
//! 非 Windows：无钩子（演示环境），接口同形、恒无事件。

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, OnceLock};

/// 一个窗口矩形（物理像素，[left, top, right, bottom]）的原子发布槽。
struct RectSlot {
    valid: AtomicBool,
    l: AtomicI32,
    t: AtomicI32,
    r: AtomicI32,
    b: AtomicI32,
}

impl RectSlot {
    fn new() -> RectSlot {
        RectSlot {
            valid: AtomicBool::new(false),
            l: AtomicI32::new(0),
            t: AtomicI32::new(0),
            r: AtomicI32::new(0),
            b: AtomicI32::new(0),
        }
    }

    fn publish(&self, rect: Option<[i32; 4]>) {
        match rect {
            Some([l, t, r, b]) if r > l && b > t => {
                self.l.store(l, Ordering::SeqCst);
                self.t.store(t, Ordering::SeqCst);
                self.r.store(r, Ordering::SeqCst);
                self.b.store(b, Ordering::SeqCst);
                self.valid.store(true, Ordering::SeqCst);
            }
            _ => self.valid.store(false, Ordering::SeqCst),
        }
    }

    fn contains(&self, x: i32, y: i32) -> bool {
        self.valid.load(Ordering::SeqCst)
            && x >= self.l.load(Ordering::SeqCst)
            && x < self.r.load(Ordering::SeqCst)
            && y >= self.t.load(Ordering::SeqCst)
            && y < self.b.load(Ordering::SeqCst)
    }
}

struct Shared {
    ctx: egui::Context,
    /// 主窗口当前可见才武装（隐藏时点外点击毫无意义，还会攒下
    /// 陈旧标志坑下一次呼出——UI 侧隐藏时也会主动 drain）。
    armed: AtomicBool,
    /// 托盘菜单打开期间挂起（见模块头）。
    suspended: AtomicBool,
    main_rect: RectSlot,
    sub_rect: RectSlot,
    /// 托盘图标矩形（v0.1.11 第三个豁免区，见模块头）。
    tray_rect: RectSlot,
    outside_pending: AtomicBool,
}

/// 进程级共享（钩子回调是 extern "system" 裸函数，拿不到捕获，
/// 与 tray.rs 同款 OnceLock 静态）。全程只建一次。
static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

fn shared() -> Option<&'static Arc<Shared>> {
    SHARED.get()
}

/// 托盘菜单挂起开关（tray.rs 在 TrackPopupMenu 前后调用）。
/// 非 Windows / 钩子未建时是空操作。
pub fn set_suspended(on: bool) {
    if let Some(sh) = shared() {
        sh.suspended.store(on, Ordering::SeqCst);
    }
}

pub struct ClickWatch {
    shared: Arc<Shared>,
}

impl ClickWatch {
    pub fn start(ctx: egui::Context) -> ClickWatch {
        let sh = Arc::new(Shared {
            ctx,
            armed: AtomicBool::new(false),
            suspended: AtomicBool::new(false),
            main_rect: RectSlot::new(),
            sub_rect: RectSlot::new(),
            tray_rect: RectSlot::new(),
            outside_pending: AtomicBool::new(false),
        });
        // 只建一次：重复 start 直接复用既有共享（钩子线程不重开）
        let shared = match SHARED.set(sh.clone()) {
            Ok(()) => {
                platform::spawn();
                sh
            }
            Err(_) => shared().map(|s| (*s).clone()).unwrap_or(sh),
        };
        ClickWatch { shared }
    }

    /// UI 每帧发布：主窗口 / 设置窗口 / 托盘图标矩形（物理像素）
    /// + 是否武装。main 为 None（outer_rect 取不到）时强制不武装
    /// ——判定失去依据就整体停用，绝不凭空藏窗口。tray 为 None
    /// （图标矩形取不到，如图标暂未建成）时该豁免区自动失效，
    /// 呼出链还有 update() 侧的 drain 兜底（见模块头）。
    pub fn publish(
        &self,
        main: Option<[i32; 4]>,
        sub: Option<[i32; 4]>,
        tray: Option<[i32; 4]>,
        armed: bool,
    ) {
        self.shared.main_rect.publish(main);
        self.shared.sub_rect.publish(sub);
        self.shared.tray_rect.publish(tray);
        self.shared
            .armed
            .store(armed && main.is_some(), Ordering::SeqCst);
    }

    /// 取走一次「点外点击」（取后清零）。UI 每帧调用。
    pub fn take_outside(&self) -> bool {
        self.shared.outside_pending.swap(false, Ordering::SeqCst)
    }

    /// 丢弃攒下的点外标志（系统文件对话框关闭后调用：对话框是
    /// 独立窗口，点它属于「点外」，但那是用户在跟我们自己的
    /// 流程交互，绝不能一关对话框就把主窗藏了）。
    pub fn drain(&self) {
        self.shared.outside_pending.store(false, Ordering::SeqCst);
    }
}

/// 钩子回调的判定核心（与平台无关，单测覆盖）：武装且未挂起、
/// 落点不在任何已发布矩形内 → 点外。
fn is_outside(sh: &Shared, x: i32, y: i32) -> bool {
    if !sh.armed.load(Ordering::SeqCst) || sh.suspended.load(Ordering::SeqCst) {
        return false;
    }
    !sh.main_rect.contains(x, y)
        && !sh.sub_rect.contains(x, y)
        && !sh.tray_rect.contains(x, y)
}

#[cfg(windows)]
mod platform {
    use super::{is_outside, shared};
    use std::sync::atomic::Ordering;
    use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, SetWindowsHookExW,
        TranslateMessage, MSLLHOOKSTRUCT, MSG, PM_NOREMOVE, WH_MOUSE_LL, WM_LBUTTONDOWN,
        WM_MBUTTONDOWN, WM_RBUTTONDOWN,
    };

    /// 低级鼠标钩子回调：只读坐标 + 原子判定 + 置位/唤醒，
    /// 然后无条件放行（见模块头纪律）。
    unsafe extern "system" fn hook_proc(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if code >= 0 {
            let msg = wparam as u32;
            if msg == WM_LBUTTONDOWN || msg == WM_RBUTTONDOWN || msg == WM_MBUTTONDOWN {
                if let Some(sh) = shared() {
                    let info = &*(lparam as *const MSLLHOOKSTRUCT);
                    if is_outside(sh, info.pt.x, info.pt.y) {
                        sh.outside_pending.store(true, Ordering::SeqCst);
                        sh.ctx.request_repaint();
                    }
                }
            }
        }
        CallNextHookEx(0, code, wparam, lparam)
    }

    pub fn spawn() {
        std::thread::Builder::new()
            .name("shunseek-clickwatch".into())
            .spawn(move || unsafe {
                // 先摸一下消息队列让它建起来，钩子的投递才有落点
                let mut msg: MSG = std::mem::zeroed();
                PeekMessageW(&mut msg, 0, 0, 0, PM_NOREMOVE);
                let hmod = GetModuleHandleW(std::ptr::null());
                let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), hmod, 0);
                if hook == 0 {
                    return; // 装不上就没点外隐藏，应用其余照常
                }
                // 消息泵：低级钩子回调在本线程上下文执行，必须泵。
                loop {
                    let r = GetMessageW(&mut msg, 0, 0, 0);
                    if r <= 0 {
                        break;
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            })
            .ok();
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn spawn() {
        // 无全局钩子：见模块头注释。
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_shared() -> Shared {
        Shared {
            ctx: egui::Context::default(),
            armed: AtomicBool::new(true),
            suspended: AtomicBool::new(false),
            main_rect: RectSlot::new(),
            sub_rect: RectSlot::new(),
            tray_rect: RectSlot::new(),
            outside_pending: AtomicBool::new(false),
        }
    }

    #[test]
    fn outside_only_when_armed_and_beyond_all_rects() {
        let sh = test_shared();
        sh.main_rect.publish(Some([100, 100, 500, 400]));
        // 窗口内（含边界左上）不算点外
        assert!(!is_outside(&sh, 100, 100));
        assert!(!is_outside(&sh, 499, 399));
        // 窗口外算点外
        assert!(is_outside(&sh, 99, 200));
        assert!(is_outside(&sh, 500, 200));
        assert!(is_outside(&sh, 300, 400));
        // 设置窗口矩形内也不算
        sh.sub_rect.publish(Some([600, 100, 900, 500]));
        assert!(!is_outside(&sh, 700, 300));
        assert!(is_outside(&sh, 950, 300));
        // 托盘图标矩形内永不算点外（v0.1.11 豁免）
        sh.tray_rect.publish(Some([1000, 740, 1024, 764]));
        assert!(!is_outside(&sh, 1010, 750));
        assert!(is_outside(&sh, 1030, 750));
        // 挂起（托盘菜单）时永不算点外
        sh.suspended.store(true, Ordering::SeqCst);
        assert!(!is_outside(&sh, 950, 300));
        sh.suspended.store(false, Ordering::SeqCst);
        // 未武装时永不算点外
        sh.armed.store(false, Ordering::SeqCst);
        assert!(!is_outside(&sh, 950, 300));
        // 主矩形失效（取不到）时整槽不认——publish(None) 清 valid
        sh.armed.store(true, Ordering::SeqCst);
        sh.main_rect.publish(None);
        assert!(is_outside(&sh, 300, 200));
    }
}
