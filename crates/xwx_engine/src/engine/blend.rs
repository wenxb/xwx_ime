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

/// 中文证据：是否存在「带词条频次证据的完整全拼命中」。
///
/// 判据（第 7 节，逐条都要满足）：非回声、非英文、`spelling == Exact`、
/// `covered == 原始输入长度`、拼音去掉音节空格后逐字等于当前小写输入、
/// `freq > FILLER_FREQ_TIER`。
///
/// 补全、简拼、纠错、`freq = 0` 的拼装句都不能靠覆盖整串伪造该证据。
pub(crate) fn strong_chinese(zh: &[Candidate], raw_input: &str) -> bool {
    let lower = raw_input.to_ascii_lowercase();
    zh.iter().any(|c| {
        !c.echo
            && !c.is_english()
            && c.spelling == crate::candidate::SpellingKind::Exact
            && c.covered == raw_input.len()
            && c.freq > super::filler_freq_tier()
            && c.pinyin.replace(' ', "") == lower
    })
}

/// `good_en`：首要英文候选有足够证据（静态频率够高，或用户学过至少 3 次）。
pub(crate) fn good_english(e0_zipf: u16, e0_user_unigram: u32) -> bool {
    e0_zipf >= 300 || e0_user_unigram >= 3
}

/// `strong_completion`：输入长度 ≥3、extra ≤4、且补全不超过词长的一半。
pub(crate) fn strong_completion(input_len: usize, target_len: usize) -> bool {
    let extra = target_len.saturating_sub(input_len);
    input_len >= 3 && extra <= 4 && 2 * input_len >= target_len
}

/// 按决策表定档（命中即停）。
///
/// `zh` 是中文通道的完整结果，`en` 是英文通道的最终候选（可空）；
/// `recent_en` = 存在可信英文上文。
pub(crate) fn classify_intent(
    raw_input: &str,
    zh: &[Candidate],
    en: &[Candidate],
    e0_zipf: u16,
    e0_user_unigram: u32,
    recent_en: bool,
) -> Option<IntentBand> {
    if en.is_empty() {
        return None; // 不混排，原样返回 zh
    }
    let z_nonempty = zh.iter().any(|c| !c.echo);
    if !z_nonempty {
        return Some(IntentBand::EnglishFirst);
    }
    let e0 = &en[0];
    let input_len = raw_input.len();
    let good_en = good_english(e0_zipf, e0_user_unigram);
    if recent_en && good_en {
        return Some(IntentBand::EnglishFirst);
    }
    if !recent_en && input_len <= 2 {
        return Some(IntentBand::Mixed); // 仅精确英文能到这里
    }
    let strong_zh = strong_chinese(zh, raw_input);
    let e0_exact = e0.english_match_kind() == Some(EnglishMatchKind::Exact);
    if strong_zh && e0_exact && good_en {
        return Some(IntentBand::Mixed);
    }
    if strong_zh {
        return Some(IntentBand::ChineseFirst);
    }
    let target_len = e0.english_key().map(str::len).unwrap_or(input_len);
    if good_en && e0_exact {
        return Some(IntentBand::EnglishFirst);
    }
    if good_en && strong_completion(input_len, target_len) {
        return Some(IntentBand::EnglishFirst);
    }
    if good_en {
        return Some(IntentBand::Mixed);
    }
    Some(IntentBand::ChineseFirst)
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
