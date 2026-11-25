//! exe 图标嵌入（仅 Windows 目标）：把 assets/shunseek.ico 写进 exe
//! 资源段，这样 exe 文件本身、任务栏、资源管理器里都是正式图标。
//!
//! 原则：图标是锦上添花，任何失败只发 cargo:warning，绝不让构建失败。
//!  - 非 Windows 目标：直接返回（build.rs 跑在主机上，判断目标必须读
//!    TARGET 环境变量，不能用 cfg!(windows)——那是主机平台）。
//!  - Linux 主机 + msvc 目标（本机 cargo check 的组合）：没有 rc.exe，
//!    winresource 必然失败，提前跳过并说明；真打包在 Windows 主机或
//!    gnu + windres 路径上完成。
//!  - Windows 主机 / gnu 目标（windres）：正常嵌入。

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/shunseek.ico");
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }
    let host = std::env::var("HOST").unwrap_or_default();
    if !host.contains("windows") && target.contains("msvc") {
        println!(
            "cargo:warning=ShunSeek: 非 Windows 主机 + msvc 目标无 rc.exe，跳过 exe 图标嵌入（打包时处理）"
        );
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/shunseek.ico");
    if let Err(e) = res.compile() {
        println!("cargo:warning=ShunSeek: exe 图标嵌入失败（{e}），继续构建");
    }
}
