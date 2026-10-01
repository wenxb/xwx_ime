//! The `ProcessKeyEvent` return-value contract, across the real D-Bus boundary.
//!
//! # Why this file exists
//!
//! `ProcessKeyEvent` returns `bool`: `true` = the input method consumed the
//! key, `false` = the daemon must hand the **raw** key to the application.
//! `crates/xwx_ibus/src/engine.rs:229-238` implements English-mode passthrough
//! with a bare `return false`, and the session layer returns
//! `SessionOutput::Noop` for a whole family of keys. Neither is reachable from
//! `cargo test`'s unit tests: they call `InputSession::handle` directly and
//! never see the boolean, which only exists at the IBus adapter
//! (`engine.rs:45-67` maps `Noop → false`, `Commit`/`Updated → true`).
//!
//! docs/HANDOFF.md §6 calls this out by name:
//!
//! > ⚠ 有一条路径单测覆盖不到，欠一次集成验证
//! > **英文模式下按键在 ibus 层直接透传、不进翻译层**
//!
//! # Assertion quality (docs/HANDOFF.md 「断言要钉…」 / 「证明它能失败」)
//!
//! Every assertion below checks **both directions**. A test that only asserts
//! "letters are consumed in Chinese mode" stays green when the boolean is
//! *inverted*, because an inverted engine still consumes something. The only
//! way to catch an inversion is to pin the passthrough case and the consume
//! case with the same keyval, adjacent, in one test:
//!
//! ```text
//!           correct engine        inverted engine (break: `return !consumed`)
//! 'n' zh    true                  false   → assert_eq!(.., true)  RED
//! 'n' en    false                 true    → assert_eq!(.., false) RED
//! ```
//!
//! Each test names the wrong behaviour it would catch in a comment.

mod support;

use support::keyval;
use support::{shared_fixture, Harness, M_CONTROL, M_LOCK, M_RELEASE, M_SHIFT};

/// The narrow definition of the contract, isolated: one keyval, two modes.
///
/// `'n'` is the cleanest probe. In Chinese mode with an empty pinyin string it
/// starts a composition and **must** be consumed. In English mode the engine
/// must bail out *before* `translate_keyval` (`engine.rs:235-238`), so the
/// application receives the original keyval+state untouched.
///
/// Wrong-behaviour witnesses:
/// - if English-mode passthrough is deleted (the `in_english` early return),
///   `'n'` is consumed → the `false` assertion goes red;
/// - if the boolean is inverted at the boundary, the two assertions cannot both
///   hold — that is the point of asserting both.
#[test]
fn english_mode_letter_passthrough_vs_chinese_consume() {
    let h = &mut Harness::new("english-passthrough");
    h.start_engine(shared_fixture());

    // —— Chinese mode: consuming is mandatory ——
    let consumed = h.key(keyval::ascii('n'), 0);
    assert!(
        consumed,
        "中文模式下 'n' 必须被消费(true):它开始了一次拼音组合。\
         若英文模式透传分支被误扩大到中文模式,或返回值被取反,这里会红。实际 {consumed}"
    );
    h.focus_out().expect("FocusOut");

    // —— English mode: passthrough is mandatory ——
    assert!(
        h.toggle_ascii(),
        "Ctrl+Space 必须被引擎消费(true),否则应用会收到 Ctrl+空格"
    );
    let consumed = h.key(keyval::ascii('n'), 0);
    assert!(
        !consumed,
        "英文模式下 'n' 必须透传(false):应用要拿到原始按键。\
         若 engine.rs:235-238 的 `if in_english {{ return false }}` 被删掉,\
         按键会进翻译层并被消费,这里会红。实际 {consumed}"
    );

    // The passthrough must hold for the whole alphabet, not just 'n'.
    // Breakage witness: re-enabling translation in English mode makes every one
    // of these `true`.
    for c in "nihao".chars() {
        assert!(
            !h.key(keyval::ascii(c), 0),
            "英文模式下 '{c}' 也必须透传;实测被消费说明有字母漏进了翻译层"
        );
    }

    // Shift must not change that: the app needs the original keyval + state.
    assert!(
        !h.key(keyval::ascii('g'), M_SHIFT),
        "英文模式下 Shift+G 必须透传:应用自己决定大小写(过了翻译层就只剩 Verbatim('G'))"
    );
    assert!(
        !h.key(keyval::ascii('g'), M_LOCK),
        "英文模式下 CapsLock+g 必须透传"
    );

    // Punctuation in English mode must also reach the app as half-width.
    assert!(
        !h.key(keyval::ascii(','), 0),
        "英文模式下 ',' 必须透传(fullwidth_punct 映射绝不能生效)"
    );

    // —— Back to Chinese: the very same keyval flips back to consumed ——
    assert!(h.toggle_ascii(), "切回中文的 Ctrl+Space 也要消费");
    assert!(
        h.key(keyval::ascii('n'), 0),
        "切回中文后 'n' 必须重新被消费;实测仍透传说明模式没切回来"
    );
}

/// English mode must **not** leave anything in the preedit.
///
/// This is the "pin what must DISAPPEAR" half of the lesson in
/// docs/HANDOFF.md: the interesting property is not that the app got the key
/// (covered above) but that the input method produced **no** preedit, no
/// candidate window and no commit. An implementation that returned `false`
/// *and* still fed the letter into the session would pass the boolean test and
/// fail this one.
///
/// Breakage witness: moving the `in_english` check *after*
/// `translate_keyval` + `handle_key` (i.e. returning `false` only for the
/// reply) would emit `UpdatePreeditText("n")` and `UpdateLookupTable`, and both
/// assertions below go red.
#[test]
fn english_mode_emits_no_preedit_and_no_lookup_table() {
    let h = &mut Harness::new("english-no-ui");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    assert!(h.toggle_ascii(), "进入英文模式");
    h.clear_backlog();

    for c in "nihao".chars() {
        assert!(!h.key(keyval::ascii(c), 0), "英文模式必须逐键透传");
    }
    let sigs = h.settle_default();

    let bad_preedit: Vec<String> = sigs
        .iter()
        .filter(|s| s.member == "UpdatePreeditText")
        .filter_map(|s| s.as_text())
        .collect();
    assert!(
        bad_preedit.is_empty(),
        "英文模式下不得发出任何 UpdatePreeditText——打的字必须完全不进输入法。\
         实际收到 {bad_preedit:?}"
    );
    assert!(
        !sigs.iter().any(|s| s.member == "UpdateLookupTable"),
        "英文模式下不得弹出候选窗(不得发 UpdateLookupTable);实际成员: {:?}",
        support::member_names(&sigs)
    );
    assert!(
        !sigs.iter().any(|s| s.member == "CommitText"),
        "英文模式下不得上屏任何东西(应用自己会处理原按键)"
    );
}

/// The idle punctuation contract, both directions, on one engine instance.
///
/// docs/VERIFY.md row 7: `+ # % @` must pass through as half-width when idle
/// (they have no full-width mapping). `translate_keyval`
/// (`ibus_proto.rs:167`) turns them into `InputEvent::Char(c)`, and the session
/// falls through to `Char(_) => Noop` (`session.rs:472`) — which the adapter
/// maps to `false`.
///
/// The complementary direction: `,` **is** in the punctuation table
/// (`punctuator.rs:44`) and idle commits the full-width `，`, so it must be
/// consumed. Asserting only the passthrough half would stay green for an engine
/// that consumed everything; asserting only the consume half would stay green
/// for an engine that passed everything through.
///
/// Breakage witness: `session.rs:472` changed from `Noop` to a commit, or
/// `fullwidth_punct('+')` given an entry, flips `+` to `true`.
#[test]
fn idle_symbols_pass_through_but_fullwidth_punctuation_is_consumed() {
    let h = &mut Harness::new("idle-passthrough");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    // Must pass through: no full-width mapping, no paging meaning, no Select.
    for (c, why) in [
        ('+', "没有全角映射"),
        ('#', "没有全角映射"),
        ('%', "没有全角映射"),
        ('@', "没有全角映射，且空闲时不能进 email 识别"),
        ('|', "没有全角映射"),
        ('&', "没有全角映射"),
        ('*', "没有全角映射"),
        ('-', "空闲时无候选窗，'-' 不能翻页"),
        ('=', "空闲时无候选窗，'=' 不能翻页"),
        ('\'', "空闲时 '\'' 是预留分隔符，透传"),
    ] {
        assert!(
            !h.key(keyval::ascii(c), 0),
            "空闲时 {c:?} 必须透传(false):{why}。若会话层把它吃掉,应用就收不到原字符。"
        );
    }

    // Must be consumed: full-width punctuation (docs/VERIFY.md rows 4-6).
    // Breakage witness: dropping the mapping, or returning Noop for the idle
    // punctuation arm, makes these `false`.
    for (c, fw) in [
        (',', "，"),
        ('.', "。"),
        (';', "；"),
        (':', "："),
        ('?', "？"),
        ('!', "！"),
    ] {
        h.clear_backlog();
        assert!(
            h.key(keyval::ascii(c), 0),
            "空闲时 {c:?} 必须被消费(true):它要上屏全角 {fw:?}"
        );
        // Non-vacuous: prove the consumption produced the full-width character,
        // not merely that a boolean flipped.
        let sigs = h.settle_default();
        assert_eq!(
            support::last_commit(&sigs).as_deref(),
            Some(fw),
            "空闲时 {c:?} 应上屏全角 {fw:?};实际信号: {:?}",
            support::member_names(&sigs)
        );
    }
}

/// Arrow keys must pass through when there is no active composition, and be
/// consumed while composing.
///
/// docs/VERIFY.md row 22 depends on ←/→ editing the pinyin string; the
/// complementary guarantee (empty string → the app moves its own caret) is what
/// makes that safe. `session.rs:552-560` documents both arms explicitly.
///
/// Breakage witness: deleting the `if !self.pinyin.is_empty()` guard on the
/// `Left | Right` arm makes the idle cases `true`; inverting the guard makes the
/// composing cases `false`.
#[test]
fn arrow_keys_passthrough_when_idle_and_consumed_while_composing() {
    let h = &mut Harness::new("arrows");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    for (kv, name) in [
        (keyval::LEFT, "←"),
        (keyval::RIGHT, "→"),
        (keyval::UP, "↑"),
        (keyval::DOWN, "↓"),
    ] {
        assert!(
            !h.key(kv, 0),
            "空闲时 {name} 必须透传:应用要移动自己的光标/滚动。实际被消费。"
        );
    }

    // While composing, all four belong to the input method.
    h.type_ascii("ni");
    h.settle_default();
    for (kv, name) in [
        (keyval::LEFT, "←"),
        (keyval::RIGHT, "→"),
        (keyval::UP, "↑"),
        (keyval::DOWN, "↓"),
    ] {
        assert!(
            h.key(kv, 0),
            "组合中 {name} 必须被消费:←/→ 移动拼音串内光标、↑/↓ 移动候选高亮。\
             实际透传会让应用的光标跟着动。"
        );
    }
    // Back to empty, back to passthrough — both directions on one engine.
    h.focus_out().expect("FocusOut");
    assert!(
        !h.key(keyval::LEFT, 0),
        "清空后 ← 必须重新透传;仍被消费说明会话没有真正复位"
    );
}

/// Modifier-bearing keys and key releases are always passed through
/// (`engine.rs:218-228`), and the two deliberately intercepted chords are not.
///
/// This is the "key stealing" risk from docs/VERIFY.md §2 (rows 8-10, 16): the
/// engine must only take keys it actually implements.
///
/// Breakage witness: moving the Ctrl+Delete interception *below* the modifier
/// filter makes `ctrl+delete` return `false` (it would never be reached);
/// widening the Ctrl+Space predicate to `state & CONTROL != 0` without the
/// other-modifier exclusion makes `Ctrl+Shift+Space` `true`.
#[test]
fn modifier_chords_and_key_releases_pass_through_except_the_two_we_intercept() {
    let h = &mut Harness::new("modifiers");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    // Generic modifier chords are application shortcuts: must never be eaten.
    for (kv, state, what) in [
        (keyval::ascii('c'), M_CONTROL, "Ctrl+C"),
        (keyval::ascii('v'), M_CONTROL, "Ctrl+V"),
        (keyval::ascii('n'), M_CONTROL, "Ctrl+N"),
        (keyval::ascii('n'), 1 << 3, "Alt+n"),
        (keyval::ascii('n'), M_CONTROL | M_SHIFT, "Ctrl+Shift+N"),
        (keyval::ascii('n'), 1 << 26, "Super+n"),
        (keyval::SPACE, M_CONTROL | M_SHIFT, "Ctrl+Shift+Space"),
    ] {
        assert!(
            !h.key(kv, state),
            "{what} 必须透传(false):它是应用快捷键,输入法不得抢占"
        );
    }

    // Key *release* events are never ours (engine.rs:181).
    for kv in [keyval::ascii('n'), keyval::SPACE, keyval::ascii(',')] {
        assert!(
            !h.key(kv, M_RELEASE),
            "keyval {kv:#x} 的释放事件必须透传;吃掉释放事件会让应用收到悬空的按下"
        );
    }
    assert!(
        !h.key(keyval::SPACE, M_RELEASE | M_CONTROL),
        "Ctrl+Space 的释放事件也必须透传(engine.rs:181 显式排除)"
    );

    // The two chords we DO intercept.
    assert!(
        h.key(keyval::SPACE, M_CONTROL),
        "Ctrl+Space 必须被消费:它是中英文切换(engine.rs:191-194)"
    );
    h.focus_out().expect("FocusOut");
    assert!(
        !h.key(keyval::DELETE, M_CONTROL | M_SHIFT),
        "Ctrl+Shift+Delete 必须透传:只认纯 Ctrl+Delete(engine.rs:199-212)"
    );
}

/// Ctrl+Delete is consumed **only** when there is a candidate to delete.
///
/// `engine.rs:202-215` deliberately delegates that decision to the session:
/// "会话层在无候选时对它返回 Noop,这里随之返回 false 透传给应用
/// (应用的「删词」快捷键不被吃掉)". That two-way dependence is exactly what a
/// unit test cannot see — the session is correct either way, the *boolean* is
/// what changes.
///
/// Breakage witness: making `is_ctrl_delete` return `true` unconditionally
/// turns the idle assertion red; removing the `DeleteCandidate` arm entirely
/// turns the composing assertion red.
#[test]
fn ctrl_delete_is_consumed_only_with_a_live_composition() {
    let h = &mut Harness::new("ctrl-delete-gate");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    assert!(
        !h.key(keyval::DELETE, M_CONTROL),
        "空闲时 Ctrl+Delete 必须透传:应用要用它删词。\
         被吃掉说明引擎无条件拦截了该组合键。"
    );

    // With a composition (candidates exist) it belongs to us. 'n' is enough to
    // open the candidate window thanks to prefix fallback (session.rs:411-412).
    h.type_ascii("ni");
    h.settle_default();
    assert!(
        h.key(keyval::DELETE, M_CONTROL),
        "有候选时 Ctrl+Delete 必须被消费(删当前高亮候选)"
    );

    // After deletion the list may shrink but the composition is still alive.
    assert!(
        h.key(keyval::DELETE, M_CONTROL),
        "删掉一条后仍有多条候选,Ctrl+Delete 应继续被消费"
    );

    // Empty composition → back to passthrough, on the same engine instance.
    h.focus_out().expect("FocusOut");
    assert!(
        !h.key(keyval::DELETE, M_CONTROL),
        "清空后 Ctrl+Delete 必须重新透传"
    );
}

/// Keys that belong to the app while idle: Enter, Escape, Backspace, Space and
/// digits.
///
/// The counterpart to the composing assertions in
/// `return_value_contract_smoke.rs::space_enter_escape_backspace_consumed_while_composing`:
/// same keyvals, opposite expected booleans, different session state. Neither
/// test alone can detect an inverted boolean.
///
/// Breakage witness: removing the `is_empty()` guards on the idle arms of
/// `session.rs` (`Space => Noop`, `Enter => Noop`, `Escape => Noop`,
/// `Backspace => Noop`, `Select(_) => Noop`) makes these `true` — and the user's
/// spacebar would stop producing spaces.
#[test]
fn idle_navigation_and_editing_keys_pass_through() {
    let h = &mut Harness::new("idle-keys");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    assert!(
        !h.key(keyval::SPACE, 0),
        "空闲时空格必须透传:否则用户打不出空格"
    );
    assert!(
        !h.key(keyval::RETURN, 0),
        "空闲时回车必须透传(session.rs:504)"
    );
    assert!(
        !h.key(keyval::ESCAPE, 0),
        "空闲时 Esc 必须透传"
    );
    assert!(
        !h.key(keyval::BACKSPACE, 0),
        "空闲时退格必须透传:否则用户删不掉自己文本框里的字"
    );
    for d in '1'..='9' {
        assert!(
            !h.key(keyval::ascii(d), 0),
            "空闲时数字 {d} 必须透传:它不是选词(没有候选)"
        );
    }
    assert!(
        !h.key(keyval::ascii('0'), 0),
        "空闲时数字 0 必须透传"
    );

    // Unmapped keysyms (DEL, Tab) are not translated at all → passthrough.
    assert!(!h.key(0x7f, 0), "DEL 不在 translate_keyval 表内,必须透传");
    assert!(!h.key(0x09, 0), "Tab 不在 translate_keyval 表内,必须透传");
    assert!(
        !h.key(0xffe9, 0),
        "Shift 等纯修饰键不处理(ibus_proto.rs 单测已钉同一事实),必须透传"
    );
}