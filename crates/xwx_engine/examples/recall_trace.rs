//! 阶段 0 诊断：对每个输入打印「门槛判定 → 混合 DFS 停止原因 → 最终候选前十」，
//! 用来回答「首次丢失在哪里」——而不是继续靠猜改预算与顺序。
//!
//! 需要 `XWX_TRACE=1` 才有内部事实；不设时只打印最终候选（与生产行为一致）。
//!
//! 用法：
//! ```sh
//! printf 'ztailan\nztai\nchuzaina\n' | XWX_TRACE=1 \
//!   cargo run --release --offline --example recall_trace
//! ```

use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Arc;

use xwx_engine::trace;
use xwx_engine::{ImeEngine, LmContext};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let dict = root.join("data/dict.bin");
    let abbrev = root.join("data/abbrev.bin");
    let gram = std::env::var_os("XWX_LM_PATH")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".local/share/xwx/wanxiang-lts-zh-hans.gram"))
        })
        .filter(|p| p.is_file());
    if !dict.is_file() {
        return Err(format!("缺词库: {}", dict.display()).into());
    }
    let mut engine = ImeEngine::new_with_lm(Some(&dict), gram.as_deref())?;
    if abbrev.is_file() {
        engine.dict = engine.dict.with_abbrev(&abbrev);
    engine.dict = engine.dict.with_syllable_graph(abbrev.with_file_name("syllable_graph.bin"));
    }
    let engine = Arc::new(engine);
    println!(
        "装配: dict={} abbrev={} gram={}",
        dict.display(),
        abbrev.is_file(),
        gram.as_ref().map_or("无".into(), |p| p.display().to_string())
    );

    for line in std::io::stdin().lock().lines() {
        let input = line?;
        let input = input.trim();
        if input.is_empty() || input.starts_with('#') {
            continue;
        }
        let cands = engine.lookup_candidates(input, &LmContext::empty())?;
        println!("\n== {input}  候选 {} 条", cands.len());
        for t in trace::take() {
            println!("   [迹] {t}");
        }
        for (i, c) in cands.iter().take(10).enumerate() {
            println!(
                "   #{:<3} {:<14} {:?} covered={}",
                i + 1,
                c.word,
                c.spelling,
                c.covered
            );
        }
    }
    Ok(())
}
