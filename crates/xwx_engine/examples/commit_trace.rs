//! 逐字符上屏轨迹:输入串 → 候选 → 连按空格(选第 1 个候选)每一步上屏了什么、
//! 剩下什么。用来复现「长串只有前半段被解释、后半段原样残留」这类报告。
//!
//! 用法:
//! ```sh
//! cargo run --release --offline -p xwx_engine --example commit_trace -- womenjintqubeij
//! ```
//! 不带参数时跑内置的一组对照输入。

use std::path::PathBuf;
use std::sync::Arc;

use xwx_engine::{ImeEngine, InputEvent, LmContext};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let inputs: Vec<String> = if args.is_empty() {
        [
            "womenjintqubeij",     // 用户报告:实际只解释出「我们今天」
            "womenjintianqubeij",  // 对照:同一意图写全一点
            "jintqubeij",          // 去掉开头的「我们」
            "jint",                // 单看这一段
            "beij",                // 半音节尾巴
            "womenjintianquxian",  // 已知良好基线
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    } else {
        args
    };

    let dir = PathBuf::from(std::env::var("HOME")?).join(".local/share/xwx");
    let mut engine = ImeEngine::new_with_lm(
        Some(&dir.join("dict.bin")),
        Some(&dir.join("wanxiang-lts-zh-hans.gram")),
    )?;
    engine.dict = engine.dict.with_abbrev(dir.join("abbrev.bin"));
    let engine = Arc::new(engine);

    for input in &inputs {
        println!("\n══════ {input} ══════");

        // —— 引擎层:候选表 ——
        let cands = engine.lookup_candidates(input, &LmContext::empty())?;
        println!("候选 {} 条,前 12:", cands.len());
        for (i, c) in cands.iter().enumerate() {
            println!(
                "  {i:>2}|{:<10}|{:<16}|freq={:<8}|层={:?}{}",
                c.word,
                c.pinyin,
                c.freq,
                c.spelling,
                if c.echo { "|echo" } else { "" }
            );
        }

        // —— 会话层:逐字符敲,然后连按空格 ——
        let mut s = engine.create_session();
        for ch in input.chars() {
            s.handle(InputEvent::Char(ch))?;
        }
        println!("打完 preedit={:?}", s.pinyin());
        let mut committed = String::new();
        for step in 1..=6 {
            let before = s.pinyin().to_string();
            if before.is_empty() {
                break;
            }
            let piece = s.commit_at(0);
            match piece {
                Some(p) => {
                    println!("  空格#{step}: 上屏 {p:?}  (preedit {before:?} → {:?})", s.pinyin());
                    committed.push_str(&p);
                }
                None => {
                    println!("  空格#{step}: 无候选可上屏,preedit 仍是 {:?}", s.pinyin());
                    break;
                }
            }
        }
        println!("连按空格结果: {committed:?}");
        println!("残留 preedit: {:?}", s.pinyin());
    }
    Ok(())
}
