//! 翻页与点选:面板的 `PageUp`/`PageDown`/`CursorUp`/`CursorDown`/
//! `CandidateClicked` 是 D-Bus 方法(`engine.rs:273-310`),单测摸不到——
//! 会话层的等价物走的是**按键**(`InputEvent::PageDown`)而不是面板方法。
//!
//! **线上坐标系(codex 审查 D5 定稿)**:`UpdateLookupTable` 只带**当前页**
//! (≤ `page_size` 条),`cursor` 是**页内**下标(0..page_size);面板据此渲染,
//! 点击回传的也是页内序号,会话用 `page_start + index` 映射回全局
//! (`InputEvent::CandidateClicked` 的契约)。会话内部只有**一个**权威全局
//! 高亮下标,页码由它推导——所以「面板显示的那条」与「空格/数字/点击上屏的
//! 那条」在构造上同源。
//!
//! 不采用「发整表 + 绝对 cursor」的原因见 `crates/xwx_ibus/src/engine.rs`:
//! 实测整表序列化 500 条 = 7.5ms/键,超过 12ms 预算的一半。
//!
//! 断言质量:每个测试都断言**候选表内容**(落在哪一页)**加** `cursor`
//! (页内高亮),两者必须同时自洽。

mod support;

use std::time::Duration;
use support::scenario::{NI_ORDER, compose_ni};
use support::{Harness, keyval, last_commit, last_table, shared_fixture};

/// `PageDown` moves to the next page: the table must be page 2's slice and the
/// page-local cursor must restart at 0.
///
/// Wrong-behaviour witnesses:
/// - `page_down` wired to `InputEvent::PageUp` (a copy-paste swap is the
///   classic bug) → the table is still page 1 → red;
/// - `goto_next_page` 不检查目标页首项是否越界 → 空页/越界下标 → red;
/// - `UpdateLookupTable` not emitted at all because the panel path forgot to
///   `refresh_ui` → `last_table` is `None` → red.
#[test]
fn page_down_advances_to_the_second_page_of_candidates() {
    let h = &mut Harness::new("page-down");
    h.start_engine(shared_fixture());
    let page1 = compose_ni(h);
    assert_eq!(page1, NI_ORDER[..9], "对照:第 1 页必须是前 9 名");

    h.clear_backlog();
    h.page_down().expect("PageDown 调用失败");
    let sigs = h.settle_default();
    let page2 = last_table(&sigs).expect("PageDown 后必须发新的候选表");
    assert_eq!(
        page2.candidates,
        NI_ORDER[9..],
        "PageDown 后候选窗必须显示第 2 页(霓 溺 昵);实测 {:?}",
        page2.candidates
    );
    assert_eq!(
        page2.cursor, 0,
        "翻到第 2 页后页内高亮从 0 起(会话的全局高亮 = page_start + 页内);实测 {}",
        page2.cursor
    );
}

/// `PageUp` goes back to the first page, and does nothing (does not wrap, does
/// not error) on the first page.
///
/// The clamping half is a real risk: an off-by-one or a `wrapping_sub` would
/// jump to the last page. A test that only checked "PageUp changes something"
/// would stay green for that bug.
///
/// Wrong-behaviour witness: replacing the `page > 0` guard with an unchecked
/// `page - 1` (or a wrapping variant) makes the first-page assertion red
/// (the table would be the last page's slice).
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
    assert_eq!(back.cursor, 0, "回首页后页内高亮在 0;实测 {}", back.cursor);

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

/// CursorUp / CursorDown move the highlight globally; crossing a page boundary
/// must switch the emitted page **and** restart the in-page cursor, so the panel
/// and the session never disagree about which candidate is highlighted.
///
/// 旧实现(已作废的协议):`cursor` 全局走、页码却由另一个字段独立维护,越过第 1 页
/// 后线上仍是第 1 页的表、页内下标被钳在 8——「面板显示第 1 页第 9 条」而
/// 「会话高亮第 2 页第 1 条」。旧断言 `c5 == 8` 记录的正是那个错状态。
///
/// Wrong-behaviour witnesses:
/// - 只发当前页但页码不跟着光标走 → 越过边界后表仍是第 1 页 → red;
/// - `CursorDown` never sent / sent as `Up` → the value stays 0 → red.
#[test]
fn cursor_up_down_crosses_the_page_boundary_consistently() {
    let h = &mut Harness::new("cursor");
    h.start_engine(shared_fixture());
    compose_ni(h);

    // Wait for the **specific** new table instead of trusting a short quiet gap,
    // which made this test intermittently panic with "没有候选表".
    let read_table = |h: &mut Harness, after: &str| -> support::LookupTable {
        let s = h
            .wait_signal("UpdateLookupTable", Duration::from_secs(3))
            .unwrap_or_else(|e| panic!("{after}: {e}"));
        last_table(std::slice::from_ref(&s))
            .unwrap_or_else(|| panic!("{after}: 候选表无法解析"))
    };

    // NOTE: every step clears the backlog **before** acting. `clear_backlog` also
    // drains the signal channel, so clearing afterwards would discard the very
    // table we are waiting for.
    h.clear_backlog();
    h.cursor_down().expect("CursorDown");
    assert_eq!(read_table(h, "CursorDown").cursor, 1, "下移一次到页内第 1 条");

    h.clear_backlog();
    h.cursor_down().expect("CursorDown");
    assert_eq!(read_table(h, "CursorDown x2").cursor, 2, "再下移到第 2 条");

    h.clear_backlog();
    h.cursor_up().expect("CursorUp");
    assert_eq!(read_table(h, "CursorUp").cursor, 1, "上移回第 1 条");

    // Must clamp at 0, not wrap. Two actions → read the resulting final state.
    h.clear_backlog();
    h.cursor_up().expect("CursorUp");
    h.cursor_up().expect("CursorUp");
    let _ = h.settle_default();
    let top = last_table(&h.backlog()).expect("CursorUp x2 后到顶: 没有候选表");
    assert_eq!(top.cursor, 0, "高亮必须钳在第 0 条(不得 wrap)");
    assert_eq!(top.candidates, NI_ORDER[..9], "仍停在第 1 页");

    // 下移越过第一页:表必须换成第 2 页,页内高亮从 0 重新计数。
    h.clear_backlog();
    for _ in 0..12 {
        h.cursor_down().expect("CursorDown");
    }
    let _ = h.settle_default();
    let t = last_table(&h.backlog()).expect("CursorDown x12: 没有候选表");
    let last_global = NI_ORDER.len() - 1;
    assert_eq!(
        t.candidates,
        NI_ORDER[9..],
        "光标越过第 1 页后,候选窗必须显示第 2 页;实测 {:?}",
        t.candidates
    );
    assert_eq!(
        t.cursor as usize,
        last_global - 9,
        "末条(全局 {last_global})在第 2 页里的页内下标是 {};实测 {}",
        last_global - 9,
        t.cursor
    );
}

/// 面板收到的表与 `cursor` 必须始终自洽:表 = `cursor / page_size` 那一页,
/// `cursor` = 页内偏移。发「整表 + 绝对下标」也能自洽,但实测代价是
/// 7.5ms/键(500 条),超出 12ms 预算的一半——所以这里钉的是**页内方案**
/// 的自洽性,而不是某一种实现。
///
/// Wrong-behaviour witnesses:
/// - 表发当前页、`cursor` 却送全局下标(> page_size)→ red;
/// - 表发整表、`cursor` 送页内值 → 第 1 页断言里 `candidates == NI_ORDER[..9]` 失败 → red;
/// - `round` 送 `true` → 与「不 wrap 的钳位导航」不一致 → red。
#[test]
fn lookup_table_stays_in_step_with_the_page() {
    let h = &mut Harness::new("cursor-page2");
    h.start_engine(shared_fixture());
    compose_ni(h);

    // 第 1 页:表是前 9 名,页内高亮 3
    h.clear_backlog();
    for _ in 0..3 {
        h.cursor_down().expect("CursorDown");
    }
    let moved = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    let first = support::last_table(&moved).expect("CursorDown 后必须有候选表");
    assert_eq!(first.candidates, NI_ORDER[..9], "第 1 页的表是前 9 名");
    assert_eq!(first.page_size, 9, "每页 9 条");
    assert!(!first.round, "不循环翻页:与会话的钳位导航一致");
    let on_page1 = first.cursor;
    assert_eq!(on_page1, 3, "第 1 页高亮第 3 条");

    // PageDown:表换成第 2 页,页内高亮归零(全局 9 = 页首)
    h.clear_backlog();
    h.page_down().expect("PageDown");
    let sigs = h.settle_default();
    let t = last_table(&sigs).expect("PageDown 后的候选表");
    assert_eq!(t.candidates, NI_ORDER[9..], "第 2 页的表(霓 溺 昵)");
    assert_eq!(
        t.cursor, 0,
        "第 2 页的页内高亮从 0 重新计数(会话的全局高亮 = 9);第 1 页实测 {on_page1:?}"
    );

    // 页内下标必须恒在表内
    assert!(
        (t.cursor as usize) < t.candidates.len(),
        "页内高亮 {} 必须在表({} 条)内",
        t.cursor,
        t.candidates.len()
    );
}

/// `CandidateClicked(index)` commits the candidate at that index **within the
/// current page** — page 1 makes the two coordinate systems agree, so this test
/// only pins the plain case; the cross-page case is the next test.
///
/// Wrong-behaviour witnesses:
/// - `page_start` added twice on page 1 (or `commit_at(index + page_start())`
///   with a page-local index) → 点 1 上屏「尼」以外的词 → red;
/// - `CandidateClicked` returning early without a commit when the index is off
///   by one → no `CommitText` at all → red.
#[test]
fn candidate_clicked_commits_within_the_current_page() {
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
            "第 1 页点第 {index} 条必须上屏 {expected:?}(fixture 顺序 {NI_ORDER:?});\
             实测 CommitText={:?}",
            last_commit(&sigs)
        );
    }
}

/// 第 2 页上的点击带的是**页内**下标:点第 1 条(霓)必须上屏霓,不是第 1 页的泥。
///
/// 这就是 codex 审查 D5 的集成层见证:旧实现把面板回传的页内下标直接当全局
/// 下标用,点第 2 页第 1 条会上屏「泥」——面板显示与上屏文本分叉。
///
/// Wrong-behaviour witness: 会话里 `CandidateClicked` 不做 `page_start + index`
/// 映射 → 上屏「泥」而不是「霓」 → red。
#[test]
fn candidate_clicked_on_page_two_uses_the_in_page_index() {
    let h = &mut Harness::new("click-page2");
    h.start_engine(shared_fixture());
    compose_ni(h);
    h.page_down().expect("PageDown");
    h.settle_default();
    h.clear_backlog();

    h.candidate_clicked(1).expect("CandidateClicked(1)");
    let sigs = h.settle_default();
    assert_eq!(
        last_commit(&sigs).as_deref(),
        Some(NI_ORDER[10]),
        "第 2 页第 1 条(页内下标 1)= {}(全局下标 10);旧实现会点上屏 {}。\
         实测 {:?}",
        NI_ORDER[10],
        NI_ORDER[1],
        last_commit(&sigs)
    );
    assert!(
        last_table(&sigs).is_some() || support::member_names(&sigs).contains(&"HideLookupTable"),
        "上屏后必须刷新或收起候选窗。实际: {:?}",
        support::member_names(&sigs)
    );
}

/// 翻页之后「面板高亮的那条」与「空格上屏的那条」必须是同一条。
///
/// 旧实现里页与光标是两份状态(`page` vs `cursor`),`PageDown` 只动页不动
/// 光标,空格却按全局光标取词——两者一旦不同步,「显示的是谁」与「上屏的是谁」
/// 就分叉,而且面板送出的下拉消息还会把页内下标泄漏成全局下标。
///
/// 有意变更(旧断言已作废,原文见本次提交说明):旧实现翻页保留光标,所以
/// 「下移 2 次 → PageDown → PageUp → 空格」会上屏第 3 条(尼);新契约里
/// `PageUp` 把光标移回第 1 页首项(全局 0)。这是 librime selector 的翻页
/// 口径——翻页后高亮落在目标页首项,而不是回到旧位置。
#[test]
fn highlight_and_page_stay_consistent_across_paging() {
    let h = &mut Harness::new("paging-consistency");
    h.start_engine(shared_fixture());
    compose_ni(h);

    // 下移两次再翻页:高亮应落在**第 2 页首项**(全局 9 / 页内 0)。
    h.cursor_down().expect("CursorDown");
    h.cursor_down().expect("CursorDown");
    h.page_down().expect("PageDown");
    let paged = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    h.clear_backlog();
    // 旧写法先 `clear_backlog()` 再去等一个**永远不会再来**的信号,于是
    // `if let Some(t)` 让「应回到第 1 页」这条断言一直被静默跳过——测试假装在测。
    let t = support::last_table(&paged).expect("PageDown 之后必须有候选窗信号");
    assert_eq!(t.candidates, NI_ORDER[9..], "面板此刻显示第 2 页");
    assert_eq!(t.cursor, 0, "高亮落在第 2 页首项");
    let shown = t.candidates[t.cursor as usize].clone();

    assert!(h.key(keyval::SPACE, 0));
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    let committed = last_commit(&sigs);
    assert_eq!(
        committed.as_deref(),
        Some(shown.as_str()),
        "空格上屏的必须是面板高亮的那条(页内 cursor={} 指向 {shown:?});\
         实际 CommitText={committed:?}",
        t.cursor
    );

    // 回翻:回到第 1 页首项(不 wrap),空格上屏同一坐标系下的那一条。
    // 换一个干净 harness——上半场已经上屏过「霓」,个人化(计数增益)会把它
    // 抬到第 1 名,继续用同一个用户模型的话「第 1 页是什么」就被学习改写了,
    // 翻页断言会被无关因素掩盖。
    let h = &mut Harness::new("paging-consistency-back");
    h.start_engine(shared_fixture());
    compose_ni(h);
    h.page_down().expect("PageDown");
    h.page_up().expect("PageUp");
    let back = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    h.clear_backlog();
    let t = support::last_table(&back).expect("PageUp 之后必须有候选窗信号");
    assert_eq!(t.candidates, NI_ORDER[..9], "回翻后面板显示第 1 页");
    assert_eq!(t.cursor, 0, "回翻后高亮在第 1 页首项");
    let shown = t.candidates[0].clone();
    assert!(h.key(keyval::SPACE, 0));
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(
        last_commit(&sigs).as_deref(),
        Some(shown.as_str()),
        "回翻后空格应上屏第 1 页首项 {shown:?};实际 {:?}",
        last_commit(&sigs)
    );
}
