//! `session` 单测:paging_delete(从 `session/tests.rs` 拆出,2026-10-02)。

use super::*;

#[test]
fn period_pages_down_when_menu_open() {
    // 候选窗有候选、在第 1 页:`.` → 翻到第 2 页(has_menu 语义)。
    let mut s = session_with_many_candidates();
    for c in "ni".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let out = s.handle(InputEvent::Char('.')).expect("不应失败");
    match out {
        SessionOutput::Updated(u) => {
            assert_eq!(u.page, 1, "`.` 应翻到第 2 页(下标 1)");
            assert_eq!(u.preedit, "ni", "拼音串不变");
        }
        other => panic!("`.` 翻页应返回 Updated,实际 {other:?}"),
    }
}

#[test]
fn comma_pages_up_only_after_paging() {
    // `,` 用 paging 语义:仅「已翻过页」(page>0)才回翻。
    let mut s = session_with_many_candidates();
    for c in "ni".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    // 第 1 页:`,` 不翻页,出中文逗号「,」(最关键的一条:
    // 中文标点与翻页不冲突的保证)
    let out = s.handle(InputEvent::Char(',')).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t.ends_with('\u{FF0C}')),
        "第 1 页的 `,` 应上屏中文逗号,不是翻页: {out:?}"
    );

    // 重新输入并翻到第 2 页,再按 `,` → 回第 1 页
    let mut s = session_with_many_candidates();
    for c in "ni".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    s.handle(InputEvent::Char('.')).expect("`.` 应翻页"); // → page 1
    let out = s.handle(InputEvent::Char(',')).expect("不应失败");
    match out {
        SessionOutput::Updated(u) => {
            assert_eq!(u.page, 0, "第 2 页的 `,` 应回翻到第 1 页(下标 0)");
        }
        other => panic!("paging 状态下 `,` 应回翻(Updated),实际 {other:?}"),
    }
}

#[test]
fn idle_comma_period_still_fullwidth() {
    // 拼音串为空(无候选窗):`.` → 。,`,` → ,——标点优先,无翻页对象。
    let mut s = session_with_many_candidates();
    let out = s.handle(InputEvent::Char('.')).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "\u{3002}"), "空闲 `.` 应出 。");
    let out = s.handle(InputEvent::Char(',')).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "\u{FF0C}"), "空闲 `,` 应出 ,");
}

#[test]
fn minus_equal_page_when_menu_open() {
    // `-`/`=`(librime paging_with_minus_equal,has_menu):候选窗开着时翻页。
    // `-` 同 `,` 的 paging 边界:第 1 页按 `-` 回翻钳在 0(saturating),
    // 与 PageUp 在首页的行为一致(不 wrap、不上屏、不丢候选)。
    let mut s = session_with_many_candidates();
    for c in "ni".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    // `=` 翻到第 2 页
    let out = s.handle(InputEvent::Char('=')).expect("不应失败");
    assert!(matches!(out, SessionOutput::Updated(ref u) if u.page == 1), "`=` 应翻下一页");
    // `-` 回第 1 页
    let out = s.handle(InputEvent::Char('-')).expect("不应失败");
    assert!(matches!(out, SessionOutput::Updated(ref u) if u.page == 0), "`-` 应回上一页");
    // 第 1 页再按 `-`:钳在 0,不出标点、不上屏、不 panic
    let out = s.handle(InputEvent::Char('-')).expect("不应失败");
    assert!(matches!(out, SessionOutput::Updated(ref u) if u.page == 0), "首页 `-` 钳在 0");

    // 空闲(无候选窗):`-`/`=` 落回 catch-all 透传(键不被吃掉),
    // 接管自 unmapped_ascii_symbols_still_passthrough 移出的两条
    let mut s = session_with_many_candidates();
    for c in ['-', '='] {
        let out = s.handle(InputEvent::Char(c)).expect("不应失败");
        assert!(matches!(out, SessionOutput::Noop), "空闲时 {c:?} 应透传(Noop): {out:?}");
    }
}

#[test]
fn period_at_last_page_stays_and_comma_recovers() {
    // 末页再按 `.`:钳在末页(min(max_page)),不 wrap、不上屏;
    // 之后按 `,` 仍能回翻(page>0 满足 paging)。
    let mut s = session_with_many_candidates(); // 12 候选 → 2 页(0,1)
    for c in "ni".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    s.handle(InputEvent::Char('.')).expect("翻页"); // page 1(末页)
    let out = s.handle(InputEvent::Char('.')).expect("末页再按");
    assert!(matches!(out, SessionOutput::Updated(ref u) if u.page == 1), "末页 `.` 钳在末页");
    let out = s.handle(InputEvent::Char(',')).expect("回翻");
    assert!(matches!(out, SessionOutput::Updated(ref u) if u.page == 0), "`,` 应从末页回翻");
}

#[test]
fn nihao_comma_still_commits_fullwidth() {
    // 不许退化:`nihao,` 仍出「你好,」——只有 1 个候选(你好)时
    // candidates 非空、page=0,`,` 不满足 paging(page>0),走标点上屏。
    let mut s = session_with_dict();
    for c in "nihao".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let out = s.handle(InputEvent::Char(',')).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "你好\u{FF0C}"),
        "nihao, 应仍出「你好,」: {out:?}"
    );
}

#[test]
fn apostrophe_disambiguates_xian() {
    // `xi'an` → 「西安」:强制切分点让 [xi, an] 成为唯一双音节切法,
    // 西安(xi an)应出现在候选里,且其 `pinyin` 字段不含 `'`。
    //
    // 三段式(原断言选错了观测点,本轮改写):
    // - 原断言:「西安**出现在**候选里」。但西安在不带 `'` 的 `xian`
    //   下本来就会出现(实机第 18 名),所以这条断言在「`'` 根本没
    //   生效」的 bug 存在时也是绿的——它没测到消歧;
    // - 改为**消歧语义**:消歧的定义是「另一种切法的候选**消失**」。
    //   `xi'an` 的候选里**不得出现**只能由 `[xian]` 这条单音节切法
    //   产生的词(如「先」,pinyin 恰为 `xian`),同时「西安」要在;
    // - 为什么新行为对:`'` 是用户敲的强制切分点(librime 的
    //   `delimiters`),`xi'an` 把输入钉成 [xi | an] 两段,`[xian]`
    //   这条单音节边**不得**再产生——「先/现/显/线」这些只能来自
    //   `[xian]` 的字因此消失,这才是 `'` 起作用的证据。只断「西安
    //   存在」无法区分「`'` 生效」与「`'` 被静默删掉」两种实现。
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("西安", "xi an", 100).expect("插入失败");
    engine.dict.upsert("先", "xian", 50).expect("插入失败");
    let cands = engine
        .lookup_candidates("xi'an", &crate::LmContext::empty())
        .expect("查询失败");
    assert!(
        cands.iter().any(|c| c.word == "西安"),
        "xi'an 应能消歧出「西安」: {:?}",
        cands.iter().take(6).map(|c| &c.word).collect::<Vec<_>>()
    );
    let c = cands.iter().find(|c| c.word == "西安").expect("应存在");
    assert_eq!(c.pinyin, "xi an", "词典 key 不含 `'`:{:?}", c.pinyin);
    // 消歧的关键:只能由 `[xian]` 单音节切法产生的「先」必须消失。
    // 它是 `[xian]` 复活的标志——`'` 若被静默删掉,「先」会照常出现。
    assert!(
        !cands.iter().any(|c| c.word == "先" && c.pinyin == "xian"),
        "xi'an 不应再召回只能由 `[xian]` 切出的「先」(`'` 未生效回归): {:?}",
        cands.iter().take(6).map(|c| &c.word).collect::<Vec<_>>()
    );
}

#[test]
fn apostrophe_never_leaks_into_candidate_pinyin() {
    // `'` 不能进任何 `Candidate.pinyin`(词典 key 是空格分隔)。
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("西安", "xi an", 100).expect("插入失败");
    engine.dict.upsert("先", "xian", 50).expect("插入失败");
    engine.dict.upsert("岸", "an", 80).expect("插入失败");
    let cands = engine
        .lookup_candidates("xi'an", &crate::LmContext::empty())
        .expect("查询失败");
    for c in &cands {
        assert!(!c.pinyin.contains('\''), "Candidate.pinyin 不应含 `'`:{:?}", c.pinyin);
    }
}

#[test]
fn apostrophe_backspace_and_caret_cross_it() {
    // `'` 能被 Backspace 删掉、caret 能跨过它(B1 的可编辑缓冲不许坏)。
    let mut s = session_with_dict();
    for c in "xi'an".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    assert_eq!(s.pinyin(), "xi'an");
    assert_eq!(s.caret(), 5);

    // caret 左移跨过 `'`(从 5 到 2,停在 i 后 ' 前)
    for _ in 0..3 {
        s.handle(InputEvent::Left).expect("不应失败");
    }
    assert_eq!(s.caret(), 2, "caret 应跨过 `'` 与字母");
    // Backspace 删 caret 前的 i → x'an
    s.handle(InputEvent::Backspace).expect("不应失败");
    assert_eq!(s.pinyin(), "x'an");

    // caret 移到 `'` 后(位置 2),Backspace 删 `'` → xan
    let mut s = session_with_dict();
    for c in "x'a".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    // caret 在 3(串尾),左移 1 到 2(' 后),Backspace 删 '
    s.handle(InputEvent::Left).expect("不应失败");
    assert_eq!(s.caret(), 2);
    s.handle(InputEvent::Backspace).expect("不应失败");
    assert_eq!(s.pinyin(), "xa", "Backspace 应能删掉 `'`");
}

#[test]
fn apostrophe_edge_cases_do_not_panic() {
    // 连续 `''`、串首 `'`、串尾 `'`、全 `'` 都不 panic、都返回某个
    // 合法的 SessionOutput(不断言具体候选,只钉住「不崩」)。
    let mut s = session_with_dict();
    for input in ["''", "an'", "'a", "a''n", "'''"] {
        // 每个用全新会话,避免前一个的 preedit 干扰
        let mut s = std::mem::replace(&mut s, session_with_dict());
        for c in input.chars() {
            // 串首的 `'` 会透传(Noop),后续进串;都不应 panic
            let _ = s.handle(InputEvent::Char(c)).expect("不应失败");
        }
    }
}

#[test]
fn ctrl_delete_removes_highlighted_candidate() {
    // 删掉高亮候选后,重查同一拼音,该候选不再出现。
    let engine = Arc::new(
        crate::ImeEngine::new(None)
            .expect("内存引擎创建失败")
            .with_user_lm(crate::UserLM::in_memory()),
    );
    engine.dict.upsert("你", "ni", 200).expect("插入失败");
    engine.dict.upsert("逆", "ni", 50).expect("插入失败");
    let mut s = InputSession::new(Arc::clone(&engine));

    for c in "ni".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    // 高亮在 cursor=0(「你」,词频最高);Ctrl+Delete 删掉它
    let out = s.handle(InputEvent::DeleteCandidate).expect("不应失败");
    match out {
        SessionOutput::Updated(u) => {
            assert!(
                u.candidates.iter().all(|c| c.word != "你"),
                "删掉后「你」不应再出现: {:?}",
                u.candidates.iter().map(|c| &c.word).collect::<Vec<_>>()
            );
            assert!(u.candidates.iter().any(|c| c.word == "逆"), "「逆」应还在");
        }
        other => panic!("DeleteCandidate 应返回 Updated,实际 {other:?}"),
    }
    // 重新查询同一拼音:「你」仍被屏蔽
    let cands = engine
        .lookup_candidates("ni", &crate::LmContext::empty())
        .expect("查询失败");
    assert!(cands.iter().all(|c| c.word != "你"), "重查后「你」仍应被屏蔽");
}

#[test]
fn ctrl_delete_user_coined_entry_also_removed() {
    // 删掉用户造的词 → 用户词条也被删掉(连根拔)。
    let (engine, mut s) = session_for_coin(&[
        ("张", "zhang", 100),
        ("三", "san", 100),
        ("丰", "feng", 100),
    ]);
    let user = engine.user_lm().expect("应有用户层");
    type_and_commit(&mut s, "zhang", "张");
    type_and_commit(&mut s, "san", "三");
    type_and_commit(&mut s, "feng", "丰");
    s.break_coin_run();
    assert_eq!(user.entry_len(), 1, "已造出「张三丰」");

    // 打出整串,定位并删掉「张三丰」候选
    for c in "zhangsanfeng".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let cands = s.engine
        .lookup_candidates("zhangsanfeng", &crate::LmContext::empty())
        .expect("查询失败");
    let idx = cands.iter().position(|c| c.word == "张三丰").expect("应能查到造词");
    // 把高亮移到该候选,再 Ctrl+Delete
    while s.candidates.get(s.cursor).map(|c| c.word.as_str()) != Some("张三丰") {
        s.handle(InputEvent::Down).expect("下移失败");
    }
    let _ = idx;
    s.handle(InputEvent::DeleteCandidate).expect("删除失败");
    assert_eq!(user.entry_len(), 0, "删掉用户造词应连同词条删除");
    assert!(
        user.snapshot().is_blocked("zhang san feng", "张三丰"),
        "且已屏蔽"
    );
}

#[test]
fn ctrl_delete_dict_word_only_blocks_not_touch_dict() {
    // 删掉词库词:屏蔽生效,但词库只读——dict.bin 不被修改。
    // 用内存词库塞一个词当「词库词」,屏蔽后词库里它仍在
    // (lookup 能查到),只是候选收口把它滤掉。
    let engine = Arc::new(
        crate::ImeEngine::new(None)
            .expect("内存引擎创建失败")
            .with_user_lm(crate::UserLM::in_memory()),
    );
    engine.dict.upsert("词库词", "ci ku ci", 100).expect("插入失败");
    let mut s = InputSession::new(Arc::clone(&engine));
    for c in "cikuci".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    // 定位并删掉
    while s.candidates.get(s.cursor).map(|c| c.word.as_str()) != Some("词库词") {
        if s.handle(InputEvent::Down).is_err() { break; }
        if s.cursor > 50 { panic!("找不到词库词候选"); }
    }
    s.handle(InputEvent::DeleteCandidate).expect("删除失败");
    // 词库里它仍在(只读词库没有被改——对内存词库而言 lookup 仍命中)
    let still_in_dict = engine.dict.lookup("ci ku ci").expect("查询失败");
    assert!(
        still_in_dict.iter().any(|c| c.word == "词库词"),
        "词库词只屏蔽,词库本体不动"
    );
    // 但候选列表里它被滤掉
    let cands = engine
        .lookup_candidates("cikuci", &crate::LmContext::empty())
        .expect("查询失败");
    assert!(cands.iter().all(|c| c.word != "词库词"), "候选里应被屏蔽");
}

#[test]
fn ctrl_delete_down_to_empty_does_not_panic() {
    // 删到候选列表空:不 panic,update 照常(cursor/page 收敛到 0)。
    let engine = Arc::new(
        crate::ImeEngine::new(None)
            .expect("内存引擎创建失败")
            .with_user_lm(crate::UserLM::in_memory()),
    );
    engine.dict.upsert("你", "ni", 200).expect("插入失败");
    let mut s = InputSession::new(Arc::clone(&engine));
    for c in "ni".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    // 连续删,直到没有候选(每删一个 refresh 一次,可能冒出回声;
    // 回声也能被 Ctrl+Delete 屏蔽——它的 (key,word)=(ni,ni))
    for _ in 0..8 {
        let has = s.candidates.get(s.cursor).is_some();
        if !has { break; }
        let out = s.handle(InputEvent::DeleteCandidate).expect("删除不应失败");
        assert!(matches!(out, SessionOutput::Updated(_)), "删到空也应 Updated");
    }
    // 再按一次:无候选,吃掉按键、状态不变、不 panic
    let out = s.handle(InputEvent::DeleteCandidate).expect("不应失败");
    assert!(matches!(out, SessionOutput::Updated(_)), "空候选时 DeleteCandidate 吃掉按键不 panic");
}

#[test]
fn ctrl_delete_passthrough_when_idle() {
    // 拼音串为空:Ctrl+Delete 透传给应用(应用的删词快捷键不被吃掉)。
    let mut s = session_with_dict();
    let out = s.handle(InputEvent::DeleteCandidate).expect("不应失败");
    assert!(matches!(out, SessionOutput::Noop), "空闲时 Ctrl+Delete 应透传");
}
