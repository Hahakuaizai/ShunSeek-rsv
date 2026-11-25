//! 系统托盘：搜索窗平时隐藏，托盘是它的常驻入口与退出出口。
//!
//! 菜单（v0.1.5 定稿）：只放快捷动作，不放任何开关——
//!   打开瞬觅 / 重新建立索引 / 设置 / 关于瞬觅 vX.Y.Z / 退出
//! 开关类（主题、宫格、自启动、热键等）一律在应用内「设置」窗口里，
//! 两边不做重复功能（原版两边一样被用户点过名）。图标左键单击仍是
//! 显示/隐藏切换。「重新建立索引」重建中置灰 + 文案「正在建立索引…」，
//! 是全菜单唯一需要状态同步的项，句柄保存在 WinTray 里。
//!
//! 事件通路（v0.1.5 重做，修「隐藏后托盘关不掉」）：
//! 旧版起两个线程阻塞在 MenuEvent / TrayIconEvent 的接收通道上，收到
//! 事件入队 + request_repaint，再由 update() 里的 poll() 取走。真机上
//! 窗口隐藏后这条路断了：eframe 事件循环停转，poll 轮不到，「退出」
//! 点了没反应，只能任务管理器杀进程。
//! 新版改用 tray-icon 的**全局事件 handler**（MenuEvent / TrayIconEvent
//! 各设一个 set_event_handler）：handler 由 tray-icon 的内部机制直接
//! 回调，里面只做两件事——把动作入队（Mutex<VecDeque>）+ 对 egui
//! Context 调 request_repaint() 把 update() 敲醒来取。handler 可能在
//! 任意线程触发，所以绝不碰 UI 状态，入队 + 唤醒之外什么都不做。
//! 注意：设了全局 handler 后，接收通道就不再收到事件（二选一），
//! 所以旧的接收线程一并删了，不要再往 receiver() 上等。
//!
//! 退出时 update() 先调 Tray::shutdown() 把托盘图标撤掉（set_visible
//! (false) + drop），再发 Close 关窗——顺序反了的话进程退了图标还
//! 残留在托盘区，要等鼠标划过才消失（幽灵图标）。
//!
//! 非 Windows：tray-icon 在 Linux 上依赖 GTK + appindicator 系统库，
//! 无头构建机不具备，故 Cargo.toml 里只给 Windows 目标引入该依赖；
//! 这里保留骨架（接口与 Windows 版一致，poll 恒返回 None），
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
    /// 菜单「退出」。
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

    /// 取走一个待处理动作（UI 每帧调用）。
    pub fn poll(&mut self) -> Option<TrayAction> {
        let mut q = self.actions.lock().ok()?;
        q.pop_front()
    }

    /// 退出前撤掉托盘图标（见文件头注释），再由调用方关窗。
    pub fn shutdown(&mut self) {
        #[cfg(windows)]
        if let Some(inner) = &mut self.inner {
            inner.shutdown();
        }
    }

    /// 重建进行中：置灰 + 文案换成「正在建立索引…」，完成恢复。
    pub fn set_rebuild_enabled(&mut self, enabled: bool) {
        #[cfg(windows)]
        if let Some(inner) = &self.inner {
            inner.rebuild.set_enabled(enabled);
            inner.rebuild.set_text(if enabled {
                "重新建立索引"
            } else {
                "正在建立索引…"
            });
        }
        #[cfg(not(windows))]
        let _ = enabled;
    }
}

#[cfg(windows)]
mod win {
    use super::{TrayAction, TrayInit};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

    pub struct WinTray {
        icon: Option<TrayIcon>,
        pub rebuild: MenuItem,
    }

    /// 正式应用图标：assets/icon-64.rgba（64×64 原始 RGBA 字节，
    /// 父级与 .ico 一并生成），不再程序现画占位图。
    fn app_icon() -> Option<Icon> {
        let rgba = include_bytes!("../assets/icon-64.rgba");
        Icon::from_rgba(rgba.to_vec(), 64, 64).ok()
    }

    impl WinTray {
        pub fn create(
            ctx: egui::Context,
            actions: Arc<Mutex<VecDeque<TrayAction>>>,
            init: &TrayInit,
        ) -> Option<WinTray> {
            // 全局事件 handler：只入队 + request_repaint 唤醒（见文件头）。
            // handler 全局只能设一次，这里在建托盘时设，时机正好。
            {
                let queue = actions.clone();
                let ctx = ctx.clone();
                MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
                    let action = match ev.id.0.as_str() {
                        "show" => Some(TrayAction::Show),
                        "rebuild" => Some(TrayAction::RebuildIndex),
                        "settings" => Some(TrayAction::OpenSettings),
                        "about" => Some(TrayAction::About),
                        "exit" => Some(TrayAction::Exit),
                        _ => None,
                    };
                    if let Some(a) = action {
                        if let Ok(mut q) = queue.lock() {
                            q.push_back(a);
                        }
                        ctx.request_repaint();
                    }
                }));
            }
            {
                let queue = actions.clone();
                let ctx = ctx.clone();
                TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| {
                    // 左键抬起 = 切换显隐（双击会连发两次 Click，与旧版
                    // 接收线程方案及 C# 版双击切换的体感一致）
                    if let TrayIconEvent::Click {
                        button: tray_icon::MouseButton::Left,
                        button_state: tray_icon::MouseButtonState::Up,
                        ..
                    } = ev
                    {
                        if let Ok(mut q) = queue.lock() {
                            q.push_back(TrayAction::Toggle);
                        }
                        ctx.request_repaint();
                    }
                }));
            }

            let show_item = MenuItem::with_id("show", "打开瞬觅", true, None);
            let rebuild_item = MenuItem::with_id(
                "rebuild",
                if init.rebuilding {
                    "正在建立索引…"
                } else {
                    "重新建立索引"
                },
                !init.rebuilding,
                None,
            );
            let settings_item = MenuItem::with_id("settings", "设置", true, None);
            let about_item = MenuItem::with_id(
                "about",
                format!("关于瞬觅 v{}", env!("CARGO_PKG_VERSION")),
                true,
                None,
            );
            let exit_item = MenuItem::with_id("exit", "退出", true, None);
            let menu = Menu::new();
            let items: [&dyn tray_icon::menu::IsMenuItem; 5] = [
                &show_item,
                &rebuild_item,
                &settings_item,
                &about_item,
                &exit_item,
            ];
            if menu.append_items(&items).is_err() {
                return None;
            }
            let mut builder = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip("瞬觅 ShunSeek");
            if let Some(icon) = app_icon() {
                builder = builder.with_icon(icon);
            }
            let icon = builder.build().ok()?;
            Some(WinTray {
                icon: Some(icon),
                rebuild: rebuild_item,
            })
        }

        /// 撤图标：先设不可见，再 drop 掉 TrayIcon 本体（drop 本身也会
        /// 从托盘区移除，双保险防幽灵图标残留）。
        pub fn shutdown(&mut self) {
            if let Some(icon) = self.icon.take() {
                let _ = icon.set_visible(false);
                drop(icon);
            }
        }
    }
}
