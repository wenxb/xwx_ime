//! 语料指标（Q3）：召回 / MRR / 首屏命中 / 完成所需选择次数。
//!
//! **显式选生产同款装配**：词库 + 简拼副索引 + 万象 LM（+ 可选用户层），
//! 与 `src/main.rs` 同一批环境变量。**缺词库或 LM 直接报错退出**——绝不
//! 「没装万象还打印万象结果」。
//!
//! 用法：`XWX_LM_PATH=.../xx.gram cargo run --release --example eval_rank [-- <语料.jsonl>...]`

use std::path::{Path, PathBuf};

use xwx_engine::{ImeEngine, LmContext};

/// 首屏命中判定线：目标词在前 9 条（与一屏 `page_size` 对齐）。
const FIRST_SCREEN: usize = 9;

fn parse(path: &Path) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("读 {}: {e}", path.display()));
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let get = |key: &str| -> String {
            let pat = format!("\"{key}\":\"");
            let start = line.find(&pat).unwrap_or_else(|| panic!("{line} 缺 {key}")) + pat.len();
            let rest = &line[start..];
            rest[..rest.find('"').expect("字段未闭合")].to_string()
        };
        out.push((get("input"), get("target")));
    }
    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dict_path: PathBuf = std::env::var("XWX_DICT_PATH")
        .unwrap_or_else(|_| "data/dict.bin".into())
        .into();
    let abbrev_path: PathBuf = std::env::var("XWX_ABBREV_PATH")
        .unwrap_or_else(|_| "data/abbrev.bin".into())
        .into();
    let lm_path: PathBuf = std::env::var("XWX_LM_PATH")
        .map_err(|_| "未设置 XWX_LM_PATH：指标必须在生产同款装配下跑，缺 LM 不能出数")?
        .into();
    for (what, p) in [("词库", &dict_path), ("副索引", &abbrev_path), ("语言模型", &lm_path)] {
        if !p.is_file() {
            return Err(format!("缺{what}: {}", p.display()).into());
        }
    }

    let mut engine = ImeEngine::new_with_lm(Some(&dict_path), Some(&lm_path))?;
    engine.dict = engine.dict.with_abbrev(&abbrev_path);

    let corpora: Vec<PathBuf> = if std::env::args().len() > 1 {
        std::env::args().skip(1).map(PathBuf::from).collect()
    } else {
        ["daily", "mixed", "edits"]
            .iter()
            .map(|k| PathBuf::from(format!("tests/corpora/{k}.jsonl")))
            .collect()
    };

    for path in corpora {
        let items = parse(&path);
        let (mut recalled, mut hit, mut mrr, mut choices) = (0usize, 0usize, 0.0f64, 0.0f64);
        let mut misses = Vec::new();
        for (input, target) in &items {
            let cands = engine.lookup_candidates(input, &LmContext::empty())?;
            match cands.iter().position(|c| &c.word == target) {
                Some(rank) => {
                    recalled += 1;
                    mrr += 1.0 / (rank as f64 + 1.0);
                    if rank < FIRST_SCREEN {
                        hit += 1;
                    }
                    // 选择次数：整串读法一条空格；只覆盖前缀的按剩余串继续数。
                    let covered = cands[rank].covered;
                    choices += if covered >= input.len() { 1.0 } else { 2.0 };
                }
                None => misses.push((input.clone(), target.clone(), cands.iter().take(3).map(|c| c.word.clone()).collect::<Vec<_>>())),
            }
        }
        let n = items.len() as f64;
        println!(
            "\n== {}（{} 条）召回 {}/{} · MRR {:.3} · 首屏 {}/{} · 平均选择 {:.2}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            items.len(),
            recalled,
            items.len(),
            mrr / n,
            hit,
            items.len(),
            choices / n
        );
        for (input, target, top) in misses {
            println!("   未召回 {input} → 期望「{target}」/ 实际前 3 {top:?}");
        }
    }
    Ok(())
}
