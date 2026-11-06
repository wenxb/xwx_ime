//! 密码框边界(`ContentType` 属性):聚焦密码框时输入法必须**完全让位**。
//!
//! # 为什么必须有这一组
//!
//! ibus-daemon 在输入框切换时向引擎发
//! `Properties.Set("org.freedesktop.IBus.Engine", "ContentType", (uu))`，
//! 实测密码框是 `(8, 6144)`、普通框是 `(0, 0)`（8 = `IBUS_INPUT_PURPOSE_PASSWORD`）。
//! 这条路径只在 D-Bus 边界上存在：`InputSession::handle` 根本不知道焦点
//! 落在哪种输入框里，单测结构上摸不到。
//!
//! # 断言方向
//!
//! 只断言「密码框里按键被透传」是不够的——一个**永远透传**的引擎也能过。
//! 所以每组都成对断言：
//!
//! ```text
//!                    正确引擎    永远透传的引擎
//! 普通框 'n'   true   false  → 红
//! 密码框 'n'   false  true   → 红
//! ```
//!
//! 每条测试的注释点名它会抓到哪种错实现。

mod support;

use std::time::Duration;

use support::{Harness, keyval, shared_fixture};

/// 密码框里**任何**按键都原样交给应用：字母、数字、标点、空格、回车。
///
/// 只测一个 `'n'` 是不够的：`translate_keyval` 会把字母翻成 `Char`，把
/// 空格翻成选词——两类按键走的是完全不同的分支，只堵住其中一条的话，
/// 密码框里敲空格仍然会选词上屏。
///
/// 错实现对照：
/// - 闸门放在 `translate_keyval` 之后 → 空格已被翻译成「选第一个候选」→ 红；
/// - 闸门只挡字母不挡标点 → `'/'` 被标点表消费成 `/` → 红。
#[test]
fn password_field_forwards_every_key() {
    let h = &mut Harness::new("content-type-password");
    h.start_engine(shared_fixture());

    // 先确认普通框里这些键**是**被消费的，否则下面的 false 可能只是引擎
    // 本来就不吃这些键，测不出任何东西。
    h.set_content_type(0, 0).expect("ContentType=(0,0)");
    assert!(
        h.key(keyval::ascii('n'), 0),
        "普通输入框里 'n' 必须被消费（否则本测试的对照不成立）"
    );
    h.focus_out().expect("FocusOut 复位");
    h.clear_backlog();

    h.set_content_type(8, 6144).expect("ContentType=(8,6144) 密码框");

    for (label, keyval) in [
        ("字母 'n'", keyval::ascii('n')),
        ("数字 '1'", keyval::ascii('1')),
        ("标点 '/'", keyval::ascii('/')),
        ("空格", keyval::SPACE),
        ("回车", keyval::RETURN),
    ] {
        assert!(
            !h.key(keyval, 0),
            "密码框里的{label}必须透传给应用，绝不能被输入法消费"
        );
    }
}

/// 密码框里连 Ctrl+空格 也不拦：用户看不见顶栏状态，切模式只会让人困惑。
///
/// 闸门放在 `ProcessKeyEvent` 最前面才能过这条；放在 Ctrl+空格 之后就会红。
#[test]
fn password_field_does_not_toggle_mode() {
    let h = &mut Harness::new("content-type-toggle");
    h.start_engine(shared_fixture());

    h.set_content_type(8, 6144).expect("ContentType=(8,6144) 密码框");
    assert!(
        !h.toggle_ascii(),
        "密码框里 Ctrl+空格 必须透传（不能悄悄切中英）"
    );

    // 回到普通框，模式应当还是中文：若上面那次切换生效了，这里 'n'
    // 会被当成英文透传，断言随之变红。
    h.set_content_type(0, 0).expect("ContentType=(0,0) 普通框");
    assert!(
        h.key(keyval::ascii('n'), 0),
        "密码框里的 Ctrl+空格 不得改变中英模式（回到普通框后 'n' 仍应组词）"
    );
}

/// 从组合态切进密码框：候选窗必须收起，未上屏的拼音必须丢掉。
///
/// 用户先打了 `ni`（候选窗开着），然后点了密码框——此时窗口若留着，
/// 密码框旁边就挂着一个显示「你 泥 尼…」的候选窗，既无用又泄露。
///
/// 错实现对照：
/// - 只在 `FocusOut` 清理、不处理 `ContentType` → 候选窗留在屏幕上 → 红；
/// - 清理时顺手把拼音提交上屏 → 密码框里凭空多出「你」→ 红。
#[test]
fn entering_password_field_hides_candidates_without_committing() {
    let h = &mut Harness::new("content-type-cleanup");
    h.start_engine(shared_fixture());

    h.set_content_type(0, 0).expect("ContentType=(0,0) 普通框");
    h.type_ascii("ni");
    let sigs = h.settle_default();
    assert!(
        support::last_table(&sigs).is_some(),
        "前置条件：普通框里打 'ni' 必须弹出候选窗"
    );
    h.clear_backlog();

    h.set_content_type(8, 6144).expect("ContentType=(8,6144) 密码框");
    // 清理要发信号，而属性 setter 拿不到 SignalContext，所以延到下一次按键。
    assert!(
        !h.key(keyval::ascii('x'), 0),
        "密码框里的按键必须透传"
    );

    let sigs = h.settle_default();
    let names = support::member_names(&sigs);
    assert!(
        names.contains(&"HideLookupTable"),
        "切进密码框必须收起候选窗。实际信号: {names:?}"
    );
    assert!(
        names.contains(&"HidePreeditText"),
        "切进密码框必须收起预编辑区。实际信号: {names:?}"
    );
    assert!(
        support::commits(&sigs).is_empty(),
        "切进密码框时绝不能把未确认的拼音提交上屏。实际上屏: {:?}",
        support::commits(&sigs)
    );

    // 组合串真的没了：回到普通框打 'ni'，预编辑必须是 'ni' 而不是 'nixni'。
    h.set_content_type(0, 0).expect("ContentType=(0,0) 普通框");
    h.clear_backlog();
    h.type_ascii("ni");
    let sigs = h.settle_default();
    assert_eq!(
        support::last_preedit(&sigs).as_deref(),
        Some("ni"),
        "密码框里那次按键不能进入组合串"
    );
}

/// 真实 daemon 的下发顺序是 `FocusOut` → `ContentType`，此时不该有第二次清理。
///
/// daemon 切换输入框时先发 `FocusOut`（会话已清空、隐藏信号也在那时发出），
/// 再写 `ContentType`。若引擎在这条路径上又补一次清理，就会往面板多发一对
/// Hide 信号——面板从没显示过的东西被要求隐藏，某些 shell 会记错误日志。
#[test]
fn daemon_order_focus_out_then_content_type_emits_no_extra_hides() {
    let h = &mut Harness::new("content-type-daemon-order");
    h.start_engine(shared_fixture());

    h.set_content_type(0, 0).expect("ContentType=(0,0) 普通框");
    h.type_ascii("ni");
    h.settle_default();
    h.clear_backlog();

    // daemon 的真实顺序：FocusOut 会清会话并收起窗口，这对信号是**正常的**。
    h.focus_out().expect("FocusOut");
    let focus_out_sigs = h.settle(Duration::from_millis(300), Duration::from_secs(2));
    assert!(
        support::member_names(&focus_out_sigs).contains(&"HideLookupTable"),
        "前置条件：FocusOut 应在这时收起候选窗"
    );
    h.clear_backlog();

    // 之后写 ContentType：不该再有任何信号（会话已经空了）。
    h.set_content_type(8, 6144).expect("ContentType=(8,6144)");
    let sigs = h.settle(Duration::from_millis(300), Duration::from_secs(2));
    h.clear_backlog();

    // 密码框里第一个按键：同样不该补发隐藏。
    assert!(!h.key(keyval::ascii('x'), 0), "密码框里按键必须透传");
    let sigs2 = h.settle(Duration::from_millis(300), Duration::from_secs(2));
    let mut names = support::member_names(&sigs);
    names.extend(support::member_names(&sigs2));
    assert!(
        !names.contains(&"HideLookupTable") && !names.contains(&"HidePreeditText"),
        "FocusOut 已经清过会话，ContentType 之后不该再发隐藏信号。实际: {names:?}"
    );
    assert!(
        support::commits(&sigs).is_empty() && support::commits(&sigs2).is_empty(),
        "这条路径上不能有任何上屏"
    );
}

/// 密码框状态是**粘性**的：只要 daemon 没把它改回去，就一直让位。
///
/// 这条钉住「读一次就丢」的错实现：如果 `in_password_field` 在第一次
/// 按键后就自我清除，第二次按键就会重新进入中文引擎。
#[test]
fn password_state_persists_across_keys() {
    let h = &mut Harness::new("content-type-sticky");
    h.start_engine(shared_fixture());

    h.set_content_type(8, 6144).expect("ContentType=(8,6144) 密码框");

    for i in 0..3 {
        assert!(
            !h.key(keyval::ascii('a'), 0),
            "第 {i} 次按键仍必须透传（密码状态不能读一次就丢）"
        );
    }
}

/// 离开密码框后立刻恢复正常输入。
///
/// 反向断言：若密码状态只进不出（`set_content_type` 只在 purpose==8 时
/// 置位、从不复位），回到普通框后打字仍然透传 → 红。
#[test]
fn leaving_password_field_restores_composition() {
    let h = &mut Harness::new("content-type-restore");
    h.start_engine(shared_fixture());

    h.set_content_type(8, 6144).expect("ContentType=(8,6144) 密码框");
    assert!(!h.key(keyval::ascii('n'), 0), "密码框里 'n' 透传");

    h.set_content_type(0, 0).expect("ContentType=(0,0) 普通框");
    assert!(
        h.key(keyval::ascii('n'), 0),
        "离开密码框后 'n' 必须重新进入组合（密码状态要能退出）"
    );
    let sigs = h.settle_default();
    assert_eq!(
        support::last_preedit(&sigs).as_deref(),
        Some("n"),
        "离开密码框后应重新出现预编辑"
    );
}
