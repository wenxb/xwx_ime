//! `session` 翻页（`,`/`.`/`-`/`=`、深翻页、坐标一致性）与删词（Ctrl+Delete）单测。

use super::*;

fn visible_page(s: &InputSession) -> Vec<String> {
    let start = s.page() * s.page_size;
    s.candidates
        .iter()
        .skip(start)
        .take(s.page_size)
        .map(|c| c.word.clone())
        .collect()
}

fn session_on_second_page() -> (InputSession, Vec<String>) {
    let mut s = session_with_many_candidates();
    type_str(&mut s, "ni");
    s.handle(InputEvent::PageDown).expect("翻页不应失败");
    assert_eq!(s.page(), 1);
    let page = visible_page(&s);
    assert_eq!(page.len(), 3, "12 条候选、每页 9 条 → 第 2 页 3 条: {page:?}");
    (s, page)
}

#[test]
fn period_and_comma_paging_asymmetry() {
    // 首页 `,` 不翻页，直接上屏全角逗号。
    let mut s = session_with_many_candidates();
    type_str(&mut s, "ni");
    let out = s.handle(InputEvent::Char(',')).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t.ends_with('\u{FF0C}')),
        "首页 `,` 应上屏中文逗号: {out:?}"
    );

    // 首页 `.` 翻到末页（page 1），末页再按 `.` 钳位不动，之后 `,` 回翻到 page 0。
    let mut s = session_with_many_candidates();
    type_str(&mut s, "ni");
    match s.handle(InputEvent::Char('.')).expect("不应失败") {
        SessionOutput::Updated(u) => {
            assert_eq!(u.page, 1);
            assert_eq!(u.preedit, "ni");
        }
        other => panic!("`.` 翻页应返回 Updated，实际 {other:?}"),
    }
    let out = s.handle(InputEvent::Char('.')).expect("末页再按");
    assert!(matches!(out, SessionOutput::Updated(ref u) if u.page == 1), "末页 `.` 应钳位");
    let out = s.handle(InputEvent::Char(',')).expect("回翻");
    assert!(matches!(out, SessionOutput::Updated(ref u) if u.page == 0), "`,` 应回翻到首页");
}

#[test]
fn minus_equal_page_when_menu_open() {
    let mut s = session_with_many_candidates();
    type_str(&mut s, "ni");
    assert!(matches!(
        s.handle(InputEvent::Char('=')).expect("不应失败"),
        SessionOutput::Updated(ref u) if u.page == 1
    ));
    assert!(matches!(
        s.handle(InputEvent::Char('-')).expect("不应失败"),
        SessionOutput::Updated(ref u) if u.page == 0
    ));
    assert!(matches!(
        s.handle(InputEvent::Char('-')).expect("不应失败"),
        SessionOutput::Updated(ref u) if u.page == 0
    ));

    // 空闲时 `-`/`=` 透传给应用。
    let mut s = session_with_many_candidates();
    for c in ['-', '='] {
        assert!(matches!(
            s.handle(InputEvent::Char(c)).expect("不应失败"),
            SessionOutput::Noop
        ));
    }
}

#[test]
fn ctrl_delete_blocks_dict_word_without_mutating_dict() {
    let (engine, mut s) = session_for_coin(&[("你", "ni", 200), ("逆", "ni", 50)]);
    type_str(&mut s, "ni");
    match s.handle(InputEvent::DeleteCandidate).expect("不应失败") {
        SessionOutput::Updated(u) => {
            assert!(u.candidates.iter().all(|c| c.word != "你"), "「你」应从候选消失");
            assert!(u.candidates.iter().any(|c| c.word == "逆"), "「逆」应保留");
        }
        other => panic!("DeleteCandidate 应返回 Updated，实际 {other:?}"),
    }
    let cands = engine
        .lookup_candidates("ni", &crate::LmContext::empty())
        .expect("查询失败");
    assert!(cands.iter().all(|c| c.word != "你"), "重查后「你」仍被屏蔽");

    // 底层只读词库不受影响。
    let still_in_dict = engine.dict.lookup("ni").expect("查询失败");
    assert!(
        still_in_dict.iter().any(|c| c.word == "你"),
        "词库词只屏蔽，词库本体不动"
    );
}

#[test]
fn ctrl_delete_user_coined_entry_also_removed() {
    let (engine, mut s) = session_for_coin(&[
        ("张", "zhang", 100),
        ("三", "san", 100),
        ("丰", "feng", 100),
    ]);
    let user = engine.user_lm().expect("应有用户层");
    type_str(&mut s, "zhangsanfeng");
    for w in ["张", "三", "丰"] {
        let idx = s.candidates.iter().position(|c| c.word == w).expect("应有单字");
        s.handle(InputEvent::CandidateClicked(idx)).expect("选词失败");
    }
    assert_eq!(user.entry_len(), 1);

    type_str(&mut s, "zhangsanfeng");
    assert!(s.candidates.iter().any(|c| c.word == "张三丰"), "应能查到造词");
    while s.candidates.get(s.cursor).map(|c| c.word.as_str()) != Some("张三丰") {
        s.handle(InputEvent::Down).expect("下移失败");
    }
    s.handle(InputEvent::DeleteCandidate).expect("删除失败");
    assert_eq!(user.entry_len(), 0, "删掉用户造词应连同词条移除");
    assert!(user.snapshot().is_blocked("zhang san feng", "张三丰"));
}

#[test]
fn ctrl_delete_down_to_empty_and_idle_passthrough() {
    let (_engine, mut s) = session_for_coin(&[("你", "ni", 200)]);
    // 空闲时透传给应用。
    assert!(matches!(
        s.handle(InputEvent::DeleteCandidate).expect("不应失败"),
        SessionOutput::Noop
    ));

    type_str(&mut s, "ni");
    for _ in 0..8 {
        if s.candidates.get(s.cursor).is_none() {
            break;
        }
        assert!(matches!(
            s.handle(InputEvent::DeleteCandidate).expect("删除不应失败"),
            SessionOutput::Updated(_)
        ));
    }
    // 候选已空时继续按 DeleteCandidate 不 panic 且吃掉按键。
    assert!(matches!(
        s.handle(InputEvent::DeleteCandidate).expect("不应失败"),
        SessionOutput::Updated(_)
    ));
}

#[test]
fn digit_without_a_candidate_on_this_page_is_a_noop() {
    let (mut s, _) = session_on_second_page();
    let out = s.handle(InputEvent::Select(4)).expect("不应失败");
    assert!(matches!(out, SessionOutput::Noop), "末页不存在的编号应 Noop: {out:?}");
}

#[test]
fn cursor_moves_across_the_page_boundary() {
    let mut s = session_with_many_candidates();
    type_str(&mut s, "ni");
    for _ in 0..s.page_size {
        s.handle(InputEvent::Down).expect("不应失败");
    }
    assert_eq!(s.cursor, s.page_size);
    assert_eq!(s.page(), 1);
    assert_eq!(visible_page(&s).len(), 3);

    s.handle(InputEvent::Up).expect("不应失败");
    assert_eq!(s.cursor, s.page_size - 1);
    assert_eq!(s.page(), 0);
}

#[test]
fn every_visible_candidate_is_committed_by_digit_space_and_click() {
    let page_size = session_with_many_candidates().page_size;
    let (total, page_count) = {
        let mut s = session_with_many_candidates();
        type_str(&mut s, "ni");
        (s.candidates.len(), s.candidates.len().div_ceil(page_size))
    };
    assert_eq!((total, page_count), (12, 2));

    for page in 0..page_count {
        let shown = {
            let mut s = session_with_many_candidates();
            type_str(&mut s, "ni");
            for _ in 0..page {
                s.handle(InputEvent::PageDown).expect("不应失败");
            }
            assert_eq!(s.page(), page);
            visible_page(&s)
        };
        assert!(!shown.is_empty());

        for (offset, expected) in shown.iter().enumerate() {
            let global = page * page_size + offset;

            // ① 数字（页内编号 = offset + 1）
            let mut s = session_with_many_candidates();
            type_str(&mut s, "ni");
            for _ in 0..page {
                s.handle(InputEvent::PageDown).expect("不应失败");
            }
            let out = s.handle(InputEvent::Select((offset + 1) as u8)).expect("不应失败");
            assert!(matches!(&out, SessionOutput::Commit(t) if t == expected));

            // ② 空格（高亮移动至对应条）
            let mut s = session_with_many_candidates();
            type_str(&mut s, "ni");
            for _ in 0..page {
                s.handle(InputEvent::PageDown).expect("不应失败");
            }
            for _ in 0..offset {
                s.handle(InputEvent::Down).expect("不应失败");
            }
            assert_eq!(s.cursor, global);
            let out = s.handle(InputEvent::Space).expect("不应失败");
            assert!(matches!(&out, SessionOutput::Commit(t) if t == expected));

            // ③ 点击（页内下标）
            let mut s = session_with_many_candidates();
            type_str(&mut s, "ni");
            for _ in 0..page {
                s.handle(InputEvent::PageDown).expect("不应失败");
            }
            let out = s.handle(InputEvent::CandidateClicked(offset)).expect("不应失败");
            assert!(matches!(&out, SessionOutput::Commit(t) if t == expected));
        }
    }
}

#[test]
fn cursor_move_shares_the_candidate_table_instead_of_cloning_it() {
    let mut s = session_with_many_candidates();
    type_str(&mut s, "ni");
    let SessionOutput::Updated(first) = s.handle(InputEvent::Char('.')).expect("不应失败") else {
        panic!("`.` 翻页应返回 Updated");
    };
    let SessionOutput::Updated(second) = s.handle(InputEvent::Char('.')).expect("不应失败") else {
        panic!("`.` 翻页应返回 Updated");
    };
    assert!(
        Arc::ptr_eq(&first.candidates, &second.candidates),
        "仅移动光标应共享同一份候选表 Arc"
    );

    let SessionOutput::Updated(third) = s.handle(InputEvent::Char('h')).expect("不应失败") else {
        panic!("打字母应返回 Updated");
    };
    assert!(
        !Arc::ptr_eq(&second.candidates, &third.candidates),
        "候选变化后应生成新 Arc"
    );
}

#[test]
fn deep_paging_stays_in_step_with_the_table() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    for i in 0..200u32 {
        let w = char::from_u32(0x4E00 + i).unwrap().to_string();
        engine.dict.upsert(&w, "ni", 1000 - i as u64).expect("插入失败");
    }
    let mut s = InputSession::new(Arc::new(engine));
    type_str(&mut s, "ni");

    let all: Vec<String> = s.candidates.iter().map(|c| c.word.clone()).collect();
    let page_size = s.page_size;
    assert_eq!(all.len(), 200);
    let last_page = (all.len() - 1) / page_size;
    let target = last_page.min(20);

    for page in 1..=target {
        let SessionOutput::Updated(u) = s.handle(InputEvent::PageDown).expect("不应失败") else {
            panic!("PageDown 应返回 Updated");
        };
        assert_eq!(u.page, page);
        assert_eq!(u.cursor, u.page_start(), "翻页后高亮应落在本页首项");
        assert_eq!(
            u.page_candidates().iter().map(|c| c.word.clone()).collect::<Vec<_>>(),
            all[page * page_size..((page + 1) * page_size).min(all.len())].to_vec()
        );
    }

    for _ in target..last_page {
        s.handle(InputEvent::PageDown).expect("不应失败");
    }
    let SessionOutput::Updated(u) = s.handle(InputEvent::PageDown).expect("不应失败") else {
        panic!("PageDown 应返回 Updated");
    };
    assert_eq!(u.page, last_page, "末页再翻应钳位停住");
    assert_eq!(
        u.page_candidates().iter().map(|c| c.word.clone()).collect::<Vec<_>>(),
        all[last_page * page_size..].to_vec()
    );
}
