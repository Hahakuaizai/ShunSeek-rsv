//! 瞬觅 ShunSeek 界面（Rust 版，egui 重绘）。
//! 对照原 WPF 版逐项搬：MainWindow.xaml 的四段布局、MainViewModel 的
//! 搜索调度（取消 + 30ms 防抖 + 后台线程）、App.xaml.cs 的启动装配
//! （单实例除外，见下）、HotkeyManager 的 Alt+Space、TrayIconManager 的托盘。
//!
//! 与 WPF 版的差异（如实记录）：
//!  - 背景：真 Mica 需要 DWM 的 DWMWA_SYSTEMBACKDROP_TYPE，egui 不直接
//!    支持；这里用手工竖向渐变 + 圆角 + 投影 + 细描边近似（见 update()
//!    里的 paint_vertical_gradient 与 CentralPanel 的 Frame）。
//!  - 视觉语言（v0.1.2）：借 LeiBox 的玻璃/光束设计——搜索框光束游走
//!    描边、分类玻璃标签栏 + 亮色磨砂胶囊、窗口顶部高光带与双层描边、
//!    色块玻璃高光、弹窗深色玻璃；粒子/折射类重动效 egui 做不到，未移植。
//!  - 主题（v0.1.3）：全部取色收进 Palette（深/浅两套），深色数值与
//!    v0.1.2 逐项一致；浅色按 LeiBox 原作的浅色磨砂感另定。状态栏右侧
//!    按钮切换，选择存数据目录 theme.txt，启动时读回。
//!  - v0.1.5：结果行真实系统图标（icons.rs，Windows SHGetFileInfoW
//!    后台取图）；exe/窗口/托盘正式图标（build.rs + assets）；垃圾
//!    结果降权后归类到尾部折叠（demoted 分组，普通同父 ≥4 也折叠）；
//!    宫格模式（view.txt 持久化）；悬停 0.4 秒玻璃详情卡（metadata
//!    按需取、按路径缓存）；首页 = 常用（空查询按 UsageStore 排序）；
//!    索引分级建立（应用+浅层先行、逐盘 MFT 并入）+ 缓存直存
//!    index.bin（save_to 自带 tmp+rename）；托盘菜单补全（打开 /
//!    重建索引 / 深色·宫格·自启动勾选 / 关于 / 退出）且事件走全局
//!    handler 唤醒队列，隐藏时也能退出；浅色去灰（近纯白 + 描边分层）。
//!  - v0.1.6（真机暴雷修复 + 视觉照抄原版）：托盘改专职线程 + 原生
//!    Win32（tray.rs），退出独立于 UI 线程；搜索冻结根治（切片前先
//!    收敛字符边界 + 全 app 锁中毒恢复 + 搜索线程 catch_unwind 兜底）；
//!    图标 worker 初始化 COM + 失败回退通用图标 + 计数可观测；窗口
//!    补齐最小化/最大化/关闭（✕ 恒隐藏到托盘，不再有 close_action
//!    设置）、可拖可缩放（780×540 / 最小 600×400）、进任务栏、不再
//!    置顶 + 失焦自动隐藏；首页改用户自选 pins（pins.txt，强制宫格、
//!    只显名称、悬浮看详情）；筛选面板点外可关、选中态改原版 Pill
//!    语言；视觉令牌整体换成原版 .NET 的 ThemeManager 色板与布局
//!    常量（朱砂强调、Ink 三段渐变、Serif 书眉/名单），逐项对照见
//!    docs/feature-parity.md「设计令牌」节。
//!  - 单实例：v0.1.8 落地（命名 Mutex + 命名事件唤醒已有实例，
//!    见 main() 与 WAKE_FLAG）。
//!  - 全局热键：轮询实现，见 hotkey.rs；非 Windows 无全局热键。
//!  - v0.1.10（真机三诉紧急修复）：失焦隐藏整套拆除、换原版
//!    GlobalClickWatcher 点外隐藏（clickwatch.rs）；全窗拖动背景
//!    层删除、改控件矩形排除的手判拖动；四角双根因（eframe 默认
//!    clear color 暗 veil + 渐变分带底部直弦）根治；顶部收紧
//!    （书眉/搜索框 46/54 → 36/44，用户裁决覆盖原版令牌）；筛选
//!    面板 chips 化 + 左竖右横展开动画 + 全集归一化；内容层动画
//!    一组；绿色版数据布局（exe 旁 data/）。逐项见 CHANGELOG。
//!  - v0.1.12（可见性整层换原生）：v0.1.6–v0.1.11 六版真机唤回
//!    全灭的统一病根是「隐藏时 UI 线程靠 repaint 唤醒再消费队列」
//!    这条链不可靠。主窗口 HWND 启动即存入全局原子量（winvis.rs），
//!    显示/隐藏/最小化/还原全部改走 ShowWindow 且由发起线程
//!    （托盘/热键/钩子/单实例唤醒）当场执行，状态判定现读
//!    IsWindowVisible/IsIconic；UI 侧 show()/hide() 收敛为同一组
//!    Win32 调用的薄封装，repaint 只负责呼出后的副作用帧。

#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod autostart;
mod clickwatch;
mod demo;
mod hotkey;
mod icons;
mod names;
mod opener;
mod tray;
mod winvis;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{
    Align, Align2, Color32, FontId, Key, Layout, RichText, Rounding, Stroke,
    TextFormat, Vec2, ViewportCommand,
};
use shunseek_core::delta::Tombstones;
#[cfg(windows)]
use shunseek_core::delta::DeltaIndex;
use shunseek_core::filetype::{self, Importance};
use shunseek_core::{
    build_index, search_merged, Category, IndexStore, RawEntry, ScopeFilter, SearchEngine,
    UsageStore,
};

const RESULT_LIMIT: usize = 100;
const ROW_H: f32 = 52.0;

/// 宫格格子尺寸（宽×高）：图标 40 + 名字至多 2 行。
const GRID_CELL_W: f32 = 104.0;
const GRID_CELL_H: f32 = 96.0;
/// 首页磁贴尺寸（原版 TileItem）：170×92，项间距 12。
// 首页磁贴（v0.1.7 用户裁决，覆盖原版 170×92 大卡片令牌）：
// 桌面图标式紧凑格 96×88、间隔 8。
const HOME_TILE_W: f32 = 96.0;
const HOME_TILE_H: f32 = 88.0;
const HOME_TILE_GAP: f32 = 8.0;
/// 空索引护栏文案：数据源全灭时亮红字，让用户能原样转达给开发者。
const ALERT_EMPTY_INDEX: &str = "索引为空（0 条）——数据源都没拿到数据，把这行字发给开发者";

// ---------------------------------------------------------------- 主题

#[derive(Clone, Copy, PartialEq, Eq)]
enum Theme {
    Dark,
    Light,
}

impl Theme {
    fn palette(self) -> Palette {
        match self {
            Theme::Dark => Palette::dark(),
            Theme::Light => Palette::light(),
        }
    }

    fn toggled(self) -> Theme {
        match self {
            Theme::Dark => Theme::Light,
            Theme::Light => Theme::Dark,
        }
    }
}

/// 全套界面取色：绘制代码只从 Palette 取色，不写字面量。
/// v0.1.6 起数值整体换成原版 .NET 的设计令牌（ThemeManager.cs 的
/// 37 个命名画刷 + InkFill/TopGlow 渐变），字段名尽量沿用原版令牌名，
/// 逐项对照见 docs/feature-parity.md「设计令牌」节。此前 LeiBox 玻璃
/// 语言（白胶囊选中 / 光束描边 / 玻璃高光）整体退役。
/// 用构造函数而非 const：from_rgba_unmultiplied 等不是 const fn。
#[derive(Clone, Copy)]
struct Palette {
    /// 窗口底 InkFill 三段渐变（上 / 0.55 处 / 下）。
    grad_top: Color32,
    grad_mid: Color32,
    grad_bottom: Color32,
    /// 顶部光晕 TopGlow 的峰值 alpha（原版渐变峰值 × 层不透明度 0.5 已折入）。
    top_highlight_peak: u8,
    /// 窗口外壳描边 ShellStroke。
    panel_stroke: Color32,
    /// 设置窗口等独立 viewport 的底色（取 InkFill 底段近似）。
    panel_fill: Color32,
    /// extreme_bg（输入控件深底）= Surface。
    extreme_bg: Color32,
    /// 菜单/弹窗：MenuFill / MenuStroke / 投影色（ShadowColor × ShadowOpacity）。
    popup_fill: Color32,
    popup_stroke: Color32,
    popup_shadow_color: Color32,
    /// 朱砂强调 AccentBrush（强调文字 AccentText 落在
    /// chip_sel_text_rgb 与 highlight 两槽，同值）。
    accent: Color32,
    /// 主文字 TextHi / 次文字 TextMid。
    name_text: Color32,
    text_mid: Color32,
    /// 弱文字 TextLow：路径 / 状态 / 副标题共用（三个语义槽同值）。
    path_text: Color32,
    empty_text: Color32,
    status_text: Color32,
    /// 发丝线 HairLine / 一般描边 EdgeLine / 强描边 EdgeStrong。
    hairline: Color32,
    edge_line: Color32,
    edge_strong: Color32,
    /// 表面三态：Surface / SurfaceHover / SurfaceSel。
    surface: Color32,
    surface_hover: Color32,
    surface_sel: Color32,
    /// 搜索框：填充 = Surface、放大镜 = TextLow、输入文字 = TextHi。
    search_fill: Color32,
    search_icon: Color32,
    search_text: Color32,
    /// 占位文字色（两主题都显式给 TextLow）。
    hint: Option<Color32>,
    /// 小芯片（Enter 键帽 / 工具栏小片）：ChipFill 底 / TextLow 字。
    esc_fill: Color32,
    esc_text: Color32,
    /// 筛选控件收起态：底 = Surface、描边 = EdgeLine。
    bar_fill: Color32,
    bar_stroke: Color32,
    /// chip 未选 / 选中文字 RGB（选中过渡按进度插值）：TextMid / AccentText。
    chip_text_rgb: (u8, u8, u8),
    chip_sel_text_rgb: (u8, u8, u8),
    /// 选中语言（原版 FilterChip）：PillFill 淡填充 / PillHover / PillStroke 描边。
    pill_fill: Color32,
    pill_hover: Color32,
    pill_stroke: Color32,
    /// 未选行/chip 的 hover 填充 = MenuHover。
    chip_hover_fill: Color32,
    /// 结果行三态：RowHover / RowSel / RowSelStroke。
    row_hover_fill: Color32,
    row_sel_fill: Color32,
    row_sel_stroke: Color32,
    /// 折叠组行底 FoldFill / 图标占位盘 IconPlate。
    fold_fill: Color32,
    icon_plate: Color32,
    /// 名字命中段高亮 = AccentText。
    highlight: Color32,
    /// 警戒态文字（空索引护栏）= CloseHover 红。
    alert_text: Color32,
    /// 首页固定标记 PinMark（朱砂 @70%）。
    pin_mark: Color32,
    /// 重要度点三档：ImportanceHigh / Normal / Low。
    importance_high: Color32,
    importance_normal: Color32,
    importance_low: Color32,
    /// 关闭按钮悬停 / 按下底与悬停字色（两主题同值）。
    close_hover: Color32,
    close_pressed: Color32,
    close_text: Color32,
}
impl Palette {

    /// 深色：v0.1.2 全部硬编码数值原样。
    /// 深色（原版默认）：ThemeManager.cs 深色画刷逐值。
    fn dark() -> Self {
        Self {
            grad_top: Color32::from_rgb(18, 24, 28), // InkFill #12181C
            grad_mid: Color32::from_rgb(14, 20, 24), // InkFill #0E1418
            grad_bottom: Color32::from_rgb(10, 15, 18), // InkFill #0A0F12
            top_highlight_peak: 10,                  // TopGlow 白@8% × 层 0.5
            panel_stroke: Color32::from_rgb(42, 52, 58), // ShellStroke #2A343A
            panel_fill: Color32::from_rgb(10, 15, 18),
            extreme_bg: Color32::from_rgb(22, 29, 34), // Surface #161D22
            popup_fill: Color32::from_rgba_unmultiplied(20, 26, 31, 250), // MenuFill #141A1F
            popup_stroke: Color32::from_rgb(38, 48, 54), // MenuStroke #263036
            popup_shadow_color: Color32::from_black_alpha(140), // #000 @0.55
            accent: Color32::from_rgb(196, 87, 63),    // AccentBrush #C4573F
            name_text: Color32::from_rgb(237, 233, 225), // TextHi #EDE9E1
            text_mid: Color32::from_rgb(154, 165, 171), // TextMid #9AA5AB
            path_text: Color32::from_rgb(115, 126, 132), // TextLow #737E84
            empty_text: Color32::from_rgb(115, 126, 132),
            status_text: Color32::from_rgb(115, 126, 132),
            hairline: Color32::from_rgb(30, 38, 43),   // HairLine #1E262B
            edge_line: Color32::from_rgb(36, 45, 51),  // EdgeLine #242D33
            edge_strong: Color32::from_rgb(48, 59, 66), // EdgeStrong #303B42
            surface: Color32::from_rgb(22, 29, 34),    // Surface #161D22
            surface_hover: Color32::from_rgb(28, 36, 42), // SurfaceHover #1C242A
            surface_sel: Color32::from_rgb(33, 43, 50), // SurfaceSel #212B32
            search_fill: Color32::from_rgb(22, 29, 34),
            search_icon: Color32::from_rgb(115, 126, 132),
            search_text: Color32::from_rgb(237, 233, 225),
            hint: Some(Color32::from_rgb(115, 126, 132)),
            esc_fill: Color32::from_rgb(26, 34, 39),   // ChipFill #1A2227
            esc_text: Color32::from_rgb(115, 126, 132),
            bar_fill: Color32::from_rgb(22, 29, 34),
            bar_stroke: Color32::from_rgb(36, 45, 51),
            chip_text_rgb: (154, 165, 171),
            chip_sel_text_rgb: (217, 122, 95),
            pill_fill: Color32::from_rgba_unmultiplied(196, 87, 63, 31), // @12%
            pill_hover: Color32::from_rgba_unmultiplied(196, 87, 63, 58), // @23%
            pill_stroke: Color32::from_rgba_unmultiplied(196, 87, 63, 89), // @35%
            chip_hover_fill: Color32::from_rgb(30, 40, 46), // MenuHover #1E282E
            row_hover_fill: Color32::from_rgb(22, 29, 34), // RowHover #161D22
            row_sel_fill: Color32::from_rgb(33, 43, 50), // RowSel #212B32
            row_sel_stroke: Color32::from_rgb(47, 59, 67), // RowSelStroke #2F3B43
            fold_fill: Color32::from_rgb(21, 29, 34),  // FoldFill #151D22
            icon_plate: Color32::from_rgb(27, 36, 42), // IconPlate #1B242A
            highlight: Color32::from_rgb(217, 122, 95),
            alert_text: Color32::from_rgb(193, 68, 50), // CloseHover #C14432
            pin_mark: Color32::from_rgba_unmultiplied(196, 87, 63, 179), // @70%
            importance_high: Color32::from_rgb(196, 87, 63),
            importance_normal: Color32::from_rgb(124, 136, 144), // #7C8890
            importance_low: Color32::from_rgb(74, 85, 92), // #4A555C
            close_hover: Color32::from_rgb(193, 68, 50),
            close_pressed: Color32::from_rgb(166, 55, 40), // #A63728
            close_text: Color32::from_rgb(253, 247, 242), // #FDF7F2
        }
    }
    /// 浅色：LeiBox 原作的浅色磨砂感——近白渐变底、白卡片浮起、
    /// 深字、强调蓝只出现在选中行与聚焦处。
    /// v0.1.5 按用户反馈去灰：渐变改近纯白（#FFFFFF → #F6F7F9），
    /// 不再用带灰蓝的 #F7F8FA→#E9ECF1；底变白后，原先靠半透明白叠
    /// 灰底显形的元素（搜索框 / 玻璃栏）提高不透明度、描边加深，
    /// 改靠描边分层。
    /// 浅色：ThemeManager.cs 浅色画刷逐值。
    fn light() -> Self {
        Self {
            // v0.1.7 用户裁决（覆盖原版浅色令牌）：浅色底纯白，
            // InkFill 改中性微渐变 #FFFFFF → #FDFDFD → #FAFAFA，
            // 去掉原版的绿/灰色偏；层次靠描边与中性填充区分。
            grad_top: Color32::from_rgb(255, 255, 255), // 纯白 #FFFFFF
            grad_mid: Color32::from_rgb(253, 253, 253), // #FDFDFD
            grad_bottom: Color32::from_rgb(250, 250, 250), // #FAFAFA
            top_highlight_peak: 51,                     // TopGlow 白@40% × 层 0.5
            panel_stroke: Color32::from_rgb(200, 200, 200), // 中性化（原 ShellStroke #C3CBC4 带绿调）
            panel_fill: Color32::from_rgb(250, 250, 250),
            extreme_bg: Color32::from_rgb(255, 255, 255), // Surface #FFFFFF
            popup_fill: Color32::from_rgba_unmultiplied(255, 255, 255, 250), // MenuFill 纯白
            popup_stroke: Color32::from_rgb(216, 216, 216), // 中性化（原 MenuStroke #D6DCD5）
            popup_shadow_color: Color32::from_rgba_unmultiplied(0, 0, 0, 71), // 中性黑 @0.28（原 #4A5A50 绿调）
            accent: Color32::from_rgb(178, 67, 44),     // AccentBrush #B2432C
            name_text: Color32::from_rgb(28, 28, 28),   // 中性化（原 TextHi #1B2320 带绿调）
            text_mid: Color32::from_rgb(90, 90, 90),    // 中性化（原 TextMid #53605A）
            path_text: Color32::from_rgb(117, 117, 117), // 中性化（原 TextLow #6F7B74）
            empty_text: Color32::from_rgb(117, 117, 117),
            status_text: Color32::from_rgb(117, 117, 117),
            hairline: Color32::from_rgb(222, 222, 222), // 中性化（原 HairLine #DDE2DC）
            edge_line: Color32::from_rgb(207, 207, 207), // 中性化（原 EdgeLine #CCD3CC）
            edge_strong: Color32::from_rgb(181, 181, 181), // 中性化（原 EdgeStrong #B0BBB2）
            surface: Color32::from_rgb(255, 255, 255),  // Surface #FFFFFF
            surface_hover: Color32::from_rgb(245, 245, 245), // 中性化（原 SurfaceHover #F4F6F3）
            surface_sel: Color32::from_rgb(236, 236, 236), // 中性化（原 SurfaceSel #EAEFE9）
            search_fill: Color32::from_rgb(255, 255, 255),
            search_icon: Color32::from_rgb(117, 117, 117),
            search_text: Color32::from_rgb(28, 28, 28),
            hint: Some(Color32::from_rgb(117, 117, 117)),
            esc_fill: Color32::from_rgb(243, 243, 243), // 中性化（原 ChipFill #F2F4F1）
            esc_text: Color32::from_rgb(117, 117, 117),
            bar_fill: Color32::from_rgb(255, 255, 255),
            bar_stroke: Color32::from_rgb(207, 207, 207),
            chip_text_rgb: (90, 90, 90),
            chip_sel_text_rgb: (156, 58, 36),
            pill_fill: Color32::from_rgba_unmultiplied(178, 67, 44, 26), // @10%
            pill_hover: Color32::from_rgba_unmultiplied(178, 67, 44, 48), // @19%
            pill_stroke: Color32::from_rgba_unmultiplied(178, 67, 44, 89), // @35%
            chip_hover_fill: Color32::from_rgb(238, 238, 238), // 中性化（原 MenuHover #EDF1EC）
            row_hover_fill: Color32::from_rgb(243, 243, 243), // 中性化（原 RowHover #F2F5F1）
            row_sel_fill: Color32::from_rgb(234, 234, 234), // 中性化（原 RowSel #E8EDE7）
            row_sel_stroke: Color32::from_rgb(203, 203, 203), // 中性化（原 RowSelStroke #C8D1C8）
            fold_fill: Color32::from_rgb(241, 241, 241), // 中性化（原 FoldFill #F0F3EF）
            icon_plate: Color32::from_rgb(238, 238, 238), // 中性化（原 IconPlate #EDEFEA）
            highlight: Color32::from_rgb(156, 58, 36),
            alert_text: Color32::from_rgb(193, 68, 50),
            pin_mark: Color32::from_rgba_unmultiplied(178, 67, 44, 179), // @70%
            importance_high: Color32::from_rgb(178, 67, 44),
            importance_normal: Color32::from_rgb(143, 143, 143), // 中性化（原 #8A948D）
            importance_low: Color32::from_rgb(197, 197, 197), // 中性化（原 #C2CAC2）
            close_hover: Color32::from_rgb(193, 68, 50),
            close_pressed: Color32::from_rgb(166, 55, 40),
            close_text: Color32::from_rgb(255, 255, 255),
        }
    }

    /// 两块调色板逐字段插值（主题切换 170ms 内容层交叉淡入用，
    /// v0.1.10）。含 alpha 的颜色按未预乘通道插值——两块板同名
    /// 字段的 alpha 约定一致（pill_fill 等都是朱砂定值 alpha），
    /// 插值结果语义正确。
    fn lerp(&self, other: &Palette, t: f32) -> Palette {
        fn lc(a: Color32, b: Color32, t: f32) -> Color32 {
            let m = |x: u8, y: u8| -> u8 {
                (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8
            };
            Color32::from_rgba_unmultiplied(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()), m(a.a(), b.a()))
        }
        fn lt(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
            let c = lc(
                Color32::from_rgb(a.0, a.1, a.2),
                Color32::from_rgb(b.0, b.1, b.2),
                t,
            );
            (c.r(), c.g(), c.b())
        }
        Palette {
            grad_top: lc(self.grad_top, other.grad_top, t),
            grad_mid: lc(self.grad_mid, other.grad_mid, t),
            grad_bottom: lc(self.grad_bottom, other.grad_bottom, t),
            top_highlight_peak: (self.top_highlight_peak as f32
                + (other.top_highlight_peak as f32 - self.top_highlight_peak as f32) * t)
                .round()
                .clamp(0.0, 255.0) as u8,
            panel_stroke: lc(self.panel_stroke, other.panel_stroke, t),
            panel_fill: lc(self.panel_fill, other.panel_fill, t),
            extreme_bg: lc(self.extreme_bg, other.extreme_bg, t),
            popup_fill: lc(self.popup_fill, other.popup_fill, t),
            popup_stroke: lc(self.popup_stroke, other.popup_stroke, t),
            popup_shadow_color: lc(self.popup_shadow_color, other.popup_shadow_color, t),
            accent: lc(self.accent, other.accent, t),
            name_text: lc(self.name_text, other.name_text, t),
            text_mid: lc(self.text_mid, other.text_mid, t),
            path_text: lc(self.path_text, other.path_text, t),
            empty_text: lc(self.empty_text, other.empty_text, t),
            status_text: lc(self.status_text, other.status_text, t),
            hairline: lc(self.hairline, other.hairline, t),
            edge_line: lc(self.edge_line, other.edge_line, t),
            edge_strong: lc(self.edge_strong, other.edge_strong, t),
            surface: lc(self.surface, other.surface, t),
            surface_hover: lc(self.surface_hover, other.surface_hover, t),
            surface_sel: lc(self.surface_sel, other.surface_sel, t),
            search_fill: lc(self.search_fill, other.search_fill, t),
            search_icon: lc(self.search_icon, other.search_icon, t),
            search_text: lc(self.search_text, other.search_text, t),
            hint: match (self.hint, other.hint) {
                (Some(a), Some(b)) => Some(lc(a, b, t)),
                _ => {
                    if t < 0.5 {
                        self.hint
                    } else {
                        other.hint
                    }
                }
            },
            esc_fill: lc(self.esc_fill, other.esc_fill, t),
            esc_text: lc(self.esc_text, other.esc_text, t),
            bar_fill: lc(self.bar_fill, other.bar_fill, t),
            bar_stroke: lc(self.bar_stroke, other.bar_stroke, t),
            chip_text_rgb: lt(self.chip_text_rgb, other.chip_text_rgb, t),
            chip_sel_text_rgb: lt(self.chip_sel_text_rgb, other.chip_sel_text_rgb, t),
            pill_fill: lc(self.pill_fill, other.pill_fill, t),
            pill_hover: lc(self.pill_hover, other.pill_hover, t),
            pill_stroke: lc(self.pill_stroke, other.pill_stroke, t),
            chip_hover_fill: lc(self.chip_hover_fill, other.chip_hover_fill, t),
            row_hover_fill: lc(self.row_hover_fill, other.row_hover_fill, t),
            row_sel_fill: lc(self.row_sel_fill, other.row_sel_fill, t),
            row_sel_stroke: lc(self.row_sel_stroke, other.row_sel_stroke, t),
            fold_fill: lc(self.fold_fill, other.fold_fill, t),
            icon_plate: lc(self.icon_plate, other.icon_plate, t),
            highlight: lc(self.highlight, other.highlight, t),
            alert_text: lc(self.alert_text, other.alert_text, t),
            pin_mark: lc(self.pin_mark, other.pin_mark, t),
            importance_high: lc(self.importance_high, other.importance_high, t),
            importance_normal: lc(self.importance_normal, other.importance_normal, t),
            importance_low: lc(self.importance_low, other.importance_low, t),
            close_hover: lc(self.close_hover, other.close_hover, t),
            close_pressed: lc(self.close_pressed, other.close_pressed, t),
            close_text: lc(self.close_text, other.close_text, t),
        }
    }
}
// ---------------------------------------------------------------- 设置存储

/// 全部可持久化设置（v0.1.5 统一）：数据目录 settings.txt，key=value 行。
/// 键：theme / view / hide_on_close / hotkey / result_limit / fold_demoted /
/// hide_outside（点外自动隐藏，缺省 0 = 关：窗口点外面保持原样）。
/// 开机自启动不入文件（以注册表实际状态为准，见 autostart.rs）。
/// load 时 theme/view 在 settings.txt 缺省时回退老版 theme.txt /
/// view.txt 的值（老用户无感迁移）；save 只写 settings.txt，
/// 老文件留着不动。
/// 关窗行为（v0.1.6 用户钉死）：✕ 恒最小化到托盘，无条件，无设置项；
/// 进程唯一正常退出 = 托盘菜单「退出」。旧 settings.txt 里的
/// close_action / hide_on_close 键加载时直接忽略（不读不写）。
#[derive(Clone)]
struct Settings {
    theme: Theme,
    view: ViewMode,
    hotkey: hotkey::HotkeyCombo,
    result_limit: usize,
    fold_demoted: bool,
    /// 点击主窗/设置窗/托盘图标之外时是否自动隐藏到托盘。
    /// v0.1.14 起可配置且缺省关闭：原版「点外即藏」在真机上
    /// 表现为「点别的窗口软件连同任务栏图标一起消失」，对把它
    /// 当常驻小工具用的场景是干扰，故改为用户显式开启。
    hide_outside: bool,
    /// 搜索范围选中集（scope_* 键）：预设键名 / 盘符 / 自定义完整路径。
    scope_presets: Vec<String>,
    scope_drives: Vec<char>,
    /// 已添加的自定义文件夹全集（含未选中的）。
    scope_custom: Vec<String>,
    /// 其中真正参与搜索的那部分（可多选）。
    scope_custom_sel: Vec<String>,
}

fn load_settings() -> Settings {
    let dir = data_dir();
    let mut kv: HashMap<String, String> = HashMap::new();
    let mut scope_custom: Vec<String> = Vec::new();
    let mut scope_custom_sel: Vec<String> = Vec::new();
    let mut has_custom_sel_key = false;
    if let Ok(text) = std::fs::read_to_string(dir.join("settings.txt")) {
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                let (k, v) = (k.trim(), v.trim());
                // scope_custom / scope_custom_sel 可重复多行（一行一个
                // 文件夹），单独收集。前者是全集，后者是选中集。
                if k == "scope_custom" {
                    if !v.is_empty() {
                        scope_custom.push(v.to_string());
                    }
                } else if k == "scope_custom_sel" {
                    has_custom_sel_key = true;
                    if !v.is_empty() {
                        scope_custom_sel.push(v.to_string());
                    }
                } else {
                    kv.insert(k.to_string(), v.to_string());
                }
            }
        }
    }
    // 旧档只有 scope_custom（那时全部自定义都参与搜索）：没有
    // scope_custom_sel 键就把它们全当成选中，保持旧行为。
    if !has_custom_sel_key {
        scope_custom_sel = scope_custom.clone();
    }
    // 范围缺省（settings.txt 里没有任何 scope 键）= 全部：
    // 预设全选 + 现存磁盘全选。有键则按存的值（选中集语义见工具栏）。
    let (mut scope_presets, mut scope_drives) =
        if !kv.contains_key("scope_presets") && !kv.contains_key("scope_drives") {
            (
                PRESET_KEYS.iter().map(|(k, _, _)| k.to_string()).collect(),
                existing_drives(),
            )
        } else {
            (
                kv.get("scope_presets")
                    .map(|s| {
                        s.split(',')
                            .map(|x| x.trim().to_string())
                            .filter(|x| !x.is_empty())
                            .collect::<Vec<String>>()
                    })
                    .unwrap_or_default(),
                kv.get("scope_drives")
                    .map(|s| s.chars().filter(|c| c.is_ascii_uppercase()).collect::<Vec<char>>())
                    .unwrap_or_default(),
            )
        };
    // 全集归一化（加载层，v0.1.10）：存档里若预设 ⊇ 全部预设且
    // 磁盘 ⊇ 全部现存盘，折叠为规范全集（与 scope_select_all
    // 写出的值逐字一致）——语义等价（全集包含一切），但保证状态
    // 表示唯一：全选只有一个长相，不存在「逐项全选」的第二种
    // 表示漏到渲染层去逐项点亮（渲染层另有归一化兜底，见
    // draw_scope_panel / draw_type_panel 的 all_now / full）。
    {
        let presets_full = PRESET_KEYS
            .iter()
            .all(|(k, _, _)| scope_presets.iter().any(|p| p == k));
        let drives_now = existing_drives();
        let drives_full = drives_now.iter().all(|d| scope_drives.contains(d));
        if presets_full && drives_full {
            scope_presets = PRESET_KEYS.iter().map(|(k, _, _)| k.to_string()).collect();
            scope_drives = drives_now;
        }
    }
    let theme = match kv.get("theme").map(|s| s.as_str()) {
        Some("light") => Theme::Light,
        Some("dark") => Theme::Dark,
        _ => match std::fs::read_to_string(dir.join("theme.txt")) {
            Ok(s) if s.trim() == "light" => Theme::Light,
            _ => Theme::Dark,
        },
    };
    let view = match kv.get("view").map(|s| s.as_str()) {
        Some("grid") => ViewMode::Grid,
        Some("list") => ViewMode::List,
        _ => match std::fs::read_to_string(dir.join("view.txt")) {
            Ok(s) if s.trim() == "grid" => ViewMode::Grid,
            _ => ViewMode::List,
        },
    };
    let bool_or = |key: &str, default: bool| match kv.get(key).map(|s| s.as_str()) {
        Some("1") => true,
        Some("0") => false,
        _ => default,
    };
    Settings {
        theme,
        view,
        // close_action / hide_on_close 键：v0.1.6 起忽略（✕ 恒到托盘）
        // 用户存过的值优先（解析成功就用，绝不覆盖）；没有/解析
        // 失败才走 v0.1.8 候选链解析默认（Ctrl+Alt+Space 起）。
        hotkey: kv
            .get("hotkey")
            .and_then(|s| hotkey::HotkeyCombo::parse(s))
            .unwrap_or_else(hotkey::resolve_default),
        result_limit: kv
            .get("result_limit")
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|n| [60usize, 100, 200].contains(n))
            .unwrap_or(RESULT_LIMIT),
        fold_demoted: bool_or("fold_demoted", true),
        hide_outside: bool_or("hide_outside", false),
        scope_presets,
        scope_drives,
        scope_custom,
        scope_custom_sel,
    }
}

/// 首页 pins 读取（数据目录 pins.txt，一行一条完整路径；
/// 文件缺失/为空都是合法状态——首页显示引导文案 + 添加格）。
fn load_pins(dir: &PathBuf) -> Vec<String> {
    std::fs::read_to_string(dir.join("pins.txt"))
        .map(|text| {
            text.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// 范围预设：(settings 键名, %USERPROFILE% 下子目录, 显示名)。
/// home 的子目录为空串 = 用户主目录本身。
const PRESET_KEYS: [(&str, &str, &str); 4] = [
    ("desktop", "Desktop", "桌面"),
    ("documents", "Documents", "文档"),
    ("downloads", "Downloads", "下载"),
    ("home", "", "用户主目录"),
];

/// 当前实际存在的固定盘盘符（带 30s 缓存，Windows 上用 GetLogicalDrives + DRIVE_FIXED 避免探测网络/休眠盘）。
fn existing_drives() -> Vec<char> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Mutex<(Instant, Vec<char>)>> = OnceLock::new();
    let lock = CACHE.get_or_init(|| Mutex::new((Instant::now() - Duration::from_secs(60), Vec::new())));
    let mut g = lock_rec(lock);
    if !g.1.is_empty() && g.0.elapsed() < Duration::from_secs(30) {
        return g.1.clone();
    }
    let list = compute_existing_drives();
    *g = (Instant::now(), list.clone());
    list
}

fn compute_existing_drives() -> Vec<char> {
    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
        const DRIVE_FIXED: u32 = 3;
        let mask = unsafe { GetLogicalDrives() };
        let mut out = Vec::new();
        for i in 2..26u32 {
            if (mask & (1 << i)) == 0 {
                continue;
            }
            let d = (b'A' + i as u8) as char;
            let root: Vec<u16> = OsStr::new(&format!("{d}:\\"))
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            if unsafe { GetDriveTypeW(root.as_ptr()) } == DRIVE_FIXED {
                out.push(d);
            }
        }
        out
    }
    #[cfg(not(windows))]
    {
        ('C'..='Z')
            .filter(|d| std::path::Path::new(&format!("{d}:\\")).exists())
            .collect()
    }
}

/// 全部 12 个分类（类型筛选的全集状态）。
const ALL_CATEGORIES: [Category; 12] = [
    Category::App,
    Category::Shortcut,
    Category::InstallDir,
    Category::Folder,
    Category::Document,
    Category::Image,
    Category::Video,
    Category::Audio,
    Category::Archive,
    Category::Code,
    Category::Executable,
    Category::Other,
];

fn all_categories() -> HashSet<Category> {
    ALL_CATEGORIES.iter().copied().collect()
}

/// 哪个筛选控件处于展开态（两控件互斥，同时只许一个展开）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum FilterKind {
    Type,
    Scope,
}

// ---------------------------------------------------------------- 视图模式

/// 结果展示模式（v0.1.5）：列表 / 宫格。持久化到数据目录 view.txt
///（"grid" / "list"，默认列表），chips 栏右端按钮与托盘菜单都能切。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    List,
    Grid,
}

impl ViewMode {
    fn toggled(self) -> ViewMode {
        match self {
            ViewMode::List => ViewMode::Grid,
            ViewMode::Grid => ViewMode::List,
        }
    }
}

/// （load/save 已统一到上面的 Settings / load_settings /
/// ShunSeekApp::save_settings，view 键与 chips 栏页面按钮共用。）

// ---------------------------------------------------------------- 字体

// 启动时把系统 CJK 字体塞进 egui：egui 自带字体没有汉字，不装就是豆腐块。
// 无衬线走 Proportional（界面主体），另装 Serif 家族供原版 SerifFont
// 的落点（书眉标题 / 搜索输入 / 结果名 / 磁贴名）使用。

/// Serif 家族名（原版 SerifFont 的 egui 落点，见 install_cjk_font）。
const SERIF_FAMILY: &str = "serif";

/// Serif 字体快捷构造（书眉标题 22 / 搜索输入 17 / 结果名 14 /
/// 组行 12 / 磁贴名 12.5 / 空状态 14 都用它）。
fn serif(size: f32) -> FontId {
    FontId::new(size, egui::FontFamily::Name(SERIF_FAMILY.into()))
}

fn install_cjk_font(ctx: &egui::Context) {
    let sans_candidates: &[&str] = &[
        // Windows
        r"C:\Windows\Fonts\msyh.ttc", // 微软雅黑
        r"C:\Windows\Fonts\simhei.ttf",
        // Linux（发行版路径不一，逐个试）
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        // macOS
        "/System/Library/Fonts/PingFang.ttc",
    ];
    // 原版 SerifFont 栈：Noto Serif SC → 华文中宋 → SimSun（宋体）。
    // 本机一般只有宋体可命中；Noto Serif SC 若装了优先。
    let serif_candidates: &[&str] = &[
        r"C:\Windows\Fonts\NotoSerifSC-Regular.otf",
        r"C:\Windows\Fonts\simsun.ttc", // 宋体
        "/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSerifCJK-Regular.ttc",
        "/System/Library/Fonts/Supplemental/Songti.ttc",
    ];
    let load = |paths: &[&str]| paths.iter().find_map(|p| std::fs::read(p).ok());
    let sans = load(sans_candidates);
    let serif_bytes = load(serif_candidates);
    if sans.is_none() && serif_bytes.is_none() {
        eprintln!("ShunSeek: 未找到系统 CJK 字体，汉字可能显示为缺字框");
        return;
    }
    let mut fonts = egui::FontDefinitions::default();
    if let Some(bytes) = sans {
        fonts.font_data.insert(
            "cjk".to_owned(),
            egui::FontData::from_owned(bytes),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            if let Some(list) = fonts.families.get_mut(&family) {
                list.insert(0, "cjk".to_owned());
            }
        }
    }
    // Serif 家族：优先真衬线字体，缺失时回退无衬线 CJK（汉字不缺字，
    // 只是字形不衬线，属已知近似差距）。
    let mut serif_list: Vec<String> = Vec::new();
    if let Some(bytes) = serif_bytes {
        fonts.font_data.insert(
            "serif_cjk".to_owned(),
            egui::FontData::from_owned(bytes),
        );
        serif_list.push("serif_cjk".to_owned());
    }
    if fonts.font_data.contains_key("cjk") {
        serif_list.push("cjk".to_owned());
    }
    if !serif_list.is_empty() {
        fonts
            .families
            .insert(egui::FontFamily::Name(SERIF_FAMILY.into()), serif_list);
    }
    ctx.set_fonts(fonts);
}
// ---------------------------------------------------------------- 数据目录

/// 便携数据目录（v0.1.10 绿色版布局）：exe 同目录存在 data/
/// 文件夹时返回它——设置/索引/pins/日志全部住 exe 旁，不再藏
/// %LOCALAPPDATA%。注意与索引的「便携范围」（portable_roots，
/// 无管理员时的索引**来源**口径）是两个概念：那是索引从哪来，
/// 这是数据落到哪，命名刻意分开（portable_data vs portable_roots）。
fn portable_data_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.join("data");
    if dir.is_dir() {
        Some(dir)
    } else {
        None
    }
}

/// 常规数据目录：Windows %LocalAppData%\ShunSeek；其他 ~/.local/share/ShunSeek。
fn appdata_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(base) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(base).join("ShunSeek");
        }
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("ShunSeek");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local").join("share").join("ShunSeek")
}

/// 数据目录唯一入口：便携数据模式（exe 旁有 data/）优先，
/// 否则回落常规目录。全应用只许走这里，不许别处拼路径。
fn data_dir() -> PathBuf {
    portable_data_dir().unwrap_or_else(appdata_dir)
}

/// 便携迁移（main 启动时调一次）：便携模式首启且 data/ 里还
/// 没有任何核心文件、而旧 AppData 里已有数据时，把设置/pins/
/// 索引缓存/用量/日志拷一份过来——老用户换绿色版不丢 pins、
/// 不丢现成索引。只拷不删（旧目录原样留着，回滚无损）。
fn migrate_into_portable_data() {
    let Some(dst) = portable_data_dir() else {
        return;
    };
    let src = appdata_dir();
    if dst == src || !src.is_dir() {
        return;
    }
    let markers = ["settings.txt", "pins.txt", "index.bin"];
    if markers.iter().any(|f| dst.join(f).exists()) {
        return; // data/ 已有核心文件 = 不是首启，不覆盖
    }
    for f in [
        "settings.txt",
        "pins.txt",
        "index.bin",
        "index-next.bin",
        "usage.dat",
        "theme.txt",
        "view.txt",
        "tray-hint-shown",
    ] {
        let from = src.join(f);
        if from.is_file() {
            let _ = std::fs::copy(&from, dst.join(f));
        }
    }
    let slog = src.join("logs");
    if slog.is_dir() {
        let dlog = dst.join("logs");
        let _ = std::fs::create_dir_all(&dlog);
        if let Ok(rd) = std::fs::read_dir(&slog) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_file() {
                    let _ = std::fs::copy(&p, dlog.join(e.file_name()));
                }
            }
        }
    }
}

// ---------------------------------------------------------------- 通用辅助

/// 全 app 锁的统一入口（v0.1.6 冻结根治）：锁中毒（持有者 panic）时
/// 恢复内部值继续用，绝不让一次 panic 把此后所有 lock() 变成 Err
/// 被静默吞掉——那正是 v0.1.5「搜一次之后结果再也不变」的根因。
fn lock_rec<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 把字节偏移向下收敛到最近的字符边界。用 core 给的字节偏移
/// （SearchHit.match_start）切片前必须先过这里：偏移理论上总落在
/// 边界上，但 UI 不依赖这一点（v0.1.5 真机曾因此 panic 毒化锁）。
fn floor_char_boundary(s: &str, mut b: usize) -> usize {
    if b > s.len() {
        b = s.len();
    }
    while b > 0 && !s.is_char_boundary(b) {
        b -= 1;
    }
    b
}

/// 千分位分组（原版状态栏 "{N0} 项" 的格式）：353041 → "353,041"。
fn group_thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------- 索引装配
enum IndexMsg {
    Rebuilt {
        store: IndexStore,
        count: usize,
        summary: String,
        final_stage: bool,
        watch: Vec<(char, Option<i64>)>,
    },
    /// 最终缓存落盘结果（在 UI 换入新索引、释放旧 Mmap 后由后台保存回传）。
    CacheSaved(bool),
    Delta {
        gen: u64,
        delta_count: usize,
        total_count: usize,
        updated_hhmm: String,
    },
    NeedRebuild {
        gen: u64,
    },
}

/// 单个数据源的隔离包装：某个源 panic / 报错只丢它自己的数据，
/// 不许拖垮整个重建线程（否则消息永远发不出来，界面只剩旧空索引）。
fn guarded<T>(f: impl FnOnce() -> T) -> Option<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).ok()
}

/// 非 Windows 数据源汇总（在后台线程跑）：DemoIndexSource 的演示数据。
/// Windows 的真实数据源走 rebuild_windows 的分级建立，不经这里。
#[cfg(not(windows))]
fn collect_entries() -> (Vec<RawEntry>, Option<HashMap<char, String>>, String) {
    #[cfg(windows)]
    {
        use shunseek_core::sources::{appscan, mft, walker};
        let mut entries: Vec<RawEntry> = Vec::new();
        let mut roots: HashMap<char, String> = HashMap::new();
        let mut parts: Vec<String> = Vec::new();
        let mut drive_roots: Vec<String> = Vec::new();
        // ① 各固定盘 MFT
        let mut mft_total = 0usize;
        for drive in 'C'..='Z' {
            let root = format!("{drive}:\\");
            if !std::path::Path::new(&root).exists() {
                continue;
            }
            roots.insert(drive, root.clone());
            drive_roots.push(root);
            let got = guarded(|| mft::load_volume(drive));
            match got {
                Some(Ok(mut v)) => {
                    parts.push(format!("MFT {drive}:{}", v.len()));
                    mft_total += v.len();
                    entries.append(&mut v);
                }
                _ => parts.push(format!("MFT {drive}:失败")),
            }
        }
        // ② 快捷方式 + 已装应用
        let app_entries = guarded(|| appscan::load_all()).unwrap_or_default();
        parts.push(format!("应用 {}", app_entries.len()));
        entries.extend(app_entries);
        // ③ MFT 一条都没拿到 → 遍历用户目录兜底
        let mut walk_count = 0usize;
        if mft_total == 0 {
            if let Ok(profile) = std::env::var("USERPROFILE") {
                let v = guarded(|| walker::walk(&[PathBuf::from(profile)], true))
                    .unwrap_or_default();
                walk_count = v.len();
                entries.extend(v);
            }
        }
        parts.push(format!("遍历 {walk_count}"));
        // ④ 还是 0 条 → 每个固定盘根全盘遍历（慢，但保命）
        if entries.is_empty() && !drive_roots.is_empty() {
            let mut full_count = 0usize;
            for root in &drive_roots {
                let root = PathBuf::from(root);
                let v = guarded(|| walker::walk(&[root], true)).unwrap_or_default();
                full_count += v.len();
                entries.extend(v);
            }
            parts.push(format!("全盘遍历 {full_count}"));
        }
        let roots = if roots.is_empty() { None } else { Some(roots) };
        (entries, roots, parts.join(" · "))
    }
    #[cfg(not(windows))]
    {
        let entries = demo::demo_entries();
        let summary = format!("演示数据 {}", entries.len());
        (entries, None, summary)
    }
}

/// 退出看门狗：正常退出路径（关窗 → eframe 结束事件循环）万一被卡住，
/// 1.5 秒后强制结束进程。这是「托盘关不掉」bug 的最后一道保险——
/// 该 bug 的根源是隐藏状态下事件循环停转、退出动作无人处理；v0.1.5
/// 已改事件唤醒根治，这里再兜一层：只要退出意图已发出，进程必死。
/// 设置与使用记录都在变更时即时落盘，强制结束不丢数据。
fn spawn_exit_watchdog() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(1500));
        std::process::exit(0);
    });
}

fn spawn_rebuild(dir: PathBuf, tx: Sender<IndexMsg>, ctx: egui::Context) {
    std::thread::Builder::new()
        .name("shunseek-index".into())
        .spawn(move || {
            #[cfg(windows)]
            {
                if guarded(|| rebuild_windows(&dir, &tx, &ctx)).is_none() {
                    let store = build_index(Vec::new(), None);
                    let _ = tx.send(IndexMsg::Rebuilt {
                        store,
                        count: 0,
                        summary: "重建线程异常".to_string(),
                        final_stage: true,
                        watch: Vec::new(),
                    });
                    ctx.request_repaint();
                }
            }
            #[cfg(not(windows))]
            {
                let _ = &dir;
                let built = guarded(|| {
                    let (entries, roots, summary) = collect_entries();
                    let store = build_index(entries, roots.as_ref());
                    let count = store.count();
                    (store, count, summary)
                });
                let (store, count, summary) = match built {
                    Some(v) => v,
                    None => (
                        build_index(Vec::new(), None),
                        0,
                        "重建线程异常".to_string(),
                    ),
                };
                let _ = tx.send(IndexMsg::Rebuilt {
                    store,
                    count,
                    summary,
                    final_stage: true,
                    watch: Vec::new(),
                });
                ctx.request_repaint();
            }
        })
        .ok();
}

fn send_index_stage(
    tx: &Sender<IndexMsg>,
    ctx: &egui::Context,
    entries: &[RawEntry],
    roots: &HashMap<char, String>,
    summary: String,
    final_stage: bool,
) {
    send_index_stage_watch(tx, ctx, entries, roots, summary, final_stage, Vec::new());
}

#[allow(clippy::too_many_arguments)]
fn send_index_stage_watch(
    tx: &Sender<IndexMsg>,
    ctx: &egui::Context,
    entries: &[RawEntry],
    roots: &HashMap<char, String>,
    summary: String,
    final_stage: bool,
    watch: Vec<(char, Option<i64>)>,
) {
    let roots = if roots.is_empty() { None } else { Some(roots) };
    let store = build_index(entries.to_vec(), roots);
    let count = store.count();
    let _ = tx.send(IndexMsg::Rebuilt {
        store,
        count,
        summary,
        final_stage,
        watch,
    });
    ctx.request_repaint();
}

/// 浅层扫描一棵目录（depth = 向下层数）：只用 read_dir 不进深处，
/// 保证第一级几百毫秒内出得来。跳过 desktop.ini 这类系统噪音。
#[cfg(windows)]
fn scan_shallow(dir: &std::path::Path, depth: usize, out: &mut Vec<RawEntry>) {
    if depth == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.eq_ignore_ascii_case("desktop.ini") {
            continue;
        }
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let category = if is_dir {
            Category::Folder
        } else {
            shunseek_core::classify_by_extension(&name)
        };
        let mut entry = RawEntry::new(name, e.path().to_string_lossy().into_owned(), category);
        entry.is_dir = is_dir;
        out.push(entry);
        if is_dir {
            scan_shallow(&e.path(), depth - 1, out);
        }
    }
}

/// 第一级的浅层部分：桌面（2 层，桌面文件夹里的东西也算常用）+
/// %USERPROFILE% 顶层（1 层：Documents / Downloads 等目录本体与散文件）。
#[cfg(windows)]
fn shallow_profile_entries() -> Vec<RawEntry> {
    let mut out = Vec::new();
    if let Ok(profile) = std::env::var("USERPROFILE") {
        let base = PathBuf::from(&profile);
        scan_shallow(&base.join("Desktop"), 2, &mut out);
        scan_shallow(&base, 1, &mut out);
    }
    out
}

/// NTFS 文件引用号打卷标签（v0.1.8）：高 16 位 = 盘符序号
///（'A'=1…），低 48 位 = 卷内原始引用号（NTFS 记录号 ≤48 位）。
/// MFT 枚举产出的是卷内裸号，多卷同号（如卷根都是 5）；不打标签
/// 时增量墓碑与目录映射会跨卷误杀。core 的 load_volume 产出裸号，
/// app 在并入时统一改写 file_ref/parent_ref；USN 变更进
/// DeltaIndex 前同样改写（见监视 supervisor）。
#[cfg(windows)]
fn tag_ref(drive: char, raw: u64) -> u64 {
    let tag = (drive as u64).saturating_sub(64) << 48;
    tag | (raw & 0x0000_FFFF_FFFF_FFFF)
}

/// 进程是否已提升（管理员）：OpenProcessToken + TokenElevation。
/// MFT 直读需要管理员（原版口径：管理员+NTFS 才走 MFT 全盘）。
#[cfg(windows)]
fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token: isize = 0;
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elev: TOKEN_ELEVATION = std::mem::zeroed();
        let mut ret_len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elev as *mut TOKEN_ELEVATION as *mut std::ffi::c_void,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        );
        CloseHandle(token);
        ok != 0 && elev.TokenIsElevated != 0
    }
}

/// 固定盘的文件系统名（如 "NTFS"）；非固定盘/查询失败返回 None。
#[cfg(windows)]
fn drive_fs_name(drive: char) -> Option<String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumeInformationW};
    const DRIVE_FIXED: u32 = 3; // windows-sys 0.52 未导出该常量
    let root: Vec<u16> = OsStr::new(&format!("{drive}:\\"))
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        if GetDriveTypeW(root.as_ptr()) != DRIVE_FIXED {
            return None;
        }
        let mut fs = vec![0u16; 64];
        let ok = GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        );
        if ok == 0 {
            return None;
        }
        let end = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
        Some(String::from_utf16_lossy(&fs[..end]))
    }
}

/// 当前作用域串（v0.1.9 缓存信任字段，对齐原版 IndexService
/// 的 scope 口径）：MFT 模式 = "C:,D: · MFT 直读全盘"，便携
/// 模式 = "用户目录 · 便携模式"，未提权时附 "（未提权）"。
/// 与 rebuild_windows 的分档判定同源（管理员 + NTFS 固定盘）；
/// 缓存保存时写入、启动加载时比对，对不上即弃缓存。
#[cfg(windows)]
fn current_scope() -> String {
    let mut ntfs: Vec<char> = Vec::new();
    for drive in 'C'..='Z' {
        if std::path::Path::new(&format!("{drive}:\\")).exists()
            && drive_fs_name(drive)
                .map(|fs| fs.eq_ignore_ascii_case("NTFS"))
                .unwrap_or(false)
        {
            ntfs.push(drive);
        }
    }
    let elevated = is_elevated();
    let mut scope = if elevated && !ntfs.is_empty() {
        let list: Vec<String> = ntfs.iter().map(|d| format!("{d}:")).collect();
        format!("{} · MFT 直读全盘", list.join(","))
    } else {
        "用户目录 · 便携模式".to_string()
    };
    if !elevated {
        scope += "（未提权）";
    }
    scope
}

/// 非 Windows（演示构建）的作用域串：恒定值，缓存自洽即可。
#[cfg(not(windows))]
fn current_scope() -> String {
    "演示数据".to_string()
}

/// 当前 Unix 秒（缓存信任字段的建表时间用）。
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 任意 Unix 秒 → 本地 "HH:MM"（缓存状态文案用）。
fn hhmm_of_unix(secs: u64) -> String {
    #[cfg(windows)]
    {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        win_time::to_local_string(t)
            .map(|full| full.chars().skip(11).take(5).collect())
            .filter(|s: &String| !s.is_empty())
            .unwrap_or_else(|| "--:--".to_string())
    }
    #[cfg(not(windows))]
    {
        format!("{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60)
    }
}

/// 便携模式索引范围（v0.1.8，对齐原版 IndexService 便携口径）：
/// UserProfile + 公共目录 + ProgramData（含开始菜单）+ ProgramFiles。
#[cfg(windows)]
fn portable_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for key in ["USERPROFILE", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                roots.push(PathBuf::from(v));
            }
        }
        // USERPROFILE 之后插入公共目录与 ProgramData（顺序只影响
        // 遍历先后，不影响结果集）
        if key == "USERPROFILE" {
            roots.push(PathBuf::from("C:\\Users\\Public\\Desktop"));
            roots.push(PathBuf::from("C:\\Users\\Public\\Documents"));
            if let Ok(pd) = std::env::var("ProgramData") {
                if !pd.is_empty() {
                    roots.push(PathBuf::from(&pd).join("Microsoft\\Windows\\Start Menu"));
                    roots.push(PathBuf::from(pd));
                }
            }
        }
    }
    roots
}

/// 用户在范围面板里勾选的根（预设 / 磁盘 / 自定义文件夹）。索引建立
/// 时必须一并扫描：便携模式默认只索引 %USERPROFILE% 一带，用户把
/// D:\某文件夹设成范围后，索引里根本没有它，过滤下来就是「0 项」。
#[cfg(windows)]
fn scope_extra_roots() -> Vec<PathBuf> {
    let s = load_settings();
    let base = PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default());
    let mut out: Vec<PathBuf> = Vec::new();
    for (key, sub, _) in PRESET_KEYS {
        if s.scope_presets.iter().any(|k| k == key) {
            out.push(if sub.is_empty() {
                base.clone()
            } else {
                base.join(sub)
            });
        }
    }
    for d in &s.scope_drives {
        out.push(PathBuf::from(format!("{d}:\\")));
    }
    for c in &s.scope_custom {
        out.push(PathBuf::from(c));
    }
    out.retain(|p| p.is_dir());
    out
}

/// 筛掉已经被某个既有根覆盖的路径（前缀 + 目录边界），避免把
/// %USERPROFILE% 整树再走一遍。
#[cfg(windows)]
fn uncovered_roots(extra: Vec<PathBuf>, covered: &[PathBuf]) -> Vec<PathBuf> {
    let norm = |p: &std::path::Path| {
        p.to_string_lossy()
            .to_lowercase()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_string()
    };
    let cov: Vec<String> = covered.iter().map(|p| norm(p)).collect();
    extra
        .into_iter()
        .filter(|p| {
            let s = norm(p);
            s.is_empty()
                || !cov.iter().any(|c| {
                    s == *c
                        || (s.starts_with(c.as_str()) && s.as_bytes().get(c.len()) == Some(&b'\\'))
                })
        })
        .collect()
}

/// 便携模式排除前缀（v0.1.8，对齐原版 IndexService 的 13 项，
/// 以 LOCALAPPDATA / APPDATA 为基拼出；基为空的项自动丢弃）。
#[cfg(windows)]
fn portable_excludes() -> Vec<String> {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let roaming = std::env::var("APPDATA").unwrap_or_default();
    let j = |base: &str, sub: &str| format!("{base}\\{sub}");
    [
        j(&roaming, "Tencent\\xwechat\\radium"),
        j(&roaming, "Tencent\\WeChat\\All Users"),
        j(&local, "Temp"),
        j(&local, "Packages"),
        j(&local, "Microsoft\\Windows"),
        j(&local, "CrashDumps"),
        j(&roaming, "Microsoft\\Windows\\Recent"),
        j(&local, "ShunSeek"),
        j(&local, "Google\\Chrome\\User Data\\Default\\Cache"),
        j(&local, "Microsoft\\Edge\\User Data\\Default\\Cache"),
        j(&roaming, "Code\\Cache"),
        j(&roaming, "Code\\CachedData"),
        j(&roaming, "Code\\logs"),
    ]
    .into_iter()
    .filter(|s| !s.starts_with('\\'))
    .collect()
}

/// Windows 分级建立（v0.1.5）：打开就能搜，不等全量建完。
/// 第一级：appscan（快捷方式 + 已装应用）+ 桌面/用户目录浅层，先建成
/// 索引换入，搜索立刻可用；第二级：逐盘 MFT，每完成一盘把「第一级 +
/// 已完成盘」的合并索引再换入一次，状态栏摘要实时报进度。搜索永远
/// 针对当前已换入的索引，输入框全程不禁用、不出等待态。
/// 兜底语义与旧版一致：MFT 总数为 0 → 全遍历 %USERPROFILE%；条目仍
/// 为 0 → 每个固定盘根全盘遍历。最终一级存缓存 index.bin。
#[cfg(windows)]
fn rebuild_windows(_dir: &PathBuf, tx: &Sender<IndexMsg>, ctx: &egui::Context) {
    use shunseek_core::sources::{appscan, mft, usn, walker};
    let mut entries: Vec<RawEntry> = Vec::new();
    let mut roots: HashMap<char, String> = HashMap::new();
    let mut drives: Vec<char> = Vec::new();
    for drive in 'C'..='Z' {
        let root = format!("{drive}:\\");
        if std::path::Path::new(&root).exists() {
            roots.insert(drive, root);
            drives.push(drive);
        }
    }

    // ---- 第一级：应用 + 浅层 ----
    let app_entries = guarded(|| appscan::load_all()).unwrap_or_default();
    let apps = app_entries.len();
    entries.extend(app_entries);
    let shallow = guarded(shallow_profile_entries).unwrap_or_default();
    let shallow_n = shallow.len();
    entries.extend(shallow);
    send_index_stage(
        tx,
        ctx,
        &entries,
        &roots,
        format!("索引：应用 {apps} · 浅层 {shallow_n} · 全量建立中…"),
        false,
    );

    // ---- 分档（v0.1.8，对齐原版 IndexService 模式口径）----
    // 管理员 + 有 NTFS 固定盘 → MFT 模式（NTFS 盘直读、其余固定盘
    // 遍历并入）；否则便携模式（便携范围 + 噪音目录跳过 + 13 项
    // 排除前缀）。MFT 模式同时产出 USN 监视计划供增量层用。
    let ntfs_drives: Vec<char> = drives
        .iter()
        .copied()
        .filter(|d| {
            drive_fs_name(*d)
                .map(|fs| fs.eq_ignore_ascii_case("NTFS"))
                .unwrap_or(false)
        })
        .collect();
    let mft_mode = is_elevated() && !ntfs_drives.is_empty();

    let mut parts: Vec<String> = vec![format!(
        "模式 {}",
        if mft_mode { "MFT" } else { "便携" }
    )];
    let mut mft_total = 0usize;
    let mut walk_count = 0usize;
    let mut watch_plan: Vec<(char, Option<i64>)> = Vec::new();
    if mft_mode {
        // ---- 第二级：逐盘 MFT，逐盘并入 ----
        for (di, drive) in ntfs_drives.iter().enumerate() {
            // 先记 USN 起点再枚举：枚举期间发生的增删改由增量层补
            let start_usn = usn::query_next_usn(*drive);
            match guarded(|| mft::load_volume(*drive)) {
                Some(Ok(mut v)) => {
                    mft_total += v.len();
                    parts.push(format!("MFT {drive}:{}", v.len()));
                    // refs 打卷标签（多卷同号防增量误杀，见 tag_ref）
                    for e in &mut v {
                        if e.file_ref != 0 {
                            e.file_ref = tag_ref(*drive, e.file_ref);
                        }
                        if e.parent_ref != 0 {
                            e.parent_ref = tag_ref(*drive, e.parent_ref);
                        }
                    }
                    entries.append(&mut v);
                    watch_plan.push((*drive, start_usn));
                }
                _ => parts.push(format!("MFT {drive}:失败")),
            }
            if di + 1 < ntfs_drives.len() {
                let done: Vec<String> = ntfs_drives[..=di]
                    .iter()
                    .map(|d| format!("MFT {d}:完成"))
                    .collect();
                send_index_stage(
                    tx,
                    ctx,
                    &entries,
                    &roots,
                    format!(
                        "索引：应用 {apps} · 浅层 {shallow_n} · {} · 其余盘建立中…",
                        done.join(" · ")
                    ),
                    false,
                );
            }
        }
        // 非 NTFS 固定盘：遍历并入（噪音目录整树跳过）
        for drive in drives.iter().filter(|d| !ntfs_drives.contains(d)) {
            let root = PathBuf::from(format!("{drive}:\\"));
            let v = guarded(|| walker::walk_with_excludes(&[root], true, &[]))
                .unwrap_or_default();
            walk_count += v.len();
            parts.push(format!("遍历 {drive}:{}", v.len()));
            entries.extend(v);
        }
        if mft_total == 0 {
            // MFT 全灭：退便携范围，别让索引只剩应用 + 浅层
            watch_plan.clear();
            let mut walk_roots = portable_roots();
            walk_roots.extend(uncovered_roots(scope_extra_roots(), &walk_roots));
            let v = guarded(|| {
                walker::walk_with_excludes(&walk_roots, true, &portable_excludes())
            })
            .unwrap_or_default();
            walk_count += v.len();
            entries.extend(v);
        }
    } else {
        // ---- 便携模式：便携范围遍历（噪音跳过 + 排除前缀）----
        // 范围面板勾选的根一并走：否则自定义文件夹永远搜不到。
        let mut walk_roots = portable_roots();
        walk_roots.extend(uncovered_roots(scope_extra_roots(), &walk_roots));
        let v = guarded(|| {
            walker::walk_with_excludes(&walk_roots, true, &portable_excludes())
        })
        .unwrap_or_default();
        walk_count = v.len();
        entries.extend(v);
    }
    parts.push(format!("应用 {apps}"));
    parts.push(format!("浅层 {shallow_n}"));
    if walk_count > 0 {
        parts.push(format!("遍历 {walk_count}"));
    }
    if entries.is_empty() && !drives.is_empty() {
        // 最后兜底（旧语义）：什么都没拿到 → 全盘遍历保命
        let mut full_count = 0usize;
        for drive in &drives {
            let root = PathBuf::from(format!("{drive}:\\"));
            let v = guarded(|| walker::walk(&[root], true)).unwrap_or_default();
            full_count += v.len();
            entries.extend(v);
        }
        parts.push(format!("全盘遍历 {full_count}"));
    }

    // ---- 最终一级：换入后由 UI 释放旧 Mmap 再异步写缓存 ----
    if mft_mode && mft_total > 0 {
        send_index_stage_watch(tx, ctx, &entries, &roots, parts.join(" · "), true, watch_plan);
    } else {
        send_index_stage(tx, ctx, &entries, &roots, parts.join(" · "), true);
    }
}

#[cfg(windows)]
fn now_hhmm() -> String {
    win_time::to_local_string(std::time::SystemTime::now())
        .map(|full| full.chars().skip(11).take(5).collect())
        .filter(|s: &String| !s.is_empty())
        .unwrap_or_else(|| "--:--".to_string())
}

/// USN 监视 supervisor（v0.1.8，仅 MFT 模式）：每卷一个
/// UsnWatcher 线程把变更喂进一条 mpsc 通道；flush 线程独占
/// DeltaIndex，每 300ms 把攒下的变更批量应用、freeze 成
/// (增量索引, 墓碑）快照换进 delta_state，并发 IndexMsg::Delta
/// 让 UI 刷页脚 + 重搜；墓碑超限/目录改名等触发 NeedRebuild。
/// 代际退休：watch_gen 与自己代号不符时停掉全部 watcher 并退出
///（每次最终重建都会 +1 代，旧 supervisor 自动收摊）。
/// 路径解析靠主索引的文件夹 ref→路径表（dir_paths，refs 已打
/// 卷标签）；父目录不在表里（索引外新建的目录树）→ 请求全量
/// 重建兜底，绝不硬拼错路径。便携模式不进这里（无监视、仅手动
/// 重建），不会崩。
#[cfg(windows)]
fn spawn_watch_supervisor(
    gen: u64,
    plan: Vec<(char, Option<i64>)>,
    store: Arc<Mutex<Arc<IndexStore>>>,
    delta_state: Arc<Mutex<(IndexStore, Tombstones)>>,
    watch_gen: Arc<std::sync::atomic::AtomicU64>,
    flush_now: Arc<AtomicBool>,
    tx: Sender<IndexMsg>,
    ctx: egui::Context,
    base_count: usize,
) {
    use shunseek_core::sources::usn::{UsnChange, UsnChangeKind, UsnWatcher};
    std::thread::Builder::new()
        .name("shunseek-usn-flush".into())
        .spawn(move || {
            let mut dir_paths: HashMap<u64, String> = HashMap::new();
            {
                let st = lock_rec(&store).clone();
                for i in 0..st.count() {
                    let r = st.file_ref(i);
                    if r != 0 && st.category(i) == Category::Folder {
                        dir_paths.insert(r, st.full_path(i));
                    }
                }
            }
            for (drive, _) in &plan {
                dir_paths.insert(tag_ref(*drive, 5), format!("{drive}:\\"));
            }
            let (ch_tx, ch_rx) = std::sync::mpsc::channel::<(char, UsnChange)>();
            let mut watchers: Vec<Arc<UsnWatcher>> = Vec::new();
            for (drive, start) in &plan {
                let w = Arc::new(UsnWatcher::with_start(*drive, *start));
                watchers.push(w.clone());
                let ch_tx = ch_tx.clone();
                let drive = *drive;
                std::thread::Builder::new()
                    .name(format!("shunseek-usn-{drive}"))
                    .spawn(move || {
                        let _ = w.run(|ch| {
                            let _ = ch_tx.send((drive, ch));
                        });
                    })
                    .ok();
            }
            drop(ch_tx);
            let mut delta = DeltaIndex::new();
            let mut last_changes = 0usize;
            let mut rebuild_sent = false;
            let join = |parent: &str, name: &str| -> String {
                if parent.ends_with('\\') {
                    format!("{parent}{name}")
                } else {
                    format!("{parent}\\{name}")
                }
            };
            loop {
                std::thread::sleep(Duration::from_millis(300));
                if watch_gen.load(Ordering::SeqCst) != gen {
                    for w in &watchers {
                        w.stop();
                    }
                    break;
                }
                let force_report = flush_now.swap(false, Ordering::SeqCst);
                let mut touched = false;
                while let Ok((drive, ch)) = ch_rx.try_recv() {
                    touched = true;
                    let tref = tag_ref(drive, ch.file_ref);
                    let tparent = tag_ref(drive, ch.parent_ref);
                    match ch.kind {
                        UsnChangeKind::Created | UsnChangeKind::RenamedNewName => {
                            if ch.is_dir
                                && matches!(ch.kind, UsnChangeKind::RenamedNewName)
                            {
                                delta.request_rebuild();
                            }
                            match dir_paths.get(&tparent) {
                                Some(parent) => {
                                    let full = join(parent, &ch.name);
                                    if ch.is_dir {
                                        dir_paths.insert(tref, full.clone());
                                    }
                                    delta.upsert(RawEntry {
                                        name: ch.name.clone(),
                                        full_path: Some(full),
                                        category: if ch.is_dir {
                                            Category::Folder
                                        } else {
                                            shunseek_core::classify_by_extension(&ch.name)
                                        },
                                        file_ref: tref,
                                        parent_ref: tparent,
                                        is_dir: ch.is_dir,
                                    });
                                }
                                None => delta.request_rebuild(),
                            }
                        }
                        UsnChangeKind::Deleted | UsnChangeKind::RenamedOldName => {
                            if ch.is_dir {
                                if matches!(ch.kind, UsnChangeKind::RenamedOldName) {
                                    delta.request_rebuild();
                                }
                                match dir_paths.remove(&tref) {
                                    Some(p) => {
                                        delta.remove_directory(&p, true);
                                        delta.remove(tref, &p);
                                    }
                                    None => delta.remove(tref, ""),
                                }
                            } else {
                                let path = dir_paths
                                    .get(&tparent)
                                    .map(|p| join(p, &ch.name))
                                    .unwrap_or_default();
                                delta.remove(tref, &path);
                            }
                        }
                        UsnChangeKind::Other => {}
                    }
                }
                let changed = delta.total_changes() != last_changes;
                if !touched && !changed && !force_report {
                    continue;
                }
                if watch_gen.load(Ordering::SeqCst) != gen {
                    for w in &watchers {
                        w.stop();
                    }
                    break;
                }
                last_changes = delta.total_changes();
                let (frozen, tombs) = delta.freeze();
                let dcount = frozen.count();
                let dead = tombs.dead_count();
                *lock_rec(&delta_state) = (frozen, tombs);
                let _ = tx.send(IndexMsg::Delta {
                    gen,
                    delta_count: dcount,
                    total_count: base_count.saturating_sub(dead) + dcount,
                    updated_hhmm: now_hhmm(),
                });
                if delta.requires_rebuild() && !rebuild_sent {
                    rebuild_sent = true;
                    let _ = tx.send(IndexMsg::NeedRebuild { gen });
                }
                ctx.request_repaint();
            }
        })
        .ok();
}

// ---------------------------------------------------------------- 搜索

#[derive(Clone)]
struct ResultRow {
    name: String,
    path: String,
    category: Category,
    /// 命中段在 name 中的字节偏移；usize::MAX = 不高亮（空查询 / 拼音首字母命中）
    match_start: usize,
    match_len: usize,
    /// 是否被 core 降权（哈希名 / 垃圾路径）：v0.1.8 起只作折叠
    /// 判定的输入之一（与重要度 Low 取或），不再单独成段。
    demoted: bool,
    /// 具体类型名（v0.1.8，core filetype::describe）：如「Word 文档」
    /// 「快捷方式」；行右侧簇 / 宫格副行 / 悬停卡的类型都用它。
    type_label: String,
    /// 重要度三档（v0.1.8，core filetype）：折叠分组与重要度点取色用。
    importance: Importance,
}

/// 展示行：由原始结果 rows 派生，绘制与键盘导航都以它为准。
/// v0.1.8 折叠口径换成原版 FoldByType（见 rebuild_display）：
/// High 永不折；前 5 条非 High 保底平铺；Normal 同类型总数 ≥4
/// 折成「N 个{类型}」组（组头恒在，展开成员跟随其后、再点收起）；
/// Low（或 demoted 信号）汇成尾部一个「其他 N 项」组。空查询
/// 不折叠，display 与 rows 一一对应。
#[derive(Clone)]
enum DisplayRow {
    Single { row: usize, indent: bool },
    Group {
        /// 展开状态键："type:{类型名}" 或 "low"。
        key: String,
        /// 组头整行文字（如「12 个日志文件」/「其他 8 项（…）」）。
        label: String,
        count: usize,
        expanded: bool,
    },
}

/// 父目录的最后一段（范围摘要等处用）。
fn dir_last(dir: &str) -> String {
    dir.split(['\\', '/'])
        .filter(|s| !s.is_empty())
        .last()
        .unwrap_or(dir)
        .to_string()
}

/// 悬停详情卡的目标：绘制行/格时若被悬停就记下，帧末统一处理
///（计时 0.4 秒 + 取 metadata + 绘卡，见 update_hover_card）。
#[derive(Clone)]
struct HoverTarget {
    name: String,
    path: String,
    category: Category,
}

/// 详情卡用的文件元信息：索引里没有大小/时间（紧凑条目装不下），
/// 悬停时按需 fs::metadata 取、按路径缓存（含失败结果，不反复打磁盘）。
#[derive(Clone)]
struct FileMeta {
    is_dir: bool,
    size: u64,
    modified: Option<std::time::SystemTime>,
}

/// 文件大小格式化：B / KB / MB / GB，1 位小数（1024 进制）。
fn format_size(bytes: u64) -> String {
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if b < 1024.0 * 1024.0 {
        format!("{:.1} KB", b / 1024.0)
    } else if b < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", b / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", b / (1024.0 * 1024.0 * 1024.0))
    }
}

/// 修改时间 →「YYYY-MM-DD HH:MM」。不引时间库：Windows 走系统 API
/// 转本地时区；非 Windows 用 Hinnant 的 days→civil 算法按 UTC 算
///（仅演示数据，无时区诉求）。
fn format_modified(t: std::time::SystemTime) -> Option<String> {
    #[cfg(windows)]
    {
        win_time::to_local_string(t)
    }
    #[cfg(not(windows))]
    {
        let secs = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
        let days = secs.div_euclid(86_400);
        let tod = secs.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        Some(format!(
            "{y:04}-{m:02}-{d:02} {:02}:{:02}",
            tod / 3600,
            (tod % 3600) / 60
        ))
    }
}

/// Hinnant days→(年, 月, 日)（公历，含闰年规则），z 为距 1970-01-01 的天数。
#[cfg(not(windows))]
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(windows)]
mod win_time {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

    /// SystemTime(unix) → 本地时间字符串：先转 FILETIME，再经系统 API
    /// 做 UTC→本地时区转换，夏令时规则交给系统，不自己算偏移。
    pub fn to_local_string(t: std::time::SystemTime) -> Option<String> {
        let secs = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        let ticks = (secs + 11_644_473_600) * 10_000_000;
        let ft = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        unsafe {
            let mut utc = std::mem::zeroed::<SYSTEMTIME>();
            if FileTimeToSystemTime(&ft, &mut utc) == 0 {
                return None;
            }
            let mut local = std::mem::zeroed::<SYSTEMTIME>();
            if SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0 {
                return None;
            }
            Some(format!(
                "{:04}-{:02}-{:02} {:02}:{:02}",
                local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute
            ))
        }
    }
}

enum SearchMsg {
    Done {
        rows: Vec<ResultRow>,
        elapsed_ms: f64,
        empty_query: bool,
        /// 搜索线程 panic 兜底标记（v0.1.6）：true 时 rows 为空且无意义，
        /// UI 只显示「搜索线程异常，已自动恢复」，保留现有结果不冻结。
        error: bool,
    },
}

// ---------------------------------------------------------------- App

struct ShunSeekApp {
    query: String,
    /// 类型筛选选中集（全集 = 全部 12 类，永不为空集，见工具栏
    /// toggle 规则）；搜索时折成 core 的 u16 掩码。
    cat_set: HashSet<Category>,
    /// 哪个筛选控件展开中（两控件互斥）。
    expanded_filter: Option<FilterKind>,
    /// 范围筛选选中集：预设键名 / 盘符 / 自定义文件夹（完整路径）。
    scope_presets: HashSet<String>,
    scope_drives: HashSet<char>,
    /// 已添加的自定义文件夹全集（面板逐行列出，含未选中的）。
    scope_custom: Vec<String>,
    /// 其中参与搜索的那几个（可多选；面板上点行名即切换）。
    scope_custom_sel: HashSet<String>,
    /// 自定义行「第一次单击」的快照（路径, 点击前是否选中, 预设, 磁盘,
    /// 选中集）。双击的第二下到达时用它回滚第一下造成的范围变动——
    /// egui 里双击必然先产生一次 clicked，不回滚就等于「双击顺手改了
    /// 选中状态」。
    scope_click_memo: Option<(String, bool, HashSet<String>, HashSet<char>, HashSet<String>)>,
    /// 当前主题（深/浅）；全部绘制取色走 theme.palette()。
    theme: Theme,
    /// 结果展示模式（列表/宫格），view.txt 持久化。
    view_mode: ViewMode,
    /// 宫格上一帧的列数（键盘 ↑↓ 按它跳行，绘制时更新）。
    grid_cols: usize,
    rows: Vec<ResultRow>,
    /// 由 rows 派生的展示行（折叠后）；selected / 绘制都以它为准。
    display: Vec<DisplayRow>,
    /// 已展开的折叠组，按组键记（"type:{类型}" / "low"，跨搜索保留）。
    expanded_groups: HashSet<String>,
    /// 本批结果是否启用折叠（空查询不折叠）。
    fold_active: bool,
    /// 结果行真实系统图标缓存（Windows；非 Windows 恒空回退色块）。
    icons: icons::IconCache,
    /// 显示名解析（v0.1.9 ShellNames 对齐）：文件夹本地化名 /
    /// .lnk 显示名 / .exe 描述，渲染期后台解析、按路径缓存。
    names: names::NameService,
    selected: usize,
    status: String,
    /// 状态栏警戒标志（空索引护栏）：true 时状态文字与圆点标红。
    status_alert: bool,
    /// 搜索框上一帧是否聚焦（放大镜着色读它，一帧延迟无感）。
    search_focused: bool,
    egui_ctx: egui::Context,
    store: Arc<Mutex<Arc<IndexStore>>>,
    usage: Arc<Mutex<UsageStore>>,
    /// 增量索引冻结快照（v0.1.8）：(增量小索引, 墓碑集)，flush
    /// 线程每 300ms 换新，搜索线程与主索引一起走 search_merged。
    delta_state: Arc<Mutex<(IndexStore, Tombstones)>>,
    /// USN 监视代号：每次最终重建 +1，旧一代监视/flush 线程自退。
    watch_gen: Arc<std::sync::atomic::AtomicU64>,
    /// 托盘「立即刷新索引」：置位后 flush 线程立刻合并一次。
    flush_now: Arc<AtomicBool>,
    /// USN 监视是否在跑（共享给托盘后台回调）。
    watch_active: Arc<AtomicBool>,
    /// 是否有重建正在跑（共享给托盘菜单置灰与后台回调）。
    rebuilding: Arc<AtomicBool>,
    /// 展开折叠组时的一次性结果上限提升（→300，见 toggle_group）：
    /// Some 时 schedule_search 用它代替 result_limit；查询文本一变
    /// 即失效（override_query 记武装时的查询）。
    limit_override: Option<usize>,
    override_query: String,
    search_cancel: Arc<AtomicBool>,
    search_tx: Sender<SearchMsg>,
    search_rx: Receiver<SearchMsg>,
    index_rx: Receiver<IndexMsg>,
    hotkey: hotkey::Hotkey,
    tray: tray::Tray,
    /// 窗口可见性的**影子缓存**（v0.1.12）：每帧由
    /// sync_shadow_from_os 从 OS（IsWindowVisible/IsIconic）
    /// 同步，只供绘制与点外武装判定读；任何可见性**决策**
    /// 不许读它（决策在发起线程现读 winvis::state()）。
    visible: bool,
    /// 启动首几帧的可见性自愈计数（v0.1.15）：winit 建窗到首帧
    /// 渲染之间有一段窗口仍是 SW_HIDE 的竞态，帧首 OS 同步会
    /// 据此把影子判成「已隐藏」，此后不再自绘——真机表现就是
    /// 「进程活着、任务管理器有它、界面永远不出来」。前三帧各
    /// 兜一次 ShowWindow；--tray 静默启动时为 0，不参与。
    boot_show_frames: u8,
    /// 内部预览开关（--ui-probe=type|scope|settings）：首帧强制
    /// 展开某块面板/打开设置，供无鼠标的界面自检使用。
    probe: Option<String>,
    allow_exit: bool,
    focus_search: bool,
    scroll_to_selected: bool,
    /// 数据目录（托盘「重新建立索引」重新 spawn 重建线程要用）。
    data_dir: PathBuf,
    /// 索引消息发送端（同上，App 持一份 clone）。
    index_tx: Sender<IndexMsg>,
    /// 本帧被悬停的行/格（绘制时写，帧末 update_hover_card 消费）。
    hover_seen: Option<HoverTarget>,
    /// 正在计时的悬停目标与起始时刻（egui time，秒）。
    hover_target: Option<HoverTarget>,
    hover_since: f64,
    /// 详情卡 metadata 缓存：路径 → 元信息（None = 取过但失败）。
    meta_cache: HashMap<String, Option<FileMeta>>,
    /// 设置窗口（v0.1.5，独立 viewport）：开关 / 自启动勾选
    ///（打开时从注册表回读，以实际状态为准）。
    settings_open: bool,
    settings_autostart: bool,
    /// 设置项（与 settings.txt 一一对应，见 Settings）：
    /// 唤醒热键预设 / 结果上限 / 是否折叠不重要结果 / 点外隐藏。
    hotkey_combo: hotkey::HotkeyCombo,
    result_limit: usize,
    fold_demoted: bool,
    hide_outside: bool,
    /// 最近一次换入索引的条数与来源摘要完整版（设置「索引」分区
    /// 只读显示用；状态栏展示的是截断版）。
    index_count: usize,
    index_summary: String,
    /// 「清除使用记录」二次确认状态（点一次变确认、再点才执行）。
    usage_confirm: bool,
    /// 设置窗口内的操作反馈（清缓存 / 清记录后显示在窗口底部）。
    settings_note: String,
    /// 热键捕获态（设置里点「修改」后等用户按键）/ 检测提示 /
    /// 冲突时过滤出的推荐组合。
    hotkey_capture: bool,
    hotkey_note: String,
    hotkey_recs: Vec<hotkey::HotkeyCombo>,
    /// 首页 pins（v0.1.6）：用户自选的完整路径列表，持久化到数据
    /// 目录 pins.txt（一行一条）。home_items 是 pins 派生的展示行
    /// （分类尽量取自索引），空查询时由 draw_home 强制宫格绘制。
    pins: Vec<String>,
    home_items: Vec<ResultRow>,
    /// 首页宫格上一帧列数（键盘导航按它跳行，绘制时更新）。
    home_cols: usize,
    /// 页脚中央结果计数（原版 ResultText："{n} 项 · {ms} ms"）。
    result_text: String,
    /// 窗口是否最大化（自绘最大化按钮的图标切换 + 命令取反用）。
    maximized: bool,
    /// 筛选控件上一帧的圆点 Rect 与展开面板 Rect（点外关闭判定）。
    filter_dot_rect: Option<egui::Rect>,
    filter_panel_rect: Option<egui::Rect>,
    /// 首页「＋ 添加」弹层是否打开（弹层锚在添加格上）。
    home_add_open: bool,
    /// 窗口最小化的**影子缓存**（v0.1.12）：每帧由
    /// sync_shadow_from_os 从 OS（IsIconic）同步，自绘最小化
    /// 按钮按下时先行置位只是预热。最小化 ≠ 隐藏；托盘/热键
    /// 的还原决策在发起线程现读 winvis::state()，不读此缓存。
    minimized: bool,
    /// 全局点外隐藏（clickwatch 模块，WH_MOUSE_LL 钩子）：点在
    /// 主窗/设置窗/托盘图标矩形之外 → 钩子线程当场
    /// ShowWindowAsync(SW_HIDE)（v0.1.12；此前是置标志等 UI
    /// 下一帧 hide()）。v0.1.10 起隐藏只认几何判定，焦点事件
    /// 完全退出这条链——v0.1.6–v0.1.9 的失焦隐藏及其补丁群
    /// （对话框抑制、恢复宽限期、指针守卫）整套拆除：那套机制
    /// 在任务栏/托盘等系统外壳区域焦点语义混乱，真机表现为
    /// 「点别处就消失且唤不回」。
    clickwatch: clickwatch::ClickWatch,
    /// 手判拖动/拉伸的控件矩形集：每帧绘制时把所有交互控件的
    /// rect 推进 _cur，帧末轮换为 _prev。这套矩形现在的唯一用途
    /// 是「双击空白区最大化」的排除判定；拖动不再受它限制
    /// （v0.1.14：任何位置按下都能拖，靠 8px 位移阈值区分
    /// 点击与拖动）。见 handle_window_press。
    /// 滚动条带（上一帧 / 本帧）：条上按下拖动是滚内容，
    /// 不参与窗口拖动（与 blockers 分开存，语义不同）。
    scroll_prev: Vec<egui::Rect>,
    scroll_cur: Vec<egui::Rect>,
    blockers_prev: Vec<egui::Rect>,
    blockers_cur: Vec<egui::Rect>,
    /// 按下后待命的拖动起点：任意非拉伸带位置按下都记下，
    /// 指针位移越过 8px 才真正 StartDrag——既留出双击判定窗口，
    /// 也让静止按下/微小抖动什么都不发生（仍归控件）。
    pending_drag: Option<egui::Pos2>,
    /// 筛选面板展开动画的原始进度 [类型, 范围]（0..1 线性时间，
    /// 渲染时过 cubic_out 缓动）。展开/收起 0.24s；目标由
    /// expanded_filter 决定。见 advance_filter_anim。
    filter_anim_t: [f32; 2],
    /// 两块筛选面板上一帧的完整尺寸（动画裁剪要先知道全高/
    /// 全宽，一帧延迟的标准做法；初值是估值，第一帧后自校正）。
    filter_panel_full: [Vec2; 2],
    /// 右键菜单 / 首页添加菜单的打开时刻（egui 时间，进场动画
    /// 180ms 高度+淡入；关闭保持即时——菜单语义，见绘制处）。
    ctx_menu_born: f64,
    home_add_born: f64,
    /// 结果集进场动画起点（egui 时间）：新一批结果到达时整组
    /// 淡入（0.55→1）+ 上浮 ≤8px，220ms。见 poll_channels 置位。
    results_born: Option<f64>,
    /// 当前帧实际使用的调色板：主题切换时从旧板向新板 170ms
    /// 交叉淡入（内容层插值，窗口本体不受影响）。绘制代码读
    /// 它而不是 theme.palette()；静止时恒等于目标主题板。
    display_pal: Palette,
    /// 主题交叉淡入进行中：(旧板, 起点时刻)。
    theme_anim: Option<(Palette, f64)>,
    /// 呼出时给搜索框设全选（show() 置位，draw_search_box 消费）。
    select_all_search: bool,
    /// 搜索框 TextEdit 的 Id（绘制时记下，Ctrl+C 判定文本选区用）。
    search_edit_id: Option<egui::Id>,
    /// 自绘右键菜单状态（v0.1.9 菜单语言统一）：右键结果行 /
    /// 宫格格子 / 首页磁贴时置位，update 在主面板之后统一
    /// 绘制并执行动作。
    ctx_menu: Option<CtxMenu>,
}

/// 自绘右键菜单的目标（下标以打开时为准；搜索一变菜单即关，
/// 不会指到新结果上）。
#[derive(Clone, Copy)]
enum CtxTarget {
    /// 搜索结果展示行（列表行 / 宫格格子共用展示下标）。
    Result(usize),
    /// 首页磁贴。
    Home(usize),
}

/// 自绘右键菜单状态：右键点（绘制时再钳制进窗口）+ 目标。
#[derive(Clone, Copy)]
struct CtxMenu {
    pos: egui::Pos2,
    target: CtxTarget,
}

impl ShunSeekApp {
    fn new(cc: &eframe::CreationContext<'_>) -> ShunSeekApp {
        // 主窗口 HWND 存入 winvis（v0.1.12，可见性原生层的地基）：
        // 必须赶在托盘/热键/唤醒等线程启动之前——它们从此直接
        // 拿这个句柄调 ShowWindow，不再等 UI 线程消费队列。
        // CreationContext 的 HasWindowHandle 在原生 runner 下由
        // winit 窗口填充；万一这里取不到（Err/非 Win32 句柄），
        // update() 首帧会再从 Frame 重取一次（见 update 开头）。
        #[cfg(windows)]
        {
            use raw_window_handle::HasWindowHandle;
            if let Ok(handle) = cc.window_handle() {
                if let raw_window_handle::RawWindowHandle::Win32(w) =
                    handle.as_raw()
                {
                    winvis::store_hwnd(w.hwnd.get());
                }
            }
        }
        winvis::store_ctx(cc.egui_ctx.clone());
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);

        let usage = UsageStore::load(Some(dir.join("usage.dat")));

        // 缓存索引秒开：有 index.bin 先 mmap 顶上，后台重建完再换新的。
        // v0.1.9 缓存信任字段（对齐原版 IndexCache/IndexService）：
        // 先读 meta（scope + 建表时间）——scope 与当前作用域对不上
        // （如提权状态/盘符变了）→ 缓存不用、直接冷建；超过 24h →
        // 先用着，状态标「（后台更新中）」（启动本来就会后台重建，
        // 语义与原版「先用后重扫」一致，只是没有 15s 延迟）。
        let index_path = dir.join("index.bin");
        let meta = IndexStore::read_cache_meta(&index_path).ok();
        let scope_ok = meta
            .as_ref()
            .map(|(s, _)| s == &current_scope())
            .unwrap_or(false);
        let (store, status) = if scope_ok {
            match IndexStore::load_mmap(&index_path) {
                Ok(s) => {
                    let n = s.count();
                    let built = meta.map(|(_, b)| b).unwrap_or(0);
                    let stale = now_unix().saturating_sub(built) > 24 * 3600;
                    let status = if stale {
                        format!(
                            "缓存索引 · {} · {} 条（后台更新中）",
                            hhmm_of_unix(built),
                            group_thousands(n)
                        )
                    } else {
                        format!(
                            "缓存索引 · {} · {} 条",
                            hhmm_of_unix(built),
                            group_thousands(n)
                        )
                    };
                    (s, status)
                }
                Err(_) => (build_index(Vec::new(), None), "正在准备索引…".to_string()),
            }
        } else {
            // 无 meta / scope 对不上：不用任何旧缓存（含历史的
            // index-next.bin——它没有信任字段，拿来顶上会架空
            // scope 判定），直接冷建，后台重建马上跟上。
            (build_index(Vec::new(), None), "正在准备索引…".to_string())
        };

        let (search_tx, search_rx) = std::sync::mpsc::channel();
        let (index_tx, index_rx) = std::sync::mpsc::channel();
        let index_tx_app = index_tx.clone();
        let rebuilding = Arc::new(AtomicBool::new(true));
        let watch_active = Arc::new(AtomicBool::new(false));
        let flush_now = Arc::new(AtomicBool::new(false));
        spawn_rebuild(dir.clone(), index_tx.clone(), cc.egui_ctx.clone());

        // 单实例唤醒等待线程（v0.1.8，Windows）：main 建好的命名
        // 事件句柄在这里等——第二进程 SetEvent 后，本线程**直接**
        // winvis::show() 把已有窗口呼出来（v0.1.12：不再只置
        // WAKE_FLAG 等 UI 线程消费——隐藏态下那条唤醒链正是六版
        // 真机全灭的病根）。WAKE_FLAG + repaint 保留，只负责让
        // UI 随后补一帧呼出副作用（重搜/全选/聚焦请求）。
        #[cfg(windows)]
        {
            let h = WAKE_EVENT.load(Ordering::SeqCst);
            if h != 0 {
                let ctx = cc.egui_ctx.clone();
                std::thread::Builder::new()
                    .name("shunseek-wake".into())
                    .spawn(move || {
                        use windows_sys::Win32::System::Threading::{
                            ResetEvent, WaitForSingleObject,
                        };
                        loop {
                            let r = unsafe { WaitForSingleObject(h, u32::MAX) };
                            if r != 0 {
                                break; // WAIT_FAILED 等异常：线程收工
                            }
                            unsafe {
                                ResetEvent(h);
                            }
                            // 当场呼出（本线程直接调 ShowWindow）
                            winvis::show();
                            WAKE_FLAG.store(true, Ordering::SeqCst);
                            ctx.request_repaint();
                        }
                    })
                    .ok();
            }
        }

        let settings = load_settings();
        let theme = settings.theme;
        let view_mode = settings.view;

        let bg_dir = dir.clone();
        let bg_tx = index_tx_app.clone();
        let bg_ctx = cc.egui_ctx.clone();
        let bg_rebuilding = rebuilding.clone();
        let bg_watch_active = watch_active.clone();
        let bg_flush_now = flush_now.clone();
        let on_bg_action: Arc<dyn Fn(tray::TrayBgAction) + Send + Sync> =
            Arc::new(move |act| match act {
                tray::TrayBgAction::RebuildIndex => {
                    if !bg_rebuilding.swap(true, Ordering::SeqCst) {
                        spawn_rebuild(bg_dir.clone(), bg_tx.clone(), bg_ctx.clone());
                    }
                }
                tray::TrayBgAction::RefreshIndex => {
                    if bg_watch_active.load(Ordering::SeqCst) {
                        bg_flush_now.store(true, Ordering::SeqCst);
                    } else if !bg_rebuilding.swap(true, Ordering::SeqCst) {
                        spawn_rebuild(bg_dir.clone(), bg_tx.clone(), bg_ctx.clone());
                    }
                }
                tray::TrayBgAction::AdminRestart => {
                    #[cfg(windows)]
                    {
                        if restart_elevated() {
                            std::process::exit(0);
                        } else {
                            winvis::show();
                            bg_ctx.request_repaint();
                        }
                    }
                }
            });

        let mut app = ShunSeekApp {
            query: String::new(),
            cat_set: all_categories(),
            expanded_filter: None,
            scope_presets: settings.scope_presets.iter().cloned().collect(),
            scope_drives: settings.scope_drives.iter().copied().collect(),
            scope_custom: settings.scope_custom.clone(),
            scope_custom_sel: settings.scope_custom_sel.iter().cloned().collect(),
            scope_click_memo: None,
            theme,
            view_mode,
            grid_cols: 6,
            rows: Vec::new(),
            display: Vec::new(),
            expanded_groups: HashSet::new(),
            fold_active: false,
            icons: icons::IconCache::new(&cc.egui_ctx),
            names: names::NameService::new(&cc.egui_ctx),
            selected: 0,
            status,
            status_alert: false,
            search_focused: false,
            egui_ctx: cc.egui_ctx.clone(),
            store: Arc::new(Mutex::new(Arc::new(store))),
            usage: Arc::new(Mutex::new(usage)),
            delta_state: Arc::new(Mutex::new((
                build_index(Vec::new(), None),
                Tombstones::default(),
            ))),
            watch_gen: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            flush_now,
            watch_active,
            rebuilding: rebuilding.clone(),
            limit_override: None,
            override_query: String::new(),
            search_cancel: Arc::new(AtomicBool::new(true)),
            search_tx,
            search_rx,
            index_rx,
            hotkey: hotkey::Hotkey::start(cc.egui_ctx.clone(), settings.hotkey),
            tray: tray::Tray::new(
                cc.egui_ctx.clone(),
                tray::TrayInit {
                    rebuilding: rebuilding.clone(),
                    data_dir: dir.clone(),
                    hotkey_label: settings.hotkey.display(),
                    on_bg_action: Some(on_bg_action),
                },
            ),
            visible: !START_HIDDEN.load(Ordering::SeqCst),
            boot_show_frames: if START_HIDDEN.load(Ordering::SeqCst) {
                0
            } else {
                3
            },
            probe: std::env::args().find_map(|a| {
                a.strip_prefix("--ui-probe=").map(|s| s.to_string())
            }),
            allow_exit: false,
            focus_search: true,
            scroll_to_selected: false,
            data_dir: dir.clone(),
            index_tx: index_tx_app,
            hover_seen: None,
            hover_target: None,
            hover_since: 0.0,
            meta_cache: HashMap::new(),
            settings_open: false,
            settings_autostart: false,
            hotkey_combo: settings.hotkey,
            result_limit: settings.result_limit,
            fold_demoted: settings.fold_demoted,
            hide_outside: settings.hide_outside,
            index_count: 0,
            index_summary: String::new(),
            usage_confirm: false,
            settings_note: String::new(),
            hotkey_capture: false,
            hotkey_note: String::new(),
            hotkey_recs: Vec::new(),
            pins: Vec::new(),
            home_items: Vec::new(),
            home_cols: 4,
            result_text: String::new(),
            maximized: false,
            filter_dot_rect: None,
            filter_panel_rect: None,
            home_add_open: false,
            minimized: false,
            clickwatch: clickwatch::ClickWatch::start(cc.egui_ctx.clone()),
            scroll_prev: Vec::new(),
            scroll_cur: Vec::new(),
            blockers_prev: Vec::new(),
            blockers_cur: Vec::new(),
            pending_drag: None,
            filter_anim_t: [0.0, 0.0],
            filter_panel_full: [Vec2::new(216.0, 320.0), Vec2::new(372.0, 330.0)],
            ctx_menu_born: 0.0,
            home_add_born: 0.0,
            results_born: None,
            display_pal: theme.palette(),
            theme_anim: None,
            select_all_search: false,
            search_edit_id: None,
            ctx_menu: None,
        };
        app.pins = load_pins(&dir);
        app.rebuild_home_items();
        app.schedule_search();
        app
    }

    /// 输入变化 → 取消上次 → 30ms 防抖 → 后台线程搜索 → 通道回 UI。
    /// 输入变化 → 取消上次 → 30ms 防抖 → 后台线程搜索 → 通道回 UI。
    /// v0.1.6 冻结根治三件套：① 结果行映射切片前先收敛字符边界；
    /// ② store/usage 锁一律 lock_rec 中毒恢复，不再 Err 静默 return；
    /// ③ 线程体整体 catch_unwind，panic 也发 Done（error 标记），UI
    /// 显示「搜索线程异常，已自动恢复」，绝不冻结在旧结果上。
    /// 空查询不再走 UsageStore 首页：首页 = 用户 pins，由 UI 侧
    /// home_items 绘制（usage 只用于搜索排序加分，见 core）。
    fn schedule_search(&mut self) {
        // 搜索一变，右键菜单的下标目标即失效、添加菜单也失去
        // 锚点语境，一律关掉（v0.1.9 自绘菜单的卫生习惯）
        self.ctx_menu = None;
        self.home_add_open = false;
        self.search_cancel.store(true, Ordering::SeqCst);
        let cancel = Arc::new(AtomicBool::new(false));
        self.search_cancel = cancel.clone();

        let query = self.query.clone();
        let empty_query = query.trim().is_empty();
        let cat_set = self.cat_set.clone();
        let cat_mask: u16 = cat_set
            .iter()
            .fold(0u16, |m, c| m | (1u16 << (*c as u8)));
        // 展开折叠组的一次性上限提升：查询文本变了即失效
        if self.limit_override.is_some() && self.override_query != self.query {
            self.limit_override = None;
        }
        let result_limit = self.limit_override.unwrap_or(self.result_limit);
        let scope = self.build_scope_filter();
        let store = self.store.clone();
        let usage = self.usage.clone();
        let delta_state = self.delta_state.clone();
        let tx = self.search_tx.clone();
        let ctx = self.egui_ctx.clone();
        std::thread::Builder::new()
            .name("shunseek-search".into())
            .spawn(move || {
                let tx_panic = tx.clone();
                let ctx_panic = ctx.clone();
                let body = std::panic::AssertUnwindSafe(|| {
                    std::thread::sleep(Duration::from_millis(30));
                    if cancel.load(Ordering::SeqCst) {
                        return;
                    }
                    let started = Instant::now();
                    let rows: Vec<ResultRow>;
                    {
                        let store_arc = lock_rec(&store).clone();
                        let store_g = &*store_arc;
                        let usage_g = lock_rec(&usage);
                        let delta_g = lock_rec(&delta_state);
                        if empty_query {
                            // 首页由 UI 侧 pins 绘制；空查询只回空结果集，
                            // 保持消息管线（计数/选中复位）一致。
                            rows = Vec::new();
                        } else {
                            // v0.1.8：增量层非空（有增量条目或墓碑）时
                            // 走合并搜索（主索引带墓碑 + 增量层独立搜
                            // 后合并，命中下标 >= 主索引条数的是增量层）；
                            // 否则原单引擎路径，行为不变。
                            let (delta_store, tombs) = &*delta_g;
                            let hits = if delta_store.count() > 0 || !tombs.is_empty() {
                                search_merged(
                                    &store_g,
                                    Some(delta_store),
                                    Some(tombs),
                                    Some(&usage_g),
                                    &query,
                                    cat_mask,
                                    result_limit,
                                    &cancel,
                                    scope.as_ref(),
                                )
                            } else {
                                SearchEngine::new(&store_g, Some(&usage_g)).search_scoped(
                                    &query,
                                    cat_mask,
                                    result_limit,
                                    &cancel,
                                    scope.as_ref(),
                                )
                            };
                            if cancel.load(Ordering::SeqCst) {
                                return;
                            }
                            let base_count = store_g.count();
                            rows = hits
                                .iter()
                                .map(|h| {
                                    let (st, i): (&IndexStore, usize) =
                                        if (h.index as usize) < base_count {
                                            (&store_g, h.index as usize)
                                        } else {
                                            (delta_store, h.index as usize - base_count)
                                        };
                                    let name = st.name(i);
                                    // 高亮长度：core 给的是名字段的字符数
                                    // （路径模式下不含路径片段），换算成字节数。
                                    // match_start 是字节偏移：切片数字符前先
                                    // 收敛到字符边界（v0.1.6 根治——旧版直接
                                    // name[..start_byte] 切片，偏移落进多字节
                                    // 字符中间即 panic 并毒化互斥锁）。
                                    let match_len = if h.match_len == 0 {
                                        0
                                    } else {
                                        let start_byte = floor_char_boundary(
                                            name,
                                            h.match_start as usize,
                                        );
                                        let start_char =
                                            name[..start_byte].chars().count();
                                        let end_byte = name
                                            .char_indices()
                                            .nth(start_char + h.match_len as usize)
                                            .map(|(b, _)| b)
                                            .unwrap_or(name.len());
                                        end_byte - start_byte
                                    };
                                    let path = st.full_path(i);
                                    let category = st.category(i);
                                    // 类型名 + 重要度（v0.1.8）：每行
                                    // 构建时算一次，绘制/折叠直接读。
                                    let (type_label, importance) = filetype::describe(
                                        &path,
                                        category,
                                        matches!(
                                            category,
                                            Category::Folder | Category::InstallDir
                                        ),
                                    );
                                    ResultRow {
                                        name: name.to_string(),
                                        path,
                                        category,
                                        // 拼音首字母 / 全拼 / 子序列命中的
                                        // match_start 不是连续命中段，不
                                        // 高亮以免标错位置；连续命中也先
                                        // 收敛边界再存。
                                        match_start: if h.via_initials
                                            || h.via_pinyin
                                            || h.fuzzy
                                        {
                                            usize::MAX
                                        } else {
                                            floor_char_boundary(
                                                name,
                                                h.match_start as usize,
                                            )
                                        },
                                        match_len,
                                        demoted: h.demoted,
                                        type_label,
                                        importance,
                                    }
                                })
                                .collect();
                        }
                    }
                    if cancel.load(Ordering::SeqCst) {
                        return;
                    }
                    let _ = tx.send(SearchMsg::Done {
                        rows,
                        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
                        empty_query,
                        error: false,
                    });
                    ctx.request_repaint();
                });
                if std::panic::catch_unwind(body).is_err() {
                    let _ = tx_panic.send(SearchMsg::Done {
                        rows: Vec::new(),
                        elapsed_ms: 0.0,
                        empty_query,
                        error: true,
                    });
                    ctx_panic.request_repaint();
                }
            })
            .ok();
    }
    /// 隐藏到托盘（UI 线程内的落点：✕ 按钮 / Esc 终段 / 打开
    /// 条目后 / 点外裁决兜底）。v0.1.12 起实际动作收敛到
    /// winvis::hide()（ShowWindow SW_HIDE，本线程当场执行）；
    /// 此前六版走 ViewportCommand::Visible 的老路已整段删除。
    /// 非 Windows 无 Win32 可调，同一调用点以 ViewportCommand
    /// 兜底（演示平台机制，不进 Windows 链）。
    fn hide(&mut self, ctx: &egui::Context) {
        self.visible = false;
        // 设置窗是 immediate viewport，靠主窗每帧 update 驱动；主窗
        // 一旦 SW_HIDE，它就变成点不动、也关不掉的死窗口（v0.1.14
        // 真机的「设置界面有时关不掉」）。藏主窗时顺手把设置视口
        // 关掉——直接发 Close 命令，不等下一帧（下一帧可能永远不来）。
        if self.settings_open {
            self.settings_open = false;
            if self.hotkey_capture {
                self.hotkey_capture = false;
                self.hotkey.set_paused(false);
            }
            ctx.send_viewport_cmd_to(
                egui::ViewportId::from_hash_of("shunseek_settings"),
                ViewportCommand::Close,
            );
        }
        self.on_hidden();
        clickwatch::disarm();
        winvis::hide();
        #[cfg(not(windows))]
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
    }

    /// 隐藏的界面清理（与 hide() 本体拆开：窗口被其他线程直接
    /// ShowWindow 藏掉时，UI 是在帧首 OS 同步里发现状态变化的，
    /// 清理走同一入口——见 sync_shadow_from_os 的跃迁处理）。
    /// 隐藏时把所有弹出层一并收掉：下次呼出是干净界面，也免得
    /// 面板开着的状态被钩子判定链拖进下一轮。
    fn on_hidden(&mut self) {
        self.expanded_filter = None;
        self.ctx_menu = None;
        self.home_add_open = false;
        self.pending_drag = None;
    }

    /// 呼出（UI 线程内的落点）。实际动作收敛到 winvis::show()
    /// （最小化 → SW_RESTORE、否则 SW_SHOW + SetForegroundWindow，
    /// 本线程当场执行）；旧版「Visible+Minimized+Focus 三件套」
    /// 已删除——它依赖 UI 线程先被 repaint 唤醒，正是病根。
    /// 非 Windows 同 hide() 口径以 ViewportCommand 兜底。
    /// 注：v0.1.12 起外部呼出（托盘/热键/唤醒）都由发起线程
    /// 直接执行，本方法是 UI 侧唯一的呼出入口，暂无调用点
    /// 是设计结果而非遗漏（可见性单点收敛，见 winvis 模块头）。
    #[allow(dead_code)]
    fn show(&mut self, _ctx: &egui::Context) {
        winvis::show();
        #[cfg(not(windows))]
        {
            _ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            _ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            _ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
        self.visible = true;
        self.minimized = false;
        self.on_shown();
    }

    /// 呼出的 UI 副作用（与 show() 本体拆开：窗口被托盘/热键/
    /// 唤醒线程直接 ShowWindow 呼出时，可见性已经落地，UI 在
    /// 随后的帧里只需补这组副作用——见 after_external_summon）。
    fn on_shown(&mut self) {
        // 呼出保留查询词 + 全选（v0.1.9 对齐原版 Show 的
        // SelectAll 口径）：打字即覆盖、按方向键即改词，比旧版
        // 「呼出即清空」更接近原版体感。全选在 draw_search_box
        // 里随聚焦一起落地（select_all_search 标志）。
        self.select_all_search = true;
        // 保留查询也重搜一次：索引可能已增量更新；空查询时首页
        // 按使用频次排序，刚打开过的条目计数 +1 要立刻反映。
        self.schedule_search();
        self.focus_search = true;
    }

    /// 每帧把影子标志与 OS 状态对齐（v0.1.12）：visible /
    /// minimized 从此只是缓存，真相在 winvis::state()
    /// （IsWindowVisible / IsIconic）。可见性现在会被托盘/热键/
    /// 钩子/唤醒四个线程直接改动，影子必然滞后——绘制与武装
    /// 判定读缓存即可（滞后一帧无害），但任何**决策**不许读
    /// 缓存（托盘/热键的切换决策在发起线程里现读 OS，见
    /// winvis::toggle）。跃迁到 Hidden 时补跑 on_hidden 清理
    /// （外部线程藏窗时 UI 没有别的入口知道）。
    fn sync_shadow_from_os(&mut self) {
        let st = winvis::state();
        let now_visible = st != winvis::VisState::Hidden;
        if self.visible && !now_visible {
            self.on_hidden();
        }
        self.visible = now_visible;
        self.minimized = st == winvis::VisState::Minimized;
        #[cfg(windows)]
        {
            self.maximized = winvis::is_maximized();
        }
    }

    /// 外部显式呼出（单实例唤醒 / 热键 / 托盘 Toggle·Show·
    /// 设置·关于）在 UI 侧的统一收尾：发起线程已经直接
    /// ShowWindow 改完可见性，这里只做两件事——影子与 OS
    /// 对齐（sync 内含跃迁清理）、窗口确实在显示时补呼出
    /// 副作用，最后 drain 点外标志防陈旧标志坑下一轮。
    fn after_external_summon(&mut self) {
        self.sync_shadow_from_os();
        if self.visible {
            self.on_shown();
        }
        self.clickwatch.drain();
    }

    /// 把主窗/设置窗/托盘图标的矩形（物理像素）发布给点外钩子（clickwatch）。
    /// egui 的 outer_rect 是监控器空间的逻辑点，钩子拿到的是
    /// MSLLHOOKSTRUCT 的物理像素，必须乘 native_pixels_per_point
    /// 对齐。取不到主窗矩形 → publish 内部强制不武装。
    fn publish_click_bounds(&self, ctx: &egui::Context) {
        let conv = |v: &egui::ViewportInfo, fallback_ppp: f32| -> Option<[i32; 4]> {
            let r = v.outer_rect?;
            let ppp = fallback_ppp;
            Some([
                (r.left() * ppp).round() as i32,
                (r.top() * ppp).round() as i32,
                (r.right() * ppp).round() as i32,
                (r.bottom() * ppp).round() as i32,
            ])
        };
        let (egui_main, ppp) = ctx.input(|i| (conv(i.viewport(), i.pixels_per_point), i.pixels_per_point));
        let main = winvis::window_rect().or(egui_main);
        let sub = if self.settings_open {
            let settings_id = egui::ViewportId::from_hash_of("shunseek_settings");
            ctx.input_for(settings_id, |i| conv(i.viewport(), ppp))
        } else {
            None
        };
        let tray = self.tray.icon_rect();
        // 只有用户显式开启「点外自动隐藏」才武装钩子；关闭时 armed
        // 恒 false，钩子仍在但不产生任何动作（点别的窗口/桌面/
        // 任务栏都不再把它藏走）。
        let armed = self.hide_outside
            && !self.settings_open
            && winvis::state() == winvis::VisState::Shown;
        self.clickwatch.publish(main, sub, tray, armed);
    }

    /// 主题交叉淡入推进（170ms）：display_pal 从旧板向目标板
    /// cubic_out 插值；到位后停表，静止零重绘。egui 的 Visuals
    /// （install_visuals）仍在 set_theme 时一次切到目标——受其
    /// 影响的只有 egui 原生控件底色，本应用全部自绘面走
    /// display_pal 平滑过渡。
    fn refresh_display_pal(&mut self, ctx: &egui::Context) {
        let target = self.theme.palette();
        if let Some((from, t0)) = self.theme_anim {
            let age = ctx.input(|i| i.time) - t0;
            let t = (age / 0.17).clamp(0.0, 1.0) as f32;
            if t >= 1.0 {
                self.display_pal = target;
                self.theme_anim = None;
            } else {
                self.display_pal = from.lerp(&target, egui::emath::easing::cubic_out(t));
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
            }
        } else {
            self.display_pal = target;
        }
    }

    /// 筛选面板动画推进：filter_anim_t 朝目标（展开的那块 =1、
    /// 其余 =0）以 0.24s 线性时间推进，渲染时过 cubic_out。
    /// 只在未到位时请求下一帧——静止零重绘。
    fn advance_filter_anim(&mut self, ctx: &egui::Context) {
        let dt = ctx.input(|i| i.stable_dt).min(0.05);
        for (i, kind) in [FilterKind::Type, FilterKind::Scope].iter().enumerate() {
            let target = if self.expanded_filter == Some(*kind) {
                1.0
            } else {
                0.0
            };
            let t = &mut self.filter_anim_t[i];
            if *t != target {
                let dir = if target > *t { 1.0 } else { -1.0 };
                *t = (*t + dir * dt / 0.24).clamp(0.0, 1.0);
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
            }
        }
    }

    /// 拖动 / 拉伸手判（v0.1.10，替代整窗背景控件）：本帧只读
    /// 输入状态 + 上一帧的控件矩形集（blockers_prev），不注册
    /// 任何交互控件，因此物理上不可能抢走任何按钮的 click。
    /// 判定顺序：拉伸带（仅 8px 边框带/12px 角，内容区控件
    /// 最小内边距 12 起，永不重叠）→ 双击最大化 → 控件排除 →
    /// 待命拖动（位移越过 4px 才 StartDrag）。按下点落在控件上
    /// 时 pending 根本不设，微位移点击永远归控件。
    fn handle_window_press(&mut self, ctx: &egui::Context) {
        if !self.visible {
            self.pending_drag = None;
            return;
        }
        let screen = ctx.screen_rect();
        let (press_origin, primary_pressed, primary_down, dbl, latest) =
            ctx.input(|i| {
                (
                    i.pointer.press_origin(),
                    i.pointer.primary_pressed(),
                    i.pointer.primary_down(),
                    i.pointer.button_double_clicked(egui::PointerButton::Primary),
                    i.pointer.latest_pos(),
                )
            });
        if self.maximized {
            self.pending_drag = None;
            if primary_pressed && dbl {
                if let Some(origin) = press_origin {
                    if screen.contains(origin)
                        && !self.blockers_prev.iter().any(|r| r.contains(origin))
                    {
                        self.maximized = false;
                        ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
                    }
                }
            }
            return;
        }
        // 悬停在拉伸带上时给方向光标（无按下才做，纯提示）
        if !primary_down {
            if let Some(p) = latest {
                if screen.contains(p) {
                    if let Some((_, cursor)) = resize_zone_at(screen, p) {
                        ctx.set_cursor_icon(cursor);
                    }
                }
            }
        }
        if primary_pressed {
            if let Some(origin) = press_origin {
                if screen.contains(origin) {
                    // ① 拉伸带：带内没有任何控件，直接发命令
                    if let Some((dir, _)) = resize_zone_at(screen, origin) {
                        ctx.send_viewport_cmd(
                            ViewportCommand::BeginResize(dir),
                        );
                        self.pending_drag = None;
                        return;
                    }
                    let blocked =
                        self.blockers_prev.iter().any(|r| r.contains(origin));
                    // ② 双击空白区：最大化/还原
                    if dbl {
                        if !blocked {
                            self.maximized = true;
                            ctx.send_viewport_cmd(
                                ViewportCommand::Maximized(true),
                            );
                        }
                        self.pending_drag = None;
                        return;
                    }
                    // ③ 滚动条带上：拖动是滚内容，绝不拖窗
                    if self.scroll_prev.iter().any(|r| r.contains(origin)) {
                        self.pending_drag = None;
                        return;
                    }
                    // ④ 其余任何位置（含控件上）：一律进入待命拖动。
                    // 旧版（v0.1.10–v0.1.13）在这里用「按下点是否落在
                    // 控件矩形内」直接排除——而结果行/卡片/搜索区这些
                    // rect 都是拉满整行整列的扁平矩形，视觉上的「空白」
                    // 常常仍落在里面，于是时灵时不灵。改为统一待命，
                    // 靠下面的 8px 位移阈值区分意图：位移不足 8px
                    // （大于 egui 的 6px 点击阈值再多给 2px 余量）仍
                    // 归控件，超过就开始拖窗——纯点击不会被抢。
                    self.pending_drag = Some(origin);
                }
            }
        }
        if let Some(origin) = self.pending_drag {
            if !primary_down {
                self.pending_drag = None;
            } else if let Some(p) = latest {
                if p.distance(origin) >= 8.0 {
                    self.pending_drag = None;
                    ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                }
            }
        }
    }

    /// 滚动条带登记为拖动排除区：内容可滚时滚动条贴在
    /// inner_rect 右侧（inner_rect 按 egui 定义不含滚动条），
    /// 在条上按下拖动是滚动操作，绝不能变成窗口拖动。
    fn push_scroll_blocker(&mut self, out: &egui::scroll_area::ScrollAreaOutput<()>) {
        if out.content_size.y > out.inner_rect.height() + 1.0 {
            let band = egui::Rect::from_min_max(
                egui::pos2(out.inner_rect.right(), out.inner_rect.top()),
                egui::pos2(out.inner_rect.right() + 14.0, out.inner_rect.bottom()),
            );
            self.blockers_cur.push(band);
            self.scroll_cur.push(band);
        }
    }

    fn open_row(&mut self, ctx: &egui::Context, idx: usize, locate: bool) {
        let Some(row) = self.rows.get(idx).cloned() else {
            return;
        };
        // 打开失败提示（v0.1.8）：路径已不存在（被移动/删除而
        // 索引还没刷新）时不再盲开 ShellExecute，只在状态栏说明。
        if !row.path.is_empty() && !std::path::Path::new(&row.path).exists() {
            self.status = "路径已不存在（可能被移动或删除）".to_string();
            return;
        }
        opener::open(&row.path, &row.name, locate);
        {
            let mut u = lock_rec(&self.usage);
            u.record_use(&row.path);
            let _ = u.save();
        }
        self.hide(ctx);
    }

    fn open_selected(&mut self, ctx: &egui::Context, locate: bool) {
        // 首页：Enter 打开选中的 pin 磁贴。
        if self.is_home() {
            if !self.home_items.is_empty() {
                let idx = self.selected.min(self.home_items.len() - 1);
                self.open_home_item(ctx, idx, locate);
            }
            return;
        }
        // 与 C# 版一致：没选中时退回第一条。组行 Enter = 展开/收起，不打开。
        let idx = if self.display.get(self.selected).is_some() {
            self.selected
        } else {
            0
        };
        match self.display.get(idx).cloned() {
            Some(DisplayRow::Group { key, .. }) => self.toggle_group(&key),
            Some(DisplayRow::Single { row, .. }) => self.open_row(ctx, row, locate),
            None => {}
        }
    }

    fn move_selection(&mut self, delta: i32) {
        // 导航长度：首页 = pins 数；搜索结果 = 展示行数。
        let len = if self.is_home() {
            self.home_items.len()
        } else {
            self.display.len()
        };
        if len == 0 {
            return;
        }
        let next = (self.selected as i32 + delta).clamp(0, len as i32 - 1);
        self.selected = next as usize;
        self.scroll_to_selected = true;
    }

    /// 由 rows 重建展示行（v0.1.8，原版 MainViewModel.FoldByType
    /// 口径）：只在 fold_active && fold_demoted 时折叠，其余平铺。
    /// 折叠判定按行的重要度（core filetype）+ demoted 信号：
    ///  - High 永不折，永远平铺；
    ///  - 前 5 条非 High 结果保底平铺（原版 AlwaysVisible=5）；
    ///  - Normal：同一类型名在非 High 中的总数 ≥4（FoldThreshold）
    ///    时，其余（非保底）成员折成一个「{N} 个{类型}」组，组头
    ///    出现在该类型第一个被折成员的位置；N = 总数 − 保底数；
    ///  - Low 或 demoted：非保底成员全部汇成尾部一个「其他 N 项
    ///    （日志/缓存/临时文件）」组（原版 LowGroupLabel 口径）；
    ///    只剩 1 条时直接平铺在尾部、不成组。
    /// 组头恒保留：展开时成员以 indent 紧随组头，再点组头收起
    ///（展开状态按组键存 expanded_groups，跨搜索保留）。
    fn rebuild_display(&mut self) {
        let mut display: Vec<DisplayRow> = Vec::with_capacity(self.rows.len());
        if self.fold_active && self.fold_demoted {
            let rows = &self.rows;
            // 非 High 总数按类型名计（折叠阈值口径）
            let mut totals: HashMap<&str, usize> = HashMap::new();
            for r in rows {
                if r.importance != Importance::High {
                    *totals.entry(r.type_label.as_str()).or_default() += 1;
                }
            }
            // 保底集：前 5 条非 High 的下标
            let mut always: HashSet<usize> = HashSet::new();
            for (i, r) in rows.iter().enumerate() {
                if always.len() >= 5 {
                    break;
                }
                if r.importance != Importance::High {
                    always.insert(i);
                }
            }
            let mut emitted: HashSet<&str> = HashSet::new();
            let mut low_rows: Vec<usize> = Vec::new();
            for (i, r) in rows.iter().enumerate() {
                let is_low = r.importance == Importance::Low || r.demoted;
                if r.importance == Importance::High {
                    display.push(DisplayRow::Single {
                        row: i,
                        indent: false,
                    });
                } else if is_low {
                    if always.contains(&i) {
                        display.push(DisplayRow::Single {
                            row: i,
                            indent: false,
                        });
                    } else {
                        low_rows.push(i);
                    }
                } else if always.contains(&i) {
                    display.push(DisplayRow::Single {
                        row: i,
                        indent: false,
                    });
                } else if totals.get(r.type_label.as_str()).copied().unwrap_or(0) >= 4 {
                    // Normal 且同类够多：首个被折成员处出组头
                    if emitted.insert(r.type_label.as_str()) {
                        let label = r.type_label.clone();
                        // 组头计数 = 该组实际代表的成员数（非保底、
                        // Normal、未被 demoted 信号划去 Low 组的）
                        let count = rows
                            .iter()
                            .enumerate()
                            .filter(|(j, rr)| {
                                rr.importance == Importance::Normal
                                    && !rr.demoted
                                    && rr.type_label == label
                                    && !always.contains(j)
                            })
                            .count();
                        let key = format!("type:{label}");
                        let expanded = self.expanded_groups.contains(&key);
                        display.push(DisplayRow::Group {
                            key,
                            label: format!("{count} 个{label}"),
                            count,
                            expanded,
                        });
                        if expanded {
                            for (j, rr) in rows.iter().enumerate() {
                                if rr.importance == Importance::Normal
                                    && !rr.demoted
                                    && rr.type_label == label
                                    && !always.contains(&j)
                                {
                                    display.push(DisplayRow::Single {
                                        row: j,
                                        indent: true,
                                    });
                                }
                            }
                        }
                    }
                    // 其余成员已被组行代表，跳过
                } else {
                    display.push(DisplayRow::Single {
                        row: i,
                        indent: false,
                    });
                }
            }
            // Low 组恒在尾部（原版：低组在主循环之后统一发出）
            match low_rows.len() {
                0 => {}
                1 => display.push(DisplayRow::Single {
                    row: low_rows[0],
                    indent: false,
                }),
                n => {
                    let key = "low".to_string();
                    let expanded = self.expanded_groups.contains(&key);
                    display.push(DisplayRow::Group {
                        label: format!("其他 {n} 项（日志/缓存/临时文件）"),
                        key,
                        count: n,
                        expanded,
                    });
                    if expanded {
                        for &j in &low_rows {
                            display.push(DisplayRow::Single {
                                row: j,
                                indent: true,
                            });
                        }
                    }
                }
            }
        } else {
            display.extend((0..self.rows.len()).map(|i| DisplayRow::Single {
                row: i,
                indent: false,
            }));
        }
        self.display = display;
        if self.selected >= self.display.len() {
            self.selected = self.display.len().saturating_sub(1);
        }
    }

    /// 展开/收起一个折叠组（按组键记状态），重建展示行；组头恒
    /// 保留、位置不因展开/收起而移动，selected 无需调整。
    /// 展开时若当前结果已顶到上限且上限 <300，一次性把上限提到
    /// 300 重查（原版 Expand 行为：组里可能还有没取回的成员）；
    /// 提升只对当前查询有效，查询一变即失效（见 schedule_search）。
    fn toggle_group(&mut self, key: &str) {
        let expanding = !self.expanded_groups.contains(key);
        if expanding {
            self.expanded_groups.insert(key.to_string());
        } else {
            self.expanded_groups.remove(key);
        }
        if expanding
            && self.rows.len() >= self.result_limit
            && self.result_limit < 300
            && self.limit_override.is_none()
        {
            self.limit_override = Some(300);
            self.override_query = self.query.clone();
            self.schedule_search();
        }
        self.rebuild_display();
    }

    /// 切主题的唯一入口（状态栏快捷按钮 / 设置窗口共用）：
    /// 换 visuals + 写 settings.txt，两处不许各自为政。
    fn set_theme(&mut self, ctx: &egui::Context, theme: Theme) {
        if theme != self.theme {
            // 内容层交叉淡入起点 = 当前帧正在显示的板（动画中途
            // 再切也从中间态接着走，不跳）
            let now = ctx.input(|i| i.time);
            self.theme_anim = Some((self.display_pal, now));
        }
        self.theme = theme;
        install_visuals(ctx, theme);
        self.save_settings();
    }

    /// 切展示模式的唯一入口：chips 栏右端的页面按钮是主控件，
    /// 设置窗口「启动时默认显示」与它共用同一状态与持久化键。
    fn set_view_mode(&mut self, mode: ViewMode) {
        self.view_mode = mode;
        self.save_settings();
    }

    /// 把当前全部设置写回 settings.txt（唯一写入口，见 load_settings）。
    /// 范围编码：scope_presets=逗号分隔的预设键名；scope_drives=盘符
    /// 连写（如 CD）；scope_custom 一行一个完整路径（可重复多行）。
    fn save_settings(&self) {
        let mut text = format!(
            "theme={}\nview={}\nhotkey={}\nresult_limit={}\nfold_demoted={}\nhide_outside={}\n",
            match self.theme {
                Theme::Dark => "dark",
                Theme::Light => "light",
            },
            match self.view_mode {
                ViewMode::List => "list",
                ViewMode::Grid => "grid",
            },
            self.hotkey_combo.key_str(),
            self.result_limit,
            self.fold_demoted as u8,
            self.hide_outside as u8,
        );
        let mut presets: Vec<&str> = self.scope_presets.iter().map(|s| s.as_str()).collect();
        presets.sort_unstable();
        text.push_str(&format!("scope_presets={}\n", presets.join(",")));
        let mut drives: Vec<char> = self.scope_drives.iter().copied().collect();
        drives.sort_unstable();
        text.push_str(&format!(
            "scope_drives={}\n",
            drives.iter().collect::<String>()
        ));
        for c in &self.scope_custom {
            text.push_str(&format!("scope_custom={c}\n"));
        }
        let mut sel: Vec<String> = self
            .scope_custom
            .iter()
            .filter(|c| self.scope_custom_sel.contains(*c))
            .cloned()
            .collect();
        sel.sort_unstable();
        // 一个都没选中也要留一个空行：加载层靠「有没有 sel 键」区分
        // 「旧档（全部自定义都参与）」和「用户显式一个都不选」。
        if sel.is_empty() && !self.scope_custom.is_empty() {
            text.push_str("scope_custom_sel=\n");
        }
        for c in &sel {
            text.push_str(&format!("scope_custom_sel={c}\n"));
        }
        let _ = std::fs::write(self.data_dir.join("settings.txt"), text);
    }

    /// 当前范围是否「全部」：预设全选 + 现存磁盘全选。自定义文件夹
    /// 不影响全部判定——它们是额外根，全部状态下加入即在选中集里，
    /// 离开全部状态后作为根生效。
    fn scope_is_all(&self) -> bool {
        PRESET_KEYS
            .iter()
            .all(|(k, _, _)| self.scope_presets.contains(*k))
            && existing_drives()
                .iter()
                .all(|d| self.scope_drives.contains(d))
    }

    /// 把范围选中集展开成 core 的 ScopeFilter；全部 → None 不限制。
    fn build_scope_filter(&self) -> Option<ScopeFilter> {
        if self.scope_is_all() {
            return None;
        }
        let profile = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        let base = PathBuf::from(&profile);
        let mut roots: Vec<String> = Vec::new();
        for (key, sub, _) in PRESET_KEYS {
            if self.scope_presets.contains(key) {
                roots.push(if sub.is_empty() {
                    base.to_string_lossy().into_owned()
                } else {
                    base.join(sub).to_string_lossy().into_owned()
                });
            }
        }
        for d in &self.scope_drives {
            roots.push(format!("{d}:\\"));
        }
        roots.extend(
            self.scope_custom
                .iter()
                .filter(|c| self.scope_custom_sel.contains(*c))
                .cloned(),
        );
        if roots.is_empty() {
            None
        } else {
            Some(ScopeFilter::new(&roots))
        }
    }

    /// 范围摘要（圆点旁小胶囊 / 悬浮提示用）：全部 / 单个名 / 首个 +N。
    fn scope_summary(&self) -> String {
        if self.scope_is_all() {
            return "全部".to_string();
        }
        let mut names: Vec<String> = Vec::new();
        for (key, _, label) in PRESET_KEYS {
            if self.scope_presets.contains(key) {
                names.push(label.to_string());
            }
        }
        let mut drives: Vec<char> = self.scope_drives.iter().copied().collect();
        drives.sort_unstable();
        for d in drives {
            names.push(format!("{d}:"));
        }
        for c in &self.scope_custom {
            if self.scope_custom_sel.contains(c) {
                names.push(dir_last(c));
            }
        }
        match names.len() {
            0 => "全部".to_string(),
            1 => names[0].clone(),
            n => format!("{} +{}", names[0], n - 1),
        }
    }

    /// pins 持久化（数据目录 pins.txt，一行一条完整路径）。
    fn save_pins(&self) {
        let _ = std::fs::write(self.data_dir.join("pins.txt"), self.pins.join("\n"));
    }

    fn add_pin(&mut self, path: &str) {
        if self.pins.iter().any(|p| p.eq_ignore_ascii_case(path)) {
            return;
        }
        // 新固定插队首（对齐原版：最近固定的排最前）
        self.pins.insert(0, path.to_string());
        self.save_pins();
        self.rebuild_home_items();
    }

    fn remove_pin(&mut self, path: &str) {
        self.pins.retain(|p| !p.eq_ignore_ascii_case(path));
        self.save_pins();
        self.rebuild_home_items();
    }

    fn is_pinned(&self, path: &str) -> bool {
        self.pins.iter().any(|p| p.eq_ignore_ascii_case(path))
    }

    /// 由 pins 派生首页展示行：直接按路径与扩展名推断分类，避免在 UI 线程全表扫描索引。
    fn rebuild_home_items(&mut self) {
        self.home_items = self
            .pins
            .iter()
            .map(|p| {
                let name = p
                    .trim_end_matches(['\\', '/'])
                    .rsplit(['\\', '/'])
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(p.as_str())
                    .to_string();
                let is_dir = std::path::Path::new(p).is_dir();
                let category = if is_dir {
                    Category::Folder
                } else {
                    let lower = name.to_lowercase();
                    if lower.ends_with(".lnk") {
                        Category::Shortcut
                    } else if lower.ends_with(".exe") {
                        Category::App
                    } else {
                        shunseek_core::classify_by_extension(&name)
                    }
                };
                let (type_label, importance) = filetype::describe(p, category, is_dir);
                ResultRow {
                    name,
                    path: p.clone(),
                    category,
                    match_start: usize::MAX,
                    match_len: 0,
                    demoted: false,
                    type_label,
                    importance,
                }
            })
            .collect();
    }

    /// 打开一个首页磁贴（与 open_row 同语义：记录使用 + 隐藏）。
    fn open_home_item(&mut self, ctx: &egui::Context, idx: usize, locate: bool) {
        let Some(row) = self.home_items.get(idx).cloned() else {
            return;
        };
        if !row.path.is_empty() && !std::path::Path::new(&row.path).exists() {
            self.status = "路径已不存在（可能被移动或删除）".to_string();
            return;
        }
        opener::open(&row.path, &row.name, locate);
        {
            let mut u = lock_rec(&self.usage);
            u.record_use(&row.path);
            let _ = u.save();
        }
        self.hide(ctx);
    }

    /// 当前是否首页（空查询）：首页只显 pins，与搜索结果管线分开。
    fn is_home(&self) -> bool {
        self.query.trim().is_empty()
    }

    /// 重新建立索引的唯一入口（托盘 / 设置面板共用）：已有重建在跑
    /// 就忽略（并发护栏），进度沿用分级建立的状态栏摘要。
    fn start_rebuild(&mut self) {
        if self.rebuilding.swap(true, Ordering::SeqCst) {
            return;
        }
        self.tray.set_rebuild_enabled(false);
        spawn_rebuild(
            self.data_dir.clone(),
            self.index_tx.clone(),
            self.egui_ctx.clone(),
        );
    }

    fn open_settings(&mut self) {
        self.settings_open = true;
        self.settings_autostart = autostart::is_enabled();
        self.usage_confirm = false;
    }

    fn close_settings(&mut self) {
        self.settings_open = false;
        if self.hotkey_capture {
            self.hotkey_capture = false;
            self.hotkey.set_paused(false);
        }
        self.focus_search = true;
    }

    /// 原始按键事件处理（绕开 TextEdit 对方向键等的消费，保证在输入框
    /// 聚焦时 ↑↓ / Enter / Esc 也归我们管，同 C# 版 PreviewKeyDown）。
    /// 当前选中条目的完整路径（Ctrl+C 复制用）：首页取磁贴项，
    /// 搜索态取选中展示行（折叠组行无单一路径，返回 None）。
    fn selected_path(&self) -> Option<String> {
        if self.is_home() {
            self.home_items.get(self.selected).map(|it| it.path.clone())
        } else {
            match self.display.get(self.selected) {
                Some(DisplayRow::Single { row, .. }) => {
                    self.rows.get(*row).map(|r| r.path.clone())
                }
                _ => None,
            }
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        // 自绘菜单开着时（v0.1.9）：Esc 只关菜单，不许穿透到
        // 下面的 Esc 三段链（否则关菜单顺带清查询/藏窗口）；
        // 右键菜单是模态的，其余键一律不处理（防菜单还开着时
        // Enter 把选中行打开、方向键在菜单底下乱跑）。
        if self.ctx_menu.is_some() || self.home_add_open {
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.ctx_menu = None;
                self.home_add_open = false;
                return;
            }
            if self.ctx_menu.is_some() {
                return;
            }
        }
        let events: Vec<egui::Event> = ctx.input(|i| i.raw.events.clone());
        for ev in &events {
            if let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = ev
            {
                match *key {
                    // Esc 三段链（v0.1.8 补中间一档，对齐原版）：
                    // 先收起展开中的筛选控件 → 再清查询（回到
                    // 首页）→ 最后才隐藏窗口。筛选面板 v0.1.10 起
                    // 是自绘覆盖层，收起 = expanded_filter 清空后
                    // 动画回缩，不再有 egui popup 可关。
                    Key::Escape => {
                        if self.expanded_filter.take().is_some() {
                            // 已收起（动画回缩由 advance_filter_anim 接管）
                        } else if !self.query.is_empty() {
                            self.query.clear();
                            self.schedule_search();
                        } else {
                            self.hide(ctx);
                        }
                    }
                    // Ctrl+C 复制（v0.1.9 对齐原版；优先级钉死）：
                    // 搜索框里有选中文本 → 什么都不做，TextEdit 会
                    // 在绘制时自己复制文本；无选中文本 → 复制当前
                    // 选中条目的完整路径。事件不消费：两条路互斥
                    // 靠选区判定，不靠拦截。
                    Key::C
                        if modifiers.ctrl
                            && !modifiers.alt
                            && !modifiers.shift =>
                    {
                        let has_text_selection = self
                            .search_edit_id
                            .and_then(|id| egui::TextEdit::load_state(ctx, id))
                            .and_then(|st| st.cursor.char_range())
                            .map(|r| r.primary.index != r.secondary.index)
                            .unwrap_or(false);
                        if !has_text_selection {
                            if let Some(path) = self.selected_path() {
                                ctx.copy_text(path);
                            }
                        }
                    }
                    // PgUp/PgDn：三行/三排步进（v0.1.8）；Home/End
                    // 跳首/尾。步长口径与方向键一致（列表 1、宫格
                    // 按列数、首页按首页列数）。
                    Key::PageDown | Key::PageUp => {
                        let unit = if self.is_home() {
                            self.home_cols.max(1) as i32
                        } else if self.view_mode == ViewMode::Grid {
                            self.grid_cols.max(1) as i32
                        } else {
                            1
                        };
                        let d = unit * 3;
                        self.move_selection(if *key == Key::PageDown { d } else { -d });
                    }
                    Key::Home => {
                        self.selected = 0;
                        self.scroll_to_selected = true;
                    }
                    Key::End => {
                        let len = if self.is_home() {
                            self.home_items.len()
                        } else {
                            self.display.len()
                        };
                        if len > 0 {
                            self.selected = len - 1;
                            self.scroll_to_selected = true;
                        }
                    }
                    // 宫格步长：首页用首页列数、搜索宫格用宫格列数；
                    // 列表步长 1。首页强制宫格，←→ 恒可用。
                    Key::ArrowDown => {
                        let d = if self.is_home() {
                            self.home_cols.max(1) as i32
                        } else if self.view_mode == ViewMode::Grid {
                            self.grid_cols.max(1) as i32
                        } else {
                            1
                        };
                        self.move_selection(d);
                    }
                    Key::ArrowUp => {
                        let d = if self.is_home() {
                            self.home_cols.max(1) as i32
                        } else if self.view_mode == ViewMode::Grid {
                            self.grid_cols.max(1) as i32
                        } else {
                            1
                        };
                        self.move_selection(-d);
                    }
                    Key::ArrowLeft
                        if self.is_home() || self.view_mode == ViewMode::Grid =>
                    {
                        self.move_selection(-1)
                    }
                    Key::ArrowRight
                        if self.is_home() || self.view_mode == ViewMode::Grid =>
                    {
                        self.move_selection(1)
                    }
                    Key::Enter => self.open_selected(ctx, modifiers.ctrl),
                    _ => {}
                }
            }
        }
    }

    fn poll_channels(&mut self, ctx: &egui::Context) {
        // 索引重建完成：store 本体内存直传，直接换入，不再经文件。
        // 换入锁走 lock_rec：就算此前有线程 panic 毒化过也能恢复。
        while let Ok(msg) = self.index_rx.try_recv() {
            match msg {
                IndexMsg::Rebuilt {
                    store,
                    count,
                    summary,
                    final_stage,
                    watch,
                } => {
                    let new_arc = Arc::new(store);
                    {
                        let mut g = lock_rec(&self.store);
                        *g = new_arc.clone();
                    }
                    self.index_count = count;
                    self.index_summary = summary.clone();
                    self.rebuild_home_items();
                    if final_stage {
                        self.rebuilding.store(false, Ordering::SeqCst);
                        self.tray.set_rebuild_enabled(true);
                        self.status_alert = count == 0;
                        self.status = if count == 0 {
                            ALERT_EMPTY_INDEX.to_string()
                        } else {
                            format!("{} 项", group_thousands(count))
                        };
                        // 旧 Mmap 已从 self.store 卸下，在后台线程异步落盘（短暂重试避开搜索线程持有）
                        let save_dir = self.data_dir.clone();
                        let save_tx = self.index_tx.clone();
                        let save_ctx = self.egui_ctx.clone();
                        std::thread::Builder::new()
                            .name("shunseek-save-cache".into())
                            .spawn(move || {
                                let scope = current_scope();
                                let ts = now_unix();
                                let path = save_dir.join("index.bin");
                                let mut saved = false;
                                for _ in 0..6 {
                                    if new_arc.save_with_meta(&path, &scope, ts).is_ok() {
                                        saved = true;
                                        break;
                                    }
                                    std::thread::sleep(Duration::from_millis(50));
                                }
                                let _ = save_tx.send(IndexMsg::CacheSaved(saved));
                                save_ctx.request_repaint();
                            })
                            .ok();

                        let gen = self
                            .watch_gen
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                            + 1;
                        *lock_rec(&self.delta_state) = (
                            build_index(Vec::new(), None),
                            Tombstones::default(),
                        );
                        self.watch_active.store(!watch.is_empty(), Ordering::SeqCst);
                        #[cfg(windows)]
                        if !watch.is_empty() {
                            spawn_watch_supervisor(
                                gen,
                                watch,
                                self.store.clone(),
                                self.delta_state.clone(),
                                self.watch_gen.clone(),
                                self.flush_now.clone(),
                                self.index_tx.clone(),
                                self.egui_ctx.clone(),
                                count,
                            );
                        }
                        #[cfg(not(windows))]
                        let _ = gen;
                    } else {
                        self.status = "正在建立索引…".to_string();
                    }
                    self.schedule_search();
                }
                IndexMsg::CacheSaved(saved) => {
                    if !saved && !self.status_alert && self.index_count > 0 {
                        self.status = format!("{} 项 · 缓存写入失败", group_thousands(self.index_count));
                    }
                }
                IndexMsg::Delta {
                    gen,
                    delta_count,
                    total_count,
                    updated_hhmm,
                } => {
                    if gen != self.watch_gen.load(Ordering::SeqCst) {
                        continue;
                    }
                    self.index_count = total_count;
                    self.status = if delta_count > 0 {
                        format!(
                            "{} 项 · 增量 {} · {} 更新",
                            group_thousands(total_count),
                            group_thousands(delta_count),
                            updated_hhmm
                        )
                    } else {
                        format!("{} 项 · {} 更新", group_thousands(total_count), updated_hhmm)
                    };
                    self.schedule_search();
                }
                IndexMsg::NeedRebuild { gen } => {
                    if gen == self.watch_gen.load(Ordering::SeqCst) {
                        self.start_rebuild();
                    }
                }
            }
        }
        // 搜索结果：只取最新一条
        let mut latest: Option<SearchMsg> = None;
        while let Ok(msg) = self.search_rx.try_recv() {
            latest = Some(msg);
        }
        if let Some(SearchMsg::Done {
            rows,
            elapsed_ms,
            empty_query,
            error,
        }) = latest
        {
            if error {
                // 搜索线程 panic 过：保留现有结果不冻结，只提示已恢复；
                // 下一次输入会重新调度（锁可中毒恢复，不再全灭）。
                self.status = "搜索线程异常，已自动恢复".to_string();
            } else {
                self.rows = rows;
                self.fold_active = !empty_query;
                self.rebuild_display();
                self.selected = 0;
                // 结果集进场动画起点（v0.1.10）：draw_results 按它
                // 做整组淡入 + 上浮（见 results_born 字段注释）
                self.results_born = Some(ctx.input(|i| i.time));
                // 结果计数走页脚中央（原版 ResultText "{n} 项 · {ms} ms"）；
                // 左侧状态只管索引状态，不再被每次搜索覆盖。
                self.result_text = if empty_query {
                    String::new()
                } else {
                    format!("{} 项 · {elapsed_ms:.0} ms", self.rows.len())
                };
            }
        }
        let _ = ctx;
    }
}

// ---------------------------------------------------------------- UI

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
}

/// hover 缓动强度（0..1，150ms cubic_out）：结果行 / 宫格格子 /
/// 首页磁贴的 hover 填充共用——填充按强度缩放 alpha 淡入淡出，
/// 不再瞬时跳变（v0.1.10 内容层动画）。过渡期 16ms 续命重绘，
/// 到位即停。
fn hover_strength(ui: &egui::Ui, id: impl std::hash::Hash, hovered: bool) -> f32 {
    let v = ui.ctx().animate_bool_with_time_and_easing(
        ui.make_persistent_id(id),
        hovered,
        0.15,
        egui::emath::easing::cubic_out,
    );
    if v > 0.0 && v < 1.0 {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(16));
    }
    v
}

/// 按强度缩放颜色 alpha（hover / 淡入用）。
fn fade_alpha(c: Color32, t: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(
        c.r(),
        c.g(),
        c.b(),
        (c.a() as f32 * t.clamp(0.0, 1.0)) as u8,
    )
}

/// 光束彩虹带：青 #22D3EE → 蓝 #3B82F6 → 紫 #A855F7 → 粉 #EC4899，
/// s 是段内位置（0=束头，1=束尾），供光束游走时给每小段上色。
fn beam_color(s: f32) -> (u8, u8, u8) {
    const STOPS: [(f32, (u8, u8, u8)); 4] = [
        (0.0, (34, 211, 238)),
        (1.0 / 3.0, (59, 130, 246)),
        (2.0 / 3.0, (168, 85, 247)),
        (1.0, (236, 72, 153)),
    ];
    let s = s.clamp(0.0, 1.0);
    for w in STOPS.windows(2) {
        let (t0, c0) = w[0];
        let (t1, c1) = w[1];
        if s <= t1 {
            let k = if t1 > t0 { (s - t0) / (t1 - t0) } else { 0.0 };
            return (
                lerp_u8(c0.0, c1.0, k),
                lerp_u8(c0.1, c1.1, k),
                lerp_u8(c0.2, c1.2, k),
            );
        }
    }
    (236, 72, 153)
}

/// 往轮廓折线里加点（与上一点重合的跳过）。
fn push_pt(pts: &mut Vec<egui::Pos2>, p: egui::Pos2) {
    if pts.last().map(|l: &egui::Pos2| l.distance(p) > 0.01).unwrap_or(true) {
        pts.push(p);
    }
}

/// 圆角矩形的轮廓折线（顺时针，不闭合，首尾靠绘制时回绕相连）。
/// 分段密度按 v0.1.6 减负要求减半：直边每 ~12px 一个点、圆弧每
/// 15° 一个点（v0.1.5 是 6px / 7.5°），供流光描边按弧长重采样。
fn rounded_rect_outline(rect: egui::Rect, radius: f32) -> Vec<egui::Pos2> {
    let mut pts: Vec<egui::Pos2> = Vec::new();
    let r = radius.min(rect.width() / 2.0).min(rect.height() / 2.0);
    let edge_steps = |len: f32| ((len / 12.0).ceil().max(1.0)) as usize;
    let arc = |pts: &mut Vec<egui::Pos2>, cx: f32, cy: f32, a0: f32| {
        let mut a = a0;
        while a < a0 + 90.0 {
            let rad = a.to_radians();
            push_pt(pts, egui::pos2(cx + r * rad.cos(), cy + r * rad.sin()));
            a += 15.0;
        }
        let rad = (a0 + 90.0).to_radians();
        push_pt(pts, egui::pos2(cx + r * rad.cos(), cy + r * rad.sin()));
    };
    // 顶边（左→右）
    let n = edge_steps(rect.width() - r * 2.0);
    for i in 0..=n {
        push_pt(
            &mut pts,
            egui::pos2(
                rect.left() + r + (rect.width() - r * 2.0) * (i as f32 / n as f32),
                rect.top(),
            ),
        );
    }
    // 右上弧 -90°→0°
    arc(&mut pts, rect.right() - r, rect.top() + r, -90.0);
    // 右边（上→下）
    let n = edge_steps(rect.height() - r * 2.0);
    for i in 1..=n {
        push_pt(
            &mut pts,
            egui::pos2(
                rect.right(),
                rect.top() + r + (rect.height() - r * 2.0) * (i as f32 / n as f32),
            ),
        );
    }
    // 右下弧 0°→90°
    arc(&mut pts, rect.right() - r, rect.bottom() - r, 0.0);
    // 底边（右→左）
    let n = edge_steps(rect.width() - r * 2.0);
    for i in 1..=n {
        push_pt(
            &mut pts,
            egui::pos2(
                rect.right() - r - (rect.width() - r * 2.0) * (i as f32 / n as f32),
                rect.bottom(),
            ),
        );
    }
    // 左下弧 90°→180°
    arc(&mut pts, rect.left() + r, rect.bottom() - r, 90.0);
    // 左边（下→上）
    let n = edge_steps(rect.height() - r * 2.0);
    for i in 1..=n {
        push_pt(
            &mut pts,
            egui::pos2(
                rect.left(),
                rect.bottom() - r - (rect.height() - r * 2.0) * (i as f32 / n as f32),
            ),
        );
    }
    // 左上弧 180°→270°，终点即顶边起点
    arc(&mut pts, rect.left() + r, rect.top() + r, 180.0);
    pts
}

/// 【LeiBox 光束游走描边】（v0.1.7 按用户裁决加回）底圈是一整圈
/// 淡边（faint 参数传入，现行 = pal.edge_line，与搜索框 Frame 自带
/// 描边同色同位、重合不可见）；beam_phase 有值时，一束约 1/4 周长
/// 的光沿周界游走：束头最亮、向尾按距离渐隐到 0，颜色沿段内位置
/// 走青→蓝→紫→粉彩虹带；每小段先画一层线宽 4、alpha 约 1/3 的
/// 同色辉光，再画线宽 1.5 的亮芯。beam 子段数按减负要求减半：
/// SUB 20（v0.1.5 为 40），两遍共 40 段。beam_phase=None 时只留
/// 淡边，静态不动。
fn paint_flow_border(
    painter: &egui::Painter,
    rect: egui::Rect,
    radius: f32,
    beam_phase: Option<f32>,
    faint_color: Color32,
) {
    let pts = rounded_rect_outline(rect, radius);
    if pts.len() < 2 {
        return;
    }
    // 底圈淡边
    let faint = Stroke::new(1.0_f32, faint_color);
    let n = pts.len();
    for i in 0..n {
        painter.line_segment([pts[i], pts[(i + 1) % n]], faint);
    }
    let Some(phase) = beam_phase else {
        return;
    };
    // 累计弧长（含收尾回绕段）+ 按弧长取点
    let mut cum: Vec<f32> = Vec::with_capacity(n + 1);
    cum.push(0.0);
    for i in 0..n {
        let d = pts[i].distance(pts[(i + 1) % n]);
        cum.push(cum[i] + d);
    }
    let total = cum[n];
    if total <= 0.0 {
        return;
    }
    let point_at = |d: f32| -> egui::Pos2 {
        let d = d.rem_euclid(total);
        let seg = match cum.binary_search_by(|v| v.partial_cmp(&d).unwrap()) {
            Ok(i) => i.min(n - 1),
            Err(i) => i.saturating_sub(1).min(n - 1),
        };
        let seg_len = (cum[seg + 1] - cum[seg]).max(f32::EPSILON);
        let k = ((d - cum[seg]) / seg_len).clamp(0.0, 1.0);
        pts[seg].lerp(pts[(seg + 1) % n], k)
    };
    // 光束：束头在 phase 处，束身向后（逆行进方向）拖 1/4 周长
    const SUB: usize = 20;
    let beam_len = total * 0.25;
    let head = phase.rem_euclid(1.0) * total;
    // 先整束辉光（宽、淡），再整束亮芯（细、亮），保证芯压在辉光上
    for pass in 0..2 {
        for i in 0..SUB {
            let s = (i as f32 + 0.5) / SUB as f32; // 段内位置：0 束头 → 1 束尾
            let fade = 1.0 - s;
            let alpha = (255.0 * fade * fade) as u8;
            if alpha == 0 {
                continue;
            }
            let (r, g, b) = beam_color(s);
            let d0 = head - beam_len * (i as f32 / SUB as f32);
            let d1 = head - beam_len * ((i + 1) as f32 / SUB as f32);
            if pass == 0 {
                painter.line_segment(
                    [point_at(d0), point_at(d1)],
                    Stroke::new(4.0_f32, Color32::from_rgba_unmultiplied(r, g, b, alpha / 3)),
                );
            } else {
                painter.line_segment(
                    [point_at(d0), point_at(d1)],
                    Stroke::new(1.5_f32, Color32::from_rgba_unmultiplied(r, g, b, alpha)),
                );
            }
        }
    }
}

/// 竖向渐变背景：把圆角矩形轮廓 + 圆心做扇形三角化（epaint Mesh），
/// 每个顶点按自己的 y 在 top/bottom 之间插值上色，得到平滑渐变，
/// 且四角严格贴合圆角，不会像分段矩形那样在角上戳出去。
/// 圆角轮廓在高度 y 处的左右内缩量（圆方程），渐变/光晕 Mesh 共用。
fn rounded_inset_at(rect: egui::Rect, radius: f32, y: f32) -> f32 {
    let dy = (y - rect.top()).min(rect.bottom() - y);
    if dy >= radius {
        0.0
    } else {
        radius - (radius * radius - (radius - dy) * (radius - dy)).max(0.0).sqrt()
    }
}

/// 三段渐变插值（InkFill：0 → 0.55 → 1）。
fn grad_color(top: Color32, mid: Color32, bottom: Color32, t: f32) -> Color32 {
    let (a, b, k) = if t <= 0.55 {
        (top, mid, t / 0.55)
    } else {
        (mid, bottom, (t - 0.55) / 0.45)
    };
    Color32::from_rgb(
        lerp_u8(a.r(), b.r(), k),
        lerp_u8(a.g(), b.g(), k),
        lerp_u8(a.b(), b.b(), k),
    )
}

/// 渐变类 Mesh 的横向分带边界。分带是横向弦，弦与真圆弧之间
/// 没涂到的楔形会露出底层（v0.1.10 前的 clear 色是半透明黑灰，
/// 浅色背景上就是肉眼可见的黑角），所以圆角区必须细步长：
/// 距边 <3px 用 0.5px（圆弧在弧顶切线近垂直，弦偏差随步长平方
/// 衰减，0.5px 步长时偏差 <0.8px，AA 后不可见）、圆角区其余
/// 2px、内部粗分带。还有一条硬规则：**粗步不许一脚跨进底部
/// 圆角区**——必须在圆角区边界精确落脚再由细步接管。v0.1.9
/// 正是死在这里：540 高时从顶部走粗步（82px），最后一步从
/// y=516 直跳 540，最后一带 24px 高、弦从 inset 0 直拉到 12，
/// 右下/左下角各留一条约 6px 宽的斜楔（真机截图实测：白色
/// 轮廓在右下呈直线斜边、楔内像素是 clear 色合成值 (79,80,81)）。
/// 顶部没事只是因为步行从顶边出发、天然细步——相位依赖，
/// 换个窗口高度顶部也会翻车，所以规则对两头都成立（顶部靠
/// 出发点保证，底部靠落脚规则保证）。
fn gradient_band_ys(rect: egui::Rect, radius: f32) -> Vec<f32> {
    let top = rect.top();
    let bottom = rect.bottom();
    let h = (bottom - top).max(1.0);
    let corner = (radius * 2.0).min(h * 0.25);
    let coarse = ((h - corner * 2.0) / 6.0).max(8.0);
    let mut ys = vec![top];
    let mut y = top;
    while y < bottom - 0.01 {
        let d_top = y - top;
        let d_bot = bottom - y;
        let near = d_top.min(d_bot);
        // 弧顶细步区：顶部按带的起点判（d_top<3，步行从顶边出发
        // 天然对齐）；底部按带的**末端**判——一条 2px 带可以起点
        // 在 4、末端落在 2，末端已进弧顶陡段，所以 d_bot<5 就
        // 提前切 0.5 步长，保证任何末端距边 <3 的带都 ≤0.5。
        // 注意边界取等号：恰好站在圆角区边界上（near == corner）
        // 也必须走细步——h=400 时粗步恰好落在边界点，下一脚若
        // 还按粗步走就会 24px 直穿整个圆角区（单测实测抓到）。
        let mut step = if d_top < 3.0 || d_bot < 5.0 {
            0.5
        } else if near <= corner {
            2.0
        } else {
            coarse
        };
        // 粗步落脚规则：下一步会跨进底部圆角区时，先精确落在
        // 圆角区边界上（d_bot == corner 处），细步从那里接管。
        if d_bot > corner && d_bot - step <= corner {
            step = d_bot - corner;
        }
        y = (y + step).min(bottom);
        ys.push(y);
    }
    ys
}

/// 竖向渐变背景（v0.1.6 改回单个 Mesh）：横向分带拼成一张
/// 三角网（**1 个 Shape**），替代旧版每帧 100 条 rect_filled
/// 色带。旧版弃 Mesh 是因为「顶点色被压暗」的观察——后来证实
/// 灰幕真凶是 Frame 阴影（已删），Mesh 本身无罪。
/// 顶点色按原版 InkFill 的对角方向 (0,0)→(0.45,1) 取 t：
/// t = (0.45·nx + ny) / 1.45，左右顶点 t 不同即成对角渐变；
/// 分带边界按圆角轮廓收边（步长见 gradient_band_ys），四角
/// 不缺角不出界。
fn paint_vertical_gradient(
    painter: &egui::Painter,
    rect: egui::Rect,
    radius: f32,
    top: Color32,
    mid: Color32,
    bottom: Color32,
) {
    let ys = gradient_band_ys(rect, radius);
    let mut mesh = egui::Mesh::default();
    for &y in &ys {
        let inset = rounded_inset_at(rect, radius, y);
        let ny = (y - rect.top()) / rect.height().max(1.0);
        for (x, nx) in [
            (rect.left() + inset, 0.0f32),
            (rect.right() - inset, 1.0f32),
        ] {
            let t = ((0.45 * nx + ny) / 1.45).clamp(0.0, 1.0);
            mesh.colored_vertex(
                egui::pos2(x, y),
                grad_color(top, mid, bottom, t),
            );
        }
    }
    for i in 0..ys.len() - 1 {
        let a = (i * 2) as u32;
        mesh.indices
            .extend_from_slice(&[a, a + 1, a + 2, a + 1, a + 3, a + 2]);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// 顶部光晕（原版 TopGlow）：窗口顶部 220px 的白色渐隐层，方向
/// (0,0)→(0.2,1) 近似为竖向；峰值 alpha 由 palette 给（已折入原版
/// 层 Opacity 0.5）。v0.1.6 起与渐变同法：单个 Mesh，替代旧版
/// 18×4=72 个小矩形切片；分带与背景渐变共用 gradient_band_ys
/// （顶部圆角区 2px 细步长，弦不许离开圆弧，v0.1.9）。
fn paint_top_glow(painter: &egui::Painter, rect: egui::Rect, radius: f32, peak: u8) {
    const GLOW_H: f32 = 220.0;
    let h = GLOW_H.min(rect.height());
    let glow_rect =
        egui::Rect::from_min_size(rect.min, Vec2::new(rect.width(), h));
    let ys = gradient_band_ys(glow_rect, radius);
    let mut mesh = egui::Mesh::default();
    for &y in &ys {
        // 只在圆角区（顶部 radius 范围内）需要收边
        let inset = if y - rect.top() < radius {
            rounded_inset_at(rect, radius, y)
        } else {
            0.0
        };
        let t = (y - rect.top()) / h.max(1.0);
        let alpha = (peak as f32 * (1.0 - t)) as u8;
        let color = Color32::from_white_alpha(alpha);
        mesh.colored_vertex(egui::pos2(rect.left() + inset, y), color);
        mesh.colored_vertex(egui::pos2(rect.right() - inset, y), color);
    }
    for i in 0..ys.len() - 1 {
        let a = (i * 2) as u32;
        mesh.indices
            .extend_from_slice(&[a, a + 1, a + 2, a + 1, a + 3, a + 2]);
    }
    painter.add(egui::Shape::mesh(mesh));
}
/// 菜单最小宽（v0.1.9 用户定的宽松尺度：宁可宽松，不许再出现
/// 两行字贴脸挤在一个小灰盒里的样子；所有自绘弹出菜单共用）。
const MENU_MIN_W: f32 = 200.0;
/// 菜单条目行高。
const MENU_ITEM_H: f32 = 34.0;
/// 菜单条目之间的气口。
const MENU_ITEM_GAP: f32 = 2.0;
/// 菜单内边距（Frame inner margin，四边同值）。
const MENU_PAD: f32 = 8.0;

/// 应用菜单的 Frame（原版右键菜单令牌）：MenuFill 底 +
/// MenuStroke 描边 + 圆角 10 + 柔和投影（与筛选面板同一套
/// popup 参数）+ 内边距 8。首页添加菜单、自绘右键菜单共用，
/// 不再出现 egui 默认灰盒子。
fn menu_frame(pal: Palette) -> egui::Frame {
    egui::Frame::none()
        .fill(pal.popup_fill)
        .stroke(Stroke::new(1.0_f32, pal.popup_stroke))
        .rounding(Rounding::same(10.0))
        .shadow(egui::epaint::Shadow {
            offset: Vec2::new(0.0, 5.0),
            blur: 22.0,
            spread: 0.0,
            color: pal.popup_shadow_color,
        })
        .inner_margin(egui::Margin::same(MENU_PAD))
}

/// 菜单条目（整行可点）：高 34、Sans 13、文字左内边距 12
/// （即原版条目内边距 12,8 的横向值，纵向由行高居中实现），
/// hover 整行 MenuHover 填充（chip_hover_fill)、圆角 6。
/// enabled=false 时只画灰字不响应（非 Windows 的占位项）。
/// 返回本帧是否被点击。
fn menu_item(ui: &mut egui::Ui, pal: Palette, label: &str, enabled: bool) -> bool {
    // 宽取「可用宽与菜单内容宽的较小者」：菜单里就是 184
    // （200 - 两侧内边距 8×2）；放进更宽的面板（如范围面板）
    // 也保持菜单条目宽度，不拉满。
    let w = ui.available_width().min(MENU_MIN_W - MENU_PAD * 2.0);
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(w, MENU_ITEM_H),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let painter = ui.painter_at(rect);
    if enabled && resp.hovered() {
        painter.rect_filled(rect, Rounding::same(6.0), pal.chip_hover_fill);
    }
    let color = if enabled { pal.name_text } else { pal.path_text };
    painter.text(
        egui::pos2(rect.left() + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.0),
        color,
    );
    enabled && resp.clicked()
}

/// 菜单分隔线：上下各 4px 气口的发丝线，左右内缩 12 与条目
/// 文字对齐。整段占高 9（4 + 1 + 4），与 menu_height 口径一致。
fn menu_separator(ui: &mut egui::Ui, pal: Palette) {
    ui.add_space(4.0);
    let w = ui.available_width().min(MENU_MIN_W - MENU_PAD * 2.0);
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(w, 1.0), egui::Sense::hover());
    ui.painter_at(rect).rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(rect.left() + 12.0, rect.top()),
            egui::pos2(rect.right() - 12.0, rect.bottom()),
        ),
        Rounding::ZERO,
        pal.hairline,
    );
    ui.add_space(4.0);
}

/// 菜单总高估算（定位钳制用）：n 个条目 + s 条分隔线；条目间
/// 气口按 n-1 个计（分隔线所在接缝的气口含在它的 9 里，估算
/// 略偏大 2px，方向安全——宁可早一点改向上弹）。
fn menu_height(n_items: usize, n_seps: usize) -> f32 {
    MENU_PAD * 2.0
        + n_items as f32 * MENU_ITEM_H
        + n_items.saturating_sub(1) as f32 * MENU_ITEM_GAP
        + n_seps as f32 * 9.0
}

/// 把菜单左上角钳制进窗口：右/下溢出时贴边收回（留 6px 边距），
/// 左/上同样不许出界。
/// 拉伸命中区（纯函数，单测覆盖）：只认窗口最外 8px 边框带
/// 与 12px 角方，角优先于边。内容区从 12px 内边距起，任何控件
/// 都不会落进带内——命中判定与控件点击天然不冲突（与 v0.1.9
/// window_chrome 同一张分区表，但那边靠交互控件抢命中、这边
/// 只做坐标纯判定）。
fn resize_zone_at(
    screen: egui::Rect,
    p: egui::Pos2,
) -> Option<(egui::viewport::ResizeDirection, egui::CursorIcon)> {
    use egui::viewport::ResizeDirection as D;
    use egui::CursorIcon as C;
    // 拉伸带（v0.1.14 收窄）：旧版直边 8px / 角 12px，而无边框窗口
    // 的视觉描边正好压在这圈里——用户按「边框」想拖窗口，实际触发
    // 的是缩放，表现为「拖不动」。直边收到 4px、角 10px，把描边
    // 内侧让给拖动，缩放仍可从最外沿摸到。
    const EDGE: f32 = 4.0;
    const CORNER: f32 = 10.0;
    if !screen.contains(p) {
        return None;
    }
    let west = p.x - screen.left() < CORNER;
    let east = screen.right() - p.x < CORNER;
    let north = p.y - screen.top() < CORNER;
    let south = screen.bottom() - p.y < CORNER;
    let corner = match (north, south, west, east) {
        (true, _, true, _) => Some((D::NorthWest, C::ResizeNorthWest)),
        (true, _, _, true) => Some((D::NorthEast, C::ResizeNorthEast)),
        (_, true, true, _) => Some((D::SouthWest, C::ResizeSouthWest)),
        (_, true, _, true) => Some((D::SouthEast, C::ResizeSouthEast)),
        _ => None,
    };
    if corner.is_some() {
        return corner;
    }
    if p.y - screen.top() < EDGE {
        Some((D::North, C::ResizeVertical))
    } else if screen.bottom() - p.y < EDGE {
        Some((D::South, C::ResizeVertical))
    } else if p.x - screen.left() < EDGE {
        Some((D::West, C::ResizeHorizontal))
    } else if screen.right() - p.x < EDGE {
        Some((D::East, C::ResizeHorizontal))
    } else {
        None
    }
}

fn clamp_menu_pos(pos: egui::Pos2, size: Vec2, screen: egui::Rect) -> egui::Pos2 {
    let x = pos
        .x
        .min(screen.right() - size.x - 6.0)
        .max(screen.left() + 6.0);
    let y = pos
        .y
        .min(screen.bottom() - size.y - 6.0)
        .max(screen.top() + 6.0);
    egui::pos2(x, y)
}

/// 类型控件展开后的迷你 chip 顺序：全部 + 12 类（安装目录在旧
/// chips 栏里被塞进「更多」，这里一并摆出，免得该类没法单独筛选）。
const TYPE_CHIPS: &[Option<Category>] = &[
    None,
    Some(Category::App),
    Some(Category::InstallDir),
    Some(Category::Folder),
    Some(Category::Document),
    Some(Category::Image),
    Some(Category::Audio),
    Some(Category::Video),
    Some(Category::Archive),
    Some(Category::Code),
    Some(Category::Shortcut),
    Some(Category::Executable),
    Some(Category::Other),
];

/// 范围选中集切换（与类型同构、永不空选）：presets + drives 合并
/// 看，取消到最后一项时跳回全选；solo = 只留该项（已 solo 则回全选）。
fn scope_toggle_preset(
    presets: &mut HashSet<String>,
    drives: &mut HashSet<char>,
    key: &str,
    solo: bool,
) {
    if solo {
        if presets.len() == 1 && drives.is_empty() && presets.contains(key) {
            scope_select_all(presets, drives);
        } else {
            presets.clear();
            drives.clear();
            presets.insert(key.to_string());
        }
        return;
    }
    if presets.len() + drives.len() == 1 && presets.contains(key) {
        scope_select_all(presets, drives);
    } else if !presets.remove(key) {
        presets.insert(key.to_string());
    }
}

fn scope_toggle_drive(
    presets: &mut HashSet<String>,
    drives: &mut HashSet<char>,
    d: char,
    solo: bool,
) {
    if solo {
        if drives.len() == 1 && presets.is_empty() && drives.contains(&d) {
            scope_select_all(presets, drives);
        } else {
            presets.clear();
            drives.clear();
            drives.insert(d);
        }
        return;
    }
    if presets.len() + drives.len() == 1 && drives.contains(&d) {
        scope_select_all(presets, drives);
    } else if !drives.remove(&d) {
        drives.insert(d);
    }
}

fn scope_select_all(presets: &mut HashSet<String>, drives: &mut HashSet<char>) {
    *presets = PRESET_KEYS.iter().map(|(k, _, _)| k.to_string()).collect();
    *drives = existing_drives().into_iter().collect();
}

enum RowAction {
    /// 选中一个展示行（下标是 display 下标）。
    Select(usize),
    /// 打开一个展示行对应的结果（下标是 display 下标）。
    Open(usize),
    /// 打开结果所在文件夹（右键菜单 / Ctrl+Enter 路径）。
    Locate(usize),
    /// 展开/收起一个折叠组（display 下标 + 父目录路径）。
    Toggle(usize, String),
}

/// 首页磁贴动作（下标是 home_items 下标）。「从首页移除」走
/// 自绘右键菜单（draw_ctx_menu）直接执行，不经此枚举。
enum HomeAction {
    Select(usize),
    Open(usize),
    Locate(usize),
}

impl ShunSeekApp {
    /// 自绘右键菜单（v0.1.9）：结果行 / 宫格格子 / 首页磁贴
    /// 共用，语言与尺度同 menu_frame / menu_item（宽 200、
    /// 行高 34、条目内边距 12,8、字 13、菜单内边距 8、气口
    /// 2），替代 egui 默认 context_menu 的紧凑灰菜单。由
    /// update 在主面板之后调用：位置按右键点钳制进窗口，
    /// 点外关闭（Esc 在 handle_keys 里拦截），动作执行后即关。
    fn draw_ctx_menu(&mut self, ctx: &egui::Context) {
        let Some(menu) = self.ctx_menu else {
            return;
        };
        let pal = self.display_pal;
        let screen = ctx.screen_rect();
        let labels: [&str; 3] = match menu.target {
            CtxTarget::Result(_) => ["打开", "打开所在文件夹", "添加到首页"],
            CtxTarget::Home(_) => ["打开", "打开所在文件夹", "从首页移除"],
        };
        let size = Vec2::new(MENU_MIN_W, menu_height(3, 1));
        let pos = clamp_menu_pos(menu.pos, size, screen);
        // 进场动画（v0.1.10 内容层动画）：180ms 高度揭示 + 淡入
        // （cubic_out）；关闭保持即时（菜单语义）。菜单项从第一
        // 帧起就以完整尺寸布局在裁剪区内，动画不挡点击。
        let age = ctx.input(|i| i.time) - self.ctx_menu_born;
        let mv = egui::emath::easing::cubic_out((age / 0.18).clamp(0.0, 1.0) as f32);
        if mv < 1.0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        let menu_clip = egui::Rect::from_min_max(
            egui::pos2(pos.x - 8.0, pos.y - 4.0),
            egui::pos2(pos.x + size.x + 8.0, pos.y + size.y * mv + 26.0 * mv),
        );
        let mut action: Option<&'static str> = None;
        let area = egui::Area::new(egui::Id::new("ss_ctx_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                ui.set_clip_rect(menu_clip);
                ui.multiply_opacity(0.3 + 0.7 * mv);
                menu_frame(pal).show(ui, |ui| {
                    if menu_item(ui, pal, labels[0], true) {
                        action = Some(labels[0]);
                    }
                    ui.add_space(MENU_ITEM_GAP);
                    if menu_item(ui, pal, labels[1], true) {
                        action = Some(labels[1]);
                    }
                    menu_separator(ui, pal);
                    if menu_item(ui, pal, labels[2], true) {
                        action = Some(labels[2]);
                    }
                })
            });
        // 菜单本体是拖动排除区（按下不许变窗口拖动）
        self.blockers_cur.push(area.response.rect);
        if action.is_none() {
            // 点外关闭：本帧 primary 按下点落在菜单之外
            let press = ctx.input(|i| {
                if i.pointer.primary_pressed() {
                    i.pointer
                        .press_origin()
                        .or_else(|| i.pointer.interact_pos())
                } else {
                    None
                }
            });
            if let Some(p) = press {
                if !area.response.rect.contains(p) {
                    self.ctx_menu = None;
                    return;
                }
            }
        }
        let Some(act) = action else {
            return;
        };
        self.ctx_menu = None;
        match (menu.target, act) {
            (CtxTarget::Result(idx), "打开") => {
                self.open_display_row(ctx, idx, false)
            }
            (CtxTarget::Result(idx), "打开所在文件夹") => {
                self.selected = idx;
                self.open_display_row(ctx, idx, true)
            }
            (CtxTarget::Result(idx), "添加到首页") => {
                if let Some(DisplayRow::Single { row, .. }) =
                    self.display.get(idx).cloned()
                {
                    if let Some(r) = self.rows.get(row) {
                        let p = r.path.clone();
                        self.add_pin(&p);
                    }
                }
            }
            (CtxTarget::Home(idx), "打开") => self.open_home_item(ctx, idx, false),
            (CtxTarget::Home(idx), "打开所在文件夹") => {
                self.open_home_item(ctx, idx, true)
            }
            (CtxTarget::Home(idx), "从首页移除") => {
                if let Some(item) = self.home_items.get(idx).cloned() {
                    self.remove_pin(&item.path);
                }
            }
            _ => {}
        }
    }

    /// 搜索框（原版 SearchField）：Surface 填充 + EdgeLine 描边的
    /// 圆角 12 面板；左放大镜 16 号 TextLow；输入 Serif 17
    /// TextHi、占位 Serif 16 TextLow；聚焦时左侧 2px 朱砂焦点条
    /// （淡入 ~150ms）；右侧「Enter」键帽（ChipFill + EdgeLine 边，
    /// Sans 9.5 TextLow）。v0.1.6 起 LeiBox 光束游走描边退役（视觉
    /// 照抄原版），这里唯一的动画是焦点条淡入：过渡期间用
    /// repaint_after(16ms) 续命，静止后一帧不多画。
    /// 书眉：左 = 拖动区（标题「瞬觅」Serif 20 TextHi + 1×14
    /// EdgeStrong 竖线 + 副标题「SHUNSEEK · 本机索引」Sans 9.5
    /// TextLow）；右 = 工具按钮组（主题 / 最小化 / 最大化-还原 /
    /// 关闭，32×28）。高度口径 v0.1.10 经用户明确覆盖原版令牌
    /// （书眉行区 46→36、搜索框 54→44）：标题区只占 28，省出的
    /// 高度全给内容区。拖动不再由本区控件负责——标题区只登记
    /// Sense::hover，拖动/双击最大化由 handle_window_press 手判
    /// （标题区不进 blockers、其余按钮全进，见该函数）。✕ = 隐藏
    /// 到托盘（v0.1.6 用户钉死：无条件，进程只从托盘菜单退出）。
    fn draw_header(&mut self, ui: &mut egui::Ui) {
        let pal = self.display_pal;
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            // ---- 左：标题拖动区（仅 hover，不登记交互） ----
            let title_g = ui.fonts(|f| {
                f.layout_no_wrap("瞬觅".to_string(), serif(20.0), pal.name_text)
            });
            let cap_g = ui.fonts(|f| {
                f.layout_no_wrap(
                    "SHUNSEEK · 本机索引".to_string(),
                    FontId::proportional(9.5),
                    pal.path_text,
                )
            });
            let title_w = title_g.size().x;
            let drag_w = title_w + 14.0 + 1.0 + 14.0 + cap_g.size().x + 2.0;
            let (drag_rect, _) =
                ui.allocate_exact_size(Vec2::new(drag_w, 28.0), egui::Sense::hover());
            let painter = ui.painter_at(drag_rect);
            let cy = drag_rect.center().y;
            painter.galley(
                egui::pos2(drag_rect.left(), cy - title_g.size().y / 2.0),
                title_g,
                pal.name_text,
            );
            let div_x = drag_rect.left() + title_w + 14.0;
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(div_x, cy - 7.0), Vec2::new(1.0, 14.0)),
                Rounding::ZERO,
                pal.edge_strong,
            );
            painter.galley(
                egui::pos2(div_x + 15.0, cy - cap_g.size().y / 2.0),
                cap_g,
                pal.path_text,
            );

            // ---- 右：工具按钮组（从右往左放：关闭/最大化/最小化/主题） ----
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let close_resp = header_tool_button(ui, pal, WinGlyph::Close);
                self.blockers_cur.push(close_resp.rect);
                if close_resp
                    .on_hover_text("关闭（最小化到托盘）")
                    .clicked()
                {
                    self.hide(&ctx);
                }
                ui.add_space(6.0);
                let max_glyph = if self.maximized {
                    WinGlyph::Restore
                } else {
                    WinGlyph::Max
                };
                let max_resp = header_tool_button(ui, pal, max_glyph);
                self.blockers_cur.push(max_resp.rect);
                if max_resp
                    .on_hover_text(if self.maximized { "还原" } else { "最大化" })
                    .clicked()
                {
                    self.maximized = !self.maximized;
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(self.maximized));
                }
                ui.add_space(2.0);
                let min_resp = header_tool_button(ui, pal, WinGlyph::Min);
                self.blockers_cur.push(min_resp.rect);
                if min_resp.on_hover_text("最小化").clicked() {
                    // v0.1.12：最小化也走 ShowWindow（SW_MINIMIZE，
                    // 本线程当场执行），不再发 ViewportCommand::
                    // Minimized。影子先行置位只是缓存预热——托盘/
                    // 热键的决策现读 OS（IsIconic），不依赖它；
                    // 每帧 sync_shadow_from_os 也会再对齐一次。
                    // 非 Windows 以 ViewportCommand 兜底（演示口径）。
                    winvis::minimize();
                    #[cfg(not(windows))]
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                    self.minimized = true;
                }
                ui.add_space(2.0);
                // 主题切换（原版书眉的主题 ToolButton）：字形 = 将去的
                // 主题（深色时画太阳 = 去浅色），13px 级手绘。
                let (trect, tresp) =
                    ui.allocate_exact_size(Vec2::new(32.0, 28.0), egui::Sense::click());
                self.blockers_cur.push(trect);
                let tp = ui.painter_at(trect);
                if tresp.is_pointer_button_down_on() {
                    tp.rect_filled(trect, Rounding::same(8.0), pal.surface_sel);
                } else if tresp.hovered() {
                    tp.rect_filled(trect, Rounding::same(8.0), pal.surface_hover);
                }
                let tfg = if tresp.hovered() {
                    pal.name_text
                } else {
                    pal.text_mid
                };
                match self.theme {
                    Theme::Dark => paint_sun(&tp, trect.center(), 15.0, tfg),
                    Theme::Light => {
                        paint_moon(&tp, trect.center(), 15.0, tfg, pal.panel_fill)
                    }
                }
                if tresp
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(match self.theme {
                        Theme::Dark => "切换到浅色",
                        Theme::Light => "切换到深色",
                    })
                    .clicked()
                {
                    let next = self.theme.toggled();
                    self.set_theme(&ctx, next);
                }
            });
        });
    }

    fn draw_search_box(&mut self, ui: &mut egui::Ui) {
        let pal = self.display_pal;
        let frame = egui::Frame::none()
            .fill(pal.search_fill)
            .rounding(Rounding::same(12.0))
            .stroke(Stroke::new(1.0_f32, pal.edge_line))
            .inner_margin(egui::Margin {
                left: 17.0,
                right: 14.0,
                top: 10.0,
                bottom: 10.0,
            });
        let frame_resp = frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                // 放大镜：手绘圆 + 斜线，TextLow（原版不随聚焦变色）。
                // 框高 54→44（v0.1.10 布局收紧）后随框等比收：圆 r 4.4。
                let (icon_rect, _) =
                    ui.allocate_exact_size(Vec2::new(14.0, 24.0), egui::Sense::hover());
                let painter = ui.painter_at(icon_rect);
                let c = icon_rect.center() + Vec2::new(-1.0, -1.0);
                let st = Stroke::new(1.6_f32, pal.search_icon);
                painter.circle_stroke(c, 4.4, st);
                painter.line_segment(
                    [c + Vec2::new(3.2, 3.2), c + Vec2::new(6.4, 6.4)],
                    st,
                );
                ui.add_space(11.0);
                let hint_text = RichText::new("搜索文件、应用、文件夹…")
                    .color(pal.hint.unwrap_or(pal.path_text))
                    .size(16.0);
                // 输入框宽度必须有界：右侧还要放 Enter 键帽，给
                // INFINITY 会把键帽顶出框外被窗口切掉。
                let edit = egui::TextEdit::singleline(&mut self.query)
                    .desired_width((ui.available_width() - 64.0).max(60.0))
                    .hint_text(hint_text)
                    .text_color(pal.search_text)
                    .frame(false)
                    .font(serif(17.0));
                let out = edit.show(ui);
                self.search_focused = out.response.has_focus();
                self.search_edit_id = Some(out.response.id);
                if self.focus_search {
                    out.response.request_focus();
                    self.focus_search = false;
                    // 呼出全选（show() 置位）：把选区设为全文，
                    // 下一帧 TextEdit 按此状态渲染
                    if self.select_all_search {
                        self.select_all_search = false;
                        if let Some(mut state) =
                            egui::TextEdit::load_state(ui.ctx(), out.response.id)
                        {
                            let len = self.query.chars().count();
                            state.cursor.set_char_range(Some(
                                egui::text_selection::CCursorRange::two(
                                    egui::text::CCursor::new(0),
                                    egui::text::CCursor::new(len),
                                ),
                            ));
                            egui::TextEdit::store_state(
                                ui.ctx(),
                                out.response.id,
                                state,
                            );
                            ui.ctx().request_repaint();
                        }
                    }
                }
                if out.response.changed() {
                    self.schedule_search();
                }
                // 右侧「Enter」键帽（纯提示，与原版一致；随框高
                // 收紧内边距 7,3 → 6,2）
                egui::Frame::none()
                    .fill(pal.esc_fill)
                    .rounding(Rounding::same(5.0))
                    .stroke(Stroke::new(1.0_f32, pal.edge_line))
                    .inner_margin(egui::Margin::symmetric(6.0, 2.0))
                    .show(ui, |ui| {
                        ui.label(RichText::new("Enter").size(9.5).color(pal.esc_text));
                    });
            });
        });
        // 焦点标记条：2px 朱砂竖条，左内缩 1、上下内缩 8，圆角 1；
        // 聚焦淡入 / 失焦淡出（animate 0.18s 近似原版 150/200ms）。
        let rect = frame_resp.response.rect;
        // 拖动排除区（handle_window_press 的 blockers）：在搜索框
        // 里按下拖动留给文本选择，不触发窗口拖动
        self.blockers_cur.push(rect);
        let v = ui.ctx().animate_bool_with_time(
            ui.make_persistent_id("search_focus_bar"),
            self.search_focused,
            0.18,
        );
        if v > 0.0 && v < 1.0 {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(16));
        }
        if v > 0.01 {
            let bar = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 1.0, rect.top() + 8.0),
                egui::pos2(rect.left() + 3.0, rect.bottom() - 8.0),
            );
            ui.painter().rect_filled(
                bar,
                Rounding::same(1.0),
                Color32::from_rgba_unmultiplied(
                    pal.accent.r(),
                    pal.accent.g(),
                    pal.accent.b(),
                    (255.0 * v) as u8,
                ),
            );
        }
        // 流光描边（v0.1.7 按用户裁决加回）：画在 Frame 边框之上
        // （与 v0.1.5 同层序），与框内左侧的朱砂焦点条互不冲突。
        // 聚焦或悬停且窗口可见时游走（相位 0.14/s ≈ 7 秒一圈），
        // 静止时只留静态淡边（pal.edge_line，与 Frame 描边同色）。
        // 续命用 repaint_after(16ms)，动画停下自动睡死，不许无条件
        // request_repaint（v0.1.6 帧率纪律）。
        let hovered = frame_resp.response.hovered();
        let animated = (self.search_focused || hovered) && self.visible;
        let beam_phase = if animated {
            Some((ui.ctx().input(|i| i.time) * 0.14).rem_euclid(1.0) as f32)
        } else {
            None
        };
        if animated {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(16));
        }
        paint_flow_border(&ui.painter_at(rect), rect, 12.0, beam_phase, pal.edge_line);
    }
    /// 工具栏一行（v0.1.5 定稿）：[类型控件] [范围控件] ……右端
    /// [宫格/列表切换]。原来的整排分类 chips 玻璃栏撤掉，腾出的
    /// 高度归结果区。两个筛选控件独立、互斥展开（expanded_filter
    /// 单字段，同时只许一个展开）。
    fn draw_toolbar(&mut self, ui: &mut egui::Ui) {
        let mut type_dot = None;
        let mut scope_dot = None;
        ui.horizontal(|ui| {
            type_dot = Some(self.draw_type_dot(ui));
            ui.add_space(8.0);
            scope_dot = Some(self.draw_scope_dot(ui));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // 视图切换是主页面主控件：一眼可见、点即切换、切换即
                // 持久化（与设置「启动时默认显示」共用同一状态/键）。
                // 文案 = 点后会变成的形态，图标 = 当前形态。
                let (label, grid_now) = match self.view_mode {
                    ViewMode::List => ("宫格", false),
                    ViewMode::Grid => ("列表", true),
                };
                let resp = view_toggle_button(ui, self.display_pal, label, grid_now);
                self.blockers_cur.push(resp.rect);
                if resp.clicked() {
                    self.set_view_mode(self.view_mode.toggled());
                }
            });
        });
        // 筛选面板是覆盖层（锚在各自圆点下方），在工具栏行之后
        // 绘制、层级压在内容区之上。展开方向按工具栏位置钉死
        // （v0.1.10 用户规格）：左边的类型 = 竖向长高，右边的
        // 范围 = 横向长开。
        if let Some(r) = type_dot {
            self.draw_type_panel(ui, r);
        }
        if let Some(r) = scope_dot {
            self.draw_scope_panel(ui, r);
        }
    }

    /// 类型筛选圆点（工具栏左侧）：26px 圆点（漏斗图形；恰选中
    /// 一类时圆点底色换该分类色、漏斗保持白色；多选时强调描边 +
    /// 数量角标；全选时纯玻璃）。点击展开/收起类型面板（竖向
    /// 展开，见 draw_type_panel）。选中语义是「全集模型」：全部 =
    /// 真实选项、永不空选，见 toggle_cat。
    fn draw_type_dot(&mut self, ui: &mut egui::Ui) -> egui::Rect {
        let pal = self.display_pal;
        let expanded = self.expanded_filter == Some(FilterKind::Type);
        let open = expanded || self.filter_anim_t[0] > 0.0;
        let full = self.cat_set.len() >= ALL_CATEGORIES.len();
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(30.0), egui::Sense::click());
        self.blockers_cur.push(rect);
        if expanded {
            self.filter_dot_rect = Some(rect);
        }
        let painter = ui.painter_at(rect);
        // v0.1.15：26px 圆点 → 30px 圆角方按钮（圆角 9）。常态无底，
        // hover 才浮出 SurfaceHover，展开时换 SurfaceSel + 发丝描边；
        // 不再画「灰圆 + 朱砂环」，那圈红环正是最跳的廉价感来源。
        let rr = Rounding::same(9.0);
        // 展开态只给一层浅底，不再描边——30px 方框 + 描边 + 细线
        // 图标三层叠在一起，远看就是一个空方框。
        if open {
            painter.rect_filled(rect, rr, pal.chip_hover_fill);
        } else if resp.hovered() {
            painter.rect_filled(rect, rr, pal.surface_hover);
        }
        // 图标恒朱砂（与范围按钮同一语言），全类型时右下角不加角标。
        let icon_col = pal.accent;
        paint_filter_lines(&painter, rect.center(), 18.0, icon_col);
        // 单选：右下角一枚 3px 分类色点，保留类型识别又不糊满整块
        if self.cat_set.len() == 1 && !open {
            if let Some(c) = self.cat_set.iter().next() {
                let (r, g, b) = c.tile_color();
                painter.circle_filled(
                    egui::pos2(rect.right() - 7.0, rect.bottom() - 7.0),
                    3.0,
                    Color32::from_rgb(r, g, b),
                );
            }
        }
        // 多选数量角标
        if !full && self.cat_set.len() > 1 {
            let badge = egui::pos2(rect.right() - 3.0, rect.top() + 3.0);
            painter.circle_filled(badge, 6.5, pal.accent);
            painter.text(
                badge,
                egui::Align2::CENTER_CENTER,
                self.cat_set.len().to_string(),
                FontId::proportional(9.0),
                Color32::WHITE,
            );
        }
        if resp.clicked() {
            self.expanded_filter = if expanded {
                None
            } else {
                Some(FilterKind::Type)
            };
        }
        let tip = if full {
            "类型筛选：全部".to_string()
        } else {
            let names: Vec<&str> = ALL_CATEGORIES
                .iter()
                .filter(|c| self.cat_set.contains(c))
                .map(|c| c.display_name())
                .collect();
            format!("类型筛选：{}", names.join("、"))
        };
        resp.on_hover_text(tip);
        rect
    }

    /// 类型面板（竖向展开）：覆盖层锚在圆点下方，宽 216 固定，
    /// 内容以完整尺寸布局、靠裁剪高度 = 全高 × ease 进度揭示——
    /// 面板像从圆点下方平滑长出来（0.24s cubic_out）；淡入同步
    /// （0.3→1）。收起同理回缩，动画途中点外/再点圆点/Esc 都能
    /// 关（expanded_filter 一翻，进度立即反向）。chips 是 30 高
    /// 全圆角胶囊（filter_chip），纵向流式换行排布。
    /// 全集归一化（渲染层）：cat_set 满员时只点亮「全部」，
    /// 其余 chip 一律未选态——不存在「每一颗都亮」的全选长相。
    fn draw_type_panel(&mut self, ui: &mut egui::Ui, dot_rect: egui::Rect) {
        let t = self.filter_anim_t[0];
        if t <= 0.001 {
            return;
        }
        let pal = self.display_pal;
        let v = egui::emath::easing::cubic_out(t);
        let full = self.cat_set.len() >= ALL_CATEGORIES.len();
        let width = 240.0;
        let full_size = self.filter_panel_full[0];
        let screen = ui.ctx().screen_rect();
        let pos = clamp_menu_pos(
            egui::pos2(dot_rect.left(), dot_rect.bottom() + 6.0),
            Vec2::new(width, full_size.y),
            screen,
        );
        // 裁剪区：面板本体按动画高度揭示，下方多留一段给投影
        // （投影随 v 淡入，不许在展开中途被裁出一道直边）
        let clip = egui::Rect::from_min_max(
            egui::pos2(pos.x - 8.0, pos.y),
            egui::pos2(pos.x + width + 8.0, pos.y + full_size.y * v + 28.0 * v),
        );
        let area = egui::Area::new(egui::Id::new("ss_type_panel"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                ui.set_clip_rect(clip);
                ui.multiply_opacity(0.3 + 0.7 * v);
                panel_frame(pal).show(ui, |ui| {
                    ui.set_width(width - 32.0);
                    // 头部：标题 + 当前选中摘要（右侧小字）
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("按类型筛选")
                                .size(12.0)
                                .color(pal.name_text),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                let note = if full {
                                    "全部".to_string()
                                } else {
                                    format!("已选 {} 项", self.cat_set.len())
                                };
                                ui.label(
                                    RichText::new(note)
                                        .size(10.5)
                                        .color(pal.path_text),
                                );
                            },
                        );
                    });
                    ui.add_space(10.0);
                    // v0.1.15：胶囊流 → 2 列卡片网格。每格 100×34，
                    // 左 3px 分类色点 + 左对齐 11.5px 文字；选中 =
                    // 朱砂淡底 + 左缘 2px 朱砂条 + 朱砂字。14 项正好
                    // 7 行，两列定宽比原来一排大小不一的胶囊安定得多。
                    let cell_w = 100.0;
                    let mut i = 0usize;
                    while i < TYPE_CHIPS.len() {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                            for k in 0..2 {
                                if let Some(chip) = TYPE_CHIPS.get(i + k) {
                                    let (label, dot, selected) = match chip {
                                        None => ("全部".to_string(), None, full),
                                        Some(c) => (
                                            c.display_name().to_string(),
                                            Some(c.tile_color()),
                                            !full && self.cat_set.contains(c),
                                        ),
                                    };
                                    let r =
                                        type_cell(ui, pal, &label, dot, selected, cell_w);
                                    if r.double_clicked() {
                                        if let Some(c) = chip {
                                            self.toggle_cat(*c, true);
                                        }
                                    } else if r.clicked() {
                                        match chip {
                                            None => {
                                                if !full {
                                                    self.cat_set = all_categories();
                                                    self.schedule_search();
                                                }
                                            }
                                            Some(c) => self.toggle_cat(*c, false),
                                        }
                                    }
                                }
                            }
                        });
                        i += 2;
                    }
                });
            });
        self.filter_panel_full[0] = area.response.rect.size();
        let vis = egui::Rect::from_min_size(pos, Vec2::new(width, full_size.y * v));
        if self.expanded_filter == Some(FilterKind::Type) {
            self.filter_panel_rect = Some(vis);
        }
        self.blockers_cur.push(vis);
    }

    fn toggle_cat(&mut self, cat: Category, solo: bool) {
        let full = self.cat_set.len() >= ALL_CATEGORIES.len();
        if solo {
            if self.cat_set.len() == 1 && self.cat_set.contains(&cat) {
                self.cat_set = all_categories();
            } else {
                self.cat_set = HashSet::from([cat]);
            }
        } else if full {
            // 从全选态单击某一项：改为独选该项，而不是反向取消它
            self.cat_set = HashSet::from([cat]);
        } else if self.cat_set.len() == 1 && self.cat_set.contains(&cat) {
            self.cat_set = all_categories();
        } else if !self.cat_set.remove(&cat) {
            self.cat_set.insert(cat);
        }
        self.schedule_search();
    }

    /// 范围筛选圆点（工具栏右侧）：26px 文件夹圆点（非全部时
    /// 强调描边）+ 旁边摘要胶囊（全部 / 桌面 / 桌面 +2）。点击
    /// 展开/收起范围面板（横向展开，见 draw_scope_panel）。
    /// 选择语义与类型同构（永不空选，见 scope_toggle_*）。
    fn draw_scope_dot(&mut self, ui: &mut egui::Ui) -> egui::Rect {
        let pal = self.display_pal;
        let expanded = self.expanded_filter == Some(FilterKind::Scope);
        let open = expanded || self.filter_anim_t[1] > 0.0;
        let all = self.scope_is_all();
        let summary = self.scope_summary();
        // v0.1.16：摘要胶囊常显（含「全部」）。旧版全盘时工具栏只剩
        // 一个灰色的文件夹图标，看起来像被禁用了，用户会怀疑范围没生效。
        let show_summary = true;
        let font = FontId::proportional(11.0);
        let sum_col = if all { pal.esc_text } else { pal.accent };
        let sum_galley =
            ui.fonts(|f| f.layout_no_wrap(summary.clone(), font, sum_col));
        let sum_w = if show_summary {
            sum_galley.size().x + 18.0
        } else {
            0.0
        };
        let w = 30.0 + if sum_w > 0.0 { 6.0 + sum_w } else { 0.0 };
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(w, 30.0), egui::Sense::click());
        self.blockers_cur.push(rect);
        if expanded {
            self.filter_dot_rect = Some(rect);
        }
        let painter = ui.painter_at(rect);
        // v0.1.15：与类型按钮同一套「30 方、圆角 9、常态无底」，
        // 文件夹只画细线、去掉朱砂外圈。
        let btn = egui::Rect::from_min_size(rect.min, Vec2::splat(30.0));
        let rr = Rounding::same(9.0);
        if open {
            painter.rect_filled(btn, rr, pal.chip_hover_fill);
        } else if resp.hovered() {
            painter.rect_filled(btn, rr, pal.surface_hover);
        }
        // 图标恒朱砂：这两个按钮是「可用控件」，不该有半点禁用感。
        let icon_col = pal.accent;
        let tile = egui::Rect::from_center_size(btn.center(), Vec2::splat(18.0));
        paint_folder_glyph(&painter, tile, icon_col);
        if show_summary {
            let pill = egui::Rect::from_min_size(
                egui::pos2(rect.left() + 36.0, rect.center().y - 10.0),
                Vec2::new(sum_w, 20.0),
            );
            if !open {
                if all {
                    painter.rect_filled(pill, Rounding::same(7.0), pal.esc_fill);
                } else {
                    painter.rect_filled(pill, Rounding::same(7.0), pal.pill_fill);
                    painter.rect_stroke(
                        pill,
                        Rounding::same(7.0),
                        Stroke::new(1.0_f32, pal.pill_stroke),
                    );
                }
            }
            painter.galley(
                egui::pos2(
                    pill.center().x - sum_galley.size().x / 2.0,
                    pill.center().y - sum_galley.size().y / 2.0,
                ),
                sum_galley,
                sum_col,
            );
        }
        if resp.clicked() {
            self.expanded_filter = if expanded {
                None
            } else {
                Some(FilterKind::Scope)
            };
        }
        resp.on_hover_text(format!("范围筛选：{summary}"));
        rect
    }

    /// 范围面板（横向展开）：覆盖层锚在圆点下方，全宽 372，
    /// 内容以完整尺寸布局，裁剪宽/高从圆点足迹平滑长到全尺寸
    /// （0.24s cubic_out）——分区（预设/磁盘/自定义文件夹）保留，
    /// 每区内 chips 横向流式多行排布（用户规格：横向展开与分区
    /// 冲突时分区优先、区内流式）。淡入同步（0.3→1）；收起同理，
    /// 动画途中三路关闭照常有效。
    /// 全集归一化（渲染层）：预设与磁盘同时满员时只点亮「全部」
    /// 一颗 chip，其余全回未选态——v0.1.9 真机的「整片橙墙」
    /// （全集被逐项点亮）就此消失；加载层另有折叠，见
    /// load_settings 的归一化注释。状态编辑走本地克隆 + 收尾
    /// 写回（避开 Area 闭包与 &mut self 的借用冲突）。
    fn draw_scope_panel(&mut self, ui: &mut egui::Ui, dot_rect: egui::Rect) {
        let t = self.filter_anim_t[1];
        if t <= 0.001 {
            return;
        }
        let pal = self.display_pal;
        let v = egui::emath::easing::cubic_out(t);
        let width = 400.0;
        let full_size = self.filter_panel_full[1];
        let screen = ui.ctx().screen_rect();
        let pos = clamp_menu_pos(
            egui::pos2(dot_rect.left(), dot_rect.bottom() + 6.0),
            Vec2::new(width, full_size.y),
            screen,
        );
        let vis_w = dot_rect.width() + (width - dot_rect.width()) * v;
        let vis_h = 30.0 + (full_size.y - 30.0) * v;
        let clip = egui::Rect::from_min_max(
            egui::pos2(pos.x - 8.0, pos.y),
            egui::pos2(pos.x + vis_w + 8.0, pos.y + vis_h + 28.0 * v),
        );
        let mut presets = self.scope_presets.clone();
        let mut drives = self.scope_drives.clone();
        let mut custom = self.scope_custom.clone();
        let mut sel: HashSet<String> = self.scope_custom_sel.clone();
        // 自定义列表的滚动条区域要在窗口之后登记成「不可拖动区」，
        // 否则拖条滚动会变成拖窗口。
        let mut sc_out: Option<egui::scroll_area::ScrollAreaOutput<()>> = None;
        let mut changed = false;
        let mut add_folder = false;
        let area = egui::Area::new(egui::Id::new("ss_scope_panel"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                ui.set_clip_rect(clip);
                ui.multiply_opacity(0.3 + 0.7 * v);
                panel_frame(pal).show(ui, |ui| {
                    ui.set_width(width - 32.0);
                    let all_now = PRESET_KEYS
                        .iter()
                        .all(|(k, _, _)| presets.contains(*k))
                        && existing_drives().iter().all(|d| drives.contains(d));
                    section_title(ui, pal, "预设");
                    // v0.1.15：胶囊流 → 3 列定宽卡片（「全部」+ 4 个
                    // 预设正好两行）。同类型面板的格子语言，扫读整齐。
                    let cell_w = (width - 32.0 - 16.0) / 3.0;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                        if type_cell(ui, pal, "全部", None, all_now, cell_w).clicked() {
                            scope_select_all(&mut presets, &mut drives);
                            changed = true;
                        }
                        for (key, _, label) in PRESET_KEYS.iter().take(2) {
                            let sel = !all_now && presets.contains(*key);
                            let r = type_cell(ui, pal, label, None, sel, cell_w);
                            if r.double_clicked() {
                                scope_toggle_preset(
                                    &mut presets,
                                    &mut drives,
                                    *key,
                                    true,
                                );
                                changed = true;
                            } else if r.clicked() {
                                scope_toggle_preset(
                                    &mut presets,
                                    &mut drives,
                                    *key,
                                    all_now,
                                );
                                changed = true;
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                        for (key, _, label) in PRESET_KEYS.iter().skip(2) {
                            let sel = !all_now && presets.contains(*key);
                            let r = type_cell(ui, pal, label, None, sel, cell_w);
                            if r.double_clicked() {
                                scope_toggle_preset(
                                    &mut presets,
                                    &mut drives,
                                    *key,
                                    true,
                                );
                                changed = true;
                            } else if r.clicked() {
                                scope_toggle_preset(
                                    &mut presets,
                                    &mut drives,
                                    *key,
                                    all_now,
                                );
                                changed = true;
                            }
                        }
                    });
                    ui.add_space(11.0);
                    section_title(ui, pal, "磁盘");
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                        for d in existing_drives() {
                            let sel = !all_now && drives.contains(&d);
                            let r = type_cell(
                                ui,
                                pal,
                                &format!("{d}:\\"),
                                Some((112, 121, 132)),
                                sel,
                                cell_w,
                            );
                            if r.double_clicked() {
                                scope_toggle_drive(&mut presets, &mut drives, d, true);
                                changed = true;
                            } else if r.clicked() {
                                scope_toggle_drive(&mut presets, &mut drives, d, all_now);
                                changed = true;
                            }
                        }
                    });
                    ui.add_space(11.0);
                    section_title(ui, pal, "自定义文件夹");
                    ui.label(
                        RichText::new("点名称选中/取消（可多选），双击在资源管理器打开")
                            .size(11.0)
                            .color(pal.path_text),
                    );
                    ui.add_space(5.0);
                    // v0.1.16：自定义文件夹支持多选 —— 每行是独立开关，
                    // 点名称选中/取消，选中几个就一起搜几个；双击行直接
                    // 在资源管理器里打开该文件夹。旧版只能选一个、且点了
                    // 没反馈，用户以为加完就在搜，实际还在全盘。
                    let mut remove: Option<usize> = None;
                    let mut toggled: Option<String> = None;
                    let mut open_dir: Option<String> = None;
                    // 跨帧保留「第一次单击」快照，双击时回滚。
                    let mut memo = self.scope_click_memo.take();
                    sc_out = Some(egui::ScrollArea::vertical()
                        .id_salt("ss_scope_custom_scroll")
                        .max_height(196.0)
                        .auto_shrink([false, true])
                        .drag_to_scroll(false)
                        .show(ui, |ui| {
                            ui.set_width(width - 32.0);
                            for (i, path) in custom.iter().enumerate() {
                                let selected = !all_now && sel.contains(path);
                                let (click, dbl, close) = scope_row(
                                    ui,
                                    pal,
                                    &dir_last(path),
                                    path,
                                    width - 32.0,
                                    selected,
                                );
                                if close {
                                    remove = Some(i);
                                } else if dbl {
                                    // 双击：把第一下 click 已经改过的范围
                                    // 状态原样退回，只保留「打开文件夹」。
                                    if let Some((mp, _was, p0, d0, s0)) = memo.take() {
                                        if mp == *path {
                                            presets = p0;
                                            drives = d0;
                                            sel = s0;
                                            changed = true;
                                        }
                                    }
                                    open_dir = Some(path.clone());
                                } else if click {
                                    memo = Some((
                                        path.clone(),
                                        sel.contains(path),
                                        presets.clone(),
                                        drives.clone(),
                                        sel.clone(),
                                    ));
                                    toggled = Some(path.clone());
                                }
                            }
                        }));
                    self.scope_click_memo = memo;
                    if let Some(i) = remove {
                        let p = custom.remove(i);
                        sel.remove(&p);
                        changed = true;
                    }
                    if let Some(p) = open_dir {
                        #[cfg(windows)]
                        {
                            let _ = std::process::Command::new("explorer")
                                .arg(&p)
                                .spawn();
                        }
                    }
                    if let Some(p) = toggled {
                        if all_now {
                            // 从「全部」切到自定义：清掉预设与磁盘，
                            // 这一行进选中集（其余已选的自定义保留）。
                            presets.clear();
                            drives.clear();
                            sel.insert(p);
                        } else if !sel.remove(&p) {
                            sel.insert(p);
                        }
                        changed = true;
                    }
                    // 一个根都不剩 → 回「全部」，别留空范围当全盘用。
                    if sel.is_empty() && presets.is_empty() && drives.is_empty() {
                        scope_select_all(&mut presets, &mut drives);
                        changed = true;
                    }
                    let can_add = cfg!(windows);
                    if dashed_row(ui, pal, "＋ 添加文件夹", width - 32.0, can_add) {
                        add_folder = true;
                    }
                });
            });
        self.filter_panel_full[1] = area.response.rect.size();
        if let Some(o) = &sc_out {
            self.push_scroll_blocker(o);
        }
        let vis = egui::Rect::from_min_size(pos, Vec2::new(vis_w, vis_h));
        if self.expanded_filter == Some(FilterKind::Scope) {
            self.filter_panel_rect = Some(vis);
        }
        self.blockers_cur.push(vis);
        let mut added_root = false;
        if add_folder {
            #[cfg(windows)]
            {
                let _guard = clickwatch::SuspendGuard::new();
                // v0.1.16：多选对话框，一次可以加好几个文件夹。
                if let Some(paths) = rfd::FileDialog::new().pick_folders() {
                    let mut added = false;
                    for p in paths {
                        let s = p.to_string_lossy().into_owned();
                        if !custom.contains(&s) {
                            custom.push(s.clone());
                            sel.insert(s);
                            added = true;
                        }
                    }
                    // 新加的文件夹立刻进入搜索范围：清掉「全部」，
                    // 否则加完一看还是全盘结果，等于白加。
                    if added {
                        presets.clear();
                        drives.clear();
                        changed = true;
                        added_root = true;
                    }
                }
            }
        }
        if changed {
            self.scope_presets = presets;
            self.scope_drives = drives;
            self.scope_custom = custom;
            self.scope_custom_sel = sel;
            self.save_settings();
            self.schedule_search();
        }
        if added_root {
            // 新文件夹很可能不在现有索引里（便携模式只索引
            // %USERPROFILE% 一带）→ 立刻重建，否则选中也搜不到。
            self.start_rebuild();
        }
    }

    fn draw_results(&mut self, ui: &mut egui::Ui) {
        // 结果集进场动画（v0.1.10 内容层动画，原版令牌：进入快、
        // 淡入起点 ≥0.55、位移 ≤10px、只动 Opacity/位移）：新一批
        // 结果到达后 220ms 内整组淡入（0.55→1）+ 上浮 8px 收敛。
        // 上浮用顶部留白实现——布局与命中 rect 一起移动，动画
        // 期间点击照样精准，不逐行乱序、不挡点击。动画结束即
        // 停表，静止零重绘。
        if let Some(t0) = self.results_born {
            let age = ui.ctx().input(|i| i.time) - t0;
            if age >= 0.22 {
                self.results_born = None;
            } else {
                let v = egui::emath::easing::cubic_out((age / 0.22) as f32);
                ui.multiply_opacity(0.55 + 0.45 * v);
                ui.add_space(8.0 * (1.0 - v));
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(16));
            }
        }
        // 首页（空查询）：只显用户 pins、强制宫格，不走搜索结果管线
        if self.is_home() {
            self.draw_home(ui);
            return;
        }
        if self.rows.is_empty() {
            let pal = self.display_pal;
            // 空状态文案（原版）：索引还在建 / 没有匹配的内容
            let text = if self.rebuilding.load(Ordering::SeqCst) && self.index_count == 0 {
                "索引还在建，等它跑完再搜"
            } else {
                "没有匹配的内容"
            };
            ui.centered_and_justified(|ui| {
                let rect = ui.max_rect();
                let galley = ui.fonts(|f| {
                    f.layout_no_wrap(text.to_string(), serif(14.0), pal.empty_text)
                });
                ui.painter().galley(
                    egui::pos2(
                        rect.center().x - galley.size().x / 2.0,
                        rect.center().y - galley.size().y / 2.0,
                    ),
                    galley,
                    pal.empty_text,
                );
            });
            return;
        }
        match self.view_mode {
            ViewMode::List => self.draw_results_list(ui),
            ViewMode::Grid => self.draw_results_grid(ui),
        }
    }
    /// 首页（v0.1.6 用户定义）：只显示用户自选 pins，强制宫格
    /// （不受 view_mode 影响），磁贴只显图标 + 名称，详情走悬浮卡。
    /// 末尾恒有「＋ 添加」格；pins 为空时另显一行引导文案。
    fn draw_home(&mut self, ui: &mut egui::Ui) {
        let pal = self.display_pal;
        // 引导文案不再独占一行（v0.1.10 布局收紧）：无 pins 时
        // 它并入下方空状态区（宫格之后剩余空间居中显示，见本
        // 函数尾部）；有 pins 时只留「已固定 N」说明行。
        if !self.home_items.is_empty() {
            // 说明行（原版 HomeCaption：Sans 10.5 TextLow；下间距
            // v0.1.7 按用户反馈从 10 收紧到 6）
            ui.label(
                RichText::new(format!("已固定 {}", self.home_items.len()))
                    .size(10.5)
                    .color(pal.status_text),
            );
            // 标题 → 磁贴收紧到约 6（v0.1.7，原 10 显空）
            ui.add_space(6.0);
        }

        let avail_w = ui.available_width();
        let cols = (((avail_w + HOME_TILE_GAP) / (HOME_TILE_W + HOME_TILE_GAP)) as usize).max(1);
        self.home_cols = cols;
        let total_cells = self.home_items.len() + 1; // pins + 添加格
        let grid_rows = total_cells.div_ceil(cols);
        let row_pitch = HOME_TILE_H + HOME_TILE_GAP;
        let mut action: Option<HomeAction> = None;
        let mut add_clicked = false;
        let mut add_resp: Option<egui::Response> = None;
        // drag_to_scroll(false)：ScrollArea 默认会对整个视口
        // 做 Sense::drag 交互（egui scroll_area.rs），会抢走
        // 空白区的整窗拖动（v0.1.9 背景层在更低层级必输）。
        // 桌面端滚轮滚动不受影响，拖拽滚动是触屏特性。
        // 无 pins 时纵向收缩到内容高（只剩添加格一行），剩余
        // 空间让给空状态文案；有 pins 时照旧撑满
        let mut scroll = egui::ScrollArea::vertical()
            .auto_shrink([false, self.home_items.is_empty()])
            .drag_to_scroll(false);
        if self.scroll_to_selected {
            let offset =
                ((self.selected / cols) as f32 * row_pitch - 120.0).max(0.0);
            scroll = scroll.vertical_scroll_offset(offset);
            self.scroll_to_selected = false;
        }
        let scroll_out = scroll.show_rows(ui, row_pitch, grid_rows, |ui, range| {
            for gr in range {
                let (row_rect, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), row_pitch),
                    egui::Sense::hover(),
                );
                for c in 0..cols {
                    let idx = gr * cols + c;
                    if idx >= total_cells {
                        break;
                    }
                    let cell = egui::Rect::from_min_size(
                        row_rect.min
                            + Vec2::new(c as f32 * (HOME_TILE_W + HOME_TILE_GAP), 0.0),
                        Vec2::new(HOME_TILE_W, HOME_TILE_H),
                    );
                    if idx < self.home_items.len() {
                        if let Some(a) = self.draw_home_tile(ui, cell, idx) {
                            action = Some(a);
                        }
                    } else {
                        // 「＋ 添加」格：与磁贴同尺寸同排版（桌面图标
                        // 式）——默认无底，hover 给 PillFill 淡填充；
                        // 加号居图标位中心，文字紧贴其下
                        let resp = ui.interact(
                            cell,
                            ui.id().with("home_add_cell"),
                            egui::Sense::click(),
                        );
                        let painter = ui.painter_at(cell);
                        // hover 填充缓动（v0.1.10 内容层动画，150ms
                        // cubic_out，与磁贴/结果行同款）
                        let hv = ui.ctx().animate_bool_with_time_and_easing(
                            ui.make_persistent_id("home_add_hover"),
                            resp.hovered(),
                            0.15,
                            egui::emath::easing::cubic_out,
                        );
                        if (0.0..1.0).contains(&hv) {
                            ui.ctx().request_repaint_after(
                                std::time::Duration::from_millis(16),
                            );
                        }
                        if hv > 0.01 {
                            painter.rect_filled(
                                cell,
                                Rounding::same(10.0),
                                Color32::from_rgba_unmultiplied(
                                    pal.pill_fill.r(),
                                    pal.pill_fill.g(),
                                    pal.pill_fill.b(),
                                    (pal.pill_fill.a() as f32 * hv) as u8,
                                ),
                            );
                        }
                        let cc = egui::pos2(cell.center().x, cell.top() + 28.0);
                        let plus = Stroke::new(2.0_f32, pal.text_mid);
                        painter.line_segment(
                            [egui::pos2(cc.x - 9.0, cc.y), egui::pos2(cc.x + 9.0, cc.y)],
                            plus,
                        );
                        painter.line_segment(
                            [egui::pos2(cc.x, cc.y - 9.0), egui::pos2(cc.x, cc.y + 9.0)],
                            plus,
                        );
                        painter.text(
                            egui::pos2(cell.center().x, cell.top() + 62.0),
                            Align2::CENTER_CENTER,
                            "添加",
                            serif(12.5),
                            pal.path_text,
                        );
                        if resp.clicked() {
                            add_clicked = true;
                        }
                        add_resp = Some(resp);
                    }
                }
            }
        });

        // 拖动排除区登记：滚动条带（可滚时才有）与添加格本体
        self.push_scroll_blocker(&scroll_out);
        if let Some(resp) = &add_resp {
            self.blockers_cur.push(resp.rect);
        }

        // 空状态文案（无 pins 时）：并入宫格下方的剩余空间居中，
        // 不再独占顶部一行（v0.1.10 布局收紧）
        if self.home_items.is_empty() {
            let rest_h = ui.available_height();
            if rest_h > 40.0 {
                let (rect, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), rest_h),
                    egui::Sense::hover(),
                );
                let galley = ui.fonts(|f| {
                    f.layout_no_wrap(
                        "右键任意搜索结果选『添加到首页』，或点＋添加".to_string(),
                        serif(14.0),
                        pal.empty_text,
                    )
                });
                ui.painter_at(rect).galley(
                    egui::pos2(
                        rect.center().x - galley.size().x / 2.0,
                        rect.center().y - galley.size().y / 2.0,
                    ),
                    galley,
                    pal.empty_text,
                );
            }
        }

        // 「＋ 添加」菜单（v0.1.9 改自绘）：锚在添加格下方，
        // 走应用菜单语言与宽松尺度（menu_frame / menu_item：
        // 宽 200、行高 34、字 13），位置钳制在窗口内（下方
        // 放不下改向上弹、右溢出贴边收回）——旧版 egui 默认
        // popup 是灰按钮条，还会溢出到屏幕外。Esc 关闭在
        // handle_keys 里统一拦截，点外关闭在本块末尾判定。
        if add_clicked {
            self.home_add_open = !self.home_add_open;
            if self.home_add_open {
                self.home_add_born = ui.ctx().input(|i| i.time);
            }
        }
        #[allow(unused_mut)] // 只有 Windows 分支会改它们（rfd 对话框）
        let mut pick_file = false;
        #[allow(unused_mut)]
        let mut pick_folder = false;
        if self.home_add_open {
            match &add_resp {
                Some(resp) => {
                    let pal = self.display_pal;
                    let screen = ui.ctx().screen_rect();
                    let anchor = resp.rect;
                    let size = Vec2::new(MENU_MIN_W, menu_height(2, 0));
                    let mut pos =
                        egui::pos2(anchor.left(), anchor.bottom() + 4.0);
                    if pos.y + size.y > screen.bottom() - 6.0 {
                        // 靠下缘：向上弹
                        pos.y = anchor.top() - size.y - 4.0;
                    }
                    pos = clamp_menu_pos(pos, size, screen);
                    // 进场动画（v0.1.10 内容层动画）：180ms 高度
                    // 揭示 + 淡入（cubic_out）；关闭保持即时——
                    // 菜单语义（点外/Esc 一点就没，拖尾会显得黏）
                    let age = ui.ctx().input(|i| i.time) - self.home_add_born;
                    let mv = egui::emath::easing::cubic_out(
                        (age / 0.18).clamp(0.0, 1.0) as f32,
                    );
                    if mv < 1.0 {
                        ui.ctx().request_repaint_after(
                            std::time::Duration::from_millis(16),
                        );
                    }
                    let menu_clip = egui::Rect::from_min_max(
                        egui::pos2(pos.x - 8.0, pos.y - 4.0),
                        egui::pos2(
                            pos.x + size.x + 8.0,
                            pos.y + size.y * mv + 26.0 * mv,
                        ),
                    );
                    let area = egui::Area::new(
                        ui.make_persistent_id("home_add_menu"),
                    )
                    .order(egui::Order::Foreground)
                    .fixed_pos(pos)
                    .show(ui.ctx(), |ui| {
                        ui.set_clip_rect(menu_clip);
                        ui.multiply_opacity(0.3 + 0.7 * mv);
                        menu_frame(pal).show(ui, |ui| {
                            if menu_item(ui, pal, "选择文件…", cfg!(windows)) {
                                pick_file = true;
                            }
                            ui.add_space(MENU_ITEM_GAP);
                            if menu_item(ui, pal, "选择文件夹…", cfg!(windows))
                            {
                                pick_folder = true;
                            }
                        })
                    });
                    self.blockers_cur.push(area.response.rect);
                    if pick_file || pick_folder {
                        self.home_add_open = false;
                    } else {
                        // 点外关闭：按下点落在菜单与锚点之外
                        let press = ui.ctx().input(|i| {
                            if i.pointer.primary_pressed() {
                                i.pointer
                                    .press_origin()
                                    .or_else(|| i.pointer.interact_pos())
                            } else {
                                None
                            }
                        });
                        if let Some(p) = press {
                            if !area.response.rect.expand(3.0).contains(p)
                                && !anchor.expand(3.0).contains(p)
                            {
                                self.home_add_open = false;
                            }
                        }
                    }
                }
                None => self.home_add_open = false,
            }
        }
        // 系统对话框是独立窗口：用户在对话框里的点击对钩子而言
        // 全是「点外」。对话框是模态的、UI 线程全程阻塞，钩子只
        // 置位攒着——返回后立刻 drain 掉，免得对话框一关主窗
        // 跟着被藏（v0.1.10 点外链的对话框豁免）。
        #[cfg(windows)]
        {
            if pick_file {
                let _guard = clickwatch::SuspendGuard::new();
                if let Some(p) = rfd::FileDialog::new().pick_file() {
                    self.add_pin(&p.to_string_lossy().into_owned());
                }
            }
            if pick_folder {
                let _guard = clickwatch::SuspendGuard::new();
                if let Some(p) = rfd::FileDialog::new().pick_folder() {
                    self.add_pin(&p.to_string_lossy().into_owned());
                }
            }
        }
        let _ = (pick_file, pick_folder);

        match action {
            Some(HomeAction::Select(idx)) => self.selected = idx,
            Some(HomeAction::Open(idx)) => {
                let ctx = ui.ctx().clone();
                self.open_home_item(&ctx, idx, false);
            }
            Some(HomeAction::Locate(idx)) => {
                let ctx = ui.ctx().clone();
                self.open_home_item(&ctx, idx, true);
            }
            None => {}
        }
    }

    /// 一个首页磁贴（v0.1.7 用户裁决，覆盖原版 170×92 令牌）：
    /// 桌面图标式紧凑格 96×88，默认无底无边，hover / 选中才给
    /// 一层 PillFill 语言淡填充；图标 44×44 居中上方（上距 6），
    /// 未取到时 40×40 IconPlate 占位盘 + 类别字形（不许空盘）；
    /// 名称 Serif 12.5 紧贴图标下方、居中、最多 2 行省略。
    /// 右键菜单：打开 / 打开所在文件夹 / 从首页移除。
    fn draw_home_tile(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        idx: usize,
    ) -> Option<HomeAction> {
        let pal = self.display_pal;
        let response = ui.interact(rect, ui.id().with(("home_tile", idx)), egui::Sense::click());
        self.blockers_cur.push(rect);
        let painter = ui.painter_at(rect);
        let selected = idx == self.selected;
        if selected {
            painter.rect_filled(rect, Rounding::same(10.0), pal.pill_hover);
            painter.rect_stroke(
                rect,
                Rounding::same(10.0),
                Stroke::new(1.0_f32, pal.pill_stroke),
            );
        } else {
            // hover 填充缓动（v0.1.10，150ms cubic_out，与结果行同款）
            let hv = hover_strength(ui, ("tile_hover", idx), response.hovered());
            if hv > 0.01 {
                painter.rect_filled(
                    rect,
                    Rounding::same(10.0),
                    fade_alpha(pal.pill_fill, hv),
                );
            }
        }
        let item = self.home_items.get(idx)?;
        // 图标：真实图标优先（按路径取，目标已删时 worker 取不到
        // 就恒占位盘，格子照常显示，不崩）
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(rect.center().x - 22.0, rect.top() + 6.0),
            Vec2::splat(44.0),
        );
        let icon_key = icons::key_for(&item.name, &item.path, item.category);
        let tex = self.icons.texture(ui.ctx(), &icon_key);
        match tex {
            Some(tex) => {
                painter.image(
                    tex.id(),
                    icon_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            None => {
                let plate =
                    egui::Rect::from_center_size(icon_rect.center(), Vec2::splat(40.0));
                painter.rect_filled(plate, Rounding::same(10.0), pal.icon_plate);
                // 图标总规（v0.1.8 用户钉死）：文件类图标只许 Windows
                // 系统原版（Shell 取图链：真实路径 → .lnk 目标解析 →
                // USEFILEATTRIBUTES 通用图标），不许自绘字形冒充。
                // Windows 上图标未到达时只画空占位盘，到达后换真图标；
                // 非 Windows 演示构建没有 Shell，才保留类别字形。
                #[cfg(not(windows))]
                {
                    let glyph =
                        egui::Rect::from_center_size(plate.center(), Vec2::splat(26.0));
                    match item.category {
                        Category::Folder | Category::InstallDir => {
                            paint_folder_glyph(&painter, glyph, pal.path_text)
                        }
                        Category::Document => {
                            paint_doc_glyph(&painter, glyph, pal.text_mid)
                        }
                        _ => paint_file_glyph(&painter, glyph, pal.text_mid),
                    }
                }
            }
        }
        // 名称：Serif 12.5，紧贴图标下方（图标底 +4），居中，
        // 最多 2 行、超出收尾加省略号
        let name_top = rect.top() + 54.0;
        let wrap_w = rect.width() - 10.0;
        let name_clip = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 2.0, name_top),
            egui::pos2(rect.right() - 2.0, rect.bottom() - 2.0),
        );
        let np = painter.with_clip_rect(name_clip);
        // 排版走 fonts().layout（与宫格格子同路径，已验证居中无偏
        // 移）；手拼 LayoutJob 设 halign=Center 时 galley 行偏移会
        // 让整段文字左移出格（v0.1.7 首版预览实测），不要再用。
        // 显示名（v0.1.9 ShellNames）：解析到替换名时磁贴用替换名
        let tile_name = self
            .names
            .get(
                &item.path,
                matches!(item.category, Category::Folder | Category::InstallDir),
            )
            .unwrap_or_else(|| item.name.clone());
        let mut display = tile_name.clone();
        let mut galley =
            ui.fonts(|f| f.layout(display.clone(), serif(12.5), pal.name_text, wrap_w));
        if galley.rows.len() > 2 {
            let mut chars: Vec<char> = tile_name.chars().collect();
            while galley.rows.len() > 2 && !chars.is_empty() {
                let cut = (chars.len() / 8).max(1);
                chars.truncate(chars.len().saturating_sub(cut));
                display = format!("{}…", chars.iter().collect::<String>());
                galley = ui.fonts(|f| {
                    f.layout(display.clone(), serif(12.5), pal.name_text, wrap_w)
                });
            }
        }
        np.galley(
            egui::pos2(rect.center().x - galley.size().x / 2.0, name_top),
            galley,
            pal.name_text,
        );
        if response.hovered() {
            self.hover_seen = Some(HoverTarget {
                name: item.name.clone(),
                path: item.path.clone(),
                category: item.category,
            });
        }
        let mut act = None;
        if response.double_clicked() {
            act = Some(HomeAction::Open(idx));
        } else if response.clicked() {
            act = Some(HomeAction::Select(idx));
        }
        // 右键菜单（v0.1.9 自绘，见 draw_ctx_menu）：这里只
        // 登记打开状态，绘制与动作执行在 update 里统一做
        if response.secondary_clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                self.ctx_menu_born = ui.ctx().input(|i| i.time);
                self.ctx_menu = Some(CtxMenu {
                    pos: p,
                    target: CtxTarget::Home(idx),
                });
            }
        }
        act
    }

    fn draw_results_list(&mut self, ui: &mut egui::Ui) {
        let mut action: Option<RowAction> = None;
        let mut scroll = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .drag_to_scroll(false);
        if self.scroll_to_selected {
            let offset = (self.selected as f32 * ROW_H - 140.0).max(0.0);
            scroll = scroll.vertical_scroll_offset(offset);
            self.scroll_to_selected = false;
        }
        let out = scroll.show_rows(ui, ROW_H, self.display.len(), |ui, range| {
            for idx in range {
                if let Some(a) = self.draw_display_row(ui, idx) {
                    action = Some(a);
                }
            }
        });
        self.push_scroll_blocker(&out);
        match action {
            Some(RowAction::Select(idx)) => self.selected = idx,
            Some(RowAction::Open(idx)) => {
                let ctx = ui.ctx().clone();
                self.open_display_row(&ctx, idx, false);
            }
            Some(RowAction::Locate(idx)) => {
                self.selected = idx;
                let ctx = ui.ctx().clone();
                self.open_display_row(&ctx, idx, true);
            }
            Some(RowAction::Toggle(idx, dir)) => {
                self.selected = idx;
                self.toggle_group(&dir);
            }
            None => {}
        }
    }

    /// 宫格模式：格子约 104×96（40×40 图标 + 名字至多 2 行 + 分类
    /// 小字），列数 = 可用宽度 / 104。一屏约 30 项（列表约 7 行），
    /// 密度差就是这个模式的卖点。展示项与列表共用同一份 display
    ///（组项在宫格里是一个组格），选中索引同样按展示项维护。
    fn draw_results_grid(&mut self, ui: &mut egui::Ui) {
        let avail_w = ui.available_width();
        let cols = ((avail_w / GRID_CELL_W) as usize).max(1);
        self.grid_cols = cols;
        let grid_rows = self.display.len().div_ceil(cols);
        let mut action: Option<RowAction> = None;
        let mut scroll = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .drag_to_scroll(false);
        if self.scroll_to_selected {
            let offset = ((self.selected / cols) as f32 * GRID_CELL_H - 140.0).max(0.0);
            scroll = scroll.vertical_scroll_offset(offset);
            self.scroll_to_selected = false;
        }
        let out = scroll.show_rows(ui, GRID_CELL_H, grid_rows, |ui, range| {
            let x_off = ((ui.available_width() - cols as f32 * GRID_CELL_W) / 2.0).max(0.0);
            for gr in range {
                let (row_rect, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), GRID_CELL_H),
                    egui::Sense::hover(),
                );
                for c in 0..cols {
                    let idx = gr * cols + c;
                    if idx >= self.display.len() {
                        break;
                    }
                    let cell = egui::Rect::from_min_size(
                        row_rect.min + Vec2::new(x_off + c as f32 * GRID_CELL_W, 0.0),
                        Vec2::new(GRID_CELL_W, GRID_CELL_H),
                    );
                    if let Some(a) = self.draw_grid_cell(ui, cell, idx) {
                        action = Some(a);
                    }
                }
            }
        });
        self.push_scroll_blocker(&out);
        match action {
            Some(RowAction::Select(idx)) => self.selected = idx,
            Some(RowAction::Open(idx)) => {
                let ctx = ui.ctx().clone();
                self.open_display_row(&ctx, idx, false);
            }
            Some(RowAction::Locate(idx)) => {
                self.selected = idx;
                let ctx = ui.ctx().clone();
                self.open_display_row(&ctx, idx, true);
            }
            Some(RowAction::Toggle(idx, dir)) => {
                self.selected = idx;
                self.toggle_group(&dir);
            }
            None => {}
        }
    }

    /// 一个宫格格子：普通项 = 图标 40 + 名字（Serif 12，≤2 行）；
    /// 组项 = 占位盘 + 文件夹字形 + 父目录末段 + 「N 项」。
    /// 悬停 SurfaceHover、选中 SurfaceSel + PillStroke（原版磁贴
    /// 语言）。绘制借用 rows 不 clone（v0.1.6 减负）。右键菜单与
    /// 列表行同：打开 / 打开所在文件夹 / 添加到首页（组项无菜单）。
    fn draw_grid_cell(
        &mut self,
        ui: &mut egui::Ui,
        cell: egui::Rect,
        idx: usize,
    ) -> Option<RowAction> {
        let pal = self.display_pal;
        // 先把展示项按小拷贝取出（Single 全是 Copy 字段；Group 只
        // clone dir 字符串），行本体绘制时借用，不整行 clone。
        enum CellData {
            Single(usize, bool),
            /// (组键, 组头文字, 成员数, 是否展开)
            Group(String, String, usize, bool),
        }
        let data = match self.display.get(idx)? {
            DisplayRow::Single { row, indent } => CellData::Single(*row, *indent),
            DisplayRow::Group {
                key,
                label,
                count,
                expanded,
            } => CellData::Group(key.clone(), label.clone(), *count, *expanded),
        };
        let rect = cell.shrink(3.0);
        let response = ui.interact(rect, ui.id().with(("grid_cell", idx)), egui::Sense::click());
        self.blockers_cur.push(rect);
        let painter = ui.painter_at(rect);
        let selected = idx == self.selected;
        if selected {
            painter.rect_filled(rect, Rounding::same(10.0), pal.surface_sel);
            painter.rect_stroke(
                rect,
                Rounding::same(10.0),
                Stroke::new(1.0_f32, pal.pill_stroke),
            );
        } else {
            let hv = hover_strength(ui, ("grid_hover", idx), response.hovered());
            if hv > 0.01 {
                painter.rect_filled(
                    rect,
                    Rounding::same(10.0),
                    fade_alpha(pal.surface_hover, hv),
                );
            }
        }
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(rect.center().x - 20.0, rect.top() + 6.0),
            Vec2::splat(40.0),
        );
        match &data {
            CellData::Single(row_idx, indent) => {
                let r = self.rows.get(*row_idx)?;
                if response.hovered() {
                    self.hover_seen = Some(HoverTarget {
                        name: r.name.clone(),
                        path: r.path.clone(),
                        category: r.category,
                    });
                }
                // 图标：真实图标优先（40×40），未取到 = IconPlate 占位盘
                let icon_key = icons::key_for(&r.name, &r.path, r.category);
                let drawn = self
                    .icons
                    .texture(ui.ctx(), &icon_key)
                    .map(|tex| {
                        painter.image(
                            tex.id(),
                            icon_rect,
                            egui::Rect::from_min_max(
                                egui::pos2(0.0, 0.0),
                                egui::pos2(1.0, 1.0),
                            ),
                            Color32::WHITE,
                        );
                    })
                    .is_some();
                if !drawn {
                    painter.rect_filled(
                        icon_rect,
                        Rounding::same(10.0),
                        pal.icon_plate,
                    );
                }
                if *indent {
                    painter.circle_filled(
                        egui::pos2(rect.left() + 8.0, rect.top() + 8.0),
                        2.5,
                        pal.accent,
                    );
                }
                // 名字：Serif 12，最多 2 行、超出省略、居中；
                // 显示名解析到替换名时用替换名（v0.1.9 ShellNames）
                let cell_name = self
                    .names
                    .get(
                        &r.path,
                        matches!(r.category, Category::Folder | Category::InstallDir),
                    )
                    .unwrap_or_else(|| r.name.clone());
                let galley = ui.fonts(|f| {
                    f.layout(
                        cell_name,
                        serif(12.0),
                        pal.name_text,
                        92.0,
                    )
                });
                painter.galley(
                    egui::pos2(
                        rect.center().x - galley.size().x / 2.0,
                        rect.top() + 50.0,
                    ),
                    galley,
                    pal.name_text,
                );
                // 类型副行（v0.1.8）：格子底部一行小字具体类型名
                painter.text(
                    egui::pos2(rect.center().x, rect.bottom() - 8.0),
                    egui::Align2::CENTER_CENTER,
                    r.type_label.clone(),
                    FontId::proportional(9.5),
                    pal.path_text,
                );
                let mut act = None;
                if response.double_clicked() {
                    act = Some(RowAction::Open(idx));
                } else if response.clicked() {
                    act = Some(RowAction::Select(idx));
                }
                // 右键菜单（v0.1.9 自绘，见 draw_ctx_menu）
                if response.secondary_clicked() {
                    if let Some(p) = response.interact_pointer_pos() {
                        self.ctx_menu_born = ui.ctx().input(|i| i.time);
                self.ctx_menu = Some(CtxMenu {
                            pos: p,
                            target: CtxTarget::Result(idx),
                        });
                    }
                }
                act
            }
            CellData::Group(key, label, count, expanded) => {
                // 占位盘（组格没有单一真实图标可取）。图标总规
                //（v0.1.8）：Windows 上不许拿自绘文件夹字形冒充文件
                // 夹图标，只画空盘；文件夹字形仅非 Windows 演示保留。
                // 类型组没有单一路径，不弹悬停详情卡。
                painter.rect_filled(icon_rect, Rounding::same(10.0), pal.icon_plate);
                #[cfg(not(windows))]
                {
                    let glyph = egui::Rect::from_center_size(
                        icon_rect.center(),
                        Vec2::splat(30.0),
                    );
                    paint_folder_glyph(&painter, glyph, pal.path_text);
                }
                let galley = ui.fonts(|f| {
                    f.layout(
                        label.clone(),
                        serif(12.0),
                        pal.name_text,
                        92.0,
                    )
                });
                painter.galley(
                    egui::pos2(
                        rect.center().x - galley.size().x / 2.0,
                        rect.top() + 50.0,
                    ),
                    galley,
                    pal.name_text,
                );
                painter.text(
                    egui::pos2(rect.center().x, rect.bottom() - 8.0),
                    egui::Align2::CENTER_CENTER,
                    format!("{} 项 {}", count, if *expanded { "▾" } else { "▸" }),
                    FontId::proportional(10.5),
                    pal.path_text,
                );
                if response.clicked() || response.double_clicked() {
                    Some(RowAction::Toggle(idx, key.clone()))
                } else {
                    None
                }
            }
        }
    }
    /// 打开一个展示行：普通行打开对应结果（locate = 打开所在
    /// 文件夹），组行展开/收起（鼠标路径用；键盘路径走
    /// open_selected，逻辑一致）。
    fn open_display_row(&mut self, ctx: &egui::Context, idx: usize, locate: bool) {
        match self.display.get(idx).cloned() {
            Some(DisplayRow::Group { key, .. }) => self.toggle_group(&key),
            Some(DisplayRow::Single { row, .. }) => self.open_row(ctx, row, locate),
            None => {}
        }
    }

    fn draw_display_row(&mut self, ui: &mut egui::Ui, idx: usize) -> Option<RowAction> {
        // 借用取出（v0.1.6 减负）：Single 的字段是 Copy；Group 只
        // clone 键与标签字符串，不再整行 clone DisplayRow。
        match self.display.get(idx)? {
            DisplayRow::Single { row, indent } => {
                let (r, i) = (*row, *indent);
                self.draw_row(ui, r, idx, i)
            }
            DisplayRow::Group {
                key,
                label,
                count,
                expanded,
            } => {
                let (k, l, c, e) = (key.clone(), label.clone(), *count, *expanded);
                self.draw_group_row(ui, idx, &k, &l, c, e)
            }
        }
    }

    /// 手绘一行（原版 ResultRow 样式）：行高 52、圆角 10；内容左
    /// Margin 18，图标 24×24（真实图标优先，未取到 = IconPlate
    /// 占位盘圆角 6，不再画分类色块字）；名 Serif 14 TextHi（命中
    /// 段 AccentText 高亮）、路径 Sans 10 TextLow；右侧簇 = 类型
    /// 标签 Sans 10 TextLow + 重要度点 5px + 已固定标记（PinMark）。
    /// 悬停 RowHover；选中 RowSel + RowSelStroke + 左侧 3×26 朱砂
    /// 标记条；行底发丝线（左 56 起、HairLine）悬停/选中时隐藏。
    /// 绘制只借用 rows 不 clone（v0.1.6 减负，悬停时才 clone 一份
    /// HoverTarget）。右键菜单：打开 / 打开所在文件夹 / 添加到首页。
    fn draw_row(
        &mut self,
        ui: &mut egui::Ui,
        row_idx: usize,
        display_idx: usize,
        indent: bool,
    ) -> Option<RowAction> {
        let pal = self.display_pal;
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), ROW_H),
            egui::Sense::click(),
        );
        let row = self.rows.get(row_idx)?;
        let selected = display_idx == self.selected;
        self.blockers_cur.push(rect);
        let painter = ui.painter_at(rect);
        let rounding = Rounding::same(10.0);
        if selected {
            painter.rect_filled(rect, rounding, pal.row_sel_fill);
            painter.rect_stroke(
                rect,
                rounding,
                Stroke::new(1.0_f32, pal.row_sel_stroke),
            );
            // 选中标记条：3×26 朱砂，圆角 1.5，左 Margin 6
            let mark = egui::Rect::from_min_size(
                egui::pos2(rect.left() + 6.0, rect.center().y - 13.0),
                Vec2::new(3.0, 26.0),
            );
            painter.rect_filled(mark, Rounding::same(1.5), pal.accent);
        } else {
            // hover 填充缓动（v0.1.10 内容层动画）：150ms
            // cubic_out 按强度缩放 RowHover 的 alpha，不再瞬跳
            let hv = hover_strength(ui, ("row_hover", display_idx), response.hovered());
            if hv > 0.01 {
                painter.rect_filled(
                    rect,
                    rounding,
                    fade_alpha(pal.row_hover_fill, hv),
                );
            }
        }

        let indent_x = if indent { 16.0 } else { 0.0 };
        // 图标 24×24（列位左 18）：真实系统图标优先，未取到回退占位盘
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(
                rect.left() + 18.0 + indent_x,
                rect.center().y - icons::ICON_SIZE / 2.0,
            ),
            Vec2::splat(icons::ICON_SIZE),
        );
        let icon_key = icons::key_for(&row.name, &row.path, row.category);
        let icon_tex = self.icons.texture(ui.ctx(), &icon_key);
        match icon_tex {
            Some(tex) => {
                painter.image(
                    tex.id(),
                    icon_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            None => {
                painter.rect_filled(icon_rect, Rounding::same(6.0), pal.icon_plate);
            }
        }

        // 右侧簇（右 Margin 12 起向左）：类型标签 → 重要度点 → 固定标记
        // 类型标签 v0.1.8 起用具体类型名（filetype，如「Word 文档」），
        // 重要度点按三档取色（demoted 信号强制按 Low 显示）。
        let type_galley = painter.layout_no_wrap(
            row.type_label.clone(),
            FontId::proportional(10.0),
            pal.path_text,
        );
        let type_x = rect.right() - 12.0 - type_galley.size().x;
        painter.galley(
            egui::pos2(type_x, rect.center().y - type_galley.size().y / 2.0),
            type_galley,
            pal.path_text,
        );
        let imp_color = if row.demoted {
            pal.importance_low
        } else {
            match row.importance {
                Importance::High => pal.importance_high,
                Importance::Normal => pal.importance_normal,
                Importance::Low => pal.importance_low,
            }
        };
        let dot_c = egui::pos2(type_x - 7.0 - 2.5, rect.center().y);
        painter.circle_filled(dot_c, 2.5, imp_color);
        let mut text_right = dot_c.x - 2.5 - 8.0;
        if self.is_pinned(&row.path) {
            // 固定标记（原版 E840 图钉的近似：圆头 + 尖尾，PinMark 色）
            let pc = egui::pos2(text_right - 4.5, rect.center().y);
            painter.circle_filled(pc + Vec2::new(0.0, -1.5), 3.2, pal.pin_mark);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pc + Vec2::new(-2.2, 0.5),
                    pc + Vec2::new(2.2, 0.5),
                    pc + Vec2::new(0.0, 4.5),
                ],
                pal.pin_mark,
                Stroke::NONE,
            ));
            text_right = pc.x - 4.5 - 8.0;
        }

        // 名字（命中段高亮）+ 路径，横向裁剪到右侧簇之前
        let text_x = rect.left() + 60.0 + indent_x;
        let text_clip = egui::Rect::from_min_max(
            egui::pos2(text_x, rect.top()),
            egui::pos2(text_right - 12.0, rect.bottom()),
        );
        let painter = painter.with_clip_rect(text_clip);
        let name_font = serif(14.0);
        let mut job = egui::text::LayoutJob::default();
        // 显示名（v0.1.9 ShellNames 对齐）：解析到替换名（文件夹
        // 本地化名 / .lnk 显示名 / .exe 描述）就用它，并跳过高亮
        // ——高亮区间是按索引名算的字节量，套到替换名上必错位。
        let shell_name = self.names.get(
            &row.path,
            matches!(row.category, Category::Folder | Category::InstallDir),
        );
        let name: &str = shell_name.as_deref().unwrap_or(&row.name);
        // 高亮区间：match_start/match_len 都是字节量，切片前双双
        // 收敛到字符边界（core 语义钉死 + app 侧防御，见 search.rs）
        let hl = if shell_name.is_some() {
            None
        } else if row.match_start != usize::MAX && row.match_len > 0 {
            let s = floor_char_boundary(name, row.match_start);
            let e = floor_char_boundary(name, s + row.match_len);
            if s < e { Some((s, e)) } else { None }
        } else {
            None
        };
        let base_fmt = TextFormat {
            font_id: name_font,
            color: pal.name_text,
            ..Default::default()
        };
        match hl {
            Some((s, e)) => {
                job.append(&name[..s], 0.0, base_fmt.clone());
                job.append(
                    &name[s..e],
                    0.0,
                    TextFormat {
                        font_id: serif(14.0),
                        color: pal.highlight,
                        underline: Stroke::new(1.0_f32, pal.highlight),
                        ..Default::default()
                    },
                );
                job.append(&name[e..], 0.0, base_fmt);
            }
            None => job.append(name, 0.0, base_fmt),
        }
        job.wrap.max_width = f32::INFINITY;
        let galley = painter.layout_job(job);
        painter.galley(egui::pos2(text_x, rect.top() + 8.0), galley, Color32::WHITE);

        let path_galley = painter.layout_no_wrap(
            row.path.clone(),
            FontId::proportional(10.0),
            pal.path_text,
        );
        painter.galley(
            egui::pos2(text_x, rect.top() + 31.0),
            path_galley,
            pal.path_text,
        );

        // 行底发丝线：左 56 起、右 12 止；悬停/选中时隐藏（原版规则）
        if !selected && !response.hovered() {
            ui.painter_at(rect).line_segment(
                [
                    egui::pos2(rect.left() + 56.0 + indent_x, rect.bottom() - 0.5),
                    egui::pos2(rect.right() - 12.0, rect.bottom() - 0.5),
                ],
                Stroke::new(1.0_f32, pal.hairline),
            );
        }

        if response.hovered() {
            self.hover_seen = Some(HoverTarget {
                name: row.name.clone(),
                path: row.path.clone(),
                category: row.category,
            });
        }
        let mut act = None;
        if response.double_clicked() {
            act = Some(RowAction::Open(display_idx));
        } else if response.clicked() {
            act = Some(RowAction::Select(display_idx));
        }
        // 右键菜单（v0.1.9 自绘，见 draw_ctx_menu）
        if response.secondary_clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                self.ctx_menu_born = ui.ctx().input(|i| i.time);
                self.ctx_menu = Some(CtxMenu {
                    pos: p,
                    target: CtxTarget::Result(display_idx),
                });
            }
        }
        act
    }
    /// 折叠组行（原版 FoldRow）：FoldFill 底、圆角 9，内缩近似
    /// 原版 Margin 2,4,6,4；左侧小三角字形（9.5 级 TextLow）+
    /// Serif 12 TextMid 单行文字 = 组头标签（「N 个{类型}」/
    /// 「其他 N 项（…）」）+ 展开状态后缀。组头恒保留：展开后
    /// 再点收起（用户钉死）。选中描 PillStroke 边，悬停叠
    /// SurfaceHover。点击 = 展开/收起（RowAction::Toggle 带组键）。
    fn draw_group_row(
        &mut self,
        ui: &mut egui::Ui,
        display_idx: usize,
        key: &str,
        label: &str,
        count: usize,
        expanded: bool,
    ) -> Option<RowAction> {
        let _ = count; // 计数已含在 label 里，参数留作调试/将来副行用
        let pal = self.display_pal;
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), ROW_H),
            egui::Sense::click(),
        );
        let selected = display_idx == self.selected;
        let painter = ui.painter_at(rect);
        let bg = egui::Rect::from_min_max(
            rect.min + Vec2::new(2.0, 5.0),
            rect.max - Vec2::new(6.0, 5.0),
        );
        self.blockers_cur.push(rect);
        painter.rect_filled(bg, Rounding::same(9.0), pal.fold_fill);
        if selected {
            painter.rect_stroke(
                bg,
                Rounding::same(9.0),
                Stroke::new(1.0_f32, pal.pill_stroke),
            );
        } else {
            let hv = hover_strength(ui, ("group_hover", display_idx), response.hovered());
            if hv > 0.01 {
                painter.rect_filled(
                    bg,
                    Rounding::same(9.0),
                    fade_alpha(pal.surface_hover, hv),
                );
            }
        }

        // 三角字形：收起向右（▸）、展开向下（▾），手绘，TextLow
        let c = egui::pos2(bg.left() + 18.0, bg.center().y);
        let tri = if expanded {
            vec![
                egui::pos2(c.x - 4.5, c.y - 2.5),
                egui::pos2(c.x + 4.5, c.y - 2.5),
                egui::pos2(c.x, c.y + 3.5),
            ]
        } else {
            vec![
                egui::pos2(c.x - 2.5, c.y - 4.5),
                egui::pos2(c.x - 2.5, c.y + 4.5),
                egui::pos2(c.x + 3.5, c.y),
            ]
        };
        painter.add(egui::Shape::convex_polygon(tri, pal.path_text, Stroke::NONE));

        // 单行文字（裁剪到组行右内边距）：组头标签 + 状态后缀
        let text = format!(
            "{label}{}",
            if expanded { " · 点击收起" } else { " · 点击展开" }
        );
        let text_x = bg.left() + 32.0;
        let clip = egui::Rect::from_min_max(
            egui::pos2(text_x, bg.top()),
            egui::pos2(bg.right() - 14.0, bg.bottom()),
        );
        let tp = painter.with_clip_rect(clip);
        let galley = tp.layout_no_wrap(text, serif(12.0), pal.text_mid);
        tp.galley(
            egui::pos2(text_x, bg.center().y - galley.size().y / 2.0),
            galley,
            pal.text_mid,
        );

        // 类型组没有单一路径，不弹悬停详情卡
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.clicked() || response.double_clicked() {
            Some(RowAction::Toggle(display_idx, key.to_string()))
        } else {
            None
        }
    }
    /// 页脚（原版 Footer）：顶部发丝线，其下左 = 状态文字（Sans 10
    /// TextLow，限宽 340、超长省略——索引长摘要只在设置里，状态栏
    /// 永远短格式）、中 = 结果计数「{n} 项 · {ms} ms」、右 = 设置
    /// 按钮（齿轮 11 + 「设置」Sans 10.5 TextMid，悬停 SurfaceHover
    /// 圆角 7）。右侧控件位置恒定，永不被左侧文字挤压重叠（v0.1.5
    /// 真机「状态栏挤成一团」的根治）。主题切换在书眉，不在这里。
    fn draw_status_bar(&mut self, ui: &mut egui::Ui) {
        let pal = self.display_pal;
        // 顶部发丝线（其下留 7px，合计近似原版 Margin 上 11）
        let (line_rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), egui::Sense::hover());
        ui.painter_at(line_rect)
            .rect_filled(line_rect, Rounding::ZERO, pal.hairline);
        ui.add_space(7.0);

        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 20.0), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        // 右：设置按钮（固定宽，先占位，位置恒定）
        let set_w = 62.0;
        let set_rect = egui::Rect::from_min_size(
            egui::pos2(rect.right() - set_w, rect.center().y - 10.0),
            Vec2::new(set_w, 20.0),
        );
        let set_resp = ui.interact(set_rect, ui.id().with("footer_settings"), egui::Sense::click());
        self.blockers_cur.push(set_rect);
        if set_resp.is_pointer_button_down_on() {
            painter.rect_filled(set_rect, Rounding::same(7.0), pal.surface_sel);
        } else if set_resp.hovered() {
            painter.rect_filled(set_rect, Rounding::same(7.0), pal.surface_hover);
        }
        paint_gear(
            &painter,
            egui::pos2(set_rect.left() + 13.0, set_rect.center().y),
            11.0,
            pal.text_mid,
        );
        let footer_label = if self.settings_open { "关闭" } else { "设置" };
        painter.text(
            egui::pos2(set_rect.left() + 25.0, set_rect.center().y),
            Align2::LEFT_CENTER,
            footer_label,
            FontId::proportional(10.5),
            pal.text_mid,
        );
        // 中：结果计数（居中于左右之间；先量宽给左侧截断让位）
        let res_g = if self.result_text.is_empty() {
            None
        } else {
            let g = painter.layout_no_wrap(
                self.result_text.clone(),
                FontId::proportional(10.0),
                pal.status_text,
            );
            painter.galley(
                egui::pos2(
                    rect.center().x - g.size().x / 2.0,
                    rect.center().y - g.size().y / 2.0,
                ),
                g.clone(),
                pal.status_text,
            );
            Some(g)
        };
        // 左：状态文字（限宽 340 且不压中央计数，layout_no_wrap + clip）
        let text_color = if self.status_alert {
            pal.alert_text
        } else {
            pal.status_text
        };
        let status_g = painter.layout_no_wrap(
            self.status.clone(),
            FontId::proportional(10.0),
            text_color,
        );
        let center_left = res_g
            .map(|g| rect.center().x - g.size().x / 2.0 - 8.0)
            .unwrap_or(rect.right() - set_w - 8.0);
        let left_limit = (rect.left() + 340.0).min(center_left).max(rect.left());
        let clip = egui::Rect::from_min_max(
            rect.min,
            egui::pos2(left_limit, rect.bottom()),
        );
        painter.with_clip_rect(clip).galley(
            egui::pos2(rect.left(), rect.center().y - status_g.size().y / 2.0),
            status_g,
            text_color,
        );
        if set_resp
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("设置 / 关闭设置")
            .clicked()
        {
            // v0.1.15：同一个按钮做开/关切换（用户点名「再点一次要能
            // 关掉」）。关闭只清标记，设置视口不再被 show，下一帧即消失。
            if self.settings_open {
                self.close_settings();
            } else {
                self.open_settings();
            }
        }
    }
}

/// ------------------------------------------------------------ 手绘图标
/// 全局 UI 规范：图标图案必须一眼能认出功能，不许用汉字充当图标。
/// 全部用 egui painter 手绘矢量图形，线宽 ≥1.8px、尺寸 ≥14px；
/// 纯图标控件另配 on_hover_text 兜底说明。

/// 漏斗（类型筛选）：上宽下窄 + 下方小竖管，闭合描线。
/// 筛选图标（v0.1.15 重绘）：三条递减横线（标准 filter 语义），
/// 线宽 1.6、行距按 size 等比，最下一条最短。比旧漏斗克制得多，
/// 16px 下也不糊；颜色由调用方给（常态次文字色 / 激活朱砂）。
fn paint_filter_lines(painter: &egui::Painter, c: egui::Pos2, size: f32, color: Color32) {
    let w = size * 0.68;
    let gap = size * 0.20;
    let st = Stroke::new(2.0_f32, color);
    for (i, f) in [1.0f32, 0.68, 0.36].iter().enumerate() {
        let y = c.y - gap + i as f32 * gap;
        let hw = w * f / 2.0;
        painter.line_segment(
            [egui::pos2(c.x - hw, y), egui::pos2(c.x + hw, y)],
            st,
        );
    }
}

/// 文件夹字形（v0.1.15 第四次重绘）：改为**实心**——左上 tab 片
/// 与主体两枚同色圆角矩形拼成经典文件夹轮廓。前三版都走线框，
/// 在 18px 按钮里线距太密、最后糊成一枚空方框；实心块在同尺寸下
/// 轮廓清楚，颜色也能直接承载「已筛选」的朱砂语义。
fn paint_folder_glyph(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let (w, h) = (rect.width(), rect.height());
    let p = |fx: f32, fy: f32| egui::pos2(rect.left() + w * fx, rect.top() + h * fy);
    // tab 片（左上凸起）
    painter.rect_filled(
        egui::Rect::from_min_max(p(0.04, 0.16), p(0.48, 0.40)),
        Rounding::same((w * 0.05).max(1.5)),
        color,
    );
    // 主体
    painter.rect_filled(
        egui::Rect::from_min_max(p(0.04, 0.28), p(0.96, 0.86)),
        Rounding::same((w * 0.09).max(2.0)),
        color,
    );
}

/// 文档字形（首页占位盘用）：竖页 + 右上折角 + 三行正文线，
/// 描边取传入色（调用方给 TextMid，保证在占位盘里清晰可见）。
/// 文档字形：仅非 Windows 演示构建的占位盘用（v0.1.8 图标总规：
/// Windows 上文件类图标只许系统原版，不许自绘冒充，故本函数在
/// Windows 构建中不存在）。
#[cfg(not(windows))]
fn paint_doc_glyph(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let (w, h) = (rect.width(), rect.height());
    let (x0, y0) = (rect.left(), rect.top());
    let stroke = Stroke::new((1.8 * (w / 26.0)).max(1.2), color);
    let fold = w * 0.22;
    let page = egui::Rect::from_min_max(
        egui::pos2(x0 + w * 0.24, y0 + h * 0.06),
        egui::pos2(x0 + w * 0.76, y0 + h * 0.94),
    );
    // 页身轮廓（右上角切掉折角，末点回到起点闭合）
    let outline = vec![
        page.left_top(),
        egui::pos2(page.right() - fold, page.top()),
        egui::pos2(page.right(), page.top() + fold),
        page.right_bottom(),
        page.left_bottom(),
        page.left_top(),
    ];
    painter.add(egui::Shape::line(outline, stroke));
    // 折角的两条边
    painter.line_segment(
        [
            egui::pos2(page.right() - fold, page.top()),
            egui::pos2(page.right() - fold, page.top() + fold),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(page.right() - fold, page.top() + fold),
            egui::pos2(page.right(), page.top() + fold),
        ],
        stroke,
    );
    // 三行正文线
    for i in 0..3 {
        let y = page.top() + h * (0.44 + 0.17 * i as f32);
        painter.line_segment(
            [
                egui::pos2(page.left() + w * 0.09, y),
                egui::pos2(page.right() - w * 0.09, y),
            ],
            stroke,
        );
    }
}

/// 通用文件字形（首页占位盘用）：圆角页身轮廓 + 中间一短横，
/// 与文档字形（折角 + 多行）区分。
/// 通用文件字形：仅非 Windows 演示构建的占位盘用（同 paint_doc_glyph）。
#[cfg(not(windows))]
fn paint_file_glyph(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let (w, h) = (rect.width(), rect.height());
    let (x0, y0) = (rect.left(), rect.top());
    let stroke = Stroke::new((1.8 * (w / 26.0)).max(1.2), color);
    let page = egui::Rect::from_min_max(
        egui::pos2(x0 + w * 0.22, y0 + h * 0.08),
        egui::pos2(x0 + w * 0.78, y0 + h * 0.92),
    );
    painter.rect_stroke(page, Rounding::same((w * 0.06).max(1.0)), stroke);
    let y = y0 + h * 0.5;
    painter.line_segment(
        [egui::pos2(x0 + w * 0.36, y), egui::pos2(x0 + w * 0.64, y)],
        stroke,
    );
}

/// 宫格图形：2×2 四个小方块。
/// 宫格图形：2×2 四个小方块。
fn paint_grid_glyph(painter: &egui::Painter, c: egui::Pos2, size: f32, color: Color32) {
    let sq = size * 0.42;
    let gap = size * 0.14;
    let o = c - Vec2::splat((sq * 2.0 + gap) / 2.0);
    for dx in 0..2 {
        for dy in 0..2 {
            let r = egui::Rect::from_min_size(
                o + Vec2::new(dx as f32 * (sq + gap), dy as f32 * (sq + gap)),
                Vec2::splat(sq),
            );
            painter.rect_filled(r, Rounding::same(2.0), color);
        }
    }
}

/// 列表图形：三条横线 + 左侧小圆点。
fn paint_list_glyph(painter: &egui::Painter, c: egui::Pos2, size: f32, color: Color32) {
    let stroke = Stroke::new(1.8_f32, color);
    let x0 = c.x - size / 2.0;
    let x1 = c.x + size / 2.0;
    for i in -1..=1 {
        let y = c.y + i as f32 * size * 0.34;
        painter.circle_filled(egui::pos2(x0 + 1.5, y), 1.8, color);
        painter.line_segment(
            [egui::pos2(x0 + 5.5, y), egui::pos2(x1, y)],
            stroke,
        );
    }
}

/// 齿轮（设置）：外圈 8 齿（粗辐线）+ 内圈 + 轴心。
fn paint_gear(painter: &egui::Painter, c: egui::Pos2, size: f32, color: Color32) {
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::FRAC_PI_4;
        let dir = egui::vec2(a.cos(), a.sin());
        painter.line_segment(
            [c + dir * (size * 0.24), c + dir * (size * 0.5)],
            Stroke::new(2.4_f32, color),
        );
    }
    painter.circle_stroke(c, size * 0.30, Stroke::new(1.9_f32, color));
    painter.circle_filled(c, 2.0, color);
}

/// 太阳（切到浅色）：圆 + 8 条短辐线。
fn paint_sun(painter: &egui::Painter, c: egui::Pos2, size: f32, color: Color32) {
    painter.circle_stroke(c, size * 0.26, Stroke::new(1.9_f32, color));
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::FRAC_PI_4;
        let dir = egui::vec2(a.cos(), a.sin());
        painter.line_segment(
            [c + dir * (size * 0.36), c + dir * (size * 0.5)],
            Stroke::new(1.8_f32, color),
        );
    }
}

/// 月亮（切到深色）：实心圆被偏移圆以底色扣掉一口的月牙近似
///（cut 传面板底色）。
fn paint_moon(painter: &egui::Painter, c: egui::Pos2, size: f32, color: Color32, cut: Color32) {
    painter.circle_filled(c, size * 0.42, color);
    painter.circle_filled(
        c + Vec2::new(size * 0.18, -size * 0.10),
        size * 0.34,
        cut,
    );
}

/// 窗口控制字形（原版 Segoe Fluent E921/E922/E923/E8BB 的手绘等价）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum WinGlyph {
    Min,
    Max,
    Restore,
    Close,
}

/// 书眉工具按钮（原版 ToolButton）：32×28（v0.1.10 布局收紧，
/// 点击区仍 ≥28 见方），圆角 8；悬停浮层
/// SurfaceHover、按下 SurfaceSel，字形 TextMid → 悬停 TextHi
/// （原版淡入 120/淡出 180ms，egui 无逐按钮不透明度管线，即时
/// 换色近似）。关闭按钮走 CloseHover/ClosePressed/CloseText。
fn header_tool_button(ui: &mut egui::Ui, pal: Palette, glyph: WinGlyph) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(32.0, 28.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    let is_close = glyph == WinGlyph::Close;
    let down = response.is_pointer_button_down_on();
    let (fill, fg): (Option<Color32>, Color32) = if is_close {
        if down {
            (Some(pal.close_pressed), pal.close_text)
        } else if response.hovered() {
            (Some(pal.close_hover), pal.close_text)
        } else {
            (None, pal.text_mid)
        }
    } else if down {
        (Some(pal.surface_sel), pal.name_text)
    } else if response.hovered() {
        (Some(pal.surface_hover), pal.name_text)
    } else {
        (None, pal.text_mid)
    };
    if let Some(f) = fill {
        painter.rect_filled(rect, Rounding::same(8.0), f);
    }
    let c = rect.center();
    let s = 10.0;
    let stroke = Stroke::new(1.3_f32, fg);
    match glyph {
        WinGlyph::Min => {
            painter.line_segment(
                [egui::pos2(c.x - s / 2.0, c.y), egui::pos2(c.x + s / 2.0, c.y)],
                stroke,
            );
        }
        WinGlyph::Max => {
            painter.rect_stroke(
                egui::Rect::from_center_size(c, Vec2::splat(s)),
                Rounding::ZERO,
                stroke,
            );
        }
        WinGlyph::Restore => {
            let back = egui::Rect::from_center_size(
                c + Vec2::new(2.0, -2.0),
                Vec2::splat(s - 2.0),
            );
            let front = egui::Rect::from_center_size(
                c + Vec2::new(-1.5, 1.5),
                Vec2::splat(s - 2.0),
            );
            painter.rect_stroke(back, Rounding::ZERO, stroke);
            // 前框填底色遮住后框的重叠段（底色取当前按钮浮层/面板底近似）
            painter.rect_filled(front, Rounding::ZERO, fill.unwrap_or(pal.panel_fill));
            painter.rect_stroke(front, Rounding::ZERO, stroke);
        }
        WinGlyph::Close => {
            let d = s / 2.0;
            painter.line_segment(
                [egui::pos2(c.x - d, c.y - d), egui::pos2(c.x + d, c.y + d)],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(c.x + d, c.y - d), egui::pos2(c.x - d, c.y + d)],
                stroke,
            );
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// 小芯片骨架（原版 ChipFill 语言：填充 + EdgeLine 1px 描边，
/// 圆角 5；Enter 键帽与工具栏小片共用）。
fn glass_pill(painter: &egui::Painter, rect: egui::Rect, pal: Palette) {
    painter.rect_filled(rect, Rounding::same(5.0), pal.esc_fill);
    painter.rect_stroke(
        rect,
        Rounding::same(5.0),
        Stroke::new(1.0_f32, pal.edge_line),
    );
}

/// 工具栏右端的视图切换按钮：图标 = 当前形态（列表时三横线、
/// 宫格时四方块），文案 = 点后会变成的形态名，两者并存不冲突。
fn view_toggle_button(
    ui: &mut egui::Ui,
    pal: Palette,
    label: &str,
    grid_now: bool,
) -> egui::Response {
    let font = FontId::proportional(11.0);
    let galley = ui.fonts(|f| f.layout_no_wrap(label.to_string(), font, pal.esc_text));
    let size = Vec2::new(16.0 + 5.0 + galley.size().x + 12.0, 22.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter_at(rect);
    glass_pill(&painter, rect, pal);
    let c = egui::pos2(rect.left() + 6.0 + 8.0, rect.center().y);
    if grid_now {
        paint_grid_glyph(&painter, c, 14.0, pal.esc_text);
    } else {
        paint_list_glyph(&painter, c, 14.0, pal.esc_text);
    }
    painter.galley(
        egui::pos2(
            rect.left() + 16.0 + 5.0 + 6.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        pal.esc_text,
    );
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(if grid_now {
            "当前宫格 · 切换到列表"
        } else {
            "当前列表 · 切换到宫格"
        })
}



/// 筛选面板外框（v0.1.10 chips 面板）：MenuFill 底 + MenuStroke
/// 边 + 圆角 12 + 柔和投影 + 内边距 14，与自绘菜单同语言、
/// 尺度按原版筛选面板令牌（面板宽 340 级、内边距宽松）。
fn panel_frame(pal: Palette) -> egui::Frame {
    egui::Frame::none()
        .fill(pal.popup_fill)
        .stroke(Stroke::new(1.0_f32, pal.popup_stroke))
        .rounding(Rounding::same(16.0))
        .shadow(egui::epaint::Shadow {
            offset: Vec2::new(0.0, 6.0),
            blur: 26.0,
            spread: 0.0,
            color: pal.popup_shadow_color,
        })
        .inner_margin(egui::Margin::same(16.0))
}

/// 面板分区标题（v0.1.14 重排）：朱砂短竖条 + 11.5px 次文字，与
/// 下方 chips 留 8px。旧版只是一行 11px 灰字，和 chip 堆一起没有
/// 层级；竖条给分区一个明确起点，扫视时不必逐个读字。
fn section_title(ui: &mut egui::Ui, pal: Palette, text: &str) {
    ui.horizontal(|ui| {
        let (bar, _) = ui.allocate_exact_size(Vec2::new(3.0, 12.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(bar, Rounding::same(1.5), pal.accent);
        ui.add_space(4.0);
        ui.label(RichText::new(text).size(11.5).color(pal.text_mid));
    });
    ui.add_space(8.0);
}

/// 类型面板的单格（v0.1.15）：100×34、圆角 8，左 15px 处一枚 3px
/// 分类色点 + 11.5px 左对齐文字；选中 = 朱砂淡底 + 左缘 2px 朱砂条
/// + 朱砂字。比等宽胶囊更像「设置里的选项」，扫读时也更容易对齐。
fn type_cell(
    ui: &mut egui::Ui,
    pal: Palette,
    label: &str,
    dot: Option<(u8, u8, u8)>,
    selected: bool,
    w: f32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 34.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    let rr = Rounding::same(9.0);
    // v0.1.15：每格都带淡底 + 发丝边（选中 = 朱砂淡底 + 朱砂 35%
    // 边）。没有边界时整块面板只剩几行"色点 + 文字"，读起来像散装
    // 列表；给每格一圈边界，网格才立起来。
    if selected {
        painter.rect_filled(rect, rr, pal.pill_fill);
        painter.rect_stroke(rect, rr, Stroke::new(1.0_f32, pal.pill_stroke));
    } else {
        let bg = if resp.hovered() {
            pal.surface_sel
        } else {
            pal.chip_hover_fill
        };
        painter.rect_filled(rect, rr, bg);
        painter.rect_stroke(rect, rr, Stroke::new(1.0_f32, pal.hairline));
    }
    let dot_col = match dot {
        Some((r, g, b)) => Color32::from_rgb(r, g, b),
        None => {
            if selected {
                pal.accent
            } else {
                pal.path_text
            }
        }
    };
    painter.circle_filled(
        egui::pos2(rect.left() + 15.0, rect.center().y),
        3.0,
        dot_col,
    );
    painter.text(
        egui::pos2(rect.left() + 26.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(11.5),
        if selected { pal.accent } else { pal.text_mid },
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// 自定义文件夹行（v0.1.16）：通栏 34 高、圆角 9，左文件夹图标 +
/// 名称，右端 ✕ 删除区。点名称 = 切换该行是否参与搜索（可多选），
/// 双击 = 在资源管理器打开该文件夹。
/// 返回 (单击, 双击, 点了 ✕)。
fn scope_row(
    ui: &mut egui::Ui,
    pal: Palette,
    label: &str,
    full_path: &str,
    w: f32,
    selected: bool,
) -> (bool, bool, bool) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 34.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    let rr = Rounding::same(9.0);
    if selected {
        painter.rect_filled(rect, rr, pal.pill_fill);
        painter.rect_stroke(rect, rr, Stroke::new(1.0_f32, pal.pill_stroke));
    } else if resp.hovered() {
        painter.rect_filled(rect, rr, pal.chip_hover_fill);
    }
    // 选中态左侧再来一根 3px 竖条，和其他"已选中"语言对齐。
    if selected {
        painter.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(rect.left() + 3.0, rect.center().y - 9.0),
                Vec2::new(3.0, 18.0),
            ),
            Rounding::same(1.5),
            pal.accent,
        );
    }
    let glyph = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 18.0, rect.center().y),
        Vec2::splat(14.0),
    );
    paint_folder_glyph(
        &painter,
        glyph,
        if selected { pal.accent } else { pal.text_mid },
    );
    painter.text(
        egui::pos2(rect.left() + 32.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(12.0),
        if selected { pal.accent } else { pal.text_mid },
    );
    let x_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - 26.0, rect.center().y - 8.0),
        Vec2::splat(16.0),
    );
    let xr = ui.interact(
        x_rect,
        ui.id().with(("ss_scope_row_x", full_path)),
        egui::Sense::click(),
    );
    let xc = if xr.hovered() {
        pal.accent
    } else {
        pal.path_text
    };
    let st = egui::Stroke::new(1.3_f32, xc);
    let c = x_rect.center();
    painter.line_segment(
        [c + Vec2::new(-3.5, -3.5), c + Vec2::new(3.5, 3.5)],
        st,
    );
    painter.line_segment(
        [c + Vec2::new(-3.5, 3.5), c + Vec2::new(3.5, -3.5)],
        st,
    );
    // 先取点击态再 on_hover_text：后者按值消费 Response。
    let row_click = resp.clicked();
    let row_dbl = resp.double_clicked();
    let _ = resp.on_hover_text(format!("{full_path}\n单击选中，双击打开"));
    (row_click, row_dbl, xr.clicked())
}

/// 通栏虚线按钮（v0.1.15）：32 高，「＋ 添加文件夹」这类动作入口
/// 用虚线圈出，和实底卡片区分开，不会误读成已选项。
fn dashed_row(
    ui: &mut egui::Ui,
    pal: Palette,
    label: &str,
    w: f32,
    enabled: bool,
) -> bool {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 32.0), sense);
    let painter = ui.painter_at(rect);
    let rr = Rounding::same(8.0);
    if enabled && resp.hovered() {
        painter.rect_filled(rect, rr, pal.chip_hover_fill);
    }
    let color = if enabled {
        pal.edge_strong
    } else {
        pal.edge_line
    };
    let st = egui::Stroke::new(1.0_f32, color);
    let quad = [
        [rect.left_top(), rect.right_top()],
        [rect.right_top(), rect.right_bottom()],
        [rect.right_bottom(), rect.left_bottom()],
        [rect.left_bottom(), rect.left_top()],
    ];
    for seg in quad {
        for s in egui::Shape::dashed_line(&seg[..], st, 4.0, 3.0) {
            painter.add(s);
        }
    }
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(11.5),
        if enabled { pal.text_mid } else { pal.empty_text },
    );
    resp.clicked()
}

/// 筛选 chip（原版 FilterChip 语言，v0.1.10 面板定稿）：高 30、
/// 全圆角胶囊、字 12、左右内边距 12。未选 = 透明底 + EdgeLine
/// 细描边 + TextMid 字（hover 叠 MenuHover 淡填充）；选中 =
/// PillFill 淡填充（朱砂 ≤12%）+ PillStroke 描边 + AccentText
/// 字——绝不整行实色块、不用勾选符号、不用白填充。选中过渡
/// ~0.15s 插值填充与字色，过渡期 repaint_after(16ms) 续命。
fn filter_chip(ui: &mut egui::Ui, pal: Palette, label: &str, selected: bool) -> egui::Response {
    let font = FontId::proportional(12.0);
    // 选中进度 v：0=未选中，1=完全选中，切换时平滑过渡
    let v = ui.ctx().animate_bool_with_time_and_easing(
        ui.make_persistent_id(("filter_chip_sel", label)),
        selected,
        0.15,
        egui::emath::easing::cubic_out,
    );
    if v > 0.0 && v < 1.0 {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(16));
    }
    let galley = ui.fonts(|f| f.layout_no_wrap(label.to_string(), font, Color32::PLACEHOLDER));
    let size = Vec2::new(galley.size().x + 26.0, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter_at(rect);
    let rounding = Rounding::same(15.0);
    // v0.1.14 换语言：选中 = 朱砂实底 + 白字，未选 = 透明底 + 发丝
    // 描边（hover 才浮出淡底）。旧版「12% 淡填充 + 35% 描边」在白
    // 面板上几乎看不出来，整排 chip 一片灰，层级全靠猜。
    if v > 0.01 {
        let acc = pal.accent;
        let base = if response.hovered() {
            Color32::from_rgb(
                (acc.r() as f32 * 0.88) as u8,
                (acc.g() as f32 * 0.88) as u8,
                (acc.b() as f32 * 0.88) as u8,
            )
        } else {
            acc
        };
        painter.rect_filled(
            rect,
            rounding,
            Color32::from_rgba_unmultiplied(
                base.r(),
                base.g(),
                base.b(),
                (255.0 * v) as u8,
            ),
        );
    } else {
        if response.hovered() {
            painter.rect_filled(rect, rounding, pal.chip_hover_fill);
        }
        painter.rect_stroke(
            rect,
            rounding,
            Stroke::new(
                1.0_f32,
                if response.hovered() {
                    pal.edge_strong
                } else {
                    pal.edge_line
                },
            ),
        );
    }
    let (sr, sg, sb) = pal.chip_text_rgb;
    let text_color = Color32::from_rgb(
        (sr as f32 + (255.0 - sr as f32) * v) as u8,
        (sg as f32 + (255.0 - sg as f32) * v) as u8,
        (sb as f32 + (255.0 - sb as f32) * v) as u8,
    );
    painter.galley(
        egui::pos2(rect.center().x - galley.size().x / 2.0, rect.center().y - galley.size().y / 2.0),
        galley,
        text_color,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// 自定义文件夹 chip：与 filter_chip 同语言（未选态），尾部带
/// × 删除区（点 × 删除、点主体只悬停看全路径）。返回 true =
/// 本帧点了 ×。自定义文件夹不在「选择集」里（build_scope_filter
/// 恒追加），所以没有选中态，× 是它唯一的动作。
fn custom_chip(ui: &mut egui::Ui, pal: Palette, label: &str, full_path: &str) -> bool {
    let font = FontId::proportional(12.0);
    let galley = ui.fonts(|f| f.layout_no_wrap(label.to_string(), font, Color32::PLACEHOLDER));
    // 左 12 + 文字 + 6 + ×区 14 + 右 8
    let size = Vec2::new(galley.size().x + 12.0 + 6.0 + 14.0 + 8.0, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let rounding = Rounding::same(15.0);
    if response.hovered() {
        painter.rect_filled(rect, rounding, pal.chip_hover_fill);
    }
    painter.rect_stroke(rect, rounding, Stroke::new(1.0_f32, pal.edge_line));
    painter.galley(
        egui::pos2(rect.left() + 12.0, rect.center().y - galley.size().y / 2.0),
        galley,
        pal.text_mid,
    );
    // × 删除区：独立点击判定（小号 × 字形，hover 变朱砂）
    let x_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - 22.0, rect.center().y - 7.0),
        Vec2::new(14.0, 14.0),
    );
    let x_resp = ui.interact(x_rect, ui.id().with(("custom_chip_x", full_path)), egui::Sense::click());
    let xc = x_rect.center();
    let xcol = if x_resp.hovered() { pal.accent } else { pal.path_text };
    let st = Stroke::new(1.3_f32, xcol);
    painter.line_segment([xc + Vec2::new(-3.5, -3.5), xc + Vec2::new(3.5, 3.5)], st);
    painter.line_segment([xc + Vec2::new(-3.5, 3.5), xc + Vec2::new(3.5, -3.5)], st);
    let _ = response.on_hover_text(full_path.to_string());
    x_resp.clicked()
}

/// 虚线描边胶囊按钮（「添加文件夹…」）：与 chips 同高同圆角，
/// 虚线边 = 「还能往里加」的语义。禁用态（非 Windows 无 rfd）
/// 只画不响应。返回 true = 本帧点击。
fn dashed_chip(ui: &mut egui::Ui, pal: Palette, label: &str, enabled: bool) -> bool {
    let font = FontId::proportional(12.0);
    let galley = ui.fonts(|f| f.layout_no_wrap(label.to_string(), font, Color32::PLACEHOLDER));
    let size = Vec2::new(galley.size().x + 24.0, 30.0);
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let painter = ui.painter_at(rect);
    if enabled && response.hovered() {
        painter.rect_filled(rect, Rounding::same(15.0), pal.chip_hover_fill);
    }
    // 圆角矩形虚线轮廓：四角小弧采样成闭合折线后 dashed_line
    let r = 15.0_f32;
    let mut pts: Vec<egui::Pos2> = Vec::new();
    let corners = [
        (rect.right() - r, rect.top() + r, -90.0f32),
        (rect.right() - r, rect.bottom() - r, 0.0),
        (rect.left() + r, rect.bottom() - r, 90.0),
        (rect.left() + r, rect.top() + r, 180.0),
    ];
    for (cx, cy, start_deg) in corners {
        for s in 0..=4 {
            let a = (start_deg + s as f32 * 22.5).to_radians();
            pts.push(egui::pos2(cx + r * a.cos(), cy + r * a.sin()));
        }
    }
    pts.push(pts[0]);
    let col = if enabled { pal.edge_strong } else { pal.edge_line };
    for shape in egui::Shape::dashed_line(&pts, Stroke::new(1.0_f32, col), 4.0, 3.0) {
        painter.add(shape);
    }
    let text_color = if enabled { pal.text_mid } else { pal.path_text };
    painter.galley(
        egui::pos2(rect.center().x - galley.size().x / 2.0, rect.center().y - galley.size().y / 2.0),
        galley,
        text_color,
    );
    enabled && response.clicked()
}

impl ShunSeekApp {
    /// 设置窗口（独立 viewport，标题「瞬觅 设置」，520×660，内容
    /// 超出走纵向滚动），不置顶、可关。入口：状态栏齿轮按钮 +
    /// 托盘「设置」项。
    /// 开关类设置全在这里，托盘只放动作，两边不重复。
    fn draw_settings_viewport(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let pal = self.display_pal;
        let mut close_me = false;
        let builder = egui::ViewportBuilder::default()
            .with_title("瞬觅 设置")
            .with_inner_size([520.0, 660.0])
            .with_min_inner_size([440.0, 520.0]);
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("shunseek_settings"),
            builder,
            |vctx, _class| {
                if vctx.input(|i| i.viewport().close_requested()) {
                    close_me = true;
                }
                // 保活（v0.1.14）：immediate viewport 完全靠主窗每帧
                // update 驱动；主窗一旦被 SW_HIDE（点外隐藏/✕），这里
                // 的点击就全部无响应，表现正是「设置窗有时关不掉」。
                // 每 300ms 主动要一帧，另在主窗 hide 时显式关掉本视口。
                vctx.request_repaint_after(std::time::Duration::from_millis(300));
                let frame = egui::Frame::none()
                    .fill(pal.panel_fill)
                    .inner_margin(egui::Margin::symmetric(20.0, 16.0));
                egui::CentralPanel::default().frame(frame).show(vctx, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            self.settings_sections(ui, vctx, pal);
                        });
                });
            },
        );
        if close_me {
            self.close_settings();
        }
    }

    fn settings_sections(&mut self, ui: &mut egui::Ui, vctx: &egui::Context, pal: Palette) {
        // 页面头（v0.1.14）：标题 + 版本 + 一句定位，替代「一进来就是
        // 一排原生复选框」的空洞开场。
        ui.horizontal(|ui| {
            ui.label(RichText::new("设置").size(20.0).color(pal.name_text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                        .size(10.5)
                        .color(pal.path_text),
                );
            });
        });
        ui.add_space(3.0);
        ui.label(
            RichText::new("索引、外观与数据管理")
                .size(11.5)
                .color(pal.path_text),
        );
        // ---------- 常规 ----------
        settings_heading(ui, pal, "常规");
        // 开机自启动：勾选状态以注册表实际为准（不入 settings.txt）
        let mut auto = self.settings_autostart;
        let auto_before = auto;
        ss_row_switch(ui, pal, "开机自启动", &mut auto);
        if auto != auto_before {
            autostart::set_enabled(auto);
            self.settings_autostart = autostart::is_enabled();
        }
        // 点外自动隐藏（v0.1.14 新增，缺省关）：勾上才恢复原版
        //「点窗口外即收进托盘」的行为，不勾则窗口常驻，点别的窗口
        // 不会连任务栏图标一起消失。
        let mut hide_outside = self.hide_outside;
        let hide_before = hide_outside;
        ss_row_switch(ui, pal, "点击窗口外自动隐藏", &mut hide_outside);
        if hide_outside != hide_before {
            self.hide_outside = hide_outside;
            self.save_settings();
            if !hide_outside {
                clickwatch::disarm();
            }
        }
        ui.add_space(4.0);
        // 关窗行为（v0.1.6 用户钉死，不再是设置项）：✕ 恒最小化到
        // 托盘，进程的唯一正常退出 = 托盘菜单「退出」。只放说明。
        ui.label(
            RichText::new("关闭窗口（✕）将最小化到托盘；退出程序请用托盘菜单的「退出」。")
                .size(11.0)
                .color(pal.path_text),
        );
        ui.add_space(6.0);
        // 唤醒热键：显示当前组合 +「修改」进捕获态；候选立即跑
        // 冲突检测（名单 + RegisterHotKey 探测），通过才保存生效。
        ui.label(RichText::new("唤醒热键").size(12.5).color(pal.name_text));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            // 当前组合按「键帽」画：淡底 + 发丝描边 + 圆角 8，
            // 比一行裸文字更像快捷键，也与主窗 Enter 键帽同语言。
            let g = ui.fonts(|f| {
                f.layout_no_wrap(
                    self.hotkey_combo.display(),
                    FontId::proportional(13.0),
                    pal.name_text,
                )
            });
            let (r, _) = ui.allocate_exact_size(
                Vec2::new(g.size().x + 20.0, 28.0),
                egui::Sense::hover(),
            );
            ui.painter()
                .rect_filled(r, Rounding::same(8.0), pal.surface_sel);
            ui.painter().rect_stroke(
                r,
                Rounding::same(8.0),
                Stroke::new(1.0_f32, pal.edge_line),
            );
            ui.painter().galley(
                egui::pos2(
                    r.center().x - g.size().x / 2.0,
                    r.center().y - g.size().y / 2.0,
                ),
                g,
                pal.name_text,
            );
            ui.add_space(6.0);
            if self.hotkey_capture {
                ui.label(
                    RichText::new("请按下快捷键… Esc 取消").color(pal.highlight),
                );
            } else if ss_button(ui, pal, "修改", false) {
                self.hotkey_capture = true;
                self.hotkey.set_paused(true);
                self.hotkey_note.clear();
                self.hotkey_recs.clear();
            }
        });
        if self.hotkey_combo == hotkey::HotkeyCombo::ALT_SPACE {
            ui.label(
                RichText::new(
                    "提示：Alt+Space 与系统窗口菜单共用，在其他窗口聚焦时可能弹出该窗口的菜单。",
                )
                .size(11.0)
                .color(pal.path_text),
            );
        }
        if self.hotkey_capture {
            self.poll_hotkey_capture(vctx);
        }
        if !self.hotkey_note.is_empty() {
            ui.label(
                RichText::new(&self.hotkey_note)
                    .size(11.5)
                    .color(pal.path_text),
            );
        }
        if !self.hotkey_recs.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("推荐：").size(11.5).color(pal.text_mid));
                let mut pick: Option<hotkey::HotkeyCombo> = None;
                for c in &self.hotkey_recs {
                    if filter_chip(ui, pal, &c.display(), false).clicked() {
                        pick = Some(*c);
                    }
                }
                if let Some(c) = pick {
                    self.apply_hotkey(c, format!("已设置唤醒热键：{}", c.display()));
                }
            });
        }

        // ---------- 外观 ----------
        settings_heading(ui, pal, "外观");
        ui.horizontal(|ui| {
            ui.label(RichText::new("主题").size(12.5).color(pal.name_text));
            ui.add_space(10.0);
            let sel = if self.theme == Theme::Dark { 0 } else { 1 };
            if let Some(i) = ss_segments(ui, pal, "theme", &["深色", "浅色"], sel) {
                let t = if i == 0 { Theme::Dark } else { Theme::Light };
                if t != self.theme {
                    self.set_theme(vctx, t);
                }
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            // 与主页面宫格/列表按钮共用同一状态与持久化键，页面按钮
            // 是主控件，这里只是「启动时默认显示」的默认项入口。
            ui.label(RichText::new("启动时默认显示").size(12.5).color(pal.name_text));
            ui.add_space(10.0);
            let sel = if self.view_mode == ViewMode::List { 0 } else { 1 };
            if let Some(i) = ss_segments(ui, pal, "view", &["列表", "宫格"], sel) {
                let m = if i == 0 { ViewMode::List } else { ViewMode::Grid };
                if m != self.view_mode {
                    self.set_view_mode(m);
                }
            }
        });

        // ---------- 搜索 ----------
        settings_heading(ui, pal, "搜索");
        ui.horizontal(|ui| {
            ui.label(RichText::new("结果上限").size(12.5).color(pal.name_text));
            ui.add_space(10.0);
            let sel = match self.result_limit {
                100 => 1,
                200 => 2,
                _ => 0,
            };
            if let Some(i) = ss_segments(ui, pal, "limit", &["60", "100", "200"], sel) {
                let n = [60usize, 100, 200][i];
                if self.result_limit != n {
                    self.result_limit = n;
                    self.save_settings();
                    self.schedule_search();
                }
            }
        });
        ui.add_space(4.0);
        let mut fold = self.fold_demoted;
        let fold_before = fold;
        ss_row_switch(ui, pal, "折叠不重要结果", &mut fold);
        if fold != fold_before {
            self.fold_demoted = fold;
            self.save_settings();
            self.rebuild_display();
        }

        // ---------- 索引 ----------
        settings_heading(ui, pal, "索引");
        let status = if self.index_summary.is_empty() {
            "尚未建立".to_string()
        } else {
            format!("共 {} 条 · {}", self.index_count, self.index_summary)
        };
        ui.label(RichText::new(status).size(11.5).color(pal.text_mid));
        // 图标管线健康度（v0.1.6 可观测性）：真机排障时看这一行就
        // 知道 Shell 取图是全灭还是个别失败，不用再猜。
        let (icon_req, icon_ok, icon_fail) = self.icons.stats();
        ui.label(
            RichText::new(format!(
                "图标：已加载 {icon_ok} · 失败 {icon_fail}（已请求 {icon_req}）"
            ))
            .size(11.5)
            .color(pal.path_text),
        );
        ui.horizontal(|ui| {
            let is_rebuilding = self.rebuilding.load(Ordering::SeqCst);
            let rebuild_label = if is_rebuilding {
                "正在建立索引…"
            } else {
                "重新建立索引"
            };
            if ss_button(ui, pal, rebuild_label, false) && !is_rebuilding {
                self.start_rebuild();
            }
            if ss_button(ui, pal, "打开缓存文件夹", false) {
                let dir = self.data_dir.to_string_lossy().into_owned();
                opener::open(&dir, "", false);
            }
            if ss_button(ui, pal, "清除索引缓存", true) {
                {
                    let mut g = lock_rec(&self.store);
                    if g.is_mapped() {
                        *g = Arc::new(g.to_owned_store());
                    }
                }
                for f in ["index.bin", "index-next.bin", "index.tmp", "index-next.tmp"] {
                    let _ = std::fs::remove_file(self.data_dir.join(f));
                }
                if self.data_dir.join("index.bin").exists() {
                    self.settings_note = "索引缓存文件正被占用，请稍后再试".to_string();
                } else {
                    self.settings_note =
                        "索引缓存已清除，下次启动将重新建立（当前索引仍可继续使用）".to_string();
                }
            }
        });

        // ---------- 数据 ----------
        settings_heading(ui, pal, "数据");
        // 确认态走应用胶囊语言（v0.1.9）：确认 = 选中态胶囊
        // （朱砂淡填充，表「已武装的危险动作」），取消 = 普通
        // 胶囊；不再用 egui 默认灰按钮
        if self.usage_confirm {
            ui.horizontal(|ui| {
                if filter_chip(ui, pal, "确认清除？", true).clicked() {
                    {
                        let mut u = lock_rec(&self.usage);
                        u.clear();
                        let _ = u.save();
                    }
                    self.usage_confirm = false;
                    self.settings_note = "使用记录已清除，常用统计从零开始".to_string();
                    self.schedule_search();
                }
                if filter_chip(ui, pal, "取消", false).clicked() {
                    self.usage_confirm = false;
                }
            });
        } else if filter_chip(ui, pal, "清除使用记录", false).clicked() {
            self.usage_confirm = true;
        }

        // ---------- 关于 ----------
        settings_heading(ui, pal, "关于");
        ui.label(RichText::new(about_text()).color(pal.name_text));
        if !self.settings_note.is_empty() {
            ui.add_space(8.0);
            ui.label(
                RichText::new(&self.settings_note)
                    .size(11.5)
                    .color(pal.highlight),
            );
        }
        ui.add_space(12.0);
    }


    /// 热键捕获态轮询：读设置窗口这一帧的原始按键事件。Esc 取消；
    /// 修饰键 + 主键成组合后立即跑冲突检测，通过（可用 / 共用）
    /// 才保存生效；冲突 / 被占用则给出原因 + 过滤后的推荐列表。
    fn poll_hotkey_capture(&mut self, ctx: &egui::Context) {
        let events: Vec<egui::Event> = ctx.input(|i| i.raw.events.clone());
        for ev in &events {
            if let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = ev
            {
                if *key == Key::Escape {
                    self.hotkey_capture = false;
                    self.hotkey.set_paused(false);
                    self.hotkey_note = "已取消修改".to_string();
                    return;
                }
                let Some(vk) = egui_key_to_vk(*key) else {
                    continue;
                };
                let combo = hotkey::HotkeyCombo::new(
                    modifiers.ctrl,
                    modifiers.alt,
                    modifiers.shift,
                    hotkey::win_key_down(),
                    vk,
                );
                if !(combo.ctrl || combo.alt || combo.shift || combo.win) {
                    self.hotkey_note =
                        "需要至少一个修饰键（Ctrl / Alt / Shift / Win）+ 主键".to_string();
                    continue;
                }
                self.hotkey_capture = false;
                self.apply_hotkey_check(combo);
                self.hotkey.set_paused(false);
                return;
            }
        }
    }

    fn apply_hotkey_check(&mut self, combo: hotkey::HotkeyCombo) {
        match hotkey::check(combo) {
            hotkey::HotkeyCheck::Available => {
                self.apply_hotkey(combo, format!("已设置唤醒热键：{}（可用）", combo.display()));
            }
            hotkey::HotkeyCheck::Shared(reason) => {
                self.apply_hotkey(
                    combo,
                    format!("已设置唤醒热键：{}。注意：{reason}", combo.display()),
                );
            }
            hotkey::HotkeyCheck::Conflict(reason) => {
                self.hotkey_note = format!("{} 不可用：{reason}", combo.display());
                self.refresh_hotkey_recs();
            }
            hotkey::HotkeyCheck::Occupied => {
                self.hotkey_note = format!("{} 已被其他程序占用", combo.display());
                self.refresh_hotkey_recs();
            }
        }
    }

    fn apply_hotkey(&mut self, combo: hotkey::HotkeyCombo, note: String) {
        self.hotkey_combo = combo;
        self.hotkey.set_combo(combo);
        self.save_settings();
        self.hotkey_note = note;
        self.hotkey_recs.clear();
    }

    /// 推荐列表：固定候选逐个跑同一套冲突检测，只展示检测可用的。
    fn refresh_hotkey_recs(&mut self) {
        self.hotkey_recs = hotkey::RECOMMEND_CANDIDATES
            .iter()
            .copied()
            .filter(|c| {
                *c != self.hotkey_combo
                    && hotkey::check(*c) == hotkey::HotkeyCheck::Available
            })
            .collect();
    }

    /// 悬浮详情卡（列表 / 宫格共用）：行/格绘制时把悬停目标写进
    /// hover_seen，这里消费并计时——同一目标悬停满 ~0.4s 后在鼠标
    /// 旁弹出玻璃卡（全名 / 完整路径 / 类型 / 大小 / 修改时间）。
    /// 元数据按路径按需取、含失败结果一起缓存，不反复打磁盘。
    fn update_hover_card(&mut self, ctx: &egui::Context) {
        let seen = self.hover_seen.take();
        match &seen {
            Some(t) => {
                let same = self
                    .hover_target
                    .as_ref()
                    .map(|h| h.path == t.path)
                    .unwrap_or(false);
                if !same {
                    self.hover_since = ctx.input(|i| i.time);
                }
                self.hover_target = Some(t.clone());
            }
            None => {
                self.hover_target = None;
                return;
            }
        }
        let target = self.hover_target.clone().unwrap();
        let elapsed = ctx.input(|i| i.time) - self.hover_since;
        if elapsed < 0.4 {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
            return;
        }
        if !self.meta_cache.contains_key(&target.path) {
            let meta = std::fs::metadata(&target.path).ok().map(|m| FileMeta {
                is_dir: m.is_dir(),
                size: m.len(),
                modified: m.modified().ok(),
            });
            self.meta_cache.insert(target.path.clone(), meta);
        }
        let meta = self.meta_cache.get(&target.path).cloned().unwrap_or(None);
        let pal = self.display_pal;
        let Some(pointer) = ctx.input(|i| i.pointer.hover_pos()) else {
            return;
        };
        let screen = ctx.input(|i| i.screen_rect());
        let card_w = 330.0;
        let mut pos = pointer + Vec2::new(16.0, 18.0);
        if pos.x + card_w > screen.right() {
            pos.x = (pointer.x - card_w - 8.0).max(screen.left() + 4.0);
        }
        if pos.y + 130.0 > screen.bottom() {
            pos.y = (pointer.y - 130.0).max(screen.top() + 4.0);
        }
        egui::Area::new(egui::Id::new("hover_detail_card"))
            .order(egui::Order::Tooltip)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(pal.popup_fill)
                    .stroke(Stroke::new(1.0_f32, pal.popup_stroke))
                    .rounding(Rounding::same(10.0))
                    .inner_margin(egui::Margin::same(12.0))
                    .shadow(egui::epaint::Shadow {
                        offset: Vec2::new(0.0, 6.0),
                        blur: 18.0,
                        spread: 0.0,
                        color: pal.popup_shadow_color,
                    })
                    .show(ui, |ui| {
                        ui.set_width(card_w - 24.0);
                        ui.label(
                            RichText::new(&target.name)
                                .size(14.0)
                                .color(pal.name_text),
                        );
                        ui.label(
                            RichText::new(&target.path)
                                .size(11.5)
                                .color(pal.path_text),
                        );
                        ui.add_space(4.0);
                        let is_dir = meta
                            .as_ref()
                            .map(|m| m.is_dir)
                            .unwrap_or(target.category == Category::Folder);
                        // 类型用具体类型名（v0.1.8 filetype，如
                        // 「PDF 文档」「快捷方式」），不再是粗分类名
                        let type_name = filetype::describe(
                            &target.path,
                            target.category,
                            is_dir,
                        )
                        .0;
                        ui.label(
                            RichText::new(format!("类型：{type_name}"))
                                .size(11.5)
                                .color(pal.path_text),
                        );
                        if let Some(m) = &meta {
                            if !m.is_dir {
                                ui.label(
                                    RichText::new(format!(
                                        "大小：{}",
                                        format_size(m.size)
                                    ))
                                    .size(11.5)
                                    .color(pal.path_text),
                                );
                            }
                            if let Some(s) = m.modified.and_then(format_modified) {
                                ui.label(
                                    RichText::new(format!("修改：{s}"))
                                        .size(11.5)
                                        .color(pal.path_text),
                                );
                            }
                        }
                    });
            });
    }
}

/// 设置窗口的分区标题（v0.1.14）：朱砂短竖条 + 13px 主文字，
/// 右侧一条发丝线把剩余宽度填满——把一长条原生控件流切成清晰
/// 的段，扫视时不必逐个读字。
fn settings_heading(ui: &mut egui::Ui, pal: Palette, title: &str) {
    ui.add_space(22.0);
    ui.horizontal(|ui| {
        let (bar, _) = ui.allocate_exact_size(Vec2::new(3.0, 16.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(bar, Rounding::same(1.5), pal.accent);
        ui.add_space(7.0);
        ui.label(RichText::new(title).size(14.0).color(pal.name_text));
        let rest = ui.available_width() - 2.0;
        if rest > 20.0 {
            let (line, _) = ui.allocate_exact_size(Vec2::new(rest, 1.0), egui::Sense::hover());
            ui.painter().rect_filled(line, Rounding::ZERO, pal.hairline);
        }
    });
    ui.add_space(10.0);
}

/// 自绘开关（v0.1.14）：40×22 胶囊，选中朱砂实底 + 白色圆钮，
/// 切换 0.14s 插值。替代 egui 默认的小灰方块复选框——后者在
/// 白底上几乎看不出开/关，也不跟主界面同一套圆角语言。
fn ss_switch(
    ui: &mut egui::Ui,
    pal: Palette,
    id_key: &str,
    value: &mut bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(40.0, 22.0), egui::Sense::click());
    let v = ui.ctx().animate_bool_with_time_and_easing(
        ui.make_persistent_id(("ss_switch", id_key)),
        *value,
        0.14,
        egui::emath::easing::cubic_out,
    );
    if v > 0.0 && v < 1.0 {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(16));
    }
    let painter = ui.painter_at(rect);
    let track = rect.shrink2(Vec2::new(0.0, 1.0));
    let rounding = Rounding::same(track.height() / 2.0);
    let cold = pal.chip_hover_fill;
    let hot = pal.accent;
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * v) as u8;
    painter.rect_filled(
        track,
        rounding,
        Color32::from_rgb(
            mix(cold.r(), hot.r()),
            mix(cold.g(), hot.g()),
            mix(cold.b(), hot.b()),
        ),
    );
    painter.rect_stroke(
        track,
        rounding,
        Stroke::new(1.0_f32, pal.edge_line),
    );
    let cx = track.left() + track.height() / 2.0 + (track.width() - track.height()) * v;
    painter.circle_filled(
        egui::pos2(cx, track.center().y),
        track.height() / 2.0 - 3.0,
        Color32::WHITE,
    );
    if resp.clicked() {
        *value = !*value;
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// 设置页一行「左侧文字 + 右侧开关」：开关贴右边界，行高 30。
fn ss_row_switch(
    ui: &mut egui::Ui,
    pal: Palette,
    label: &str,
    value: &mut bool,
) {
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), 36.0),
        egui::Sense::hover(),
    );
    ui.painter().text(
        egui::pos2(rect.left() + 1.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.5),
        pal.name_text,
    );
    let sw = egui::Rect::from_min_size(
        egui::pos2(
            ui.max_rect().right() - 40.0,
            rect.center().y - 11.0,
        ),
        Vec2::new(40.0, 22.0),
    );
    if ui
        .interact(
            sw,
            ui.make_persistent_id(("ss_switch_hit", label)),
            egui::Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        *value = !*value;
    }
    let v = ui.ctx().animate_bool_with_time_and_easing(
        ui.make_persistent_id(("ss_switch", label)),
        *value,
        0.14,
        egui::emath::easing::cubic_out,
    );
    if v > 0.0 && v < 1.0 {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(16));
    }
    let painter = ui.painter_at(sw);
    let rounding = Rounding::same(sw.height() / 2.0);
    let cold = pal.chip_hover_fill;
    let hot = pal.accent;
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * v) as u8;
    painter.rect_filled(
        sw,
        rounding,
        Color32::from_rgb(
            mix(cold.r(), hot.r()),
            mix(cold.g(), hot.g()),
            mix(cold.b(), hot.b()),
        ),
    );
    painter.rect_stroke(sw, rounding, Stroke::new(1.0_f32, pal.edge_line));
    let cx = sw.left() + sw.height() / 2.0 + (sw.width() - sw.height()) * v;
    painter.circle_filled(
        egui::pos2(cx, sw.center().y),
        sw.height() / 2.0 - 3.0,
        Color32::WHITE,
    );
}

/// 分段选择器（v0.1.14）：整条胶囊容器 + 选中段朱砂实底白字，
/// 替代 egui::selectable_label 的蓝色方块高亮。
fn ss_segments(
    ui: &mut egui::Ui,
    pal: Palette,
    id_key: &str,
    labels: &[&str],
    selected: usize,
) -> Option<usize> {
    let font = FontId::proportional(13.0);
    let galleys: Vec<_> = labels
        .iter()
        // v0.1.15 修「看不清」：layout 时不能写死颜色，否则选中段
        // 是「灰字压在朱砂底」——galley 里已着色的 run 不会接受
        // painter.galley 的 fallback 色。用 PLACEHOLDER 让颜色交给
        // 绘制端决定，选中段才真正是白字。
        .map(|l| {
            ui.fonts(|f| {
                f.layout_no_wrap(l.to_string(), font.clone(), Color32::PLACEHOLDER)
            })
        })
        .collect();
    let widths: Vec<f32> = galleys.iter().map(|g| g.size().x + 26.0).collect();
    let w: f32 = widths.iter().sum::<f32>() + 6.0;
    let h = 32.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, h), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let rounding = Rounding::same(h / 2.0);
    painter.rect_filled(rect, rounding, pal.surface_sel);
    painter.rect_stroke(rect, rounding, Stroke::new(1.0_f32, pal.hairline));
    let mut clicked = None;
    let mut x = rect.left() + 3.0;
    for (i, g) in galleys.iter().enumerate() {
        let seg = egui::Rect::from_min_size(
            egui::pos2(x, rect.top() + 3.0),
            Vec2::new(widths[i], h - 6.0),
        );
        let resp = ui.interact(
            seg,
            ui.make_persistent_id(("ss_seg", id_key, i)),
            egui::Sense::click(),
        );
        let sr = Rounding::same(seg.height() / 2.0);
        if i == selected {
            painter.rect_filled(seg, sr, pal.accent);
        } else if resp.hovered() {
            painter.rect_filled(seg, sr, pal.chip_hover_fill);
        }
        let col = if i == selected {
            Color32::WHITE
        } else {
            pal.text_mid
        };
        painter.galley(
            egui::pos2(
                seg.center().x - g.size().x / 2.0,
                seg.center().y - g.size().y / 2.0,
            ),
            g.clone(),
            col,
        );
        if resp.clicked() {
            clicked = Some(i);
        }
        // v0.1.15 修：段位置必须逐段累加，否则所有段叠在同一格，
        // 后画的选中段把前面几段的文字整个盖住（真机只剩一枚
        // 孤零零的红胶囊，右边一片空白）。
        x += widths[i];
    }
    clicked
}

/// 自绘胶囊按钮（v0.1.14）：高 30、全圆角，淡底 + 发丝描边，
/// hover 浮出 SurfaceHover；danger = 朱砂文字 + 朱砂 35% 描边。
fn ss_button(ui: &mut egui::Ui, pal: Palette, label: &str, danger: bool) -> bool {
    let font = FontId::proportional(13.0);
    let galley = ui.fonts(|f| f.layout_no_wrap(label.to_string(), font, Color32::PLACEHOLDER));
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(galley.size().x + 30.0, 32.0),
        egui::Sense::click(),
    );
    let painter = ui.painter_at(rect);
    let rounding = Rounding::same(15.0);
    if resp.hovered() {
        painter.rect_filled(
            rect,
            rounding,
            if danger { pal.pill_fill } else { pal.surface_hover },
        );
    } else {
        painter.rect_filled(rect, rounding, pal.surface_sel);
    }
    painter.rect_stroke(
        rect,
        rounding,
        Stroke::new(
            1.0_f32,
            if danger { pal.pill_stroke } else { pal.edge_line },
        ),
    );
    painter.galley(
        egui::pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        if danger { pal.accent } else { pal.text_mid },
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// egui 按键 → Windows 虚拟键码（热键捕获用；经 Debug 名映射，
/// 只认字母 / 数字 / 空格 / Tab / F1–F12，其余不作主键）。
fn egui_key_to_vk(key: Key) -> Option<u32> {
    let name = format!("{key:?}");
    match name.as_str() {
        "Space" => Some(0x20),
        "Tab" => Some(0x09),
        s if s.len() == 1 => {
            let b = s.as_bytes()[0];
            if b.is_ascii_alphanumeric() {
                Some(b.to_ascii_uppercase() as u32)
            } else {
                None
            }
        }
        s if s.starts_with("Num") && s.len() == 4 => Some(s.as_bytes()[3] as u32),
        s if s.starts_with('F') => s[1..]
            .parse::<u32>()
            .ok()
            .filter(|n| (1..=12).contains(n))
            .map(|n| 0x6F + n),
        _ => None,
    }
}

impl eframe::App for ShunSeekApp {
    /// 清屏色全透明（v0.1.10 四角根治之一）：eframe 默认 clear
    /// 色是 (12,12,12,α180)——凡是自有绘制没盖到的像素（圆角外、
    /// 弦弧楔）都会被它染成半透明黑灰，在浅色背景上就是一块
    /// 均匀的深色楔（用户截图右下角采样 (79,80,81)，恰是该色
    /// 在 ~240 亮底上的合成值，分毫不差）。透明窗口的清屏色
    /// 必须是全透明，圆角外该露什么露什么。
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 帧首快照：本帧绘制前的筛选展开状态。帧末的点外关闭
        // 只处理「帧首就展开、本帧没被圆点 toggle 改动」的面板
        // ——快速点击（press+release 落在同一帧）时 toggle 在
        // 绘制中刚把面板打开，帧末若拿同一个 press 再判点外，
        // 会把刚开的面板当场关掉（回归实测：面板永远打不开）。
        let expanded_at_frame_start = self.expanded_filter;
        // HWND 兜底重取（v0.1.12）：App::new 里从 CreationContext
        // 没取到时，这里从 Frame 再取一次（两处都实现了
        // HasWindowHandle）。存入后恒非 0，本分支永不再走。
        #[cfg(windows)]
        if winvis::hwnd() == 0 {
            use raw_window_handle::HasWindowHandle;
            if let Ok(handle) = _frame.window_handle() {
                if let raw_window_handle::RawWindowHandle::Win32(w) =
                    handle.as_raw()
                {
                    winvis::store_hwnd(w.hwnd.get());
                }
            }
        }
        // 可见性影子与 OS 对齐（v0.1.12）：先同步、再发布点外
        // 判定矩形——武装位要的是同步后的可见状态。跃迁到
        // Hidden 时 sync 内部已跑 on_hidden 清理（外部线程直接
        // 藏窗时 UI 没有别的入口知道）。
        self.sync_shadow_from_os();
        // 全局点外隐藏（v0.1.10 几何判定、v0.1.12 执行点前移）：
        // 钩子线程命中点外时**当场** ShowWindowAsync(SW_HIDE)
        // 把窗口藏掉（见 clickwatch 模块头），UI 这里不再有
        // 「消费标志再 hide()」的排队环节——那正是旧链的唤醒
        // 依赖，也是 v0.1.11「托盘呼出被陈旧标志同帧反杀」的
        // 战场：钩子能当场隐藏时根本不置标志（标志只在 HWND
        // 未存入的降级情形兜底），反杀的弹药从源头消失。
        // 这里剩下的只有：发布本帧矩形 + 隐藏期间清账 + 上述
        // 降级兜底的一次消费。
        self.publish_click_bounds(ctx);
        if self.visible {
            if self.clickwatch.take_outside() {
                self.hide(ctx);
            }
        } else {
            // 隐藏期间攒下的点外标志直接丢弃，别坑下一次呼出
            self.clickwatch.drain();
        }
        // --tray 静默启动（v0.1.8）：首帧把窗口藏进托盘（窗口是
        // 先建成再隐藏的，ViewportBuilder 没有初始不可见选项）
        if START_HIDDEN.swap(false, Ordering::SeqCst) {
            self.hide(ctx);
        }
        // 启动自愈（v0.1.15）：见 boot_show_frames 字段说明。放在
        // --tray 分支之后，静默启动不会被这里提前弹窗。
        if self.boot_show_frames > 0 {
            self.boot_show_frames -= 1;
            #[cfg(windows)]
            if matches!(winvis::state(), winvis::VisState::Hidden) {
                winvis::show();
                self.visible = true;
            }
        }
        if let Some(p) = self.probe.take() {
            match p.as_str() {
                "type" => self.expanded_filter = Some(FilterKind::Type),
                "scope" => self.expanded_filter = Some(FilterKind::Scope),
                "settings" => self.open_settings(),
                // 自检探针：范围切到「仅自定义文件夹」并搜一次，
                // 用来端到端确认搜索结果确实被范围过滤住。
                "scope-custom" => {
                    self.scope_custom = vec!["E:\\project_开\\ShunSeek".to_string()];
                    self.scope_custom_sel = self.scope_custom.iter().cloned().collect();
                    self.scope_presets.clear();
                    self.scope_drives.clear();
                    self.query = "shun".to_string();
                    self.schedule_search();
                }
                // 自检探针：两个自定义根多选（验证多根并集过滤）。
                "scope-multi" => {
                    self.scope_custom = vec![
                        "E:\\project_开\\ShunSeek".to_string(),
                        "D:\\Doubao_work".to_string(),
                    ];
                    self.scope_custom_sel = self.scope_custom.iter().cloned().collect();
                    self.scope_presets.clear();
                    self.scope_drives.clear();
                    self.query = "shun".to_string();
                    self.expanded_filter = Some(FilterKind::Scope);
                    self.schedule_search();
                }
                // 自检探针：不改范围，只搜一个中文词，验证自定义根内的
                // 中文文件能被索引命中。
                "scope-kcb" => {
                    self.query = "课表".to_string();
                    self.schedule_search();
                }
                _ => {}
            }
        }
        // 单实例唤醒（v0.1.8 建链、v0.1.12 换执行点）：等待线程
        // 已在事件到达时直接 winvis::show() 把窗口呼出来，这里
        // 只补呼出副作用（重搜/全选/聚焦请求）+ 清账。
        if WAKE_FLAG.swap(false, Ordering::SeqCst) {
            self.after_external_summon();
        }
        // 热键（v0.1.12）：轮询线程已在按下边沿直接
        // winvis::toggle() 改完可见性，这里只补副作用/清理。
        // 托盘同理（见下方分发各臂）——可见性不再经 UI 队列，
        // 本帧处理到时窗口早已显示/隐藏，repaint 只管副作用。
        if self.hotkey.take_fired() {
            self.after_external_summon();
        }
        while let Some(action) = self.tray.poll() {
            match action {
                tray::TrayAction::Toggle => {
                    // 托盘线程已在图标回调里直接 winvis::toggle()
                    // 按 OS 状态改完可见性（见 tray.rs），这里只
                    // 补呼出副作用/隐藏清理 + 清账。
                    self.after_external_summon();
                }
                tray::TrayAction::Show => {
                    // 托盘线程已在菜单分发时直接 winvis::show()
                    // 呼出（SW_RESTORE/SW_SHOW + SetForegroundWindow，
                    // 不经 UI 队列——旧版在这里发三件套
                    // ViewportCommand 的前提是 UI 先被 repaint
                    // 唤醒，正是六版真机全灭的病根，已整段删除）。
                    self.after_external_summon();
                }
                tray::TrayAction::RebuildIndex => self.start_rebuild(),
                tray::TrayAction::RefreshIndex => {
                    if self.watch_active.load(Ordering::SeqCst) {
                        self.flush_now.store(true, Ordering::SeqCst);
                    } else {
                        self.start_rebuild();
                    }
                }
                tray::TrayAction::OpenSettings => {
                    // 托盘线程已直接 winvis::show() 呼出主窗；
                    // 设置 viewport 只能由 UI 线程绘制，这里补
                    // 呼出副作用后打开它。窗口已被 ShowWindow
                    // 显示，winit 事件循环有系统消息驱动，本帧
                    // 必然被处理——与旧链「隐藏态靠 repaint 唤醒」
                    // 有本质区别。
                    self.after_external_summon();
                    self.open_settings();
                }
                tray::TrayAction::About => {
                    // 托盘线程已直接 winvis::show() 呼出主窗，
                    // 这里只补副作用并写状态栏。
                    self.after_external_summon();
                    // 版本信息临时显示在状态栏（下一次搜索完成会覆盖）
                    self.status = about_text();
                }
                tray::TrayAction::AdminRestart => {
                    #[cfg(windows)]
                    {
                        if restart_elevated() {
                            self.tray.shutdown_sync();
                            std::process::exit(0);
                        } else {
                            self.status = "管理员授权未完成，已取消重启".to_string();
                        }
                    }
                }
                tray::TrayAction::Exit => {
                    // 先撤托盘图标再关窗，防幽灵图标残留
                    self.tray.shutdown();
                    self.allow_exit = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                    spawn_exit_watchdog();
                }
            }
        }
        // ✕ 恒隐藏到托盘（v0.1.6 用户钉死，无条件）：窗口 ✕ 按钮、
        // 系统 WM_CLOSE、任务栏右键「关闭窗口」都到这里，一律取消
        // 关闭并隐藏；进程的唯一正常退出 = 托盘菜单「退出」
        // （allow_exit 只由那条路径置位，它随后自己发 Close）。
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_exit {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.hide(ctx);
        }

        // （可见性/最小化影子已在帧首 sync_shadow_from_os 与
        // OS 对齐——旧版在这里读 viewport 上报的 minimized 做
        // 同步，v0.1.12 起 IsWindowVisible/IsIconic 是唯一真相，
        // 见 winvis 模块头。点外判定与呼出收尾同在帧首，顺序见
        // update() 开头。）

        self.poll_channels(ctx);
        if self.visible {
            self.handle_keys(ctx);
        }

        // 主题交叉淡入推进 + 筛选面板动画推进（都只在变化的一小
        // 段时间内请求重绘，静止不转）
        self.refresh_display_pal(ctx);
        self.advance_filter_anim(ctx);

        // 拖动/拉伸手判（读上一帧的控件矩形集，不注册任何覆盖
        // 全窗的交互控件——v0.1.9 的全窗 click_and_drag 背景区
        // 抢 drag 吞点击，是「所有界面都点不了」的元凶，已删）
        self.handle_window_press(ctx);

        // 悬浮窗背景：原版 InkFill 三段渐变（单个 Mesh，见
        // paint_vertical_gradient）画在背景层，面板本身透明，
        // 靠 Frame 的 ShellStroke 描边定义外壳。取色全走 palette。
        // 最大化时圆角归零（原版口径）：渐变 / 光晕 / 壳体描边
        // 同用 shell_radius，圆角外一像素都不画（黑角根治的
        // 另一半：Mesh 分带加密在 paint 函数里）。
        let pal = self.display_pal;
        let bg_painter = ctx.layer_painter(egui::LayerId::background());
        let screen = ctx.screen_rect();
        let shell_radius = if self.maximized { 0.0 } else { 12.0 };
        paint_vertical_gradient(
            &bg_painter,
            screen,
            shell_radius,
            pal.grad_top,
            pal.grad_mid,
            pal.grad_bottom,
        );
        // 顶部光晕（原版 TopGlow：顶部 220px 白色渐隐层，单个 Mesh）
        paint_top_glow(&bg_painter, screen, shell_radius, pal.top_highlight_peak);
        // 注意：这里绝不能给 Frame 加 shadow。面板填充是透明的，egui 的
        // 阴影画在面板形状之后、内容之前，会整片透过透明面板压在渐变上
        // ——浅色下就是一层均匀的灰幕（v0.1.3 浅色「太灰」的真凶，深色下
        // 黑压黑看不出来）。本窗口与面板同尺寸，阴影本就没有可落的余地，
        // 边缘靠双层描边定义。
        let panel_frame = egui::Frame::central_panel(&ctx.style())
            .fill(Color32::TRANSPARENT)
            .rounding(Rounding::same(shell_radius))
            // 阴影显式 NONE（不靠默认值）：壳体不许有任何实色
            // 垫底，圆角外保持真透明（v0.1.9 黑角根治）
            .shadow(egui::epaint::Shadow::NONE)
            .stroke(Stroke::new(1.0_f32, pal.panel_stroke))
            // 内容 Margin 对齐原版：左 26 / 右 18（右侧少 8 给滚动条）
            .inner_margin(egui::Margin {
                left: 26.0,
                right: 18.0,
                top: 12.0,
                bottom: 12.0,
            });
        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                // 宽度护栏：内容宽 = 屏宽 - 左右 Margin（26+18）。某些
                // 环境下（虚拟屏 DPI 不一致等）面板给到的可用宽会多出
                // 一截，右侧内容被窗口边缘切掉（v0.1.3 预览里分类胶囊/
                // 状态栏文字被切就是这个），按屏宽显式收住。
                let content_w = (ctx.screen_rect().width() - 44.0).max(200.0);
                ui.set_max_width(content_w);
                self.draw_header(ui);
                ui.add_space(5.0);
                self.draw_search_box(ui);
                ui.add_space(5.0);
                // 搜索框下发丝线（原版：高 1、HairLine）
                let (line_rect, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 1.0),
                    egui::Sense::hover(),
                );
                ui.painter_at(line_rect)
                    .rect_filled(line_rect, Rounding::ZERO, pal.hairline);
                ui.add_space(5.0);
                self.draw_toolbar(ui);
                // 工具栏 → 内容区间距（v0.1.10 布局收紧统一 5；
                // 顶部固定区总高 167 → 131，见 feature-parity
                // 有意差异记录）
                ui.add_space(5.0);
                let footer_h = 28.0;
                let results_h =
                    (ui.available_height() - footer_h - 6.0).max(60.0);
                ui.allocate_ui(Vec2::new(ui.available_width(), results_h), |ui| {
                    self.draw_results(ui);
                });
                ui.add_space(6.0);
                self.draw_status_bar(ui);
            });

        // 自绘右键菜单（主面板之后、设置窗口之前）
        self.draw_ctx_menu(ctx);

        // 设置窗口（独立 viewport）与悬停详情卡都画在主面板之后
        self.draw_settings_viewport(ctx);
        self.update_hover_card(ctx);

        // 筛选面板点外关闭（第三条关闭路径，与圆点再点切换、Esc
        // 独立生效）：特意放在全部绘制**之后**判定——用本帧刚画
        // 出的圆点/面板 Rect，且判定区外扩 3px 与 egui 自身的
        // 点击命中区对齐。旧版在帧首拿上一帧 Rect 在 press 帧判，
        // 与圆点 clicked()（release 帧切换）是两套几何：点落在
        // 圆点下缘的命中区内、旧 Rect 之外时，press 先判点外
        // 关闭、release 又 toggle 重开，面板永远关不掉（Linux
        // 回归实测 + 日志实证）。挪到绘制后：再点圆点时 toggle
        // 已先行生效，这里看到的是切换后的状态与新 Rect，不打架。
        if let Some(kind0) = expanded_at_frame_start {
            if self.expanded_filter == Some(kind0) {
            let press = ctx.input(|i| {
                if i.pointer.primary_pressed() {
                    i.pointer
                        .press_origin()
                        .or_else(|| i.pointer.interact_pos())
                } else {
                    None
                }
            });
            if let Some(p) = press {
                let in_panel = self
                    .filter_panel_rect
                    .map(|r| r.expand(3.0).contains(p))
                    .unwrap_or(false);
                let in_dot = self
                    .filter_dot_rect
                    .map(|r| r.expand(3.0).contains(p))
                    .unwrap_or(false);
                if !in_panel && !in_dot {
                    self.expanded_filter = None;
                }
            }
            }
        } else if self.expanded_filter.is_none()
            && !self.filter_anim_t.iter().any(|t| *t > 0.0)
        {
            self.filter_dot_rect = None;
            self.filter_panel_rect = None;
        }

        // 控件矩形集轮换：本帧绘制期间推进 blockers_cur 的全部
        // rect 成为下一帧按下判定的 blockers_prev（handle_window_press）
        self.blockers_prev = std::mem::take(&mut self.blockers_cur);
        self.scroll_prev = std::mem::take(&mut self.scroll_cur);
    }
}

/// 关于文案的唯一来源（托盘「关于」与设置「关于」分区共用）。
fn about_text() -> String {
    format!(
        "瞬觅 ShunSeek v{} · 本地文件快速搜索",
        env!("CARGO_PKG_VERSION")
    )
}

/// 全局 Visuals 微调：以主题对应的 egui 基础 Visuals（dark/light）打底，
/// 再套本应用的参数——控件圆角统一 8、选中态朱砂半透明、弹窗/菜单走
/// 原版 MenuFill/MenuStroke + 投影（ blur 22 / depth 5）。启动时按
/// settings 的主题调一次，切换主题时由 set_theme 再调。
fn install_visuals(ctx: &egui::Context, theme: Theme) {
    let pal = theme.palette();
    let mut visuals = match theme {
        Theme::Dark => egui::Visuals::dark(),
        Theme::Light => egui::Visuals::light(),
    };
    visuals.panel_fill = pal.panel_fill;
    // 弹窗/右键菜单/筛选面板：MenuFill 底 + MenuStroke 边 + 柔和
    // 投影，与窗口 InkFill 底拉开层次（v0.1.5 浅色白贴白的根治）。
    visuals.window_fill = pal.popup_fill;
    visuals.window_stroke = Stroke::new(1.0_f32, pal.popup_stroke);
    visuals.popup_shadow = egui::epaint::Shadow {
        offset: Vec2::new(0.0, 5.0),
        blur: 22.0,
        spread: 0.0,
        color: pal.popup_shadow_color,
    };
    visuals.extreme_bg_color = pal.extreme_bg;
    visuals.selection.bg_fill = Color32::from_rgba_unmultiplied(
        pal.accent.r(),
        pal.accent.g(),
        pal.accent.b(),
        120,
    );
    visuals.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
    let r = Rounding::same(8.0);
    visuals.widgets.noninteractive.rounding = r;
    visuals.widgets.inactive.rounding = r;
    visuals.widgets.hovered.rounding = r;
    visuals.widgets.active.rounding = r;
    visuals.widgets.open.rounding = r;
    visuals.window_rounding = Rounding::same(10.0);
    ctx.set_visuals(visuals);
}

/// --tray 静默启动标志（自启带 --tray 时 main 置位，首帧消费）。
static START_HIDDEN: AtomicBool = AtomicBool::new(false);
/// 单实例唤醒标志：等待线程收到命名事件后置位，update 消费。
static WAKE_FLAG: AtomicBool = AtomicBool::new(false);
/// 单实例唤醒事件句柄（main 创建；0 = 无/非 Windows）。
static WAKE_EVENT: std::sync::atomic::AtomicIsize =
    std::sync::atomic::AtomicIsize::new(0);

/// 崩溃日志（v0.1.8）：panic 时先把信息写到数据目录
/// logs/crash-<unix秒>.log，再链回默认 hook（stderr 照常输出）。
fn install_crash_log_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let dir = data_dir().join("logs");
        let _ = std::fs::create_dir_all(&dir);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let body = format!(
            "ShunSeek v{} panic\n{info}\n",
            env!("CARGO_PKG_VERSION")
        );
        let _ = std::fs::write(dir.join(format!("crash-{ts}.log")), body);
        prev(info);
    }));
}

/// 单实例获取（v0.1.8，Windows）：命名 Mutex 已存在 → 给已有
/// 实例的命名事件 SetEvent 唤醒它、返回 false（本进程退出）；
/// 首实例 → 建手动重置唤醒事件（句柄存 WAKE_EVENT 给等待线程）
/// 并返回 true。Mutex 创建失败时保守放行（宁可双开不可锁死）。
#[cfg(windows)]
static SINGLE_INSTANCE_MUTEX: std::sync::atomic::AtomicIsize =
    std::sync::atomic::AtomicIsize::new(0);

#[cfg(windows)]
fn single_instance_acquire() -> bool {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateMutexW, OpenEventW, SetEvent, EVENT_MODIFY_STATE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
    let wide = |s: &str| -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    };
    unsafe {
        let mutex_name = wide("ShunSeekSingleInstanceMutex");
        let mh = CreateMutexW(std::ptr::null(), 1, mutex_name.as_ptr());
        if mh == 0 {
            return true;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let mut replaced = false;
            if std::env::args().any(|a| a == "--from-restart") {
                use windows_sys::Win32::System::Threading::WaitForSingleObject;
                let w = WaitForSingleObject(mh, 3000);
                replaced = w == 0 || w == 0x80;
            }
            if !replaced {
                let _ = AllowSetForegroundWindow(u32::MAX);
                let ev_name = wide("ShunSeekWakeEvent");
                let eh = OpenEventW(EVENT_MODIFY_STATE, 0, ev_name.as_ptr());
                if eh != 0 {
                    SetEvent(eh);
                    CloseHandle(eh);
                }
                CloseHandle(mh);
                return false;
            }
        }
        SINGLE_INSTANCE_MUTEX.store(mh, Ordering::SeqCst);
        let ev_name = wide("ShunSeekWakeEvent");
        let eh = CreateEventW(std::ptr::null(), 1, 0, ev_name.as_ptr());
        if eh != 0 {
            WAKE_EVENT.store(eh, Ordering::SeqCst);
        }
        true
    }
}

#[cfg(windows)]
fn restart_elevated() -> bool {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let mut args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--tray" && a != "--silent" && a != "--from-restart")
        .map(|a| {
            if a.contains(' ') || a.contains('\t') {
                format!("\"{}\"", a.replace('"', "\\\""))
            } else {
                a
            }
        })
        .collect();
    args.push("--from-restart".to_string());
    let params = args.join(" ");
    let wide = |s: &OsStr| -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    };
    let verb = wide(OsStr::new("runas"));
    let file = wide(exe.as_os_str());
    let params_w = wide(OsStr::new(&params));
    let r = unsafe {
        ShellExecuteW(
            0,
            verb.as_ptr(),
            file.as_ptr(),
            if params.is_empty() {
                std::ptr::null()
            } else {
                params_w.as_ptr()
            },
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        )
    };
    r as isize > 32
}

fn main() -> eframe::Result<()> {
    // 便携数据迁移要在任何 data_dir() 消费之前（崩溃日志钩子
    // 本身就写 data_dir/logs）：exe 旁 data/ 首启时从旧 AppData
    // 拷一份数据过来（见 migrate_into_portable_data）
    migrate_into_portable_data();
    install_crash_log_hook();
    // --tray / --silent：自启静默启动，只进托盘不弹窗
    if std::env::args().any(|a| a == "--tray" || a == "--silent") {
        START_HIDDEN.store(true, Ordering::SeqCst);
    }
    // 单实例：第二个进程唤醒已有实例后退出（见 single_instance_acquire）
    #[cfg(windows)]
    {
        if !single_instance_acquire() {
            return Ok(());
        }
    }
    let viewport = egui::ViewportBuilder::default()
        .with_title("瞬觅 ShunSeek")
        .with_icon(egui::IconData {
            rgba: include_bytes!("../assets/icon-64.rgba").to_vec(),
            width: 64,
            height: 64,
        })
        // 尺寸对齐原版：默认 780×540、最小 600×400、可拉伸
        .with_inner_size([780.0, 540.0])
        .with_min_inner_size([600.0, 400.0])
        .with_resizable(true)
        .with_decorations(false)
        .with_transparent(true);
    // v0.1.6：不再置顶（默认普通 WindowLevel，别的窗口能盖住它）、
    // 进任务栏（默认显示）。v0.1.5 的 AlwaysOnTop + with_taskbar(false)
    // 让窗口霸屏且在任务栏找不到、托盘又退不出，只能任务管理器
    // 强杀——改回原版行为（Topmost=False、ShowInTaskbar=True）。
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "瞬觅 ShunSeek",
        options,
        Box::new(|cc| {
            install_cjk_font(&cc.egui_ctx);
            install_visuals(&cc.egui_ctx, load_settings().theme);
            Ok(Box::new(ShunSeekApp::new(cc)))
        }),
    )
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    /// 圆角区内分带步长上限：弧顶 3px 内 ≤0.5、圆角区其余 ≤2。
    /// 这是「弦不许离开圆弧」的数值化——v0.1.9 的 24px 末带
    /// 在这里必然挂掉（540 高正是当时的翻车相位）。
    #[test]
    fn band_steps_inside_corner_zones() {
        for h in [400.0f32, 540.0, 541.0, 543.0, 555.0, 800.0, 1080.0] {
            let rect = egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(780.0, h),
            );
            let ys = gradient_band_ys(rect, 12.0);
            assert_eq!(ys.first(), Some(&0.0));
            assert_eq!(ys.last(), Some(&h));
            for w in ys.windows(2) {
                let (a, b) = (w[0], w[1]);
                let gap = b - a;
                assert!(gap > 0.0, "h={h}: 步长非正 {a}..{b}");
                let d_top = a;
                let d_bot = h - b;
                if d_top < 24.0 || d_bot < 24.0 {
                    assert!(
                        gap <= 2.01,
                        "h={h}: 圆角区步长 {gap}（{a}..{b}）"
                    );
                }
                if d_top < 3.0 || d_bot < 3.0 {
                    assert!(
                        gap <= 0.51,
                        "h={h}: 弧顶步长 {gap}（{a}..{b}）"
                    );
                }
            }
        }
    }

    /// 每条带的弦（端点 inset 线性插值）在带中点与真圆弧 inset
    /// 的偏差 <0.8px——楔形残留的上限，AA 后不可见。
    #[test]
    fn band_chord_deviation_subpixel() {
        for h in [400.0f32, 540.0, 541.0, 543.0, 555.0, 800.0, 1080.0] {
            let rect = egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(780.0, h),
            );
            let ys = gradient_band_ys(rect, 12.0);
            for w in ys.windows(2) {
                let (y0, y1) = (w[0], w[1]);
                let ym = (y0 + y1) / 2.0;
                let chord = (rounded_inset_at(rect, 12.0, y0)
                    + rounded_inset_at(rect, 12.0, y1))
                    / 2.0;
                let dev = (chord - rounded_inset_at(rect, 12.0, ym)).abs();
                assert!(
                    dev < 0.8,
                    "h={h}: 带 {y0}..{y1} 弦弧偏差 {dev}"
                );
            }
        }
    }

    /// 拉伸命中区：角优先于边、带外为 None、带内不侵入内容区
    /// （内容 Margin 最小 12 > 角带 12 的内界，控件永远不在带内）。
    #[test]
    fn resize_zone_geometry() {
        let s = egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            Vec2::new(780.0, 540.0),
        );
        use egui::viewport::ResizeDirection as D;
        assert_eq!(resize_zone_at(s, egui::pos2(2.0, 2.0)).map(|z| z.0), Some(D::NorthWest));
        assert_eq!(resize_zone_at(s, egui::pos2(778.0, 538.0)).map(|z| z.0), Some(D::SouthEast));
        assert_eq!(resize_zone_at(s, egui::pos2(400.0, 3.0)).map(|z| z.0), Some(D::North));
        assert_eq!(resize_zone_at(s, egui::pos2(400.0, 537.0)).map(|z| z.0), Some(D::South));
        assert_eq!(resize_zone_at(s, egui::pos2(3.0, 300.0)).map(|z| z.0), Some(D::West));
        assert_eq!(resize_zone_at(s, egui::pos2(777.0, 300.0)).map(|z| z.0), Some(D::East));
        assert_eq!(resize_zone_at(s, egui::pos2(400.0, 300.0)), None);
        assert_eq!(resize_zone_at(s, egui::pos2(20.0, 20.0)), None);
    }
}
