//! 具体类型名 + 重要度三档（对应 C# 版 FileTypeName，逐项移植）：
//! .docx → 「Word 文档」而不是笼统的「文档」；重要度 High（应用/文档/
//! 文件夹，人真正会点开的）/ Normal（图片/视频/音频/压缩包/代码）/
//! Low（日志/缓存/临时/转储等中间产物），驱动结果行圆点、悬浮卡
//! 类型行与搜索结果折叠策略（见 app 的展示派生）。

use crate::category::Category;

/// 条目重要级（数值序与 C# 版 TypeImportance 一致）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Importance {
    High = 0,
    Normal = 1,
    Low = 2,
}

/// 已知扩展名 →（具体类型名，重要度）。与 C# 版 Known 表逐项一致
///（扩展名含点、比较不区分大小写）。
fn known(ext_lower: &str) -> Option<(&'static str, Importance)> {
    use Importance::*;
    let v = match ext_lower {
        // 文档
        ".doc" | ".docx" => ("Word 文档", High),
        ".pdf" => ("PDF 文档", High),
        ".xls" | ".xlsx" => ("Excel 表格", High),
        ".csv" => ("CSV 表格", High),
        ".ppt" | ".pptx" => ("PPT 演示", High),
        ".md" | ".markdown" => ("Markdown", High),
        ".txt" => ("文本", High),
        ".rtf" => ("富文本", Normal),
        ".epub" => ("电子书", Normal),
        ".wps" => ("WPS 文档", High),
        ".et" => ("WPS 表格", High),
        ".dps" => ("WPS 演示", High),
        // 图片
        ".jpg" | ".jpeg" => ("JPEG 图片", Normal),
        ".png" => ("PNG 图片", Normal),
        ".gif" => ("GIF 动图", Normal),
        ".webp" => ("WebP 图片", Normal),
        ".bmp" => ("BMP 图片", Normal),
        ".svg" => ("SVG 矢量图", Normal),
        ".ico" => ("图标", Normal),
        ".psd" => ("PSD 源文件", Normal),
        ".ai" => ("Illustrator 源文件", Normal),
        ".heic" => ("HEIC 图片", Normal),
        ".raw" => ("相机原始图", Normal),
        // 视频 / 音频
        ".mp4" => ("MP4 视频", Normal),
        ".mkv" => ("MKV 视频", Normal),
        ".avi" => ("AVI 视频", Normal),
        ".mov" => ("MOV 视频", Normal),
        ".flv" => ("FLV 视频", Normal),
        ".mp3" => ("MP3 音频", Normal),
        ".flac" => ("FLAC 无损", Normal),
        ".wav" => ("WAV 音频", Normal),
        ".m4a" => ("M4A 音频", Normal),
        // 压缩包 / 磁盘镜像
        ".zip" => ("ZIP 压缩包", Normal),
        ".rar" => ("RAR 压缩包", Normal),
        ".7z" => ("7Z 压缩包", Normal),
        ".tar" => ("TAR 归档", Normal),
        ".iso" => ("光盘镜像", Normal),
        // 代码 / 配置
        ".cs" => ("C# 源码", Normal),
        ".py" => ("Python 源码", Normal),
        ".js" => ("JavaScript", Normal),
        ".ts" => ("TypeScript", Normal),
        ".java" => ("Java 源码", Normal),
        ".cpp" => ("C++ 源码", Normal),
        ".c" => ("C 源码", Normal),
        ".h" => ("C 头文件", Normal),
        ".go" => ("Go 源码", Normal),
        ".rs" => ("Rust 源码", Normal),
        ".html" => ("网页", Normal),
        ".css" => ("样式表", Normal),
        ".json" => ("JSON 配置", Normal),
        ".xml" => ("XML 配置", Normal),
        ".yml" | ".yaml" => ("YAML 配置", Normal),
        ".ini" => ("配置", Normal),
        ".sln" => ("VS 解决方案", Normal),
        ".csproj" => ("C# 工程", Normal),
        // 可执行 / 脚本
        ".exe" => ("可执行文件", High),
        ".msi" => ("安装包", High),
        ".bat" => ("批处理脚本", Normal),
        ".ps1" => ("PowerShell 脚本", Normal),
        ".sh" => ("Shell 脚本", Normal),
        // 低价值：中间产物、日志、缓存
        ".log" => ("日志", Low),
        ".tmp" => ("临时文件", Low),
        ".bak" => ("备份文件", Low),
        ".cache" => ("缓存文件", Low),
        ".dmp" => ("转储文件", Low),
        ".pdb" => ("调试符号", Low),
        ".obj" => ("编译中间产物", Low),
        _ => return None,
    };
    Some(v)
}

/// 这些文件名（完整文件名，含扩展名）基本就是日志/缓存/临时产物
///（与 C# 版 NoiseNames 一致，比较不区分大小写）。
fn is_noise_name(file_name: &str) -> bool {
    let n = file_name.to_lowercase();
    matches!(
        n.as_str(),
        "log"
            | "logs"
            | "tmp"
            | "temp"
            | "cache"
            | "caches"
            | "debug"
            | "debug.log"
            | "trace"
            | "output"
            | "console"
            | "stdout"
            | "stderr"
            | "crash"
            | "dump"
            | "thumbs.db"
            | "desktop.ini"
            | ".ds_store"
            | "lock"
            | "index"
            | "leveldb"
    )
}

/// 路径里出现过这些目录名（作为一段），就认为它在缓存/日志区
///（与 C# 版 IsUnderNoiseDirectory 一致：从最内层父目录向外逐段查，
/// 段名整段命中 NoiseNames，或段名含 "cache" 子串——cef_cache /
/// shadercache / Cache2；log 只认整段相等，免得 blog、catalog 误判）。
fn is_under_noise_directory(path: &str) -> bool {
    let mut rest = path;
    loop {
        let Some(slash) = rest.rfind(['\\', '/']) else {
            return false;
        };
        if slash == 0 {
            return false;
        }
        rest = &rest[..slash];
        let start = rest.rfind(['\\', '/']).map(|p| p + 1).unwrap_or(0);
        let segment = &rest[start..];
        if is_noise_name(segment) {
            return true;
        }
        if segment.to_lowercase().contains("cache") {
            return true;
        }
        if start == 0 {
            return false;
        }
    }
}

/// 识别条目「是什么」并给出重要级（对应 C# 版 FileTypeName.Describe）。
/// path 用完整路径（噪音目录判定要看父目录段）；is_dir 为真或分类是
/// Folder/InstallDir 时按目录处理。
pub fn describe(path: &str, category: Category, is_dir: bool) -> (String, Importance) {
    if is_dir || matches!(category, Category::Folder | Category::InstallDir) {
        return if category == Category::InstallDir {
            ("安装目录".to_string(), Importance::High)
        } else {
            ("文件夹".to_string(), Importance::High)
        };
    }
    if category == Category::App {
        return ("应用".to_string(), Importance::High);
    }
    if category == Category::Shortcut {
        return ("快捷方式".to_string(), Importance::High);
    }

    let file_name = match path.rfind(['\\', '/']) {
        Some(p) => &path[p + 1..],
        None => path,
    };
    let ext = match file_name.rfind('.') {
        Some(p) if p > 0 => Some(&file_name[p..]),
        _ => None,
    };
    if let Some(e) = ext {
        if let Some((label, imp)) = known(&e.to_lowercase()) {
            return (label.to_string(), imp);
        }
    }

    // 没有扩展名命中、但名字本身就在喊「我是垃圾」
    if is_noise_name(file_name) {
        return ("日志/缓存".to_string(), Importance::Low);
    }
    // 放在缓存/日志目录里的，一并不重要
    if is_under_noise_directory(path) {
        return ("缓存/日志文件".to_string(), Importance::Low);
    }

    // 认不出来的扩展名：说清「是什么类别的什么文件」
    let fallback = match ext {
        None => "无扩展名文件".to_string(),
        Some(e) => format!("{} 文件", e[1..].to_uppercase()),
    };
    let importance = match category {
        Category::Document => Importance::High,
        _ => Importance::Normal,
    };
    (fallback, importance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_types() {
        assert_eq!(
            describe("C:\\x\\报告.docx", Category::Document, false),
            ("Word 文档".to_string(), Importance::High)
        );
        assert_eq!(
            describe("C:\\x\\a.py", Category::Code, false).0,
            "Python 源码"
        );
        assert_eq!(
            describe("C:\\x\\a.iso", Category::Archive, false).0,
            "光盘镜像"
        );
        assert_eq!(
            describe("C:\\x\\a.log", Category::Other, false),
            ("日志".to_string(), Importance::Low)
        );
    }

    #[test]
    fn dirs_and_apps_are_high() {
        assert_eq!(
            describe("C:\\x\\dir", Category::Folder, true),
            ("文件夹".to_string(), Importance::High)
        );
        assert_eq!(
            describe("C:\\Program Files\\WeChat", Category::InstallDir, true).0,
            "安装目录"
        );
        assert_eq!(
            describe("C:\\x\\WeChat.exe", Category::App, false).0,
            "应用"
        );
    }

    #[test]
    fn noise_dirs_are_low() {
        // 缓存目录里的普通文件 → Low（cef_cache 含 cache 子串）
        let (_, imp) = describe("C:\\app\\cef_cache\\data.bin", Category::Other, false);
        assert_eq!(imp, Importance::Low);
        // blog 目录不能误判（log 只认整段）
        let (_, imp) = describe("C:\\blog\\post.txt", Category::Document, false);
        assert_eq!(imp, Importance::High);
        // 噪音文件名
        let (label, imp) = describe("C:\\x\\debug.log", Category::Other, false);
        assert_eq!((label.as_str(), imp), ("日志", Importance::Low));
        let (_, imp) = describe("C:\\x\\thumbs.db", Category::Other, false);
        assert_eq!(imp, Importance::Low);
    }

    #[test]
    fn unknown_ext_fallback() {
        assert_eq!(
            describe("C:\\x\\a.xyz", Category::Other, false).0,
            "XYZ 文件"
        );
        assert_eq!(
            describe("C:\\x\\noext", Category::Other, false).0,
            "无扩展名文件"
        );
    }
}
