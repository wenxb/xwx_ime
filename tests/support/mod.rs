//! Hermetic D-Bus end-to-end test harness for the XWX IBus engine.
//!
//! # Why this exists
//!
//! Every other test in this workspace is a unit test: it calls
//! `InputSession::handle` / `ImeEngine::lookup_candidates` in-process and
//! therefore **structurally cannot** reach the D-Bus boundary. The paths that
//! live only there (docs/HANDOFF.md §6, docs/VERIFY.md §2) are:
//!
//! - the `bool` return value of `ProcessKeyEvent` — the "does the app get the
//!   raw key" contract (English-mode passthrough, idle punctuation
//!   passthrough, arrow keys on an empty pinyin string);
//! - the three signals `CommitText` / `UpdatePreeditText` /
//!   `UpdateLookupTable` and their GVariant payloads (does the *rendered*
//!   candidate really read 北京?);
//! - `FocusOut` / `Reset` / `Disable` and the panel callbacks
//!   (`PageUp` / `PageDown` / `CandidateClicked` / `CursorUp` / `CursorDown`).
//!
//! # Hermeticity (hard requirement)
//!
//! A test run MUST NOT touch:
//!
//! - `~/.local/share/xwx/*` — the user's real dictionary / language model /
//!   **learning data**. `UserLM::open_or_create` writes `user_bigram.txt`, so
//!   every spawned engine gets `XWX_USER_LM_PATH` pointed into a temp dir (and
//!   `HOME` / `XDG_DATA_HOME` are redirected too, as a second line of defence);
//! - `/usr/share/ibus/component/` — never read or written here;
//! - the real `ibus-daemon` — we start our own private `dbus-daemon` and pass
//!   its address via `IBUS_ADDRESS`, so `src/main.rs:284` `ibus_address()`
//!   short-circuits and `start_ibus_daemon()` is never reached;
//! - `~/.local/share/fcitx5/rime` — never referenced.
//!
//! `Harness::drop` kills the engine and the private bus on both success and
//! failure paths (no orphan processes on assertion panics).
//!
//! # How it speaks D-Bus
//!
//! Real processes, real bus, real wire traffic:
//!
//! 1. `dbus-daemon --session --print-address --nofork` → private bus address;
//! 2. the real `xwx_ime --ibus` binary is spawned with that address;
//! 3. the engine claims `org.freedesktop.IBus.Engine.xwx` and exports
//!    `/org/freedesktop/IBus/Engine/xwx`; we **poll** for the bus name (never
//!    a fixed sleep) because the engine mmaps a dictionary at startup;
//! 4. method calls go through `zbus` (already a dependency of the root
//!    package) and signals are captured by a `zbus` match-rule iterator.
//!    `busctl`/`gdbus`/`dbus-monitor` were used to prototype this and give the
//!    same results, but staying in-process keeps parsing authoritative: no
//!    scanner can confuse the *candidate* string `"IBusLookupTable"` with the
//!    type-name string next to it.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type as MsgType;
use zbus::MatchRule;
use zvariant::{Array, OwnedValue, Structure, Value};

/// Engine service name (mirrors `xwx_ibus::ibus_proto::ENGINE_SERVICE`).
pub const ENGINE_SERVICE: &str = "org.freedesktop.IBus.Engine.xwx";
/// Engine object path (mirrors `ENGINE_PATH`).
pub const ENGINE_PATH: &str = "/org/freedesktop/IBus/Engine/xwx";
/// Engine interface (mirrors `engine.rs`'s `#[interface(name = ...)]`).
pub const ENGINE_IFACE: &str = "org.freedesktop.IBus.Engine";

// —— IBus modifier masks (mirrors crates/xwx_ibus/src/ibus_proto.rs:62-69) ——
pub const M_SHIFT: u32 = 1 << 0;
pub const M_LOCK: u32 = 1 << 1;
pub const M_CONTROL: u32 = 1 << 2;
pub const M_RELEASE: u32 = 1 << 30;

/// IBus keysyms used by the tests (mirrors `ibus_proto::keyval`).
pub mod keyval {
    pub const SPACE: u32 = 0x20;
    pub const RETURN: u32 = 0xff0d;
    pub const ESCAPE: u32 = 0xff1b;
    pub const BACKSPACE: u32 = 0xff08;
    pub const LEFT: u32 = 0xff51;
    pub const UP: u32 = 0xff52;
    pub const RIGHT: u32 = 0xff53;
    pub const DOWN: u32 = 0xff54;
    pub const PAGE_UP: u32 = 0xff55;
    pub const PAGE_DOWN: u32 = 0xff56;
    pub const DELETE: u32 = 0xffff;

    /// ASCII keyval for a letter/digit/punctuation (`'a'` → 0x61).
    pub fn ascii(c: char) -> u32 {
        c as u32
    }
}

/// Generous ceiling for "engine appeared on the bus".
///
/// The tiny fixture engine is ready in well under a second, but the same
/// harness must stay honest when pointed at the *real* data set (80 MB
/// `dict.bin` + a 420 MB `.gram`), which takes a long time to mmap. Polling
/// with a ceiling is the point: a fixed sleep is either flaky or slow.
pub const NAME_TIMEOUT: Duration = Duration::from_secs(30);

/// Hard ceiling on a single engine method call.
///
/// `zbus`'s blocking `call_method` has **no** timeout of its own. When the
/// engine stops replying, the call parks in `epoll_wait(-1)` forever and the
/// test runner's join never returns — the suite hangs with no failing test to
/// point at (observed: a run wedged for 15+ minutes and left four orphaned
/// engines on the box). This bound turns that into an ordinary failure naming
/// the method.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(10);

fn msg<T: std::fmt::Display>(e: T) -> String {
    e.to_string()
}

// ===========================================================================
// Fixture: a tiny deterministic dictionary
// ===========================================================================

/// A small, deterministic dictionary built at test time.
///
/// `data/dict.bin` (80 MB) and `data/abbrev.bin` (69 MB) are gitignored and may
/// be absent, so the core tests must never depend on them. This fixture is
/// built from an in-memory SQLite table with a handful of controlled entries
/// and converted with the project's own `scripts/dict_to_bin.py`
/// (`SELECT pinyin, word, freq FROM entries` — the schema is read by that
/// script, see its line 33).
///
/// The entries are chosen so that the expectations are *exact*:
///
/// | pinyin key  | words (freq desc)                                        |
/// |-------------|----------------------------------------------------------|
/// | `ni`        | 你 泥 尼 逆 匿 腻 拟 妮 倪 霓 溺 昵  (12 → 2 pages)      |
/// | `hao`       | 好 号 毫 豪 浩 皓 耗 郝 嚎 壕 濠 蚝 颢 昊 (14 → 2 pages)  |
/// | `ni hao`    | 你好                                                     |
/// | `bei jing`  | 北京                                                     |
/// | `zhong guo` | 中国                                                     |
/// | `xian`      | 先                                                       |
/// | `xi an`     | 西安                                                     |
///
/// Because the engine sorts by frequency when the language model is empty, the
/// expected candidate order is exactly the frequency order above.
pub struct Fixture {
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    dict_bin: PathBuf,
    abbrev_bin: PathBuf,
    sqlite: PathBuf,
}

/// One fixture per test *process*, built once and shared read-only.
///
/// Tests run on parallel threads; building a fresh SQLite + running two Python
/// converters per test would double the suite's wall time for no benefit. The
/// fixture is immutable after construction.
pub fn shared_fixture() -> &'static Fixture {
    static FIX: std::sync::OnceLock<Fixture> = std::sync::OnceLock::new();
    FIX.get_or_init(Fixture::build)
}

impl Fixture {
    /// Build the fixture (requires `python3`, like the rest of the project's
    /// data pipeline).
    pub fn build() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir for fixture");
        let root = tmp.path().to_path_buf();
        let sqlite = root.join("dict.sqlite3");
        let dict_bin = root.join("dict.bin");

        let rows: &[(&str, &str, u32)] = &[
            ("ni hao", "你好", 9000),
            ("bei jing", "北京", 8000),
            ("zhong guo", "中国", 7000),
            ("xian", "先", 6000),
            ("ni", "你", 5000),
            ("ni", "泥", 4900),
            ("ni", "尼", 4800),
            ("ni", "逆", 4700),
            ("ni", "匿", 4600),
            ("ni", "腻", 4500),
            ("ni", "拟", 4400),
            ("ni", "妮", 4300),
            ("ni", "倪", 4200),
            ("ni", "霓", 4100),
            ("ni", "溺", 3900),
            ("ni", "昵", 3800),
            ("hao", "好", 4000),
            ("hao", "号", 3900),
            ("hao", "毫", 3800),
            ("hao", "豪", 3700),
            ("hao", "浩", 3600),
            ("hao", "皓", 3500),
            ("hao", "耗", 3400),
            ("hao", "郝", 3300),
            ("hao", "嚎", 3200),
            ("hao", "壕", 3100),
            ("hao", "濠", 3000),
            ("hao", "蚝", 2900),
            ("hao", "颢", 2800),
            ("hao", "昊", 2700),
            ("xi an", "西安", 3000),
            // Entries whose *keys* cover the prefixes used by the per-keystroke
            // end-to-end sequences (`zhang` covers z/zh/zha/zhan/zhang, `wang`
            // covers w/wa/wan/wang, `hao` covers h/ha/hao for `http:`).
            // Without them the fallback candidate list is empty for `www`, `.`
            // would fall through to the punctuation arm, and the URL sequence
            // could not be typed at all.
            ("zhang", "张", 2600),
            ("san", "三", 2400),
            ("feng", "风", 2200),
            ("wang", "王", 2000),
            ("ren", "人", 1900),
            // Needed by the abbreviation side index: `zgr` is its initials.
            ("zhong guo ren", "中国人", 5000),
        ];

        // Build the SQLite source with python3 (stdlib sqlite3; no new crates).
        let mut script = String::from(
            "import sqlite3,sys\n\
             c=sqlite3.connect(sys.argv[1])\n\
             c.execute('CREATE TABLE entries (pinyin TEXT, word TEXT, freq INTEGER)')\n\
             rows=[\n",
        );
        for (p, w, f) in rows {
            script.push_str(&format!("({p:?},{w:?},{f}),\n"));
        }
        script.push_str(
            "]\n\
             c.executemany('INSERT INTO entries VALUES (?,?,?)',rows)\n\
             c.commit()\n",
        );
        run_python(&script, &[sqlite.as_os_str()], "build fixture sqlite");

        let script_path = repo_root().join("scripts/dict_to_bin.py");
        let out = Command::new("python3")
            .arg(&script_path)
            .arg(&sqlite)
            .arg(&dict_bin)
            .output()
            .unwrap_or_else(|e| panic!("运行 {} 失败: {e}", script_path.display()));
        assert!(
            out.status.success(),
            "dict_to_bin.py 失败: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );

        // The abbreviation side index is built from the same dictionary so the
        // abbreviation tests exercise the real `MmapDict::with_abbrev` path.
        // `abbrev_to_bin.py` refuses to overwrite its input, hence the copy.
        let scratch = root.join("abbrev-src");
        std::fs::create_dir_all(&scratch).expect("mkdir abbrev-src");
        let src = scratch.join("dict.bin");
        std::fs::copy(&dict_bin, &src).expect("copy dict.bin");
        let abbrev_bin = scratch.join("abbrev.bin");
        let script = repo_root().join("scripts/abbrev_to_bin.py");
        let out = Command::new("python3")
            .arg(&script)
            .arg(&src)
            .arg(&abbrev_bin)
            .output()
            .unwrap_or_else(|e| panic!("运行 {} 失败: {e}", script.display()));
        assert!(
            out.status.success(),
            "abbrev_to_bin.py 失败: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(abbrev_bin.is_file(), "abbrev.bin 未生成");

        Self {
            _tmp: tmp,
            root,
            dict_bin,
            abbrev_bin,
            sqlite,
        }
    }

    pub fn dict_bin(&self) -> &Path {
        &self.dict_bin
    }

    pub fn abbrev_bin(&self) -> &Path {
        &self.abbrev_bin
    }

    /// The SQLite source (kept around for debugging fixture failures).
    pub fn sqlite(&self) -> &Path {
        &self.sqlite
    }
}

/// Workspace root, derived from this crate's manifest dir (`<root>/tests/..`).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The binary under test, built by the same cargo invocation (so it is not
/// stale), located via the `.cargo/config.toml` target dir.
pub fn engine_bin() -> PathBuf {
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(repo_root())
        .output()
        .expect("cargo metadata");
    assert!(out.status.success(), "cargo metadata 失败");
    let json = String::from_utf8_lossy(&out.stdout);
    let key = "\"target_directory\":";
    let at = json.find(key).expect("target_directory 不在 cargo metadata 输出里") + key.len();
    let rest = &json[at..];
    let rest = rest.trim_start();
    let start = rest.find('"').expect("target_directory 格式异常") + 1;
    let end = rest[start..].find('"').expect("target_directory 格式异常") + start;
    let target_dir = PathBuf::from(&rest[start..end]);
    let is_debug = cfg!(debug_assertions);
    let bin = target_dir
        .join(if is_debug { "debug" } else { "release" })
        .join("xwx_ime");
    assert!(
        bin.is_file(),
        "找不到被测二进制 {}（先 cargo build --bin xwx_ime）",
        bin.display()
    );
    bin
}

fn run_python(script: &str, args: &[&std::ffi::OsStr], what: &str) {
    let mut cmd = Command::new("python3");
    cmd.arg("-c").arg(script);
    for a in args {
        cmd.arg(a);
    }
    let out = cmd.output().unwrap_or_else(|e| panic!("{what}: python3 失败: {e}"));
    assert!(
        out.status.success(),
        "{what}: python3 退出码 {:?}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

// ===========================================================================
// Signals
// ===========================================================================

/// One captured D-Bus signal.
#[derive(Debug)]
pub struct Signal {
    pub member: String,
    pub value: OwnedValue,
}

/// Decoded `IBusLookupTable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupTable {
    /// `page_size` (field 2).
    pub page_size: u32,
    /// `cursor` **within the page** (field 3) — NOT a global candidate index.
    pub cursor_in_page: u32,
    pub cursor_visible: bool,
    pub round: bool,
    pub orientation: i32,
    /// Candidate words of this page, in display order (field 7, `av`).
    pub candidates: Vec<String>,
    /// Number of label entries (field 8); the engine leaves it empty so the
    /// panel auto-numbers 1..9.
    pub labels: usize,
}

/// Get a structure out of a value that may be a **variant wrapping** one.
///
/// IBus puts emitted objects behind a variant everywhere: the signal body's
/// field 0 is a `v`, and every element of the candidate `av` is a `v`. A bare
/// `Structure::try_from(value)` fails on those, which silently produced empty
/// text/candidate lists.
fn unwrap_variant(v: &Value<'_>) -> Option<Structure<'static>> {
    let owned: OwnedValue = match v {
        Value::Value(inner) => (**inner).try_to_owned().ok()?,
        other => other.try_to_owned().ok()?,
    };
    Structure::try_from(Value::from(owned)).ok()
}

impl Signal {
    /// The signal's **full wire body**, e.g. `(vubu)` for `UpdatePreeditText`.
    ///
    /// Field indices here are the signal's own arguments: for
    /// `UpdatePreeditText(v, u, b, u)` index 1 is `cursor_pos`, 2 `visible`,
    /// 3 `mode`. Use this for those; use [`Self::structure`] for the payload
    /// object itself.
    fn body_structure(&self) -> Option<Structure<'static>> {
        Structure::try_from(Value::from(self.value.try_clone().ok()?)).ok()
    }

    /// The signal's **payload** structure (the `IBusText` / `IBusLookupTable`).
    ///
    /// IBus wraps the emitted object in a variant: the wire body of
    /// `UpdatePreeditText` is `(vubu)` and of `UpdateLookupTable` is `(vb)`,
    /// where field 0 is a `v` holding the `IBusText` / `IBusLookupTable`
    /// structure. Callers want that *inner* object — its signature is the
    /// `(sa{sv}sv)` / `(sa{sv}uubbiavav)` the assertions pin, and its field
    /// indices are the ones `as_text` / `as_lookup_table` read.
    ///
    /// So unwrap the variant here, once, instead of at every accessor. (Reading
    /// field 2 of the *outer* body used to yield `Bool(true)` and made every
    /// text assertion come back `None` while the engine was emitting correctly.)
    fn structure(&self) -> Option<Structure<'static>> {
        let outer = self.body_structure()?;
        if let Some(Value::Value(inner)) = outer.fields().first() {
            if let Ok(v) = (**inner).try_clone() {
                if let Ok(inner_st) = Structure::try_from(v) {
                    return Some(inner_st);
                }
            }
        }
        Some(outer)
    }

    /// Top-level string fields, in wire order.
    ///
    /// For an `IBusText` this is `["IBusText", <text>]`; for an
    /// `IBusLookupTable` it is `["IBusLookupTable"]` (candidates live in the
    /// nested `av`). Calling this out explicitly matters: a naive text scan of
    /// the wire bytes also picks up the literal `"IBusText"` /
    /// `"IBusAttrList"` type names, which is exactly the kind of false
    /// observation this suite must avoid.
    pub fn strings(&self) -> Vec<String> {
        self.structure()
            .map(|st| {
                st.fields()
                    .iter()
                    .filter_map(|f| f.downcast_ref::<&str>().ok().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Structure view for signature assertions (public so tests can pin the
    /// exact wire signature, not just the decoded fields).
    pub fn structure_for_test(&self) -> Structure<'static> {
        self.structure().expect("信号负载应为结构体")
    }

    /// Read a `u32` field of the signal's **body** by wire index.
    ///
    /// Body, not payload: `cursor_pos` / `mode` are arguments of the signal
    /// itself (`UpdatePreeditText(v, u, b, u)`), not fields of the `IBusText`.
    pub fn as_u32_field(&self, idx: usize) -> Option<u32> {
        self.body_structure()?
            .fields()
            .get(idx)?
            .downcast_ref::<u32>()
            .ok()
    }

    /// Read a `bool` field of the signal's **body** by wire index
    /// (`visible` for both `UpdatePreeditText` and `UpdateLookupTable`).
    pub fn as_bool_field(&self, idx: usize) -> Option<bool> {
        self.body_structure()?
            .fields()
            .get(idx)?
            .downcast_ref::<bool>()
            .ok()
    }

    /// `IBusText` payload (field 2). `None` for signals with no text.
    pub fn as_text(&self) -> Option<String> {
        let st = self.structure()?;
        st.fields()
            .get(2)?
            .downcast_ref::<&str>()
            .ok()
            .map(str::to_string)
    }

    /// `IBusLookupTable` payload, or `None` for other signals.
    pub fn as_lookup_table(&self) -> Option<LookupTable> {
        let st = self.structure()?;
        let f = st.fields();
        if f.len() != 9 {
            return None;
        }
        let page_size = f[2].downcast_ref::<u32>().ok()?;
        let cursor_in_page = f[3].downcast_ref::<u32>().ok()?;
        let cursor_visible = f[4].downcast_ref::<bool>().ok()?;
        let round = f[5].downcast_ref::<bool>().ok()?;
        let orientation = f[6].downcast_ref::<i32>().ok()?;
        let arr = Array::try_from(f[7].try_clone().ok()?).ok()?;
        let candidates = arr
            .inner()
            .iter()
            // Each element is an `IBusText` wrapped in a variant (`av`), so it
            // must be unwrapped before it is a structure — this is why the
            // candidate list used to come back empty while the engine was
            // sending nine correct words.
            .filter_map(|v| {
                let text = unwrap_variant(v)?;
                text.fields()
                    .get(2)?
                    .downcast_ref::<&str>()
                    .ok()
                    .map(str::to_string)
            })
            .collect();
        let labels = Array::try_from(f[8].try_clone().ok()?)
            .ok()?
            .inner()
            .len();
        Some(LookupTable {
            page_size,
            cursor_in_page,
            cursor_visible,
            round,
            orientation,
            candidates,
            labels,
        })
    }
}

/// Convenience readers over a captured batch of signals.
/// `OwnedValue` is not `Clone`, so a signal is cloned field-by-field.
pub fn clone_signal(s: &Signal) -> Signal {
    Signal {
        member: s.member.clone(),
        value: s.value.try_clone().expect("克隆信号负载"),
    }
}

pub fn member_names(signals: &[Signal]) -> Vec<&str> {
    signals.iter().map(|s| s.member.as_str()).collect()
}

/// `last_commit` reachable from an inherent method body above the free
/// functions' definition order.
fn support_last_commit(signals: &[Signal]) -> Option<String> {
    last_commit(signals)
}

/// Every candidate page the engine sent, in order.
pub fn lookup_tables(signals: &[Signal]) -> Vec<LookupTable> {
    signals
        .iter()
        .filter(|s| s.member == "UpdateLookupTable")
        .filter_map(|s| s.as_lookup_table())
        .collect()
}

/// The last `UpdateLookupTable` payload in the batch.
pub fn last_table(signals: &[Signal]) -> Option<LookupTable> {
    lookup_tables(signals).pop()
}

/// The last `UpdatePreeditText` text in the batch.
pub fn last_preedit(signals: &[Signal]) -> Option<String> {
    signals
        .iter()
        .rev()
        .find(|s| s.member == "UpdatePreeditText")
        .and_then(|s| s.as_text())
}

/// The last `CommitText` text in the batch.
pub fn last_commit(signals: &[Signal]) -> Option<String> {
    signals
        .iter()
        .rev()
        .find(|s| s.member == "CommitText")
        .and_then(|s| s.as_text())
}

/// All `CommitText` texts in the batch, in order.
pub fn commits(signals: &[Signal]) -> Vec<String> {
    signals
        .iter()
        .filter(|s| s.member == "CommitText")
        .filter_map(|s| s.as_text())
        .collect()
}

// ===========================================================================
// Harness
// ===========================================================================

/// A private bus + a spawned engine + a signal collector, all cleaned up on
/// drop.
pub struct Harness {
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    address: String,
    bus: Child,
    engine: Option<Child>,
    /// PID of the engine child, kept so a stalled call can report whether the
    /// process is still alive (`Child::try_wait` needs `&mut`; `/proc` does not).
    engine_pid: Option<u32>,
    control: Connection,
    monitor: Option<Connection>,
    rx: Option<Receiver<Signal>>,
    /// Signals captured before the last `drain()` (kept so a helper can look at
    /// everything observed so far without re-plumbing the channel).
    backlog: VecDeque<Signal>,
    engine_log: PathBuf,
}

impl Harness {
    /// Start a private bus. The engine is started separately with
    /// [`Harness::start_engine`].
    pub fn new(label: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir for harness");
        let root = tmp.path().to_path_buf();
        for sub in ["cfg", "xdg", "run", "home"] {
            std::fs::create_dir_all(root.join(sub)).expect("mkdir harness subdir");
        }
        assert!(
            !label.is_empty(),
            "label 用于 engine.log 命名，不应为空"
        );

        let (bus, address) = spawn_private_bus();

        let control = zbus::blocking::connection::Builder::address(address.as_str())
            .expect("解析私有总线地址")
            .build()
            .unwrap_or_else(|e| panic!("连接私有总线失败: {e}"));

        let engine_log = root.join("engine.log");

        Self {
            _tmp: tmp,
            root,
            address,
            bus,
            engine: None,
            engine_pid: None,
            control,
            monitor: None,
            rx: None,
            backlog: VecDeque::new(),
            engine_log,
        }
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    /// The private bus's pid (for the Drop-cleanup test).
    pub fn bus_pid_for_test(&self) -> u32 {
        self.bus.id()
    }

    /// Stop the engine with **SIGINT** so `src/main.rs`'s `tokio::signal::ctrl_c`
    /// branch runs its flush-the-tail path (`main.rs:201-212`). This is the only
    /// way to make `UserLM::flush` reach disk deterministically without waiting
    /// for the 64-learning threshold or the 30 s tick.
    ///
    /// Bounded: waits up to 5 s for the process to exit, then falls back to the
    /// hard kill in `Drop`.
    pub fn shutdown_engine_gracefully(&mut self) {
        if let Some(child) = self.engine.as_mut() {
            let pid = child.id().to_string();
            let _ = Command::new("kill")
                .args(["-INT", &pid])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            for _ in 0..100 {
                match child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) => thread::sleep(Duration::from_millis(50)),
                    Err(_) => return,
                }
            }
        }
    }

    /// The spawned engine's pid (0 when not started). Used by the hermeticity
    /// test to deliver SIGINT and exercise the flush-on-exit path.
    pub fn engine_pid_for_test(&self) -> u32 {
        self.engine.as_ref().map(|c| c.id()).unwrap_or(0)
    }

    /// Spawn `xwx_ime --ibus` against this bus with fully redirected data
    /// paths, then poll until it owns `org.freedesktop.IBus.Engine.xwx`.
    pub fn start_engine(&mut self, fix: &Fixture) {
        self.start_engine_with(fix.dict_bin(), Some(fix.abbrev_bin()), NAME_TIMEOUT)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// Like [`Harness::start_engine`] but with the abbreviation side index
    /// deliberately absent: `XWX_ABBREV_PATH` points at a non-existent file,
    /// which is the production state when the 69 MB `abbrev.bin` was never
    /// generated (`MmapDict::with_abbrev` warns and degrades).
    pub fn start_engine_without_abbrev(&mut self, fix: &Fixture) {
        let absent = self.root.join("absent-abbrev.bin");
        self.start_engine_with(fix.dict_bin(), Some(absent.as_path()), NAME_TIMEOUT)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// Point every data path at something that does not exist: no dictionary,
    /// no side index, no language model. Used to prove the harness is
    /// hermetic rather than data-dependent.
    pub fn start_engine_with_no_data(&mut self) {
        let absent = self.root.join("absent-dict.bin");
        self.start_engine_with(&absent, Some(absent.as_path()), NAME_TIMEOUT)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    pub fn start_engine_with(
        &mut self,
        dict: &Path,
        abbrev: Option<&Path>,
        timeout: Duration,
    ) -> Result<(), String> {
        assert!(self.engine.is_none(), "一个 Harness 只应启动一个引擎");
        // Materialise the paths first: `Command::env` needs owned values that
        // outlive the borrow, and `Option<&PathBuf>` does not coerce to
        // `Option<&Path>`.
        let abbrev_path = abbrev
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.join("absent-abbrev.bin"));
        let lm_path = self.root.join("absent-lm.gram");
        let user_lm_path = self.root.join("cfg/user_bigram.txt");
        let log = std::fs::File::create(&self.engine_log).map_err(msg)?;
        let log2 = log.try_clone().map_err(msg)?;
        let bin = engine_bin();
        let child = Command::new(&bin)
            .arg("--ibus")
            .env("IBUS_ADDRESS", &self.address)
            .env("DBUS_SESSION_BUS_ADDRESS", &self.address)
            .env("XWX_DICT_PATH", dict)
            .env("XWX_ABBREV_PATH", &abbrev_path)
            // Explicit non-existent .gram: src/main.rs honours XWX_LM_PATH
            // verbatim and degrades to the empty bigram model instead of
            // scanning for a 420 MB model. Deterministic and instant.
            .env("XWX_LM_PATH", &lm_path)
            // The one that matters for hermeticity: UserLM::open_or_create
            // writes this file, so it must never point at the user's data dir.
            .env("XWX_USER_LM_PATH", &user_lm_path)
            .env("XDG_DATA_HOME", self.root.join("xdg"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("HOME", self.root.join("home"))
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log2))
            .spawn()
            .map_err(|e| format!("启动 {} 失败: {e}", bin.display()))?;
        self.engine_pid = Some(child.id());
        self.engine = Some(child);
        // Start listening **before** the test acts. The monitor used to be
        // created lazily inside the first `settle`/`drain`, so a test that typed
        // before its first settle emitted its signals before the match rule
        // existed and then saw an empty batch — `last_table` returned `None`
        // and the test blamed the engine.
        self.ensure_monitor();
        self.wait_for_name(timeout)
    }

    /// Poll `GetNameOwner` until the engine answers, or panic with the engine
    /// log so a startup failure is diagnosable instead of a mystery timeout.
    pub fn wait_for_name(&self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            // Owning the name is necessary but NOT sufficient: the engine claims
            // it before its object server starts dispatching, and a call that
            // lands in that window is never answered. Observed as a bare
            // `FocusOut` timeout on the *first* call after startup, with the
            // engine process alive and idle (`/proc` state `S`) and a clean log.
            // `Peer.Ping` is answered by zbus itself, so it is a true readiness
            // probe rather than a guess.
            if self.name_owner().is_some() && self.ping().is_ok() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                let log = std::fs::read_to_string(&self.engine_log).unwrap_or_default();
                return Err(format!(
                    "引擎未在 {:?} 内占用总线名 {ENGINE_SERVICE}\n--- engine.log ---\n{log}",
                    timeout
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn name_owner(&self) -> Option<String> {
        let reply = self
            .control
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "GetNameOwner",
                &(ENGINE_SERVICE,),
            )
            .ok()?;
        reply.body().deserialize::<String>().ok()
    }

    /// Readiness probe: `org.freedesktop.DBus.Peer.Ping` on the engine object.
    ///
    /// zbus answers this itself, so a reply proves the object server is
    /// dispatching — unlike `GetNameOwner`, which only proves the name is
    /// claimed. Bounded well below `CALL_TIMEOUT` because it is called in a
    /// startup poll loop.
    fn ping(&self) -> Result<(), String> {
        let conn = self.control.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let r = conn
                .call_method(
                    Some(ENGINE_SERVICE),
                    ENGINE_PATH,
                    Some("org.freedesktop.DBus.Peer"),
                    "Ping",
                    &(),
                )
                .map(|_| ())
                .map_err(|e| e.to_string());
            let _ = tx.send(r);
        });
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(r) => r,
            Err(_) => Err("Ping 超时(对象服务尚未开始分发)".into()),
        }
    }

    /// Raw method call against the engine object.
    pub fn call<B>(&self, method: &str, body: B) -> Result<zbus::Message, String>
    where
        B: zvariant::DynamicType + serde::Serialize + Send + 'static,
    {
        // Run the (unbounded) blocking call on a worker thread and bound our
        // wait, so a wedged engine cannot hang the suite — see `CALL_TIMEOUT`.
        let conn = self.control.clone();
        let name = method.to_string();
        let worker_name = name.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            // Bind the label separately: `as_str()` borrows the value while
            // `format!` would otherwise capture that same value in one
            // expression.
            let label = worker_name.clone();
            let r = conn
                .call_method(
                    Some(ENGINE_SERVICE),
                    ENGINE_PATH,
                    Some(ENGINE_IFACE),
                    worker_name.as_str(),
                    &body,
                )
                .map_err(|e| format!("调用 {label} 失败: {e}"));
            let _ = tx.send(r);
        });
        match rx.recv_timeout(CALL_TIMEOUT) {
            Ok(r) => r,
            Err(_) => {
                // Attach the engine's log tail: a stalled call is otherwise a
                // mystery, and the tempdir (with engine.log) is deleted when the
                // Harness drops, so this is the only chance to surface it.
                let state = self.engine_pid.and_then(|pid| {
                    std::fs::read_to_string(format!("/proc/{pid}/stat"))
                        .ok()
                        .and_then(|s| {
                            // "pid (comm) S ..." — comm may contain spaces/parens.
                            s.rsplit(')')
                                .next()
                                .and_then(|r| r.split_whitespace().next().map(str::to_string))
                        })
                });
                let log = std::fs::read_to_string(&self.engine_log).unwrap_or_default();
                let tail: Vec<&str> = log.lines().rev().take(30).collect();
                let tail: Vec<&str> = tail.into_iter().rev().collect();
                Err(format!(
                    "引擎未在 {CALL_TIMEOUT:?} 内回复 {name}(pid={:?} /proc 状态={:?})\n\
                     --- engine.log 末尾 ---\n{}",
                    self.engine_pid, state, tail.join("\n")
                ))
            }
        }
    }

    // —— the `ProcessKeyEvent` contract: bool out, so it is asserted, not ignored ——

    /// Send one key press and return the engine's "did I consume it" bool.
    ///
    /// `state` is the IBus modifier mask; `keycode` is passed as 0 (the engine
    /// documents that it does not use it, `engine.rs:164`).
    pub fn key(&self, keyval: u32, state: u32) -> bool {
        match self.key_checked(keyval, state) {
            Ok(v) => v,
            Err(e) => panic!("{e}"),
        }
    }

    pub fn key_checked(&self, keyval: u32, state: u32) -> Result<bool, String> {
        let reply = self.call("ProcessKeyEvent", (keyval, 0u32, state))?;
        let v: bool = reply
            .body()
            .deserialize()
            .map_err(|e| format!("ProcessKeyEvent 返回值不是 bool: {e}"))?;
        Ok(v)
    }

    /// Type an ASCII string as individual key presses (no modifiers).
    pub fn type_ascii(&self, s: &str) -> Vec<bool> {
        s.chars()
            .map(|c| self.key(keyval::ascii(c), 0))
            .collect()
    }

    /// Ctrl+Space (keyval 0x20, `IBUS_CONTROL_MASK` = 1<<2, no other
    /// modifiers). Must return `true` — the application must not see it.
    pub fn toggle_ascii(&self) -> bool {
        self.key(keyval::SPACE, M_CONTROL)
    }

    // —— panel / lifecycle methods (void returns) ——

    pub fn focus_in(&self) -> Result<(), String> {
        self.call("FocusIn", ()).map(|_| ())
    }
    pub fn focus_out(&self) -> Result<(), String> {
        self.call("FocusOut", ()).map(|_| ())
    }
    pub fn reset_session(&self) -> Result<(), String> {
        self.call("Reset", ()).map(|_| ())
    }
    pub fn enable(&self) -> Result<(), String> {
        self.call("Enable", ()).map(|_| ())
    }
    pub fn disable(&self) -> Result<(), String> {
        self.call("Disable", ()).map(|_| ())
    }
    pub fn page_up(&self) -> Result<(), String> {
        self.call("PageUp", ()).map(|_| ())
    }
    pub fn page_down(&self) -> Result<(), String> {
        self.call("PageDown", ()).map(|_| ())
    }
    pub fn cursor_up(&self) -> Result<(), String> {
        self.call("CursorUp", ()).map(|_| ())
    }
    pub fn cursor_down(&self) -> Result<(), String> {
        self.call("CursorDown", ()).map(|_| ())
    }

    /// `CandidateClicked(index, button, state)` — `index` is the **global**
    /// candidate index (across pages), per `engine.rs:294`.
    pub fn candidate_clicked(&self, index: u32) -> Result<(), String> {
        self.call("CandidateClicked", (index, 1u32, 0u32)).map(|_| ())
    }

    // —— signals ——

    fn ensure_monitor(&mut self) {
        if self.monitor.is_some() {
            return;
        }
        let conn = zbus::blocking::connection::Builder::address(self.address.as_str())
            .expect("解析私有总线地址")
            .build()
            .expect("连接总线以捕获信号");
        // Scoped to the engine interface so method *replies* on the same
        // connection (the clock would otherwise never go quiet) are not mixed
        // into the stream.
        let rule = MatchRule::builder()
            .msg_type(MsgType::Signal)
            .interface(ENGINE_IFACE)
            .expect("interface 名合法")
            .build();
        let iter = MessageIterator::for_match_rule(rule, &conn, Some(4096))
            .expect("注册信号 match rule 失败");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for m in iter.flatten() {
                let Some(member) = m.header().member().map(|m| m.to_string()) else {
                    continue;
                };
                // Deserialize the body as a **`Structure`**, not an
                // `OwnedValue`.
                //
                // `Body::deserialize::<OwnedValue>()` expects the body to be a
                // single value; every IBus signal body is a multi-field struct
                // (`UpdatePreeditText` is `(vubu)`, `UpdateLookupTable` is
                // `(vbbu)`), so that deserialization FAILS and the signal was
                // silently discarded — leaving `sigs` empty and every signal
                // assertion reporting `已见: []` even though the engine had
                // emitted the signal correctly. `Structure` matches the struct
                // signature, and `try_to_owned` gives the `'static` value the
                // `Signal` keeps.
                let body = m.body();
                // `HidePreeditText()` / `HideLookupTable()` carry an **empty**
                // body, so deserializing a `Structure` fails. Keep those signals
                // with a body-less sentinel — tests assert that the hide signal
                // was emitted, and dropping it made them report `实际: []`.
                let value = match body.deserialize::<Structure<'_>>() {
                    Ok(st) => match Value::Structure(st).try_to_owned() {
                        Ok(v) => v,
                        Err(_) => continue,
                    },
                    Err(_) => match Value::U8(0u8).try_to_owned() {
                        Ok(v) => v,
                        Err(_) => continue,
                    },
                };
                if tx.send(Signal { member, value }).is_err() {
                    break;
                }
            }
        });
        // The match rule reaches the bus asynchronously; give it a moment
        // before the first action so no signal is missed.
        thread::sleep(Duration::from_millis(150));
        self.monitor = Some(conn);
        self.rx = Some(rx);
    }

    /// Non-blocking: everything queued right now.
    pub fn drain(&mut self) -> Vec<Signal> {
        self.ensure_monitor();
        let mut out = Vec::new();
        while let Some(s) = self.try_recv() {
            out.push(s);
        }
        out
    }

    fn try_recv(&mut self) -> Option<Signal> {
        let rx = self.rx.as_ref()?;
        rx.try_recv().ok()
    }

    fn recv_timeout(&mut self, d: Duration) -> Option<Signal> {
        let rx = self.rx.as_ref()?;
        match rx.recv_timeout(d) {
            Ok(s) => Some(s),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    /// Collect signals until the stream has been quiet for `gap`, or until
    /// `max` elapses (whichever comes first). Always bounded.
    ///
    /// Quiet-gap collection is the right shape here because the engine emits
    /// `UpdatePreeditText` and `UpdateLookupTable` as *two* async signals per
    /// keystroke with no ordering guarantee we should rely on (zbus dispatches
    /// each on its own task).
    pub fn settle(&mut self, gap: Duration, max: Duration) -> Vec<Signal> {
        self.ensure_monitor();
        let start = Instant::now();
        let mut out = Vec::new();
        loop {
            let elapsed = start.elapsed();
            if elapsed >= max {
                break;
            }
            let wait = gap.min(max - elapsed);
            match self.recv_timeout(wait) {
                Some(s) => out.push(s),
                None => break,
            }
        }
        self.backlog.extend(out.iter().map(clone_signal));
        out
    }

    /// Default settle budget: 300 ms of silence, 5 s ceiling.
    pub fn settle_default(&mut self) -> Vec<Signal> {
        self.settle(Duration::from_millis(300), Duration::from_secs(5))
    }

    /// Wait until a signal with `member` arrives (bounded).
    pub fn wait_signal(&mut self, member: &str, timeout: Duration) -> Result<Signal, String> {
        self.ensure_monitor();
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "等待信号 {member} 超时（{:?}）；已见: {:?}",
                    timeout,
                    member_names(&self.drain())
                ));
            }
            match self.recv_timeout(remaining.min(Duration::from_millis(200))) {
                Some(s) if s.member == member => return Ok(s),
                Some(s) => self.backlog.push_back(s),
                None => continue,
            }
        }
    }

    /// Record everything observed on the bus, cleared. Returns the batch.
    pub fn take_backlog(&mut self) -> Vec<Signal> {
        self.drain();
        let out: Vec<Signal> = self.backlog.iter().map(clone_signal).collect();
        self.backlog.clear();
        out
    }

    /// Wait until the engine sends a `CommitText` whose text equals `expect`
    /// (bounded). Returns the full batch captured while waiting.
    ///
    /// Matching on the *content* rather than on "a commit happened" is the
    /// point: a commit of the wrong text is a failure, not a pass.
    pub fn wait_commit(
        &mut self,
        expect: &str,
        timeout: Duration,
    ) -> Result<Vec<Signal>, String> {
        self.ensure_monitor();
        let deadline = Instant::now() + timeout;
        let mut batch: Vec<Signal> = Vec::new();
        loop {
            if batch
                .iter()
                .any(|s| s.member == "CommitText" && s.as_text().as_deref() == Some(expect))
            {
                return Ok(batch);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let seen: Vec<String> = batch
                    .iter()
                    .filter(|s| s.member == "CommitText")
                    .map(|s| s.as_text().unwrap_or_default())
                    .collect();
                return Err(format!(
                    "等待 CommitText({expect:?}) 超时；已见的 CommitText: {seen:?}；\
                     全部信号: {:?}",
                    member_names(&batch)
                ));
            }
            match self.recv_timeout(remaining.min(Duration::from_millis(100))) {
                Some(s) => batch.push(s),
                None => continue,
            }
        }
    }

    /// Type a full pinyin string and press space, returning the batch and the
    /// committed text (if any).
    pub fn type_and_commit(&mut self, pinyin: &str, timeout: Duration) -> (Vec<Signal>, Option<String>) {
        self.type_ascii(pinyin);
        let _ = self.settle(Duration::from_millis(80), timeout);
        self.clear_backlog();
        let consumed = self.key(keyval::SPACE, 0);
        assert!(consumed, "空格在有拼音串时必须被消费(true)");
        let batch = self.settle_default();
        let committed = support_last_commit(&batch);
        (batch, committed)
    }

    /// Everything captured so far (settled batches + wait results).
    pub fn backlog(&self) -> Vec<Signal> {
        self.backlog.iter().map(clone_signal).collect()
    }

    pub fn clear_backlog(&mut self) {
        self.backlog.clear();
        let _ = self.drain();
    }

}

impl Drop for Harness {
    /// Kill the engine and the private bus. Runs on assertion panics too, so a
    /// failing test never leaves orphan processes behind.
    fn drop(&mut self) {
        // Drop the monitor first so its match rule is deregistered while the
        // bus is still alive.
        self.monitor = None;
        self.rx = None;
        if let Some(mut c) = self.engine.take() {
            let _ = Command::new("kill")
                .args(["-9", &c.id().to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = c.wait();
        }
        let bus_pid = self.bus.id().to_string();
        let _ = Command::new("kill")
            .args(["-9", &bus_pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.bus.wait();
    }
}

/// Start `dbus-daemon --session --print-address --nofork` and return it with
/// the first line of its stdout (the bus address).
///
/// The reader runs on a helper thread so a daemon that never prints cannot hang
/// the suite; the thread keeps draining the pipe until the bus dies.
fn spawn_private_bus() -> (Child, String) {
    let mut child = Command::new("dbus-daemon")
        .args(["--session", "--print-address", "--nofork"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("无法执行 dbus-daemon（测试需要它）");
    let stdout = child.stdout.take().expect("dbus-daemon stdout");
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                // Nobody is listening any more; keep draining so the daemon
                // never sees a closed pipe.
            }
        }
    });
    let addr = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("等待 dbus-daemon 打印地址超时")
        .trim()
        .to_string();
    assert!(
        addr.starts_with("unix:") || addr.starts_with("tcp:"),
        "dbus-daemon 输出的地址不合预期: {addr:?}"
    );
    (child, addr)
}

// ===========================================================================
// Hermeticity probe (used by the isolation test)
// ===========================================================================

/// A cheap fingerprint of a path that must not change during a test run.
#[derive(Debug, PartialEq, Eq)]
pub struct PathFingerprint {
    pub exists: bool,
    pub len: u64,
    pub modified: Option<std::time::SystemTime>,
}

pub fn fingerprint(p: &Path) -> PathFingerprint {
    match std::fs::metadata(p) {
        Ok(m) => PathFingerprint {
            exists: true,
            len: m.len(),
            modified: m.modified().ok(),
        },
        Err(_) => PathFingerprint {
            exists: false,
            len: 0,
            modified: None,
        },
    }
}

/// The real user data paths this suite must never touch.
pub fn real_user_paths() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/nonexistent"));
    vec![
        home.join(".local/share/xwx/user_bigram.txt"),
        home.join(".local/share/xwx/dict.bin"),
        home.join(".local/share/xwx"),
        PathBuf::from("/usr/share/ibus/component/xwx.xml"),
    ]
}