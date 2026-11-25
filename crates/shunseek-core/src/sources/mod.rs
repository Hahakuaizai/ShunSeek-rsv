//! 数据源（对应 C# 版 Core/Sources）：统一产出 RawEntry，IndexBuilder 只管汇总。
//!  - walker：兜底目录遍历（全平台）
//!  - mft / usn：NTFS 直读与增量监听（仅 Windows，kernel32 FFI 手写，无重依赖）
//!  - appscan：开始菜单/桌面快捷方式 + 注册表 Uninstall（仅 Windows）

pub mod walker;

#[cfg(windows)]
pub mod win32;
#[cfg(windows)]
pub mod mft;
#[cfg(windows)]
pub mod usn;
#[cfg(windows)]
pub mod appscan;
