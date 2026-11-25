//! kernel32/user32 的最小 FFI（对应 C# 版 NativeMethods 的 P/Invoke）。
//! 数值都对照 winioctl.h：FSCTL_* 用 CTL_CODE(FILE_DEVICE_FILE_SYSTEM=9, func,
//! METHOD_NEITHER=3, FILE_ANY_ACCESS=0) 算出，与 C# 版常量逐项相同。

#![allow(non_snake_case)]

use std::ffi::c_void;

pub const FSCTL_ENUM_USN_DATA: u32 = 0x000900B3; // func 44
pub const FSCTL_QUERY_USN_JOURNAL: u32 = 0x000900F4; // func 61
pub const FSCTL_READ_USN_JOURNAL: u32 = 0x000900BB; // func 46

pub const GENERIC_READ: u32 = 0x80000000;
pub const FILE_SHARE_READ: u32 = 0x00000001;
pub const FILE_SHARE_WRITE: u32 = 0x00000002;
pub const OPEN_EXISTING: u32 = 3;
pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x00000010;
pub const ERROR_HANDLE_EOF: i32 = 38;
pub const FILE_REF_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

pub const USN_REASON_FILE_CREATE: u32 = 0x00000100;
pub const USN_REASON_FILE_DELETE: u32 = 0x00000200;
pub const USN_REASON_RENAME_OLD_NAME: u32 = 0x00001000;
pub const USN_REASON_RENAME_NEW_NAME: u32 = 0x00002000;

pub type HANDLE = *mut c_void;
pub const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;

extern "system" {
    pub fn CreateFileW(
        file_name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *mut c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: HANDLE,
    ) -> HANDLE;
    pub fn DeviceIoControl(
        device: HANDLE,
        control_code: u32,
        in_buffer: *const c_void,
        in_size: u32,
        out_buffer: *mut c_void,
        out_size: u32,
        bytes_returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    pub fn CloseHandle(h: HANDLE) -> i32;
    pub fn GetLastError() -> i32;
}

/// 卷句柄 RAII（对应 C# 版 VolumeHandle）：打开 \\.\X: 卷本身以便发 FSCTL。
pub struct VolumeHandle(pub HANDLE);

impl VolumeHandle {
    pub fn open_volume(drive: char) -> std::io::Result<VolumeHandle> {
        let path: Vec<u16> = format!(r"\\.\{}:", drive)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(VolumeHandle(h))
        }
    }
}

impl Drop for VolumeHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// 从 USN_RECORD_V2 缓冲里解出一条记录（60 字节定长头 + UTF-16LE 文件名）。
/// 返回 (记录总长, 名字, file_ref, parent_ref, 属性, reason, usn)。
pub fn parse_usn_record(buf: &[u8], offset: usize) -> Option<(usize, String, u64, u64, u32, u32, i64)> {
    if offset + 60 > buf.len() {
        return None;
    }
    let rd32 = |o: usize| u32::from_le_bytes(buf[o..o + 4].try_into().unwrap());
    let rd64 = |o: usize| u64::from_le_bytes(buf[o..o + 8].try_into().unwrap());
    let rd16 = |o: usize| u16::from_le_bytes(buf[o..o + 2].try_into().unwrap());
    let record_len = rd32(offset) as usize;
    if record_len < 60 {
        return None;
    }
    let name_len = rd16(offset + 56) as usize;
    let name_off = offset + rd16(offset + 58) as usize;
    if name_off + name_len > buf.len() {
        return None;
    }
    let units: Vec<u16> = buf[name_off..name_off + name_len]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let name = String::from_utf16_lossy(&units);
    Some((
        record_len,
        name,
        rd64(offset + 8) & FILE_REF_MASK,
        rd64(offset + 16) & FILE_REF_MASK,
        rd32(offset + 52),
        rd32(offset + 40),
        rd64(offset + 24) as i64,
    ))
}
