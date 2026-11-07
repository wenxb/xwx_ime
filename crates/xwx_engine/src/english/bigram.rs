//! `english_bigram.bin`（M2 静态二元数据）的只读访问。
//!
//! 格式见 `docs/english-input/04-prediction.md` 第 4 节。与 [`super::dict`]
//! 同一取舍：**加载即全量校验**，失败只关闭静态二元增强——已有英文词库、
//! 前缀补全与用户学习一律不受影响。

use std::ops::Deref;
use std::path::Path;

use memmap2::Mmap;

use super::format::{is_english_key, read_u16, read_u32, read_u64, to_usize, EnglishError};

/// 魔数：`XWXENB1` 后接一个零字节。
pub(super) const BIGRAM_MAGIC: [u8; 8] = *b"XWXENB1\0";
/// 格式版本。
pub(super) const BIGRAM_VERSION: u32 = 1;
/// 头部长度。
pub(super) const BIGRAM_HEADER_LEN: usize = 64;
/// context 记录长度。
pub(super) const CONTEXT_RECORD_LEN: usize = 24;
/// edge 记录长度。
pub(super) const EDGE_RECORD_LEN: usize = 16;
/// 防御上限：文件最大 256 MiB。
pub(super) const MAX_BIGRAM_FILE_LEN: u64 = 256 * 1024 * 1024;
/// 防御上限：context 数。
pub(super) const MAX_CONTEXTS: u32 = 2_000_000;
/// 防御上限：edge 数。
pub(super) const MAX_EDGES: u32 = 64_000_000;
/// 每个 prev 最多保留的 next 数。
pub(super) const TOP_PER_CONTEXT: usize = 32;

/// 一条二元边（借用视图）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BigramEdge<'a> {
    /// 下一个词的小写 key。
    pub next: &'a str,
    /// 该词对的观测计数。
    pub count: u64,
}

/// 一个 prev 的行视图：**只带边界，不分配**（热路径每次预测都会查它）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BigramRow {
    /// 该 prev 下**截断前**的观测总数（`observed_total`）。
    pub observed_total: u64,
    edge_start: usize,
    edge_count: usize,
}

impl BigramRow {
    /// 行内边数（1..=32）。
    pub fn len(&self) -> usize {
        self.edge_count
    }

    /// 行是否为空（结构上不可能是空，保留给 clippy 的 `len` 配套）。
    pub fn is_empty(&self) -> bool {
        self.edge_count == 0
    }

    /// 按 (count 降序, next 升序) 迭代这一行。
    pub fn iter<'m>(self, model: &'m EnglishBigram) -> impl Iterator<Item = BigramEdge<'m>> + 'm {
        (0..self.edge_count).filter_map(move |j| model.edge_at(self.edge_start + j).ok())
    }
}

/// 底层字节来源（生产 mmap / 测试内存）。
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

/// 只读静态二元模型。
pub struct EnglishBigram {
    data: Backing,
    context_count: usize,
    edge_count: usize,
    contexts_offset: usize,
    edges_offset: usize,
    strings_offset: usize,
    file_len: usize,
}

impl std::fmt::Debug for EnglishBigram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnglishBigram")
            .field("contexts", &self.context_count)
            .field("edges", &self.edge_count)
            .field("file_len", &self.file_len)
            .finish()
    }
}

impl EnglishBigram {
    /// 打开并完整校验一个 `english_bigram.bin`。
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, EnglishError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: 与既有 mmap 数据同一取舍——文件在映射期内不应被原地截断，
        // 构建端因此用「同目录临时文件 + 原子替换」发布。
        let map = unsafe { Mmap::map(&file)? };
        Self::from_backing(Backing::Mapped(map))
    }

    /// 从内存字节构造（测试与自检用）。走**同一条**校验路径。
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, EnglishError> {
        Self::from_backing(Backing::Owned(bytes))
    }

    fn from_backing(data: Backing) -> Result<Self, EnglishError> {
        if data.len() < BIGRAM_HEADER_LEN {
            return Err(EnglishError::corrupt("文件过短，不是完整 english_bigram.bin"));
        }
        if data[0..8] != BIGRAM_MAGIC {
            return Err(EnglishError::corrupt("魔数不匹配，不是 english_bigram.bin"));
        }
        if read_u32(&data, 8)? != BIGRAM_VERSION {
            return Err(EnglishError::corrupt("english_bigram.bin 版本不兼容"));
        }
        if read_u32(&data, 12)? as usize != BIGRAM_HEADER_LEN {
            return Err(EnglishError::corrupt("header_len 必须是 64"));
        }
        let context_count = read_u32(&data, 16)?;
        let edge_count = read_u32(&data, 20)?;
        if context_count > MAX_CONTEXTS {
            return Err(EnglishError::corrupt("context_count 超过防御上限"));
        }
        if edge_count > MAX_EDGES {
            return Err(EnglishError::corrupt("edge_count 超过防御上限"));
        }
        let contexts_offset = to_usize(read_u64(&data, 24)?, "contexts_offset")?;
        let edges_offset = to_usize(read_u64(&data, 32)?, "edges_offset")?;
        let strings_offset = to_usize(read_u64(&data, 40)?, "strings_offset")?;
        let file_len = to_usize(read_u64(&data, 48)?, "file_len")?;
        if read_u64(&data, 56)? != 0 {
            return Err(EnglishError::corrupt("头部 reserved 必须为 0"));
        }
        if file_len != data.len() {
            return Err(EnglishError::corrupt("file_len 与实际文件长度不一致"));
        }
        if file_len as u64 > MAX_BIGRAM_FILE_LEN {
            return Err(EnglishError::corrupt("文件超过 256 MiB 防御上限"));
        }
        if !(BIGRAM_HEADER_LEN <= contexts_offset
            && contexts_offset <= edges_offset
            && edges_offset <= strings_offset
            && strings_offset <= file_len)
        {
            return Err(EnglishError::corrupt(
                "区段顺序不合法: contexts/edges/strings 起点必须递增且落在文件内",
            ));
        }
        if contexts_offset != BIGRAM_HEADER_LEN {
            return Err(EnglishError::corrupt("contexts 必须紧接头部"));
        }
        let want_edges = contexts_offset
            .checked_add((context_count as usize).checked_mul(CONTEXT_RECORD_LEN).ok_or_else(
                || EnglishError::corrupt("contexts 区长度溢出"),
            )?)
            .ok_or_else(|| EnglishError::corrupt("contexts 区偏移溢出"))?;
        if want_edges != edges_offset {
            return Err(EnglishError::corrupt("contexts 区长度与 context_count 不一致"));
        }
        let want_strings = edges_offset
            .checked_add(
                (edge_count as usize)
                    .checked_mul(EDGE_RECORD_LEN)
                    .ok_or_else(|| EnglishError::corrupt("edges 区长度溢出"))?,
            )
            .ok_or_else(|| EnglishError::corrupt("edges 区偏移溢出"))?;
        if want_strings != strings_offset {
            return Err(EnglishError::corrupt("edges 区长度与 edge_count 不一致"));
        }
        let model = Self {
            data,
            context_count: context_count as usize,
            edge_count: edge_count as usize,
            contexts_offset,
            edges_offset,
            strings_offset,
            file_len,
        };
        model.validate()?;
        Ok(model)
    }

    /// context 数。
    pub fn context_count(&self) -> usize {
        self.context_count
    }

    /// edge 数。
    pub fn edge_count(&self) -> usize {
        self.edge_count
    }

    /// 文件字节长度（报告用）。
    pub fn file_len(&self) -> usize {
        self.file_len
    }

    fn string_at(&self, off: usize, len: usize) -> Result<&str, EnglishError> {
        let start = self
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

    /// 读一条 context 记录的 prev key 与它的行边界。
    fn context_at(&self, idx: usize) -> Result<(&str, usize, usize, u64), EnglishError> {
        if idx >= self.context_count {
            return Err(EnglishError::corrupt("context 下标越界"));
        }
        let off = self.contexts_offset + idx * CONTEXT_RECORD_LEN;
        let prev_off = read_u32(&self.data, off)? as usize;
        let prev_len = read_u16(&self.data, off + 4)? as usize;
        let edge_count = read_u16(&self.data, off + 6)? as usize;
        let edge_start = read_u32(&self.data, off + 8)? as usize;
        if read_u32(&self.data, off + 12)? != 0 {
            return Err(EnglishError::corrupt("context.reserved 必须为 0"));
        }
        let observed_total = read_u64(&self.data, off + 16)?;
        Ok((
            self.string_at(prev_off, prev_len)?,
            edge_count,
            edge_start,
            observed_total,
        ))
    }

    /// 读一条 edge。
    fn edge_at(&self, idx: usize) -> Result<BigramEdge<'_>, EnglishError> {
        if idx >= self.edge_count {
            return Err(EnglishError::corrupt("edge 下标越界"));
        }
        let off = self.edges_offset + idx * EDGE_RECORD_LEN;
        let next_off = read_u32(&self.data, off)? as usize;
        let next_len = read_u16(&self.data, off + 4)? as usize;
        if read_u16(&self.data, off + 6)? != 0 {
            return Err(EnglishError::corrupt("edge.reserved 必须为 0"));
        }
        let count = read_u64(&self.data, off + 8)?;
        Ok(BigramEdge {
            next: self.string_at(next_off, next_len)?,
            count,
        })
    }

    /// 全量校验（只在 `open` 时跑一次）。
    fn validate(&self) -> Result<(), EnglishError> {
        let mut prev: Option<&str> = None;
        let mut seen_edges = 0usize;
        for idx in 0..self.context_count {
            let (key, edge_count, edge_start, observed_total) = self.context_at(idx)?;
            if !is_english_key(key) {
                return Err(EnglishError::corrupt("context.prev 不是合法小写英文键"));
            }
            if let Some(p) = prev {
                if p >= key {
                    return Err(EnglishError::corrupt("context 未按 prev 严格升序"));
                }
            }
            prev = Some(key);
            if edge_count == 0 || edge_count > TOP_PER_CONTEXT {
                return Err(EnglishError::corrupt("context.edge_count 必须在 1..=32"));
            }
            if observed_total == 0 {
                return Err(EnglishError::corrupt("context.observed_total 必须 > 0"));
            }
            if edge_start
                .checked_add(edge_count)
                .is_none_or(|end| end > self.edge_count)
            {
                return Err(EnglishError::corrupt("context 的 edges 引用越界"));
            }
            seen_edges += edge_count;

            let mut prev_edge: Option<(u64, &str)> = None;
            let mut total = 0u64;
            for j in 0..edge_count {
                let e = self.edge_at(edge_start + j)?;
                if !is_english_key(e.next) {
                    return Err(EnglishError::corrupt("edge.next 不是合法小写英文键"));
                }
                if e.count == 0 || e.count > observed_total {
                    return Err(EnglishError::corrupt("edge.count 必须 >0 且不超过 observed_total"));
                }
                if let Some((pc, pn)) = prev_edge {
                    if pc < e.count || (pc == e.count && pn >= e.next) {
                        return Err(EnglishError::corrupt("edge 行内顺序必须按 count 降序、next 升序"));
                    }
                }
                prev_edge = Some((e.count, e.next));
                total += e.count;
            }
            if total > observed_total {
                return Err(EnglishError::corrupt("行内 count 总和超过 observed_total"));
            }
        }
        if seen_edges != self.edge_count {
            return Err(EnglishError::corrupt("edge 引用总数与 edge_count 不一致"));
        }
        Ok(())
    }

    /// 按 prev 二分查一行（不存在返回 `None`）。**零分配、零锁**。
    pub fn row(&self, prev: &str) -> Result<Option<BigramRow>, EnglishError> {
        let (mut lo, mut hi) = (0usize, self.context_count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let key = self.context_at(mid)?.0;
            match key.cmp(prev) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    let (_, edge_count, edge_start, observed_total) = self.context_at(mid)?;
                    return Ok(Some(BigramRow {
                        observed_total,
                        edge_start,
                        edge_count,
                    }));
                }
            }
        }
        Ok(None)
    }

    /// 某词对的计数（未收录返回 0）。
    pub fn count(&self, prev: &str, next: &str) -> Result<u64, EnglishError> {
        let Some(row) = self.row(prev)? else {
            return Ok(0);
        };
        Ok(row
            .iter(self)
            .find(|e| e.next == next)
            .map(|e| e.count)
            .unwrap_or(0))
    }
}
