//! crate 级单测:fuzzy。

use super::*;

/// 精确匹配的候选应排在只能靠模糊匹配到达的候选之前。
///
/// 构造:输入 "ni",词库里有「你」(ni,精确)和「里」(li,仅 n/l
/// 混淆到达),两者词频相同。fuzzy 开启后「里」应出现,但分数
/// 必须严格低于「你」(零惩罚 vs -log10(0.8) ≈ 0.097)。
#[test]
fn exact_match_outranks_fuzzy_match() {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    engine.dict.upsert("里", "li", 100).expect("插入失败");
    engine.fuzzy_enabled = true;

    let cands = engine
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");
    let rank = |word: &str| cands.iter().position(|c| c.word == word);
    let (ri, rl) = (rank("你"), rank("里"));
    assert!(ri.is_some(), "精确匹配「你」应在候选里: {cands:?}");
    assert!(rl.is_some(), "模糊匹配「里」应在候选里: {cands:?}");
    assert!(ri < rl, "精确「你」应排在模糊「里」前: {cands:?}");

    let si = cands[ri.unwrap()].score;
    let sl = cands[rl.unwrap()].score;
    assert!(si > sl, "精确分 {si} 应严格大于模糊分 {sl}");
    // 惩罚量级:-log10(0.8) ≈ 0.0969
    assert!(
        (si - sl - 0.0969).abs() < 1e-2,
        "分差应≈0.097(n/l 单步惩罚), 实际 {}",
        si - sl
    );
}

/// 高 weight 混淆对(n/l = 0.8)产生的候选应排在低 weight
/// (s/x = 0.2)产生的候选之前——其它条件相同。
///
/// 构造:输入 "ni",词库里有「里」(li,经 n/l=0.8)和「希」
/// (xi,经 s/x=0.2 的反向 x→s?不,需要 n→x?)。直接构造两组
/// 混淆对:输入 "a",词「甲」(b,经 a/b=0.8)与「乙」(c,经
/// a/c=0.2),同词频。
#[test]
fn high_weight_confusion_outranks_low_weight() {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    // 词频相同,唯一差别是到达路径的混淆对 weight
    engine.dict.upsert("甲", "b", 100).expect("插入失败");
    engine.dict.upsert("乙", "c", 100).expect("插入失败");
    engine.fuzzy_pairs = vec![
        ConfusionPair::new("a", "b", 0.8),
        ConfusionPair::new("a", "c", 0.2),
    ];
    engine.fuzzy_enabled = true;

    let cands = engine
        .lookup_candidates("a", &LmContext::empty())
        .expect("查询失败");
    let rank = |word: &str| cands.iter().position(|c| c.word == word);
    let (ra, rb) = (rank("甲"), rank("乙"));
    assert!(ra.is_some() && rb.is_some(), "两词都应出现: {cands:?}");
    assert!(ra < rb, "高 weight 的「甲」应排在低 weight 的「乙」前: {cands:?}");

    let sa = cands[ra.unwrap()].score;
    let sb = cands[rb.unwrap()].score;
    // 分差 = -log10(0.2) - (-log10(0.8)) = 0.699 - 0.097 ≈ 0.602
    assert!(
        (sa - sb - 0.602).abs() < 1e-2,
        "分差应≈0.602, 实际 {}",
        sa - sb
    );
}

/// 多处混淆叠加时惩罚应累加:经两次混淆到达的候选,分数应比
/// 经一次混淆到达的低约一个 -log10(weight)。
///
/// 构造:输入 "nan",词库有「难」(nan,精确)、「蓝」(lan,经
/// n/l 一次)、「狼」(lang,经 n/l + an/ang 两次),三者词频相同。
/// 「蓝」应比「难」低 ≈0.097,「狼」应比「蓝」低 ≈0.301。
#[test]
fn multi_confusion_penalty_accumulates() {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("难", "nan", 100).expect("插入失败");
    engine.dict.upsert("蓝", "lan", 100).expect("插入失败");
    engine.dict.upsert("狼", "lang", 100).expect("插入失败");
    engine.fuzzy_pairs = vec![
        ConfusionPair::new("n", "l", 0.8),
        ConfusionPair::new("an", "ang", 0.5),
    ];
    engine.fuzzy_enabled = true;

    let cands = engine
        .lookup_candidates("nan", &LmContext::empty())
        .expect("查询失败");
    let score_of = |word: &str| {
        cands.iter().find(|c| c.word == word).map(|c| c.score)
    };
    let (snan, slan, slang) = (
        score_of("难").expect("难应在候选里"),
        score_of("蓝").expect("蓝应在候选里"),
        score_of("狼").expect("狼应在候选里"),
    );
    assert!(snan > slan && slan > slang,
        "精确 > 单混淆 > 双混淆: 难={snan} 蓝={slan} 狼={slang}");
    // 单混淆惩罚 -log10(0.8) ≈ 0.097
    assert!((snan - slan - 0.0969).abs() < 1e-2,
        "单混淆差应≈0.097, 实际 {}", snan - slan);
    // 双混淆再叠加 -log10(0.5) ≈ 0.301
    assert!((slan - slang - (-0.5f32.log10())).abs() < 1e-2,
        "双混淆累加差应≈0.301, 实际 {}", slan - slang);
}

/// fuzzy_enabled = false 时行为应与改动前完全一致:模糊变体不
/// 进候选,排序纯由词频 + LM 决定,不受 fuzzy_pairs 配置影响。
#[test]
fn fuzzy_disabled_is_unchanged() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    engine.dict.upsert("逆", "ni", 50).expect("插入失败");
    engine.dict.upsert("里", "li", 200).expect("插入失败");
    // fuzzy_enabled 保持默认 false;fuzzy_pairs 有值也不应生效
    assert!(!engine.fuzzy_enabled);

    let cands = engine
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");
    // 「里」词频最高(200),但它是 li,输入 ni 时不应出现
    assert!(!cands.iter().any(|c| c.word == "里"),
        "fuzzy 关闭时 li 不应出现在 ni 的候选里: {cands:?}");
    assert_eq!(cands.first().map(|c| c.word.as_str()), Some("你"));
    // 排序与旧版一致:你(100) 前于 逆(50)
    let rank = |word: &str| cands.iter().position(|c| c.word == word);
    assert!(rank("你") < rank("逆"));
}

/// 分层排序的核心:精确匹配整体优先于模糊匹配,**即使模糊匹配
/// 的词频高得多**(固定惩罚压不住任意大的词频差,分层可以)。
///
/// 构造对应真实词库实测的「然(734万, r/l 模糊)压过蓝(43万, 精确)」:
/// 输入 "lan",「然」(ran, r→l 混淆,weight 0.3)词频是「蓝」
/// (lan, 精确)的 10 倍。分层前「然」在「蓝」前;分层后「蓝」
/// 必须在「然」前,哪怕分数更低。
#[test]
fn exact_layer_outranks_fuzzy_despite_much_higher_freq() {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    // 精确:蓝 lan freq=100;模糊:然 ran freq=1000(10 倍词频差,
    // 对数域差 1.0,远超 r/l 的惩罚 -log10(0.3) ≈ 0.52)
    engine.dict.upsert("蓝", "lan", 100).expect("插入失败");
    engine.dict.upsert("然", "ran", 1000).expect("插入失败");
    engine.fuzzy_enabled = true;

    let cands = engine
        .lookup_candidates("lan", &LmContext::empty())
        .expect("查询失败");
    let pos = |word: &str| cands.iter().position(|c| c.word == word);
    let (pl, pr) = (pos("蓝").expect("蓝应在候选里"), pos("然").expect("然应在候选里"));
    assert!(pl < pr, "精确「蓝」应排在高频模糊「然」前: {cands:?}");

    let (sl, sr) = (cands[pl].score, cands[pr].score);
    assert!(sr > sl, "然的分数确实更高(词频差压过惩罚),分层前它排前面");
    assert_eq!(cands[pl].spelling, SpellingKind::Exact, "蓝是精确匹配");
    assert_eq!(cands[pr].spelling, SpellingKind::Fuzzy, "然是模糊匹配(r/l)");

    // 层间边界:所有精确候选都在所有模糊候选之前
    let first_fuzzy = cands.iter().position(|c| c.spelling == SpellingKind::Fuzzy);
    if let Some(ff) = first_fuzzy {
        assert!(
            cands[..ff].iter().all(|c| c.spelling == SpellingKind::Exact),
            "精确层内不应混入模糊/简拼候选: {cands:?}"
        );
    }
}

/// 模糊层内部仍按惩罚排序:高 weight 混淆(n/l=0.8)产生的候选
/// 排在低 weight(s/x=0.2)之前——分层不改变层内的惩罚区分。
#[test]
fn fuzzy_layer_internally_sorted_by_penalty() {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    // 两个模糊候选同词频,唯一差别是混淆对 weight;再加一个精确
    // 候选确认模糊层整体在精确层之后。
    engine.dict.upsert("精", "a", 10).expect("插入失败");
    engine.dict.upsert("高", "b", 100).expect("插入失败");
    engine.dict.upsert("低", "c", 100).expect("插入失败");
    engine.fuzzy_pairs = vec![
        ConfusionPair::new("a", "b", 0.8),
        ConfusionPair::new("a", "c", 0.2),
    ];
    engine.fuzzy_enabled = true;

    let cands = engine
        .lookup_candidates("a", &LmContext::empty())
        .expect("查询失败");
    let pos = |word: &str| cands.iter().position(|c| c.word == word);
    let (pe, ph, pl) = (
        pos("精").expect("精应在候选里"),
        pos("高").expect("高应在候选里"),
        pos("低").expect("低应在候选里"),
    );
    assert!(pe < ph && pe < pl, "精确层整体在前: {cands:?}");
    assert!(ph < pl, "模糊层内高 weight 的应在前: {cands:?}");
    assert_eq!(cands[ph].spelling, SpellingKind::Fuzzy);
    assert_eq!(cands[pl].spelling, SpellingKind::Fuzzy);
    assert_eq!(cands[pe].spelling, SpellingKind::Exact);
}

/// 混合路径(精确边 + 模糊边)归模糊层:一处不精确则整体不精确。
/// 判断用累计惩罚是否为 0——与逐边布尔或等价,但惩罚本就沿
/// arena 累计,实现零额外开销。
///
/// 构造:输入 "naha"(无音节重启歧义:na+ha 是唯一两音节切分,
/// 避开 nan+ni 这类段器不支持的重启,那是与分层无关的既有行为)。
/// 「那哈」(na ha,两精确边)→ 精确层;「拉哈」(la ha,经 n→l
/// 一条模糊边 + 一条精确边)→ 混合路径,应归模糊层。
#[test]
fn mixed_exact_and_fuzzy_edges_belong_to_fuzzy_layer() {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("那", "na", 100).expect("插入失败");
    engine.dict.upsert("哈", "ha", 100).expect("插入失败");
    engine.dict.upsert("那哈", "na ha", 50).expect("插入失败");
    engine.dict.upsert("拉哈", "la ha", 10000).expect("插入失败");
    engine.fuzzy_pairs = vec![ConfusionPair::new("n", "l", 0.8)];
    engine.fuzzy_enabled = true;

    let cands = engine
        .lookup_candidates("naha", &LmContext::empty())
        .expect("查询失败");
    let get = |word: &str| cands.iter().find(|c| c.word == word);
    let pure = get("那哈").expect("那哈应在候选里");
    let mixed = get("拉哈").expect("拉哈应在候选里");
    assert_eq!(pure.spelling, SpellingKind::Exact,
        "纯精确边应在精确层: {cands:?}");
    assert_eq!(mixed.spelling, SpellingKind::Fuzzy,
        "该候选由模糊边到达,应归模糊层: {cands:?}");

    // 混合路径词频再高(10000 vs 50)也得排在精确层之后
    let pos = |word: &str| cands.iter().position(|c| c.word == word);
    assert!(pos("那哈") < pos("拉哈"), "精确层整体优先: {cands:?}");
    // Block A 起候选只含起始于位置 0 的词:「哈」只覆盖后段音节
    // ha(位置 2),不再作为候选出现;也就不再有「继承所在路径的
    // 模糊层归属」的问题(那是旧实现「候选取自路径所有词位」的
    // 连带语义)。
    assert!(
        !cands.iter().any(|c| c.word == "哈"),
        "只覆盖后段音节的「哈」不应出现在 naha 的候选里: {cands:?}"
    );
}

/// fuzzy_enabled = false 时排序与分层前完全一致:全部候选 fuzzy
/// 都是 false,首层 key 相等,退化为纯分数排序。逐字段对比开关
/// 两种状态——开 fuzzy 只应追加模糊层,精确层的前缀必须逐位相同。
#[test]
fn layered_sort_disabled_matches_legacy_exactly() {
    let build = |fuzzy: bool| {
        let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
        for (w, py, f) in [
            ("你", "ni", 100),
            ("逆", "ni", 50),
            ("妮", "ni", 80),
            ("泥", "ni", 60),
            ("里", "li", 1000), // 高频模糊词:开 fuzzy 时会出现,但只能进模糊层
        ] {
            engine.dict.upsert(w, py, f).expect("插入失败");
        }
        engine.fuzzy_enabled = fuzzy;
        engine
    };

    let off = build(false)
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");
    let on = build(true)
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");

    // 关 fuzzy:候选与改动前完全一致(里 不出现,纯按词频排)
    assert!(!off.iter().any(|c| c.word == "里"));
    assert!(off.iter().all(|c| c.spelling == SpellingKind::Exact),
        "关 fuzzy 时应全是精确层");
    let off_words: Vec<&str> = off.iter().map(|c| c.word.as_str()).collect();
    assert_eq!(off_words, ["你", "妮", "泥", "逆"], "词频降序: {off:?}");

    // 开 fuzzy:精确层前缀逐位相同,模糊层追加在后
    let on_exact: Vec<&str> = on.iter()
        .filter(|c| c.spelling == SpellingKind::Exact)
        .map(|c| c.word.as_str())
        .collect();
    assert_eq!(on_exact, off_words, "精确层应与关 fuzzy 时逐位一致");
    let on_fuzzy: Vec<&str> = on.iter()
        .filter(|c| c.spelling == SpellingKind::Fuzzy)
        .map(|c| c.word.as_str())
        .collect();
    assert_eq!(on_fuzzy, ["里"], "模糊层只应有「里」: {on:?}");
}

/// 环境变量解析的**全表契约**。
///
/// 这条测试的存在理由:此前 `fuzzy_enabled` 在生产里永远是 false
/// (没有任何配置入口),所以「解析器」本身不存在、也无人测。
/// 现在它成了用户唯一的开启通道,取值口径必须逐条钉死——
/// 尤其是**无法识别的值必须回落到默认值而不是报错**
/// (输入法启动失败比「模糊音没开」严重得多)。
#[test]
fn fuzzy_env_parsing_table() {
    // 开启词(大小写不敏感、允许两侧空白)
    for v in ["1", "true", "TRUE", "True", "on", "ON", "yes", "Yes", " 1 ", " on "] {
        assert!(parse_fuzzy_env(Some(v)), "{v:?} 应解析为开启");
    }
    // 关闭词
    for v in ["0", "false", "FALSE", "off", "OFF", "no", "NO", "", "  "] {
        assert!(!parse_fuzzy_env(Some(v)), "{v:?} 应解析为关闭");
    }
    // 未设置 → 生产默认值
    assert_eq!(
        parse_fuzzy_env(None),
        DEFAULT_FUZZY_ENABLED,
        "未设置必须等于生产默认值"
    );
    // 无法识别 → 同样回落到默认值(不 panic、不报错)
    for v in ["2", "maybe", "enable", "模糊", "y", "n", "t", "f"] {
        assert_eq!(
            parse_fuzzy_env(Some(v)),
            DEFAULT_FUZZY_ENABLED,
            "{v:?} 无法识别,应回落到默认值而不是报错"
        );
    }
    // 「认不认得」与「解析成什么」必须一致(两份词表会漂移)。
    assert!(fuzzy_env_value_recognized(None), "未设置不是错误,算认得");
    for v in ["1", "true", "on", "yes", "0", "false", "off", "no", "", " ON "] {
        assert!(fuzzy_env_value_recognized(Some(v)), "{v:?} 应认得");
    }
    for v in ["2", "maybe", "enable", "模糊", "y"] {
        assert!(!fuzzy_env_value_recognized(Some(v)), "{v:?} 不应认得");
    }
}

/// 默认值必须是**实测裁决过的那个值**,不是随手写的。
///
/// 钉住 `false`:实测(`examples/fuzzy_bench.rs`,release、真实
/// dict.bin + 万象 gram)开启后最长全拼串 3.376ms → 22.008ms
/// (**6.48x**),突破本项目自定的 12ms 上限;`diannao` 前 10 有
/// 一处候选换血;收益仅 2 组新召回且名次在 16~27。
///
/// **如果这条断言红了**,说明有人改了默认值——请先重跑
/// `fuzzy_bench` 并更新 `DEFAULT_FUZZY_ENABLED` 文档里那张表,
/// 而不是直接改这里的期望值。判据本身会腐烂(HANDOFF.md
/// 「判据本身会腐烂,要定期重验」),这张表就是它现在的口径。
#[test]
fn fuzzy_default_matches_measured_verdict() {
    // 用 `const { ... }` 而不是普通 `assert!`:断言的是编译期常量,
    // 普通 assert 会被编译器直接折叠掉(clippy 的
    // `assertions_on_constants` 正是为此报错)。const 块把这条判据
    // 变成**编译期**检查——有人改默认值时是编译失败,而不是一个
    // 可能被忽略的测试红。这比运行时断言强,正是本判据需要的强度
    // (默认值直接决定用户的击键延迟)。
    const {
        assert!(
            !DEFAULT_FUZZY_ENABLED,
            "默认必须关闭:实测开启后长串 22ms(超 12ms 上限)、有排序退化、收益弱。\
             改默认值前先重跑 examples/fuzzy_bench.rs 并更新文档表格"
        );
    }
    // 环境变量名是对外契约(用户的 shell 里写的就是它),改名要同步文档。
    assert_eq!(FUZZY_ENV, "XWX_FUZZY", "环境变量名是对外契约");
}

/// 默认关闭时,引擎的模糊能力**逐字节不变**——即「接线」这件事
/// 不能顺手改动任何既有行为。
///
/// 这是「不许退化」硬约束在本次改动上的落点:接线前 fuzzy_enabled
/// 恒为 false,接线后默认仍是 false,所以默认路径必须与接线前
/// 完全一致。用同一词库、同一输入,断言候选列表逐位相同。
#[test]
fn default_config_reproduces_pre_wiring_behavior() {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    for (w, py, f) in [("你", "ni", 100), ("逆", "ni", 50), ("里", "li", 1000)] {
        engine.dict.upsert(w, py, f).expect("插入失败");
    }
    // 显式把引擎置回「接线前的生产状态」:false。
    engine.fuzzy_enabled = parse_fuzzy_env(None);
    assert!(!engine.fuzzy_enabled);

    let cands = engine
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");
    // 「里」(li)词频 1000 远高于「你」(100),但精确匹配下它不该
    // 出现——这正是「关模糊 = 不做展开」的证据。
    assert!(
        !cands.iter().any(|c| c.word == "里"),
        "默认关闭时不应展开出模糊候选「里」: {cands:?}"
    );
    assert!(
        cands.iter().all(|c| c.spelling == SpellingKind::Exact),
        "默认关闭时全部候选都应是精确层: {cands:?}"
    );
    let words: Vec<&str> = cands.iter().map(|c| c.word.as_str()).collect();
    assert_eq!(words, ["你", "逆"], "词频降序,且无模糊候选: {cands:?}");
}
