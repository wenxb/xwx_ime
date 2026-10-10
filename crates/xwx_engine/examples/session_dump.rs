//! 复现用户真实场景：**先打了中文，再打一个恰好也是拼音前缀的英文词**。
//!
//! `english_perf --dump` 用的是空 `LmContext`，而真实会话里前面有中文——
//! 语言模型的二元上下文会给中文候选加分。用户报告 `rug` 出来的是「如果」、
//! `bir` 出来的是「比如」，与空上下文的输出（`rug`/`bird` 排第一）矛盾；
//! 这里用**真实会话路径**（`InputSession`）复现，看上下文是不是变量。
//!
//! 用法：
//! ```text
//! cargo run --release -p xwx_engine --example session_dump -- \
//!   --data data --lm <gram> --after jintian --input rug,bir,app
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use xwx_engine::{EnglishDict, EnglishEngine, ImeEngine, InputEvent};

fn build(data: &PathBuf, lm: &PathBuf, english: Option<&PathBuf>) -> Result<ImeEngine, Box<dyn std::error::Error>> {
    let mut engine = ImeEngine::new_with_lm(Some(&data.join("dict.bin")), Some(lm))?;
    engine.dict = engine.dict.with_abbrev(data.join("abbrev.bin"));
    engine.dict = engine.dict.with_syllable_graph(data.join("syllable_graph.bin"));
    // **必须接上英文通道**：不接的话 `self.english` 是 None，`lookup_composition`
    // 直接走纯中文路径——那测的是另一回事（踩过一次，输出全中文让人误以为
    // 「英文被挤掉了」）。
    if let Some(p) = english {
        engine = engine.with_english(EnglishEngine::new(EnglishDict::open(p)?).with_predict(true));
    }
    Ok(engine)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let get = |name: &str, default: &str| -> String {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_else(|| default.to_string())
    };
    let data = PathBuf::from(get("--data", "data"));
    let lm = PathBuf::from(get("--lm", ""));
    let after = get("--after", "jintian");
    let inputs = get("--input", "rug,bir");

    let english = args
        .iter()
        .position(|a| a == "--english")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let engine = Arc::new(build(&data, &lm, english.as_ref())?);

    for input in inputs.split(',') {
        // 每个输入都从头来：先打一段中文并提交（造出真实上文），再打目标串。
        let mut s = engine.create_session();
        for ch in after.chars() {
            let _ = s.handle(InputEvent::Char(ch));
        }
        let _ = s.handle(InputEvent::Space); // 提交，制造中文上文
        let committed = String::new();

        for ch in input.chars() {
            let _ = s.handle(InputEvent::Char(ch));
        }
        let cands: Vec<String> = s
            .candidates()
            .iter()
            .take(60)
            .map(|c| {
                if c.is_english() {
                    format!("{}[en:{:?}]", c.word, c.english_match_kind())
                } else if c.echo {
                    format!("{}[echo]", c.word)
                } else {
                    format!("{}[zh]", c.word)
                }
            })
            .collect();
        println!("先打 {after:?}（上屏「{committed}」）再打 {input:?}:");
        println!("   {:?}\n", cands);
    }
    Ok(())
}
