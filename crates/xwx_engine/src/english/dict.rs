//! `english.bin` 的只读访问：精确查询与前缀 Top-32。
//!
//! 映射本身很便宜，**完整校验是线性工作**，在 `open` 时一次性做完：
//! 校验失败返回错误，装配层只关闭英文并告警，绝不 panic、不牵连中文。

use std::ops::Deref;
use std::path::Path;

use memmap2::Mmap;

use super::format::{
    is_english_key, parse_header, read_u16, read_u32, valid_word_text, EnglishError, Header,
    MAX_ZIPF_CENTI, PREFIX_RECORD_LEN, PREFIX_SCORE_STEP, PREFIX_TOP_LIMIT, WORD_RECORD_LEN,
};

/// 底层字节的来源：生产是 mmap，测试/内存构造是自有 `Vec<u8>`。
///
/// 两条路走**同一个** [`EnglishDict::validate`]，不是两套解析。
enum Backing {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

impl Deref for Backing {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Backing::Mapped(m) => m,
            Backing::Owned(v) => v,
        }
    }
}

/// 词库里一条词的借用视图（不复制词面）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishEntryRef<'a> {
    /// 完整单词的小写键。
    pub key: &'a str,
    /// 展示形式（规范大小写，如 `Python`）。
    pub display: &'a str,
    /// 百分之一 Zipf 单位的静态频率。
    pub zipf_centi: u16,
    /// 词在字典序中的编号。
    pub word_id: u32,
}

/// 静态补全分（百分之一 Zipf 单位）：
/// `zipf_centi - 15 × (word.len - prefix.len)`。
///
/// 与构建端 [`PREFIX_SCORE_STEP`] 同源；改这里必须重建数据并更新兼容版本。
pub fn static_score_centi(zipf_centi: u16, word_len: usize, prefix_len: usize) -> i32 {
    zipf_centi as i32 - PREFIX_SCORE_STEP * word_len.saturating_sub(prefix_len) as i32
}

/// 只读英文词库。
pub struct EnglishDict {
    data: Backing,
    header: Header,
}

impl std::fmt::Debug for EnglishDict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnglishDict")
            .field("word_count", &self.word_count())
            .field("prefix_count", &self.prefix_count())
            .field("file_len", &self.file_len())
            .finish()
    }
}

impl EnglishDict {
    /// 打开并完整校验一个 `english.bin`。
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, EnglishError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: 与项目既有 mmap 词库同一取舍——文件在映射期内不应被原地截断，
        // 构建端因此用「同目录临时文件 + 原子替换」发布。
        let map = unsafe { Mmap::map(&file)? };
        Self::from_backing(Backing::Mapped(map))
    }

    /// 从内存字节构造（测试与构建校验用）。走同一条校验路径。
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, EnglishError> {
        Self::from_backing(Backing::Owned(bytes))
    }

    fn from_backing(data: Backing) -> Result<Self, EnglishError> {
        let header = parse_header(&data)?;
        let dict = Self { data, header };
        dict.validate()?;
        Ok(dict)
    }

    /// 词条数。
    pub fn word_count(&self) -> usize {
        self.header.word_count as usize
    }

    /// 前缀数。
    pub fn prefix_count(&self) -> usize {
        self.header.prefix_count as usize
    }

    /// 整个文件的字节长度（报告用）。
    pub fn file_len(&self) -> usize {
        self.header.file_len
    }

    /// 在 strings 区里取一段字节并解码为 `&str`。
    fn string_at(&self, off: usize, len: usize) -> Result<&str, EnglishError> {
        let start = self
            .header
            .strings_offset
            .checked_add(off)
            .ok_or_else(|| EnglishError::corrupt("字符串偏移溢出"))?;
        let end = start
            .checked_add(len)
            .ok_or_else(|| EnglishError::corrupt("字符串长度溢出"))?;
        let b = self
            .data
            .get(start..end)
            .ok_or_else(|| EnglishError::corrupt("字符串引用越界"))?;
        std::str::from_utf8(b).map_err(|_| EnglishError::corrupt("字符串不是合法 UTF-8"))
    }

    /// 读一条 word 记录。
    pub fn entry_at(&self, word_id: u32) -> Result<EnglishEntryRef<'_>, EnglishError> {
        if word_id >= self.header.word_count {
            return Err(EnglishError::corrupt("word_id 越界"));
        }
        let off = self.header.words_offset + word_id as usize * WORD_RECORD_LEN;
        let key_off = read_u32(&self.data, off)? as usize;
        let disp_off = read_u32(&self.data, off + 4)? as usize;
        let key_len = read_u16(&self.data, off + 8)? as usize;
        let disp_len = read_u16(&self.data, off + 10)? as usize;
        let zipf_centi = read_u16(&self.data, off + 12)?;
        Ok(EnglishEntryRef {
            key: self.string_at(key_off, key_len)?,
            display: self.string_at(disp_off, disp_len)?,
            zipf_centi,
            word_id,
        })
    }

    /// 精确查询（按 key 二分）。
    pub fn exact(&self, key: &str) -> Result<Option<EnglishEntryRef<'_>>, EnglishError> {
        let n = self.header.word_count as usize;
        let (mut lo, mut hi) = (0usize, n);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let e = self.entry_at(mid as u32)?;
            match e.key.cmp(key) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Ok(Some(e)),
            }
        }
        Ok(None)
    }

    /// key 是否存在（用户索引过滤用）。
    pub fn contains(&self, key: &str) -> bool {
        matches!(self.exact(key), Ok(Some(_)))
    }

    /// 前缀补全缓存：最多 [`PREFIX_TOP_LIMIT`] 条，按构建端的静态分序。
    ///
    /// 精确命中不依赖它——查询入口会另外二分强制召回（见数据分册第 5 节）。
    pub fn prefix_top(&self, prefix: &str) -> Result<Vec<EnglishEntryRef<'_>>, EnglishError> {
        if prefix.is_empty() || !prefix.is_ascii() || prefix.len() > 48 {
            return Ok(Vec::new());
        }
        let n = self.header.prefix_count as usize;
        let (mut lo, mut hi) = (0usize, n);
        let mut found = None;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let p = self.prefix_at(mid as u32)?;
            match p.cmp(prefix) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    found = Some(mid);
                    break;
                }
            }
        }
        let Some(idx) = found else {
            return Ok(Vec::new());
        };
        let off = self.header.prefixes_offset + idx * PREFIX_RECORD_LEN;
        let id_count = read_u16(&self.data, off + 6)? as usize;
        let start = read_u32(&self.data, off + 8)? as usize;
        let mut out = Vec::with_capacity(id_count);
        for i in 0..id_count {
            let id = self.id_at(start + i)?;
            out.push(self.entry_at(id)?);
        }
        Ok(out)
    }

    /// 读一条 prefix 记录的键。
    fn prefix_at(&self, idx: u32) -> Result<&str, EnglishError> {
        if idx >= self.header.prefix_count {
            return Err(EnglishError::corrupt("prefix 下标越界"));
        }
        let off = self.header.prefixes_offset + idx as usize * PREFIX_RECORD_LEN;
        let p_off = read_u32(&self.data, off)? as usize;
        let p_len = read_u16(&self.data, off + 4)? as usize;
        self.string_at(p_off, p_len)
    }

    /// 读 ids 数组里的一个 word_id。
    fn id_at(&self, idx: usize) -> Result<u32, EnglishError> {
        let off = self
            .header
            .ids_offset
            .checked_add(idx.checked_mul(4).ok_or_else(|| EnglishError::corrupt("ids 下标溢出"))?)
            .ok_or_else(|| EnglishError::corrupt("ids 偏移溢出"))?;
        let v = read_u32(&self.data, off)?;
        if v >= self.header.word_count {
            return Err(EnglishError::corrupt("ids 区引用了不存在的 word_id"));
        }
        Ok(v)
    }

    /// 全量校验：词表、前缀表、ids 区。只在 `open` 时跑一次。
    fn validate(&self) -> Result<(), EnglishError> {
        self.validate_words()?;
        self.validate_prefixes()?;
        Ok(())
    }

    fn validate_words(&self) -> Result<(), EnglishError> {
        let mut prev: Option<&str> = None;
        for id in 0..self.header.word_count {
            let e = self.entry_at(id)?;
            if !is_english_key(e.key) {
                return Err(EnglishError::corrupt("word.key 不是合法小写英文键"));
            }
            if !valid_word_text(e.display) || !e.display.eq_ignore_ascii_case(e.key) {
                return Err(EnglishError::corrupt(
                    "word.display 不合法或大小写形式与 key 不一致",
                ));
            }
            if e.zipf_centi > MAX_ZIPF_CENTI {
                return Err(EnglishError::corrupt("zipf_centi 超过 900"));
            }
            let off = self.header.words_offset + id as usize * WORD_RECORD_LEN;
            if read_u16(&self.data, off + 14)? != 0 {
                return Err(EnglishError::corrupt("word.reserved 必须为 0"));
            }
            if let Some(p) = prev {
                if p >= e.key {
                    return Err(EnglishError::corrupt("word 记录未按 key 严格升序"));
                }
            }
            prev = Some(e.key);
        }
        Ok(())
    }

    fn validate_prefixes(&self) -> Result<(), EnglishError> {
        let ids_total = (self.header.strings_offset - self.header.ids_offset) / 4;
        let mut prev: Option<&str> = None;
        let mut sum_ids = 0usize;
        for idx in 0..self.header.prefix_count {
            let key = self.prefix_at(idx)?;
            if !valid_word_text(key) {
                return Err(EnglishError::corrupt("prefix 不是合法 ASCII 字母串"));
            }
            if let Some(p) = prev {
                if p >= key {
                    return Err(EnglishError::corrupt("prefix 记录未按字节序严格升序"));
                }
            }
            prev = Some(key);

            let off = self.header.prefixes_offset + idx as usize * PREFIX_RECORD_LEN;
            if read_u32(&self.data, off + 12)? != 0 {
                return Err(EnglishError::corrupt("prefix.reserved 必须为 0"));
            }
            let id_count = read_u16(&self.data, off + 6)? as usize;
            if id_count == 0 || id_count > PREFIX_TOP_LIMIT {
                return Err(EnglishError::corrupt("prefix.id_count 必须在 1..=32"));
            }
            let start = read_u32(&self.data, off + 8)? as usize;
            if start
                .checked_add(id_count)
                .is_none_or(|end| end > ids_total)
            {
                return Err(EnglishError::corrupt("prefix 的 ids 引用越界"));
            }
            sum_ids += id_count;
            self.validate_prefix_list(key, start, id_count)?;
        }
        if sum_ids != ids_total {
            return Err(EnglishError::corrupt(
                "ids 区长度与各 prefix 的 id_count 总和不一致",
            ));
        }
        Ok(())
    }

    /// 单个前缀的引用列表：无重复 ID、被引用词确实以该前缀开头、
    /// 列表按 (静态分降序, key 升序) 排好。
    fn validate_prefix_list(
        &self,
        prefix: &str,
        start: usize,
        id_count: usize,
    ) -> Result<(), EnglishError> {
        let mut seen: Vec<u32> = Vec::with_capacity(id_count);
        let mut prev_key: Option<&str> = None;
        let mut prev_score = i32::MAX;
        for i in 0..id_count {
            let id = self.id_at(start + i)?;
            if seen.contains(&id) {
                return Err(EnglishError::corrupt("同一 prefix 的引用列表里有重复 word_id"));
            }
            seen.push(id);
            let e = self.entry_at(id)?;
            if !e.key.starts_with(prefix) {
                return Err(EnglishError::corrupt("prefix 引用了不以它开头的词"));
            }
            let score = static_score_centi(e.zipf_centi, e.key.len(), prefix.len());
            match prev_key {
                Some(_) if prev_score < score => {
                    return Err(EnglishError::corrupt("prefix 列表未按静态分降序"));
                }
                Some(pk) if prev_score == score && pk >= e.key => {
                    return Err(EnglishError::corrupt("prefix 列表同分时未按 key 升序"));
                }
                None => {}
                _ => {}
            }
            prev_key = Some(e.key);
            prev_score = score;
        }
        Ok(())
    }
}
