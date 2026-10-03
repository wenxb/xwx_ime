//! 阶段 3B 的验收线：什么时候该让整句接管首位。
//!
//! 上游判据是 `has_reliable_phrase`（`script_translator.cc:503-508`）：
//! 「≥2 音节 且没有**可靠**的整串词条」才生成整句，并在流内排首位。
//! 现状把「有覆盖整串的词条」直接当成「可靠」——`nihao`（你好 freq=332885）
//! 与 `chuzaina` 被同等对待，于是两者都不生成整句：前者是对的，后者是错的
//! （用户要的是「出在哪」）。
//!
//! 所以这条用例把两端一起钉住，改判据时两头都不许弄坏。

use super::*;

#[test]
#[ignore = "阶段 3B 验收线：整串词条不可靠时整句应接管首位（当前红：chuzaina 首位是「杵在那」）"]
fn reliable_whole_string_entry_keeps_first_place_while_an_unreliable_one_yields_to_the_sentence() {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let mut engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);

    let first = |input: &str| -> String {
        engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败")
            .first()
            .map(|c| c.word.clone())
            .unwrap_or_default()
    };

    // 端一：覆盖整串的词条**可靠**（高频双音节词），必须留住首位。
    // 这一半在当前实现上就是绿的，是回归保险。
    assert_eq!(first("nihao"), "你好", "可靠整串词条不该被整句顶掉");

    // 端二：覆盖整串的词条**不可靠**（低频组合词），整句「出+在+哪」
    // 应接管首位。这一半当前是红的。
    assert_eq!(first("chuzaina"), "出在哪", "不可靠整串词条应让位给整句");
}
