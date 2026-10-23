//! 语言模型（language model）抽象与二元模型实现。
//!
//! 拼音输入法的候选排序高度依赖语言模型：`"xian"` 该切分成 `"xian"`
//! 还是 `"xi an"`，候选里 `"西安"` 和 `"先"` 谁在前，都要靠模型打分。
//!
//! 接口设计：**状态化上下文**（KenLM 风格）。
//!
//! - 核心语义是 [`LanguageModel::score`]：给定当前状态（不透明）与
//!   候选词，返回 log10 概率和**推进后的新状态**。状态里装什么由实现
//!   自己决定——bigram 装「前一个词」，万象（Rime octagram `.gram`，
//!   字符级窗口，见 [`crate::wanxiang`]）装「上文末 ≤5 字」的查询素材；
//!   调用方（beam search 解码）不需要知道区别，只需要沿路径单独维护
//!   一份状态；
//! - 聚合沿用词库的枚举分发惯例（[`crate::dict::DictStore`]）：带关联
//!   类型的 trait 无法用 `dyn Trait`，所以由 [`LmState`] 统一收纳各
//!   实现的状态、[`LmStore`] 收纳各实现（当前 [`BigramLM`] 与
//!   [`crate::wanxiang::WanxiangLM`] 两个变体）。
//!
//! `score` 不返回 `Result`：各实现（bigram、将来的万象模型）对任何输入
//! 都有平滑回退，打分不会失败；模型数据加载/格式错误由 [`LmError`]
//! 在 `load` 时报告。
//!
//! 当前 `BigramLM` 是纯内存的 HashMap 二元模型（词对计数），足够跑通
//! 骨架；TODO: 数据量大后改为 mmap 二进制格式（`memmap2` 依赖已就位）。

use std::collections::HashMap;
use std::io::BufRead;

use thiserror::Error;

use crate::user_lm::{UserLM, UserLmSnapshot};
use crate::wanxiang::{WanxiangLM, WanxiangState};

/// 语言模型错误。
#[derive(Debug, Error)]
pub enum LmError {
    /// 模型数据文件某行格式不对。
    #[error("语言模型数据格式错误（第 {line} 行）: {detail}")]
    BadLine { line: u64, detail: String },
    /// 万象 `.gram` 文件格式/结构错误（头部不足、数组越界等）。
    #[error(".gram 格式错误: {0}")]
    BadGram(String),
    /// 该模型变体不支持文本流 `load`（万象只能 mmap `open`）。
    #[error("模型 {model} 不支持从文本流加载（请用 mmap open / new_with_lm）")]
    UnsupportedLoad { model: &'static str },
    /// I/O 错误。
    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),
}

/// 语言模型抽象：状态化打分。
///
/// 所有分数都是 **log10 概率**（负值，越大越好），这样多词序列可以
/// 直接相加而不会下溢。上下文不是「一个词」而是不透明状态，由
/// [`Self::State`] 承载，实现自己决定状态里存什么。
pub trait LanguageModel: Send + Sync {
    /// 不透明上下文状态：bigram 存前一个词，万象存「上文末 ≤5 字 +
    /// 词首 ≤5 字」的查询键，trigram 存前两个词。
    ///
    /// `Clone` 是硬要求：Viterbi/beam search 在路径分叉时沿每条候选
    /// 路径各复制一份状态继续推进。
    type State: Clone;

    /// 初始上下文（句首 / 无上文）：bigram 里等价于 `prev = None`，
    /// 万象里等价于空上文（只给出候选词的平滑分）。
    ///
    /// 恒等于 `state_from_context(&LmContext::empty())`，调用方在新句 /
    /// 无上文时用哪个都行（默认实现保证两者永远一致）。
    fn initial_state(&self) -> Self::State {
        self.state_from_context(&LmContext::empty())
    }

    /// 「无证据基线」分（log10）：上下文与词都不提供信息时模型给出的值。
    ///
    /// 打分层用它做**路径级补偿**：多词读法不该按词数反复支付这个基线。
    /// 默认 0.0 = 该模型没有这个概念、调用方不补偿。
    fn floor_log10(&self) -> f32 {
        0.0
    }

    /// 从真实已上屏上下文构造状态。
    ///
    /// 第 2 步起，`InputSession` 保留已上屏文本（有界），每轮查询前用
    /// 它构造状态再打分：bigram 读 [`LmContext::last_word`]（前一个词），
    /// 将来的万象（Rime octagram `.gram`）读 [`LmContext::tail`] 的
    /// 末 ≤5 字（字符级窗口，键里无分隔符）。
    fn state_from_context(&self, ctx: &LmContext) -> Self::State;

    /// 在上下文 `state` 下给 `word` 打分。
    ///
    /// 返回 `(log10 概率, 消费 `word` 后的新状态)`。`word` 的语义由
    /// 实现解释：bigram 里它就是「上一个词」本身；万象实现则只取
    /// 其头部 ≤5 字参与查询（词尾不进入查询键）。
    ///
    /// 调用方要不要采用返回的新状态由自己决定——例如 Viterbi 只沿
    /// 被选中的路径推进状态，被剪掉的路径丢弃。
    fn score(&self, state: &Self::State, word: &str) -> (f32, Self::State);

    /// 只要分数、不要新状态。
    ///
    /// 后向 DP、独立候选评分这类调用方**立刻丢弃**推进出来的状态,而
    /// [`Self::score`] 必须为它付一次分配(bigram 一个 `String`、万象一个
    /// 查询键、混合层一个 `Box` + 状态克隆)。默认实现退化成
    /// `self.score(state, word).0`,**语义必须逐位相同**;各实现覆盖它,
    /// 才真的省下那次分配。相等性由 `lm/tests.rs` 的
    /// `score_only_matches_score_bit_for_bit` 在每个实现上钉住。
    fn score_only(&self, state: &Self::State, word: &str) -> f32 {
        self.score(state, word).0
    }
}

/// 已上屏文本构成的 LM 查询上下文。
///
/// 两个字段**都要留**：bigram 只读 [`Self::last_word`]（词级），将来的
/// 万象只读 [`Self::tail`]（字符级窗口模型）。只留一个会让第 3 步再改
/// 一次接口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LmContext<'a> {
    /// 已上屏文本的末尾若干字（字符级模型用；万象取末 ≤5 字）。
    pub tail: &'a str,
    /// 最后一个上屏的词（词级 bigram 用；`None` = 无词级上文）。
    pub last_word: Option<&'a str>,
}

impl<'a> LmContext<'a> {
    /// 无上下文（句首 / 没上过屏）。
    pub const fn empty() -> LmContext<'static> {
        LmContext { tail: "", last_word: None }
    }
}

/// 未知词的对数概率兜底值（log10(10^-8)）。
/// TODO: 由词表规模推导（如 `-log10(vocab * k)`），当前为经验值。
pub(crate) const UNK_LOG_PROB: f32 = -8.0;

/// 基于 HashMap 计数的二元语言模型。
#[derive(Debug, Default)]
pub struct BigramLM {
    /// 一元计数：词 -> 出现次数。
    pub(crate) unigrams: HashMap<String, u64>,
    /// 二元计数：前词 -> (后词 -> 出现次数)。
    ///
    /// 用嵌套 map 而不是 `(String, String)` 复合键：打分热路径
    /// （beam search 每步每边一次 `score`）可以直接拿 `&str` 做两层
    /// 借用查找，不用为查询临时分配两个 `String`。
    pub(crate) bigrams: HashMap<String, HashMap<String, u64>>,
    /// 一元计数总和 Σc(w)，用于算归一化概率。
    pub(crate) unigram_total: u64,
    /// 不同词的数量（加一平滑的分母项）。
    vocab: u64,
}

impl BigramLM {
    /// 构造空模型。
    pub fn new() -> Self {
        Self::default()
    }

    /// 累加一元计数（首次出现的词会更新 `vocab`）。
    pub fn add_unigram(&mut self, word: &str, count: u64) {
        if count == 0 {
            return;
        }
        let entry = self.unigrams.entry(word.to_string()).or_insert(0);
        if *entry == 0 {
            // 新词：词表 +1（加一平滑需要）
            self.vocab += 1;
        }
        *entry += count;
        self.unigram_total += count;
    }

    /// 累加二元计数。
    pub fn add_bigram(&mut self, prev: &str, word: &str, count: u64) {
        if count == 0 {
            return;
        }
        *self.bigrams
            .entry(prev.to_string())
            .or_default()
            .entry(word.to_string())
            .or_insert(0) += count;
    }

    /// 从文本流加载模型。
    ///
    /// 格式（`data/README.md` 有完整说明）：
    /// - 两个制表符字段：`词<TAB>次数` -> 一元计数；
    /// - 三个制表符字段：`前词<TAB>后词<TAB>次数` -> 二元计数；
    /// - `#` 开头为注释，空行跳过。
    ///
    /// TODO: 增加二进制 mmap 格式（`memmap2`），把加载时间从 O(n) 降到 O(1)
    /// 映射；同时支持增量更新文件。
    pub fn load(&mut self, reader: impl BufRead) -> Result<(), LmError> {
        for (i, line) in reader.lines().enumerate() {
            let line_no = (i + 1) as u64;
            let line = line?;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                [word, count] => {
                    let count: u64 = count.parse().map_err(|e| LmError::BadLine {
                        line: line_no,
                        detail: format!("次数不是整数: {count:?} ({e})"),
                    })?;
                    self.add_unigram(word, count);
                }
                [prev, word, count] => {
                    let count: u64 = count.parse().map_err(|e| LmError::BadLine {
                        line: line_no,
                        detail: format!("次数不是整数: {count:?} ({e})"),
                    })?;
                    self.add_bigram(prev, word, count);
                }
                _ => {
                    return Err(LmError::BadLine {
                        line: line_no,
                        detail: format!("期望 2 或 3 个制表符分隔字段，实际: {line:?}"),
                    });
                }
            }
        }
        Ok(())
    }

    /// 词表规模（调试/统计用）。
    pub fn vocab_size(&self) -> u64 {
        self.vocab
    }

    /// 核心打分：log10 P(word | prev)，加一平滑 + 回退。
    ///
    /// 回退链：bigram 计数 → unigram 计数 → 未知词兜底，逐级降级，
    /// 保证任何输入都返回有限分数（不 panic、不返回 NaN）。
    fn log10_prob(&self, prev: Option<&str>, word: &str) -> f32 {
        // 1) 有上文且 bigram 有计数：P = (c(prev,word) + 1) / (c(prev) + V)
        if let Some(prev) = prev {
            if let Some(&c) = self
                .bigrams
                .get(prev)
                .and_then(|inner| inner.get(word))
            {
                let ch = self.unigrams.get(prev).copied().unwrap_or(0) as f32;
                let denom = ch + self.vocab as f32;
                if denom > 0.0 {
                    return ((c as f32 + 1.0) / denom).log10();
                }
            }
        }
        // 2) 回退到 unigram：P = (c(word) + 1) / (N + V)
        let cw = self.unigrams.get(word).copied().unwrap_or(0) as f32;
        let denom = self.unigram_total as f32 + self.vocab as f32;
        if cw > 0.0 && denom > 0.0 {
            return ((cw + 1.0) / denom).log10();
        }
        // 3) 未知词：统一兜底分
        // TODO: 用词表规模推导，并考虑与 unigram 分对齐。
        UNK_LOG_PROB
    }
}

impl LanguageModel for BigramLM {
    /// bigram 的上文状态：前一个词（`None` = 无上文 / 句首）。
    type State = Option<String>;

    /// 只取 [`LmContext::last_word`]：bigram 是词级模型，`tail` 属于
    /// 万象（字符级）；`tail` 里有字但 `last_word` 为 `None`（如原始
    /// 标点上屏）就是无词级上文。
    fn state_from_context(&self, ctx: &LmContext) -> Self::State {
        ctx.last_word.map(str::to_string)
    }

    fn score(&self, state: &Self::State, word: &str) -> (f32, Self::State) {
        let logp = self.log10_prob(state.as_deref(), word);
        // 消费这个词后上下文推进为它自己（是否采用由调用方决定）。
        (logp, Some(word.to_string()))
    }

    /// 只要分数:同一个 `log10_prob`,不建 `Some(word.to_string())`。
    fn score_only(&self, state: &Self::State, word: &str) -> f32 {
        self.log10_prob(state.as_deref(), word)
    }
}


// ---------------------------------------------------------------------------
// 混合语言模型(第 5 步:通用模型 + 用户学习模型)
// ---------------------------------------------------------------------------

/// 用户层「计数增益」的标定常数:**上屏一次 ≈ 该词词频提高 R 倍**。
///
/// 个人化的口径是**计数驱动**的加法项(不是概率插值,见 [`MixedLM`]):
/// 用户层第 c 次上屏某个词,给它的 LM 分加
/// `log10(1 + c·R)`。取 R = 1000(2026-10-02 实测标定,真实词库 + 万象):
///
/// - 一次上屏 = **+3.0 log10**(≈ ×1000 等效词频)——实测同类候选之间
///   「盖过第 1 名」需要的分差只有 0.002~3.7(`ky` 0.005 / `xian` 0.91 /
///   `nihao` 1.96 / `zhongguo` 2.12 / `weishenme` 3.70),所以 R=1000 时
///   **一次误选/一次正确选择都能在同类候选里登顶**,与主流输入法
///   「选过的词排前面」一致;
/// - 三次上屏 = +3.6,十次 = +4.0,三十次 ≈ +4.5——**单调、收敛**
///   (log 增长),引用计数越多越靠前但不会无限霸屏;
/// - **翻不过「整词 vs 逐字」那道结构墙**:实测只覆盖前缀的单字落后
///   13.7~14.7 log10(`women` 的「我」、`diannao` 的「电」),按本曲线
///   要上屏 5×10¹⁰ 次以上才会翻过去——结构上不可能,这条护栏是刻意的
///   (否则打过一次「我」,以后 `women` 永远先出「我」)。
///
/// 为什么不用概率插值(旧实现):用户层的概率分母是**用户自己的语料规模**,
/// 与通用层量纲不可比。实测同一句「学 1 次目标词」,加分随无关语料从
/// +11.45(1 条)掉到 +7.45(10001 条)——重度用户的个人化比新用户弱
/// 4 个数量级;而计数与语料规模无关(由
/// `mixed_boost_independent_of_corpus_size` 钉住)。
pub const USER_COUNT_GAIN_R: f32 = 1000.0;

/// 单个计数项的加分(0 次 = 不加分,曲线在 0 处连续)。
///
/// 抽成独立函数是为了让「与语料规模无关」这条性质可被直接测:
/// 它只吃计数,不碰任何分母。
pub(crate) fn count_gain(c: u32) -> f32 {
    if c == 0 {
        0.0
    } else {
        (1.0 + c as f32 * USER_COUNT_GAIN_R).log10()
    }
}

/// 混合语言模型:通用层(万象 / bigram)+ 用户学习层。
///
/// 口径是 **backoff + 计数增益**(不是概率插值):
/// `score = base(word|ctx) + Σ count_gain(c)`,
/// 计数来自用户层的一元(词被上屏几次)与二元(该词跟在上一词后上屏几次)。
/// 三条性质都由这个形状直接给出:
///
/// - **没有证据就不改变通用层**(c=0 ⇒ 加分为 0,逐位相同)——旧实现给
///   「没学过」的词发一个绝对中性分,导致用户层的绝对量纲(随语料规模
///   变化)把学过的词压到没学过的词下面;
/// - **学习永不降权**(加分 ≥ 0),负证据走 Ctrl+Delete 的屏蔽表;
/// - **与用户语料规模无关**(加分只吃计数)。
///
/// 为什么做成独立模型而不是给 `ImeEngine` 加 `user_lm` 字段:
/// 调用方(beam 解码/兜底/会话)只跟 `LanguageModel` 一个接口打交道,
/// 换模型/加模型不该改调用方——这正是第 1 步状态化 trait 的初衷;
/// 混合作为一种模型实现收进 [`LmStore::Mixed`],[`LmState::Mixed`]
/// 的穷尽性检查会强制所有分发点补全。
pub struct MixedLM {
    /// 通用层(人人一样的语料统计,枚举分发)。
    ///
    /// `Box` 间接是硬性需要:`LmStore::Mixed(MixedLM)` 与这个字段互相
    /// 递归,没有间接层类型大小无限(E0072);顺带让「Mixed 套 Mixed」
    /// 在类型层面仍然是可能的(递归 enum),因此嵌套约束由
    /// [`MixedLM::new`] 的 debug_assert + 测试守住(见 [`LmStore::Mixed`])。
    base: Box<LmStore>,
    /// 用户层(自己打字攒的,内部可变;学习由会话钩子驱动)。
    user: UserLM,
}

impl std::fmt::Debug for MixedLM {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MixedLM")
            .field("base", &self.base)
            .field("gain_r", &USER_COUNT_GAIN_R)
            .finish_non_exhaustive()
    }
}

/// 混合模型的 beam 路径状态:两层状态 + 打分快照。
///
/// 快照内置在状态里的原因:一次查询的几十上百次 `score` 必须看到
/// 同一份用户计数(否则 beam 中途一次上屏会让同层路径不可比),
/// 且不能为每次 `score` 取 `RwLock`;快照是 `Arc` 克隆,O(1)。
#[derive(Debug, Clone, PartialEq)]
pub struct MixedState {
    /// 通用层状态(`Box` 间接原因同 [`MixedLM::base`]:打破
    /// `LmState::Mixed(MixedState)` 的无限大小递归)。
    base: Box<LmState>,
    /// 用户层的词级上文(上一个词;`None` = 无)。
    user_prev: Option<String>,
    /// 用户计数快照(查询开始时取自 [`UserLM::snapshot`])。
    snap: UserLmSnapshot,
}

impl MixedLM {
    /// 构造混合模型。`base` 不能自身再是 `Mixed`(嵌套混合没有意义,
    /// 调用方直接搭两层即可)。
    ///
    /// 没有权重参数:个人化的强度由 [`USER_COUNT_GAIN_R`] 单点决定,
    /// 且与用户语料规模无关(旧实现的 `user_weight` 是概率域插值权重,
    /// 已被计数增益取代)。
    pub fn new(base: LmStore, user: UserLM) -> Self {
        debug_assert!(
            !matches!(base, LmStore::Mixed(_)),
            "混合模型不应再嵌套混合"
        );
        MixedLM { base: Box::new(base), user }
    }

    /// 用户层句柄(学习/落盘走这里;打分不需要它)。
    pub fn user(&self) -> &UserLM {
        &self.user
    }
}

impl LanguageModel for MixedLM {
    fn floor_log10(&self) -> f32 {
        self.base.floor_log10()
    }

    type State = MixedState;

    fn state_from_context(&self, ctx: &LmContext) -> MixedState {
        MixedState {
            base: Box::new(self.base.state_from_context(ctx)),
            user_prev: ctx.last_word.map(str::to_string),
            snap: self.user.snapshot(),
        }
    }

    fn score(&self, state: &MixedState, word: &str) -> (f32, MixedState) {
        let (base_logp, base_next) = self.base.score(&state.base, word);
        // 个人化 = 计数增益的**加法项**,不碰通用层的量纲:
        // 一元计数(这个词被上屏几次)+ 二元计数(它跟在上一词后又几次)。
        // 两个计数各自过 `count_gain`,0 次时加 0(逐位回退通用层)。
        let (c_uni, c_bi) = self
            .user
            .counts_with(&state.snap, state.user_prev.as_deref(), word);
        let boost = count_gain(c_uni) + count_gain(c_bi);
        (
            base_logp + boost,
            MixedState {
                base: Box::new(base_next),
                user_prev: Some(word.to_string()),
                snap: state.snap.clone(),
            },
        )
    }

    /// 只要分数:不建 `MixedState`(那要 `Box` 一次 + 一次 `String` +
    /// 一次快照克隆)。**表达式顺序与 `score` 保持一致**——f32 加法不满足
    /// 结合律,换个括号就不再逐位相同了。
    fn score_only(&self, state: &MixedState, word: &str) -> f32 {
        let base_logp = self.base.score_only(&state.base, word);
        let (c_uni, c_bi) = self
            .user
            .counts_with(&state.snap, state.user_prev.as_deref(), word);
        let boost = count_gain(c_uni) + count_gain(c_bi);
        base_logp + boost
    }
}

/// 统一的语言模型状态。
///
/// [`LmStore`] 做枚举分发时，带关联类型的 [`LanguageModel`] 需要一个
/// 单一状态类型，因此各实现的状态收进这一个枚举，调用方只把它当
/// **不透明值**传递（克隆、喂给下一轮 `score`），不碰内部结构。
#[derive(Debug, Clone, PartialEq)]
pub enum LmState {
    /// bigram 的上文：前一个词（`None` = 无上文）。
    Bigram(Option<String>),
    /// 万象（`.gram` 字符级窗口）的上文状态：上文末 ≤5 字
    /// （空串 = 无上文；配合窗口模型直接拼接查询键）。
    Wanxiang(WanxiangState),
    /// 混合模型的状态:两层状态 + 用户计数快照(见 [`MixedState`])。
    Mixed(MixedState),
}

/// 语言模型聚合（枚举分发，与 [`crate::dict::DictStore`] 同一惯例）。
///
/// 带关联类型的 trait 无法 `dyn Trait`，沿用仓库「具体类型」的偏好：
/// bigram（文本计数）与万象 LTS（mmap `.gram`）二选一承载。
#[derive(Debug)]
pub enum LmStore {
    /// 词对二元模型（文本流加载）。
    Bigram(BigramLM),
    /// 万象 LTS 字符级窗口模型（mmap `.gram`，见 [`crate::wanxiang`]）。
    Wanxiang(WanxiangLM),
    /// 两层混合模型:通用层(上面的任一变体)+ 用户学习层,
    /// 打分在 log10 概率域线性插值(见 [`MixedLM`];嵌套 Mixed
    /// 被 [`MixedLM::new`] 的 debug_assert 拒绝)。
    Mixed(MixedLM),
}

impl LmStore {
    /// 以文本流加载模型（仅 bigram 变体支持；万象变体请用
    /// [`crate::ImeEngine::new_with_lm`] 或 mmap [`crate::wanxiang::WanxiangLM::open`]）。
    pub fn load(&mut self, reader: impl BufRead) -> Result<(), LmError> {
        match self {
            Self::Bigram(lm) => lm.load(reader),
            Self::Wanxiang(_) => Err(LmError::UnsupportedLoad { model: "wanxiang" }),
            Self::Mixed(_) => Err(LmError::UnsupportedLoad { model: "mixed" }),
        }
    }

    /// 词表规模（调试/统计用；万象变体未统计，见
    /// [`crate::wanxiang::WanxiangLM::vocab_size`]）。
    pub fn vocab_size(&self) -> u64 {
        match self {
            Self::Bigram(lm) => lm.vocab_size(),
            Self::Wanxiang(lm) => lm.vocab_size(),
            Self::Mixed(_) => 0, // 混合模型未统计(用户层规模见 UserLM)
        }
    }
}

impl LanguageModel for LmStore {
    fn floor_log10(&self) -> f32 {
        match self {
            Self::Bigram(lm) => lm.floor_log10(),
            Self::Wanxiang(lm) => lm.floor_log10(),
            Self::Mixed(lm) => lm.floor_log10(),
        }
    }

    type State = LmState;

    fn state_from_context(&self, ctx: &LmContext) -> LmState {
        match self {
            Self::Bigram(lm) => LmState::Bigram(lm.state_from_context(ctx)),
            Self::Wanxiang(lm) => LmState::Wanxiang(lm.state_from_context(ctx)),
            Self::Mixed(lm) => LmState::Mixed(lm.state_from_context(ctx)),
        }
    }

    fn score(&self, state: &LmState, word: &str) -> (f32, LmState) {
        match (self, state) {
            (Self::Bigram(lm), LmState::Bigram(s)) => {
                let (logp, next) = lm.score(s, word);
                (logp, LmState::Bigram(next))
            }
            (Self::Wanxiang(lm), LmState::Wanxiang(s)) => {
                let (logp, next) = lm.score(s, word);
                (logp, LmState::Wanxiang(next))
            }
            (Self::Mixed(lm), LmState::Mixed(s)) => {
                let (logp, next) = lm.score(s, word);
                (logp, LmState::Mixed(next))
            }
            // 状态类型与模型不匹配：只可能是调用方手工构造了错误的状态
            // 枚举变体（会话状态永远来自同一个 LmStore），属编程错误。
            _ => unreachable!("LmState 变体与 LmStore 变体不匹配"),
        }
    }

    /// 转发给具体实现的 `score_only`(同样只做匹配、不建新状态)。
    fn score_only(&self, state: &LmState, word: &str) -> f32 {
        match (self, state) {
            (Self::Bigram(lm), LmState::Bigram(s)) => lm.score_only(s, word),
            (Self::Wanxiang(lm), LmState::Wanxiang(s)) => lm.score_only(s, word),
            (Self::Mixed(lm), LmState::Mixed(s)) => lm.score_only(s, word),
            _ => unreachable!("LmState 变体与 LmStore 变体不匹配"),
        }
    }
}

mod prepared;
#[cfg(test)]
mod prepared_tests;
#[cfg(test)]
mod tests;
