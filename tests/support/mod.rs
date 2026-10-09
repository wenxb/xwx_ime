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
//!   its address via `IBUS_ADDRESS`, so `src/main.rs:335` `ibus_address()`
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

//! # 文件布局(纯拆分,2026-10-02)
//!
//! 这个模块原来是单文件 1325 行,按「谁在用」拆成四块:
//! `fixture`(词库夹具与隔离探针)、`signal`(信号解包与取值)、
//! `harness`(私有总线上的引擎生命周期、按键、信号收集)、
//! 以及本文件的常量与 D-Bus 名字。**对外只用 `support::X` 一处名字**,
//! 拆分靠 `pub use`,测试文件一行都不用改。

mod fixture;
mod harness;
pub mod scenario;
mod signal;

pub use fixture::*;
pub use harness::*;
pub use signal::*;

use std::time::Duration;


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
    /// Tab（`0xff09`）——只在预测态接受下一词。
    pub const TAB: u32 = 0xff09;

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
