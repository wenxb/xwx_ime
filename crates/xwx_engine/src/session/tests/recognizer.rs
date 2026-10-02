//! `session` 单测:recognizer(从 `session/tests.rs` 拆出,2026-10-02)。

use super::*;

#[test]
fn email_suppresses_pinyin_candidates_and_commits_raw() {
    // 验收 A:zhangsan@gmail.com 全程(自 `@` 起)不出拼音候选,
    // 回车/空格原样上屏。
    let mut s = session_with_dict();
    // `@` 之前:正常拼音态(zhangsan 可能有候选,不断言内容,只断不识别)
    type_str(&mut s, "zhangsan");
    assert_eq!(s.recognized, None, "@ 之前不应识别");
    // `@` 落键:整串命中 email,候选窗立刻只剩原文一条
    let out = s.handle(InputEvent::Char('@')).expect("不应失败");
    match out {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "zhangsan@");
            assert_eq!(u.candidates.len(), 1, "@ 落键后候选应只剩原文: {:?}", u.candidates);
            assert_eq!(u.candidates[0].word, "zhangsan@");
        }
        other => panic!("@ 应返回 Updated,实际 {other:?}"),
    }
    // 继续敲 gmail.com:识别态保持(逐步断言见
    // continued_typing_after_recognition_does_not_oscillate)
    type_str(&mut s, "gmail.com");
    assert_recognized(&s, "zhangsan@gmail.com");
    // 回车原样上屏
    let out = s.handle(InputEvent::Enter).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "zhangsan@gmail.com"),
        "回车应原样上屏整个邮箱: {out:?}"
    );
    assert!(!s.is_active(), "上屏后复位");
    assert_eq!(s.recognized, None, "上屏后识别态清空");
}

#[test]
fn email_space_commits_raw_via_echo_candidate() {
    // 空格路径:识别态下唯一候选是原文回声,空格选中它上屏 = 原文。
    let mut s = session_with_dict();
    type_str(&mut s, "abc@x.com");
    assert_recognized(&s, "abc@x.com");
    let out = s.handle(InputEvent::Space).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "abc@x.com"),
        "空格应上屏原文(经回声候选): {out:?}"
    );
}

#[test]
fn abc_at_sign_replaces_pinyin_candidates() {
    // 「回溯」的显式断言:`abc` 三个字母已进拼音串、候选窗开着
    // (哪怕只有回声候选);`@` 出现后,候选列表里**不得再有**
    // 任何词 ≠ 原文的候选——在真实词库下「安伯尘/安保处」这类
    // 拼音候选必须消失。内存词库里 abc 切不出合法音节、只有回声
    // 候选,所以这里断的是更弱但同构的一点:候选恰一条且词随 `@`
    // 从「abc」变「abc@」——识别逻辑短路掉时,`@` 透传、候选
    // 仍是「abc」,本条即红(验证见报告第 4 节)。
    let mut s = session_with_dict();
    type_str(&mut s, "abc");
    assert_eq!(s.recognized, None);
    let before: Vec<&str> = s.candidates.iter().map(|c| c.word.as_str()).collect();
    assert_eq!(before, ["abc"], "abc 切不出音节,候选应只有回声");
    s.handle(InputEvent::Char('@')).expect("不应失败");
    assert_recognized(&s, "abc@");
}

#[test]
fn url_www_dot_not_eaten_by_paging_or_punct() {
    // 验收 A 的关键边界:`www.example.com` 里第一个 `.` 是网址的
    // 一部分,不能被翻页/标点吃掉。
    //
    // 三段式(原断言基于「`www` + `.` 落键即识别」的设计,
    // 该设计在 code review 中被自己推翻,本轮改写):
    // - 原断言:「`www` 后按 `.`,点立刻进串、preedit 为 `www.`、
    //   识别态成立」。这要求「前缀 + 空后缀」就判 url 命中;
    // - 改为:**`.` 落键时不识别、进识别态的动作推迟到 `.` 后的
    //   第一个字母**(`www.e`)。`.`,在中文模式下首先是句号/翻页
    //   键,「`www` 后用户可能只想敲个句号」的意图不该被网址
    //   识别抢走;`www.` 这个点随后由识别态的「字符直通」补进
    //   串里(识别态下任何可打印字符直接进串,`.` 不会被标点
    //   分支拦),最终上屏 `www.example.com` 一个字符不缺;
    // - 为什么新行为对:与 librime 的差异只存在于「`www.` 单独
    //   出现时」这一个落键瞬间(librime 前缀 + `.*$` 立吞,
    //   我们要求后缀非空);此后行为逐键一致。而「`.` 落键不被
    //   吃掉」这一条验收判据的本意——**最终能打出一个含点的
    //   网址**——在新行为下完整成立,见本测试末尾的上屏断言。
    //
    // 另:识别态下「`.` 直通进串」这一条,由
    // url_scheme_prefixes_recognized 的 `mailto:a@b.c` 与
    // email 全程用例(`zhangsan@gmail.com` 的第二个点)覆盖——
    // 那里 `.` 若被标点吃掉,串根本长不到目标形状。
    let mut s = session_with_dict();

    type_str(&mut s, "www");
    assert_eq!(s.recognized, None, "www 无后缀时必须是拼音(不退化)");
    assert!(!s.candidates.is_empty(), "候选窗开着(回声候选也在)");

    // `.` 落键:候选窗非空 → 既有 has_menu 翻页语义,串不变。
    // (此刻「用户要敲网址」尚无证据,`.` 保持它一贯的键义。)
    let out = s.handle(InputEvent::Char('.')).expect("不应失败");
    match out {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "www", "`.` 落键瞬间串不变(翻页语义): {u:?}");
            assert_eq!(s.recognized, None, "此刻仍不识别");
        }
        other => panic!("`.` 翻页应返回 Updated,实际 {other:?}"),
    }

    // 再敲一个字母:此刻「www.」+「e」构成 url,进入识别态——
    // 候选窗里的拼音候选**消失**、只剩一条原文回声(这是
    // recognizer 生效的标志;只断「能原样上屏」在 bug 存在时
    // 也成立,不算数)。
    let out = s.handle(InputEvent::Char('e')).expect("不应失败");
    match out {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "www.e", "e 落键后串含点: {u:?}");
            assert_eq!(u.candidates.len(), 1, "候选应只剩原文回声: {:?}", u.candidates);
            assert_eq!(u.candidates[0].word, "www.e");
            assert!(u.candidates[0].echo, "应为回声形态");
        }
        other => panic!("识别应返回 Updated,实际 {other:?}"),
    }
    // 识别态下 `.` 直通(不被标点/翻页吃掉),打完整个网址
    type_str(&mut s, "xample.com");
    assert_recognized(&s, "www.example.com");
    let out = s.handle(InputEvent::Enter).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "www.example.com"),
        "回车应原样上屏完整网址: {out:?}");
}

#[test]
fn url_scheme_prefixes_recognized() {
    // 验收 A 点名的其余前缀,**逐字符交互**(维护者裁决 2 后全部
    // 可从零打出):http(s)://、mailto:、ftp.、file:。
    // `:`/`/`/`.` 在识别态下都必须直通进串(不被标点吃掉——
    // `/` 尤其不能走 fullwidth_punct 映射成顿号)。
    for (input, why) in [
        ("http://x.com", "http: 前缀,:// 直通"),
        ("https://x.com", "https: 前缀"),
        ("mailto:a@b.c", "mailto: 前缀;串内 @ 也直通"),
        ("ftp.x.com", "ftp. 前缀(`.` 触发键补回)"),
        ("file:/x", "file: 前缀"),
    ] {
        let mut s = session_with_dict();
        type_str(&mut s, input);
        assert_recognized(&s, input);
        let out = s.handle(InputEvent::Enter).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == input),
            "{why}:回车应原样上屏 {input:?},实际 {out:?}"
        );
    }
}

#[test]
fn colon_enters_string_when_scheme_prefix() {
    // `:` 触发臂的前向判定(维护者裁决 2 的核心):`mailto` 后敲
    // `:` → `:` 进串、**不**上屏「mailto:」、候选窗里拼音候选
    // 消失(只剩一条原文回声)——这是「`:` 不被标点分支吃掉」
    // 的直接断言。初版的坏结果是「mailto 被当拼音上屏成某词 +
    // 全角冒号」,本条在该 bug 存在时必红。
    let mut s = session_with_dict();
    type_str(&mut s, "mailto");
    assert_eq!(s.recognized, None, "mailto 单独打不得识别");
    let out = s.handle(InputEvent::Char(':')).expect("不应失败");
    match out {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "mailto:", "`:` 应进串: {u:?}");
            assert!(
                u.candidates.iter().all(|c| c.echo),
                "识别态下候选应只剩回声(拼音候选消失): {:?}",
                u.candidates.iter().map(|c| &c.word).collect::<Vec<_>>()
            );
        }
        SessionOutput::Commit(t) => {
            panic!("`:` 不应触发上屏(初版 bug:上屏「拼音词+冒号」),实际 Commit({t:?})")
        }
        other => panic!("`:` 应返回 Updated,实际 {other:?}"),
    }
    // 继续打完,`@` 与 `.` 直通
    type_str(&mut s, "a@b.c");
    assert_recognized(&s, "mailto:a@b.c");
}

#[test]
fn mailto_alone_keeps_pinyin_candidates() {
    // 「mailto 单独打(后面不接 `:`)仍出拼音候选」——`:` 触发臂
    // 不许污染裸前缀。玩具词库里塞一个 mailto 切得出来的词
    // ([mai, l, to] → 「买」确认可切),断言它在、且不进识别态。
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("买", "mai", 100).expect("插入失败");
    let mut s = InputSession::new(Arc::new(engine));
    type_str(&mut s, "mailto");
    assert_eq!(s.recognized, None, "mailto 不接 `:` 不得识别");
    assert!(
        s.candidates.iter().any(|c| c.word == "买" && !c.echo),
        "mailto 应出真实拼音候选「买」: {:?}",
        s.candidates.iter().take(8).map(|c| &c.word).collect::<Vec<_>>()
    );
}

#[test]
fn bare_domain_stays_pinyin_per_ruling() {
    // 裁决 1 的既定行为钉住:`example.com`(裸域名,无显式前缀)
    // **仍走拼音路径**——`.` 落键时按既有标点/翻页语义消耗,
    // 不进串、不进识别态。这条测试防止将来有人把它当「漏了」
    // 误修。librime 的 `^[a-z]+[.].+$` 子句被有意排除,理由见
    // recognizer.rs 模块文档。
    let mut s = session_with_dict();
    type_str(&mut s, "example");
    assert_eq!(s.recognized, None);
    // `.` 落键:候选窗非空 → 翻页(has_menu),串不变、不识别
    let out = s.handle(InputEvent::Char('.')).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Updated(ref u) if u.preedit == "example"),
        "裸域名的 `.` 应按翻页消耗,串不变: {out:?}"
    );
    assert_eq!(s.recognized, None, "裸域名不得进识别态");
    // 继续敲 com:走正常拼音(字母进串)
    type_str(&mut s, "com");
    assert_eq!(s.pinyin(), "examplecom", "`.` 未进串,字母照拼");
    assert_eq!(s.recognized, None, "裸域名全程不得识别");
}

#[test]
fn recognized_samples_end_to_end_commit_identity() {
    // 端到端(维护者实测发现问题的方法,表驱动):对每个样例
    // 「逐字符 handle + 末尾 Enter,拼接所有 Commit 输出」断言
    // 等于原串。单键断言覆盖不到的「中间某键偷偷上屏了一段」
    // 在这条下无处遁形——`http://` 出「http:、、xcom」就是
    // 这样被抓到的。
    for input in [
        "zhangsan@gmail.com",
        "www.example.com",
        "http://x.com",
        "https://x.com",
        "mailto:a@b.c",
        "ftp.x.com",
        "file:/x",
    ] {
        let mut s = session_with_dict();
        let mut committed = String::new();
        for ch in input.chars() {
            if let SessionOutput::Commit(t) =
                s.handle(InputEvent::Char(ch)).expect("按键失败")
            {
                committed.push_str(&t);
            }
        }
        if let SessionOutput::Commit(t) = s.handle(InputEvent::Enter).expect("回车失败") {
            committed.push_str(&t);
        }
        assert_eq!(committed, input, "端到端上屏应等于原串 {input:?}");
    }
}

#[test]
fn continued_typing_after_recognition_does_not_oscillate() {
    // 「识别成立后继续打字不反悔」:`abc@gmail.com` 逐个字符推进,
    // 每一步都保持识别态、候选恒一条。
    let mut s = session_with_dict();
    type_str(&mut s, "abc@");
    let mut expect = String::from("abc@");
    for c in "gmail.com".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
        expect.push(c);
        assert_eq!(s.candidates.len(), 1, "第 {expect:?} 步候选应恒一条");
        assert_eq!(s.candidates[0].word, expect, "第 {expect:?} 步候选应为当前原文");
        assert!(s.recognized.is_some(), "第 {expect:?} 步应保持识别态");
    }
}

#[test]
fn backspace_out_of_recognition_restores_pinyin() {
    // 验收 A:`abc@` 删掉 `@` → 回到正常拼音态,候选恢复。
    let mut s = session_with_dict();
    type_str(&mut s, "abc@");
    assert_recognized(&s, "abc@");
    let out = s.handle(InputEvent::Backspace).expect("不应失败");
    match out {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "abc", "删掉 @ 后应回到 abc");
        }
        other => panic!("Backspace 应返回 Updated,实际 {other:?}"),
    }
    assert_eq!(s.recognized, None, "删掉 @ 后应退出识别态");
    // 候选恢复为拼音路径的结果(内存词库下 abc 只有回声候选;
    // 「恢复」的判据是候选词回到 abc 而不是 abc@)
    assert_eq!(s.candidates.len(), 1);
    assert_eq!(s.candidates[0].word, "abc", "候选应回到拼音态的回声");
    // 再删光:识别态同样清空
    s.handle(InputEvent::Backspace).expect("不应失败");
    s.handle(InputEvent::Backspace).expect("不应失败");
    s.handle(InputEvent::Backspace).expect("不应失败");
    assert!(!s.is_active());
    assert_eq!(s.recognized, None);
}

#[test]
fn backspace_mid_email_stays_recognized() {
    // 识别态下回删但整串仍命中(`zhangsan@gmail.co` 删成
    // `zhangsan@gmail.c`):保持识别态,不回拼音。
    let mut s = session_with_dict();
    type_str(&mut s, "a@b.co");
    assert_recognized(&s, "a@b.co");
    s.handle(InputEvent::Backspace).expect("不应失败");
    assert_recognized(&s, "a@b.c");
}

#[test]
fn escape_in_recognized_state_clears() {
    // 验收 A:Escape 在识别态下清空。
    let mut s = session_with_dict();
    type_str(&mut s, "abc@x");
    assert!(s.recognized.is_some(), "前置:应处于识别态");
    let out = s.handle(InputEvent::Escape).expect("不应失败");
    match out {
        SessionOutput::Updated(u) => assert!(u.preedit.is_empty(), "Escape 应清空"),
        other => panic!("Escape 应返回 Updated,实际 {other:?}"),
    }
    assert_eq!(s.recognized, None, "Escape 后识别态清空");
    assert!(!s.is_active());
}

#[test]
fn ascii_toggle_during_recognition_leaves_no_residue() {
    // 验收 A:识别态中途切英文再切回来,不残留识别态。
    let mut s = session_with_dict();
    type_str(&mut s, "abc@x");
    assert!(s.recognized.is_some());
    // 切英文:未上屏串原样上屏(toggle 的既有语义),识别态随 reset 清空
    let out = s.handle(InputEvent::ToggleAscii).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "abc@x"),
        "切英文应原样上屏未确认串: {out:?}"
    );
    assert_eq!(s.recognized, None, "切英文后识别态清空");
    assert!(s.english_mode());
    // 切回中文:重新打字,正常拼音态
    s.handle(InputEvent::ToggleAscii).expect("不应失败");
    assert!(!s.english_mode());
    type_str(&mut s, "ni");
    assert_eq!(s.recognized, None, "切回中文后不应残留识别态");
}

#[test]
fn recognized_commit_does_not_learn_and_does_not_coin() {
    // 识别态上屏的是原文,不是词:不进 UserLM 学习、不进造词缓冲
    // (借回声候选的既有语义,与 echo_candidate_commit_does_not_learn
    // 同构,但走的是「识别态回声」这条新通道)。
    let engine = Arc::new(
        crate::ImeEngine::new(None)
            .expect("内存引擎创建失败")
            .with_user_lm(crate::UserLM::in_memory()),
    );
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    let mut s = InputSession::new(Arc::clone(&engine));
    let user = engine.user_lm().expect("应有用户层");
    let before_uni = user.unigram_len();

    type_str(&mut s, "a@b.c");
    assert!(s.recognized.is_some());
    let out = s.handle(InputEvent::Space).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "a@b.c"));
    assert_eq!(user.unigram_len(), before_uni, "识别态上屏不应学习");
    assert_eq!(s.last_word, None, "原文不是词");
    // 紧接着选一次真实候选词,不应与刚才的原文粘成造词
    type_str(&mut s, "ni");
    let out = s.handle(InputEvent::Space).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "你"));
    s.break_coin_run();
    assert_eq!(user.entry_len(), 0, "识别态原文不应参与造词");
}

#[test]
fn unrecognized_inputs_keep_pinyin_candidates() {
    // 不退化:「像前缀但没敲触发字符」的串仍走正常拼音。
    // `www`(缺 `.` + 后缀)不得识别;真实词库下「万维网/呜呜呜」
    // 的召回由 probe 对拍基线保证(玩具词库里 w 不是声母、
    // `www` 切不出音节,只有回声候选——判据落在「不进识别态 +
    // 候选走查词典通道」)。
    let mut s = session_with_dict();
    type_str(&mut s, "www");
    assert_eq!(s.recognized, None, "www 单独打不得识别");
    assert!(
        s.candidates.iter().all(|c| c.echo),
        "玩具词库下 www 只应有回声候选(识别态未劫持): {:?}",
        s.candidates.iter().map(|c| &c.word).collect::<Vec<_>>()
    );

    // 同构的一条:「能切出真实拼音候选」的串,识别逻辑不许碰——
    // `nini`(切得出 [ni,ni])在玩具词库里「你」应在;若
    // recognizer 误判这类串,候选会被一条原文回声换掉。
    let mut s2 = session_with_dict();
    type_str(&mut s2, "nini");
    assert_eq!(s2.recognized, None, "nini 不得识别");
    assert!(
        s2.candidates.iter().any(|c| c.word == "你" && !c.echo),
        "nini 应出真实拼音候选「你」: {:?}",
        s2.candidates.iter().take(5).map(|c| &c.word).collect::<Vec<_>>()
    );
}

#[test]
fn caret_editing_in_recognized_state() {
    // 识别态与 B1 可编辑缓冲共存:caret 不在串尾时插入字符、
    // Backspace 删 caret 前字符,重判基于完整新串。
    let mut s = session_with_dict();
    type_str(&mut s, "a@b.com");
    assert_recognized(&s, "a@b.com");
    // caret 移到串首后一位,插入 x → ax@b.com,仍是 email
    for _ in 0..6 {
        s.handle(InputEvent::Left).expect("不应失败");
    }
    assert_eq!(s.caret(), 1);
    s.handle(InputEvent::Char('x')).expect("不应失败");
    assert_recognized(&s, "ax@b.com");
    assert_eq!(s.caret(), 2);
    // caret 处 Backspace 删掉 @ → axb.com 不再命中,退回拼音
    let mut s = session_with_dict();
    type_str(&mut s, "a@b");
    // caret 在 3(串尾),左移 1 到 2(@ 后),Backspace 删 @
    s.handle(InputEvent::Left).expect("不应失败");
    assert_eq!(s.caret(), 2);
    s.handle(InputEvent::Backspace).expect("不应失败");
    assert_eq!(s.pinyin(), "ab");
    assert_eq!(s.recognized, None, "删掉 @ 应退出识别态");
}

#[test]
fn recognized_state_char_fallthrough_when_not_recognized() {
    // 防御:`@` 在「不可能识别」的位置(串首)仍透传,不被吃掉;
    // 数字串 + @ 不识别(首字符非字母,但数字根本进不了串——
    // 这条只测串首 @)。
    let mut s = session_with_dict();
    let out = s.handle(InputEvent::Char('@')).expect("不应失败");
    assert!(matches!(out, SessionOutput::Noop), "空闲时 @ 应透传");
    assert!(!s.is_active());
}

/// 识别态下**数字**必须按原文进串,不能被当成「选第 N 个候选」(codex 审查 D7)。
///
/// `http://x1.com` 的 `1` 在 ibus 层被折成 `Select(1)`;旧实现接不住,
/// 于是它选中那条回声候选 → **地址被提前提交**。
#[test]
fn digit_inside_a_recognized_url_extends_the_text_instead_of_selecting() {
    let mut s = session_with_dict();
    for c in "http://x".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    assert!(s.recognized.is_some(), "`http://x` 应已进识别态");
    let out = s.handle(InputEvent::Select(1)).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Updated(_)),
        "识别态下数字只该延长原文,实际 {out:?}"
    );
    assert_eq!(s.pinyin(), "http://x1", "数字要进串");
    let out = s.handle(InputEvent::Select(0)).expect("不应失败");
    assert!(matches!(out, SessionOutput::Updated(_)), "`0` 同样是原文字符");
    assert_eq!(s.pinyin(), "http://x10");
    // 继续打字仍然在识别态(没被提前提交)
    for c in ".com".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    assert_eq!(s.pinyin(), "http://x10.com");
    assert!(s.recognized.is_some());
}

/// 识别态下**大写字母**必须按原文进串,不能结束组合(codex 审查 D7)。
#[test]
fn capital_letter_inside_a_recognized_url_extends_the_text() {
    let mut s = session_with_dict();
    for c in "http://x/".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    assert!(s.recognized.is_some(), "`http://x/` 应已进识别态");
    let out = s.handle(InputEvent::Verbatim('A')).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Updated(_)),
        "识别态下大写只该延长原文,实际 {out:?}"
    );
    assert_eq!(s.pinyin(), "http://x/A", "大写及其大小写都要保留");
}

/// 非识别态行为不变:数字仍是数字选词,大写仍结束组合(codex 审查 D7 的回归线)。
#[test]
fn digit_select_and_capital_keep_their_normal_meaning_while_composing() {
    let mut s = session_with_dict();
    for c in "nihao".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    assert!(s.recognized.is_none());
    let out = s.handle(InputEvent::Select(1)).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "你好"),
        "普通拼音态 `1` 仍是选第 1 个候选: {out:?}"
    );

    let mut s = session_with_dict();
    for c in "nihao".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let out = s.handle(InputEvent::Verbatim('G')).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "你好G"),
        "普通拼音态大写仍是「上屏候选 + 大写字母」: {out:?}"
    );
}
