//! 全局热键呼出 / 隐藏搜索窗：自定义组合（设置里捕获修改）。
//! Windows：优先使用专用消息窗口上的原生 `RegisterHotKey`（系统自动
//! 授予前台窗口切换权且不透传按键）；若注册被其他程序占用，则在同一
//! 消息循环里用 30ms 定时器降级轮询 `GetAsyncKeyState`（精确匹配修饰键）。

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::Arc;

/// 一个热键组合：修饰键四件套 + 主键虚拟键码。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HotkeyCombo {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub vk: u32,
}

impl HotkeyCombo {
    pub const ALT_SPACE: HotkeyCombo = HotkeyCombo {
        ctrl: false,
        alt: true,
        shift: false,
        win: false,
        vk: 0x20,
    };

    pub const CTRL_ALT_SPACE: HotkeyCombo = HotkeyCombo {
        ctrl: true,
        alt: true,
        shift: false,
        win: false,
        vk: 0x20,
    };
    pub const CTRL_SHIFT_Q: HotkeyCombo = HotkeyCombo {
        ctrl: true,
        alt: false,
        shift: true,
        win: false,
        vk: 0x51,
    };
    pub const ALT_Q: HotkeyCombo = HotkeyCombo {
        ctrl: false,
        alt: true,
        shift: false,
        win: false,
        vk: 0x51,
    };

    pub fn new(ctrl: bool, alt: bool, shift: bool, win: bool, vk: u32) -> HotkeyCombo {
        HotkeyCombo {
            ctrl,
            alt,
            shift,
            win,
            vk,
        }
    }

    pub fn display(self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.win {
            parts.push("Win".to_string());
        }
        parts.push(key_name(self.vk));
        parts.join("+")
    }

    pub fn key_str(self) -> String {
        self.display().to_lowercase()
    }

    pub fn parse(s: &str) -> Option<HotkeyCombo> {
        match s.trim() {
            "alt_space" => return Some(HotkeyCombo::ALT_SPACE),
            "ctrl_space" => {
                return Some(HotkeyCombo::new(true, false, false, false, 0x20));
            }
            "alt_q" => return Some(HotkeyCombo::new(false, true, false, false, 0x51)),
            _ => {}
        }
        let mut combo = HotkeyCombo::new(false, false, false, false, 0);
        for part in s.split('+') {
            match part.trim().to_lowercase().as_str() {
                "ctrl" | "control" => combo.ctrl = true,
                "alt" => combo.alt = true,
                "shift" => combo.shift = true,
                "win" | "super" | "cmd" | "command" => combo.win = true,
                key => combo.vk = vk_from_name(key)?,
            }
        }
        if combo.vk == 0 || !(combo.ctrl || combo.alt || combo.shift || combo.win) {
            None
        } else {
            Some(combo)
        }
    }

    fn pack(self) -> u64 {
        (self.vk as u64)
            | (self.ctrl as u64) << 32
            | (self.alt as u64) << 33
            | (self.shift as u64) << 34
            | (self.win as u64) << 35
    }

    fn unpack(v: u64) -> HotkeyCombo {
        HotkeyCombo {
            vk: (v & 0xFFFF_FFFF) as u32,
            ctrl: (v >> 32) & 1 == 1,
            alt: (v >> 33) & 1 == 1,
            shift: (v >> 34) & 1 == 1,
            win: (v >> 35) & 1 == 1,
        }
    }
}

pub fn key_name(vk: u32) -> String {
    match vk {
        0x20 => "Space".to_string(),
        0x09 => "Tab".to_string(),
        0x1B => "Esc".to_string(),
        0x41..=0x5A | 0x30..=0x39 => (vk as u8 as char).to_string(),
        0x70..=0x7B => format!("F{}", vk - 0x6F),
        _ => format!("VK{vk}"),
    }
}

fn vk_from_name(name: &str) -> Option<u32> {
    match name {
        "space" => Some(0x20),
        "tab" => Some(0x09),
        s if s.len() == 1 => {
            let c = s.chars().next()?.to_ascii_uppercase();
            if c.is_ascii_alphanumeric() {
                Some(c as u32)
            } else {
                None
            }
        }
        s if s.starts_with('f') => s[1..]
            .parse::<u32>()
            .ok()
            .filter(|n| (1..=12).contains(n))
            .map(|n| 0x6F + n),
        _ => None,
    }
}

// ---------------------------------------------------------------- 冲突检测

#[derive(Clone, Debug, PartialEq)]
pub enum HotkeyCheck {
    Available,
    Shared(String),
    Conflict(String),
    Occupied,
}

fn hard_reserved(combo: HotkeyCombo) -> Option<&'static str> {
    let c = HotkeyCombo::new;
    let reason = match combo {
        _ if combo == c(false, true, false, false, 0x09) => "Alt+Tab 是系统窗口切换快捷键",
        _ if combo == c(false, true, false, false, 0x1B) => "Alt+Esc 是系统窗口切换快捷键",
        _ if combo == c(true, false, false, false, 0x1B) => "Ctrl+Esc 是开始菜单快捷键",
        _ if combo == c(false, true, false, false, 0x73) => "Alt+F4 是关闭窗口快捷键",
        _ if combo == c(false, false, false, true, 0x4C) => "Win+L 是锁屏快捷键",
        _ if combo == c(false, false, false, true, 0x44) => "Win+D 是显示桌面快捷键",
        _ if combo == c(false, false, false, true, 0x45) => "Win+E 是资源管理器快捷键",
        _ if combo == c(false, false, false, true, 0x52) => "Win+R 是运行对话框快捷键",
        _ if combo == c(false, false, false, true, 0x53) => "Win+S 是系统搜索快捷键",
        _ if combo == c(false, false, false, true, 0x09) => "Win+Tab 是任务视图快捷键",
        _ => return None,
    };
    Some(reason)
}

fn shared_reserved(combo: HotkeyCombo) -> Option<&'static str> {
    if combo == HotkeyCombo::ALT_SPACE {
        Some("与系统窗口菜单共用：在其他窗口聚焦时按 Alt+Space，可能弹出该窗口的系统菜单")
    } else if combo == HotkeyCombo::new(true, false, false, false, 0x20) {
        Some("与输入法切换共用：可能同时触发输入法的中英文切换")
    } else {
        None
    }
}

pub fn check(combo: HotkeyCombo) -> HotkeyCheck {
    if let Some(reason) = hard_reserved(combo) {
        return HotkeyCheck::Conflict(reason.to_string());
    }
    #[cfg(windows)]
    {
        if win_probe::is_occupied(combo) {
            return HotkeyCheck::Occupied;
        }
    }
    if let Some(reason) = shared_reserved(combo) {
        return HotkeyCheck::Shared(reason.to_string());
    }
    HotkeyCheck::Available
}

pub fn resolve_default() -> HotkeyCombo {
    for cand in [
        HotkeyCombo::CTRL_ALT_SPACE,
        HotkeyCombo::CTRL_SHIFT_Q,
        HotkeyCombo::ALT_Q,
    ] {
        if check(cand) == HotkeyCheck::Available {
            return cand;
        }
    }
    HotkeyCombo::CTRL_ALT_SPACE
}

pub const RECOMMEND_CANDIDATES: [HotkeyCombo; 5] = [
    HotkeyCombo {
        ctrl: false,
        alt: true,
        shift: false,
        win: false,
        vk: 0x51, // Alt+Q
    },
    HotkeyCombo {
        ctrl: true,
        alt: true,
        shift: false,
        win: false,
        vk: 0x51, // Ctrl+Alt+Q
    },
    HotkeyCombo {
        ctrl: false,
        alt: true,
        shift: true,
        win: false,
        vk: 0x20, // Alt+Shift+Space
    },
    HotkeyCombo {
        ctrl: true,
        alt: false,
        shift: true,
        win: false,
        vk: 0x20, // Ctrl+Shift+Space
    },
    HotkeyCombo {
        ctrl: false,
        alt: true,
        shift: false,
        win: false,
        vk: 0x58, // Alt+X
    },
];

#[cfg(windows)]
mod win_probe {
    use super::HotkeyCombo;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
        MOD_WIN,
    };

    pub fn to_mods(combo: HotkeyCombo) -> u32 {
        let mut mods = MOD_NOREPEAT;
        if combo.ctrl {
            mods |= MOD_CONTROL;
        }
        if combo.alt {
            mods |= MOD_ALT;
        }
        if combo.shift {
            mods |= MOD_SHIFT;
        }
        if combo.win {
            mods |= MOD_WIN;
        }
        mods
    }

    pub fn is_occupied(combo: HotkeyCombo) -> bool {
        let mods = to_mods(combo);
        const TEST_ID: i32 = 0x5EED;
        unsafe {
            let ok = RegisterHotKey(0, TEST_ID, mods, combo.vk);
            if ok != 0 {
                UnregisterHotKey(0, TEST_ID);
                false
            } else {
                GetLastError() == 1409
            }
        }
    }
}

/// 当前物理 Win 键（左或右）是否处于按下状态（供设置页快捷键捕获区分 Ctrl 与 Win）。
pub fn win_key_down() -> bool {
    platform::win_key_down()
}

// ---------------------------------------------------------------- 热键监听

pub struct Hotkey {
    fired: Arc<AtomicBool>,
    combo: Arc<AtomicU64>,
    paused: Arc<AtomicBool>,
    hwnd: Arc<AtomicIsize>,
}

impl Hotkey {
    pub fn start(ctx: egui::Context, combo: HotkeyCombo) -> Hotkey {
        let fired = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(AtomicU64::new(combo.pack()));
        let paused = Arc::new(AtomicBool::new(false));
        let hwnd = Arc::new(AtomicIsize::new(0));
        platform::spawn(
            ctx,
            fired.clone(),
            shared.clone(),
            paused.clone(),
            hwnd.clone(),
        );
        Hotkey {
            fired,
            combo: shared,
            paused,
            hwnd,
        }
    }

    pub fn set_combo(&self, combo: HotkeyCombo) {
        self.combo.store(combo.pack(), Ordering::SeqCst);
        platform::notify_update(&self.hwnd);
    }

    /// 暂停/恢复热键监听（设置页按键捕获期间暂停，防止触发当前热键或自身冲突）。
    pub fn set_paused(&self, paused: bool) {
        let prev = self.paused.swap(paused, Ordering::SeqCst);
        if prev != paused {
            platform::notify_update(&self.hwnd);
        }
    }

    pub fn take_fired(&self) -> bool {
        self.fired.swap(false, Ordering::SeqCst)
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, RegisterHotKey, UnregisterHotKey,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, KillTimer,
        PostMessageW, RegisterClassW, SetTimer, TranslateMessage, MSG, WM_HOTKEY, WM_TIMER,
        WNDCLASSW,
    };

    const VK_CONTROL: i32 = 0x11;
    const VK_MENU: i32 = 0x12; // Alt
    const VK_SHIFT: i32 = 0x10;
    const VK_LWIN: i32 = 0x5B;
    const VK_RWIN: i32 = 0x5C;

    const WM_SS_HK_UPDATE: u32 = 0x8000 + 0x31;
    const HOTKEY_ID: i32 = 0x5348;
    const TIMER_ID: usize = 1;

    struct HkState {
        ctx: egui::Context,
        fired: Arc<AtomicBool>,
        combo: Arc<AtomicU64>,
        paused: Arc<AtomicBool>,
        prev_main: AtomicBool,
    }

    static HK_STATE: OnceLock<Arc<HkState>> = OnceLock::new();

    fn key_down(vk: i32) -> bool {
        unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
    }

    pub fn win_key_down() -> bool {
        key_down(VK_LWIN) || key_down(VK_RWIN)
    }

    pub fn notify_update(hwnd: &Arc<AtomicIsize>) {
        let h = hwnd.load(Ordering::SeqCst);
        if h != 0 {
            unsafe {
                PostMessageW(h as HWND, WM_SS_HK_UPDATE, 0, 0);
            }
        }
    }

    unsafe fn apply_registration(hwnd: HWND, st: &HkState) {
        let _ = UnregisterHotKey(hwnd, HOTKEY_ID);
        st.prev_main.store(false, Ordering::SeqCst);
        if st.paused.load(Ordering::SeqCst) {
            KillTimer(hwnd, TIMER_ID);
            return;
        }
        let c = HotkeyCombo::unpack(st.combo.load(Ordering::SeqCst));
        let mods = super::win_probe::to_mods(c);
        if RegisterHotKey(hwnd, HOTKEY_ID, mods, c.vk) != 0 {
            KillTimer(hwnd, TIMER_ID);
        } else {
            // 被占用的热键降级走 30ms 轮询
            SetTimer(hwnd, TIMER_ID, 30, None);
        }
    }

    unsafe extern "system" fn hk_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_HOTKEY if wparam as i32 == HOTKEY_ID => {
                if let Some(st) = HK_STATE.get() {
                    if !st.paused.load(Ordering::SeqCst) {
                        crate::winvis::toggle();
                        st.fired.store(true, Ordering::SeqCst);
                        st.ctx.request_repaint();
                    }
                }
                0
            }
            WM_SS_HK_UPDATE => {
                if let Some(st) = HK_STATE.get() {
                    apply_registration(hwnd, st);
                }
                0
            }
            WM_TIMER if wparam == TIMER_ID => {
                if let Some(st) = HK_STATE.get() {
                    if st.paused.load(Ordering::SeqCst) {
                        st.prev_main.store(false, Ordering::SeqCst);
                        return 0;
                    }
                    let c = HotkeyCombo::unpack(st.combo.load(Ordering::SeqCst));
                    // 严格精确匹配修饰键集合，避免 Alt+Q 被 Ctrl+Alt+Q 误触发
                    let mods_exact = (key_down(VK_CONTROL) == c.ctrl)
                        && (key_down(VK_MENU) == c.alt)
                        && (key_down(VK_SHIFT) == c.shift)
                        && ((key_down(VK_LWIN) || key_down(VK_RWIN)) == c.win);
                    let down = key_down(c.vk as i32);
                    let prev = st.prev_main.swap(down, Ordering::SeqCst);
                    if down && !prev && mods_exact {
                        crate::winvis::toggle();
                        st.fired.store(true, Ordering::SeqCst);
                        st.ctx.request_repaint();
                    }
                }
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    pub fn spawn(
        ctx: egui::Context,
        fired: Arc<AtomicBool>,
        combo: Arc<AtomicU64>,
        paused: Arc<AtomicBool>,
        hwnd_out: Arc<AtomicIsize>,
    ) {
        let st = Arc::new(HkState {
            ctx,
            fired,
            combo,
            paused,
            prev_main: AtomicBool::new(false),
        });
        let _ = HK_STATE.set(st);
        std::thread::Builder::new()
            .name("shunseek-hotkey".into())
            .spawn(move || unsafe {
                let hmod = GetModuleHandleW(std::ptr::null());
                let class_name: Vec<u16> = "ShunSeekHotkeyWindow"
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                let wc = WNDCLASSW {
                    style: 0,
                    lpfnWndProc: Some(hk_wndproc),
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: hmod,
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
                    class_name.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    -3isize as HWND,
                    0,
                    hmod,
                    std::ptr::null(),
                );
                if hwnd == 0 {
                    return;
                }
                hwnd_out.store(hwnd, Ordering::SeqCst);
                if let Some(st) = HK_STATE.get() {
                    apply_registration(hwnd, st);
                }
                let mut msg: MSG = std::mem::zeroed();
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
    use super::*;

    pub fn win_key_down() -> bool {
        false
    }

    pub fn notify_update(_hwnd: &Arc<AtomicIsize>) {}

    pub fn spawn(
        _ctx: egui::Context,
        _fired: Arc<AtomicBool>,
        _combo: Arc<AtomicU64>,
        _paused: Arc<AtomicBool>,
        _hwnd_out: Arc<AtomicIsize>,
    ) {
    }
}
