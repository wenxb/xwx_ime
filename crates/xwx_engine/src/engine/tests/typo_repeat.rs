//! 叠字母(多打一个字母)的纠错:拼音里没有 `aa`/`ii`/`oo` 这种写法,所以
//! 叠字母是「多打了一个字母」的**结构证据**——读法由结构决定,不由代价决定。
//!
//! 为什么需要它:`shanghaai`(= `shang` + `haai`)能切成 `shang`+`ha`+`ai`,
//! 精确路走得通、也没有卡死位置,所以「可达 / 能否切分」这两条结构判据都
//! 发现不了它(容错格压根没建,首位是逐字读法「上哈爱」);而按代价取读法时,
//! 窗口 `haai` 的替换读法 `huai`(0.01→代价 2.0)比删除读法 `hai`
//! (0.004→2.4)**便宜**,只留最便宜一条就把「上海」丢成「上坏/上怀」。

use super::*;

/// 真实词库 + 副索引 + 真实 LM(没有 .gram 时返回 None,调用方跳过——
/// 纠错读法与逐字读法的胜负由 LM 定,空模型里比不出结论)。
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

/// 叠字母窗口只认「删掉一个字母」的读法:`shanghaai` → 上海。
#[test]
fn doubled_letter_prefers_the_deletion_reading() {
    let Some(engine) = real_engine() else {
        return;
    };
    assert_eq!(
        first(&engine, "shanghaai"),
        "上海",
        "叠字母只可能是多打了一个字母,该窗口的正确读法是删掉它"
    );
}

/// 打对的输入一律不碰(叠字母分支不进)。
#[test]
fn correctly_typed_inputs_are_untouched() {
    let Some(engine) = real_engine() else {
        return;
    };
    assert_eq!(first(&engine, "shanghai"), "上海");
    assert_eq!(first(&engine, "nihao"), "你好");
    assert_eq!(first(&engine, "womenjintianquxian"), "我们今天去西安");
}
