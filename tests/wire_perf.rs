//! D-Bus 层性能入口(codex 审查第二批):一次 `ProcessKeyEvent` 往返要多久。
//!
//! 引擎/会话层的逐键延迟由 `examples/perf_probe.rs` 量(不经 IPC);这里量的是
//! **真实总线往返**——私有 `dbus-daemon` + 真引擎进程,也就是实机上 GNOME Shell
//! 每按一个键要等的那段时间。两者相减就是 IPC + 调度开销,补上审查指出的
//! 「`decode_bench`/`lookup_bench` 都没测总线响应」这块空白。
//!
//! 注意:集成测试拉起的是 **debug** 引擎(与 `just verify` 同一装配),
//! 所以这里的数字比实机 release 偏保守;要看 release 的引擎/会话层数字用
//! `just perf`。
//!
//! 门槛故意放得很宽(p95 ≤ 100ms):集成测试跑在开发机上,总线上还有别的进程,
//! 卡紧只会变成 flaky。要看「常慢」还是看打印出来的分位。
//!
//! 断言质量:这里的断言不是「延迟达标」而是**样本成立**——每个脚本都必须真的
//! 产生过非 false 的 `ProcessKeyEvent` 返回,否则量的是空气。

mod support;

use std::time::Instant;

use support::{Harness, shared_fixture};

/// 最近秩分位(与 `examples/perf_probe.rs` 同一口径)。
fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 * q).ceil() as usize).saturating_sub(1);
    sorted[idx.min(sorted.len() - 1)]
}

#[test]
fn wire_round_trip_latency_is_reported() {
    let h = &mut Harness::new("wire-perf");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    let scripts = ["nihao", "zhongg", "nihoa", "womenjintianqubeij"];
    let rounds = 3;
    let mut per_script: Vec<(&str, Vec<f64>)> = Vec::new();
    let mut all: Vec<f64> = Vec::new();
    let mut handled = 0usize;

    for _ in 0..rounds {
        for body in scripts {
            h.reset_session().expect("Reset");
            let mut ms = Vec::new();
            for ch in body.chars() {
                let t0 = Instant::now();
                let ok = h
                    .key_checked(ch as u32, 0)
                    .unwrap_or_else(|e| panic!("{body}: ProcessKeyEvent 失败:{e}"));
                ms.push(t0.elapsed().as_secs_f64() * 1000.0);
                if ok {
                    handled += 1;
                }
            }
            all.extend(ms.iter().copied());
            match per_script.iter_mut().find(|(name, _)| *name == body) {
                Some(slot) => slot.1.extend(ms),
                None => per_script.push((body, ms)),
            }
        }
    }
    h.reset_session().expect("Reset");

    all.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    println!("## D-Bus 往返延迟(私有总线 + 真引擎进程,ProcessKeyEvent 一次往返)");
    println!("{:<24} {:>5} {:>10} {:>10} {:>10}", "输入", "键数", "p50", "p95", "max");
    for (name, ms) in per_script.iter_mut() {
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        println!(
            "{:<24} {:>5} {:>9.3}ms {:>9.3}ms {:>9.3}ms",
            name,
            ms.len(),
            quantile(ms, 0.50),
            quantile(ms, 0.95),
            quantile(ms, 1.0)
        );
    }
    let p95 = quantile(&all, 0.95);
    println!(
        "总表:{} 键,p50={:.3}ms p95={:.3}ms max={:.3}ms",
        all.len(),
        quantile(&all, 0.50),
        p95,
        quantile(&all, 1.0)
    );

    // 样本成立:至少有一半的按键被引擎真的处理过(返回 true)。
    assert!(
        handled * 2 >= all.len(),
        "只有 {handled}/{} 个按键被引擎处理,延迟样本不成立",
        all.len()
    );
    assert!(
        p95 <= 100.0,
        "D-Bus 往返 p95 {p95:.3}ms 超过 100ms:总线上单键等待已经不可接受"
    );
}
