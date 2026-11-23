//! 容错窗口（5-6 字母调换、漏打、同价并列、邻键与叠字母删字）及正确输入保护。

use super::*;

fn top1(engine: &ImeEngine, input: &str) -> String {
    engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败")
        .first()
        .map(|c| c.word.clone())
        .unwrap_or_default()
}

fn rank_of(engine: &ImeEngine, input: &str, word: &str) -> Option<usize> {
    engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败")
        .iter()
        .position(|c| c.word == word)
        .map(|i| i + 1)
}

/// 5-6 字母窗口调换、漏打、同价并列、邻键与叠字母删字均纠出目标词。
#[test]
fn typo_windows_and_doubled_letters_recover_intended_words() {
    let Some(engine) = real_full_engine() else {
        return;
    };
    for (input, want) in [
        ("xaing", "想"),
        ("womem", "我们"),
        ("womemjintian", "我们今天"),
        ("nohao", "你好"),
        ("shanghaai", "上海"),
    ] {
        assert_eq!(top1(engine, input), want, "{input} 首位应纠出「{want}」");
    }
    for (input, want, max_rank) in [("zhuagn", "装", 3usize), ("xang", "想", 10usize)] {
        let r = rank_of(engine, input, want).unwrap_or_else(|| panic!("{input} 应纠出「{want}」"));
        assert!(r <= max_rank, "{input} 的「{want}」应在前 {max_rank}，实际 #{r}");
    }
}

/// 打对的全拼输入不受容错与叠字母逻辑影响。
#[test]
fn correctly_typed_inputs_are_untouched() {
    let Some(engine) = real_full_engine() else {
        return;
    };
    for (input, want) in [
        ("xiang", "想"),
        ("zhuang", "装"),
        ("women", "我们"),
        ("nihao", "你好"),
        ("shanghai", "上海"),
        ("womenjintianqugongyuan", "我们今天去公园"),
        ("womenjintianquxian", "我们今天去西安"),
    ] {
        assert_eq!(top1(engine, input), want, "打对的 {input} 不应受扰动");
    }
}
