//! 中英意图分档与**稳定混排**。
//!
//! 决策表见 `docs/english-input/02-engine.md` 第 8 节，模板见第 9 节。
//! 这里**不调用中文排序器**：中文候选进来什么样，出去还是什么样（只在
//! 原文兜底与重复消除上动位置）。

use crate::candidate::Candidate;

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

/// **中文可读**：中文通道里有「拼音真的以输入开头、且有真实词频」的候选。
///
/// 两个条件缺一不可：
///
/// - **拼音前缀**：`rug`→如果（`ruguo` 以 `rug` 开头）算；`hello`→黑龙
///   （`heilong` 第 3 个字母就不对）不算。
/// - **真实词频**：简拼通道能把任何字母串拼成中文（`thank` → 他和安康），
///   那种拼装链的 `freq` 是 0，拿它当判据的话一切英文输入都成了"可能是
///   中文"，英文就永远出不来。要求 `freq > 填充档` 把它们挡掉。
///
/// 两个一起，`rug`/`bir`/`cache` 判为中文可读，`python`/`hello`/`thank`
/// （`th` 不是声母）判不出来——正好是用户要的分界。
fn plausible_chinese(zh: &[Candidate], raw_input: &str) -> bool {
    let lower = raw_input.to_ascii_lowercase();
    zh.iter()
        .any(|c| {
            !c.echo
                && !c.is_english()
                && c.freq > super::filler_freq_tier()
                && c.pinyin.replace(' ', "").starts_with(&lower)
        })
}

/// `good_en`：首要英文候选有足够证据（静态频率够高，或用户学过至少 3 次）。
pub(crate) fn good_english(e0_zipf: u16, e0_user_unigram: u32) -> bool {
    e0_zipf >= 300 || e0_user_unigram >= 3
}

/// 按决策表定档（命中即停）。
///
/// `zh` 是中文通道的完整结果，`en` 是英文通道的最终候选（可空）；
/// `recent_en` = 存在可信英文上文。
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
    if recent_en && good_en {
        return Some(IntentBand::EnglishFirst); // 英文串进行中
    }
    if raw_input.chars().count() <= 2 {
        // 1~2 个字母是真歧义区（`ok`/`us` 这类英文词 vs 两键拼音缩写），
        // 交替混排：中文在前，英文仍在第 2 位可点。
        return Some(IntentBand::Mixed);
    }
    if plausible_chinese(zh, raw_input) {
        // **中文可读 ⇒ 中文优先**（用户定的方向）：输入既然有真拼音读法
        // （`rug`→如果、`bir`→必然），就更可能是在打中文。「不明确的英文」
        // 不要抢前排，退到末位保留入口。
        //
        // 分界靠 `Exact` 这一档：英文几乎没有辅音连缀，`python`/`hello`/
        // `thank`（`th` 不是声母）不可能被拼音读出来，走不到这里——英文
        // 照常排前。受影响的只是**同时是合法拼音前缀**的那批输入。
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
