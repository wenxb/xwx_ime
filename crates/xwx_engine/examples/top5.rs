//! 真实词库（dict.bin）+ 真实万象语言模型（wanxiang-lts-zh-hans.gram）下的
//! 候选 top-5 与性能实测（第 3 步验收用）。
//!
//! 用法：
//!   cargo run --manifest-path crates/xwx_engine/Cargo.toml --example top5 -- [dict.bin] [.gram]
//!
//! 缺省路径：`~/.local/share/xwx/` 下的 `dict.bin` 与
//! `wanxiang-lts-zh-hans.gram`，可用 `XWX_DICT_PATH` / `XWX_LM_PATH`
//! 或命令行参数覆盖。需要这两个真实文件（测试不依赖它们，此例依赖）。

use std::path::PathBuf;

use xwx_engine::{ImeEngine, LanguageModel, LmContext, LmStore};

const INPUTS: [&str; 4] = ["nihao", "womenjintianquxian", "zhongguoren", "xian"];

fn resolve_path(cli: Option<&str>, env_key: &str, file: &str) -> PathBuf {
    if let Some(p) = cli {
        return PathBuf::from(p);
    }
    if let Some(p) = std::env::var_os(env_key) {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"))
        })
        .unwrap_or_default();
    base.join("xwx").join(file)
}

fn top5(engine: &ImeEngine, pinyin: &str, label: &str) {
    let cands = engine
        .lookup_candidates(pinyin, &LmContext::empty())
        .expect("查询失败");
    println!("{label}：{pinyin}");
    for (i, c) in cands.iter().take(5).enumerate() {
        println!(
            "  {}. {}  (freq={}, 路径分={:.3})",
            i + 1,
            c.word,
            c.freq,
            c.score
        );
    }
    if cands.is_empty() {
        println!("  （无候选）");
    }
}

fn top5_with_ctx(engine: &ImeEngine, pinyin: &str, ctx_tail: &str) {
    let ctx = LmContext { tail: ctx_tail, last_word: None };
    let cands = engine
        .lookup_candidates(pinyin, &ctx)
        .expect("查询失败");
    println!("{}（上文「{}」）：", pinyin, ctx_tail);
    for (i, c) in cands.iter().take(5).enumerate() {
        println!("  {}. {}  (freq={}, score={:.3})", i + 1, c.word, c.freq, c.score);
    }
}

/// 单次 score() 耗时：用固定上文 + 常用候选词热循环测量（debug 构建）。
fn bench_score(engine: &ImeEngine, ctx_tail: &str) {
    let ctx = LmContext { tail: ctx_tail, last_word: None };
    let state = engine.lm.state_from_context(&ctx);
    const N: u64 = 500_000;
    // 预热（mmap 缺页/分支预测）
    for _ in 0..20_000 {
        let _ = engine.lm.score(&state, "天气");
    }
    let t0 = std::time::Instant::now();
    for _ in 0..N {
        let _ = engine.lm.score(&state, "天气");
    }
    let dt = t0.elapsed();
    let per = dt.as_secs_f64() / N as f64 * 1e9;
    println!(
        "score() 上文「{ctx_tail}」（{} 字窗）：{per:.0} ns/次 ≈ {:.0} 万次/秒",
        ctx_tail.chars().count(),
        N as f64 / dt.as_secs_f64() / 10_000.0
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dict = resolve_path(args.first().map(String::as_str), "XWX_DICT_PATH", "dict.bin");
    let gram = resolve_path(args.get(1).map(String::as_str), "XWX_LM_PATH", "wanxiang-lts-zh-hans.gram");

    if !dict.is_file() {
        eprintln!("缺 dict.bin：{}（此例需要真实文件）", dict.display());
        std::process::exit(1);
    }
    if !gram.is_file() {
        eprintln!("缺 .gram：{}（此例需要真实文件）", gram.display());
        std::process::exit(1);
    }

    // —— 对照基线：空语言模型（= 接入万象前）——
    let plain = ImeEngine::new(Some(&dict)).expect("引擎创建失败");
    // —— 接入万象 ——
    let wanxiang = ImeEngine::new_with_lm(Some(&dict), Some(&gram)).expect("引擎（含万象）创建失败");
    let lm_state = match &wanxiang.lm {
        LmStore::Wanxiang(lm) => format!("wanxiang（{} 单元）", lm.num_units()),
        LmStore::Bigram(_) => "bigram（空）".to_string(),
        LmStore::Mixed(_) => "mixed(此例未挂用户层)".to_string(),
    };
    println!("═══ 真实词库 × 真实语言模型 ═══");
    println!("dict.bin：{} 条；LM：{}", wanxiang.dict_count(), lm_state);
    println!("（对照基线引擎无语言模型，等价接入万象前的打分）\n");

    println!("──── 空 LM（当前现状对照） ────");
    for pinyin in INPUTS {
        top5(&plain, pinyin, pinyin);
        println!();
    }

    println!("──── 万象 .gram ────");
    for pinyin in INPUTS {
        top5(&wanxiang, pinyin, pinyin);
        println!();
    }

    println!("──── 有上文的万象（上文「今天」） ────");
    for pinyin in ["xian", "nihao"] {
        top5_with_ctx(&wanxiang, pinyin, "今天");
        println!();
    }

    println!("──── 性能 ────");
    bench_score(&wanxiang, "今");
    bench_score(&wanxiang, "今天");
    bench_score(&wanxiang, "今天天气好");
}
