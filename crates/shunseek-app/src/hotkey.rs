//! 全局热键呼出 / 隐藏搜索窗：自定义组合（设置里捕获修改）。
//! 默认值 v0.1.8 起走候选链解析（Ctrl+Alt+Space → Ctrl+Shift+Q
//! → Alt+Q，见 resolve_default；Alt+Space 在用户机器上被占用，
//! 只作为用户手动存入的值存在）。settings.txt 的 hotkey 键存
//! 规范串（如 "alt+space"、"ctrl+alt+q"），存过的值永远优先。
//!
//! Windows：C# 版用 RegisterHotKey + 窗口消息钩子。egui/eframe 没有把
//! WM_HOTKEY 暴露给应用，这里改用后台线程轮询 GetAsyncKeyState（user32 FFI）：
//! 每 20ms 查一次修饰键与主键的物理状态，主键由松到按的边沿触发一次，
//! 置位共享标志并 request_repaint 把 UI 线程唤醒来处理。组合存共享
//! 原子值，设置里改完下一圈轮询即按新值生效。
//! 代价：窗口隐藏时 UI 靠这个 repaint 唤醒，轮询本身开销可忽略。
//!
//! 冲突检测（check）：两道——硬编码系统保留名单优先判（Alt+Space
//! 这类可能注册成功，名单说了算），再用 RegisterHotKey 实测占用
//!（1409 = 已被其他程序注册）。非 Windows 跳过检测、恒可用。
//!
//! 非 Windows：egui 没有跨平台全局热键支持，轮询降级为无全局热键——
//! 窗口就是普通窗口，托盘 / 任务栏入口照常。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

    /// v0.1.8 默认热键候选链（对齐原版）：Ctrl+Alt+Space 优先
    ///（Alt+Space 在用户机器上被占用/冲突），依次回退
    /// Ctrl+Shift+Q → Alt+Q。
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

    /// 显示名："Alt+Space" / "Ctrl+Alt+Q" / "Win+L"。
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

    /// settings.txt 规范串（显示名小写）："alt+space"。
    pub fn key_str(self) -> String {
        self.display().to_lowercase()
    }

    /// 解析规范串；兼容旧版预设值 alt_space / ctrl_space / alt_q。
    /// 至少一个修饰键 + 一个主键才合法。
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

/// 主键显示名（VK → 人读名）。
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

/// 冲突检测三态（外加硬冲突）：见 check()。
#[derive(Clone, Debug, PartialEq)]
pub enum HotkeyCheck {
    /// 可用：可直接设置。
    Available,
    /// 与系统快捷键共用：可设置，但要把原因提示给用户。
    Shared(String),
    /// 系统保留硬冲突：不可设置，附原因。
    Conflict(String),
    /// 已被其他程序注册占用：不可设置。
    Occupied,
}

/// 系统保留硬冲突名单（命中即不可设置）。只认精确组合。
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

/// 与系统共用名单：可设置，但要提示。
fn shared_reserved(combo: HotkeyCombo) -> Option<&'static str> {
    if combo == HotkeyCombo::ALT_SPACE {
        Some("与系统窗口菜单共用：在其他窗口聚焦时按 Alt+Space，可能弹出该窗口的系统菜单")
    } else if combo == HotkeyCombo::new(true, false, false, false, 0x20) {
        Some("与输入法切换共用：可能同时触发输入法的中英文切换")
    } else {
        None
    }
}

/// 冲突检测：名单优先判（硬冲突 → 占用探测 → 共用提示 → 可用）。
/// 非 Windows 跳过探测，名单之外一律可用。
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

/// 解析默认热键（v0.1.8）：候选链 Ctrl+Alt+Space → Ctrl+Shift+Q
/// → Alt+Q 顺序过 check()，第一个 Available 的胜出；全军覆没时
/// 退回链首（总比没有热键强，用户可在设置里再改）。仅在
/// settings.txt 没有 hotkey 键（或解析失败）时调用——用户已存
/// 的值永远优先，绝不覆盖。
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

/// 推荐候选（冲突时给出）：每个都要先过 check、只留 Available 的。
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

    /// 占用探测：拿临时 id 真注册一次。成功 = 没被占（立即注销）；
    /// 失败且 GetLastError = 1409（ERROR_HOTKEY_ALREADY_REGISTERED）
    /// = 已被其他程序注册；其他失败原因保守按未占用处理（名单已先判）。
    pub fn is_occupied(combo: HotkeyCombo) -> bool {
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

// ---------------------------------------------------------------- 轮询

pub struct Hotkey {
    fired: Arc<AtomicBool>,
    combo: Arc<AtomicU64>,
}

impl Hotkey {
    pub fn start(ctx: egui::Context, combo: HotkeyCombo) -> Hotkey {
        let fired = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(AtomicU64::new(combo.pack()));
        platform::spawn(ctx, fired.clone(), shared.clone());
        Hotkey {
            fired,
            combo: shared,
        }
    }

    /// 设置里换了组合：写共享原子值，轮询线程下一圈就按新值生效。
    pub fn set_combo(&self, combo: HotkeyCombo) {
        self.combo.store(combo.pack(), Ordering::SeqCst);
    }

    /// 取走一次触发（取后清零）。UI 在每帧 update 开头调用。
    pub fn take_fired(&self) -> bool {
        self.fired.swap(false, Ordering::SeqCst)
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::time::Duration;

    const VK_CONTROL: i32 = 0x11;
    const VK_MENU: i32 = 0x12; // Alt
    const VK_SHIFT: i32 = 0x10;
    const VK_LWIN: i32 = 0x5B;
    const VK_RWIN: i32 = 0x5C;

    #[link(name = "user32")]
    extern "system" {
        fn GetAsyncKeyState(v_key: i32) -> i16;
    }

    fn key_down(vk: i32) -> bool {
        // 最高位为 1 = 当前正按着
        unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
    }

    pub fn spawn(ctx: egui::Context, fired: Arc<AtomicBool>, combo: Arc<AtomicU64>) {
        std::thread::Builder::new()
            .name("shunseek-hotkey".into())
            .spawn(move || {
                let mut prev_main = false;
                loop {
                    let c = HotkeyCombo::unpack(combo.load(Ordering::Relaxed));
                    let mods_down = (!c.ctrl || key_down(VK_CONTROL))
                        && (!c.alt || key_down(VK_MENU))
                        && (!c.shift || key_down(VK_SHIFT))
                        && (!c.win || key_down(VK_LWIN) || key_down(VK_RWIN));
                    let down = key_down(c.vk as i32);
                    if down && !prev_main && mods_down {
                        fired.store(true, Ordering::SeqCst);
                        ctx.request_repaint();
                    }
                    prev_main = down;
                    // 轮询间隔 30ms（v0.1.6 帧率纪律：只在按下边沿
                    // request_repaint，间隔不小于 30ms，空转不唤醒 UI）
                    std::thread::sleep(Duration::from_millis(30));
                }
            })
            .ok();
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub fn spawn(_ctx: egui::Context, _fired: Arc<AtomicBool>, _combo: Arc<AtomicU64>) {
        // 无全局热键：见模块头注释。
    }
}
