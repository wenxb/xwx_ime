//! 整串精确命中优先的契约钉子(2026-10-12 排序分级 band 引入)。
//!
//! 规则:输入恰好是某个读法的**完整拼音**时,该读法在同桶 Exact 层内
//! 恒领先一切尾音节补全;补全只有证明是**鼻音截断**(`jin→jing` 类,
//! 见 `rank::nasal_completion`)才可经支配豁免回到层首。元音/其他截断
//! (`la→lai`、`ji→jie`)是「猜用户在打别的词」,永不越位。

use super::*;

/// 真实词库 + 副索引 + 真实 LM(没有 .gram 就跳过)。
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

fn position_of(engine: &ImeEngine, input: &str, word: &str) -> Option<usize> {
    engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败")
        .iter()
        .position(|c| c.word == word)
}

/// 元音截断的补全不得压过整串精确命中:`xiala` 打全了「下拉」的拼音。
#[test]
fn vowel_truncation_completion_never_beats_full_pinyin_entry() {
    let Some(engine) = real_engine() else {
        return;
    };
    let xiala = position_of(&engine, "xiala", "下拉").expect("xiala 应召回「下拉」");
    assert_eq!(xiala, 0, "「下拉」应是首位");
    let xialai = position_of(&engine, "xiala", "下来");
    let xiexin = position_of(&engine, "xiala", "下");
    if let (Some(lai), Some(xia)) = (xialai, xiexin) {
        assert!(lai < xia, "补全「下来」应排在碎片单字「下」之前(第 {} vs {})", lai + 1, xia + 1);
    }
    let jianji = position_of(&engine, "jianji", "剪辑").expect("jianji 应召回「剪辑」");
    assert_eq!(jianji, 0, "「剪辑」应是首位(「简介」是元音截断补全)");
}

/// 鼻音截断的支配豁免保留:`beiji`/`beijin` 少打的 n/g 照常由补全救回。
#[test]
fn nasal_truncation_dominance_is_preserved() {
    let Some(engine) = real_engine() else {
        return;
    };
    let beiji = position_of(&engine, "beiji", "北京").expect("beiji 应召回「北京」");
    assert_eq!(beiji, 0, "鼻音截断补全「北京」应是首位");
    let beijin = position_of(&engine, "beijin", "北京").expect("beijin 应召回「北京」");
    assert_eq!(beijin, 0, "鼻音截断补全「北京」应是首位");
}

/// 鼻音豁免与整串词条同层按分数竞争:`weixi` 的「微信」(鼻音)与
/// 「维系」(整串词条)都在层首,词频定序;元音补全(维修/危险)让位。
#[test]
fn weixin_leads_and_weixi_follows() {
    let Some(engine) = real_engine() else {
        return;
    };
    let weixin = position_of(&engine, "weixi", "微信").expect("weixi 应召回「微信」");
    let weixi = position_of(&engine, "weixi", "维系").expect("weixi 应召回「维系」");
    let weixiu = position_of(&engine, "weixi", "维修");
    assert_eq!(weixin, 0, "「微信」应是首位");
    assert_eq!(weixi, 1, "「维系」应是第 2 位");
    if let Some(w) = weixiu {
        assert!(w > weixi, "元音补全「维修」不得越过整串词条「维系」");
    }
}
