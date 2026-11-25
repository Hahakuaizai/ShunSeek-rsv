//! 系统托盘：搜索窗平时隐藏，托盘是它的常驻入口与退出出口（同 C# 版
//! TrayIconManager：菜单「显示搜索 (Alt+Space)」/「退出」，双击图标切换显隐）。
//!
//! Windows：tray-icon 实现。菜单 / 点击事件走 tray-icon 的全局接收通道，
//! 另起两个小线程阻塞在通道上，事件一到就 request_repaint 唤醒 UI 线程，
//! UI 在 update 里 poll 本模块的动作队列——避免隐藏状态下事件无人处理。
//!
//! 非 Windows：tray-icon 在 Linux 上依赖 GTK + appindicator 系统库，
//! 无头构建机不具备，故 Cargo.toml 里只给 Windows 目标引入该依赖；
//! 这里保留骨架（new/poll 接口与 Windows 版一致，poll 恒返回 None），
//! 应用其余部分不感知平台差异。TODO：Linux 发布时在本机装好
//! libgtk-3-dev / libappindicator3-dev 后，把依赖放开即可启用同一份实现。

use std::sync::{Arc, Mutex};

// 非 Windows 骨架下无人构造这两个变体（poll 恒空），故 allow(dead_code)
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayAction {
    Toggle,
    Exit,
}

pub struct Tray {
    actions: Arc<Mutex<Vec<TrayAction>>>,
    #[cfg(windows)]
    _inner: Option<win::WinTray>,
}

impl Tray {
    pub fn new(ctx: egui::Context) -> Tray {
        let actions: Arc<Mutex<Vec<TrayAction>>> = Arc::new(Mutex::new(Vec::new()));
        #[cfg(windows)]
        let inner = win::WinTray::create(ctx, actions.clone());
        #[cfg(not(windows))]
        let _ = &ctx; // 骨架：不创建任何系统托盘对象
        Tray {
            actions,
            #[cfg(windows)]
            _inner: inner,
        }
    }

    /// 取走一个待处理动作（UI 每帧调用）。
    pub fn poll(&mut self) -> Option<TrayAction> {
        let mut q = self.actions.lock().ok()?;
        if q.is_empty() {
            None
        } else {
            Some(q.remove(0))
        }
    }
}

#[cfg(windows)]
mod win {
    use super::TrayAction;
    use std::sync::{Arc, Mutex};
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

    pub struct WinTray {
        _icon: TrayIcon,
    }

    /// 32×32 纯色图标（主题蓝圆角方块的粗略近似）：正式 .ico 资源打包是
    /// 发布阶段的事，接入点就在这里换 Icon 来源。
    fn placeholder_icon() -> Option<Icon> {
        let (w, h) = (32u32, 32u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let corner = (x < 4 || x >= w - 4) && (y < 4 || y >= h - 4);
                rgba[i] = 37; // #2563EB 主题蓝
                rgba[i + 1] = 99;
                rgba[i + 2] = 235;
                rgba[i + 3] = if corner { 0 } else { 255 };
            }
        }
        Icon::from_rgba(rgba, w, h).ok()
    }

    impl WinTray {
        pub fn create(
            ctx: egui::Context,
            actions: Arc<Mutex<Vec<TrayAction>>>,
        ) -> Option<WinTray> {
            let toggle_item = MenuItem::with_id("toggle", "显示搜索 (Alt+Space)", true, None);
            let exit_item = MenuItem::with_id("exit", "退出", true, None);
            let menu = Menu::new();
            if menu.append(&toggle_item).is_err() || menu.append(&exit_item).is_err() {
                return None;
            }
            let mut builder = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip("瞬觅 ShunSeek");
            if let Some(icon) = placeholder_icon() {
                builder = builder.with_icon(icon);
            }
            let icon = builder.build().ok()?;

            // 菜单事件线程：阻塞等事件 → 入队 → 唤醒 UI。
            {
                let actions = actions.clone();
                let ctx = ctx.clone();
                std::thread::Builder::new()
                    .name("shunseek-tray-menu".into())
                    .spawn(move || {
                        let rx = MenuEvent::receiver();
                        while let Ok(ev) = rx.recv() {
                            let action = if ev.id.0 == "toggle" {
                                Some(TrayAction::Toggle)
                            } else if ev.id.0 == "exit" {
                                Some(TrayAction::Exit)
                            } else {
                                None
                            };
                            if let Some(a) = action {
                                if let Ok(mut q) = actions.lock() {
                                    q.push(a);
                                }
                                ctx.request_repaint();
                            }
                        }
                    })
                    .ok();
            }
            // 图标点击事件线程：左键抬起 = 切换显隐（双击在 tray-icon 里
            // 也是两次 Click，这里与 C# 版双击切换的体感一致）。
            {
                let ctx = ctx.clone();
                std::thread::Builder::new()
                    .name("shunseek-tray-click".into())
                    .spawn(move || {
                        let rx = TrayIconEvent::receiver();
                        while let Ok(ev) = rx.recv() {
                            if let TrayIconEvent::Click {
                                button: tray_icon::MouseButton::Left,
                                button_state: tray_icon::MouseButtonState::Up,
                                ..
                            } = ev
                            {
                                if let Ok(mut q) = actions.lock() {
                                    q.push(TrayAction::Toggle);
                                }
                                ctx.request_repaint();
                            }
                        }
                    })
                    .ok();
            }
            Some(WinTray { _icon: icon })
        }
    }
}
