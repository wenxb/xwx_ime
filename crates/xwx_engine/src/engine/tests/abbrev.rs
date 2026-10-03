//! crate 级单测:abbrev。

use super::*;

/// B4 分层(真实词库):排序键 = (u_min 桶, 层序, 分数 − 覆盖罚分)。
/// 输入 `beijdx`:兜底联想(`beijing` 等 bei-j* 前缀词)给精确层
/// (几十条,装不满 MAX_CANDIDATES),`lookup_abbrev("beijdx")` 命中
/// 「北京」类 b-j-d-x 词给简拼层。用真实词库而非玩具词库:玩具词库
/// 查不出「高频简拼词压过精确词」。
///
/// 三段式(首键从「裸未解释字母数」改为「u_min 桶」后改写):
/// - 原断言:「同覆盖度(未解释字母数相同)内层序不交错 + 未解释
///   字母数整体非降」——裸 u 当首键时的排序正确性断言;
/// - 改为:**同 u_min 桶内**层序不交错 + u_min 桶整体非降。裸 u
///   让「拼音长度==输入长度」的生僻组合词(被禁锢/狄安娜,u=0)
///   压过高频单字(被/点,u=4);u_min 桶把「终点落在不可精确切分
///   段内」的候选(被禁锢 bei jin [g]、进啊 jin [an])与「终点在
///   音节边界」的候选(被 bei、点 dian)分开,段内词一律沉底——
///   这是对裸 u 的结构性修正,不是放松:同桶内「精确 > 模糊 >
///   简拼」的层序保证原样钉住(桶即新首键);
/// - 为什么新行为对:裸 u 只看「解释了几个字母」,不看「剩下的
///   字母能不能精确切分」——被禁锢(u=0)与被(u=4)在裸 u 下
///   分属 0 键与 4 键,被禁锢合法地排在被之前;但 beijing 从
///   bei jin 起剩下的 g 不可精确切分(u_min=1),而从 bei 起
///   剩下的 jing 可以(u_min=0)——u_min 桶把这个结构差异变成
///   首键,段内组合词因此不再凭「拼音凑巧等长」浮上第一页。
#[test]
fn real_dict_abbrev_layer_never_outranks_exact() {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let mut engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);
    assert!(engine.dict.has_abbrev(), "副索引应已装配");

    let cands = engine
        .lookup_candidates("beijdx", &LmContext::empty())
        .expect("查询失败");
    assert!(
        cands.iter().any(|c| c.spelling == SpellingKind::Exact),
        "beijdx 应有精确层候选(兜底联想 bei-j* 前缀): {:?}",
        cands.iter().take(5).map(|c| (&c.word, c.spelling)).collect::<Vec<_>>()
    );
    assert!(
        cands.iter().any(|c| c.spelling == SpellingKind::Abbrev),
        "beijdx 应有简拼层候选(b-j-d-x 简拼词): {:?}",
        cands.iter().map(|c| (&c.word, c.spelling)).collect::<Vec<_>>()
    );
    // 同桶内层间不交错:按 u_min 桶分组,组内层序整体非降。
    // (跨组允许简拼层在精确层之前——0 桶简拼词排在 3 桶精确字之前
    // 是首键的预期效果。)
    let keys: Vec<(usize, SpellingKind)> = cands
        .iter()
        .map(|c| (sort_bucket("beijdx", c), c.spelling))
        .collect();
    let mut by_cov: HashMap<usize, Vec<SpellingKind>> = HashMap::new();
    for (u, k) in &keys {
        by_cov.entry(*u).or_default().push(*k);
    }
    for (u, kinds) in &by_cov {
        assert!(
            kinds.windows(2).all(|w| w[0] <= w[1]),
            "u_min 桶={u} 的组内层序不得交错: {kinds:?}\n全列表: {:?}",
            cands.iter().map(|c| (&c.word, c.spelling)).collect::<Vec<_>>()
        );
    }
    // 首键整体非降:u_min 桶必须单调不减(排序正确性的直接断言)。
    let unx: Vec<usize> = keys.iter().map(|(u, _)| *u).collect();
    assert!(
        unx.windows(2).all(|w| w[0] <= w[1]),
        "u_min 桶应整体非降(首键): {unx:?}"
    );
    // 精确层内相对顺序不被打乱:同桶的精确候选按分非增。
    let exact_scores: Vec<f32> = cands
        .iter()
        .filter(|c| c.spelling == SpellingKind::Exact
            && sort_bucket("beijdx", c) == 3)
        .map(|c| c.score)
        .collect();
    assert!(
        exact_scores.windows(2).all(|w| w[0] >= w[1]),
        "同桶精确层内仍按分降序: {exact_scores:?}"
    );
}

/// ü 的另一种键盘写法(真实词库):`lue`/`nue` 必须能打到词库里的
/// `lve`/`nve` 音节——用户报的「`hulue` 打不出 忽略」正是这个。
///
/// 关键在于**走主切分层**:分词器把 `lue` 归一成 `lve` 之后,格子里才
/// 有 `hu lve` 这条边,候选才是 `Exact`。不归一的话(D-C 之前的实测)
/// 候选只能从简拼层兜底进来,层序是 `Abbrev`,排在所有 `Exact` 之后
/// ——同一个词、同一个分数,名次从第 1 掉到第 59。
///
/// 失败见证:把 `build_lattice` 里的 `segmenter::canonical_syllable(...)`
/// 换回 `&pinyin[pos..pos + len]` → 红(`忽略` 变 `Abbrev`、位次跌出前 10);
/// 给 `segmenter::canonical_syllable` 加上 `"lu" => "lv"` → 最后一条断言红。
#[test]
fn real_dict_ue_input_reaches_the_v_spelling() {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let mut engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);
    let ctx = LmContext::empty();

    for (input, word) in [("hulue", "忽略"), ("lue", "略"), ("nuedai", "虐待")] {
        let cands = engine.lookup_candidates(input, &ctx).expect("查询失败");
        let pos = cands.iter().position(|c| c.word == word).unwrap_or_else(|| {
            panic!(
                "`{input}` 必须能打出「{word}」: 前 5 = {:?}",
                cands.iter().take(5).map(|c| &c.word).collect::<Vec<_>>()
            )
        });
        assert_eq!(
            cands[pos].spelling,
            SpellingKind::Exact,
            "`{input}` 的「{word}」应来自主切分层(不是简拼层兜底): 第 {} 名",
            pos + 1
        );
        assert!(pos < 10, "`{input}` 的「{word}」应在前 10: 第 {} 名", pos + 1);
    }

    // 用户明确否掉的口径:`lu`/`nu` 是「路/努」,不当 ü ——
    // 打 `nuren` 不得混进「女人」。
    let cands = engine.lookup_candidates("nuren", &ctx).expect("查询失败");
    assert!(
        !cands.iter().any(|c| c.word == "女人"),
        "`nuren` 不得出现「女人」(`lu`/`nu` 不合并到 `lv`/`nv`): {:?}",
        cands.iter().take(5).map(|c| &c.word).collect::<Vec<_>>()
    );
}

/// **一字一字母**(真实词库):简拼输入下,候选的字数不得超过输入字母数
/// ——用户报的「`wsm` 出现四个字」。
///
/// 三个来源都要堵,少一个都还有漏网:副索引 `lookup_abbrev`(key 前缀
/// 匹配,`wsm` 命中 `wsmy`)、`MixedDfs::collect` 的末位声母分支
/// (`wei shen m` 是字节前缀,`wei shen me yao` 也满足)、兜底联想的
/// `lookup_prefix*`(`zho` 捞出 `zhong a guan xi`)。
///
/// 失败见证:分别去掉这三处的过滤 → `wsm`(78 条超长)、`zho`(13 条)、
/// `sh`(1 条)各自变红。
///
/// **2026-10-02 收严到「每个字都得有字母」**:上面那条只挡「字数 >
/// 字母数」,挡不住「残缺音节之后又接了一整个音节」——`yij`(3 字母)配
/// `yi jing shi`(3 字)正好放行,用户报的「`yij` 出现已经是」就是它。
/// 判据换成 `MixedDfs::collect` 里的「残缺音节之后不许再有音节」后,
/// 期望词表里的 `zho`→中国 也一并消失(「国」一个字母都没打),
/// 改成断言 `zho`→中。
#[test]
fn real_dict_candidates_never_exceed_one_character_per_letter() {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let mut engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);
    let ctx = LmContext::empty();

    for input in ["wsm", "zgr", "ky", "zho", "sh", "zh", "xian", "nih", "w"] {
        let cands = engine.lookup_candidates(input, &ctx).expect("查询失败");
        let over: Vec<_> = cands
            .iter()
            .filter(|c| c.word.chars().count() > input.len())
            .map(|c| (&c.word, &c.pinyin))
            .collect();
        assert!(over.is_empty(), "`{input}` 出现字数 > 字母数的候选: {over:?}");
    }

    // 界限不得误杀目标词:简拼该给的一个都不能少。
    for (input, word) in [
        ("wsm", "为什么"),
        ("zgr", "中国人"),
        ("zgr", "这个人"),
        ("ky", "可以"),
        ("zho", "中"),
        ("xian", "西安"),
    ] {
        let cands = engine.lookup_candidates(input, &ctx).expect("查询失败");
        assert!(
            cands.iter().any(|c| c.word == word),
            "`{input}` 仍须给出「{word}」: {:?}",
            cands.iter().take(5).map(|c| &c.word).collect::<Vec<_>>()
        );
    }
}

/// **补全读法要进第一页**(2026-10-02):末尾半音节补全(`beijin` → `bei jing`)
/// 是「精确前缀」读法,层序必须与格子里「打字中的补全词」同为 `Exact`;
/// 标成 `Abbrev` 它就会排在**所有**精确候选之后,用户第一页只看得到单字。
///
/// 失败见证:把 `dict.rs::lookup_mixed` 里 T2 那一轮的层序标回 `Abbrev`
/// (等价于 `abbrev_candidates` 恢复「统一覆盖 spelling」)→ 四条全红
/// (`beijin` 的北京 #1 → #35、`niha` 的你好 #2 → #26、`meiyo` 的没有
/// #1 → #32、`zhongguor` 的中国 #4 → #80)。
#[test]
fn real_dict_partial_syllable_completions_reach_the_first_page() {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let mut engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);
    let ctx = LmContext::empty();
    for (input, want, max_rank) in [
        ("beijin", "北京", 3),
        ("niha", "你好", 3),
        ("meiyo", "没有", 2),
        ("zhongguor", "中国", 10),
    ] {
        let cands = engine.lookup_candidates(input, &ctx).expect("查询失败");
        let rank = cands.iter().position(|c| c.word == want).map(|i| i + 1);
        assert!(
            matches!(rank, Some(r) if r <= max_rank),
            "`{input}` 的「{want}」应在前 {max_rank} 名(实测 {rank:?}): {:?}",
            cands.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()
        );
    }
}

/// B4 层间边界(真实词库):同覆盖度内,层序必须整体非降——
/// 任何简拼候选夹在同覆盖度的精确候选之间就是交错。输入
/// `beijdx`:兜底联想给精确层(几十条),`lookup_abbrev` 给简拼层,
/// 两层都进得了候选(精确层装不满 MAX_CANDIDATES)。
///
/// 三段式(首键从「裸未解释字母数」改为「u_min 桶」后改写):
/// - 原断言:排序键整体非降——(未解释字母数, 层序) 的二元组
///   序列字典序非降(裸 u 当首键时的首键单调 + 同键内层序);
/// - 改为:(u_min 桶, 层序) 二元组字典序非降。裸 u 的首键单调
///   语义由 u_min 桶原样继承(桶单调 ⇔ 首键正确),同桶内层序
///   保证不变——断言形式不变,首键换成排序真正用的那个;
/// - 为什么新行为对:裸 u 把「拼音凑巧等长」的生僻组合词
///   (被禁锢,u=0)顶上第一页;u_min 桶按「终点能否精确切分到
///   串尾」分桶,段内词(u_min>0)一律沉底。同桶内「精确 >
///   模糊 > 简拼」的层序保证原样钉住,不是放松。
///   (模糊层的边界由玩具词库测试
///   `mixed_exact_and_fuzzy_edges_belong_to_fuzzy_layer` /
///   `layered_sort_disabled_matches_legacy_exactly` 覆盖——真实词库
///   下模糊层是否存在取决于词库,不宜钉死。)
#[test]
fn real_dict_layer_boundary_no_interleave() {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let mut engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);

    let cands = engine
        .lookup_candidates("beijdx", &LmContext::empty())
        .expect("查询失败");
    let kinds: Vec<SpellingKind> = cands.iter().map(|c| c.spelling).collect();
    assert!(kinds.contains(&SpellingKind::Exact), "beijdx 应有精确层: {kinds:?}");
    assert!(kinds.contains(&SpellingKind::Abbrev), "beijdx 应有简拼层: {kinds:?}");
    // (u_min 桶, 层序) 二元组字典序非降:首键单调 + 同键内层序
    // 单调,一条断言同时钉住两个排序键的正确性。
    let pair_keys: Vec<(usize, SpellingKind)> = cands
        .iter()
        .map(|c| (sort_bucket("beijdx", c), c.spelling))
        .collect();
    assert!(
        pair_keys.windows(2).all(|w| w[0] <= w[1]),
        "(未解释字母数, 层序) 应整体非降(首键 + 同键内层序): {:?}",
        cands.iter()
            .map(|c| (&c.word, c.spelling, sort_bucket("beijdx", c)))
            .collect::<Vec<_>>()
    );
}

/// 简拼层去重(真实词库):`zgr` 走 `lookup_abbrev` 命中的词,与
/// `lookup_mixed`(残余声母路径)命中的同一词不得重复出现;整个
/// 候选列表里同一 `(word, pinyin)` 只出现一次。
#[test]
fn real_dict_abbrev_no_duplicates_across_sources() {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let mut engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);

    for input in ["zgr", "zhongg", "nihao"] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        let mut seen: std::collections::HashSet<(&str, &str)> =
            std::collections::HashSet::new();
        for c in &cands {
            assert!(
                seen.insert((c.word.as_str(), c.pinyin.as_str())),
                "{input} 的 (word, pinyin) 不得跨层重复: ({}, {})",
                c.word,
                c.pinyin
            );
        }
    }
}

/// 副索引缺失时优雅退化(引擎层):`MmapDict::open()` 不挂副索引,
/// 引擎不得 panic,且全拼行为与「没接简拼」完全一致(精确候选
/// 逐位不变,简拼层为空)。
#[test]
fn engine_without_abbrev_index_degrades_gracefully() {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    if !dict_path.is_file() {
        return;
    }
    let engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
    assert!(!engine.dict.has_abbrev(), "未装配副索引");

    // 全拼:精确候选不受影响
    let nihao = engine
        .lookup_candidates("nihao", &LmContext::empty())
        .expect("查询失败");
    assert!(nihao.iter().any(|c| c.word == "你好"), "全拼应照常: {nihao:?}");
    // **2026-10-02 改断言(逐条说明)**:旧断言是「无副索引 ⇒ 一个简拼层
    // 候选都没有」。T2 之后这不再成立——末尾半音节的补充召回走的是**主
    // 索引**(`partial_tail_readings` + `collect`),不依赖副索引,所以
    // `nihao` 会多出「你好吗/你好啊」这类 Abbrev 层候选。
    // 副索引真正负责的是**纯首字母**简拼(`zgr`/`bjdx`),新的断言就断它:
    // 没有副索引时,纯首字母串不得有 Abbrev 候选。
    assert!(
        nihao.iter().any(|c| c.word == "你好"),
        "全拼照常(不受副索引影响)"
    );
    let zgr = engine
        .lookup_candidates("zgr", &LmContext::empty())
        .expect("查询失败");
    assert!(
        zgr.iter().all(|c| c.spelling != SpellingKind::Abbrev),
        "无副索引时纯首字母串不应有简拼层: {:?}",
        zgr.iter().map(|c| (&c.word, c.spelling)).collect::<Vec<_>>()
    );

    // 纯简拼:退化为回声(副索引缺失 ⇒ lookup_abbrev 空;输入
    // 切不出音节 ⇒ lookup_mixed 也空),引擎不 panic。
    let zgr = engine
        .lookup_candidates("zgr", &LmContext::empty())
        .expect("查询失败");
    assert_eq!(zgr.len(), 1, "无副索引时 zgr 应只剩回声: {zgr:?}");
    assert!(zgr[0].echo, "无副索引时 zgr 应回回声候选");

    // 混合简拼:靠静态音节表照常工作(不依赖副索引),这是
    // `real_abbrev_mixed_lookup` 同一不变量在引擎层的对应。
    // 直接在词库层断言(`lookup_mixed` 本身),绕开「精确层装满
    // 时简拼层被截断」的引擎层遮蔽——那条遮蔽是 MAX_CANDIDATES
    // 的既定行为,与本不变量无关。
    let dict_path2 = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let dict = crate::dict::MmapDict::open(&dict_path2).expect("词库加载失败");
    assert!(!dict.has_abbrev(), "裸 open 不应挂副索引");
    let mixed = dict.lookup_mixed("zhongguor").expect("混合简拼查询失败");
    assert!(
        mixed.iter().any(|c| c.word == "中国人"),
        "无副索引时 lookup_mixed 仍应命中中国人(静态音节表): {:?}",
        mixed.iter().take(5).map(|c| (&c.word, &c.pinyin)).collect::<Vec<_>>()
    );
}

/// **双字母声母的两种简拼写法都要认**(上游 RIME 的 `abbrev/^([zcs]h).+$/$1/`
/// 与 `abbrev/^(\l).+$/$1/` 并存)。
///
/// 副索引的 key 是「每个音节取一个字母」,所以 `shh`(时候 = shi hou)在表里对应
/// `sh`——用户敲的第三个字母 `h` 是「整体声母」写法留下的。只查字面串时
/// `shh` 的候选里**根本没有「时候」**(实测第一是「社会化」,靠 `s h h` 三个
/// 单字母读法命中),这正是用户报的「zh 这种需要特殊处理」。
///
/// **折叠校验(2026-10-03,⑤⑩⑥)**:「去 h」变体声称整体声母读法,对应音节
/// 必须真的以 zh/ch/sh 开头。`chh` 因此**不再**给出「策划」(ce hua:ce 不是
/// ch 声母,`chh` 只能读成 [ch]+[h])——旧断言钉住的是无校验宽松匹配的伪
/// 召回,同类的还有 `sh` 经折叠变体 `s` 命中 三/色/死(它们的 key 都是 `s`)。
/// 真 [ch]+[h] 词(称号/场合/窗户)不受影响,`ch`(两字母)仍给 策划。
#[test]
fn real_dict_two_letter_initial_abbreviations_hit_the_side_index() {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let dict = crate::dict::MmapDict::open(&dict_path)
        .expect("词库加载失败")
        .with_abbrev(&abbrev_path);
    assert!(dict.has_abbrev(), "副索引应已装配");

    for (input, want) in [("shh", "时候"), ("zhh", "只会"), ("chh", "称号")] {
        let got = dict.lookup_abbrev(input).expect("简拼查询失败");
        assert!(
            got.iter().any(|c| c.word == want),
            "`{input}` 应能命中 `{want}`(整体声母写法),实际前 5: {:?}",
            got.iter().take(5).map(|c| (&c.word, c.freq)).collect::<Vec<_>>()
        );
    }
    // 折叠校验的负向断言:整体声母读法不许带出「声母对不上」的词。
    // (取单读音字:色 有 sè/shǎi 两读,shǎi 经校验合法保留,不能当负例。)
    for (input, bad) in [("chh", "策划"), ("sh", "所"), ("sh", "四"), ("zh", "在")] {
        let got = dict.lookup_abbrev(input).expect("简拼查询失败");
        assert!(
            !got.iter().any(|c| c.word == bad),
            "`{input}` 不该给出「{bad}」(它不是该输入的整体声母读法): 前 5: {:?}",
            got.iter().take(5).map(|c| (&c.word, c.freq)).collect::<Vec<_>>()
        );
    }
    // **多折叠点的音节定位**:`zhzhzhzh` 的 4 个折叠点各落在第 0/1/2/3 音节,
    // 校验必须逐点对位(曾是 split().nth() 相对前进的错位 bug 的回归守卫)。
    let got = dict.lookup_abbrev("zhzhzhzh").expect("zhzhzhzh 查询失败");
    assert!(
        got.iter().any(|c| c.word == "重中之重"),
        "`zhzhzhzh`(4 个折叠点)仍须给出「重中之重」: 前 5: {:?}",
        got.iter().take(5).map(|c| (&c.word, c.freq)).collect::<Vec<_>>()
    );
    // 两字母字面写法不受校验影响:`ch` = c+h 两个首字母,策划(ce hua)照常。
    let got = dict.lookup_abbrev("ch").expect("ch 查询失败");
    assert!(
        got.iter().any(|c| c.word == "策划"),
        "`ch` 仍须给出「策划」: 前 5: {:?}",
        got.iter().take(5).map(|c| (&c.word, c.freq)).collect::<Vec<_>>()
    );
}

/// 末尾**单字母**尾巴也应补全成词库里的真实整串键。
///
/// `daishix`（dai+shi+x，差一个字母）现在给的是容错读法「带上/戴上/代收」——
/// 因为 `needs_abbrev` 里的 `!tolerant_explains` 把简拼/末尾补全那条路关掉了，
/// 「x → xian」这条读法（键 `dai shi xian` = 待实现）**压根没被产出**。
/// 对照：完整输入 `daishixian` 能出「待实现」。
///
/// 修法限定在「精确格不可达 + 尾部单字母 + 该字母能拼出词库里真实存在的整串键」，
/// 不整条拆掉 `!tolerant_explains`（早前整体拆过：390 行漂移、零改善）。
#[test]
fn single_letter_tail_completion_reaches_the_real_key() {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    // 这半必须带 LM：无模型时首位是「带实现」（容错层的单词读法），
    // 带模型才轮到末尾补全的「待实现」——与 sentence_gate 那半同理。
    let gram = std::env::var("XWX_LM_PATH")
        .ok()
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| {
            let home = std::env::var("HOME").ok()?;
            let p = std::path::PathBuf::from(home).join(".local/share/xwx/wanxiang-lts-zh-hans.gram");
            p.is_file().then_some(p)
        });
    if gram.is_none() {
        eprintln!("跳过：没有 .gram 模型（无模型时该形态到不了首位）");
        return;
    }
    let mut engine = ImeEngine::new_with_lm(Some(&dict_path), gram.as_deref()).expect("词库加载失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);
    let cands = engine
        .lookup_candidates("daishix", &LmContext::empty())
        .expect("查询失败");
    let top3: Vec<&str> = cands.iter().take(3).map(|c| c.word.as_str()).collect();
    assert!(
        top3.contains(&"待实现"),
        "前 3 条里应有「待实现」，实际: {top3:?}"
    );
}
