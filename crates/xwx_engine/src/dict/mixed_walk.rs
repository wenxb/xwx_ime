//! 混合形态的**索引驱动**检索：把输入切成「完整音节 / 缩写」的片段序列，
//! 每一片让副索引的键区间**逐位收窄**，走到串尾时键恰好等于收窄后的前缀，
//! 那批词条就是全部匹配。
//!
//! 与 [`super::mixed`] 交错 DFS 的区别在**代价**：
//! - DFS 枚举输入的**读法**（每种读法一次二分），预算
//!   `MAX_MIXED_DFS_STEPS` 是全局的、深度优先 ⇒ 缩写位在开头/中间时预算在
//!   左侧子树烧光（实测 `dshzhang` → 董事长 不可达，top3 是打胜仗/大上涨）；
//! - 本模块的代价 ∝ **命中词条数**：每片只做一次区间收窄（区间随深度变小），
//!   死分支当场消失——没有预算，也就没有「哪种形状不可达」。
//!
//! 可表达的写法（副索引的键是「每音节一首字母」）：
//! - 完整音节位：键贡献该音节的**首字母**，约束「这一位等于该音节」；
//! - 单字母缩写位：键贡献该字母，首字母约束由键本身保证；
//! - 整体声母缩写位（`zh`/`ch`/`sh`）：键贡献首字母，约束「这一位以 `Xh` 开头」。
//!
//! 因此折叠（`jzd`）与全声母（`jzhd`）走**同一条**代码路径，不再需要
//! `abbrev_variant_reads` 的变体枚举与事后真值校验：约束是拿到词条拼音后
//! 逐位验证的，不存在「哪种写法覆盖不全」。

use std::collections::HashMap;
use std::sync::OnceLock;

use super::abbrev::AbbrevIndex;
use super::mixed::syllable_matches;
use super::{Candidate, DictError, MmapDict};

/// 单次查询最多收集的词条数（收集期上界；最终候选名额由引擎的
/// `MAX_CANDIDATES` 决定）。
///
/// **1024 → 512（2026-10-05）**：上界与引擎窗口（`MAX_CANDIDATES = 512`）
/// 对齐。超出的部分排完序也会被截掉，但它们在**收集期**就要过一次
/// LM + 词条打分——`zh`/`sh` 这类池子上万的键实测多花 ~1.4ms/查询
/// （`perf_probe` 的 `mixed:zhongg` 第 2 键）。
///
/// 为什么不是更小：`abbrev.bin` 的 2 字母键词条数 p90 = 570、p99 = 912，
/// 512 覆盖到 p90 附近；再小就开始按扫描顺序丢真候选了。
/// `lookup_abbrev` 的 500 早停仍是独立的第二道口子。
const MAX_WALK_RESULTS: usize = 512;

/// 状态访问上限：病态输入（`zzzzzzzz`）的最坏情况钳成常数。
///
/// 真实输入的访问量 ∝ 命中的键前缀数（实测目标形态在几十~几百），
/// 离这个上限很远；它的作用只是「任意输入的最坏情况有界」。
const MAX_WALK_STATES: usize = 8192;

/// 区间上界哨兵：大于任何小写字母，用来切出「以 `prefix + c` 开头」的区间。
const RANGE_END: u8 = 0x7f;

/// 词条某一位音节的约束（由该片段在输入里的写法决定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cons {
    /// 缩写位（1 个字母）：键的首字母已经保证这一位以该字母开头。
    Init,
    /// 整体声母缩写位（`zh`/`ch`/`sh`）：这一位必须以「该字母 + h」开头。
    Digraph(u8),
    /// 完整音节位：这一位必须等于该音节。
    Full(&'static str),
}

/// 音节表按首字母分桶（长度降序），只建一次。
///
/// 与 `mixed.rs` 的分桶同一目的：`syllable_matches` 要求首字节相同，
/// 别的桶永远不可能匹配；不分桶时每个状态都要把 412 条音节全试一遍。
fn buckets() -> &'static ([Vec<&'static str>; 26], Vec<&'static str>) {
    static BUCKETS: OnceLock<([Vec<&'static str>; 26], Vec<&'static str>)> = OnceLock::new();
    BUCKETS.get_or_init(|| {
        let mut all: Vec<&'static str> = crate::segmenter::SYLLABLES.to_vec();
        // 长度降序：完整音节里「打得最全」的读法优先落地。
        all.sort_by_key(|s| (std::cmp::Reverse(s.len()), *s));
        let mut b: [Vec<&'static str>; 26] = std::array::from_fn(|_| Vec::new());
        for s in &all {
            if let Some(i) = s.as_bytes().first().and_then(|c| c.checked_sub(b'a')) {
                if (i as usize) < 26 {
                    b[i as usize].push(s);
                }
            }
        }
        (b, all)
    })
}

struct Walk<'a> {
    idx: &'a AbbrevIndex,
    input: &'a [u8],
    /// 收集池：同一 (词, 全拼) 只留最高词频（不同片段序列可能命中同一条）。
    out: Vec<Candidate>,
    seen: HashMap<(String, String), usize>,
    states: usize,
    /// [`Self::narrow`] 的复用缓冲：每个状态至多试 6 个音节 + 2 条缩写弧，
    /// 每次新建 `Vec` 是 49k 次分配/查询的实测来源之一（长串输入尤其明显）。
    probe: Vec<u8>,
}

impl MmapDict {
    /// 混合形态查询：输入按「完整音节 / 缩写」的全部切法对齐到副索引的键。
    ///
    /// 副索引缺失时返回空（与 [`MmapDict::lookup_abbrev`] 同一退化口径）。
    pub fn lookup_abbrev_patterns(&self, input: &str) -> Result<Vec<Candidate>, DictError> {
        let Some(idx) = self.abbrev.as_ref() else {
            return Ok(Vec::new());
        };
        if input.is_empty() || !input.is_ascii() {
            return Ok(Vec::new());
        }
        let mut w = Walk {
            idx,
            input: input.as_bytes(),
            out: Vec::new(),
            seen: HashMap::new(),
            states: 0,
            probe: Vec::with_capacity(input.len() + 2),
        };
        w.walk(&mut Vec::new(), &mut Vec::new(), 0, idx.key_count(), 0)?;
        Ok(w.out)
    }
}

impl Walk<'_> {
    /// 在键区间 `[lo, hi)`（已按 `prefix` 收窄）与输入位置 `pos` 处继续切分。
    fn walk(
        &mut self,
        prefix: &mut Vec<u8>,
        cons: &mut Vec<Cons>,
        lo: usize,
        hi: usize,
        pos: usize,
    ) -> Result<(), DictError> {
        if self.states >= MAX_WALK_STATES || self.out.len() >= MAX_WALK_RESULTS {
            return Ok(());
        }
        self.states += 1;
        if pos == self.input.len() {
            return self.collect(prefix, cons, lo, hi);
        }
        let rest = &self.input[pos..];
        // 片段按「长优先」：完整音节 → 双字母声母 → 单字母缩写。
        let (by_first, all) = buckets();
        let bucket: &[&'static str] = match rest[0].checked_sub(b'a') {
            Some(i) if (i as usize) < 26 => &by_first[i as usize],
            _ => all,
        };
        for &syl in bucket {
            if !syllable_matches(syl, rest) {
                continue;
            }
            let c = syl.as_bytes()[0];
            if let Some((s, e)) = self.narrow(prefix, c, lo, hi)? {
                prefix.push(c);
                cons.push(Cons::Full(syl));
                self.walk(prefix, cons, s, e, pos + syl.len())?;
                cons.pop();
                prefix.pop();
            }
        }
        if rest.len() >= 2 && matches!(&rest[..2], b"zh" | b"ch" | b"sh") {
            let c = rest[0];
            if let Some((s, e)) = self.narrow(prefix, c, lo, hi)? {
                prefix.push(c);
                cons.push(Cons::Digraph(c));
                self.walk(prefix, cons, s, e, pos + 2)?;
                cons.pop();
                prefix.pop();
            }
        }
        if rest[0].is_ascii_lowercase() {
            let c = rest[0];
            if let Some((s, e)) = self.narrow(prefix, c, lo, hi)? {
                prefix.push(c);
                cons.push(Cons::Init);
                self.walk(prefix, cons, s, e, pos + 1)?;
                cons.pop();
                prefix.pop();
            }
        }
        Ok(())
    }

    /// 把 `[lo, hi)` 收窄到「键以 `prefix + c` 开头」的子区间；没有则 `None`。
    fn narrow(
        &mut self,
        prefix: &[u8],
        c: u8,
        lo: usize,
        hi: usize,
    ) -> Result<Option<(usize, usize)>, DictError> {
        // 分字段借用：`probe` 当缓冲、`idx` 去查表，互不冲突。
        let Walk { idx, probe, .. } = self;
        probe.clear();
        probe.extend_from_slice(prefix);
        probe.push(c);
        let s = idx.lower_bound_in(probe, lo, hi)?;
        if s >= hi || !idx.key_starts_with(s, probe)? {
            return Ok(None);
        }
        probe.push(RANGE_END);
        let e = idx.lower_bound_in(probe, s, hi)?;
        Ok(Some((s, e)))
    }

    /// 串尾收口：键必须**恰好等于** `prefix`（音节数 == 片段数），
    /// 再逐位验证完整音节/双声母约束。
    fn collect(
        &mut self,
        prefix: &[u8],
        cons: &[Cons],
        lo: usize,
        hi: usize,
    ) -> Result<(), DictError> {
        let room = MAX_WALK_RESULTS - self.out.len();
        for (word, pinyin, freq) in self.idx.exact_entries(lo, hi, prefix.len(), room)? {
            if !cons_ok(&pinyin, cons) {
                continue;
            }
            match self.seen.get(&(word.clone(), pinyin.clone())) {
                Some(&i) => {
                    if freq > self.out[i].freq {
                        self.out[i].freq = freq;
                    }
                }
                None => {
                    self.seen.insert((word.clone(), pinyin.clone()), self.out.len());
                    self.out.push(Candidate::new(word, pinyin, freq));
                }
            }
        }
        Ok(())
    }
}

/// 逐位校验词条拼音是否满足片段序列给出的约束；音节数必须相等。
fn cons_ok(pinyin: &str, cons: &[Cons]) -> bool {
    let mut n = 0usize;
    for syl in pinyin.split(' ') {
        let Some(c) = cons.get(n) else {
            return false;
        };
        n += 1;
        let ok = match *c {
            // 键的首字母就是这一位的首字母，无需再验。
            Cons::Init => true,
            Cons::Digraph(letter) => {
                let b = syl.as_bytes();
                b.len() >= 2 && b[0] == letter && b[1] == b'h'
            }
            Cons::Full(want) => {
                // **精确相等**（不是 `syllable_matches` 的前缀匹配——那条口径
                // 是「拿音节去比输入剩余串」，`zhan` 会匹配上 `zhang`）。
                // üe 的两套写法由全项目唯一的归一函数抹平。
                let (a, b) = (
                    crate::segmenter::canonical_syllable(syl),
                    crate::segmenter::canonical_syllable(want),
                );
                a == b
            }
        };
        if !ok {
            return false;
        }
    }
    n == cons.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cons_check_rejects_wrong_syllable_count() {
        // 音节数必须等于片段数：多一位/少一位都不算匹配。
        assert!(!cons_ok("dong shi zhang", &[Cons::Init, Cons::Init]));
        assert!(!cons_ok("dong shi", &[Cons::Init, Cons::Init, Cons::Init]));
        assert!(cons_ok("dong shi zhang", &[Cons::Init, Cons::Init, Cons::Init]));
    }

    #[test]
    fn cons_check_digraph_and_full() {
        assert!(cons_ok("dong shi zhang", &[Cons::Init, Cons::Digraph(b's'), Cons::Full("zhang")]));
        // 折叠写法不该被双声母约束放行。
        assert!(!cons_ok("dong si zhang", &[Cons::Init, Cons::Digraph(b's'), Cons::Full("zhang")]));
        // 完整音节位要求逐字节相等。
        assert!(!cons_ok("dong shi zhang", &[Cons::Init, Cons::Init, Cons::Full("zhan")]));
        // üe 归一化（`lue`/`lve`）走共享匹配口径。
        assert!(cons_ok("nue", &[Cons::Full("nve")]));
    }
}
