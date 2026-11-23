//! 私有 D-Bus 总线上的引擎进程生命周期管理与 `Drop` 清理。

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};
use zbus::blocking::Connection;

use super::fixture::engine_bin;
use super::*;

mod keys;
mod signals;

pub struct Harness {
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    address: String,
    bus: Child,
    engine: Option<Child>,
    engine_pid: Option<u32>,
    control: Connection,
    monitor: Option<Connection>,
    rx: Option<Receiver<Signal>>,
    engine_log: PathBuf,
    extra_env: Vec<(String, OsString)>,
}

impl Harness {
    pub fn new(label: &str) -> Self {
        assert!(!label.is_empty(), "label 不应为空");
        let tmp = tempfile::tempdir().expect("tempdir for harness");
        let root = tmp.path().to_path_buf();
        for sub in ["cfg", "xdg", "run", "home", "services"] {
            std::fs::create_dir_all(root.join(sub)).expect("mkdir harness subdir");
        }
        // 默认预置中文模式，避免非配置类测试受出厂 en 默认值干扰
        let cfg_dir = root.join("home/.config/xwx");
        std::fs::create_dir_all(&cfg_dir).expect("mkdir config dir");
        std::fs::write(cfg_dir.join("config.toml"), "default_language = \"zh\"\n")
            .expect("写测试配置");

        let (bus, address) = spawn_private_bus(&root);
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
            engine_log,
            extra_env: Vec::new(),
        }
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn engine_pid(&self) -> u32 {
        self.engine.as_ref().map_or(0, Child::id)
    }

    /// 发送信号（如 `-INT` / `-TERM`）等待引擎落盘并退出，超时交回 `Drop` 强杀。
    pub fn shutdown_engine_with(&mut self, signal: &str) {
        let Some(mut child) = self.engine.take() else {
            return;
        };
        let pid = child.id().to_string();
        let _ = Command::new("kill")
            .args([signal, &pid])
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
        self.engine = Some(child);
    }

    pub fn without_config(self) -> Self {
        let _ = std::fs::remove_file(self.root.join("home/.config/xwx/config.toml"));
        self
    }

    pub fn with_env(mut self, key: &str, value: impl AsRef<OsStr>) -> Self {
        self.extra_env
            .push((key.to_string(), value.as_ref().to_os_string()));
        self
    }

    /// 启用英文词库 + 静态二元表 + M2 零输入预测面板。
    pub fn with_english_prediction(self) -> Self {
        self.with_env("XWX_ENGLISH", "1")
            .with_env("XWX_ENGLISH_DICT_PATH", english_fixture("words.bin"))
            .with_env("XWX_ENGLISH_BIGRAM_PATH", english_fixture("bigram.bin"))
            .with_env("XWX_ENGLISH_PREDICT", "1")
    }

    pub fn start_engine(&mut self, fix: &Fixture) {
        self.start_engine_with(fix.dict_bin(), Some(fix.abbrev_bin()), NAME_TIMEOUT)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    pub fn start_engine_without_abbrev(&mut self, fix: &Fixture) {
        let absent = self.root.join("absent-abbrev.bin");
        self.start_engine_with(fix.dict_bin(), Some(absent.as_path()), NAME_TIMEOUT)
            .unwrap_or_else(|e| panic!("{e}"))
    }

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
            .env("XWX_NO_IBUS_AUTOSTART", "1")
            .env("XWX_DICT_PATH", dict)
            .env("XWX_ABBREV_PATH", &abbrev_path)
            .env("XWX_LM_PATH", &lm_path)
            .env("XWX_USER_LM_PATH", &user_lm_path)
            .env("XDG_DATA_HOME", self.root.join("xdg"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("home/.config"))
            .env("XWX_ENGLISH", "0")
            .env("XWX_ENGLISH_PREDICT", "0")
            .env("XWX_ENGLISH_DICT_PATH", self.root.join("absent-english.bin"))
            .env(
                "XWX_ENGLISH_BIGRAM_PATH",
                self.root.join("absent-english-bigram.bin"),
            )
            .env(
                "XWX_ENGLISH_USER_LM_PATH",
                self.root.join("cfg/user_english.txt"),
            )
            .env("RUST_LOG", "info")
            .envs(
                self.extra_env
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_os_str())),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log2))
            .spawn()
            .map_err(|e| format!("启动 {} 失败: {e}", bin.display()))?;
        self.engine_pid = Some(child.id());
        self.engine = Some(child);
        self.ensure_monitor();
        self.wait_for_name(timeout)
    }

    fn wait_for_name(&self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.name_owner().is_some() && self.ping().is_ok() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                let log = std::fs::read_to_string(&self.engine_log).unwrap_or_default();
                return Err(format!(
                    "引擎未在 {timeout:?} 内占用总线名 {ENGINE_SERVICE}\n--- engine.log ---\n{log}"
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

    pub fn call<B>(&self, method: &str, body: B) -> Result<zbus::Message, String>
    where
        B: zvariant::DynamicType + serde::Serialize + Send + 'static,
    {
        let conn = self.control.clone();
        let name = method.to_string();
        let worker_name = name.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
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
                let state = self.engine_pid.and_then(|pid| {
                    std::fs::read_to_string(format!("/proc/{pid}/stat"))
                        .ok()
                        .and_then(|s| {
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
                    self.engine_pid,
                    state,
                    tail.join("\n")
                ))
            }
        }
    }

    pub fn set_content_type(&self, purpose: u32, hints: u32) -> Result<(), String> {
        let conn = self.control.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let value = zvariant::Value::from((purpose, hints));
            let r = conn
                .call_method(
                    Some(ENGINE_SERVICE),
                    ENGINE_PATH,
                    Some("org.freedesktop.DBus.Properties"),
                    "Set",
                    &(ENGINE_IFACE, "ContentType", value),
                )
                .map(|_| ())
                .map_err(|e| format!("Properties.Set 失败: {e}"));
            let _ = tx.send(r);
        });
        match rx.recv_timeout(CALL_TIMEOUT) {
            Ok(r) => r,
            Err(_) => Err("Properties.Set 超时".into()),
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if std::thread::panicking() {
            if let Ok(log) = std::fs::read_to_string(&self.engine_log) {
                let tail: Vec<&str> = log.lines().rev().take(40).collect();
                if !tail.is_empty() {
                    let tail: Vec<&str> = tail.into_iter().rev().collect();
                    eprintln!("--- engine.log 末尾 ---\n{}", tail.join("\n"));
                }
            }
        }
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

/// 启动严格隔离的私有 `dbus-daemon`（空 `<servicedir>`，绝不激活宿主 IBus）。
fn spawn_private_bus(root: &Path) -> (Child, String) {
    let services = root.join("services");
    let conf_path = root.join("bus.conf");
    let sock = root.join("bus.sock");
    let conf = format!(
        "<busconfig>\n  <type>session</type>\n  <listen>unix:path={}</listen>\n  <servicedir>{}</servicedir>\n  <policy context=\"default\">\n    <allow send_destination=\"*\" eavesdrop=\"true\"/>\n    <allow eavesdrop=\"true\"/>\n    <allow own=\"*\"/>\n  </policy>\n</busconfig>\n",
        sock.display(),
        services.display()
    );
    std::fs::write(&conf_path, conf).expect("写私有总线配置失败");
    let mut child = Command::new("dbus-daemon")
        .arg(format!("--config-file={}", conf_path.display()))
        .args(["--print-address", "--nofork"])
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("home/.config"))
        .env("XDG_DATA_HOME", root.join("xdg"))
        .env("XDG_CACHE_HOME", root.join("home/.cache"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env_remove("IBUS_ADDRESS")
        .env_remove("IBUS_DAEMON_PATH")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("无法执行 dbus-daemon");
    let stdout = child.stdout.take().expect("dbus-daemon stdout");
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let _ = tx.send(line);
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
