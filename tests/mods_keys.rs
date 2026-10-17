//! 修饰键与标点:Shift/CapsLock 的字母必须**原样**进串或上屏,
//! 绝不能污染拼音串;`ni,` 这类「上屏候选 + 标点」要一次提交。
//!
//! 这些路径只在 D-Bus 层存在(`ProcessKeyEvent` 的返回布尔值 + IBus 修饰掩码),
//! 单测走的是 `InputEvent`,结构上摸不到。

mod support;

use support::{Harness, keyval, last_commit, last_preedit, shared_fixture};

/// Shift+letter must reach the session as a verbatim **uppercase** letter and
/// join the composition (docs/VERIFY.md row 13): `nihao` + Shift+G keeps the
/// composition alive as preedit `nihaoG` with **no commit**; Enter commits the
/// raw string in exactly one `CommitText`.
///
/// `translate_keyval` (`ibus_proto.rs`) converts Shift+letter into
/// `InputEvent::Verbatim('G')`. The session must neither commit on the
/// keystroke (旧实现逐字直出,还会把后续输入吞成英文) nor lowercase the
/// letter into the pinyin buffer.
///
/// Wrong-behaviour witnesses:
/// - committing on Shift+G → a `CommitText` appears before the user asked
///   for one → red;
/// - lowercasing into the pinyin string → preedit `nihaog` → red.
#[test]
fn shift_letter_enters_the_composition_instead_of_committing() {
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
        "Shift+G 必须被消费(进组合)"
    );
    let sigs = h.settle_default();
    assert!(
        support::commits(&sigs).is_empty(),
        "大写进组合不得直出: {:?}",
        support::commits(&sigs)
    );
    assert_eq!(
        last_preedit(&sigs).as_deref(),
        Some("nihaoG"),
        "大写应进预编辑组合;全部信号: {:?}",
        support::member_names(&sigs)
    );

    // 回车 = 原样逃生口:一次 CommitText「nihaoG」,不多不少。
    h.key(keyval::RETURN, 0);
    let sigs = h.settle_default();
    let cs = support::commits(&sigs);
    assert_eq!(
        cs,
        vec!["nihaoG".to_string()],
        "回车应把原样串一次上屏。实测 {cs:?};全部信号: {:?}",
        support::member_names(&sigs)
    );
}

/// CapsLock with no Shift must also reach the session as a verbatim uppercase
/// letter and join the composition (docs/VERIFY.md row 14).
///
/// The must-not half: if CapsLock were ignored (or the letter lowercased),
/// `nihao` + CapsLock+`g` would compose `nihaog` as pinyin. The preedit must
/// read `nihaoG` — the capital is preserved, the composition stays alive, and
/// only Enter commits the raw string.
///
/// Wrong-behaviour witness: dropping `IBUS_LOCK_MASK` from
/// `translate_keyval`'s shift/caps logic makes the preedit `nihaog` → red.
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
        "CapsLock+g 必须被消费(进组合)"
    );
    let sigs = h.settle_default();
    assert!(
        support::commits(&sigs).is_empty(),
        "CapsLock 大写进组合不得直出: {:?}",
        support::commits(&sigs)
    );
    // The must-not: no preedit may ever read "nihaog" (lowercased into pinyin).
    assert_eq!(
        last_preedit(&sigs).as_deref(),
        Some("nihaoG"),
        "CapsLock 下 g 必须以大写进组合,绝不能归一成小写拼音"
    );
    assert!(
        !sigs
            .iter()
            .any(|s| s.member == "UpdatePreeditText" && s.as_text().as_deref() == Some("nihaog")),
        "CapsLock 的字母绝不能归一成小写拼音(nihaog 不得出现)"
    );

    // 回车:原样串一次上屏,预编辑收起。
    h.key(keyval::RETURN, 0);
    let sigs = h.settle_default();
    assert_eq!(
        last_commit(&sigs).as_deref(),
        Some("nihaoG"),
        "回车应把原样串(含大写)一次上屏"
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
/// makes this green case commit `nihaoG` instead of composing `nihaog` → red.
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
         若实测为 'nihaoG' 或空串,说明 shift/caps 的异或判定被写成了「任一即大写」"
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

/// 大写之后照常组合:Shift+G 进预编辑,随后打的字母继续进同一个组合
/// (`Gwo`),绝不能逐字直出成英文(2026-10 用户实测:大写后打 `woshi`,
/// 应用收到原样字母、候选窗不弹)。旧版会点亮「英文跟随」,把大写之后
/// 的中文词整段吞掉——这条在完整 D-Bus 路径上钉死修复。
#[test]
fn letters_after_a_capital_keep_composing_instead_of_streaming_out() {
    let h = &mut Harness::new("seq-capital-then-pinyin");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    assert!(
        h.key(keyval::ascii('g'), support::M_SHIFT),
        "Shift+G 必须被消费(进组合)"
    );
    let sigs = h.settle_default();
    h.clear_backlog();
    assert!(
        support::commits(&sigs).is_empty(),
        "大写进组合不得直出: {:?}",
        support::commits(&sigs)
    );

    h.type_ascii("wo");
    let sigs = h.settle_default();
    assert!(
        support::commits(&sigs).is_empty(),
        "大写之后的字母必须继续组合,不得直出: {:?}",
        support::commits(&sigs)
    );
    assert_eq!(
        last_preedit(&sigs).as_deref(),
        Some("Gwo"),
        "大写与后续字母应同处一个组合"
    );
    assert!(
        support::last_table(&sigs).is_some(),
        "组合应弹出候选窗(原样上屏逃生口)"
    );
}
