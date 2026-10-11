//! 中英意图分档与**稳定混排**。
//!
//! 决策表见 `docs/english-input/02-engine.md` 第 8 节，模板见第 9 节。
//! 这里**不调用中文排序器**：中文候选进来什么样，出去还是什么样（只在
//! 原文兜底与重复消除上动位置）。

use crate::candidate::{Candidate, EnglishMatchKind};

use super::max_candidates;

/// 意图分档。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentBand {
    /// 英文靠前（首屏模板 `E E Z E Z Z Z Z Z`）。
    EnglishFirst,
    /// 交替（`Z E Z E Z Z Z Z Z`）。
    Mixed,
    /// 中文保持首选，英文只占末位（`Z Z Z Z Z Z Z Z E`）。
    ChineseFirst,
}

impl IntentBand {
    /// 供 trace/测试判读的短名。
    pub fn label(self) -> &'static str {
        match self {
            IntentBand::EnglishFirst => "EnglishFirst",
            IntentBand::Mixed => "Mixed",
            IntentBand::ChineseFirst => "ChineseFirst",
        }
    }
}

/// 首屏模板：`true` = 取英文，`false` = 取中文非回声候选。
///
/// `EnglishFirst` 给英文**前三个槽位**。原模板是 `E E Z E Z Z Z Z Z`：
/// 第 3 槽被中文占掉，于是英文通道的第 3 名在混排里恒为第 4 名——
/// 档名与档义（「英文靠前」，第 8 节决策表的 `good_en 且 strong_completion
/// ⇒ EnglishFirst` 就是为了让补全能进首屏）自相矛盾，验收里
/// 「补全 Top-3」因此有一条结构性的天花板。中文候选一个不丢（模板只调
/// 次序），只是首个中文位从第 3 槽挪到第 4 槽。
const TEMPLATE_ENGLISH_FIRST: [bool; 9] = [true, true, true, false, false, false, false, false, false];
const TEMPLATE_MIXED: [bool; 9] = [false, true, false, true, false, false, false, false, false];
const TEMPLATE_CHINESE_FIRST: [bool; 9] = [false, false, false, false, false, false, false, false, true];

fn template(band: IntentBand) -> &'static [bool; 9] {
    match band {
        IntentBand::EnglishFirst => &TEMPLATE_ENGLISH_FIRST,
        IntentBand::Mixed => &TEMPLATE_MIXED,
        IntentBand::ChineseFirst => &TEMPLATE_CHINESE_FIRST,
    }
}

/// **简拼读法对齐**：这串拼音能否按「逐音节前缀」读出输入串。
///
/// 第一个音节必须**打全**，其后的每个音节消耗输入里一段非空前缀
/// （整音节、只取首字母的简拼、或结尾没打完的截断），输入字母恰好
/// 用完即算可读（剩余音节是没打的尾巴，允许剩下）。`yidd` →
/// `yi dian dian`（yi | d | d）就是这样读出来的；字面前缀判据看不见
/// 这种读法，会漏判中文可读。
///
/// 「首音节必须打全」是把关：**简拼打法总是先打全第一个字**（yidd
/// 的 yi）；而纯声母开头的串（`pyt`、`app`、`str`）是英文前缀的形状
/// ——任何三个字母都能拼进某条真实三字词的声母里（`p|y|t` → 披X踏
/// 这种真词条目），放进来英文就永远出不来。真用纯简拼的输入（`ydd`/
/// `wsm`）在英文通道没有竞争（召回为空，`en` 为空根本不混排），轮
/// 不到本判据说话。
///
/// 带失败记忆的 DFS：状态 (已耗字母, 已用音节) 至多 `input×syllables`
/// 个，每状态至多试 `syllable.len()` 种切法；输入 ≤48 字母、词条音节
/// 一只手数得过来，按键路径上代价可忽略。
pub(super) fn pinyin_reads_input(input: &[u8], pinyin: &str) -> bool {
    let syl: Vec<&str> = pinyin.split(' ').filter(|s| !s.is_empty()).collect();
    let (t0, rest) = match syl.split_first() {
        Some((t0, rest)) => (t0, rest),
        None => return false,
    };
    // 首音节打全：整段相等，后面的简拼才有「接着打」可言。
    let tail = match input.strip_prefix(t0.as_bytes()) {
        Some(tail) => tail,
        None => return false,
    };
    let stride = rest.len() + 1;
    let mut failed = vec![false; (tail.len() + 1) * stride];
    fn walk(input: &[u8], syl: &[&str], failed: &mut [bool], stride: usize, i: usize, j: usize) -> bool {
        if i == input.len() {
            return true; // 字母用完即可读
        }
        if j == syl.len() {
            return false; // 字母没用完，音节先尽
        }
        if failed[i * stride + j] {
            return false;
        }
        let t = syl[j].as_bytes();
        // 前缀逐位延长：某一位失配，更长的前缀只会错得更远，到此为止。
        for l in 1..=t.len().min(input.len() - i) {
            if input[i + l - 1] != t[l - 1] {
                break;
            }
            if walk(input, syl, failed, stride, i + l, j + 1) {
                return true;
            }
        }
        failed[i * stride + j] = true;
        false
    }
    walk(tail, rest, &mut failed, stride, 0, 0)
}

/// **中文可读**：中文通道里有「拼音真的读得出输入、且有真实词频」的候选。
///
/// 「读得出」有两种形态，缺一不可的条件是**真实词频**（`freq > 填充档`）：
///
/// - **全拼前缀**：`rug`→如果（`ruguo` 以 `rug` 开头）；`hello`→黑龙
///   （`heilong` 第 3 个字母就不对）不算。
/// - **简拼读法**：`yidd`→一点点（`yi | d | d`）；字面上
///   `yidiandian` 并不以 `yidd` 开头，只能按逐音节前缀对齐判。
///
/// 真实词频挡掉简拼通道的拼装链（`thank`→他和安康那种 `freq=0` 的
/// 货色）——不然一切英文输入都成了"可能是中文"。两个一起，`rug`/
/// `bir`/`yidd` 判为中文可读，`python`/`hello`/`thank` 判不出来——
/// 正好是用户要的分界。
fn plausible_chinese(zh: &[Candidate], raw_input: &str) -> bool {
    let lower = raw_input.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    zh.iter().any(|c| {
        !c.echo
            && !c.is_english()
            && c.freq > super::filler_freq_tier()
            && (c.pinyin.replace(' ', "").starts_with(&lower)
                || pinyin_reads_input(bytes, &c.pinyin))
    })
}

/// `good_en`：首要英文候选有足够证据（静态频率够高，或用户学过至少 3 次）。
pub(crate) fn good_english(e0_zipf: u16, e0_user_unigram: u32) -> bool {
    e0_zipf >= 300 || e0_user_unigram >= 3
}

/// 按决策表定档（命中即停）。
///
/// `zh` 是中文通道的完整结果，`en` 是英文通道的最终候选（可空）；
/// `recent_en` = 存在可信英文上文；`good_en` = 首要英文候选有足够证据。
pub(crate) fn classify_intent(
    raw_input: &str,
    zh: &[Candidate],
    en: &[Candidate],
    recent_en: bool,
    good_en: bool,
) -> Option<IntentBand> {
    if en.is_empty() {
        return None; // 不混排，原样返回 zh
    }
    let z_nonempty = zh.iter().any(|c| !c.echo);
    if !z_nonempty {
        return Some(IntentBand::EnglishFirst); // 输入不可能是拼音
    }
    // **「英文串进行中」只认完整英文词**：E0 是 Exact 才说明用户在
    // 连着打英文（thank/you/can——c07 的裁决）；E0 只是前缀补全
    // （ke→keep、yidd→yiddish）证明不了「在打英文」，管不了分档。
    if recent_en
        && good_en
        && en.first().and_then(|c| c.english_match_kind()) == Some(EnglishMatchKind::Exact)
    {
        return Some(IntentBand::EnglishFirst);
    }
    if raw_input.chars().count() <= 2 {
        // 1~2 个字母是真歧义区（`ok`/`us`/`on` 这类完整英文词 vs 两键
        // 拼音缩写），交替混排：中文在前，英文仍在第 2 位可点。
        // 用户实测的 ke 也在这条上收场：可#1，keep/key#2。
        return Some(IntentBand::Mixed);
    }
    if plausible_chinese(zh, raw_input) {
        // **中文可读 ⇒ 中文优先**（用户定的方向）：输入既然有真拼音
        // 读法（全拼前缀 rug→如果、简拼 yidd→一点点），就更可能是在
        // 打中文。「不明确的英文」不要抢前排，退到末位保留入口。
        return Some(IntentBand::ChineseFirst);
    }
    // 中文读不出来 ⇒ 用户就是在打英文。
    if good_en {
        return Some(IntentBand::EnglishFirst);
    }
    Some(IntentBand::Mixed)
}

/// 稳定混排：按模板取首屏，再依次追加剩余 Z、剩余 E、剩余 R。
///
/// - `Z` = 中文非回声候选（内部相对次序不变，不按混合分二次排序）；
/// - `E` = 英文候选（同上）；
/// - `R` = 原文回声候选；
/// - 上限 = 原中文候选上限 + 8；**不丢弃中文长尾**。
/// - 与真实英文的 `(word, covered)` 都相同的 R 会被去掉（`python` 与
///   `Python` 大小写不同，不算相同）。
pub(crate) fn stable_blend(
    zh: Vec<Candidate>,
    en: Vec<Candidate>,
    band: IntentBand,
) -> Vec<Candidate> {
    let mut z: Vec<Candidate> = Vec::with_capacity(zh.len());
    let mut r: Vec<Candidate> = Vec::new();
    for c in zh {
        if c.echo {
            r.push(c);
        } else {
            z.push(c);
        }
    }
    // 去重：与真实英文 word + covered 都相同的原文回声不再重复出现。
    r.retain(|rc| {
        !en.iter()
            .any(|ec| ec.word == rc.word && ec.covered == rc.covered)
    });

    let cap = max_candidates().saturating_add(en.len().min(8));
    let mut out: Vec<Candidate> = Vec::with_capacity(cap.min(z.len() + en.len() + r.len()));
    let (mut zi, mut ei) = (0usize, 0usize);
    for slot_en in template(band) {
        if *slot_en {
            if let Some(c) = en.get(ei) {
                out.push(c.clone());
                ei += 1;
            }
        } else if let Some(c) = z.get(zi) {
            out.push(c.clone());
            zi += 1;
        }
    }
    out.extend(z.into_iter().skip(zi));
    out.extend(en.into_iter().skip(ei));
    out.extend(r);
    out.truncate(cap);
    out
}
