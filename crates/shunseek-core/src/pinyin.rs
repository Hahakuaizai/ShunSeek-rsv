//! 汉字 → 拼音首字母，与 C# 版 PinyinInitials 同算法：
//! 「GB2312 一级字边界字表」法。一级汉字（3755 个）按拼音排序，每个声母段的
//! 第一个字是边界字。C# 版靠系统 GB2312 编码器把汉字转成双字节码；Rust 标准库
//! 没有 GB2312 编码，所以构建期用 Python 的 gb2312 编解码器生成了
//! pinyin_table.rs（码点 → GB2312 码，恰好 3755 项，与 C# 注释里的数字对上），
//! 运行时两次二分查找，零依赖、零分配。
//!
//! 已知局限（与 C# 版相同）：多音字只取一级字表段归属；二级生僻字跳过；
//! 英文/数字原样保留并转小写，其余符号丢弃。"王者荣耀" → "wzry"（有单测）。

use crate::pinyin_table::GB2312_LEVEL1;

// 各声母段边界字的 GB2312 码，与 LETTERS 一一对应，升序（与 C# 版 SecValues 相同）。
static SEC_VALUES: [u16; 23] = [
    0xB0A1, 0xB0C5, 0xB2C1, 0xB4EE, 0xB6EA, 0xB7A2, 0xB8C1, 0xB9FE, 0xBBF7, 0xBFA6, 0xC0AC, 0xC2E8,
    0xC4C3, 0xC5B6, 0xC5BE, 0xC6DA, 0xC8BB, 0xC8F6, 0xCBFA, 0xCDDA, 0xCEF4, 0xD1B9, 0xD4D1,
];
static LETTERS: [u8; 23] = *b"abcdefghjklmnopqrstwxyz";

/// 单个汉字的拼音首字母；非一级汉字返回 None。
pub fn initial_of(ch: char) -> Option<u8> {
    let cp = ch as u32;
    if !(0x4E00..0x9FA6).contains(&cp) {
        return None;
    }
    let idx = GB2312_LEVEL1.binary_search_by_key(&(cp as u16), |e| e.0).ok()?;
    let code = GB2312_LEVEL1[idx].1;
    // 找最后一个 ≤ code 的边界
    let seg = match SEC_VALUES.binary_search(&code) {
        Ok(i) => i,
        Err(i) => i.checked_sub(1)?,
    };
    Some(LETTERS[seg])
}

/// 把任意显示名转成拼音首字母串（全小写，只含 a-z0-9），直接写进给定的字节缓冲。
pub fn initials_into(text: &str, out: &mut Vec<u8>) {
    for ch in text.chars() {
        if ch.is_ascii() {
            if ch.is_ascii_alphanumeric() {
                out.push(ch.to_ascii_lowercase() as u8);
            }
        } else if let Some(i) = initial_of(ch) {
            out.push(i);
        }
    }
}

/// 便捷版：返回 String（构建索引时用 initials_into 热路径即可）。
pub fn initials(text: &str) -> String {
    let mut v = Vec::with_capacity(text.len());
    initials_into(text, &mut v);
    // initials 只含 ASCII，unwrap 安全
    String::from_utf8(v).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wzry() {
        assert_eq!(initials("王者荣耀"), "wzry");
    }

    #[test]
    fn mixed() {
        assert_eq!(initials("QQ音乐"), "qqyl"); // 乐在 GB2312 一级字表归 l 段（lè），与 C# 实现的实际行为一致（其注释写的 qqyy 是笔误）
        assert_eq!(initials("Visual Studio Code"), "visualstudiocode");
        assert_eq!(initials("报告2024"), "bg2024");
    }
}
