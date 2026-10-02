//! Focus lifecycle, paging and candidate deletion — the panel-facing methods
//! and the cross-application boundary.
//!
//! # Why this file exists
//!
//! docs/VERIFY.md §2 names two of these as "most likely broken":
//!
//! - **rows 8-10 (`PageUp`/`PageDown` and the `,`/`.` state machine)** — "键位抢占。
//!   `.` 和 `,` 同时是标点和翻页键，靠状态机区分";
//! - **row 27 (cross-application)** — "跨进程。靠 IBus 的 `FocusOut` 触发，单测摸不到".
//!
//! `PageUp` / `PageDown` / `CandidateClicked` / `FocusOut` are D-Bus methods
//! (`engine.rs:273-310`); nothing in `cargo test` calls them, because the
//! session-level equivalents take a different route (`InputEvent::PageDown` from
//! a *key* versus the method call from the *panel*).
//!
//! # Assertion quality
//!
//! Every test here asserts the exact candidate list of the page it lands on, on
//! the wire. "The page changed" is not an assertion — paging *backwards* would
//! satisfy it.

mod support;

use std::time::Duration;

use support::{keyval, last_commit, last_table, shared_fixture, Harness, M_CONTROL};

/// Fixture order for `ni` (frequency desc, no language model).
const NI_ORDER: &[&str] = &[
    "你", "泥", "尼", "逆", "匿", "腻", "拟", "妮", "倪", "霓", "溺", "昵",
];

/// Type `ni` and return the first page's candidates.
fn compose_ni(h: &mut Harness) -> Vec<String> {
    h.focus_out().expect("FocusOut 复位");
    h.clear_backlog();
    h.type_ascii("ni");
    let sigs = h.settle_default();
    last_table(&sigs)
        .expect("应弹出候选窗")
        .candidates
}

/// `PageDown` moves forward one page; the table must show the *second* page.
///
/// Wrong-behaviour witnesses:
/// - `page_down` wired to `InputEvent::PageUp` (a copy-paste swap is the
///   classic bug) → the emitted page is still page 1 → red;
/// - `page` incremented without clamping, or `page_candidates()` still slicing
///   from 0 → the emitted candidates are page 1 again → red;
/// - `UpdateLookupTable` not emitted at all because the panel path forgot to
///   `refresh_ui` → `last_table` is `None` → red.
#[test]
fn page_down_advances_to_the_second_page_of_candidates() {
    let h = &mut Harness::new("page-down");
    h.start_engine(shared_fixture());
    let page1 = compose_ni(h);
    assert_eq!(page1, NI_ORDER[..9], "对照:第 1 页必须先是前 9 名");

    h.clear_backlog();
    h.page_down().expect("PageDown 调用失败成功");
    let sigs = h.settle_default();
    let page2 = last_table(&sigs).expect("PageDown 后必须发新的候选表");
    assert_eq!(
        page2.candidates,
        NI_ORDER[9..],
        "PageDown 后候选窗必须显示第 2 页(霓 溺 昵);实测 {:?}",
        page2.candidates
    );
}

/// `PageUp` goes back to the first page, and does nothing (does not wrap, does
/// not error) on the first page.
///
/// The clamping half is a real risk: `session.rs:569-573` uses
/// `saturating_sub(0)`, and an off-by-one or a `wrapping_sub` would jump to the
/// last page. A test that only checked "PageUp changes the page" would stay
/// green for that bug.
///
/// Wrong-behaviour witness: replacing `self.page.saturating_sub(1)` with
/// `self.page - 1` (or a wrapping variant) makes the first-page assertion red.
#[test]
fn page_up_returns_to_first_page_and_clamps_at_the_first_page() {
    let h = &mut Harness::new("page-up");
    h.start_engine(shared_fixture());
    compose_ni(h);

    // Down then up: back to page 1.
    h.page_down().expect("PageDown");
    h.settle_default();
    h.clear_backlog();
    h.page_up().expect("PageUp");
    let sigs = h.settle_default();
    let back = last_table(&sigs).expect("PageUp 后必须发候选表");
    assert_eq!(
        back.candidates,
        NI_ORDER[..9],
        "PageUp 必须回到第 1 页;实测 {:?}",
        back.candidates
    );

    // Already on page 1: PageUp must clamp, not wrap to the last page.
    h.clear_backlog();
    h.page_up().expect("首页再 PageUp");
    let sigs = h.settle_default();
    let still = last_table(&sigs).expect("首页 PageUp 仍应发一次候选表(状态不变)");
    assert_eq!(
        still.candidates,
        NI_ORDER[..9],
        "首页再按 PageUp 必须停在首页(不得 wrap 到末页);实测 {:?}",
        still.candidates
    );

    // And PageDown past the last page must also clamp. `ni` has 12 candidates
    // = 2 pages, so a second PageDown must stay on page 2.
    h.clear_backlog();
    h.page_down().expect("PageDown 到末页");
    h.page_down().expect("末页再 PageDown");
    let sigs = h.settle_default();
    let last = last_table(&sigs).expect("末页 PageDown 应发候选表");
    assert_eq!(
        last.candidates,
        NI_ORDER[9..],
        "末页再按 PageDown 必须停在末页(不得溢出成空页);实测 {:?}",
        last.candidates
    );
}

/// CursorUp / CursorDown move the highlight *within* the current page and the
/// wire field `cursor_in_page` must follow.
///
/// `engine.rs:109-112` subtracts the page start to convert the global cursor
/// into the panel's in-page index. Asserting only the candidate list would miss
/// a broken conversion entirely: the list is identical whichever candidate is
/// highlighted.
///
/// Wrong-behaviour witnesses:
/// - `cursor_in_page` computed from `u.cursor` without subtracting
///   `page_start()` → on page 2 the value would be 9..11 instead of 0..2 → red;
/// - `CursorDown` never sent / sent as `Up` → the value stays 0 → red.
#[test]
fn cursor_up_down_moves_the_highlight_field_within_the_page() {
    let h = &mut Harness::new("cursor");
    h.start_engine(shared_fixture());
    compose_ni(h);

    // Wait for the **specific** new table instead of trusting a short quiet gap,
    // which made this test intermittently panic with "没有候选表".
    let read_cursor = |h: &mut Harness, after: &str| -> u32 {
        let s = h
            .wait_signal("UpdateLookupTable", Duration::from_secs(3))
            .unwrap_or_else(|e| panic!("{after}: {e}"));
        last_table(std::slice::from_ref(&s))
            .unwrap_or_else(|| panic!("{after}: 候选表无法解析"))
            .cursor_in_page
    };

    // NOTE: every step clears the backlog **before** acting. `clear_backlog` also
    // drains the signal channel, so clearing afterwards would discard the very
    // table we are waiting for.
    h.clear_backlog();
    h.cursor_down().expect("CursorDown");
    let c1 = read_cursor(h, "CursorDown");
    assert_eq!(c1, 1, "CursorDown 应把高亮移到页内第 1 位");

    h.clear_backlog();
    h.cursor_down().expect("CursorDown");
    let c2 = read_cursor(h, "CursorDown x2");
    assert_eq!(c2, 2, "再 CursorDown 应到页内第 2 位");

    h.clear_backlog();
    h.cursor_up().expect("CursorUp");
    let c3 = read_cursor(h, "CursorUp");
    assert_eq!(c3, 1, "CursorUp 应回到页内第 1 位");

    // Must clamp at 0, not wrap. Two actions → read the resulting final state.
    h.clear_backlog();
    h.cursor_up().expect("CursorUp");
    h.cursor_up().expect("CursorUp");
    let _ = h.settle_default();
    let c4 = last_table(&h.backlog())
        .expect("CursorUp x2 后到顶: 没有候选表")
        .cursor_in_page;
    assert_eq!(c4, 0, "高亮必须钳在页内第 0 位(不得 wrap 到页尾)");

    // Down past the last candidate of the page must clamp too.
    h.clear_backlog();
    for _ in 0..12 {
        h.cursor_down().expect("CursorDown");
    }
    let _ = h.settle_default();
    let c5 = last_table(&h.backlog())
        .expect("CursorDown x12: 没有候选表")
        .cursor_in_page;
    assert_eq!(
        c5, 8,
        "高亮最多到本页最后一位(第 1 页有 9 条 → 下标 8);实测 {c5}"
    );
}

/// `cursor_in_page` is an **in-page** index, so on page 2 it restarts at 0.
///
/// This is the only assertion that can distinguish "global index" from "in-page
/// index". A test that paged and merely re-checked the candidate list would pass
/// with the wrong conversion.
///
/// Wrong-behaviour witness: `engine.rs:109-112` changed to pass `u.cursor`
/// directly (no `saturating_sub(u.page_start())`) makes this 9 instead of 0.
#[test]
fn cursor_field_restarts_at_zero_on_the_second_page() {
    let h = &mut Harness::new("cursor-page2");
    h.start_engine(shared_fixture());
    compose_ni(h);

    // Move the global cursor down three times (still page 1, index 3).
    for _ in 0..3 {
        h.cursor_down().expect("CursorDown");
    }
    h.settle_default();
    h.clear_backlog();
    let on_page1 = support::last_table(&h.backlog()).map(|t| t.cursor_in_page);

    // PageDown keeps the global cursor; the panel must be told the *in-page* one.
    h.page_down().expect("PageDown");
    let sigs = h.settle_default();
    let t = last_table(&sigs).expect("PageDown 后的候选表");
    assert_eq!(t.candidates, NI_ORDER[9..], "应在第 2 页");
    assert_eq!(
        t.cursor_in_page, 0,
        "第 2 页的 cursor_in_page 必须从页内 0 重新计数(全局下标 9 → 页内 0);\
         若直接送全局下标这里会是 9。第 1 页实测 {on_page1:?}"
    );
}

/// `CandidateClicked(index)` commits the candidate at that **global** index.
///
/// docs/VERIFY.md has no row for this (it is a mouse/panel action), but
/// `engine.rs:294` documents `index` as "全局下标（跨页）" — a contract nothing
/// currently checks. The failure mode is obvious and user-visible: clicking the
/// second candidate commits the wrong word, or `commit_at` indexes a page-local
/// list and panics/returns `None`.
///
/// Wrong-behaviour witnesses:
/// - `commit_at(index)` replaced by `commit_at(index + page_start())` → clicking
///   1 on page 1 commits 尼 instead of 泥 → red;
/// - `CandidateClicked` returning early (`if let Some(word)`) without a commit
///   when the index is off by one → no `CommitText` at all → red.
#[test]
fn candidate_clicked_commits_the_candidate_at_that_global_index() {
    for (index, expected) in [(0usize, "你"), (1, "泥"), (3, "逆")] {
        let h = &mut Harness::new("click");
        h.start_engine(shared_fixture());
        compose_ni(h);
        h.clear_backlog();

        h.candidate_clicked(index as u32)
            .unwrap_or_else(|e| panic!("CandidateClicked({index}) 失败: {e}"));
        let sigs = h.settle_default();
        assert_eq!(
            last_commit(&sigs).as_deref(),
            Some(expected),
            "点击全局下标 {index} 必须上屏 {expected:?}(fixture 顺序 {NI_ORDER:?});\
             实测 CommitText={:?}",
            last_commit(&sigs)
        );
    }
}

/// Clicking a candidate **on page 2** must resolve the global index, not the
/// in-page one. Index 9 is 霓 (the 10th candidate).
///
/// This is the cross-page half of the same contract and cannot be checked on
/// page 1. `commit_at` is documented as taking the global index
/// (`session.rs:791`), and `CandidateClicked` passes its argument straight
/// through.
///
/// Wrong-behaviour witness: `engine.rs:306` changed to
/// `commit_at(index + u.page_start())` double-counts the page, so clicking 9
/// would try index 18 (out of range → no commit) → red.
#[test]
fn candidate_clicked_on_page_two_uses_the_global_index() {
    let h = &mut Harness::new("click-page2");
    h.start_engine(shared_fixture());
    compose_ni(h);
    h.page_down().expect("PageDown");
    h.settle_default();
    h.clear_backlog();

    h.candidate_clicked(9).expect("CandidateClicked(9)");
    let sigs = h.settle_default();
    assert_eq!(
        last_commit(&sigs).as_deref(),
        Some("霓"),
        "全局下标 9 = 第 2 页第 1 条(霓);实测 {:?}",
        last_commit(&sigs)
    );
    assert!(
        last_table(&sigs).is_some() || support::member_names(&sigs).contains(&"HideLookupTable"),
        "上屏后必须刷新或收起候选窗。实际: {:?}",
        support::member_names(&sigs)
    );
}

/// `Ctrl+Delete` must make the highlighted candidate **disappear** from the
/// next identical query — this is the "该消失的消失了" rule from
/// docs/HANDOFF.md applied to the delete-candidate feature (VERIFY.md rows 29-30).
///
/// Asserting "the candidate list still contains 你" (or even "it is now 泥")
/// would be wrong: the definition of deletion is that the blocked word is
/// **gone**. So the assertion is a `must-not-contain` on the wire payload,
/// checked against the pre-deletion baseline taken from the same engine.
///
/// Wrong-behaviour witnesses:
/// - `delete_candidate` not wired to the blocklist → the word is still on the
///   wire → red;
/// - the blocklist keyed without the pinyin (a bare word) → deleting `ni`'s 你
///   also kills it under other readings; that is covered by the fixture having
///   one reading, so here we pin the positive half only;
/// - deleting the wrong entry (cursor vs page offset) → 你 still present → red.
#[test]
fn ctrl_delete_removes_the_candidate_from_subsequent_queries() {
    let h = &mut Harness::new("ctrl-delete");
    h.start_engine(shared_fixture());

    // Baseline: 你 is present and highlighted first.
    let before = compose_ni(h);
    assert_eq!(
        before.first().map(String::as_str),
        Some("你"),
        "对照条件:删除前「你」必须存在,否则本测试证明不了「消失了」"
    );
    assert_eq!(before, NI_ORDER[..9], "对照条件:删除前顺序");

    // Highlight the first candidate (cursor is already 0) and delete it.
    assert!(
        h.key(keyval::DELETE, M_CONTROL),
        "有候选时 Ctrl+Delete 必须被消费"
    );
    let sigs = h.settle_default();
    let after = last_table(&sigs).expect("删除后必须立刻刷新候选表");
    assert!(
        !after.candidates.contains(&"你".to_string()),
        "Ctrl+Delete 后「你」必须从候选窗消失(该消失的消失了)。\
         实测第 1 页: {:?}",
        after.candidates
    );
    assert_eq!(
        after.candidates.first().map(String::as_str),
        Some("泥"),
        "删除后第 1 名应上移为「泥」;实测 {:?}",
        after.candidates
    );

    // —— and it must stay gone after re-querying from scratch ——
    // (VERIFY.md row 30: 「重新打同一拼音 → 被删的候选不再出现」)
    let requery = compose_ni(h);
    assert!(
        !requery.contains(&"你".to_string()),
        "重新打同一拼音后「你」仍不得出现(屏蔽是持久的,不是本次会话的一次性过滤)。\
         实测: {requery:?}"
    );
    assert_eq!(
        requery.first().map(String::as_str),
        Some("泥"),
        "重新查询后的新第 1 名应是「泥」"
    );
}

/// Deleting every candidate must not panic and must converge to an empty,
/// hidden window rather than a stale one.
///
/// `session.rs:582-583` explicitly claims "删到列表空也不 panic". That claim is
/// only reachable through the panel path.
///
/// Wrong-behaviour witness: removing the `candidates.get(self.cursor)` guard, or
/// letting `cursor` stay past the end, makes `page_candidates()` return stale
/// data (a candidate list that should have disappeared) — caught by the
/// must-not-contain assertion.
#[test]
fn deleting_every_candidate_hides_the_window_without_panicking() {
    let h = &mut Harness::new("ctrl-delete-all");
    h.start_engine(shared_fixture());
    let total = compose_ni(h).len();
    assert_eq!(total, 9, "对照:第 1 页 9 条");

    // Delete the highlighted candidate 12 times (> the 12 candidates in `ni`).
    // The engine must keep answering and never panic.
    for i in 0..12 {
        assert!(
            h.key(keyval::DELETE, M_CONTROL),
            "第 {i} 次 Ctrl+Delete 仍应被消费(拼音串还在)"
        );
        h.settle(Duration::from_millis(60), Duration::from_secs(2));
    }
    // 精确等到窗口被收起,而不是「等一小段静默后去翻日志」:异步信号可能比
    // 静默期晚到(负载下会偶发变红)。全部候选删完后引擎必然发
    // `HideLookupTable`(`engine.rs:105-106`),所以这里等得到。
    // 循环里的 `settle` 可能已经把它收进 backlog,而 `wait_signal` 只看新信号
    // ——先查 backlog,没有再等。
    let mut all = h.backlog();
    if !all.iter().any(|s| s.member == "HideLookupTable") {
        let hide = h.wait_signal("HideLookupTable", Duration::from_secs(5)).unwrap_or_else(|e| {
            panic!(
                "12 条全部删完后必须收起候选窗: {e}\nbacklog 实际: {:?}",
                support::member_names(&all)
            )
        });
        all.push(hide);
    }
    // Convergence to a hidden window means the last *table-related* signal is a
    // `HideLookupTable`. Reading the most recent `UpdateLookupTable` is not the
    // same thing and was wrong: once every candidate is deleted the engine stops
    // sending tables (`engine.rs:105-106` sends `HideLookupTable` because the
    // page is empty while the preedit string is still up), so the newest table
    // in the log is a stale one from an earlier iteration.
    let last_show = all.iter().rposition(|s| s.member == "UpdateLookupTable");
    let last_hide = all.iter().rposition(|s| s.member == "HideLookupTable");
    let hidden = match (last_show, last_hide) {
        (Some(show), Some(hide)) => hide > show,
        (Some(_), None) => false,
        // Never showed a table at all → nothing stale is on screen.
        (None, _) => true,
    };
    assert!(
        hidden,
        "12 条全部删除后候选窗必须被收起(HideLookupTable 须晚于最后一次 \
         UpdateLookupTable);实际成员序列: {:?}",
        support::member_names(&all)
    );
    // The engine is still alive and answering D-Bus (no panic).
    assert!(
        !h.key(0x7f, 0),
        "删空后引擎必须仍在服务(DEL 透传),说明进程没有 panic"
    );
}

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

    // Idle: no signals at all.
    h.clear_backlog();
    h.focus_in().expect("FocusIn 必须成功返回");
    h.enable().expect("Enable 必须成功返回");
    let sigs = h.settle(Duration::from_millis(250), Duration::from_secs(2));
    assert!(
        sigs.is_empty(),
        "空闲时 FocusIn/Enable 不得发信号。实际: {:?}",
        support::member_names(&sigs)
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

/// The `,` / `.` paging state machine (docs/VERIFY.md rows 8-10) — the
/// project's own "most likely to break" list, item 1.
///
/// The rules are asymmetric on purpose (`session.rs:396-413`, from librime
/// `key_bindings.yaml`):
///
/// | key | window open      | window closed |
/// |-----|------------------|---------------|
/// | `.` | **next page**    | 。            |
/// | `,` | **prev page only if page > 0**, else ， | ， |
/// | `=` | next page        | passthrough   |
/// | `-` | prev page only if page > 0, else passthrough | passthrough |
///
/// Rows 8/9/10 are exactly this: `.` pages, `,` pages back *only after* having
/// paged, and on page 1 `,` is a comma. A test that asserted only "the candidate
/// list changed" would pass for an engine that made `,` page forward too.
///
/// Wrong-behaviour witnesses:
/// - dropping the `self.page > 0` guard on `,` → on page 1 the comma would page
///   back (clamped, so the list would be unchanged but **no commit** would be
///   emitted) → the row-10 assertion goes red;
/// - treating `.` as punctuation when the window is open → `。` is committed and
///   the candidate list does not move → the row-8 assertion goes red.
#[test]
fn comma_and_period_paging_state_machine_matches_librime() {
    let h = &mut Harness::new("paging-state-machine");
    h.start_engine(shared_fixture());
    compose_ni(h);

    // —— row 8: '.' with an open window pages to page 2, it does NOT commit 。 ——
    h.clear_backlog();
    assert!(h.key(keyval::ascii('.'), 0), "'.' 在候选窗开着时应被消费");
    let sigs = h.settle_default();
    let t = last_table(&sigs).expect("'.' 翻页必须发候选表");
    assert_eq!(
        t.candidates,
        NI_ORDER[9..],
        "候选窗开着时 '.' 必须翻到第 2 页,而不是上屏「。」;实测 {:?}",
        t.candidates
    );
    assert!(
        last_commit(&sigs).is_none(),
        "翻页不得上屏任何文本(尤其不得上屏「。」);实际 CommitText={:?}",
        last_commit(&sigs)
    );

    // —— row 9: ',' after paging goes back to page 1, and commits nothing ——
    h.clear_backlog();
    assert!(h.key(keyval::ascii(','), 0), "已翻页时 ',' 应被消费(回翻)");
    let sigs = h.settle_default();
    let t = last_table(&sigs).expect("',' 回翻必须发候选表");
    assert_eq!(
        t.candidates,
        NI_ORDER[..9],
        "已翻过页时 ',' 必须回翻到第 1 页;实测 {:?}",
        t.candidates
    );
    assert!(
        last_commit(&sigs).is_none(),
        "回翻不得上屏「，」;实际 CommitText={:?}",
        last_commit(&sigs)
    );

    // —— row 10: ',' on page 1 is a full-width comma that ALSO commits the
    //     highlighted candidate ("你好，" shape: candidate first, punctuation
    //     appended). This is the asymmetric half. ——
    h.clear_backlog();
    assert!(
        h.key(keyval::ascii(','), 0),
        "第 1 页的 ',' 仍必须被消费(上屏「，」)"
    );
    let sigs = h.settle_default();
    assert_eq!(
        last_commit(&sigs).as_deref(),
        Some("你，"),
        "第 1 页打 ',' 必须上屏「高亮候选 + ，」而不是翻页。\
         若实测为 None 说明 ',' 被当成了回翻键;实际 CommitText={:?}",
        last_commit(&sigs)
    );

    // —— row 11: '=' / '-' are the explicit paging keys ——
    compose_ni(h);
    h.clear_backlog();
    assert!(h.key(keyval::ascii('='), 0), "'=' 应被消费(下一页)");
    let sigs = h.settle_default();
    let t = last_table(&sigs).expect("'=' 翻页应发候选表");
    assert_eq!(t.candidates, NI_ORDER[9..], "'=' 应翻到第 2 页");

    h.clear_backlog();
    assert!(h.key(keyval::ascii('-'), 0), "'-' 应被消费(上一页)");
    let sigs = h.settle_default();
    let t = last_table(&sigs).expect("'-' 翻页应发候选表");
    assert_eq!(t.candidates, NI_ORDER[..9], "'-' 应回翻到第 1 页");

    // Idle: '=' and '-' have no paging meaning and no full-width mapping, so
    // they fall back to passthrough (session.rs:471-472).
    h.focus_out().expect("FocusOut");
    assert!(
        !h.key(keyval::ascii('='), 0),
        "空闲时 '=' 必须透传(无候选窗就没有翻页语义)"
    );
    assert!(
        !h.key(keyval::ascii('-'), 0),
        "空闲时 '-' 必须透传"
    );
}

/// Paging must not corrupt the *commit* path: after paging back and forth, the
/// space key still selects the candidate that the panel shows as highlighted.
///
/// This ties the two halves together — paging state and the highlight are
/// separate fields (`page` vs `cursor`), and `SessionUpdate::page_candidates`
/// slices by page while `Space` uses the global `cursor`
/// (`session.rs:486`). Nothing above checks they stay consistent.
///
/// Wrong-behaviour witness: if `page_down` reset `cursor` to 0 (or if the
/// global cursor were recomputed from `cursor_in_page`), the committed word
/// after paging back would differ from the on-screen highlight → red.
#[test]
fn highlight_and_page_stay_consistent_across_paging() {
    let h = &mut Harness::new("paging-consistency");
    h.start_engine(shared_fixture());
    compose_ni(h);

    // Move the highlight to index 2 (尼) and page away and back.
    h.cursor_down().expect("CursorDown");
    h.cursor_down().expect("CursorDown");
    h.page_down().expect("PageDown");
    h.page_up().expect("PageUp");
    // 面板此刻已回到第 1 页:这批信号里必须有那张表。旧写法先 `clear_backlog()`
    // 再去等一个**永远不会再来**的信号,于是 `if let Some(t)` 让「应回到第 1 页」
    // 这条断言一直被静默跳过——测试假装在测。
    let paged = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    h.clear_backlog();
    let t = support::last_table(&paged)
        .expect("PageUp 之后必须有候选窗信号(否则本测试等于没测)");
    assert_eq!(t.candidates, NI_ORDER[..9], "应回到第 1 页");
    let panel_cursor = Some(t.cursor_in_page);

    assert!(h.key(keyval::SPACE, 0));
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    let committed = last_commit(&sigs);
    assert_eq!(
        committed.as_deref(),
        Some("尼"),
        "空格上屏的必须是面板高亮的那条(全局下标 2 = 尼)。\
         若 page/cursor 在翻页时被改乱,这里会上屏别的词。面板 cursor_in_page={panel_cursor:?};\
         实际 CommitText={committed:?}"
    );
}