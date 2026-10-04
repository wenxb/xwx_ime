//! 简拼副索引查询性能基准:真实 dict.bin + abbrev.bin 上测纯简拼与混合简拼
//! 的单次查询耗时,验收门槛:zhongg 单次查询 < 5ms(第 1 批架构设计风险清单)。
//!
//! 用法(target 目录由环境变量指走,避免动仓库配置):
//!
//! ```sh
//! CARGO_TARGET_DIR=/tmp/xwx_target \
//!   cargo run --release --offline -p xwx_engine --example abbrev_bench
//! ```
//!
//! 硬性验收:**`zhongg` 单次查询 < 5ms**(Python 原型 210ms,扫描 33 万 key;
//! 本基准必须给出实测数字,不接受「Rust 应该更快」这类论证)。

use std::path::Path;
use std::time::{Duration, Instant};

use xwx_engine::MmapDict;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let dict_path = root.join("dict.bin");
    let abbrev_path = root.join("abbrev.bin");

    let dict = MmapDict::open(&dict_path)?.with_abbrev(&abbrev_path);
    println!("dict entries: {}", dict.count());
    println!("abbrev loaded: {}", dict.has_abbrev());

    // 预热音节表(混合简拼首次查询会懒扫描主索引建表,不计入单次延迟)。
    let warm = Instant::now();
    let _ = dict.lookup_mixed("ni")?;
    println!("音节表预热(一次性懒扫描): {:.1}ms\n", warm.elapsed().as_secs_f64() * 1000.0);

    // (输入, 是否混合简拼)。纯简拼走副索引二分;混合走主索引前缀 + 声母过滤。
    let cases: &[(&str, bool)] = &[
        ("zgr", false),
        ("bjdx", false),
        ("nh", false),
        ("xx", false),
        ("zhongg", true),
        ("zhongguor", true),
        // 末尾四条是**预算守卫**(`MAX_MIXED_DFS_STEPS` = 20_000 步):它们代表
        // 交错 DFS 的最坏形状(全首字母 / 反复 zh / 单字母音节堆积),用来证明
        // 「病态输入的耗时也是常数」是实测而不是推理。`nhao`/`feicde` 是本次
        // 新增能力的代表(缩写出现在**开头**/**中间**),留作回归哨兵。
        ("nhao", true),
        ("feicde", true),
        ("zzzz", true),
        ("zhzhzhzh", true),
        ("aaaaaaaa", true),
        // T1 守卫(2026-10-02):`meiy` 是「同一音节位可选 token 极多」的形状
        // (`mei y*` 下几千条),旧实现在这里撞 500 条早停、按探索顺序截断,
        // 把词频最高的「没有」丢了。改成「收集到预算为止 + 排序后按词频截断」
        // 之后,这条输入是耗时最坏的代表,必须一直量着。
        ("meiy", true),
        // T2 守卫(2026-10-02):末尾半音节读法让「整串就是半个音节」的输入
        // (`sh`/`zh`)去扫整个音节区间(`sh*` = 上/是/说/时…几千条),
        // 这是新路径里最大的区间,必须一直量着。
        ("sh", true),
        ("zh", true),
    ];

    println!("== 简拼/混合简拼单次查询延迟(预热 20 次后取 100 次的 min/avg/p95)==");
    for &(input, mixed) in cases {
        let stats = bench(&dict, input, mixed, 20, 100);
        let kind = if mixed { "混合" } else { "简拼" };
        let gate = if input == "zhongg" {
            if stats.min < 5.0 { "  [达标 <5ms]" } else { "  [未达标 ≥5ms]" }
        } else {
            ""
        };
        println!("{input:<12} [{kind}] min {:>7.3}ms  avg {:>7.3}ms  p95 {:>7.3}ms  ({} 候选){gate}",
            stats.min, stats.avg, stats.p95, stats.cands);
    }
    Ok(())
}

struct Stats {
    min: f64,
    avg: f64,
    p95: f64,
    cands: usize,
}

fn bench(dict: &MmapDict, input: &str, mixed: bool, warmup: usize, rounds: usize) -> Stats {
    let run = || -> usize {
        if mixed {
            dict.lookup_mixed(input).expect("混合简拼查询失败").len()
        } else {
            dict.lookup_abbrev(input).expect("简拼查询失败").len()
        }
    };
    for _ in 0..warmup {
        run();
    }
    let mut times: Vec<Duration> = Vec::with_capacity(rounds);
    let mut cands = 0;
    let mut min = Duration::MAX;
    for _ in 0..rounds {
        let start = Instant::now();
        cands = run();
        let elapsed = start.elapsed();
        min = min.min(elapsed);
        times.push(elapsed);
    }
    times.sort_unstable();
    let avg = times.iter().sum::<Duration>().as_secs_f64() / rounds as f64 * 1000.0;
    let p95 = times[(rounds * 95 / 100).min(rounds - 1)].as_secs_f64() * 1000.0;
    Stats { min: min.as_secs_f64() * 1000.0, avg, p95, cands }
}
