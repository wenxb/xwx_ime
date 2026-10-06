//! 末尾半音节补全**进格子**之后的行为钉子。
//!
//! 补全本身早就有(候选池那条路:输入是候选拼音的前缀、最后一个音节没打完,
//! `beijin` → 北京)。池里只收**单词条**,所以前面还有词时就串不起来——
//! `nihaobeijin` 的首位是「你好被禁」、「womenjintianqugongyua」是
//! 「我们今天去公寓啊」。把补全做成格子里的读法弧之后,前面的词才能跟
//! 补全出来的词连成一条路径。
//!
//! 两条排除(都由结构决定,见 `lattice/build.rs` 的补全分支):
//! - 纯声母窗口(`zh`)还在打字中,不是「少打一个字母」;
//! - 窗口**自己就是音节**且还能切成 ≥2 个音节时(`xian` = 先 = `xi`+`an` 西安),
//!   不补全——否则 `womenjintianquxian` 的首位会变成「我们今天去想」。

use super::*;

/// 真实词库 + 副索引 + 真实 LM(没有 .gram 就跳过:补全读法能否胜出由 LM 定)。
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

fn first(engine: &ImeEngine, input: &str) -> String {
    engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败")
        .first()
        .map(|c| c.word.clone())
        .unwrap_or_default()
}

/// 补全读法要能与**前面的词**串成一条路径。
#[test]
fn tail_completion_composes_with_a_preceding_word() {
    let Some(engine) = real_engine() else {
        return;
    };
    assert_eq!(first(&engine, "nihaobeijin"), "你好北京", "末尾少打一个 g");
    assert_eq!(
        first(&engine, "womenjintianqugongyua"),
        "我们今天去公园",
        "末尾少打一个 n"
    );
}

/// 窗口自己能切成 ≥2 个音节时不许补全(`xian` = `xi`+`an` 西安)。
#[test]
fn splittable_complete_syllable_is_not_completed() {
    let Some(engine) = real_engine() else {
        return;
    };
    assert_eq!(first(&engine, "womenjintianquxian"), "我们今天去西安");
    assert_eq!(first(&engine, "quxian"), "曲线");
}

/// 精确命中优先于补全:补全是「猜测用户还没打完」,由排序层的**补全层**兜住
/// (排在全部精确命中之后);只有比同桶同层最强精确命中高出
/// [`COMPLETION_DOMINANCE`] 的补全才豁免。
#[test]
fn exact_match_ranks_above_completion() {
    let Some(engine) = real_engine() else {
        return;
    };
    assert_eq!(first(&engine, "qingxi"), "清晰", "补全读法(xing/xian)不得压住精确命中");
    assert_eq!(first(&engine, "xuexi"), "学习", "补全读法(xiao)不得压住精确命中");
    // 补全的豁免口是「证据优势 ≥ COMPLETION_DOMINANCE」:北京(f1119506)对
    // 被禁(f13150)差 1.93,远大于 1.0,照常登顶。
    assert_eq!(first(&engine, "beijin"), "北京", "支配性补全不得被压到精确命中之后");
}

fn letters(pinyin: &str) -> String {
    pinyin.chars().filter(|c| *c != ' ').collect()
}

/// 精确命中要**成块**排在补全之前,不只是第一位:打全了 `liji` 的用户不该在
/// 第一页看到一堆「猜他还没打完」的 `li jie`/`li jiang`/`li jing`。
///
/// 这里连**带上下文**一起钉:旧的数值惩罚(0.25)只在无上文时站得住,一带
/// 真实 LM 上下文就翻(实测上文「我」时 理解 #1、立即 #2、里脊 掉到第 11)。
#[test]
fn exact_matches_precede_completions_as_a_block() {
    let Some(engine) = real_engine() else {
        return;
    };
    for (tail, last_word) in [("", None), ("我", Some("我")), ("今天", Some("今天"))] {
        let ctx = LmContext { tail, last_word };
        let cands = engine.lookup_candidates("liji", &ctx).expect("查询失败");
        let is_exact = |c: &Candidate| letters(&c.pinyin).len() == 4;
        let first_completion = cands.iter().position(|c| !is_exact(c));
        let last_exact = cands.iter().rposition(is_exact);
        assert_eq!(cands[0].word, "立即", "上文 {tail:?}:首位应是精确命中");
        if let (Some(fc), Some(le)) = (first_completion, last_exact) {
            assert!(
                le < fc,
                "上文 {tail:?}:精确命中「{}」被补全「{}」插队",
                cands[le].word,
                cands[fc].word
            );
        }
    }
}
