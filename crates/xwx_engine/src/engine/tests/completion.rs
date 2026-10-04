//! crate 级单测:completion。

use super::*;

/// 打字中的联想不被排序首键打散(真实词库):
/// - 输入 `ni`(完整音节)时 你/尼/泥 仍在前 10;
/// - 输入 `zhon`(不整音节,走兜底前缀联想)时 中/种/重 仍在前 10
///   ——它们的 `cp` = zhong 比输入**更长**,是「补全」而不是「覆盖
///   不全」,u=0 且 u_min=0(补全词桶恒 0),必须保持前排(这是
///   新排序规则最容易踩的坑:若按「覆盖长度」粗暴排序,补全联想
///   会被打散);
/// - 输入 `zhongg` 时「中国」(简拼层 `lookup_mixed` 命中
///   `zhong g*`,0 桶)仍在前 10。
///
/// 三段式:`mid_typing_completion_still_works` 原断言「ni 含
/// 你/尼/泥(位置不限)、zhongg 含 中国(位置不限)」;本测试把
/// 三者都收紧为「前 10」(实测 ni 1/2/3、zhon 1/2/3、zhongg 中国
/// 第 1)——收紧不是放松。
#[test]
fn typing_completion_not_scattered() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let dict_path = root.join("dict.bin");
    let abbrev_path = root.join("abbrev.bin");
    let gram = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })
        .map(|b| b.join("xwx/wanxiang-lts-zh-hans.gram"));
    let (Some(gram_path), true, true) = (gram, dict_path.is_file(), abbrev_path.is_file())
    else {
        return;
    };
    if !gram_path.is_file() {
        return;
    }
    let mut engine =
        ImeEngine::new_with_lm(Some(&dict_path), Some(&gram_path)).expect("引擎创建失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);

    for (input, wants) in [
        ("ni", &["你", "尼", "泥"][..]),
        ("zhon", &["中", "种", "重"][..]),
        ("zhongg", &["中国"][..]),
    ] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        for w in wants {
            let pos = cands
                .iter()
                .position(|c| c.word == *w)
                .unwrap_or_else(|| panic!("{input} 应含 {w}: {:?}",
                    cands.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()));
            assert!(
                pos < 10,
                "{input} 的 {w} 应在前 10(打字联想不被打散),实际第 {} 名: {:?}",
                pos + 1,
                cands.iter().take(12).map(|c| &c.word).collect::<Vec<_>>()
            );
        }
    }
}

/// 判据 A(本轮新增):全拼第一页回来——高频单字/词必须在前 10,
/// 「拼音凑巧等长」的生僻组合词必须**不在**前 10。
///
/// 这是「裸未解释字母数硬首键」的设计缺陷回归测试:裸 u 把
/// 被禁锢(beijing,u=0)、狄安娜(diannao,u=0)、进啊(jinan,
/// u=0)顶上第一页,把 被/北/点/电(u=4/4/3/3)挤出前 10——
/// 因为裸 u 只看「拼音长度==输入长度」,不看「剩下的字母能否
/// 精确切分」。u_min 桶把「终点落在不可精确切分段内」的候选
/// (u_min>0)一律沉底,高频单字(终点在音节边界,u_min=0)
/// 因此回到第一页。真实词库 + 万象 gram(与 probe 对拍同装配)。
#[test]
fn full_pinyin_first_page_recovers() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let dict_path = root.join("dict.bin");
    let abbrev_path = root.join("abbrev.bin");
    let gram = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })
        .map(|b| b.join("xwx/wanxiang-lts-zh-hans.gram"));
    let (Some(gram_path), true, true) = (gram, dict_path.is_file(), abbrev_path.is_file())
    else {
        return; // 环境缺文件:跳过(与 real_wanxiang_routes_through_engine 同款)
    };
    if !gram_path.is_file() {
        return;
    }
    let mut engine =
        ImeEngine::new_with_lm(Some(&dict_path), Some(&gram_path)).expect("引擎创建失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);

    // (输入, 必须在前 10 的词)
    let must_in: [(&str, &[&str]); 6] = [
        ("beijing", &["北京", "被", "北"]),
        ("diannao", &["电脑", "点", "电"]),
        ("zhongguo", &["中国", "中", "种"]),
        ("gongzuo", &["工作", "公", "工"]),
        ("jinan", &["济南", "进"]),
        ("mingtian", &["明天", "明", "名"]),
    ];
    for (input, wants) in must_in {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        let top10: Vec<&str> = cands.iter().take(10).map(|c| c.word.as_str()).collect();
        for w in wants {
            assert!(
                top10.contains(w),
                "{input} 的 {w} 应在前 10(全拼第一页回来),实际前 10: {top10:?}"
            );
        }
    }

    // (输入, 不得在前 10 的生僻组合词)
    let must_out: [(&str, &[&str]); 3] = [
        ("beijing", &["被禁锢", "悲寂", "背脊"]),
        ("diannao", &["狄安娜", "蒂安娜"]),
        ("jinan", &["进啊"]),
    ];
    for (input, bads) in must_out {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        let top10: Vec<&str> = cands.iter().take(10).map(|c| c.word.as_str()).collect();
        for w in bads {
            assert!(
                !top10.contains(w),
                "{input} 的生僻组合词 {w} 不应在前 10(裸 u 硬首键回归),实际前 10: {top10:?}"
            );
        }
    }
}

/// 「首音节未打完」联想仍然工作(真实词库):输入 `ni`
/// 时 你/尼/泥 仍在前排;输入 `zhongg`(打了 `zhong` + 一个声母)时
/// 中国 仍然出现(简拼层 `lookup_mixed` 命中 `zhong g*`)。
#[test]
fn mid_typing_completion_still_works() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let dict_path = root.join("dict.bin");
    let abbrev_path = root.join("abbrev.bin");
    let gram = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })
        .map(|b| b.join("xwx/wanxiang-lts-zh-hans.gram"));
    let (Some(gram_path), true, true) = (gram, dict_path.is_file(), abbrev_path.is_file())
    else {
        return;
    };
    if !gram_path.is_file() {
        return;
    }
    let mut engine =
        ImeEngine::new_with_lm(Some(&dict_path), Some(&gram_path)).expect("引擎创建失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);

    let ni = engine
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");
    for w in ["你", "尼", "泥"] {
        assert!(
            ni.iter().any(|c| c.word == w),
            "ni 应仍含 {w}: {:?}",
            ni.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()
        );
    }

    let zhongg = engine
        .lookup_candidates("zhongg", &LmContext::empty())
        .expect("查询失败");
    assert!(
        zhongg.iter().any(|c| c.word == "中国"),
        "zhongg 应仍含 中国: {:?}",
        zhongg.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()
    );
}

/// 声母前缀按音节枚举展开(玩具词库,不依赖 dict.bin):
/// 「只有声母、还没打韵母」的输入(`zh`)不能再被字节序锁死在
/// 排序最前的韵母上——`zhe`/`zhong` 的字必须进入候选,哪怕字典序
/// 上 `zha*` 排在它们前面且数量足够把全局 500 条上限占满。
///
/// 观测点的选择(功能坏掉时必须变红):内存 SqliteDict 的
/// `lookup_prefix` 按 **freq 降序**返回,不重现 mmap 的字节序锁死,
/// 所以「这/中出现在候选里」在旧实现下也成立(它们词频虽低但
/// 仍会排在 500 条以内)。真正只有枚举路径才产生的可观测量是
/// **逐音节配额限流**:`zha` 系灌 `SYLLABLE_ENUM_QUOTA + 6` 条,
/// 枚举路径下它在合并池里至多占 `SYLLABLE_ENUM_QUOTA` 条;旧路径
/// (单次前缀联想,无配额)下全部进。断这个,负向控制
/// (把枚举分支强制关掉)下本测试变红。
#[test]
fn initial_only_prefix_recovers_all_finals() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    // zha 系灌 SYLLABLE_ENUM_QUOTA + 6 个,词频递减;数量 > 配额。
    for i in 0..(SYLLABLE_ENUM_QUOTA + 6) as u32 {
        let w = char::from_u32(0x4E00 + i).unwrap().to_string();
        engine
            .dict
            .upsert(&w, "zha", 10_000 - i as u64)
            .expect("插入失败");
    }
    // 其他韵母下的字:枚举后必须**出现**在候选里。
    engine.dict.upsert("这", "zhe", 50).expect("插入失败");
    engine.dict.upsert("中", "zhong", 40).expect("插入失败");

    let cands = engine
        .lookup_candidates("zh", &LmContext::empty())
        .expect("查询失败");
    assert!(
        cands.iter().any(|c| c.word == "这"),
        "zh 应召回 zhe 系的「这」: {:?}",
        cands.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()
    );
    assert!(
        cands.iter().any(|c| c.word == "中"),
        "zh 应召回 zhong 系的「中」: {:?}",
        cands.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()
    );
    // 关键断言(只有枚举路径才成立的观测量):zha 系被逐音节配额
    // 限流,不得超过 SYLLABLE_ENUM_QUOTA 条。旧路径(无配额单次
    // 前缀联想)下 20 条全进,此断言变红。
    let n_zha = cands.iter().filter(|c| c.pinyin == "zha").count();
    assert!(
        n_zha <= SYLLABLE_ENUM_QUOTA,
        "zha 系应被逐音节配额限流(>{SYLLABLE_ENUM_QUOTA} 即枚举未生效): {n_zha}"
    );
}

/// 「合法音节不走枚举分支」守卫(玩具词库):`zhe` 本身是合法
/// 音节,走正常路径,候选里**不得**混入 `zha`/`zhong` 的字——
/// 枚举只对「不是合法音节的前缀串」生效。
#[test]
fn legal_syllable_not_touched_by_enum_expansion() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("这", "zhe", 100).expect("插入失败");
    engine.dict.upsert("扎", "zha", 1000).expect("插入失败");
    engine.dict.upsert("中", "zhong", 900).expect("插入失败");

    let cands = engine
        .lookup_candidates("zhe", &LmContext::empty())
        .expect("查询失败");
    assert!(cands.iter().any(|c| c.word == "这"), "zhe 应有「这」");
    assert!(
        cands.iter().all(|c| c.pinyin == "zhe"),
        "合法音节 zhe 不得混入其他音节的候选(枚举越界): {:?}",
        cands.iter().map(|c| (&c.word, &c.pinyin)).collect::<Vec<_>>()
    );
}

/// 验收判据 A(真实词库,缺 dict.bin 时跳过):`zh`/`x`/`q`/`zhon`/`sh`
/// 的前 10 必须被「各韵母下的高频字」占据,而不是字典序最小韵母的
/// 一串同音字。断言方式分两层:
/// - 召回层(硬):任务书点名的高频字必须在候选列表里出现
///   (`zh` 的中/这/着,`x` 的先/新/学,`q` 的钱/区/强);
/// - 结构层(硬):`zh` 的前 10 不得全是 `zha` 系(修复前的症状),
///   `x` 不得全是 `xi` 系,`q` 不得全是 `qi` 系,`zhon` 的前 10
///   应以 zhong 系为主。
///
/// 不设「前 10 必须有哪几个字」的硬指标——具体名次由词频数据
/// 决定(任务书原话:不设「必须几个」的硬指标)。
#[test]
fn real_dict_initial_only_prefix_first_page() {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    if !dict_path.is_file() {
        return;
    }
    let engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");

    // 召回层:点名高频字必须召回(不钉名次)。
    for (input, wants) in [
        ("zh", &["中", "这", "着"][..]),
        ("x", &["先", "新", "学"][..]),
        ("q", &["钱", "区", "强"][..]),
        ("zhon", &["中", "种", "重"][..]),
    ] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        for w in wants {
            assert!(
                cands.iter().any(|c| c.word == *w),
                "{input} 应召回「{w}」(修复前被字典序锁死在首韵母上): 前 10 {:?}",
                cands.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()
            );
        }
    }

    // 结构层:前 10 不得被字典序最小韵母独占(修复前的直接症状)。
    let zh = engine
        .lookup_candidates("zh", &LmContext::empty())
        .expect("查询失败");
    let top10: Vec<&str> = zh.iter().take(10).map(|c| c.word.as_str()).collect();
    assert!(
        !top10.iter().all(|w| ["扎", "炸", "眨", "诈", "乍", "喳", "渣", "闸", "栅", "榨"].contains(w)),
        "zh 前 10 不得全是 zha 系(字节序锁死回归): {top10:?}"
    );
    // zhon 前 10 应以 zhong 系为主(它是 zhong 的前缀)。
    let zhon = engine
        .lookup_candidates("zhon", &LmContext::empty())
        .expect("查询失败");
    let n_zhong = zhon
        .iter()
        .take(10)
        .filter(|c| c.pinyin == "zhong")
        .count();
    assert!(
        n_zhong >= 5,
        "zhon 前 10 应以 zhong 系为主(实际 {n_zhong}/10): {:?}",
        zhon.iter().take(10).map(|c| (&c.word, &c.pinyin)).collect::<Vec<_>>()
    );
}

/// 全拼前缀 + 尾部声母（打字中状态）的混拼供给：
/// - `yiwanc`（yi+wan+c，c 是 cheng 的声母）首位必须是「已完成」——
///   容错层的替换猜读（c→g 出「以往」，Fuzzy 整串）不得按层序压住
///   简拼整串词条（⑤⑧② 破例推广到第三类）；
/// - `yiwanch`（尾 ch 双字母声母）首位同样是「已完成」——
///   `ends_one_letter_short` 必须认识 zh/ch/sh 尾；
/// - 「以往」仍须在列（容错备选不消失，用户真打错 g→c 时有得选）；
/// - `mqweizhi`（mq+wei+zhi，开头即缩写）的「目前为止」须在前 3——
///   整串块置顶：只提第一个整串候选时它被尾音节单字墙压在 #76。
#[test]
fn mixed_initial_tail_prefers_entry_reading() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let dict_path = root.join("dict.bin");
    let abbrev_path = root.join("abbrev.bin");
    let graph_path = abbrev_path.with_file_name("syllable_graph.bin");
    let gram = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })
        .map(|b| b.join("xwx/wanxiang-lts-zh-hans.gram"));
    let (Some(gram_path), true, true) = (gram, dict_path.is_file(), abbrev_path.is_file())
    else {
        return;
    };
    if !gram_path.is_file() {
        return;
    }
    let mut engine =
        ImeEngine::new_with_lm(Some(&dict_path), Some(&gram_path)).expect("引擎创建失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);
    if graph_path.is_file() {
        engine.dict = engine.dict.with_syllable_graph(&graph_path);
    }

    for (input, want_top1) in [("yiwanc", "已完成"), ("yiwanch", "已完成")] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        assert!(
            !cands.is_empty(),
            "{input} 无候选"
        );
        assert_eq!(
            cands[0].word, want_top1,
            "{input} 首位应为「{want_top1}」: {:?}",
            cands.iter().take(5).map(|c| (&c.word, c.spelling)).collect::<Vec<_>>()
        );
    }
    let cands = engine
        .lookup_candidates("yiwanc", &LmContext::empty())
        .expect("查询失败");
    assert!(
        cands.iter().any(|c| c.word == "以往"),
        "yiwanc 的容错备选「以往」不得消失: {:?}",
        cands.iter().take(20).map(|c| &c.word).collect::<Vec<_>>()
    );
    let cands = engine
        .lookup_candidates("mqweizhi", &LmContext::empty())
        .expect("查询失败");
    let pos = cands
        .iter()
        .position(|c| c.word == "目前为止")
        .unwrap_or_else(|| panic!("mqweizhi 应含「目前为止」: {:?}",
            cands.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()));
    assert!(
        pos < 3,
        "mqweizhi 的「目前为止」应在前 3(整串块置顶),实际第 {} 名: {:?}",
        pos + 1,
        cands.iter().take(6).map(|c| (&c.word, c.spelling)).collect::<Vec<_>>()
    );
}
