//! 修饰键与标点:Shift/CapsLock 的字母必须**原样**进串或上屏,
//! 绝不能污染拼音串;`ni,` 这类「上屏候选 + 标点」要一次提交。
//!
//! 这些路径只在 D-Bus 层存在(`ProcessKeyEvent` 的返回布尔值 + IBus 修饰掩码),
//! 单测走的是 `InputEvent`,结构上摸不到。

mod support;

use support::{Harness, keyval, last_commit, last_preedit, shared_fixture};

/// Shift+letter verbatim input must survive the full D-Bus path — with
/// docs/VERIFY.md row 13's exact shape: `nihao` + Shift+G → 「你好G」.
///
/// `translate_keyval` (`ibus_proto.rs:147-159`) converts Shift+letter into
/// `InputEvent::Verbatim('G')`, and `session.rs:439-452` commits the highlighted
/// candidate first and appends the letter. The *whole point* is that a single
/// `CommitText` carries both, which is only visible on the wire.
///
/// Wrong-behaviour witnesses:
/// - Shift+letter normalised into the pinyin string instead of `Verbatim` →
///   the composed word is `nihaog` and the commit is a different word → red;
/// - `Verbatim` committed as two separate signals → the concatenation is still
///   right, so this test also asserts the *number* of commits (exactly one) to
///   pin the "one commit carrying candidate+letter" contract.
#[test]
fn shift_letter_appends_verbatim_to_the_committed_candidate() {
    let h = &mut Harness::new("seq-verbatim");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    h.type_ascii("nihao");
    let sigs = h.settle_default();
    let t = support::last_table(&sigs).expect("nihao 应出候选窗");
    assert!(
        t.candidates.contains(&"你好".to_string()),
        "对照条件:nihao 应召回「你好」"
    );
    h.clear_backlog();

    assert!(
        h.key(keyval::ascii('g'), support::M_SHIFT),
        "Shift+G 必须被消费(Verbatim 上屏)"
    );
    let sigs = h.settle_default();
    let cs = support::commits(&sigs);
    assert_eq!(
        cs,
        vec!["你好G".to_string()],
        "Shift+G 必须产生一次 CommitText「你好G」——先上屏高亮候选再追加大写字母。\
         实测 {cs:?};全部信号: {:?}",
        support::member_names(&sigs)
    );
}

/// CapsLock with no Shift must also produce a verbatim uppercase letter and
/// must NOT enter the pinyin string (docs/VERIFY.md row 14).
///
/// The must-not half: if CapsLock were ignored, `G` would go into the pinyin
/// buffer as a lowercase `g`, and the sequence `nihao` + CapsLock+`g` would
/// compose `nihaog` instead of committing 「你好G」. Our fixture has no `nihaog`
/// key, so the preedit assertion is the sharp one: the preedit must be *hidden*
/// (commit → `hide_ui`), not updated to `nihaog`.
///
/// Wrong-behaviour witness: dropping `IBUS_LOCK_MASK` from
/// `translate_keyval`'s shift/caps logic makes the preedit `nihaog` and the
/// commit a pinyin word → red.
#[test]
fn capslock_letter_is_verbatim_and_never_enters_the_pinyin_string() {
    let h = &mut Harness::new("seq-capslock");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    h.type_ascii("nihao");
    h.settle_default();
    h.clear_backlog();

    assert!(
        h.key(keyval::ascii('g'), support::M_LOCK),
        "CapsLock+g 必须被消费(Verbatim 上屏)"
    );
    let sigs = h.settle_default();
    assert_eq!(
        last_commit(&sigs).as_deref(),
        Some("你好G"),
        "CapsLock 下 g 必须是 Verbatim 大写,与候选一起上屏"
    );
    // The must-not: no preedit may ever read "nihaog".
    assert!(
        !sigs
            .iter()
            .any(|s| s.member == "UpdatePreeditText" && s.as_text().as_deref() == Some("nihaog")),
        "CapsLock 的字母绝不能进拼音串(nihaog 不得出现)"
    );
    assert!(
        support::member_names(&sigs).contains(&"HidePreeditText"),
        "上屏后必须收起预编辑区,而不是把它更新成 'nihaog'"
    );
}

/// CapsLock **plus** Shift cancels out and returns the letter to the pinyin
/// string (`ibus_proto.rs:150-158` documents the terminal/editor semantics).
///
/// This is the mirror image of the test above and the reason both are needed:
/// a naive implementation that keyed off "any modifier ⇒ verbatim" would pass
/// one and fail the other.
///
/// Wrong-behaviour witness: changing `if shift != caps` to `if shift || caps`
/// makes this green case commit `你好G` instead of composing `nihaog` → red.
#[test]
fn capslock_plus_shift_returns_the_letter_to_the_pinyin_string() {
    let h = &mut Harness::new("seq-caps-shift");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    h.type_ascii("nihao");
    h.settle_default();
    h.clear_backlog();

    assert!(
        h.key(keyval::ascii('g'), support::M_LOCK | support::M_SHIFT),
        "CapsLock+Shift+g 必须被消费(进拼音串)"
    );
    let sigs = h.settle_default();
    assert_eq!(
        last_preedit(&sigs).as_deref(),
        Some("nihaog"),
        "CapsLock+Shift+g 必须回到小写拼音串(两者各翻转一次,相互抵消)。\
         若实测为 '你好G' 或空串,说明 shift/caps 的异或判定被写成了「任一即大写」"
    );
    assert!(
        !support::member_names(&sigs).contains(&"CommitText"),
        "字母进拼音串时不得上屏"
    );
}

/// A punctuation key typed immediately after a full pinyin string must commit
/// the candidate **and** the full-width punctuation as one text —
/// docs/VERIFY.md row 6 (`nihao,` → 「你好，」).
///
/// The shape (candidate + punctuation in a single `CommitText`) is what
/// `session.rs:414-430` implements, and it is invisible to a unit test that
/// inspects `SessionOutput::Commit` strings in isolation from the wire.
///
/// Wrong-behaviour witness: committing the punctuation as a *second*
/// `CommitText` still yields the same concatenation, so the assertion counts the
/// signals (exactly one) as well — a two-commit implementation is a real bug for
/// the app (undo, IME state).
#[test]
fn pinyin_then_punctuation_commits_candidate_and_punctuation_together() {
    let h = &mut Harness::new("seq-punct-after-pinyin");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    h.type_ascii("nihao");
    h.settle_default();
    h.clear_backlog();

    assert!(h.key(keyval::ascii(','), 0), "',' 必须被消费");
    let sigs = h.settle_default();
    let cs = support::commits(&sigs);
    assert_eq!(
        cs,
        vec!["你好，".to_string()],
        "nihao, 应产生一次 CommitText「你好，」(候选 + 全角逗号)。\
         实测 {cs:?};全部信号: {:?}",
        support::member_names(&sigs)
    );
    assert_eq!(
        cs.len(),
        1,
        "候选与标点必须是同一次上屏,不得拆成两次 CommitText"
    );
}
