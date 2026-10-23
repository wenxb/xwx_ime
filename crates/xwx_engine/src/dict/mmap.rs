//! 主索引(`dict.bin`)的内存映射与索引原语:头部解析、二分定位、
//! 记录/词串读取,以及 [`Dictionary`] 的实现。

use std::cmp::Ordering;
use std::sync::OnceLock;
use std::fs::File;
use std::path::Path;

use memmap2::Mmap;

use crate::candidate::Candidate;
use crate::mmap_advice::advise_random;

use super::{AbbrevIndex, DictError, Dictionary, KeyPrefix, MAX_LOOKUP_RESULTS, MMAP_HEADER_LEN,
            MMAP_HEADER_LEN_V1, MMAP_MAGIC, MMAP_MIN_VERSION, MMAP_RECORD_LEN, MMAP_VERSION,
            MmapDict, MmapRecord, SyllableGraph, read_u16, read_u32, read_u64};

impl MmapDict {
    /// 打开并 mmap 映射一个 dict.bin 词库文件。
    ///
    /// 校验魔数与版本，损坏文件返回 [`DictError::Corrupt`]。
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, DictError> {
        let file = File::open(path)?;
        let map = unsafe { Mmap::map(&file)? };
        // 二分是跨页随机跳,声明后内核不再按 64KB 预读放大常驻(实测随机读
        // 72MB → 63MB);失败不影响正确性,见 `mmap_advice`。
        advise_random(&map);
        Self::from_bytes(map)
    }

    /// 从已映射字节解析头部（独立成函数便于单测构造坏文件）。
    fn from_bytes(map: Mmap) -> Result<Self, DictError> {
        let bytes: &[u8] = &map;
        if bytes.len() < MMAP_HEADER_LEN_V1 {
            return Err(DictError::Corrupt("文件过短，不是完整 dict.bin"));
        }
        if bytes.get(0..8) != Some(MMAP_MAGIC.as_slice()) {
            return Err(DictError::Corrupt("魔数不匹配，不是 dict.bin 词库"));
        }
        let version = read_u32(bytes, 8)?;
        if !(MMAP_MIN_VERSION..=MMAP_VERSION).contains(&version) {
            return Err(DictError::Corrupt("dict.bin 版本不兼容"));
        }
        // v2 多 8 字节头；文件不够长就不该按 v2 解析尾部字段。
        if version >= 2 && bytes.len() < MMAP_HEADER_LEN {
            return Err(DictError::Corrupt("文件过短，不是完整 dict.bin(v2 头 60 字节)"));
        }
        let header_len = if version >= 2 { MMAP_HEADER_LEN } else { MMAP_HEADER_LEN_V1 };
        // 头部字段先全部读出再构造结构体：`bytes` 借用 `map`，
        // 与字段读取（借用）同一表达式中不能再移动 `map`。
        let pinyin_count = read_u64(bytes, 12)? as usize;
        let total_entries = read_u64(bytes, 20)?;
        let records_off = read_u64(bytes, 28)? as usize;
        let strings_off = read_u64(bytes, 36)? as usize;
        let entries_off = read_u64(bytes, 44)? as usize;
        // v2:构建期算好的全表词频总和，省掉运行时的整段扫描。
        // 读成 0 说明头部可疑（生成脚本拒绝写 0），此时**回落到扫描**而不是
        // 直接采信：采信 0 会让 `log10(总频)` 变成 -inf，整层词频项失效。
        let total_freq = match version >= 2 {
            true => Some(read_u64(bytes, 52)?).filter(|&v| v > 0),
            false => None,
        };
        // **区段顺序与范围校验（Q4）**：头部只报三个起点，而每次 `record()` /
        // `slice()` 都拿它们做加法。先在这里证明「header ≤ records ≤ strings ≤
        // entries ≤ 文件长」且记录数组落在 strings 区之前，后面的加法才有意义——
        // 否则越界只能靠 `get` 兜底，报错信息也离谱（「偏移 2^60 越界」而不是
        // 「头部损坏」）。
        if !(header_len <= records_off
            && records_off <= strings_off
            && strings_off <= entries_off
            && entries_off <= bytes.len())
        {
            return Err(DictError::Corrupt("区段顺序/范围不合法: records/strings/entries 起点必须递增且落在文件内"));
        }
        let records_len = pinyin_count
            .checked_mul(MMAP_RECORD_LEN)
            .ok_or_else(|| DictError::Corrupt("记录数溢出"))?;
        if records_off + records_len > strings_off {
            return Err(DictError::Corrupt("记录数组越界: pinyin_count × 记录长度超出 strings 区"));
        }
        Ok(Self {
            map,
            pinyin_count,
            total_entries,
            records_off,
            strings_off,
            entries_off,
            total_freq: total_freq.map_or_else(OnceLock::new, OnceLock::from),
            abbrev: None,
            graph: None,
        })
    }

    /// 装配简拼副索引(构建脚本 `scripts/abbrev_to_bin.py` 的产物)。
    ///
    /// 参照 `ImeEngine::new_with_lm` 对缺失 LM 的处理:副索引缺失/损坏时
    /// 仅记录 warning 并置 `None`,**绝不 panic**——此时简拼查询返回空,
    /// 引擎自然退回精确查询路径,不会静默变成空词库。
    pub fn with_abbrev<P: AsRef<Path>>(mut self, path: P) -> Self {
        match AbbrevIndex::open(path.as_ref()) {
            Ok(idx) => {
                // **同源校验**：副索引由主索引生成，关联数恒等于词条总数。拿错代的
                // 副索引只会静默给出错候选（简拼层命中别的词），比装不上更糟——
                // 所以不一致就不装配，简拼退化为空、精确查询不受影响。
                if idx.association_count() != self.total_entries {
                    tracing::warn!(
                        dict_entries = self.total_entries,
                        abbrev_associations = idx.association_count(),
                        "副索引与词库不同代(关联数不符)，不装配"
                    );
                } else {
                    self.abbrev = Some(idx);
                }
            }
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

    /// 装配音节级转移索引(`scripts/build_syllable_graph.py` 的产物)。
    ///
    /// 同代校验用索引头部的 `n_keys`(生成时的词库记录数)：不符说明两者不是
    /// 一起生成的，拿错代索引会给出别的词的音节转移，比没有更糟——不装配。
    pub fn with_syllable_graph<P: AsRef<Path>>(mut self, path: P) -> Self {
        match SyllableGraph::open(path.as_ref()) {
            Ok(g) => {
                if g.keys() != self.pinyin_count as u64 {
                    tracing::warn!(
                        dict_records = self.pinyin_count,
                        graph_records = g.keys(),
                        "音节级索引与词库不同代(记录数不符)，不装配"
                    );
                } else {
                    self.graph = Some(g);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, path = %path.as_ref().display(),
                    "音节级索引打开失败,缩写弧回退到逐次主索引二分(功能不变)");
            }
        }
        self
    }

    /// 音节级转移索引(未装配时为 `None`)。
    pub fn syllable_graph(&self) -> Option<&SyllableGraph> {
        self.graph.as_ref()
    }

    /// 词条总数（调试/统计用）。
    pub fn count(&self) -> u64 {
        self.total_entries
    }

    /// 边界检查的切片：越界一律返回损坏错误（不 panic）。
    pub(super) fn slice(&self, off: usize, len: usize) -> Result<&[u8], DictError> {
        let end = off.checked_add(len).ok_or(DictError::Corrupt("区段长度溢出"))?;
        self.map.get(off..end).ok_or(DictError::Corrupt("区段越界"))
    }

    /// 第 `i` 条拼音索引记录（数组内按拼音字节序排序）。
    pub(super) fn record(&self, i: usize) -> Result<MmapRecord, DictError> {
        let b = self.slice(self.records_off + i * MMAP_RECORD_LEN, MMAP_RECORD_LEN)?;
        Ok(MmapRecord {
            str_off: read_u32(b, 0)? as usize,
            str_len: read_u16(b, 4)? as usize,
            ent_off: read_u32(b, 6)? as usize,
            ent_len: read_u16(b, 10)? as usize,
        })
    }

    /// 精确查找：返回拼音 key 对应的索引记录；不存在返回 `None`。
    pub(super) fn find(&self, pinyin: &str) -> Result<Option<MmapRecord>, DictError> {
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
    pub(super) fn lower_bound(&self, prefix: &[u8]) -> Result<usize, DictError> {
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
    pub(super) fn first_entry_freq(&self, ent_off: usize, ent_len: usize) -> Result<u64, DictError> {
        let block = self.slice(ent_off, ent_len)?;
        let wl = *block.first().ok_or(DictError::Corrupt("词块为空"))? as usize;
        Ok(read_u32(block, 1 + wl)? as u64)
    }

    pub(super) fn parse_entries(&self, ent_off: usize, ent_len: usize) -> Result<Vec<(String, u64)>, DictError> {
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

    /// 主库同词同音的首个频率；仍校验整个词块，不因命中提前吞掉坏数据。
    pub(crate) fn entry_freq(&self, pinyin: &str, word: &str) -> Result<Option<u64>, DictError> {
        let Some(rec) = self.find(pinyin)? else { return Ok(None) };
        let block = self.slice(self.entries_off + rec.ent_off, rec.ent_len)?;
        let mut pos = 0;
        let mut found = None;
        while pos < block.len() {
            let wl = *block.get(pos).ok_or(DictError::Corrupt("词块越界"))? as usize;
            let bytes = block.get(pos + 1..pos + 1 + wl)
                .ok_or(DictError::Corrupt("词块越界"))?;
            let freq = read_u32(block, pos + 1 + wl)?;
            let entry = std::str::from_utf8(bytes)
                .map_err(|_| DictError::Corrupt("词条不是 UTF-8"))?;
            if found.is_none() && entry == word {
                found = Some(freq as u64);
            }
            pos += 1 + wl + 4;
        }
        Ok(found)
    }

    /// 线性扫词条区求词频总和（约 32MB 词条区 / 186 万条）。
    ///
    /// **只有 v1 数据文件会走到这里**。v2 把这个和写在头部，就没有这一步了：
    /// 一次启动为求一个 `u64` 把整段词条区拉进 page cache，实测让 `dict.bin`
    /// 常驻从 ~30MB 抬到 63MB。保留它是为了旧数据文件仍能用（换二进制不必
    /// 同时重建数据），不是常规路径。
    pub(super) fn scan_total_freq(&self) -> u64 {
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
    pub(super) fn syllable_table(&self) -> &'static [&'static str] {
        crate::segmenter::SYLLABLES
    }

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

    /// 简拼弧剪枝：见 [`MmapDict::key_prefix_state`]。
    fn key_prefix_state(&self, pinyin: &str) -> Result<KeyPrefix, DictError> {
        MmapDict::key_prefix_state(self, pinyin)
    }

    /// 带区间的版本：见 [`MmapDict::key_prefix_state_in`]。
    fn key_prefix_state_in(
        &self,
        pinyin: &str,
        lo: usize,
        hi: usize,
    ) -> Result<(KeyPrefix, usize), DictError> {
        MmapDict::key_prefix_state_in(self, pinyin, lo, hi)
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
