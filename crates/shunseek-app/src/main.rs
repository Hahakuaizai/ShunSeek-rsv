//! 瞬觅 ShunSeek 界面（Rust 版，egui 重绘）。
//! 对照原 WPF 版逐项搬：MainWindow.xaml 的四段布局、MainViewModel 的
//! 搜索调度（取消 + 30ms 防抖 + 后台线程）、App.xaml.cs 的启动装配
//! （单实例除外，见下）、HotkeyManager 的 Alt+Space、TrayIconManager 的托盘。
//!
//! 与 WPF 版的差异（如实记录）：
//!  - 背景：真 Mica 需要 DWM 的 DWMWA_SYSTEMBACKDROP_TYPE，egui 不直接
//!    支持；这里用手工竖向渐变 + 圆角 + 投影 + 细描边近似（见 update()
//!    里的 paint_vertical_gradient 与 CentralPanel 的 Frame）。
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
const ROW_H: f32 = 52.0;
/// 强调蓝（选中 / 聚焦 / chips），与 WPF 版 AccentBrush 对齐。
const ACCENT: Color32 = Color32::from_rgb(37, 99, 235);
/// 空索引护栏文案：数据源全灭时亮红字，让用户能原样转达给开发者。
const ALERT_EMPTY_INDEX: &str = "索引为空（0 条）——数据源都没拿到数据，把这行字发给开发者";

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
    /// 后台重建完成：新索引本体直接随消息传给 UI 线程换入（内存直传，
    /// 不再走 rename + mmap 的文件换入——那条路一旦 save/rename/load
    /// 任一步失败就会把新索引丢掉、沿用旧的空索引，还不给任何提示）。
    /// saved 只说明缓存 index-next.bin 有没有存上，失败不影响本次使用；
    /// summary 是各数据源的条数摘要，进状态栏让用户能看到来源构成。
    Rebuilt {
        store: IndexStore,
        count: usize,
        saved: bool,
        summary: String,
    },
}

/// 单个数据源的隔离包装：某个源 panic / 报错只丢它自己的数据，
/// 不许拖垮整个重建线程（否则消息永远发不出来，界面只剩旧空索引）。
fn guarded<T>(f: impl FnOnce() -> T) -> Option<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).ok()
}

/// 平台数据源汇总（在后台线程跑），返回 (条目, 盘根表, 来源摘要)。
/// Windows 分层兜底：①各固定盘 MFT → ②appscan → ③MFT 总条数为 0 时
/// walker 遍历 %USERPROFILE% → ④总条数仍为 0 时对每个固定盘根全盘遍历。
/// 每个源独立 catch_unwind 并分别计数，计数拼进摘要字符串。
/// 非 Windows：DemoIndexSource 的演示数据。
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
            // 整个重建体再套一层隔离：就算 build_index 里出了意外，
            // 也要把「空索引 + 异常摘要」发出去，让 UI 的空索引护栏
            // 能亮红字，而不是消息永不达、用户对着旧空索引干瞪眼。
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
            // 缓存尽力存：失败只丢缓存（saved=false），store 本体照常直传。
            let next = dir.join("index-next.bin");
            let saved = store.save_to(&next).is_ok();
            let _ = tx.send(IndexMsg::Rebuilt {
                store,
                count,
                saved,
                summary,
            });
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
    /// 状态栏警戒标志（空索引护栏）：true 时状态文字与圆点标红。
    status_alert: bool,
    /// 最近一次索引就绪文案（含来源摘要）；空查询的搜索完成消息
    /// 会覆盖 status，用它把就绪文案找回来，别让摘要一闪而过。
    ready_status: Option<String>,
    /// 搜索框上一帧是否聚焦（画聚焦描边时读它，一帧延迟无感）。
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
            status_alert: false,
            ready_status: None,
            search_focused: false,
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
        // 索引重建完成：store 本体内存直传，直接换入，不再经文件。
        while let Ok(msg) = self.index_rx.try_recv() {
            match msg {
                IndexMsg::Rebuilt {
                    store,
                    count,
                    saved,
                    summary,
                } => {
                    if let Ok(mut guard) = self.store.lock() {
                        *guard = store;
                    }
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
                        let mut s = format!("索引就绪 · {count} 条（来源 {summary_short}）");
                        if !saved {
                            s.push_str(" · 缓存写入失败，不影响本次使用");
                        }
                        s
                    };
                    self.status = text.clone();
                    self.ready_status = Some(text);
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
    // 圆角轮廓点（顺时针，从右上弧开始，每 15° 一个点）
    let mut outline: Vec<egui::Pos2> = Vec::new();
    let corners: [(f32, f32, f32, f32); 4] = [
        // (圆心 x, 圆心 y, 起始角°, 终止角°)
        (rect.right() - radius, rect.top() + radius, -90.0, 0.0),
        (rect.right() - radius, rect.bottom() - radius, 0.0, 90.0),
        (rect.left() + radius, rect.bottom() - radius, 90.0, 180.0),
        (rect.left() + radius, rect.top() + radius, 180.0, 270.0),
    ];
    for (cx, cy, a0, a1) in corners {
        let mut a = a0;
        while a < a1 {
            let rad = a.to_radians();
            outline.push(egui::pos2(
                cx + radius * rad.cos(),
                cy + radius * rad.sin(),
            ));
            a += 15.0;
        }
        let rad = a1.to_radians();
        outline.push(egui::pos2(
            cx + radius * rad.cos(),
            cy + radius * rad.sin(),
        ));
    }
    let center = rect.center();
    let mut mesh = egui::epaint::Mesh::default();
    mesh.vertices.push(egui::epaint::Vertex {
        pos: center,
        uv: egui::epaint::WHITE_UV,
        color: color_at(center.y),
    });
    for p in &outline {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: *p,
            uv: egui::epaint::WHITE_UV,
            color: color_at(p.y),
        });
    }
    let n = outline.len() as u32;
    for i in 0..n {
        let a = 1 + i;
        let b = 1 + (i + 1) % n;
        mesh.indices.extend_from_slice(&[0, a, b]);
    }
    painter.add(egui::Shape::mesh(mesh));
}

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
        // 聚焦态读上一帧记录的 has_focus（一帧延迟，体感无差），
        // 聚焦时描边换成 1.5px 强调蓝，未聚焦是淡白细线。
        let (stroke, fill) = if self.search_focused {
            (
                Stroke::new(1.5_f32, ACCENT),
                Color32::from_white_alpha(22),
            )
        } else {
            (
                Stroke::new(1.0_f32, Color32::from_white_alpha(30)),
                Color32::from_white_alpha(22),
            )
        };
        let frame = egui::Frame::none()
            .fill(fill)
            .stroke(stroke)
            .rounding(Rounding::same(10.0))
            .inner_margin(egui::Margin::symmetric(12.0, 10.0));
        frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                // 左侧放大镜：手绘圆 + 斜线两笔，不用 emoji
                let (icon_rect, _) = ui.allocate_exact_size(
                    Vec2::new(22.0, 22.0),
                    egui::Sense::hover(),
                );
                let painter = ui.painter_at(icon_rect);
                let c = icon_rect.center() + Vec2::new(-1.5, -1.5);
                let icon_stroke =
                    Stroke::new(1.6_f32, Color32::from_rgb(139, 148, 158));
                painter.circle_stroke(c, 5.5, icon_stroke);
                painter.line_segment(
                    [
                        c + Vec2::new(4.0, 4.0),
                        c + Vec2::new(8.0, 8.0),
                    ],
                    icon_stroke,
                );
                let edit = egui::TextEdit::singleline(&mut self.query)
                    .desired_width(f32::INFINITY)
                    .hint_text("搜索应用、文件、文件夹…")
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
                // 右侧 ESC 徽标（不可点，纯提示，与 WPF 版一致；弱化处理）
                egui::Frame::none()
                    .fill(Color32::from_white_alpha(10))
                    .rounding(Rounding::same(4.0))
                    .inner_margin(egui::Margin::symmetric(6.0, 1.0))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("ESC")
                                .size(10.0)
                                .color(Color32::from_rgb(110, 118, 129)),
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
                    ui.spacing_mut().item_spacing.x = 8.0;
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
                            .size(12.5)
                            .color(Color32::from_rgb(201, 209, 217)),
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
                Color32::from_rgba_unmultiplied(37, 99, 235, 46),
            );
            // 左侧 3px 强调蓝竖条（内缩，带圆角）
            let bar = egui::Rect::from_min_size(
                rect.min + Vec2::new(1.0, 10.0),
                Vec2::new(3.0, ROW_H - 20.0),
            );
            painter.rect_filled(bar, Rounding::same(2.0), ACCENT);
        } else if response.hovered() {
            painter.rect_filled(rect, Rounding::same(8.0), Color32::from_white_alpha(8));
        }

        // 分类色块 36×36 + display_name 首字
        let (r, g, b) = row.category.tile_color();
        let tile_rect = egui::Rect::from_min_size(
            rect.min + Vec2::new(6.0, (ROW_H - 36.0) / 2.0),
            Vec2::splat(36.0),
        );
        painter.rect_filled(tile_rect, Rounding::same(9.0), Color32::from_rgb(r, g, b));
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

        // 右侧分类胶囊：分类色低透明底（alpha 40）+ 同色提亮文字，先量宽，
        // 名字区域要给它让位
        let cat_name = row.category.display_name();
        let cat_font = FontId::proportional(10.5);
        let cat_text_color = Color32::from_rgb(r.max(165), g.max(165), b.max(165));
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
        let text_x = rect.left() + 50.0;
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
                        font_id: FontId::proportional(14.0),
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
        painter.galley(egui::pos2(text_x, rect.top() + 7.0), galley, Color32::WHITE);

        let path_galley = painter.layout_no_wrap(
            row.path.clone(),
            FontId::proportional(11.5),
            Color32::from_rgb(139, 148, 158),
        );
        painter.galley(
            egui::pos2(text_x, rect.top() + 30.0),
            path_galley,
            Color32::from_rgb(139, 148, 158),
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
            // 左侧状态圆点：就绪绿 / 警戒红（空索引护栏）
            let (dot_rect, _) =
                ui.allocate_exact_size(Vec2::splat(12.0), egui::Sense::hover());
            let dot_color = if self.status_alert {
                Color32::from_rgb(248, 113, 113) // #F87171
            } else {
                Color32::from_rgb(52, 211, 153) // #34D399
            };
            ui.painter_at(dot_rect)
                .circle_filled(dot_rect.center(), 3.0, dot_color);
            let text_color = if self.status_alert {
                Color32::from_rgb(248, 113, 113)
            } else {
                Color32::from_rgb(154, 154, 154)
            };
            ui.label(RichText::new(&self.status).size(11.0).color(text_color));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new("↑↓ 选择 · Enter 打开 · Ctrl+Enter 打开所在文件夹")
                        .size(11.0)
                        .color(Color32::from_rgb(110, 110, 110)),
                );
            });
        });
    }
}

fn chip_button(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let (fill, stroke, text) = if selected {
        (
            ACCENT,
            Stroke::new(1.0_f32, ACCENT),
            RichText::new(label).size(12.5).strong().color(Color32::WHITE),
        )
    } else {
        (
            Color32::from_white_alpha(8),
            Stroke::new(1.0_f32, Color32::from_white_alpha(18)),
            RichText::new(label)
                .size(12.5)
                .color(Color32::from_rgb(201, 209, 217)),
        )
    };
    ui.add(
        egui::Button::new(text)
            .fill(fill)
            .stroke(stroke)
            .rounding(Rounding::same(13.0))
            .min_size(Vec2::new(0.0, 26.0)),
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

        // 悬浮窗背景：手工竖向渐变（上 #252A35 → 下 #16181D）画在背景层，
        // 面板本身透明，靠 Frame 的投影 + 淡描边撑出 Fluent 悬浮感。
        paint_vertical_gradient(
            &ctx.layer_painter(egui::LayerId::background()),
            ctx.screen_rect(),
            12.0,
            Color32::from_rgb(37, 42, 53),
            Color32::from_rgb(22, 24, 29),
        );
        let panel_frame = egui::Frame::central_panel(&ctx.style())
            .fill(Color32::TRANSPARENT)
            .rounding(Rounding::same(12.0))
            .stroke(Stroke::new(1.0_f32, Color32::from_white_alpha(18)))
            .shadow(egui::epaint::Shadow {
                offset: Vec2::new(0.0, 8.0),
                blur: 24.0,
                spread: 0.0,
                color: Color32::from_black_alpha(140),
            })
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

/// 全局 Visuals 微调：控件圆角统一 8、选中态强调蓝半透明、
/// extreme 底色更深，往 DeskBox/WPF 深色 Fluent 的精致感靠。
fn install_visuals(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = Color32::from_rgb(22, 24, 29);
    visuals.window_fill = Color32::from_rgb(22, 24, 29);
    visuals.extreme_bg_color = Color32::from_rgb(10, 11, 14);
    visuals.selection.bg_fill = Color32::from_rgba_unmultiplied(37, 99, 235, 120);
    visuals.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
    let r = Rounding::same(8.0);
    visuals.widgets.noninteractive.rounding = r;
    visuals.widgets.inactive.rounding = r;
    visuals.widgets.hovered.rounding = r;
    visuals.widgets.active.rounding = r;
    visuals.widgets.open.rounding = r;
    visuals.window_rounding = r;
    ctx.set_visuals(visuals);
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
            install_visuals(&cc.egui_ctx);
            Ok(Box::new(ShunSeekApp::new(cc)))
        }),
    )
}
