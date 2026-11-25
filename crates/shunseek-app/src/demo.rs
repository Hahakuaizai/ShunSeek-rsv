//! 演示数据源：逐条照抄 C# 版 DemoIndexSource.cs（36 条，路径与分类一致）。
//! 非 Windows 平台没有 MFT/AppScanner，索引重建时用这批数据兜底，
//! 让 UI 在任何平台都能完整跑通搜索 / 筛选 / 打开逻辑。
//! Windows 上它只在 MFT 枚举为空时的旧装配里出现过，正式装配见 main.rs。

use shunseek_core::{Category, RawEntry};

pub fn demo_entries() -> Vec<RawEntry> {
    let items: &[(&str, &str, Category)] = &[
        ("微信", r"C:\Program Files\Tencent\WeChat\WeChat.exe", Category::App),
        ("微信", r"C:\Program Files\Tencent\WeChat", Category::InstallDir),
        ("微信.lnk", r"C:\Users\demo\Desktop\微信.lnk", Category::Shortcut),
        ("网易云音乐", r"D:\Apps\NetEase\CloudMusic\cloudmusic.exe", Category::App),
        ("网易云音乐.lnk", r"C:\Users\demo\Desktop\网易云音乐.lnk", Category::Shortcut),
        ("王者荣耀.lnk", r"C:\Users\demo\Desktop\王者荣耀.lnk", Category::Shortcut),
        ("Visual Studio Code", r"C:\Users\demo\AppData\Local\Programs\Microsoft VS Code\Code.exe", Category::App),
        ("WPS Office", r"C:\Program Files\Kingsoft\WPS Office\ksolaunch.exe", Category::App),
        ("Steam", r"D:\Games\Steam\steam.exe", Category::App),
        ("Photoshop", r"C:\Program Files\Adobe\Photoshop\Photoshop.exe", Category::App),
        ("毕业论文终稿.docx", r"D:\学习\毕业论文\毕业论文终稿.docx", Category::Document),
        ("毕业论文答辩.pptx", r"D:\学习\毕业论文\毕业论文答辩.pptx", Category::Document),
        ("2026年度报告.pdf", r"D:\工作\报告\2026年度报告.pdf", Category::Document),
        ("财务报表2026.xlsx", r"D:\工作\财务\财务报表2026.xlsx", Category::Document),
        ("会议纪要-11月.md", r"D:\工作\会议\会议纪要-11月.md", Category::Document),
        ("课程表.txt", r"C:\Users\demo\Documents\课程表.txt", Category::Document),
        ("IMG_20261101_120433.jpg", r"C:\Users\demo\Pictures\IMG_20261101_120433.jpg", Category::Image),
        ("毕业合影.png", r"C:\Users\demo\Pictures\毕业合影.png", Category::Image),
        ("桌面壁纸.webp", r"C:\Users\demo\Pictures\桌面壁纸.webp", Category::Image),
        ("屏幕截图 2026-11-20.png", r"C:\Users\demo\Pictures\Screenshots\屏幕截图 2026-11-20.png", Category::Image),
        ("旅行录像.mp4", r"D:\视频\旅行录像.mp4", Category::Video),
        ("课程录屏-第3章.mp4", r"D:\视频\课程\课程录屏-第3章.mp4", Category::Video),
        ("电影.mkv", r"D:\视频\电影.mkv", Category::Video),
        ("周杰伦-晴天.mp3", r"D:\音乐\周杰伦-晴天.mp3", Category::Audio),
        ("播客第12期.mp3", r"D:\音乐\播客\播客第12期.mp3", Category::Audio),
        ("无损合集.flac", r"D:\音乐\无损合集.flac", Category::Audio),
        ("照片备份.zip", r"E:\备份\照片备份.zip", Category::Archive),
        ("课程资料.rar", r"E:\备份\课程资料.rar", Category::Archive),
        ("main.py", r"D:\Projects\demo\main.py", Category::Code),
        ("app.js", r"D:\Projects\course-vault\src\app.js", Category::Code),
        ("config.json", r"D:\Projects\demo\config.json", Category::Code),
        ("学习", r"D:\学习", Category::Folder),
        ("工作", r"D:\工作", Category::Folder),
        ("视频", r"D:\视频", Category::Folder),
        ("Downloads", r"C:\Users\demo\Downloads", Category::Folder),
        ("setup.exe", r"C:\Users\demo\Downloads\setup.exe", Category::Executable),
    ];
    items
        .iter()
        .map(|(name, path, cat)| {
            let mut e = RawEntry::new(name.to_string(), path.to_string(), *cat);
            // 与 C# 版一致：文件夹与安装目录带 IsDirectory 标记
            e.is_dir = matches!(cat, Category::Folder | Category::InstallDir);
            e
        })
        .collect()
}
