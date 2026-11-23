//! 索引驱动的混合简拼读法（缩写位在头/中/尾、折叠对称、整串链召回与边界保护）。

use super::*;

fn top(engine: &ImeEngine, input: &str, n: usize) -> Vec<String> {
    engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败")
        .into_iter()
        .take(n)
        .map(|c| c.word)
        .collect()
}

fn rank_of(engine: &ImeEngine, input: &str, want: &str) -> Option<usize> {
    engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败")
        .iter()
        .position(|c| c.word == want)
}

/// 缩写位在开头/中间/末尾的混拼形状均整串命中并居首位。
#[test]
fn abbreviation_at_head_and_interior_reaches_the_target() {
    let Some(engine) = real_full_engine() else {
        return;
    };
    for (input, want) in [
        ("dshzhang", "董事长"),
        ("dszhang", "董事长"),
        ("dshizh", "董事长"),
        ("shshgongs", "上市公司"),
        ("yougbmen", "有关部门"),
        ("julbu", "俱乐部"),
        ("meixdao", "没想到"),
        ("kaizhle", "开展了"),
        ("zmgai", "怎么改"),
    ] {
        assert_eq!(
            rank_of(engine, input, want),
            Some(0),
            "{input} 首位应是「{want}」: {:?}",
            top(engine, input, 5)
        );
    }
    for (input, want) in [("ygbumen", "有关部门"), ("wugyi", "五个亿")] {
        assert!(
            rank_of(engine, input, want).is_some_and(|r| r < 2),
            "{input} 前 2 应含「{want}」: {:?}",
            top(engine, input, 5)
        );
    }
}

/// 折叠（`jzd`/`zgr`）与全声母（`jzhd`/`zhgr`）两种写法对称。
#[test]
fn fold_and_digraph_spellings_are_symmetric() {
    let Some(engine) = real_full_engine() else {
        return;
    };
    let jzd = top(engine, "jzd", 3);
    let jzhd = top(engine, "jzhd", 3);
    assert_eq!(jzd, jzhd, "折叠与全声母写法前 3 应一致");
    assert!(rank_of(engine, "jzd", "校准点").is_some());
    assert!(rank_of(engine, "jzhd", "校准点").is_some());

    assert_eq!(top(engine, "zgr", 1), vec!["中国人"]);
    assert_eq!(top(engine, "zhgr", 1), vec!["中国人"]);
}

/// 混合读法填充档词条不得顶掉容错纠错（`zhuagn`→装），可达全拼不混入简拼层。
#[test]
fn filler_mixed_readings_do_not_displace_typos_or_reachable_pinyin() {
    let Some(engine) = real_full_engine() else {
        return;
    };
    assert_eq!(
        rank_of(engine, "zhuagn", "装"),
        Some(0),
        "zhuagn 首位应是「装」: {:?}",
        top(engine, "zhuagn", 5)
    );
    for (input, want) in [("xian", "先"), ("zhongguo", "中国")] {
        assert_eq!(rank_of(engine, input, want), Some(0), "{input} 首位不变");
    }
    let long = engine
        .lookup_candidates("womenjintianquxian", &LmContext::empty())
        .expect("查询失败");
    assert_eq!(
        long.iter()
            .take(10)
            .filter(|c| c.spelling == SpellingKind::Abbrev)
            .count(),
        0,
        "可达全拼前 10 不应出现简拼层"
    );
}

/// 混拼链、首字母+完整音节尾与整串简拼读法召回。
#[test]
fn mixed_chain_and_initial_plus_literal_tail_recall_targets() {
    let Some(engine) = real_full_engine() else {
        return;
    };
    assert_eq!(
        rank_of(engine, "jintqbj", "今天请病假"),
        Some(0),
        "jintqbj 首位应是「今天请病假」: {:?}",
        top(engine, "jintqbj", 5)
    );
    for (input, want) in [("jingcankao", "仅供参考"), ("eshouche", "二手车")] {
        assert!(
            rank_of(engine, input, want).is_some(),
            "{input} 应召回「{want}」: {:?}",
            top(engine, input, 10)
        );
    }
    for (input, want) in [("ztailan", "状态栏"), ("ztai", "状态")] {
        let top5 = top(engine, input, 5);
        assert!(top5.iter().any(|w| w == want), "{input} 前 5 应含「{want}」: {top5:?}");
    }
    let mixed_sentence = engine
        .lookup_candidates("womenjintqubeij", &LmContext::empty())
        .expect("查询失败");
    assert!(
        mixed_sentence.iter().any(|c| {
            c.covered == "womenjintqubeij".len()
                && c.spelling == SpellingKind::Abbrev
                && c.word == "我们今天去北京"
        }),
        "womenjintqubeij 应产出覆盖整串的「我们今天去北京」: {:?}",
        mixed_sentence
            .iter()
            .take(5)
            .map(|c| (&c.word, c.covered, c.spelling))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        mixed_sentence[0].covered,
        "womenjintqubeij".len(),
        "首位应覆盖整串以支持一次空格上屏"
    );
}
