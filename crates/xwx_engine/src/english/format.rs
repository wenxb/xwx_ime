//! `english.bin` 的字节级校验与读取原语。
//!
//! 格式定义见 `docs/english-input/01-data.md` 第 4 节。这里只做三件事：
//! 边界安全的定长读取、头部解析与区段校验、词面合法性检查。
//! **不放任何排序或召回逻辑**（那在 `dict.rs`）。

use thiserror::Error;

/// 文件魔数（8 字节，含尾部零字节）。
pub(super) const MAGIC: [u8; 8] = *b"XWXENG1\0";
/// 格式版本。
pub(super) const VERSION: u32 = 1;
/// 头部长度。
pub(super) const HEADER_LEN: usize = 64;
/// 单条 word 记录长度。
pub(super) const WORD_RECORD_LEN: usize = 16;
/// 单条 prefix 记录长度。
pub(super) const PREFIX_RECORD_LEN: usize = 16;
/// 防御上限：文件最大 256 MiB。
pub(super) const MAX_FILE_LEN: u64 = 256 * 1024 * 1024;
/// 防御上限：词条数。
pub(super) const MAX_WORDS: u32 = 200_000;
/// 防御上限：前缀数。
pub(super) const MAX_PREFIXES: u32 = 4_000_000;
/// 单个前缀最多缓存的补全条数（格式 v1 的排序约定）。
pub(super) const PREFIX_TOP_LIMIT: usize = 32;
/// 静态补全分每个多余字母的扣分（百分之一 Zipf）。
pub(super) const PREFIX_SCORE_STEP: i32 = 15;
/// `zipf_centi` 合法上界。
pub(super) const MAX_ZIPF_CENTI: u16 = 900;
/// 词条键/显示串的长度区间（字节，ASCII 字母）。
pub(super) const MIN_WORD_LEN: usize = 1;
pub(super) const MAX_WORD_LEN: usize = 48;

/// 英文词库层错误。
#[derive(Debug, Error)]
pub enum EnglishError {
    /// 打开/映射文件失败。
    #[error("英文词库 I/O 错误: {0}")]
    Io(#[from] std::io::Error),
    /// 文件内容不合法（魔数/版本/长度/区段/记录）。
    #[error("英文词库损坏: {0}")]
    Corrupt(String),
}

impl EnglishError {
    /// 便捷构造：静态消息。
    pub(super) fn corrupt(msg: &'static str) -> Self {
        EnglishError::Corrupt(msg.to_string())
    }
}

/// 边界安全的 `u16` 读取（越界返回 `Corrupt` 而不是 panic）。
pub(super) fn read_u16(bytes: &[u8], off: usize) -> Result<u16, EnglishError> {
    let b = bytes
        .get(off..off + 2)
        .ok_or_else(|| EnglishError::corrupt("读取 u16 越界"))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

/// 边界安全的 `u32` 读取。
pub(super) fn read_u32(bytes: &[u8], off: usize) -> Result<u32, EnglishError> {
    let b = bytes
        .get(off..off + 4)
        .ok_or_else(|| EnglishError::corrupt("读取 u32 越界"))?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// 边界安全的 `u64` 读取。
pub(super) fn read_u64(bytes: &[u8], off: usize) -> Result<u64, EnglishError> {
    let b = bytes
        .get(off..off + 8)
        .ok_or_else(|| EnglishError::corrupt("读取 u64 越界"))?;
    Ok(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
}

/// `u64` → `usize`：不可表示即报错，**不做 `as` 截断**。
pub(super) fn to_usize(v: u64, what: &'static str) -> Result<usize, EnglishError> {
    usize::try_from(v).map_err(|_| EnglishError::Corrupt(format!("{what} 超出 usize 表示范围")))
}

/// 解析出的 64 字节头部（各字段已做过基本校验）。
#[derive(Debug, Clone, Copy)]
pub(super) struct Header {
    pub word_count: u32,
    pub prefix_count: u32,
    pub words_offset: usize,
    pub prefixes_offset: usize,
    pub ids_offset: usize,
    pub strings_offset: usize,
    pub file_len: usize,
}

/// 解析并校验头部，返回各字段 + 已校验的区段边界。
///
/// 覆盖：魔数、版本、头长、`file_len` 与实际长度一致、区段次序、
/// 各段起点 + 容量不越界、ids 区长度是 4 的倍数、资源上限。
/// 溢出一律走 `checked_*`。
pub(super) fn parse_header(bytes: &[u8]) -> Result<Header, EnglishError> {
    if bytes.len() < HEADER_LEN {
        return Err(EnglishError::corrupt("文件过短，不是完整 english.bin"));
    }
    if bytes[0..8] != MAGIC {
        return Err(EnglishError::corrupt("魔数不匹配，不是 english.bin"));
    }
    if read_u32(bytes, 8)? != VERSION {
        return Err(EnglishError::corrupt("english.bin 版本不兼容"));
    }
    if read_u32(bytes, 12)? as usize != HEADER_LEN {
        return Err(EnglishError::corrupt("header_len 必须是 64"));
    }
    let word_count = read_u32(bytes, 16)?;
    let prefix_count = read_u32(bytes, 20)?;
    if word_count > MAX_WORDS {
        return Err(EnglishError::corrupt("word_count 超过防御上限"));
    }
    if prefix_count > MAX_PREFIXES {
        return Err(EnglishError::corrupt("prefix_count 超过防御上限"));
    }
    let words_offset = to_usize(read_u64(bytes, 24)?, "words_offset")?;
    let prefixes_offset = to_usize(read_u64(bytes, 32)?, "prefixes_offset")?;
    let ids_offset = to_usize(read_u64(bytes, 40)?, "ids_offset")?;
    let strings_offset = to_usize(read_u64(bytes, 48)?, "strings_offset")?;
    let file_len = to_usize(read_u64(bytes, 56)?, "file_len")?;

    if file_len != bytes.len() {
        return Err(EnglishError::corrupt("file_len 与实际文件长度不一致"));
    }
    if file_len as u64 > MAX_FILE_LEN {
        return Err(EnglishError::corrupt("文件超过 256 MiB 防御上限"));
    }
    // 区段次序：header ≤ words ≤ prefixes ≤ ids ≤ strings ≤ 文件尾
    if !(HEADER_LEN <= words_offset
        && words_offset <= prefixes_offset
        && prefixes_offset <= ids_offset
        && ids_offset <= strings_offset
        && strings_offset <= file_len)
    {
        return Err(EnglishError::corrupt(
            "区段顺序不合法: words/prefixes/ids/strings 起点必须递增且落在文件内",
        ));
    }
    if words_offset != HEADER_LEN {
        return Err(EnglishError::corrupt("words 区必须紧接头部"));
    }
    let words_len = (word_count as usize)
        .checked_mul(WORD_RECORD_LEN)
        .ok_or_else(|| EnglishError::corrupt("words 区长度溢出"))?;
    let prefixes_len = (prefix_count as usize)
        .checked_mul(PREFIX_RECORD_LEN)
        .ok_or_else(|| EnglishError::corrupt("prefixes 区长度溢出"))?;
    if words_offset
        .checked_add(words_len)
        .is_none_or(|end| end > prefixes_offset)
    {
        return Err(EnglishError::corrupt("words 区越界"));
    }
    if prefixes_offset
        .checked_add(prefixes_len)
        .is_none_or(|end| end > ids_offset)
    {
        return Err(EnglishError::corrupt("prefixes 区越界"));
    }
    if (strings_offset - ids_offset) % 4 != 0 {
        return Err(EnglishError::corrupt("ids 区长度不是 4 的倍数"));
    }
    Ok(Header {
        word_count,
        prefix_count,
        words_offset,
        prefixes_offset,
        ids_offset,
        strings_offset,
        file_len,
    })
}

/// 词面（key 与 display）是否合法：全 ASCII 字母、长度 1..=48。
pub(super) fn valid_word_text(s: &str) -> bool {
    let n = s.len();
    (MIN_WORD_LEN..=MAX_WORD_LEN).contains(&n) && s.bytes().all(|b| b.is_ascii_alphabetic())
}

/// key 是否是合法的小写英文键（构建端与用户索引共用同一口径）。
pub fn is_english_key(s: &str) -> bool {
    valid_word_text(s) && s.bytes().all(|b| b.is_ascii_lowercase())
}
