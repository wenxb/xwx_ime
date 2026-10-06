//! 索引驱动的混合简拼读法（`engine/indexed_mixed.rs` + `dict/mixed_walk.rs`）。
//!
//! 这批用例钉的是打字模拟评测暴露的三类形状（真实词库 + 真实万象 gram，
//! 缺文件时跳过）：
//!
//! 1. **缩写位在开头/中间**（`dshzhang`→董事长、`yougbmen`→有关部门）——
//!    交错 DFS 的步数预算在这类形状上烧光，索引遍历不该有这个问题；
//! 2. **全拼部分含 `i`/`u`/`v`**（`meixdao`→没想到、`julbu`→俱乐部）——
//!    前三类门槛都要求「每个字母都能起一个音节」，这类输入连简拼格都不建；
//! 3. **折叠 / 全声母两种写法对称**（`jzd`≡`jzhd`、`zgr`≡`zhgr`）——
//!    约束在拿到词条拼音后逐位验证，不再靠查询期猜变体 + 事后校验。
//!
//! 同时钉住**不许误伤**的两面：容错纠错（`zhuagn`→装、`womem`→我们）与
//! 全拼输入（`xian`/`nihao`/`zhongguo` 首位不变）。

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
    // 生产同款装配：音节级转移索引在就挂上（缺件时引擎自动回退到逐次二分，
    // 行为不依赖它，但 `zmgai` 这类首字母串的链解码要靠它）。
    let graph = abbrev.with_file_name("syllable_graph.bin");
    if graph.is_file() {
        engine.dict = engine.dict.with_syllable_graph(&graph);
    }
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

/// 某个输入的前 n 个候选。
fn top(engine: &ImeEngine, input: &str, n: usize) -> Vec<String> {
    engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败")
        .into_iter()
        .take(n)
        .map(|c| c.word)
        .collect()
}

/// 目标词的名次（候选表里第几名）。
fn rank_of(engine: &ImeEngine, input: &str, want: &str) -> Option<usize> {
    engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败")
        .iter()
        .position(|c| c.word == want)
}

/// 缩写位在**开头 / 中间 / 末尾**的三种形状都必须整串命中；首位即目标。
#[test]
fn abbreviation_at_head_and_interior_reaches_the_target() {
    let Some(engine) = real_engine() else {
        return;
    };
    for (input, want) in [
        ("dshzhang", "董事长"),   // 1,1,0：头两个音节缩写
        ("dszhang", "董事长"),    // 1,1,0（折叠写法）
        ("dshizh", "董事长"),     // 1,0,1：首尾缩写、中间全拼
        ("shshgongs", "上市公司"), // 双声母缩写 + 全拼尾
        ("yougbmen", "有关部门"),  // 全拼含 u：前三类门槛之外
        ("ygbumen", "有关部门"),
        ("julbu", "俱乐部"),      // 全拼含 u
        ("meixdao", "没想到"),    // 全拼含 i
        ("kaizhle", "开展了"),
        ("wugyi", "五个亿"),      // 全拼含 u
        ("zmgai", "怎么改"),
    ] {
        let r = rank_of(&engine, input, want);
        assert!(
            r.is_some(),
            "{input} 应能召回「{want}」（索引遍历按「完整音节/缩写」对齐词条）: {:?}",
            top(&engine, input, 5)
        );
        assert_eq!(r, Some(0), "{input} 的「{want}」应在首位: {:?}", top(&engine, input, 5));
    }
}

/// 折叠（`jzd`/`zgr`）与全声母（`jzhd`/`zhgr`）两种写法**对称**：
/// 同一条代码路径，只差一位约束的校验。
#[test]
fn fold_and_digraph_spellings_are_symmetric() {
    let Some(engine) = real_engine() else {
        return;
    };
    let jzd = top(&engine, "jzd", 3);
    let jzhd = top(&engine, "jzhd", 3);
    assert_eq!(jzd, jzhd, "折叠与全声母写法的前 3 应一致");
    assert!(rank_of(&engine, "jzd", "校准点").is_some(), "jzd 应召回校准点: {jzd:?}");
    assert!(rank_of(&engine, "jzhd", "校准点").is_some(), "jzhd 应召回校准点: {jzhd:?}");

    let zgr = top(&engine, "zgr", 3);
    let zhgr = top(&engine, "zhgr", 3);
    assert_eq!(zgr[0], "中国人", "zgr 首位应是中国人: {zgr:?}");
    assert_eq!(zhgr[0], "中国人", "zhgr 首位应是中国人: {zhgr:?}");
}

/// **不许误伤容错纠错**：混合读法只放行带真实词频证据的词条
/// （`freq > FILLER_FREQ_TIER`），填充档读法无权否决容错层的正确读法。
///
/// 失败见证：去掉 `indexed_mixed.rs` 里的词频证据过滤 → `zhuagn` 按混合形态
/// 切出的填充档词条（转化功能，freq=100）覆盖整串 ⇒ 容错层的「装」被分数闸
/// 挡住，首位退回填充档读法。
#[test]
fn typo_correction_is_not_displaced_by_filler_readings() {
    let Some(engine) = real_engine() else {
        return;
    };
    for (input, want) in [("zhuagn", "装"), ("xaing", "想"), ("womem", "我们")] {
        let r = rank_of(&engine, input, want);
        assert!(
            r.is_some(),
            "{input} 的纠错读法「{want}」不该被混合读法挤掉: {:?}",
            top(&engine, input, 5)
        );
        assert_eq!(r, Some(0), "{input} 首位应是「{want}」: {:?}", top(&engine, input, 5));
    }
}

/// **全拼输入一个候选都不许动**：`reachable` 的输入不进索引驱动的混合通道
/// （结构判据，不是测量结论——见 `lookup.rs` 里 `!reachable` 的两道门槛）。
#[test]
fn reachable_inputs_are_untouched() {
    let Some(engine) = real_engine() else {
        return;
    };
    for (input, want) in [
        ("xian", "先"),
        ("nihao", "你好"),
        ("zhongguo", "中国"),
        ("womenjintianquxian", "我们今天去线"),
    ] {
        let r = rank_of(&engine, input, want);
        if want == "我们今天去线" {
            // 整句读法的首词不必是这条串；这里只要求前 10 里没有简拼层候选。
            let cands = engine
                .lookup_candidates(input, &LmContext::empty())
                .expect("查询失败");
            let n_abbrev = cands
                .iter()
                .take(10)
                .filter(|c| c.spelling == SpellingKind::Abbrev)
                .count();
            assert_eq!(n_abbrev, 0, "{input} 前 10 不该出现简拼层候选");
            continue;
        }
        assert_eq!(r, Some(0), "{input} 首位应是「{want}」: {:?}", top(&engine, input, 5));
    }
}
