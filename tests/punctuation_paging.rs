//! 标点与翻页的**同一批键**:`.` 和 `,` 既是标点又是翻页键,靠状态机区分
//! (docs/VERIFY.md §2 第 8-10 行「键位抢占」)。这个测试逐键走完状态机,
//! 断言与 librime 的行为一致。

mod support;

use support::{Harness, keyval, last_commit, last_table, shared_fixture};
use support::scenario::{NI_ORDER, compose_ni};

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
