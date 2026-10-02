//! 翻页与点选:面板的 `PageUp`/`PageDown`/`CursorUp`/`CursorDown`/
//! `CandidateClicked` 是 D-Bus 方法(`engine.rs:273-310`),单测摸不到——
//! 会话层的等价物走的是**按键**(`InputEvent::PageDown`)而不是面板方法。
//!
//! 断言质量:每个测试都断言**落在哪一页的完整候选表**。「页变了」不算断言
//! ——往回翻一页也满足它。

mod support;

use std::time::Duration;
use support::{Harness, keyval, last_commit, last_table, shared_fixture};
use support::scenario::{NI_ORDER, compose_ni};

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
