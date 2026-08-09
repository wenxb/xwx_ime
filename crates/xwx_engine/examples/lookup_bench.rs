//! `lookup_candidates` 端到端延迟基准(含简拼路径):与 `decode_bench`
//! 的差别是本基准给引擎装配 `abbrev.bin`,因此能测到 2 引入的
//! 简拼合并代价——IBus 侧是同步按键处理,每次按键都付这个钱。
//!
//! 用法:
//!
//! ```sh
//! cargo run --release --offline -p xwx_engine --example lookup_bench
//! ```

use std::path::Path;
use std::time::{Duration, Instant};

use xwx_engine::{ImeEngine, LmContext};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let mut engine = ImeEngine::new(Some(&dir.join("dict.bin")))?;
    engine.dict = engine.dict.with_abbrev(dir.join("abbrev.bin"));
    println!(
        "dict entries: {}  abbrev attached: {}",
        engine.dict_count(),
        engine.dict.has_abbrev()
    );

    let ctx = LmContext::empty();
    // 任务书点名的四类:最长全拼串(17 字符)、混合简拼、纯简拼、两音节全拼。
    let inputs = [
        ("womenjintianquxian", "最长全拼串"),
        ("zhongg", "混合简拼"),
        ("zgr", "纯简拼"),
        ("nihao", "两音节全拼"),
        ("xx", "病态纯简拼(命中上万条)"),
    ];

    println!("\n== lookup_candidates 端到端(预热 50 次后取 200 次 min/avg/p95)==");
    for (input, label) in inputs {
        let stats = bench(&engine, input, &ctx, 50, 200);
        println!(
            "{input:<20} ({label:<12}) min {:>7.3}ms  avg {:>7.3}ms  p95 {:>7.3}ms  ({} 候选)",
            stats.min, stats.avg, stats.p95, stats.cands
        );
    }
    Ok(())
}

struct Stats {
    min: f64,
    avg: f64,
    p95: f64,
    cands: usize,
}

fn bench(engine: &ImeEngine, input: &str, ctx: &LmContext<'_>, warmup: usize, rounds: usize) -> Stats {
    for _ in 0..warmup {
        engine.lookup_candidates(input, ctx).expect("查询失败");
    }
    let mut times: Vec<Duration> = Vec::with_capacity(rounds);
    let mut cands = 0;
    let mut min = Duration::MAX;
    for _ in 0..rounds {
        let start = Instant::now();
        cands = engine.lookup_candidates(input, ctx).expect("查询失败").len();
        let elapsed = start.elapsed();
        min = min.min(elapsed);
        times.push(elapsed);
    }
    times.sort_unstable();
    let avg = times.iter().sum::<Duration>().as_secs_f64() / rounds as f64 * 1000.0;
    let p95 = times[(rounds * 95 / 100).min(rounds - 1)].as_secs_f64() * 1000.0;
    Stats {
        min: min.as_secs_f64() * 1000.0,
        avg,
        p95,
        cands,
    }
}
