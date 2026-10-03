//! 跨应用边界(docs/VERIFY.md §2 第 27 行「跨进程」):`FocusOut`/`Reset`/
//! `Disable` 必须清掉组合串并收起候选窗;空闲时不许乱发信号;
//! 而 `FocusIn`/`Enable` 是**无害的空操作**,不能吃掉正在打的组合。

mod support;

use std::time::Duration;
use support::{Harness, keyval, last_commit, shared_fixture};
use support::scenario::{compose_ni};

/// `FocusOut` must clear the composition **and** hide the UI.
///
/// docs/VERIFY.md row 27 calls this out as the cross-application boundary:
/// "靠 IBus 的 FocusOut 触发，单测摸不到". `reset_state` (`engine.rs:135-146`)
/// only sends the hides when the session *was* active, so an implementation that
/// always hides would be wire-different (extra signals), and one that never
/// hides leaks the preedit into the next application.
///
/// Wrong-behaviour witnesses:
/// - `was_active` computed after the reset → always `false` → no hides at all →
///   red;
/// - `FocusOut` not resetting the session → typing `hi` afterwards composes
///   `nihi` instead of `hi` → the preedit assertion goes red.
#[test]
fn focus_out_clears_the_composition_and_hides_the_ui() {
    let h = &mut Harness::new("focus-out");
    h.start_engine(shared_fixture());

    // Compose something, so the engine is active and the window is open.
    compose_ni(h);
    h.clear_backlog();

    h.focus_out().expect("FocusOut 调用");
    let sigs = h.settle_default();
    let names = support::member_names(&sigs);
    assert!(
        names.contains(&"HidePreeditText"),
        "FocusOut 必须收起预编辑区(否则拼音会留在下一个应用里)。实际: {names:?}"
    );
    assert!(
        names.contains(&"HideLookupTable"),
        "FocusOut 必须收起候选窗。实际: {names:?}"
    );

    // —— the composition is really gone, not merely hidden ——
    // Re-typing must start a fresh composition; if the pinyin survived, the next
    // preedit would read "nihi".
    h.clear_backlog();
    h.type_ascii("hi");
    let sigs = h.settle_default();
    assert_eq!(
        support::last_preedit(&sigs).as_deref(),
        Some("hi"),
        "FocusOut 后重新打字必须从空串开始;若预编辑是 'nihi' 说明串没被清掉"
    );
}

/// `FocusOut` while idle must not emit spurious hides.
///
/// The inverse of the test above, and the reason `was_active` exists: a
/// `FocusOut` with nothing composed must be *silent*. An always-hide
/// implementation passes the previous test but produces needless traffic on
/// every window switch (and, worse, tells the panel to hide a window that was
/// never shown, which some shells log as an error).
///
/// Wrong-behaviour witness: deleting the `if was_active` guard in
/// `reset_state` makes this red.
#[test]
fn focus_out_while_idle_emits_no_hide_signals() {
    let h = &mut Harness::new("focus-out-idle");
    h.start_engine(shared_fixture());
    h.focus_out().expect("先复位到空闲");
    h.clear_backlog();

    h.focus_out().expect("空闲时 FocusOut");
    let sigs = h.settle(Duration::from_millis(300), Duration::from_secs(2));
    assert!(
        sigs.is_empty(),
        "空闲时 FocusOut 不得发任何信号(was_active 为 false)。实际: {:?}",
        support::member_names(&sigs)
    );
}

/// `Reset` and `Disable` share the same teardown as `FocusOut` and must clear
/// the composition too.
///
/// `engine.rs:256-270` routes all three through `reset_state`. `Disable` is what
/// IBus calls when the user switches input method, so a composition surviving it
/// would be pasted into the next IME.
///
/// Wrong-behaviour witness: `Disable` left as a no-op (as `Enable`/`FocusIn`
/// currently are, by design) → the preedit assertion after re-typing fails.
#[test]
fn reset_and_disable_also_clear_the_composition() {
    for (name, call) in [
        ("Reset", Harness::reset_session as fn(&Harness) -> Result<(), String>),
        ("Disable", Harness::disable as fn(&Harness) -> Result<(), String>),
    ] {
        let h = &mut Harness::new("reset-disable");
        h.start_engine(shared_fixture());
        compose_ni(h);
        h.clear_backlog();

        call(h).unwrap_or_else(|e| panic!("{name} 调用失败: {e}"));
        let sigs = h.settle_default();
        let names = support::member_names(&sigs);
        assert!(
            names.contains(&"HidePreeditText") && names.contains(&"HideLookupTable"),
            "{name} 必须收起预编辑区与候选窗(engine.rs:256-270 与 FocusOut 同路径)。\
             实际: {names:?}"
        );

        h.clear_backlog();
        h.type_ascii("hi");
        let sigs = h.settle_default();
        assert_eq!(
            support::last_preedit(&sigs).as_deref(),
            Some("hi"),
            "{name} 后必须从空串重新组合;若含 'ni' 说明状态没清掉"
        );
    }
}

/// `FocusIn` and `Enable` are documented no-ops: they must answer the call
/// (so the daemon's `FocusIn` does not error) and change nothing.
///
/// This is the "don't accidentally implement" half. `engine.rs:246-248` and
/// `engine.rs:262-264` are literally empty bodies with TODOs; a test that
/// asserted only "the call succeeds" would be vacuous, so the state is checked
/// too: an idle preedit must stay absent, and an active composition must
/// survive `FocusIn` (IBus sends `FocusIn` when a window regains focus — it must
/// not throw away the user's in-progress input the way `FocusOut` does).
///
/// Wrong-behaviour witness: copying the `reset_state` body into `focus_in`
/// makes the second half red.
#[test]
fn focus_in_and_enable_are_noops_that_preserve_a_live_composition() {
    let h = &mut Harness::new("focus-in-enable");
    h.start_engine(shared_fixture());

    // 空闲时 FocusIn 什么都不发；Enable 只注册中/英属性表（唯一的例外）。
    h.clear_backlog();
    h.focus_in().expect("FocusIn 必须成功返回");
    let sigs = h.settle(Duration::from_millis(250), Duration::from_secs(2));
    assert!(
        sigs.is_empty(),
        "空闲时 FocusIn 不得发信号。实际: {:?}",
        support::member_names(&sigs)
    );

    h.clear_backlog();
    h.enable().expect("Enable 必须成功返回");
    let sigs = h.settle(Duration::from_millis(250), Duration::from_secs(2));
    let names = support::member_names(&sigs);
    assert!(
        names.len() == 1 && names[0] == "RegisterProperties",
        "空闲时 Enable 只该把中/英属性注册给面板，不该发别的信号。实际: {names:?}"
    );

    // Composing: the composition must survive FocusIn (it is not FocusOut).
    compose_ni(h);
    h.clear_backlog();
    h.focus_in().expect("组合中 FocusIn");
    let sigs = h.settle(Duration::from_millis(250), Duration::from_secs(2));
    assert!(
        !support::member_names(&sigs).contains(&"HidePreeditText"),
        "FocusIn 绝不能像 FocusOut 那样清空输入:它只是「焦点进入」通知。\
         实际: {:?}",
        support::member_names(&sigs)
    );
    // The composition is still there, so space still commits 你.
    h.clear_backlog();
    assert!(h.key(keyval::SPACE, 0), "组合仍在:空格应可选词");
    let sigs = h.settle_default();
    assert_eq!(
        last_commit(&sigs).as_deref(),
        Some("你"),
        "FocusIn 之后组合必须完好,空格仍上屏「你」"
    );
}
