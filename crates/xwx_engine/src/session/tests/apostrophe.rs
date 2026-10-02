//! `'` 强制切分点(B2 音节分隔符)的语义与**原文保真**。
//!
//! 本模块从 `paging_delete.rs` 收拢了四条既有用例(`'` 的消歧、不进 key、
//! 可编辑缓冲、边界输入不 panic),并按 codex 审查 D9 补上「分隔符必须同样
//! 约束**兜底**路径」与「回声/回车只能回**原文**」两组用例:
//!
//! - 旧兜底把清洗串(去掉 `'`)当普通串重新切分,`xi'anq` 因此切出跨界的
//!   `xian` → 召回「先/现/显/线」,`'` 在整串解不出时形同不存在;
//! - 旧回声用的是清洗串,`zz'zz` 上屏成 `zzzz`——用户敲的分隔符被吞掉。
//!
//! 判据只此一条(与格构建、beam 逐字相同):**分隔符不得落在音节内部**;
//! 落在音节边界上正是用户要的切分。

use super::*;

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

// ——— codex 审查 D9:分隔符必须同样约束**兜底**,回声只能来自原文 ———

/// 兜底路径不得召回跨分隔符的单音节读法。
///
/// `xi'anq`:整串解不出(尾巴 `q` 不是音节)→ 走兜底。旧兜底在**清洗串**
/// `xianq` 上找最长可切分前缀,切出 `xian`——那正好穿过位置 2 的分隔符,
/// 于是「先/现/显/线」这一整族被灌回候选(复现:`xi'anq` 前 12 条全是
/// `pinyin=xian` 的字),用户敲的 `'` 完全失去意义。
///
/// 新判据:切法里任何音节都不得穿过分隔符(与格构建同一条
/// `syllable_crosses_boundary`);于是最长可切分前缀退到 `xian`
/// 的边界内读法 [xi | an] → 西安,尾巴 `q` 留给分段上屏收尾。
///
/// 旧实现必红:旧实现会给出 `pinyin == "xian"` 的「先」。
#[test]
fn fallback_never_crosses_the_separator() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("西安", "xi an", 100).expect("插入失败");
    // 「先」故意给更高词频:旧实现会把它顶到第 1 名,更容易观测
    engine.dict.upsert("先", "xian", 900).expect("插入失败");
    let cands = engine
        .lookup_candidates("xi'anq", &crate::LmContext::empty())
        .expect("查询失败");
    let shown: Vec<(&str, &str)> = cands
        .iter()
        .take(8)
        .map(|c| (c.word.as_str(), c.pinyin.as_str()))
        .collect();
    assert!(
        !cands.iter().any(|c| c.pinyin == "xian"),
        "兜底不得召回跨分隔符的单音节读法「先」: {shown:?}"
    );
    assert!(
        cands.iter().any(|c| c.word == "西安"),
        "应给出边界内的读法(西安 = xi an): {shown:?}"
    );
    let xian = cands.iter().find(|c| c.word == "西安").expect("应有西安");
    assert_eq!(
        xian.covered, 5,
        "覆盖范围按**原文**算:`xi'an` 是 5 字节(旧实现按清洗串给 4)"
    );
}

/// 泛化判据:任何候选的拼音 key 都不得穿过输入里的分隔符。
///
/// 输入覆盖四类边界(合法前缀 + 错尾、前导、尾随、连续),候选覆盖
/// 格边层与兜底层。回声候选不在此列——它的 key 就是原文本身(见下一条
/// 用例),含 `'` 是**对的**。
#[test]
fn every_pinyin_candidate_respects_the_separator() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("西安", "xi an", 100).expect("插入失败");
    engine.dict.upsert("先", "xian", 900).expect("插入失败");
    engine.dict.upsert("岸", "an", 80).expect("插入失败");
    engine.dict.upsert("洗", "xi", 70).expect("插入失败");

    for input in ["xi'an", "xi'anq", "'xian", "xian'", "xi''an", "xi'a"] {
        // 分隔符在**清洗串**(去掉 `'`)里的字节位置——key 的坐标系是清洗串
        let marks: Vec<usize> = input
            .char_indices()
            .filter(|(_, c)| *c == '\'')
            .map(|(i, _)| input[..i].chars().filter(|c| *c != '\'').count())
            .collect();
        let cands = engine
            .lookup_candidates(input, &crate::LmContext::empty())
            .unwrap_or_else(|e| panic!("{input} 查询失败: {e}"));
        for c in cands.iter().filter(|c| !c.echo) {
            // key 的音节边界(字节偏移),含起点 0
            let mut pos = 0usize;
            let mut edges = vec![0usize];
            for syl in c.pinyin.split(' ') {
                pos += syl.len();
                edges.push(pos);
            }
            for m in &marks {
                if *m >= pos {
                    continue; // 分隔符落在 key 覆盖范围之外(前缀候选)——无关
                }
                assert!(
                    edges.contains(m),
                    "{input} 的候选 {} (key {:?}) 穿过了位置 {m} 的分隔符;\
                     音节边界在 {edges:?}",
                    c.word,
                    c.pinyin
                );
            }
        }
    }
}

/// 回声候选的文本只能来自**原文**:`zz'zz` 上屏必须是 `zz'zz`。
///
/// 旧实现把清洗串喂给回声候选,用户敲的 `'` 被静默吞掉——上屏成 `zzzz`。
/// 会话层两条出口一起钉:空格(选回声候选)与回车(原样上屏)。
///
/// 旧实现必红:回声 word 是 `zzzz`。
#[test]
fn echo_and_enter_keep_the_raw_separator_input() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    let cands = engine
        .lookup_candidates("zz'zz", &crate::LmContext::empty())
        .expect("查询失败");
    assert_eq!(cands.len(), 1, "无真实候选时应只剩回声: {cands:?}");
    assert!(cands[0].echo, "应是回声候选: {cands:?}");
    assert_eq!(cands[0].word, "zz'zz", "回声文本必须来自原文(不是清洗串 zzzz)");

    // 空格选回声候选
    let mut s = InputSession::new(Arc::new(ImeEngine::new(None).expect("内存引擎创建失败")));
    for c in "zz'zz".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let out = s.handle(InputEvent::Space).expect("不应失败");
    assert!(
        matches!(&out, SessionOutput::Commit(t) if t == "zz'zz"),
        "空格选回声应上屏原文: {out:?}"
    );

    // 回车原样上屏
    let mut s = InputSession::new(Arc::new(ImeEngine::new(None).expect("内存引擎创建失败")));
    for c in "zz'zz".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let out = s.handle(InputEvent::Enter).expect("不应失败");
    assert!(
        matches!(&out, SessionOutput::Commit(t) if t == "zz'zz"),
        "回车应原样上屏: {out:?}"
    );
}

/// 兜底读法 + 分段上屏:空格吃掉边界内那一段(西安),尾巴 `q` 留在组合里。
///
/// 这条把 D9 与「方案 b(确认段留在组合里)」连起来看:覆盖范围必须是
/// **原文**字节数,否则 `'` 会被算进剩余串(上屏「西安」后 preedit 剩
/// `'q`)或者把尾巴一起吃掉。
///
/// 旧实现必红:第一次空格吃掉的是跨界的「先」,preedit 变成 `先q`。
#[test]
fn separator_segment_then_the_rest_stays_editable() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("西安", "xi an", 100).expect("插入失败");
    engine.dict.upsert("先", "xian", 900).expect("插入失败");
    let mut s = InputSession::new(Arc::new(engine));
    for c in "xi'anq".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let out = s.handle(InputEvent::Space).expect("不应失败");
    match out {
        SessionOutput::Updated(u) => {
            assert_eq!(
                u.preedit, "西安q",
                "空格应确认「西安」(覆盖原文 xi'an)并把 q 留在组合里;实际 {u:?}"
            );
        }
        other => panic!("只覆盖前缀时应返回 Updated(段留在组合里),实际 {other:?}"),
    }
}
