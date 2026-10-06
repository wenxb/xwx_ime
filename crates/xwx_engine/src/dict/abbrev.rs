//! 简拼副索引(`abbrev.bin`):与主索引同构的「首字母 → 词」索引。
//! 只回答一个问题:输入是纯首字母缩写时,哪些词可能匹配。

use std::fs::File;
use std::path::Path;

use memmap2::Mmap;

use crate::candidate::Candidate;

use super::{read_u16, read_u32, read_u64, DictError, MmapDict, MmapRecord, ABBREV_HEADER_LEN,
            ABBREV_MAGIC, ABBREV_VERSION, MAX_LOOKUP_RESULTS, MMAP_RECORD_LEN};

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
pub struct AbbrevIndex {
    map: Mmap,
    key_count: usize,
    /// 关联数（见 [`Self::association_count`]）。
    association_count: u64,
    records_off: usize,
    strings_off: usize,
    entries_off: usize,
}

/// 简拼查询写法变体的**带真值**版本：每个变体自带「哪些音节是整体声母」。
///
/// 折叠（去掉 h）不是免费的简写：它声称「用户把 zh/ch/sh 当**整体声母**敲」，
/// 因此对应音节必须**真的**以该双字母声母开头。这条真值只有生成变体的这里
/// 知道——查完副索引再从词条拼音反推是推不出来的：`是(shi)` 与 `三(san)`
/// 的 key 都是 `s`，`matched_len`（= key 长度）分不出二者（⑤⑩⓪/⑤⑩⑥）。
/// 校验的消费者在 `MmapDict::lookup_abbrev`。
pub struct VariantRead {
    /// 拿去查副索引的 key 前缀（旧 `abbrev_variants` 的元素）。
    pub query: String,
    /// 折叠声母 `(音节序号, 声母首字母)`：该音节必须以「首字母 + h」开头。
    /// 按音节序号升序；无折叠时为空（校验零成本）。
    pub digraphs: Vec<(usize, u8)>,
}

/// 枚举一次简拼查询的全部写法变体，并带出每个变体的折叠真值。
///
/// 走法与旧逻辑同一步进（marks 的判定逐字节一致），只是额外记录：被去掉的
/// h 意味着「这个 zh/ch/sh 是一个整体声母音节」；保留的 h 则是**下一个
/// 音节**的首字母（`shh` = `s`+`h`+`h` 三个音节，或 `sh`+`h` 两个——两种
/// 读法各对应一个变体）。
pub fn abbrev_variant_reads(abbrev: &str) -> Vec<VariantRead> {
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
        return vec![VariantRead { query: abbrev.to_string(), digraphs: Vec::new() }];
    }
    let mut out: Vec<VariantRead> = Vec::with_capacity(1 << marks.len());
    for bits in 0..(1u32 << marks.len()) {
        let drop: Vec<bool> = (0..marks.len()).map(|k| bits & (1 << k) != 0).collect();
        let mut s = String::with_capacity(abbrev.len());
        let mut digraphs: Vec<(usize, u8)> = Vec::new();
        let (mut i, mut syl) = (0usize, 0usize);
        while i < bytes.len() {
            if i + 1 < bytes.len()
                && matches!(bytes[i], b'z' | b'c' | b's')
                && bytes[i + 1] == b'h'
            {
                if drop[marks.iter().position(|&m| m == i + 1).unwrap_or(usize::MAX)] {
                    // 折叠：键里只贡献声母首字母，且该音节必须真的以 `Xh` 开头。
                    digraphs.push((syl, bytes[i]));
                    s.push(bytes[i] as char);
                    i += 2;
                } else {
                    // 保留：h 归下一个音节，下一轮按普通字母走。
                    s.push(bytes[i] as char);
                    i += 1;
                }
                syl += 1;
            } else {
                s.push(bytes[i] as char);
                i += 1;
                syl += 1;
            }
        }
        out.push(VariantRead { query: s, digraphs });
    }
    out.sort_by(|a, b| a.query.cmp(&b.query));
    out.dedup_by(|a, b| a.query == b.query);
    out
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
        let association_count = read_u64(bytes, 20)?;
        let records_off = read_u64(bytes, 28)? as usize;
        let strings_off = read_u64(bytes, 36)? as usize;
        let entries_off = read_u64(bytes, 44)? as usize;
        // **区段顺序与范围校验（Q4）**：与主索引 `MmapDict::from_bytes` 同一套判据。
        // 头部只报三个起点，查询靠加法定位；不先证明顺序与范围，越界只能靠切片
        // 兜底，报错离谱且可能悄悄读到别的区段。
        if !(ABBREV_HEADER_LEN <= records_off
            && records_off <= strings_off
            && strings_off <= entries_off
            && entries_off <= bytes.len())
        {
            return Err(DictError::Corrupt(
                "副索引区段顺序/范围不合法: records/strings/entries 起点须递增且落在文件内",
            ));
        }
        let records_len = key_count
            .checked_mul(MMAP_RECORD_LEN)
            .ok_or(DictError::Corrupt("副索引记录数溢出"))?;
        if records_off + records_len > strings_off {
            return Err(DictError::Corrupt(
                "副索引记录数组越界: key_count × 记录长度超出 strings 区",
            ));
        }
        Ok(Self {
            map,
            key_count,
            association_count,
            records_off,
            strings_off,
            entries_off,
        })
    }

    /// 关联数（词条数）:生成脚本保证「同一词条只进一个首字母 key」，
    /// 所以它恒等于主索引的词条总数——装配时用它做**同源校验**。
    pub fn association_count(&self) -> u64 {
        self.association_count
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
                // 把「这次前缀匹配吃掉了多少个输入字母」如实带出来：key 就是
                // 被匹配到的那个首字母串（`s`），长度即匹配长度。`s` 全部是
                // ASCII 字母（副索引 key 由 `scripts/abbrev_to_bin.py` 生成），
                // 故字节数 == 字母数。**这是唯一能区分「整串命中」与「前缀
                // 命中」的真值**——`lookup_prefix` 是前缀匹配，`wmjt` 会被
                // `wmjtqbj` 查到，从词条拼音反推匹配长度三次都失败了（⑤⑨②/
                // ⑤⑨③/⑤⑨⑦）。消费者见 `Candidate::matched_len` 的字段文档。
                let mut cand = Candidate::new(word, pinyin, freq as u64);
                cand.matched_len = s.len();
                out.push(cand);
                if out.len() >= MAX_LOOKUP_RESULTS as usize {
                    return Ok(out);
                }
                pos += 2 + wl + pl + 4;
            }
        }
        Ok(out)
    }
}

/// 折叠读法的音节校验：第 `idx` 个音节必须真的以「声母 + h」开头。
/// 音节数不足同样算不匹配（该读法需要的音节根本不存在）。
/// 折叠点升序，单趟扫描；注意 `split().nth()` 是**相对**前进，不能连用
/// （实测 `zhzhzhzh` 的 4 个折叠点因此全部错位、整池误杀）。
/// 从 `dict/lookup.rs` 移入：它的唯一消费者是「副索引词条 × 折叠变体」，
/// 与简拼纠错的证据探针（本文件）共用。
pub(super) fn syllables_carry_digraphs(pinyin: &str, digraphs: &[(usize, u8)]) -> bool {
    let mut seen = 0usize; // 已扫过的音节数
    for (i, s) in pinyin.split(' ').enumerate() {
        seen = i + 1;
        if let Some(&(_, initial)) = digraphs.iter().find(|&&(idx, _)| idx == i) {
            let b = s.as_bytes();
            if b.first() != Some(&initial) || b.get(1) != Some(&b'h') {
                return false;
            }
        }
    }
    digraphs.last().is_none_or(|&(last, _)| seen > last)
}

impl AbbrevIndex {
    /// 精确等于 `key` 的首字母索引记录里，**通过折叠校验**的词条最高词频。
    ///
    /// [`Self::lookup_prefix`] 是前缀匹配且 500 条早停，回答不了「这个
    /// 精确 key 下有没有真实词条」——简拼纠错的一级披露（`O(log n)` 证据
    /// 探针）需要的是后一个问题。精确命中 ⇒ `key.len()` == 音节数，天然
    /// 满足「一字一字母」判据，无需再筛。
    fn exact_key_best_freq(
        &self,
        key: &str,
        digraphs: &[(usize, u8)],
    ) -> Result<Option<u64>, DictError> {
        let pb = key.as_bytes();
        let i = self.lower_bound(pb)?;
        if i >= self.key_count {
            return Ok(None);
        }
        let rec = self.record(i)?;
        let s = self.slice(self.strings_off + rec.str_off, rec.str_len)?;
        if s != pb {
            return Ok(None);
        }
        let block = self.slice(self.entries_off + rec.ent_off, rec.ent_len)?;
        let mut best: Option<u64> = None;
        let mut pos = 0usize;
        while pos < block.len() {
            let wl = *block.get(pos).ok_or(DictError::Corrupt("词块越界"))? as usize;
            block
                .get(pos + 1..pos + 1 + wl)
                .ok_or(DictError::Corrupt("词块越界"))?;
            let pl = *block
                .get(pos + 1 + wl)
                .ok_or(DictError::Corrupt("词块越界"))? as usize;
            let py_bytes = block
                .get(pos + 2 + wl..pos + 2 + wl + pl)
                .ok_or(DictError::Corrupt("词块越界"))?;
            let freq = read_u32(block, pos + 2 + wl + pl)? as u64;
            let pinyin = std::str::from_utf8(py_bytes)
                .map_err(|_| DictError::Corrupt("全拼不是 UTF-8"))?;
            if digraphs.is_empty() || syllables_carry_digraphs(pinyin, digraphs) {
                best = Some(best.unwrap_or(0).max(freq));
            }
            pos += 2 + wl + pl + 4;
        }
        Ok(best)
    }

    /// 键总数（索引遍历的初始区间上界）。
    pub(super) fn key_count(&self) -> usize {
        self.key_count
    }

    /// `[lo, hi)` 内第一条键 ≥ `prefix` 的记录下标（区间不越界）。
    pub(super) fn lower_bound_in(
        &self,
        prefix: &[u8],
        lo: usize,
        hi: usize,
    ) -> Result<usize, DictError> {
        let (mut lo, mut hi) = (lo.min(self.key_count), hi.min(self.key_count));
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

    /// 第 `i` 条记录的键是否以 `prefix` 开头（区间收窄后的存活性判据）。
    pub(super) fn key_starts_with(&self, i: usize, prefix: &[u8]) -> Result<bool, DictError> {
        if i >= self.key_count {
            return Ok(false);
        }
        let rec = self.record(i)?;
        Ok(self
            .slice(self.strings_off + rec.str_off, rec.str_len)?
            .starts_with(prefix))
    }

    /// 收集**恰好等于**长度为 `key_len` 的那条键（`[lo, hi)` 内）的全部词条，
    /// 最多 `limit` 条。
    ///
    /// 键按字节序排：同一个前缀下「等于前缀」的那条键必然排在更长的键之前，
    /// 故遇到更长的键即可收工——这条也是「一字一字母」（每个音节都打了字母）
    /// 判据的落点：键长度 == 音节数 == 片段数。
    pub(super) fn exact_entries(
        &self,
        lo: usize,
        hi: usize,
        key_len: usize,
        limit: usize,
    ) -> Result<Vec<(String, String, u64)>, DictError> {
        let mut out = Vec::new();
        for i in lo..hi.min(self.key_count) {
            if out.len() >= limit {
                break;
            }
            let rec = self.record(i)?;
            let key = self.slice(self.strings_off + rec.str_off, rec.str_len)?;
            if key.len() > key_len {
                break;
            }
            if key.len() < key_len {
                continue;
            }
            let block = self.slice(self.entries_off + rec.ent_off, rec.ent_len)?;
            let mut pos = 0usize;
            while pos < block.len() && out.len() < limit {
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
                let freq = read_u32(block, pos + 2 + wl + pl)? as u64;
                let word = std::str::from_utf8(word_bytes)
                    .map_err(|_| DictError::Corrupt("词条不是 UTF-8"))?;
                let pinyin = std::str::from_utf8(py_bytes)
                    .map_err(|_| DictError::Corrupt("全拼不是 UTF-8"))?;
                out.push((word.to_owned(), pinyin.to_owned(), freq));
                pos += 2 + wl + pl + 4;
            }
        }
        Ok(out)
    }
}

impl MmapDict {
    /// 简拼纠错的一级证据（**词条级**）：`variant` 本身（含平翘舌折叠写法）
    /// 精确命中副索引 key 时，返回通过折叠校验的词条最高词频；无命中
    /// ⇒ `None`。调用方（`engine/abbrev_correct.rs`）用 `FILLER_FREQ_TIER`
    /// 过滤填充档——词库层只如实报告，不替引擎做证据裁决。
    pub(crate) fn abbrev_entry_evidence(&self, variant: &str) -> Result<Option<u64>, DictError> {
        let Some(idx) = &self.abbrev else {
            return Ok(None);
        };
        let mut best: Option<u64> = None;
        for read in abbrev_variant_reads(variant) {
            if let Some(f) = idx.exact_key_best_freq(&read.query, &read.digraphs)? {
                best = Some(best.unwrap_or(0).max(f));
            }
        }
        Ok(best)
    }

    /// 简拼纠错的一级证据（**链级**）：存在真实词条（最高词频 ≥ `min_freq`）
    /// 的首字母串是 `variant` 的**真前缀**（长度 ≥ `min_prefix`）⇒ 整串链读法
    /// 的首词有据可查（`wmjtqbj` 型读法不在副索引里，但它的首词「我们今天」
    /// = `wmjt` 在）。取**最深**命中前缀，返回 `(前缀长, 词频)`——深度供调用方
    /// 算覆盖率（前缀长/变体长）：同一原串的各 1-编辑变体共享同一条首词
    /// 证据时，删字母型变体（变体更短）覆盖率更高，是平局集里唯一有信息量
    /// 的次级信号。
    ///
    /// 折叠写法不参与（局限，见下）：前缀 key 里 `zh/ch/sh` 的折叠形态
    /// 是「每音节一字母」与「双字母声母」两套写法的位置语义，`exact_key_
    /// best_freq` 的单串探针管不过来，先按字面串查——链证据是「有据可查」
    /// 的宽松信号，漏报可接受，误报由二级解码 + 分数闸兜底。
    pub(crate) fn abbrev_chain_evidence(
        &self,
        variant: &str,
        min_prefix: usize,
        min_freq: u64,
    ) -> Result<Option<(usize, u64)>, DictError> {
        let Some(idx) = &self.abbrev else {
            return Ok(None);
        };
        for i in (min_prefix..variant.len()).rev() {
            if let Some(f) = idx.exact_key_best_freq(&variant[..i], &[])? {
                if f >= min_freq {
                    return Ok(Some((i, f)));
                }
            }
        }
        Ok(None)
    }
}
