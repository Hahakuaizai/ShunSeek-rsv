//! 全局热键 Alt+Space 呼出 / 隐藏搜索窗。
//!
//! Windows：C# 版用 RegisterHotKey + 窗口消息钩子。egui/eframe 没有把
//! WM_HOTKEY 暴露给应用，这里改用后台线程轮询 GetAsyncKeyState（user32 FFI）：
//! 每 20ms 查一次 Alt 与 Space 的物理键状态，Space 由松到按的边沿触发一次，
//! 置位共享标志并 request_repaint 把 UI 线程唤醒来处理。
//! 代价：窗口隐藏时 UI 靠这个 repaint 唤醒，轮询本身开销可忽略。
//!
//! 非 Windows：egui 没有跨平台全局热键支持，这里降级为无全局热键——
//! 窗口就是普通窗口，托盘 / 任务栏入口照常。真要补可用各桌面环境的
//! 自定义快捷键把本程序的 --toggle 参数绑到 Alt+Space（TODO）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct Hotkey {
    fired: Arc<AtomicBool>,
}

impl Hotkey {
    pub fn start(ctx: egui::Context) -> Hotkey {
        let fired = Arc::new(AtomicBool::new(false));
        platform::spawn(ctx, fired.clone());
        Hotkey { fired }
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

    const VK_MENU: i32 = 0x12; // Alt
    const VK_SPACE: i32 = 0x20;

    #[link(name = "user32")]
    extern "system" {
        fn GetAsyncKeyState(v_key: i32) -> i16;
    }

    fn key_down(vk: i32) -> bool {
        // 最高位为 1 = 当前正按着
        unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
    }

    pub fn spawn(ctx: egui::Context, fired: Arc<AtomicBool>) {
        std::thread::Builder::new()
            .name("shunseek-hotkey".into())
            .spawn(move || {
                let mut prev_space = false;
                loop {
                    let space = key_down(VK_SPACE);
                    if space && !prev_space && key_down(VK_MENU) {
                        fired.store(true, Ordering::SeqCst);
                        ctx.request_repaint();
                    }
                    prev_space = space;
                    std::thread::sleep(Duration::from_millis(20));
                }
            })
            .ok();
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub fn spawn(_ctx: egui::Context, _fired: Arc<AtomicBool>) {
        // 无全局热键：见模块头注释。
    }
}
