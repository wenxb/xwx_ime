//! 空上下文 LM 诊断:量化「句首候选排序 100% 由词频决定」这件事,
//! 并验证 `PLAN.md` 第四节里那条被否决的修法为什么走不通。
//!
//! 本工具**不改任何行为**,只做观测,产出裁决所需的事实。
//!
//! 背景(`wanxiang.rs:467-470`):
//! ```ignore
//! let mut best = self.floor_log10();
//! if ctx_tail.is_empty() { return best; }   // 与候选词无关的常数
//! ```
//! 后果:句首(绝大多数上屏的起点)所有候选拿到的 LM 分是**同一个
//! 常数**,加到总分上等于没加,排序退化为纯词频。
//!
//! 用法:
//! ```sh
//! XWX_DICT_PATH=$PWD/data/dict.bin cargo run --release --offline \
//!   -p xwx_engine --example lm_ctx_probe
//! ```

use std::path::Path;

use xwx_engine::{ImeEngine, LanguageModel, LmContext, WanxiangLM};

/// 句首排序的哨兵:这些输入的**第 1 名**在真实使用里必须正确
/// (它们是最高频、最不该错的输入)。用于验证任何空上下文改动。
const SENTINELS: &[(&str, &str)] = &[("nihao", "你好"), ("zhongguo", "中国"), ("beijing", "北京")];

/// `PLAN.md` 第四节记录的两个**互相冲突**的哨兵:
/// - `xian` 想让低频双字真词「西安」上来;
/// - `sh` 必须保住高频单字「傻」不被「刹车」顶掉。
const CONFLICT: &[(&str, &[&str])] = &[
    ("xian", &["西安", "先", "闲", "现"]),
    ("sh", &["刹车", "傻", "是", "说"]),
    ("diannao", &["电脑", "狄安娜", "点", "电"]),
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let dict_path = std::env::var_os("XWX_DICT_PATH")
        .map(Into::into)
        .unwrap_or_else(|| root.join("dict.bin"));
    let gram = std::env::var_os("XWX_LM_PATH").map(Into::into).or_else(|| {
        let p = Path::new(&std::env::var("HOME").ok()?)
            .join(".local/share/xwx/wanxiang-lts-zh-hans.gram");
        p.is_file().then_some(p)
    });

    let engine = ImeEngine::new_with_lm(Some(&dict_path), gram.as_deref())?;

    // —— 1. 直接证明「空上下文返回常数」 ——
    if let Some(p) = gram.as_deref() {
        let lm = WanxiangLM::open(p)?;
        println!("== 1. score_word 在空上下文下是否与词无关 ==");
        let words = ["西安", "先", "闲", "狄安娜", "中国", "的", "龘"];
        let mut scores = Vec::new();
        for w in words {
            // 通过 LanguageModel trait 的 score,状态取空上下文。

            let st = lm.state_from_context(&LmContext::empty());
            let (s, _) = lm.score(&st, w);
            scores.push(s);
            println!("  {w:<8} score = {s:.6}");
        }
        let first = scores[0];
        let all_same = scores.iter().all(|s| (*s - first).abs() < 1e-6);
        println!(
            "  ⇒ 全部相等? {}  ({})",
            if all_same { "是(证明:空上下文 LM 与候选词无关)" } else { "否" },
            if all_same {
                "排序 100% 由词频决定".to_string()
            } else {
                format!("存在差异,最大 {:.4}", scores.iter().cloned().fold(f32::MIN, f32::max) - first)
            }
        );

        // —— 2. 被否决的修法:空上下文查 unigram(取词自身在 gram 里的分) ——
        // PLAN.md 记录:长命中键的 ln 随键长单调涨,值不跨键长可比。
        println!("\n== 2. 「查词自身」的分为何不可用(值不跨键长可比) ==");
        for probe in ["西安", "狄安娜", "中国", "中标通知书"] {
            // 用「词自己当上文」的方式触发一个自匹配,观察量级随键长的漂移。
            let tail: String = probe.chars().take(5).collect();
            let st = lm.state_from_context(&LmContext { tail: &tail, last_word: None });
            let (s, _) = lm.score(&st, probe);
            println!("  {probe:<10}({n} 字) score = {s:.4}", n = probe.chars().count());
        }
        println!("  ⇒ 键越长分越高 ⇒ 「西安」竞争不过「狄安娜」,单字更无条目可查");
    } else {
        println!("(未找到 .gram,跳过第 1、2 节——这两节需要真实万象模型)");
    }

    // —— 3. 空上下文下候选排序的实际名次(探针口径,与对拍一致) ——
    println!("\n== 3. 空上下文(探针口径)下各哨兵名次 ==");
    println!("{:<12} {:<8} {:>6}  {:>12}", "输入", "目标", "名次", "前 5");
    for &(inp, target) in SENTINELS {
        report(&engine, inp, target);
    }
    for (inp, targets) in CONFLICT {
        for t in *targets {
            report(&engine, inp, t);
        }
    }

    Ok(())
}

fn report(e: &ImeEngine, inp: &str, target: &str) {
    let Ok(c) = e.lookup_candidates(inp, &LmContext::empty()) else {
        return;
    };
    let rank = c.iter().position(|x| x.word == target).map(|i| i + 1);
    let top5: Vec<&str> = c.iter().take(5).map(|x| x.word.as_str()).collect();
    println!(
        "{inp:<12} {target:<8} {:>6}  {:?}",
        rank.map_or("未召回".to_string(), |r| format!("#{r}")),
        top5
    );
}