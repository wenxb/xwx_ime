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
