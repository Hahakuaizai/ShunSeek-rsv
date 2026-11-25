//! 瞬觅速度基准（Rust 版，移植自 .NET 版 ShunSeek.Bench）：
//! 合成 200 万条混合条目 → 建紧凑索引 → 跑同一组代表性查询 → 再测落盘/ mmap 加载。
//! 合成数据的分布、词干、分类比例与 C# 版 Generate 逐项一致；随机数发生器不同
//! （C# 是 System.Random(42)，这里是 xorshift64* 同种子），所以条目不逐条相同，
//! 但量级可比。换机器请 Release 配置重跑。

use std::sync::atomic::AtomicBool;
use std::time::Instant;

use shunseek_core::{build_index, Category, IndexStore, RawEntry, SearchEngine};

const ENTRY_COUNT: usize = 2_000_000;

struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo)
    }
}

fn generate(count: usize) -> Vec<RawEntry> {
    let mut rng = Rng(42);
    let zh_stems = [
        "报告", "合同", "照片", "会议纪要", "项目计划", "毕业论文", "财务报表", "产品需求",
        "测试数据", "学习笔记", "设计稿", "简历", "课件", "备份", "年度总结", "报价单",
    ];
    let en_stems = [
        "report", "invoice", "photo", "project-plan", "meeting-notes", "budget", "design",
        "backup", "resume", "dataset", "release-notes", "homework",
    ];
    let softwares = [
        "微信", "QQ", "网易云音乐", "王者荣耀", "Steam", "WPS Office", "Photoshop",
        "Visual Studio Code", "Chrome", "钉钉",
    ];
    let doc_exts = [".docx", ".pdf", ".xlsx", ".pptx", ".txt", ".md"];
    let img_exts = [".jpg", ".png", ".gif", ".webp"];
    let vid_exts = [".mp4", ".mkv", ".mov"];
    let aud_exts = [".mp3", ".flac", ".wav"];
    let arc_exts = [".zip", ".rar", ".7z"];
    let code_exts = [".cs", ".py", ".js", ".json"];
    let dir_names = [
        "工作", "学习", "娱乐", "项目", "资料", "Downloads", "Documents", "Pictures",
        "Projects", "Archive",
    ];
    let drives = ["C:", "D:", "E:"];

    let random_dir = |rng: &mut Rng| -> String {
        let depth = rng.range(2, 6);
        let mut parts = vec![drives[rng.below(drives.len())].to_string()];
        for _ in 1..depth {
            parts.push(format!(
                "{}{}",
                dir_names[rng.below(dir_names.len())],
                rng.range(1, 20)
            ));
        }
        parts.join("\\")
    };

    let mut list = Vec::with_capacity(count + 2);
    for _ in 0..count {
        let roll = rng.below(100);
        let dir = random_dir(&mut rng);
        let (name, cat, is_dir) = if roll < 15 {
            (
                format!("{}_{}", dir_names[rng.below(dir_names.len())], rng.below(10000)),
                Category::Folder,
                true,
            )
        } else if roll < 20 {
            (
                format!("{}.lnk", softwares[rng.below(softwares.len())]),
                Category::Shortcut,
                false,
            )
        } else if roll < 22 {
            (
                softwares[rng.below(softwares.len())].to_string(),
                Category::App,
                false,
            )
        } else if roll < 45 {
            (
                format!(
                    "{}{}{}",
                    zh_stems[rng.below(zh_stems.len())],
                    rng.range(2018, 2027),
                    doc_exts[rng.below(doc_exts.len())]
                ),
                Category::Document,
                false,
            )
        } else if roll < 60 {
            (
                format!("IMG_{}{}", rng.range(100000, 999999), img_exts[rng.below(img_exts.len())]),
                Category::Image,
                false,
            )
        } else if roll < 68 {
            (
                format!(
                    "{}_录像{}{}",
                    zh_stems[rng.below(zh_stems.len())],
                    rng.below(1000),
                    vid_exts[rng.below(vid_exts.len())]
                ),
                Category::Video,
                false,
            )
        } else if roll < 73 {
            (
                format!("歌曲{}{}", rng.below(10000), aud_exts[rng.below(aud_exts.len())]),
                Category::Audio,
                false,
            )
        } else if roll < 78 {
            (
                format!(
                    "{}-backup-{}{}",
                    en_stems[rng.below(en_stems.len())],
                    rng.below(100),
                    arc_exts[rng.below(arc_exts.len())]
                ),
                Category::Archive,
                false,
            )
        } else if roll < 88 {
            (
                format!(
                    "{}_{}{}",
                    en_stems[rng.below(en_stems.len())],
                    rng.below(500),
                    code_exts[rng.below(code_exts.len())]
                ),
                Category::Code,
                false,
            )
        } else {
            (
                format!("{}-{}.dat", en_stems[rng.below(en_stems.len())], rng.below(100000)),
                Category::Other,
                false,
            )
        };
        let full = format!("{dir}\\{name}");
        list.push(RawEntry {
            name,
            full_path: Some(full),
            category: cat,
            file_ref: 0,
            parent_ref: 0,
            is_dir,
        });
    }
    list.push(RawEntry::new(
        "王者荣耀.lnk".into(),
        r"C:\Users\Public\Desktop\王者荣耀.lnk".into(),
        Category::Shortcut,
    ));
    list.push(RawEntry::new(
        "王者荣耀".into(),
        r"D:\Games\王者荣耀".into(),
        Category::App,
    ));
    list
}

fn run_query(engine: &SearchEngine, query: &str, category: Option<Category>) {
    let flag = AtomicBool::new(false);
    let mut times = Vec::new();
    let mut hits = Vec::new();
    for _ in 0..5 {
        let sw = Instant::now();
        hits = engine.search(query, category, 100, &flag);
        times.push(sw.elapsed().as_secs_f64() * 1000.0);
    }
    let avg: f64 = times[1..].iter().sum::<f64>() / (times.len() - 1) as f64;
    let label = match category {
        Some(c) => format!("{query} [{}]", c.display_name()),
        None => query.to_string(),
    };
    println!(
        "查询 \"{}\": 首次 {:.1} ms, 平均 {:.1} ms, 返回 {} 条",
        label,
        times[0],
        avg,
        hits.len()
    );
    for h in hits.iter().take(3) {
        let s = engine.store();
        println!(
            "    · {}  [{}]  {}",
            s.name(h.index as usize),
            s.category(h.index as usize).display_name(),
            s.full_path(h.index as usize)
        );
    }
}

fn main() {
    println!("=== ShunSeek Bench (Rust) ===");
    println!(
        "条目数: {}   线程数: {}   OS: {}",
        ENTRY_COUNT,
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        std::env::consts::OS
    );

    let sw = Instant::now();
    let raw = generate(ENTRY_COUNT);
    println!("合成数据: {} ms", sw.elapsed().as_millis());

    let sw = Instant::now();
    let store = build_index(raw, None);
    let build_ms = sw.elapsed().as_millis();
    println!("建索引:   {} ms   条目: {}", build_ms, store.count());
    println!(
        "索引内存: {:.1} MB（精确值：全部数组+池的实际字节数）",
        store.memory_bytes() as f64 / 1024.0 / 1024.0
    );
    println!("驻留目录数: {}", store.dir_count());

    let engine = SearchEngine::new(&store, None);
    run_query(&engine, "报告", None);
    run_query(&engine, "合同", None);
    run_query(&engine, "report", None);
    run_query(&engine, "wzry", None);
    run_query(&engine, "mp4", Some(Category::Video));
    run_query(&engine, "2024", None);

    // ---- 落盘 + mmap 加载（Rust 版新增，原版只有内存索引）----
    let path = std::env::temp_dir().join("shunseek-bench-index.bin");
    let sw = Instant::now();
    store.save_to(&path).expect("save index");
    println!("落盘: {} ms（文件 {} 字节）", sw.elapsed().as_millis(), std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0));

    let sw = Instant::now();
    let loaded: IndexStore = IndexStore::load_mmap(&path).expect("load index");
    let load_us = sw.elapsed().as_micros();
    println!("mmap 加载: {} µs   条目: {}", load_us, loaded.count());
    let engine2 = SearchEngine::new(&loaded, None);
    run_query(&engine2, "wzry", None);
    let _ = std::fs::remove_file(&path);
}
