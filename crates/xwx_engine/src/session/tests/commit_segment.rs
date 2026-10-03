//! 分段确认（方案 b）与退回上一段：确认段留在组合里、剩余串继续组合、
//! 退格回退上一次确认、组合结束时一次交给应用。
//!
//! 从 `commit.rs` 拆出（单文件 ≤600 行）。

use super::*;


/// 选中只覆盖前缀的候选之后,**剩下的字母必须留在串里**,并且面板
/// 立刻显示剩余串的候选——这是「一长串拼音连不起来时,正确的先选,
/// 选完接着后面的」的最小可验证形状。
///
/// 定向破坏(逐条都能让它变红):
/// - `consume_covered` 改成 `reset()`(改动前的行为)→ `pinyin()` 为空、
///   输出变回 `Commit` → 红;
/// - `covered_len` 忽略 `Candidate::covered` 一律整串 → 同上 → 红;
/// - `commit_candidate` 忘了 `refresh()` → 剩余串有 preedit 但候选窗
///   是空的(或还是旧列表)→ `update.candidates` 断言红;
/// - 少扣一次 caret → 下面的 caret 断言红。
#[test]
fn committing_a_prefix_candidate_keeps_the_rest_of_the_composition() {
    let mut s = session_with_segments();
    for c in "womenjintian".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let idx = s
        .candidates
        .iter()
        .position(|c| c.word == "我们")
        .expect("候选里应有「我们」");
    assert_eq!(s.candidates[idx].covered, 5, "「我们」覆盖 wo men");

    let out = s.handle(InputEvent::CandidateClicked(idx)).expect("不应失败");
    // 方案 b:这一段**不交给应用**,只进预编辑区(于是退格能退回来)。
    match out {
        SessionOutput::Updated(update) => {
            assert_eq!(update.preedit, "我们jintian", "确认段 + 剩余串都在组合里");
            // 光标按**字符**偏移:已确认段是中文,不能按字节加。
            // 消耗前缀后光标平移到剩余串末尾(7 个字母),加确认段 2 字。
            assert_eq!(update.caret, 2 + 7, "光标落在剩余串末尾");
            assert!(
                !update.candidates.is_empty(),
                "剩余串必须有候选(面板不能变成空的)"
            );
        }
        other => panic!("期望 Updated(确认段留在组合里),实际 {other:?}"),
    }
    assert_eq!(s.pinyin(), "jintian", "剩余串仍在会话里");
    assert!(!s.candidates.is_empty(), "剩余串的候选已刷新");
}

/// **退格回退上一次确认**(方案 b 的退格心智模型)。
///
/// 定向破坏:去掉 Backspace 分支里的 `confirmed.pop()`(退回到「只删
/// 剩余串末位字母」)→ 下面的 pinyin/确认段断言红;`letters` 存成空串 →
/// 恢复出来的输入串不完整 → 红。
#[test]
fn backspace_at_the_segment_boundary_undoes_the_last_confirmation() {
    let mut s = session_with_segments();
    for c in "womenjintian".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let idx = s
        .candidates
        .iter()
        .position(|c| c.word == "我们")
        .expect("候选里应有「我们」");
    s.handle(InputEvent::CandidateClicked(idx)).expect("不应失败");
    assert_eq!(s.pinyin(), "jintian");

    // 光标正停在「已确认段 | 剩余串」的分界处(选完一段的默认状态)。
    let out = s.handle(InputEvent::Backspace).expect("不应失败");
    match out {
        SessionOutput::Updated(update) => {
            assert_eq!(update.preedit, "womenjintian", "确认段退回,输入串原样恢复");
            // 光标回到「选之前那一步」:整串末尾(继续打字就在后面接)。
            assert_eq!(update.caret, "womenjintian".len());
        }
        other => panic!("期望 Updated(退回上一步),实际 {other:?}"),
    }
    assert_eq!(s.pinyin(), "womenjintian", "字母一个都不能少");
    assert!(!s.candidates.is_empty(), "重新出候选(按恢复后的整串算)");
    // 再退一次:没有已确认段了,回到文本编辑惯例(删末位字母)。
    let out = s.handle(InputEvent::Backspace).expect("不应失败");
    match out {
        SessionOutput::Updated(update) => {
            assert_eq!(update.preedit, "womenjintia", "无确认段时退格删末位字母");
        }
        other => panic!("期望 Updated(删字符),实际 {other:?}"),
    }
}

/// 每一步都确认,最后整串结束时**一次**交给应用(方案 b 的落点)。
#[test]
fn confirmed_segments_are_committed_once_at_the_end() {
    let mut s = session_with_segments();
    for c in "womenjintian".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let idx = s
        .candidates
        .iter()
        .position(|c| c.word == "我们")
        .expect("候选里应有「我们」");
    s.handle(InputEvent::CandidateClicked(idx)).expect("不应失败");
    // 剩余串是 jintian,选「今天」(覆盖整串)→ 组合结束。
    let idx = s
        .candidates
        .iter()
        .position(|c| c.word == "今天")
        .expect("剩余串应有「今天」");
    let out = s.handle(InputEvent::CandidateClicked(idx)).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "我们今天"),
        "确认段与最后一段一起上屏: {out:?}"
    );
    assert!(!s.is_active(), "组合结束");
}

/// 覆盖整串的候选仍走**旧路径**(上屏 + 复位 + 收起界面):这条保证
/// 改动对「选一个词就打完了」的常规输入逐字节不变。
#[test]
fn committing_a_whole_input_candidate_still_uses_the_old_path() {
    let mut s = session_with_dict();
    for c in "nihao".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let out = s.handle(InputEvent::Space).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "你好"),
        "覆盖整串时应保持旧的 Commit 语义: {out:?}"
    );
    assert!(!s.is_active(), "上屏后会话复位");
}

/// 不变量:**用户敲下的字母一个都不能少**。
///
/// 「已消耗的字母 + 剩余输入串」恒等于敲过的字母数——改动前选一段会
/// 把后面的字母静默丢掉(既不进文档、也不留在串里),这条断言就是钉
/// 那个缺陷。
#[test]
fn segmented_commits_never_drop_typed_letters() {
    let mut s = session_with_segments();
    let typed = "womenjintian";
    for c in typed.chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let mut consumed = 0usize;
    for step in 0..8 {
        if s.pinyin().is_empty() {
            break;
        }
        let c = s.candidates.first().cloned().unwrap_or_else(|| {
            panic!("第 {step} 步:非空输入串必须有候选(回声兜底是最后一道)")
        });
        let covered = if c.covered == 0 {
            s.pinyin().len()
        } else {
            c.covered
        };
        s.handle(InputEvent::Space).expect("不应失败");
        consumed += covered;
        assert_eq!(
            consumed + s.pinyin().len(),
            typed.len(),
            "第 {step} 步:消耗 {consumed} + 剩余 {} 应等于 {}(字母丢失)",
            s.pinyin().len(),
            typed.len()
        );
        assert!(
            typed.ends_with(s.pinyin()),
            "剩余串必须是原串的后缀,实际 {:?}",
            s.pinyin()
        );
    }
}

/// 逐段上屏**整词**不是造词场景:只有「全部片段都是单字」才够格造成
/// 用户词条(见 [`COIN_ALL_SEGMENTS_SINGLE_CHAR`] 的判据)。
///
/// 分段上屏会让「一次组合里连续上屏多个整词」变得常见,这条测试保证
/// 它不会把整句粘进用户词库——那正是之前修掉的「造词失控」的新入口。
#[test]
fn segmented_commits_of_whole_words_do_not_coin_a_phrase() {
    let (engine, mut s) = session_for_coin(&[
        ("我们", "wo men", 300),
        ("我", "wo", 500),
        ("今天", "jin tian", 300),
    ]);
    let user = engine.user_lm().expect("应有用户层");
    assert_eq!(user.entry_len(), 0, "初始无造词条目");

    for c in "womenjintian".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let idx = s
        .candidates
        .iter()
        .position(|c| c.word == "我们")
        .expect("候选里应有「我们」");
    s.handle(InputEvent::CandidateClicked(idx)).expect("不应失败");
    let idx = s
        .candidates
        .iter()
        .position(|c| c.word == "今天")
        .expect("剩余串的候选里应有「今天」");
    s.handle(InputEvent::CandidateClicked(idx)).expect("不应失败");
    s.break_coin_run();
    assert_eq!(
        user.entry_len(),
        0,
        "连续上屏两个整词不该造词(两个片段都不是单字)"
    );
}

/// 标点结束组合时,**没被候选解释的字母原样跟上**。
///
/// 契约变更:旧断言是 `我们，`——`jintian` 被静默丢掉(测试把「丢字母」
/// 固化成了正确行为)。现在上屏文本 = 确认段 + 候选 + 未覆盖后缀 + 标点,
/// 组合结束;不替用户自动选择余串候选(选错比原样保留更糟)。
#[test]
fn punctuation_after_a_segment_keeps_the_rest_composing() {
    let mut s = session_with_segments();
    for c in "womenjintian".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let idx = s
        .candidates
        .iter()
        .position(|c| c.word == "我们")
        .expect("候选里应有「我们」");
    s.cursor = idx;
    let out = s.handle(InputEvent::Char(',')).expect("不应失败");
    match out {
        // 方案 b:标点结束这次组合——已确认段 + 当前候选 + 标点一次上屏。
        SessionOutput::Commit(text) => {
            assert_eq!(text, "我们jintian，", "候选 + 未覆盖后缀 + 标点,一个字母都不许丢");
        }
        other => panic!("期望 Commit(标点结束组合),实际 {other:?}"),
    }
    assert!(!s.is_active(), "标点之后组合结束");
}

/// `commit_immediately`(不经 `handle` 的低层出口)也要把剩余串的候选刷成新的,
/// 否则调用方读到的是「剩余串 + 旧候选」这种自相矛盾的会话。
#[test]
fn commit_immediately_refreshes_candidates_for_the_remaining_input() {
    let mut s = session_with_segments();
    for c in "womenjintian".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let idx = s
        .candidates
        .iter()
        .position(|c| c.word == "我们")
        .expect("候选里应有「我们」");
    let word = s.commit_immediately(idx).expect("应有候选");
    assert_eq!(word, "我们");
    assert_eq!(s.pinyin(), "jintian");
    let expected = s
        .engine
        .lookup_candidates("jintian", &s.lm_context())
        .expect("查询失败");
    assert_eq!(
        s.candidates.first().map(|c| c.word.clone()),
        expected.first().map(|c| c.word.clone()),
        "剩余串的候选应与引擎对该串的查询一致"
    );
}

/// 第 ③ 期「剩余串的出路」:解不完整(这里 `zz` 解释不了)时,候选表里
/// 有一条**覆盖整串**的「原样上屏」逃生口;选中它就把敲过的字母原样交给
/// 应用,不必退格删光重打。
///
/// 定向破坏:把逃生口那条 `if !ranked.iter().any(covered >= len)` 去掉 →
/// `position` 找不到 → 红;把它改成 `spelling = Abbrev`(进简拼层)→ 会排在
/// 真实候选之前、把「你好」挤下去 → 上面那条 `candidates[0] == 你好` 的
/// 断言会红。
#[test]
fn escape_hatch_commits_the_unexplained_remainder_verbatim() {
    let (engine, mut s) = session_for_coin(&[("你好", "ni hao", 900)]);
    let user = engine.user_lm().expect("应有用户层");
    for c in "nihaozzzzz".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    assert_eq!(
        s.candidates.first().map(|c| c.word.clone()).as_deref(),
        Some("你好"),
        "真实候选仍在最前: {:?}",
        s.candidates.iter().map(|c| &c.word).collect::<Vec<_>>()
    );
    let idx = s
        .candidates
        .iter()
        .position(|c| c.echo && c.word == "nihaozzzzz")
        .unwrap_or_else(|| {
            panic!(
                "应有原样上屏的逃生口: {:?}",
                s.candidates
                    .iter()
                    .map(|c| (&c.word, c.echo, c.covered))
                    .collect::<Vec<_>>()
            )
        });
    s.cursor = idx;
    let out = s.handle(InputEvent::Space).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "nihaozzzzz"),
        "逃生口应把剩余字母原样上屏: {out:?}"
    );
    s.break_coin_run();
    assert_eq!(user.entry_len(), 0, "原样上屏不是词,不该造词");
}

/// 点击越界/无候选时不做任何事(不 panic、不动状态)。
#[test]
fn clicking_an_out_of_range_candidate_is_a_noop() {
    let mut s = session_with_segments();
    for c in "women".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let before = s.pinyin().to_owned();
    let out = s
        .handle(InputEvent::CandidateClicked(999))
        .expect("不应失败");
    assert!(matches!(out, SessionOutput::Noop), "越界点击应无事发生");
    assert_eq!(s.pinyin(), before);
}

#[test]
fn committed_text_feeds_next_candidates() {
    // 端到端：上屏「中」后继续打 guo，上文必须真的进到打分里
    // （bigram(中,国) 命中 → 「国」压过词频更高的「过」）。
    let mut s = session_with_lm();
    for c in ['z', 'h', 'o', 'n', 'g'] {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let out = s.handle(InputEvent::Space).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "中"));
    assert_eq!(s.last_word.as_deref(), Some("中"), "上屏词应成为词级上文");
    assert_eq!(s.committed_tail, "中");

    for c in ['g', 'u', 'o'] {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let out = s.handle(InputEvent::Space).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "国"),
        "上文「中」应让「国」压过「过」成为首选"
    );
    assert_eq!(s.committed_tail, "中国", "上下文应跨候选累积");
}

#[test]
fn raw_commit_resets_last_word_but_keeps_tail() {
    let mut s = session_with_dict();
    for c in ['n', 'i'] {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    // 选词上屏：last_word = 该词
    let out = s.handle(InputEvent::Space).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "你"));
    assert_eq!(s.last_word.as_deref(), Some("你"));

    // 回车：原始字母上屏（不是词）→ last_word 置 None，尾部仍累积
    for c in ['h', 'a', 'o'] {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let out = s.handle(InputEvent::Enter).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "hao"));
    assert_eq!(s.last_word, None, "原始字母上屏后 last_word 应为 None");
    assert_eq!(s.committed_tail, "你hao", "上屏文本应累积进尾部");
}

#[test]
fn committed_tail_is_bounded_on_char_boundary() {
    let mut s = session_with_dict();

    // 20 个汉字（40 字节）超出上限：只留末尾 16 个完整字符
    s.commit_raw("一二三四五六七八九十一二三四五六七八九十".to_string());
    assert_eq!(s.committed_tail, "五六七八九十一二三四五六七八九十");
    assert_eq!(
        s.committed_tail.chars().count(),
        COMMITTED_TAIL_LIMIT,
        "尾部应有界"
    );

    // 多字节字符（emoji 4 字节/个）：截断必须按 char，不能按字节
    // （按字节会切坏 UTF-8 序列或 panic）
    s.commit_raw("😀".repeat(20));
    s.commit_raw("尾".to_string());
    assert_eq!(s.committed_tail.chars().count(), COMMITTED_TAIL_LIMIT);
    assert_eq!(s.committed_tail.chars().last(), Some('尾'), "最新上屏字符应在尾部");
    assert!(
        std::str::from_utf8(s.committed_tail.as_bytes()).is_ok(),
        "截断后必须是合法 UTF-8（char 边界截断）"
    );
}

/// 大写/Verbatim 结束组合时同样不许丢后缀。
#[test]
fn verbatim_after_a_segment_keeps_the_uncovered_suffix() {
    let mut s = session_with_segments();
    for c in "womenjintian".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let idx = s
        .candidates
        .iter()
        .position(|c| c.word == "我们")
        .expect("候选里应有「我们」");
    s.cursor = idx;
    let out = s.handle(InputEvent::Verbatim('G')).expect("不应失败");
    match out {
        SessionOutput::Commit(text) => {
            assert_eq!(text, "我们jintianG", "Shift+字母:候选 + 未覆盖后缀 + 大写字母");
        }
        other => panic!("期望 Commit(大写结束组合),实际 {other:?}"),
    }
    assert!(!s.is_active(), "组合结束");
}

/// 候选覆盖整串时行为不变(后缀为空):标点照旧紧跟候选。
#[test]
fn covered_candidate_keeps_the_punctuation_immediately_after_it() {
    let mut s = session_with_segments();
    for c in "womenjintian".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    // 先确认「我们」「今天」两段,让剩下的候选覆盖整串。
    let idx = s.candidates.iter().position(|c| c.word == "我们").expect("应有我们");
    s.handle(InputEvent::CandidateClicked(idx)).expect("不应失败");
    let idx = s.candidates.iter().position(|c| c.word == "今天").expect("应有今天");
    s.cursor = idx;
    let out = s.handle(InputEvent::Char(',')).expect("不应失败");
    match out {
        SessionOutput::Commit(text) => assert_eq!(text, "我们今天，"),
        other => panic!("期望 Commit,实际 {other:?}"),
    }
}

/// 确认一段**既不上屏也不学习**,两者都推迟到组合结束。
///
/// 这条钉的是交互语义,不是接口名字:同一步用「立即提交」接口
/// (`commit_immediately`)会立刻学习并返回文本,所以它在旧语义下必然失败。
/// 回归探针 C 段记录的是同一条路径的真实按键输出。
#[test]
fn confirming_a_segment_defers_commit_and_learning_until_the_combination_ends() {
    let engine = Arc::new(
        crate::ImeEngine::new(None)
            .expect("内存引擎创建失败")
            .with_user_lm(crate::UserLM::in_memory()),
    );
    engine.dict.upsert("我们", "wo men", 300).expect("插入失败");
    engine.dict.upsert("今天", "jin tian", 300).expect("插入失败");
    let mut s = InputSession::new(Arc::clone(&engine));
    for c in "womenjintian".chars() {
        s.handle(InputEvent::Char(c)).expect("按键失败");
    }

    let user = engine.user_lm().expect("应有用户层");
    // 每次取**新的**快照:快照是当时表的克隆,拿旧快照读「现在学没学」读不到。
    let learned = |w: &str| user.counts_with(&user.snapshot(), None, w);

    // ① 确认「我们」(只覆盖 women):界面刷新、组合还在、零学习。
    let idx = s
        .candidates
        .iter()
        .position(|c| c.word == "我们")
        .expect("应有「我们」");
    let out = s.handle(InputEvent::CandidateClicked(idx)).expect("确认失败");
    let SessionOutput::Updated(u) = &out else {
        panic!("确认段不应上屏: {out:?}");
    };
    assert!(
        u.preedit.contains("我们") && u.preedit.contains("jintian"),
        "确认段留在预编辑区、剩余串继续显示: {:?}",
        u.preedit
    );
    assert_eq!(s.pinyin(), "jintian", "剩余串仍是未上屏的输入");
    assert!(s.is_active(), "组合没有结束");
    assert_eq!(learned("我们"), (0, 0), "确认段不学习");

    // ② 退格回退上一次确认:字母原样还回,依然零学习。
    let out = s.handle(InputEvent::Backspace).expect("退格失败");
    assert!(
        matches!(out, SessionOutput::Updated(_)),
        "回退只刷新界面: {out:?}"
    );
    assert_eq!(s.pinyin(), "womenjintian", "退格把已确认段的字母还回输入串");
    assert_eq!(learned("我们"), (0, 0), "回退不留学习痕迹");

    // ③ 重新确认到底:这一下才上屏,而且这时才学习。
    let idx = s
        .candidates
        .iter()
        .position(|c| c.word == "我们")
        .expect("应有「我们」");
    s.handle(InputEvent::CandidateClicked(idx)).expect("确认失败");
    let idx = s
        .candidates
        .iter()
        .position(|c| c.word == "今天")
        .expect("应有「今天」");
    let out = s.handle(InputEvent::CandidateClicked(idx)).expect("确认失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "我们今天"),
        "整串结束时一次交给应用: {out:?}"
    );
    assert!(
        learned("我们").0 > 0 && learned("今天").0 > 0,
        "上屏时才学习: 我们={:?} 今天={:?}",
        learned("我们"),
        learned("今天")
    );
}

/// 反面对照(见证):同一个输入、同一步用**立即提交**接口,行为完全不同——
/// 它立刻返回文本、立刻学习。这正是 `commit_immediately` 不能冒充真实
/// 交互路径的原因,两条接口的差别由这两条用例分别钉住。
#[test]
fn immediate_commit_learns_at_once_unlike_the_real_interaction_path() {
    let engine = Arc::new(
        crate::ImeEngine::new(None)
            .expect("内存引擎创建失败")
            .with_user_lm(crate::UserLM::in_memory()),
    );
    engine.dict.upsert("我们", "wo men", 300).expect("插入失败");
    engine.dict.upsert("今天", "jin tian", 300).expect("插入失败");
    let mut s = InputSession::new(Arc::clone(&engine));
    for c in "womenjintian".chars() {
        s.handle(InputEvent::Char(c)).expect("按键失败");
    }
    let user = engine.user_lm().expect("应有用户层");
    let idx = s
        .candidates
        .iter()
        .position(|c| c.word == "我们")
        .expect("应有「我们」");

    let text = s.commit_immediately(idx).expect("立即提交应有文本");
    assert_eq!(text, "我们");
    assert!(
        user.counts_with(&user.snapshot(), None, "我们").0 > 0,
        "立即提交接口在返回文本的同一步就学习了"
    );
}

/// 确认段要进打分的**字符级窗口**:确认「北京」之后,剩余串的候选要看得到它。
///
/// 旧实现里 `lm_context()` 的 tail 只有 `committed_tail`(已经上屏的文本),
/// 方案 b 的确认段不在其中 ⇒ 这条断言在旧实现上必然失败。
/// 用户层的学习仍推迟到组合结束(那是刻意设计),这里钉的是 LM 上文这一半。
#[test]
fn confirming_a_segment_feeds_the_language_model_window() {
    let engine = Arc::new(
        crate::ImeEngine::new(None)
            .expect("内存引擎创建失败")
            .with_user_lm(crate::UserLM::in_memory()),
    );
    engine
        .dict
        .upsert("北京", "bei jing", 3000)
        .expect("插入失败");
    engine.dict.upsert("大学", "da xue", 300).expect("插入失败");
    let mut s = InputSession::new(Arc::clone(&engine));
    for c in "beijingdaxue".chars() {
        s.handle(InputEvent::Char(c)).expect("按键失败");
    }
    assert!(
        !s.lm_context().tail.contains("北京"),
        "既没确认也没上屏,上下文里不该有它: {:?}",
        s.lm_context().tail
    );

    let idx = s
        .candidates
        .iter()
        .position(|c| c.word == "北京")
        .expect("应有「北京」");
    s.handle(InputEvent::CandidateClicked(idx)).expect("确认失败");

    assert_eq!(s.pinyin(), "daxue", "剩余串仍是未上屏的输入");
    assert!(
        s.lm_context().tail.contains("北京"),
        "确认段要进 LM 的字符级窗口: {:?}",
        s.lm_context().tail
    );
}
