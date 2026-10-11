//! **总线级延迟压测**：引擎发信号会不会被反压到「卡几秒」。
//!
//! 为什么需要这一层：`latency_fuzz`/`crash_fuzz` 打的是**引擎库**（进程内函数
//! 调用），量不到 zbus。而 `ProcessKeyEvent` 是**同步 D-Bus 方法**——引擎处理完
//! 还要给面板发 `UpdatePreeditText`/`UpdateLookupTable`，这些 `.await` 若被对端
//! 反压，方法就不返回，ibus-daemon 一直等，等到超时**把键扔掉**。这正好同时
//! 解释用户的「卡几秒」和「丢字」，而且随机（取决于面板当时忙不忙）。
//!
//! 手法：起一个真实私有总线上的引擎，**连打几千个键**，量每次 `ProcessKeyEvent`
//! 的往返耗时。候选表越大、面板消费越慢，反压越明显——所以用真实词库
//! （候选 512 条）而不是测试用的小词库。
//!
//! 标了 `#[ignore]`：它要装配真实词库、跑几千次总线往返，不该拖慢每次
//! `cargo test`。手动跑：
//! ```text
//! cargo test --release --test bus_latency -- --ignored --nocapture
//! ```

mod support;

use std::path::Path;
use std::time::{Duration, Instant};

use support::{keyval, Harness, M_RELEASE};

/// 单次往返的告警门槛（毫秒）。正常应该是个位数。
const WARN_MS: f64 = 50.0;

/// 真实语言模型（用户目录下，与生产同一份）。
fn real_lm() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let p = std::path::PathBuf::from(home)
        .join(".local/share/xwx/wanxiang-lts-zh-hans.gram");
    p.is_file().then(|| p.to_string_lossy().to_string())
}

#[test]
#[ignore = "总线压测：装配真实词库 + 数千次总线往返，手动跑"]
fn process_key_event_round_trip_stays_fast_under_load() {
    // **真实词库 + 真实语言模型**：候选多、信号大，才压得出反压。
    let mut h = Harness::new("bus-latency");
    if let Some(lm) = real_lm() {
        h = h.with_env("XWX_LM_PATH", lm);
    }
    h.start_engine_with(
        Path::new("data/dict.bin"),
        Some(Path::new("data/abbrev.bin")),
        Duration::from_secs(120),
    )
    .unwrap_or_else(|e| panic!("{e}"));

    let n = 3000usize;
    let mut times = Vec::with_capacity(n);
    let mut worst = (0.0f64, String::new());
    // 真实中文句子的拼音，循环打——字母进拼音串、触发候选刷新（大表信号）。
    let keys = "jintiantianqibucuowoyaoshangbanyaolejinhoushianpaihaole".as_bytes();

    let t_all = Instant::now();
    for i in 0..n {
        let c = keys[i % keys.len()] as char;
        let t = Instant::now();
        let _ = h.key(keyval::ascii(c), M_RELEASE);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        times.push(ms);
        if ms > worst.0 {
            worst = (ms, format!("第{i}键 {c}"));
        }
    }
    let elapsed = t_all.elapsed();

    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let q = |p: f64| times[((times.len() - 1) as f64 * p).round() as usize];
    println!("\n== ProcessKeyEvent 往返（{n} 键，真实词库 + 真实 LM）==");
    println!(
        "总耗时 {:.1}s  p50={:.2}ms p95={:.2}ms p99={:.2}ms max={:.2}ms",
        elapsed.as_secs_f64(),
        q(0.50),
        q(0.95),
        q(0.99),
        worst.0
    );
    println!("最慢的一次：{}", worst.1);

    let over = times.iter().filter(|t| **t > WARN_MS).count();
    println!("超过 {WARN_MS}ms 的 {over} 次");

    // 断言的意义：**单键往返不该到秒级**。若这里出现秒级，就复现了用户的卡死，
    // 且能确定卡点在总线层（引擎库自己的计算早已量过 ≤9ms）。
    assert!(
        worst.0 < 1000.0,
        "单键往返到了秒级（{:.0}ms）——复现了「卡死几秒」；卡点在总线层",
        worst.0
    );
}
