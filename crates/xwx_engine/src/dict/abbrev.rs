//! 简拼副索引(`abbrev.bin`):与主索引同构的「首字母 → 词」索引。
//! 只回答一个问题:输入是纯首字母缩写时,哪些词可能匹配。

use std::fs::File;
use std::path::Path;

use memmap2::Mmap;

use crate::candidate::Candidate;

use super::{read_u16, read_u32, read_u64, DictError, MmapRecord, ABBREV_HEADER_LEN, ABBREV_MAGIC,
            ABBREV_VERSION, MAX_LOOKUP_RESULTS, MMAP_RECORD_LEN};

/// 基于只读 mmap 二进制的词库（运行时主力实现）。
///
/// 由 `scripts/dict_to_bin.py` 从 SQLite 词库转换而来；启动时 `Mmap::map`
/// 把文件映射进地址空间（O(1)），查询就是在内存上二分 + 切片，零 SQL、
/// 零锁、无逐击键磁盘 IO。文件只读 → 天然线程安全，不需要 `Mutex`。
///
/// 二进制布局（小端）见 `data/README.md`「dict.bin 二进制格式」。
/// 简拼(首字母)副索引:`zgr` → 词条,与主索引同构的只读 mmap 二进制。
///
/// 由 `scripts/abbrev_to_bin.py` 从 `dict.bin` 生成(路线 (c),见
/// `docs/design-batch1.md`「B3 spike 结论」)。key 是「拼音各音节首字母串」
/// (`"zhong guo ren"` → `"zgr"`),词条区每条比主索引多带一个「全拼」字段,
/// 好让查出的词能回送后续打分流程。布局见 `data/README.md`「abbrev.bin
/// 二进制格式」。文件只读,天然线程安全。
#[derive(Debug)]
pub struct AbbrevIndex {
    map: Mmap,
    key_count: usize,
    records_off: usize,
    strings_off: usize,
    entries_off: usize,
}

/// 简拼查询的**写法变体**：把每个 `zh`/`ch`/`sh` 的 `h` 要么保留、要么去掉。
///
/// 上游（RIME 的简拼代数）两条规则**并存**：
/// - `abbrev/^(\l).+$/$1/`：每个音节取首字母（`shi hou` → `sh`）
/// - `abbrev/^([zcs]h).+$/$1/`：`zh`/`ch`/`sh` 作为一个声母整体（`shi hou` → `shh`）
///
/// 副索引是**按「每音节一个字母」建的 key**，所以用户敲的 `shh`（3 个字母、
/// 2 个音节）在表里对应的是 `sh`：多出来的 `h` 是「整体声母」写法留下的。
/// 因此查询要把所有写法都查一遍再合并——`shh` → {`shh`, `sh`}，`zgr` 不变。
///
/// 变体数 2^k（k = 命中的 `zh`/`ch`/`sh` 个数），k 超过 4 就只保留原串——
/// 病态输入（`zhzhzhzh`）不该把一次查询放大成几十次二分。
pub fn abbrev_variants(abbrev: &str) -> Vec<String> {
    let bytes = abbrev.as_bytes();
    let mut marks: Vec<usize> = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if matches!(bytes[i], b'z' | b'c' | b's') && bytes[i + 1] == b'h' {
            marks.push(i + 1); // 这是要被「去掉」的那个 h
            i += 2;
        } else {
            i += 1;
        }
    }
    if marks.is_empty() || marks.len() > 4 {
        return vec![abbrev.to_string()];
    }
    let mut out = Vec::with_capacity(1 << marks.len());
    for bits in 0..(1u32 << marks.len()) {
        let drop: Vec<bool> = (0..marks.len()).map(|k| bits & (1 << k) != 0).collect();
        let mut s = String::with_capacity(abbrev.len());
        for (idx, ch) in abbrev.char_indices() {
            let is_h = marks.contains(&idx);
            if is_h && drop[marks.iter().position(|&m| m == idx).unwrap_or(0)] {
                continue;
            }
            s.push(ch);
        }
        out.push(s);
    }
    out.sort();
    out.dedup();
    out
}

impl AbbrevIndex {
    /// 打开并 mmap 映射一个 abbrev.bin 副索引文件。
    ///
    /// 校验魔数与版本,损坏文件返回 [`DictError::Corrupt`]。
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, DictError> {
        let file = File::open(path)?;
        let map = unsafe { Mmap::map(&file)? };
        let bytes: &[u8] = &map;
        if bytes.len() < ABBREV_HEADER_LEN {
            return Err(DictError::Corrupt("文件过短,不是完整 abbrev.bin"));
        }
        if bytes.get(0..8) != Some(ABBREV_MAGIC.as_slice()) {
            return Err(DictError::Corrupt("魔数不匹配,不是 abbrev.bin 副索引"));
        }
        if read_u32(bytes, 8)? != ABBREV_VERSION {
            return Err(DictError::Corrupt("abbrev.bin 版本不兼容"));
        }
        let key_count = read_u64(bytes, 12)? as usize;
        let records_off = read_u64(bytes, 28)? as usize;
        let strings_off = read_u64(bytes, 36)? as usize;
        let entries_off = read_u64(bytes, 44)? as usize;
        Ok(Self { map, key_count, records_off, strings_off, entries_off })
    }

    /// 边界检查的切片:越界一律返回损坏错误(不 panic)。
    fn slice(&self, off: usize, len: usize) -> Result<&[u8], DictError> {
        let end = off.checked_add(len).ok_or(DictError::Corrupt("区段长度溢出"))?;
        self.map.get(off..end).ok_or(DictError::Corrupt("区段越界"))
    }

    /// 第 `i` 条首字母索引记录(数组内按首字母串字节序排序)。
    fn record(&self, i: usize) -> Result<MmapRecord, DictError> {
        let b = self.slice(self.records_off + i * MMAP_RECORD_LEN, MMAP_RECORD_LEN)?;
        Ok(MmapRecord {
            str_off: read_u32(b, 0)? as usize,
            str_len: read_u16(b, 4)? as usize,
            ent_off: read_u32(b, 6)? as usize,
            ent_len: read_u16(b, 10)? as usize,
        })
    }

    /// 前缀查找的下界:第一条以 `prefix` 开头的记录下标。
    fn lower_bound(&self, prefix: &[u8]) -> Result<usize, DictError> {
        let (mut lo, mut hi) = (0usize, self.key_count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let rec = self.record(mid)?;
            let s = self.slice(self.strings_off + rec.str_off, rec.str_len)?;
            if s < prefix {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    }

    /// 纯简拼前缀查询:`zgr` 二分副索引,返回带全拼的词条(块内已按 freq 降序)。
    ///
    /// 带早停:凑够 `MAX_LOOKUP_RESULTS` 即止,不物化全部命中。
    pub fn lookup_prefix(&self, abbrev: &str) -> Result<Vec<Candidate>, DictError> {
        let pb = abbrev.as_bytes();
        let start = self.lower_bound(pb)?;
        let mut out: Vec<Candidate> = Vec::new();
        for i in start..self.key_count {
            let rec = self.record(i)?;
            let s = self.slice(self.strings_off + rec.str_off, rec.str_len)?;
            if !s.starts_with(pb) {
                break;
            }
            let block = self.slice(self.entries_off + rec.ent_off, rec.ent_len)?;
            let mut pos = 0usize;
            while pos < block.len() {
                let wl = *block.get(pos).ok_or(DictError::Corrupt("词块越界"))? as usize;
                let word_bytes = block
                    .get(pos + 1..pos + 1 + wl)
                    .ok_or(DictError::Corrupt("词块越界"))?;
                let pl = *block
                    .get(pos + 1 + wl)
                    .ok_or(DictError::Corrupt("词块越界"))? as usize;
                let py_bytes = block
                    .get(pos + 2 + wl..pos + 2 + wl + pl)
                    .ok_or(DictError::Corrupt("词块越界"))?;
                let freq = read_u32(block, pos + 2 + wl + pl)?;
                let word = std::str::from_utf8(word_bytes)
                    .map_err(|_| DictError::Corrupt("词条不是 UTF-8"))?;
                let pinyin = std::str::from_utf8(py_bytes)
                    .map_err(|_| DictError::Corrupt("全拼不是 UTF-8"))?;
                out.push(Candidate::new(word, pinyin, freq as u64));
                if out.len() >= MAX_LOOKUP_RESULTS as usize {
                    return Ok(out);
                }
                pos += 2 + wl + pl + 4;
            }
        }
        Ok(out)
    }
}
