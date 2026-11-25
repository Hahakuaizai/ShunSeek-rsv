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

/// 宫格格子尺寸（宽×高）：图标 40 + 名字 2 行 + 分类小字。
const GRID_CELL_W: f32 = 104.0;
const GRID_CELL_H: f32 = 96.0;
/// 强调蓝（选中 / 聚焦 / chips），与 WPF 版 AccentBrush 对齐。
const ACCENT: Color32 = Color32::from_rgb(37, 99, 235);
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

/// 全套界面取色。v0.1.2 时这些颜色硬编码散落在各绘制函数里，v0.1.3
/// 全部收进这里：绘制代码只从 Palette 取色，不再写字面量。
/// Palette::dark() 的每个数值都与 v0.1.2 的硬编码逐项对齐，深色外观
/// 零变化；Palette::light() 按 LeiBox 原作（流光描边 / 玻璃标签栏本就是
/// 浅色设计）的浅色磨砂感另定，不是深色反色。
/// 用构造函数而非 const：from_white_alpha 等在 egui 0.29 不是 const fn。
#[derive(Clone, Copy)]
struct Palette {
    /// 窗口竖向渐变的上 / 下色。
    grad_top: Color32,
    grad_bottom: Color32,
    /// 顶部玻璃高光带的峰值 alpha（纵向渐隐到 0）。
    top_highlight_peak: u8,
    /// 面板描边：外层（Frame stroke）/ 内层（背景层内缩 1px 手绘）。
    panel_stroke: Color32,
    panel_inner_stroke: Color32,
    /// 面板投影色（offset/blur 在 update() 里固定，只换颜色）。
    shadow_color: Color32,
    /// 全局 Visuals 的 panel_fill / extreme_bg_color。
    panel_fill: Color32,
    extreme_bg: Color32,
    /// 弹窗（「更多」菜单等）填充 / 描边 / 投影色。
    popup_fill: Color32,
    popup_stroke: Color32,
    popup_shadow_color: Color32,
    /// 搜索框填充 / 流光描边的底圈淡边。
    search_fill: Color32,
    search_faint: Color32,
    /// 放大镜未聚焦色（聚焦色两主题同为 #3B82F6，见 draw_search_box）。
    search_icon: Color32,
    /// 输入文字色。深色取 egui 0.29 深色 Visuals 下 TextEdit 的默认
    /// widgets.inactive 文字色 gray(180)，显式写出与不写效果一致。
    search_text: Color32,
    /// hint 色。None = 不指定（egui 用它自算的 weak 色）——深色保持
    /// v0.1.2 原样；浅色给 Some(#9AA3B2)，靠带色 RichText 精确落色。
    hint: Option<Color32>,
    /// ESC 徽标与主题切换按钮共用的玻璃小片：填充 / 顶部高光 / 文字。
    esc_fill: Color32,
    esc_highlight: Color32,
    esc_text: Color32,
    /// 玻璃标签栏：填充 / 顶部高光 / 描边（深色无描边，TRANSPARENT）。
    bar_fill: Color32,
    bar_highlight: Color32,
    bar_stroke: Color32,
    /// chip 未选文字 RGB / 选中文字 RGB（选中态切换要按进度插值，
    /// 所以存 RGB 三元组而不是 Color32）。
    chip_text_rgb: (u8, u8, u8),
    chip_sel_text_rgb: (u8, u8, u8),
    /// 选中胶囊：白色填充的满格 alpha（按选中进度 v 缩放）。
    chip_sel_fill_alpha: u8,
    /// 选中胶囊脚下投影（向下 1px 黑影）的满格 alpha。
    chip_sel_shadow_alpha: u8,
    /// 选中胶囊顶部高光线的满格 alpha；浅色为 0（改走投影 + 描边圈）。
    chip_sel_highlight_alpha: u8,
    /// 选中胶囊描边圈（深色 TRANSPARENT 不画；浅色黑 alpha 10）。
    chip_sel_ring: Color32,
    /// 未选 chip 的 hover 填充。
    chip_hover_fill: Color32,
    /// 结果行 hover 填充 / 选中填充 / 选中行顶部高光线。
    row_hover_fill: Color32,
    row_sel_fill: Color32,
    row_sel_highlight: Color32,
    /// 分类色块上半部的玻璃高光 alpha。
    tile_gloss_alpha: u8,
    /// 名字 / 路径 / 空列表提示的文字色。
    name_text: Color32,
    path_text: Color32,
    empty_text: Color32,
    /// 名字命中段高亮色（兼下划线色）。
    highlight: Color32,
    /// 状态栏文字 / 右侧快捷键提示文字。
    status_text: Color32,
    status_hint: Color32,
    /// 警戒态文字与圆点色（空索引护栏）。
    alert_text: Color32,
    /// 就绪圆点色。
    dot_ok: Color32,
    /// 分类胶囊文字的调色方向：false=深色做法（各通道提亮，下限 165）；
    /// true=浅色做法（各通道压暗 ×0.62）。胶囊底两主题同为分类色 alpha 40。
    cat_text_darken: bool,
}

impl Palette {
    /// 分类胶囊文字色：由分类色按主题方向调出（见 cat_text_darken）。
    fn cat_text_color(&self, r: u8, g: u8, b: u8) -> Color32 {
        if self.cat_text_darken {
            Color32::from_rgb(
                (r as f32 * 0.62) as u8,
                (g as f32 * 0.62) as u8,
                (b as f32 * 0.62) as u8,
            )
        } else {
            Color32::from_rgb(r.max(165), g.max(165), b.max(165))
        }
    }

    /// 深色：v0.1.2 全部硬编码数值原样。
    fn dark() -> Self {
        Self {
    grad_top: Color32::from_rgb(37, 42, 53), // #252A35
    grad_bottom: Color32::from_rgb(22, 24, 29), // #16181D
    top_highlight_peak: 26,
    panel_stroke: Color32::from_white_alpha(22),
    panel_inner_stroke: Color32::from_white_alpha(10),
    shadow_color: Color32::from_black_alpha(140),
    panel_fill: Color32::from_rgb(22, 24, 29),
    extreme_bg: Color32::from_rgb(10, 11, 14),
    popup_fill: Color32::from_rgba_unmultiplied(30, 34, 43, 235),
    popup_stroke: Color32::from_white_alpha(24),
    popup_shadow_color: Color32::from_black_alpha(120),
    search_fill: Color32::from_white_alpha(22),
    search_faint: Color32::from_white_alpha(26),
    search_icon: Color32::from_rgb(139, 148, 158),
    search_text: Color32::from_gray(180),
    hint: None,
    esc_fill: Color32::from_white_alpha(10),
    esc_highlight: Color32::from_white_alpha(50),
    esc_text: Color32::from_rgb(110, 118, 129),
    bar_fill: Color32::from_white_alpha(14),
    bar_highlight: Color32::from_white_alpha(60),
    bar_stroke: Color32::TRANSPARENT,
    chip_text_rgb: (201, 209, 217),     // #C9D1D9
    chip_sel_text_rgb: (26, 29, 35),    // #1A1D23
    chip_sel_fill_alpha: 225,
    chip_sel_shadow_alpha: 80,
    chip_sel_highlight_alpha: 200,
    chip_sel_ring: Color32::TRANSPARENT,
    chip_hover_fill: Color32::from_white_alpha(10),
    row_hover_fill: Color32::from_white_alpha(8),
    row_sel_fill: Color32::from_rgba_unmultiplied(37, 99, 235, 46),
    row_sel_highlight: Color32::from_white_alpha(50),
    tile_gloss_alpha: 46,
    name_text: Color32::from_rgb(242, 242, 242),
    path_text: Color32::from_rgb(139, 148, 158),
    empty_text: Color32::from_rgb(154, 154, 154),
    highlight: Color32::from_rgb(251, 191, 36), // #FBBF24
    status_text: Color32::from_rgb(154, 154, 154),
    status_hint: Color32::from_rgb(110, 110, 110),
    alert_text: Color32::from_rgb(248, 113, 113), // #F87171
    dot_ok: Color32::from_rgb(52, 211, 153),      // #34D399
    cat_text_darken: false,
        }
    }

    /// 浅色：LeiBox 原作的浅色磨砂感——近白渐变底、白卡片浮起、
    /// 深字、强调蓝只出现在选中行与聚焦处。
    /// v0.1.5 按用户反馈去灰：渐变改近纯白（#FFFFFF → #F6F7F9），
    /// 不再用带灰蓝的 #F7F8FA→#E9ECF1；底变白后，原先靠半透明白叠
    /// 灰底显形的元素（搜索框 / 玻璃栏）提高不透明度、描边加深，
    /// 改靠描边分层。
    fn light() -> Self {
        Self {
    grad_top: Color32::from_rgb(255, 255, 255), // 纯白（v0.1.5 去灰）
    grad_bottom: Color32::from_rgb(246, 247, 249), // #F6F7F9，只留一丝层次
    top_highlight_peak: 90,
    panel_stroke: Color32::from_white_alpha(160),
    panel_inner_stroke: Color32::from_black_alpha(10),
    shadow_color: Color32::from_black_alpha(70),
    panel_fill: Color32::from_rgb(255, 255, 255),
    extreme_bg: Color32::from_rgb(255, 255, 255),
    popup_fill: Color32::from_rgba_unmultiplied(250, 250, 252, 242),
    popup_stroke: Color32::from_black_alpha(12),
    popup_shadow_color: Color32::from_black_alpha(60),
    search_fill: Color32::from_white_alpha(235), // 近实白（v0.1.5：白底上靠描边分层）
    search_faint: Color32::from_black_alpha(38),
    search_icon: Color32::from_rgb(138, 147, 163), // #8A93A3
    search_text: Color32::from_rgb(23, 26, 33),    // #171A21
    hint: Some(Color32::from_rgb(154, 163, 178)),  // #9AA3B2
    esc_fill: Color32::from_black_alpha(6),
    esc_highlight: Color32::from_white_alpha(120),
    esc_text: Color32::from_rgb(107, 114, 128), // #6B7280
    bar_fill: Color32::from_white_alpha(215), // v0.1.5：白底上提高不透明度
    bar_highlight: Color32::from_white_alpha(200),
    bar_stroke: Color32::from_black_alpha(12),
    chip_text_rgb: (74, 85, 104),      // #4A5568
    chip_sel_text_rgb: (23, 26, 33),   // #171A21
    chip_sel_fill_alpha: 255,
    chip_sel_shadow_alpha: 28,
    chip_sel_highlight_alpha: 0,
    chip_sel_ring: Color32::from_black_alpha(10),
    chip_hover_fill: Color32::from_black_alpha(6),
    row_hover_fill: Color32::from_black_alpha(5),
    row_sel_fill: Color32::from_rgba_unmultiplied(37, 99, 235, 26),
    row_sel_highlight: Color32::from_white_alpha(160),
    tile_gloss_alpha: 46,
    name_text: Color32::from_rgb(23, 26, 33),
    path_text: Color32::from_rgb(122, 132, 148), // #7A8494
    empty_text: Color32::from_rgb(122, 132, 148),
    highlight: Color32::from_rgb(217, 119, 6), // #D97706
    status_text: Color32::from_rgb(122, 132, 148),
    status_hint: Color32::from_rgb(154, 163, 178),
    alert_text: Color32::from_rgb(220, 38, 38), // #DC2626
    dot_ok: Color32::from_rgb(52, 211, 153),    // #34D399，浅色下仍可读
    cat_text_darken: true,
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
/// 关窗行为（设置「常规」二选一）：Tray = 最小化到托盘（默认）；
/// Exit = 退出程序。settings.txt 键 close_action=tray|exit
///（旧 hide_on_close 键作废，load 时按其值迁移一次）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum CloseAction {
    Tray,
    Exit,
}

impl CloseAction {
    fn key_str(self) -> &'static str {
        match self {
            CloseAction::Tray => "tray",
            CloseAction::Exit => "exit",
        }
    }
}

#[derive(Clone)]
struct Settings {
    theme: Theme,
    view: ViewMode,
    close_action: CloseAction,
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
        close_action: match kv.get("close_action").map(|s| s.as_str()) {
            Some("exit") => CloseAction::Exit,
            Some("tray") => CloseAction::Tray,
            // 旧 hide_on_close 键迁移：0 曾表示关窗即退出
            _ if kv.get("hide_on_close").map(|s| s.as_str()) == Some("0") => CloseAction::Exit,
            _ => CloseAction::Tray,
        },
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

/// 启动时把系统 CJK 字体塞进 egui：egui 自带字体没有汉字，不装就是豆腐块。
/// 逐个路径尝试，第一个读到的为准；都找不到就用默认字体（界面仍可用，
/// 汉字显示成缺字框，需要用户自行装 Noto Sans CJK / 微软雅黑）。
fn install_cjk_font(ctx: &egui::Context) {
    let candidates: &[&str] = &[
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
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "cjk".to_owned(),
                egui::FontData::from_owned(bytes),
            );
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                if let Some(list) = fonts.families.get_mut(&family) {
                    list.insert(0, "cjk".to_owned());
                }
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
    eprintln!("ShunSeek: 未找到系统 CJK 字体，汉字可能显示为缺字框");
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
        total: usize,
        empty_query: bool,
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
    /// 最近一次索引就绪文案（含来源摘要）；空查询的搜索完成消息
    /// 会覆盖 status，用它把就绪文案找回来，别让摘要一闪而过。
    ready_status: Option<String>,
    /// 搜索框上一帧是否聚焦（放大镜着色读它，一帧延迟无感）。
    search_focused: bool,
    /// 搜索框上一帧是否悬停（与聚焦一起决定流光描边是流动还是静止）。
    search_hovered: bool,
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
    /// 设置项（与 settings.txt 一一对应，见 Settings）：关窗行为 /
    /// 唤醒热键预设 / 结果上限 / 是否折叠不重要结果。
    close_action: CloseAction,
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
                (s, format!("索引就绪（缓存）· {n} 条"))
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
            ready_status: None,
            search_focused: false,
            search_hovered: false,
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
            close_action: settings.close_action,
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
        };
        app.schedule_search();
        app
    }

    /// 输入变化 → 取消上次 → 30ms 防抖 → 后台线程搜索 → 通道回 UI。
    fn schedule_search(&mut self) {
        self.search_cancel.store(true, Ordering::SeqCst);
        let cancel = Arc::new(AtomicBool::new(false));
        self.search_cancel = cancel.clone();

        let query = self.query.clone();
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
                std::thread::sleep(Duration::from_millis(30));
                if cancel.load(Ordering::SeqCst) {
                    return;
                }
                let started = Instant::now();
                let rows: Vec<ResultRow>;
                let total: usize;
                {
                    let store_g = match store.lock() {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                    let usage_g = match usage.lock() {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                    total = store_g.count();
                    let empty_query = query.trim().is_empty();
                    if empty_query {
                        // 首页 = 常用（.NET 版语义）：按 UsageStore 次数
                        // 降序（同次按最近使用降序）取前 N；从未打开过
                        // 任何条目时回退索引原序前 N（首次运行别空着）。
                        // chips 选了分类时首页按该分类过滤——「全部」
                        // 与首页是同一视图，不另做一套。
                        let snap: HashMap<String, (u32, u64)> = usage_g
                            .snapshot()
                            .into_iter()
                            .map(|(p, c, t)| (p, (c, t)))
                            .collect();
                        let mut picked: Vec<(usize, u32, u64)> = Vec::new();
                        if !snap.is_empty() {
                            for i in 0..total {
                                if cat_set.len() < ALL_CATEGORIES.len()
                                    && !cat_set.contains(&store_g.category(i))
                                {
                                    continue;
                                }
                                if let Some(s) = &scope {
                                    if !s.matches(&store_g.full_path(i)) {
                                        continue;
                                    }
                                }
                                let mut p = store_g.full_path(i);
                                p.make_ascii_lowercase();
                                if let Some(&(cnt, last)) = snap.get(&p) {
                                    picked.push((i, cnt, last));
                                }
                            }
                            picked.sort_by(|a, b| {
                                b.1.cmp(&a.1)
                                    .then_with(|| b.2.cmp(&a.2))
                                    .then_with(|| a.0.cmp(&b.0))
                            });
                            picked.truncate(result_limit);
                        }
                        let idxs: Vec<usize> = if picked.is_empty() {
                            (0..total)
                                .filter(|&i| {
                                    (cat_set.len() >= ALL_CATEGORIES.len()
                                        || cat_set.contains(&store_g.category(i)))
                                        && scope.as_ref().map_or(true, |s| {
                                            s.matches(&store_g.full_path(i))
                                        })
                                })
                                .take(result_limit)
                                .collect()
                        } else {
                            picked.iter().map(|&(i, _, _)| i).collect()
                        };
                        rows = idxs
                            .iter()
                            .map(|&i| ResultRow {
                                name: store_g.name(i).to_string(),
                                path: store_g.full_path(i),
                                category: store_g.category(i),
                                match_start: usize::MAX,
                                match_len: 0,
                                demoted: false,
                            })
                            .collect();
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
                                //（路径模式下不含路径片段），换算成字节数。
                                // match_start 是字节偏移 → 先数它前面的
                                // 字符数，再前进 match_len 个字符取终点。
                                let match_len = if h.match_len == 0 {
                                    0
                                } else {
                                    let start_byte =
                                        (h.match_start as usize).min(name.len());
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
                                    // 不是连续命中段，不高亮以免标错位置
                                    match_start: if h.via_initials || h.fuzzy {
                                        usize::MAX
                                    } else {
                                        h.match_start as usize
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
                    total,
                    empty_query: query.trim().is_empty(),
                });
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
            self.show(ctx);
        }
    }

    fn open_row(&mut self, ctx: &egui::Context, idx: usize, locate: bool) {
        let Some(row) = self.rows.get(idx).cloned() else {
            return;
        };
        opener::open(&row.path, &row.name, locate);
        {
            let mut u = self.usage.lock().unwrap();
            u.record_use(&row.path);
            let _ = u.save();
        }
        self.hide(ctx);
    }

    fn open_selected(&mut self, ctx: &egui::Context, locate: bool) {
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
        if self.display.is_empty() {
            return;
        }
        let next = (self.selected as i32 + delta).clamp(0, self.display.len() as i32 - 1);
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
            "theme={}\nview={}\nclose_action={}\nhotkey={}\nresult_limit={}\nfold_demoted={}\n",
            match self.theme {
                Theme::Dark => "dark",
                Theme::Light => "light",
            },
            match self.view_mode {
                ViewMode::List => "list",
                ViewMode::Grid => "grid",
            },
            self.close_action.key_str(),
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
                    // 宫格模式：↑↓ 按列数跳一行，←→ 按 1 格走；
                    // 列表模式与旧版一致（只有 ↑↓，步长 1）。
                    Key::ArrowDown => {
                        let d = if self.view_mode == ViewMode::Grid {
                            self.grid_cols.max(1) as i32
                        } else {
                            1
                        };
                        self.move_selection(d);
                    }
                    Key::ArrowUp => {
                        let d = if self.view_mode == ViewMode::Grid {
                            self.grid_cols.max(1) as i32
                        } else {
                            1
                        };
                        self.move_selection(-d);
                    }
                    Key::ArrowLeft if self.view_mode == ViewMode::Grid => {
                        self.move_selection(-1)
                    }
                    Key::ArrowRight if self.view_mode == ViewMode::Grid => {
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
        while let Ok(msg) = self.index_rx.try_recv() {
            match msg {
                IndexMsg::Rebuilt {
                    store,
                    count,
                    saved,
                    summary,
                    final_stage,
                } => {
                    if let Ok(mut guard) = self.store.lock() {
                        *guard = store;
                    }
                    self.index_count = count;
                    self.index_summary = summary.clone();
                    if final_stage {
                        self.rebuilding = false;
                        self.tray.set_rebuild_enabled(true);
                        self.status_alert = count == 0;
                        let text = if count == 0 {
                            ALERT_EMPTY_INDEX.to_string()
                        } else {
                            // 摘要过长截断到约 60 字，免得状态栏被撑爆
                            let summary_short = if summary.chars().count() > 60 {
                                let mut s: String = summary.chars().take(59).collect();
                                s.push('…');
                                s
                            } else {
                                summary
                            };
                            let mut s =
                                format!("索引就绪 · {count} 条（来源 {summary_short}）");
                            if !saved {
                                s.push_str(" · 缓存写入失败，不影响本次使用");
                            }
                            s
                        };
                        self.status = text.clone();
                        self.ready_status = Some(text);
                    } else {
                        // 阶段性换入：状态栏报建立进度；搜索已可对
                        // 当前换入的索引用，不等最终一级。
                        self.status = summary.clone();
                        self.ready_status = Some(summary);
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
            total,
            empty_query,
        }) = latest
        {
            self.rows = rows;
            self.fold_active = !empty_query;
            self.rebuild_display();
            self.selected = 0;
            self.status = if self.status_alert {
                // 警戒态下别让搜索文案把红字冲掉
                ALERT_EMPTY_INDEX.to_string()
            } else if empty_query {
                // 空查询回到就绪文案（含来源摘要），没有就用总数文案
                self.ready_status.clone().unwrap_or_else(|| {
                    format!("全部 {total} 条 · 输入关键词开始搜索")
                })
            } else {
                format!("{} 条结果 · {elapsed_ms:.0} ms", self.rows.len())
            };
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
fn paint_vertical_gradient(
    painter: &egui::Painter,
    rect: egui::Rect,
    radius: f32,
    top: Color32,
    bottom: Color32,
) {
    let color_at = |y: f32| {
        let t = ((y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
        Color32::from_rgb(
            lerp_u8(top.r(), bottom.r(), t),
            lerp_u8(top.g(), bottom.g(), t),
            lerp_u8(top.b(), bottom.b(), t),
        )
    };
    // 用一摞水平色带（rect_filled）拼渐变，不用 Mesh：v0.1.3 的 Mesh 顶点色
    // 在部分渲染栈上会被压暗（实测浅色底被压成灰 #ADADB2，而同期 rect_filled
    // 的色块分毫不差），色带路径与普通形状同管线，颜色在哪都准。
    // 每条带按其 y 在圆角轮廓内的横向跨度收边，圆角不缺角也不出界。
    const STRIPS: i32 = 100;
    let strip_h = rect.height() / STRIPS as f32;
    let inset_at = |y: f32| -> f32 {
        // 距上/下边的距离落进圆角区时，按圆方程给出左右内缩量
        let dy = (y - rect.top()).min(rect.bottom() - y);
        if dy >= radius {
            0.0
        } else {
            radius - (radius * radius - (radius - dy) * (radius - dy)).max(0.0).sqrt()
        }
    };
    for i in 0..STRIPS {
        let y0 = rect.top() + strip_h * i as f32;
        let y1 = y0 + strip_h + 0.5; // 轻微重叠，防带间露缝
        let inset = inset_at(y0 + strip_h * 0.5);
        let strip = egui::Rect::from_min_max(
            egui::pos2(rect.left() + inset, y0),
            egui::pos2(rect.right() - inset, y1),
        );
        painter.rect_filled(strip, Rounding::ZERO, color_at(y0 + strip_h * 0.5));
    }
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

/// 圆角矩形的稠密轮廓折线（顺时针，不闭合，首尾靠绘制时回绕相连）。
/// 直边每 ~6px 一个点、圆弧每 7.5° 一个点，供流光描边按弧长重采样。
fn rounded_rect_outline(rect: egui::Rect, radius: f32) -> Vec<egui::Pos2> {
    let mut pts: Vec<egui::Pos2> = Vec::new();
    let r = radius.min(rect.width() / 2.0).min(rect.height() / 2.0);
    let edge_steps = |len: f32| ((len / 6.0).ceil().max(1.0)) as usize;
    let arc = |pts: &mut Vec<egui::Pos2>, cx: f32, cy: f32, a0: f32| {
        let mut a = a0;
        while a < a0 + 90.0 {
            let rad = a.to_radians();
            push_pt(pts, egui::pos2(cx + r * rad.cos(), cy + r * rad.sin()));
            a += 7.5;
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

/// 【LeiBox 光束游走描边】底圈是一整圈淡边（深色 white alpha 26 /
/// 浅色黑 alpha 28，由 faint 参数传入）；
/// beam_phase 有值时，一束约 1/4 周长的光沿周界游走：束头最亮、向尾
/// 按距离渐隐到 0，颜色沿段内位置走青→蓝→紫→粉彩虹带；每小段先画
/// 一层线宽 4、alpha 约 1/3 的同色辉光，再画线宽 1.5 的亮芯。
/// beam_phase=None（未聚焦）时只留淡边，静态不动画。
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
    const SUB: usize = 40;
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

/// 窗口顶部玻璃高光带：顶部 90px 内白色 alpha 自峰值（深色 26 / 浅色 90）
/// 渐隐到 0，且横向偏左上加强（每片横向分 4 段、权重 1.0→0.35 递减，
/// 模拟光从左上打在玻璃面上；玻璃透镜的折射 egui 做不了，只取这点受光感）。
/// 纵向切片叠画，首片带上圆角，贴合面板圆角轮廓。
fn paint_top_glass_highlight(painter: &egui::Painter, rect: egui::Rect, peak: u8) {
    const BAND_H: f32 = 90.0;
    const SLICES: usize = 18;
    const X_WEIGHTS: [f32; 4] = [1.0, 0.8, 0.55, 0.35];
    let inner = rect.shrink(1.0);
    let slice_h = BAND_H / SLICES as f32;
    let seg_w = inner.width() / X_WEIGHTS.len() as f32;
    for i in 0..SLICES {
        let t = i as f32 / SLICES as f32;
        let alpha = peak as f32 * (1.0 - t);
        if alpha < 1.0 {
            continue;
        }
        for (j, w) in X_WEIGHTS.iter().enumerate() {
            let seg = egui::Rect::from_min_max(
                egui::pos2(
                    inner.left() + seg_w * j as f32,
                    inner.top() + slice_h * i as f32,
                ),
                egui::pos2(
                    inner.left() + seg_w * (j + 1) as f32,
                    inner.top() + slice_h * (i + 1) as f32,
                ),
            );
            // 只有首片的最左/最右段需要贴合面板上圆角
            let rounding = if i == 0 && j == 0 {
                Rounding {
                    nw: 11.0,
                    ne: 0.0,
                    sw: 0.0,
                    se: 0.0,
                }
            } else if i == 0 && j == X_WEIGHTS.len() - 1 {
                Rounding {
                    nw: 0.0,
                    ne: 11.0,
                    sw: 0.0,
                    se: 0.0,
                }
            } else {
                Rounding {
                    nw: 0.0,
                    ne: 0.0,
                    sw: 0.0,
                    se: 0.0,
                }
            };
            painter.rect_filled(seg, rounding, Color32::from_white_alpha((alpha * w) as u8));
        }
    }
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
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    if selected {
        painter.rect_filled(
            rect,
            Rounding::same(7.0),
            Color32::from_white_alpha(pal.chip_sel_fill_alpha),
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
    /// 展开/收起一个折叠组（display 下标 + 父目录路径）。
    Toggle(usize, String),
}

impl ShunSeekApp {
    fn draw_search_box(&mut self, ui: &mut egui::Ui) {
        let pal = self.theme.palette();
        // Frame 本身不再描边：边框由 paint_flow_border 手工画——
        // 一圈淡边 + 一束沿周界游走的彩虹光（LeiBox 光束描边）。
        // 聚焦/悬停时光束游走，未聚焦只留静态淡边（不请求重绘，省电）。
        let frame = egui::Frame::none()
            .fill(pal.search_fill)
            .rounding(Rounding::same(10.0))
            .inner_margin(egui::Margin::symmetric(12.0, 10.0));
        let frame_resp = frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                // 左侧放大镜：手绘圆 + 斜线两笔，不用 emoji；
                // 聚焦时换成流光蓝 #3B82F6（两主题同值），未聚焦走 palette。
                let (icon_rect, _) = ui.allocate_exact_size(
                    Vec2::new(22.0, 22.0),
                    egui::Sense::hover(),
                );
                let painter = ui.painter_at(icon_rect);
                let c = icon_rect.center() + Vec2::new(-1.5, -1.5);
                let icon_color = if self.search_focused {
                    Color32::from_rgb(59, 130, 246)
                } else {
                    pal.search_icon
                };
                let icon_stroke = Stroke::new(1.6_f32, icon_color);
                painter.circle_stroke(c, 5.5, icon_stroke);
                painter.line_segment(
                    [
                        c + Vec2::new(4.0, 4.0),
                        c + Vec2::new(8.0, 8.0),
                    ],
                    icon_stroke,
                );
                // hint：浅色带显式色（#9AA3B2）；深色不带色，
                // 让 egui 沿用它自算的 weak 色，与 v0.1.2 逐像素一致。
                let hint_text = match pal.hint {
                    Some(c) => RichText::new("搜索应用、文件、文件夹…").color(c),
                    None => RichText::new("搜索应用、文件、文件夹…"),
                };
                // 输入框宽度必须有界：右侧还要放 ESC 徽标，给 INFINITY 会把
                // 徽标顶出框外被窗口切掉（v0.1.3 预览里徽标只剩「ES」）。
                let edit = egui::TextEdit::singleline(&mut self.query)
                    .desired_width((ui.available_width() - 52.0).max(60.0))
                    .hint_text(hint_text)
                    .text_color(pal.search_text)
                    .frame(false)
                    .font(FontId::proportional(17.0));
                let out = edit.show(ui);
                self.search_focused = out.response.has_focus();
                if self.focus_search {
                    out.response.request_focus();
                    self.focus_search = false;
                }
                if out.response.changed() {
                    self.schedule_search();
                }
                // 右侧 ESC 徽标（不可点，纯提示，与 WPF 版一致）：
                // 玻璃小片——半透填充 + 顶部 1px 高光线，取色与主题切换按钮共用。
                let badge = egui::Frame::none()
                    .fill(pal.esc_fill)
                    .rounding(Rounding::same(4.0))
                    .inner_margin(egui::Margin::symmetric(6.0, 1.0))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("ESC")
                                .size(10.0)
                                .color(pal.esc_text),
                        );
                    });
                let br = badge.response.rect;
                ui.painter().line_segment(
                    [
                        egui::pos2(br.left() + 3.0, br.top() + 0.5),
                        egui::pos2(br.right() - 3.0, br.top() + 0.5),
                    ],
                    Stroke::new(1.0_f32, pal.esc_highlight),
                );
            });
        });
        // 光束描边：画在 Frame 之上（同一层后画者居上）。
        let rect = frame_resp.response.rect;
        let hovered = frame_resp.response.hovered();
        self.search_hovered = hovered;
        let animated = self.search_focused || hovered;
        let ctx = ui.ctx().clone();
        let beam_phase = if animated {
            // 约 7 秒绕一圈，束头位置随时间推进
            Some((ctx.input(|i| i.time) * 0.14) as f32 % 1.0)
        } else {
            None
        };
        paint_flow_border(&ui.painter_at(rect), rect, 10.0, beam_phase, pal.search_faint);
        // 只在窗口可见且处于游走态时才请求重绘驱动动画
        if animated && self.visible {
            ctx.request_repaint();
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
            ui.ctx().request_repaint();
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
                painter.circle_stroke(dot_center, 12.5, Stroke::new(1.6_f32, ACCENT));
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
            painter.circle_filled(badge, 7.0, ACCENT);
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
                painter.circle_stroke(dot_center, 12.5, Stroke::new(1.6_f32, ACCENT));
            }
        }
        // 文件夹图形（填充版小色块，一眼是「范围/目录」）
        let tile = egui::Rect::from_center_size(dot_center, Vec2::splat(16.0));
        paint_folder_tile(&painter, tile, pal);
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
        egui::containers::popup::popup_below_widget(
            ui,
            popup_id,
            &resp,
            egui::containers::popup::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_min_width(230.0);
                ui.label(
                    RichText::new("预设").size(11.5).color(pal.path_text),
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
                ui.add_space(4.0);
                ui.label(
                    RichText::new("磁盘").size(11.5).color(pal.path_text),
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
                ui.add_space(4.0);
                ui.label(
                    RichText::new("自定义文件夹")
                        .size(11.5)
                        .color(pal.path_text),
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
            },
        );
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
        if self.rows.is_empty() {
            let pal = self.theme.palette();
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new(if self.query.trim().is_empty() {
                        "暂无条目"
                    } else {
                        "没有找到结果"
                    })
                    .size(13.0)
                    .color(pal.empty_text),
                );
            });
            return;
        }
        match self.view_mode {
            ViewMode::List => self.draw_results_list(ui),
            ViewMode::Grid => self.draw_results_grid(ui),
        }
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
                self.open_display_row(&ctx, idx);
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
                self.open_display_row(&ctx, idx);
            }
            Some(RowAction::Toggle(idx, dir)) => {
                self.selected = idx;
                self.toggle_group(&dir);
            }
            None => {}
        }
    }

    /// 一个宫格格子：普通项 = 图标 + 名字（≤2 行省略）+ 分类小字；
    /// 组项 = 文件夹色块 + 父目录末段 + 「N 项」+ 展开三角。点击 =
    /// 选中、双击普通项 = 打开、组项单击/双击 = 展开收起（与列表
    /// 的 RowAction 语义完全一致）。
    fn draw_grid_cell(
        &mut self,
        ui: &mut egui::Ui,
        cell: egui::Rect,
        idx: usize,
    ) -> Option<RowAction> {
        let pal = self.theme.palette();
        let item = self.display.get(idx)?.clone();
        let rect = cell.shrink(3.0);
        let response = ui.interact(rect, ui.id().with(("grid_cell", idx)), egui::Sense::click());
        let painter = ui.painter_at(rect);
        let selected = idx == self.selected;
        if selected {
            painter.rect_filled(rect, Rounding::same(10.0), pal.row_sel_fill);
            painter.rect_stroke(rect, Rounding::same(10.0), Stroke::new(1.2_f32, ACCENT));
        } else if response.hovered() {
            painter.rect_filled(rect, Rounding::same(10.0), pal.row_hover_fill);
        }
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(rect.center().x - 20.0, rect.top() + 6.0),
            Vec2::splat(40.0),
        );
        match &item {
            DisplayRow::Single { row, indent } => {
                let r = self.rows.get(*row)?.clone();
                if response.hovered() {
                    self.hover_seen = Some(HoverTarget {
                        name: r.name.clone(),
                        path: r.path.clone(),
                        category: r.category,
                    });
                }
                // 图标：真实图标优先（40×40 绘制），没有就分类色块
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
                    let (tr, tg, tb) = r.category.tile_color();
                    painter.rect_filled(
                        icon_rect,
                        Rounding::same(9.0),
                        Color32::from_rgb(tr, tg, tb),
                    );
                    let gloss = egui::Rect::from_min_size(
                        icon_rect.min,
                        Vec2::new(icon_rect.width(), icon_rect.height() / 2.0),
                    );
                    painter.rect_filled(
                        gloss,
                        Rounding {
                            nw: 9.0,
                            ne: 9.0,
                            sw: 0.0,
                            se: 0.0,
                        },
                        Color32::from_white_alpha(pal.tile_gloss_alpha),
                    );
                    let ch = r
                        .category
                        .display_name()
                        .chars()
                        .next()
                        .unwrap_or('?')
                        .to_string();
                    painter.text(
                        icon_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        ch,
                        FontId::proportional(17.0),
                        Color32::WHITE,
                    );
                }
                if *indent {
                    painter.circle_filled(
                        egui::pos2(rect.left() + 8.0, rect.top() + 8.0),
                        2.5,
                        ACCENT,
                    );
                }
                // 名字：最多 2 行、超出省略、居中
                let mut job = egui::text::LayoutJob::default();
                job.wrap = egui::text::TextWrapping {
                    max_width: 92.0,
                    max_rows: 2,
                    break_anywhere: true,
                    overflow_character: Some('…'),
                };
                job.halign = egui::Align::Center;
                job.append(
                    &r.name,
                    0.0,
                    TextFormat {
                        font_id: FontId::proportional(12.0),
                        color: pal.name_text,
                        ..Default::default()
                    },
                );
                let galley = painter.layout_job(job);
                painter.galley(
                    egui::pos2(
                        rect.center().x - galley.size().x / 2.0,
                        rect.top() + 50.0,
                    ),
                    galley,
                    pal.name_text,
                );
                // 分类小字
                painter.text(
                    egui::pos2(rect.center().x, rect.bottom() - 8.0),
                    egui::Align2::CENTER_CENTER,
                    r.category.display_name(),
                    FontId::proportional(10.5),
                    pal.path_text,
                );
                if response.double_clicked() {
                    Some(RowAction::Open(idx))
                } else if response.clicked() {
                    Some(RowAction::Select(idx))
                } else {
                    None
                }
            }
            DisplayRow::Group {
                dir,
                count,
                expanded,
            } => {
                if response.hovered() {
                    self.hover_seen = Some(HoverTarget {
                        name: dir_last(dir),
                        path: dir.clone(),
                        category: Category::Folder,
                    });
                }
                paint_folder_tile(&painter, icon_rect, pal);
                let mut job = egui::text::LayoutJob::default();
                job.wrap = egui::text::TextWrapping {
                    max_width: 92.0,
                    max_rows: 2,
                    break_anywhere: true,
                    overflow_character: Some('…'),
                };
                job.halign = egui::Align::Center;
                job.append(
                    &dir_last(dir),
                    0.0,
                    TextFormat {
                        font_id: FontId::proportional(12.0),
                        color: pal.name_text,
                        ..Default::default()
                    },
                );
                let galley = painter.layout_job(job);
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

    /// 打开一个展示行：普通行打开对应结果，组行展开/收起（鼠标路径用；
    /// 键盘路径走 open_selected，逻辑一致）。
    fn open_display_row(&mut self, ctx: &egui::Context, idx: usize) {
        match self.display.get(idx).cloned() {
            Some(DisplayRow::Group { dir, .. }) => self.toggle_group(&dir),
            Some(DisplayRow::Single { row, .. }) => self.open_row(ctx, row, false),
            None => {}
        }
    }

    fn draw_display_row(&mut self, ui: &mut egui::Ui, idx: usize) -> Option<RowAction> {
        match self.display.get(idx)?.clone() {
            DisplayRow::Single { row, indent } => self.draw_row(ui, row, idx, indent),
            DisplayRow::Group {
                dir,
                count,
                expanded,
            } => self.draw_group_row(ui, idx, &dir, count, expanded),
        }
    }

    /// 手绘一行：图标位（真图标优先，回退分类色块）+ 名字（命中段
    /// 高亮）/ 路径 / 分类胶囊。row_idx 是原始结果下标，display_idx
    /// 是展示行下标（选中态与 RowAction 都按展示行算）；indent 为
    /// 折叠组展开后的成员行，内容整体右移一截表示从属。
    /// 手绘而不用子控件拼装，是为了 show_rows 虚拟化下每行开销恒定。
    fn draw_row(
        &mut self,
        ui: &mut egui::Ui,
        row_idx: usize,
        display_idx: usize,
        indent: bool,
    ) -> Option<RowAction> {
        let pal = self.theme.palette();
        let row = self.rows.get(row_idx)?.clone();
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), ROW_H),
            egui::Sense::click(),
        );
        let selected = display_idx == self.selected;
        let painter = ui.painter_at(rect);
        if selected {
            painter.rect_filled(
                rect,
                Rounding::same(8.0),
                pal.row_sel_fill,
            );
            // 左侧 3px 强调蓝竖条（内缩，带圆角；#2563EB 两主题同值）
            let bar = egui::Rect::from_min_size(
                rect.min + Vec2::new(1.0, 10.0),
                Vec2::new(3.0, ROW_H - 20.0),
            );
            painter.rect_filled(bar, Rounding::same(2.0), ACCENT);
            // 顶部 1px 高光线（玻璃上缘受光，与玻璃栏/胶囊同一语言）
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 8.0, rect.top() + 0.5),
                    egui::pos2(rect.right() - 8.0, rect.top() + 0.5),
                ],
                Stroke::new(1.0_f32, pal.row_sel_highlight),
            );
        } else if response.hovered() {
            painter.rect_filled(rect, Rounding::same(8.0), pal.row_hover_fill);
        }

        // 图标位 36×36：先查真实系统图标（v0.1.5，Windows 按 key 后台
        // 取、到了自动换上）；没有（未取到 / 非 Windows / 取失败）回退
        // 分类色块 + 首字，与旧版一致。
        let indent_x = if indent { 16.0 } else { 0.0 };
        let (r, g, b) = row.category.tile_color();
        let tile_rect = egui::Rect::from_min_size(
            rect.min + Vec2::new(6.0 + indent_x, (ROW_H - 36.0) / 2.0),
            Vec2::splat(36.0),
        );
        let icon_key = icons::key_for(&row.name, &row.path, row.category);
        let icon_tex = self.icons.texture(ui.ctx(), &icon_key);
        if let Some(tex) = icon_tex {
            let icon_rect = egui::Rect::from_min_size(
                tile_rect.min + Vec2::splat((36.0 - icons::ICON_SIZE) / 2.0),
                Vec2::splat(icons::ICON_SIZE),
            );
            painter.image(
                tex.id(),
                icon_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            painter.rect_filled(tile_rect, Rounding::same(9.0), Color32::from_rgb(r, g, b));
            // 色块玻璃感：上半部叠一层高光（上圆角、下直角），像光打在玻璃面上
            let tile_gloss = egui::Rect::from_min_size(
                tile_rect.min,
                Vec2::new(tile_rect.width(), tile_rect.height() / 2.0),
            );
            painter.rect_filled(
                tile_gloss,
                Rounding {
                    nw: 9.0,
                    ne: 9.0,
                    sw: 0.0,
                    se: 0.0,
                },
                Color32::from_white_alpha(pal.tile_gloss_alpha),
            );
            let tile_char = row
                .category
                .display_name()
                .chars()
                .next()
                .unwrap_or('?')
                .to_string();
            painter.text(
                tile_rect.center(),
                Align2::CENTER_CENTER,
                tile_char,
                FontId::proportional(15.0),
                Color32::WHITE,
            );
        }

        // 右侧分类胶囊：分类色低透明底（alpha 40）+ 分类色调出的文字
        // （深色提亮 / 浅色压暗，见 Palette::cat_text_color），先量宽，
        // 名字区域要给它让位
        let cat_name = row.category.display_name();
        let cat_font = FontId::proportional(10.5);
        let cat_text_color = pal.cat_text_color(r, g, b);
        let cat_galley = painter.layout_no_wrap(
            cat_name.to_string(),
            cat_font.clone(),
            cat_text_color,
        );
        let capsule_w = cat_galley.size().x + 14.0;
        let capsule_rect = egui::Rect::from_min_size(
            egui::pos2(
                rect.right() - capsule_w - 6.0,
                rect.center().y - 9.0,
            ),
            Vec2::new(capsule_w, 18.0),
        );
        painter.rect_filled(
            capsule_rect,
            Rounding::same(8.0),
            Color32::from_rgba_unmultiplied(r, g, b, 40),
        );
        painter.galley(
            egui::pos2(
                capsule_rect.center().x - cat_galley.size().x / 2.0,
                capsule_rect.center().y - cat_galley.size().y / 2.0,
            ),
            cat_galley,
            cat_text_color,
        );

        // 名字（命中段高亮）+ 路径，横向裁剪不换行
        let text_x = rect.left() + 50.0 + indent_x;
        let text_clip = egui::Rect::from_min_max(
            egui::pos2(text_x, rect.top()),
            egui::pos2(capsule_rect.left() - 8.0, rect.bottom()),
        );
        let painter = painter.with_clip_rect(text_clip);
        let name_font = FontId::proportional(14.0);
        let mut job = egui::text::LayoutJob::default();
        let name = &row.name;
        let hl = if row.match_start != usize::MAX
            && row.match_start < name.len()
            && row.match_len > 0
        {
            let start = row.match_start;
            let end = (start + row.match_len).min(name.len());
            // 保证落在字符边界上（match_start 是字节偏移，查询长度按字节算）
            if name.is_char_boundary(start) && name.is_char_boundary(end) {
                Some((start, end))
            } else {
                None
            }
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
                        font_id: FontId::proportional(14.0),
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
        painter.galley(egui::pos2(text_x, rect.top() + 7.0), galley, Color32::WHITE);

        let path_galley = painter.layout_no_wrap(
            row.path.clone(),
            FontId::proportional(11.5),
            pal.path_text,
        );
        painter.galley(
            egui::pos2(text_x, rect.top() + 30.0),
            path_galley,
            pal.path_text,
        );

        if response.hovered() {
            self.hover_seen = Some(HoverTarget {
                name: row.name.clone(),
                path: row.path.clone(),
                category: row.category,
            });
        }
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.double_clicked() {
            Some(RowAction::Open(display_idx))
        } else if response.clicked() {
            Some(RowAction::Select(display_idx))
        } else {
            None
        }
    }

    /// 折叠组行：文件夹色块（手绘文件夹图形）+ 父目录末两段 +
    /// 「N 个同目录结果已折叠 / 已展开」+ 右侧展开箭头（三角手绘）。
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
        if selected {
            painter.rect_filled(rect, Rounding::same(8.0), pal.row_sel_fill);
            let bar = egui::Rect::from_min_size(
                rect.min + Vec2::new(1.0, 10.0),
                Vec2::new(3.0, ROW_H - 20.0),
            );
            painter.rect_filled(bar, Rounding::same(2.0), ACCENT);
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 8.0, rect.top() + 0.5),
                    egui::pos2(rect.right() - 8.0, rect.top() + 0.5),
                ],
                Stroke::new(1.0_f32, pal.row_sel_highlight),
            );
        } else if response.hovered() {
            painter.rect_filled(rect, Rounding::same(8.0), pal.row_hover_fill);
        }

        // 文件夹色块（与宫格组格 / 范围圆点共用同一画法）
        let tile_rect = egui::Rect::from_min_size(
            rect.min + Vec2::new(6.0, (ROW_H - 36.0) / 2.0),
            Vec2::splat(36.0),
        );
        paint_folder_tile(&painter, tile_rect, pal);

        // 主文字：父目录末两段；副文字：折叠计数（裁剪到箭头之前）
        let text_x = rect.left() + 50.0;
        let text_clip = egui::Rect::from_min_max(
            egui::pos2(text_x, rect.top()),
            egui::pos2(rect.right() - 34.0, rect.bottom()),
        );
        let painter = painter.with_clip_rect(text_clip);
        let title_galley = painter.layout_no_wrap(
            dir_tail(dir),
            FontId::proportional(14.0),
            pal.name_text,
        );
        painter.galley(
            egui::pos2(text_x, rect.top() + 7.0),
            title_galley,
            pal.name_text,
        );
        let sub = if expanded {
            format!("{count} 个同目录结果 · 已展开")
        } else {
            format!("{count} 个同目录结果已折叠")
        };
        let sub_galley = painter.layout_no_wrap(
            sub,
            FontId::proportional(11.5),
            pal.path_text,
        );
        painter.galley(
            egui::pos2(text_x, rect.top() + 30.0),
            sub_galley,
            pal.path_text,
        );

        // 右侧展开箭头：收起时向右（▸ 意象），展开后向下（▾ 意象），手绘三角
        let c = egui::pos2(rect.right() - 18.0, rect.center().y);
        let tri = if expanded {
            vec![
                egui::pos2(c.x - 5.0, c.y - 3.0),
                egui::pos2(c.x + 5.0, c.y - 3.0),
                egui::pos2(c.x, c.y + 4.0),
            ]
        } else {
            vec![
                egui::pos2(c.x - 3.0, c.y - 5.0),
                egui::pos2(c.x - 3.0, c.y + 5.0),
                egui::pos2(c.x + 4.0, c.y),
            ]
        };
        ui.painter_at(rect).add(egui::Shape::convex_polygon(
            tri,
            pal.path_text,
            Stroke::NONE,
        ));

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

    fn draw_status_bar(&mut self, ui: &mut egui::Ui) {
        let pal = self.theme.palette();
        ui.horizontal(|ui| {
            // 左侧状态圆点：就绪绿 / 警戒红（空索引护栏）
            let (dot_rect, _) =
                ui.allocate_exact_size(Vec2::splat(12.0), egui::Sense::hover());
            let dot_color = if self.status_alert {
                pal.alert_text
            } else {
                pal.dot_ok
            };
            ui.painter_at(dot_rect)
                .circle_filled(dot_rect.center(), 3.0, dot_color);
            let text_color = if self.status_alert {
                pal.alert_text
            } else {
                pal.status_text
            };
            ui.label(RichText::new(&self.status).size(11.0).color(text_color));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new("↑↓ 选择 · Enter 打开 · Ctrl+Enter 打开所在文件夹")
                        .size(11.0)
                        .color(pal.status_hint),
                );
                // 主题切换（主界面快捷切换，保留）：图标按「目标主题」
                // 画（去浅色画太阳、去深色画月亮）+ 目标名文字，玻璃
                // 小片与 ESC 徽标同款；切换走 set_theme（同步 settings.txt）。
                ui.add_space(10.0);
                if theme_icon_button(ui, pal, self.theme).clicked() {
                    let next = self.theme.toggled();
                    self.set_theme(ui.ctx(), next);
                }
                // 设置按钮（齿轮图形）：打开独立设置窗口
                ui.add_space(6.0);
                if gear_button(ui, pal).clicked() {
                    self.open_settings();
                }
            });
        });
    }
}

/// ------------------------------------------------------------ 手绘图标
/// 全局 UI 规范：图标图案必须一眼能认出功能，不许用汉字充当图标。
/// 全部用 egui painter 手绘矢量图形，线宽 ≥1.8px、尺寸 ≥14px；
/// 纯图标控件另配 on_hover_text 兜底说明。

/// 漏斗（类型筛选）：上宽下窄 + 下方小竖管，闭合描线。
fn paint_funnel(painter: &egui::Painter, c: egui::Pos2, size: f32, color: Color32) {
    let s = size / 2.0;
    let pts = [
        egui::pos2(c.x - s, c.y - s * 0.8),
        egui::pos2(c.x + s, c.y - s * 0.8),
        egui::pos2(c.x + s * 0.3, c.y + s * 0.05),
        egui::pos2(c.x + s * 0.3, c.y + s * 0.9),
        egui::pos2(c.x - s * 0.3, c.y + s * 0.9),
        egui::pos2(c.x - s * 0.3, c.y + s * 0.05),
        egui::pos2(c.x - s, c.y - s * 0.8),
    ];
    painter.add(egui::Shape::line(pts.to_vec(), Stroke::new(1.8_f32, color)));
}

/// 文件夹色块（Folder 分类色 + 玻璃高光 + 白色文件夹图形）：
/// 组行 / 宫格组格 / 范围圆点共用，按 tile 尺寸等比缩放
///（36px 时的坐标与旧组行手绘逐值一致）。
fn paint_folder_tile(painter: &egui::Painter, tile: egui::Rect, pal: Palette) {
    let (r, g, b) = Category::Folder.tile_color();
    let round = (tile.width() / 4.0).min(9.0);
    painter.rect_filled(tile, Rounding::same(round), Color32::from_rgb(r, g, b));
    let gloss = egui::Rect::from_min_size(
        tile.min,
        Vec2::new(tile.width(), tile.height() / 2.0),
    );
    painter.rect_filled(
        gloss,
        Rounding {
            nw: round,
            ne: round,
            sw: 0.0,
            se: 0.0,
        },
        Color32::from_white_alpha(pal.tile_gloss_alpha),
    );
    let glyph = Color32::from_white_alpha(235);
    let k = tile.width() / 36.0;
    let back = egui::Rect::from_min_max(
        tile.min + Vec2::new(7.0 * k, 11.0 * k),
        tile.min + Vec2::new(29.0 * k, 26.0 * k),
    );
    painter.rect_filled(back, Rounding::same(2.0), glyph);
    let tab = egui::Rect::from_min_max(
        tile.min + Vec2::new(7.0 * k, 8.0 * k),
        tile.min + Vec2::new(16.0 * k, 12.0 * k),
    );
    painter.rect_filled(tab, Rounding::same(1.0), glyph);
}

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

/// 玻璃小片骨架（与 ESC 徽标同款：填充 + 顶部 1px 高光线）。
fn glass_pill(painter: &egui::Painter, rect: egui::Rect, pal: Palette) {
    painter.rect_filled(rect, Rounding::same(4.0), pal.esc_fill);
    painter.line_segment(
        [
            egui::pos2(rect.left() + 3.0, rect.top() + 0.5),
            egui::pos2(rect.right() - 3.0, rect.top() + 0.5),
        ],
        Stroke::new(1.0_f32, pal.esc_highlight),
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

/// 状态栏主题按钮：图标按目标主题画（将去浅色 → 太阳、将去
/// 深色 → 月亮）+ 目标名文字，与旧版纯文字按钮同位置同尺寸感。
fn theme_icon_button(ui: &mut egui::Ui, pal: Palette, theme: Theme) -> egui::Response {
    let (label, to_light) = match theme {
        Theme::Dark => ("浅色模式", true),
        Theme::Light => ("深色模式", false),
    };
    let font = FontId::proportional(11.0);
    let galley = ui.fonts(|f| f.layout_no_wrap(label.to_string(), font, pal.esc_text));
    let size = Vec2::new(16.0 + 5.0 + galley.size().x + 12.0, galley.size().y + 4.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter_at(rect);
    glass_pill(&painter, rect, pal);
    let c = egui::pos2(rect.left() + 6.0 + 8.0, rect.center().y);
    if to_light {
        paint_sun(&painter, c, 14.0, pal.esc_text);
    } else {
        paint_moon(&painter, c, 14.0, pal.esc_text, pal.panel_fill);
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
        .on_hover_text(label)
}

/// 状态栏设置按钮：纯齿轮图形 + 悬浮文字兜底。
fn gear_button(ui: &mut egui::Ui, pal: Palette) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(22.0, 19.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    glass_pill(&painter, rect, pal);
    paint_gear(&painter, rect.center(), 14.0, pal.esc_text);
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("设置")
}

fn glass_chip(ui: &mut egui::Ui, pal: Palette, label: &str, selected: bool) -> egui::Response {
    let font = FontId::proportional(12.5);
    // 选中进度 v：0=未选中，1=完全选中，切换时平滑过渡
    let v = ui
        .ctx()
        .animate_bool_with_time(ui.make_persistent_id(("glass_chip_sel", label)), selected, 0.18);
    let (tr, tg, tb) = pal.chip_text_rgb;
    let (sr, sg, sb) = pal.chip_sel_text_rgb;
    let text_color = Color32::from_rgb(
        lerp_u8(tr, sr, v),
        lerp_u8(tg, sg, v),
        lerp_u8(tb, sb, v),
    );
    let galley = ui
        .fonts(|f| f.layout_no_wrap(label.to_string(), font.clone(), text_color));
    let size = Vec2::new(galley.size().x + 24.0, 26.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter_at(rect);
    let rounding = Rounding::same(13.0);
    if v > 0.01 {
        // 柔和投影（向下 1px 的黑影，随选中进度淡入）
        painter.rect_filled(
            rect.translate(Vec2::new(0.0, 1.0)),
            rounding,
            Color32::from_black_alpha((pal.chip_sel_shadow_alpha as f32 * v) as u8),
        );
        // 磨砂白填充
        painter.rect_filled(
            rect,
            rounding,
            Color32::from_white_alpha((pal.chip_sel_fill_alpha as f32 * v) as u8),
        );
        // 描边圈（只有浅色，深色 TRANSPARENT 跳过）
        if pal.chip_sel_ring.a() > 0 {
            painter.rect_stroke(
                rect,
                rounding,
                Stroke::new(1.0_f32, pal.chip_sel_ring.gamma_multiply(v)),
            );
        }
        // 顶部高光线（浅色 alpha 为 0，自动不画）
        if pal.chip_sel_highlight_alpha > 0 {
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 10.0, rect.top() + 0.5),
                    egui::pos2(rect.right() - 10.0, rect.top() + 0.5),
                ],
                Stroke::new(
                    1.0_f32,
                    Color32::from_white_alpha((pal.chip_sel_highlight_alpha as f32 * v) as u8),
                ),
            );
        }
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
        // 关闭窗口后：二选一分段控件（胶囊选中样式，与筛选控件同语言）
        ui.label(RichText::new("关闭窗口后").color(pal.name_text));
        self.close_action_card(
            ui,
            pal,
            CloseAction::Tray,
            "最小化到托盘",
            "程序继续在托盘运行，热键可随时唤出；从托盘菜单退出才真正结束。",
        );
        self.close_action_card(
            ui,
            pal,
            CloseAction::Exit,
            "退出程序",
            "关闭窗口即完全退出，托盘图标一并消失，下次使用需重新启动。",
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
                        let mut u = self.usage.lock().unwrap();
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

    /// 「关闭窗口后」的一个选项卡：标题 + 小字描述；选中 = 胶囊
    /// 填充（与筛选控件同一选中语言），不用勾选框。
    fn close_action_card(
        &mut self,
        ui: &mut egui::Ui,
        pal: Palette,
        action: CloseAction,
        title: &str,
        desc: &str,
    ) {
        let selected = self.close_action == action;
        let (rect, response) = ui
            .allocate_exact_size(Vec2::new(ui.available_width(), 46.0), egui::Sense::click());
        let painter = ui.painter_at(rect);
        if selected {
            painter.rect_filled(
                rect,
                Rounding::same(9.0),
                Color32::from_white_alpha(pal.chip_sel_fill_alpha),
            );
            if pal.chip_sel_ring.a() > 0 {
                painter.rect_stroke(
                    rect,
                    Rounding::same(9.0),
                    Stroke::new(1.0_f32, pal.chip_sel_ring),
                );
            }
        } else if response.hovered() {
            painter.rect_filled(rect, Rounding::same(9.0), pal.chip_hover_fill);
        }
        let (tr, tg, tb) = if selected {
            pal.chip_sel_text_rgb
        } else {
            pal.chip_text_rgb
        };
        let title_color = Color32::from_rgb(tr, tg, tb);
        let title_g = painter.layout_no_wrap(
            title.to_string(),
            FontId::proportional(12.5),
            title_color,
        );
        painter.galley(rect.min + Vec2::new(10.0, 6.0), title_g, title_color);
        let desc_color = if selected { title_color } else { pal.path_text };
        let desc_g = painter.layout_no_wrap(
            desc.to_string(),
            FontId::proportional(10.5),
            desc_color,
        );
        let desc_clip = egui::Rect::from_min_max(
            rect.min + Vec2::new(10.0, 24.0),
            rect.max - Vec2::new(8.0, 2.0),
        );
        painter
            .with_clip_rect(desc_clip)
            .galley(rect.min + Vec2::new(10.0, 26.0), desc_g, desc_color);
        if response.clicked() && !selected {
            self.close_action = action;
            self.save_settings();
        }
        ui.add_space(4.0);
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
                }
            }
        }
        // 关窗行为走设置 close_action：tray = 隐藏到托盘（退出走
        // 托盘菜单）；exit = 关窗即真退出（同样先撤托盘图标）。
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_exit {
            if self.close_action == CloseAction::Tray {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.hide(ctx);
            } else {
                self.tray.shutdown();
                self.allow_exit = true;
            }
        }

        self.poll_channels(ctx);
        if self.visible {
            self.handle_keys(ctx);
        }

        // 悬浮窗背景：手工竖向渐变（深色 #252A35→#16181D / 浅色
        // #FFFFFF→#F6F7F9，v0.1.5 去灰）画在背景层，面板本身透明，
        // 靠 Frame 的投影 + 淡描边撑出 Fluent 悬浮感。取色全走 palette。
        let pal = self.theme.palette();
        let bg_painter = ctx.layer_painter(egui::LayerId::background());
        let screen = ctx.screen_rect();
        paint_vertical_gradient(
            &bg_painter,
            screen,
            12.0,
            pal.grad_top,
            pal.grad_bottom,
        );
        // 玻璃/冰霜质感：顶部 90px 高光带（峰值 alpha 走 palette）+ 内层描边，
        // 与面板 Frame 的外层描边组成双层玻璃边缘。
        paint_top_glass_highlight(&bg_painter, screen, pal.top_highlight_peak);
        bg_painter.rect_stroke(
            screen.shrink(1.0),
            Rounding::same(11.0),
            Stroke::new(1.0_f32, pal.panel_inner_stroke),
        );
        // 注意：这里绝不能给 Frame 加 shadow。面板填充是透明的，egui 的
        // 阴影画在面板形状之后、内容之前，会整片透过透明面板压在渐变上
        // ——浅色下就是一层均匀的灰幕（v0.1.3 浅色「太灰」的真凶，深色下
        // 黑压黑看不出来）。本窗口与面板同尺寸，阴影本就没有可落的余地，
        // 边缘靠双层描边定义。
        let panel_frame = egui::Frame::central_panel(&ctx.style())
            .fill(Color32::TRANSPARENT)
            .rounding(Rounding::same(12.0))
            .stroke(Stroke::new(1.0_f32, pal.panel_stroke))
            .inner_margin(egui::Margin::symmetric(14.0, 10.0));
        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                // 宽度护栏：内容宽 = 屏宽 - 左右边距（各 14）。某些环境下
                // （虚拟屏 DPI 不一致等）面板给到的可用宽会多出一截，右侧
                // 内容被窗口边缘切掉（v0.1.3 预览里分类胶囊/状态栏文字
                // 被切就是这个），这里按屏宽显式收住，正常环境下是空操作。
                let content_w = (ctx.screen_rect().width() - 28.0).max(200.0);
                ui.set_max_width(content_w);
                self.draw_search_box(ui);
                ui.add_space(8.0);
                self.draw_toolbar(ui);
                ui.add_space(6.0);
                let status_h = 22.0;
                let results_h =
                    (ui.available_height() - status_h - 6.0).max(60.0);
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
/// 再套本应用的参数——控件圆角统一 8、选中态强调蓝半透明、弹窗走玻璃
/// （填充/描边/投影取 palette）。启动时按 theme.txt 调一次，切换主题时
/// 由状态栏按钮再调。注意深色下 window_rounding 的有效值是 8：
/// v0.1.2 的 install_visuals 先写 10 又被后面的统一圆角覆盖回 8，
/// 这里保留这个实际效果，不动深色观感；浅色按新规用 10。
fn install_visuals(ctx: &egui::Context, theme: Theme) {
    let pal = theme.palette();
    let mut visuals = match theme {
        Theme::Dark => egui::Visuals::dark(),
        Theme::Light => egui::Visuals::light(),
    };
    visuals.panel_fill = pal.panel_fill;
    // 弹窗（「更多」菜单等）走玻璃：微透填充 + 淡描边 + 柔和投影。
    visuals.window_fill = pal.popup_fill;
    visuals.window_stroke = Stroke::new(1.0_f32, pal.popup_stroke);
    visuals.popup_shadow = egui::epaint::Shadow {
        offset: Vec2::new(0.0, 4.0),
        blur: 16.0,
        spread: 0.0,
        color: pal.popup_shadow_color,
    };
    visuals.extreme_bg_color = pal.extreme_bg;
    visuals.selection.bg_fill = Color32::from_rgba_unmultiplied(37, 99, 235, 120);
    visuals.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
    let r = Rounding::same(8.0);
    visuals.widgets.noninteractive.rounding = r;
    visuals.widgets.inactive.rounding = r;
    visuals.widgets.hovered.rounding = r;
    visuals.widgets.active.rounding = r;
    visuals.widgets.open.rounding = r;
    visuals.window_rounding = match theme {
        Theme::Dark => Rounding::same(8.0),
        Theme::Light => Rounding::same(10.0),
    };
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
        .with_inner_size([700.0, 500.0])
        .with_min_inner_size([700.0, 500.0])
        .with_resizable(false)
        .with_decorations(false)
        .with_transparent(true)
        .with_window_level(egui::WindowLevel::AlwaysOnTop)
        .with_taskbar(false); // 不进任务栏（0.29 里是 with_taskbar(是否显示)）
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
