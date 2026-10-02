//! 私有总线上的引擎生命周期:`Harness::new` 起 `dbus-daemon`,
//! 按需拉起引擎,`Drop` 保证不留孤儿进程。
//!
//! 按键(`keys.rs`)与信号收集(`signals.rs`)是同一个结构体的另外两块
//! `impl`;因为它们都是本模块的**子模块**,`Harness` 的私有字段对它们
//! 直接可见,不需要任何放宽可见性。

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use zbus::blocking::Connection;

use super::*;

mod keys;
mod signals;

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
}

impl Drop for Harness {
    /// Kill the engine and the private bus. Runs on assertion panics too, so a
    /// failing test never leaves orphan processes behind.
    fn drop(&mut self) {
        // 断言失败时 tempdir(以及里面的 engine.log)马上就要被删,而那正是
        // 最需要日志的时刻:`wait_for_name`/`call` 的超时路径已经会附日志,
        // 但**普通断言失败**(最常见的一类)原先只剩一句断言文本。
        if std::thread::panicking() {
            if let Ok(log) = std::fs::read_to_string(&self.engine_log) {
                let tail: Vec<&str> = log.lines().rev().take(40).collect();
                if !tail.is_empty() {
                    let tail: Vec<&str> = tail.into_iter().rev().collect();
                    eprintln!(
                        "--- engine.log 末尾(引擎马上被收掉,tempdir 即将删除)---\n{}",
                        tail.join("\n")
                    );
                }
            }
        }
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
