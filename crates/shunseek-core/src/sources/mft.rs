//! NTFS MFT 直读（对应 C# 版 MftEnumerator + MftIndexSource）：
//! FSCTL_ENUM_USN_DATA 以 1MB 批读枚举全卷记录，再沿父引用链拼全路径。
//! 跳过引用号 <16 的 $ 系统元文件（与 C# 版一致）。

use std::collections::HashMap;

use super::win32::*;
use crate::category::{classify_by_extension, Category};
use crate::store::RawEntry;

#[derive(Clone, Debug)]
pub struct MftRecord {
    pub name: String,
    pub file_ref: u64,
    pub parent_ref: u64,
    pub is_dir: bool,
    pub file_attributes: u32,
}

const BUFFER_SIZE: usize = 1024 * 1024;

pub fn enumerate_volume(drive: char) -> std::io::Result<Vec<MftRecord>> {
    let handle = VolumeHandle::open_volume(drive)?;
    let mut out = Vec::new();
    let mut buffer = vec![0u8; BUFFER_SIZE];
    // MFT_ENUM_DATA_V0：StartFileReferenceNumber u64 + LowUsn i64 + HighUsn i64
    let mut in_buffer = [0u8; 24];
    in_buffer[16..24].copy_from_slice(&i64::MAX.to_le_bytes());
    let mut start_ref: u64 = 0;
    loop {
        in_buffer[0..8].copy_from_slice(&start_ref.to_le_bytes());
        let mut returned: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                handle.0,
                FSCTL_ENUM_USN_DATA,
                in_buffer.as_ptr() as *const _,
                in_buffer.len() as u32,
                buffer.as_mut_ptr() as *mut _,
                buffer.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            if err == ERROR_HANDLE_EOF {
                break;
            }
            return Err(std::io::Error::from_raw_os_error(err));
        }
        if returned <= 8 {
            break;
        }
        start_ref = u64::from_le_bytes(buffer[0..8].try_into().unwrap());
        let mut offset = 8usize;
        let limit = returned as usize;
        while offset + 60 <= limit {
            match parse_usn_record(&buffer[..limit], offset) {
                Some((len, name, file_ref, parent_ref, attrs, _reason, _usn)) => {
                    out.push(MftRecord {
                        name,
                        file_ref,
                        parent_ref,
                        is_dir: attrs & FILE_ATTRIBUTE_DIRECTORY != 0,
                        file_attributes: attrs,
                    });
                    offset += len;
                }
                None => break,
            }
        }
        if start_ref == 0 {
            break;
        }
    }
    Ok(out)
}

/// 一个卷 → RawEntry 列表（路径已拼好，与 C# 版 MftIndexSource.Load 一致）。
pub fn load_volume(drive: char) -> std::io::Result<Vec<RawEntry>> {
    let records = enumerate_volume(drive)?;
    let mut by_ref: HashMap<u64, usize> = HashMap::with_capacity(records.len());
    for (i, r) in records.iter().enumerate() {
        by_ref.insert(r.file_ref, i);
    }
    let root = format!("{drive}:\\");
    let mut out = Vec::with_capacity(records.len());
    for (i, r) in records.iter().enumerate() {
        if r.name.starts_with('$') && r.file_ref < 16 {
            continue;
        }
        let path = resolve_path(i, &records, &by_ref, &root);
        let category = if r.is_dir {
            Category::Folder
        } else {
            classify_by_extension(&r.name)
        };
        out.push(RawEntry {
            name: r.name.clone(),
            full_path: Some(path),
            category,
            file_ref: r.file_ref,
            parent_ref: r.parent_ref,
            is_dir: r.is_dir,
        });
    }
    Ok(out)
}

fn resolve_path(
    index: usize,
    records: &[MftRecord],
    by_ref: &HashMap<u64, usize>,
    root: &str,
) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let mut cur = index;
    for _ in 0..64 {
        let r = &records[cur];
        if r.file_ref == 5 {
            break; // 到卷根，根名不进路径
        }
        parts.push(&r.name);
        if r.parent_ref == r.file_ref {
            break;
        }
        match by_ref.get(&r.parent_ref) {
            Some(&p) => cur = p,
            None => break,
        }
    }
    parts.reverse();
    if parts.is_empty() {
        root.to_string()
    } else {
        format!("{}{}", root, parts.join("\\"))
    }
}
