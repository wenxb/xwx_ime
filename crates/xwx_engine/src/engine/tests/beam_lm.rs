//! crate 级单测:beam_lm。

use super::*;

#[test]
fn real_dict_bin_loads() {
    // 冒烟测试：仓库里若生成了 data/dict.bin，验证转换器产物可被
    // mmap 加载、词条数与抽查询正确（无文件时跳过，不依赖数据入库）。
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    if !path.is_file() {
        return;
    }
    let engine = ImeEngine::new(Some(&path)).expect("mmap 词库加载失败");
    let DictStore::Mmap(dict) = &engine.dict else {
        panic!("dict.bin 应加载为 Mmap 词库");
    };
    // 构建自 rime-ice 全量词表（含 tencent 自动注音）后约 190 万条
    assert!(dict.count() >= 1_000_000, "词条数应超过百万: {}", dict.count());
    let cands = dict.lookup("ni").expect("查询失败");
    assert!(!cands.is_empty(), "ni 应有候选");
    assert_eq!(cands[0].word, "你", "最高频词应为 你");
    assert!(!dict.lookup_prefix("zhon").expect("前缀查询失败").is_empty());
}

/// B2 消歧(真实词库):`xi'an` 的候选里**不得**含只能由 `[xian]`
/// 单音节切法产生的高频字(先/现/显/线/险/仙……),且「西安」必须
/// 出现且名次显著前移。玩具词库里只有两三个词,挡不住「`'` 被静默
/// 删掉、`[xian]` 原样复活」这类问题——真实词库下 `xian` 有 100 个
/// 候选,这些字是否消失才看得出来。缺 `data/dict.bin` 时跳过。
#[test]
fn real_dict_apostrophe_disambiguates_xian() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    if !path.is_file() {
        return;
    }
    let engine = ImeEngine::new(Some(&path)).expect("mmap 词库加载失败");

    // 对照组:不带 `'` 的 `xian` 必须**有**这些单音节字(消歧的前提)。
    let plain = engine
        .lookup_candidates("xian", &LmContext::empty())
        .expect("查询失败");
    assert!(
        plain.iter().any(|c| c.word == "先" && c.pinyin == "xian"),
        "对照组 `xian` 应含「先」(否则本测试的消歧断言失去意义)"
    );

    // 实验组:`xi'an` 不得召回任何只能由 `[xian]` 切出的字。
    let cands = engine
        .lookup_candidates("xi'an", &LmContext::empty())
        .expect("查询失败");
    for w in ["先", "现", "显", "线", "险", "仙"] {
        assert!(
            !cands.iter().any(|c| c.word == w && c.pinyin == "xian"),
            "xi'an 不应再召回只能由 `[xian]` 切出的「{w}」(`'` 未生效回归): 前 10 {:?}",
            cands.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()
        );
    }
    // 「西安」必须在,且名次显著前移(对照组第 18 名;消歧后候选池
    // 只剩 [xi|an] 系,应在第一页前列)。不设「必须第 1」硬指标——
    // 名次由数据决定,但绝不应再被 [xian] 的高频单字压在 18 名。
    let rank = cands
        .iter()
        .position(|c| c.word == "西安")
        .unwrap_or_else(|| panic!("xi'an 应含「西安」: {:?}", cands.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()));
    assert!(
        rank < 10,
        "xi'an 的「西安」应显著前移(对照组第 18 名),实际第 {} 名: 前 10 {:?}",
        rank + 1,
        cands.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()
    );
}

#[test]
fn context_changes_candidate_ranking() {
    // 端到端验收：lookup_candidates 收到上文后，词对命中会真的改排序。
    // 词频：过(20) > 国(10)；无上文时「过」在前，"中"→"国" 命中后
    // 「国」应被抬到最前（否则说明上下文没进打分）。
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("国", "guo", 10).expect("插入失败");
    engine.dict.upsert("过", "guo", 20).expect("插入失败");
    let LmStore::Bigram(lm) = &mut engine.lm else {
        panic!("默认引擎应为 bigram（测试用 new(None) 创建）");
    };
    lm.add_unigram("中", 100);
    lm.add_unigram("国", 100);
    lm.add_unigram("过", 100);
    lm.add_bigram("中", "国", 90);

    let no_ctx = LmContext::empty();
    let with_ctx = LmContext {
        tail: "中",
        last_word: Some("中"),
    };

    let plain = engine
        .lookup_candidates("guo", &no_ctx)
        .expect("查询失败");
    assert_eq!(plain[0].word, "过", "无上文：词频高的「过」应在最前");

    let after = engine
        .lookup_candidates("guo", &with_ctx)
        .expect("查询失败");
    assert_eq!(after[0].word, "国", "上文「中」应让「国」压过「过」");
}

/// 短语整词路径应压过逐字拆解路径（第 2b 步的核心可见变化）：
/// `"nihao"` 首选整串解码出的「你好」，而不是词频更高的单字「你」。
#[test]
fn phrase_path_outranks_split_words() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你", "ni", 200).expect("插入失败");
    engine.dict.upsert("好", "hao", 150).expect("插入失败");
    engine.dict.upsert("你好", "ni hao", 100).expect("插入失败");

    // 空 LM：路径[你好] = 100-800 = -700 > 路径[你,好] = 200-800+150-800 = -1250
    let cands = engine
        .lookup_candidates("nihao", &LmContext::empty())
        .expect("查询失败");
    assert_eq!(cands[0].word, "你好", "整串最优路径的短语应排第一");
    // 单字候选仍要保留(用户可能只想上屏一个字)。
    // Block A 起候选只含起始于位置 0 的词:「你」(ni,位置 0)保留;
    // 「好」只覆盖后段音节 hao(位置 2),不再作为候选出现——
    // 输入 nihao 时用户无法有意义地选中「好」(语义同 librime:
    // 其同输入候选全部起始于位置 0)。
    assert!(cands.iter().any(|c| c.word == "你"), "单字「你」应在候选里");
    assert!(
        !cands.iter().any(|c| c.word == "好"),
        "只覆盖后段音节的「好」不应出现在 nihao 的候选里: {cands:?}"
    );
}

/// 整句解码优于逐词贪心：k=1（贪心）在第一个位置选了局部最优的
/// 「你」，永远到不了「泥→门」这条件更好的整条路径；k=16 的
/// beam 能看到全局并选出 [泥,门]。
#[test]
fn whole_path_beam_beats_greedy() {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    engine.dict.upsert("泥", "ni", 90).expect("插入失败");
    engine.dict.upsert("门", "men", 50).expect("插入失败");
    engine.dict.upsert("们", "men", 50).expect("插入失败");
    let LmStore::Bigram(lm) = &mut engine.lm else {
        panic!("默认引擎应为 bigram（测试用 new(None) 创建）");
    };
    lm.add_unigram("你", 100);
    lm.add_unigram("泥", 10);
    lm.add_unigram("门", 100);
    // 泥→门 的强 bigram：这是贪心局部看不到的「未来」
    lm.add_bigram("泥", "门", 5000);

    let ctx = LmContext::empty();
    let greedy = engine.decode_paths("nimen", &ctx, 1).expect("解码失败");
    let beam = engine.decode_paths("nimen", &ctx, BEAM_WIDTH).expect("解码失败");

    assert_eq!(greedy[0].words, vec!["你", "门"], "贪心停在局部最优");
    assert_eq!(beam[0].words, vec!["泥", "门"], "beam 应找到全局最优");
    assert!(beam[0].score > greedy[0].score, "整句解码分应更高");

    // 可见面:候选排序由「以该候选开头的最优完整路径分」决定
    // (Block A)。「泥」的候选分 = 泥自身 + 补全「门」,「你」的
    // 候选分 = 你自身 + 补全「门」;两者后缀相同,而补全分由后向
    // DP 在无上文 LM 状态下近似(**看不到**「泥→门」的 bigram,
    // 只能拿「门」的 unigram 分——近似是刻意的,见
    // `backward_completion` 注释)。因此候选排序不再继承 beam 的
    // bigram 增益:「你」(unigram 100)排在「泥」(unigram 10)前。
    // 整句首选仍由前向 beam 的精确 bigram 状态决定(上面的
    // beam[0] == [泥,门]),两条口径互不干扰。
    let cands = engine
        .lookup_candidates("nimen", &ctx)
        .expect("查询失败");
    assert_eq!(
        cands[0].word, "你",
        "候选排序用近似 LM(无 bigram 增益):unigram 更高的「你」应排最前"
    );
}

/// 上文能改变整条路径的选择：无上文时 [你,门] 最优；
/// 上文「澡」命中 bigram(澡,泥) 后，[泥,门] 整体翻盘。
#[test]
fn context_changes_whole_path_choice() {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    engine.dict.upsert("泥", "ni", 90).expect("插入失败");
    engine.dict.upsert("门", "men", 50).expect("插入失败");
    let LmStore::Bigram(lm) = &mut engine.lm else {
        panic!("默认引擎应为 bigram（测试用 new(None) 创建）");
    };
    lm.add_unigram("你", 100);
    lm.add_unigram("泥", 10);
    lm.add_unigram("门", 100);
    lm.add_unigram("澡", 100);
    lm.add_bigram("澡", "泥", 900);

    let no_ctx = LmContext::empty();
    let with_ctx = LmContext { tail: "澡", last_word: Some("澡") };

    let before = engine.decode_paths("nimen", &no_ctx, BEAM_WIDTH).expect("解码失败");
    let after = engine.decode_paths("nimen", &with_ctx, BEAM_WIDTH).expect("解码失败");
    assert_eq!(before[0].words, vec!["你", "门"], "无上文：[你,门] 最优");
    assert_eq!(after[0].words, vec!["泥", "门"], "上文应改变整条路径的选择");

    // 可见面一致：上文让「泥」压过「你」
    let cands = engine
        .lookup_candidates("nimen", &with_ctx)
        .expect("查询失败");
    let rank = |word: &str| cands.iter().position(|c| c.word == word);
    assert!(
        rank("泥") < rank("你"),
        "上文命中 bigram 后「泥」应排在「你」前: {:?}",
        cands
    );
}

/// beam 宽度边界：k 决定保留的完整路径条数；候选词的来源被限制在
/// beam 的路径里（剪枝 = 候选召回的上界）。
#[test]
fn beam_width_bounds_exploration() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    engine.dict.upsert("逆", "ni", 90).expect("插入失败");
    engine.dict.upsert("妮", "ni", 80).expect("插入失败");
    let ctx = LmContext::empty();

    assert_eq!(engine.decode_paths("ni", &ctx, 0).expect("解码失败").len(), 0);
    assert_eq!(engine.decode_paths("ni", &ctx, 1).expect("解码失败").len(), 1);
    assert_eq!(engine.decode_paths("ni", &ctx, 2).expect("解码失败").len(), 2);
    assert_eq!(engine.decode_paths("ni", &ctx, 3).expect("解码失败").len(), 3);
    // 宽度只多不少：k=3 的前缀路径在 k=16 里都还在
    let narrow = engine.decode_paths("ni", &ctx, 3).expect("解码失败");
    let wide = engine.decode_paths("ni", &ctx, BEAM_WIDTH).expect("解码失败");
    for p in &narrow {
        assert!(
            wide.iter().any(|w| w.words == p.words),
            "k 增大后已有路径不应丢失: {:?}",
            p.words
        );
    }
    // 排序单调：首路径一致且得分非增
    assert_eq!(narrow[0].words, wide[0].words);
    assert!(narrow[0].score >= wide[0].score);

    // 产出的候选 (word, pinyin) 全部来自 beam 路径（剪枝即召回上界）
    let cands = engine
        .lookup_candidates("ni", &ctx)
        .expect("查询失败");
    let beam_words: Vec<(String, String)> = wide
        .iter()
        .flat_map(|p| p.words.iter().cloned().zip(p.pinyin.iter().cloned()))
        .collect();
    for c in &cands {
        assert!(
            beam_words.contains(&(c.word.clone(), c.pinyin.clone())),
            "候选 {} 应来自 beam 路径",
            c.word
        );
    }
}

/// 病态输入不超时、不爆炸：全同音/无音节/双歧义/超长串。
/// 结构上格构建被 MAX_CHAINS_PER_START 与 MAX_EDGE_SYLLABLES 截断，
/// 这里用宽松的墙上时间兜底（debug 构建下正常耗时也在毫秒级）。
#[test]
fn pathological_inputs_do_not_explode() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    // 喂点词，让格真的长出边来（而不是全空）
    for (w, py, f) in [
        ("啊", "a", 100),
        ("爱", "ai", 90),
        ("安", "an", 80),
        ("昂", "ang", 70),
        ("那", "na", 100),
        ("难", "nan", 90),
    ] {
        engine.dict.upsert(w, py, f).expect("插入失败");
    }
    let ctx = LmContext::empty();
    let cases = [
        "aaaaaaaaaaaa".to_string(),
        "zzzzzzzz".to_string(),
        "nananananana".to_string(),
        "anang".to_string(),
        "a".repeat(40),
        "n".repeat(30),
    ];
    for input in cases {
        let start = std::time::Instant::now();
        let cands = engine.lookup_candidates(&input, &ctx).expect("查询失败");
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "{input:?}（{} 字符）耗时 {elapsed:?}，疑似组合爆炸",
            input.chars().count()
        );
        assert!(cands.len() <= MAX_CANDIDATES, "{input:?} 候选数超上限");
    }
}

/// 万象 `.gram` 缺失时优雅退化（验收点 3 之一）：
/// `new_with_lm` 不应 panic / 报错，退回空 bigram，查询照常工作。
#[test]
fn wanxiang_missing_gram_degrades_gracefully() {
    let engine =
        ImeEngine::new_with_lm(None, Some(Path::new("/nonexistent/wanxiang.gram")))
            .expect("模型缺失时引擎仍应创建成功");
    assert!(
        matches!(engine.lm, LmStore::Bigram(_)),
        "缺失 .gram 应退回空 bigram"
    );
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    let cands = engine
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询不应失败");
    assert!(!cands.is_empty(), "退化后查询应正常工作");
}

/// `new_with_lm` 在 `.gram` 存在时应真的加载成 Wanxiang 变体，
/// 且候选查询、语境注入都通过枚举分发正常路由（真实文件测试，
/// 缺文件时自动跳过）。
#[test]
fn real_wanxiang_routes_through_engine() {
    let Some(dir) = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".local/share"))
    }) else {
        return;
    };
    let gram = dir.join("xwx").join("wanxiang-lts-zh-hans.gram");
    if !gram.is_file() {
        return; // 缺 .gram 时静默跳过(与其它真实文件测试同款)
    }
    let engine = ImeEngine::new_with_lm(None, Some(&gram)).expect("引擎创建失败");
    let LmStore::Wanxiang(_) = &engine.lm else {
        panic!("存在 .gram 时应加载为 Wanxiang 模型");
    };
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    engine.dict.upsert("好", "hao", 50).expect("插入失败");
    // 端到端：有上文的打分要能改变候选排序（「今天」会命中 real gram）
    let no_ctx = engine
        .lookup_candidates("ni", &LmContext::empty())
        .expect("无上文查询失败");
    let with_ctx = engine
        .lookup_candidates("ni", &LmContext { tail: "今天", last_word: None })
        .expect("有上文查询失败");
    assert!(!no_ctx.is_empty() && !with_ctx.is_empty());
}
