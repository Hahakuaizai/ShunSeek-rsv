//! 全局点外隐藏（照原版 .NET GlobalClickWatcher 机制）：
//! WH_MOUSE_LL 低级鼠标钩子装在专职线程上（带自己的消息泵），
//! 全局左/右/中键按下时，落点不在主窗口/设置窗口/托盘图标矩形内 → 点外命中。
//! 命中时先立即撤防（disarm），再通过 winvis::hide_from_hook() 异步
//! PostMessage 给 UI 线程执行 SW_HIDE，钩子回调微秒返回、绝不阻塞系统鼠标流。

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

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
    armed: AtomicBool,
    suspended: AtomicBool,
    suspend_depth: AtomicUsize,
    main_rect: RectSlot,
    sub_rect: RectSlot,
    tray_rect: RectSlot,
    outside_pending: AtomicBool,
    shown_at_ms: AtomicU64,
}

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
static EPOCH: OnceLock<Instant> = OnceLock::new();

fn now_ms() -> u64 {
    let base = EPOCH.get_or_init(Instant::now);
    // 偏移 1000ms，保证 0 永远代表「从未记录」
    base.elapsed().as_millis() as u64 + 1000
}

fn shared() -> Option<&'static Arc<Shared>> {
    SHARED.get()
}

/// 托盘菜单 / 系统文件框打开期间挂起（支持嵌套计数）。
pub fn set_suspended(on: bool) {
    if let Some(sh) = shared() {
        if on {
            sh.suspend_depth.fetch_add(1, Ordering::SeqCst);
            sh.suspended.store(true, Ordering::SeqCst);
        } else {
            let prev = sh.suspend_depth.load(Ordering::SeqCst);
            if prev > 0 {
                let next = sh.suspend_depth.fetch_sub(1, Ordering::SeqCst) - 1;
                if next == 0 {
                    sh.suspended.store(false, Ordering::SeqCst);
                }
            } else {
                sh.suspended.store(false, Ordering::SeqCst);
            }
        }
    }
}

/// RAII 挂起守卫：构造时挂起钩子，析构时恢复并清理残留标志。
pub struct SuspendGuard;

impl SuspendGuard {
    pub fn new() -> Self {
        set_suspended(true);
        SuspendGuard
    }
}

impl Drop for SuspendGuard {
    fn drop(&mut self) {
        set_suspended(false);
        if let Some(sh) = shared() {
            sh.outside_pending.store(false, Ordering::SeqCst);
        }
    }
}

/// 立即撤防：窗口隐藏/最小化时调用，防止隐藏状态下每次系统点击仍触发隐藏。
pub fn disarm() {
    if let Some(sh) = shared() {
        sh.armed.store(false, Ordering::SeqCst);
        sh.outside_pending.store(false, Ordering::SeqCst);
    }
}

/// 记录刚完成一次显式呼出，200ms 内免除点外隐藏（防 Win11 折叠托盘拿不到图标矩形时反杀）。
pub fn note_shown() {
    if let Some(sh) = shared() {
        sh.shown_at_ms.store(now_ms(), Ordering::SeqCst);
        sh.outside_pending.store(false, Ordering::SeqCst);
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
            suspend_depth: AtomicUsize::new(0),
            main_rect: RectSlot::new(),
            sub_rect: RectSlot::new(),
            tray_rect: RectSlot::new(),
            outside_pending: AtomicBool::new(false),
            shown_at_ms: AtomicU64::new(0),
        });
        let shared = match SHARED.set(sh.clone()) {
            Ok(()) => {
                platform::spawn();
                sh
            }
            Err(_) => shared().map(|s| (*s).clone()).unwrap_or(sh),
        };
        ClickWatch { shared }
    }

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

    pub fn take_outside(&self) -> bool {
        self.shared.outside_pending.swap(false, Ordering::SeqCst)
    }

    pub fn drain(&self) {
        self.shared.outside_pending.store(false, Ordering::SeqCst);
    }
}

/// 钩子回调的判定核心（与平台无关，单测覆盖）：武装且未挂起、
/// 不在呼出保护期、落点不在任何已发布矩形内 → 点外。
fn is_outside_at(sh: &Shared, x: i32, y: i32, cur_ms: u64) -> bool {
    if !sh.armed.load(Ordering::SeqCst) || sh.suspended.load(Ordering::SeqCst) {
        return false;
    }
    let shown = sh.shown_at_ms.load(Ordering::SeqCst);
    if shown != 0 && cur_ms.saturating_sub(shown) < 200 {
        return false;
    }
    !sh.main_rect.contains(x, y)
        && !sh.sub_rect.contains(x, y)
        && !sh.tray_rect.contains(x, y)
}

#[cfg_attr(not(windows), allow(dead_code))]
fn is_outside(sh: &Shared, x: i32, y: i32) -> bool {
    is_outside_at(sh, x, y, now_ms())
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
                        // 立即撤防，避免连续点击重复投递
                        sh.armed.store(false, Ordering::SeqCst);
                        if crate::winvis::hwnd() != 0 {
                            crate::winvis::hide_from_hook();
                        } else {
                            sh.outside_pending.store(true, Ordering::SeqCst);
                            sh.ctx.request_repaint();
                        }
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
                let mut msg: MSG = std::mem::zeroed();
                PeekMessageW(&mut msg, 0, 0, 0, PM_NOREMOVE);
                let hmod = GetModuleHandleW(std::ptr::null());
                let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), hmod, 0);
                if hook == 0 {
                    return;
                }
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
    pub fn spawn() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_shared() -> Shared {
        Shared {
            ctx: egui::Context::default(),
            armed: AtomicBool::new(true),
            suspended: AtomicBool::new(false),
            suspend_depth: AtomicUsize::new(0),
            main_rect: RectSlot::new(),
            sub_rect: RectSlot::new(),
            tray_rect: RectSlot::new(),
            outside_pending: AtomicBool::new(false),
            shown_at_ms: AtomicU64::new(0),
        }
    }

    #[test]
    fn outside_only_when_armed_and_beyond_all_rects() {
        let sh = test_shared();
        sh.main_rect.publish(Some([100, 100, 500, 400]));
        assert!(!is_outside_at(&sh, 100, 100, 5000));
        assert!(!is_outside_at(&sh, 499, 399, 5000));
        assert!(is_outside_at(&sh, 99, 200, 5000));
        assert!(is_outside_at(&sh, 500, 200, 5000));
        assert!(is_outside_at(&sh, 300, 400, 5000));

        sh.sub_rect.publish(Some([600, 100, 900, 500]));
        assert!(!is_outside_at(&sh, 700, 300, 5000));
        assert!(is_outside_at(&sh, 950, 300, 5000));

        sh.tray_rect.publish(Some([1000, 740, 1024, 764]));
        assert!(!is_outside_at(&sh, 1010, 750, 5000));
        assert!(is_outside_at(&sh, 1030, 750, 5000));

        sh.suspended.store(true, Ordering::SeqCst);
        assert!(!is_outside_at(&sh, 950, 300, 5000));
        sh.suspended.store(false, Ordering::SeqCst);

        // 呼出 200ms 保护期内不判点外
        sh.shown_at_ms.store(4900, Ordering::SeqCst);
        assert!(!is_outside_at(&sh, 950, 300, 5050));
        assert!(is_outside_at(&sh, 950, 300, 5200));

        sh.armed.store(false, Ordering::SeqCst);
        assert!(!is_outside_at(&sh, 950, 300, 5200));

        sh.armed.store(true, Ordering::SeqCst);
        sh.main_rect.publish(None);
        assert!(is_outside_at(&sh, 300, 200, 5200));
    }
}
