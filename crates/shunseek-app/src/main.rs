//! 瞬觅 ShunSeek 界面（Rust 版，egui 重绘）。
//! 对照原 WPF 版逐项搬：MainWindow.xaml 的四段布局、MainViewModel 的
//! 搜索调度（取消 + 30ms 防抖 + 后台线程）、App.xaml.cs 的启动装配
//! （单实例除外，见下）、HotkeyManager 的 Alt+Space、TrayIconManager 的托盘。
//!
//! 与 WPF 版的差异（如实记录）：
//!  - 背景：真 Mica 需要 DWM 的 DWMWA_SYSTEMBACKDROP_TYPE，egui 不直接
//!    支持；这里用深色纯色 + 圆角 + 细描边近似（见 CentralPanel 的 Frame）。
//!  - 单实例：C# 版用命名 Mutex；Rust 版 TODO（需要 Windows 命名互斥 FFI
//!    或单实例 socket），当前不做，重复启动会开两个窗口。
//!  - 全局热键：轮询实现，见 hotkey.rs；非 Windows 无全局热键。

#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod demo;
mod hotkey;
mod opener;
mod tray;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{
    Align, Align2, Color32, FontId, Key, Layout, RichText, Rounding, Stroke,
    TextFormat, Vec2, ViewportCommand,
};
use shunseek_core::{build_index, Category, IndexStore, RawEntry, SearchEngine, UsageStore};

const RESULT_LIMIT: usize = 100;
const ROW_H: f32 = 46.0;

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
    /// 后台重建完成：新索引已存到 path（index-next.bin），条数 count。
    Rebuilt { count: usize, path: PathBuf },
}

/// 平台数据源汇总（在后台线程跑）：
/// Windows：存在的固定盘逐个 MFT + appscan（快捷方式 + 已装应用）；
/// MFT 全失败或一条都没有时，用 walker 遍历 %USERPROFILE% 兜底。
/// 非 Windows：DemoIndexSource 的演示数据。
fn collect_entries() -> (Vec<RawEntry>, Option<HashMap<char, String>>) {
    #[cfg(windows)]
    {
        use shunseek_core::sources::{appscan, mft, walker};
        let mut entries: Vec<RawEntry> = Vec::new();
        let mut roots: HashMap<char, String> = HashMap::new();
        let mut mft_ok = false;
        for drive in 'C'..='Z' {
            let root = format!("{drive}:\\");
            if !std::path::Path::new(&root).exists() {
                continue;
            }
            roots.insert(drive, root);
            if let Ok(mut v) = mft::load_volume(drive) {
                if !v.is_empty() {
                    mft_ok = true;
                }
                entries.append(&mut v);
            }
        }
        entries.extend(appscan::load_all());
        if !mft_ok || entries.is_empty() {
            if let Ok(profile) = std::env::var("USERPROFILE") {
                entries.extend(walker::walk(&[PathBuf::from(profile)], true));
            }
        }
        let roots = if roots.is_empty() { None } else { Some(roots) };
        (entries, roots)
    }
    #[cfg(not(windows))]
    {
        (demo::demo_entries(), None)
    }
}

fn spawn_rebuild(dir: PathBuf, tx: Sender<IndexMsg>) {
    std::thread::Builder::new()
        .name("shunseek-index".into())
        .spawn(move || {
            let (entries, roots) = collect_entries();
            let store = build_index(entries, roots.as_ref());
            let count = store.count();
            // 先存到 index-next.bin：当前索引可能正被 mmap 打开（Windows 下
            // 覆盖映射中的文件会失败），换名替换的动作留给 UI 线程在
            // 卸下旧 store 之后做。
            let next = dir.join("index-next.bin");
            if store.save_to(&next).is_ok() {
                let _ = tx.send(IndexMsg::Rebuilt { count, path: next });
            } else {
                let _ = tx.send(IndexMsg::Rebuilt {
                    count,
                    path: PathBuf::new(),
                });
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
    category: Option<Category>,
    rows: Vec<ResultRow>,
    selected: usize,
    status: String,
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
    dir: PathBuf,
}

impl ShunSeekApp {
    fn new(cc: &eframe::CreationContext<'_>) -> ShunSeekApp {
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);

        let usage = UsageStore::load(Some(dir.join("usage.dat")));

        // 缓存索引秒开：有 index.bin 先 mmap 顶上，后台重建完再换新的。
        let index_path = dir.join("index.bin");
        let (store, status) = match IndexStore::load_mmap(&index_path) {
            Ok(s) => {
                let n = s.count();
                (s, format!("索引就绪（缓存）· {n} 条"))
            }
            Err(_) => (build_index(Vec::new(), None), "正在准备索引…".to_string()),
        };

        let (search_tx, search_rx) = std::sync::mpsc::channel();
        let (index_tx, index_rx) = std::sync::mpsc::channel();
        spawn_rebuild(dir.clone(), index_tx);

        let mut app = ShunSeekApp {
            query: String::new(),
            category: None,
            rows: Vec::new(),
            selected: 0,
            status,
            store: Arc::new(Mutex::new(store)),
            usage: Arc::new(Mutex::new(usage)),
            search_cancel: Arc::new(AtomicBool::new(true)),
            search_tx,
            search_rx,
            index_rx,
            hotkey: hotkey::Hotkey::start(cc.egui_ctx.clone()),
            tray: tray::Tray::new(cc.egui_ctx.clone()),
            visible: true,
            allow_exit: false,
            focus_search: true,
            scroll_to_selected: false,
            dir,
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
        let category = self.category;
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
                        // 空查询展示全部前 N 条（同 C# 版；真实大索引时
                        // 这里应换成最近使用列表，阶段 4 的事）
                        let n = total.min(RESULT_LIMIT);
                        rows = (0..n)
                            .map(|i| ResultRow {
                                name: store_g.name(i).to_string(),
                                path: store_g.full_path(i),
                                category: store_g.category(i),
                                match_start: usize::MAX,
                                match_len: 0,
                            })
                            .collect();
                    } else {
                        let engine = SearchEngine::new(&store_g, Some(&usage_g));
                        let hits = engine.search(&query, category, RESULT_LIMIT, &cancel);
                        if cancel.load(Ordering::SeqCst) {
                            return;
                        }
                        let qlen = query.trim().len();
                        rows = hits
                            .iter()
                            .map(|h| {
                                let i = h.index as usize;
                                ResultRow {
                                    name: store_g.name(i).to_string(),
                                    path: store_g.full_path(i),
                                    category: store_g.category(i),
                                    // 拼音首字母命中的 match_start 指向首字母串，
                                    // 不是名字本身，不高亮以免标错位置
                                    match_start: if h.via_initials {
                                        usize::MAX
                                    } else {
                                        h.match_start as usize
                                    },
                                    match_len: qlen,
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
            self.schedule_search();
        }
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
        // 与 C# 版一致：没选中时退回第一条
        let idx = if self.rows.get(self.selected).is_some() {
            self.selected
        } else {
            0
        };
        if !self.rows.is_empty() {
            self.open_row(ctx, idx, locate);
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.rows.is_empty() {
            return;
        }
        let next = (self.selected as i32 + delta).clamp(0, self.rows.len() as i32 - 1);
        self.selected = next as usize;
        self.scroll_to_selected = true;
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
                    Key::Escape => self.hide(ctx),
                    Key::ArrowDown => self.move_selection(1),
                    Key::ArrowUp => self.move_selection(-1),
                    Key::Enter => self.open_selected(ctx, modifiers.ctrl),
                    _ => {}
                }
            }
        }
    }

    fn poll_channels(&mut self, ctx: &egui::Context) {
        // 索引重建完成：卸下旧 store → 换名 → mmap 新索引
        while let Ok(msg) = self.index_rx.try_recv() {
            match msg {
                IndexMsg::Rebuilt { count, path } => {
                    let mut replaced = false;
                    if !path.as_os_str().is_empty() {
                        let target = self.dir.join("index.bin");
                        if let Ok(mut guard) = self.store.lock() {
                            *guard = build_index(Vec::new(), None); // 先卸旧 mmap
                            let load_path = if std::fs::rename(&path, &target).is_ok() {
                                target.clone()
                            } else {
                                path.clone()
                            };
                            if let Ok(new_store) = IndexStore::load_mmap(&load_path) {
                                *guard = new_store;
                                replaced = true;
                            }
                        }
                    }
                    self.status = if replaced {
                        format!("索引就绪 · {count} 条 · Alt+Space 呼出")
                    } else {
                        format!("索引就绪 · {count} 条（缓存更新失败，沿用旧索引）")
                    };
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
            self.selected = 0;
            self.status = if empty_query {
                format!("全部 {total} 条 · 输入关键词开始搜索")
            } else {
                format!("{} 条结果 · {elapsed_ms:.0} ms", self.rows.len())
            };
        }
        let _ = ctx;
    }
}

// ---------------------------------------------------------------- UI

/// 分类筛选条：前 9 个直接摆出，其余收进「更多」菜单（原版是占位按钮，
/// 这里做成真功能）。顺序与 Category / WPF 版一致。
const CHIP_CATS: &[Option<Category>] = &[
    None,
    Some(Category::App),
    Some(Category::Shortcut),
    Some(Category::InstallDir),
    Some(Category::Folder),
    Some(Category::Document),
    Some(Category::Image),
    Some(Category::Video),
    Some(Category::Audio),
];
const MORE_CATS: &[Category] = &[
    Category::Archive,
    Category::Code,
    Category::Executable,
    Category::Other,
];

fn chip_label(cat: Option<Category>) -> String {
    match cat {
        None => "全部".to_string(),
        Some(c) => c.display_name().to_string(),
    }
}

enum RowAction {
    Select(usize),
    Open(usize),
}

impl ShunSeekApp {
    fn draw_search_box(&mut self, ui: &mut egui::Ui) {
        let frame = egui::Frame::none()
            .fill(Color32::from_white_alpha(16))
            .stroke(Stroke::new(1.0_f32, Color32::from_white_alpha(30)))
            .rounding(Rounding::same(8.0))
            .inner_margin(egui::Margin::symmetric(10.0, 7.0));
        frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                let edit = egui::TextEdit::singleline(&mut self.query)
                    .desired_width(f32::INFINITY)
                    .hint_text("搜索应用、文件、文件夹…")
                    .frame(false)
                    .font(FontId::proportional(16.0));
                let out = edit.show(ui);
                if self.focus_search {
                    out.response.request_focus();
                    self.focus_search = false;
                }
                if out.response.changed() {
                    self.schedule_search();
                }
                // 右侧 ESC 徽标（不可点，纯提示，与 WPF 版一致）
                egui::Frame::none()
                    .fill(Color32::from_white_alpha(20))
                    .rounding(Rounding::same(4.0))
                    .inner_margin(egui::Margin::symmetric(6.0, 1.0))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("ESC")
                                .size(10.0)
                                .color(Color32::from_rgb(168, 168, 168)),
                        );
                    });
            });
        });
    }

    fn draw_chips(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::horizontal()
            .id_salt("chips")
            .auto_shrink([false, true])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for &cat in CHIP_CATS {
                        let selected = self.category == cat;
                        if chip_button(ui, &chip_label(cat), selected).clicked() {
                            self.category = cat;
                            self.schedule_search();
                        }
                    }
                    // 「更多」：弹出菜单（原版是占位按钮，这里是真筛选）
                    let resp = ui.menu_button(
                        RichText::new("更多 ▾")
                            .size(12.0)
                            .color(Color32::from_rgb(232, 232, 232)),
                        |ui| {
                            for &cat in MORE_CATS {
                                let sel = self.category == Some(cat);
                                if ui.selectable_label(sel, cat.display_name()).clicked() {
                                    self.category = Some(cat);
                                    self.schedule_search();
                                    ui.close_menu();
                                }
                            }
                        },
                    );
                    let _ = resp;
                    let _ = MORE_CATS
                        .iter()
                        .any(|c| Some(*c) == self.category);
                });
            });
    }

    fn draw_results(&mut self, ui: &mut egui::Ui) {
        if self.rows.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new(if self.query.trim().is_empty() {
                        "暂无条目"
                    } else {
                        "没有找到结果"
                    })
                    .size(13.0)
                    .color(Color32::from_rgb(154, 154, 154)),
                );
            });
            return;
        }
        let mut action: Option<RowAction> = None;
        let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if self.scroll_to_selected {
            let offset = (self.selected as f32 * ROW_H - 140.0).max(0.0);
            scroll = scroll.vertical_scroll_offset(offset);
            self.scroll_to_selected = false;
        }
        scroll.show_rows(ui, ROW_H, self.rows.len(), |ui, range| {
            for idx in range {
                if let Some(a) = self.draw_row(ui, idx) {
                    action = Some(a);
                }
            }
        });
        match action {
            Some(RowAction::Select(idx)) => self.selected = idx,
            Some(RowAction::Open(idx)) => {
                let ctx = ui.ctx().clone();
                self.open_row(&ctx, idx, false);
            }
            None => {}
        }
    }

    /// 手绘一行：分类色块 + 首字 / 名字（命中段高亮）/ 路径 / 分类胶囊。
    /// 手绘而不用子控件拼装，是为了 show_rows 虚拟化下每行开销恒定。
    fn draw_row(&self, ui: &mut egui::Ui, idx: usize) -> Option<RowAction> {
        let row = self.rows.get(idx)?.clone();
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), ROW_H),
            egui::Sense::click(),
        );
        let selected = idx == self.selected;
        let painter = ui.painter_at(rect);
        if selected {
            painter.rect_filled(
                rect,
                Rounding::same(8.0),
                Color32::from_rgba_unmultiplied(37, 99, 235, 48),
            );
        } else if response.hovered() {
            painter.rect_filled(rect, Rounding::same(8.0), Color32::from_white_alpha(10));
        }

        // 分类色块 32×32 + display_name 首字
        let (r, g, b) = row.category.tile_color();
        let tile_rect = egui::Rect::from_min_size(
            rect.min + Vec2::new(4.0, (ROW_H - 32.0) / 2.0),
            Vec2::splat(32.0),
        );
        painter.rect_filled(tile_rect, Rounding::same(7.0), Color32::from_rgb(r, g, b));
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
            FontId::proportional(14.0),
            Color32::WHITE,
        );

        // 右侧分类胶囊：先量宽，名字区域要给它让位
        let cat_name = row.category.display_name();
        let cat_font = FontId::proportional(10.5);
        let cat_galley = painter.layout_no_wrap(
            cat_name.to_string(),
            cat_font.clone(),
            Color32::from_rgb(207, 207, 207),
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
            Color32::from_white_alpha(16),
        );
        painter.galley(
            egui::pos2(
                capsule_rect.center().x - cat_galley.size().x / 2.0,
                capsule_rect.center().y - cat_galley.size().y / 2.0,
            ),
            cat_galley,
            Color32::from_rgb(207, 207, 207),
        );

        // 名字（命中段高亮）+ 路径，横向裁剪不换行
        let text_x = rect.left() + 44.0;
        let text_clip = egui::Rect::from_min_max(
            egui::pos2(text_x, rect.top()),
            egui::pos2(capsule_rect.left() - 8.0, rect.bottom()),
        );
        let painter = painter.with_clip_rect(text_clip);
        let name_font = FontId::proportional(13.5);
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
            color: Color32::from_rgb(242, 242, 242),
            ..Default::default()
        };
        match hl {
            Some((s, e)) => {
                job.append(&name[..s], 0.0, base_fmt.clone());
                job.append(
                    &name[s..e],
                    0.0,
                    TextFormat {
                        font_id: FontId::proportional(13.5),
                        color: Color32::from_rgb(251, 191, 36),
                        underline: Stroke::new(1.0_f32, Color32::from_rgb(251, 191, 36)),
                        ..Default::default()
                    },
                );
                job.append(&name[e..], 0.0, base_fmt);
            }
            None => job.append(name, 0.0, base_fmt),
        }
        job.wrap.max_width = f32::INFINITY;
        let galley = painter.layout_job(job);
        painter.galley(egui::pos2(text_x, rect.top() + 5.0), galley, Color32::WHITE);

        let path_galley = painter.layout_no_wrap(
            row.path.clone(),
            FontId::proportional(11.0),
            Color32::from_rgb(154, 154, 154),
        );
        painter.galley(
            egui::pos2(text_x, rect.top() + 25.0),
            path_galley,
            Color32::from_rgb(154, 154, 154),
        );

        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.double_clicked() {
            Some(RowAction::Open(idx))
        } else if response.clicked() {
            Some(RowAction::Select(idx))
        } else {
            None
        }
    }

    fn draw_status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&self.status)
                    .size(11.5)
                    .color(Color32::from_rgb(154, 154, 154)),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new("↑↓ 选择 · Enter 打开 · Ctrl+Enter 打开所在文件夹")
                        .size(11.5)
                        .color(Color32::from_rgb(110, 110, 110)),
                );
            });
        });
    }
}

fn chip_button(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let fill = if selected {
        Color32::from_rgb(37, 99, 235)
    } else {
        Color32::from_white_alpha(10)
    };
    let stroke = if selected {
        Stroke::new(1.0_f32, Color32::from_rgb(37, 99, 235))
    } else {
        Stroke::new(1.0_f32, Color32::from_white_alpha(18))
    };
    ui.add(
        egui::Button::new(
            RichText::new(label)
                .size(12.0)
                .color(Color32::from_rgb(232, 232, 232)),
        )
        .fill(fill)
        .stroke(stroke)
        .rounding(Rounding::same(12.0)),
    )
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
                tray::TrayAction::Exit => {
                    self.allow_exit = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }
        // 关窗 = 隐藏（退出走托盘），与 C# 版 ShutdownMode.OnExplicitShutdown 一致
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_exit {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.hide(ctx);
        }

        self.poll_channels(ctx);
        if self.visible {
            self.handle_keys(ctx);
        }

        // 深色仿 Mica：纯色深底 + 圆角 + 细描边（真 Mica 需 DWM，见文件头）
        let panel_frame = egui::Frame::central_panel(&ctx.style())
            .fill(Color32::from_rgb(30, 32, 38))
            .rounding(Rounding::same(12.0))
            .stroke(Stroke::new(1.0_f32, Color32::from_white_alpha(28)))
            .inner_margin(egui::Margin::symmetric(14.0, 10.0));
        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                self.draw_search_box(ui);
                ui.add_space(8.0);
                self.draw_chips(ui);
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

    }
}

fn main() -> eframe::Result<()> {
    let viewport = egui::ViewportBuilder::default()
        .with_title("瞬觅 ShunSeek")
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
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(ShunSeekApp::new(cc)))
        }),
    )
}
