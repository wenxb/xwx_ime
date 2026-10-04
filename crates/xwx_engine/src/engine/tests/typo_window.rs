//! 容错窗口的上限与每窗口读法数(第一/二阶段-A 的第一批):
//! 单音节打错一处里最长的一类是 5~6 字母音节(`xaing`/`zhuagn`),
//! 窗口上限 4 时这些窗口整个不开弧,只能把串切成碎音节拼垃圾候选;
//! 而每个窗口只留「最便宜一条读法」会让同价的不同读法由 `HashMap`
//! 迭代顺序决定(`mem` 的 `mei`/`men` 同价 2.0)。
//!
//! 现状(真词库 + 副索引 + 真 LM;缺文件时跳过):
//! - 窗口上限 6、每窗口按「代价并列一起留 + 名额有剩补一条不同编辑类别
//!   且不是窗口子序列的读法」;
//! - `xaing`/`zhuagn` 这类 5~6 字母窗口变成可纠;
//! - 打对的输入一律不碰(不建容错格)。

use super::*;

fn real_engine() -> Option<ImeEngine> {
    let dict = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict.is_file() || !abbrev.is_file() {
        return None;
    }
    let gram = gram_path()?;
    let mut engine = ImeEngine::new_with_lm(Some(&dict), Some(&gram)).ok()?;
    engine.dict = engine.dict.with_abbrev(&abbrev);
    Some(engine)
}

fn gram_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("XWX_LM_PATH") {
        let p = std::path::PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let home = std::env::var("HOME").ok()?;
    let p = std::path::PathBuf::from(home).join(".local/share/xwx/wanxiang-lts-zh-hans.gram");
    p.is_file().then_some(p)
}

/// 某个输入的前若干候选(顺序即排名)。
fn top(engine: &ImeEngine, input: &str, n: usize) -> Vec<String> {
    engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败")
        .into_iter()
        .take(n)
        .map(|c| c.word)
        .collect()
}

fn rank_of(engine: &ImeEngine, input: &str, word: &str) -> Option<usize> {
    engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败")
        .iter()
        .position(|c| c.word == word)
        .map(|i| i + 1)
}

/// 5 字母窗口内的调换:`xaing` → 想(用户报的场景)。
#[test]
fn five_letter_window_transposition_is_corrected() {
    let Some(engine) = real_engine() else {
        return;
    };
    assert_eq!(top(&engine, "xaing", 1), vec!["想"], "xaing 的首位应是「想」");
}

/// 6 字母窗口内的调换:`zhuagn` → 装(`uang` 内的 `a`/`u` 颠倒)。
#[test]
fn six_letter_window_transposition_is_corrected() {
    let Some(engine) = real_engine() else {
        return;
    };
    let r = rank_of(&engine, "zhuagn", "装").expect("zhuagn 应能纠到「装」");
    assert!(r <= 3, "「装」应在前三,实际 #{r}");
}

/// 漏打中间一个字母(`xang` → `xiang`):窗口里同时有十几条同价的替换读法
/// (`sang`/`bang`/…),名额必须留给**不同编辑类别**的读法。
///
/// 这里只钉「在候选里且进前 10」:读法进了格子之后谁排前由 LM 定,
/// 而 `xang` 的字面读法(`sang`,混淆对 x/s 只 0.7)本来就便宜——
/// 钉死名次会把 LM 的正常发挥当成回归。
#[test]
fn missed_medial_letter_reading_survives_the_substitution_crowd() {
    let Some(engine) = real_engine() else {
        return;
    };
    let r = rank_of(&engine, "xang", "想").expect("xang 应能纠到「想」");
    assert!(r <= 10, "「想」应在前十,实际 #{r}");
}

/// 代价并列的读法一起留:`mem` 的 `mei`/`men` 同价 2.0,
/// 只按字典序留一条会把「我们」丢掉。
#[test]
fn tied_readings_are_both_kept() {
    let Some(engine) = real_engine() else {
        return;
    };
    assert_eq!(top(&engine, "womem", 1), vec!["我们"], "womem 的首位应是「我们」");
    assert_eq!(top(&engine, "womemjintian", 1)[0], "我们今天");
}

/// 邻键误触 + 漏打型读法保底:`nohao` 的窗口 `no` 里最便宜的读法是
/// 混淆对 `lo`(0.10),漏打型 `nao` 占一个名额,邻键 `ni`(1.30)靠第三个
/// 名额进格子——「你好」由此登顶(名额 2 时它整个缺席)。
#[test]
fn neighbor_typo_with_omission_slot_reaches_nihao() {
    let Some(engine) = real_engine() else {
        return;
    };
    assert_eq!(
        top(&engine, "nohao", 1),
        vec!["你好"],
        "nohao(i 被邻键 o 打掉)的首位应是「你好」"
    );
}

/// 打对的输入一律不碰:窗口上限放宽只影响容错格,精确格逐字节不变。
#[test]
fn correctly_typed_inputs_are_untouched() {
    let Some(engine) = real_engine() else {
        return;
    };
    for (input, want) in [
        ("xiang", "想"),
        ("zhuang", "装"),
        ("women", "我们"),
        ("nihao", "你好"),
        ("womenjintianqugongyuan", "我们今天去公园"),
    ] {
        assert_eq!(top(&engine, input, 1), vec![want], "打对的 {input} 不该变");
    }
}
