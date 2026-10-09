//! M1 英文内部排序。
//!
//! 公式与排序键见 `docs/english-input/02-engine.md` 第 6 节。**不与中文
//! score 比较**：英文分只决定英文通道内部次序，跨语言次序由混排模板决定。

use crate::candidate::{Candidate, EnglishMatchKind};
use crate::user_lm::UserLmSnapshot;

use super::context::EnglishContext;

/// 排序后最多展示的英文候选数（与召回池上限是两个不同的数）。
pub const ENGLISH_DISPLAY_LIMIT: usize = 8;
/// 一元用户计数的封顶（超出不再增益，避免一个词学多了永久压住全部英文）。
const USER_COUNT_CAP: u32 = 1000;
/// 一元增益系数。
const UNIGRAM_GAIN: f32 = 0.6;
/// 二元增益系数。
const BIGRAM_GAIN: f32 = 0.9;

/// M2 条件基础项：有该 prev 行时用平滑频率估计，否则返回 `None`
/// （调用方退回 M1 的 `z = zipf_centi / 100`）。
///
/// ```text
/// p0 = 10^(z - 9)
/// p  = 0.8 × c/T + 0.2 × p0
/// lexical = 9 + log10(p)
/// ```
///
/// `c = 0`（词对未收录）时 `p = 0.2 × p0 > 0`，所以结果永远有限——
/// **绝不会出现 log10(0) 或负无穷**。0.8/0.2 是待调参起点，不是最优。
pub(crate) fn conditional_base(zipf_centi: u16, count: u64, observed_total: u64) -> Option<f32> {
    if observed_total == 0 {
        return None; // 没有这一行：基础项直接用 z
    }
    let z = zipf_centi as f64 / 100.0;
    let p0 = 10f64.powf(z - 9.0);
    let p = 0.8 * (count as f64 / observed_total as f64) + 0.2 * p0;
    if !(p > 0.0) || !p.is_finite() {
        return None; // 防御：任何非正/非有限值都退回 M1 的 z
    }
    let lexical = 9.0 + p.log10();
    lexical.is_finite().then_some(lexical as f32)
}

/// 一条召回命中的英文词（去重后、打分前）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EnglishHit {
    /// 完整单词的小写键。
    pub key: String,
    /// 规范显示形式。
    pub display: String,
    /// 百分之一 Zipf 单位的静态频率。
    pub zipf_centi: u16,
    /// 匹配方式（`key == input_key` 即 Exact）。
    pub match_kind: EnglishMatchKind,
    /// 用户一元计数。
    pub user_unigram: u32,
    /// 用户二元计数（`上文 → 该词`）。
    pub user_bigram: u32,
    /// M2 的静态条件基础项（`None` = 没有该 prev 行，用 M1 的 z）。
    pub static_base: Option<f32>,
}

/// 英文分：`S = z - 0.15 × extra + U + B`。
///
/// 实现按文档要求走**整数 base**：先算 `base_centi = q - 15 × extra`，
/// 再除以 100.0 加 U/B——无学习时的同分关系因此与离线 Top-32 缓存一致，
/// 不受两次浮点舍入影响。
pub(crate) fn english_score(hit: &EnglishHit, input_len: usize) -> f32 {
    let extra = hit.key.len().saturating_sub(input_len);
    // 没有静态二元行时走**整数** base：同分关系与离线 Top-32 缓存逐位一致，
    // 不受两次浮点舍入影响（M1 的行为不能因为 M2 而漂移）。
    let base = match hit.static_base {
        Some(b) => b - 0.15 * extra as f32,
        None => (hit.zipf_centi as i32 - 15 * extra as i32) as f32 / 100.0,
    };
    base + unigram_gain(hit.user_unigram) + bigram_gain(hit.user_bigram)
}

/// `U = 0.6 × log10(1 + min(u, 1000))`；0 次严格加 0。
pub(crate) fn unigram_gain(u: u32) -> f32 {
    if u == 0 {
        return 0.0;
    }
    UNIGRAM_GAIN * (1.0 + u.min(USER_COUNT_CAP) as f32).log10()
}

/// `B = 0.9 × log10(1 + min(b, 1000))`；0 次严格加 0。
pub(crate) fn bigram_gain(b: u32) -> f32 {
    if b == 0 {
        return 0.0;
    }
    BIGRAM_GAIN * (1.0 + b.min(USER_COUNT_CAP) as f32).log10()
}

/// 排好序（并已截断到展示上限）的英文候选，连同分数与原始命中。
///
/// 意图判定要用 `E0` 的**静态频率与目标词长**——这些不在 `Candidate` 的
/// 元数据里（元数据只有 key 与 match_kind），所以命中本身要一起带出来。
#[derive(Debug, Clone)]
pub(crate) struct RankedEnglish {
    /// 已排序、已截断到 [`ENGLISH_DISPLAY_LIMIT`] 的命中。
    pub hits: Vec<EnglishHit>,
    /// 与 `hits` 同序的分数。
    pub scores: Vec<f32>,
}

impl RankedEnglish {
    /// 首要英文候选。
    pub(crate) fn first(&self) -> Option<&EnglishHit> {
        self.hits.first()
    }

    /// 转成混排用的候选（`covered` = 原始输入字节数）。
    pub(crate) fn candidates(&self, input_len: usize) -> Vec<Candidate> {
        self.hits
            .iter()
            .zip(self.scores.iter())
            .map(|(h, s)| {
                Candidate::new_english(h.display.clone(), h.key.clone(), h.match_kind, *s, input_len)
            })
            .collect()
    }
}

/// 把召回结果排成最终英文候选（最多 [`ENGLISH_DISPLAY_LIMIT`] 条）。
///
/// 排序键：① Exact 在 Completion 前；② S 降序；③ extra 升序；
/// ④ `zipf_centi` 降序；⑤ key 字节序升序。有限值全序 + 确定性收尾键，
/// 不用 epsilon 比较器。
pub(crate) fn rank_english(
    hits: Vec<EnglishHit>,
    input_len: usize,
    ctx: &EnglishContext<'_>,
) -> RankedEnglish {
    let _ = ctx; // M1 的上文只通过用户二元计数体现;公式本身不额外读 ctx
    let mut scored: Vec<(f32, EnglishHit)> = hits
        .into_iter()
        .map(|h| (english_score(&h, input_len), h))
        .collect();
    scored.sort_by(|(sa, a), (sb, b)| {
        let a_exact = a.match_kind == EnglishMatchKind::Exact;
        let b_exact = b.match_kind == EnglishMatchKind::Exact;
        b_exact
            .cmp(&a_exact)
            .then_with(|| sb.total_cmp(sa))
            .then_with(|| extra(a, input_len).cmp(&extra(b, input_len)))
            .then_with(|| b.zipf_centi.cmp(&a.zipf_centi))
            .then_with(|| a.key.cmp(&b.key))
    });
    scored.truncate(ENGLISH_DISPLAY_LIMIT);
    let (scores, hits): (Vec<f32>, Vec<EnglishHit>) = scored.into_iter().unzip();
    RankedEnglish { hits, scores }
}

fn extra(h: &EnglishHit, input_len: usize) -> usize {
    h.key.len().saturating_sub(input_len)
}

/// 从快照读用户计数（召回阶段一次取一代）。
pub(crate) fn user_counts(
    snapshot: &UserLmSnapshot,
    ctx: &EnglishContext<'_>,
    key: &str,
) -> (u32, u32) {
    let uni = snapshot.english_unigram_count(key);
    let bi = ctx
        .previous_word
        .map(|p| snapshot.english_bigram_count(p, key))
        .unwrap_or(0);
    (uni, bi)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(key: &str, q: u16, kind: EnglishMatchKind, u: u32, b: u32) -> EnglishHit {
        EnglishHit {
            key: key.to_string(),
            display: key.to_string(),
            zipf_centi: q,
            match_kind: kind,
            user_unigram: u,
            user_bigram: b,
            static_base: None,
        }
    }

    #[test]
    fn conditional_base_matches_the_hand_computed_formula() {
        // 手算例：z=4.72（hello 的 q=472）、c=7_852_480、T=2_718_215_040
        // p0 = 10^(4.72-9) = 1.9055e-5
        // p  = 0.8×0.002889 + 0.2×1.9055e-5 = 0.0023151
        // lexical = 9 + log10(0.0023151) = 6.36449
        let got = conditional_base(472, 7_852_480, 2_718_215_040).expect("有行");
        assert!((got - 6.36449).abs() < 1e-4, "lexical={got}");
    }

    #[test]
    fn conditional_base_is_finite_for_unseen_pairs_and_when_there_is_no_row() {
        // c = 0：不能是负无穷，只能是 z - log10(5) ≈ z - 0.699
        let got = conditional_base(472, 0, 1_000_000).expect("有行");
        assert!(got.is_finite());
        assert!((got - (4.72 - 0.69897)).abs() < 1e-4, "未收录词对={got}");
        // 没有这一行：退回 None（调用方用 z），不做指数/对数往返
        assert_eq!(conditional_base(472, 0, 0), None);
    }

    #[test]
    fn conditional_base_never_exceeds_the_observed_distribution() {
        // c = T（该 prev 只连这一个词）：p = 0.8 + 0.2p0，lexical ≈ 9 + log10(0.8)
        let got = conditional_base(900, 1_000_000, 1_000_000).expect("有行");
        assert!(got < 9.0, "lexical 不得超过 9（log10(p≤1)）: {got}");
        assert!((got - (9.0 + 0.8f64.log10()) as f32).abs() < 1e-3, "{got}");
    }

    #[test]
    fn conditional_base_makes_a_strong_pair_beat_a_stronger_unpaired_word() {
        // 这是 M2 存在的理由：`thank → you`(T 很大) 的 lexical 必须高过
        // 一个静态频率更高、但没有该 prev 行的词退化成 z 的结果。
        let paired = conditional_base(500, 733_376_704, 800_000_000).expect("有行");
        let unpaired_z = 5.60f32; // 假设另一个词的 z 更高
        assert!(
            paired > unpaired_z,
            "成对证据必须能压过更高的裸词频: {paired} vs {unpaired_z}"
        );
    }

    #[test]
    fn pyth_completion_matches_hand_computed_score() {
        // 手算例（数据分册/引擎分册）：Python q=410，输入 pyth
        // extra = 6 - 4 = 2，S = 4.10 - 0.15×2 = 3.80
        let h = hit("python", 410, EnglishMatchKind::Completion, 0, 0);
        assert!((english_score(&h, 4) - 3.80).abs() < 1e-6);
    }

    #[test]
    fn hello_exact_matches_hand_computed_score() {
        // hello q=450，输入 hello，u=b=0 ⇒ S = 4.50
        let h = hit("hello", 450, EnglishMatchKind::Exact, 0, 0);
        assert!((english_score(&h, 5) - 4.50).abs() < 1e-6);
    }

    #[test]
    fn zero_counts_add_exactly_zero() {
        assert_eq!(unigram_gain(0), 0.0);
        assert_eq!(bigram_gain(0), 0.0);
        let h = hit("hello", 450, EnglishMatchKind::Exact, 0, 0);
        assert_eq!(english_score(&h, 5), 4.5);
    }

    #[test]
    fn learning_a_word_never_lowers_its_score() {
        let mut prev = 0.0f32;
        for u in 0..50u32 {
            let h = hit("hello", 450, EnglishMatchKind::Exact, u, u);
            let s = english_score(&h, 5);
            assert!(s >= prev, "u={u}: {s} < {prev}");
            prev = s;
        }
    }

    #[test]
    fn exact_sort_key_beats_higher_score_completion() {
        // C04：精确优先于更高频补全——`hello` 精确排在某高频补全之前。
        let hits = vec![
            hit("helloworld", 899, EnglishMatchKind::Completion, 0, 0),
            hit("hello", 100, EnglishMatchKind::Exact, 0, 0),
        ];
        let out = rank_english(hits, 5, &EnglishContext::empty()).candidates(5);
        assert_eq!(out[0].word, "hello");
        assert_eq!(out[0].english_key(), Some("hello"));
    }

    #[test]
    fn ties_break_by_extra_then_zipf_then_key() {
        // 同 S：短 extra 先；同 extra：zipf 高先；全同：key 字节序升序
        let hits = vec![
            hit("abcde", 500, EnglishMatchKind::Completion, 0, 0),
            hit("abc", 500, EnglishMatchKind::Completion, 0, 0),
            hit("abce", 500, EnglishMatchKind::Completion, 0, 0),
        ];
        let out = rank_english(hits, 3, &EnglishContext::empty()).candidates(3);
        let keys: Vec<&str> = out.iter().filter_map(|c| c.english_key()).collect();
        assert_eq!(keys, ["abc", "abce", "abcde"]);
    }

    #[test]
    fn display_limit_is_eight_and_order_is_deterministic() {
        let hits: Vec<EnglishHit> = (0..40)
            .map(|i| {
                let k = format!("w{}{}", (b'a' + (i / 26) as u8) as char, (b'a' + (i % 26) as u8) as char);
                hit(&k, 300 + i as u16, EnglishMatchKind::Completion, 0, 0)
            })
            .collect();
        let a = rank_english(hits.clone(), 1, &EnglishContext::empty());
        let b = rank_english(hits, 1, &EnglishContext::empty());
        assert_eq!(a.hits.len(), ENGLISH_DISPLAY_LIMIT);
        assert_eq!(a.hits, b.hits);
    }

    #[test]
    fn scores_are_always_finite() {
        for (q, u, b, len) in [(0u16, 0u32, 0u32, 48usize), (900, 1000, 1000, 1), (900, u32::MAX, u32::MAX, 0)] {
            let h = hit("abcdefgh", q, EnglishMatchKind::Completion, u, b);
            assert!(english_score(&h, len).is_finite(), "q={q} u={u} b={b}");
        }
    }
}
