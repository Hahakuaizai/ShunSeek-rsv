//! 瞬觅 ShunSeek 搜索引擎核心（Rust 版）。
//! 引擎与 UI 完全解耦，可在任何平台单独编译和跑基准——与原 .NET 版同一原则。
//! 移植对照：Category↔EntryCategory、pinyin↔PinyinInitials、store↔IndexStore、
//! builder↔IndexBuilder、search↔SearchEngine、usage↔UsageStore、sources↔Sources。

pub mod builder;
pub mod category;
pub mod delta;
pub mod filetype;
pub mod pinyin;
mod pinyin_full_table;
mod pinyin_table;
pub mod search;
pub mod sources;
pub mod store;
pub mod usage;

pub use builder::build_index;
pub use category::{classify_by_extension, Category};
pub use delta::{DeltaIndex, Tombstones};
pub use filetype::Importance;
pub use search::{search_merged, ScopeFilter, SearchEngine, SearchHit};
pub use store::{IndexStore, RawEntry};
pub use usage::UsageStore;
