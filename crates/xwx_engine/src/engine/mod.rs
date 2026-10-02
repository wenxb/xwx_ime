use super::*;

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

/// 模糊拼音的**生产默认值**：关闭。
///
/// 历史状态与本次裁决（2026-10-01，`examples/fuzzy_bench.rs` 实测）：
/// 在本次改动之前 `fuzzy_enabled` 在生产进程里**从未被置为 true**——
/// 它只在单测里由测试自己设置（见本文件 `fuzzy_*` 测试），`main.rs`
/// 既不设置它、也没有任何配置入口，而 `README.md` 早已把它列为已完成
/// 特性。也就是说：**能力实现了、测试是绿的、用户却用不到**。
///
/// 本次把它接成真正的开关（`XWX_FUZZY=1`，见 `src/main.rs`），并保留
/// 「默认关闭」。默认值不是随手定的，是实测裁决的（release，真实
/// dict.bin + 万象 gram，`examples/fuzzy_bench.rs`）：
///
/// | 维度 | 实测结果 | 判定 |
/// |---|---|---|
/// | 延迟 | `nihao` 0.235→0.546ms(2.29x)、`zhongguo` 0.589→1.526ms(2.54x)、`womenjintianquxian` 3.376→22.008ms(**6.48x**) | ✗ 超预算 |
/// | 排序 | `diannao` 前 10 第 9/10 名「点那」被模糊候选「惦」顶掉 | ✗ 有退化 |
/// | 召回 | `lansheng`→男生 #27、`zongguo`→中国 #16（另有 1 组仍不可召回） | △ 收益弱 |
///
/// 关键的是延迟那一行：**最长全拼串 22ms 已经突破本项目自定的 12ms
/// 上限**（PLAN.md 第五节性能预算）。IBus 侧按键是同步处理的，
/// 22ms/击键在长句输入下是肉眼可感的卡顿。而收益侧只有 2 组新召回、
/// 且名次在 16~27（要翻 2~3 页才够得着）——**代价确定、收益微弱**，
/// 所以默认关闭，作为用户显式打开的 opt-in。
///
/// 开启后行为完全可用（13 组混淆对都参与展开与降权），
/// 只是请知悉上表的三项代价。若将来要改默认值，请重跑
/// `fuzzy_bench`——**词频数据或 LM 权重一变，这张表的结论就可能翻转**
/// （与 `COVERAGE_PENALTY` 的余量同一种性质）。
pub const DEFAULT_FUZZY_ENABLED: bool = false;

/// 环境变量名：置 `1`/`true`/`on`/`yes`（大小写不敏感）即启用模糊拼音。
///
/// 放这里（而不是 `src/main.rs`）是为了让「解析规则」本身可单测——
/// 进程级环境变量的解析是纯函数，与引擎不耦合。
pub const FUZZY_ENV: &str = "XWX_FUZZY";

/// 解析 [`FUZZY_ENV`] 的取值；未设置或无法识别时回落到
/// [`DEFAULT_FUZZY_ENABLED`]。
///
/// 见 [`fuzzy_env_value_recognized`]——调用方用它来区分「解析结果」
/// 与「这个值到底认不认得」，以便对无法识别的值记一条 warning。
pub fn parse_fuzzy_env(value: Option<&str>) -> bool {
    if fuzzy_env_value_recognized(value) {
        matches!(
            value.map(str::trim).map(|v| v.to_ascii_lowercase()),
            Some(ref v) if matches!(v.as_str(), "1" | "true" | "on" | "yes")
        )
    } else {
        // 未设置或无法识别:一律取默认值。
        //
        // **无法识别的值按「未设置」处理而不是报错**:输入法启动失败
        // 比「模糊音没开」严重得多(与引擎对缺失词库/模型一律优雅
        // 退化的取舍一致)。调用方负责把无法识别的值记一条 warning。
        DEFAULT_FUZZY_ENABLED
    }
}

/// `value` 是否是 [`FUZZY_ENV`] 认得的取值（含「未设置」）。
///
/// 拆出来是**为了避免取值词表出现两份**：`src/main.rs` 需要知道
/// 「用户写的值被忽略了」，而这件事的定义就是「不认得」。
/// 让它复用本函数，而不是在进程入口另抄一份 `matches!` 字面量
/// ——两份词表迟早会漂移，而漂移的症状是**静默吞掉用户配置**。
///
/// `None`（未设置）算「认得」：它不是错误。
pub fn fuzzy_env_value_recognized(value: Option<&str>) -> bool {
    match value.map(str::trim).map(|v| v.to_ascii_lowercase()) {
        None => true,
        Some(ref v) => matches!(
            v.as_str(),
            "1" | "true" | "on" | "yes" | "0" | "false" | "off" | "no" | ""
        ),
    }
}
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
///
/// **codex 审查第二批之后,生产查询不再走 beam**:精确路径的「有没有覆盖
/// 整串的读法」改用 `ImeEngine::lattice_reachable` 的布尔 DP(0 次 LM 调用),
/// 容错路径早在第 ③ 期就改用了后向 DP。因此这个值**不再是逐键延迟的上界
/// 来源**,它现在只服务独立算法接口(`decode_paths`/`decode_beam_with_lattice`
/// 与解码测试);上面那段实测数字是 beam 还在生产路径上时测的,保留作为
/// 那个接口的规模依据。常量本身冻结(owner 裁决),不得擅自改。
#[cfg_attr(not(test), allow(dead_code))]
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

/// 容错解码的编辑预算:**整条路径**最多允许的编辑次数。
///
/// Damerau(1964):80% 的输入错误只有一次编辑操作。文献里「阈值取 2」说的是
/// **单个音节内**的编辑数(那是音节索引的事,见
/// [`segmenter::TypoSyllables`],它天生只做 ≤1 次编辑);这里管的是**整条
/// 路径**上出现几次「打错的音节」。实测目标场景(`nihoa`/`shenem`/
/// `shanghaai`/长串多打或乱码一段)全部只需要 1 次;放到 2 会把容错格的规模
/// 翻一倍(长串实测 12ms → 20ms+),再把 12ms 端到端门槛破掉。
const MAX_TYPO_EDITS: u8 = 1;

/// 单次「吃掉一段字母」(多打/打乱)弧的长度上限。
const MAX_TYPO_GAP: usize = 2;

/// 走「音节级 ≤1 编辑」弧的窗口**至少**这么长。
///
/// 1 个字母的窗口对任何单字母音节都是「1 次编辑可达」(`n` → `na`/`ne`/`ni`/
/// `nu`…二十来个),展开它等于用最不靠谱的读法吃掉整个链预算——实测会让
/// 同一起点的精确音节弧根本轮不到(`nihoa` 的 `ni` 边缘被挤掉)。
/// 「少打一个字母」的真实形态窗口是 2~3 个字母,不受此限。
const MIN_TYPO_WINDOW: usize = 2;

/// 单个窗口最多展开几条容错读法(按代价升序,只留**最便宜的一条**)。
///
/// 一个窗口可能同时像好几个音节(`em` 像 `me`/`en`…),全展开会让格的分支
/// 数乘起来——实测长串上容错解码从 ~5ms 涨到 23ms(破 12ms 端到端门槛),
/// 而多出来的读法几乎都排不进候选前列。只留最便宜的一条:它是「用户最可能
/// 打错的形状」,其余交给别的窗口/别的切法。
const MAX_TYPO_READINGS_PER_WINDOW: usize = 1;

/// 容错模式下的**每起点链预算**(精确模式用 `MAX_CHAINS_PER_START`)。
///
/// 容错弧在每个窗口都可能多开分支,沿用 64 会让长串的格构建成本翻几倍;
/// 实测目标场景(`nihoa`/`shanghaai`/长串末尾多打一个字母)需要的读法都排在
/// 前几条,12 足够而成本可控。
/// 注意:这个值**不能**取太小——实测 12 会让长串(`womenjintianqugongyuann`)
/// 的链在到达 `wo men jin tian` 之前就把预算花光,整句读法直接消失;真正
/// 压住成本的是「窗口 ≤4 字母 + 只留 1 条读法 + 路径 ≤1 次编辑」三条限制。
const MAX_TOLERANT_CHAINS_PER_START: usize = 64;



/// 启用容错解码所需的最短输入长度(见 `lookup_candidates` 的门槛⓪)。
const MIN_TOLERANT_INPUT_LEN: usize = 4;

/// 「原样上屏剩余串」逃生口候选的分数。
///
/// 语义上它不是词(与回声候选同源),分数必须远低于任何真实候选:实测真实
/// 候选的分数量级在 -5 ~ -70(整句读法是 10 个以上词的 log 概率之和),
/// -1000 足够低且仍是有限值(不引入 -inf/NaN 打印与排序歧义)。
const ESCAPE_HATCH_SCORE: f32 = -1000.0;

/// 容错弧只对**不超过**这么长的窗口开放。
///
/// 替换/漏打/多打/调换的窗口都很短(`em`、`hoa`、`haai`、`mem`);再长的
/// 窗口要么是乱码段(交给「吃掉一段字母」的空词弧),要么是和弦切分不同,
/// 不属于单音节打错。
const MAX_TYPO_WINDOW: usize = 4;

/// 声母前缀枚举展开时,每个音节的前缀联想条数上限。
///
/// 「只打了声母/半个音节」的输入(`zh`/`x`/`q`/`zhon`)不是合法音节,
/// 旧的兜底对它做一次整串字节前缀联想:词典按字节序排列,`zh` 的命中
/// 区间以 `zha*` 开头,凑满 `MAX_LOOKUP_RESULTS`(500)即早停——
/// 于是候选被字典序锁死在韵母 `a`,`zhe`/`zhong` 的高频字(这/中)
/// 根本进不了候选(参照实现:libime 的 `PinyinFinal::Invalid` 在 trie 上
/// 枚举全部韵母;librime 的 script_translator 在拼写层就切开了音节)。
///
/// 修法(不改存储层):对「不是合法音节、但是某些音节的前缀」的串,用
/// 编译期静态音节表枚举它的所有音节补全(`zh` → `zha, zhai, zhan, ...`),
/// 每个音节做一次**带配额**的前缀联想,合并后按分排序——每个韵母下的
/// 高频字都有机会进候选,而不是被字节序最小者独占。
///
/// 配额取值理由:
/// - 字节序偏置只在「字典序最前的那几个音节」上,只要每个音节都拿到
///   **非零**条数,偏置即被打破;24 条对单音节字(`zhi` 系 20+ 同音字)
///   的头部够用;
/// - 同音节内的词条按**词频降序**取(mmap 词块本身就是按 freq 降序
///   存的),24 条保证「在/区/钱」这类同音节头部字不会被同音字的
///   数量堆出去(实测配额 16 时 `qu` 系的「区」(该音节第 20 条)
///   进不了前 30,24 可以);
/// - 总查询量的上界:枚举音节数 ≤ 37(`z`:9 个 z 系 + 28 个 zh 系),
///   合并池 ≤ 37 × 24 ≈ 900 条,仍在旧路径单次 500 条的 2 倍以内,
///   排序/打分开销同量级;
/// - 再大(如 64)收益趋零:候选位只有 100 个,字频排序后前 24 条
///   之外的词本就到不了第一页,只是白做多倍打分。
const SYLLABLE_ENUM_QUOTA: usize = 24;

/// 切分格上的一条词边：覆盖 `pinyin[start..end]`，对应该区间查到的一个词。
///
///  是给容错格用的:容错格以精确格为底(克隆),只在错处附近补新边
/// (见 [`ImeEngine::build_tolerant_lattice`])。
#[derive(Debug, Clone)]
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
    /// **容错解码**的编辑惩罚(对数域加性扣分,非负)。
    ///
    /// 走「音节级 ≤1 次编辑」弧的边带上它(见 [`segmenter::TypoSyllables`]
    /// 与 [`Self::build_lattice_with`]),口径是噪声信道的 `-log10 P(编辑操作)`。
    /// 与 `fuzzy_penalty`(模糊音)分开记,是因为两者的**来源不同**:
    /// 模糊音是「用户就是这么念的」,容错是「用户打错了」,代价表不一样。
    typo_penalty: f32,
    /// 本边引入的**容错编辑数**(codex 审查 D13)。
    ///
    /// 音节级「≤1 次编辑」弧与「吃掉一段字母」的空词弧各记 1;精确边与
    /// 模糊音边都是 0。口径是「这串里坏了几处」而不是「被删掉几个字母」
    /// ——后者会把一次多打两个字母判成两次编辑,与 [`MAX_TYPO_EDITS`] 的
    /// 语义(最多几处打错)不符。后向 DP 按「剩余预算」递推,整条路径的
    /// 累计编辑数因此真正受限;此前它只卡单次链枚举。
    edit_count: u8,
    /// 空词边:容错解码里「吃掉一段无法解释的字母」的弧。
    ///
    /// 它没有词、没有拼音、没有 LM 转移,只有 `typo_penalty` 一项分数;
    /// 唯一作用是让 beam 能**跨过**多打/打乱的字母继续前进。这种边绝不能
    /// 成为候选(见 [`Self::lattice_candidates`] 的过滤)。
    gap: bool,
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
    /// 走到本节点为止**已经用掉**的容错编辑数(codex 审查 D13)。
    ///
    /// beam 是路径级搜索,预算必须跟着状态走:同一位置可以有不同的已用
    /// 预算,超预算的节点直接不再扩展。
    used_edits: u8,
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

use rank::{merge_with_abbrev, ordered_candidates, prune_beam, sort_candidates};

mod api;
mod decode;
mod lattice;
mod lookup;
mod rank;


#[cfg(test)]
mod tests;
