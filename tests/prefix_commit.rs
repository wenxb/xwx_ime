//! 前缀上屏:选了一个「只覆盖部分输入」的候选后,剩下的拼音必须留在
//! 组合串里继续可编辑(分段上屏),并且 preedit 要显示剩下那一段。

mod support;

use std::time::Duration;
use support::{Harness, keyval, last_preedit, shared_fixture};

/// Selecting a candidate that covers only a **prefix** of the composition must
/// commit that segment and leave the rest of the pinyin composing — with a live
/// candidate window for what is left.
///
/// Regression target: before this, `commit_word` → `reset()` cleared the whole
/// input, so typing `nihaobeijing` and picking 你好 silently **dropped**
/// `beijing`: no commit for it, no preedit, empty candidate window. The user
/// sees "选完就没下文了" and has to retype.
///
/// Wrong-behaviour witnesses (each makes this test red):
/// - the session still returns `Commit` (commit + hide UI) → there is no
///   `UpdatePreeditText("beijing")` after the commit → `last_preedit` is `None`;
/// - coverage counted as the whole string → the preedit is empty and the lookup
///   table is hidden;
/// - coverage counted as 0 (nothing consumed) → the second space commits 你好
///   again instead of 北京;
/// - the IBus layer treating `CommitSegment` like `Commit` (`commit()` hides
///   the UI) → no `UpdateLookupTable` carrying 北京 → red.
#[test]
fn selecting_a_prefix_candidate_keeps_the_remaining_pinyin_composing() {
    let h = &mut Harness::new("segment-commit");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    h.clear_backlog();
    h.type_ascii("nihaobeijing");
    h.clear_backlog();

    // 方案 b:确认段**留在组合里**(预编辑区显示为已确认文本),这一步
    // 一个字节都不交给应用——于是退格能回退、也不会出现「已上屏的段再也
    // 收不回来」。这是与「上屏一段 + CommitText」最关键的差别。
    assert!(h.key(keyval::SPACE, 0), "空格应选中候选");
    let sigs = h.settle_response(Duration::from_millis(250), Duration::from_secs(4));
    assert_eq!(
        support::commits(&sigs),
        Vec::<String>::new(),
        "确认段不立刻交给应用(方案 b)"
    );
    assert_eq!(
        last_preedit(&sigs).as_deref(),
        Some("你好beijing"),
        "预编辑区 = 已确认段 + 剩余串"
    );
    let table = support::last_table(&sigs).expect("剩余串必须有候选窗——不能什么都没有");
    assert_eq!(
        table.candidates.first().map(String::as_str),
        Some("北京"),
        "剩余串的首选应是「北京」,实际 {:?}",
        table.candidates
    );

    // **退格回退上一次确认**:剩余串原样回到输入串,重新出候选。
    h.clear_backlog();
    assert!(h.key(keyval::BACKSPACE, 0), "退格应回退上一次确认");
    let sigs = h.settle_response(Duration::from_millis(250), Duration::from_secs(4));
    assert_eq!(
        support::commits(&sigs),
        Vec::<String>::new(),
        "回退不上屏任何东西"
    );
    assert_eq!(
        last_preedit(&sigs).as_deref(),
        Some("nihaobeijing"),
        "字母一个都不能少地还回输入串"
    );

    // 重新选一遍,然后打完:整串结束时**一次**提交。
    assert!(h.key(keyval::SPACE, 0), "重新选中「你好」");
    let _ = h.settle_response(Duration::from_millis(250), Duration::from_secs(4));
    h.clear_backlog();
    assert!(h.key(keyval::SPACE, 0), "剩余串仍有候选,空格应继续选词");
    let sigs = h.settle_response(Duration::from_millis(250), Duration::from_secs(4));
    assert_eq!(
        support::commits(&sigs),
        vec!["你好北京".to_owned()],
        "组合结束时把已确认段与最后一段一次交给应用"
    );
    assert_eq!(
        last_preedit(&sigs),
        None,
        "组合结束,预编辑区收起(HidePreeditText,不是留一个空串)"
    );
}
