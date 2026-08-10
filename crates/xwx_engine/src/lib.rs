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
pub mod punctuator;
pub mod recognizer;
pub mod segmenter;
pub mod session;
pub mod user_lm;
pub mod wanxiang;

pub use candidate::{Candidate, SpellingKind};
pub use corrector::{expand_fuzzy, expand_fuzzy_scored, ConfusionPair, COMMON_CONFUSION_PAIRS};
pub use dict::{DictError, DictStore, Dictionary, MmapDict, SqliteDict};
pub use lm::{BigramLM, LanguageModel, LmContext, LmError, LmState, LmStore};
pub use lm::{MixedLM, MixedState, MIX_USER_WEIGHT};
pub use recognizer::{recognize, Recognized, URL_PREFIXES};
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
/// 简拼层的保底名额(不是封顶):总条数超 `MAX_CANDIDATES` 时,
/// 简拼层至少留这么多个候选位,不被精确/模糊/兜底层整层挤掉。
///
/// 理由(实测依据,见 PLAN.md 第三节「混合简拼被截断」):
/// - 缺口输入(`zhongguor`/`nih`/`renm`)的目标词清一色落在简拼层
///   第 1 名,保底 5 个绰绰有余;
/// - 代价侧:head 装满 100 条时被挤掉的只是第 96~100 名(局部切分
///   出的碎片单字/兜底长尾),翻 10 页才到,代价可忽略;
/// - 只是保底,不是上限:head 很短时简拼层照旧补满 100(与旧
///   「合并排序截 100」逐位一致,见 `merge_with_abbrev`);
/// - 简拼层不足 5 条时按实际条数预留;为 0 时 head 照旧用满 100,
///   不白留空位。
const ABBREV_QUOTA: usize = 5;
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
///
/// Block A 之后 `lookup_candidates` 只消费「是否为空」(决定走不走
/// 兜底),字段本体仅供 `decode_paths`(测试/调试)读取,故 lib 构建
/// 下标记 `allow(dead_code)`。
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug)]
struct BeamPath {
    /// 累计分 = Σ(log10(freq/总词频) + lm_weight × log10 P(word | 路径上文))。
    ///
    /// 两项同量纲（都是对数概率，负值）才可加：词频项对数化后，短语
    /// 不再被百万级原始词频淹没（第 4 步量级失衡修复的核心）。
    score: f32,
    /// 路径已选词的顺序。
    words: Vec<WordSpot>,
}

/// beam 搜索节点：arena 里的一条路径节点，`parent` 指向上一步。
///
/// 用「节点 + 父指针」而不是在每个 BeamPath 里拷贝整条词序列：
/// 扩展只 push 一个节点（O(1) 分配），结束时再沿父链重建词序。
#[derive(Debug)]
struct BeamNode {
    /// 累计分 = Σ(log10(freq/总词频) + lm_weight × log10 P(word | 路径上文))
    ///          - Σ(各边 fuzzy_penalty)(模糊惩罚已揉进分数,非负)。
    score: f32,
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

/// 路径里一个词的位置(字段仅供 `decode_paths` 测试/调试读取)。
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug)]
struct WordSpot {
    word: String,
    pinyin: String,
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

    /// 记一条造词条目(unity_table_encoder 的等价物:把上屏的整段
    /// 反查编码后写入用户词典)。
    ///
    /// 由 [`InputSession`](crate::session::InputSession) 的造词缓冲在
    /// 「连续候选上屏段被打断/超长且 ≥ 2 字」时调用。`pinyin_key`
    /// 是空格分隔的音节串(与词典 key 同格式),由缓冲里各片段的
    /// 候选拼音拼接而成。无用户层时是 no-op(与 `learn_committed`
    /// 同一取舍)。落盘复用用户模型的 dirty/flush 调度,不在此同步做。
    pub(crate) fn coin_entry(&self, word: &str, pinyin_key: &str) {
        let Some(user) = self.user_lm() else {
            return;
        };
        user.add_entry(word, pinyin_key);
    }

    /// 删一条候选(Ctrl+Delete 的引擎侧落点):屏蔽它,此后同拼音查询
    /// 不再出现;若是用户造词条目则连同词条删掉(词库词只屏蔽——
    /// 词库只读,mmap 不可改,屏蔽是唯一可行的删除形态)。
    ///
    /// 无用户层时是 no-op(与 [`Self::learn_committed`] / [`Self::coin_entry`]
    /// 同一取舍:删候选是用户数据,没有用户数据层就没有「删除」的载体)。
    /// 落盘复用用户模型的 dirty/flush 调度,不在此同步做。
    pub(crate) fn delete_candidate(&self, word: &str, pinyin_key: &str) {
        let Some(user) = self.user_lm() else {
            return;
        };
        user.block_candidate(word, pinyin_key);
    }

    /// 当前词库词条总数（启动日志/统计用；内存空词库时为 0）。
    pub fn dict_count(&self) -> u64 {
        match &self.dict {
            DictStore::Sqlite(d) => d.count().unwrap_or(0),
            DictStore::Mmap(d) => d.count(),
        }
    }

    /// 核心查询管线:给定拼音串,返回排序后的候选列表。
    ///
    /// 做法(Block A 重构后:候选来源与整句解码解耦):
    ///
    /// 1. 构建切分格:[`Self::build_lattice`] 从每个位置枚举音节链,
    ///    每条链的拼音串查词库得「词边」(含模糊拼音变体边),
    ///    不再枚举全部切分;
    /// 2. beam search:[`Self::decode_beam_with_lattice`] 沿位置推进,
    ///    每个位置只保留 top-K 条部分路径,LM 状态**沿路径传递**——
    ///    beam 的职责只剩「算出完整路径是否存在」(它同时是兜底分支的
    ///    判定信号),不再充当候选来源(旧实现候选被 BEAM_WIDTH 钳死);
    /// 3. 候选收集(A1+A2):候选 = `lattice[0]` 上的**全部**词边——
    ///    只取起始于位置 0 的词(旧实现把路径上所有词位都列为候选,
    ///    导致 `nihao` 里混进只覆盖 `hao` 的「好/号/毫」),且不被
    ///    beam 宽度钳制(词典查询在格构建时已完成,这里零新增查询);
    /// 4. 打分口径(A3):候选分 = **「以该候选开头的最优完整路径分」**
    ///    = 边自身得分 + 从 `edge.end` 到串尾的最优补全分,补全分由
    ///    一次后向 DP 在格上算出(见 [`Self::backward_completion`])。
    ///    这样「覆盖整串的词」(你好)与「只覆盖前缀的词」(你)落在
    ///    同一量纲上,前者自然胜出;
    /// 5. 整串无法完整解码(还没打完,如 `"nih"`、`"z"`)时走
    ///    [`Self::fallback_candidates`] 的边打字边联想兜底。
    ///
    /// 两条路径(正常 / 兜底)末尾都会再合并一次简拼来源
    /// ([`Self::abbrev_candidates`]):简拼候选本来就覆盖整个输入串
    /// (用户打前缀、拿到整词),不进 lattice、不拆成边,按
    /// [`SpellingKind::Abbrev`] 排进候选列表的简拼层。
    pub fn lookup_candidates(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
    ) -> Result<Vec<Candidate>, EngineError> {
        if pinyin.is_empty() {
            return Ok(Vec::new());
        }

        // B2 音节分隔符:`'` 是用户敲的**强制切分点**(`xi'an` 消歧出
        // 「西安」)。这里把它归一化成「干净串 + 边界表」两个量:
        // - `pinyin`:去掉 `'` 的干净串。`'` 不进词典 key、不进
        //   `Candidate.pinyin`(词典 key 是空格分隔音节);
        // - `boundaries`:每个 `'` 在干净串中对应位置的**字节偏移**
        //   (`xi'an` → 干净串 `xian` + `[2]`),即「不允许任何音节/边
        //   跨越」的位置表,贯穿格构建、beam、后向 DP 与排序的 u_min
        //   桶([`Self::build_lattice`] 与 [`unreachable_tail`] 的注释)。
        //
        // **只删 `'` 不传边界是错的**(上一版实现):删完 `xi'an` 变回
        // `xian`,而 `xian` 本身是合法音节,`[xian]` 这条单音节切法原样
        // 复活,「先/现/显/线」照常在候选里——`'` 完全没起到强制切分的
        // 作用(实机:`xian` 与 `xi'an` 候选逐字节相同)。正确语义是
        // librime 的 `delimiters`:`'` 把输入钉成 [xi | an] 两段,
        // 段内照常枚举音节链,**跨段边一律不建**。
        //
        // 归一化在这里做(而不是会话层):让「展示给用户的串」与
        // 「查词典的串」解耦——会话/preedit 保留用户敲的 `xi'an`,
        // 引擎内部只认干净拼音。无 `'` 输入走 `Cow::Borrowed` +
        // 空边界表,零分配,既有路径**逐字节不变**(判退化的硬约束)。
        let (pinyin, boundaries): (std::borrow::Cow<'_, str>, Vec<usize>) =
            if pinyin.contains('\'') {
                let mut clean = String::with_capacity(pinyin.len());
                let mut bounds = Vec::new();
                for ch in pinyin.chars() {
                    if ch == '\'' {
                        // `'` 是单字节 ASCII,clean.len() 即干净串中的字节偏移
                        bounds.push(clean.len());
                    } else {
                        clean.push(ch);
                    }
                }
                (std::borrow::Cow::Owned(clean), bounds)
            } else {
                (std::borrow::Cow::Borrowed(pinyin), Vec::new())
            };
        let pinyin: &str = &pinyin;
        if pinyin.is_empty() {
            // 输入全是 `'`(如 `'`、`''`):归一化后为空,无候选。
            return Ok(Vec::new());
        }
        let bounds: Option<&[usize]> = if boundaries.is_empty() {
            None
        } else {
            Some(&boundaries)
        };

        let lattice = self.build_lattice(pinyin, bounds)?;
        let paths = self.decode_beam_with_lattice(pinyin, ctx, BEAM_WIDTH, &lattice)?;
        if paths.is_empty() {
            // 整串还不能完整解码(首音节没打完 / 停在半音节):兜底联想,
            // 否则 IBus 层会在打字中途发 HideLookupTable,候选窗闪一下就没。
            let ranked = self.fallback_candidates(pinyin, ctx)?;
            // 简拼来源合并(2):简拼候选覆盖整串,本身就是整串候选。
            // 注意:必须在 fallback 截断**之后**合并——fallback 自己
            // 已按 MAX_CANDIDATES 截断,若先合并再截断,简拼层(在排序
            // 最末)会整体被截掉。合并后重排截断,简拼层得以保留
            // (它只与同样排在末尾的兜底长尾竞争最后的候选位)。
            // 简拼层只在「无 `'`」时合并:用户敲了 `'` 就是给了明确的
            // 全拼切分,`lookup_abbrev`/`lookup_mixed` 会把干净串当简拼
            // 前缀重新查出 `[xian]` 系的字(先/现/显),把刚消掉的歧义
            // 又灌回来——含 `'` 的查询语义上不是简拼,跳过简拼层。
            let abbrev = if bounds.is_none() {
                self.abbrev_candidates(pinyin, ctx, &ranked)?
            } else {
                Vec::new()
            };
            let (_, mut ranked) = merge_with_abbrev(pinyin, ranked, abbrev);
            self.filter_blocked(&mut ranked);
            if ranked.is_empty() {
                ranked.push(Self::echo_candidate(pinyin));
            }
            return Ok(ranked);
        }

        // A3 打分:候选分 = 以该候选开头的最优完整路径分。
        // 先跑后向 DP 拿「每个位置到串尾的最优补全分」,再给 lattice[0]
        // 上每条边打分。
        let best_suffix = self.backward_completion(pinyin, ctx, &lattice)?;
        let total_freq = self.dict.total_freq()?;
        let log10_total = (total_freq as f64).log10();

        // 首词(位置 0)的 LM 状态由真实上文 ctx 给出,这是精确的
        // (首词的前文就是已上屏文本,不存在近似问题)。
        let initial = self.lm.state_from_context(ctx);

        // 同一 (word, pinyin) 可能由多条边给出(不同音节链切到同一拼音
        // key,或模糊展开键与精确键撞车),保留得分最高的一条。
        let mut best: HashMap<(String, String), Candidate> = HashMap::new();
        for edge in &lattice[0] {
            let (logp, _) = self.lm.score(&initial, &edge.word);
            let dict_logp = if total_freq > 0 {
                ((edge.freq.max(1) as f64).log10() - log10_total) as f32
            } else {
                0.0
            };
            // 边自身得分与前向 beam 完全同式;补全分取自后向 DP。
            // 边终点必有完整补全:beam 已确认整串可解,且任何有出边的
            // 位置都能沿格走到串尾(边严格前进、终点不超串长),
            // 故 best_suffix[edge.end] 必为有限值。
            let score = dict_logp + logp * self.lm_weight - edge.fuzzy_penalty
                + best_suffix[edge.end];
            let cand = Candidate {
                word: edge.word.clone(),
                pinyin: edge.pinyin.clone(),
                score,
                freq: edge.freq, // 词典原始频率,供展示/回退
                // 该边自身是否模糊(B4 起为逐边判定的分层来源)。
                spelling: if edge.fuzzy_penalty > 0.0 {
                    SpellingKind::Fuzzy
                } else {
                    SpellingKind::Exact
                },
                echo: false,     // 解码出的都是真实候选
            };
            let entry = best
                .entry((cand.word.clone(), cand.pinyin.clone()))
                .or_insert_with(|| cand.clone());
            if cand.score > entry.score {
                *entry = cand;
            }
        }

        let ranked: Vec<Candidate> = best.into_values().collect();
        // 简拼来源合并(2):已在精确/模糊层出现过的 (word, pinyin)
        // 不再进简拼层(`abbrev_candidates` 内部去重)。截断走
        // `merge_with_abbrev`:简拼层有 ABBREV_QUOTA 个保底名额,
        // 精确层装满 100 时不再把简拼层整层截掉(PLAN.md 第三节缺口)。
        // 含 `'` 时跳过简拼层(见上一条路径的注释):用户给了明确切分,
        // 简拼会把 `[xian]` 系的字重新灌回来,破坏消歧。
        let abbrev = if bounds.is_none() {
            self.abbrev_candidates(pinyin, ctx, &ranked)?
        } else {
            Vec::new()
        };
        let (_, mut ranked) = merge_with_abbrev(pinyin, ranked, abbrev);
        // 回声候选兜底(公开 API 后置条件:非空输入 ⇒ 候选非空):
        // 整串连兜底联想都解不出任何候选时(如不支持的简拼),追加一个
        // word = 原始输入串的回声候选,让用户至少看见自己打的东西,
        // 而不是 IBus 层 HideLookupTable 后"输入法像死了"。语义照搬
        // librime echo_translator:仅当全场一个候选都没有时才冒头,
        // 绝不污染正常候选列表。必须在 truncate 之后追加,才不会被截掉。
        if ranked.is_empty() && !pinyin.is_empty() {
            ranked.push(Self::echo_candidate(pinyin));
        }
        self.filter_blocked(&mut ranked);
        Ok(ranked)
    }

    /// 候选收口的统一过滤:把被用户 Ctrl+Delete 屏蔽的候选从最终
    /// 列表里滤掉(在排序、截断、合并、回声兜底**全部完成后**——
    /// 这样屏蔽不改变任何「保留候选」的相对排序与得分,只做删除)。
    ///
    /// 快照取自用户层(若有):无用户层时一次都不查,**空屏蔽表时
    /// 候选列表逐字节不变**(判退化的结构性保证)。回声候选的
    /// (key, word) = (输入串, 输入串),天然撞不上任何屏蔽键。
    fn filter_blocked(&self, ranked: &mut Vec<Candidate>) {
        let Some(snap) = crate::user_lm::user_entries_snapshot(&self.lm) else {
            return;
        };
        ranked.retain(|c| !snap.is_blocked(&c.pinyin, &c.word));
    }

    /// 后向 DP(A3):`result[pos]` = 从 `pos` 到串尾的最优补全分
    /// (log10 域,越大越好);不可达串尾的位置为 `f32::NEG_INFINITY`。
    ///
    /// 递推(从串尾往前,与 [`Self::decode_beam_with_lattice`] 的前向
    /// beam 对称):`result[pos] = max over lattice[pos] 的边 e of
    /// (e 的词频项 + lm_weight × LM 近似分 - e.fuzzy_penalty + result[e.end])`,
    /// 边界 `result[len] = 0`(空后缀代价为零)。每条边只处理一次,
    /// 复杂度 O(边数)。
    ///
    /// **刻意的近似,不是 bug**:LM 是左到右的(打分需要左侧上下文),
    /// 后向 DP 在位置 `pos` 拿不到「从开头走到这里」的精确 LM 状态,
    /// 只能从「无上文」状态起评每条边的 LM 分。bigram 下等价于每条边
    /// 只拿 unigram/backoff 分、拿不到 bigram 接续分。这个近似
    /// **只影响候选之间的相对排序**(所有候选的补全分用同一口径,
    /// 比较仍然公平),**不影响 beam 选出的整句首选**——首选由前向
    /// beam 用精确 LM 状态决定,不经过这里。若未来要消除近似,只能
    /// 把后向 DP 换成「从 beam 幸存路径反查」(回到被 BEAM_WIDTH 钳死
    /// 的老路)或引入双向 LM,两者都不值得。
    fn backward_completion(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        lattice: &[Vec<LatticeEdge>],
    ) -> Result<Vec<f32>, EngineError> {
        let len = pinyin.len();
        let mut result = vec![f32::NEG_INFINITY; len + 1];
        result[len] = 0.0;
        let total_freq = self.dict.total_freq()?;
        let log10_total = (total_freq as f64).log10();
        // LM 近似的起点状态:无上文。`ctx` 仅供首词(位置 0)精确打分,
        // 后缀位置统一用 initial(见函数级注释的近似说明)。
        let _ = ctx;
        let initial = self.lm.initial_state();

        // `'` 强制切分点的语义已在 `build_lattice` 的逐音节检查里完全
        // 落实——格里本来就不含任何跨 `'` 的边,后向 DP 在格上递推,
        // 继承格的边界语义即可,不需要也不应再做边界过滤。
        for pos in (0..len).rev() {
            for edge in &lattice[pos] {
                if !result[edge.end].is_finite() {
                    continue; // 该边到不了串尾,不构成完整补全
                }
                let (logp, _) = self.lm.score(&initial, &edge.word);
                let dict_logp = if total_freq > 0 {
                    ((edge.freq.max(1) as f64).log10() - log10_total) as f32
                } else {
                    0.0
                };
                let score = dict_logp + logp * self.lm_weight - edge.fuzzy_penalty
                    + result[edge.end];
                if score > result[pos] {
                    result[pos] = score;
                }
            }
        }
        Ok(result)
    }

    /// 构造一个回声候选:word / pinyin 都是原始输入串本身,freq = 0。
    ///
    /// 仅在全场一个真实候选都没有时由 `lookup_candidates` 追加(照搬
    /// librime echo_translator 的最低优先级兜底项),让候选列表非空。
    fn echo_candidate(pinyin: &str) -> Candidate {
        Candidate {
            word: pinyin.to_string(),
            pinyin: pinyin.to_string(),
            score: 0.0,
            freq: 0,
            spelling: SpellingKind::Exact,
            echo: true,
        }
    }

    /// 简拼来源候选(2):`lookup_abbrev` / `lookup_mixed` 命中的整串词。
    ///
    /// 打分口径与 [`Self::fallback_candidates`] 完全同式(同一量纲):
    /// `log10(freq/总词频) + lm_weight × LM(ctx) 分`——简拼候选覆盖
    /// 整串,只打一次词频项与 LM 项,与「一个词覆盖全串」的兜底词同权。
    ///
    /// 去重:已在 `existing`(精确/模糊/兜底层)出现过的 `(word, pinyin)`
    /// 不再进简拼层——同一词条只能属于一个层,靠前层优先(精确 > 模糊
    /// > 简拼的结构性保证因此成立,见 `sort_candidates`)。
    ///
    /// 优雅退化:副索引缺失时 `lookup_abbrev` 返回空、`lookup_mixed`
    /// 靠编译期静态音节表照常工作(见 `MmapDict::lookup_mixed` 注释),
    /// 两条路径都不 panic;`DictStore::Sqlite`(测试/内存词库)没有
    /// 简拼能力,直接返回空。
    ///
    /// 调用方约定:本函数只在 `existing` 排序截断**之前**合并,
    /// 否则截断后的 100 条会丢掉简拼层(merge-then-truncate,
    /// 见 `lookup_candidates` 两条路径的合并点)。
    fn abbrev_candidates(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        existing: &[Candidate],
    ) -> Result<Vec<Candidate>, EngineError> {
        let raw: Vec<Candidate> = match &self.dict {
            DictStore::Mmap(d) => {
                let mut v = d.lookup_abbrev(pinyin)?;
                v.extend(d.lookup_mixed(pinyin)?);
                v
            }
            DictStore::Sqlite(_) => Vec::new(),
        };
        if raw.is_empty() {
            return Ok(Vec::new());
        }

        let total_freq = self.dict.total_freq()?;
        let log10_total = (total_freq as f64).log10();
        let state = self.lm.state_from_context(ctx);
        let seen: std::collections::HashSet<(&str, &str)> = existing
            .iter()
            .map(|c| (c.word.as_str(), c.pinyin.as_str()))
            .collect();

        let mut best: HashMap<(String, String), Candidate> = HashMap::new();
        for mut c in raw {
            if seen.contains(&(c.word.as_str(), c.pinyin.as_str())) {
                continue; // 已在更靠前的层出现:同词不进简拼层
            }
            let (lm_score, _) = self.lm.score(&state, c.word.as_str());
            let dict_logp = if total_freq > 0 {
                ((c.freq.max(1) as f64).log10() - log10_total) as f32
            } else {
                0.0
            };
            c.score = dict_logp + lm_score * self.lm_weight;
            c.spelling = SpellingKind::Abbrev;
            // lookup_abbrev(前缀命中多个简拼 key)与 lookup_mixed(多种
            // 切法)都可能重复命中同一词,简拼层内部仍按 (word, pinyin) 去重。
            let entry = best
                .entry((c.word.clone(), c.pinyin.clone()))
                .or_insert_with(|| c.clone());
            if c.score > entry.score {
                *entry = c;
            }
        }
        Ok(best.into_values().collect())
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
    ///
    /// `bounds` 是 B2 的 `'` 强制切分点(干净串中的字节偏移表,升序;
    /// `None` = 无 `'` 输入,走原有零开销路径)。每个边界把输入钉成前后
    /// 两段:**任何边不得跨越它**——链的音节一旦恰好落在边界上就停下
    /// (不再延长),于是 `xi'an`(干净串 `xian` + 边界 2)的格里没有
    /// [xian] 这条单音节边,「先/现/显/线」从源头消失,「西安」
    /// ([xi, an])成为唯一的整串双音节切法。`'` 本身已在
    /// `lookup_candidates` 归一化时剥掉,干净串里没有 `'` 字符,
    /// 词典 key 与 `Candidate.pinyin` 因此永远不含 `'`。
    fn build_lattice(
        &self,
        pinyin: &str,
        bounds: Option<&[usize]>,
    ) -> Result<Vec<Vec<LatticeEdge>>, EngineError> {
        let n = pinyin.len();
        let mut lattice: Vec<Vec<LatticeEdge>> = (0..=n).map(|_| Vec::new()).collect();
        if !pinyin.is_ascii() {
            return Ok(lattice);
        }
        // 造词条目快照:一次查询取一次(Arc 克隆 O(1)),与 LM 打分
        // 使用的用户计数快照同源(同一份 Arc,不同字段)。引擎没挂
        // 用户层时为 None,下面的合并一次都不发生。
        let user_entries = crate::user_lm::user_entries_snapshot(&self.lm);
        let mut cache: HashMap<String, Vec<Candidate>> = HashMap::new();
        for start in 0..n {
            let mut budget = MAX_CHAINS_PER_START;
            let mut chain: Vec<&str> = Vec::new();
            self.collect_chain_edges(
                pinyin, start, start, &mut chain, &mut lattice, &mut cache, &mut budget,
                user_entries.as_ref(), bounds,
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
        user_entries: Option<&crate::user_lm::UserLmSnapshot>,
        bounds: Option<&[usize]>,
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
            // `'` 强制切分点:当前音节 [pos, pos+len) 内部不得含边界
            // (边界落在音节*边界*上是合法的、正是消歧要的切分;落在音节
            // *内部*才非法——如 `xian` 单音节跨 2)。`bounds` 为 None
            // (无 `'` 输入)时此分支一次都不进,与改动前逐字节一致。
            if bounds.is_some_and(|b| b.iter().any(|&x| pos < x && x < pos + len)) {
                continue;
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
                // 造词条目合并(仅精确键):与词典条目同区间同层,
                // 后续打分/排序/截断完全同式。freq 取 0(经既有
                // `freq.max(1)` 兜底拿到词典最低档词频分);排序增益
                // 由 LM 混合层的用户计数给出,不靠词频造假。
                if *fuzzy_penalty == 0.0 {
                    if let Some(snap) = user_entries {
                        for w in snap.lookup_entries(key) {
                            lattice[start].push(LatticeEdge {
                                end,
                                word: w.clone(),
                                pinyin: key.clone(),
                                freq: 0,
                                fuzzy_penalty: 0.0,
                            });
                        }
                    }
                }
            }

            // 深度优先：延长链（下一音节从 `end` 继续）
            self.collect_chain_edges(
                pinyin, start, end, chain, lattice, cache, budget, user_entries, bounds,
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
    #[cfg_attr(not(test), allow(dead_code))] // 只被 decode_paths(测试)调用
    fn decode_beam(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        k: usize,
    ) -> Result<Vec<BeamPath>, EngineError> {
        if pinyin.is_empty() {
            return Ok(Vec::new());
        }
        self.decode_beam_with_lattice(pinyin, ctx, k, &self.build_lattice(pinyin, None)?)
    }

    /// 与 [`Self::decode_beam`] 相同，但格子由调用方传入（分层便于
    /// 把「格构建」与「beam 推进」分开看，性能排查/测试用）。
    fn decode_beam_with_lattice(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        k: usize,
        lattice: &[Vec<LatticeEdge>],
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
            state: self.lm.state_from_context(ctx),
            edge: None,
            parent: None,
        });
        beams[0].push(0);

        // 注:`'` 强制切分点已在 `build_lattice` 的逐音节检查里完全
        // 落实——格里本来就不含任何跨 `'` 的边,beam 只在格上推进,
        // 不需要也不应再做边界过滤(边的合法性由格决定)。
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
            let mut words: Vec<WordSpot> = Vec::new();
            let mut cur = Some(idx);
            while let Some(i) = cur {
                if let Some((start_pos, slot)) = arena[i].edge {
                    let e = &lattice[start_pos][slot];
                    words.push(WordSpot {
                        word: e.word.clone(),
                        pinyin: e.pinyin.clone(),
                    });
                }
                cur = arena[i].parent;
            }
            words.reverse();
            paths.push(BeamPath {
                score: arena[idx].score,
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
    /// 最长可完整切分前缀的整切分词 + 各切法首音节的词；一个音节都切不出来时
    /// 整串前缀联想（`lookup_prefix`）。
    /// 兜底候选没有路径，按旧语义各自从 `ctx` 独立打分。
    ///
    /// A1 不变量在此同样成立(补漏:此前只修了格路径):候选必须起始于
    /// 位置 0——每种切法只查它的**首音节**与整段 `joined`,不再遍历
    /// 所有音节。旧行为把非起始音节(`zhongguor` 切出 `[zhong,gu,o]`
    /// 时的 `gu`/`o`)的词也灌进候选,实测 `zhongguor` 前 20 名里 13 个
    /// 是「只覆盖非起始音节」的词(过/国/果/古/哦……),用户无法有意义地
    /// 选中——与 A1 修掉的「`nihao` 出现好/号/毫」是同一个缺陷。
    /// 多种切法的**首音节可以不同**(`qia` → `[qi,a]` 与 `[qia]`),
    /// 每种切法的首音节都要查(`qi` 与 `qia` 都是合法的位置 0 起点)。
    ///
    /// 注意：这里**不再**把切分残余字母（`nih` 切出 `ni` 后剩下的 `h`）
    /// 当整音节做 `lookup_prefix`——那会把「只解释输入尾部 1~2 个字母的
    /// 词」（所有 h 开头的高频词：哈/哈尔滨/哈哈大笑）灌进候选，还带着
    /// 整词高频优势排在「覆盖整个输入串」的词之前（实测 `nih` 的「你好」
    /// 被挤到第 97 名）。残余字母的正确解释是「后续音节的声母」，
    /// 由简拼层 `lookup_mixed` 处理（`nih` → `ni h*` → 你好），
    /// 见 [`Self::abbrev_candidates`]。下面的整串前缀联想分支仍覆盖
    /// 「首音节一个都没切出来」的场景（如 `zhon` → 中/种/重）。
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
                // A1:只查首音节(覆盖位置 0 的前缀词),不遍历后续音节
                // ——非起始音节的词对当前输入没有意义(见函数级文档)。
                // 多种切法各自的首音节都会查到(循环按切法逐个处理)。
                if let Some(first) = seg.first() {
                    merged.extend(self.dict.lookup(first)?);
                }
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
        sort_candidates(pinyin, &mut ranked);
        ranked.truncate(MAX_CANDIDATES);
        Ok(ranked)
    }
}

/// 候选按最终排序规则排好(u_min 桶 → 层序 → 分数−覆盖罚分 → 同分
/// 频次降序 → 再按词/拼音),保证输出确定性与亲和性(旧实现同分时
/// 顺序任意,依赖 HashMap 序)。排序键的构成与理由见函数内注释。
fn sort_candidates(inp: &str, ranked: &mut [Candidate]) {
    // 排序键 = (u_min 桶, 拼写来源层序, 分数 − 覆盖罚分, 频次降序, ...):
    //
    // **首键「u_min 桶」**:候选终点位置 `pos = cp.len()` 处的「不可
    // 避免未解释字母数」(见 [`unreachable_tail`])——0 ⇔ 输入从
    // pos 起存在一条精确的音节路径到串尾;>0 ⇔ pos 落在一个不可精确
    // 切分的段内(如 `beijing` 的段内切分 `bei jin [g]`、`jinan` 的
    // `jin [an]` 与段内 `ji nan` 起点)。
    //
    // 这个首键修正的是上一轮「裸未解释字母数硬首键」的设计缺陷:裸 u
    // 把「候选拼音长度 == 输入长度」(u=0)的生僻组合词(beijing→
    // 被禁锢/悲寂/背脊,diannao→狄安娜,jinan→进啊)全部顶上第一页,
    // 把高频单字(被/北/点/电/中/种)挤出前 10。这些组合词的拼音
    // **不是输入的合法音节前缀**——共同点是终点落在不可精确切分的
    // 段内(u_min>0);而高频单字(`被`bei、`点`dian)终点在音节
    // 边界上(u_min=0)。u_min 桶把两者分开:段内词一律排在段界词
    // 之后,与词频无关——这是对裸 u 的结构性修正,不是数值调参。
    //
    // 混合简拼目标词(简拼层)构造上覆盖整串、恒 0 桶,与覆盖整串的
    // 精确词、打字中的补全词(`zhon` 时的 `中` cp=zhong 比输入长,
    // u=0)同桶;同桶内层序(第二键)保持「精确 → 模糊 → 简拼」的
    // 结构性优先(防 `xx` 类上千条简拼候选冲垮全拼第一页),简拼
    // 目标词进第一页的通道是配额(`ABBREV_QUOTA`,保留),不是排序。
    //
    // **第三键「分数 − 覆盖罚分」**:桶内把「未解释字母数」折算成
    // `COVERAGE_PENALTY × u_local` 的分数惩罚(u_local = u − u_min,
    // 可免的那部分未解释),与词频、LM 分在同一量纲竞争——系数经
    // 实测标定(见常量注释),让 `zhongguo` 的「中/种」压过段内词,
    // 但不再出现裸 u 硬键的「生僻组合词淹没高频单字」。
    // `inp` 是排序时的查询串:可能含 B2 的 `'`(会话层把原始输入串直接
    // 传进来)。排序首键 u_min 桶要与格构建/beam/后向 DP 用**同一份**
    // 「不许跨 `'`」的可达性,故在这里把 `'` 归一化成边界表(与
    // `lookup_candidates` 开头的归一化同式);无 `'` 时 `bounds` 为
    // `None`,`unreachable_tail` 走原有零开销路径。
    let (inp_clean, bounds_vec): (std::borrow::Cow<'_, str>, Vec<usize>) =
        if inp.contains('\'') {
            let mut clean = String::with_capacity(inp.len());
            let mut bounds = Vec::new();
            for ch in inp.chars() {
                if ch == '\'' {
                    bounds.push(clean.len());
                } else {
                    clean.push(ch);
                }
            }
            (std::borrow::Cow::Owned(clean), bounds)
        } else {
            (std::borrow::Cow::Borrowed(inp), Vec::new())
        };
    let inp: &str = &inp_clean;
    let bounds: Option<&[usize]> = if bounds_vec.is_empty() {
        None
    } else {
        Some(&bounds_vec)
    };
    let umin = unreachable_tail(inp, bounds);
    // 预计算每个候选的数值键:`sort_by` 每次比较调两次 `sort_key`,
    // 而 `sort_key` 里的 `unexplained_letters` / `cp_len` 都要对
    // `c.pinyin` 做一次 chars 过滤(O(拼音长));500 条候选的简拼层
    // 排序 ~4500 次比较 × 2 键 × 2 次过滤,release 下实测 zhongg
    // 从 0.99ms 涨到 1.23ms。预算成 (bucket, 罚后分) 的 PlainKey
    // 后比较是 O(1),延迟回到 0.99ms 量级。
    // 键预算 + 克隆重排:`sort_by` 每次比较调两次 `sort_key`,而
    // `sort_key` 里的 `unexplained_letters` / `cp_len` 都要对
    // `c.pinyin` 做一次 chars 过滤;500 条候选的简拼层排序
    // ~4500 次比较 × 2 键 × 2 次过滤,release 下实测 zhongg 从
    // 0.99ms 涨到 1.23ms。预算成 PlainKey 后比较是 O(1),延迟
    // 回到 1.10ms(克隆 500 条候选的代价比反复重算键小)。
    let mut keyed: Vec<(PlainKey, Candidate)> = ranked
        .iter()
        .map(|c| (plain_key(inp, &umin, c), c.clone()))
        .collect();
    keyed.sort_by(|(ka, a), (kb, b)| {
        ka.cmp(kb).then_with(|| {
            // 数值键并列时的确定性收尾,与旧 `sort_key` 尾部字段一致
            b.freq
                .cmp(&a.freq)
                .then_with(|| a.word.cmp(&b.word))
                .then_with(|| a.pinyin.cmp(&b.pinyin))
        })
    });
    ranked.clone_from_slice(&keyed.into_iter().map(|(_, c)| c).collect::<Vec<_>>());
}

/// `sort_key` 的数值前缀(bucket + 罚后分),供 `sort_candidates`
/// 预计算——语义与 `sort_key` 的前三键完全一致,只是抽成可 Clone
/// 的独立值,排序时不再反复重算。
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PlainKey {
    /// 首键:u_min 桶(见 [`sort_candidates`])。
    bucket: usize,
    /// 第二键:拼写来源层序(Exact < Fuzzy < Abbrev)。
    spelling: SpellingKind,
    /// 第三键:分数 − 覆盖罚分,降序。
    score: ScoreDesc,
}

/// 从候选算出数值排序键(与 `sort_key` 的前三键同式)。
fn plain_key(inp: &str, umin: &[usize], c: &Candidate) -> PlainKey {
    let k = sort_key(inp, umin, c);
    PlainKey {
        bucket: k.bucket,
        spelling: k.spelling,
        score: k.score,
    }
}

/// 覆盖罚分系数(log10 量纲):「愿意为多解释一个可免字母付出多少个
/// 数量级的词频劣势」。`log10(freq/total)` 每数量级差 1.0,故系数
/// 0.2 表示「少解释 5 个可免字母 ≈ 词频低 1 个数量级」。
///
/// 标定(sweep 实测,真实词库 + 万象 gram,逐系数表见交付报告):
/// 系数只管 0 桶内「全拼整串词」与「高 u_local 单字」之间的竞争
/// (u_min>0 桶的候选 u_local=0 不吃罚,段内字靠桶序被压,与系数
/// 无关)。窗口 (0, ~0.25],取 0.2——
/// - 上限:`diannao` 的狄安娜(u_local=1,与「店」原始分差 2.78
///   log)在系数 0.3 时爬进前 10,0.25 及以内不在;`zhongguo` 的
///   钟鼓在 1.0 时才进前 10,余量更大。瓶颈是 diannao;
/// - 下限:混合简拼的段内碎片字(renm 的「人」、zhongguor 的「中」)
///   u_local=0 恒不吃罚,目标词靠 u_min 桶保护,与系数无关——
///   系数即使取 0,B 组也不退化(实测 0.1 时 B 组名次不变);
/// - 0.2 离翻转点 0.25 有 20% 余量,且 `woshizhongguoren` 类长串
///   的前缀词(「我始终」u_local≈17)吃 3.4 log 罚分正常沉底——
///   那本来就是它们该在的位置。
const COVERAGE_PENALTY: f32 = 0.2;

/// 候选的完整排序键,供 [`sort_candidates`] 与 [`merge_with_abbrev`]
/// 的配额位置换算共用(两处用同一个函数,键定义不可能漂移)。
/// 各字段含义见 `sort_candidates` 函数内注释。分数字段包成
/// [`ScoreDesc`] 表达「降序」语义(高分排前),与旧实现的
/// `b.score.partial_cmp(&a.score)` 等价;引擎分数域不会出现 NaN
/// (打分是 log10(正数) 的加权和),NaN 在 [`ScoreDesc`] 里钳到最末,
/// 与旧实现 `unwrap_or(Ordering::Equal)` 的兜底最接近。
fn sort_key<'a>(inp: &str, umin: &[usize], c: &'a Candidate) -> SortKey<'a> {
    let u = unexplained_letters(inp, c);
    // u_min(终点处的不可避免未解释数):补全词(cp 比输入长)必以
    // 输入开头(u=0),桶恒 0;简拼层构造上覆盖整串,恒 0;只覆盖
    // 前缀的候选查表。
    let cp_len = c.pinyin.chars().filter(|ch| *ch != ' ').count();
    let u_min = if c.spelling == SpellingKind::Abbrev || cp_len > inp.len() {
        0
    } else {
        umin[cp_len]
    };
    let u_local = u.saturating_sub(u_min) as f32;
    SortKey {
        bucket: u_min,
        spelling: c.spelling,
        score: ScoreDesc(c.score - COVERAGE_PENALTY * u_local),
        freq: std::cmp::Reverse(c.freq),
        word: c.word.as_str(),
        pinyin: c.pinyin.as_str(),
    }
}

/// [`sort_key`] 的返回类型:具名字段的全序键(派生 `Ord` 即按声明
/// 序的字典序,字段声明顺序就是键优先级)。
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SortKey<'a> {
    /// 首键:候选终点处的不可避免未解释字母数(u_min 桶,越小越靠前)。
    bucket: usize,
    /// 第二键:拼写来源层序(Exact < Fuzzy < Abbrev,枚举声明序)。
    spelling: SpellingKind,
    /// 第三键:分数 − 覆盖罚分,降序(高分在前,见 [`ScoreDesc`])。
    score: ScoreDesc,
    /// 第四键:同分时原始词频降序(回退基准)。
    freq: std::cmp::Reverse<u64>,
    /// 第五、六键:词与拼音的字典序(确定性收尾)。
    word: &'a str,
    pinyin: &'a str,
}

/// `umin[pos]` = 输入从 `pos` 起的**不可避免未解释字母数**:
/// 0 ⇔ 存在一条从 pos 到串尾的精确音节路径(只查音节表,不查词典);
/// >0 ⇔ pos 落在某个不可精确切分段内,取到「最近一个可达点」的距离。
///
/// 后向一次 DP 算出可达性,再正向把不可达位置映射到距离;返回表长
/// `inp.len() + 1`,索引即字节位置(调用方已保证 `inp` 为 ASCII)。
fn unreachable_tail(inp: &str, bounds: Option<&[usize]>) -> Vec<usize> {
    let n = inp.len();
    let mut reach = vec![false; n + 1];
    reach[n] = true;
    // 直接查编译期静态音节表(`SYLLABLES` 已按字典序排列,binary_search
    // O(log 412));不要在这里 `Segmenter::new()`——它每次要把 412 个
    // 音节 collect 进 HashSet,而本函数每次 `sort_candidates` 调用都
    // 跑一次(一次查询几十次),实测会把击键延迟放大 7~50 倍
    // (lookup_bench:womenjintianquxian 1.5ms→10.2ms)。
    //
    // `bounds` 是 B2 的 `'` 强制切分点(干净串字节偏移):音节链不得跨越
    // 它(与 `build_lattice` 同一语义)。长度为 l 的音节 [i, i+l) 若跨过
    // 某个边界(边界严格落在 (i, i+l) 内部)就跳过该链——`xi'an`(干净串
    // `xian`,边界 2)的 `xian` 单音节链由此不可达,[xi, an] 才是唯一的
    // 整串切法。`bounds` 为 `None`(无 `'` 输入)时此分支一次都不进,与
    // 改动前逐字节一致(判退化的硬约束)。
    for i in (0..n).rev() {
        let max_len = (n - i).min(segmenter::MAX_SYLLABLE_LEN);
        reach[i] = (1..=max_len).any(|l| {
            if !reach[i + l] {
                return false;
            }
            if let Some(b) = bounds {
                // 跨过 ⇔ 存在边界严格落在 (i, i+l) 内部。
                if b.iter().any(|&x| i < x && x < i + l) {
                    return false;
                }
            }
            SYLLABLES.binary_search(&&inp[i..i + l]).is_ok()
        });
    }
    (0..=n)
        .map(|pos| {
            if reach[pos] {
                0
            } else {
                let mut d = 1;
                while pos + d <= n && !reach[pos + d] {
                    d += 1;
                }
                d.min(n - pos)
            }
        })
        .collect()
}

/// `f32` 分数的「降序 + 全序」包装:高分排前;NaN 钳到最末
/// (引擎分数域不会出现 NaN,这只是把 `partial_cmp` 补齐为 `Ord`
/// 的防御,语义同旧实现的 `unwrap_or(Equal)` 兜底)。
#[derive(Debug, Clone, Copy, PartialEq)]
struct ScoreDesc(f32);
impl Eq for ScoreDesc {}
impl PartialOrd for ScoreDesc {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ScoreDesc {
    fn cmp(&self, other: &Self) -> Ordering {
        // 降序:自右向左比较(other vs self)。
        other
            .0
            .partial_cmp(&self.0)
            .unwrap_or_else(|| match (other.0.is_nan(), self.0.is_nan()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater, // 对方 NaN ⇒ 对方排更后 ⇒ self 更小
                (false, true) => Ordering::Less,    // self NaN ⇒ self 排更后
                (false, false) => unreachable!("partial_cmp 只对 NaN 返回 None"),
            })
    }
}

/// 候选**未解释的输入字母数**(覆盖度度量;**不再是排序首键**——
/// 首键已改为 [`unreachable_tail`] 的 u_min 桶,本函数的返回值只在
/// 罚分项 `u_local = u − u_min` 里参与打分,并供测试引用)。
///
/// 语义保留不变(任务书硬约束),变的只是用法:从「排序键」改成
/// 「罚分因子」。
///
/// 设 `cp` = 候选拼音去掉空格后的音节串:
/// - `cp` 以 `inp` 开头(含相等):候选是整个输入的**补全**(如 `zhon`
///   时的 `zhong` 系字)——没解释的不是「没覆盖」而是「输入还没打完」,
///   计 **0**,保持联想靠前;
/// - `inp` 以 `cp` 开头(真前缀):候选**只覆盖前缀**(如 `zhongguor`
///   时的 `zhong` 系字),计 `inp.len() - cp.len()`(漏掉多少字母);
/// - 都不是前缀关系(兜底路径的异常键、个别模糊变体):解释数按
///   保守下界 0 计(与补全同档)。理由:够不到「候选真前缀于输入」
///   这条下界时,无法证明候选漏掉了输入的任何字母,零罚最稳;
///   且这类候选在正常路径不出现(`lattice[0]` 的边必然前缀匹配),
///   在兜底路径按分数竞争时本就在补全词之后,极少浮到前排。
/// - 简拼层候选构造上覆盖整串(`lookup_abbrev` / `lookup_mixed` 的
///   key 由整串切出),恒为 **0**——不走拼音比对,避免「简拼词的
///   完整拼音与残余声母输入天然不是前缀关系」被误伤(如 `zhongguor`
///   的「中国人」`cp` = zhongguoren,与 inp 非前缀关系)。
///
/// 字节序比较合法:`inp` 与 `cp` 都是 ASCII 拼音串(非 ASCII 输入在
/// `build_lattice` / `fallback_candidates` 已被挡掉),前缀关系按字节
/// 判定即按字母判定。
fn unexplained_letters(inp: &str, c: &Candidate) -> usize {
    if c.spelling == SpellingKind::Abbrev {
        return 0; // 简拼层构造上覆盖整串
    }
    let cp: String = c.pinyin.chars().filter(|ch| *ch != ' ').collect();
    if cp.starts_with(inp) {
        0 // 补全:候选拼音比输入更长(或相等),输入还没打完
    } else if cp.len() < inp.len() && inp.starts_with(cp.as_str()) {
        inp.len() - cp.len() // 只覆盖前缀:漏掉 inp[cp.len()..]
    } else {
        0 // 非前缀关系:解释数取保守下界 0(见函数级注释)
    }
}

/// 合并简拼层并按 [`MAX_CANDIDATES`] 收尾,给简拼层留
/// [`ABBREV_QUOTA`] 个保底名额。
///
/// 结构不变(配额语义逐字保留):`ranked`(head,非简拼)与 `abbrev`
/// 仍各自排序、各自截取、再按序拼接——截取边界是「head 允许的长度」,
/// 与新排序键无关,因此配额算法不需要知道简拼候选在全局排第几。
/// 变化只在拼接之后:对拼接结果**整体重排一次**
/// [`sort_candidates`](排序首键是 u_min 桶:简拼层恒 0 桶,与同桶的
/// 精确补全/全覆盖候选按层序、罚后分混排——「中国人」(0 桶) 由此
/// 能排在只覆盖 `zhong` 的单字(段内,u_min>0 桶)之前,这正是本次
/// 要修的行为;排序是稳定全序,重排不打乱两层内部各自的相对次序)。
/// 设简拼层 `n` 条:
/// - 总条数 ≤ `MAX_CANDIDATES` 时两层原样拼接,不截断;
/// - 总条数超限时,简拼层取 `max(min(ABBREV_QUOTA, n), 100 − head 条数)`
///   条——保底 ABBREV_QUOTA 条,但不封顶:head 很短时简拼层照旧
///   补满 100(head ≤ 100 时与旧「合并截 100」逐位一致,全拼无退化);
/// - head 截到 `100 − 简拼层实取条数`(`n = 0` 时照旧用满 100,
///   不白留空位)。
///
/// 配额只影响「谁被截断」,不影响排序。返回值 = (合并后总条数,
/// 截断并重排后的列表);`abbrev` 必须未经截断,总条数才真实
/// (供断言「简拼层装满时被配额截断」——截断后的前 5 条分不出
/// 「恰好 5 条」与「被截到 5 条」)。
fn merge_with_abbrev(
    inp: &str,
    mut ranked: Vec<Candidate>,
    abbrev: Vec<Candidate>,
) -> (usize, Vec<Candidate>) {
    let abbrev_total = abbrev.len();
    ranked.extend(abbrev);
    let total = ranked.len();
    // 简拼层在「排序前」的 ranked 中是尾部 abbrev_total 条,先切出来
    // 各自排序截断——配额语义只在「分层截取」上有边界可依附。
    let mut head = ranked;
    let mut tail = head.split_off(total - abbrev_total);
    sort_candidates(inp, &mut head);
    sort_candidates(inp, &mut tail);
    if total > MAX_CANDIDATES {
        // 只有总条数超上限才需要截断。配额是「保底名额」而非「简拼层
        // 封顶」:简拼层取「剩余空位」与配额中较大的那个——
        // - head 装满(≥100−配额)时:空位 ≈ 0,配额生效,简拼层保底
        //   ABBREV_QUOTA 条(本任务要救的场景:zhongguor 等);
        // - head 很短(如 ren 只有 21 条)时:空位有 79 条,简拼层照旧
        //   填满到 100——与旧「合并排序截 100」行为逐位一致,不退化
        //   (head ≤ 100 时旧行为等价于「先取 head 全量、再用简拼层
        //   补满 100」,与这里逐位相同)。
        let tail_keep = ABBREV_QUOTA
            .min(tail.len())
            .max(MAX_CANDIDATES.saturating_sub(head.len()));
        tail.truncate(tail_keep);
        head.truncate(MAX_CANDIDATES - tail_keep);
    }
    head.extend(tail);
    // 拼接后整体重排:让排序首键(u_min 桶)跨层生效(见函数级
    // 注释)。sort_by 是稳定排序,重排不改变同键候选的相对次序。
    sort_candidates(inp, &mut head);
    (total, head)
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

    /// 测试内的排序首键(与 [`sort_key`] 的 bucket 字段同式):
    /// 候选终点处的不可避免未解释字母数(u_min 桶)。
    /// 断言「(bucket, 层序) 整体非降」比旧的「(未解释字母数, 层序)
    /// 非降」更贴近排序键本身——bucket 才是首键;`unexplained_letters`
    /// 只是罚分因子,不再是键。
    fn sort_bucket(inp: &str, c: &Candidate) -> usize {
        if c.spelling == SpellingKind::Abbrev {
            return 0;
        }
        let cp_len = c.pinyin.chars().filter(|ch| *ch != ' ').count();
        if cp_len > inp.len() {
            0 // 补全词必以输入开头,u=0,桶恒 0
        } else {
            unreachable_tail(inp, None)[cp_len]
        }
    }

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
        // 打字打到一半(如 "nih"):整串还切不完整,但候选不能消失——
        // 应按"已切分前缀"继续给候选(对应候选窗闪一下就没的 bug)。
        // 残余字母("nih" 切出 "ni" 后剩下的 "h")不再当整音节做前缀联想
        // (旧行为会把所有 h 开头的高频词灌进候选,见 fallback_candidates
        // 函数级文档);"ni + h 开头音节"的词由简拼层 lookup_mixed 负责
        // (玩具词库无副索引,这里只验证兜底层不再产出 h 开头的词)。
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
            cands.iter().all(|c| c.word != "哈"),
            "残余字母 h 不应再当整音节联想出「哈」(它只解释输入尾部 1 个字母): {:?}",
            cands.iter().map(|c| &c.word).collect::<Vec<_>>()
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

    /// B2 消歧(真实词库):`xi'an` 的候选里**不得**含只能由 `[xian]`
    /// 单音节切法产生的高频字(先/现/显/线/险/仙……),且「西安」必须
    /// 出现且名次显著前移。玩具词库里只有两三个词,挡不住「`'` 被静默
    /// 删掉、`[xian]` 原样复活」这类问题——真实词库下 `xian` 有 100 个
    /// 候选,这些字是否消失才看得出来。缺 `data/dict.bin` 时跳过。
    #[test]
    fn real_dict_apostrophe_disambiguates_xian() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
        if !path.is_file() {
            return;
        }
        let engine = ImeEngine::new(Some(&path)).expect("mmap 词库加载失败");

        // 对照组:不带 `'` 的 `xian` 必须**有**这些单音节字(消歧的前提)。
        let plain = engine
            .lookup_candidates("xian", &LmContext::empty())
            .expect("查询失败");
        assert!(
            plain.iter().any(|c| c.word == "先" && c.pinyin == "xian"),
            "对照组 `xian` 应含「先」(否则本测试的消歧断言失去意义)"
        );

        // 实验组:`xi'an` 不得召回任何只能由 `[xian]` 切出的字。
        let cands = engine
            .lookup_candidates("xi'an", &LmContext::empty())
            .expect("查询失败");
        for w in ["先", "现", "显", "线", "险", "仙"] {
            assert!(
                !cands.iter().any(|c| c.word == w && c.pinyin == "xian"),
                "xi'an 不应再召回只能由 `[xian]` 切出的「{w}」(`'` 未生效回归): 前 10 {:?}",
                cands.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()
            );
        }
        // 「西安」必须在,且名次显著前移(对照组第 18 名;消歧后候选池
        // 只剩 [xi|an] 系,应在第一页前列)。不设「必须第 1」硬指标——
        // 名次由数据决定,但绝不应再被 [xian] 的高频单字压在 18 名。
        let rank = cands
            .iter()
            .position(|c| c.word == "西安")
            .unwrap_or_else(|| panic!("xi'an 应含「西安」: {:?}", cands.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()));
        assert!(
            rank < 10,
            "xi'an 的「西安」应显著前移(对照组第 18 名),实际第 {} 名: 前 10 {:?}",
            rank + 1,
            cands.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()
        );
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
        // 单字候选仍要保留(用户可能只想上屏一个字)。
        // Block A 起候选只含起始于位置 0 的词:「你」(ni,位置 0)保留;
        // 「好」只覆盖后段音节 hao(位置 2),不再作为候选出现——
        // 输入 nihao 时用户无法有意义地选中「好」(语义同 librime:
        // 其同输入候选全部起始于位置 0)。
        assert!(cands.iter().any(|c| c.word == "你"), "单字「你」应在候选里");
        assert!(
            !cands.iter().any(|c| c.word == "好"),
            "只覆盖后段音节的「好」不应出现在 nihao 的候选里: {cands:?}"
        );
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

        // 可见面:候选排序由「以该候选开头的最优完整路径分」决定
        // (Block A)。「泥」的候选分 = 泥自身 + 补全「门」,「你」的
        // 候选分 = 你自身 + 补全「门」;两者后缀相同,而补全分由后向
        // DP 在无上文 LM 状态下近似(**看不到**「泥→门」的 bigram,
        // 只能拿「门」的 unigram 分——近似是刻意的,见
        // `backward_completion` 注释)。因此候选排序不再继承 beam 的
        // bigram 增益:「你」(unigram 100)排在「泥」(unigram 10)前。
        // 整句首选仍由前向 beam 的精确 bigram 状态决定(上面的
        // beam[0] == [泥,门]),两条口径互不干扰。
        let cands = engine
            .lookup_candidates("nimen", &ctx)
            .expect("查询失败");
        assert_eq!(
            cands[0].word, "你",
            "候选排序用近似 LM(无 bigram 增益):unigram 更高的「你」应排最前"
        );
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
            return; // 缺 .gram 时静默跳过(与其它真实文件测试同款)
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
        assert_eq!(cands[pl].spelling, SpellingKind::Exact, "蓝是精确匹配");
        assert_eq!(cands[pr].spelling, SpellingKind::Fuzzy, "然是模糊匹配(r/l)");

        // 层间边界:所有精确候选都在所有模糊候选之前
        let first_fuzzy = cands.iter().position(|c| c.spelling == SpellingKind::Fuzzy);
        if let Some(ff) = first_fuzzy {
            assert!(
                cands[..ff].iter().all(|c| c.spelling == SpellingKind::Exact),
                "精确层内不应混入模糊/简拼候选: {cands:?}"
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
        assert_eq!(cands[ph].spelling, SpellingKind::Fuzzy);
        assert_eq!(cands[pl].spelling, SpellingKind::Fuzzy);
        assert_eq!(cands[pe].spelling, SpellingKind::Exact);
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
        assert_eq!(pure.spelling, SpellingKind::Exact,
            "纯精确边应在精确层: {cands:?}");
        assert_eq!(mixed.spelling, SpellingKind::Fuzzy,
            "该候选由模糊边到达,应归模糊层: {cands:?}");

        // 混合路径词频再高(10000 vs 50)也得排在精确层之后
        let pos = |word: &str| cands.iter().position(|c| c.word == word);
        assert!(pos("那哈") < pos("拉哈"), "精确层整体优先: {cands:?}");
        // Block A 起候选只含起始于位置 0 的词:「哈」只覆盖后段音节
        // ha(位置 2),不再作为候选出现;也就不再有「继承所在路径的
        // 模糊层归属」的问题(那是旧实现「候选取自路径所有词位」的
        // 连带语义)。
        assert!(
            !cands.iter().any(|c| c.word == "哈"),
            "只覆盖后段音节的「哈」不应出现在 naha 的候选里: {cands:?}"
        );
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
        assert!(off.iter().all(|c| c.spelling == SpellingKind::Exact),
            "关 fuzzy 时应全是精确层");
        let off_words: Vec<&str> = off.iter().map(|c| c.word.as_str()).collect();
        assert_eq!(off_words, ["你", "妮", "泥", "逆"], "词频降序: {off:?}");

        // 开 fuzzy:精确层前缀逐位相同,模糊层追加在后
        let on_exact: Vec<&str> = on.iter()
            .filter(|c| c.spelling == SpellingKind::Exact)
            .map(|c| c.word.as_str())
            .collect();
        assert_eq!(on_exact, off_words, "精确层应与关 fuzzy 时逐位一致");
        let on_fuzzy: Vec<&str> = on.iter()
            .filter(|c| c.spelling == SpellingKind::Fuzzy)
            .map(|c| c.word.as_str())
            .collect();
        assert_eq!(on_fuzzy, ["里"], "模糊层只应有「里」: {on:?}");
    }

    /// 回声候选:词库里查不到的输入(整串无法解码、兜底联想也为空),
    /// `lookup_candidates` 应返回**恰好 1 个**回声候选,word == 输入串,
    /// echo == true——保证公开 API 的后置条件「非空输入 ⇒ 候选非空」。
    #[test]
    fn unresolvable_input_yields_single_echo_candidate() {
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        // 空词库:任何输入都解不出候选
        let cands = engine
            .lookup_candidates("zgr", &LmContext::empty())
            .expect("查询失败");
        assert_eq!(cands.len(), 1, "查不到时应恰好 1 个回声候选: {cands:?}");
        assert_eq!(cands[0].word, "zgr", "回声候选的 word 应为原始输入串");
        assert_eq!(cands[0].pinyin, "zgr");
        assert!(cands[0].echo, "应标记为回声候选");
        assert_eq!(cands[0].freq, 0);
    }

    /// 回声候选绝不污染正常候选列表:词库里有的词,结果里不应有
    /// 任何 echo == true 的候选(语义同 librime:别人有候选时回声不冒头)。
    #[test]
    fn resolvable_input_has_no_echo_candidate() {
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("你", "ni", 100).expect("插入失败");
        engine.dict.upsert("你好", "ni hao", 200).expect("插入失败");

        let cands = engine
            .lookup_candidates("ni", &LmContext::empty())
            .expect("查询失败");
        assert!(!cands.is_empty(), "词库命中时应有候选");
        assert!(
            cands.iter().all(|c| !c.echo),
            "正常有候选时不应混入回声候选: {cands:?}"
        );
    }

    /// Block A1:候选必须全部起始于位置 0——只覆盖后段音节的词
    /// 不该出现在候选里(用户无法有意义地选中它们)。
    ///
    /// 构造:输入 `nihao`,词库有「好」(hao,仅覆盖第二音节)。新语义
    /// 下它不应出现;「你好」(覆盖整串)与「你」(覆盖首音节)应出现。
    /// 对照 librime:同输入的候选全部起始于位置 0。
    #[test]
    fn candidates_all_start_at_position_zero() {
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("你好", "ni hao", 100).expect("插入失败");
        engine.dict.upsert("你", "ni", 200).expect("插入失败");
        engine.dict.upsert("好", "hao", 150).expect("插入失败");
        engine.dict.upsert("号", "hao", 120).expect("插入失败");
        engine.dict.upsert("毫", "hao", 80).expect("插入失败");

        let cands = engine
            .lookup_candidates("nihao", &LmContext::empty())
            .expect("查询失败");
        assert!(
            cands.iter().any(|c| c.word == "你好"),
            "覆盖整串的「你好」应在候选里: {cands:?}"
        );
        assert!(
            cands.iter().any(|c| c.word == "你"),
            "覆盖首音节的「你」应在候选里: {cands:?}"
        );
        for w in ["好", "号", "毫"] {
            assert!(
                !cands.iter().any(|c| c.word == w),
                "只覆盖后段音节 hao 的「{w}」不应出现在 nihao 的候选里: {cands:?}"
            );
        }
        // 不变量:所有候选的拼音都必须以首音节 ni 开头
        // (即对应 lattice[0] 上的边)。
        assert!(
            cands
                .iter()
                .all(|c| c.pinyin == "ni" || c.pinyin.starts_with("ni ")),
            "所有候选都应起始于位置 0(拼音以 ni 开头): {cands:?}"
        );
    }

    /// Block A1 不变量在**兜底路径**同样成立(补漏回归):整串尚不能
    /// 完整解码时(如 `zhongguor`、`qiax`),`fallback_candidates` 曾经
    /// 遍历每种切法的**所有**音节查词,把只覆盖非起始音节的词
    /// (`zhongguor` 切出 `[zhong,gu,o]` 时 `gu` 的「古」、`o` 的「哦」)
    /// 灌进候选——与 A1 在格路径修掉的缺陷同源。
    ///
    /// 判定逻辑(不硬编码字表):候选拼音(空格分隔音节串)的**首音节**
    /// 必须属于「最长可完整切分前缀」的某种切法的首音节集合——即存在
    /// 一种从位置 0 出发的音节切法以它开头。这是「候选起始于位置 0」
    /// 的直接判据:首音节不从位置 0 起头的词(过 guo / 古 gu / 哦 o)
    /// 不可能由任何首段切法产出;而首音节不同的多条切法
    /// (`qiax` 的 `[qi,a]` 与 `[qia]`)各自的首音节都合法。
    #[test]
    fn fallback_candidates_all_start_at_position_zero() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
        let dict_path = root.join("dict.bin");
        let abbrev_path = root.join("abbrev.bin");
        if !dict_path.is_file() {
            return; // 环境缺词库:跳过
        }
        let mut engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
        if abbrev_path.is_file() {
            engine.dict = engine.dict.with_abbrev(&abbrev_path);
        }

        // 各输入的最长可切前缀切法由引擎同款 Segmenter 现算,不硬编码:
        // zhongguor → 前缀 zhongguo:[zhong,gu,o] / [zhong,guo]
        // qiax      → 前缀 qia     :[qi,a] / [qia]
        let expected: [(&str, &[&str]); 2] = [
            ("zhongguor", &["zhong"]),
            ("qiax", &["qi", "qia"]),
        ];
        for (input, want_firsts) in expected {
            let k = (1..=input.len())
                .rev()
                .find(|&k| !engine.segmenter.segment(&input[..k]).is_empty())
                .expect("这两个输入都应有可切前缀");
            let allowed_firsts: std::collections::HashSet<String> = engine
                .segmenter
                .segment(&input[..k])
                .iter()
                .filter_map(|seg| seg.first().cloned())
                .collect();
            let want: std::collections::HashSet<String> =
                want_firsts.iter().map(|s| s.to_string()).collect();
            assert_eq!(
                allowed_firsts, want,
                "{input} 的首音节集合(判据的前提)与预期不符"
            );

            let cands = engine
                .lookup_candidates(input, &LmContext::empty())
                .expect("查询失败");
            assert!(!cands.is_empty(), "{input} 不应没有候选");
            for c in &cands {
                if c.echo || c.spelling == SpellingKind::Abbrev {
                    // 回声候选 word=输入串,无音节结构;简拼候选覆盖整串、
                    // 其 pinyin 是完整词拼音、天然从位置 0 起,均豁免。
                    continue;
                }
                let first_syl = c.pinyin.split(' ').next().expect("候选拼音非空");
                assert!(
                    allowed_firsts.contains(first_syl),
                    "{input} 的候选「{}」(拼音 {})首音节 {first_syl} 不在合法首音节集合                      {allowed_firsts:?} 里——它只覆盖非起始音节(A1 兜底路径回归)",
                    c.word, c.pinyin
                );
            }
        }
    }

    /// Block A2:候选数不再被 `BEAM_WIDTH` 钳死——候选来源是
    /// `lattice[0]` 全量,而非 beam 的幸存路径。
    ///
    /// 构造:单音节 `ni` 的词库里放 20 个同音字。旧实现最多返回
    /// `BEAM_WIDTH = 16` 个;新实现应返回全部 20 个。
    #[test]
    fn candidate_count_not_clamped_by_beam_width() {
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        // 20 个不同的同音字,词频递减以保证确定性排序
        let words: Vec<String> = (0..20u32)
            .map(|i| char::from_u32(0x4E00 + i).unwrap().to_string())
            .collect();
        for (i, w) in words.iter().enumerate() {
            engine
                .dict
                .upsert(w, "ni", 1000 - i as u64)
                .expect("插入失败");
        }

        let cands = engine
            .lookup_candidates("ni", &LmContext::empty())
            .expect("查询失败");
        assert!(
            cands.len() > BEAM_WIDTH,
            "候选数 {} 应大于 BEAM_WIDTH={BEAM_WIDTH}(不再被 beam 钳死)",
            cands.len()
        );
        assert_eq!(cands.len(), 20, "20 个同音字应全部成为候选: {cands:?}");
    }

    /// Block A3:打分口径——「覆盖整串的词」应排在「只覆盖前缀的词」
    /// 之前,即使后者自身词频更高(覆盖整串只需一次词频+LM 打分,
    /// 前缀词还需补全剩余音节,多付一次词频+LM 代价)。
    ///
    /// 构造:输入 `nihao`,「你」(ni, freq=200)自身词频高于「你好」
    /// (ni hao, freq=100),但「你好」覆盖整串,「你」还需补全 hao,
    /// 故「你好」总分应更高、排在前面。
    #[test]
    fn full_coverage_word_outranks_prefix_word() {
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("你", "ni", 200).expect("插入失败");
        engine.dict.upsert("好", "hao", 150).expect("插入失败");
        engine.dict.upsert("你好", "ni hao", 100).expect("插入失败");

        let cands = engine
            .lookup_candidates("nihao", &LmContext::empty())
            .expect("查询失败");
        let pos = |word: &str| cands.iter().position(|c| c.word == word);
        let (p_full, p_prefix) = (
            pos("你好").expect("「你好」应在候选里"),
            pos("你").expect("「你」应在候选里"),
        );
        assert!(
            p_full < p_prefix,
            "覆盖整串的「你好」应排在只覆盖前缀的「你」之前: {cands:?}"
        );
        assert!(
            cands[p_full].score > cands[p_prefix].score,
            "「你好」分 {} 应高于「你」分 {}",
            cands[p_full].score,
            cands[p_prefix].score
        );
    }

    /// B4 分层(真实词库):排序键 = (u_min 桶, 层序, 分数 − 覆盖罚分)。
    /// 输入 `beijdx`:兜底联想(`beijing` 等 bei-j* 前缀词)给精确层
    /// (几十条,装不满 MAX_CANDIDATES),`lookup_abbrev("beijdx")` 命中
    /// 「北京」类 b-j-d-x 词给简拼层。用真实词库而非玩具词库:玩具词库
    /// 查不出「高频简拼词压过精确词」。
    ///
    /// 三段式(首键从「裸未解释字母数」改为「u_min 桶」后改写):
    /// - 原断言:「同覆盖度(未解释字母数相同)内层序不交错 + 未解释
    ///   字母数整体非降」——裸 u 当首键时的排序正确性断言;
    /// - 改为:**同 u_min 桶内**层序不交错 + u_min 桶整体非降。裸 u
    ///   让「拼音长度==输入长度」的生僻组合词(被禁锢/狄安娜,u=0)
    ///   压过高频单字(被/点,u=4);u_min 桶把「终点落在不可精确切分
    ///   段内」的候选(被禁锢 bei jin [g]、进啊 jin [an])与「终点在
    ///   音节边界」的候选(被 bei、点 dian)分开,段内词一律沉底——
    ///   这是对裸 u 的结构性修正,不是放松:同桶内「精确 > 模糊 >
    ///   简拼」的层序保证原样钉住(桶即新首键);
    /// - 为什么新行为对:裸 u 只看「解释了几个字母」,不看「剩下的
    ///   字母能不能精确切分」——被禁锢(u=0)与被(u=4)在裸 u 下
    ///   分属 0 键与 4 键,被禁锢合法地排在被之前;但 beijing 从
    ///   bei jin 起剩下的 g 不可精确切分(u_min=1),而从 bei 起
    ///   剩下的 jing 可以(u_min=0)——u_min 桶把这个结构差异变成
    ///   首键,段内组合词因此不再凭「拼音凑巧等长」浮上第一页。
    #[test]
    fn real_dict_abbrev_layer_never_outranks_exact() {
        let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
        let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
        if !dict_path.is_file() || !abbrev_path.is_file() {
            return;
        }
        let mut engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
        engine.dict = engine.dict.with_abbrev(&abbrev_path);
        assert!(engine.dict.has_abbrev(), "副索引应已装配");

        let cands = engine
            .lookup_candidates("beijdx", &LmContext::empty())
            .expect("查询失败");
        assert!(
            cands.iter().any(|c| c.spelling == SpellingKind::Exact),
            "beijdx 应有精确层候选(兜底联想 bei-j* 前缀): {:?}",
            cands.iter().take(5).map(|c| (&c.word, c.spelling)).collect::<Vec<_>>()
        );
        assert!(
            cands.iter().any(|c| c.spelling == SpellingKind::Abbrev),
            "beijdx 应有简拼层候选(b-j-d-x 简拼词): {:?}",
            cands.iter().map(|c| (&c.word, c.spelling)).collect::<Vec<_>>()
        );
        // 同桶内层间不交错:按 u_min 桶分组,组内层序整体非降。
        // (跨组允许简拼层在精确层之前——0 桶简拼词排在 3 桶精确字之前
        // 是首键的预期效果。)
        let keys: Vec<(usize, SpellingKind)> = cands
            .iter()
            .map(|c| (sort_bucket("beijdx", c), c.spelling))
            .collect();
        let mut by_cov: HashMap<usize, Vec<SpellingKind>> = HashMap::new();
        for (u, k) in &keys {
            by_cov.entry(*u).or_default().push(*k);
        }
        for (u, kinds) in &by_cov {
            assert!(
                kinds.windows(2).all(|w| w[0] <= w[1]),
                "u_min 桶={u} 的组内层序不得交错: {kinds:?}\n全列表: {:?}",
                cands.iter().map(|c| (&c.word, c.spelling)).collect::<Vec<_>>()
            );
        }
        // 首键整体非降:u_min 桶必须单调不减(排序正确性的直接断言)。
        let unx: Vec<usize> = keys.iter().map(|(u, _)| *u).collect();
        assert!(
            unx.windows(2).all(|w| w[0] <= w[1]),
            "u_min 桶应整体非降(首键): {unx:?}"
        );
        // 精确层内相对顺序不被打乱:同桶的精确候选按分非增。
        let exact_scores: Vec<f32> = cands
            .iter()
            .filter(|c| c.spelling == SpellingKind::Exact
                && sort_bucket("beijdx", c) == 3)
            .map(|c| c.score)
            .collect();
        assert!(
            exact_scores.windows(2).all(|w| w[0] >= w[1]),
            "同桶精确层内仍按分降序: {exact_scores:?}"
        );
    }

    /// B4 层间边界(真实词库):同覆盖度内,层序必须整体非降——
    /// 任何简拼候选夹在同覆盖度的精确候选之间就是交错。输入
    /// `beijdx`:兜底联想给精确层(几十条),`lookup_abbrev` 给简拼层,
    /// 两层都进得了候选(精确层装不满 MAX_CANDIDATES)。
    ///
    /// 三段式(首键从「裸未解释字母数」改为「u_min 桶」后改写):
    /// - 原断言:排序键整体非降——(未解释字母数, 层序) 的二元组
    ///   序列字典序非降(裸 u 当首键时的首键单调 + 同键内层序);
    /// - 改为:(u_min 桶, 层序) 二元组字典序非降。裸 u 的首键单调
    ///   语义由 u_min 桶原样继承(桶单调 ⇔ 首键正确),同桶内层序
    ///   保证不变——断言形式不变,首键换成排序真正用的那个;
    /// - 为什么新行为对:裸 u 把「拼音凑巧等长」的生僻组合词
    ///   (被禁锢,u=0)顶上第一页;u_min 桶按「终点能否精确切分到
    ///   串尾」分桶,段内词(u_min>0)一律沉底。同桶内「精确 >
    ///   模糊 > 简拼」的层序保证原样钉住,不是放松。
    ///   (模糊层的边界由玩具词库测试
    ///   `mixed_exact_and_fuzzy_edges_belong_to_fuzzy_layer` /
    ///   `layered_sort_disabled_matches_legacy_exactly` 覆盖——真实词库
    ///   下模糊层是否存在取决于词库,不宜钉死。)
    #[test]
    fn real_dict_layer_boundary_no_interleave() {
        let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
        let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
        if !dict_path.is_file() || !abbrev_path.is_file() {
            return;
        }
        let mut engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
        engine.dict = engine.dict.with_abbrev(&abbrev_path);

        let cands = engine
            .lookup_candidates("beijdx", &LmContext::empty())
            .expect("查询失败");
        let kinds: Vec<SpellingKind> = cands.iter().map(|c| c.spelling).collect();
        assert!(kinds.contains(&SpellingKind::Exact), "beijdx 应有精确层: {kinds:?}");
        assert!(kinds.contains(&SpellingKind::Abbrev), "beijdx 应有简拼层: {kinds:?}");
        // (u_min 桶, 层序) 二元组字典序非降:首键单调 + 同键内层序
        // 单调,一条断言同时钉住两个排序键的正确性。
        let pair_keys: Vec<(usize, SpellingKind)> = cands
            .iter()
            .map(|c| (sort_bucket("beijdx", c), c.spelling))
            .collect();
        assert!(
            pair_keys.windows(2).all(|w| w[0] <= w[1]),
            "(未解释字母数, 层序) 应整体非降(首键 + 同键内层序): {:?}",
            cands.iter()
                .map(|c| (&c.word, c.spelling, sort_bucket("beijdx", c)))
                .collect::<Vec<_>>()
        );
    }

    /// 简拼层去重(真实词库):`zgr` 走 `lookup_abbrev` 命中的词,与
    /// `lookup_mixed`(残余声母路径)命中的同一词不得重复出现;整个
    /// 候选列表里同一 `(word, pinyin)` 只出现一次。
    #[test]
    fn real_dict_abbrev_no_duplicates_across_sources() {
        let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
        let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
        if !dict_path.is_file() || !abbrev_path.is_file() {
            return;
        }
        let mut engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
        engine.dict = engine.dict.with_abbrev(&abbrev_path);

        for input in ["zgr", "zhongg", "nihao"] {
            let cands = engine
                .lookup_candidates(input, &LmContext::empty())
                .expect("查询失败");
            let mut seen: std::collections::HashSet<(&str, &str)> =
                std::collections::HashSet::new();
            for c in &cands {
                assert!(
                    seen.insert((c.word.as_str(), c.pinyin.as_str())),
                    "{input} 的 (word, pinyin) 不得跨层重复: ({}, {})",
                    c.word,
                    c.pinyin
                );
            }
        }
    }

    /// 回声语义未变(真实词库 + 简拼):`zgr` 现在有真实简拼候选,
    /// 回声候选**不得**出现(回声只在全场无候选时冒头);简拼接入
    /// 后回声自然更少出现,这是预期——但解不出的串(非拼音字母组合
    /// `vq`,词典无任何 `v` 起始音节,简拼副索引也没有 `vq` 前缀)
    /// 仍必须回回声,语义不变。
    #[test]
    fn real_dict_abbrev_suppresses_echo_but_unresolvable_still_echoes() {
        let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
        let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
        if !dict_path.is_file() || !abbrev_path.is_file() {
            return;
        }
        let mut engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
        engine.dict = engine.dict.with_abbrev(&abbrev_path);

        let zgr = engine
            .lookup_candidates("zgr", &LmContext::empty())
            .expect("查询失败");
        assert!(!zgr.is_empty(), "zgr 应有简拼候选");
        assert!(
            zgr.iter().all(|c| !c.echo),
            "有简拼候选时不应出现回声候选: {:?}",
            zgr.iter().take(3).map(|c| (&c.word, c.echo)).collect::<Vec<_>>()
        );
        assert!(
            zgr.iter().all(|c| c.spelling == SpellingKind::Abbrev),
            "zgr 的候选应全部在简拼层(无精确/模糊来源): {:?}",
            zgr.iter().take(3).map(|c| (&c.word, c.spelling)).collect::<Vec<_>>()
        );

        let vq = engine
            .lookup_candidates("vq", &LmContext::empty())
            .expect("查询失败");
        assert_eq!(vq.len(), 1, "vq 解不出候选,应只有回声: {vq:?}");
        assert!(vq[0].echo, "vq 应回回声候选");
    }

    /// 副索引缺失时优雅退化(引擎层):`MmapDict::open()` 不挂副索引,
    /// 引擎不得 panic,且全拼行为与「没接简拼」完全一致(精确候选
    /// 逐位不变,简拼层为空)。
    #[test]
    fn engine_without_abbrev_index_degrades_gracefully() {
        let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
        if !dict_path.is_file() {
            return;
        }
        let engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
        assert!(!engine.dict.has_abbrev(), "未装配副索引");

        // 全拼:精确候选不受影响
        let nihao = engine
            .lookup_candidates("nihao", &LmContext::empty())
            .expect("查询失败");
        assert!(nihao.iter().any(|c| c.word == "你好"), "全拼应照常: {nihao:?}");
        assert!(
            nihao.iter().all(|c| c.spelling == SpellingKind::Exact),
            "无副索引时不应出现简拼层: {:?}",
            nihao.iter().map(|c| (&c.word, c.spelling)).collect::<Vec<_>>()
        );

        // 纯简拼:退化为回声(副索引缺失 ⇒ lookup_abbrev 空;输入
        // 切不出音节 ⇒ lookup_mixed 也空),引擎不 panic。
        let zgr = engine
            .lookup_candidates("zgr", &LmContext::empty())
            .expect("查询失败");
        assert_eq!(zgr.len(), 1, "无副索引时 zgr 应只剩回声: {zgr:?}");
        assert!(zgr[0].echo, "无副索引时 zgr 应回回声候选");

        // 混合简拼:靠静态音节表照常工作(不依赖副索引),这是
        // `real_abbrev_mixed_lookup` 同一不变量在引擎层的对应。
        // 直接在词库层断言(`lookup_mixed` 本身),绕开「精确层装满
        // 时简拼层被截断」的引擎层遮蔽——那条遮蔽是 MAX_CANDIDATES
        // 的既定行为,与本不变量无关。
        let dict_path2 = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
        let dict = crate::dict::MmapDict::open(&dict_path2).expect("词库加载失败");
        assert!(!dict.has_abbrev(), "裸 open 不应挂副索引");
        let mixed = dict.lookup_mixed("zhongguor").expect("混合简拼查询失败");
        assert!(
            mixed.iter().any(|c| c.word == "中国人"),
            "无副索引时 lookup_mixed 仍应命中中国人(静态音节表): {:?}",
            mixed.iter().take(5).map(|c| (&c.word, &c.pinyin)).collect::<Vec<_>>()
        );
    }

    /// 简拼层保底配额(PLAN.md 第三节缺口的修法):
    /// 精确+兜底层装满 `MAX_CANDIDATES` 时,简拼层仍有 `ABBREV_QUOTA`
    /// 条进候选列表,且目标词必须出现。真实词库 + 万象 gram,
    /// 与 probe 对拍同一装配;缺文件时跳过(同 `real_wanxiang_routes_through_engine`)。
    #[test]
    fn abbrev_layer_survives_full_exact_layer() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
        let dict_path = root.join("dict.bin");
        let abbrev_path = root.join("abbrev.bin");
        let gram = std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| std::path::PathBuf::from(h).join(".local/share"))
            })
            .map(|b| b.join("xwx/wanxiang-lts-zh-hans.gram"));
        let (Some(gram_path), true, true) = (gram, dict_path.is_file(), abbrev_path.is_file())
        else {
            return; // 环境缺文件:跳过(与 real_wanxiang_routes_through_engine 同款)
        };
        if !gram_path.is_file() {
            return;
        }
        let mut engine =
            ImeEngine::new_with_lm(Some(&dict_path), Some(&gram_path)).expect("引擎创建失败");
        engine.dict = engine.dict.with_abbrev(&abbrev_path);

        for (input, want) in [
            ("zhongguor", "中国人"),
            ("nih", "你好"),
            ("renm", "人们"),
            ("beijdx", "北京大学"),
            ("zhonggr", "中国人"),
        ] {
            let cands = engine
                .lookup_candidates(input, &LmContext::empty())
                .expect("查询失败");
            assert!(
                cands.iter().any(|c| c.word == want),
                "{input} 的候选里必须出现 {want}(简拼层保底配额): 末尾 {:?}",
                cands.iter().rev().take(8).map(|c| &c.word).collect::<Vec<_>>()
            );
        }

        // 装满组:候选总数达到 MAX_CANDIDATES。简拼层条数 = max(ABBREV_QUOTA,
        // 100 − 非简拼层条数)——配额是保底不是封顶。只断结构:
        // 简拼层 ≥ ABBREV_QUOTA 且 (u_min 桶, 层序) 排序键整体非降
        // (三段式:原断言「(未解释字母数, 层序) 整体非降」——裸 u 当
        // 首键时的排序正确性;改为「(u_min 桶, 层序) 整体非降」——首键
        // 换成 u_min 桶:0 桶的简拼目标词仍排在段内精确字(u_min>0 桶)
        // 之前,但「拼音凑巧等长」的生僻组合词不再凭裸 u=0 浮上第一页;
        // 同桶内的层序保证由二元组字典序断言原样钉住,不是放松)。
        for input in ["zhongguor", "nih", "renm"] {
            let cands = engine
                .lookup_candidates(input, &LmContext::empty())
                .expect("查询失败");
            assert_eq!(cands.len(), MAX_CANDIDATES, "{input} 应装满");
            let n_abbrev = cands
                .iter()
                .filter(|c| c.spelling == SpellingKind::Abbrev)
                .count();
            assert!(
                n_abbrev >= ABBREV_QUOTA,
                "{input} 简拼层应至少保底 {ABBREV_QUOTA} 条(实际 {n_abbrev})"
            );
            let pair_keys: Vec<(usize, SpellingKind)> = cands
                .iter()
                .map(|c| (sort_bucket(input, c), c.spelling))
                .collect();
            assert!(
                pair_keys.windows(2).all(|w| w[0] <= w[1]),
                "{input} (u_min 桶, 层序) 应整体非降: {:?}",
                cands.iter()
                    .map(|c| (&c.word, c.spelling, sort_bucket(input, c)))
                    .collect::<Vec<_>>()
            );
        }
    }
    /// 残余字母联想交给简拼层后的可见性(真实词库 + 万象 gram):
    /// `nih`/`renm`/`zhongguor` 的目标词必须出现且位于简拼层,
    /// 而不再被「只解释尾部 1 个字母的词」挤出前 95 名
    /// (修前:你好 97 / 人们 96 / 中国人 96)。排名上限是当前实测值
    /// 的回归锉(不许变差),不是验收目标——目标是前 20。
    #[test]
    fn mixed_abbrev_targets_are_visible() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
        let dict_path = root.join("dict.bin");
        let abbrev_path = root.join("abbrev.bin");
        let gram = std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| std::path::PathBuf::from(h).join(".local/share"))
            })
            .map(|b| b.join("xwx/wanxiang-lts-zh-hans.gram"));
        let (Some(gram_path), true, true) = (gram, dict_path.is_file(), abbrev_path.is_file())
        else {
            return; // 环境缺文件:跳过
        };
        if !gram_path.is_file() {
            return;
        }
        let mut engine =
            ImeEngine::new_with_lm(Some(&dict_path), Some(&gram_path)).expect("引擎创建失败");
        engine.dict = engine.dict.with_abbrev(&abbrev_path);

        // (输入, 目标词, 排名上限回归锉);上限 = 实测值,不允许再往后退。
        // 三段式(首键从「裸未解释字母数」改为「u_min 桶」后,名次锚
        // 不变):原断言「中国人/人们 ≤ 1、你好 ≤ 2」——裸 u 首键时
        // 的实测位;新规则(u_min 桶 + 覆盖罚分 0.2)下实测位不变
        // (中国人/人们 #1、你好 #2,见交付报告 B 组表)——目标词靠
        // 「简拼层恒 0 桶 + 配额」保护,段内碎片字靠 u_min>0 桶沉底,
        // 与系数无关,名次锚原样保留,不是放松。
        for (input, want, rank_ceiling) in [
            ("nih", "你好", 2usize),
            ("renm", "人们", 1usize),
            ("zhongguor", "中国人", 1usize),
        ] {
            let cands = engine
                .lookup_candidates(input, &LmContext::empty())
                .expect("查询失败");
            let pos = cands
                .iter()
                .position(|c| c.word == want)
                .unwrap_or_else(|| panic!("{input} 应含 {want}: {:?}", cands.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()));
            assert!(
                pos < rank_ceiling,
                "{input} 的 {want} 排第 {} 名,不应比修理后实测位({rank_ceiling})更靠后",
                pos + 1
            );
            // 目标词必须在简拼层,且排序键(u_min 桶, 层序)整体
            // 非降——同桶内的层序保证未被破坏(三段式:原断言
            // 「(未解释字母数, 层序) 整体非降」;改为 (u_min 桶, 层序)
            // 整体非降——首键换成排序真正用的桶,跨桶时 0 桶简拼词
            // 合法地排在 u_min>0 桶精确字之前,这是首键的预期效果,
            // 不是交错)。
            assert_eq!(
                cands[pos].spelling,
                SpellingKind::Abbrev,
                "{input} 的 {want} 应来自简拼层"
            );
            let pair_keys: Vec<(usize, SpellingKind)> = cands
                .iter()
                .map(|c| (sort_bucket(input, c), c.spelling))
                .collect();
            assert!(
                pair_keys.windows(2).all(|w| w[0] <= w[1]),
                "{input} (u_min 桶, 层序) 应整体非降: {:?}",
                cands.iter()
                    .map(|c| (&c.word, c.spelling, sort_bucket(input, c)))
                    .collect::<Vec<_>>()
            );
        }
    }

    /// 打字中的联想不被排序首键打散(真实词库):
    /// - 输入 `ni`(完整音节)时 你/尼/泥 仍在前 10;
    /// - 输入 `zhon`(不整音节,走兜底前缀联想)时 中/种/重 仍在前 10
    ///   ——它们的 `cp` = zhong 比输入**更长**,是「补全」而不是「覆盖
    ///   不全」,u=0 且 u_min=0(补全词桶恒 0),必须保持前排(这是
    ///   新排序规则最容易踩的坑:若按「覆盖长度」粗暴排序,补全联想
    ///   会被打散);
    /// - 输入 `zhongg` 时「中国」(简拼层 `lookup_mixed` 命中
    ///   `zhong g*`,0 桶)仍在前 10。
    ///
    /// 三段式:`mid_typing_completion_still_works` 原断言「ni 含
    /// 你/尼/泥(位置不限)、zhongg 含 中国(位置不限)」;本测试把
    /// 三者都收紧为「前 10」(实测 ni 1/2/3、zhon 1/2/3、zhongg 中国
    /// 第 1)——收紧不是放松。
    #[test]
    fn typing_completion_not_scattered() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
        let dict_path = root.join("dict.bin");
        let abbrev_path = root.join("abbrev.bin");
        let gram = std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| std::path::PathBuf::from(h).join(".local/share"))
            })
            .map(|b| b.join("xwx/wanxiang-lts-zh-hans.gram"));
        let (Some(gram_path), true, true) = (gram, dict_path.is_file(), abbrev_path.is_file())
        else {
            return;
        };
        if !gram_path.is_file() {
            return;
        }
        let mut engine =
            ImeEngine::new_with_lm(Some(&dict_path), Some(&gram_path)).expect("引擎创建失败");
        engine.dict = engine.dict.with_abbrev(&abbrev_path);

        for (input, wants) in [
            ("ni", &["你", "尼", "泥"][..]),
            ("zhon", &["中", "种", "重"][..]),
            ("zhongg", &["中国"][..]),
        ] {
            let cands = engine
                .lookup_candidates(input, &LmContext::empty())
                .expect("查询失败");
            for w in wants {
                let pos = cands
                    .iter()
                    .position(|c| c.word == *w)
                    .unwrap_or_else(|| panic!("{input} 应含 {w}: {:?}",
                        cands.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()));
                assert!(
                    pos < 10,
                    "{input} 的 {w} 应在前 10(打字联想不被打散),实际第 {} 名: {:?}",
                    pos + 1,
                    cands.iter().take(12).map(|c| &c.word).collect::<Vec<_>>()
                );
            }
        }
    }

    /// 判据 A(本轮新增):全拼第一页回来——高频单字/词必须在前 10,
    /// 「拼音凑巧等长」的生僻组合词必须**不在**前 10。
    ///
    /// 这是「裸未解释字母数硬首键」的设计缺陷回归测试:裸 u 把
    /// 被禁锢(beijing,u=0)、狄安娜(diannao,u=0)、进啊(jinan,
    /// u=0)顶上第一页,把 被/北/点/电(u=4/4/3/3)挤出前 10——
    /// 因为裸 u 只看「拼音长度==输入长度」,不看「剩下的字母能否
    /// 精确切分」。u_min 桶把「终点落在不可精确切分段内」的候选
    /// (u_min>0)一律沉底,高频单字(终点在音节边界,u_min=0)
    /// 因此回到第一页。真实词库 + 万象 gram(与 probe 对拍同装配)。
    #[test]
    fn full_pinyin_first_page_recovers() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
        let dict_path = root.join("dict.bin");
        let abbrev_path = root.join("abbrev.bin");
        let gram = std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| std::path::PathBuf::from(h).join(".local/share"))
            })
            .map(|b| b.join("xwx/wanxiang-lts-zh-hans.gram"));
        let (Some(gram_path), true, true) = (gram, dict_path.is_file(), abbrev_path.is_file())
        else {
            return; // 环境缺文件:跳过(与 real_wanxiang_routes_through_engine 同款)
        };
        if !gram_path.is_file() {
            return;
        }
        let mut engine =
            ImeEngine::new_with_lm(Some(&dict_path), Some(&gram_path)).expect("引擎创建失败");
        engine.dict = engine.dict.with_abbrev(&abbrev_path);

        // (输入, 必须在前 10 的词)
        let must_in: [(&str, &[&str]); 6] = [
            ("beijing", &["北京", "被", "北"]),
            ("diannao", &["电脑", "点", "电"]),
            ("zhongguo", &["中国", "中", "种"]),
            ("gongzuo", &["工作", "公", "工"]),
            ("jinan", &["济南", "进"]),
            ("mingtian", &["明天", "明", "名"]),
        ];
        for (input, wants) in must_in {
            let cands = engine
                .lookup_candidates(input, &LmContext::empty())
                .expect("查询失败");
            let top10: Vec<&str> = cands.iter().take(10).map(|c| c.word.as_str()).collect();
            for w in wants {
                assert!(
                    top10.contains(w),
                    "{input} 的 {w} 应在前 10(全拼第一页回来),实际前 10: {top10:?}"
                );
            }
        }

        // (输入, 不得在前 10 的生僻组合词)
        let must_out: [(&str, &[&str]); 3] = [
            ("beijing", &["被禁锢", "悲寂", "背脊"]),
            ("diannao", &["狄安娜", "蒂安娜"]),
            ("jinan", &["进啊"]),
        ];
        for (input, bads) in must_out {
            let cands = engine
                .lookup_candidates(input, &LmContext::empty())
                .expect("查询失败");
            let top10: Vec<&str> = cands.iter().take(10).map(|c| c.word.as_str()).collect();
            for w in bads {
                assert!(
                    !top10.contains(w),
                    "{input} 的生僻组合词 {w} 不应在前 10(裸 u 硬首键回归),实际前 10: {top10:?}"
                );
            }
        }
    }

    /// 「首音节未打完」联想仍然工作(真实词库):输入 `ni`
    /// 时 你/尼/泥 仍在前排;输入 `zhongg`(打了 `zhong` + 一个声母)时
    /// 中国 仍然出现(简拼层 `lookup_mixed` 命中 `zhong g*`)。
    #[test]
    fn mid_typing_completion_still_works() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
        let dict_path = root.join("dict.bin");
        let abbrev_path = root.join("abbrev.bin");
        let gram = std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| std::path::PathBuf::from(h).join(".local/share"))
            })
            .map(|b| b.join("xwx/wanxiang-lts-zh-hans.gram"));
        let (Some(gram_path), true, true) = (gram, dict_path.is_file(), abbrev_path.is_file())
        else {
            return;
        };
        if !gram_path.is_file() {
            return;
        }
        let mut engine =
            ImeEngine::new_with_lm(Some(&dict_path), Some(&gram_path)).expect("引擎创建失败");
        engine.dict = engine.dict.with_abbrev(&abbrev_path);

        let ni = engine
            .lookup_candidates("ni", &LmContext::empty())
            .expect("查询失败");
        for w in ["你", "尼", "泥"] {
            assert!(
                ni.iter().any(|c| c.word == w),
                "ni 应仍含 {w}: {:?}",
                ni.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()
            );
        }

        let zhongg = engine
            .lookup_candidates("zhongg", &LmContext::empty())
            .expect("查询失败");
        assert!(
            zhongg.iter().any(|c| c.word == "中国"),
            "zhongg 应仍含 中国: {:?}",
            zhongg.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()
        );
    }


    /// 配额只是保底不是封顶,且不浪费名额(回归:`ren` 旧行为是
    /// head 21 条 + 简拼联想补满 100,曾有一版实现把简拼层无条件
    /// 截到配额导致退化)。真实词库 + 空 LM(无需 gram):`ren` 的
    /// head ≪ 100,简拼层应补满到 100;`zhongguo`(整词精确,
    /// 简拼层为空)不受影响,全拼照旧。
    #[test]
    fn abbrev_quota_is_floor_not_cap() {
        let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
        let abbrev_path = dict_path.with_file_name("abbrev.bin");
        if !dict_path.is_file() || !abbrev_path.is_file() {
            return;
        }
        let mut engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
        engine.dict = engine.dict.with_abbrev(&abbrev_path);

        // head 很短时:简拼层补满到 MAX_CANDIDATES(保底≠封顶)
        let ren = engine
            .lookup_candidates("ren", &LmContext::empty())
            .expect("查询失败");
        assert_eq!(ren.len(), MAX_CANDIDATES, "ren 应补满 100: {}", ren.len());
        let n_abbrev = ren
            .iter()
            .filter(|c| c.spelling == SpellingKind::Abbrev)
            .count();
        assert!(
            n_abbrev > ABBREV_QUOTA,
            "ren 的简拼层应远超配额(保底不是封顶): {n_abbrev}"
        );
        // 排序键未被破坏:(u_min 桶, 层序) 整体非降(三段式:原断言
        // 「(未解释字母数, 层序) 整体非降」;改为 (u_min 桶, 层序) 整体
        // 非降——同桶内的层序保证原样钉住,跨桶时 0 桶简拼词合法排在
        // u_min>0 桶精确字之前)。
        let pair_keys: Vec<(usize, SpellingKind)> = ren
            .iter()
            .map(|c| (sort_bucket("ren", c), c.spelling))
            .collect();
        assert!(
            pair_keys.windows(2).all(|w| w[0] <= w[1]),
            "ren (u_min 桶, 层序) 应整体非降: {:?}",
            ren.iter()
                .map(|c| (&c.word, c.spelling, sort_bucket("ren", c)))
                .collect::<Vec<_>>()
        );

        // 简拼层为空时:head 照旧,不因配额浪费名额(逐位不变由对拍保证,
        // 这里断结构:无 Abbrev 候选、条数与无配额时一致)。
        let zhongguo = engine
            .lookup_candidates("zhongguo", &LmContext::empty())
            .expect("查询失败");
        assert!(
            zhongguo.iter().all(|c| c.spelling != SpellingKind::Abbrev),
            "zhongguo 不应有简拼层"
        );
        assert!(!zhongguo.is_empty(), "zhongguo 候选非空");
    }

    /// `merge_with_abbrev` 的合成单元测试(不依赖真实词库):
    /// 覆盖「head 装满 / 简拼层为 0 / 两层都短 / 层序收尾」四个分支。
    /// 本测试所有候选的 `pinyin` 字段都是空串:空 `cp` 是任何输入的
    /// 前缀 ⇒ 「未解释字母数」首键恒 0(补全),整体排序退化为
    /// 「层序 + 分数 + …」,配额截取后拼接即终态——同键(同覆盖度)
    /// 下简拼层仍整体排在精确层之后(分层降为第二键,不是拆掉)。
    #[test]
    fn merge_with_abbrev_quota_branches() {
        let mk = |word: String, freq: u64, kind: SpellingKind| Candidate {
            word,
            pinyin: String::new(),
            score: 0.0,
            freq,
            spelling: kind,
            echo: false,
        };
        let head: Vec<Candidate> = (0..100u64)
            .map(|i| mk(format!("e{i}"), 10_000 - i, SpellingKind::Exact))
            .collect();
        let abbrev: Vec<Candidate> = (0..10u64)
            .map(|i| mk(format!("a{i}"), 100 - i, SpellingKind::Abbrev))
            .collect();

        // head 装满 100 + 简拼 10 → 95 + 5,简拼层保底 5 条且排在最后
        let (total, out) = merge_with_abbrev("", head.clone(), abbrev.clone());
        assert_eq!(total, 110);
        assert_eq!(out.len(), MAX_CANDIDATES);
        assert_eq!(
            out.iter().filter(|c| c.spelling == SpellingKind::Abbrev).count(),
            ABBREV_QUOTA
        );
        assert_eq!(out[95].word, "a0", "简拼层第 1 名应落在第 96 位");
        assert!(out[..95].iter().all(|c| c.spelling == SpellingKind::Exact));
        // head 的前 95 条逐位保留(被挤掉的是第 96~100 名)
        assert!(out[..95].iter().zip(head.iter()).all(|(a, b)| a.word == b.word));

        // 简拼层为 0 → head 用满 100,一个名额不浪费
        let (_, out) = merge_with_abbrev("", head.clone(), Vec::new());
        assert_eq!(out.len(), MAX_CANDIDATES);
        assert!(out.iter().all(|c| c.spelling == SpellingKind::Exact));

        // 两层都短(总条数 ≤100)→ 原样拼接,简拼层不被配额截断
        let (_, out) = merge_with_abbrev("", head[..20].to_vec(), abbrev.clone());
        assert_eq!(out.len(), 30, "总数不足 100 时不许截断");
        assert_eq!(
            out.iter().filter(|c| c.spelling == SpellingKind::Abbrev).count(),
            10
        );

        // head 短 + 简拼层长(总数 >100)→ 简拼层补满 100,不按配额封顶
        let big_abbrev: Vec<Candidate> = (0..90u64)
            .map(|i| mk(format!("b{i}"), 100 - i, SpellingKind::Abbrev))
            .collect();
        let (total, out) = merge_with_abbrev("", head[..20].to_vec(), big_abbrev);
        assert_eq!(total, 110);
        assert_eq!(out.len(), MAX_CANDIDATES);
        assert_eq!(
            out.iter().filter(|c| c.spelling == SpellingKind::Abbrev).count(),
            80,
            "head 20 条时简拼层应补满到 100(保底不是封顶)"
        );
        assert!(out[..20].iter().all(|c| c.spelling == SpellingKind::Exact));

        // 首键跨层生效(合成锚):同分时「只覆盖前缀的精确候选」必须排在
        // 「覆盖整串的简拼候选」之后——pinyin 非空时 u_min 桶不再恒 0
        // (inp=abcdef:「只覆盖前缀」cp=abc,从 3 起 "def" 不可切,
        // u_min>0 桶),简拼层(恒 0 桶)压过段内精确候选。
        let cover_head = vec![
            Candidate { word: "只覆盖前缀".into(), pinyin: "abc".into(),
                score: 0.0, freq: 1, spelling: SpellingKind::Exact, echo: false },
        ];
        let cover_abbrev = vec![
            Candidate { word: "覆盖整串".into(), pinyin: "abc def".into(),
                score: 0.0, freq: 1, spelling: SpellingKind::Abbrev, echo: false },
        ];
        let (_, out) = merge_with_abbrev("abcdef", cover_head, cover_abbrev);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].word, "覆盖整串", "0 桶简拼应压过段内(u_min>0 桶)精确: {out:?}");
        assert_eq!(out[1].word, "只覆盖前缀");
    }
}
