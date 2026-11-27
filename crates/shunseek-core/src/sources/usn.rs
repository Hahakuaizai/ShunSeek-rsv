//! USN 日志增量监听（对应 C# 版 UsnWatcher）：FSCTL_READ_USN_JOURNAL 轮询，
//! 只关心 创建/删除/改名新旧名 四类原因。调用方拿到 UsnChange 后自行决定如何
//! 更新索引（重建或增量维护，与 C# 版的定位相同：watcher 只报变化）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::win32::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UsnChangeKind {
    Created,
    Deleted,
    RenamedOldName,
    RenamedNewName,
    Other,
}

#[derive(Clone, Debug)]
pub struct UsnChange {
    pub kind: UsnChangeKind,
    pub name: String,
    pub file_ref: u64,
    pub parent_ref: u64,
    pub is_dir: bool,
    pub reason: u32,
}

pub struct UsnWatcher {
    drive: char,
    stop: Arc<AtomicBool>,
    /// 起始 USN（v0.1.8）：建索引时先记下该卷的 NextUsn，监听从
    /// 那一刻起的变化，不漏建索引期间发生的增删改。
    start_usn: Option<i64>,
}

/// 查某卷 USN 日志的当前 NextUsn（v0.1.8，增量监听的起点）；
/// 非 NTFS / 无权限 / 任何 FFI 失败都返回 None（调用方据此判定
/// 该卷走不了增量，别崩）。
pub fn query_next_usn(drive: char) -> Option<i64> {
    let handle = VolumeHandle::open_volume(drive).ok()?;
    let mut journal_buf = [0u8; 56];
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            handle.0,
            FSCTL_QUERY_USN_JOURNAL,
            std::ptr::null(),
            0,
            journal_buf.as_mut_ptr() as *mut _,
            journal_buf.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return None;
    }
    Some(i64::from_le_bytes(journal_buf[16..24].try_into().ok()?))
}

impl UsnWatcher {
    pub fn new(drive: char) -> UsnWatcher {
        UsnWatcher {
            drive,
            stop: Arc::new(AtomicBool::new(false)),
            start_usn: None,
        }
    }

    /// 带起始 USN 的监听器（v0.1.8）：start 为建索引时 query_next_usn
    /// 记下的值；None 时退回原行为（从日志当前末尾开始）。
    pub fn with_start(drive: char, start_usn: Option<i64>) -> UsnWatcher {
        UsnWatcher {
            drive,
            stop: Arc::new(AtomicBool::new(false)),
            start_usn,
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// 阻塞式运行循环（放到单独线程里跑）；on_change 每条变化回调一次。
    pub fn run<F: FnMut(UsnChange)>(&self, mut on_change: F) -> std::io::Result<()> {
        let handle = VolumeHandle::open_volume(self.drive)?;
        // FSCTL_QUERY_USN_JOURNAL → USN_JOURNAL_DATA_V0（56 字节）
        let mut journal_buf = [0u8; 56];
        let mut returned: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                handle.0,
                FSCTL_QUERY_USN_JOURNAL,
                std::ptr::null(),
                0,
                journal_buf.as_mut_ptr() as *mut _,
                journal_buf.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let journal_id = u64::from_le_bytes(journal_buf[0..8].try_into().unwrap());
        let mut next_usn = self
            .start_usn
            .unwrap_or_else(|| i64::from_le_bytes(journal_buf[16..24].try_into().unwrap()));

        let reason_mask = USN_REASON_FILE_CREATE
            | USN_REASON_FILE_DELETE
            | USN_REASON_RENAME_OLD_NAME
            | USN_REASON_RENAME_NEW_NAME;
        let mut out_buffer = vec![0u8; 256 * 1024];
        while !self.stop.load(Ordering::Relaxed) {
            // READ_USN_JOURNAL_DATA_V0（40 字节）
            let mut in_buffer = [0u8; 40];
            in_buffer[0..8].copy_from_slice(&next_usn.to_le_bytes());
            in_buffer[8..12].copy_from_slice(&reason_mask.to_le_bytes());
            // ReturnOnlyOnClose = 0（12..16）
            in_buffer[16..24].copy_from_slice(&0u64.to_le_bytes()); // Timeout = 0 (不阻塞11天)
            in_buffer[24..32].copy_from_slice(&0u64.to_le_bytes()); // BytesToWaitFor = 0
            in_buffer[32..40].copy_from_slice(&journal_id.to_le_bytes());
            let mut returned: u32 = 0;
            let ok = unsafe {
                DeviceIoControl(
                    handle.0,
                    FSCTL_READ_USN_JOURNAL,
                    in_buffer.as_ptr() as *const _,
                    in_buffer.len() as u32,
                    out_buffer.as_mut_ptr() as *mut _,
                    out_buffer.len() as u32,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            if returned <= 8 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
            next_usn = i64::from_le_bytes(out_buffer[0..8].try_into().unwrap());
            let limit = returned as usize;
            let mut offset = 8usize;
            while offset + 60 <= limit {
                match parse_usn_record(&out_buffer[..limit], offset) {
                    Some((len, name, file_ref, parent_ref, attrs, reason, _usn)) => {
                        on_change(UsnChange {
                            kind: classify(reason),
                            name,
                            file_ref,
                            parent_ref,
                            is_dir: attrs & FILE_ATTRIBUTE_DIRECTORY != 0,
                            reason,
                        });
                        offset += len;
                    }
                    None => break,
                }
            }
        }
        Ok(())
    }
}

fn classify(reason: u32) -> UsnChangeKind {
    // 同一句柄未关闭前 NTFS 会累积 reason 位：先判删除/改名，再判创建
    if reason & USN_REASON_FILE_DELETE != 0 {
        UsnChangeKind::Deleted
    } else if reason & USN_REASON_RENAME_NEW_NAME != 0 {
        UsnChangeKind::RenamedNewName
    } else if reason & USN_REASON_RENAME_OLD_NAME != 0 {
        UsnChangeKind::RenamedOldName
    } else if reason & USN_REASON_FILE_CREATE != 0 {
        UsnChangeKind::Created
    } else {
        UsnChangeKind::Other
    }
}
