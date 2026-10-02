//! 兜底分支的「前缀混拼读法」:整串覆盖不了时,仍要能确认**覆盖输入前缀**的
//! 混拼读法,剩下的串继续收尾。
//!
//! 用户报告:`womenjintqubeij` 实际只能一段段确认到「我」「门禁」,剩下的
//! `tqubeij` 收不了尾——因为混拼读法只在**整串被完整覆盖**时才产出,
//! 而「我们今天」只覆盖前 9 个字母(`wo men jin t*`)。
//!
//! 这里走**真实会话 API**(`InputEvent::Char` / `Space`),断言的是用户
//! 实际看到的东西:第一次空格吃掉最长的那一段,剩余串自己收尾。
//!
//! 真实词库用例:词库/副索引缺失时跳过(与 `engine/tests/abbrev.rs` 同口径)。

use std::path::Path;

use super::*;

#[test]
fn real_dict_prefix_mixed_reading_keeps_the_rest_resolvable() {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let mut engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);
    assert!(engine.dict.has_abbrev(), "副索引应已装配");
    let mut s = InputSession::new(Arc::new(engine));

    for c in "womenjintqubeij".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }

    // 整串(15 字母)覆盖不了,但「我们今天」覆盖前 9 个字母
    // (`wo men jin t*`,每个音节都有字母 → 合法读法)。
    let top = s.candidates.first().expect("应有候选");
    assert_eq!(
        (top.word.as_str(), top.covered),
        ("我们今天", 9),
        "第一次空格应确认**最长**的那一段,实际前 5 条: {:?}",
        s.candidates
            .iter()
            .take(5)
            .map(|c| (&c.word, c.covered, c.spelling))
            .collect::<Vec<_>>()
    );

    match s.handle(InputEvent::Space).expect("不应失败") {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "我们今天qubeij", "确认段留在组合里");
            assert!(
                !u.candidates.is_empty(),
                "剩余串必须有候选(面板不能变成空的)"
            );
        }
        other => panic!("期望 Updated(确认段留在组合里),实际 {other:?}"),
    }
    assert_eq!(s.pinyin(), "qubeij", "剩余串是 qubeij");

    // 剩余串自己就能收尾(`qu` + `bei` + `j*` → 去北京)。
    assert_eq!(
        s.candidates.first().map(|c| c.word.as_str()),
        Some("去北京"),
        "剩余串 qubeij 的第一条应是「去北京」"
    );
    let out = s.handle(InputEvent::Space).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "我们今天去北京"),
        "两次空格把两段一起上屏: {out:?}"
    );
}
