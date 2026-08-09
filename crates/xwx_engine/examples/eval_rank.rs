//! 候选排序评测（第 4 步核心交付物）：在真实 dict.bin × 真实 .gram 上
//! 跑评测集，输出 top-1 / top-5 命中率，按词长分档。
//!
//! 评测集由 `scripts/build_eval.py` 生成（JSONL，见该脚本头部说明）：
//! - `kind == "main"`：无上文，输入 = 词条拼音去空格，期望首选 = 词条；
//! - `kind == "ctx"`  ：带上文，`ctx` 字段为上文词，期望首选 = `word`，
//!   同时用空上文跑一遍（对照「上文有没有用」）。
//! - `kind == "unbiased"`：人工「反词频」小集（第 4b 步），期望首选
//!   刻意取「输入组内非最高频」的搭配词；**只比较带上文一行**，
//!   无上文一行用于对照（空 LM 无状态，带上文/无上文必然同分，
//!   输出里已注明，不是 bug）。
//!
//! 用法：
//! ```sh
//! CARGO_TARGET_DIR=/tmp/xwx_target \
//!   cargo run --release --offline -p xwx_engine --example eval_rank -- \
//!     [dict.bin] [wanxiang.gram] [eval_samples.jsonl] [--max N] [--weights w1,w2]
//! ```
//! 缺省路径：仓库 `data/dict.bin`（与 `~/.local/share/xwx/dict.bin`
//! md5 相同）、`~/.local/share/xwx/wanxiang-lts-zh-hans.gram`、
//! `scripts/eval_samples.jsonl`；均可用 `XWX_DICT_PATH` / `XWX_LM_PATH`
//! / `XWX_EVAL_SAMPLES` 覆盖。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use xwx_engine::{ImeEngine, LmContext};

fn resolve(cli: Option<&str>, env_key: &str, default: &str) -> PathBuf {
    resolve_with_fallback(cli, env_key, default, None)
}

fn resolve_with_fallback(
    cli: Option<&str>,
    env_key: &str,
    default: &str,
    fallback: Option<PathBuf>,
) -> PathBuf {
    if let Some(p) = cli {
        return PathBuf::from(p);
    }
    if let Some(p) = std::env::var_os(env_key) {
        return PathBuf::from(p);
    }
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join(default);
    if p.is_file() || fallback.is_none() {
        return p;
    }
    fallback.unwrap()
}

fn xdg_xwx(file: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("xwx").join(file))
}

#[derive(Debug)]
struct Sample {
    kind: String,
    input: String,
    word: String,
    len: usize,
    ctx: String,
    dom: Option<bool>,
    freq: u64,
}

fn load_samples(path: &PathBuf, max: usize) -> Result<Vec<Sample>, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if out.len() >= max {
            break;
        }
        let v: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| format!("{}:{}: {e}", path.display(), i + 1))?;
        out.push(Sample {
            kind: v["kind"].as_str().unwrap_or("").to_string(),
            input: v["input"].as_str().unwrap_or("").to_string(),
            word: v["word"].as_str().unwrap_or("").to_string(),
            len: v["len"].as_u64().unwrap_or(0) as usize,
            ctx: v["ctx"].as_str().unwrap_or("").to_string(),
            dom: v.get("dom").and_then(|d| d.as_bool()),
            freq: v["freq"].as_u64().unwrap_or(0),
        });
    }
    Ok(out)
}

#[derive(Default)]
struct Bucket {
    n: u64,
    top1: u64,
    top5: u64,
}

impl Bucket {
    fn add(&mut self, cands: &[String], expected: &str) {
        self.n += 1;
        if cands.first().is_some_and(|c| c == expected) {
            self.top1 += 1;
        }
        if cands.iter().take(5).any(|c| c == expected) {
            self.top5 += 1;
        }
    }
    fn print(&self, label: &str, _total: &Bucket) {
        if self.n == 0 {
            println!("  {label:<8} （无样本）");
            return;
        }
        println!(
            "  {label:<8} top1 {:5.1}%  top5 {:5.1}%   (n={})",
            self.top1 as f64 / self.n as f64 * 100.0,
            self.top5 as f64 / self.n as f64 * 100.0,
            self.n
        );
    }
}

fn run(engine: &ImeEngine, samples: &[Sample], is_empty_lm: bool) {
    let mut main: HashMap<usize, Bucket> = HashMap::new();
    let mut main_all = Bucket::default();
    let mut ctx_with = Bucket::default();
    let mut ctx_without = Bucket::default();
    let mut ctx_dom_with = (Bucket::default(), Bucket::default());
    let mut ctx_sens_with = (Bucket::default(), Bucket::default());
    let mut unbiased_with = Bucket::default();
    let mut unbiased_without = Bucket::default();

    for s in samples {
        if s.kind == "main" {
            let ctx = LmContext::empty();
            let cands = engine
                .lookup_candidates(&s.input, &ctx)
                .expect("查询失败");
            let words: Vec<String> = cands.iter().map(|c| c.word.clone()).collect();
            main.entry(s.len).or_default().add(&words, &s.word);
            main_all.add(&words, &s.word);
        } else {
            let ctx = LmContext { tail: &s.ctx, last_word: None };
            let with = engine
                .lookup_candidates(&s.input, &ctx)
                .expect("查询失败");
            let no = engine
                .lookup_candidates(&s.input, &LmContext::empty())
                .expect("查询失败");
            let w: Vec<String> = with.iter().map(|c| c.word.clone()).collect();
            let n: Vec<String> = no.iter().map(|c| c.word.clone()).collect();
            if s.kind == "unbiased" {
                unbiased_with.add(&w, &s.word);
                unbiased_without.add(&n, &s.word);
            } else {
                ctx_with.add(&w, &s.word);
                ctx_without.add(&n, &s.word);
                if s.dom == Some(true) {
                    ctx_dom_with.0.add(&w, &s.word);
                    ctx_dom_with.1.add(&n, &s.word);
                } else {
                    ctx_sens_with.0.add(&w, &s.word);
                    ctx_sens_with.1.add(&n, &s.word);
                }
            }
        }
    }

    println!("── main（无上文）");
    main_all.print("总", &main_all);
    for len in [2usize, 3, 4] {
        main.entry(len).or_default().print(&format!("{len}字"), &main_all);
    }
    println!("── ctx（带）");
    ctx_with.print("带上文", &ctx_with);
    ctx_without.print("无上文", &ctx_without);
    ctx_dom_with.0.print("带上文·主导", &ctx_with);
    ctx_dom_with.1.print("无上文·主导", &ctx_with);
    ctx_sens_with.0.print("带上文·敏感", &ctx_with);
    ctx_sens_with.1.print("无上文·敏感", &ctx_with);
    if is_empty_lm {
        // 空 LM 逐词分数不含上下文，带上文/无上文必然同分；
        // 加一行说明，避免被当成「没跑出区别」的输出缺陷。
        println!("  （空 LM 不读上下文：带上文 ≡ 无上文，同分是预期行为）");
    }
    println!("── unbiased（无偏小集，期望词均非组内最高频）");
    unbiased_with.print("带上文", &unbiased_with);
    unbiased_without.print("无上文", &unbiased_without);
    if is_empty_lm {
        println!("  （空 LM 不读上下文：带上文 ≡ 无上文，同分是预期行为）");
    }
}

/// 只跑 main 样本，打印前 `n` 个 top-1 未命中的样本与其前 8 候选
/// （诊断用：弄清 misses 是 LM 与词库意见分歧，还是格子/beam 覆盖问题）。
fn debug_fails(engine: &ImeEngine, samples: &[Sample], n: usize) {
    let mut shown = 0;
    for s in samples.iter().filter(|s| s.kind == "main") {
        if shown >= n {
            break;
        }
        let cands = engine
            .lookup_candidates(&s.input, &LmContext::empty())
            .expect("查询失败");
        if cands.first().is_some_and(|c| c.word == s.word) {
            continue;
        }
        println!(
            "✗ {} 期望「{}」freq={} → {}",
            s.input,
            s.word,
            s.freq,
            cands
                .iter()
                .take(8)
                .map(|c| format!("{}({:.1})", c.word, c.score))
                .collect::<Vec<_>>()
                .join("  ")
        );
        shown += 1;
    }
    eprintln!("（top-1 未命中打印 {shown} 条）");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut rest: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut max = usize::MAX;
    let mut weights: Vec<f32> = vec![1.0];
    let mut fails = 0usize;
    if let Some(i) = rest.iter().position(|a| *a == "--max") {
        max = rest.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(max);
        rest.drain(i..i + 2);
    }
    if let Some(i) = rest.iter().position(|a| *a == "--weights") {
        if let Some(list) = rest.get(i + 1) {
            weights = list
                .split(',')
                .filter_map(|s| s.trim().parse::<f32>().ok())
                .collect();
        }
        rest.drain(i..i + 2);
    }
    if let Some(i) = rest.iter().position(|a| *a == "--fails") {
        fails = rest.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(0);
        rest.drain(i..i + 2);
    }
    let dict = resolve(rest.first().copied(), "XWX_DICT_PATH", "data/dict.bin");
    let gram = resolve_with_fallback(
        rest.get(1).copied(),
        "XWX_LM_PATH",
        "wanxiang-lts-zh-hans.gram",
        xdg_xwx("wanxiang-lts-zh-hans.gram"),
    );
    let samples_path = resolve(
        rest.get(2).copied(),
        "XWX_EVAL_SAMPLES",
        "scripts/eval_samples.jsonl",
    );

    let samples = load_samples(&samples_path, max)?;
    let gram_opt = if Path::exists(&gram) { Some(gram.as_ref()) } else { None };
    let dict_ref = dict.as_ref();

    let plain = ImeEngine::new(Some(dict_ref)).expect("引擎创建失败");
    println!(
        "dict={}（{} 条）；LM={:?}；样本 {} 条；weights={:?}",
        dict.display(),
        plain.dict_count(),
        if gram_opt.is_some() { "wanxiang".to_string() } else { "空".to_string() },
        samples.len(),
        weights
    );

    println!("\n===== 无 LM（对照：纯词频对数归一） =====");
    run(&plain, &samples, true);
    if fails > 0 {
        debug_fails(&plain, &samples, fails);
    }
    for w in weights {
        let mut engine =
            ImeEngine::new_with_lm(Some(dict_ref), gram_opt).expect("引擎创建失败");
        engine.lm_weight = w;
        println!("\n===== 万象 .gram × lm_weight={w} =====");
        run(&engine, &samples, false);
        if fails > 0 {
            debug_fails(&engine, &samples, fails);
        }
    }
    Ok(())
}
