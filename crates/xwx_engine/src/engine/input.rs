//! 归一化后的输入:原文、干净串、B2 边界表与两套坐标之间的映射。
//!
//! codex 审查 A1:归一化原先在 [`super::lookup`] 与 [`super::rank`] 各写
//! 一遍,「干净串覆盖长度 → 原文长度」的换算又写在 [`super::lattice`],
//! 三处口径只能靠注释同步。这里集中成一个内部类型,让坐标在类型上就分得清:
//!
//! - **原文**(`raw`):用户敲的串,含 `'`。**展示**坐标——preedit、
//!   回显、分段上屏的切点都按它算([`Candidate::covered`] 也是它);
//! - **干净串**(`clean`):去掉 `'` 的串。**切分/查表**坐标——格构建、
//!   beam、后向 DP、u_min 表([`super::rank::unreachable_tail`])都按它算;
//! - **规范拼音**(`Candidate::pinyin`):词典 key 的坐标,空格分隔音节、
//!   ü 记成 v,既不等于原文也不等于干净串。
//!
//! 三者不能混用:把干净串坐标当原文用会让 `xi'an` 的分段上屏少吃掉
//! `'`(codex 审查 D9 的反面),把候选拼音长度当消费位置会让混拼/容错
//! 候选落进错误的 u_min 桶(A1 记的另一半,待排序改用真实覆盖时落地)。

use std::borrow::Cow;

/// 一次查询的输入,归一化一次、往下传一次。
///
/// 无 `'` 的输入走 `Cow::Borrowed` + 空边界表:零分配、零拷贝,与引入本
/// 类型之前逐字节一致(判退化的硬约束)。有 `'` 时 `'` 本身就是单字节
/// ASCII,干净串里的字节偏移可以直接当边界表用。
#[derive(Debug, Clone)]
pub(super) struct NormalizedInput<'a> {
    raw: &'a str,
    clean: Cow<'a, str>,
    /// 每个 `'` 在**干净串**中的字节偏移(原始顺序、可重复)。
    bounds: Vec<usize>,
}

impl<'a> NormalizedInput<'a> {
    /// 按 B2 分隔符语义归一化:删掉 `'`、记下它落在干净串的哪个位置。
    pub(super) fn new(raw: &'a str) -> Self {
        if !raw.contains('\'') {
            return Self {
                raw,
                clean: Cow::Borrowed(raw),
                bounds: Vec::new(),
            };
        }
        let mut clean = String::with_capacity(raw.len());
        let mut bounds = Vec::new();
        for ch in raw.chars() {
            if ch == '\'' {
                bounds.push(clean.len());
            } else {
                clean.push(ch);
            }
        }
        Self {
            raw,
            clean: Cow::Owned(clean),
            bounds,
        }
    }

    /// 用户敲的原文(含 `'`),展示坐标。
    pub(super) fn raw(&self) -> &'a str {
        self.raw
    }

    /// 去掉 `'` 的干净串,切分/查表坐标。
    pub(super) fn clean(&self) -> &str {
        &self.clean
    }

    /// 干净串的字节长度。
    pub(super) fn clean_len(&self) -> usize {
        self.clean.len()
    }

    /// 归一化后是否为空(输入全是 `'`,如 `'`、`''`)。
    pub(super) fn is_empty(&self) -> bool {
        self.clean.is_empty()
    }

    /// 边界表;`None` 表示没有 `'`,调用方走零开销路径。
    pub(super) fn bounds(&self) -> Option<&[usize]> {
        if self.bounds.is_empty() {
            None
        } else {
            Some(&self.bounds)
        }
    }

    /// 干净串坐标里的消费长度 → **原文**坐标里的消费长度。
    ///
    /// `'` 也是被消费的字节(分段上屏要把它一起从组合串里吃掉),所以
    /// 落在消费范围内的每个边界各加 1。边界严格落在音节内部时非法由
    /// 格构建负责([`super::lattice`] 的 `syllable_crosses_boundary`),
    /// 本函数只做长度换算。
    pub(super) fn to_raw_len(&self, clean_len: usize) -> usize {
        // 越界长度钳在整串:调用方已经把覆盖长度钳进 `0..=clean_len`
        // (见 [`super::lattice::ImeEngine::fill_covered`]),这里再钳一次
        // 是为了让「原文坐标 ≤ raw.len()」成为类型级不变量,而不是靠
        // 每个调用方各自记得。
        let clean_len = clean_len.min(self.clean.len());
        clean_len + self.bounds.iter().filter(|&&x| x <= clean_len).count()
    }
}
