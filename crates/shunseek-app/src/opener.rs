//! 打开结果条目（对应 C# 版 MainWindow.OpenSelected）：
//!  - 普通打开：Windows 走 ShellExecuteW（shell32，动词 open），
//!    与资源管理器双击等价；非 Windows 用 xdg-open（Linux）/ open（macOS）。
//!  - Ctrl+Enter「打开所在文件夹」：Windows 用 explorer.exe /select,<路径>
//!    定位到文件本身；非 Windows 退化为打开父目录。
//! 打开失败只记日志不弹窗崩溃（演示数据路径是假的，打不开属预期）。

#[cfg(not(windows))]
use std::path::Path;

/// path 为空（App 类条目只在注册表里有名字、没有可执行路径时）退回用 name 兜底。
/// TODO：这类条目正解是拼 shell:AppsFolder\<AUMID> 再 ShellExecute，
/// 需要 appscan 侧把 AUMID 带进索引（core 暂无此字段），先按名字尽力打开。
pub fn open(path: &str, name: &str, locate_in_folder: bool) {
    let target = if path.is_empty() { name } else { path };
    if target.is_empty() {
        return;
    }
    if locate_in_folder {
        locate(target);
    } else {
        launch(target);
    }
}

#[cfg(windows)]
fn launch(target: &str) {
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: isize,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show_cmd: i32,
        ) -> isize;
    }
    const SW_SHOWNORMAL: i32 = 1;
    let op = wide("open");
    let file = wide(target);
    let r = unsafe {
        ShellExecuteW(
            0,
            op.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if r <= 32 {
        eprintln!("ShunSeek: ShellExecute 打开失败 ({r}): {target}");
    }
}

#[cfg(not(windows))]
fn launch(target: &str) {
    #[cfg(target_os = "macos")]
    let prog = "open";
    #[cfg(not(target_os = "macos"))]
    let prog = "xdg-open";
    if let Err(e) = std::process::Command::new(prog).arg(target).spawn() {
        eprintln!("ShunSeek: {prog} 打开失败: {target} ({e})");
    }
}

#[cfg(windows)]
fn locate(target: &str) {
    use std::os::windows::process::CommandExt;
    // explorer /select,"<路径>"：用 raw_arg 避免 Command::arg 自动给整段加引号导致空格路径失效
    let raw = format!("/select,\"{target}\"");
    if let Err(e) = std::process::Command::new("explorer.exe").raw_arg(raw).spawn() {
        eprintln!("ShunSeek: explorer 定位失败: {target} ({e})");
    }
}

#[cfg(not(windows))]
fn locate(target: &str) {
    let parent = Path::new(target)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| target.to_string());
    launch(&parent);
}
