//! 三套固定语料（Q3）：日常全拼 / 简拼混拼 / 逐键编辑。
//!
//! 语料是**验收资产**，必须进仓库（此前散在 `/tmp`，不算数）。本文件只校验
//! 语料本身：能解析、每条都有 target、kind 与文件名一致——**不对行为下断言**
//! （行为验收由指标脚本负责，见 `examples/eval_rank.rs`）。
//!
//! 判定线（用户已裁定）：目标词出现在**前 9 条**算「首屏命中」（与一屏
//! `page_size` 对齐）；`note` 里写明已知差距，不当作通过。

use std::path::Path;

const KINDS: [(&str, &str); 3] = [
    ("daily", "全拼日常"),
    ("mixed", "简拼/混拼"),
    ("edits", "打错/半成品尾巴"),
];

fn parse(path: &Path) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("读 {}: {e}", path.display()));
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // 极简 JSONL 取值：只认 "key":"value" 形式，避免为语料引入依赖。
        let get = |key: &str| -> Option<String> {
            let pat = format!("\"{key}\":\"");
            let start = line.find(&pat)? + pat.len();
            let rest = &line[start..];
            let end = rest.find('"')?;
            Some(rest[..end].to_string())
        };
        let input = get("input").unwrap_or_else(|| panic!("{}:{} 缺 input", path.display(), i + 1));
        let target = get("target").unwrap_or_else(|| panic!("{}:{} 缺 target", path.display(), i + 1));
        assert!(!input.is_empty() && !target.is_empty(), "{}:{} 空字段", path.display(), i + 1);
        out.push((input, target));
    }
    out
}

#[test]
fn three_corpora_exist_and_are_well_formed() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpora");
    for (kind, desc) in KINDS {
        let path = dir.join(format!("{kind}.jsonl"));
        assert!(path.is_file(), "缺语料 {desc}: {}", path.display());
        let items = parse(&path);
        assert!(items.len() >= 6, "{desc} 语料太小: {} 条", items.len());
        println!("{desc}: {} 条", items.len());
    }
}
