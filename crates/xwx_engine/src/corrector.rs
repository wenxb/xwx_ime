//! 拼音纠错：常见混淆对 + 模糊展开（fuzzy expansion）。
//!
//! 很多方言区用户 n/l、zh/z、sh/s、an/ang 不分，模糊拼音（如搜狗、QQ 拼音的
//! "模糊音"）会把输入串按混淆对展开成多个候选拼音再去查词。例如开了
//! n/l 模糊后输入 `"ni"`，会同时查 `"ni"` 和 `"li"`。
//!
//! 本模块只做**纯文本展开**，不涉及词典/模型——展开结果交给上层引擎
//! 逐个查词并合并打分。

use std::collections::HashMap;
#[cfg(test)]
use std::collections::HashSet;

/// 一对易混淆拼音串（展开时双向替换：`a -> b` 与 `b -> a` 都会尝试）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConfusionPair {
    /// 混淆串 A，如 `"n"`。
    pub a: &'static str,
    /// 混淆串 B，如 `"l"`。
    pub b: &'static str,
    /// 展开后的惩罚权重（0~1，越小代表越不常见，排序时对应降权）。
    /// TODO: 接入 `lib.rs` 的候选打分，当前仅作配置占位。
    pub weight: f32,
}

impl ConfusionPair {
    /// 构造混淆对。
    pub const fn new(a: &'static str, b: &'static str, weight: f32) -> Self {
        Self { a, b, weight }
    }
}

/// 常见拼音混淆对（参照主流输入法的模糊音设置，权重为经验值）。
///
/// TODO: 整理成可配置项（配置文件里开关），并补充更多方言混淆
/// （如 z/zh 与 c/ch 的组合、ü 相关、前后鼻音混合等）。
pub const COMMON_CONFUSION_PAIRS: &[ConfusionPair] = &[
    ConfusionPair::new("n", "l", 0.8), // 南京/四川等：你=里
    ConfusionPair::new("zh", "z", 0.8), // 平翘舌
    ConfusionPair::new("ch", "c", 0.8),
    ConfusionPair::new("sh", "s", 0.8),
    ConfusionPair::new("an", "ang", 0.5), // 前后鼻音
    ConfusionPair::new("en", "eng", 0.5),
    ConfusionPair::new("in", "ing", 0.5),
    ConfusionPair::new("f", "h", 0.3), // 福建等：花=发
    ConfusionPair::new("r", "l", 0.3), // 部分吴语区
    ConfusionPair::new("k", "g", 0.3), // 部分方言清浊不分
    ConfusionPair::new("c", "q", 0.2), // 部分方言 c/q 不分
    ConfusionPair::new("z", "j", 0.2), // 部分方言 z/j 不分
    ConfusionPair::new("s", "x", 0.2), // 部分方言 s/x 不分
];

/// 单个输入串最多展开出的变体数（防止组合爆炸）。
const MAX_EXPANSIONS: usize = 64;
/// 替换链最大深度（`an`→`ang`→`an` 之类会形成环，必须限深）。
const MAX_DEPTH: usize = 4;

/// 把输入串按混淆对做模糊展开,返回去重后的全部变体。
///
/// - 原串**始终排第一**(精确匹配优先);
/// - 其余变体按发现顺序(深度优先、对组顺序)排列,结果确定;
/// - 空输入返回空。
///
/// 例(开 n/l + an/ang):`"nan"` -> `["nan", "nang", "lan", "lang"]`。
///
/// 只返回字符串、不带代价;排序需要惩罚信息时用
/// [`expand_fuzzy_scored`]。
pub fn expand_fuzzy(input: &str, pairs: &[ConfusionPair]) -> Vec<String> {
    expand_fuzzy_scored(input, pairs)
        .into_iter()
        .map(|(variant, _penalty)| variant)
        .collect()
}

/// 把输入串按混淆对做模糊展开,返回去重后的全部变体及其累计惩罚。
///
/// 返回 `(变体, 累计惩罚)` 列表:
///
/// - 原串**始终排第一**,惩罚为 `0.0`(精确匹配零惩罚);
/// - 其余变体的惩罚是沿替换链**累加**的对数域代价:
///   每应用一次混淆对 `i`,加上 `-log10(weight_i)`;
/// - 同一变体若可由多条替换链到达(如 `"nang"` 既可 `nan→nang`
///   一步 `an→ang`,也可 `nan→lan→lal→nal→nalg→nang` 绕路),
///   记录**所有链中代价最小**的那条——绕远路不应惩罚更重。
///
/// 惩罚公式推导:词频/LM 分都是 `log10` 概率(负值),模糊惩罚要在
/// 同一量纲做加性扣分。把混淆对的可信度 `weight ∈ (0,1]` 解释成
/// 「这个变体相对精确输入的条件概率」,则其对数代价
/// `-log10(weight)` 正好是同量纲的加性惩罚:
///
/// - `weight = 0.8`(n/l、zh/z) → 惩罚 ≈ 0.097,约等于词频差
///   1.25 倍——温和降权,可信混淆基本不挡路;
/// - `weight = 0.5`(an/ang) → 惩罚 ≈ 0.30,约等于词频差 2 倍;
/// - `weight = 0.2`(s/x) → 惩罚 ≈ 0.70,约等于词频差 5 倍——
///   明显降权但不一票否决,高频词仍能浮上来;
/// - 多处混淆叠加时惩罚累加(`-log10(w1) + -log10(w2)`),符合
///   「打两个错音比一个错音更不可信」的直觉,也正好是联合概率
///   `w1 × w2` 的对数。
///
/// 防爆炸上限 [`MAX_EXPANSIONS`] / [`MAX_DEPTH`] 与 [`expand_fuzzy`]
/// 一致(同一 DFS 骨架,只是 `seen` 升级为最小代价表)。
pub fn expand_fuzzy_scored(input: &str, pairs: &[ConfusionPair]) -> Vec<(String, f32)> {
    if input.is_empty() {
        return Vec::new();
    }
    // best:变体 → 当前已知最小代价(松弛更新);
    // order:变体首次被发现的顺序(保证返回顺序确定,与原 DFS 一致)。
    let mut best: HashMap<String, f32> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    let original = input.to_string();
    best.insert(original.clone(), 0.0);
    order.push(original);

    // 深度优先展开:每替换一处混淆串就生成一个新变体并继续展开,
    // 惩罚沿链累加;发现更小代价的到达链时松弛更新并重新展开。
    expand_inner(input, pairs, &mut best, &mut order, 0, 0.0);

    order
        .into_iter()
        .map(|variant| {
            let penalty = best[&variant];
            (variant, penalty)
        })
        .collect()
}

/// DFS 主体。`best` 记录每个变体的最小代价(同时充当 seen 角色),
/// `order` 记录首次发现顺序;`cost` 是沿当前替换链累计的惩罚。
///
/// 与旧版 `seen: HashSet` 的差别:旧版「首次发现即定死」,绕远路
/// 先到达的变体会被记上偏高的代价;这里改为「发现更小代价就更新
/// 并继续从它展开」(SPFA 式松弛),保证每个变体拿到全图最小惩罚。
/// 终止性由 [`MAX_DEPTH`] / [`MAX_EXPANSIONS`] 保证;代价严格递减
/// 才更新,不会无限循环。
fn expand_inner(
    current: &str,
    pairs: &[ConfusionPair],
    best: &mut HashMap<String, f32>,
    order: &mut Vec<String>,
    depth: usize,
    cost: f32,
) {
    if depth >= MAX_DEPTH || order.len() >= MAX_EXPANSIONS {
        return;
    }
    for pair in pairs {
        // 双向替换:a->b 与 b->a
        for (from, to) in [(pair.a, pair.b), (pair.b, pair.a)] {
            if from.is_empty() || from == to {
                continue;
            }
            // 单次替换的对数域代价:weight 越高(越可信)惩罚越小。
            // weight 恒在 (0,1],log10 有限且 ≤ 0,取负得非负惩罚。
            let step = -pair.weight.log10();
            // 逐个找到所有出现位置并替换(ASCII 拼音,字节索引安全)
            let mut start = 0;
            while let Some(rel) = current[start..].find(from) {
                let idx = start + rel;
                let mut next = String::with_capacity(current.len() + to.len());
                next.push_str(&current[..idx]);
                next.push_str(to);
                next.push_str(&current[idx + from.len()..]);

                let new_cost = cost + step;
                match best.get(&next) {
                    // 首次发现:记录代价并递归展开
                    None => {
                        best.insert(next.clone(), new_cost);
                        order.push(next.clone());
                        expand_inner(&next, pairs, best, order, depth + 1, new_cost);
                    }
                    // 松弛:发现更小代价的到达链,更新并重新展开
                    Some(&old_cost) if new_cost < old_cost - f32::EPSILON => {
                        best.insert(next.clone(), new_cost);
                        expand_inner(&next, pairs, best, order, depth + 1, new_cost);
                    }
                    _ => {}
                }
                // 跳过已替换位置,避免原地打转
                start = idx + from.len();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_first() {
        let v = expand_fuzzy("ni", COMMON_CONFUSION_PAIRS);
        assert_eq!(v.first().map(String::as_str), Some("ni"));
        assert!(v.iter().any(|s| s == "li"), "应包含 n->l 的变体");
    }

    #[test]
    fn both_directions() {
        let v = expand_fuzzy("li", COMMON_CONFUSION_PAIRS);
        assert!(v.iter().any(|s| s == "ni"), "应包含 l->n 的变体");
    }

    #[test]
    fn nested_replacement() {
        // an->ang 与 n->l 可组合出 lang
        let v = expand_fuzzy("nan", &[
            ConfusionPair::new("n", "l", 0.8),
            ConfusionPair::new("an", "ang", 0.5),
        ]);
        for want in ["nan", "nang", "lan", "lang"] {
            assert!(v.iter().any(|s| s == want), "缺少变体 {want}: {v:?}");
        }
    }

    #[test]
    fn no_duplicates() {
        let v = expand_fuzzy("shang", COMMON_CONFUSION_PAIRS);
        let uniq: HashSet<&String> = v.iter().collect();
        assert_eq!(uniq.len(), v.len());
    }

    #[test]
    fn scored_original_has_zero_penalty() {
        let v = expand_fuzzy_scored("ni", COMMON_CONFUSION_PAIRS);
        assert_eq!(v.first().map(|(s, p)| (s.as_str(), *p)), Some(("ni", 0.0)));
    }

    #[test]
    fn scored_penalty_is_additive_log10() {
        // 单组混淆:n/l = 0.8 → 惩罚 -log10(0.8) ≈ 0.0969
        let v = expand_fuzzy_scored("ni", &[ConfusionPair::new("n", "l", 0.8)]);
        let li = v.iter().find(|(s, _)| s == "li").expect("应包含 li");
        assert!((li.1 - 0.0969).abs() < 1e-3, "n->l 惩罚应≈0.097, 实际 {}", li.1);
    }

    #[test]
    fn scored_multi_substitution_accumulates() {
        // 两处混淆叠加:"nan" 同时 n->l 且 an->ang → lang,
        // 惩罚 = -log10(0.8) + -log10(0.5) ≈ 0.0969 + 0.3010 = 0.3979
        let v = expand_fuzzy_scored("nan", &[
            ConfusionPair::new("n", "l", 0.8),
            ConfusionPair::new("an", "ang", 0.5),
        ]);
        let lang = v.iter().find(|(s, _)| s == "lang").expect("应包含 lang");
        assert!(
            (lang.1 - 0.3979).abs() < 1e-3,
            "双混淆惩罚应≈0.398, 实际 {}",
            lang.1
        );
        // 单混淆的 lan / nang 惩罚应各自小于 lang
        let lan = v.iter().find(|(s, _)| s == "lan").expect("应包含 lan");
        let nang = v.iter().find(|(s, _)| s == "nang").expect("应包含 nang");
        assert!(lan.1 < lang.1 && nang.1 < lang.1);
    }

    #[test]
    fn scored_matches_unscored_variants() {
        // 同一输入下,带分版本与不带分版本应产出完全相同的变体集合与顺序
        for input in ["ni", "li", "nan", "shang", "zhong"] {
            let plain = expand_fuzzy(input, COMMON_CONFUSION_PAIRS);
            let scored = expand_fuzzy_scored(input, COMMON_CONFUSION_PAIRS);
            let scored_variants: Vec<&str> = scored.iter().map(|(s, _)| s.as_str()).collect();
            let plain_variants: Vec<&str> = plain.iter().map(String::as_str).collect();
            assert_eq!(scored_variants, plain_variants, "{input} 两版变体应一致");
        }
    }
}
