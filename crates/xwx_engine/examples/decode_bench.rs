//! 解码性能基准：真实 dict.bin 上测典型输入与病态输入的单次查询耗时。
//!
//! 用法（target 目录由环境变量指走，避免动仓库配置）：
//!
//! ```sh
//! CARGO_TARGET_DIR=/tmp/xwx_target \
//!   cargo run --release --offline -p xwx_engine --example decode_bench
//! ```
//!
//! 报告里引用的数字都来自这个 example（数据文件 data/dict.bin 由运行时
//! mmap 只读映射，与生产路径一致）。

use std::path::Path;
use std::time::{Duration, Instant};

use xwx_engine::{ImeEngine, LmContext};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let engine = ImeEngine::new(Some(&dict_path))?;
    println!("dict entries: {}", engine.dict_count());
    println!("beam width: 16（引擎内定值，见 lib.rs 的 BEAM_WIDTH 注释）");

    let ctx = LmContext::empty();
    // 典型输入：一个完整句子 + 打字中途（半音节）+ 两音节
    let typical = [
        "womenjintianquxian",
        "nihao",
        "xian",
        "zhonghuarenmingongheguo",
        "wo",
    ];
    // 病态输入：全同音 / 无音节 / 双歧义连锁 / 超长
    let pathological = [
        "aaaaaaaaaaaaaaaa",
        "zzzzzzzz",
        "nananananananananana",
        &"a".repeat(40),
    ];

    println!("\n== 典型输入（预热 50 次后取 200 次的 min/avg/p95）==");
    for input in typical {
        let stats = bench(&engine, input, &ctx, 50, 200);
        println!("{input:<24} min {:>8.3}ms  avg {:>8.3}ms  p95 {:>8.3}ms  ({} 候选)",
            stats.min, stats.avg, stats.p95, stats.cands);
    }

    println!("\n== 病态输入（每次单独计时）==");
    for input in pathological {
        let start = Instant::now();
        let cands = engine.lookup_candidates(input, &ctx)?;
        println!("{input:<28} {:>8.3}ms  {} 候选", start.elapsed().as_secs_f64() * 1000.0, cands.len());
    }
    Ok(())
}

struct Stats {
    min: f64,
    avg: f64,
    p95: f64,
    cands: usize,
}

fn bench(
    engine: &ImeEngine,
    input: &str,
    ctx: &LmContext<'_>,
    warmup: usize,
    rounds: usize,
) -> Stats {
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
