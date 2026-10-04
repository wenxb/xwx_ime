//! crate 级单测:lookup。

use super::*;

#[test]
fn lookup_merges_and_ranks() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你好", "ni hao", 10).expect("插入失败");
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    engine.dict.upsert("逆", "ni", 50).expect("插入失败");

    let cands = engine
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");
    assert_eq!(cands.first().map(|c| c.word.as_str()), Some("你"));
    assert!(cands.iter().any(|c| c.word == "逆"));
}

#[test]
fn fuzzy_expansion_reaches_dict() {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("里", "li", 100).expect("插入失败");
    engine.fuzzy_enabled = true;

    // 输入 ni（n/l 混淆），应能查到 li -> 里
    let cands = engine
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");
    assert!(cands.iter().any(|c| c.word == "里"));
}

#[test]
fn mid_syllable_keeps_candidates() {
    // 打字打到一半(如 "nih"):整串还切不完整,但候选不能消失——
    // 应按"已切分前缀"继续给候选(对应候选窗闪一下就没的 bug)。
    // 残余字母("nih" 切出 "ni" 后剩下的 "h")不再当整音节做前缀联想
    // (旧行为会把所有 h 开头的高频词灌进候选,见 fallback_candidates
    // 函数级文档);"ni + h 开头音节"的词由简拼层 lookup_mixed 负责
    // (玩具词库无副索引,这里只验证兜底层不再产出 h 开头的词)。
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    engine.dict.upsert("尼", "ni", 50).expect("插入失败");
    engine.dict.upsert("哈", "ha", 40).expect("插入失败");

    let cands = engine
        .lookup_candidates("nih", &LmContext::empty())
        .expect("查询失败");
    assert!(!cands.is_empty(), "nih 不应没有候选");
    assert!(
        cands.iter().any(|c| c.word == "你"),
        "应包含最长可切前缀 ni 的候选"
    );
    assert!(
        cands.iter().all(|c| c.word != "哈"),
        "残余字母 h 不应再当整音节联想出「哈」(它只解释输入尾部 1 个字母): {:?}",
        cands.iter().map(|c| &c.word).collect::<Vec<_>>()
    );
}

#[test]
fn first_syllable_incomplete_uses_prefix() {
    // 首音节都还没切出来（如 "z"、"zhon"）：整串前缀联想
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("中", "zhong", 100).expect("插入失败");
    engine.dict.upsert("在", "zai", 90).expect("插入失败");

    for partial in ["z", "zhon"] {
        let cands = engine
            .lookup_candidates(partial, &LmContext::empty())
            .expect("查询失败");
        assert!(!cands.is_empty(), "{partial} 不应没有候选");
        assert!(
            cands.iter().any(|c| c.word == "中"),
            "{partial} 前缀联想应包含 中"
        );
    }
}

#[test]
fn complete_pinyin_untouched_by_fallback() {
    // 完整可切分时兜底不应抢戏：候选只来自整串相关查询
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    engine.dict.upsert("你好", "ni hao", 200).expect("插入失败");

    let cands = engine
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");
    assert!(cands.iter().any(|c| c.word == "你"));
    assert!(
        cands.iter().all(|c| c.pinyin == "ni"),
        "完整切分不应混入其它音节候选: {:?}",
        cands
    );
}

/// 覆盖范围(分段上屏的地基):候选只覆盖输入串开头**它自己那一段**,
/// 而不是整串。
///
/// 没有这个字段,会话层就不知道上屏之后该留下哪些字母,只能整串复位
/// ——那正是改动前「一长串拼音里选了一段,后面全被静默丢掉」的原因。
#[test]
fn candidate_covered_spans_only_the_input_it_matches() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("我们", "wo men", 300).expect("插入失败");
    engine.dict.upsert("我", "wo", 500).expect("插入失败");
    // 必须有**整串可解**的词条,否则查询会落到兜底路径(只出「我」),
    // 而本测试要钉的是格边候选的覆盖范围。
    engine.dict.upsert("今天", "jin tian", 300).expect("插入失败");

    let input = "womenjintian";
    let cands = engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败");
    let wo_men = cands.iter().find(|c| c.word == "我们").expect("应含「我们」");
    assert_eq!(wo_men.covered, 5, "「我们」只覆盖 wo men(5 字节)");
    let wo = cands.iter().find(|c| c.word == "我").expect("应含「我」");
    assert_eq!(wo.covered, 2, "「我」只覆盖 wo");
    // 位置 0 上的候选不可能覆盖超过整串,也不可能覆盖 0 字节
    // (0 的语义是「未知/整串」,留给没标注的来源)。
    for c in &cands {
        assert!(
            c.covered > 0 && c.covered <= input.len(),
            "覆盖范围必须落在 (0, 串长] 内:{:?} covered={}",
            c.word,
            c.covered
        );
    }
}

/// 兜底来源的覆盖范围:`zzzz` 的回声候选整串上屏;`nihaozzzzz` 的前缀
/// 候选只覆盖它能解释的那一段(词 = 整个可切分前缀,首音节词 = 首音节)。
///
/// 这三条口径缺一条,分段上屏就会「吃多」或「吃少」:吃多会丢掉用户
/// 还没解释的字母,吃少会让剩余串永远缩不短。
#[test]
fn fallback_and_echo_candidates_carry_their_own_coverage() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    let echo = engine
        .lookup_candidates("zzzz", &LmContext::empty())
        .expect("查询失败");
    assert_eq!(echo.len(), 1, "纯乱码只应有一个回声候选");
    assert!(echo[0].echo);
    assert_eq!(echo[0].covered, 4, "回声候选=原样上屏整串");

    engine.dict.upsert("你好", "ni hao", 100).expect("插入失败");
    engine.dict.upsert("你", "ni", 200).expect("插入失败");
    let cands = engine
        .lookup_candidates("nihaozzzzz", &LmContext::empty())
        .expect("查询失败");
    let ni_hao = cands.iter().find(|c| c.word == "你好").expect("应含「你好」");
    assert_eq!(ni_hao.covered, 5, "「你好」覆盖最长可切分前缀 ni hao");
    let ni = cands.iter().find(|c| c.word == "你").expect("应含「你」");
    assert_eq!(ni.covered, 2, "「你」只覆盖首音节 ni");
    assert!(
        cands
            .iter()
            .filter(|c| !c.echo)
            .all(|c| c.covered < "nihaozzzzz".len()),
        "真实候选都不该声称吃掉了整串(zzzzz 还没被解释)"
    );
    // 第 ③ 期「剩余串的出路」:解不完整时补一条原样上屏的逃生口,
    // 它**就是**覆盖整串的那一个(与回声候选同源:不学词、不动上文)。
    let escape = cands
        .iter()
        .find(|c| c.echo)
        .unwrap_or_else(|| panic!("应有原样上屏的逃生口: {cands:?}"));
    assert_eq!(escape.word, "nihaozzzzz");
    assert_eq!(escape.covered, "nihaozzzzz".len());
}

/// `'` 占原文一个字节:覆盖停在分隔符前的候选必须把那个 `'` 一起吃掉,
/// 否则上屏「西安」之后 preedit 会剩下 `'an`——一个既无意义又会被用户
/// 当成多余字符的前导分隔符。
#[test]
fn covered_swallows_the_apostrophe_separator() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    if !path.is_file() {
        return;
    }
    let engine = ImeEngine::new(Some(&path)).expect("mmap 词库加载失败");
    let input = "xi'anha";
    let cands = engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败");
    let xian = cands
        .iter()
        .find(|c| c.word == "西安")
        .unwrap_or_else(|| {
            panic!(
                "xi'anha 应含「西安」: 前 10 {:?}",
                cands.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()
            )
        });
    assert_eq!(
        xian.covered, 5,
        "干净串 xian(4 字节)+ 它后面那个 `'` = 5 字节"
    );
    assert!(xian.covered < input.len(), "剩余 ha 要留给会话继续组合");
}

/// 真实词库下的长串:首选「我们今天」只覆盖 13 字节,剩下的
/// `qugongyuan` 必须留给会话(这是生产里「选完接着后面的」的样子)。
#[test]
fn real_dict_long_input_candidates_cover_only_their_own_span() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    if !path.is_file() {
        return;
    }
    let engine = ImeEngine::new(Some(&path)).expect("mmap 词库加载失败");
    let input = "womenjintianqugongyuan";
    let cands = engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败");
    assert!(!cands.is_empty(), "长串应有候选");
    // **首位是覆盖整串的整句读法**（上游 `script_translator.cc:497-508` 生成整句、
    // `PrepareCandidate` :524-531 放进流内首位）。旧实现断言「首选只覆盖一段」，
    // 那正是「整串打完首屏只有一个词/一个字」的来源。
    let top = &cands[0];
    assert_eq!(
        top.covered,
        input.len(),
        "首位应是覆盖整串的读法,实际 {:?} covered={},前 5: {:?}",
        top.word,
        top.covered,
        cands.iter().take(5).map(|c| &c.word).collect::<Vec<_>>()
    );
    assert!(
        cands.iter().any(|c| c.covered < input.len()),
        "前缀候选仍要在(分段上屏靠它们),实际前 5: {:?}",
        cands.iter().take(5).map(|c| (&c.word, c.covered)).collect::<Vec<_>>()
    );
}

/// 回声候选:词库里查不到的输入(整串无法解码、兜底联想也为空),
/// `lookup_candidates` 应返回**恰好 1 个**回声候选,word == 输入串,
/// echo == true——保证公开 API 的后置条件「非空输入 ⇒ 候选非空」。
#[test]
fn unresolvable_input_yields_single_echo_candidate() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    // 空词库:任何输入都解不出候选
    let cands = engine
        .lookup_candidates("zgr", &LmContext::empty())
        .expect("查询失败");
    assert_eq!(cands.len(), 1, "查不到时应恰好 1 个回声候选: {cands:?}");
    assert_eq!(cands[0].word, "zgr", "回声候选的 word 应为原始输入串");
    assert_eq!(cands[0].pinyin, "zgr");
    assert!(cands[0].echo, "应标记为回声候选");
    assert_eq!(cands[0].freq, 0);
}

/// 回声候选绝不污染正常候选列表:词库里有的词,结果里不应有
/// 任何 echo == true 的候选(语义同 librime:别人有候选时回声不冒头)。
#[test]
fn resolvable_input_has_no_echo_candidate() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    engine.dict.upsert("你好", "ni hao", 200).expect("插入失败");

    let cands = engine
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");
    assert!(!cands.is_empty(), "词库命中时应有候选");
    assert!(
        cands.iter().all(|c| !c.echo),
        "正常有候选时不应混入回声候选: {cands:?}"
    );
}

/// Block A1:候选必须全部起始于位置 0——只覆盖后段音节的词
/// 不该出现在候选里(用户无法有意义地选中它们)。
///
/// 构造:输入 `nihao`,词库有「好」(hao,仅覆盖第二音节)。新语义
/// 下它不应出现;「你好」(覆盖整串)与「你」(覆盖首音节)应出现。
/// 对照 librime:同输入的候选全部起始于位置 0。
#[test]
fn candidates_all_start_at_position_zero() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你好", "ni hao", 100).expect("插入失败");
    engine.dict.upsert("你", "ni", 200).expect("插入失败");
    engine.dict.upsert("好", "hao", 150).expect("插入失败");
    engine.dict.upsert("号", "hao", 120).expect("插入失败");
    engine.dict.upsert("毫", "hao", 80).expect("插入失败");

    let cands = engine
        .lookup_candidates("nihao", &LmContext::empty())
        .expect("查询失败");
    assert!(
        cands.iter().any(|c| c.word == "你好"),
        "覆盖整串的「你好」应在候选里: {cands:?}"
    );
    assert!(
        cands.iter().any(|c| c.word == "你"),
        "覆盖首音节的「你」应在候选里: {cands:?}"
    );
    for w in ["好", "号", "毫"] {
        assert!(
            !cands.iter().any(|c| c.word == w),
            "只覆盖后段音节 hao 的「{w}」不应出现在 nihao 的候选里: {cands:?}"
        );
    }
    // 不变量:所有候选的拼音都必须以首音节 ni 开头
    // (即对应 lattice[0] 上的边)。
    assert!(
        cands
            .iter()
            .all(|c| c.pinyin == "ni" || c.pinyin.starts_with("ni ")),
        "所有候选都应起始于位置 0(拼音以 ni 开头): {cands:?}"
    );
}

/// Block A1 不变量在**兜底路径**同样成立(补漏回归):整串尚不能
/// 完整解码时(如 `zhongguor`、`qiax`),`fallback_candidates` 曾经
/// 遍历每种切法的**所有**音节查词,把只覆盖非起始音节的词
/// (`zhongguor` 切出 `[zhong,gu,o]` 时 `gu` 的「古」、`o` 的「哦」)
/// 灌进候选——与 A1 在格路径修掉的缺陷同源。
///
/// 判定逻辑(不硬编码字表):候选拼音(空格分隔音节串)的**首音节**
/// 必须属于「最长可完整切分前缀」的某种切法的首音节集合——即存在
/// 一种从位置 0 出发的音节切法以它开头。这是「候选起始于位置 0」
/// 的直接判据:首音节不从位置 0 起头的词(过 guo / 古 gu / 哦 o)
/// 不可能由任何首段切法产出;而首音节不同的多条切法
/// (`qiax` 的 `[qi,a]` 与 `[qia]`)各自的首音节都合法。
#[test]
fn fallback_candidates_all_start_at_position_zero() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let dict_path = root.join("dict.bin");
    let abbrev_path = root.join("abbrev.bin");
    if !dict_path.is_file() {
        return; // 环境缺词库:跳过
    }
    let mut engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
    if abbrev_path.is_file() {
        engine.dict = engine.dict.with_abbrev(&abbrev_path);
    }

    // 各输入的最长可切前缀切法由引擎同款 Segmenter 现算,不硬编码:
    // zhongguor → 前缀 zhongguo:[zhong,gu,o] / [zhong,guo]
    // qiax      → 前缀 qia     :[qi,a] / [qia]
    let expected: [(&str, &[&str]); 2] = [
        ("zhongguor", &["zhong"]),
        ("qiax", &["qi", "qia"]),
    ];
    for (input, want_firsts) in expected {
        let k = (1..=input.len())
            .rev()
            .find(|&k| !engine.segmenter.segment(&input[..k]).is_empty())
            .expect("这两个输入都应有可切前缀");
        let allowed_firsts: std::collections::HashSet<String> = engine
            .segmenter
            .segment(&input[..k])
            .iter()
            .filter_map(|seg| seg.first().cloned())
            .collect();
        let want: std::collections::HashSet<String> =
            want_firsts.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            allowed_firsts, want,
            "{input} 的首音节集合(判据的前提)与预期不符"
        );

        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        assert!(!cands.is_empty(), "{input} 不应没有候选");
        for c in &cands {
            if c.echo
                || c.spelling == SpellingKind::Abbrev
                || c.spelling == SpellingKind::Fuzzy
            {
                // 三类豁免:
                // - 回声候选 word=输入串,无音节结构;
                // - 简拼候选覆盖整串、其 pinyin 是完整词拼音,天然从位置 0 起;
                // - 模糊/容错候选(2026-10-03)**故意**改掉一个字母
                //   (`qiax` → 前 qian),首音节不再等于输入前缀——这是它的
                //   职责,不是 A1 兜底路径回归。本测试钉的是精确层的性质。
                continue;
            }
            let first_syl = c.pinyin.split(' ').next().expect("候选拼音非空");
            assert!(
                allowed_firsts.contains(first_syl),
                "{input} 的候选「{}」(拼音 {})首音节 {first_syl} 不在合法首音节集合                      {allowed_firsts:?} 里——它只覆盖非起始音节(A1 兜底路径回归)",
                c.word, c.pinyin
            );
        }
    }
}

/// Block A2:候选数不再被 `BEAM_WIDTH` 钳死——候选来源是
/// `lattice[0]` 全量,而非 beam 的幸存路径。
///
/// 构造:单音节 `ni` 的词库里放 20 个同音字。旧实现最多返回
/// `BEAM_WIDTH = 16` 个;新实现应返回全部 20 个。
#[test]
fn candidate_count_not_clamped_by_beam_width() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    // 20 个不同的同音字,词频递减以保证确定性排序
    let words: Vec<String> = (0..20u32)
        .map(|i| char::from_u32(0x4E00 + i).unwrap().to_string())
        .collect();
    for (i, w) in words.iter().enumerate() {
        engine
            .dict
            .upsert(w, "ni", 1000 - i as u64)
            .expect("插入失败");
    }

    let cands = engine
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");
    assert!(
        cands.len() > BEAM_WIDTH,
        "候选数 {} 应大于 BEAM_WIDTH={BEAM_WIDTH}(不再被 beam 钳死)",
        cands.len()
    );
    assert_eq!(cands.len(), 20, "20 个同音字应全部成为候选: {cands:?}");
}

/// Block A3:打分口径——「覆盖整串的词」应排在「只覆盖前缀的词」
/// 之前,即使后者自身词频更高(覆盖整串只需一次词频+LM 打分,
/// 前缀词还需补全剩余音节,多付一次词频+LM 代价)。
///
/// 构造:输入 `nihao`,「你」(ni, freq=200)自身词频高于「你好」
/// (ni hao, freq=100),但「你好」覆盖整串,「你」还需补全 hao,
/// 故「你好」总分应更高、排在前面。
#[test]
fn full_coverage_word_outranks_prefix_word() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你", "ni", 200).expect("插入失败");
    engine.dict.upsert("好", "hao", 150).expect("插入失败");
    engine.dict.upsert("你好", "ni hao", 100).expect("插入失败");

    let cands = engine
        .lookup_candidates("nihao", &LmContext::empty())
        .expect("查询失败");
    let pos = |word: &str| cands.iter().position(|c| c.word == word);
    let (p_full, p_prefix) = (
        pos("你好").expect("「你好」应在候选里"),
        pos("你").expect("「你」应在候选里"),
    );
    assert!(
        p_full < p_prefix,
        "覆盖整串的「你好」应排在只覆盖前缀的「你」之前: {cands:?}"
    );
    assert!(
        cands[p_full].score > cands[p_prefix].score,
        "「你好」分 {} 应高于「你」分 {}",
        cands[p_full].score,
        cands[p_prefix].score
    );
}

/// 容错解码(2026-10-03):**单音节里打错一个字母,不再让整串作废**。
///
/// 用玩具词库而不是真实 `dict.bin`:容错的机制(音节级索引 + 容错格 +
/// 分层)与词库大小无关,而玩具词库的期望值是确定的——真实词库里同一
/// 输入会有几十条长尾候选,断言容易写成「恰好第 1 名是某词」那种脆弱的
/// 形式。真实词库上的实测数据随交付记录归档(git 历史可查)。
#[test]
fn typo_pinyin_still_yields_the_intended_word() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你好", "ni hao", 900).expect("插入失败");
    engine.dict.upsert("你", "ni", 500).expect("插入失败");
    engine.dict.upsert("什么", "shen me", 800).expect("插入失败");
    // 中间音节的词条:`shenem` 的错处定位是**词级**的(精确格走到
    // 「shen」之后再无出边),所以词库里必须有「深」这类 shen 词条——
    // 真实词库永远有,玩具词库要显式建出来。
    engine.dict.upsert("深", "shen", 400).expect("插入失败");
    // 四类编辑各一条代表 + 一条「少打一个字母」的反面样本(它不属于本
    // 阶段能力,历史已知缺口)。
    for (input, want) in [
        ("nihoa", "你好"),  // 调换相邻字母(o/a)
        ("niqhao", "你好"), // 中间多打一个字母
        ("qnihao", "你好"), // 开头多打一个字母
        ("shenem", "什么"), // 调换(m/e),CHIME 的经典例子
        ("nihaoo", "你好"), // 重复字母(对照组:本来就该出)
    ] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        assert!(
            cands.iter().any(|c| c.word == want),
            "{input} 的候选里应有「{want}」(容错解码回归): {:?}",
            cands.iter().take(8).map(|c| &c.word).collect::<Vec<_>>()
        );
    }
}

/// 容错只在**精确解不出整串**时启用:打对的输入不出现模糊/容错层候选。
///
/// 这是「不退化」的结构性保证(第一个门槛),不依赖测量:整串可切且精确
/// 路能解出完整路径时,容错格根本不会被构建(模糊音默认关闭,所以这些
/// 输入不该有任何模糊层候选)。
#[test]
fn tolerant_pass_leaves_exactly_typed_inputs_alone() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let dict = root.join("dict.bin");
    if !dict.is_file() {
        return;
    }
    let mut engine = ImeEngine::new(Some(&dict)).expect("mmap 词库加载失败");
    let abbrev = root.join("abbrev.bin");
    if abbrev.is_file() {
        engine.dict = engine.dict.with_abbrev(&abbrev);
    }
    for input in ["nihao", "womenjintianqugongyuan", "zhongguor", "wsm", "zgr", "nih"] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        assert!(!cands.is_empty(), "{input} 应有候选");
        assert!(
            cands.iter().all(|c| c.spelling != SpellingKind::Fuzzy),
            "{input}: 打对的输入不该出现模糊/容错层候选: {:?}",
            cands.iter().take(8).map(|c| (&c.word, c.spelling)).collect::<Vec<_>>()
        );
    }
}

/// 公开查询入口必须挡住非拼音文本:
/// 中文/emoji/混合串过去会直接进按字节切片的排序代码并 panic。
/// 非 ASCII 一律返回**原文回声**(原样上屏的既有出口),绝不 panic。
#[test]
fn non_ascii_input_returns_an_echo_instead_of_panicking() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    for input in ["你", "你好", "🎉", "a中b", "ni你hao", "\u{0301}"] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .unwrap_or_else(|e| panic!("{input:?} 不该报错: {e}"));
        assert!(
            cands.iter().any(|c| c.echo && c.word == input),
            "{input:?} 应返回原文回声,实际: {:?}",
            cands.iter().map(|c| (&c.word, c.echo)).collect::<Vec<_>>()
        );
    }
    // 空串仍是空(既有契约)。
    assert!(engine.lookup_candidates("", &LmContext::empty()).unwrap().is_empty());
    // ASCII 既有输出不变:同一次调用两次结果一致且非空。
    let ascii = engine.lookup_candidates("nihao", &LmContext::empty()).expect("查询失败");
    assert!(!ascii.is_empty(), "ASCII 路径不该被这次守卫影响");
}

/// 「开头一个缩写音节 + 后面完整字面音节链」必须被召回。
///
/// `z + tai lan`（状态栏）、`z + tai`（状态）：阶段 0 的证据显示通用混拼 DFS
/// 在这类输入上把 20000 步预算烧光却零收集，于是这两条读法压根不进池子。
/// 修法是先跑一遍**可穷尽的约束匹配**（缩写音节的每个展开 × 尾巴的完整音节键
/// 精确查词），规模只有该首字母的音节数。
#[test]
fn initial_plus_literal_tail_is_recalled() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let dict_path = root.join("dict.bin");
    let abbrev_path = root.join("abbrev.bin");
    let gram = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })
        .map(|b| b.join("xwx/wanxiang-lts-zh-hans.gram"));
    let (Some(gram_path), true, true) = (gram, dict_path.is_file(), abbrev_path.is_file()) else {
        return; // 环境缺文件:跳过
    };
    if !gram_path.is_file() {
        return;
    }
    let mut engine =
        ImeEngine::new_with_lm(Some(&dict_path), Some(&gram_path)).expect("引擎创建失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);

    for (input, want) in [("ztailan", "状态栏"), ("ztai", "状态")] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        assert!(
            cands.iter().take(5).any(|c| c.word == want),
            "{input} 前 5 条应出现「{want}」，实际: {:?}",
            cands.iter().take(5).map(|c| &c.word).collect::<Vec<_>>()
        );
    }
}
