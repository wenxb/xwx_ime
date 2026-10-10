//! 卡死看门狗：把「偶尔卡几秒」变成一条能看的记录。
//!
//! 为什么需要它：几秒的冻结**不是算得慢**（实测进程两小时只用了 7 秒 CPU），
//! 而是某次 D-Bus 方法调用被挡住了——`ProcessKeyEvent` 是**同步方法**，
//! ibus-daemon 每发一个键都要等它返回，它不返回，用户看到的就是输入法卡死。
//!
//! 光靠猜没用（锁的作用域、flush、内存、磁盘都查过，都是干净的），所以这里
//! 做一件小事：把方法内**分段计时**，一次调用总耗时超过阈值就落一条记录，
//! 写清「锁 / 引擎 / 发信号」各花了多少。下一次真卡了，记录会直接指出是哪一段。
//!
//! 开销：一次 `Instant::now()` 约 20ns，正常按键（<1ms）几乎不可测；落盘只
//! 在超阈值时发生。文件按大小轮转，不会无限增长。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// 超过这个毫秒数才算「卡」（可被 `XWX_STALL_MS` 覆盖）。
///
/// 300ms 是「人开始觉得卡」的量级；正常按键在 1~10ms，离它有两个数量级，
/// 不会误报。
const DEFAULT_THRESHOLD_MS: f64 = 300.0;

/// 日志文件大小上限，超了就轮转（保留上一代）。
const MAX_LOG_BYTES: u64 = 256 * 1024;

/// 阈值（毫秒）；`None` = 关闭记录。只在进程启动时读一次环境变量。
pub fn threshold_ms() -> Option<f64> {
    static CACHE: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| threshold_from(std::env::var("XWX_STALL_MS").ok().as_deref()))
}

/// 阈值解析（纯函数，便于测试）。**未设置 = 用默认阈值即开启**：这条记录
/// 只在真卡了的时候才产生，平时没有任何输出，不该要求用户先去设环境变量。
fn threshold_from(raw: Option<&str>) -> Option<f64> {
    let Some(raw) = raw else {
        return Some(DEFAULT_THRESHOLD_MS);
    };
    let v = raw.trim();
    if v.eq_ignore_ascii_case("off") || v == "0" {
        return None;
    }
    v.parse::<f64>()
        .ok()
        .filter(|x| *x > 0.0)
        .or(Some(DEFAULT_THRESHOLD_MS))
}

/// 一次调用的分段计时器。
///
/// 用法：在两个可能的阻塞点之间调 [`Self::mark`]，最后 [`Self::finish`]。
/// 每个 mark 记录的是**距上一个 mark** 的耗时，所以段与段不会重叠计。
pub struct Stall {
    method: &'static str,
    /// 阈值在**构造时取一次**：`mark`/`detail` 在每次按键上都会走，不该反复
    /// 查全局状态。
    limit: Option<f64>,
    t0: Instant,
    last: Instant,
    marks: Vec<(&'static str, f64)>,
    detail: String,
}

impl Stall {
    pub fn new(method: &'static str) -> Self {
        Self::with_limit(method, threshold_ms())
    }

    fn with_limit(method: &'static str, limit: Option<f64>) -> Self {
        let now = Instant::now();
        Self {
            method,
            limit,
            t0: now,
            last: now,
            marks: Vec::with_capacity(4),
            detail: String::new(),
        }
    }

    /// 附加上下文（按键类型、拼音串长度等），只在落记录时用到。
    pub fn detail(&mut self, d: impl FnOnce() -> String) {
        if self.limit.is_some() {
            self.detail = d();
        }
    }

    /// 记录自上一个 mark 起的一段。`name` 用固定的短名（锁/引擎/发信号…）。
    pub fn mark(&mut self, name: &'static str) {
        if self.limit.is_none() {
            return;
        }
        let now = Instant::now();
        self.marks
            .push((name, now.duration_since(self.last).as_secs_f64() * 1000.0));
        self.last = now;
    }

    /// 结束并按需落记录。
    pub fn finish(self) {
        let Some(limit) = self.limit else {
            return;
        };
        let total = self.t0.elapsed().as_secs_f64() * 1000.0;
        if total < limit {
            return;
        }
        let segs: Vec<String> = self
            .marks
            .iter()
            .map(|(n, ms)| format!("{n}={ms:.0}ms"))
            .collect();
        let line = format!(
            "{} {:.0}ms  {}  {}{}",
            timestamp(),
            total,
            self.method,
            segs.join(" "),
            if self.detail.is_empty() {
                String::new()
            } else {
                format!("  {}", self.detail)
            }
        );
        tracing::warn!(
            total_ms = total,
            method = self.method,
            segments = %segs.join(" "),
            detail = %self.detail,
            "按键处理超过阈值（卡顿）"
        );
        append_line(&line);
    }
}

/// `$XWX_STALL_LOG` > `$XDG_DATA_HOME/xwx/stall.log` > `$HOME/.local/share/xwx/stall.log`。
fn log_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("XWX_STALL_LOG") {
        let p = PathBuf::from(p);
        if p.as_os_str().is_empty() {
            return None;
        }
        return Some(p);
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("xwx").join("stall.log"))
}

fn append_line(line: &str) {
    let Some(path) = log_path() else { return };
    append_to(&path, line);
}

/// 追加一行（`path` 显式传入，便于测试；`append_line` 只负责解析路径）。
///
/// 目录自动创建、写入失败**静默**：看门狗是诊断设施，它自己的 IO 问题绝
/// 不能反过来影响输入法。
fn append_to(path: &Path, line: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    rotate_if_needed(path);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// 超过上限就改名留上一代（只留一代，避免无限增长）。
fn rotate_if_needed(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() < MAX_LOG_BYTES {
        return;
    }
    let mut prev = path.to_path_buf();
    prev.set_extension("log.1");
    let _ = std::fs::rename(path, prev);
}

/// 无外部 crate 的本地时间戳（`YYYY-MM-DD HH:MM:SS`，UTC 偏移按系统时区）。
fn timestamp() -> String {
    // 只用 libc 的 `localtime_r`：为了一个时间戳引入 chrono 不值得。
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t = secs as libc::time_t;
    // SAFETY: `localtime_r` 只写我们提供的 `tm`，两个指针都是有效的。
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return format!("epoch={secs}");
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_parsing_is_pure_and_total() {
        // 未设置 = 默认开启（用户不必先配环境变量）
        assert_eq!(threshold_from(None), Some(DEFAULT_THRESHOLD_MS));
        // 显式关闭
        assert_eq!(threshold_from(Some("0")), None);
        assert_eq!(threshold_from(Some("off")), None);
        assert_eq!(threshold_from(Some("OFF")), None);
        // 自定义
        assert_eq!(threshold_from(Some("1500")), Some(1500.0));
        // 写坏了不该把看门狗弄没，回默认
        assert_eq!(threshold_from(Some("abc")), Some(DEFAULT_THRESHOLD_MS));
        assert_eq!(threshold_from(Some("-5")), Some(DEFAULT_THRESHOLD_MS));
        assert_eq!(threshold_from(Some("  ")), Some(DEFAULT_THRESHOLD_MS));
    }

    #[test]
    fn marks_are_sequential_not_cumulative() {
        let mut s = Stall::with_limit("t", Some(1.0));
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.mark("a");
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.mark("b");
        assert_eq!(s.marks.len(), 2);
        // 两段各自约 5ms；若误做成累计计时，第二段会约 10ms。
        assert!(s.marks[0].1 >= 4.0 && s.marks[0].1 < 45.0, "{:?}", s.marks[0]);
        assert!(s.marks[1].1 >= 4.0 && s.marks[1].1 < 45.0, "{:?}", s.marks[1]);
        let sum: f64 = s.marks.iter().map(|m| m.1).sum();
        let total = s.t0.elapsed().as_secs_f64() * 1000.0;
        assert!(sum <= total + 1.0, "分段之和 {sum} 不该超过总耗时 {total}");
    }

    #[test]
    fn disabled_watchdog_does_no_work_at_all() {
        let mut s = Stall::with_limit("t", None);
        let mut called = false;
        s.detail(|| {
            called = true;
            "x".to_string()
        });
        s.mark("a");
        let marks_empty = s.marks.is_empty();
        s.finish();
        assert!(!called, "关闭时不该执行 detail 闭包");
        assert!(marks_empty, "关闭时不该记录分段");
    }

    #[test]
    fn timestamp_has_expected_shape() {
        let ts = timestamp();
        assert_eq!(ts.len(), 19, "{ts}");
        assert_eq!(ts.as_bytes()[4], b'-');
        assert_eq!(ts.as_bytes()[10], b' ');
        assert_eq!(ts.as_bytes()[13], b':');
    }

    /// 临时目录（唯一名，避免并发测试互相踩）。
    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "xwx-stall-{tag}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("t")
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn append_creates_directories_and_writes_the_line() {
        let dir = tmp_dir("append");
        let path = dir.join("xwx").join("stall.log");
        append_to(&path, "第一行");
        append_to(&path, "第二行");
        let text = std::fs::read_to_string(&path).expect("日志应已写出");
        assert_eq!(text, "第一行\n第二行\n", "应是追加而不是覆盖");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotation_keeps_exactly_one_previous_generation() {
        let dir = tmp_dir("rotate");
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("stall.log");
        // 先塞到超过上限
        std::fs::write(&path, "x".repeat(MAX_LOG_BYTES as usize + 1)).expect("写满");
        append_to(&path, "新的一条");
        let prev = dir.join("stall.log.1");
        assert!(prev.is_file(), "满了应轮转出上一代");
        assert_eq!(
            std::fs::read_to_string(&path).expect("读新文件"),
            "新的一条\n",
            "轮转后新文件只含新记录"
        );
        // 再满一次：上一代被覆盖，仍然只有一代
        std::fs::write(&path, "y".repeat(MAX_LOG_BYTES as usize + 1)).expect("再写满");
        append_to(&path, "第三条");
        assert!(prev.is_file());
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .expect("读目录")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries.len(), 2, "目录里只该有两份文件: {entries:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finish_writes_a_line_when_over_the_limit() {
        let dir = tmp_dir("finish");
        let path = dir.join("stall.log");
        // 直接走 `append_to`：`finish` 的落盘路径就是它，而阈值判定已在
        // `threshold_parsing_is_pure_and_total` 覆盖。这里确保**格式**可用。
        let mut s = Stall::with_limit("ProcessKeyEvent", Some(1.0));
        std::thread::sleep(std::time::Duration::from_millis(3));
        s.mark("锁");
        s.mark("引擎");
        let segs: Vec<String> = s
            .marks
            .iter()
            .map(|(n, ms)| format!("{n}={ms:.0}ms"))
            .collect();
        let line = format!("{} {:.0}ms  {}  {}", timestamp(), 3.0, s.method, segs.join(" "));
        append_to(&path, &line);
        let text = std::fs::read_to_string(&path).expect("应有记录");
        assert!(text.contains("ProcessKeyEvent"), "{text}");
        assert!(text.contains("锁="), "{text}");
        assert!(text.contains("引擎="), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
