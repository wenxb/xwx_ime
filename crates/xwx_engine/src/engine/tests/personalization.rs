//! crate 级单测:personalization。

use super::*;

/// **纯简拼下「用户学过的词」必须排在前面**(用户实测报的 `ky` → 可以)。
///
/// 事故(2026-10-02,真实词库 + 真实用户模型):用户层对「没学过」的词
/// 给一个绝对中性分 `-1.0`,而用户层自己的概率按**用户语料规模**
/// 归一化(计数 3 时 ≈ log10(4/175) = -1.64),于是学过的「可以」
/// 反而比没学过的「跨越/可用」低 0.64 个数量级——实测第 **11** 名。
/// 修法见 `lm::MixedLM::score`:无个人证据原样回退通用层,有证据时
/// 插值且不低于通用层。
///
/// 失败见证:把 `MixedLM::score` 的无证据分支换回任何「固定分」写法
/// → 「可以」的总分要比「跨越」低 0.64(log10(505814/497857) 只有
/// 0.007,补不回来)→ 红。
#[test]
fn real_dict_learned_word_tops_pure_abbrev() {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let build = || {
        let mut e = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
        e.dict = e.dict.with_abbrev(&abbrev_path);
        e
    };
    let score_of = |e: &ImeEngine, word: &str| -> f32 {
        e.lookup_candidates("ky", &LmContext::empty())
            .expect("查询失败")
            .into_iter()
            .find(|c| c.word == word)
            .unwrap_or_else(|| panic!("ky 候选里应有「{word}」"))
            .score
    };

    let bare = build();
    let user = crate::UserLM::in_memory();
    for _ in 0..3 {
        user.learn(None, "可以"); // 等价于用户上屏 3 次
    }
    let with_user = build().with_user_lm(user);

    let learned_before = score_of(&bare, "可以");
    let learned_after = score_of(&with_user, "可以");
    assert!(
        learned_after > learned_before,
        "学过 3 次的「可以」应比没学过时高: {learned_before} -> {learned_after}"
    );

    // 没学过的近频竞争者必须**原样保留**通用层分(学习不连坐)。
    for rival in ["跨越", "可用"] {
        let b = score_of(&bare, rival);
        let u = score_of(&with_user, rival);
        assert_eq!(b, u, "没学过的「{rival}」不得被用户层改动: {b} vs {u}");
    }

    let cands = with_user
        .lookup_candidates("ky", &LmContext::empty())
        .expect("查询失败");
    let head: Vec<(&str, f32)> = cands.iter().take(3).map(|c| (c.word.as_str(), c.score)).collect();
    assert_eq!(
        cands[0].word, "可以",
        "纯简拼 `ky` 下学过的「可以」必须排第一(旧实现排第 11): {head:?}"
    );
    assert!(
        score_of(&with_user, "可以") > score_of(&with_user, "跨越"),
        "学过的「可以」应高于没学过的「跨越」"
    );
}

/// **同类候选:选过一次就登顶**(计数增益口径,2026-10-02)。
///
/// 实测(真实词库;测试环境不挂 `.gram`,分差与挂了时逐位相同):
/// `xian` 第 1 名「先」比第 10 名「闲」高 **0.913** log10,而一次上屏的
/// 计数增益是 `log10(1 + 1×USER_COUNT_GAIN_R)` = **3.0** > 0.913
/// ⇒ 一次误选就登顶。这正是用户要的「选过的词排在前面」。
///
/// 失败见证:把 `USER_COUNT_GAIN_R` 调到 ≤ 9(一次增益 < 0.913)→ 红。
#[test]
fn real_dict_single_pick_tops_same_class_rivals() {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let mut engine = ImeEngine::new(Some(&dict_path))
        .expect("词库加载失败")
        .with_user_lm(crate::UserLM::in_memory());
    engine.dict = engine.dict.with_abbrev(&abbrev_path);
    let rank_of = |w: &str| {
        engine
            .lookup_candidates("xian", &LmContext::empty())
            .expect("查询失败")
            .iter()
            .position(|c| c.word == w)
            .map(|i| i + 1)
    };

    let before = rank_of("闲").expect("「闲」应在候选里");
    assert!(before > 1, "学习前「闲」不该在第一位: 第 {before}");
    engine
        .user_lm()
        .expect("引擎应挂用户层")
        .learn(None, "闲");
    assert_eq!(
        rank_of("闲"),
        Some(1),
        "选过一次的「闲」在同类候选里应登顶"
    );
}

/// **结构墙**:只覆盖前缀的单字,计数增益也翻不过「整词」。
///
/// 实测(不挂 `.gram`):`women` 的「我」落后整词「我们」**10.088** log10,
/// 按计数增益曲线要翻过去得选 ≈ 1.2×10⁷ 次;`diannao` 的「电」差
/// **11.604**、`zhongguoren` 的「中国」差 **10.568**。
///
/// 这条护栏是刻意的:否则打过一次「我」,以后 `women` 永远先出「我」
/// ——那是「输入法坏了」,不是「个人化」。
///
/// 失败见证:把 `USER_COUNT_GAIN_R` 抬到 10¹¹ 量级(或去掉逐字拆解的
/// 补全罚分)→ 红。
#[test]
fn real_dict_structural_wall_survives_learning() {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let mut engine = ImeEngine::new(Some(&dict_path))
        .expect("词库加载失败")
        .with_user_lm(crate::UserLM::in_memory());
    engine.dict = engine.dict.with_abbrev(&abbrev_path);
    let user = engine.user_lm().expect("引擎应挂用户层");

    let mut learned = 0usize;
    for n in [1usize, 3, 10, 100] {
        while learned < n {
            user.learn(None, "我");
            learned += 1;
        }
        let cands = engine
            .lookup_candidates("women", &LmContext::empty())
            .expect("查询失败");
        let pos_word = cands
            .iter()
            .position(|c| c.word == "我们")
            .unwrap_or_else(|| panic!("「我们」应在候选里(学过 {n} 次)"));
        let pos_char = cands
            .iter()
            .position(|c| c.word == "我")
            .unwrap_or_else(|| panic!("「我」应在候选里(学过 {n} 次)"));
        assert_eq!(cands[0].word, "我们", "学过 {n} 次「我」后整词仍须第一");
        assert!(
            pos_char > pos_word,
            "学过 {n} 次「我」仍不得压过整词「我们」: 我 @{pos_char}, 我们 @{pos_word}"
        );
    }
}

/// 回声语义未变(真实词库 + 简拼):`zgr` 现在有真实简拼候选,
/// 回声候选**不得**出现(回声只在全场无候选时冒头);简拼接入
/// 后回声自然更少出现,这是预期——但解不出的串(非拼音字母组合
/// `vq`,词典无任何 `v` 起始音节,简拼副索引也没有 `vq` 前缀)
/// 仍必须回回声,语义不变。
#[test]
fn real_dict_abbrev_suppresses_echo_but_unresolvable_still_echoes() {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let mut engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);

    let zgr = engine
        .lookup_candidates("zgr", &LmContext::empty())
        .expect("查询失败");
    assert!(!zgr.is_empty(), "zgr 应有简拼候选");
    assert!(
        zgr.iter().all(|c| !c.echo),
        "有简拼候选时不应出现回声候选: {:?}",
        zgr.iter().take(3).map(|c| (&c.word, c.echo)).collect::<Vec<_>>()
    );
    assert!(
        zgr.iter().all(|c| c.spelling == SpellingKind::Abbrev),
        "zgr 的候选应全部在简拼层(无精确/模糊来源): {:?}",
        zgr.iter().take(3).map(|c| (&c.word, c.spelling)).collect::<Vec<_>>()
    );

    let vq = engine
        .lookup_candidates("vq", &LmContext::empty())
        .expect("查询失败");
    assert_eq!(vq.len(), 1, "vq 解不出候选,应只有回声: {vq:?}");
    assert!(vq[0].echo, "vq 应回回声候选");
}
