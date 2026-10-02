//! `session` 单测:punct_verbatim(从 `session/tests.rs` 拆出,2026-10-02)。

use super::*;

#[test]
fn punct_commits_fullwidth_when_idle() {
    // 中文模式、拼音串为空:标点键直接上屏全角标点。
    let mut s = session_with_dict();
    let out = s.handle(InputEvent::Char(',')).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "\u{FF0C}"),
        "空闲时 `,` 应上屏全角逗号: {out:?}"
    );
    assert!(!s.is_active(), "上屏后会话应保持空闲");
    assert_eq!(s.last_word, None, "标点不是词,last_word 应为 None");
    assert_eq!(s.committed_tail, "\u{FF0C}", "标点应计入上屏尾部");
}

#[test]
fn punct_commits_highlighted_candidate_then_fullwidth() {
    // 中文模式、拼音串非空(`nihao`):`,` 先按空格语义上屏高亮候选
    // (首选整词「你好」,与 commit_immediately_commits_global_index 同一词典),
    // 再追加全角逗号——用户得到「你好,」。
    let mut s = session_with_dict();
    for c in "nihao".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let out = s.handle(InputEvent::Char(',')).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "你好\u{FF0C}"),
        "`nihao,` 应上屏「你好,」: {out:?}"
    );
    assert!(!s.is_active(), "上屏后会话应复位");
    // 「你好」是词(last_word),标点紧随之后不覆盖它
    assert_eq!(s.last_word.as_deref(), Some("你好"));
    assert_eq!(s.committed_tail, "你好\u{FF0C}");
}

#[test]
fn punct_with_no_real_candidate_commits_raw_then_fullwidth() {
    // 拼音串非空但没有真实候选:与 Space 分支同一兜底语义——
    // 上屏原始输入串(空词库下唯一候选是回声候选,词即原始串),
    // 再追加全角标点。
    //
    // 三段式(`selector` 的 `,`/`.` 翻页落地后改写):
    // - 原断言用 `.`:空词库下 `zgr` 仍有 1 个回声候选(候选窗开着),
    //   而 `.` 的翻页条件是 has_menu(候选非空)——按 librime 语义它
    //   此刻该翻页,不再上屏「zgr。」。那条断言与「`.` 有候选就翻页」
    //   的新行为正面冲突,不可能同时成立;
    // - 改为用 `;`(分号,无翻页键语义)走同一条「无真实候选 →
    //   原始串 + 全角标点」路径;`. 的翻页行为由 paging 专项测试覆盖;
    // - 为什么新行为对:librime `key_bindings.yaml` 的 `.` 就是
    //   has_menu 翻页,回声候选也是候选(用户在候选窗里看得见它),
    //   「有候选就翻页」与 librime 逐字一致;要验证的本是「无真实
    //   候选时标点先上屏原始串」,换一个不翻页的标点键,语义不变。
    let engine = Arc::new(ImeEngine::new(None).expect("内存引擎创建失败"));
    let mut s = InputSession::new(engine);
    for c in "zgr".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let out = s.handle(InputEvent::Char(';')).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "zgr\u{FF1B}"),
        "无真实候选时应上屏「原始串 + ;」: {out:?}"
    );
    assert!(!s.is_active(), "上屏后会话应复位");
    assert_eq!(s.last_word, None, "回声/原始串不是词");
}

#[test]
fn punct_table_driven_session_level() {
    // 会话层面把整张标点表跑一遍(映射本身在 punctuator.rs 单测,
    // 这里断言「中文模式空闲时,每个标点键都上屏对应全角文本」)。
    let cases: &[(char, &str)] = &[
        (',', "\u{FF0C}"),
        ('.', "\u{3002}"),
        (';', "\u{FF1B}"),
        (':', "\u{FF1A}"),
        ('?', "\u{FF1F}"),
        ('!', "\u{FF01}"),
        ('\\', "\u{3001}"),
        ('<', "\u{300A}"),
        ('>', "\u{300B}"),
        ('(', "\u{FF08}"),
        (')', "\u{FF09}"),
        ('~', "\u{FF5E}"),
        ('$', "\u{FFE5}"),
        ('_', "\u{2014}\u{2014}"),
        ('^', "\u{2026}\u{2026}"),
        ('/', "\u{3001}"),
        ('"', "\u{201C}"),
        ('[', "\u{300C}"),
        (']', "\u{300D}"),
        ('{', "\u{300E}"),
        ('}', "\u{300F}"),
        ('`', "\u{FF40}"),
    ];
    for (input, expected) in cases {
        let mut s = session_with_dict();
        let out = s.handle(InputEvent::Char(*input)).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == expected),
            "标点 {input:?} 应上屏 {expected:?},实际 {out:?}"
        );
    }
}

#[test]
fn punct_passthrough_in_english_mode() {
    // 英文模式:标点键返回 Noop——IBus 层对 Noop 返回 false,daemon
    // 把按键透传给应用,应用收到原始半角字符(键没有被吃掉)。
    let mut s = session_with_dict();
    s.handle(InputEvent::ToggleAscii).expect("切换不应失败");
    assert!(s.english_mode());
    for c in [',', '.', ';', '$', '~'] {
        let out = s.handle(InputEvent::Char(c)).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Noop),
            "英文模式下 {c:?} 应透传(Noop): {out:?}"
        );
    }
    // 切回中文模式后标点恢复全角上屏
    s.handle(InputEvent::ToggleAscii).expect("切换不应失败");
    let out = s.handle(InputEvent::Char(',')).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "\u{FF0C}"));
}

#[test]
fn apostrophe_enters_pinyin_as_syllable_separator() {
    // B2 音节分隔符落地(本轮兑现 punctuator.rs 里的预留):
    // `'` 在拼音串非空时**进拼音串**当强制切分点(`xi'an` 消歧),
    // 空闲时(拼音串为空)打 `'` 没有意义,仍透传给应用。
    //
    // 三段式(B2 落地后改写,原测试名 apostrophe_passthrough_reserved_for_b2):
    // - 原断言:「中文模式下 `'` 透传(Noop),拼音串非空时也透传」
    //   ——那是 B2 未做时的过渡行为(预留键,不冲突先透传);
    // - 改为:「空闲时 `'` 透传(Noop);拼音串非空时 `'` 进拼音串
    //   (preedit 含 `'`)」——B2 的核心语义;
    // - 为什么新行为对:librime speller 的 delimiters 就是「`'` 进
    //   输入串、abc_segmentor 按它硬切」;引擎层把 `'` 当归一化掉的
    //   强制断点(任何含 `'` 的子串都不是合法音节),`xi'an` 只有
    //   [xi, an] 一种切法——消歧成立。进拼音串(而不进词典 key、
    //   不进 `Candidate.pinyin`)的边界由引擎层归一化保证。
    let mut s = session_with_dict();
    let out = s.handle(InputEvent::Char('\'')).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Noop),
        "空闲时 ' 应透传(拼音串为空,无分隔对象): {out:?}"
    );
    // 拼音串非空时:`'` 进拼音串,preedit 为 xi'
    s.handle(InputEvent::Char('x')).expect("不应失败");
    s.handle(InputEvent::Char('i')).expect("不应失败");
    let out = s.handle(InputEvent::Char('\'')).expect("不应失败");
    match out {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "xi'", "' 应进拼音串: {u:?}");
        }
        other => panic!("拼音串非空时 ' 应进拼音串(Updated),实际 {other:?}"),
    }
}

#[test]
fn digits_still_select_candidates_not_punct() {
    // 数字键仍是选词,不被标点逻辑劫持(数字经 `translate_keyval`
    // 翻成 Select(n),会话层行为不应因标点分支的引入而变化)。
    let mut s = session_with_dict();
    for c in "nihao".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let out = s.handle(InputEvent::Select(1)).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "你好"),
        "数字 1 应选第 1 个候选「你好」: {out:?}"
    );
    // 拼音串为空时数字键照旧 Noop(无候选可选)
    let out = s.handle(InputEvent::Select(1)).expect("不应失败");
    assert!(matches!(out, SessionOutput::Noop));
}

#[test]
fn shift_letter_commits_uppercase_when_idle() {
    // 中文模式、拼音串为空:Shift+G → 上屏 `G`,不进拼音串。
    let mut s = session_with_dict();
    let out = s.handle(InputEvent::Verbatim('G')).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "G"),
        "空闲时 Shift+G 应直接上屏 G: {out:?}"
    );
    assert!(!s.is_active(), "上屏后会话应保持空闲");
    assert_eq!(s.last_word, None, "单字母不是词,last_word 应为 None");
    assert_eq!(s.committed_tail, "G", "应计入上屏尾部(供后续打分的字符窗口)");
}

#[test]
fn shift_letters_spell_gpu_across_commits() {
    // 连打 Shift+G、Shift+P、Shift+U:每次上屏一个字母,
    // committed_tail 逐次累积成 GPU(应用文本框里就是 GPU)。
    let mut s = session_with_dict();
    for (ev, expect) in [
        (InputEvent::Verbatim('G'), "G"),
        (InputEvent::Verbatim('P'), "P"),
        (InputEvent::Verbatim('U'), "U"),
    ] {
        let out = s.handle(ev).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == expect),
            "应上屏 {expect}: {out:?}"
        );
    }
    assert_eq!(s.committed_tail, "GPU", "三次上屏应累积成 GPU");
}

#[test]
fn shift_letter_mid_composition_commits_candidate_then_letter() {
    // 中文模式、拼音串非空(`nihao`):Shift+G 先按空格语义上屏
    // 高亮候选(首选整词「你好」),再追加大写字母——得到「你好G」,
    // 与 `nihao,` → 「你好,」的 punctuator 既定做法一致。
    let mut s = session_with_dict();
    for c in "nihao".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let out = s.handle(InputEvent::Verbatim('G')).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "你好G"),
        "`nihao` + Shift+G 应上屏「你好G」: {out:?}"
    );
    assert!(!s.is_active(), "上屏后会话应复位");
    // 「你好」是词(last_word),大写字母紧随其后不覆盖它(与标点一致)
    assert_eq!(s.last_word.as_deref(), Some("你好"));
    assert_eq!(s.committed_tail, "你好G");
}

#[test]
fn shift_letter_mid_composition_no_candidate_commits_raw_then_letter() {
    // 拼音串非空但没有真实候选(空词库,只有回声候选):与 Space
    // 分支同一兜底语义——上屏原始输入串,再追加大写字母。
    let engine = Arc::new(ImeEngine::new(None).expect("内存引擎创建失败"));
    let mut s = InputSession::new(engine);
    for c in "zgr".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let out = s.handle(InputEvent::Verbatim('R')).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "zgrR"),
        "无真实候选时应上屏「原始串 + R」: {out:?}"
    );
    assert!(!s.is_active());
    assert_eq!(s.last_word, None, "回声/原始串不是词");
}

#[test]
fn lowercase_letter_still_enters_pinyin() {
    // 最关键的不许退化项:小写字母仍然进拼音串(别为了修大写
    // 把正常拼音输入弄坏)。`g` 进拼音串、出候选、空格能上屏。
    let mut s = session_with_dict();
    let out = s.handle(InputEvent::Char('g')).expect("不应失败");
    match out {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "g", "小写 g 应进拼音串");
            assert!(!u.candidates.is_empty(), "应有候选(至少是回声)");
        }
        other => panic!("期望 Updated,实际 {other:?}"),
    }
    assert_eq!(s.pinyin(), "g");
    assert_eq!(s.caret(), 1);
}

#[test]
fn verbatim_does_not_learn() {
    // 大写字母上屏不是词:不进 UserLM 学习(与标点同一取舍,
    // 对应 librime 标点/原始输入不进 memory)。
    let engine = Arc::new(
        crate::ImeEngine::new(None)
            .expect("内存引擎创建失败")
            .with_user_lm(crate::UserLM::in_memory()),
    );
    let mut s = InputSession::new(Arc::clone(&engine));
    let user = engine.user_lm().expect("引擎应有用户层");
    let before = user.unigram_len();
    let out = s.handle(InputEvent::Verbatim('G')).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "G"));
    assert_eq!(
        user.unigram_len(),
        before,
        "Verbatim 单字母上屏不应产生学习"
    );
}

#[test]
fn unmapped_ascii_symbols_still_passthrough() {
    // `+` `#` `%` `&` `*` `|` 没有全角映射:
    // translate_keyval 的 0x21..=0x7e 整段放行会把它们送进
    // Char(c),会话层必须落到 catch-all 透传(Noop)——键不能被
    // 吃掉,也不能被塞进拼音串。本轮动了 Char 分支的守卫,
    // 这条测试钉住这个行为不被破坏。
    //
    // 三段式(`selector` 的 `-`/`=` 翻页落地后改写):
    // - 原断言的字符表含 `=` 与 `-`:它们「无全角映射 ⇒ catch-all
    //   透传」。本轮给它们加了「候选窗开着时翻页」(librime
    //   `paging_with_minus_equal`,has_menu)——候选窗非空时不再透传,
    //   与「透传」断言正面冲突;
    // - 改为:把 `=`/`-` 从这张表移出,它们的翻页/空闲透传行为由
    //   `minus_equal_page_when_menu_open` 专项钉住;表里只留下
    //   「无映射、无翻页语义」的其余符号,透传断言逐字不变;
    // - 为什么新行为对:librime 默认就绑了 `-`/`=` 翻页
    //   (key_bindings.yaml:27-30),它们不是「无映射透传」键而是
    //   翻页键——该测试钉的本是「无映射符号不被劫持」,把两个
    //   已被赋予翻页语义的键留在表里,断言的就不再是那个不变量。
    //
    // 三段式(recognizer 落地后再改写):
    // - 原断言的字符表还含 `@`:它「无全角映射 ⇒ 任何状态下
    //   透传」。本轮 recognizer 给 `@` 加了「拼音串非空时进串
    //   尝试邮箱识别」(librime recognizer 的 email pattern
    //   `^[A-Za-z][-_.0-9A-Za-z]*@.*$` 要求 `@` 能进输入串)——
    //   串非空时 `@` 不再透传,与「透传」断言正面冲突;
    // - 改为:把 `@` 从这张表移出,它的「空闲透传 / 串中非空时
    //   进串识别」行为由 recognized_state_char_fallthrough_when_not_recognized
    //   与 email_* 用例专项钉住;表里只留下「无映射、无识别语义、
    //   无翻页语义」的其余符号,透传断言逐字不变;
    // - 为什么新行为对:librime 的 recognizer 就排在 speller 之前、
    //   把 `@` 视为输入串的合法成分(email pattern 的成分),它
    //   不是「无映射透传」键而是「识别触发」键——该测试钉的本是
    //   「无映射符号不被劫持」,把一个已被赋予识别语义的键留在
    //   表里,断言的就不再是那个不变量。空闲时(拼音串为空)`@`
    //   照旧透传,这一半行为不变。
    let unmapped = ['+', '#', '%', '&', '*', '|'];
    for c in unmapped {
        assert!(
            fullwidth_punct(c).is_none(),
            "{c:?} 应无全角映射(有映射的是另一条路径)"
        );
        // 拼音串为空:透传
        let mut s = session_with_dict();
        let out = s.handle(InputEvent::Char(c)).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Noop),
            "空闲时 {c:?} 应透传(Noop): {out:?}"
        );
        assert!(!s.is_active(), "{c:?} 不应进拼音串");
        // 拼音串非空:同样透传,拼音串不受扰动
        let mut s = session_with_dict();
        s.handle(InputEvent::Char('n')).expect("不应失败");
        s.handle(InputEvent::Char('i')).expect("不应失败");
        let out = s.handle(InputEvent::Char(c)).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Noop),
            "拼音串非空时 {c:?} 仍应透传: {out:?}"
        );
        assert_eq!(s.pinyin(), "ni", "{c:?} 不应改动拼音串");
    }
}
