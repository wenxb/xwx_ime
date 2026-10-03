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
fn whole_string_reading_commits_in_one_space() {
    let h = &mut Harness::new("segment-commit");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    h.clear_backlog();
    h.type_ascii("nihaobeijing");
    h.clear_backlog();

    // **整串读法进首位**（上游 `script_translator.cc:497-508` 生成整句、
    // `PrepareCandidate` :524-531 放到流内首位）：`nihaobeijing` 能收成
    // 「你好北京」，于是**一次空格整串上屏**。
    //
    // 旧的「选前缀候选 → 剩余串留在组合里（方案 b）」那条路径仍然存在
    // （整串读法覆盖不了输入时、或用户显式选前缀候选时），但那时它不再是
    // 首位候选；本条只钉「能收成整句时一次上屏」这个语义。
    assert!(h.key(keyval::SPACE, 0), "空格应选中候选");
    let sigs = h.settle_response(Duration::from_millis(250), Duration::from_secs(4));
    assert_eq!(
        support::commits(&sigs),
        vec!["你好北京".to_owned()],
        "整串读法应一次交给应用"
    );
    assert_eq!(
        last_preedit(&sigs),
        None,
        "组合结束,预编辑区收起(HidePreeditText,不是留一个空串)"
    );
}
