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

use std::cmp::Ordering;
use std::fs::File;
use std::io::BufRead;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use memmap2::Mmap;
use rusqlite::{params, Connection};
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
/// 全拼路径枚举的音节切分上限(每种切法都要回主索引扫一轮,限制枚举防爆炸)。
/// 只服务于 [`MmapDict::lookup_mixed_full_pinyin`](改动前的原实现)。
const MAX_SEGMENTATIONS: usize = 8;

/// 交错简拼 DFS 单次查询的**索引探测步数**上限(见 [`MmapDict::lookup_mixed`])。
///
/// 口径:一次二分迭代、一次记录读取各记 1 步。DFS 的每一项代价最终都落在
/// 「在主索引上定位或读取一条记录」上,用同一种步数计量才能把「多分支枚举」
/// 与「单次二分」放在同一把尺子上——这是本常量唯一的作用,**不是时间**,
/// 因为时间随机器变、步数不随。
///
/// 取值依据(`tools/spike_abbrev/walk.py` 原型在真实 `data/dict.bin`
/// 186 万 key 上实测的二分次数):
/// - 真实交错输入远低于此:`feicde` 248、`zguo` 570、`nhao` 962、
///   `beijdx` 1493、`shouzm` 1923、`zgren` 2085;
/// - 病态输入才会逼近:`zzzz` 2831、`zhzhzhzh` 6922;
/// - 20_000 步给真实输入留约 10 倍余量,同时把任意病态输入的最坏情况
///   钳成常数(与 `MAX_CHAINS_PER_START` / `SYLLABLE_ENUM_QUOTA` 同一种
///   「硬上限保证最坏情况有界」的思路)。
///
/// 预算耗尽时**保留已经收下的候选**(部分结果优于空结果),不报错。
const MAX_MIXED_DFS_STEPS: usize = 20_000;

/// 混合简拼的**探索上界**:收集池到这个条数就停止继续探索。
///
/// 取 [`MAX_LOOKUP_RESULTS`] 的 8 倍(4000),是实测标定的折中:
/// - 早了不行——在 500(最终名额)就停时,截断按**探索顺序**发生,实测
///   `meiy` 把词频最高的「没有」(504685,本该第 1)丢了,列表第 1 名
///   「每一个」只有 500354;
/// - 晚了也不行——完全不设上界时 DFS 会跑满 [`MAX_MIXED_DFS_STEPS`]
///   预算,`zhongg` 单次查询 0.343ms → **4.13ms**(5ms 门槛);
/// - 4000 配上「整 key 跳过」后,`zhongg` 2.8ms、`meiy` 1.3ms,
///   而 26 个抽样输入的候选条数**全部不变**(只有 `meiy`/`zhongg`/`nih`
///   这类「一个音节位下有几百个 key」的输入内容变好)。
///
/// 真正的质量决策仍然只在**[排序之后]**做(按词频取前
/// [`MAX_LOOKUP_RESULTS`] 条),这里只是「看多少」的上界。
const MAX_MIXED_COLLECT: usize = MAX_LOOKUP_RESULTS as usize * 8;


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

/// 词库抽象：按拼音查词。
pub trait Dictionary: Send + Sync {
    /// 精确查词：`pinyin` 可以是单音节（`"ni"`）或多音节短语（`"ni hao"`）。
    fn lookup(&self, pinyin: &str) -> Result<Vec<Candidate>, DictError>;

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

/// 基于 SQLite 的词库。
///
/// `rusqlite::Connection` 不是 `Sync`，这里用 `Mutex` 包一层，词库对象
/// 因此可以在多线程间共享（`ImeEngine` 随之变成 `Sync`，可供 IBus 适配层
/// 的多个输入会话并发查词；每次查词短暂持锁，命中率足够）。
/// TODO: 若查询成为瓶颈，换成连接池或只读 mmap 实现。
#[derive(Debug)]
pub struct SqliteDict {
    conn: Mutex<Connection>,
}

/// 词库建表语句。
///
/// TODO: 后续需要支持：
/// - `user_entries`（用户自造词，与系统词库分离，允许增删）；
/// - 分词/词性等附加列（`data/README.md` 中有规划）。
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS entries (
    id     INTEGER PRIMARY KEY AUTOINCREMENT,
    pinyin TEXT    NOT NULL,            -- 拼音，多音节空格分隔，如 'ni hao'
    word   TEXT    NOT NULL,            -- 词语
    freq   INTEGER NOT NULL DEFAULT 0   -- 静态词频
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_entries_pinyin_word ON entries(pinyin, word);
CREATE INDEX IF NOT EXISTS idx_entries_pinyin ON entries(pinyin);
";

impl SqliteDict {
    /// 打开（必要时创建）指定路径的词库文件；父目录不存在时自动创建。
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, DictError> {
        let path = path.as_ref();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let conn = Connection::open(path)?;
        let dict = Self { conn: Mutex::new(conn) };
        dict.create_schema()?;
        Ok(dict)
    }

    /// 打开内存词库（测试 / 无持久化场景）。
    pub fn open_in_memory() -> Result<Self, DictError> {
        let conn = Connection::open_in_memory()?;
        let dict = Self { conn: Mutex::new(conn) };
        dict.create_schema()?;
        Ok(dict)
    }

    /// 建表（幂等）。数据变更/迁移统一放这里，TODO: 引入 schema 版本号。
    pub fn create_schema(&self) -> Result<(), DictError> {
        let conn = self.conn.lock().map_err(|_| DictError::LockPoisoned)?;
        conn.execute_batch(SCHEMA)?;
        Ok(())
    }

    /// 插入（或累加）一条词条。同一 (pinyin, word) 已存在时累加词频。
    pub fn upsert(&self, word: &str, pinyin: &str, freq: u64) -> Result<(), DictError> {
        let conn = self.conn.lock().map_err(|_| DictError::LockPoisoned)?;
        conn.execute(
            "INSERT INTO entries (pinyin, word, freq) VALUES (?1, ?2, ?3)
             ON CONFLICT(pinyin, word) DO UPDATE SET freq = freq + excluded.freq",
            params![pinyin, word, freq],
        )?;
        Ok(())
    }

    /// 从 TSV 流批量导入词条。
    ///
    /// 每行格式：`拼音<TAB>词语<TAB>频率`，空行与 `#` 注释行跳过。
    /// 整个导入在一个事务里完成，中途出错则整体回滚。
    pub fn import_tsv(&self, reader: impl BufRead) -> Result<u64, DictError> {
        let conn = self.conn.lock().map_err(|_| DictError::LockPoisoned)?;
        let tx = conn.unchecked_transaction()?;
        let mut imported = 0u64;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO entries (pinyin, word, freq) VALUES (?1, ?2, ?3)
                 ON CONFLICT(pinyin, word) DO UPDATE SET freq = freq + excluded.freq",
            )?;
            for (i, line) in reader.lines().enumerate() {
                let line_no = (i + 1) as u64;
                let line = line?;
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let mut parts = line.split('\t');
                let (Some(pinyin), Some(word), Some(freq)) =
                    (parts.next(), parts.next(), parts.next())
                else {
                    return Err(DictError::BadTsvLine {
                        line: line_no,
                        detail: format!("期望 3 个制表符分隔的字段，实际: {line:?}"),
                    });
                };
                let freq: u64 = freq.trim().parse().map_err(|e| DictError::BadTsvLine {
                    line: line_no,
                    detail: format!("频率不是整数: {freq:?} ({e})"),
                })?;
                stmt.execute(params![pinyin.trim(), word.trim(), freq])?;
                imported += 1;
            }
        }
        tx.commit()?;
        Ok(imported)
    }

    /// 词条总数（调试/统计用）。
    pub fn count(&self) -> Result<u64, DictError> {
        let conn = self.conn.lock().map_err(|_| DictError::LockPoisoned)?;
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))?;
        Ok(n as u64)
    }
}

impl Dictionary for SqliteDict {
    fn lookup(&self, pinyin: &str) -> Result<Vec<Candidate>, DictError> {
        let conn = self.conn.lock().map_err(|_| DictError::LockPoisoned)?;
        let mut stmt = conn.prepare(
            "SELECT word, pinyin, freq FROM entries
             WHERE pinyin = ?1
             ORDER BY freq DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![pinyin, MAX_LOOKUP_RESULTS], |row| {
            Ok(Candidate {
                word: row.get(0)?,
                pinyin: row.get(1)?,
                score: 0.0, // 得分由引擎统一计算
                freq: row.get(2)?,
                spelling: crate::candidate::SpellingKind::Exact, // 词典条目是精确匹配
                echo: false,  // 词典条目是真实候选
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn lookup_prefix(&self, pinyin: &str) -> Result<Vec<Candidate>, DictError> {
        self.lookup_prefix_limited(pinyin, MAX_LOOKUP_RESULTS as usize)
    }

    fn lookup_prefix_limited(
        &self,
        pinyin: &str,
        limit: usize,
    ) -> Result<Vec<Candidate>, DictError> {
        let conn = self.conn.lock().map_err(|_| DictError::LockPoisoned)?;
        let mut stmt = conn.prepare(
            "SELECT word, pinyin, freq FROM entries
             WHERE pinyin LIKE (?1 || '%') ESCAPE '\\'
             ORDER BY freq DESC
             LIMIT ?2",
        )?;
        // 转义 LIKE 通配符，防止用户输入 %/_ 影响查询。
        let escaped = pinyin.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        let limit = (limit as i64).min(MAX_LOOKUP_RESULTS);
        let rows = stmt.query_map(
            params![escaped, limit],
            |row| {
                Ok(Candidate {
                    word: row.get(0)?,
                    pinyin: row.get(1)?,
                    score: 0.0,
                    freq: row.get(2)?,
                    spelling: crate::candidate::SpellingKind::Exact, // 词典条目是精确匹配
                    echo: false,  // 词典条目是真实候选
                })
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn total_freq(&self) -> Result<u64, DictError> {
        let conn = self.conn.lock().map_err(|_| DictError::LockPoisoned)?;
        Ok(conn.query_row("SELECT COALESCE(SUM(freq), 0) FROM entries", [], |r| r.get(0))?)
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

#[derive(Debug)]
pub struct MmapDict {
    map: Mmap,
    pinyin_count: usize,
    total_entries: u64,
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
}

impl MmapDict {
    /// 打开并 mmap 映射一个 dict.bin 词库文件。
    ///
    /// 校验魔数与版本，损坏文件返回 [`DictError::Corrupt`]。
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, DictError> {
        let file = File::open(path)?;
        let map = unsafe { Mmap::map(&file)? };
        Self::from_bytes(map)
    }

    /// 从已映射字节解析头部（独立成函数便于单测构造坏文件）。
    fn from_bytes(map: Mmap) -> Result<Self, DictError> {
        let bytes: &[u8] = &map;
        if bytes.len() < MMAP_HEADER_LEN {
            return Err(DictError::Corrupt("文件过短，不是完整 dict.bin"));
        }
        if bytes.get(0..8) != Some(MMAP_MAGIC.as_slice()) {
            return Err(DictError::Corrupt("魔数不匹配，不是 dict.bin 词库"));
        }
        if read_u32(bytes, 8)? != MMAP_VERSION {
            return Err(DictError::Corrupt("dict.bin 版本不兼容"));
        }
        // 头部字段先全部读出再构造结构体：`bytes` 借用 `map`，
        // 与字段读取（借用）同一表达式中不能再移动 `map`。
        let pinyin_count = read_u64(bytes, 12)? as usize;
        let total_entries = read_u64(bytes, 20)?;
        let records_off = read_u64(bytes, 28)? as usize;
        let strings_off = read_u64(bytes, 36)? as usize;
        let entries_off = read_u64(bytes, 44)? as usize;
        Ok(Self {
            map,
            pinyin_count,
            total_entries,
            records_off,
            strings_off,
            entries_off,
            total_freq: OnceLock::new(),
            abbrev: None,
        })
    }

    /// 装配简拼副索引(构建脚本 `scripts/abbrev_to_bin.py` 的产物)。
    ///
    /// 参照 `ImeEngine::new_with_lm` 对缺失 LM 的处理:副索引缺失/损坏时
    /// 仅记录 warning 并置 `None`,**绝不 panic**——此时简拼查询返回空,
    /// 引擎自然退回精确查询路径,不会静默变成空词库。
    pub fn with_abbrev<P: AsRef<Path>>(mut self, path: P) -> Self {
        match AbbrevIndex::open(path.as_ref()) {
            Ok(idx) => self.abbrev = Some(idx),
            Err(e) => {
                tracing::warn!(error = %e, path = %path.as_ref().display(),
                    "简拼副索引打开失败,简拼查询退化为空(精确查询不受影响)");
            }
        }
        self
    }

    /// 是否已装配简拼副索引(测试与诊断用)。
    pub fn has_abbrev(&self) -> bool {
        self.abbrev.is_some()
    }

    /// 词条总数（调试/统计用）。
    pub fn count(&self) -> u64 {
        self.total_entries
    }

    /// 边界检查的切片：越界一律返回损坏错误（不 panic）。
    fn slice(&self, off: usize, len: usize) -> Result<&[u8], DictError> {
        let end = off.checked_add(len).ok_or(DictError::Corrupt("区段长度溢出"))?;
        self.map.get(off..end).ok_or(DictError::Corrupt("区段越界"))
    }

    /// 第 `i` 条拼音索引记录（数组内按拼音字节序排序）。
    fn record(&self, i: usize) -> Result<MmapRecord, DictError> {
        let b = self.slice(self.records_off + i * MMAP_RECORD_LEN, MMAP_RECORD_LEN)?;
        Ok(MmapRecord {
            str_off: read_u32(b, 0)? as usize,
            str_len: read_u16(b, 4)? as usize,
            ent_off: read_u32(b, 6)? as usize,
            ent_len: read_u16(b, 10)? as usize,
        })
    }

    /// 精确查找：返回拼音 key 对应的索引记录；不存在返回 `None`。
    fn find(&self, pinyin: &str) -> Result<Option<MmapRecord>, DictError> {
        let key = pinyin.as_bytes();
        let (mut lo, mut hi) = (0usize, self.pinyin_count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let rec = self.record(mid)?;
            // str_off 是相对拼音区的偏移，需加区基址
            let s = self.slice(self.strings_off + rec.str_off, rec.str_len)?;
            match s.cmp(key) {
                Ordering::Less => lo = mid + 1,
                Ordering::Greater => hi = mid,
                Ordering::Equal => return Ok(Some(rec)),
            }
        }
        Ok(None)
    }

    /// 前缀查找的下界：第一条以 `prefix` 开头的记录下标。
    fn lower_bound(&self, prefix: &[u8]) -> Result<usize, DictError> {
        let (mut lo, mut hi) = (0usize, self.pinyin_count);
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

    /// 解析一个词块（word_len:u8 + word + freq:u32 LE，按 freq 降序）。
    /// 某个 key 的**最高词频**(词块里第一个词条的 freq)。
    ///
    /// 词块内 freq 降序(与 `abbrev.bin` 同约定,8 个 key 抽样实测非升序,
    /// 含「一」130 条的大 key),所以这就是该 key 的天花板。
    /// 只读头部几字节、**不做任何分配**——DFS 拿它做「整 key 跳过」的
    /// 分支界限(见 [`MixedDfs::collect`]),每次跳过一个 key 省下的是
    /// 几十次词条解析 + 字符串分配。
    fn first_entry_freq(&self, ent_off: usize, ent_len: usize) -> Result<u64, DictError> {
        let block = self.slice(ent_off, ent_len)?;
        let wl = *block.first().ok_or(DictError::Corrupt("词块为空"))? as usize;
        Ok(read_u32(block, 1 + wl)? as u64)
    }

    fn parse_entries(&self, ent_off: usize, ent_len: usize) -> Result<Vec<(String, u64)>, DictError> {
        let block = self.slice(ent_off, ent_len)?;
        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos < block.len() {
            let wl = *block.get(pos).ok_or(DictError::Corrupt("词块越界"))? as usize;
            let word_bytes = block
                .get(pos + 1..pos + 1 + wl)
                .ok_or(DictError::Corrupt("词块越界"))?;
            let freq = read_u32(block, pos + 1 + wl)?;
            let word = std::str::from_utf8(word_bytes)
                .map_err(|_| DictError::Corrupt("词条不是 UTF-8"))?;
            out.push((word.to_owned(), freq as u64));
            pos += 1 + wl + 4;
        }
        Ok(out)
    }

    /// 线性扫词条区求词频总和（约 80MB / 186 万条，仅首次调用时执行一次）。
    fn scan_total_freq(&self) -> u64 {
        let bytes: &[u8] = &self.map;
        let mut pos = self.entries_off;
        let mut total = 0u64;
        while pos < bytes.len() {
            let Some(&wl) = bytes.get(pos) else { break };
            let freq_off = pos + 1 + wl as usize;
            let Some(freq) = bytes.get(freq_off..freq_off + 4) else { break };
            total += u32::from_le_bytes(freq.try_into().expect("长度已校验")) as u64;
            pos = freq_off + 4;
        }
        total
    }

    /// 合法单音节表(混合简拼切分用),直接返回编译进二进制的静态表。
    ///
    /// 音节表是**编译期静态数据**(`crate::segmenter::SYLLABLES`,410 项),
    /// 这里只是零开销转发,**混合简拼不再依赖副索引存在**(此前音节表
    /// 从 abbrev.bin v2 音节表段读,`MmapDict::open()` 不挂副索引时混合
    /// 简拼会退化为空)。词库实际用到 412 个音节,静态表覆盖其中 410 个;
    /// 缺失的 `fiao`(方言)/`tei`(拟声)是**刻意取舍**——把它们加进
    /// `SYLLABLES` 会动主切分路径、影响全拼行为,故这两个音节的混合简拼
    /// 切分不支持。
    ///
    /// 绝不能回退到「懒扫描主索引 173 万 key 建表」:那次扫描实测约
    /// 840ms,且首次扫描发生在**按键处理路径上**(IBus 同步等待),不可接受。
    fn syllable_table(&self) -> &'static [&'static str] {
        crate::segmenter::SYLLABLES
    }

    /// 纯简拼查询:`zgr` → 直接二分副索引,返回带全拼的词条。
    ///
    /// 副索引缺失时返回空(优雅退化,调用方走精确路径)。结果带早停:
    /// 凑够 `MAX_LOOKUP_RESULTS` 即止,不物化全部命中。
    pub fn lookup_abbrev(&self, abbrev: &str) -> Result<Vec<Candidate>, DictError> {
        let Some(idx) = &self.abbrev else {
            return Ok(Vec::new());
        };
        idx.lookup_prefix(abbrev)
    }

    /// 混合简拼查询:`zhongguor` → 中国人,`nhao` → 您好。
    ///
    /// 词库主索引的 key 是**带空格的音节序列**(`"zhong guo ren"`),用户输入
    /// 不带空格,所以直接前缀匹配恒为 0。查询分两条路:
    ///
    /// - **输入本身就是合法全拼** → 走 [`Self::lookup_mixed_full_pinyin`],
    ///   也就是**改动前的原实现,逐字节不变**;
    /// - **否则** → 走下面这条**交错 DFS**,把首字母也当作音节来切。
    ///
    /// # 为什么必须分路(冻结约束的落点)
    ///
    /// 项目硬约束是「全拼已经跑通的行为不许退化」。交错 DFS 单独用会**破坏**
    /// 它:因为 `a`/`e`/`o` 是单字母音节,`hao` 也能被解释成
    /// `h(e|ou) + ao`,实测给「hao」多加了「后澳」「河岙」——那不是用户的
    /// 本意,**用户打的就是全拼**。
    ///
    /// 所以这里按「输入能否整体切成年节」分流。这不是「测出来一样」,
    /// 而是**结构性保证**:全拼输入走的是与改动前完全相同的代码路径,
    /// 对拍逐字节相同是必然的,不依赖测量。
    ///
    /// 这条分流也正好是「混合简拼」的定义:混合形式之所以存在,恰恰因为
    /// 有一段**不是**全拼。`nhao`(n 不是音节)、`feicde`、`shouzm`、
    /// `zhongg`、`zhongguor` 都属于这一类,交错 DFS 正是为它们而写。
    ///
    /// # 交错 DFS 的形状
    ///
    /// 每个位置要么消耗一个**字面音节**(全拼),要么消耗**一个首字母**
    /// (`zh`/`ch`/`sh` 额外试 2 字符)作为下一音节的声母,后者把该音节的
    /// 剩余部分**从索引里枚举出来**再往下走。旧实现只有「完整音节前缀 +
    /// **尾部**残余声母」,结构上只支持缩写全在末尾;`nhao`(缩写**在开头**)、
    /// `feicde`(缩写**在中间**)在旧实现下召回为 0。
    ///
    /// **至少要有一个完整音节**(`used_literal`):纯首字母串(`zgr`、`n`)是
    /// [`Self::lookup_abbrev`] 副索引的职责,本函数对它们返回空——这与旧实现
    /// `if syls.is_empty() { continue; }` 的口径**完全一致**,不是新加的限制。
    ///
    /// 收集口径(与旧实现的 `tail` 分支逐条对齐):
    /// - 前缀是**完整音节序列**时只收 `key == 前缀` 的整条词条;
    /// - 前缀停在**残缺音节**上时(末位首字母)收该前缀下的全部词条
    ///   (`zhongg` → `zhong g*` → 中国 / 中国人)。
    ///
    /// 候选的 `pinyin` **直接取索引 key**(完整音节序列),因此 `nhao` 命中的
    /// 「您好」返回的是 `nin hao` 而不是被截断的 `n hao`——**不需要任何回填**。
    ///
    /// 二分用哨兵 `'!'`(0x21)枚举同一音节位的不同取值:它 > 空格、
    /// < 任何小写字母,于是 `lower_bound(前缀 + 已取音节 + '!')` 既跳过该取值
    /// (含 `ta ...` 这类后继),又**保留** `ta`/`tan` 这类更长的同前缀音节
    /// (用 `\xff` 会把后者一并跳掉)。
    ///
    /// **成本**:单次查询的索引探测步数硬上限为 [`MAX_MIXED_DFS_STEPS`]
    /// (20_000 步),预算耗尽即停并保留已收候选——病态输入(`zzzz`、
    /// `zhzhzhzh`)的最坏情况因此是常数;收集池到 [`MAX_MIXED_COLLECT`]
    /// 也停。**注意截断只发生在排序之后**(按词频取前
    /// [`MAX_LOOKUP_RESULTS`] 条),收集期不做任何「谁先到谁留下」的决策
    /// ——那正是 2026-10-02 修掉的 bug(`meiy` 下词频最高的「没有」被
    /// 500 条早停丢掉,详见 [`MmapDict::lookup_mixed`] 的实现注释)。
    ///
    /// 音节表来自编译进二进制的静态表 `crate::segmenter::SYLLABLES`(410 项),
    /// 零扫描;**不依赖副索引存在**——本查询只走主索引,
    /// `MmapDict::open()` 未装配 abbrev.bin 时照常工作。
    pub fn lookup_mixed(&self, input: &str) -> Result<Vec<Candidate>, DictError> {
        let table = self.syllable_table();
        if table.is_empty() || input.is_empty() || !input.is_ascii() {
            return Ok(Vec::new());
        }
        if is_full_pinyin(input, table) {
            return self.lookup_mixed_full_pinyin(input);
        }
        // 按长度降序:字面音节分支贪心最长优先(与旧 segment_input 一致)。
        let mut by_len: Vec<&'static str> = table.to_vec();
        by_len.sort_by_key(|s| std::cmp::Reverse(s.len()));

        let mut best: std::collections::HashMap<(String, String), u64> =
            std::collections::HashMap::new();
        {
            let mut dfs = MixedDfs {
                dict: self,
                input: input.as_bytes(),
                by_len,
                out: &mut best,
                budget: MAX_MIXED_DFS_STEPS,
                threshold: 0,
                threshold_at: 0,
            };
            dfs.dfs(0, &[], false, false)?;
        }
        let mut out: Vec<Candidate> = best
            .into_iter()
            .map(|((word, py), freq)| Candidate::new(word, py, freq))
            .collect();
        out.sort_by_key(|c| std::cmp::Reverse(c.freq));
        // **按词频截断**(而不是收集期先到先得):上限仍是 MAX_LOOKUP_RESULTS,
        // 但留下的必须是分最高的那批。实测 `meiy`:旧口径 500 条里没有
        // 「没有」(词频 504685,应为第 1),因为它的枚举分支排在靠后位置,
        // 名额先被「每一个」(500354)等占满了。
        out.truncate(MAX_LOOKUP_RESULTS as usize);
        Ok(out)
    }
    /// 混合简拼的**全拼**路径:**改动前的原实现,逐字节不动**。
    ///
    /// 只在 [`is_full_pinyin`] 为真时调用(见 [`Self::lookup_mixed`])。
    /// 刻意保留原实现而不是「让 DFS 只走字面分支」:后者只是*看起来*等价,
    /// 而冻结约束要的是**同一条代码路径**给出的硬保证。
    fn lookup_mixed_full_pinyin(&self, input: &str) -> Result<Vec<Candidate>, DictError> {
        let table = self.syllable_table();
        if table.is_empty() || input.is_empty() {
            return Ok(Vec::new());
        }
        let segs = segment_input(input, table);
        // 按 (词, 全拼) 去重,保留最高 freq;早停在收集阶段生效。
        let mut best: std::collections::HashMap<(String, String), u64> =
            std::collections::HashMap::new();
        'outer: for (syls, tail) in &segs {
            if syls.is_empty() {
                continue;
            }
            let prefix = syls.join(" ");
            let pb = prefix.as_bytes();
            let start = self.lower_bound(pb)?;
            for i in start..self.pinyin_count {
                let rec = self.record(i)?;
                let key = self.slice(self.strings_off + rec.str_off, rec.str_len)?;
                if !key.starts_with(pb) {
                    break;
                }
                // key == prefix(完整命中,仅当无残余声母时收录)
                if key.len() == pb.len() {
                    if tail.is_empty() {
                        for (word, freq) in
                            self.parse_entries(self.entries_off + rec.ent_off, rec.ent_len)?
                        {
                            let py = std::str::from_utf8(key)
                                .map_err(|_| DictError::Corrupt("拼音不是合法文本"))?
                                .to_owned();
                            let e = best.entry((word, py)).or_insert(0);
                            if freq > *e {
                                *e = freq;
                            }
                            if best.len() >= MAX_LOOKUP_RESULTS as usize {
                                break 'outer;
                            }
                        }
                    }
                    continue;
                }
                // key 必须以 prefix + ' ' 开头才是「该前缀的更长词」
                if key[pb.len()] != b' ' {
                    continue;
                }
                if tail.is_empty() {
                    continue;
                }
                let rest = &key[pb.len() + 1..];
                let rest_syls: Vec<&[u8]> = rest.split(|&b| b == b' ').collect();
                let tb = tail.as_bytes();
                // 残余声母须逐位等于后续音节首字母,且长度不超过剩余音节数
                if tb.len() <= rest_syls.len()
                    && rest_syls
                        .iter()
                        .zip(tb.iter())
                        .all(|(syl, &c)| !syl.is_empty() && syl[0] == c)
                {
                    let py = std::str::from_utf8(key)
                        .map_err(|_| DictError::Corrupt("拼音不是合法文本"))?
                        .to_owned();
                    for (word, freq) in
                        self.parse_entries(self.entries_off + rec.ent_off, rec.ent_len)?
                    {
                        let e = best.entry((word, py.clone())).or_insert(0);
                        if freq > *e {
                            *e = freq;
                        }
                        if best.len() >= MAX_LOOKUP_RESULTS as usize {
                            break 'outer;
                        }
                    }
                }
            }
        }
        let mut out: Vec<Candidate> = best
            .into_iter()
            .map(|((word, py), freq)| Candidate::new(word, py, freq))
            .collect();
        out.sort_by_key(|c| std::cmp::Reverse(c.freq));
        Ok(out)
    }

}

/// 输入能否被**完整**切成一串音节(即「整串都是合法全拼」)。
///
/// 用 DP 而不是复用 [`segment_input`]:后者有 `MAX_SEGMENTATIONS` 上限,
/// 前 8 种切法都不完整时会**误判**成「不是全拼」,从而把一批全拼输入
/// 错送进交错 DFS——冻结约束就会漏。这里 `reachable[i]` = 前缀
/// `input[..i]` 可完整切分,与切法条数无关。
///
/// 调用方已保证 `input` 是 ASCII,故按字节索引安全。
fn is_full_pinyin(input: &str, table: &[&str]) -> bool {
    let n = input.len();
    let mut reachable = vec![false; n + 1];
    reachable[0] = true;
    for i in 0..n {
        if !reachable[i] {
            continue;
        }
        for s in table {
            if input[i..].starts_with(s) {
                reachable[i + s.len()] = true;
            }
        }
    }
    reachable[n]
}

/// 交错简拼 DFS 的状态(见 [`MmapDict::lookup_mixed`])。
///
/// 独立成结构体而不是闭包:递归要在「只读词库」与「可变收集池」之间同时持有
/// 两份借用,闭包写不出这种分离;结构体字段天然分离,也顺带把「步数预算」
/// 这种横切状态从递归参数里拿出来。
struct MixedDfs<'a> {
    dict: &'a MmapDict,
    /// 输入字节(上游已拒绝非 ASCII)。
    input: &'a [u8],
    /// `SYLLABLES` 按长度降序(字面分支贪心最长优先)。
    by_len: Vec<&'static str>,
    /// 收集池:按 (词, 全拼) 去重,保留最高 freq。
    out: &'a mut std::collections::HashMap<(String, String), u64>,
    /// 剩余索引探测步数(二分迭代或读记录各 1 步),归零即停。
    budget: usize,
    /// 收集池第 [`MAX_LOOKUP_RESULTS`] 名的**下界**(0 = 还没收满)。
    ///
    /// 只作下界用:它是「已收集集合」的第 N 名,后续还会收到更高分的条目,
    /// 真实的第 N 名只会更高——用下界做整 key 跳过**不会误杀**
    /// (见 [`MixedDfs::collect`])。
    threshold: u64,
    /// 上次算 `threshold` 时收集池的大小(翻倍才重算,O(log) 次)。
    threshold_at: usize,
}

/// 把 `part` 接到空格分隔的前缀 `prefix` 之后(空前缀不加前导空格)。
fn joined(prefix: &[u8], part: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(prefix.len() + part.len() + 1);
    v.extend_from_slice(prefix);
    if !prefix.is_empty() {
        v.push(b' ');
    }
    v.extend_from_slice(part);
    v
}

impl MixedDfs<'_> {
    /// 前缀 `prefix` 区间下界;**每次二分迭代消耗 1 步预算**。
    fn lower_bound(&mut self, prefix: &[u8]) -> Result<usize, DictError> {
        let dict = self.dict;
        let (mut lo, mut hi) = (0usize, dict.pinyin_count);
        while lo < hi {
            if self.budget == 0 {
                // 预算耗尽:返回「空区间」下标,让调用方的 `starts_with`
                // 检查自然失败——已收候选保留,不再产出。
                return Ok(dict.pinyin_count);
            }
            self.budget -= 1;
            let mid = (lo + hi) / 2;
            let rec = dict.record(mid)?;
            let s = dict.slice(dict.strings_off + rec.str_off, rec.str_len)?;
            if s < prefix {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    }

    /// 读出第 `i` 条记录的 key(读记录 1 步)。返回 owned 值,避开「持有
    /// 词库切片的同时再 `&mut self` 二分」的借用冲突。
    fn key_at(&mut self, i: usize) -> Result<Vec<u8>, DictError> {
        let dict = self.dict;
        self.budget = self.budget.saturating_sub(1);
        let rec = dict.record(i)?;
        Ok(dict
            .slice(dict.strings_off + rec.str_off, rec.str_len)?
            .to_vec())
    }

    /// 枚举 `base` 区间内**本音节位**的所有不同取值(该位音节的剩余字母),
    /// 靠 `'!'` 哨兵二分跳转。见 [`MmapDict::lookup_mixed`] 的哨兵说明。
    fn distinct_tokens(&mut self, base: &[u8]) -> Result<Vec<Vec<u8>>, DictError> {
        let mut toks: Vec<Vec<u8>> = Vec::new();
        let mut i = self.lower_bound(base)?;
        loop {
            if i >= self.dict.pinyin_count || self.budget == 0 {
                return Ok(toks);
            }
            let k = self.key_at(i)?;
            if !k.starts_with(base) {
                return Ok(toks);
            }
            let rest = &k[base.len()..];
            let end = rest.iter().position(|&b| b == b' ').unwrap_or(rest.len());
            let t = rest[..end].to_vec();
            let mut jump = base.to_vec();
            jump.extend_from_slice(&t);
            jump.push(b'!');
            toks.push(t);
            i = self.lower_bound(&jump)?;
        }
    }

    /// 收集池超 [`MAX_MIXED_COLLECT`] 时**按词频**裁剪到一半。
    ///
    /// 为什么不早停:早停(收满即止)是按 DFS 探索顺序决定谁留下,会丢高频词
    /// ——实测 `meiy` 撞上 500 条早停时,词频最高的「没有」(504685)竟不在
    /// 列表里,而第 1 名「每一个」只有 500354。裁剪按分数,与探索顺序无关。
    /// 正常情况下 `len < MAX_MIXED_COLLECT`,本函数只做一次整数比较。
    /// 重新估计「收集池第 N 名的下界」——池子翻倍才重算一次。
    ///
    /// 不重算的代价只是少跳过几个 key(下界偏保守),不会误杀;每次重算
    /// 是 O(池子大小) 的选择,所以按 2 倍增长触发,O(log) 次。
    fn refresh_threshold(&mut self) {
        let n = MAX_LOOKUP_RESULTS as usize;
        if self.out.len() < n || self.out.len() < self.threshold_at.saturating_mul(2) {
            return;
        }
        self.threshold_at = self.out.len();
        let mut f: Vec<u64> = self.out.values().copied().collect();
        f.select_nth_unstable_by(n - 1, |a, b| b.cmp(a));
        self.threshold = f[n - 1];
    }

    /// 收下 `prefix` 区间内的词条。`mid_syllable` = 前缀停在残缺音节上
    /// (末位首字母),此时该前缀下的每个 key 都在这一位继续,全部收下;
    /// 否则前缀是完整音节序列,只收与它完全相等的 key。
    fn collect(&mut self, prefix: &[u8], mid_syllable: bool) -> Result<(), DictError> {
        let dict = self.dict;
        let mut i = self.lower_bound(prefix)?;
        while i < dict.pinyin_count {
            if self.budget == 0 {
                return Ok(());
            }
            self.refresh_threshold();
            self.budget = self.budget.saturating_sub(1);
            let rec = dict.record(i)?;
            let k = dict.slice(dict.strings_off + rec.str_off, rec.str_len)?;
            if !k.starts_with(prefix) {
                return Ok(());
            }
            // **整 key 跳过**(分支界限):词块内 freq 降序,若这个 key 的最高
            // 词频都低于「收集池第 N 名的下界」,它一条都进不了最终列表。
            // 没有这一条时,`zhongg` 要把「zhong g*」下几百个 key、8000+ 条
            // 词条全部解析并分配字符串:0.343ms → 5.35ms(破 5ms 门槛)。
            if self.out.len() >= MAX_LOOKUP_RESULTS as usize
                && dict.first_entry_freq(dict.entries_off + rec.ent_off, rec.ent_len)? < self.threshold
            {
                i += 1;
                continue;
            }
            // 完整音节前缀:key 按字节序排,与前缀相等的那条最先出现,
            // 之后的只会更长,故可直接收工(与旧实现 `key.len() == pb.len()`
            // 等价)。
            if !mid_syllable && k.len() != prefix.len() {
                return Ok(());
            }
            let py = std::str::from_utf8(k)
                .map_err(|_| DictError::Corrupt("拼音不是合法文本"))?
                .to_owned();
            for (word, freq) in dict.parse_entries(dict.entries_off + rec.ent_off, rec.ent_len)?
            {
                let e = self.out.entry((word, py.clone())).or_insert(0);
                if freq > *e {
                    *e = freq;
                }
            }
            i += 1;
        }
        Ok(())
    }

    /// DFS 主体:在 `pos` 处,`prefix` 是已消耗音节以空格连接的主索引前缀。
    ///
    /// `mid_syllable` = 前缀末尾是残缺音节;`used_literal` = 已消耗过至少一个
    /// **完整音节**(纯首字母串不算混合简拼,见 [`MmapDict::lookup_mixed`])。
    fn dfs(
        &mut self,
        pos: usize,
        prefix: &[u8],
        mid_syllable: bool,
        used_literal: bool,
    ) -> Result<(), DictError> {
        // 探索上界:`MAX_MIXED_COLLECT`(8 倍于最终名额)。**不是** 500:
        // 早停在 500 时按探索顺序截断会丢高频词(实测 `meiy` 把词频最高的
        // 「没有」丢了),而完全不设上界会让 DFS 跑满 20_000 步预算
        // (`zhongg` 0.343ms → 4.13ms)。8 倍是一个实测标定的折中。
        if self.budget == 0 || self.out.len() >= MAX_MIXED_COLLECT {
            return Ok(());
        }
        let input = self.input;
        if pos == input.len() {
            // 纯首字母串(`zgr`/`n`)= 副索引的职责,这里不产候选。
            if !used_literal {
                return Ok(());
            }
            return self.collect(prefix, mid_syllable);
        }
        let rest = &input[pos..];

        // (a) 字面音节分支(全拼)。
        let n = self.by_len.len();
        for idx in 0..n {
            let s: &'static str = self.by_len[idx];
            let sb = s.as_bytes();
            if !rest.starts_with(sb) {
                continue;
            }
            let np = joined(prefix, sb);
            self.dfs(pos + sb.len(), &np, false, true)?;
        }

        // (b) 首字母分支(简拼):1 字符;zh/ch/sh 额外试 2 字符。
        let mut cands: [&[u8]; 2] = [&rest[..1], &rest[..1]];
        let mut ncand = 1usize;
        if rest.len() >= 2 && matches!(&rest[..2], b"zh" | b"ch" | b"sh") {
            cands[1] = &rest[..2];
            ncand = 2;
        }
        for c in cands.into_iter().take(ncand) {
            let base = joined(prefix, c);
            if pos + c.len() == input.len() {
                // 末位首字母:音节残缺,按前缀收更长的词(声母语义)。
                self.dfs(pos + c.len(), &base, true, used_literal)?;
            } else {
                // 中间首字母:把该音节的剩余字母从索引里枚举出来再往下走。
                for t in self.distinct_tokens(&base)? {
                    let mut full = base.clone();
                    full.extend_from_slice(&t);
                    self.dfs(pos + c.len(), &full, false, used_literal)?;
                }
            }
        }
        Ok(())
    }
}

/// 把输入切成「完整音节段 + 残余声母」,枚举前 `MAX_SEGMENTATIONS` 种切法。
///
/// 在每个位置取**最长**合法音节优先递归;走不通时把剩余全部当声母残余。
/// 这是存储层查询的私有辅助,不改动 `segmenter.rs` 的全拼切分逻辑。
fn segment_input(input: &str, table: &[&str]) -> Vec<(Vec<String>, String)> {
    // 预按长度降序,贪心最长匹配
    let mut by_len: Vec<&str> = table.to_vec();
    by_len.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let mut out: Vec<(Vec<String>, String)> = Vec::new();

    fn rec(
        s: &str,
        pos: usize,
        by_len: &[&str],
        syls: &mut Vec<String>,
        out: &mut Vec<(Vec<String>, String)>,
    ) {
        if out.len() >= MAX_SEGMENTATIONS {
            return;
        }
        if pos == s.len() {
            out.push((syls.clone(), String::new()));
            return;
        }
        let mut matched = false;
        for syl in by_len {
            if s[pos..].starts_with(syl) {
                syls.push((*syl).to_owned());
                rec(s, pos + syl.len(), by_len, syls, out);
                syls.pop();
                matched = true;
                if out.len() >= MAX_SEGMENTATIONS {
                    return;
                }
            }
        }
        if !matched {
            out.push((syls.clone(), s[pos..].to_owned()));
        }
    }

    rec(input, 0, &by_len, &mut Vec::new(), &mut out);
    out
}

impl Dictionary for MmapDict {
    fn lookup(&self, pinyin: &str) -> Result<Vec<Candidate>, DictError> {
        let Some(rec) = self.find(pinyin)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        // ent_off 是相对词区的偏移，需加区基址
        for (word, freq) in self.parse_entries(self.entries_off + rec.ent_off, rec.ent_len)? {
            out.push(Candidate::new(word, pinyin, freq));
        }
        Ok(out)
    }

    /// 前缀联想：拼音数组按字节序排好，`[下界, 首个不匹配)` 就是答案区间。
    fn lookup_prefix(&self, pinyin: &str) -> Result<Vec<Candidate>, DictError> {
        self.lookup_prefix_limited(pinyin, MAX_LOOKUP_RESULTS as usize)
    }

    fn lookup_prefix_limited(
        &self,
        pinyin: &str,
        limit: usize,
    ) -> Result<Vec<Candidate>, DictError> {
        let pb = pinyin.as_bytes();
        let limit = limit.min(MAX_LOOKUP_RESULTS as usize);
        let start = self.lower_bound(pb)?;
        let mut out: Vec<Candidate> = Vec::new();
        for i in start..self.pinyin_count {
            let rec = self.record(i)?;
            let s = self.slice(self.strings_off + rec.str_off, rec.str_len)?;
            if !s.starts_with(pb) {
                break;
            }
            let py = std::str::from_utf8(s)
                .map_err(|_| DictError::Corrupt("拼音不是合法文本"))?
                .to_owned();
            for (word, freq) in self.parse_entries(self.entries_off + rec.ent_off, rec.ent_len)? {
                out.push(Candidate::new(word, py.clone(), freq));
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }

    fn total_freq(&self) -> Result<u64, DictError> {
        Ok(*self.total_freq.get_or_init(|| self.scan_total_freq()))
    }
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
}

impl Dictionary for DictStore {
    fn lookup(&self, pinyin: &str) -> Result<Vec<Candidate>, DictError> {
        match self {
            Self::Sqlite(d) => d.lookup(pinyin),
            Self::Mmap(d) => d.lookup(pinyin),
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
fn read_u16(data: &[u8], off: usize) -> Result<u16, DictError> {
    let arr: [u8; 2] = data
        .get(off..off + 2)
        .ok_or(DictError::Corrupt("数据越界"))?
        .try_into()
        .map_err(|_| DictError::Corrupt("数据越界"))?;
    Ok(u16::from_le_bytes(arr))
}

/// 从字节切片读小端 u32（越界返回损坏错误）。
fn read_u32(data: &[u8], off: usize) -> Result<u32, DictError> {
    let arr: [u8; 4] = data
        .get(off..off + 4)
        .ok_or(DictError::Corrupt("数据越界"))?
        .try_into()
        .map_err(|_| DictError::Corrupt("数据越界"))?;
    Ok(u32::from_le_bytes(arr))
}

/// 从字节切片读小端 u64（越界返回损坏错误）。
fn read_u64(data: &[u8], off: usize) -> Result<u64, DictError> {
    let arr: [u8; 8] = data
        .get(off..off + 8)
        .ok_or(DictError::Corrupt("数据越界"))?
        .try_into()
        .map_err(|_| DictError::Corrupt("数据越界"))?;
    Ok(u64::from_le_bytes(arr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 测试词库的条目组:(拼音, [(词, freq)])。
    type TestGroups<'a> = Vec<(&'a [u8], Vec<(&'a str, u32)>)>;
    /// 测试简拼副索引的条目组:(首字母 key, [(词, 全拼, freq)])。
    type TestAbbrevGroups<'a> = Vec<(&'a [u8], Vec<(&'a str, &'a str, u32)>)>;

    /// 按 dict.bin v1 布局手工构造一个迷你词库（拼音已按字节序排好）。
    fn build_test_bin(version: u32) -> Vec<u8> {
        // (拼音, [(词, freq)])，顺序即拼音字节序：hao < ni < "ni hao"
        let groups: TestGroups = vec![
            (b"hao", vec![("好", 80)]),
            (b"ni", vec![("你", 300), ("妮", 200), ("尼", 20)]),
            (b"ni hao", vec![("你好", 400)]),
        ];
        let mut strings = Vec::new();
        let mut entries = Vec::new();
        let mut records = Vec::new();
        let mut total = 0u64;
        for (py, ws) in &groups {
            let str_off = strings.len() as u32;
            strings.extend_from_slice(py);
            let ent_off = entries.len() as u32;
            let ent_start = entries.len();
            for (w, f) in ws {
                entries.push(w.len() as u8);
                entries.extend_from_slice(w.as_bytes());
                entries.extend_from_slice(&f.to_le_bytes());
            }
            records.extend_from_slice(&str_off.to_le_bytes());
            records.extend_from_slice(&(py.len() as u16).to_le_bytes());
            records.extend_from_slice(&ent_off.to_le_bytes());
            records.extend_from_slice(&((entries.len() - ent_start) as u16).to_le_bytes());
            total += ws.len() as u64;
        }
        let records_off = MMAP_HEADER_LEN as u64;
        let strings_off = records_off + (groups.len() as u64) * MMAP_RECORD_LEN as u64;
        let entries_off = strings_off + strings.len() as u64;

        let mut out = Vec::new();
        out.extend_from_slice(MMAP_MAGIC);
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&(groups.len() as u64).to_le_bytes());
        out.extend_from_slice(&total.to_le_bytes());
        out.extend_from_slice(&records_off.to_le_bytes());
        out.extend_from_slice(&strings_off.to_le_bytes());
        out.extend_from_slice(&entries_off.to_le_bytes());
        out.extend_from_slice(&records);
        out.extend_from_slice(&strings);
        out.extend_from_slice(&entries);
        out
    }

    /// 测试用临时路径（进程 id 后缀避免并发冲突）。
    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("xwx_test_dict_{name}_{}.bin", std::process::id()))
    }

    #[test]
    fn mmap_lookup_exact() {
        let path = temp_path("lookup");
        fs::write(&path, build_test_bin(MMAP_VERSION)).expect("写测试词库失败");
        let dict = MmapDict::open(&path).expect("打开失败");
        fs::remove_file(&path).ok();

        let ni = dict.lookup("ni").expect("查询失败");
        let words: Vec<(&str, u64)> = ni.iter().map(|c| (c.word.as_str(), c.freq)).collect();
        assert_eq!(words, vec![("你", 300), ("妮", 200), ("尼", 20)],
            "应返回全部词条且组内按 freq 降序");
        assert!(ni.iter().all(|c| c.pinyin == "ni"));

        assert!(dict.lookup("hao").expect("查询失败").len() == 1);
        assert!(dict.lookup("x").expect("查询失败").is_empty(), "不存在的拼音应返回空");
    }

    #[test]
    fn mmap_lookup_phrase_with_space() {
        // 含空格的拼音 key（"ni hao"）在排序数组中与 "ni" 相邻，二分应能区分
        let path = temp_path("phrase");
        fs::write(&path, build_test_bin(MMAP_VERSION)).expect("写测试词库失败");
        let dict = MmapDict::open(&path).expect("打开失败");
        fs::remove_file(&path).ok();

        let hi = dict.lookup("ni hao").expect("查询失败");
        assert_eq!(hi.len(), 1);
        assert_eq!(hi[0].word, "你好");
    }

    #[test]
    fn mmap_lookup_prefix_range() {
        let path = temp_path("prefix");
        fs::write(&path, build_test_bin(MMAP_VERSION)).expect("写测试词库失败");
        let dict = MmapDict::open(&path).expect("打开失败");
        fs::remove_file(&path).ok();

        let words: Vec<String> = dict
            .lookup_prefix("n")
            .expect("查询失败")
            .into_iter()
            .map(|c| c.word)
            .collect();
        assert_eq!(words, vec!["你", "妮", "尼", "你好"],
            "前缀 'n' 应覆盖 'ni' 与 'ni hao' 两个拼音，且按拼音序");
        assert!(dict.lookup_prefix("x").expect("查询失败").is_empty(), "无匹配前缀应返回空");
    }

    #[test]
    fn mmap_rejects_corrupt_magic() {
        let path = temp_path("corrupt");
        fs::write(&path, b"this is not a dict.bin file").expect("写文件失败");
        assert!(MmapDict::open(&path).is_err(), "坏魔数应报错");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn mmap_rejects_version_mismatch() {
        let path = temp_path("version");
        fs::write(&path, build_test_bin(MMAP_VERSION + 1)).expect("写测试词库失败");
        assert!(MmapDict::open(&path).is_err(), "版本不兼容应报错");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn dict_store_routes_to_sqlite() {
        let store = DictStore::Sqlite(SqliteDict::open_in_memory().expect("内存词库失败"));
        store.upsert("你好", "ni hao", 10).expect("upsert 应成功");
        assert_eq!(store.lookup("ni hao").expect("查询失败").len(), 1);
        assert!(!store.lookup_prefix("ni").expect("查询失败").is_empty());
    }

    /// 按 abbrev.bin v1 布局手工构造一个迷你简拼副索引(key 已按字节序排好)。
    ///
    /// 词条区格式:word_len u8 + word + pinyin_len u8 + pinyin + freq u32 LE。
    fn build_test_abbrev_bin(version: u32) -> Vec<u8> {
        // (首字母 key, [(词, 全拼, freq)]):bjdx < zgr
        let groups: TestAbbrevGroups = vec![
            (b"bjdx", vec![("北京大学", "bei jing da xue", 211845)]),
            (b"zgr", vec![
                ("中国人", "zhong guo ren", 451625),
                ("这个人", "zhe ge ren", 167480),
            ]),
        ];
        let mut strings = Vec::new();
        let mut entries = Vec::new();
        let mut records = Vec::new();
        let mut total = 0u64;
        for (key, ws) in &groups {
            let str_off = strings.len() as u32;
            strings.extend_from_slice(key);
            let ent_off = entries.len() as u32;
            let ent_start = entries.len();
            for (w, py, f) in ws {
                entries.push(w.len() as u8);
                entries.extend_from_slice(w.as_bytes());
                entries.push(py.len() as u8);
                entries.extend_from_slice(py.as_bytes());
                entries.extend_from_slice(&f.to_le_bytes());
            }
            records.extend_from_slice(&str_off.to_le_bytes());
            records.extend_from_slice(&(key.len() as u16).to_le_bytes());
            records.extend_from_slice(&ent_off.to_le_bytes());
            records.extend_from_slice(&((entries.len() - ent_start) as u16).to_le_bytes());
            total += ws.len() as u64;
        }
        let records_off = MMAP_HEADER_LEN as u64;
        let strings_off = records_off + (groups.len() as u64) * MMAP_RECORD_LEN as u64;
        let entries_off = strings_off + strings.len() as u64;
        let mut out = Vec::new();
        out.extend_from_slice(ABBREV_MAGIC);
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&(groups.len() as u64).to_le_bytes());
        out.extend_from_slice(&total.to_le_bytes());
        out.extend_from_slice(&records_off.to_le_bytes());
        out.extend_from_slice(&strings_off.to_le_bytes());
        out.extend_from_slice(&entries_off.to_le_bytes());
        out.extend_from_slice(&records);
        out.extend_from_slice(&strings);
        out.extend_from_slice(&entries);
        out
    }

    #[test]
    fn abbrev_index_lookup_pure() {
        let path = temp_path("abbrev_pure");
        fs::write(&path, build_test_abbrev_bin(ABBREV_VERSION)).expect("写副索引失败");
        let idx = AbbrevIndex::open(&path).expect("打开副索引失败");
        fs::remove_file(&path).ok();

        let res = idx.lookup_prefix("zgr").expect("简拼查询失败");
        assert!(res.iter().any(|c| c.word == "中国人" && c.pinyin == "zhong guo ren"),
            "zgr 应返回带全拼的中国人: {res:?}");
        // 组内按 freq 降序
        assert_eq!(res[0].word, "中国人");
        let bjdx = idx.lookup_prefix("bjdx").expect("简拼查询失败");
        assert!(bjdx.iter().any(|c| c.word == "北京大学" && c.pinyin == "bei jing da xue"));
        assert!(idx.lookup_prefix("q").expect("查询失败").is_empty(), "无匹配应返回空");
    }

    #[test]
    fn abbrev_index_rejects_bad_magic() {
        let path = temp_path("abbrev_bad");
        fs::write(&path, b"not an abbrev index").expect("写文件失败");
        assert!(AbbrevIndex::open(&path).is_err(), "坏魔数应报错");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn mmap_dict_without_abbrev_degrades_gracefully() {
        // 副索引缺失:lookup_abbrev 返回空(不 panic),精确查询不受影响。
        let path = temp_path("no_abbrev");
        fs::write(&path, build_test_bin(MMAP_VERSION)).expect("写词库失败");
        let dict = MmapDict::open(&path).expect("打开词库失败");
        fs::remove_file(&path).ok();

        assert!(!dict.has_abbrev(), "未装配副索引时 has_abbrev 应为 false");
        assert!(dict.lookup_abbrev("zgr").expect("缺副索引不应报错").is_empty(),
            "缺副索引时简拼查询应退化为空");
        // 精确查询路径不受副索引缺失影响
        assert_eq!(dict.lookup("ni").expect("精确查询失败").len(), 3);
    }

    #[test]
    fn mmap_dict_with_missing_abbrev_file_still_works() {
        // 装配一个不存在的副索引路径:with_abbrev 仅告警置 None,不 panic。
        let path = temp_path("dict_ok");
        fs::write(&path, build_test_bin(MMAP_VERSION)).expect("写词库失败");
        let dict = MmapDict::open(&path)
            .expect("打开词库失败")
            .with_abbrev("/nonexistent/path/abbrev.bin");
        fs::remove_file(&path).ok();

        assert!(!dict.has_abbrev(), "缺失副索引应退化为 None");
        assert!(dict.lookup_abbrev("zgr").expect("查询不应 panic").is_empty());
        assert_eq!(dict.lookup("ni hao").expect("精确查询失败")[0].word, "你好");
    }

    #[test]
    fn real_abbrev_bin_lookup() {
        // 真实副索引(若已生成):zgr → 中国人(带全拼)、bjdx → 北京大学。
        let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
        let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
        if !dict_path.is_file() || !abbrev_path.is_file() {
            return;
        }
        let dict = MmapDict::open(&dict_path).expect("词库加载失败")
            .with_abbrev(&abbrev_path);
        assert!(dict.has_abbrev(), "abbrev.bin 应被装配");

        let zgr = dict.lookup_abbrev("zgr").expect("zgr 查询失败");
        assert!(zgr.iter().any(|c| c.word == "中国人" && c.pinyin == "zhong guo ren"),
            "zgr 应含中国人(带全拼 zhong guo ren)");
        assert_eq!(zgr[0].word, "中国人", "zgr 第一名应为中国人: {:?}",
            zgr.iter().take(3).map(|c| &c.word).collect::<Vec<_>>());

        let bjdx = dict.lookup_abbrev("bjdx").expect("bjdx 查询失败");
        assert!(bjdx.iter().any(|c| c.word == "北京大学"),
            "bjdx 应含北京大学");
    }

    #[test]
    fn real_abbrev_mixed_lookup() {
        // 混合简拼:zhongguor → 中国人(切 [zhong, guo] + 残余 r 过滤)。
        let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
        if !dict_path.is_file() {
            return;
        }
        let dict = MmapDict::open(&dict_path).expect("词库加载失败");
        let res = dict.lookup_mixed("zhongguor").expect("混合简拼查询失败");
        assert!(res.iter().any(|c| c.word == "中国人" && c.pinyin == "zhong guo ren"),
            "zhongguor 应含中国人: {:?}",
            res.iter().take(5).map(|c| (&c.word, &c.pinyin)).collect::<Vec<_>>());
    }

    #[test]
    fn real_abbrev_mixed_initial_at_any_position() {
        // 本轮新增能力:首字母可以出现在**开头**或**中间**,不再只支持末尾。
        //
        // 旧实现要求第一个音节必须是**完整音节**(`segment_input` 走不出音节
        // 就直接返回空),所以 `nhao` 整条查询召回为 0、只剩回声候选;
        // `feicde` 只能靠「非常地恩爱」这类长词偶然覆盖,目标词进不了前列。
        //
        // 失败见证:把交错分支(DFS 的 (b))删掉 → `nhao` 变回空 → 红。
        let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
        if !dict_path.is_file() {
            return;
        }
        let dict = MmapDict::open(&dict_path).expect("词库加载失败");

        // **开头**缩写:n(in) + hao。全拼必须是补全后的 `nin hao`,
        // 不能是被截断的 `n hao`(候选的 pinyin 直接取索引 key)。
        let res = dict.lookup_mixed("nhao").expect("nhao 查询失败");
        assert!(
            res.iter().any(|c| c.word == "您好" && c.pinyin == "nin hao"),
            "nhao 应含「您好」且全拼为 nin hao(不得截断成 n hao): {:?}",
            res.iter().take(6).map(|c| (&c.word, &c.pinyin)).collect::<Vec<_>>()
        );
        assert!(
            res.iter().any(|c| c.word == "你好" && c.pinyin == "ni hao"),
            "nhao 也应含「你好」(ni + hao): {:?}",
            res.iter().take(6).map(|c| (&c.word, &c.pinyin)).collect::<Vec<_>>()
        );

        // **中间**缩写:fei + c(hang) + de。
        let res = dict.lookup_mixed("feicde").expect("feicde 查询失败");
        assert!(
            res.iter().any(|c| c.word == "非常地" && c.pinyin == "fei chang de"),
            "feicde 应含「非常地」: {:?}",
            res.iter().take(6).map(|c| (&c.word, &c.pinyin)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn real_abbrev_mixed_full_pinyin_uses_the_exact_path() {
        // **冻结约束**的落点:整串是合法全拼时走原实现,不得出现「只有缩略
        // 解释才可能命中」的词。
        //
        // `hao` 是合法音节,但 `h` 当缩写再补全成 `he`/`hou` 也能凑出
        // `he ao`/`hou ao`——交错 DFS 单独用会真的加进「河岙」「后澳」
        // (a/e/o 是单字母音节,`ao` 是合法音节)。分路之后它们必须消失。
        //
        // 失败见证:去掉 `is_full_pinyin` 分流 → 这两条重新出现 → 红。
        let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
        if !dict_path.is_file() {
            return;
        }
        let dict = MmapDict::open(&dict_path).expect("词库加载失败");
        let res = dict.lookup_mixed("hao").expect("hao 查询失败");
        for w in ["河岙", "后澳"] {
            assert!(
                !res.iter().any(|c| c.word == w),
                "「hao」是合法全拼,不得出现只有缩略解释才命中得上的 {w:?}: {:?}",
                res.iter().take(6).map(|c| (&c.word, &c.pinyin)).collect::<Vec<_>>()
            );
        }
        assert!(
            res.iter().any(|c| c.word == "好"),
            "「hao」必须仍能命中「好」"
        );
    }

    #[test]
    fn real_abbrev_mixed_requires_a_full_syllable() {
        // 纯首字母串是副索引 `lookup_abbrev` 的职责,`lookup_mixed` 对它返回空
        // ——与旧实现 `if syls.is_empty() { continue; }` 同口径。这条同时挡住
        // 「单字母输入从前缀联想里拿到几百条候选」。
        //
        // 失败见证:去掉 `used_literal` 判断 → `zgr`/`n` 会返回大量前缀词 → 红。
        let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
        if !dict_path.is_file() {
            return;
        }
        let dict = MmapDict::open(&dict_path).expect("词库加载失败");
        for input in ["zgr", "n", "bjdx"] {
            let res = dict.lookup_mixed(input).expect("查询失败");
            assert!(
                res.is_empty(),
                "纯首字母串 {input:?} 应由副索引负责,lookup_mixed 必须返回空,实测 {} 条",
                res.len()
            );
        }
    }

    #[test]
    fn real_abbrev_mixed_pathological_input_is_bounded() {
        // 预算守卫:病态输入不得报错、不得突破 `MAX_LOOKUP_RESULTS`。
        // 耗时上界由 `examples/abbrev_bench.rs` 的预算守卫组实测(实测最坏
        // `zhzhzhzh` ≈1.4ms,门槛 5ms),这里钉的是「不 panic、不失控」。
        let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
        if !dict_path.is_file() {
            return;
        }
        let dict = MmapDict::open(&dict_path).expect("词库加载失败");
        // `zhongg` 是**命中量很大**的混合输入(实测正好顶到上限),把它和病态
        // 输入放在一起:前者证明早停真的生效,后者证明病态形状不 panic。
        // 失败见证:去掉 `collect` 里的早停 → `zhongg` 返回远超 500 条 → 红。
        for input in ["zzzz", "zhzhzhzh", "aaaaaaaa", "zhzhzhzhzhzhzhzh", "zhongg"] {
            let res = dict.lookup_mixed(input).expect("病态输入不得报错");
            assert!(
                res.len() <= MAX_LOOKUP_RESULTS as usize,
                "{input:?} 返回 {} 条,超过上限",
                res.len()
            );
        }
    }

    #[test]
    fn real_abbrev_mixed_truncates_by_frequency_not_by_exploration_order() {
        // **T1(2026-10-02)**:收集期的早停必须按词频,不能按探索顺序。
        //
        // `meiy` = mei + y,第二个音节下挂着几百个 key(每X/没X/美X…)。
        // 旧实现在收满 500 条就停,而截断发生在**排序之前**——按探索顺序
        // 先到先得,于是词频最高的「没有」(504685)被丢掉,列表第 1 名反而是
        // 词频 500354 的「每一个」。修法:收集期不做质量决策,排序后按词频
        // 截断(探索上界提到 8 倍 + 整 key 跳过控制成本)。
        //
        // 失败见证:把 `lookup_mixed` 末尾的 `out.truncate(MAX_LOOKUP_RESULTS)`
        // 去掉、改成收集期 `out.len() >= MAX_LOOKUP_RESULTS` 就返回 → 红。
        let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
        let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
        if !dict_path.is_file() || !abbrev_path.is_file() {
            return;
        }
        let dict = MmapDict::open(&dict_path).expect("词库加载失败")
            .with_abbrev(&abbrev_path);
        let res = dict.lookup_mixed("meiy").expect("meiy 查询失败");
        let pos = res.iter().position(|c| c.word == "没有")
            .unwrap_or_else(|| panic!("「没有」必须被召回(旧实现把它丢了): 前 5 = {:?}",
                res.iter().take(5).map(|c| (&c.word, c.freq)).collect::<Vec<_>>()));
        assert!(pos < 3, "「没有」应在前 3(它是这串里词频最高的): 第 {} 名", pos + 1);
        // 列表本身必须是词频降序:截断按词频的前提。
        assert!(
            res.windows(2).all(|w| w[0].freq >= w[1].freq),
            "返回值应按词频降序"
        );
        // 上限不变。
        assert!(res.len() <= MAX_LOOKUP_RESULTS as usize, "仍不得超过 500 条");
    }

    #[test]
    fn real_abbrev_early_stop_caps_results() {
        // 副索引(`lookup_abbrev`)的早停:xx 命中上万条,返回不得超过
        // MAX_LOOKUP_RESULTS(500)。副索引的每个 key 独立、块内已按 freq
        // 降序,所以这里的截断本来就是「按质量」的;混合简拼那条路原来不是,
        // 见 real_abbrev_mixed_truncates_by_frequency_not_by_exploration_order。
        let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
        let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
        if !dict_path.is_file() || !abbrev_path.is_file() {
            return;
        }
        let dict = MmapDict::open(&dict_path).expect("词库加载失败")
            .with_abbrev(&abbrev_path);
        let res = dict.lookup_abbrev("xx").expect("xx 查询失败");
        assert!(res.len() <= MAX_LOOKUP_RESULTS as usize,
            "早停应把结果限制在 500 条内,实际 {}", res.len());
        assert_eq!(res.len(), MAX_LOOKUP_RESULTS as usize,
            "xx 命中远超 500,应恰好截到 500(证明是早停而非全扫)");
    }
}
