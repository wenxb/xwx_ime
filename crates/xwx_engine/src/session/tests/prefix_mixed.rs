//! 简拼/混拼的整串读法:整串能拆成词串时,**一次空格整串上屏**。
//!
//! 用户报告(2026-10-02):`womenjintqubeij` 这种「部分音节只打首字母」的串,
//! 以前只在**整串恰好等于某一个词条**时才产候选,于是第一次空格只吃掉覆盖
//! 前缀的「我们今天」,剩 `qubeij` 再按一次;更糟的是纯首字母串(`wmjtqbj`)
//! 一个候选都产不出,只剩原样回显。
//!
//! 现在简拼弧也进格子(见 `engine/lattice.rs` 的 `abbrev` 参数)+ 复用同一套
//! 带状态 beam 解码,整串读法作为一条覆盖整串的候选进同一张表 → 一次空格
//! 整串上屏。这条用例走**真实会话 API**,断言用户实际看到的东西。
//!
//! 真实词库用例:词库/副索引缺失时跳过(与 `engine/tests/abbrev.rs` 同口径)。

use std::path::Path;

use super::*;

/// 装好真实词库 + 副索引的会话。
fn real_session() -> Option<InputSession> {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return None;
    }
    let mut engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);
    assert!(engine.dict.has_abbrev(), "副索引应已装配");
    Some(InputSession::new(Arc::new(engine)))
}

#[test]
fn real_dict_mixed_whole_string_commits_in_one_space() {
    let Some(mut s) = real_session() else {
        return;
    };
    let input = "womenjintqubeij";
    for c in input.chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }

    // 结构性断言:候选表里**存在覆盖整串的简拼读法**,且读法就是「我们今天去北京」。
    //
    // 这里不断言它的名次:单测不带万象 `.gram`(只有词库),打分量纲退化成
    // 「词频为主」,同族的其它读法(`我门禁提取北京`)会与它互换位置;带上真实
    // LM 的名次由 `examples/probe` 的电池钉(实测两者都是「我们今天去北京」第一)。
    assert!(
        s.candidates.iter().any(|c| {
            c.covered == input.len()
                && c.spelling == crate::SpellingKind::Abbrev
                && c.word == "我们今天去北京"
        }),
        "应有覆盖整串的简拼读法「我们今天去北京」,实际: {:?}",
        s.candidates
            .iter()
            .take(6)
            .map(|c| (&c.word, c.covered, c.spelling))
            .collect::<Vec<_>>()
    );

    // **一次空格吃掉整串**:组合直接清空,不再需要按第二次。
    let out = s.handle(InputEvent::Space).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if !t.is_empty()),
        "一次空格应整串上屏: {out:?}"
    );
    assert_eq!(s.pinyin(), "", "一次空格后组合应为空(整串被消费)");
}

/// 纯首字母串(`wmjtqbj` = 我们今天去北京的首字母)以前一个候选都没有,
/// 只剩「原样回显」;现在必须给出覆盖整串的读法。
#[test]
fn real_dict_pure_initial_string_yields_a_whole_string_reading() {
    let Some(mut s) = real_session() else {
        return;
    };
    for c in "wmjtqbj".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    // 同上:纯首字母串必须产出覆盖整串的读法(名次交给带真实 LM 的电池)。
    assert!(
        s.candidates.iter().any(|c| {
            c.covered == 7 && c.spelling == crate::SpellingKind::Abbrev && c.word == "我们今天去北京"
        }),
        "纯首字母串应有覆盖整串的读法,实际: {:?}",
        s.candidates
            .iter()
            .take(6)
            .map(|c| (&c.word, c.covered, c.spelling))
            .collect::<Vec<_>>()
    );
}

/// 结构性保证:精确格**能**覆盖整串的输入(全拼),一次都不进简拼分支——
/// 候选里不该出现简拼层。
#[test]
fn real_dict_fully_spelled_input_gets_no_abbrev_candidates() {
    let Some(mut s) = real_session() else {
        return;
    };
    for c in "womenjintian".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    assert!(
        !s.candidates
            .iter()
            .any(|c| c.spelling == crate::SpellingKind::Abbrev),
        "全拼输入不该出现简拼层候选: {:?}",
        s.candidates
            .iter()
            .take(6)
            .map(|c| (&c.word, c.spelling))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        s.candidates.first().map(|c| c.word.as_str()),
        Some("我们今天")
    );
}
