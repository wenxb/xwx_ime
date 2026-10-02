//! 候选屏蔽:`Ctrl+Delete` 之后该候选必须从后续查询里消失,删光也不许崩;
//! 屏蔽要连**计数**一起清(否则别的拼音路径会串味,见 lm.rs 的
//! `block_candidate`)。

mod support;

use std::time::Duration;
use support::{Harness, M_CONTROL, keyval, last_table, shared_fixture};
use support::scenario::{NI_ORDER, compose_ni};

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
