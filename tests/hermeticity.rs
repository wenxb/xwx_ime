//! Hermeticity: the suite must not touch the user's real data, the system
//! component directory, the real `ibus-daemon`, or fcitx5's rime directory.
//!
//! # Why this file exists
//!
//! Requirement A is a hard constraint, and a constraint that is only asserted in
//! prose rots. `UserLM::open_or_create` **writes** `user_bigram.txt`
//! (`user_lm.rs:16-27`), so a mis-pointed `XWX_USER_LM_PATH` silently corrupts
//! the developer's real learning data — the failure is invisible until the user
//! notices their input method "forgot" or "learned" garbage.
//!
//! Two mechanisms are checked here:
//!
//! 1. **The harness's isolation actually reaches the child process.** The
//!    spawned engine is asked to learn something; the file it writes must land
//!    inside the temp dir, and the real `~/.local/share/xwx` must be unchanged.
//! 2. **The engine works with no data at all.** Pointing every path at a
//!    non-existent file must still produce a functioning engine (this is the
//!    production reality: `dict.bin` and `abbrev.bin` are gitignored). This
//!    doubles as evidence that the tests cannot be reading the developer's real
//!    dictionary by accident — if they were, the no-data engine would behave
//!    differently and the "must not contain" assertion below would be red.
//!
//! None of this can be checked from a unit test: the env vars are read by
//! `src/main.rs` inside the *child process*.

mod support;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use support::{fingerprint, keyval, last_table, real_user_paths, shared_fixture, Harness};

/// The user's real data must be untouched after a full engine run that learns.
///
/// The engine is driven through a real commit (which calls
/// `learn_committed` → `UserLM::learn`), then stopped normally (SIGINT) so the
/// `ctrl_c` branch of `main.rs:201-212` runs its "flush the tail" path. If
/// `XWX_USER_LM_PATH` were not redirected, that flush would rewrite
/// `~/.local/share/xwx/user_bigram.txt`.
///
/// Wrong-behaviour witness: removing the `XWX_USER_LM_PATH` override from
/// `Harness::start_engine_with` makes the real file appear/change → the
/// fingerprint assertion goes red. Also verified by pointing the override at the
/// repo root instead of the temp dir and watching the assertion fire.
/// 真实引擎在跑吗（用户在用实机输入法时，它会持续写自己的用户模型）。
///
/// 这条用例比的是「目录指纹前后一致」，用来证明**测试套件**没有写真实数据；
/// 但用户本人正在打字时，真实引擎每敲几个字就会写一次 `user_bigram.txt`
/// ——那不是测试泄漏，而是别人的正常写入。这种情况跳过比对并打印提示，
/// 而不是报一个永远修不好的红。
fn real_engine_is_running() -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|e| {
        std::fs::read_to_string(e.path().join("comm"))
            .map(|s| s.trim() == "xwx_ime")
            .unwrap_or(false)
    })
}

#[test]
fn real_user_data_is_untouched_by_a_full_learning_run() {
    if real_engine_is_running() {
        eprintln!(
            "跳过：检测到真实 xwx_ime 引擎在运行——它会持续写 ~/.local/share/xwx，\n\
             目录指纹比对在这种环境下不成立（要严格跑这条请先停掉实机输入法）。"
        );
        return;
    }
    // Fingerprint the real paths *before* anything is spawned.
    let paths = real_user_paths();
    let before: Vec<_> = paths.iter().map(|p| fingerprint(p)).collect();

    let h = &mut Harness::new("hermetic-userdata");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    // Commit a word → `learn_committed` writes into UserLM.
    h.type_ascii("nihao");
    let sigs = h.settle_default();
    let t = last_table(&sigs).expect("打 nihao 应出候选窗");
    assert!(
        !t.candidates.is_empty(),
        "对照条件:必须有候选,否则谈不上选中并学习"
    );
    assert!(h.key(keyval::SPACE, 0), "空格应上屏");
    let sigs = h.settle_default();
    assert_eq!(
        support::last_commit(&sigs).as_deref(),
        Some("你好"),
        "对照条件:必须真的上屏一次候选,否则 UserLM 不会有任何写入,\
         本测试就退化成「什么都没发生时什么都没变」的空断言"
    );

    // Stop the engine normally so the flush-on-exit path runs.
    h.shutdown_engine_gracefully();

    // The temp dir must contain the learning data — proof the learning really
    // happened *somewhere*, which is what makes the "real path unchanged"
    // assertion meaningful.
    let temp_user_lm = h.root.join("cfg/user_bigram.txt");
    assert!(
        temp_user_lm.is_file(),
        "学习数据必须落在临时目录里({});若不存在,说明 flush 没发生,\
         本测试就证明不了「写到了别处而不是用户目录」",
        temp_user_lm.display()
    );
    let written = std::fs::read_to_string(&temp_user_lm).expect("读取临时 user_bigram.txt");
    assert!(
        written.contains("你好"),
        "临时目录里的 user_bigram.txt 必须含刚上屏的「你好」;实际内容:\n{written}"
    );

    // And the real paths must be unchanged.
    for (p, fp) in paths.iter().zip(before.iter()) {
        assert_eq!(
            &fingerprint(p),
            fp,
            "真实用户数据路径 {} 被改动了！隔离失败——\
             测试绝不能写 ~/.local/share/xwx 或 /usr/share/ibus/component",
            p.display()
        );
    }
}

/// A second engine instance must also be hermetic, and Drop must clean up.
///
/// Two harnesses alive at once exercises per-process isolation. If `Drop` failed
/// to kill the engine or the bus, the suite would leak a `dbus-daemon` (and a
/// 100 MB engine) per test — the failure mode requirement F targets.
///
/// Wrong-behaviour witness: if the harness shared one bus across tests, the
/// second `request_name` would fail and `wait_for_name` would time out → the
/// test panics with the engine log. Making `Harness::drop` a no-op is caught by
/// `drop_really_kills_the_spawned_processes` below.
#[test]
fn two_concurrent_engines_are_independently_isolated() {
    let fix = shared_fixture();
    let h1 = &mut Harness::new("isolate-a");
    h1.start_engine(fix);
    let h2 = &mut Harness::new("isolate-b");
    h2.start_engine(fix);

    assert_ne!(
        h1.address(),
        h2.address(),
        "两个测试实例必须各用一条私有总线(共用会互相抢占引擎服务名)"
    );

    h1.focus_out().expect("FocusOut");
    h2.focus_out().expect("FocusOut");

    // Independent sessions: the same keys produce the same results in both, and
    // neither sees the other's composition.
    for h in [&mut *h1, &mut *h2] {
        h.type_ascii("ni");
        let sigs = h.settle_default();
        let t = last_table(&sigs).expect("应有候选窗");
        assert_eq!(
            t.candidates.first().map(String::as_str),
            Some("你"),
            "每个实例都必须独立召回「你」"
        );
    }

    // Committing in one must not commit in the other.
    h1.clear_backlog();
    h2.clear_backlog();
    assert!(h1.key(keyval::SPACE, 0));
    let s1 = h1.settle_default();
    assert_eq!(support::last_commit(&s1).as_deref(), Some("你"));
    assert!(
        h2.settle(Duration::from_millis(250), Duration::from_secs(2))
            .iter()
            .all(|s| s.member != "CommitText"),
        "实例 A 的上屏绝不能出现在实例 B 的总线上(会话不共享)"
    );

    assert!(h1.root.join("cfg").is_dir() && h2.root.join("cfg").is_dir());
    assert_ne!(h1.root, h2.root, "两个实例必须各用独立的临时目录");
}

/// `Drop` must really kill the engine and the bus — verified by pid.
///
/// Requirement F: "Kill all spawned processes on both success and failure (use a
/// guard struct with Drop)… Tests must not hang the suite."
///
/// The failure mode this catches is a silent process leak: the suite would still
/// pass, but every run would strand a `dbus-daemon` and a 100 MB engine.
///
/// Wrong-behaviour witness: commenting out the kills in `Harness::drop` makes
/// both `/proc/<pid>` checks fail (the pids stay alive) → red.
#[test]
fn drop_really_kills_the_spawned_processes() {
    let (bus_pid, engine_pid);
    {
        let h = &mut Harness::new("drop");
        h.start_engine(shared_fixture());
        bus_pid = h.bus_pid_for_test();
        engine_pid = h.engine_pid_for_test();
        assert!(bus_pid > 0 && engine_pid > 0, "两个 pid 都应可用");
        assert!(process_alive(bus_pid), "私有总线应在运行");
        assert!(process_alive(engine_pid), "引擎应在运行");
    }
    // `Drop` has run here. Give the kernel a moment to reap.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while (process_alive(bus_pid) || process_alive(engine_pid))
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !process_alive(bus_pid),
        "Drop 之后私有总线进程 {bus_pid} 仍存活 —— 进程泄漏"
    );
    assert!(
        !process_alive(engine_pid),
        "Drop 之后引擎进程 {engine_pid} 仍存活 —— 每次测试都会漏一个 100MB 进程"
    );
}

/// An engine with **no data files at all** must still start and serve the
/// `ProcessKeyEvent` contract.
///
/// This is the strongest available evidence that the tests cannot be
/// accidentally reading the developer's real 80 MB `dict.bin`: with every path
/// pointed at a missing file the engine must (a) come up on the bus, (b) still
/// obey the passthrough/consume contract, and (c) find no dictionary words.
/// Property (c) is the discriminator — if the harness leaked the real data dir,
/// `ni` would produce candidates.
///
/// Wrong-behaviour witness: removing the `XWX_DICT_PATH` override (letting
/// `resolve_data_path` fall back to `$XDG_DATA_HOME/xwx/dict.bin`) makes 你
/// appear in the candidate list → red. The harness *also* redirects
/// `HOME`/`XDG_DATA_HOME`; that second line of defence is what this test proves.
#[test]
fn engine_starts_and_serves_with_no_data_files_at_all() {
    let h = &mut Harness::new("no-data");
    h.start_engine_with_no_data();
    h.focus_out().expect("FocusOut");

    assert!(h.toggle_ascii(), "无数据时 Ctrl+Space 仍必须被消费");
    assert!(!h.key(keyval::ascii('n'), 0), "无数据时英文模式仍必须透传");
    assert!(h.toggle_ascii(), "切回中文");
    assert!(
        h.key(keyval::ascii('n'), 0),
        "无数据时中文模式仍必须消费字母(会话起来即可,不需要词库)"
    );

    // Reset before the echo check: that `n` is still in the composition, so
    // typing "ni" now would build the preedit `nni` and the echo candidate
    // would legitimately be `nni`. Clearing the session is what makes `ni` the
    // right expectation — without this the assertion was simply wrong.
    h.reset_session().expect("Reset 清空组合");
    h.clear_backlog();
    h.type_ascii("ni");
    let sigs = h.settle_default();
    let t = last_table(&sigs).expect("空词库也应弹回声候选窗");
    assert!(
        !t.candidates.contains(&"你".to_string()),
        "空词库下不得出现「你」——出现即说明测试读到了真实 dict.bin,隔离失效。实际 {:?}",
        t.candidates
    );
    assert_eq!(
        t.candidates,
        vec!["ni".to_string()],
        "空词库下候选只能是回声候选(用户敲的串)"
    );
}

/// No files may be created in the system component dir, the fcitx5 rime dir, or
/// the repo's own `data/` dir.
///
/// This is a "must not exist / must not change" assertion — the shape
/// docs/HANDOFF.md demands for destructive operations.
///
/// Wrong-behaviour witness: if the harness passed the real `HOME` through (or
/// invoked `xwx_ime --xml <repo>/data/...`), an entry would appear in `data/`
/// and the listing comparison would fire.
#[test]
fn no_files_are_created_in_system_or_foreign_data_dirs() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let watched = [
        // The system component dir: `just install` writes here with sudo; tests
        // must never. We only check whether an entry was added.
        PathBuf::from("/usr/share/ibus/component"),
        // The user's daily fcitx5 rime config — docs/HANDOFF.md §4 is emphatic
        // that this must never be touched.
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".local/share/fcitx5/rime"))
            .unwrap_or_else(|| PathBuf::from("/nonexistent")),
        // The repo's own data dir must not gain files from a test run.
        repo.join("data"),
    ];
    let before: Vec<(PathBuf, Vec<String>)> = watched
        .iter()
        .map(|d| (d.clone(), list_dir_sorted(d)))
        .collect();

    let h = &mut Harness::new("hermetic-dirs");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");
    h.type_ascii("nihao");
    h.settle_default();
    h.key(keyval::SPACE, 0);
    h.settle_default();

    for (dir, before_entries) in before {
        let after = list_dir_sorted(&dir);
        assert_eq!(
            after,
            before_entries,
            "目录 {} 的条目发生了变化——测试产生了越界写入。\n前: {:?}\n后: {:?}",
            dir.display(),
            before_entries,
            after
        );
    }
}

/// Sorted directory listing; a missing directory yields an empty list (so
/// "still missing" and "still empty" compare equal, which is what we want for
/// the fcitx5 path that may legitimately not exist).
fn list_dir_sorted(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<String> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// Is `pid` still alive? `/proc` on Linux; `kill -0` elsewhere.
fn process_alive(pid: u32) -> bool {
    if Path::new("/proc").is_dir() {
        return Path::new(&format!("/proc/{pid}")).exists();
    }
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Read a child's stdout line-by-line on a helper thread, exposing one line at a
/// time. Used nowhere else; kept here to document the pattern the harness uses
/// for `dbus-daemon`'s address.
#[allow(dead_code)]
fn first_line_of(mut child: std::process::Child) -> Option<String> {
    let out = child.stdout.take()?;
    BufReader::new(out).lines().next()?.ok()
}