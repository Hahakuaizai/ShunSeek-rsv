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
//!  - 单实例：C# 版用命名 Mutex；Rust 版 TODO（需要 Windows 命名互斥 FFI
//!    或单实例 socket），当前不做，重复启动会开两个窗口。
//!  - 全局热键：轮询实现，见 hotkey.rs；非 Windows 无全局热键。

#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod autostart;
mod demo;
mod hotkey;
mod icons;
mod opener;
mod tray;

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
use shunseek_core::{
    build_index, Category, IndexStore, RawEntry, ScopeFilter, SearchEngine, UsageStore,
};

const RESULT_LIMIT: usize = 100;
const ROW_H: f32 = 52.0;

/// 宫格格子尺寸（宽×高）：图标 40 + 名字至多 2 行。
const GRID_CELL_W: f32 = 104.0;
const GRID_CELL_H: f32 = 96.0;
/// 首页磁贴尺寸（原版 TileItem）：170×92，项间距 12。
const HOME_TILE_W: f32 = 170.0;
const HOME_TILE_H: f32 = 92.0;
const HOME_TILE_GAP: f32 = 12.0;
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
            grad_top: Color32::from_rgb(250, 251, 249), // InkFill #FAFBF9
            grad_mid: Color32::from_rgb(242, 244, 241), // InkFill #F2F4F1
            grad_bottom: Color32::from_rgb(233, 236, 232), // InkFill #E9ECE8
            top_highlight_peak: 51,                     // TopGlow 白@40% × 层 0.5
            panel_stroke: Color32::from_rgb(195, 203, 196), // ShellStroke #C3CBC4
            panel_fill: Color32::from_rgb(233, 236, 232),
            extreme_bg: Color32::from_rgb(255, 255, 255), // Surface #FFFFFF
            popup_fill: Color32::from_rgba_unmultiplied(252, 253, 251, 250), // MenuFill #FCFDFB
            popup_stroke: Color32::from_rgb(214, 220, 213), // MenuStroke #D6DCD5
            popup_shadow_color: Color32::from_rgba_unmultiplied(74, 90, 80, 71), // #4A5A50 @0.28
            accent: Color32::from_rgb(178, 67, 44),     // AccentBrush #B2432C
            name_text: Color32::from_rgb(27, 35, 32),   // TextHi #1B2320
            text_mid: Color32::from_rgb(83, 96, 90),    // TextMid #53605A
            path_text: Color32::from_rgb(111, 123, 116), // TextLow #6F7B74
            empty_text: Color32::from_rgb(111, 123, 116),
            status_text: Color32::from_rgb(111, 123, 116),
            hairline: Color32::from_rgb(221, 226, 220), // HairLine #DDE2DC
            edge_line: Color32::from_rgb(204, 211, 204), // EdgeLine #CCD3CC
            edge_strong: Color32::from_rgb(176, 187, 178), // EdgeStrong #B0BBB2
            surface: Color32::from_rgb(255, 255, 255),  // Surface #FFFFFF
            surface_hover: Color32::from_rgb(244, 246, 243), // SurfaceHover #F4F6F3
            surface_sel: Color32::from_rgb(234, 239, 233), // SurfaceSel #EAEFE9
            search_fill: Color32::from_rgb(255, 255, 255),
            search_icon: Color32::from_rgb(111, 123, 116),
            search_text: Color32::from_rgb(27, 35, 32),
            hint: Some(Color32::from_rgb(111, 123, 116)),
            esc_fill: Color32::from_rgb(242, 244, 241), // ChipFill #F2F4F1
            esc_text: Color32::from_rgb(111, 123, 116),
            bar_fill: Color32::from_rgb(255, 255, 255),
            bar_stroke: Color32::from_rgb(204, 211, 204),
            chip_text_rgb: (83, 96, 90),
            chip_sel_text_rgb: (156, 58, 36),
            pill_fill: Color32::from_rgba_unmultiplied(178, 67, 44, 26), // @10%
            pill_hover: Color32::from_rgba_unmultiplied(178, 67, 44, 48), // @19%
            pill_stroke: Color32::from_rgba_unmultiplied(178, 67, 44, 89), // @35%
            chip_hover_fill: Color32::from_rgb(237, 241, 236), // MenuHover #EDF1EC
            row_hover_fill: Color32::from_rgb(242, 245, 241), // RowHover #F2F5F1
            row_sel_fill: Color32::from_rgb(232, 237, 231), // RowSel #E8EDE7
            row_sel_stroke: Color32::from_rgb(200, 209, 200), // RowSelStroke #C8D1C8
            fold_fill: Color32::from_rgb(240, 243, 239), // FoldFill #F0F3EF
            icon_plate: Color32::from_rgb(237, 239, 234), // IconPlate #EDEFEA
            highlight: Color32::from_rgb(156, 58, 36),
            alert_text: Color32::from_rgb(193, 68, 50),
            pin_mark: Color32::from_rgba_unmultiplied(178, 67, 44, 179), // @70%
            importance_high: Color32::from_rgb(178, 67, 44),
            importance_normal: Color32::from_rgb(138, 148, 141), // #8A948D
            importance_low: Color32::from_rgb(194, 202, 194), // #C2CAC2
            close_hover: Color32::from_rgb(193, 68, 50),
            close_pressed: Color32::from_rgb(166, 55, 40),
            close_text: Color32::from_rgb(253, 247, 242),
        }
    }
}
// ---------------------------------------------------------------- 设置存储

/// 全部可持久化设置（v0.1.5 统一）：数据目录 settings.txt，key=value 行。
/// 键：theme / view / hide_on_close / hotkey / result_limit / fold_demoted。
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
    /// 搜索范围选中集（scope_* 键）：预设键名 / 盘符 / 自定义完整路径。
    scope_presets: Vec<String>,
    scope_drives: Vec<char>,
    scope_custom: Vec<String>,
}

fn load_settings() -> Settings {
    let dir = data_dir();
    let mut kv: HashMap<String, String> = HashMap::new();
    let mut scope_custom: Vec<String> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(dir.join("settings.txt")) {
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                let (k, v) = (k.trim(), v.trim());
                // scope_custom 可重复多行（一行一个文件夹），单独收集
                if k == "scope_custom" {
                    if !v.is_empty() {
                        scope_custom.push(v.to_string());
                    }
                } else {
                    kv.insert(k.to_string(), v.to_string());
                }
            }
        }
    }
    // 范围缺省（settings.txt 里没有任何 scope 键）= 全部：
    // 预设全选 + 现存磁盘全选。有键则按存的值（选中集语义见工具栏）。
    let (scope_presets, scope_drives) =
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
                            .collect()
                    })
                    .unwrap_or_default(),
                kv.get("scope_drives")
                    .map(|s| s.chars().filter(|c| c.is_ascii_uppercase()).collect())
                    .unwrap_or_default(),
            )
        };
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
        hotkey: kv
            .get("hotkey")
            .and_then(|s| hotkey::HotkeyCombo::parse(s))
            .unwrap_or(hotkey::HotkeyCombo::ALT_SPACE),
        result_limit: kv
            .get("result_limit")
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|n| [60usize, 100, 200].contains(n))
            .unwrap_or(RESULT_LIMIT),
        fold_demoted: bool_or("fold_demoted", true),
        scope_presets,
        scope_drives,
        scope_custom,
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

/// 当前实际存在的固定盘盘符（范围面板的磁盘列表 / 全部判定用）。
fn existing_drives() -> Vec<char> {
    ('C'..='Z')
        .filter(|d| std::path::Path::new(&format!("{d}:\\")).exists())
        .collect()
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

/// 数据目录：Windows %LocalAppData%\ShunSeek；其他 ~/.local/share/ShunSeek。
fn data_dir() -> PathBuf {
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
    /// 后台重建（阶段性或最终）完成：新索引本体直接随消息传给 UI 线程
    /// 换入（内存直传，不再走 rename + mmap 的文件换入——那条路一旦
    /// save/rename/load 任一步失败就会把新索引丢掉、沿用旧的空索引，
    /// 还不给任何提示）。
    /// v0.1.5 分级建立：冷启动先发第一级（应用 + 浅层，final_stage=
    /// false），逐盘 MFT 完成再逐级发合并索引，最终一级才置 true。
    /// saved 只对最终一级有意义：缓存 index.bin 有没有存上（save_to
    /// 自带 tmp 写 + rename 原子换入），失败不影响本次使用、但会进
    /// 状态栏提示，避免用户次次冷启动还找不出原因；
    /// summary 是来源摘要 / 建立进度文案，进状态栏。
    Rebuilt {
        store: IndexStore,
        count: usize,
        saved: bool,
        summary: String,
        final_stage: bool,
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

fn spawn_rebuild(dir: PathBuf, tx: Sender<IndexMsg>) {
    std::thread::Builder::new()
        .name("shunseek-index".into())
        .spawn(move || {
            // 整个重建体再套一层隔离：就算中途出了意外，也要把
            // 「空索引 + 异常摘要」的最终消息发出去，让 UI 的空索引
            // 护栏能亮红字、托盘「重新建立索引」能解灰，而不是消息
            // 永不达、用户对着旧索引干瞪眼。
            #[cfg(windows)]
            {
                if guarded(|| rebuild_windows(&dir, &tx)).is_none() {
                    let store = build_index(Vec::new(), None);
                    let _ = tx.send(IndexMsg::Rebuilt {
                        store,
                        count: 0,
                        saved: true,
                        summary: "重建线程异常".to_string(),
                        final_stage: true,
                    });
                }
            }
            #[cfg(not(windows))]
            {
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
                // 缓存尽力存 index.bin（save_to 自带 tmp+rename 原子
                // 换入）：失败只丢缓存（saved=false），store 本体照常直传。
                let saved = store.save_to(&dir.join("index.bin")).is_ok();
                let _ = tx.send(IndexMsg::Rebuilt {
                    store,
                    count,
                    saved,
                    summary,
                    final_stage: true,
                });
            }
        })
        .ok();
}

/// 把当前累积条目建成索引发给 UI 换入。final_stage 时顺带把缓存存到
/// index.bin——旧版存的是 index-next.bin 而启动只读 index.bin，缓存
/// 写了永远读不到、次次冷启动，v0.1.5 改直存（save_to 内部 tmp 写 +
/// rename，进程中途被杀也不会留下写一半的缓存）。
fn send_index_stage(
    tx: &Sender<IndexMsg>,
    dir: &PathBuf,
    entries: &[RawEntry],
    roots: &HashMap<char, String>,
    summary: String,
    final_stage: bool,
) {
    let roots = if roots.is_empty() { None } else { Some(roots) };
    let store = build_index(entries.to_vec(), roots);
    let count = store.count();
    let saved = if final_stage {
        store.save_to(&dir.join("index.bin")).is_ok()
    } else {
        true
    };
    let _ = tx.send(IndexMsg::Rebuilt {
        store,
        count,
        saved,
        summary,
        final_stage,
    });
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

/// Windows 分级建立（v0.1.5）：打开就能搜，不等全量建完。
/// 第一级：appscan（快捷方式 + 已装应用）+ 桌面/用户目录浅层，先建成
/// 索引换入，搜索立刻可用；第二级：逐盘 MFT，每完成一盘把「第一级 +
/// 已完成盘」的合并索引再换入一次，状态栏摘要实时报进度。搜索永远
/// 针对当前已换入的索引，输入框全程不禁用、不出等待态。
/// 兜底语义与旧版一致：MFT 总数为 0 → 全遍历 %USERPROFILE%；条目仍
/// 为 0 → 每个固定盘根全盘遍历。最终一级存缓存 index.bin。
#[cfg(windows)]
fn rebuild_windows(dir: &PathBuf, tx: &Sender<IndexMsg>) {
    use shunseek_core::sources::{appscan, mft, walker};
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
        dir,
        &entries,
        &roots,
        format!("索引：应用 {apps} · 浅层 {shallow_n} · MFT 建立中…"),
        false,
    );

    // ---- 第二级：逐盘 MFT，逐盘并入 ----
    let mut parts: Vec<String> = Vec::new();
    let mut mft_total = 0usize;
    for (di, drive) in drives.iter().enumerate() {
        match guarded(|| mft::load_volume(*drive)) {
            Some(Ok(mut v)) => {
                mft_total += v.len();
                parts.push(format!("MFT {drive}:{}", v.len()));
                entries.append(&mut v);
            }
            _ => parts.push(format!("MFT {drive}:失败")),
        }
        if di + 1 < drives.len() {
            let done: Vec<String> = drives[..=di]
                .iter()
                .map(|d| format!("MFT {d}:完成"))
                .collect();
            send_index_stage(
                tx,
                dir,
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

    // ---- 兜底（旧语义）：MFT 全灭 → 遍历用户目录；仍空 → 全盘遍历 ----
    let mut walk_count = 0usize;
    if mft_total == 0 {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            let v = guarded(|| walker::walk(&[PathBuf::from(profile)], true))
                .unwrap_or_default();
            walk_count = v.len();
            entries.extend(v);
        }
    }
    parts.push(format!("应用 {apps}"));
    parts.push(format!("浅层 {shallow_n}"));
    parts.push(format!("遍历 {walk_count}"));
    if entries.is_empty() && !drives.is_empty() {
        let mut full_count = 0usize;
        for drive in &drives {
            let root = PathBuf::from(format!("{drive}:\\"));
            let v = guarded(|| walker::walk(&[root], true)).unwrap_or_default();
            full_count += v.len();
            entries.extend(v);
        }
        parts.push(format!("全盘遍历 {full_count}"));
    }

    // ---- 最终一级：存缓存 + 换入 ----
    send_index_stage(tx, dir, &entries, &roots, parts.join(" · "), true);
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
    /// 是否被 core 降权（哈希名 / 垃圾路径）：展示时归类到尾部折叠区。
    demoted: bool,
}

/// 展示行（v0.1.5）：由原始结果 rows 派生，绘制与键盘导航都以它为准。
/// 两段式派生（rebuild_display）：重要结果（demoted=false）在前，
/// 其中同一父目录命中 ≥4 条折叠成一个 Group 行（展开后成员以
/// Single(indent) 跟在组行后）；被降权的结果（demoted=true）整体
/// 移到尾部，其中同父 ≥2 条折成组、单条直接平铺。空查询不折叠，
/// display 与 rows 一一对应。
#[derive(Clone)]
enum DisplayRow {
    Single { row: usize, indent: bool },
    Group {
        /// 父目录完整路径（也是展开状态的键）。
        dir: String,
        count: usize,
        expanded: bool,
    },
}

/// 结果完整路径的父目录（最后一个分隔符之前）；无分隔符时为空串。
fn parent_dir(path: &str) -> &str {
    match path.rfind(['\\', '/']) {
        Some(p) => &path[..p],
        None => "",
    }
}

/// 父目录的末两段（组行主文字用，如 "radium\\users"）；不足两段时原样返回。
fn dir_tail(dir: &str) -> String {
    let segs: Vec<&str> = dir.split(['\\', '/']).filter(|s| !s.is_empty()).collect();
    if segs.len() <= 2 {
        dir.to_string()
    } else {
        segs[segs.len() - 2..].join("\\")
    }
}

/// 父目录的最后一段（宫格组格主文字 / 详情卡的组名用）。
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
    scope_custom: Vec<String>,
    /// 当前主题（深/浅）；全部绘制取色走 theme.palette()。
    theme: Theme,
    /// 结果展示模式（列表/宫格），view.txt 持久化。
    view_mode: ViewMode,
    /// 宫格上一帧的列数（键盘 ↑↓ 按它跳行，绘制时更新）。
    grid_cols: usize,
    rows: Vec<ResultRow>,
    /// 由 rows 派生的展示行（折叠后）；selected / 绘制都以它为准。
    display: Vec<DisplayRow>,
    /// 已展开的折叠组，按父目录路径记（跨搜索保留展开状态）。
    expanded_dirs: HashSet<String>,
    /// 本批结果是否启用折叠（空查询不折叠）。
    fold_active: bool,
    /// 结果行真实系统图标缓存（Windows；非 Windows 恒空回退色块）。
    icons: icons::IconCache,
    selected: usize,
    status: String,
    /// 状态栏警戒标志（空索引护栏）：true 时状态文字与圆点标红。
    status_alert: bool,
    /// 搜索框上一帧是否聚焦（放大镜着色读它，一帧延迟无感）。
    search_focused: bool,
    store: Arc<Mutex<IndexStore>>,
    usage: Arc<Mutex<UsageStore>>,
    search_cancel: Arc<AtomicBool>,
    search_tx: Sender<SearchMsg>,
    search_rx: Receiver<SearchMsg>,
    index_rx: Receiver<IndexMsg>,
    hotkey: hotkey::Hotkey,
    tray: tray::Tray,
    visible: bool,
    allow_exit: bool,
    focus_search: bool,
    scroll_to_selected: bool,
    /// 数据目录（托盘「重新建立索引」重新 spawn 重建线程要用）。
    data_dir: PathBuf,
    /// 索引消息发送端（同上，App 持一份 clone）。
    index_tx: Sender<IndexMsg>,
    /// 是否有重建正在跑（托盘重建项置灰 + 并发护栏）。
    rebuilding: bool,
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
    /// 唤醒热键预设 / 结果上限 / 是否折叠不重要结果。
    hotkey_combo: hotkey::HotkeyCombo,
    result_limit: usize,
    fold_demoted: bool,
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
    /// 主窗口上一帧是否聚焦（失焦自动隐藏看 true→false 跳变）。
    was_focused: bool,
    /// 打开系统文件对话框前置位：对话框关后的一帧焦点丢失不隐藏。
    suppress_focus_hide: bool,
    /// 最近一次「失焦自动隐藏」的时刻：托盘左键 Toggle 若紧接着
    /// 到达（同一次点击托盘图标既让主窗失焦、又发 Toggle），忽略
    /// 这次 Toggle，免得窗口刚被失焦藏好又被 Toggle 弹出来。
    focus_hide_at: Option<Instant>,
    /// 筛选控件上一帧的圆点/胶囊 Rect 与展开面板 Rect（点外关闭判定）。
    filter_dot_rect: Option<egui::Rect>,
    filter_panel_rect: Option<egui::Rect>,
    /// 首页「＋ 添加」弹层是否打开（popup 锚在添加格上）。
    home_add_open: bool,
}

impl ShunSeekApp {
    fn new(cc: &eframe::CreationContext<'_>) -> ShunSeekApp {
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);

        let usage = UsageStore::load(Some(dir.join("usage.dat")));

        // 缓存索引秒开：有 index.bin 先 mmap 顶上，后台重建完再换新的。
        // 旧版本（≤0.1.4）的缓存写在 index-next.bin，有就拿来顶上，
        // 别让老用户升级后第一次启动白白冷启动。
        let index_path = dir.join("index.bin");
        let (store, status) = match IndexStore::load_mmap(&index_path)
            .or_else(|_| IndexStore::load_mmap(&dir.join("index-next.bin")))
        {
            Ok(s) => {
                let n = s.count();
                (s, format!("{} 项", group_thousands(n)))
            }
            Err(_) => (build_index(Vec::new(), None), "正在准备索引…".to_string()),
        };

        let (search_tx, search_rx) = std::sync::mpsc::channel();
        let (index_tx, index_rx) = std::sync::mpsc::channel();
        let index_tx_app = index_tx.clone();
        spawn_rebuild(dir.clone(), index_tx);

        let settings = load_settings();
        let theme = settings.theme;
        let view_mode = settings.view;

        let mut app = ShunSeekApp {
            query: String::new(),
            cat_set: all_categories(),
            expanded_filter: None,
            scope_presets: settings.scope_presets.iter().cloned().collect(),
            scope_drives: settings.scope_drives.iter().copied().collect(),
            scope_custom: settings.scope_custom.clone(),
            theme,
            view_mode,
            grid_cols: 6,
            rows: Vec::new(),
            display: Vec::new(),
            expanded_dirs: HashSet::new(),
            fold_active: false,
            icons: icons::IconCache::new(&cc.egui_ctx),
            selected: 0,
            status,
            status_alert: false,
            search_focused: false,
            store: Arc::new(Mutex::new(store)),
            usage: Arc::new(Mutex::new(usage)),
            search_cancel: Arc::new(AtomicBool::new(true)),
            search_tx,
            search_rx,
            index_rx,
            hotkey: hotkey::Hotkey::start(cc.egui_ctx.clone(), settings.hotkey),
            tray: tray::Tray::new(
                cc.egui_ctx.clone(),
                tray::TrayInit {
                    // 启动时已有一轮重建在跑（上面 spawn_rebuild）
                    rebuilding: true,
                },
            ),
            visible: true,
            allow_exit: false,
            focus_search: true,
            scroll_to_selected: false,
            data_dir: dir.clone(),
            index_tx: index_tx_app,
            rebuilding: true,
            hover_seen: None,
            hover_target: None,
            hover_since: 0.0,
            meta_cache: HashMap::new(),
            settings_open: false,
            settings_autostart: false,
            hotkey_combo: settings.hotkey,
            result_limit: settings.result_limit,
            fold_demoted: settings.fold_demoted,
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
            was_focused: false,
            suppress_focus_hide: false,
            focus_hide_at: None,
            filter_dot_rect: None,
            filter_panel_rect: None,
            home_add_open: false,
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
        self.search_cancel.store(true, Ordering::SeqCst);
        let cancel = Arc::new(AtomicBool::new(false));
        self.search_cancel = cancel.clone();

        let query = self.query.clone();
        let empty_query = query.trim().is_empty();
        let cat_set = self.cat_set.clone();
        let cat_mask: u16 = cat_set
            .iter()
            .fold(0u16, |m, c| m | (1u16 << (*c as u8)));
        let result_limit = self.result_limit;
        let scope = self.build_scope_filter();
        let store = self.store.clone();
        let usage = self.usage.clone();
        let tx = self.search_tx.clone();
        std::thread::Builder::new()
            .name("shunseek-search".into())
            .spawn(move || {
                let tx_panic = tx.clone();
                let body = std::panic::AssertUnwindSafe(|| {
                    std::thread::sleep(Duration::from_millis(30));
                    if cancel.load(Ordering::SeqCst) {
                        return;
                    }
                    let started = Instant::now();
                    let rows: Vec<ResultRow>;
                    {
                        let store_g = lock_rec(&store);
                        let usage_g = lock_rec(&usage);
                        if empty_query {
                            // 首页由 UI 侧 pins 绘制；空查询只回空结果集，
                            // 保持消息管线（计数/选中复位）一致。
                            rows = Vec::new();
                        } else {
                            let engine = SearchEngine::new(&store_g, Some(&usage_g));
                            let hits = engine.search_scoped(
                                &query,
                                cat_mask,
                                result_limit,
                                &cancel,
                                scope.as_ref(),
                            );
                            if cancel.load(Ordering::SeqCst) {
                                return;
                            }
                            rows = hits
                                .iter()
                                .map(|h| {
                                    let i = h.index as usize;
                                    let name = store_g.name(i);
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
                                    ResultRow {
                                        name: name.to_string(),
                                        path: store_g.full_path(i),
                                        category: store_g.category(i),
                                        // 拼音首字母 / 子序列命中的 match_start
                                        // 不是连续命中段，不高亮以免标错位置；
                                        // 连续命中也先收敛边界再存。
                                        match_start: if h.via_initials || h.fuzzy {
                                            usize::MAX
                                        } else {
                                            floor_char_boundary(
                                                name,
                                                h.match_start as usize,
                                            )
                                        },
                                        match_len,
                                        demoted: h.demoted,
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
                });
                if std::panic::catch_unwind(body).is_err() {
                    // panic 兜底：仍发 Done（error），UI 提示已自动恢复；
                    // 就算锁被毒化，下一次搜索的 lock_rec 也能恢复。
                    let _ = tx_panic.send(SearchMsg::Done {
                        rows: Vec::new(),
                        elapsed_ms: 0.0,
                        empty_query,
                        error: true,
                    });
                }
            })
            .ok();
    }
    fn hide(&mut self, ctx: &egui::Context) {
        self.visible = false;
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
    }

    fn show(&mut self, ctx: &egui::Context) {
        self.visible = true;
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        // 呼出时清空 + 聚焦搜索框（对 C# 版 ToggleShow 的 SelectAll 的等价体感）
        if !self.query.is_empty() {
            self.query.clear();
        }
        // 无论查询是否清空都重搜一次：空查询首页按使用频次排序，
        // 刚打开过的条目计数 +1，呼出时要立刻反映到排序上。
        self.schedule_search();
        self.focus_search = true;
    }

    fn toggle(&mut self, ctx: &egui::Context) {
        if self.visible {
            self.hide(ctx);
        } else {
            // 刚被失焦隐藏（<300ms）又收到 Toggle：多半是同一次托盘
            // 点击的双路触发（失焦先藏、Toggle 后到），忽略这次，
            // 免得窗口藏好又弹出来。用户再点一次即可正常呼出。
            if let Some(t) = self.focus_hide_at {
                if t.elapsed() < Duration::from_millis(300) {
                    self.focus_hide_at = None;
                    return;
                }
            }
            self.show(ctx);
        }
    }

    fn open_row(&mut self, ctx: &egui::Context, idx: usize, locate: bool) {
        let Some(row) = self.rows.get(idx).cloned() else {
            return;
        };
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
            Some(DisplayRow::Group { dir, .. }) => self.toggle_group(&dir),
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

    /// 由 rows 重建展示行（两段式）：重要结果（demoted=false）在前，
    /// 段内同父 ≥4 条折成组行（出现在该目录首个成员的位置，展开后
    /// 成员带缩进跟在后面）；被降权的结果整体移到尾部段，段内同父
    /// ≥2 条即折成组、单条平铺。只作用于已取回的这批结果（≤100 条）。
    fn rebuild_display(&mut self) {
        let mut display: Vec<DisplayRow> = Vec::with_capacity(self.rows.len());
        if self.fold_active && self.fold_demoted {
            Self::display_section(&self.rows, &self.expanded_dirs, Some(false), 4, &mut display);
            Self::display_section(&self.rows, &self.expanded_dirs, Some(true), 2, &mut display);
        } else if self.fold_active {
            // 设置里关掉「折叠不重要结果」：demoted 不再单独归类，
            // 全体按同一套 ≥4 规则派生（core 的排序降权仍在，
            // 不重要结果自然靠后逐条平铺）
            Self::display_section(&self.rows, &self.expanded_dirs, None, 4, &mut display);
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

    /// 展示行派生的一段：从 rows 里挑 demoted 标志匹配 `demoted`
    ///（None = 全部）的条目（保持原相对顺序），同父目录计数
    /// ≥ threshold 的折成组行。
    fn display_section(
        rows: &[ResultRow],
        expanded_dirs: &HashSet<String>,
        demoted: Option<bool>,
        threshold: usize,
        display: &mut Vec<DisplayRow>,
    ) {
        let in_section = |r: &ResultRow| demoted.map_or(true, |d| r.demoted == d);
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for r in rows {
            if in_section(r) {
                let p = parent_dir(&r.path);
                if !p.is_empty() {
                    *counts.entry(p).or_default() += 1;
                }
            }
        }
        let mut emitted: HashSet<&str> = HashSet::new();
        for (i, r) in rows.iter().enumerate() {
            if !in_section(r) {
                continue;
            }
            let p = parent_dir(&r.path);
            if counts.get(p).copied().unwrap_or(0) >= threshold {
                if emitted.insert(p) {
                    let expanded = expanded_dirs.contains(p);
                    display.push(DisplayRow::Group {
                        dir: p.to_string(),
                        count: counts[p],
                        expanded,
                    });
                    if expanded {
                        for (j, rr) in rows.iter().enumerate() {
                            if in_section(rr) && parent_dir(&rr.path) == p {
                                display.push(DisplayRow::Single {
                                    row: j,
                                    indent: true,
                                });
                            }
                        }
                    }
                }
                // 该目录的其余成员已被组行代表，跳过
            } else {
                display.push(DisplayRow::Single {
                    row: i,
                    indent: false,
                });
            }
        }
    }

    /// 展开/收起一个折叠组（按父目录路径记状态），重建展示行；
    /// 组行本身的位置不因展开/收起而移动，selected 无需调整。
    fn toggle_group(&mut self, dir: &str) {
        if !self.expanded_dirs.remove(dir) {
            self.expanded_dirs.insert(dir.to_string());
        }
        self.rebuild_display();
    }

    /// 切主题的唯一入口（状态栏快捷按钮 / 设置窗口共用）：
    /// 换 visuals + 写 settings.txt，两处不许各自为政。
    fn set_theme(&mut self, ctx: &egui::Context, theme: Theme) {
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
            "theme={}\nview={}\nhotkey={}\nresult_limit={}\nfold_demoted={}\n",
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
        roots.extend(self.scope_custom.iter().cloned());
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
            names.push(dir_last(c));
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
        self.pins.push(path.to_string());
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

    /// 由 pins 派生首页展示行：分类优先从当前索引查（一次全表扫描
    /// 只为命中 pins，pins 数量小）；不在索引里则按磁盘/扩展名推断
    /// （目标已删也照常显示：名称取路径末段，图标按路径取）。
    fn rebuild_home_items(&mut self) {
        let mut found: HashMap<String, Category> = HashMap::new();
        if !self.pins.is_empty() {
            let wanted: HashSet<String> =
                self.pins.iter().map(|p| p.to_lowercase()).collect();
            let store = lock_rec(&self.store);
            for i in 0..store.count() {
                let fp = store.full_path(i);
                let low = fp.to_lowercase();
                if wanted.contains(&low) {
                    found.insert(low, store.category(i));
                }
            }
        }
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
                let category = found.get(&p.to_lowercase()).copied().unwrap_or_else(|| {
                    if std::path::Path::new(p).is_dir() {
                        Category::Folder
                    } else {
                        shunseek_core::classify_by_extension(&name)
                    }
                });
                ResultRow {
                    name,
                    path: p.clone(),
                    category,
                    match_start: usize::MAX,
                    match_len: 0,
                    demoted: false,
                }
            })
            .collect();
    }

    /// 打开一个首页磁贴（与 open_row 同语义：记录使用 + 隐藏）。
    fn open_home_item(&mut self, ctx: &egui::Context, idx: usize, locate: bool) {
        let Some(row) = self.home_items.get(idx).cloned() else {
            return;
        };
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
        if self.rebuilding {
            return;
        }
        self.rebuilding = true;
        self.tray.set_rebuild_enabled(false);
        spawn_rebuild(self.data_dir.clone(), self.index_tx.clone());
    }

    fn open_settings(&mut self) {
        self.settings_open = true;
        // 自启动勾选以注册表实际状态为准，打开时回读一次；
        // 二次确认状态复位，别让上次点到一半的确认跨次打开残留
        self.settings_autostart = autostart::is_enabled();
        self.usage_confirm = false;
    }

    fn close_settings(&mut self) {
        self.settings_open = false;
        // 焦点还给搜索框（打开设置时被面板拿走）
        self.focus_search = true;
    }

    /// 原始按键事件处理（绕开 TextEdit 对方向键等的消费，保证在输入框
    /// 聚焦时 ↑↓ / Enter / Esc 也归我们管，同 C# 版 PreviewKeyDown）。
    fn handle_keys(&mut self, ctx: &egui::Context) {
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
                    // Esc 顺序：先收起展开中的筛选控件（并关掉范围
                    // 面板），没有展开控件时才隐藏窗口。
                    Key::Escape => {
                        if self.expanded_filter.take().is_some() {
                            ctx.memory_mut(|m| m.close_popup());
                        } else {
                            self.hide(ctx);
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
                    saved,
                    summary,
                    final_stage,
                } => {
                    *lock_rec(&self.store) = store;
                    self.index_count = count;
                    self.index_summary = summary.clone();
                    // pins 的分类随索引变化（在/不在索引），重建首页项
                    self.rebuild_home_items();
                    if final_stage {
                        self.rebuilding = false;
                        self.tray.set_rebuild_enabled(true);
                        self.status_alert = count == 0;
                        // 状态栏只放短格式（原版 "{N0} 项"）；完整来源
                        // 摘要只在设置窗口「索引」区（index_summary）。
                        self.status = if count == 0 {
                            ALERT_EMPTY_INDEX.to_string()
                        } else if saved {
                            format!("{} 项", group_thousands(count))
                        } else {
                            format!("{} 项 · 缓存写入失败", group_thousands(count))
                        };
                    } else {
                        // 阶段性换入：搜索已可对当前索引用，不等最终一级
                        self.status = "正在建立索引…".to_string();
                    }
                    self.schedule_search();
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

/// 竖向渐变背景（v0.1.6 改回单个 Mesh）：12 个横向分带拼成一张
/// 三角网（13×2 顶点、24 个三角形、**1 个 Shape**），替代旧版每帧
/// 100 条 rect_filled 色带。旧版弃 Mesh 是因为「顶点色被压暗」的
/// 观察——后来证实灰幕真凶是 Frame 阴影（已删），Mesh 本身无罪。
/// 顶点色按原版 InkFill 的对角方向 (0,0)→(0.45,1) 取 t：
/// t = (0.45·nx + ny) / 1.45，左右顶点 t 不同即成对角渐变；
/// 分带边界按圆角轮廓收边，四角不缺角不出界。
fn paint_vertical_gradient(
    painter: &egui::Painter,
    rect: egui::Rect,
    radius: f32,
    top: Color32,
    mid: Color32,
    bottom: Color32,
) {
    const BANDS: usize = 12;
    let mut mesh = egui::Mesh::default();
    for i in 0..=BANDS {
        let y = rect.top() + rect.height() * (i as f32 / BANDS as f32);
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
    for i in 0..BANDS {
        let a = (i * 2) as u32;
        mesh.indices
            .extend_from_slice(&[a, a + 1, a + 2, a + 1, a + 3, a + 2]);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// 顶部光晕（原版 TopGlow）：窗口顶部 220px 的白色渐隐层，方向
/// (0,0)→(0.2,1) 近似为竖向；峰值 alpha 由 palette 给（已折入原版
/// 层 Opacity 0.5）。v0.1.6 起与渐变同法：单个 Mesh（6 分带），
/// 替代旧版 18×4=72 个小矩形切片。
fn paint_top_glow(painter: &egui::Painter, rect: egui::Rect, radius: f32, peak: u8) {
    const GLOW_H: f32 = 220.0;
    const BANDS: usize = 6;
    let h = GLOW_H.min(rect.height());
    let mut mesh = egui::Mesh::default();
    for i in 0..=BANDS {
        let y = rect.top() + h * (i as f32 / BANDS as f32);
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
    for i in 0..BANDS {
        let a = (i * 2) as u32;
        mesh.indices
            .extend_from_slice(&[a, a + 1, a + 2, a + 1, a + 3, a + 2]);
    }
    painter.add(egui::Shape::mesh(mesh));
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

enum RowHit {
    None,
    Click,
    Double,
}

/// 范围面板的一行：选中 = 整行胶囊填充（glass chip 选中语言），
/// 不用复选框方块。单击切换、双击 solo（语义由调用方处理）。
/// 范围面板的一行（v0.1.6 选中语言与原版 FilterChip 统一）：
/// 选中 = PillFill 淡填充 + PillStroke 描边 + AccentText 字，
/// **禁用白填充**（v0.1.5 白胶囊贴白面板看不见，用户点名）；
/// 未选 = 透明底 TextMid 字；hover = MenuHover 淡填充。
/// 单击切换、双击 solo（语义由调用方处理）。
fn scope_row(ui: &mut egui::Ui, pal: Palette, label: &str, selected: bool) -> RowHit {
    let font = FontId::proportional(12.5);
    let (tr, tg, tb) = if selected {
        pal.chip_sel_text_rgb
    } else {
        pal.chip_text_rgb
    };
    let text_color = Color32::from_rgb(tr, tg, tb);
    let galley = ui.fonts(|f| f.layout_no_wrap(label.to_string(), font, text_color));
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    if selected {
        // 选中行悬停时叠 PillHover（原版 pill 悬停层），否则 PillFill
        let fill = if response.hovered() {
            pal.pill_hover
        } else {
            pal.pill_fill
        };
        painter.rect_filled(rect, Rounding::same(7.0), fill);
        painter.rect_stroke(
            rect,
            Rounding::same(7.0),
            Stroke::new(1.0_f32, pal.pill_stroke),
        );
    } else if response.hovered() {
        painter.rect_filled(rect, Rounding::same(7.0), pal.chip_hover_fill);
    }
    painter.galley(
        egui::pos2(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0),
        galley,
        text_color,
    );
    if response.double_clicked() {
        RowHit::Double
    } else if response.clicked() {
        RowHit::Click
    } else {
        RowHit::None
    }
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
    /// 右键菜单「添加到首页」（display 下标，处理方解析到结果行）。
    Pin(usize),
}

/// 首页磁贴动作（下标是 home_items 下标）。
enum HomeAction {
    Select(usize),
    Open(usize),
    Locate(usize),
    Remove(usize),
}

impl ShunSeekApp {
    /// 搜索框（原版 SearchField）：Surface 填充 + EdgeLine 描边的
    /// 高 54、圆角 12 面板；左放大镜 16 号 TextLow；输入 Serif 17
    /// TextHi、占位 Serif 16 TextLow；聚焦时左侧 2px 朱砂焦点条
    /// （淡入 ~150ms）；右侧「Enter」键帽（ChipFill + EdgeLine 边，
    /// Sans 9.5 TextLow）。v0.1.6 起 LeiBox 光束游走描边退役（视觉
    /// 照抄原版），这里唯一的动画是焦点条淡入：过渡期间用
    /// repaint_after(16ms) 续命，静止后一帧不多画。
    /// 书眉（原版 Header，高 46 的行区）：左 = 拖动区（标题
    /// 「瞬觅」Serif 22 TextHi + 1×14 EdgeStrong 竖线 + 副标题
    /// 「SHUNSEEK · 本机索引」Sans 9.5 TextLow）；右 = 工具按钮组
    /// （主题 / 最小化 / 最大化-还原 / 关闭，34×30）。拖动区按下
    /// 走 StartDrag、双击切换最大化。✕ = 隐藏到托盘（v0.1.6 用户
    /// 钉死：无条件，进程只从托盘菜单退出）。
    fn draw_header(&mut self, ui: &mut egui::Ui) {
        let pal = self.theme.palette();
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            // ---- 左：标题拖动区 ----
            let title_g = ui.fonts(|f| {
                f.layout_no_wrap("瞬觅".to_string(), serif(22.0), pal.name_text)
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
            let (drag_rect, drag_resp) =
                ui.allocate_exact_size(Vec2::new(drag_w, 30.0), egui::Sense::drag());
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
            if drag_resp.double_clicked() {
                self.maximized = !self.maximized;
                ctx.send_viewport_cmd(ViewportCommand::Maximized(self.maximized));
            } else if drag_resp.drag_started() {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
            }

            // ---- 右：工具按钮组（从右往左放：关闭/最大化/最小化/主题） ----
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if header_tool_button(ui, pal, WinGlyph::Close)
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
                if header_tool_button(ui, pal, max_glyph)
                    .on_hover_text(if self.maximized { "还原" } else { "最大化" })
                    .clicked()
                {
                    self.maximized = !self.maximized;
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(self.maximized));
                }
                ui.add_space(2.0);
                if header_tool_button(ui, pal, WinGlyph::Min)
                    .on_hover_text("最小化")
                    .clicked()
                {
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                }
                ui.add_space(2.0);
                // 主题切换（原版书眉的主题 ToolButton）：字形 = 将去的
                // 主题（深色时画太阳 = 去浅色），13px 级手绘。
                let (trect, tresp) =
                    ui.allocate_exact_size(Vec2::new(34.0, 30.0), egui::Sense::click());
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
        let pal = self.theme.palette();
        let frame = egui::Frame::none()
            .fill(pal.search_fill)
            .rounding(Rounding::same(12.0))
            .stroke(Stroke::new(1.0_f32, pal.edge_line))
            .inner_margin(egui::Margin {
                left: 17.0,
                right: 14.0,
                top: 15.0,
                bottom: 15.0,
            });
        let frame_resp = frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                // 放大镜：手绘圆 + 斜线，16px，TextLow（原版不随聚焦变色）
                let (icon_rect, _) =
                    ui.allocate_exact_size(Vec2::new(16.0, 24.0), egui::Sense::hover());
                let painter = ui.painter_at(icon_rect);
                let c = icon_rect.center() + Vec2::new(-1.0, -1.0);
                let st = Stroke::new(1.6_f32, pal.search_icon);
                painter.circle_stroke(c, 5.0, st);
                painter.line_segment(
                    [c + Vec2::new(3.6, 3.6), c + Vec2::new(7.2, 7.2)],
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
                if self.focus_search {
                    out.response.request_focus();
                    self.focus_search = false;
                }
                if out.response.changed() {
                    self.schedule_search();
                }
                // 右侧「Enter」键帽（纯提示，与原版一致）
                egui::Frame::none()
                    .fill(pal.esc_fill)
                    .rounding(Rounding::same(5.0))
                    .stroke(Stroke::new(1.0_f32, pal.edge_line))
                    .inner_margin(egui::Margin::symmetric(7.0, 3.0))
                    .show(ui, |ui| {
                        ui.label(RichText::new("Enter").size(9.5).color(pal.esc_text));
                    });
            });
        });
        // 焦点标记条：2px 朱砂竖条，左内缩 1、上下内缩 10，圆角 1；
        // 聚焦淡入 / 失焦淡出（animate 0.18s 近似原版 150/200ms）。
        let rect = frame_resp.response.rect;
        let v = ui.ctx().animate_bool_with_time(
            ui.make_persistent_id("search_focus_bar"),
            self.search_focused,
            0.18,
        );
        if (0.0..1.0).contains(&v) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(16));
        }
        if v > 0.01 {
            let bar = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 1.0, rect.top() + 10.0),
                egui::pos2(rect.left() + 3.0, rect.bottom() - 10.0),
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
    }
    /// 工具栏一行（v0.1.5 定稿）：[类型控件] [范围控件] ……右端
    /// [宫格/列表切换]。原来的整排分类 chips 玻璃栏撤掉，腾出的
    /// 高度归结果区。两个筛选控件独立、互斥展开（expanded_filter
    /// 单字段，同时只许一个展开）。
    fn draw_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            self.draw_type_filter(ui);
            ui.add_space(8.0);
            self.draw_scope_filter(ui);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // 视图切换是主页面主控件：一眼可见、点即切换、切换即
                // 持久化（与设置「启动时默认显示」共用同一状态/键）。
                // 文案 = 点后会变成的形态，图标 = 当前形态。
                let (label, grid_now) = match self.view_mode {
                    ViewMode::List => ("宫格", false),
                    ViewMode::Grid => ("列表", true),
                };
                if view_toggle_button(ui, self.theme.palette(), label, grid_now).clicked() {
                    self.set_view_mode(self.view_mode.toggled());
                }
            });
        });
    }

    /// 类型筛选控件：「圆点变胶囊」。收起 = 26px 玻璃圆点（漏斗
    /// 图形；恰选中一类时圆点底色换该分类色、漏斗保持白色；多选
    /// 时强调描边 + 数量角标；全选时纯玻璃）；点击展开成胶囊，
    /// 内排「全部 + 12 类」迷你 chip（横向可滚）。选中态 = glass
    /// chip 选中语言（填充 + 深字），不用勾选符号。选择语义是
    /// 「全集模型」：全部 = 真实选项、永不空选，见 toggle_cat。
    fn draw_type_filter(&mut self, ui: &mut egui::Ui) {
        let pal = self.theme.palette();
        let expanded = self.expanded_filter == Some(FilterKind::Type);
        let v = ui.ctx().animate_bool_with_time(
            ui.make_persistent_id("type_expand"),
            expanded,
            0.18,
        );
        if (0.0..1.0).contains(&v) {
            // 展开/收起过渡期间 60fps 续命，静止即停（v0.1.6 帧率纪律）
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(16));
        }
        let full = self.cat_set.len() >= ALL_CATEGORIES.len();
        let target_w = if expanded {
            (ui.available_width() - 300.0).clamp(200.0, 470.0)
        } else {
            26.0
        };
        let w = 26.0 + (target_w - 26.0) * v;
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(w, 26.0), egui::Sense::click());
        let painter = ui.painter_at(rect);
        let dot_center = egui::pos2(rect.left() + 13.0, rect.center().y);
        if expanded {
            // 点外关闭判定（见 update）：圆点与展开胶囊是同一个 Rect
            self.filter_dot_rect = Some(rect);
            self.filter_panel_rect = Some(rect);
        }
        if v > 0.02 {
            painter.rect_filled(rect, Rounding::same(13.0), pal.bar_fill);
            if pal.bar_stroke.a() > 0 {
                painter.rect_stroke(
                    rect,
                    Rounding::same(13.0),
                    Stroke::new(1.0_f32, pal.bar_stroke),
                );
            }
        } else if self.cat_set.len() == 1 {
            let (r, g, b) = self.cat_set.iter().next().unwrap().tile_color();
            painter.circle_filled(dot_center, 13.0, Color32::from_rgb(r, g, b));
        } else {
            painter.circle_filled(dot_center, 13.0, pal.esc_fill);
            if !full {
                painter.circle_stroke(dot_center, 12.5, Stroke::new(1.6_f32, pal.accent));
            }
        }
        // 漏斗图标（单选且收起时圆点是分类色，漏斗画白色）
        let funnel_color = if self.cat_set.len() == 1 && v <= 0.02 {
            Color32::WHITE
        } else {
            pal.name_text
        };
        paint_funnel(&painter, dot_center, 14.0, funnel_color);
        // 多选数量角标
        if !full && self.cat_set.len() > 1 {
            let badge = egui::pos2(rect.left() + 22.0, rect.top() + 4.0);
            painter.circle_filled(badge, 7.0, pal.accent);
            painter.text(
                badge,
                egui::Align2::CENTER_CENTER,
                self.cat_set.len().to_string(),
                FontId::proportional(9.5),
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
        // 展开内容：迷你 chips（在漏斗点右侧，横向滚动）
        if v > 0.05 {
            let inner = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 28.0, rect.top()),
                egui::pos2(rect.right() - 4.0, rect.bottom()),
            );
            let mut child =
                ui.child_ui(inner, Layout::left_to_right(Align::Center), None);
            egui::ScrollArea::horizontal()
                .id_salt("type_chips")
                .auto_shrink([false, false])
                .scroll_bar_visibility(
                    egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                )
                .show(&mut child, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        for &chip in TYPE_CHIPS {
                            let (label, selected) = match chip {
                                None => ("全部".to_string(), full),
                                Some(c) => (
                                    c.display_name().to_string(),
                                    self.cat_set.contains(&c),
                                ),
                            };
                            let r = glass_chip(ui, pal, &label, selected);
                            if r.double_clicked() {
                                if let Some(c) = chip {
                                    self.toggle_cat(c, true);
                                }
                            } else if r.clicked() {
                                match chip {
                                    None => {
                                        if !full {
                                            self.cat_set = all_categories();
                                            self.schedule_search();
                                        }
                                    }
                                    Some(c) => self.toggle_cat(c, false),
                                }
                            }
                        }
                    });
                });
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
    }

    /// 类型选中集切换（全集模型、永不空选）：单击切换一项；
    /// 双击 solo（已是 solo 再双击 → 回全集）；取消到只剩最后一项
    /// 时再点它 → 跳回全集；逐个点满即全集（「全部」自动亮起就是
    /// cat_set 满员的派生状态，不单独存）。
    fn toggle_cat(&mut self, cat: Category, solo: bool) {
        if solo {
            if self.cat_set.len() == 1 && self.cat_set.contains(&cat) {
                self.cat_set = all_categories();
            } else {
                self.cat_set = HashSet::from([cat]);
            }
        } else if self.cat_set.len() == 1 && self.cat_set.contains(&cat) {
            self.cat_set = all_categories();
        } else if !self.cat_set.remove(&cat) {
            self.cat_set.insert(cat);
        }
        self.schedule_search();
    }

    /// 范围筛选控件：与类型同款「圆点变胶囊」。收起 = 文件夹图形
    /// 圆点（非全部时强调描边）+ 旁边摘要胶囊（全部 / 桌面 /
    /// 桌面 +2）；点击展开 → 圆点变胶囊并在下方挂出多选面板
    ///（预设 / 磁盘 / 自定义文件夹）。面板行选中 = 整行填充高亮，
    /// 不用复选框；单击切换、双击 solo，语义与类型同构（永不
    /// 空选，见 scope_toggle_*）。面板点外关闭时控件同步收起。
    fn draw_scope_filter(&mut self, ui: &mut egui::Ui) {
        let pal = self.theme.palette();
        let expanded = self.expanded_filter == Some(FilterKind::Scope);
        let all = self.scope_is_all();
        let summary = self.scope_summary();
        let show_summary = !all || expanded;
        let font = FontId::proportional(11.0);
        let sum_galley =
            ui.fonts(|f| f.layout_no_wrap(summary.clone(), font, pal.esc_text));
        let sum_w = if show_summary {
            sum_galley.size().x + 16.0
        } else {
            0.0
        };
        let w = 26.0 + if sum_w > 0.0 { 6.0 + sum_w } else { 0.0 };
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(w, 26.0), egui::Sense::click());
        let painter = ui.painter_at(rect);
        let dot_center = egui::pos2(rect.left() + 13.0, rect.center().y);
        if expanded {
            // 点外关闭判定（见 update）：圆点 Rect（面板 Rect 在 popup 里记）
            self.filter_dot_rect = Some(rect);
        }
        if expanded {
            painter.rect_filled(rect, Rounding::same(13.0), pal.bar_fill);
            if pal.bar_stroke.a() > 0 {
                painter.rect_stroke(
                    rect,
                    Rounding::same(13.0),
                    Stroke::new(1.0_f32, pal.bar_stroke),
                );
            }
        } else {
            painter.circle_filled(dot_center, 13.0, pal.esc_fill);
            if !all {
                painter.circle_stroke(dot_center, 12.5, Stroke::new(1.6_f32, pal.accent));
            }
        }
        // 文件夹字形（真实比例矢量字形，一眼是「范围/目录」）
        let tile = egui::Rect::from_center_size(dot_center, Vec2::splat(18.0));
        paint_folder_glyph(&painter, tile);
        if show_summary {
            let pill = egui::Rect::from_min_size(
                egui::pos2(rect.left() + 32.0, rect.center().y - 9.0),
                Vec2::new(sum_w, 18.0),
            );
            if !expanded {
                painter.rect_filled(pill, Rounding::same(9.0), pal.esc_fill);
            }
            painter.galley(
                egui::pos2(
                    pill.center().x - sum_galley.size().x / 2.0,
                    pill.center().y - sum_galley.size().y / 2.0,
                ),
                sum_galley,
                pal.esc_text,
            );
        }
        if resp.clicked() {
            self.expanded_filter = if expanded {
                None
            } else {
                Some(FilterKind::Scope)
            };
        }
        resp.clone()
            .on_hover_text(format!("范围筛选：{summary}"));

        // 展开面板：状态先克隆到本地改，收尾统一写回（避开 popup
        // 闭包与 &mut self 的借用冲突）。
        let popup_id = ui.make_persistent_id("scope_popup");
        if expanded {
            ui.memory_mut(|m| m.open_popup(popup_id));
        }
        let mut presets = self.scope_presets.clone();
        let mut drives = self.scope_drives.clone();
        let mut custom = self.scope_custom.clone();
        let mut changed = false;
        let mut panel_rect: Option<egui::Rect> = None;
        egui::containers::popup::popup_below_widget(
            ui,
            popup_id,
            &resp,
            egui::containers::popup::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_min_width(260.0);
                ui.label(
                    RichText::new("预设").size(11.0).color(pal.status_text),
                );
                let all_now = PRESET_KEYS
                    .iter()
                    .all(|(k, _, _)| presets.contains(*k))
                    && existing_drives().iter().all(|d| drives.contains(d));
                if matches!(scope_row(ui, pal, "全部", all_now), RowHit::Click) {
                    scope_select_all(&mut presets, &mut drives);
                    changed = true;
                }
                for (key, _, label) in PRESET_KEYS {
                    match scope_row(ui, pal, label, presets.contains(key)) {
                        RowHit::Click => {
                            scope_toggle_preset(&mut presets, &mut drives, key, false);
                            changed = true;
                        }
                        RowHit::Double => {
                            scope_toggle_preset(&mut presets, &mut drives, key, true);
                            changed = true;
                        }
                        RowHit::None => {}
                    }
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new("磁盘").size(11.0).color(pal.status_text),
                );
                for d in existing_drives() {
                    match scope_row(ui, pal, &format!("{d}:\\"), drives.contains(&d)) {
                        RowHit::Click => {
                            scope_toggle_drive(&mut presets, &mut drives, d, false);
                            changed = true;
                        }
                        RowHit::Double => {
                            scope_toggle_drive(&mut presets, &mut drives, d, true);
                            changed = true;
                        }
                        RowHit::None => {}
                    }
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new("自定义文件夹")
                        .size(11.0)
                        .color(pal.status_text),
                );
                let mut remove: Option<usize> = None;
                for (i, path) in custom.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(path).size(11.5).color(pal.name_text),
                        );
                        if ui.small_button("删除").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    custom.remove(i);
                    changed = true;
                }
                #[cfg(windows)]
                {
                    if ui.button("添加文件夹…").clicked() {
                        if let Some(p) = rfd::FileDialog::new().pick_folder() {
                            let s = p.to_string_lossy().into_owned();
                            if !custom.contains(&s) {
                                custom.push(s);
                                changed = true;
                            }
                        }
                    }
                }
                #[cfg(not(windows))]
                {
                    ui.add_enabled(false, egui::Button::new("添加文件夹…"));
                }
                // 点外关闭判定用：记下本帧面板的实际 Rect（见 update）
                panel_rect = Some(ui.min_rect());
            },
        );
        if let Some(r) = panel_rect {
            self.filter_panel_rect = Some(r);
        }
        if changed {
            self.scope_presets = presets;
            self.scope_drives = drives;
            self.scope_custom = custom;
            self.save_settings();
            self.schedule_search();
        }
        // 面板被点外关掉 → 控件同步收起
        if expanded && !ui.memory(|m| m.is_popup_open(popup_id)) {
            self.expanded_filter = None;
        }
    }

    fn draw_results(&mut self, ui: &mut egui::Ui) {
        // 首页（空查询）：只显用户 pins、强制宫格，不走搜索结果管线
        if self.is_home() {
            self.draw_home(ui);
            return;
        }
        if self.rows.is_empty() {
            let pal = self.theme.palette();
            // 空状态文案（原版）：索引还在建 / 没有匹配的内容
            let text = if self.rebuilding && self.index_count == 0 {
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
        let pal = self.theme.palette();
        if self.home_items.is_empty() {
            // 引导文案（原版空状态语言：Serif 14 TextLow 居中）
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), egui::Sense::hover());
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
            ui.add_space(6.0);
        } else {
            // 说明行（原版 HomeCaption：Sans 10.5 TextLow，下 Margin 10）
            ui.label(
                RichText::new(format!("已固定 {}", self.home_items.len()))
                    .size(10.5)
                    .color(pal.status_text),
            );
            ui.add_space(10.0);
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
        let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if self.scroll_to_selected {
            let offset =
                ((self.selected / cols) as f32 * row_pitch - 120.0).max(0.0);
            scroll = scroll.vertical_scroll_offset(offset);
            self.scroll_to_selected = false;
        }
        scroll.show_rows(ui, row_pitch, grid_rows, |ui, range| {
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
                        // 「＋ 添加」格：EdgeLine 描边 + 手绘加号 + 文字
                        let resp = ui.interact(
                            cell,
                            ui.id().with("home_add_cell"),
                            egui::Sense::click(),
                        );
                        let painter = ui.painter_at(cell);
                        if resp.hovered() {
                            painter.rect_filled(
                                cell,
                                Rounding::same(12.0),
                                pal.surface_hover,
                            );
                        }
                        painter.rect_stroke(
                            cell,
                            Rounding::same(12.0),
                            Stroke::new(1.0_f32, pal.edge_line),
                        );
                        let cc = egui::pos2(cell.center().x, cell.top() + 30.0);
                        let plus = Stroke::new(2.0_f32, pal.path_text);
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
                            FontId::proportional(11.0),
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

        // 「＋ 添加」弹层：选文件 / 选文件夹（锚在添加格下方）
        let popup_id = ui.make_persistent_id("home_add_popup");
        if add_clicked {
            self.home_add_open = !self.home_add_open;
        }
        if self.home_add_open {
            ui.memory_mut(|m| m.open_popup(popup_id));
        }
        #[allow(unused_mut)] // 只有 Windows 分支会改它们（rfd 对话框）
        let mut pick_file = false;
        #[allow(unused_mut)]
        let mut pick_folder = false;
        if let Some(resp) = &add_resp {
            egui::containers::popup::popup_below_widget(
                ui,
                popup_id,
                resp,
                egui::containers::popup::PopupCloseBehavior::CloseOnClickOutside,
                |ui| {
                    ui.set_min_width(150.0);
                    #[cfg(windows)]
                    {
                        if ui.button("选择文件…").clicked() {
                            pick_file = true;
                            ui.close_menu();
                        }
                        if ui.button("选择文件夹…").clicked() {
                            pick_folder = true;
                            ui.close_menu();
                        }
                    }
                    #[cfg(not(windows))]
                    {
                        ui.add_enabled(false, egui::Button::new("选择文件…"));
                        ui.add_enabled(false, egui::Button::new("选择文件夹…"));
                    }
                },
            );
        }
        if self.home_add_open && !ui.memory(|m| m.is_popup_open(popup_id)) {
            self.home_add_open = false;
        }
        // 系统对话框会抢焦点：先置抑制位，对话框关后一帧的失焦不隐藏
        #[cfg(windows)]
        {
            if pick_file {
                self.suppress_focus_hide = true;
                if let Some(p) = rfd::FileDialog::new().pick_file() {
                    self.add_pin(&p.to_string_lossy().into_owned());
                }
            }
            if pick_folder {
                self.suppress_focus_hide = true;
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
            Some(HomeAction::Remove(idx)) => {
                if let Some(item) = self.home_items.get(idx).cloned() {
                    self.remove_pin(&item.path);
                }
            }
            None => {}
        }
    }

    /// 一个首页磁贴（原版 TileItem）：170×92、圆角 12、Surface 底
    /// + EdgeLine 边；图标 44×44（行高 54、上距 10），未取到时
    /// 40×40 IconPlate 占位盘；名称 Serif 12.5 居中单行省略。
    /// 悬停叠 SurfaceHover，选中（键盘焦点）叠 SurfaceSel +
    /// PillStroke 边。右键菜单：打开 / 打开所在文件夹 / 从首页移除。
    fn draw_home_tile(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        idx: usize,
    ) -> Option<HomeAction> {
        let pal = self.theme.palette();
        let response = ui.interact(rect, ui.id().with(("home_tile", idx)), egui::Sense::click());
        let painter = ui.painter_at(rect);
        let selected = idx == self.selected;
        painter.rect_filled(rect, Rounding::same(12.0), pal.surface);
        if selected {
            painter.rect_filled(rect, Rounding::same(12.0), pal.surface_sel);
        } else if response.hovered() {
            painter.rect_filled(rect, Rounding::same(12.0), pal.surface_hover);
        }
        if selected {
            painter.rect_stroke(
                rect,
                Rounding::same(12.0),
                Stroke::new(1.0_f32, pal.pill_stroke),
            );
        } else {
            painter.rect_stroke(
                rect,
                Rounding::same(12.0),
                Stroke::new(1.0_f32, pal.edge_line),
            );
        }
        let item = self.home_items.get(idx)?;
        // 图标：真实图标优先（按路径取，目标已删时 worker 取不到
        // 就恒占位盘，格子照常显示，不崩）
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(rect.center().x - 22.0, rect.top() + 10.0),
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
            }
        }
        // 名称：Serif 12.5，左右 Margin 14，单行省略
        let name_clip = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 14.0, rect.top() + 60.0),
            egui::pos2(rect.right() - 14.0, rect.bottom() - 2.0),
        );
        let np = painter.with_clip_rect(name_clip);
        let galley = np.layout_no_wrap(item.name.clone(), serif(12.5), pal.name_text);
        np.galley(
            egui::pos2(
                rect.center().x - galley.size().x / 2.0,
                rect.bottom() - 10.0 - galley.size().y,
            ),
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
        let mut menu_act = None;
        response.context_menu(|ui| {
            if ui.button("打开").clicked() {
                menu_act = Some(HomeAction::Open(idx));
                ui.close_menu();
            }
            if ui.button("打开所在文件夹").clicked() {
                menu_act = Some(HomeAction::Locate(idx));
                ui.close_menu();
            }
            ui.separator();
            if ui.button("从首页移除").clicked() {
                menu_act = Some(HomeAction::Remove(idx));
                ui.close_menu();
            }
        });
        menu_act.or(act)
    }

    fn draw_results_list(&mut self, ui: &mut egui::Ui) {
        let mut action: Option<RowAction> = None;
        let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if self.scroll_to_selected {
            let offset = (self.selected as f32 * ROW_H - 140.0).max(0.0);
            scroll = scroll.vertical_scroll_offset(offset);
            self.scroll_to_selected = false;
        }
        scroll.show_rows(ui, ROW_H, self.display.len(), |ui, range| {
            for idx in range {
                if let Some(a) = self.draw_display_row(ui, idx) {
                    action = Some(a);
                }
            }
        });
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
            Some(RowAction::Pin(idx)) => {
                self.selected = idx;
                if let Some(DisplayRow::Single { row, .. }) =
                    self.display.get(idx).cloned()
                {
                    if let Some(r) = self.rows.get(row) {
                        let p = r.path.clone();
                        self.add_pin(&p);
                    }
                }
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
        let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if self.scroll_to_selected {
            let offset = ((self.selected / cols) as f32 * GRID_CELL_H - 140.0).max(0.0);
            scroll = scroll.vertical_scroll_offset(offset);
            self.scroll_to_selected = false;
        }
        scroll.show_rows(ui, GRID_CELL_H, grid_rows, |ui, range| {
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
            Some(RowAction::Pin(idx)) => {
                self.selected = idx;
                if let Some(DisplayRow::Single { row, .. }) =
                    self.display.get(idx).cloned()
                {
                    if let Some(r) = self.rows.get(row) {
                        let p = r.path.clone();
                        self.add_pin(&p);
                    }
                }
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
        let pal = self.theme.palette();
        // 先把展示项按小拷贝取出（Single 全是 Copy 字段；Group 只
        // clone dir 字符串），行本体绘制时借用，不整行 clone。
        enum CellData {
            Single(usize, bool),
            Group(String, usize, bool),
        }
        let data = match self.display.get(idx)? {
            DisplayRow::Single { row, indent } => CellData::Single(*row, *indent),
            DisplayRow::Group {
                dir,
                count,
                expanded,
            } => CellData::Group(dir.clone(), *count, *expanded),
        };
        let rect = cell.shrink(3.0);
        let response = ui.interact(rect, ui.id().with(("grid_cell", idx)), egui::Sense::click());
        let painter = ui.painter_at(rect);
        let selected = idx == self.selected;
        if selected {
            painter.rect_filled(rect, Rounding::same(10.0), pal.surface_sel);
            painter.rect_stroke(
                rect,
                Rounding::same(10.0),
                Stroke::new(1.0_f32, pal.pill_stroke),
            );
        } else if response.hovered() {
            painter.rect_filled(rect, Rounding::same(10.0), pal.surface_hover);
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
                // 名字：Serif 12，最多 2 行、超出省略、居中
                let galley = ui.fonts(|f| {
                    f.layout(
                        r.name.clone(),
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
                let mut act = None;
                if response.double_clicked() {
                    act = Some(RowAction::Open(idx));
                } else if response.clicked() {
                    act = Some(RowAction::Select(idx));
                }
                let mut menu_act = None;
                response.context_menu(|ui| {
                    if ui.button("打开").clicked() {
                        menu_act = Some(RowAction::Open(idx));
                        ui.close_menu();
                    }
                    if ui.button("打开所在文件夹").clicked() {
                        menu_act = Some(RowAction::Locate(idx));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("添加到首页").clicked() {
                        menu_act = Some(RowAction::Pin(idx));
                        ui.close_menu();
                    }
                });
                menu_act.or(act)
            }
            CellData::Group(dir, count, expanded) => {
                if response.hovered() {
                    self.hover_seen = Some(HoverTarget {
                        name: dir_last(dir),
                        path: dir.clone(),
                        category: Category::Folder,
                    });
                }
                // 占位盘 + 文件夹字形（组格没有单一真实图标可取）
                painter.rect_filled(icon_rect, Rounding::same(10.0), pal.icon_plate);
                let glyph = egui::Rect::from_center_size(
                    icon_rect.center(),
                    Vec2::splat(30.0),
                );
                paint_folder_glyph(&painter, glyph);
                let galley = ui.fonts(|f| {
                    f.layout(
                        dir_last(dir),
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
                    Some(RowAction::Toggle(idx, dir.clone()))
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
            Some(DisplayRow::Group { dir, .. }) => self.toggle_group(&dir),
            Some(DisplayRow::Single { row, .. }) => self.open_row(ctx, row, locate),
            None => {}
        }
    }

    fn draw_display_row(&mut self, ui: &mut egui::Ui, idx: usize) -> Option<RowAction> {
        // 借用取出（v0.1.6 减负）：Single 的字段是 Copy；Group 只
        // clone 一条 dir 字符串，不再整行 clone DisplayRow。
        match self.display.get(idx)? {
            DisplayRow::Single { row, indent } => {
                let (r, i) = (*row, *indent);
                self.draw_row(ui, r, idx, i)
            }
            DisplayRow::Group {
                dir,
                count,
                expanded,
            } => {
                let (d, c, e) = (dir.clone(), *count, *expanded);
                self.draw_group_row(ui, idx, &d, c, e)
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
        let pal = self.theme.palette();
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), ROW_H),
            egui::Sense::click(),
        );
        let row = self.rows.get(row_idx)?;
        let selected = display_idx == self.selected;
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
        } else if response.hovered() {
            painter.rect_filled(rect, rounding, pal.row_hover_fill);
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
        let type_galley = painter.layout_no_wrap(
            row.category.display_name().to_string(),
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
            match row.category {
                Category::App | Category::Shortcut => pal.importance_high,
                _ => pal.importance_normal,
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
        let name = &row.name;
        // 高亮区间：match_start/match_len 都是字节量，切片前双双
        // 收敛到字符边界（core 语义钉死 + app 侧防御，见 search.rs）
        let hl = if row.match_start != usize::MAX && row.match_len > 0 {
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
        let mut menu_act = None;
        response.context_menu(|ui| {
            if ui.button("打开").clicked() {
                menu_act = Some(RowAction::Open(display_idx));
                ui.close_menu();
            }
            if ui.button("打开所在文件夹").clicked() {
                menu_act = Some(RowAction::Locate(display_idx));
                ui.close_menu();
            }
            ui.separator();
            if ui.button("添加到首页").clicked() {
                menu_act = Some(RowAction::Pin(display_idx));
                ui.close_menu();
            }
        });
        menu_act.or(act)
    }
    /// 折叠组行（原版 FoldRow）：FoldFill 底、圆角 9，内缩近似
    /// 原版 Margin 2,4,6,4；左侧小三角字形（9.5 级 TextLow）+
    /// Serif 12 TextMid 单行文字「{末两段} · N 个同目录结果」。
    /// 选中描 PillStroke 边，悬停叠 SurfaceHover。
    /// 点击 = 展开/收起（RowAction::Toggle），悬停提示完整父目录。
    fn draw_group_row(
        &mut self,
        ui: &mut egui::Ui,
        display_idx: usize,
        dir: &str,
        count: usize,
        expanded: bool,
    ) -> Option<RowAction> {
        let pal = self.theme.palette();
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
        painter.rect_filled(bg, Rounding::same(9.0), pal.fold_fill);
        if selected {
            painter.rect_stroke(
                bg,
                Rounding::same(9.0),
                Stroke::new(1.0_f32, pal.pill_stroke),
            );
        } else if response.hovered() {
            painter.rect_filled(bg, Rounding::same(9.0), pal.surface_hover);
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

        // 单行文字（裁剪到组行右内边距）
        let text = format!(
            "{} · {} 个同目录结果{}",
            dir_tail(dir),
            count,
            if expanded { " · 已展开" } else { "" }
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

        if response.hovered() {
            self.hover_seen = Some(HoverTarget {
                name: dir_last(dir),
                path: dir.to_string(),
                category: Category::Folder,
            });
        }
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.clicked() || response.double_clicked() {
            Some(RowAction::Toggle(display_idx, dir.to_string()))
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
        let pal = self.theme.palette();
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
        painter.text(
            egui::pos2(set_rect.left() + 25.0, set_rect.center().y),
            Align2::LEFT_CENTER,
            "设置",
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
            .on_hover_text("设置")
            .clicked()
        {
            self.open_settings();
        }
    }
}

/// ------------------------------------------------------------ 手绘图标
/// 全局 UI 规范：图标图案必须一眼能认出功能，不许用汉字充当图标。
/// 全部用 egui painter 手绘矢量图形，线宽 ≥1.8px、尺寸 ≥14px；
/// 纯图标控件另配 on_hover_text 兜底说明。

/// 漏斗（类型筛选）：上宽下窄 + 下方小竖管，闭合描线。
fn paint_funnel(painter: &egui::Painter, c: egui::Pos2, size: f32, color: Color32) {
    let s = size / 2.0;
    // 比例复核（v0.1.6）：上口全宽、颈宽 0.28、管直到底——26px
    // 圆点里一眼是漏斗，不糊成三角块。
    let pts = [
        egui::pos2(c.x - s, c.y - s * 0.85),
        egui::pos2(c.x + s, c.y - s * 0.85),
        egui::pos2(c.x + s * 0.28, c.y + s * 0.02),
        egui::pos2(c.x + s * 0.28, c.y + s * 0.85),
        egui::pos2(c.x - s * 0.28, c.y + s * 0.85),
        egui::pos2(c.x - s * 0.28, c.y + s * 0.02),
        egui::pos2(c.x - s, c.y - s * 0.85),
    ];
    painter.add(egui::Shape::line(pts.to_vec(), Stroke::new(1.8_f32, color)));
}

/// 文件夹字形（v0.1.6 按真实比例重画）：后片带梯形凸耳（左上
/// 一截 tab + 斜切肩），前片矩形略矮盖住后片下部；填充用收了
/// 饱和的文件夹黄（#F5C04A 系），描边 1.8px（按 26px 基准等比），
/// 前片上沿一笔高光。旧版是「黄方块 + 两个白矩形」，在 16px 圆点
/// 里糊成变形黄块，用户点名重画。范围圆点 / 宫格组格共用。
fn paint_folder_glyph(painter: &egui::Painter, rect: egui::Rect) {
    let (w, h) = (rect.width(), rect.height());
    let (x0, y0) = (rect.left(), rect.top());
    let p = |fx: f32, fy: f32| egui::pos2(x0 + w * fx, y0 + h * fy);
    let back_fill = Color32::from_rgb(224, 169, 62); // 后片稍深
    let front_fill = Color32::from_rgb(245, 192, 74); // 前片 #F5C04A
    let stroke = Stroke::new((1.8 * (w / 26.0)).max(1.2), Color32::from_rgb(166, 116, 32));
    // 后片：底边齐前片，顶部左侧凸耳 + 右侧斜肩
    let back = vec![
        p(0.06, 0.82),
        p(0.06, 0.18),
        p(0.40, 0.18),
        p(0.52, 0.32),
        p(0.94, 0.32),
        p(0.94, 0.82),
    ];
    painter.add(egui::Shape::convex_polygon(back, back_fill, stroke));
    // 前片：矩形，顶边比后片主体低一截，盖住后片下部
    let front = egui::Rect::from_min_max(p(0.02, 0.40), p(0.98, 0.88));
    painter.rect_filled(front, Rounding::same((w * 0.06).max(1.0)), front_fill);
    painter.rect_stroke(front, Rounding::same((w * 0.06).max(1.0)), stroke);
    // 前片上沿高光
    painter.line_segment(
        [p(0.06, 0.44), p(0.94, 0.44)],
        Stroke::new(1.0_f32, Color32::from_white_alpha(110)),
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

/// 书眉工具按钮（原版 ToolButton）：34×30，圆角 8；悬停浮层
/// SurfaceHover、按下 SurfaceSel，字形 TextMid → 悬停 TextHi
/// （原版淡入 120/淡出 180ms，egui 无逐按钮不透明度管线，即时
/// 换色近似）。关闭按钮走 CloseHover/ClosePressed/CloseText。
fn header_tool_button(ui: &mut egui::Ui, pal: Palette, glyph: WinGlyph) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(34.0, 30.0), egui::Sense::click());
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



/// 类型 chip（原版 FilterChip 选中语言）：未选 = 透明底 TextMid 字、
/// hover = MenuHover 淡填充；选中 = PillFill 淡填充 + PillStroke
/// 1px 描边 + AccentText 字——不用勾选符号、不用白填充（用户钉死：
/// 选中靠胶囊填充高亮，白贴白看不见）。选中过渡 ~0.15s 插值填充
/// 与字色，过渡期间 repaint_after(16ms) 续命，静止即停。高 24 是
/// 为了塞进 26px 的展开胶囊（原版面板内芯片高 30，此处近似）。
fn glass_chip(ui: &mut egui::Ui, pal: Palette, label: &str, selected: bool) -> egui::Response {
    let font = FontId::proportional(11.5);
    // 选中进度 v：0=未选中，1=完全选中，切换时平滑过渡
    let v = ui
        .ctx()
        .animate_bool_with_time(ui.make_persistent_id(("glass_chip_sel", label)), selected, 0.15);
    if (0.0..1.0).contains(&v) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(16));
    }
    let (tr, tg, tb) = pal.chip_text_rgb;
    let (sr, sg, sb) = pal.chip_sel_text_rgb;
    let text_color = Color32::from_rgb(
        lerp_u8(tr, sr, v),
        lerp_u8(tg, sg, v),
        lerp_u8(tb, sb, v),
    );
    let galley = ui
        .fonts(|f| f.layout_no_wrap(label.to_string(), font.clone(), text_color));
    let size = Vec2::new(galley.size().x + 18.0, 24.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter_at(rect);
    let rounding = Rounding::same(8.0);
    if v > 0.01 {
        let fade = |c: Color32| {
            Color32::from_rgba_unmultiplied(
                c.r(),
                c.g(),
                c.b(),
                (c.a() as f32 * v) as u8,
            )
        };
        painter.rect_filled(rect, rounding, fade(pal.pill_fill));
        painter.rect_stroke(rect, rounding, Stroke::new(1.0_f32, fade(pal.pill_stroke)));
    } else if response.hovered() {
        painter.rect_filled(rect, rounding, pal.chip_hover_fill);
    }
    painter.galley(
        egui::pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        text_color,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
impl ShunSeekApp {
    /// 设置窗口（独立 viewport，标题「瞬觅 设置」，约 480×540，
    /// 不置顶、可关）。入口：状态栏齿轮按钮 + 托盘「设置」项。
    /// 开关类设置全在这里，托盘只放动作，两边不重复。
    fn draw_settings_viewport(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let pal = self.theme.palette();
        let mut close_me = false;
        let builder = egui::ViewportBuilder::default()
            .with_title("瞬觅 设置")
            .with_inner_size([480.0, 540.0])
            .with_min_inner_size([380.0, 420.0]);
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("shunseek_settings"),
            builder,
            |vctx, _class| {
                if vctx.input(|i| i.viewport().close_requested()) {
                    close_me = true;
                }
                let frame = egui::Frame::central_panel(&vctx.style()).fill(pal.panel_fill);
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
        // ---------- 常规 ----------
        settings_heading(ui, pal, "常规");
        // 开机自启动：勾选状态以注册表实际为准（不入 settings.txt）
        let mut auto = self.settings_autostart;
        if ui
            .checkbox(&mut auto, RichText::new("开机自启动").color(pal.name_text))
            .changed()
        {
            autostart::set_enabled(auto);
            self.settings_autostart = autostart::is_enabled();
        }
        ui.add_space(6.0);
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
        ui.label(RichText::new("唤醒热键").color(pal.name_text));
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(self.hotkey_combo.display())
                    .size(13.0)
                    .color(pal.name_text),
            );
            if self.hotkey_capture {
                ui.label(
                    RichText::new("请按下快捷键… Esc 取消").color(pal.highlight),
                );
            } else if ui.button("修改").clicked() {
                self.hotkey_capture = true;
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
                ui.label(RichText::new("推荐：").size(11.5).color(pal.path_text));
                let mut pick: Option<hotkey::HotkeyCombo> = None;
                for c in &self.hotkey_recs {
                    if glass_chip(ui, pal, &c.display(), false).clicked() {
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
            ui.label(RichText::new("主题").color(pal.name_text));
            if ui.selectable_label(self.theme == Theme::Dark, "深色").clicked()
                && self.theme != Theme::Dark
            {
                self.set_theme(vctx, Theme::Dark);
            }
            if ui.selectable_label(self.theme == Theme::Light, "浅色").clicked()
                && self.theme != Theme::Light
            {
                self.set_theme(vctx, Theme::Light);
            }
        });
        ui.horizontal(|ui| {
            // 与主页面宫格/列表按钮共用同一状态与持久化键，页面按钮
            // 是主控件，这里只是「启动时默认显示」的默认项入口。
            ui.label(RichText::new("启动时默认显示").color(pal.name_text));
            if ui
                .selectable_label(self.view_mode == ViewMode::List, "列表")
                .clicked()
                && self.view_mode != ViewMode::List
            {
                self.set_view_mode(ViewMode::List);
            }
            if ui
                .selectable_label(self.view_mode == ViewMode::Grid, "宫格")
                .clicked()
                && self.view_mode != ViewMode::Grid
            {
                self.set_view_mode(ViewMode::Grid);
            }
        });

        // ---------- 搜索 ----------
        settings_heading(ui, pal, "搜索");
        ui.horizontal(|ui| {
            ui.label(RichText::new("结果上限").color(pal.name_text));
            for n in [60usize, 100, 200] {
                if ui.selectable_label(self.result_limit == n, n.to_string()).clicked()
                    && self.result_limit != n
                {
                    self.result_limit = n;
                    self.save_settings();
                    self.schedule_search();
                }
            }
        });
        let mut fold = self.fold_demoted;
        if ui
            .checkbox(
                &mut fold,
                RichText::new("折叠不重要结果").color(pal.name_text),
            )
            .changed()
        {
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
        ui.label(RichText::new(status).size(11.5).color(pal.path_text));
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
            let rebuild_label = if self.rebuilding {
                "正在建立索引…"
            } else {
                "重新建立索引"
            };
            if ui
                .add_enabled(!self.rebuilding, egui::Button::new(rebuild_label))
                .clicked()
            {
                self.start_rebuild();
            }
            if ui.button("打开缓存文件夹").clicked() {
                let dir = self.data_dir.to_string_lossy().into_owned();
                opener::open(&dir, "", false);
            }
            if ui.button("清除索引缓存").clicked() {
                for f in ["index.bin", "index-next.bin", "index.tmp", "index-next.tmp"] {
                    let _ = std::fs::remove_file(self.data_dir.join(f));
                }
                self.settings_note =
                    "索引缓存已清除，下次启动将重新建立（当前索引仍可继续使用）".to_string();
            }
        });

        // ---------- 数据 ----------
        settings_heading(ui, pal, "数据");
        if self.usage_confirm {
            ui.horizontal(|ui| {
                if ui.button("确认清除？").clicked() {
                    {
                        let mut u = lock_rec(&self.usage);
                        u.clear();
                        let _ = u.save();
                    }
                    self.usage_confirm = false;
                    self.settings_note = "使用记录已清除，常用统计从零开始".to_string();
                    self.schedule_search();
                }
                if ui.button("取消").clicked() {
                    self.usage_confirm = false;
                }
            });
        } else if ui.button("清除使用记录").clicked() {
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
                    modifiers.command,
                    vk,
                );
                if !(combo.ctrl || combo.alt || combo.shift || combo.win) {
                    self.hotkey_note =
                        "需要至少一个修饰键（Ctrl / Alt / Shift / Win）+ 主键".to_string();
                    continue;
                }
                self.hotkey_capture = false;
                self.apply_hotkey_check(combo);
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
        let pal = self.theme.palette();
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
                        let type_name = if is_dir {
                            "文件夹"
                        } else {
                            target.category.display_name()
                        };
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

/// 设置窗口的分区标题。
fn settings_heading(ui: &mut egui::Ui, pal: Palette, title: &str) {
    ui.add_space(12.0);
    ui.label(
        RichText::new(title)
            .size(12.5)
            .color(pal.path_text)
            .strong(),
    );
    ui.add_space(4.0);
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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 热键 / 托盘（都要在窗口隐藏时也能响应，靠各自线程 request_repaint 唤醒）
        if self.hotkey.take_fired() {
            self.toggle(ctx);
        }
        while let Some(action) = self.tray.poll() {
            match action {
                tray::TrayAction::Toggle => self.toggle(ctx),
                tray::TrayAction::Show => {
                    if self.visible {
                        ctx.send_viewport_cmd(ViewportCommand::Focus);
                    } else {
                        self.show(ctx);
                    }
                }
                tray::TrayAction::RebuildIndex => self.start_rebuild(),
                tray::TrayAction::OpenSettings => {
                    if !self.visible {
                        self.show(ctx);
                    }
                    self.open_settings();
                }
                tray::TrayAction::About => {
                    if !self.visible {
                        self.show(ctx);
                    }
                    // 版本信息临时显示在状态栏（下一次搜索完成会覆盖）
                    self.status = about_text();
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

        // 筛选面板点外关闭（第三条关闭路径，与圆点再点切换、Esc
        // 独立生效）：用上一帧记下的面板/圆点 Rect 判定本帧的
        // primary 按下点，落在两者之外就收起。
        if self.expanded_filter.is_some() {
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
                    .map(|r| r.contains(p))
                    .unwrap_or(false);
                let in_dot = self
                    .filter_dot_rect
                    .map(|r| r.contains(p))
                    .unwrap_or(false);
                if !in_panel && !in_dot {
                    self.expanded_filter = None;
                    ctx.memory_mut(|m| m.close_popup());
                }
            }
        } else {
            self.filter_dot_rect = None;
            self.filter_panel_rect = None;
        }

        // 失焦自动隐藏（呼出式工具的标准行为，也是不再置顶后的
        // 配套）：主窗口焦点 true→false 跳变时隐藏到托盘。例外不
        // 隐藏：设置窗口开着时；筛选面板展开时（点外先收面板，
        // 见上）；刚关掉系统文件对话框的一帧（suppress 标志）。
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        if self.visible && self.was_focused && !focused {
            if self.suppress_focus_hide {
                self.suppress_focus_hide = false;
            } else if !self.settings_open && self.expanded_filter.is_none() {
                self.focus_hide_at = Some(Instant::now());
                self.hide(ctx);
            }
        }
        self.was_focused = focused;

        self.poll_channels(ctx);
        if self.visible {
            self.handle_keys(ctx);
        }

        // 悬浮窗背景：原版 InkFill 三段渐变（单个 Mesh，见
        // paint_vertical_gradient）画在背景层，面板本身透明，
        // 靠 Frame 的 ShellStroke 描边定义外壳。取色全走 palette。
        let pal = self.theme.palette();
        let bg_painter = ctx.layer_painter(egui::LayerId::background());
        let screen = ctx.screen_rect();
        paint_vertical_gradient(
            &bg_painter,
            screen,
            12.0,
            pal.grad_top,
            pal.grad_mid,
            pal.grad_bottom,
        );
        // 顶部光晕（原版 TopGlow：顶部 220px 白色渐隐层，单个 Mesh）
        paint_top_glow(&bg_painter, screen, 12.0, pal.top_highlight_peak);
        // 注意：这里绝不能给 Frame 加 shadow。面板填充是透明的，egui 的
        // 阴影画在面板形状之后、内容之前，会整片透过透明面板压在渐变上
        // ——浅色下就是一层均匀的灰幕（v0.1.3 浅色「太灰」的真凶，深色下
        // 黑压黑看不出来）。本窗口与面板同尺寸，阴影本就没有可落的余地，
        // 边缘靠双层描边定义。
        let panel_frame = egui::Frame::central_panel(&ctx.style())
            .fill(Color32::TRANSPARENT)
            .rounding(Rounding::same(12.0))
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
                ui.add_space(12.0);
                self.draw_search_box(ui);
                ui.add_space(16.0);
                // 搜索框下发丝线（原版：高 1、HairLine）
                let (line_rect, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 1.0),
                    egui::Sense::hover(),
                );
                ui.painter_at(line_rect)
                    .rect_filled(line_rect, Rounding::ZERO, pal.hairline);
                ui.add_space(10.0);
                self.draw_toolbar(ui);
                ui.add_space(8.0);
                let footer_h = 28.0;
                let results_h =
                    (ui.available_height() - footer_h - 6.0).max(60.0);
                ui.allocate_ui(Vec2::new(ui.available_width(), results_h), |ui| {
                    self.draw_results(ui);
                });
                ui.add_space(6.0);
                self.draw_status_bar(ui);
            });

        // 设置窗口（独立 viewport）与悬停详情卡都画在主面板之后
        self.draw_settings_viewport(ctx);
        self.update_hover_card(ctx);
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

fn main() -> eframe::Result<()> {
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
