//! 卡死看门狗在**真实 D-Bus 路径**上确实会触发。
//!
//! 为什么需要这一层：看门狗的价值全在「真卡的时候它真的写下了一条记录」。
//! 单元测试只证明了 `Stall` 自己的语义（分段是增量、关闭时零开销、轮转只留
//! 一代），证明不了它**接进了 `ProcessKeyEvent`**、证明不了阈值/路径/落盘这条
//! 链在装配好的引擎里是通的。装上一条从不触发的看门狗，等于没装。
//!
//! 手法：把阈值压到 1ms（正常一次按键远大于它），给一个临时日志路径，
//! 然后**通过私有总线真敲几个键**，再读那行记录。

mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use support::Harness;

/// 起一个把看门狗阈值压到 1ms、日志写到 `<root>/stall.log` 的引擎。
fn start_with_watchdog(label: &str) -> (Harness, PathBuf) {
    let mut h = Harness::new(label);
    let log = h.root.join("stall.log");
    h = h
        .with_env("XWX_STALL_MS", "1")
        .with_env("XWX_STALL_LOG", log.to_string_lossy().to_string());
    h.start_engine(support::shared_fixture());
    (h, log)
}

fn read_log(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn watchdog_records_a_real_key_with_all_three_segments() {
    let (h, log) = start_with_watchdog("stall-on");
    // 敲一段真实的中文拼音：走 ProcessKeyEvent → handle_key → 三段计时。
    h.type_ascii("jintiantianqi");
    // 落盘是同步的（在 finish 里），但给一点余量避免慢机器上的抖动。
    std::thread::sleep(Duration::from_millis(200));

    let text = read_log(&log);
    assert!(
        !text.is_empty(),
        "阈值 1ms 下敲了 13 个键，日志不该是空的（路径 {}）",
        log.display()
    );
    let line = text
        .lines()
        .find(|l| l.contains("ProcessKeyEvent"))
        .unwrap_or_else(|| panic!("日志里应有 ProcessKeyEvent 记录:\n{text}"));
    // 三段都要在：这正是看门狗存在的意义——把「锁/引擎/发信号」分开，
    // 缺任何一段就没法判断时间花在哪。
    for seg in ["锁=", "引擎=", "发信号="] {
        assert!(line.contains(seg), "记录缺 {seg} 段: {line}");
    }
    // 把真实记录打出来，人眼可核对（`--nocapture` 时可见）。
    println!("真实看门狗记录: {line}");
    // 时间戳与总耗时（`NNNms`）都要有，否则读的人无法与「刚才那一下」对上。
    assert!(line.contains("ms  ProcessKeyEvent"), "缺总耗时: {line}");
    assert!(
        line.len() >= 19 && line.as_bytes()[4] == b'-',
        "缺时间戳: {line}"
    );
}

#[test]
fn heartbeat_records_when_the_event_loop_is_blocked_from_outside() {
    // 覆盖「冻结发生在 ProcessKeyEvent 之外」：用 SIGSTOP 把引擎进程整个冻住
    // 一段时间，再 SIGCONT。这期间它一个方法都不会被调用，分段计时无从触发，
    // 只有心跳能发现「我一秒钟没被调度到」。
    let (h, log) = start_with_watchdog("stall-heartbeat");
    let pid = h.engine_pid_for_test();
    let stop = std::process::Command::new("kill")
        .args(["-STOP", &pid.to_string()])
        .status()
        .expect("发 SIGSTOP");
    assert!(stop.success(), "SIGSTOP 失败");
    std::thread::sleep(Duration::from_millis(1500));
    let cont = std::process::Command::new("kill")
        .args(["-CONT", &pid.to_string()])
        .status()
        .expect("发 SIGCONT");
    assert!(cont.success(), "SIGCONT 失败");
    // 恢复后心跳才会发现那段空档并落记录。
    std::thread::sleep(Duration::from_millis(600));

    let text = read_log(&log);
    assert!(
        text.contains("heartbeat"),
        "进程被冻住 1.5s 后应留下心跳记录，实际:\n{text}"
    );
}

#[test]
fn watchdog_stays_silent_when_the_threshold_is_not_reached() {
    // 反向用例：阈值调到很大时，敲键不该产生任何记录。
    // 没有它，「日志非空」可能只是因为看门狗**无条件**在写——那就废了。
    let mut h = Harness::new("stall-off");
    let log = h.root.join("stall.log");
    h = h
        .with_env("XWX_STALL_MS", "600000")
        .with_env("XWX_STALL_LOG", log.to_string_lossy().to_string());
    h.start_engine(support::shared_fixture());
    h.type_ascii("nihao");
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        read_log(&log).is_empty(),
        "未达阈值不该落任何记录，实际:\n{}",
        read_log(&log)
    );
}

#[test]
fn watchdog_can_be_disabled_entirely() {
    // `off` = 完全关闭（连 detail 闭包都不执行）。这条保证「不想看日志的人
    // 能真的关掉」，而不是「设了 off 仍然默默写」。
    let mut h = Harness::new("stall-disabled");
    let log = h.root.join("stall.log");
    h = h
        .with_env("XWX_STALL_MS", "off")
        .with_env("XWX_STALL_LOG", log.to_string_lossy().to_string());
    h.start_engine(support::shared_fixture());
    h.type_ascii("nihao");
    std::thread::sleep(Duration::from_millis(200));
    assert!(!log.exists(), "关闭时不该创建日志文件");
}
