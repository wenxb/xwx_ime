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
}


// ---------------------------------------------------------------------------
// 混合语言模型(第 5 步:通用模型 + 用户学习模型)
// ---------------------------------------------------------------------------

/// 混合模型里用户层的权重(log10 概率域线性插值)。
///
/// 取值 0.7 的理由:
/// - 主流输入法(Rime/搜狗)的语义都是「个人习惯压过通用统计」——
///   用户亲手选过的词,下次几乎一定要排在通用高频词前面;
/// - 但不能是 1.0:新用户模型只有几百条计数,没有任何平滑兜底能力,
///   纯用户模型会把通用语料(万象 624 万键)的覆盖率优势清零;
/// - 0.7 让用户层一次上屏(计数 1 的 unigram ≈ log10(2/(N+V)))就
///   带来约 1.4~2 个数量级的可见提升,3~5 次稳定压过词频 10 倍差;
///   同时给通用层留 0.3 的底,用户从没碰过的搭配仍由通用模型排序。
///
/// 评测验证见 `examples/learn_eval.rs` 报告数字。
pub const MIX_USER_WEIGHT: f32 = 0.7;

/// 两层加权混合语言模型:通用层(万象 / bigram)+ 用户学习层。
///
/// 插值发生在 **log10 概率域**:
/// `log10( (1-w)·P_base(word|ctx) + w·P_user(word|prev) )`,
/// 即在概率域做线性插值再取对数(Kneser 意义的 mixture);
/// 用户层无上文/无命中时退回它自己的 unigram/未知词兜底,因此
/// 「学过」的信号永远 ≥ 兜底,而通用层负责用户没碰过的广阔长尾。
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
    /// 用户层权重(见 [`MIX_USER_WEIGHT`])。
    user_weight: f32,
}

impl std::fmt::Debug for MixedLM {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MixedLM")
            .field("base", &self.base)
            .field("user_weight", &self.user_weight)
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
    pub fn new(base: LmStore, user: UserLM, user_weight: f32) -> Self {
        debug_assert!(
            !matches!(base, LmStore::Mixed(_)),
            "混合模型不应再嵌套混合"
        );
        MixedLM { base: Box::new(base), user, user_weight: user_weight.clamp(0.0, 1.0) }
    }

    /// 用户层句柄(学习/落盘走这里;打分不需要它)。
    pub fn user(&self) -> &UserLM {
        &self.user
    }
}

impl LanguageModel for MixedLM {
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
        let user_logp = self
            .user
            .score_with(&state.snap, state.user_prev.as_deref(), word);
        // 概率域线性插值(分数是 log10,先回概率再加权再取对数)。
        // log10(10^x + 10^y) = max + log10(1 + 10^(min-max)),数值稳定。
        let w = self.user_weight;
        let bx = base_logp + (1.0 - w).log10();
        let ux = user_logp + w.log10();
        let (hi, lo) = if bx >= ux { (bx, ux) } else { (ux, bx) };
        let mixed = hi + (1.0 + 10f32.powf(lo - hi)).log10();
        (
            mixed,
            MixedState {
                base: Box::new(base_next),
                user_prev: Some(word.to_string()),
                snap: state.snap.clone(),
            },
        )
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bigram_prefers_known_pair() {
        let mut lm = BigramLM::new();
        // "中国" 出现 100 次，"中" 出现 100 次，"国" 出现 50 次
        lm.add_unigram("中国", 100);
        lm.add_unigram("中", 100);
        lm.add_unigram("国", 50);
        lm.add_bigram("中", "国", 90);

        // 状态 = 上一个词：Some("中") 表示上文是「中」。
        let prev = Some("中".to_string());
        let with_pair = lm.score(&prev, "国").0;
        let without_pair = lm.score(&prev, "华").0;
        assert!(with_pair > without_pair, "有 bigram 计数时应得分更高");
    }

    #[test]
    fn unknown_word_has_finite_score() {
        let lm = BigramLM::new();
        let (s, _) = lm.score(&lm.initial_state(), "不存在的词");
        assert!(s.is_finite());
    }

    #[test]
    fn state_advances_after_score() {
        // 等价于旧 `score_sequence(&["你", "好"])`：第一个词用初始状态
        // （只有 unigram 分），随后状态推进为前一个词再打分（bigram 命中）。
        let mut lm = BigramLM::new();
        lm.add_unigram("你", 100);
        lm.add_unigram("好", 50);
        lm.add_bigram("你", "好", 80);

        let (s0, next) = lm.score(&lm.initial_state(), "你");
        assert_eq!(next, Some("你".to_string()), "打分后状态应推进为刚消费的词");
        let (s1, next1) = lm.score(&next, "好");
        assert_eq!(next1, Some("好".to_string()));

        // 句首 unigram P(你) = (100+1)/(150+2)；bigram P(好|你) = (80+1)/(100+2)
        let expected0 = ((100.0_f32 + 1.0) / (150.0_f32 + 2.0)).log10();
        let expected1 = ((80.0_f32 + 1.0) / (100.0_f32 + 2.0)).log10();
        assert!((s0 - expected0).abs() < 1e-6, "首个词应是句首 unigram 分");
        assert!((s1 - expected1).abs() < 1e-6, "状态推进后应命中 bigram 分");
    }

    #[test]
    fn lm_store_routes_via_enum_state() {
        let mut store = LmStore::Bigram(BigramLM::new());
        let data = "中国\t100\n中\t100\n国\t50\n中\t国\t90\n";
        store.load(data.as_bytes()).expect("加载失败");

        let state = store.initial_state();
        assert_eq!(state, LmState::Bigram(None), "初始状态应是无上文");

        // 无上文：unigram 分（与改动前 `score_sequence(&[word])` 等价）
        let (solo, _) = store.score(&state, "国");
        // 推进：上文 = 中
        let (_, advanced) = store.score(&state, "中");
        assert_eq!(advanced, LmState::Bigram(Some("中".to_string())));
        // 再打分：命中 bigram(中, 国)
        let (paired, _) = store.score(&advanced, "国");
        assert!(paired > solo, "状态推进后应命中 bigram 计数");
    }

    #[test]
    fn state_from_context_reads_last_word_only() {
        let lm = BigramLM::new();

        // 无上下文：空
        assert_eq!(lm.state_from_context(&LmContext::empty()), None);
        assert_eq!(
            lm.initial_state(),
            None,
            "initial_state 应等价于空上下文（默认实现）"
        );

        // 只有 tail、没有 last_word（如原始字母/标点上屏）：无词级上文
        let tail_only = LmContext { tail: "中", last_word: None };
        assert_eq!(lm.state_from_context(&tail_only), None);

        // 有 last_word：bigram 状态 = 前一个词（tail 里还有别的字也不影响）
        let ctx = LmContext { tail: "中", last_word: Some("中国") };
        assert_eq!(
            lm.state_from_context(&ctx),
            Some("中国".to_string()),
            "bigram 只应读取 last_word"
        );
    }

    #[test]
    fn context_injection_hits_bigram() {
        // 有词对的模型：注入上文后 bigram 分支必须真的命中，
        // 与无上文的打分不同（这是第 2a 步的核心验收点）。
        let mut lm = BigramLM::new();
        lm.add_unigram("中", 100);
        lm.add_unigram("国", 50);
        lm.add_bigram("中", "国", 90);

        let ctx = LmContext { tail: "中", last_word: Some("中") };
        let with_prev = lm.score(&lm.state_from_context(&ctx), "国").0;
        let no_prev = lm.score(&lm.initial_state(), "国").0;
        assert!(
            with_prev > no_prev,
            "有上文(中→国)应命中 bigram 计数而得分更高: {with_prev} vs {no_prev}"
        );

        // 经 LmStore 枚举分发也同样生效
        let store = LmStore::Bigram(lm);
        let s1 = store.score(&store.state_from_context(&ctx), "国").0;
        let s0 = store.score(&store.initial_state(), "国").0;
        assert!(s1 > s0, "LmStore 分发后同样应命中 bigram: {s1} vs {s0}");
    }

    #[test]
    fn mixed_state_advances_both_layers() {
        let mut base = BigramLM::new();
        base.add_unigram("今天", 100);
        base.add_bigram("今天", "天气", 90);
        let store = LmStore::Mixed(MixedLM::new(
            LmStore::Bigram(base),
            UserLM::in_memory(),
            MIX_USER_WEIGHT,
        ));
        let s0 = store.state_from_context(&LmContext {
            tail: "今天",
            last_word: Some("今天"),
        });
        let LmState::Mixed(m) = &s0 else {
            panic!("混合模型应产生 Mixed 状态");
        };
        assert_eq!(m.user_prev.as_deref(), Some("今天"), "用户层应读词级上文");
        let (_, s1) = store.score(&s0, "天气");
        let LmState::Mixed(m1) = s1 else {
            panic!("打分后应保持 Mixed 状态");
        };
        assert_eq!(m1.user_prev.as_deref(), Some("天气"), "用户层上文应推进");
    }

    #[test]
    fn mixed_prefers_user_hits_without_losing_base() {
        // 用户学过「斯巴克」,通用层没学过 → 混合后斯巴克应明显高于
        // 通用层的同上文竞争者;同时通用层自己的统计(今天→天气)
        // 在用户层空白处仍应起作用。
        let mut base = BigramLM::new();
        base.add_unigram("今天", 1000);
        base.add_unigram("天气", 500);
        base.add_unigram("星冰乐", 100);
        base.add_bigram("今天", "天气", 90);
        let user = UserLM::in_memory();
        for _ in 0..3 {
            user.learn(Some("星冰乐"), "斯巴克");
        }
        // 混合层插入一个 0.3/0.7 线性插值,绝对分会被改写;
        // 本测试只断言「信号存在」,因此断言前先确认两层各有多少
        // log10 增益,数值放对注释里,断言仍只比较相对大小。
        let store = LmStore::Mixed(MixedLM::new(LmStore::Bigram(base), user, MIX_USER_WEIGHT));

        // 用户层独有能力:「星冰乐→斯巴克」是通用层没有的
        let ctx = LmContext { tail: "星冰乐", last_word: Some("星冰乐") };
        let user_hit = store.score(&store.state_from_context(&ctx), "斯巴克").0;
        let user_miss = store.score(&store.state_from_context(&ctx), "星冰乐").0;
        assert!(user_hit > user_miss, "用户学过的搭配应更高: {user_hit} vs {user_miss}");
    }

    #[test]
    #[should_panic(expected = "混合模型不应再嵌套混合")]
    fn mixed_rejects_nested_mixed() {
        let inner = LmStore::Mixed(MixedLM::new(
            LmStore::Bigram(BigramLM::new()),
            UserLM::in_memory(),
            MIX_USER_WEIGHT,
        ));
        // debug_assert 在测试构建(debug)下触发 panic;
        // release 下 clamp 生效但语义上仍禁止(见 MixedLM::new 文档)。
        let _ = MixedLM::new(inner, UserLM::in_memory(), MIX_USER_WEIGHT);
    }

    #[test]
    fn mixed_score_stays_finite_on_empty_layers() {
        let store = LmStore::Mixed(MixedLM::new(
            LmStore::Bigram(BigramLM::new()),
            UserLM::in_memory(),
            MIX_USER_WEIGHT,
        ));
        let (s, _) = store.score(&store.initial_state(), "任何词");
        assert!(s.is_finite(), "两层都空时混合分仍应有限");
    }

    /// **句首排序的真正通道是用户层 unigram,不是通用层。**
    ///
    /// 钉住一个实测事实(2026-10-01,`examples/lm_ctx_probe.rs`):在空
    /// 上下文下
    /// - 万象 `.gram`(`WanxiangLM::score_word` 的 `ctx_tail.is_empty()`
    ///   早退)对**任何**词都返回同一个常数 `floor_log10()` ≈ -11.605767
    ///   ——实测「西安/先/闲/狄安娜/中国/的/龘」七词分数逐位相同;
    /// - 用户层(`UserLmSnapshot::log10_prob`)在无 `prev` 时**回落到
    ///   unigram 计数**,因此对学过的词给出**不同**的分。
    ///
    /// 这条测试的意义是防止误诊:看 `wanxiang.rs` 的空上下文早退很容易
    /// 得出「句首排序完全由词频决定」的结论,但那只在生产进程同时挂了
    /// 用户层(`main.rs` 调 `with_user_lm`)时才对**通用层**成立——
    /// **用户用过的词在句首是能被区分的**。
    ///
    /// 同时它也是「空上下文 unigram backoff 不可行」这条结论的边界说明:
    /// 那条修法针对的是**通用 gram**层(键长不可比),而**用户层**的
    /// unigram 计数本来就是可比的自然数,不需要也不应该一起否决。
    #[test]
    fn empty_context_user_layer_discriminates_but_base_does_not() {
        let base = BigramLM::new(); // 空通用层:等价于「通用层给不出信号」
        let user = UserLM::in_memory();
        for _ in 0..5 {
            user.learn(None, "西安");
        }
        user.learn(None, "的");
        let store = LmStore::Mixed(MixedLM::new(
            LmStore::Bigram(base),
            user,
            MIX_USER_WEIGHT,
        ));

        // 空上下文(句首):last_word = None。
        let st = store.state_from_context(&LmContext::empty());
        let (learned, _) = store.score(&st, "西安");
        let st2 = store.state_from_context(&LmContext::empty());
        let (rare, _) = store.score(&st2, "龘");

        assert!(
            learned.is_finite() && rare.is_finite(),
            "两词分都应有限: {learned} / {rare}"
        );
        assert!(
            learned > rare,
            "用户学过的「西安」在句首应高于没学过的「龘」——\
             这正是句首可被用户层区分的证据: {learned} vs {rare}"
        );
    }

    /// 空上下文下 `LmContext::empty()` 的两个字段都必须是「无上文」。
    ///
    /// 这是上面那条测试的前提:`tail = ""` 触发万象早退,
    /// `last_word = None` 触发用户层 unigram 回落。任一字段被填上,
    /// 口径就变了(不再是「句首」)。钉住它以免后人改 `empty()`。
    #[test]
    fn empty_context_has_both_fields_absent() {
        let c = LmContext::empty();
        assert!(c.tail.is_empty(), "tail 必须为空串(否则万象不走早退)");
        assert!(c.last_word.is_none(), "last_word 必须为 None(否则用户层走 bigram)");
    }
}
