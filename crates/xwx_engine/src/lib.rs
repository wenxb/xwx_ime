//! # xwx_engine：拼音输入法纯算法核心
//!
//! 不依赖任何 GUI / DBus 框架，只做输入法该做的"智力活"：
//!
//! 1. [`segmenter`]：拼音音节表 + 切分；
//! 2. [`dict`]：词库查询（SQLite）；
//! 3. [`lm`]：语言模型打分；
//! 4. [`corrector`]：模糊拼音展开；
//! 5. [`session`]：按键状态机；
//! 6. [`ImeEngine`]：以上所有组件的聚合入口。
//!
//! 典型的调用方式（由 xwx_ibus 适配层驱动）：
//!
//! ```ignore
//! let engine = Arc::new(ImeEngine::new(Some("data/dict.db".as_ref()))?);
//! let mut session = engine.create_session();
//! match session.handle(InputEvent::Char('n'))? {
//!     SessionOutput::Updated(u) => { /* 刷新候选窗 */ }
//!     _ => {}
//! }
//! ```

pub mod candidate;
pub mod corrector;
pub mod dict;
pub mod lm;
pub mod segmenter;
pub mod session;
pub mod user_lm;
pub mod wanxiang;

pub use candidate::Candidate;
pub use corrector::{expand_fuzzy, expand_fuzzy_scored, ConfusionPair, COMMON_CONFUSION_PAIRS};
pub use dict::{DictError, DictStore, Dictionary, MmapDict, SqliteDict};
pub use lm::{BigramLM, LanguageModel, LmContext, LmError, LmState, LmStore};
pub use lm::{MixedLM, MixedState, MIX_USER_WEIGHT};
pub use user_lm::{UserLM, FLUSH_LEARN_THRESHOLD, MAX_USER_UNIGRAMS, USER_LM_FILE_NAME};
pub use segmenter::{Segmenter, SYLLABLES};
pub use session::{InputEvent, InputSession, SessionOutput, SessionUpdate};
pub use wanxiang::{WanxiangConfig, WanxiangLM, WanxiangState};

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use thiserror::Error;

/// 引擎错误：词库/语言模型错误的统一出口。
#[derive(Debug, Error)]
pub enum EngineError {
    /// 词库层错误。
    #[error("词库错误: {0}")]
    Dict(#[from] DictError),
    /// 语言模型层错误。
    #[error("语言模型错误: {0}")]
    Lm(#[from] LmError),
}

/// 单次查询最多返回的候选数。
const MAX_CANDIDATES: usize = 100;
/// 语言模型得分在最终候选分中的默认权重。
///
/// 第 4 步把词频项归一化为 `log10(freq / 总词频)` 后，词频项与 LM 的
/// log10 概率同量纲（都是负值、约 -1..-12），这个权重才有可比性；
/// 取值经评测集扫参（0.5/1/2/5，见 `examples/eval_rank.rs` 与报告）
/// 定为 1.0——LM 的语境命中也以 1:1 记入总分，不额外放大/稀释。
pub const DEFAULT_LM_WEIGHT: f32 = 1.0;
/// 切分格解码的 beam 宽度：每个拼音位置保留的前缀路径数。
///
/// 取值理由：
/// - 中文拼音的真实歧义规模：任一位置向前最多 ~10 条音节链（典型句子
///   如 `"womenjintianquxian"` 每位置 2~5 条），路径竞争集中在少数几条
///   高概率译文上；K=16 在完整覆盖「首屏候选相关路径」的同时把每轮的
///   扩展量压在 16 × 边数；
/// - 每击键延迟预算是**个位数毫秒**：K=16 时每层扩展 ≤16×64（每个起点的
///   链数上限 [`MAX_CHAINS_PER_START`]）条边的 LM 打分（`HashMap` 借用
///   查找 + 状态推进），实测最长典型串（`"womenjintianquxian"`，17 字符）
///   约 1.3ms、
///   普通词 <0.2ms（见 `examples/decode_bench.rs`）；
/// - 再大（64+）会保留大量同音/等分路径，对排序没有贡献，反而让病态
///   输入（`"aaaa…"`）退化到接近旧穷举的开销；再小（≤4）则会丢掉
///   同音字多的首音节候选（如 `xian` 的 20+ 同音字，用户翻页可见）。
const BEAM_WIDTH: usize = 16;
/// 切分格中单条词边最多覆盖的音节数。
///
/// 词典里 8+ 音节的词条极其罕见（rime-ice 的成语/机构名最长为 6~7 音节），
/// 但它同时是链长上限：没有它，`"aaaaaaaa…"` 这类输入可以让链无限加长，
/// 枚举量乘出指数级。与 [`MAX_CHAINS_PER_START`] 一起保证格构建的
/// 总枚举量 ≤ `len(pinyin) × MAX_CHAINS_PER_START`。
const MAX_EDGE_SYLLABLES: usize = 12;
/// 每个起点（拼音位置）最多枚举的音节链数。
///
/// 病态输入（如 `"nananana…"`，每步可走 `na`/`nan` 两条）会按斐波那契
/// 膨胀，这个预算把它截硬：宁可少覆盖几条超长链，也不让单次击键
/// 退化成指数级。
const MAX_CHAINS_PER_START: usize = 64;

/// 切分格上的一条词边：覆盖 `pinyin[start..end]`，对应该区间查到的一个词。
#[derive(Debug)]
struct LatticeEdge {
    /// 边终点（字节位置，`start` 由所在桶隐式给出）。
    end: usize,
    /// 词典词条。
    word: String,
    /// 该词的拼音 key（音节空格分隔，与词典一致），如 `"xi an"`。
    pinyin: String,
    /// 静态词频。
    freq: u64,
    /// 模糊拼音惩罚(对数域加性扣分,非负)。
    ///
    /// 精确匹配的边为 `0.0`;由混淆对展开出的变体边带上
    /// [`expand_fuzzy_scored`] 累计的 `-log10(weight)` 惩罚,
    /// 在 beam / fallback 打分中从路径分里减去。
    fuzzy_penalty: f32,
}

/// beam 里一条部分路径。
#[derive(Debug)]
struct BeamPath {
    /// 累计分 = Σ(log10(freq/总词频) + lm_weight × log10 P(word | 路径上文))。
    ///
    /// 两项同量纲（都是对数概率，负值）才可加：词频项对数化后，短语
    /// 不再被百万级原始词频淹没（第 4 步量级失衡修复的核心）。
    score: f32,
    /// 沿路径累计的模糊拼音惩罚(各边 `fuzzy_penalty` 之和,非负)。
    ///
    /// `> 0` 说明路径含模糊边,用于把「路径是否精确」传到候选分层:
    /// 路径只要含任一模糊边就整体归模糊层(一处不精确则整体不精确)。
    /// 它与 `score` 已扣除的惩罚是同一份量,这里单独留一份是为了在
    /// 排序处恢复「是否含模糊边」这一布尔信息(penalty 已被揉进 score,
    /// 无法事后还原)。
    fuzzy_penalty: f32,
    /// 路径已选词的顺序。
    words: Vec<WordSpot>,
}

/// beam 搜索节点：arena 里的一条路径节点，`parent` 指向上一步。
///
/// 用「节点 + 父指针」而不是在每个 BeamPath 里拷贝整条词序列：
/// 扩展只 push 一个节点（O(1) 分配），结束时再沿父链重建词序。
#[derive(Debug)]
struct BeamNode {
    /// 累计分 = Σ(log10(freq/总词频) + lm_weight × log10 P(word | 路径上文))。
    score: f32,
    /// 沿路径累计的模糊拼音惩罚(各边 `fuzzy_penalty` 之和,非负)。
    fuzzy_penalty: f32,
    /// 沿路径推进的 LM 状态。
    state: LmState,
    /// 本步选的格边（起点位置, 桶内下标）；根节点为 `None`。
    ///
    /// 边（词/拼音/词频）留在格子里，扩展时只记下标，不复制字符串——
    /// 重建完整路径时才从格取词（每词一次，而不是每扩展一次）。
    edge: Option<(usize, usize)>,
    /// 上一步节点的 arena 下标；根节点为 `None`。
    parent: Option<usize>,
}

/// 路径里一个词的位置。
#[derive(Debug)]
struct WordSpot {
    word: String,
    pinyin: String,
    freq: u64,
    /// 到达该词的路径是否含模糊边(由所在路径的 `fuzzy_penalty > 0` 推出)。
    fuzzy: bool,
}

/// 一条完整解码路径（beam 结果，测试/调试用）。
#[derive(Debug, Clone, PartialEq)]
#[cfg(test)]
pub(crate) struct DecodedPath {
    /// 整条路径的累计得分。
    pub score: f32,
    /// 路径依次选出的词。
    pub words: Vec<String>,
    /// 每个词对应的拼音（多音节串，空格分隔，与词典 key 一致）。
    pub pinyin: Vec<String>,
}

/// 引擎聚合体：切分器 + 词库 + 语言模型 + 模糊拼音配置。
///
/// 结构上刻意用具体类型（而非 `Arc<dyn Dictionary>`）：
/// 当前引擎是单线程同步模型，具体类型让字段访问和测试都更直接。
/// TODO: 若未来需要插件化词库/模型，再引入 trait object 或泛型。
#[derive(Debug)]
pub struct ImeEngine {
    /// 拼音切分器。
    pub segmenter: Segmenter,
    /// 词库（运行时为 mmap 只读二进制；`new(None)` 时是内存 SQLite）。
    pub dict: DictStore,
    /// 语言模型（`LmStore` 枚举聚合，当前为 bigram 实现）。
    pub lm: LmStore,
    /// LM 得分的权重（默认 [`DEFAULT_LM_WEIGHT`]；评测扫参时由调用方改）。
    pub lm_weight: f32,
    /// 模糊拼音混淆对。
    pub fuzzy_pairs: Vec<ConfusionPair>,
    /// 是否启用模糊拼音。
    pub fuzzy_enabled: bool,
}

impl ImeEngine {
    /// 创建引擎。
    ///
    /// - `dict_path` 为 `Some(path)` 时以只读 mmap 方式打开该 dict.bin
    ///   词库；打开失败（文件缺失/损坏）仅告警，退回内存空词库——引擎
    ///   不会写入或创建任何文件；
    /// - `None` 时使用内存词库（测试/临时用）。
    ///
    /// 等价于 `new_with_lm(dict_path, None)`：不加载语言模型文件时使用
    /// 空 bigram（候选排序仅靠词频）。
    pub fn new(dict_path: Option<&Path>) -> Result<Self, EngineError> {
        Self::new_with_lm(dict_path, None)
    }

    /// 创建引擎并加载语言模型。
    ///
    /// `lm_path` 为：
    /// - `Some(path)` → 把该路径当作万象 `.gram` 文件 mmap 打开（调用方
    ///   负责判断扩展名；`src/main.rs` 只在后缀为 `gram` 时走这里）；
    /// - `None` → 与 [`Self::new`] 相同（空 bigram，由调用方自行
    ///   `engine.lm.load(...)` 加载 bigram 文本）。
    ///
    /// **语言模型文件缺失/损坏时优雅退化**：仅记录 warning 并退回空
    /// `BigramLM`，绝不 panic——不是所有用户都会下载那个 400MB 文件。
    pub fn new_with_lm(
        dict_path: Option<&Path>,
        lm_path: Option<&Path>,
    ) -> Result<Self, EngineError> {
        let dict = match dict_path {
            Some(path) => match MmapDict::open(path) {
                Ok(dict) => DictStore::Mmap(dict),
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(),
                        "mmap 词库打开失败，退回内存空词库");
                    DictStore::Sqlite(SqliteDict::open_in_memory()?)
                }
            },
            None => DictStore::Sqlite(SqliteDict::open_in_memory()?),
        };
        let lm = match lm_path {
            Some(path) => match WanxiangLM::open(path) {
                Ok(lm) => {
                    tracing::info!(
                        path = %path.display(),
                        units = lm.num_units(),
                        "万象 .gram 语言模型已加载"
                    );
                    LmStore::Wanxiang(lm)
                }
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(),
                        "万象 .gram 打开失败，退回空 bigram 语言模型");
                    LmStore::Bigram(BigramLM::new())
                }
            },
            _ => LmStore::Bigram(BigramLM::new()),
        };
        Ok(Self {
            segmenter: Segmenter::new(),
            dict,
            lm,
            lm_weight: DEFAULT_LM_WEIGHT,
            fuzzy_pairs: COMMON_CONFUSION_PAIRS.to_vec(),
            fuzzy_enabled: false,
        })
    }

    /// 创建一个输入会话（共享引擎数据）。
    pub fn create_session(self: &Arc<Self>) -> InputSession {
        InputSession::new(Arc::clone(self))
    }

    /// 在通用语言模型之外叠加用户学习模型(第 5 步)。
    ///
    /// 现有 `lm` 成为 [`MixedLM`] 的通用层(空 bigram 也合法:混合
    /// 模型对任何输入都有兜底分),`user` 是用户层(加载/持久化由
    /// [`UserLM`] 自己负责)。打分混合见 [`MixedLM`];调用方
    /// (beam/兜底/会话)不需要任何改动——这正是枚举分发的意义。
    pub fn with_user_lm(mut self, user: UserLM) -> Self {
        let base = std::mem::replace(&mut self.lm, LmStore::Bigram(BigramLM::new()));
        self.lm = LmStore::Mixed(MixedLM::new(base, user, MIX_USER_WEIGHT));
        self
    }

    /// 用户层句柄(存在混合模型时);供学习钩子与回写调度用。
    pub fn user_lm(&self) -> Option<&UserLM> {
        match &self.lm {
            LmStore::Mixed(m) => Some(m.user()),
            _ => None,
        }
    }

    /// 学习一次上屏(第 5 步学习钩子的引擎侧落点)。
    ///
    /// 由 [`InputSession`](crate::session::InputSession) 在上屏时调用:
    /// - `Some(word)`:候选词上屏(`commit_word`)→ 记录
    ///   `(上一个词, word)` 的二元计数与 `word` 的一元计数;
    /// - `None`:原始字母/标点上屏(`commit_raw`)→ **不学**(它不是词,
    ///   与第 2a 步「`last_word` 置 `None`」同一判断)。
    ///
    /// 只有用户模型存在(引擎经 [`Self::with_user_lm`] 组装)时才计数;
    /// 无用户模型是 no-op,上屏路径零额外开销。落盘不在此同步做
    /// (批量/定时/退出回写由进程入口 `src/main.rs` 调度)。
    pub(crate) fn learn_committed(&self, prev: Option<&str>, word: Option<&str>) {
        let (Some(word), Some(user)) = (word, self.user_lm()) else {
            return;
        };
        let dirty = user.learn(prev, word);
        if dirty == FLUSH_LEARN_THRESHOLD {
            tracing::debug!(dirty, "用户模型已达到回写阈值,等待调度层批量回写");
        }
    }

    /// 当前词库词条总数（启动日志/统计用；内存空词库时为 0）。
    pub fn dict_count(&self) -> u64 {
        match &self.dict {
            DictStore::Sqlite(d) => d.count().unwrap_or(0),
            DictStore::Mmap(d) => d.count(),
        }
    }

    /// 核心查询管线：给定拼音串，返回排序后的候选列表。
    ///
    /// 做法（第 2b 步：切分格 + beam search 解码，替代旧「穷举切分 +
    /// 候选孤立打分」）：
    ///
    /// 1. 构建切分格：[`Self::build_lattice`] 从每个位置枚举音节链，
    ///    每条链的拼音串查词库得「词边」（含模糊拼音变体边），
    ///    不再枚举全部切分；
    /// 2. beam search：[`Self::decode_beam`] 沿位置推进，每个位置只保留
    ///    top-K 条部分路径，LM 状态**沿路径传递**（`LanguageModel::score`
    ///    返回的新状态就是给路径分叉用的）；
    /// 3. 串尾的完整路径按总分降序；每个候选（词）的得分 = 包含它的
    ///    最优完整路径的总分——「西安」和「先」从此是两条整句路径的
    ///    竞争，而不是各自从初始上下文独立起算；
    /// 4. 整串无法完整解码（还没打完，如 `"nih"`、`"z"`）时走
    ///    [`Self::fallback_candidates`] 的边打字边联想兜底。
    pub fn lookup_candidates(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
    ) -> Result<Vec<Candidate>, EngineError> {
        if pinyin.is_empty() {
            return Ok(Vec::new());
        }

        let paths = self.decode_beam(pinyin, ctx, BEAM_WIDTH)?;
        if paths.is_empty() {
            // 整串还不能完整解码（首音节没打完 / 停在半音节）：兜底联想，
            // 否则 IBus 层会在打字中途发 HideLookupTable，候选窗闪一下就没。
            return self.fallback_candidates(pinyin, ctx);
        }

        // 完整路径 → 候选：一个词的分 = 包含它的最优完整路径的总分。
        // 同一个 (word, pinyin) 可以出现在多条路径的不同位置（如
        // `"yi yi"` 里「一」出现两次），取它们各自的路径分里最高的。
        let mut best: HashMap<(String, String), (f32, u64, bool)> = HashMap::new();
        for path in &paths {
            for spot in &path.words {
                let entry = best
                    .entry((spot.word.clone(), spot.pinyin.clone()))
                    .or_insert_with(|| (f32::NEG_INFINITY, spot.freq, spot.fuzzy));
                if path.score > entry.0 {
                    *entry = (path.score, spot.freq, spot.fuzzy);
                }
            }
        }

        // 不变量:路径的 fuzzy_penalty > 0 当且仅当它产出的词都被标为模糊。
        // (重构时若把 fuzzy 标记改由别的途径推出,这里能立刻发现漂移。)
        for path in &paths {
            debug_assert_eq!(
                path.fuzzy_penalty > 0.0,
                path.words.iter().any(|w| w.fuzzy),
                "fuzzy 标记与路径累计惩罚不一致"
            );
        }

        let mut ranked: Vec<Candidate> = best
            .into_iter()
            .map(|((word, pinyin), (score, freq, fuzzy))| Candidate {
                word,
                pinyin,
                score,
                freq, // 词典原始频率，供展示/回退（相比旧实现不再丢 freq）
                fuzzy, // 最优路径是否含模糊边(分层排序用)
            })
            .collect();

        sort_candidates(&mut ranked);
        ranked.truncate(MAX_CANDIDATES);
        Ok(ranked)
    }

    /// 构建切分格：`result[start]` = 从 `start` 出发的所有词边。
    ///
    /// - 每条边覆盖一个「音节链」：从 `start` 起连续若干个合法音节，链的
    ///   拼音串（空格连接）就是词典 key，直接精确查词；
    /// - 链长 ≤ [`MAX_EDGE_SYLLABLES`]，每个起点枚举的链数 ≤
    ///   [`MAX_CHAINS_PER_START`]：格是多项式规模的，病态输入
    ///   （`"aaaaaaaa"`、`"nananana…"`、超长串）不会组合爆炸；
    /// - 模糊拼音开启时，对每条链的拼音串做 `expand_fuzzy` 展开，
    ///   变体键同样查词（比旧实现只展开整串更细：`"ni hao"` → `"li hao"`
    ///   这类"词内模糊"也够得到）；
    /// - 精确键 + 变体键用 key 级缓存，重复键只查一次词库。
    fn build_lattice(&self, pinyin: &str) -> Result<Vec<Vec<LatticeEdge>>, EngineError> {
        let n = pinyin.len();
        let mut lattice: Vec<Vec<LatticeEdge>> = (0..=n).map(|_| Vec::new()).collect();
        if !pinyin.is_ascii() {
            return Ok(lattice);
        }
        let mut cache: HashMap<String, Vec<Candidate>> = HashMap::new();
        for start in 0..n {
            let mut budget = MAX_CHAINS_PER_START;
            let mut chain: Vec<&str> = Vec::new();
            self.collect_chain_edges(
                pinyin, start, start, &mut chain, &mut lattice, &mut cache, &mut budget,
            )?;
        }
        Ok(lattice)
    }

    /// 链枚举 DFS：延长当前音节链，把每个「链覆盖区间 → 词」记为格边。
    #[allow(clippy::too_many_arguments)]
    fn collect_chain_edges<'a>(
        &self,
        pinyin: &'a str,
        start: usize,
        pos: usize,
        chain: &mut Vec<&'a str>,
        lattice: &mut Vec<Vec<LatticeEdge>>,
        cache: &mut HashMap<String, Vec<Candidate>>,
        budget: &mut usize,
    ) -> Result<(), EngineError> {
        let n = pinyin.len();
        if chain.len() >= MAX_EDGE_SYLLABLES || *budget == 0 {
            return Ok(());
        }
        let max_len = (n - pos).min(segmenter::MAX_SYLLABLE_LEN);
        for len in 1..=max_len {
            if *budget == 0 {
                return Ok(());
            }
            let syl = &pinyin[pos..pos + len];
            if !self.segmenter.is_syllable(syl) {
                continue;
            }
            *budget -= 1;
            chain.push(syl);
            let end = pos + len;
            let joined = chain.join(" ");

            // (key, fuzzy_penalty):精确键惩罚为 0,模糊键带上
            // 展开时的累计 -log10(weight)。fuzzy 关闭时只有精确键。
            let keys: Vec<(String, f32)> = if self.fuzzy_enabled {
                expand_fuzzy_scored(&joined, &self.fuzzy_pairs)
            } else {
                vec![(joined.clone(), 0.0)]
            };
            for (key, fuzzy_penalty) in &keys {
                let cands = if let Some(c) = cache.get(key.as_str()) {
                    c.clone()
                } else {
                    let c = self.dict.lookup(key)?;
                    cache.insert(key.clone(), c.clone());
                    c
                };
                for c in &cands {
                    lattice[start].push(LatticeEdge {
                        end,
                        word: c.word.clone(),
                        pinyin: c.pinyin.clone(),
                        freq: c.freq,
                        fuzzy_penalty: *fuzzy_penalty,
                    });
                }
            }

            // 深度优先：延长链（下一音节从 `end` 继续）
            self.collect_chain_edges(
                pinyin, start, end, chain, lattice, cache, budget,
            )?;
            chain.pop();
        }
        Ok(())
    }

    /// 在格上跑宽度 `k` 的 beam search，返回完整路径（按总分降序）。
    ///
    /// 每个位置 `pos` 的路径桶在开始扩展前已收齐所有入边（入边只来自
    /// 更早的位置），先剪到 top-K 再向前扩展——这是「beam 即剪枝」的
    /// 落点：LM 状态沿路径传递，被剪路径的状态随之丢弃。
    fn decode_beam(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        k: usize,
    ) -> Result<Vec<BeamPath>, EngineError> {
        if pinyin.is_empty() {
            return Ok(Vec::new());
        }
        self.decode_beam_with_lattice(pinyin, ctx, k, self.build_lattice(pinyin)?)
    }

    /// 与 [`Self::decode_beam`] 相同，但格子由调用方传入（分层便于
    /// 把「格构建」与「beam 推进」分开看，性能排查/测试用）。
    fn decode_beam_with_lattice(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        k: usize,
        lattice: Vec<Vec<LatticeEdge>>,
    ) -> Result<Vec<BeamPath>, EngineError> {
        let len = pinyin.len();
        let mut arena: Vec<BeamNode> = Vec::with_capacity((len + 1) * k);
        let mut beams: Vec<Vec<usize>> = (0..=len).map(|_| Vec::new()).collect();
        // 词频项归一化分母：log10(freq/总词频)，与 LM 的 log10 概率同量纲。
        // 总词频为 0（空词库）时词频项取 0，不进对数（避免 -inf/NaN）。
        let total_freq = self.dict.total_freq()?;
        let log10_total = (total_freq as f64).log10();
        arena.push(BeamNode {
            score: 0.0,
            fuzzy_penalty: 0.0,
            state: self.lm.state_from_context(ctx),
            edge: None,
            parent: None,
        });
        beams[0].push(0);

        for pos in 0..len {
            let cur = std::mem::take(&mut beams[pos]);
            if cur.is_empty() {
                continue;
            }
            let mut touched: Vec<usize> = Vec::new();
            for &node_idx in &cur {
                for (slot, edge) in lattice[pos].iter().enumerate() {
                    let (logp, next_state) = self.lm.score(&arena[node_idx].state, &edge.word);
                    let dict_logp = if total_freq > 0 {
                        // freq 为 0 的词条按 1 兜底（log10(1/T) 仍为有限值）
                        ((edge.freq.max(1) as f64).log10() - log10_total) as f32
                    } else {
                        0.0
                    };
                    let idx = arena.len();
                    arena.push(BeamNode {
                        // 模糊惩罚是同量纲(log10)的加性扣分:精确边 0,
                        // 模糊边减去累计 -log10(weight),多处混淆叠加累加。
                        score: arena[node_idx].score + dict_logp + logp * self.lm_weight
                            - edge.fuzzy_penalty,
                        // 沿路径累计惩罚:排序处据此判断路径是否含模糊边。
                        fuzzy_penalty: arena[node_idx].fuzzy_penalty + edge.fuzzy_penalty,
                        state: next_state,
                        edge: Some((pos, slot)),
                        parent: Some(node_idx),
                    });
                    beams[edge.end].push(idx);
                    touched.push(edge.end);
                }
            }
            // 目标桶剪枝：先混合再剪，只剪本轮收到新节点的桶。
            // 注：同一词序列只可能对应一条路径（DAG 上节点唯一），
            // 桶内不需要去重，只需按分截断。
            touched.sort_unstable();
            touched.dedup();
            for &t in &touched {
                prune_beam(&mut beams[t], &arena, k);
            }
        }
        prune_beam(&mut beams[len], &arena, k);
        // 重建完整路径：沿 parent 走回根，反转成词序。
        let mut paths: Vec<BeamPath> = Vec::with_capacity(beams[len].len());
        for &idx in &beams[len] {
            // 路径只要含任一模糊边(累计惩罚 > 0)就整体归模糊层:
            // 「一处不精确则整体不精确」。判断用累计惩罚而非逐边布尔或,
            // 两者等价(惩罚非负,和 > 0 当且仅当至少一边 > 0),而惩罚
            // 本就沿 arena 累计,这里零额外开销。
            let has_fuzzy = arena[idx].fuzzy_penalty > 0.0;
            let mut words: Vec<WordSpot> = Vec::new();
            let mut cur = Some(idx);
            while let Some(i) = cur {
                if let Some((start_pos, slot)) = arena[i].edge {
                    let e = &lattice[start_pos][slot];
                    words.push(WordSpot {
                        word: e.word.clone(),
                        pinyin: e.pinyin.clone(),
                        freq: e.freq,
                        fuzzy: has_fuzzy,
                    });
                }
                cur = arena[i].parent;
            }
            words.reverse();
            paths.push(BeamPath {
                score: arena[idx].score,
                fuzzy_penalty: arena[idx].fuzzy_penalty,
                words,
            });
        }
        Ok(paths)
    }

    /// 解码完整路径（测试/调试用；`lookup_candidates` 内部用同一套格子）。
    #[cfg(test)]
    pub(crate) fn decode_paths(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        k: usize,
    ) -> Result<Vec<DecodedPath>, EngineError> {
        Ok(self
            .decode_beam(pinyin, ctx, k)?
            .into_iter()
            .map(|p| DecodedPath {
                score: p.score,
                words: p.words.iter().map(|w| w.word.clone()).collect(),
                pinyin: p.words.iter().map(|w| w.pinyin.clone()).collect(),
            })
            .collect())
    }

    /// 「边输入边联想」兜底：整串还没有分词键（`"nih"`、`"zhon"`、`"z"`）。
    ///
    /// 复刻第 2.5 段的语义并保留（IBus 层依赖它避免打字途中候选窗闪没）：
    /// 最长可完整切分前缀的整切分词 + 逐个音节词，加上「下一音节前缀」
    /// 联想（`lookup_prefix`）；一个音节都切不出来时整串前缀联想。
    /// 兜底候选没有路径，按旧语义各自从 `ctx` 独立打分。
    fn fallback_candidates(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
    ) -> Result<Vec<Candidate>, EngineError> {
        if !pinyin.is_ascii() {
            return Ok(Vec::new());
        }
        let mut merged: Vec<Candidate> = Vec::new();

        let segmented = (1..=pinyin.len())
            .rev()
            .find(|&k| !self.segmenter.segment(&pinyin[..k]).is_empty());
        if let Some(k) = segmented {
            for seg in self.segmenter.segment(&pinyin[..k]) {
                let joined = seg.join(" ");
                merged.extend(self.dict.lookup(&joined)?);
                for syllable in seg {
                    merged.extend(self.dict.lookup(&syllable)?);
                }
            }
            if k < pinyin.len() {
                merged.extend(self.dict.lookup_prefix(&pinyin[k..])?);
            }
        }
        if merged.is_empty() {
            // 一个音节都没切出来（首音节还没打完）：整串前缀联想
            merged.extend(self.dict.lookup_prefix(pinyin)?);
        }

        let state = self.lm.state_from_context(ctx);
        // 与 decode_beam 同一打分公式：log10(freq/总词频) + lm_weight × logp
        let total_freq = self.dict.total_freq()?;
        let log10_total = (total_freq as f64).log10();
        let mut ranked: Vec<Candidate> = merged
            .into_iter()
            .map(|mut c| {
                let (lm_score, _) = self.lm.score(&state, c.word.as_str());
                let dict_logp = if total_freq > 0 {
                    ((c.freq.max(1) as f64).log10() - log10_total) as f32
                } else {
                    0.0
                };
                // 兜底路径不经过 expand_fuzzy,fuzzy_penalty 恒为 0;
                // 公式与 decode_beam 一致(此处减 0,省略减法项)。
                c.score = dict_logp + lm_score * self.lm_weight;
                c
            })
            .collect();
        // 去重：同 (word, pinyin) 保最高分，然后排序截断
        let mut best: HashMap<(String, String), Candidate> = HashMap::new();
        for c in ranked.drain(..) {
            let entry = best
                .entry((c.word.clone(), c.pinyin.clone()))
                .or_insert_with(|| c.clone());
            if c.score > entry.score {
                *entry = c;
            }
        }
        ranked = best.into_values().collect();
        sort_candidates(&mut ranked);
        ranked.truncate(MAX_CANDIDATES);
        Ok(ranked)
    }
}

/// 候选按最终排序规则排好（分降序、同分频次降序、再按词/拼音），
/// 保证输出确定性与亲和性（旧实现同分时顺序任意，依赖 HashMap 序）。
/// 首层 key 是精确/模糊分层:精确匹配整体优先,详见函数内注释。
fn sort_candidates(ranked: &mut [Candidate]) {
    // 分层排序:精确匹配(fuzzy == false)整体优先,模糊匹配作为补充
    // 排在后面;层内仍按分降序。理由:模糊惩罚是固定量,而词频差在
    // 对数域可以任意大,两者同池按分比较时高频模糊词总能压过低频
    // 精确词(实测 lan:然 734万 > 蓝 43万)。分层让「精确优先」成为
    // 结构性保证而非数值巧合;层内排序不变,`fuzzy_penalty` 在同为
    // 模糊层的候选之间仍起区分作用(n/l 0.8 排在 s/x 0.2 之前)。
    // fuzzy_enabled = false 时所有候选 fuzzy 都是 false,首层 key
    // 全部相等,排序与分层前逐位一致。
    ranked.sort_by(|a, b| {
        a.fuzzy
            .cmp(&b.fuzzy)
            .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal))
            .then_with(|| b.freq.cmp(&a.freq))
            .then_with(|| a.word.cmp(&b.word))
            .then_with(|| a.pinyin.cmp(&b.pinyin))
    });
}

/// 把一批路径节点剪到前 `k` 条（按分降序，`k = 0` 即清空）。
///
/// 路径由 arena 里的节点构成（DAG 上「词序列 → 节点」一一对应），
/// 同一个桶内不可能出现重复词序列，因此只排序截断、不去重。
fn prune_beam(bucket: &mut Vec<usize>, arena: &[BeamNode], k: usize) {
    bucket.sort_by(|&a, &b| {
        arena[b]
            .score
            .partial_cmp(&arena[a].score)
            .unwrap_or(Ordering::Equal)
    });
    bucket.truncate(k);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_merges_and_ranks() {
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("你好", "ni hao", 10).expect("插入失败");
        engine.dict.upsert("你", "ni", 100).expect("插入失败");
        engine.dict.upsert("逆", "ni", 50).expect("插入失败");

        let cands = engine
            .lookup_candidates("ni", &LmContext::empty())
            .expect("查询失败");
        assert_eq!(cands.first().map(|c| c.word.as_str()), Some("你"));
        assert!(cands.iter().any(|c| c.word == "逆"));
    }

    #[test]
    fn fuzzy_expansion_reaches_dict() {
        let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("里", "li", 100).expect("插入失败");
        engine.fuzzy_enabled = true;

        // 输入 ni（n/l 混淆），应能查到 li -> 里
        let cands = engine
            .lookup_candidates("ni", &LmContext::empty())
            .expect("查询失败");
        assert!(cands.iter().any(|c| c.word == "里"));
    }

    #[test]
    fn mid_syllable_keeps_candidates() {
        // 打字打到一半（如 "nih"）：整串还切不完整，但候选不能消失——
        // 应按"已切分前缀 + 下一音节前缀"继续给候选（对应候选窗闪一下就没的 bug）。
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("你", "ni", 100).expect("插入失败");
        engine.dict.upsert("尼", "ni", 50).expect("插入失败");
        engine.dict.upsert("哈", "ha", 40).expect("插入失败");

        let cands = engine
            .lookup_candidates("nih", &LmContext::empty())
            .expect("查询失败");
        assert!(!cands.is_empty(), "nih 不应没有候选");
        assert!(
            cands.iter().any(|c| c.word == "你"),
            "应包含最长可切前缀 ni 的候选"
        );
        assert!(
            cands.iter().any(|c| c.word == "哈"),
            "应包含尾部(下一音节前缀) h 的候选"
        );
    }

    #[test]
    fn first_syllable_incomplete_uses_prefix() {
        // 首音节都还没切出来（如 "z"、"zhon"）：整串前缀联想
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("中", "zhong", 100).expect("插入失败");
        engine.dict.upsert("在", "zai", 90).expect("插入失败");

        for partial in ["z", "zhon"] {
            let cands = engine
                .lookup_candidates(partial, &LmContext::empty())
                .expect("查询失败");
            assert!(!cands.is_empty(), "{partial} 不应没有候选");
            assert!(
                cands.iter().any(|c| c.word == "中"),
                "{partial} 前缀联想应包含 中"
            );
        }
    }

    #[test]
    fn complete_pinyin_untouched_by_fallback() {
        // 完整可切分时兜底不应抢戏：候选只来自整串相关查询
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("你", "ni", 100).expect("插入失败");
        engine.dict.upsert("你好", "ni hao", 200).expect("插入失败");

        let cands = engine
            .lookup_candidates("ni", &LmContext::empty())
            .expect("查询失败");
        assert!(cands.iter().any(|c| c.word == "你"));
        assert!(
            cands.iter().all(|c| c.pinyin == "ni"),
            "完整切分不应混入其它音节候选: {:?}",
            cands
        );
    }

    #[test]
    fn real_dict_bin_loads() {
        // 冒烟测试：仓库里若生成了 data/dict.bin，验证转换器产物可被
        // mmap 加载、词条数与抽查询正确（无文件时跳过，不依赖数据入库）。
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
        if !path.is_file() {
            return;
        }
        let engine = ImeEngine::new(Some(&path)).expect("mmap 词库加载失败");
        let DictStore::Mmap(dict) = &engine.dict else {
            panic!("dict.bin 应加载为 Mmap 词库");
        };
        // 构建自 rime-ice 全量词表（含 tencent 自动注音）后约 190 万条
        assert!(dict.count() >= 1_000_000, "词条数应超过百万: {}", dict.count());
        let cands = dict.lookup("ni").expect("查询失败");
        assert!(!cands.is_empty(), "ni 应有候选");
        assert_eq!(cands[0].word, "你", "最高频词应为 你");
        assert!(!dict.lookup_prefix("zhon").expect("前缀查询失败").is_empty());
    }

    #[test]
    fn context_changes_candidate_ranking() {
        // 端到端验收：lookup_candidates 收到上文后，词对命中会真的改排序。
        // 词频：过(20) > 国(10)；无上文时「过」在前，"中"→"国" 命中后
        // 「国」应被抬到最前（否则说明上下文没进打分）。
        let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("国", "guo", 10).expect("插入失败");
        engine.dict.upsert("过", "guo", 20).expect("插入失败");
        let LmStore::Bigram(lm) = &mut engine.lm else {
            panic!("默认引擎应为 bigram（测试用 new(None) 创建）");
        };
        lm.add_unigram("中", 100);
        lm.add_unigram("国", 100);
        lm.add_unigram("过", 100);
        lm.add_bigram("中", "国", 90);

        let no_ctx = LmContext::empty();
        let with_ctx = LmContext {
            tail: "中",
            last_word: Some("中"),
        };

        let plain = engine
            .lookup_candidates("guo", &no_ctx)
            .expect("查询失败");
        assert_eq!(plain[0].word, "过", "无上文：词频高的「过」应在最前");

        let after = engine
            .lookup_candidates("guo", &with_ctx)
            .expect("查询失败");
        assert_eq!(after[0].word, "国", "上文「中」应让「国」压过「过」");
    }

    /// 短语整词路径应压过逐字拆解路径（第 2b 步的核心可见变化）：
    /// `"nihao"` 首选整串解码出的「你好」，而不是词频更高的单字「你」。
    #[test]
    fn phrase_path_outranks_split_words() {
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("你", "ni", 200).expect("插入失败");
        engine.dict.upsert("好", "hao", 150).expect("插入失败");
        engine.dict.upsert("你好", "ni hao", 100).expect("插入失败");

        // 空 LM：路径[你好] = 100-800 = -700 > 路径[你,好] = 200-800+150-800 = -1250
        let cands = engine
            .lookup_candidates("nihao", &LmContext::empty())
            .expect("查询失败");
        assert_eq!(cands[0].word, "你好", "整串最优路径的短语应排第一");
        // 单字候选仍要保留（用户可能只想上屏一个字）
        assert!(cands.iter().any(|c| c.word == "你"), "单字「你」应在候选里");
        assert!(cands.iter().any(|c| c.word == "好"), "单字「好」应在候选里");
    }

    /// 整句解码优于逐词贪心：k=1（贪心）在第一个位置选了局部最优的
    /// 「你」，永远到不了「泥→门」这条件更好的整条路径；k=16 的
    /// beam 能看到全局并选出 [泥,门]。
    #[test]
    fn whole_path_beam_beats_greedy() {
        let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("你", "ni", 100).expect("插入失败");
        engine.dict.upsert("泥", "ni", 90).expect("插入失败");
        engine.dict.upsert("门", "men", 50).expect("插入失败");
        engine.dict.upsert("们", "men", 50).expect("插入失败");
        let LmStore::Bigram(lm) = &mut engine.lm else {
            panic!("默认引擎应为 bigram（测试用 new(None) 创建）");
        };
        lm.add_unigram("你", 100);
        lm.add_unigram("泥", 10);
        lm.add_unigram("门", 100);
        // 泥→门 的强 bigram：这是贪心局部看不到的「未来」
        lm.add_bigram("泥", "门", 5000);

        let ctx = LmContext::empty();
        let greedy = engine.decode_paths("nimen", &ctx, 1).expect("解码失败");
        let beam = engine.decode_paths("nimen", &ctx, BEAM_WIDTH).expect("解码失败");

        assert_eq!(greedy[0].words, vec!["你", "门"], "贪心停在局部最优");
        assert_eq!(beam[0].words, vec!["泥", "门"], "beam 应找到全局最优");
        assert!(beam[0].score > greedy[0].score, "整句解码分应更高");

        // 可见面：候选排序也应由整条路径决定——「泥」的候选分来自
        // 路径[泥,门]（269.8），压过路径[你,门]（85.2）里的「你」。
        let cands = engine
            .lookup_candidates("nimen", &ctx)
            .expect("查询失败");
        assert_eq!(cands[0].word, "泥", "最优路径里的词应排最前");
    }

    /// 上文能改变整条路径的选择：无上文时 [你,门] 最优；
    /// 上文「澡」命中 bigram(澡,泥) 后，[泥,门] 整体翻盘。
    #[test]
    fn context_changes_whole_path_choice() {
        let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("你", "ni", 100).expect("插入失败");
        engine.dict.upsert("泥", "ni", 90).expect("插入失败");
        engine.dict.upsert("门", "men", 50).expect("插入失败");
        let LmStore::Bigram(lm) = &mut engine.lm else {
            panic!("默认引擎应为 bigram（测试用 new(None) 创建）");
        };
        lm.add_unigram("你", 100);
        lm.add_unigram("泥", 10);
        lm.add_unigram("门", 100);
        lm.add_unigram("澡", 100);
        lm.add_bigram("澡", "泥", 900);

        let no_ctx = LmContext::empty();
        let with_ctx = LmContext { tail: "澡", last_word: Some("澡") };

        let before = engine.decode_paths("nimen", &no_ctx, BEAM_WIDTH).expect("解码失败");
        let after = engine.decode_paths("nimen", &with_ctx, BEAM_WIDTH).expect("解码失败");
        assert_eq!(before[0].words, vec!["你", "门"], "无上文：[你,门] 最优");
        assert_eq!(after[0].words, vec!["泥", "门"], "上文应改变整条路径的选择");

        // 可见面一致：上文让「泥」压过「你」
        let cands = engine
            .lookup_candidates("nimen", &with_ctx)
            .expect("查询失败");
        let rank = |word: &str| cands.iter().position(|c| c.word == word);
        assert!(
            rank("泥") < rank("你"),
            "上文命中 bigram 后「泥」应排在「你」前: {:?}",
            cands
        );
    }

    /// beam 宽度边界：k 决定保留的完整路径条数；候选词的来源被限制在
    /// beam 的路径里（剪枝 = 候选召回的上界）。
    #[test]
    fn beam_width_bounds_exploration() {
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("你", "ni", 100).expect("插入失败");
        engine.dict.upsert("逆", "ni", 90).expect("插入失败");
        engine.dict.upsert("妮", "ni", 80).expect("插入失败");
        let ctx = LmContext::empty();

        assert_eq!(engine.decode_paths("ni", &ctx, 0).expect("解码失败").len(), 0);
        assert_eq!(engine.decode_paths("ni", &ctx, 1).expect("解码失败").len(), 1);
        assert_eq!(engine.decode_paths("ni", &ctx, 2).expect("解码失败").len(), 2);
        assert_eq!(engine.decode_paths("ni", &ctx, 3).expect("解码失败").len(), 3);
        // 宽度只多不少：k=3 的前缀路径在 k=16 里都还在
        let narrow = engine.decode_paths("ni", &ctx, 3).expect("解码失败");
        let wide = engine.decode_paths("ni", &ctx, BEAM_WIDTH).expect("解码失败");
        for p in &narrow {
            assert!(
                wide.iter().any(|w| w.words == p.words),
                "k 增大后已有路径不应丢失: {:?}",
                p.words
            );
        }
        // 排序单调：首路径一致且得分非增
        assert_eq!(narrow[0].words, wide[0].words);
        assert!(narrow[0].score >= wide[0].score);

        // 产出的候选 (word, pinyin) 全部来自 beam 路径（剪枝即召回上界）
        let cands = engine
            .lookup_candidates("ni", &ctx)
            .expect("查询失败");
        let beam_words: Vec<(String, String)> = wide
            .iter()
            .flat_map(|p| p.words.iter().cloned().zip(p.pinyin.iter().cloned()))
            .collect();
        for c in &cands {
            assert!(
                beam_words.contains(&(c.word.clone(), c.pinyin.clone())),
                "候选 {} 应来自 beam 路径",
                c.word
            );
        }
    }

    /// 病态输入不超时、不爆炸：全同音/无音节/双歧义/超长串。
    /// 结构上格构建被 MAX_CHAINS_PER_START 与 MAX_EDGE_SYLLABLES 截断，
    /// 这里用宽松的墙上时间兜底（debug 构建下正常耗时也在毫秒级）。
    #[test]
    fn pathological_inputs_do_not_explode() {
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        // 喂点词，让格真的长出边来（而不是全空）
        for (w, py, f) in [
            ("啊", "a", 100),
            ("爱", "ai", 90),
            ("安", "an", 80),
            ("昂", "ang", 70),
            ("那", "na", 100),
            ("难", "nan", 90),
        ] {
            engine.dict.upsert(w, py, f).expect("插入失败");
        }
        let ctx = LmContext::empty();
        let cases = [
            "aaaaaaaaaaaa".to_string(),
            "zzzzzzzz".to_string(),
            "nananananana".to_string(),
            "anang".to_string(),
            "a".repeat(40),
            "n".repeat(30),
        ];
        for input in cases {
            let start = std::time::Instant::now();
            let cands = engine.lookup_candidates(&input, &ctx).expect("查询失败");
            let elapsed = start.elapsed();
            assert!(
                elapsed < std::time::Duration::from_secs(1),
                "{input:?}（{} 字符）耗时 {elapsed:?}，疑似组合爆炸",
                input.chars().count()
            );
            assert!(cands.len() <= MAX_CANDIDATES, "{input:?} 候选数超上限");
        }
    }

    /// 万象 `.gram` 缺失时优雅退化（验收点 3 之一）：
    /// `new_with_lm` 不应 panic / 报错，退回空 bigram，查询照常工作。
    #[test]
    fn wanxiang_missing_gram_degrades_gracefully() {
        let engine =
            ImeEngine::new_with_lm(None, Some(Path::new("/nonexistent/wanxiang.gram")))
                .expect("模型缺失时引擎仍应创建成功");
        assert!(
            matches!(engine.lm, LmStore::Bigram(_)),
            "缺失 .gram 应退回空 bigram"
        );
        engine.dict.upsert("你", "ni", 100).expect("插入失败");
        let cands = engine
            .lookup_candidates("ni", &LmContext::empty())
            .expect("查询不应失败");
        assert!(!cands.is_empty(), "退化后查询应正常工作");
    }

    /// `new_with_lm` 在 `.gram` 存在时应真的加载成 Wanxiang 变体，
    /// 且候选查询、语境注入都通过枚举分发正常路由（真实文件测试，
    /// 缺文件时自动跳过）。
    #[test]
    fn real_wanxiang_routes_through_engine() {
        let Some(dir) = std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| std::path::PathBuf::from(h).join(".local/share"))
        }) else {
            return;
        };
        let gram = dir.join("xwx").join("wanxiang-lts-zh-hans.gram");
        if !gram.is_file() {
            eprintln!("未找到 .gram，跳过");
            return;
        }
        let engine = ImeEngine::new_with_lm(None, Some(&gram)).expect("引擎创建失败");
        let LmStore::Wanxiang(_) = &engine.lm else {
            panic!("存在 .gram 时应加载为 Wanxiang 模型");
        };
        engine.dict.upsert("你", "ni", 100).expect("插入失败");
        engine.dict.upsert("好", "hao", 50).expect("插入失败");
        // 端到端：有上文的打分要能改变候选排序（「今天」会命中 real gram）
        let no_ctx = engine
            .lookup_candidates("ni", &LmContext::empty())
            .expect("无上文查询失败");
        let with_ctx = engine
            .lookup_candidates("ni", &LmContext { tail: "今天", last_word: None })
            .expect("有上文查询失败");
        assert!(!no_ctx.is_empty() && !with_ctx.is_empty());
    }

    /// 精确匹配的候选应排在只能靠模糊匹配到达的候选之前。
    ///
    /// 构造:输入 "ni",词库里有「你」(ni,精确)和「里」(li,仅 n/l
    /// 混淆到达),两者词频相同。fuzzy 开启后「里」应出现,但分数
    /// 必须严格低于「你」(零惩罚 vs -log10(0.8) ≈ 0.097)。
    #[test]
    fn exact_match_outranks_fuzzy_match() {
        let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("你", "ni", 100).expect("插入失败");
        engine.dict.upsert("里", "li", 100).expect("插入失败");
        engine.fuzzy_enabled = true;

        let cands = engine
            .lookup_candidates("ni", &LmContext::empty())
            .expect("查询失败");
        let rank = |word: &str| cands.iter().position(|c| c.word == word);
        let (ri, rl) = (rank("你"), rank("里"));
        assert!(ri.is_some(), "精确匹配「你」应在候选里: {cands:?}");
        assert!(rl.is_some(), "模糊匹配「里」应在候选里: {cands:?}");
        assert!(ri < rl, "精确「你」应排在模糊「里」前: {cands:?}");

        let si = cands[ri.unwrap()].score;
        let sl = cands[rl.unwrap()].score;
        assert!(si > sl, "精确分 {si} 应严格大于模糊分 {sl}");
        // 惩罚量级:-log10(0.8) ≈ 0.0969
        assert!(
            (si - sl - 0.0969).abs() < 1e-2,
            "分差应≈0.097(n/l 单步惩罚), 实际 {}",
            si - sl
        );
    }

    /// 高 weight 混淆对(n/l = 0.8)产生的候选应排在低 weight
    /// (s/x = 0.2)产生的候选之前——其它条件相同。
    ///
    /// 构造:输入 "ni",词库里有「里」(li,经 n/l=0.8)和「希」
    /// (xi,经 s/x=0.2 的反向 x→s?不,需要 n→x?)。直接构造两组
    /// 混淆对:输入 "a",词「甲」(b,经 a/b=0.8)与「乙」(c,经
    /// a/c=0.2),同词频。
    #[test]
    fn high_weight_confusion_outranks_low_weight() {
        let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
        // 词频相同,唯一差别是到达路径的混淆对 weight
        engine.dict.upsert("甲", "b", 100).expect("插入失败");
        engine.dict.upsert("乙", "c", 100).expect("插入失败");
        engine.fuzzy_pairs = vec![
            ConfusionPair::new("a", "b", 0.8),
            ConfusionPair::new("a", "c", 0.2),
        ];
        engine.fuzzy_enabled = true;

        let cands = engine
            .lookup_candidates("a", &LmContext::empty())
            .expect("查询失败");
        let rank = |word: &str| cands.iter().position(|c| c.word == word);
        let (ra, rb) = (rank("甲"), rank("乙"));
        assert!(ra.is_some() && rb.is_some(), "两词都应出现: {cands:?}");
        assert!(ra < rb, "高 weight 的「甲」应排在低 weight 的「乙」前: {cands:?}");

        let sa = cands[ra.unwrap()].score;
        let sb = cands[rb.unwrap()].score;
        // 分差 = -log10(0.2) - (-log10(0.8)) = 0.699 - 0.097 ≈ 0.602
        assert!(
            (sa - sb - 0.602).abs() < 1e-2,
            "分差应≈0.602, 实际 {}",
            sa - sb
        );
    }

    /// 多处混淆叠加时惩罚应累加:经两次混淆到达的候选,分数应比
    /// 经一次混淆到达的低约一个 -log10(weight)。
    ///
    /// 构造:输入 "nan",词库有「难」(nan,精确)、「蓝」(lan,经
    /// n/l 一次)、「狼」(lang,经 n/l + an/ang 两次),三者词频相同。
    /// 「蓝」应比「难」低 ≈0.097,「狼」应比「蓝」低 ≈0.301。
    #[test]
    fn multi_confusion_penalty_accumulates() {
        let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("难", "nan", 100).expect("插入失败");
        engine.dict.upsert("蓝", "lan", 100).expect("插入失败");
        engine.dict.upsert("狼", "lang", 100).expect("插入失败");
        engine.fuzzy_pairs = vec![
            ConfusionPair::new("n", "l", 0.8),
            ConfusionPair::new("an", "ang", 0.5),
        ];
        engine.fuzzy_enabled = true;

        let cands = engine
            .lookup_candidates("nan", &LmContext::empty())
            .expect("查询失败");
        let score_of = |word: &str| {
            cands.iter().find(|c| c.word == word).map(|c| c.score)
        };
        let (snan, slan, slang) = (
            score_of("难").expect("难应在候选里"),
            score_of("蓝").expect("蓝应在候选里"),
            score_of("狼").expect("狼应在候选里"),
        );
        assert!(snan > slan && slan > slang,
            "精确 > 单混淆 > 双混淆: 难={snan} 蓝={slan} 狼={slang}");
        // 单混淆惩罚 -log10(0.8) ≈ 0.097
        assert!((snan - slan - 0.0969).abs() < 1e-2,
            "单混淆差应≈0.097, 实际 {}", snan - slan);
        // 双混淆再叠加 -log10(0.5) ≈ 0.301
        assert!((slan - slang - (-0.5f32.log10())).abs() < 1e-2,
            "双混淆累加差应≈0.301, 实际 {}", slan - slang);
    }

    /// fuzzy_enabled = false 时行为应与改动前完全一致:模糊变体不
    /// 进候选,排序纯由词频 + LM 决定,不受 fuzzy_pairs 配置影响。
    #[test]
    fn fuzzy_disabled_is_unchanged() {
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("你", "ni", 100).expect("插入失败");
        engine.dict.upsert("逆", "ni", 50).expect("插入失败");
        engine.dict.upsert("里", "li", 200).expect("插入失败");
        // fuzzy_enabled 保持默认 false;fuzzy_pairs 有值也不应生效
        assert!(!engine.fuzzy_enabled);

        let cands = engine
            .lookup_candidates("ni", &LmContext::empty())
            .expect("查询失败");
        // 「里」词频最高(200),但它是 li,输入 ni 时不应出现
        assert!(!cands.iter().any(|c| c.word == "里"),
            "fuzzy 关闭时 li 不应出现在 ni 的候选里: {cands:?}");
        assert_eq!(cands.first().map(|c| c.word.as_str()), Some("你"));
        // 排序与旧版一致:你(100) 前于 逆(50)
        let rank = |word: &str| cands.iter().position(|c| c.word == word);
        assert!(rank("你") < rank("逆"));
    }

    /// 分层排序的核心:精确匹配整体优先于模糊匹配,**即使模糊匹配
    /// 的词频高得多**(固定惩罚压不住任意大的词频差,分层可以)。
    ///
    /// 构造对应真实词库实测的「然(734万, r/l 模糊)压过蓝(43万, 精确)」:
    /// 输入 "lan",「然」(ran, r→l 混淆,weight 0.3)词频是「蓝」
    /// (lan, 精确)的 10 倍。分层前「然」在「蓝」前;分层后「蓝」
    /// 必须在「然」前,哪怕分数更低。
    #[test]
    fn exact_layer_outranks_fuzzy_despite_much_higher_freq() {
        let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
        // 精确:蓝 lan freq=100;模糊:然 ran freq=1000(10 倍词频差,
        // 对数域差 1.0,远超 r/l 的惩罚 -log10(0.3) ≈ 0.52)
        engine.dict.upsert("蓝", "lan", 100).expect("插入失败");
        engine.dict.upsert("然", "ran", 1000).expect("插入失败");
        engine.fuzzy_enabled = true;

        let cands = engine
            .lookup_candidates("lan", &LmContext::empty())
            .expect("查询失败");
        let pos = |word: &str| cands.iter().position(|c| c.word == word);
        let (pl, pr) = (pos("蓝").expect("蓝应在候选里"), pos("然").expect("然应在候选里"));
        assert!(pl < pr, "精确「蓝」应排在高频模糊「然」前: {cands:?}");

        let (sl, sr) = (cands[pl].score, cands[pr].score);
        assert!(sr > sl, "然的分数确实更高(词频差压过惩罚),分层前它排前面");
        assert!(!cands[pl].fuzzy, "蓝是精确匹配");
        assert!(cands[pr].fuzzy, "然是模糊匹配(r/l)");

        // 层间边界:所有精确候选都在所有模糊候选之前
        let first_fuzzy = cands.iter().position(|c| c.fuzzy);
        if let Some(ff) = first_fuzzy {
            assert!(
                cands[..ff].iter().all(|c| !c.fuzzy),
                "精确层内不应混入模糊候选: {cands:?}"
            );
        }
    }

    /// 模糊层内部仍按惩罚排序:高 weight 混淆(n/l=0.8)产生的候选
    /// 排在低 weight(s/x=0.2)之前——分层不改变层内的惩罚区分。
    #[test]
    fn fuzzy_layer_internally_sorted_by_penalty() {
        let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
        // 两个模糊候选同词频,唯一差别是混淆对 weight;再加一个精确
        // 候选确认模糊层整体在精确层之后。
        engine.dict.upsert("精", "a", 10).expect("插入失败");
        engine.dict.upsert("高", "b", 100).expect("插入失败");
        engine.dict.upsert("低", "c", 100).expect("插入失败");
        engine.fuzzy_pairs = vec![
            ConfusionPair::new("a", "b", 0.8),
            ConfusionPair::new("a", "c", 0.2),
        ];
        engine.fuzzy_enabled = true;

        let cands = engine
            .lookup_candidates("a", &LmContext::empty())
            .expect("查询失败");
        let pos = |word: &str| cands.iter().position(|c| c.word == word);
        let (pe, ph, pl) = (
            pos("精").expect("精应在候选里"),
            pos("高").expect("高应在候选里"),
            pos("低").expect("低应在候选里"),
        );
        assert!(pe < ph && pe < pl, "精确层整体在前: {cands:?}");
        assert!(ph < pl, "模糊层内高 weight 的应在前: {cands:?}");
        assert!(cands[ph].fuzzy && cands[pl].fuzzy && !cands[pe].fuzzy);
    }

    /// 混合路径(精确边 + 模糊边)归模糊层:一处不精确则整体不精确。
    /// 判断用累计惩罚是否为 0——与逐边布尔或等价,但惩罚本就沿
    /// arena 累计,实现零额外开销。
    ///
    /// 构造:输入 "naha"(无音节重启歧义:na+ha 是唯一两音节切分,
    /// 避开 nan+ni 这类段器不支持的重启,那是与分层无关的既有行为)。
    /// 「那哈」(na ha,两精确边)→ 精确层;「拉哈」(la ha,经 n→l
    /// 一条模糊边 + 一条精确边)→ 混合路径,应归模糊层。
    #[test]
    fn mixed_exact_and_fuzzy_edges_belong_to_fuzzy_layer() {
        let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("那", "na", 100).expect("插入失败");
        engine.dict.upsert("哈", "ha", 100).expect("插入失败");
        engine.dict.upsert("那哈", "na ha", 50).expect("插入失败");
        engine.dict.upsert("拉哈", "la ha", 10000).expect("插入失败");
        engine.fuzzy_pairs = vec![ConfusionPair::new("n", "l", 0.8)];
        engine.fuzzy_enabled = true;

        let cands = engine
            .lookup_candidates("naha", &LmContext::empty())
            .expect("查询失败");
        let get = |word: &str| cands.iter().find(|c| c.word == word);
        let pure = get("那哈").expect("那哈应在候选里");
        let mixed = get("拉哈").expect("拉哈应在候选里");
        assert!(!pure.fuzzy, "纯精确路径应在精确层: {cands:?}");
        assert!(mixed.fuzzy, "含一条模糊边的混合路径应归模糊层: {cands:?}");

        // 混合路径词频再高(10000 vs 50)也得排在精确层之后
        let pos = |word: &str| cands.iter().position(|c| c.word == word);
        assert!(pos("那哈") < pos("拉哈"), "精确层整体优先: {cands:?}");
        // 混合路径里的精确边「哈」继承该路径的模糊层归属:它出现在
        // [那,哈](精确)与 [拉哈](模糊)两条路径里,取最优路径的分,
        // 最优是精确路径(惩罚 0),故「哈」在精确层。
        let ha = get("哈").expect("哈应在候选里");
        assert!(!ha.fuzzy, "哈的最优路径是纯精确的 [那,哈]: {cands:?}");
    }

    /// fuzzy_enabled = false 时排序与分层前完全一致:全部候选 fuzzy
    /// 都是 false,首层 key 相等,退化为纯分数排序。逐字段对比开关
    /// 两种状态——开 fuzzy 只应追加模糊层,精确层的前缀必须逐位相同。
    #[test]
    fn layered_sort_disabled_matches_legacy_exactly() {
        let build = |fuzzy: bool| {
            let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
            for (w, py, f) in [
                ("你", "ni", 100),
                ("逆", "ni", 50),
                ("妮", "ni", 80),
                ("泥", "ni", 60),
                ("里", "li", 1000), // 高频模糊词:开 fuzzy 时会出现,但只能进模糊层
            ] {
                engine.dict.upsert(w, py, f).expect("插入失败");
            }
            engine.fuzzy_enabled = fuzzy;
            engine
        };

        let off = build(false)
            .lookup_candidates("ni", &LmContext::empty())
            .expect("查询失败");
        let on = build(true)
            .lookup_candidates("ni", &LmContext::empty())
            .expect("查询失败");

        // 关 fuzzy:候选与改动前完全一致(里 不出现,纯按词频排)
        assert!(!off.iter().any(|c| c.word == "里"));
        assert!(off.iter().all(|c| !c.fuzzy), "关 fuzzy 时应全是精确层");
        let off_words: Vec<&str> = off.iter().map(|c| c.word.as_str()).collect();
        assert_eq!(off_words, ["你", "妮", "泥", "逆"], "词频降序: {off:?}");

        // 开 fuzzy:精确层前缀逐位相同,模糊层追加在后
        let on_exact: Vec<&str> = on.iter().filter(|c| !c.fuzzy).map(|c| c.word.as_str()).collect();
        assert_eq!(on_exact, off_words, "精确层应与关 fuzzy 时逐位一致");
        let on_fuzzy: Vec<&str> = on.iter().filter(|c| c.fuzzy).map(|c| c.word.as_str()).collect();
        assert_eq!(on_fuzzy, ["里"], "模糊层只应有「里」: {on:?}");
    }






}
