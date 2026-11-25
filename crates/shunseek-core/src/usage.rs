//! 使用频次存储（对应 C# 版 UsageStore）：用户每打开一个结果记一次，
//! 搜索排序时给常打开的条目加分。加分规则与 C# 版一致：
//! min(次数, 50) × 0.2 +（7 天内用过 ? 2 : 0）。
//!
//! 与 C# 版的差别只有落盘格式：C# 用 JSON，我们用一个紧凑二进制
//! （"SSKUSG01" + 条数 + [路径长度, 路径字节, 次数, 上次使用 unix 秒]），
//! 加载失败/文件损坏一律当空表，绝不拖垮主流程——这点和 C# 版相同。

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy)]
struct Info {
    count: u32,
    last_used_unix: u64,
}

pub struct UsageStore {
    map: HashMap<String, Info>,
    file_path: Option<PathBuf>,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl UsageStore {
    pub fn load(file_path: Option<PathBuf>) -> UsageStore {
        let mut store = UsageStore {
            map: HashMap::new(),
            file_path: file_path.clone(),
        };
        if let Some(p) = &file_path {
            if let Ok(data) = std::fs::read(p) {
                store.parse(&data);
            }
        }
        store
    }

    fn parse(&mut self, data: &[u8]) {
        if data.len() < 12 || &data[0..8] != b"SSKUSG01" {
            return;
        }
        let count = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
        let mut cur = 12usize;
        for _ in 0..count {
            if cur + 4 > data.len() {
                return;
            }
            let len = u32::from_le_bytes(data[cur..cur + 4].try_into().unwrap()) as usize;
            cur += 4;
            if cur + len + 12 > data.len() {
                return;
            }
            let path = match std::str::from_utf8(&data[cur..cur + len]) {
                Ok(s) => s.to_string(),
                Err(_) => return,
            };
            cur += len;
            let cnt = u32::from_le_bytes(data[cur..cur + 4].try_into().unwrap());
            let last = u64::from_le_bytes(data[cur + 4..cur + 12].try_into().unwrap());
            cur += 12;
            self.map.insert(
                path.to_lowercase(),
                Info {
                    count: cnt,
                    last_used_unix: last,
                },
            );
        }
    }

    /// 记录一次打开。落盘由调用方在空闲时调 save（避免每次打开都写盘）。
    pub fn record_use(&mut self, path: &str) {
        let key = path.to_lowercase();
        let info = self.map.entry(key).or_insert(Info {
            count: 0,
            last_used_unix: 0,
        });
        info.count += 1;
        info.last_used_unix = now_unix();
    }

    /// 排序加分（路径大小写不敏感，与 C# 版 OrdinalIgnoreCase 一致）。
    pub fn boost(&self, path: &str) -> f32 {
        let key = path.to_lowercase();
        match self.map.get(&key) {
            None => 0.0,
            Some(info) => {
                let mut b = (info.count.min(50) as f32) * 0.2;
                if now_unix().saturating_sub(info.last_used_unix) < 7 * 24 * 3600 {
                    b += 2.0;
                }
                b
            }
        }
    }

    pub fn save(&self) -> io::Result<()> {
        let Some(p) = &self.file_path else {
            return Ok(());
        };
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = Vec::with_capacity(12 + self.map.len() * 32);
        out.extend_from_slice(b"SSKUSG01");
        out.extend_from_slice(&(self.map.len() as u32).to_le_bytes());
        for (path, info) in &self.map {
            out.extend_from_slice(&(path.len() as u32).to_le_bytes());
            out.extend_from_slice(path.as_bytes());
            out.extend_from_slice(&info.count.to_le_bytes());
            out.extend_from_slice(&info.last_used_unix.to_le_bytes());
        }
        std::fs::write(p, out)
    }
}
