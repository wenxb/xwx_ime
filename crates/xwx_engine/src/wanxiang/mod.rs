//! 万象（Rime octagram）`wanxiang-lts-zh-hans.gram` 字符级窗口语言模型。
//!
//! 本模块是 `scripts/gram_probe`（独立验证子项目，保持原样作为对照基准）
//! 里**已验证可用**读取实现的直接移植：文件头解析（`Metadata` 44 字节）、
//! darts-clone 0.32 double-array（单元 4 字节 + 位布局函数）、逐码点编码
//! `encode_grammar`（不是 UTF-8 原样）、`traverse` / `common_prefix_search`。
//! 模型文件以 mmap 只读映射（真实文件约 420MB），不把数据读进堆内存。
//!
//! 查询语义对齐 librime-octagram `src/octagram.cc` 的 `Query()`：
//!
//! - 上文取末 ≤5 字、候选词取头 ≤5 字，直接拼接成键（字符级窗口模型，
//!   键里没有任何分隔符；窗口宽度 = `collocation_max_length - 1 = 6 - 1`）；
//! - 上文后缀从最长到最短逐级（k = 5..1）查
//!   `traverse(上文末 k 字)` + `common_prefix_search(词首 ≤5 字)`，
//!   与 octagram.cc 完全一致；所有命中取折算后最高分；
//! - 命中原始值 `v = int32(log(freq) × 10000)`，**自然对数**的频次，
//!   正值、量级 13~18 万（还原 `exp(v / 10000.0)`）。
//!
//! # 分数换算：存储值 → log10 概率（trait 约定）
//!
//! 引擎 trait [`LanguageModel::score`](crate::lm::LanguageModel::score) 要求
//! 返回 **log10 概率**（负值，越大越好）。`.gram` 存的是自然对数频次，
//! 两者不是一回事，必须换算，推导如下：
//!
//! ```text
//! v = ln(freq) × 10000                      （存储值）
//! log10(freq) = ln(freq) / ln10 = v / (10000 × ln10)      …(1)
//! log10 P(key) = log10(freq / N) = log10(freq) - log10(N) …(2)  N = 语料规模
//! ```
//!
//! N 无法从文件中读出，用占位常数 `log10_corpus_size = 9`（≈1e9）。
//! 它作为**常数对每个词的分数同时叠加**：只整体平移量纲、不改变任何
//! 排序，因此不引入排序偏差；第 4 步评测调参时随惩罚一起调。
//!
//! octagram.cc 的惩罚（`collocation_penalty: -14` 等）与 `ln(freq)` 属于
//! 同一自然对数域，换到 log10 域时照同一把尺除以 ln10：
//!
//! ```text
//! logp = (v/10000 + penalty_ln) / ln10 - log10_corpus      …(3)
//! ```
//!
//! 数量级核对：常见搭配「今天天气」v ≈ 154000（ln freq ≈ 15.4）叠词配
//! 惩罚 -14 → `(15.4 - 14) / 2.303 - 9 ≈ -8.4`；无上文回退
//! （`non_collocation_penalty = -6`）→ `-6 / 2.303 - 9 ≈ -11.6`。
//! 两者都是负值、与 bigram 的 log10 概率（约 -1..-8）同数量级；
//! 于是自然满足：**有上文命中 ≥ 无上文回退**、**常见搭配 > 生僻组合**
//! （生僻组合无命中落到 -11.6 兜底分）。这套惩罚是为 Rime 自己的打分
//! 公式和万象自己的词库调的，搬进我们的引擎只是**合理的起点**，
//! 第 4 步建评测集后会重调。
//!
//! # 未实现（如实声明）
//!
//! - **句尾哨兵键**（如「绩啦$」）：octagram 在候选词是句尾时用
//!   `rear_penalty` 查 `word + "$"`；本引擎的语言模型接口
//!   [`LanguageModel::score`](crate::lm::LanguageModel::score) 不携带
//!   「位置是否句尾」信号，句尾加分需要在 beam 末端边补传信号后接线，
//!   留给第 4 步；
//! - 惩罚/语料常数未做评测调参（第 4 步建评价集后做）。
//!
//! 参考资料（BSD-3-Clause 上游）：lotem/librime-octagram `src/gram_db.h`
//! （Metadata 44 字节、`kValueScale = 10000`、`kMaxResults = 8`）、
//! `src/octagram.cc`（`Query()` 滑动窗与惩罚组合）、`gram_encoding.cc`
//! （逐码点编码）、librime vendored darts-clone 0.32（单元位布局）。

use std::fs::File;
use std::path::Path;

use memmap2::Mmap;

use crate::mmap_advice::advise_random;
use crate::lm::{LanguageModel, LmContext, LmError};

/// 上文/词首各取的字符数 = `collocation_max_length - 1`（README：6 → 5）。
fn window_chars(config: &WanxiangConfig) -> usize {
    config.collocation_max_length.saturating_sub(1).max(1)
}

/// 把 `s` 的后 `max` 个字符取出来（按 char，不按字节；不足全取）。
fn tail_chars(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().rev().take(max).collect();
    chars.into_iter().rev().collect()
}

// ---------------------------------------------------------------------------
// 文件头（librime-octagram src/gram_db.h: Metadata，44 字节无 padding）
// ---------------------------------------------------------------------------

/// `struct Metadata { char format[32]; uint32_t db_checksum;
///                    uint32_t double_array_size; OffsetPtr<char> double_array; }`
#[derive(Debug)]
struct Header {
    format: String,
    /// double-array 单元数（`GramDb::Build: array_size = trie_->size()`）。
    double_array_size: u32,
    /// double-array 在文件中的起始字节偏移。
    array_file_offset: usize,
}

impl Header {
    fn parse(buf: &[u8]) -> Result<Header, LmError> {
        if buf.len() < 44 {
            return Err(LmError::BadGram("文件头不足 44 字节".into()));
        }
        let zero_pos = buf[..32].iter().position(|&b| b == 0).unwrap_or(32);
        let format = String::from_utf8_lossy(&buf[..zero_pos]).into_owned();
        let _db_checksum = u32::from_le_bytes(buf[32..36].try_into().unwrap());
        let double_array_size = u32::from_le_bytes(buf[36..40].try_into().unwrap());
        // OffsetPtr<char> 是 int32，语义（librime mapped_file.h）：
        //   T* get() const { return (char*)&offset_ + offset_; }
        // 即「相对该偏移字段自身地址的符号偏移」。字段位于文件偏移 40。
        let ptr = i32::from_le_bytes(buf[40..44].try_into().unwrap());
        let array_file_offset = 40i64 + ptr as i64;
        if array_file_offset < 0 || array_file_offset as usize > buf.len() {
            return Err(LmError::BadGram(format!("OffsetPtr 值异常: {ptr}")));
        }
        Ok(Header {
            format,
            double_array_size,
            array_file_offset: array_file_offset as usize,
        })
    }
}

// ---------------------------------------------------------------------------
// gram_encoding.cc 的 encode()：逐 Unicode 码点编码（非 UTF-8 原样）
// ---------------------------------------------------------------------------

/// 逐码点编码（与 C++ `gram::encode` 语义一致，含回绕）。
#[cfg(test)]
fn encode_grammar(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for ch in s.chars() {
        let mut tmp = [0u8; 4];
        let n = encode_cp_into(ch as u32, &mut tmp);
        out.extend_from_slice(&tmp[..n]);
    }
    out
}

/// 单码点编码；返回写入字节数（≤4）。语义与 C++ uint32 完全一致（含回绕），
/// 但写入固定缓冲区：`score()` 热路径里不用为每个字符分配 Vec。
fn encode_cp_into(mut u: u32, out: &mut [u8]) -> usize {
    if u < 0x80 {
        out[0] = if u == 0 { 0xE0 } else { u as u8 };
        return 1;
    }
    if (0x4000..0xA000).contains(&u) {
        if (u & 0xFF) == 0 {
            out[0] = 0xE1;
            out[1] = ((u >> 8) + 0x40) as u8;
        } else {
            out[0] = ((u >> 8) + 0x40) as u8;
            out[1] = (u & 0xFF) as u8;
        }
        return 2;
    }
    let mut bits = 32;
    while bits > 0 && (u & 0xFE00_0000) == 0 {
        bits -= 7;
        u = u.wrapping_shl(7);
    }
    let mut bytes_to_encode = (bits + 6) / 7;
    out[0] = 0xE0 | bytes_to_encode as u8;
    let mut i = 1;
    while bytes_to_encode > 0 {
        bytes_to_encode -= 1;
        out[i] = (((u >> 25) & 0x7F) | 0x80) as u8;
        i += 1;
        u = u.wrapping_shl(7);
    }
    i
}

/// `encode_cp_into` 的 Vec 版（测试/通用代码用）。
#[cfg(test)]
fn encode_cp(u: u32, out: &mut Vec<u8>) {
    let mut tmp = [0u8; 4];
    let n = encode_cp_into(u, &mut tmp);
    out.extend_from_slice(&tmp[..n]);
}

/// 把 `s` 的前 `max` 个字符编码进 `buf`，返回 (字节数, 字符数)。
/// 热路径用：调用方保证 `buf` 容量足够（每字 ≤4 字节，max ≤ 5 → ≤20B）。
fn encode_first_chars(s: &str, max: usize, buf: &mut [u8]) -> (usize, usize) {
    let mut len = 0;
    let mut chars = 0;
    for ch in s.chars().take(max) {
        let mut tmp = [0u8; 4];
        let n = encode_cp_into(ch as u32, &mut tmp);
        buf[len..len + n].copy_from_slice(&tmp[..n]);
        len += n;
        chars += 1;
    }
    (len, chars)
}

// ---------------------------------------------------------------------------
// double-array 访问（darts-clone 0.32：单元 4 字节）
// ---------------------------------------------------------------------------

/// double-array 元信息；数据区间在 [`WanxiangLM::mmap`] 里，
/// 每次读取都现场切片，避免「mmap 与借用它的数组同时作字段」的自引用。
#[derive(Debug)]
struct DoubleArray {
    offset: usize,
    num_units: usize,
}

impl DoubleArray {
    /// 校验数组区间完整落在 mmap 内并构造；越界返回 None。
    fn new(mmap_len: usize, offset: usize, num_units: usize) -> Option<DoubleArray> {
        let bytes = num_units.checked_mul(4)?;
        if offset.checked_add(bytes)? > mmap_len {
            return None;
        }
        Some(DoubleArray { offset, num_units })
    }

    fn slice<'a>(&self, mmap: &'a Mmap) -> &'a [u8] {
        &mmap[self.offset..self.offset + self.num_units * 4]
    }

    #[inline]
    fn unit(&self, data: &[u8], idx: usize) -> Option<u32> {
        if idx >= self.num_units {
            return None;
        }
        let off = idx * 4;
        let b = data.get(off..off + 4)?;
        Some(u32::from_le_bytes(b.try_into().unwrap()))
    }
}

// darts-clone 0.32 DoubleArrayUnit 位布局（librime include/darts.h）
#[inline]
fn u_label(u: u32) -> u32 {
    u & 0x8000_00FF
}
#[inline]
fn u_offset(u: u32) -> usize {
    ((u >> 10) << ((u & 0x200) >> 6)) as usize
}
#[inline]
fn u_has_leaf(u: u32) -> bool {
    ((u >> 8) & 1) == 1
}
#[inline]
fn u_value(u: u32) -> u32 {
    u & 0x7FFF_FFFF
}

impl DoubleArray {
    /// 从 `start_node` 沿 key 前进；返回 (成功?, 最终 node_pos, 已匹配字节数)。
    fn traverse(&self, data: &[u8], key: &[u8], start_node: usize) -> (bool, usize, usize) {
        let mut node = start_node;
        let mut matched = 0usize;
        for &b in key {
            let u = match self.unit(data, node) {
                Some(u) => u,
                None => return (false, node, matched),
            };
            node ^= u_offset(u) ^ b as usize;
            match self.unit(data, node) {
                Some(nu) if u_label(nu) as u8 == b => {
                    matched += 1;
                }
                _ => return (false, node, matched),
            }
        }
        (true, node, matched)
    }

    /// commonPrefixSearch：从 node_pos 出发在 key 上做前缀搜索，
    /// 结果写入调用方提供的固定数组（对齐 `GramDb::Lookup`，`kMaxResults = 8`；
    /// 用固定数组避免热路径分配）。返回命中数。
    fn common_prefix_search(
        &self,
        data: &[u8],
        key: &[u8],
        node_pos: usize,
        results: &mut [(i32, usize)],
    ) -> usize {
        let mut n = 0;
        let mut node = node_pos;
        let unit0 = match self.unit(data, node) {
            Some(u) => u,
            None => return n,
        };
        node ^= u_offset(unit0);
        let mut pos = 0usize;
        for &b in key {
            pos += 1;
            node ^= b as usize;
            let unit = match self.unit(data, node) {
                Some(u) => u,
                None => break,
            };
            if u_label(unit) as u8 != b {
                break;
            }
            let has_leaf = u_has_leaf(unit);
            node ^= u_offset(unit);
            if has_leaf {
                if let Some(leaf_unit) = self.unit(data, node) {
                    let v = u_value(leaf_unit) as i32;
                    results[n] = (v, pos);
                    n += 1;
                    if n >= results.len() {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        n
    }
}

/// 编码里匹配字节数 → unicode 字符数（gram_encoding.cc advance() 规则）。
fn unicode_length(encoded: &[u8], byte_len: usize) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < byte_len {
        let b = encoded[i];
        let adv = if (b & 0x80) == 0 {
            1
        } else if (b & 0xF0) == 0xE0 {
            (b & 0x0F) as usize + 1
        } else {
            2
        };
        i += adv;
        n += 1;
    }
    n
}

// ---------------------------------------------------------------------------
// 万象语言模型
// ---------------------------------------------------------------------------

/// 万象模型参数（占位值取自万象 README；第 4 步建评价集后重调）。
///
/// 字段与 octagram.cc `GrammarConfig` + 分数换算口对应：
/// - `collocation_max_length`：组合窗口上限 +1（6 → 上下文/词各 ≤5 字）；
/// - 各 penalty 是**自然对数域**的原始值（与 `ln(freq)` 相加，见模块文档）。
#[derive(Debug, Clone, PartialEq)]
pub struct WanxiangConfig {
    /// 组合窗口最大长度；octagram 取 `collocation_max_length - 1`。
    pub collocation_max_length: usize,
    /// 判定「完整词配」的最少字符数。
    pub collocation_min_length: usize,
    /// 完整词配惩罚（自然对数域）。
    pub collocation_penalty: f64,
    /// 无上下文 / 无命中兜底惩罚（自然对数域）。
    pub non_collocation_penalty: f64,
    /// 半截窗口弱词配惩罚（自然对数域）。
    pub weak_collocation_penalty: f64,
    /// 句尾哨兵键惩罚（自然对数域；本步未接线）。
    pub rear_penalty: f64,
    /// 假定的语料规模 `log10(N)`（占位常数；排序不受其影响，第 6 步调）。
    pub log10_corpus_size: f64,
}

impl Default for WanxiangConfig {
    /// 万象 LTS README 默认值（README 里 `max_length: 6`、
    /// `collocation_min_length: 3`、`collocation_penalty: -14`、
    /// `non_collocation_penalty: -6`、`weak_collocation_penalty: -100`、
    /// `rear_penalty: -20`）。
    fn default() -> Self {
        WanxiangConfig {
            collocation_max_length: 6,
            collocation_min_length: 3,
            collocation_penalty: -14.0,
            non_collocation_penalty: -6.0,
            weak_collocation_penalty: -100.0,
            rear_penalty: -20.0,
            log10_corpus_size: 9.0,
        }
    }
}

/// 万象 LTS（`.gram` 字符级窗口模型）：mmap 映射 + double-array 查询。
/// 支持的 `.gram` 格式前缀（万象/Rime 的 `Rime::Grammar/<major>.<minor>`）。
///
/// 只认 major=1：minor 变化不影响数组布局，major 变了一定要重新核对解析。
const SUPPORTED_GRAM_FORMAT_PREFIX: &str = "Rime::Grammar/1.";

#[derive(Debug)]
pub struct WanxiangLM {
    /// 文件映射；持有映射（数据不读进堆）。
    mmap: Mmap,
    /// double-array 元信息（数据区间在 `self.mmap` 内）。
    array: DoubleArray,
    /// 打分配置（第 6 步评测调参入口）。
    pub config: WanxiangConfig,
    /// 文件头信息（调试/校验）。
    _header: Header,
}

impl WanxiangLM {
    /// mmap 打开 `.gram` 文件；文件不存在、不足 44 字节、数组越界都返回
    /// [`LmError`]（调用方负责优雅退化，见 [`crate::ImeEngine::new_with_lm`]）。
    pub fn open(path: &Path) -> Result<Self, LmError> {
        let file = File::open(path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        advise_random(&mmap);
        let header = Header::parse(&mmap)?;
        // **不能只判「非空」**：空只是最没信息的一种坏文件。真正要认的是
        // 「这个格式我们支持」——上游 octagram 的数组布局随版本变，拿不认识的
        // format 硬解只会得到静默的乱码分数（Q4）。
        if !header.format.starts_with(SUPPORTED_GRAM_FORMAT_PREFIX) {
            return Err(LmError::BadGram(format!(
                "不支持的 .gram 格式 {:?}（只支持以 {:?} 开头）",
                header.format, SUPPORTED_GRAM_FORMAT_PREFIX
            )));
        }
        if header.double_array_size == 0 {
            return Err(LmError::BadGram("double_array_size 为 0".into()));
        }
        let array = DoubleArray::new(
            mmap.len(),
            header.array_file_offset,
            header.double_array_size as usize,
        )
        .ok_or_else(|| {
            LmError::BadGram(format!(
                "double-array 越界: offset {} + {}B > 文件长度 {}",
                header.array_file_offset,
                header.double_array_size as usize * 4,
                mmap.len()
            ))
        })?;
        Ok(WanxiangLM {
            mmap,
            array,
            config: WanxiangConfig::default(),
            _header: header,
        })
    }

    /// 单元数（真实文件为 105,063,424）。
    pub fn num_units(&self) -> usize {
        self.array.num_units
    }

    /// 词表规模：万象未做全量统计（需要一遍 420MB 扫描），恒为 0；
    /// 与 [`LmStore::vocab_size`] 口径一致。
    pub fn vocab_size(&self) -> u64 {
        0
    }

    fn data(&self) -> &[u8] {
        self.array.slice(&self.mmap)
    }

    /// 把「存储值 + 自然对数惩罚」折算成 log10 概率（推导见模块文档）。
    fn log10_prob_raw(&self, value: i32, penalty_ln: f64) -> f32 {
        let logp = (value as f64 / 10_000.0 + penalty_ln) / std::f64::consts::LN_10
            - self.config.log10_corpus_size;
        // **越界值按「模型兜底分」处理，不是钳到 0**。
        //
        // 2026-10-02 实测：`score(state=我, word=满额)` 会返回 +85781 这种值（.gram
        // trie 在这个 (上文,词) 组合下读出 ≈2e9 的垃圾，`value/1e4/LN_10` 之后成了
        // 正数，底层查询 bug 单独立卡）。先前把它 `.min(0.0)` 钳成 0.0 是错的——
        // log10(1) = 0 意味着「概率 1」，等于给这种词白捡满分，实测 `womenjintianqbj`
        // 因此被「我满额你今天请病假」占住（满额 lm=0.00，一路免费）。
        // 正确语义是「模型没有可信结果」⇒ 记成非词配兜底分（与空上下文同档）。
        if logp > 0.0 {
            return (self.config.non_collocation_penalty / std::f64::consts::LN_10
                - self.config.log10_corpus_size) as f32;
        }
        logp as f32
    }

    /// 无上下文 / 无命中的兜底分（`non_collocation_penalty` 折算）。
    fn floor_log10(&self) -> f32 {
        self.log10_prob_raw(0, self.config.non_collocation_penalty)
    }

    /// 核心查询：`ctx_tail`（≤5 字）为上文、`word` 为候选词，返回 log10 分。
    ///
    /// 对齐 octagram.cc `Query()` 的滑动窗：k 自 `ctx_tail` 字数递减到 1，
    /// 逐级 `traverse(上文末 k 字)` + `common_prefix_search(词首 ≤5 字)`，
    /// 取所有命中里折算后的最高分；兜底为非词配分。完整词配
    /// （`collocation_len >= min_length` 或「整段上文 + 整个词首」同时
    /// 精确命中）用 collocation_penalty，否则 weak_collocation_penalty。
    fn score_word(&self, ctx_tail: &str, word: &str) -> f32 {
        let n = window_chars(&self.config);
        let mut best = self.floor_log10();
        if ctx_tail.is_empty() {
            // octagram: 空上下文直接回退非词配分。
            return best;
        }

        // —— 词首 ≤n 字的编码：固定缓冲，零堆分配（CJK 每字 2 字节）——
        let mut wd_buf = [0u8; 24];
        let (wd_len, wd_chars) = encode_first_chars(word, n, &mut wd_buf);
        let wd_enc = &wd_buf[..wd_len];

        // —— 上文末 ≤n 字编码 + 每字字节边界（编码逐码点变长，不能按
        //    字节直接切后缀，先记字符边界）——
        let mut ctx_buf = [0u8; 24];
        let (ctx_len, ctx_chars) = encode_first_chars(ctx_tail, n, &mut ctx_buf);
        let ctx_enc = &ctx_buf[..ctx_len];
        // pos[j] = 第 j 个字符的字节起点（j = 0..=ctx_chars）
        const MAX_WINDOW: usize = 8; // 调用方已保证 ≤5，给足余量
        let mut pos = [0usize; MAX_WINDOW + 1];
        for (i, ch) in ctx_tail.chars().take(n).enumerate() {
            let mut tmp = [0u8; 4];
            let len = encode_cp_into(ch as u32, &mut tmp);
            pos[i + 1] = pos[i] + len;
        }

        let mut hits = [(0i32, 0usize); 8]; // kMaxResults
        for k in (1..=ctx_chars).rev() {
            // 上文末 k 字（按字符边界切字节）
            let suffix = &ctx_enc[pos[ctx_chars - k]..pos[ctx_chars]];
            let (ok, node, matched) = self.array.traverse(self.data(), suffix, 0);
            if !ok || matched != suffix.len() {
                continue;
            }
            let nh = self.array.common_prefix_search(self.data(), wd_enc, node, &mut hits);
            for &(v, match_bytes) in &hits[..nh] {
                let match_chars = unicode_length(wd_enc, match_bytes);
                let colloc_len = k + match_chars;
                // octagram: `collocation_len >= min`，或「整段上文 + 整个
                // 词」都精确命中 → 词配惩罚；半截窗口 → 弱词配。
                let whole = colloc_len >= self.config.collocation_min_length
                    || (k == ctx_chars && match_chars == wd_chars);
                let penalty = if whole {
                    self.config.collocation_penalty
                } else {
                    self.config.weak_collocation_penalty
                };
                let s = self.log10_prob_raw(v, penalty);
                if s > best {
                    best = s;
                }
            }
        }
        best
    }
}

/// 万象的上文状态：上文末 ≤5 字（空串 = 无上文）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WanxiangState {
    /// 上文末 ≤5 字（空串 = 无上文）。
    pub tail: String,
}

impl LanguageModel for WanxiangLM {
    /// 见 [`LanguageModel::floor_log10`]：万象的兜底分由
    /// `non_collocation_penalty` 折算（`-6 / ln10 - log10_corpus_size`）。
    fn floor_log10(&self) -> f32 {
        WanxiangLM::floor_log10(self)
    }

    type State = WanxiangState;

    fn state_from_context(&self, ctx: &LmContext) -> Self::State {
        let n = window_chars(&self.config);
        WanxiangState {
            tail: tail_chars(ctx.tail, n),
        }
    }

    fn score(&self, state: &Self::State, word: &str) -> (f32, Self::State) {
        let logp = self.score_word(&state.tail, word);
        // 新状态 = 上文并入 word 后再截到末 ≤5 字（按 char，不按字节）。
        let n = window_chars(&self.config);
        let total = state.tail.chars().count() + word.chars().count();
        let next_tail = if total <= n {
            let mut s = String::with_capacity(state.tail.len() + word.len());
            s.push_str(&state.tail);
            s.push_str(word);
            s
        } else {
            state
                .tail
                .chars()
                .chain(word.chars())
                .skip(total - n)
                .collect()
        };
        (logp, WanxiangState { tail: next_tail })
    }

    /// 只要分数:不拼那个「上文并入 word 再截断」的新查询键(后向 DP 与
    /// 独立候选评分立刻丢弃它)。
    fn score_only(&self, state: &Self::State, word: &str) -> f32 {
        self.score_word(&state.tail, word)
    }
}

#[cfg(test)]
mod tests;
