//! 光标中段编辑的两条窄边界（第三批第 2 项）：基础编辑本身已有覆盖
//! （`basic.rs` 的左右移动/退格/上屏语义、`apostrophe.rs` 的 `'` 交互），
//! 这里只补「中段插入之后」的那一步——候选是否按**整串**重查、上屏取整串。

use super::*;

/// 中段插入后，候选应按**整串**重查：打 `niho` 再把光标移回中间补 `a`，
/// 得到 `nihao`，候选里应出现「你好」。
#[test]
fn mid_caret_insert_refreshes_candidates_for_the_whole_string() {
    let mut s = session_with_dict();
    for c in "niho".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    assert_eq!(s.caret(), 4);
    s.handle(InputEvent::Left).expect("不应失败");
    assert_eq!(s.caret(), 3, "光标移到 o 之前");

    let out = s.handle(InputEvent::Char('a')).expect("不应失败");
    assert_eq!(s.caret(), 4, "插入后光标落在插入字符之后");
    let SessionOutput::Updated(u) = out else {
        panic!("中段插入应返回 Updated，实际 {out:?}")
    };
    assert!(
        u.candidates.iter().any(|c| c.word == "你好"),
        "插入后候选应按整串 `nihao` 重查，实际前 5: {:?}",
        u.candidates.iter().take(5).map(|c| &c.word).collect::<Vec<_>>()
    );
}

/// 中段插入后按空格：上屏取**整串**（`nihao` → 你好），而不是「光标之前的片段」。
#[test]
fn space_after_mid_caret_insert_commits_the_whole_string() {
    let mut s = session_with_dict();
    for c in "niho".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    s.handle(InputEvent::Left).expect("不应失败");
    s.handle(InputEvent::Char('a')).expect("不应失败");

    match s.handle(InputEvent::Space).expect("不应失败") {
        SessionOutput::Commit(w) => assert_eq!(
            w, "你好",
            "上屏应取整串 ni hao（而不是光标前的片段）"
        ),
        other => panic!("空格应 Commit，实际 {other:?}"),
    }
}
