//! `session` 单测:coin(从 `session/tests.rs` 拆出,2026-10-02)。

use super::*;

#[test]
fn char_by_char_selection_coins_phrase() {
    // 验收判据 A1:逐字选「张」「三」「丰」上屏后,再打同样拼音,
    // 「张三丰」应作为用户词条出现在候选里。
    let (engine, mut s) = session_for_coin(&[
        ("张", "zhang", 100),
        ("三", "san", 100),
        ("丰", "feng", 100),
    ]);
    let user = engine.user_lm().expect("应有用户层");
    assert_eq!(user.entry_len(), 0, "初始无造词条目");

    type_and_commit(&mut s, "zhang", "张");
    type_and_commit(&mut s, "san", "三");
    type_and_commit(&mut s, "feng", "丰");
    // 三次上屏后缓冲仍在累积(等下个事件才知道段是否结束);
    // 这里显式结算(测试是 crate 内,可直接调 break_coin_run)。
    s.break_coin_run();
    assert_eq!(user.entry_len(), 1, "三次连续选词应造出一个词条");

    // 再打同样的整串拼音:「张三丰」应在候选里。
    let cands = engine
        .lookup_candidates("zhangsanfeng", &crate::LmContext::empty())
        .expect("查询失败");
    assert!(
        cands.iter().any(|c| c.word == "张三丰"),
        "造词后「张三丰」应能被查到: {:?}",
        cands.iter().take(8).map(|c| &c.word).collect::<Vec<_>>()
    );
    // 拼音 key 应为三段拼接。
    let c = cands.iter().find(|c| c.word == "张三丰").expect("应存在");
    assert_eq!(c.pinyin, "zhang san feng");
}

#[test]
fn coining_does_not_cross_punctuation() {
    // 验收判据 A2:选「你」「好」后打逗号,再选「世」「界」,
    // 不该造出「你好世界」——标点打断造词缓冲。
    let (engine, mut s) = session_for_coin(&[
        ("你", "ni", 100),
        ("好", "hao", 100),
        ("世", "shi", 100),
        ("界", "jie", 100),
    ]);
    let user = engine.user_lm().expect("应有用户层");

    type_and_commit(&mut s, "ni", "你");
    type_and_commit(&mut s, "hao", "好");
    // 打逗号(空闲标点上屏):应打断造词缓冲,造出「你好」而非积累。
    let out = s.handle(InputEvent::Char(',')).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(_)), "逗号应上屏");
    assert_eq!(user.entry_len(), 1, "逗号应结算「你好」词条");

    type_and_commit(&mut s, "shi", "世");
    type_and_commit(&mut s, "jie", "界");
    // 光标移出/另一个打断来结算「世界」:用回车(原始上屏)打断。
    s.handle(InputEvent::Char('x')).expect("按键失败");
    s.handle(InputEvent::Enter).expect("回车失败");
    assert_eq!(user.entry_len(), 2, "应再结算「世界」词条");

    // 验证:能查到「你好」与「世界」,查不到「你好世界」。
    let snap = user.snapshot();
    assert_eq!(snap.lookup_entries("ni hao"), &["你好".to_string()]);
    assert_eq!(snap.lookup_entries("shi jie"), &["世界".to_string()]);
    assert!(
        snap.lookup_entries("ni hao shi jie").is_empty(),
        "不应造出跨标点的「你好世界」"
    );
}

#[test]
fn coining_does_not_cross_focus_change() {
    // 焦点切换（IBus FocusOut/Reset/Disable）是造词的真边界：
    // 在一个应用里逐字选「你」「好」，切到另一个应用逐字选
    // 「世」「界」，不该粘成「你好世界」。
    //
    // 注意 reset() 本身**不打断**造词缓冲（上屏路径每次都调它，
    // 打断了就永远攒不起来）——焦点切换必须走
    // reset_for_focus_change()，这个测试就是钉住这个区别的。
    let (engine, mut s) = session_for_coin(&[
        ("你", "ni", 100),
        ("好", "hao", 100),
        ("世", "shi", 100),
        ("界", "jie", 100),
    ]);
    let user = engine.user_lm().expect("应有用户层");

    type_and_commit(&mut s, "ni", "你");
    type_and_commit(&mut s, "hao", "好");
    // 焦点离开：结算「你好」并打断，后续片段另起一段。
    s.reset_for_focus_change();
    assert_eq!(user.entry_len(), 1, "焦点切换应结算「你好」");

    type_and_commit(&mut s, "shi", "世");
    type_and_commit(&mut s, "jie", "界");
    s.reset_for_focus_change();
    assert_eq!(user.entry_len(), 2, "第二段应单独结算「世界」");

    let snap = user.snapshot();
    assert_eq!(snap.lookup_entries("ni hao"), &["你好".to_string()]);
    assert_eq!(snap.lookup_entries("shi jie"), &["世界".to_string()]);
    assert!(
        snap.lookup_entries("ni hao shi jie").is_empty(),
        "不应造出跨焦点的「你好世界」"
    );
}

#[test]
fn echo_and_verbatim_do_not_coin() {
    // 验收判据 A3:回声候选、Verbatim 大写字母不进造词。
    let (engine, mut s) = session_for_coin(&[("你", "ni", 100)]);
    let user = engine.user_lm().expect("应有用户层");

    // 回声候选上屏(空词库解不出的串):不进缓冲。
    for ch in "zgr".chars() {
        s.handle(InputEvent::Char(ch)).expect("按键失败");
    }
    s.handle(InputEvent::Space).expect("空格失败"); // 上屏回声 zgr
    // Verbatim 大写:不进缓冲。
    s.handle(InputEvent::Verbatim('G')).expect("不应失败");
    // 回车原始上屏:不进缓冲。
    s.handle(InputEvent::Char('a')).expect("按键失败");
    s.handle(InputEvent::Enter).expect("回车失败");
    assert_eq!(user.entry_len(), 0, "回声/Verbatim/回车都不应造词");
}

#[test]
fn single_complete_word_commit_does_not_duplicate() {
    // 验收判据 A4:单次上屏的完整词不重复造词——词库已有的词
    // 上屏不应在用户词典里再造一份。
    let (engine, mut s) = session_for_coin(&[("你好", "ni hao", 100)]);
    let user = engine.user_lm().expect("应有用户层");

    type_and_commit(&mut s, "nihao", "你好");
    assert_eq!(
        user.entry_len(),
        0,
        "单次完整词上屏(缓冲长度 1)不应造词条目"
    );
    // 但调频计数照常(既有 learn 通道不受影响)。
    assert_eq!(user.unigram_len(), 1, "unigram 学习应照常");
}

/// **回归测试,证据来自真实用户数据。**
///
/// `~/.local/share/xwx/user_bigram.txt` 的造词段里出现过:
/// ```text
/// 当前主线以及已经完了是吧进度  dang qian zhu xian yi ji yi jing wan le shi ba jin du
/// 设置为说着玩的说说中文        she zhi wei shuo zhe wan de shuo shuo zhong wen
/// ```
/// 这是「把候选当空格用」的连续打字被粘成词条——用户毫无感知,
/// 词条却永久占候选位。根因是 `break_coin_run` 只判「片段数 ≥ 2」。
///
/// 注意:第一版判据取的是「**存在**至少一个单字片段」,那版会漏掉
// 下面第 2 条(它的片段里有单字「为」「说」),已经是错的——
// 现在取「**全部**片段都是单字」,两条都被拦住。
#[test]
fn consecutive_multi_char_words_do_not_coin_a_sentence() {
    let (engine, mut s) = session_for_coin(&[
        ("当前", "dang qian", 100),
        ("主线", "zhu xian", 100),
        ("进度", "jin du", 100),
    ]);
    let user = engine.user_lm().expect("应有用户层");

    // 连续上屏三个「多字词」——都是完整词,不是逐字挑的。
    type_and_commit(&mut s, "dangqian", "当前");
    type_and_commit(&mut s, "zhuxian", "主线");
    type_and_commit(&mut s, "jindu", "进度");
    s.break_coin_run();

    assert_eq!(
        user.entry_len(),
        0,
        "连续上屏多字词不该粘成句子词条(修复前这里会造出「当前主线进度」)"
    );
    assert!(
        user.snapshot().lookup_entries("dang qian zhu xian jin du").is_empty(),
        "不该有这条拼起来的词条"
    );
    // 调频(learn)通道不受影响:它本来就是「用过就加频」的语义。
    assert_eq!(user.unigram_len(), 3, "unigram 调频应照常记录三个词");
}

/// **回归测试,证据是真实数据第 2 条**——它专门钉住「存在单字」
/// 那版错误判据。
///
/// 真实词条是 `设置为说着玩的说说中文`,其片段为
/// `设置(2) / 为(1) / 说着玩的(4) / 说(1) / 说中文(3)`——
/// **含单字但含多字词**。取「存在单字 ⇒ 够格」的实现会把它造出来;
/// 取「全部单字 ⇒ 够格」才拦得住。
///
/// 这条断言的价值正在于它**能证伪第一版实现**:把
/// `is_reusable_phrase` 的 `.all(...)` 改回 `.any(...)`,它立刻变红。
#[test]
fn mixed_single_and_multi_char_segments_do_not_coin() {
    let (engine, mut s) = session_for_coin(&[
        ("设置", "she zhi", 100),
        ("为", "wei", 100),
        ("说着玩的", "shuo zhe wan de", 100),
        ("说", "shuo", 100),
        ("说中文", "shuo zhong wen", 100),
    ]);
    let user = engine.user_lm().expect("应有用户层");

    type_and_commit(&mut s, "shezhi", "设置");
    type_and_commit(&mut s, "wei", "为");
    type_and_commit(&mut s, "shuozhewande", "说着玩的");
    type_and_commit(&mut s, "shuo", "说");
    type_and_commit(&mut s, "shuozhongwen", "说中文");
    s.break_coin_run();

    assert_eq!(
        user.entry_len(),
        0,
        "含整词片段的连续上屏不该造词——即使中间夹着单字。\
         这正是真实数据「设置为说着玩的说说中文」的形状"
    );
    assert!(
        user.snapshot()
            .lookup_entries("she zhi wei shuo zhe wan de shuo shuo zhong wen")
            .is_empty(),
        "不该有这条真实数据形状的词条"
    );
}

/// 逐字组词的**验收不能被上面那条误伤**。
///
/// 「张三丰」是当年点名的验收判据(逐字选三个单字,
/// 再打整串拼音应能召回)。它的片段全是单字,必须仍然造词。
///
/// 这条与上一条构成**双向判据**:只测「句子不造词」会被
/// 「造词彻底关掉」的实现骗过,只测「张三丰能造」会被原来的
/// bug 骗过。两条一起才钉住「该造的造、不该造的不造」。
#[test]
fn char_by_char_coining_still_satisfies_predicate() {
    let (engine, mut s) = session_for_coin(&[
        ("张", "zhang", 100),
        ("三", "san", 100),
        ("丰", "feng", 100),
    ]);
    let user = engine.user_lm().expect("应有用户层");

    type_and_commit(&mut s, "zhang", "张");
    type_and_commit(&mut s, "san", "三");
    // 缓冲里已有两个单字片段,判据此刻就应为真。
    assert!(
        s.is_reusable_phrase(),
        "两个单字片段应够格造词: {:?}",
        s.coin_buffer
    );
    type_and_commit(&mut s, "feng", "丰");
    s.break_coin_run();

    assert_eq!(user.entry_len(), 1, "逐字组词应造出「张三丰」");
    assert_eq!(
        user.snapshot().lookup_entries("zhang san feng"),
        &["张三丰".to_string()],
        "词条拼音 key 应为三段拼接"
    );
}

/// `is_reusable_phrase` 的**逐条判据表**:把「够格/不够格」的
/// 各种组合钉死,而不是只测两个端到端场景。
///
/// 造词是「无感知写入持久数据」的行为,判据必须能被逐条检查。
#[test]
fn reusable_phrase_predicate_table() {
    let (_engine, mut s) = session_for_coin(&[]);

    // 空缓冲 / 单片段:不够格(单片段是一次完整词上屏)。
    assert!(!s.is_reusable_phrase(), "空缓冲不够格");
    s.coin_buffer.push(("你".into(), "ni".into()));
    assert!(!s.is_reusable_phrase(), "单片段不够格");

    // 两个单字:够格。
    s.coin_buffer.push(("好".into(), "hao".into()));
    assert!(s.is_reusable_phrase(), "两个单字够格");

    // 全多字词:不够格(连续打字的形状)。
    s.coin_buffer.clear();
    s.coin_buffer.push(("你好".into(), "ni hao".into()));
    s.coin_buffer.push(("世界".into(), "shi jie".into()));
    assert!(!s.is_reusable_phrase(), "全多字词不够格");

    // 单字 + 多字混合:**不够格**。
    // 这是判据演化过的关键一格:第一版用 `.any(...)`(存在单字即可)
    // 时这里是够格的,于是真实数据的「设置为说着玩的说说中文」
    // (含单字「为」「说」)被误造成词条。改成 `.all(...)` 后不够格。
    s.coin_buffer.push(("张".into(), "zhang".into()));
    assert!(
        !s.is_reusable_phrase(),
        "只要有一个整词片段就不够格——整词意味着在组句/正常打字,不是在造词"
    );

    // 多字 + 单字,顺序无所谓(仍不够格)。
    s.coin_buffer.clear();
    s.coin_buffer.push(("你好".into(), "ni hao".into()));
    s.coin_buffer.push(("啊".into(), "a".into()));
    assert!(!s.is_reusable_phrase(), "顺序不影响判据:仍不够格");

    // 三个单字(张三丰/西双版纳的形状):够格。
    s.coin_buffer.clear();
    for (w, p) in [("西", "xi"), ("双", "shuang"), ("版", "ban"), ("纳", "na")] {
        s.coin_buffer.push((w.into(), p.into()));
    }
    assert!(s.is_reusable_phrase(), "全单字四个片段够格(西双版纳)");
}

/// 不够格时缓冲**仍须清空**——否则上一句的尾巴会被拼进下一次
/// 结算,反而造出跨句的更大垃圾词条。
#[test]
fn non_reusable_run_still_clears_buffer() {
    let (engine, mut s) = session_for_coin(&[("你好", "ni hao", 100), ("世界", "shi jie", 100)]);
    let user = engine.user_lm().expect("应有用户层");

    type_and_commit(&mut s, "nihao", "你好");
    type_and_commit(&mut s, "shijie", "世界");
    assert!(!s.coin_buffer.is_empty(), "缓冲应已累积");
    s.break_coin_run();
    assert!(s.coin_buffer.is_empty(), "结算后缓冲必须清空");
    assert_eq!(user.entry_len(), 0, "不够格不造词");
}

#[test]
fn coined_entry_persists_across_flush_and_reopen() {
    // 验收判据 A5:造词条目落盘后能读回。
    let dir = std::env::temp_dir().join(format!("xwx_coin_test_{}", std::process::id()));
    let path = dir.join("user_bigram.txt");
    let _ = std::fs::remove_dir_all(&dir);
    {
        let lm = crate::UserLM::open_or_create(&path);
        lm.learn(None, "张");
        lm.learn(Some("张"), "三");
        lm.learn(Some("三"), "丰");
        lm.add_entry("张三丰", "zhang san feng");
        lm.flush().expect("flush 不应失败");
        assert!(path.is_file());
    }
    {
        let lm = crate::UserLM::open_or_create(&path);
        assert_eq!(lm.entry_len(), 1, "重开后造词条目应读回");
        let snap = lm.snapshot();
        assert_eq!(
            snap.lookup_entries("zhang san feng"),
            &["张三丰".to_string()],
            "重开后按拼音 key 应能查到造词"
        );
        // 计数区也应读回(同一份文件两段)。
        assert_eq!(lm.unigram_len(), 3, "重开后 unigram 应读回");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn coined_entry_surfaces_via_lattice_like_dict_word() {
    // 造词条目与词库词条同层同式:它应参与整串解码(覆盖整串),
    // 不是候选列表末尾的孤岛。词库里没有「张三丰」整词,只有单字;
    // 造词后整串查询应把「张三丰」排到比「逐字路径」更前的位置
    // (整词候选分 = 自身 + 0 补全,优于逐字拆解的多次打分)。
    let (engine, mut s) = session_for_coin(&[
        ("张", "zhang", 100),
        ("三", "san", 90),
        ("丰", "feng", 80),
    ]);
    type_and_commit(&mut s, "zhang", "张");
    type_and_commit(&mut s, "san", "三");
    type_and_commit(&mut s, "feng", "丰");
    s.break_coin_run(); // 结算缓冲(测试 crate 内直接调)

    let cands = engine
        .lookup_candidates("zhangsanfeng", &crate::LmContext::empty())
        .expect("查询失败");
    let pos = cands
        .iter()
        .position(|c| c.word == "张三丰")
        .expect("造词后应能查到");
    assert_eq!(pos, 0, "整词造词候选应排第一(覆盖整串): {:?}",
        cands.iter().take(5).map(|c| &c.word).collect::<Vec<_>>());
    // 它是 Exact 层(精确匹配),不是回声/简拼。
    assert_eq!(cands[pos].spelling, crate::SpellingKind::Exact);
    assert!(!cands[pos].echo);
}

#[test]
fn coin_buffer_cap_drops_overlong_run() {
    // 长度上限:超过 MAX_COIN_CHARS 的连续上屏段被结算后清空,
    // 不无限粘成一个巨词。逐字上屏 MAX_COIN_CHARS+2 个单字,
    // 缓冲在超限处结算一次(长度足够则造词),随后从空重新累积。
    let entries: Vec<(String, String, u64)> = (0..30u32)
        .map(|i| {
            (
                char::from_u32(0x4E00 + i).unwrap().to_string(),
                "a".to_string(),
                100 - i as u64,
            )
        })
        .collect();
    let engine = Arc::new(
        crate::ImeEngine::new(None)
            .expect("内存引擎创建失败")
            .with_user_lm(crate::UserLM::in_memory()),
    );
    let mut entries_ref: Vec<(&str, &str, u64)> = Vec::new();
    for (w, py, f) in &entries {
        entries_ref.push((w.as_str(), py.as_str(), *f));
    }
    let _ = &entries_ref; // 用 engine 直接 upsert(借用生命周期)
    for (w, py, f) in &entries {
        engine.dict.upsert(w, py, *f).expect("插入失败");
    }
    let mut s = InputSession::new(Arc::clone(&engine));
    let user = engine.user_lm().expect("应有用户层");

    // 逐字上屏 MAX_COIN_CHARS + 2 个字;每个字拼音都是 "a",
    // 但逐字上屏时输入是单字母 "a",候选第一个就是当前字
    // (词频降序),逐个选「当前该选的字」太脆弱——改为直接用
    // commit_immediately(0) 上屏当前首选,只验证缓冲长度上限行为。
    for _ in 0..(MAX_COIN_CHARS + 2) {
        s.handle(InputEvent::Char('a')).expect("按键失败");
        s.commit_immediately(0).expect("应有候选");
    }
    // 超限结算:缓冲长度 ≤ MAX_COIN_CHARS(结算后重新累积)。
    let buffered: usize = s.coin_buffer.iter().map(|(w, _)| w.chars().count()).sum();
    assert!(
        buffered <= MAX_COIN_CHARS,
        "缓冲不应超过上限: {buffered}"
    );
    // 至少结算过一次超限段(造了词条或缓冲被清)。
    assert!(user.entry_len() >= 1 || buffered <= 2,
        "超限应触发结算: entry_len={} buffered={buffered}", user.entry_len());
}

/// 组合中打标点也是造词边界。
///
/// 既有 `coining_does_not_cross_punctuation` 只覆盖了**空闲时**打标点;
/// 拼音非空(还有候选高亮)时走的是另一条分支——那条以前只 `push_tail`,
/// 不结算造词缓冲,于是标点两侧的单字会粘成一个词。
#[test]
fn punctuation_while_composing_also_breaks_the_coin_run() {
    let (engine, mut s) = session_for_coin(&[
        ("张", "zhang", 100),
        ("三", "san", 100),
        ("丰", "feng", 100),
    ]);
    let user = engine.user_lm().expect("应有用户层");

    type_and_commit(&mut s, "zhang", "张");
    type_and_commit(&mut s, "san", "三");
    // 拼音非空、有候选时打逗号:应结算「张三」并结束组合。
    s.handle(InputEvent::Char('f')).expect("按键失败");
    let out = s.handle(InputEvent::Char(',')).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(_)), "逗号应上屏: {out:?}");

    assert_eq!(user.entry_len(), 1, "标点时应结算「张三」(而不是把它带过标点)");
    let snap = user.snapshot();
    assert_eq!(snap.lookup_entries("zhang san"), &["张三".to_string()]);

    // 标点后再选「丰」:标点两侧的片段不许连成一条词。
    type_and_commit(&mut s, "feng", "丰");
    s.handle(InputEvent::Char('x')).expect("按键失败");
    s.handle(InputEvent::Enter).expect("回车失败");
    let snap = user.snapshot();
    assert!(
        snap.lookup_entries("zhang san feng").is_empty(),
        "不该跨标点造出「张三丰」"
    );
}

/// 中英模式切换也是造词边界:空闲切走再切回来,
/// 两边的单字不许粘成一个词。
#[test]
fn toggling_mode_breaks_the_coin_run() {
    let (engine, mut s) = session_for_coin(&[("张", "zhang", 100), ("三", "san", 100), ("丰", "feng", 100)]);
    let user = engine.user_lm().expect("应有用户层");

    type_and_commit(&mut s, "zhang", "张");
    type_and_commit(&mut s, "san", "三");
    // 空闲时切英文、再切回来(两次 ToggleAscii)。
    s.handle(InputEvent::ToggleAscii).expect("不应失败");
    s.handle(InputEvent::ToggleAscii).expect("不应失败");
    assert_eq!(user.entry_len(), 1, "切模式时应结算「张三」");

    type_and_commit(&mut s, "feng", "丰");
    s.handle(InputEvent::Char('x')).expect("按键失败");
    s.handle(InputEvent::Enter).expect("回车失败");

    let snap = user.snapshot();
    assert!(
        snap.lookup_entries("zhang san feng").is_empty(),
        "不该跨模式切换造出「张三丰」"
    );
}

/// 超长缓冲必须**丢弃**,不能反过来造出一条 25+ 字的用户词条。
#[test]
fn over_long_coin_buffer_is_discarded_not_coined() {
    let (engine, mut s) = session_for_coin(&[("甲", "jia", 100)]);
    let user = engine.user_lm().expect("应有用户层");
    // 直接铺满缓冲(每个片段单字,数量超过 MAX_COIN_CHARS)。
    // 必须**全是单字**(否则不被「可复用短语」判据放行,测不到长度这一条)。
    for _ in 0..(MAX_COIN_CHARS + 1) {
        s.coin_buffer.push(("甲".to_string(), "jia".to_string()));
    }
    assert!(!s.is_reusable_phrase(), "超长缓冲不够格造词");
    s.break_coin_run();
    assert_eq!(user.entry_len(), 0, "超长缓冲必须只清空、不造词");
    assert!(s.coin_buffer.is_empty(), "缓冲仍要清掉");
}
