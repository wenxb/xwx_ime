//! 词库（dictionary）抽象与两种实现。
//!
//! - [`MmapDict`]：**运行时主力**。`scripts/dict_to_bin.py` 把词库转成
//!   只读二进制（dict.bin），启动时 mmap 映射，查询=内存二分查找；
//! - [`SqliteDict`]：内存词库/测试/迁移用（`ImeEngine::new(None)` 与
//!   TSV 导入），运行时默认不再走 SQLite。
//!
//! `Dictionary` trait 定义引擎对词库的唯一依赖接口，两种实现共用；
//! 引擎字段用 [`DictStore`] 枚举聚合（保持具体类型，不用 trait object）。
//!
//! 词库表结构见 `data/README.md`；核心约定：`pinyin` 字段用空格分隔多音节，
//! 如 `"ni hao"`，与 `Candidate::pinyin` 格式一致。
//!
//! # 文件布局(2026-10-02 拆分,原为单文件 2140 行)
//!
//! 按职责分文件:`mmap.rs`(主索引原语 + [`Dictionary`] 实现)、
//! `lookup.rs`(三个查询入口:副索引 / 交错简拼 / 全拼路)、
//! `mixed.rs`(交错 DFS 与半音节补全的探索层)、`abbrev.rs`(副索引)、
//! `sqlite.rs`(SQLite 后端)、`tests.rs`(单测)。**类型定义全部留在本文件**,
//! 这样各子模块能直接读它们的私有字段,只有跨模块调用的方法才标 `pub(super)`。

mod abbrev;
mod lookup;
mod mixed;
mod mmap;
mod sqlite;
mod syllable_graph;
// 末尾半音节补全的门槛:**唯一来源**在 `mixed`(那里有「为什么是 2」的依据),
// 引擎侧的补全弧要用同一条判据,所以在这里转出去。
pub(crate) use mixed::MIN_PARTIAL_TAIL;
pub use abbrev::AbbrevIndex;
pub use sqlite::SqliteDict;
pub use syllable_graph::{GraphNode, NO_ENTRY, SyllableGraph};

use std::sync::OnceLock;
use std::path::Path;

use memmap2::Mmap;
use thiserror::Error;

use crate::candidate::Candidate;

/// 一次查询最多返回的条目数（防止超大词库拖垮排序）。
const MAX_LOOKUP_RESULTS: i64 = 500;

/// dict.bin 二进制词库的魔数（8 字节，含结尾 `\0`）。
pub const MMAP_MAGIC: &[u8; 8] = b"XWXIDM1\0";
/// dict.bin 当前格式版本。
const MMAP_VERSION: u32 = 1;
/// 头部长：magic(8) + version(4) + pinyin_cnt(8) + total_entries(8)
///      + records_off(8) + strings_off(8) + entries_off(8) = 52。
const MMAP_HEADER_LEN: usize = 52;
/// 单条拼音索引：str_off(u32) + str_len(u16) + ent_off(u32) + ent_len(u16) = 12。
const MMAP_RECORD_LEN: usize = 12;

/// abbrev.bin 简拼副索引的魔数(8 字节,含结尾 `\0`)。
pub const ABBREV_MAGIC: &[u8; 8] = b"XWXABR1\0";
/// abbrev.bin 当前格式版本(v1,与主索引同构)。
const ABBREV_VERSION: u32 = 1;
/// abbrev.bin 头部长:magic(8) + version(4) + 5×u64(40) = 52(与主索引同构)。
const ABBREV_HEADER_LEN: usize = 52;

/// 词库错误。
#[derive(Debug, Error)]
pub enum DictError {
    /// SQLite 层错误。
    #[error("SQLite 错误: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// I/O 错误（建目录、打开文件等）。
    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),
    /// 词库内部锁中毒（并发访问时另一线程 panic 导致）。
    #[error("词库内部锁已中毒")]
    LockPoisoned,
    /// 只读词库（mmap 二进制）不支持写操作。
    #[error("词库只读，不支持写入")]
    ReadOnly,
    /// 二进制词库文件损坏（魔数/版本/越界/编码）。
    #[error("词库文件损坏: {0}")]
    Corrupt(&'static str),
    /// TSV 导入时某一行格式不对。
    #[error("TSV 导入失败（第 {line} 行）: {detail}")]
    BadTsvLine { line: u64, detail: String },
}

/// 主索引里 `prefix` 的存在性（简拼弧剪枝用，见 [`Dictionary::key_prefix_state`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPrefix {
    /// 没有任何 key 以它为前缀：整棵子树是死的，当场剪掉。
    None,
    /// 有更长的 key 以它为前缀，但它自己不是词条：**不查词条**，只继续往下走。
    Prefix,
    /// 它自己就是一条词条（也可能同时是别的前缀）。
    Exact,
}

/// 词库抽象：按拼音查词。
pub trait Dictionary: Send + Sync {
    /// 精确查词：`pinyin` 可以是单音节（`"ni"`）或多音节短语（`"ni hao"`）。
    fn lookup(&self, pinyin: &str) -> Result<Vec<Candidate>, DictError>;

    /// 主索引里以 `pinyin` 为前缀的 key 存在性（简拼弧剪枝用）。
    ///
    /// 三态是**性能与正确性的分界**：`Prefix` 说明「有更长的词条在这里，
    /// 但本级不是词」——简拼弧据此跳过词条解析（每个链节点一次
    /// `parse_entries` 分配，22K 链就是实测 5~7ms 的来源）；`Exact` 才去查
    /// 词条。默认 `None`：只有真实 mmap 词库有简拼能力。
    fn key_prefix_state(&self, _pinyin: &str) -> Result<KeyPrefix, DictError> {
        Ok(KeyPrefix::None)
    }

    /// 见 [`MmapDict::key_prefix_state_in`]；默认实现退化成全局查询。
    fn key_prefix_state_in(
        &self,
        pinyin: &str,
        _lo: usize,
        _hi: usize,
    ) -> Result<(KeyPrefix, usize), DictError> {
        Ok((self.key_prefix_state(pinyin)?, 0))
    }

    /// 音节级转移索引(可选)：缩写弧的半音节展开与「这条链存在吗」的剪枝用。
    ///
    /// 默认 `None`：只有装配了 `syllable_graph.bin` 的 mmap 词库有。返回 `None`
    /// 时调用方**必须**回退到 [`Dictionary::key_prefix_state`]——那条路每次是一次
    /// 跨页 mmap 二分(实测 ~1µs)，索引只是把这些查询换成内存树walk。
    fn syllable_graph(&self) -> Option<&SyllableGraph> {
        None
    }

    /// 前缀查词：返回所有 `pinyin` 以给定串开头的词条。
    ///
    /// 用于「边输入边联想」；TODO: 当前实现走 LIKE，数据量大时需要
    /// 专门的拼音前缀索引。
    fn lookup_prefix(&self, pinyin: &str) -> Result<Vec<Candidate>, DictError>;

    /// 带条数上限的前缀联想:语义与 [`Dictionary::lookup_prefix`] 相同,
    /// 但最多返回 `limit` 条。供「声母前缀按音节枚举展开」逐音节查询用
    /// (见 `ImeEngine::fallback_candidates`):`lookup_prefix` 的全局上限
    /// `MAX_LOOKUP_RESULTS` 在逐音节配额下太大(单字母声母十几个韵母,
    /// 每个都取 500 条会把合并池撑到几千条、全是低频长尾),逐音节
    /// 限流后总查询量才可控。`limit >= MAX_LOOKUP_RESULTS` 时与
    /// `lookup_prefix` 等价(各实现内部仍按全局上限早停)。
    fn lookup_prefix_limited(
        &self,
        pinyin: &str,
        limit: usize,
    ) -> Result<Vec<Candidate>, DictError>;

    /// 全表词频总和（打分归一化分母，见 `lib.rs` 的 `log10(freq/总词频)`）。
    ///
    /// 默认 0 = 空词库/未实现：调用方按「词频项为 0」处理，不除零、不产生
    /// NaN（真实词库实现总返回 >0）。
    fn total_freq(&self) -> Result<u64, DictError> {
        Ok(0)
    }
}

/// 拼音索引的一条记录：指向拼音区与词区（格式见 `data/README.md`）。
#[derive(Debug, Clone, Copy)]
struct MmapRecord {
    str_off: usize,
    str_len: usize,
    ent_off: usize,
    ent_len: usize,
}

/// 基于只读 mmap 二进制的词库（运行时主力实现）。
///
/// 由 `scripts/dict_to_bin.py` 从 SQLite 词库转换而来；启动时 `Mmap::map`
/// 把文件映射进地址空间（O(1)），查询就是在内存上二分 + 切片，零 SQL、
/// 零锁、无逐击键磁盘 IO。文件只读 → 天然线程安全，不需要 `Mutex`。
///
/// 二进制布局（小端）见 `data/README.md`「dict.bin 二进制格式」。
/// 简拼(首字母)副索引:`zgr` → 词条,与主索引同构的只读 mmap 二进制。
///
/// 由 `scripts/abbrev_to_bin.py` 从 `dict.bin` 生成(路线 (c),B3 spike 实测结论)。key 是「拼音各音节首字母串」
/// (`"zhong guo ren"` → `"zgr"`),词条区每条比主索引多带一个「全拼」字段,
/// 好让查出的词能回送后续打分流程。布局见 `data/README.md`「abbrev.bin
/// 二进制格式」。文件只读,天然线程安全。

#[derive(Debug)]
pub struct MmapDict {
    map: Mmap,
    pinyin_count: usize,
    total_entries: u64,
    /// 「音节链 → 存在性」的**跨按键备忘**（简拼弧裁剪用）。
    ///
    /// 打字时同一批前缀会被反复查询：打 `wom` 时查过 `wo`/`wo men`，打 `women`
    /// 时还会再查一次。每次查询是一次跨页 mmap 二分（实测 ~1µs，全库顺序读完
    /// 1.73M 条 key 只要 41ms，说明成本在页错误而不是比较），缓存命中就把它省掉。
    /// 上限到了整体清空即可——它只是备忘，不清也不会错。
    records_off: usize,
    strings_off: usize,
    entries_off: usize,
    /// 全表词频总和（懒扫描一次后缓存；`Dictionary::total_freq` 用）。
    total_freq: OnceLock<u64>,
    /// 简拼副索引(可选;缺省时纯简拼查询优雅退化为空,引擎走精确路径)。
    /// 音节表不随它走(编译期静态表 `segmenter::SYLLABLES`),因此混合简拼
    /// 在副索引缺失时照常工作——绝不回退到「懒扫描主索引建表」(那次扫描
    /// 实测约 840ms,且发生在按键处理路径上,不可接受)。
    abbrev: Option<AbbrevIndex>,
    /// 音节级转移索引(可选;缺省时缩写弧回退到逐次 `key_prefix_state` 二分)。
    graph: Option<SyllableGraph>,
}

/// 词库存取值：运行时用只读 mmap 二进制（[`MmapDict`]），
/// 内存词库/测试/迁移用 SQLite（[`SqliteDict`]）。
///
/// 保持"具体类型"的偏好：枚举替代 trait object，字段访问直接。
#[derive(Debug)]
pub enum DictStore {
    /// SQLite 词库（内存/测试/迁移）。
    Sqlite(SqliteDict),
    /// 只读 mmap 二进制词库（运行时主力）。
    Mmap(MmapDict),
}

impl DictStore {
    /// 词条写入（仅 SQLite 变体支持；mmap 只读，返回错误）。
    ///
    /// 供测试与工具在内存/临时词库上建数据用；运行时不写词库。
    pub fn upsert(&self, word: &str, pinyin: &str, freq: u64) -> Result<(), DictError> {
        match self {
            Self::Sqlite(d) => d.upsert(word, pinyin, freq),
            Self::Mmap(_) => Err(DictError::ReadOnly),
        }
    }

    /// 装配简拼副索引(仅 Mmap 变体有效;Sqlite 变体原样返回)。
    ///
    /// 转发到 [`MmapDict::with_abbrev`]:副索引缺失/损坏时仅告警,
    /// 绝不 panic。引擎层(`lookup_candidates` 的简拼合并)据此拿到
    /// 简拼能力;不装配时简拼查询返回空,精确/模糊行为不变。
    pub fn with_abbrev<P: AsRef<Path>>(self, path: P) -> Self {
        match self {
            Self::Mmap(d) => Self::Mmap(d.with_abbrev(path)),
            Self::Sqlite(_) => self,
        }
    }

    /// 是否已装配简拼副索引(测试与诊断用)。
    pub fn has_abbrev(&self) -> bool {
        match self {
            Self::Mmap(d) => d.has_abbrev(),
            Self::Sqlite(_) => false,
        }
    }

    /// 装配音节级转移索引(仅 Mmap 变体有效)。
    ///
    /// 与副索引同一套约定：缺失/损坏/不同代只告警，不装配也不 panic；
    /// 没有它简拼弧仍能工作，只是每次去主索引二分。
    pub fn with_syllable_graph<P: AsRef<Path>>(self, path: P) -> Self {
        match self {
            Self::Mmap(d) => Self::Mmap(d.with_syllable_graph(path)),
            Self::Sqlite(_) => self,
        }
    }

    /// 是否已装配音节级索引(测试与诊断用)。
    pub fn has_syllable_graph(&self) -> bool {
        match self {
            Self::Mmap(d) => d.syllable_graph().is_some(),
            Self::Sqlite(_) => false,
        }
    }
}

impl Dictionary for DictStore {
    fn lookup(&self, pinyin: &str) -> Result<Vec<Candidate>, DictError> {
        match self {
            Self::Sqlite(d) => d.lookup(pinyin),
            Self::Mmap(d) => d.lookup(pinyin),
        }
    }

    /// 转发到内层。**这条转发是简拼弧剪枝的正确性前提**：漏了它，引擎拿到
    /// 的是 trait 默认值 `None`，每个简拼分支都会被当成死分支剪掉
    /// （实测症状：`wmjtqbj` 连一个简拼候选都不出）。
    fn key_prefix_state(&self, pinyin: &str) -> Result<KeyPrefix, DictError> {
        match self {
            Self::Sqlite(_) => Ok(KeyPrefix::None),
            Self::Mmap(d) => d.key_prefix_state(pinyin),
        }
    }

    /// 同 [`Dictionary::key_prefix_state`] 的转发教训：漏了它引擎以为没有索引，
    /// 缩写弧全部退回 mmap 二分（功能对、速度丢），且不会有任何报错。
    fn syllable_graph(&self) -> Option<&SyllableGraph> {
        match self {
            Self::Sqlite(_) => None,
            Self::Mmap(d) => d.syllable_graph(),
        }
    }

    fn key_prefix_state_in(
        &self,
        pinyin: &str,
        lo: usize,
        hi: usize,
    ) -> Result<(KeyPrefix, usize), DictError> {
        match self {
            Self::Sqlite(_) => Ok((KeyPrefix::None, 0)),
            Self::Mmap(d) => d.key_prefix_state_in(pinyin, lo, hi),
        }
    }


    fn lookup_prefix(&self, pinyin: &str) -> Result<Vec<Candidate>, DictError> {
        match self {
            Self::Sqlite(d) => d.lookup_prefix(pinyin),
            Self::Mmap(d) => d.lookup_prefix(pinyin),
        }
    }

    fn lookup_prefix_limited(
        &self,
        pinyin: &str,
        limit: usize,
    ) -> Result<Vec<Candidate>, DictError> {
        match self {
            Self::Sqlite(d) => d.lookup_prefix_limited(pinyin, limit),
            Self::Mmap(d) => d.lookup_prefix_limited(pinyin, limit),
        }
    }

    fn total_freq(&self) -> Result<u64, DictError> {
        match self {
            Self::Sqlite(d) => d.total_freq(),
            Self::Mmap(d) => d.total_freq(),
        }
    }
}

/// 从字节切片读小端 u16（越界返回损坏错误）。
pub(super) fn read_u16(data: &[u8], off: usize) -> Result<u16, DictError> {
    let arr: [u8; 2] = data
        .get(off..off + 2)
        .ok_or(DictError::Corrupt("数据越界"))?
        .try_into()
        .map_err(|_| DictError::Corrupt("数据越界"))?;
    Ok(u16::from_le_bytes(arr))
}

/// 从字节切片读小端 u32（越界返回损坏错误）。
pub(super) fn read_u32(data: &[u8], off: usize) -> Result<u32, DictError> {
    let arr: [u8; 4] = data
        .get(off..off + 4)
        .ok_or(DictError::Corrupt("数据越界"))?
        .try_into()
        .map_err(|_| DictError::Corrupt("数据越界"))?;
    Ok(u32::from_le_bytes(arr))
}

/// 从字节切片读小端 u64（越界返回损坏错误）。
pub(super) fn read_u64(data: &[u8], off: usize) -> Result<u64, DictError> {
    let arr: [u8; 8] = data
        .get(off..off + 8)
        .ok_or(DictError::Corrupt("数据越界"))?
        .try_into()
        .map_err(|_| DictError::Corrupt("数据越界"))?;
    Ok(u64::from_le_bytes(arr))
}

#[cfg(test)]
mod tests;
