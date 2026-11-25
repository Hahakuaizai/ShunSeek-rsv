//! 条目分类。与 C# 版 EntryCategory 数值一一对应（会写进索引缓存文件，不要重排）。
//! 顺序即 UI 分类筛选条的大致顺序。

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
pub enum Category {
    Other = 0,
    App = 1,        // 已安装软件（开始菜单/注册表 Uninstall 枚举到）
    Shortcut = 2,   // 快捷方式 .lnk
    InstallDir = 3, // 软件安装目录（注册表 InstallLocation）
    Folder = 4,     // 文件夹
    Document = 5,   // 文档
    Image = 6,      // 图片
    Video = 7,      // 视频
    Audio = 8,      // 音频
    Archive = 9,    // 压缩包
    Code = 10,      // 代码/文本工程文件
    Executable = 11, // 普通 .exe（非软件枚举来源的散落可执行文件）
}

impl Category {
    pub fn from_u8(v: u8) -> Category {
        match v {
            1 => Category::App,
            2 => Category::Shortcut,
            3 => Category::InstallDir,
            4 => Category::Folder,
            5 => Category::Document,
            6 => Category::Image,
            7 => Category::Video,
            8 => Category::Audio,
            9 => Category::Archive,
            10 => Category::Code,
            11 => Category::Executable,
            _ => Category::Other,
        }
    }

    /// 分类的中文显示名，UI 标签与筛选条共用（与 C# 版一致）。
    pub fn display_name(self) -> &'static str {
        match self {
            Category::App => "应用",
            Category::Shortcut => "快捷方式",
            Category::InstallDir => "安装目录",
            Category::Folder => "文件夹",
            Category::Document => "文档",
            Category::Image => "图片",
            Category::Video => "视频",
            Category::Audio => "音频",
            Category::Archive => "压缩包",
            Category::Code => "代码",
            Category::Executable => "程序",
            Category::Other => "其他",
        }
    }

    /// 结果行图标位的底色（原 WPF 版用分类色块+首字，这里沿用同一套色板）。
    pub fn tile_color(self) -> (u8, u8, u8) {
        match self {
            Category::App => (37, 99, 235),
            Category::Shortcut => (124, 58, 237),
            Category::InstallDir => (8, 145, 178),
            Category::Folder => (217, 164, 32),
            Category::Document => (59, 130, 246),
            Category::Image => (22, 163, 74),
            Category::Video => (220, 38, 38),
            Category::Audio => (234, 88, 12),
            Category::Archive => (120, 113, 108),
            Category::Code => (13, 148, 136),
            Category::Executable => (71, 85, 105),
            Category::Other => (100, 116, 139),
        }
    }
}

/// 按扩展名把文件分到类别（与 C# 版 CategoryClassifier 的扩展名表逐项一致）。
/// 文件夹与 .lnk/.exe 的来源判定不在这里：调用方根据 is_dir 与来源决定。
pub fn classify_by_extension(name: &str) -> Category {
    let ext = match name.rfind('.') {
        Some(p) if p > 0 || !name.starts_with('.') => &name[p..],
        _ => return Category::Other,
    };
    let e = ext.to_ascii_lowercase();
    match e.as_str() {
        ".jpg" | ".jpeg" | ".png" | ".gif" | ".webp" | ".bmp" | ".svg" | ".ico" | ".tif"
        | ".tiff" | ".heic" | ".heif" | ".avif" | ".raw" | ".psd" | ".ai" => Category::Image,
        ".mp4" | ".mkv" | ".avi" | ".mov" | ".wmv" | ".flv" | ".webm" | ".m4v" | ".mpg"
        | ".mpeg" | ".3gp" | ".ts" | ".rm" | ".rmvb" | ".vob" | ".swf" => Category::Video,
        ".mp3" | ".flac" | ".wav" | ".ogg" | ".m4a" | ".aac" | ".wma" | ".ape" | ".opus"
        | ".aiff" | ".mid" | ".midi" => Category::Audio,
        ".doc" | ".docx" | ".pdf" | ".txt" | ".md" | ".markdown" | ".rtf" | ".odt" | ".xls"
        | ".xlsx" | ".csv" | ".tsv" | ".ods" | ".ppt" | ".pptx" | ".odp" | ".epub" | ".mobi"
        | ".chm" | ".wps" | ".et" | ".dps" => Category::Document,
        ".zip" | ".rar" | ".7z" | ".tar" | ".gz" | ".tgz" | ".bz2" | ".xz" | ".iso" | ".img"
        | ".cab" | ".jar" | ".war" => Category::Archive,
        ".cs" | ".py" | ".js" | ".ts" | ".jsx" | ".tsx" | ".java" | ".cpp" | ".cc" | ".c"
        | ".h" | ".hpp" | ".go" | ".rs" | ".php" | ".rb" | ".swift" | ".kt" | ".json" | ".xml"
        | ".yml" | ".yaml" | ".toml" | ".ini" | ".cfg" | ".html" | ".htm" | ".css" | ".scss"
        | ".vue" | ".sql" | ".sh" | ".bat" | ".ps1" | ".sln" | ".csproj" | ".vcxproj" => {
            Category::Code
        }
        ".lnk" => Category::Shortcut,
        ".exe" => Category::Executable,
        _ => Category::Other,
    }
}
