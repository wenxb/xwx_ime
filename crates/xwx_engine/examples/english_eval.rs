//! 英文 M1 评测工具：真实中文装配 + 真实英文词库 + JSONL 语料。
//!
//! 输出（见 `docs/english-input/05-execution-tests.md` 第 15/16 节）：
//! 词表覆盖率、精确词 Top-1/Top-3、补全 Top-3、中文保护集 Top-1/Top-9、
//! 每条失败的实际候选与意图证据、OOV 单列（不从分母删除）。
//!
//! 用法：
//! ```text
//! cargo run --release --offline -p xwx_engine --example english_eval -- \
//!   --data /path/to/chinese-data --english data/english.bin \
//!   --corpus tests/corpora/english_acceptance.jsonl --mode on
//! ```
//!
//! **缺数据必须失败**：中文 dict/abbrev/gram 或英文词库缺任何一个都直接
//! 报错退出，绝不退回空模型后输出漂亮数字。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use xwx_engine::candidate::EnglishMatchKind;
use xwx_engine::english::{EnglishBigram, EnglishContext, EnglishDict, EnglishEngine};
use xwx_engine::{ImeEngine, LmContext};

/// 首屏命中判定线（与一屏 `page_size` 对齐）。
const FIRST_SCREEN: usize = 9;

#[derive(Debug, Deserialize)]
struct Row {
    id: String,
    kind: String,
    input: String,
    target: String,
    intent: String,
    #[serde(default)]
    previous_english: Option<String>,
    max_rank: usize,
    #[serde(default)]
    expected_first_language: Option<String>,
    #[serde(default)]
    note: String,
}

/// 一条语料的评测结果。
struct Outcome {
    id: String,
    category: Category,
    hit_rank: Option<usize>,
    oov: bool,
    first_lang: &'static str,
    first_lang_ok: Option<bool>,
    actual: Vec<String>,
    /// 目标在**英文通道内部**的名次（诊断用：区分「没召回」与「混排没给位」）。
    en_rank: Option<usize>,
    /// 首要英文候选（key + 静态频率），用于解释 good_en 为什么为假。
    e0: Option<String>,
    /// E0 的静态频率（百分之一 Zipf）——`good_en` 的另一半判据。
    e0_zipf: Option<u16>,
    note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Category {
    /// M2 零输入下一词。
    NextWord,
    /// 输入就是完整单词。
    Exact,
    /// 输入是完整单词的前缀。
    Completion,
    /// 中文保护集（`intent == chinese`）。
    ChineseGuard,
}

/// 解析命令行 `--key value`。
fn arg(name: &str, required: bool) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let pos = args.iter().position(|a| a == name)?;
    Some(args.get(pos + 1).cloned().unwrap_or_else(|| panic!("{name} 缺参数")))
        .filter(|v| {
            if required && v.is_empty() {
                panic!("{name} 不能为空");
            }
            true
        })
}

/// 找语言模型：`--data` 目录里的 `.gram`（其次 `bigram.txt`）→
/// `$XWX_LM_PATH` → `$XDG_DATA_HOME/xwx/*.gram`。
///
/// 三级都找不到就**失败**：缺数据时退回空模型会输出「漂亮但无意义」的
/// 数字（那正是本工具要避免的事）。回落到显式给出的 LM 不是降级。
fn find_lm(dir: &Path) -> Option<PathBuf> {
    let mut grams: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "gram"))
        .collect();
    grams.sort();
    if let Some(p) = grams.into_iter().next() {
        return Some(p);
    }
    let txt = dir.join("bigram.txt");
    if txt.is_file() {
        return Some(txt);
    }
    if let Ok(p) = std::env::var("XWX_LM_PATH") {
        return Some(PathBuf::from(p));
    }
    let xdg = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    let xwx = xdg.join("xwx");
    let mut grams: Vec<PathBuf> = std::fs::read_dir(&xwx)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "gram"))
        .collect();
    grams.sort();
    grams.into_iter().next()
}

fn read_corpus(path: &Path) -> Vec<Row> {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("读语料 {}: {e}", path.display()));
    let mut rows = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("//") {
            continue;
        }
        // 正式 serde_json 解析：不复制旧 corpus.rs 的字符串截取解析器。
        let row: Row = serde_json::from_str(t)
            .unwrap_or_else(|e| panic!("{}:{} JSON 解析失败: {e}", path.display(), i + 1));
        rows.push(row);
    }
    rows
}

/// 候选的语言归属（与混排实现同一判据，不靠 `is_ascii` 猜）。
fn language_of(c: &xwx_engine::Candidate) -> &'static str {
    if c.echo {
        "echo"
    } else if c.is_english() {
        "english"
    } else {
        "chinese"
    }
}

/// 目标是否命中：英文行要求元数据 + 完整 key 相等；中文行要求词面相等。
fn hit_rank(cands: &[xwx_engine::Candidate], row: &Row) -> (Option<usize>, bool) {
    let target = &row.target;
    let chinese_guard = row.intent == "chinese";
    for (i, c) in cands.iter().enumerate() {
        if chinese_guard {
            if !c.echo && !c.is_english() && &c.word == target {
                return (Some(i + 1), false);
            }
        } else if let Some(key) = c.english_key() {
            // 仅 word 文本相等的原文 echo 不算英文候选命中。
            if key == target.to_ascii_lowercase() {
                return (Some(i + 1), false);
            }
        }
    }
    (None, false)
}

fn evaluate(engine: &ImeEngine, en: Option<&EnglishDict>, rows: &[Row]) -> Vec<Outcome> {
    let mut out = Vec::new();
    for row in rows {
        if row.kind == "next_word" {
            // M2 的**独立预测入口**：不给空串组合查询，直接用上一词召回。
            let ctx = match row.previous_english.as_deref() {
                Some(prev) => EnglishContext::with_previous(prev),
                None => EnglishContext::empty(),
            };
            let cands = engine
                .english()
                .and_then(|en| en.next_candidates(&ctx).ok())
                .unwrap_or_default();
            let rank = cands
                .iter()
                .position(|c| c.english_key() == Some(row.target.to_ascii_lowercase().as_str()))
                .map(|i| i + 1);
            let first_lang = cands.first().map(language_of).unwrap_or("none");
            let first_lang_ok = row
                .expected_first_language
                .as_deref()
                .map(|want| want == first_lang);
            out.push(Outcome {
                id: row.id.clone(),
                category: Category::NextWord,
                hit_rank: rank,
                oov: false,
                first_lang,
                first_lang_ok,
                actual: cands.iter().take(6).map(|c| c.word.clone()).collect(),
                en_rank: rank,
                e0: cands.first().map(|c| c.word.clone()),
                e0_zipf: None,
                note: row.note.clone(),
            });
            continue;
        }
        if row.kind != "query" {
            // 未知的 kind：如实列出而不是悄悄从分母里删掉。
            out.push(Outcome {
                id: row.id.clone(),
                category: Category::Exact,
                hit_rank: None,
                oov: false,
                first_lang: "n/a",
                first_lang_ok: None,
                actual: vec![format!("未知 kind={}", row.kind)],
                en_rank: None,
                e0: None,
                e0_zipf: None,
                note: row.note.clone(),
            });
            continue;
        }
        let ctx = match row.previous_english.as_deref() {
            Some(prev) => EnglishContext::with_previous(prev),
            None => EnglishContext::empty(),
        };
        let cands = engine
            .lookup_composition(&row.input, &LmContext::empty(), &ctx)
            .unwrap_or_else(|e| panic!("{}: 查询失败: {e}", row.id));

        // 覆盖率只对**英文目标**有意义：中文行（保护集）的目标是汉字。
        let oov = en.is_some_and(|d| {
            row.intent != "chinese" && !d.contains(&row.target.to_ascii_lowercase())
        });
        let (rank, _) = hit_rank(&cands, row);
        // 英文通道内部的相对名次：与混排名次对比，就能分清「召回没进来」
        // 和「进来了但混排把它排到后面」。
        let en_only: Vec<&xwx_engine::Candidate> =
            cands.iter().filter(|c| c.is_english()).collect();
        let en_rank = en_only
            .iter()
            .position(|c| c.english_key() == Some(row.target.to_ascii_lowercase().as_str()))
            .map(|i| i + 1);
        let e0_zipf = en_only
            .first()
            .and_then(|c| c.english_key())
            .and_then(|k| en.and_then(|d| d.exact(k).ok().flatten()))
            .map(|e| e.zipf_centi);
        let e0 = en_only.first().map(|c| {
            format!(
                "{}[{}]",
                c.english_key().unwrap_or("?"),
                cands
                    .iter()
                    .find(|x| x.english_key() == c.english_key())
                    .map(|x| x.score)
                    .unwrap_or(0.0)
            )
        });
        let first_lang = cands.first().map(language_of).unwrap_or("none");
        let first_lang_ok = row
            .expected_first_language
            .as_deref()
            .map(|want| want == first_lang);
        let category = if row.intent == "chinese" {
            Category::ChineseGuard
        } else if row.input.eq_ignore_ascii_case(&row.target) {
            Category::Exact
        } else {
            Category::Completion
        };
        out.push(Outcome {
            id: row.id.clone(),
            category,
            hit_rank: rank,
            oov,
            first_lang,
            first_lang_ok,
            actual: cands
                .iter()
                .take(14)
                .map(|c| format!("{}({})", c.word, language_of(c)))
                .collect(),
            en_rank,
            e0,
            e0_zipf,
            note: row.note.clone(),
        });
    }
    out
}

/// `(命中@≤n, 总数)`。
fn rate(out: &[Outcome], cat: Category, n: usize) -> (usize, usize) {
    let picked: Vec<&Outcome> = out.iter().filter(|o| o.category == cat).collect();
    let hit = picked
        .iter()
        .filter(|o| o.hit_rank.is_some_and(|r| r <= n))
        .count();
    (hit, picked.len())
}

fn pct(hit: usize, total: usize) -> String {
    if total == 0 {
        "n/a".into()
    } else {
        format!("{:.1}%", hit as f64 * 100.0 / total as f64)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = PathBuf::from(arg("--data", true).expect("--data 必填（中文装配目录）"));
    let english_path = PathBuf::from(arg("--english", true).expect("--english 必填"));
    let corpus = PathBuf::from(arg("--corpus", true).expect("--corpus 必填"));
    let mode = arg("--mode", true).expect("--mode 必填（on/off）");
    if mode != "on" && mode != "off" {
        return Err(format!("--mode 只认 on/off，实际 {mode}").into());
    }

    // —— 数据齐备性：缺任何一个直接失败 ——
    let dict_path = data_dir.join("dict.bin");
    let abbrev_path = data_dir.join("abbrev.bin");
    let lm_path = find_lm(&data_dir).unwrap_or_else(|| panic!("找不到语言模型（--data/\\$XWX_LM_PATH/XDG 都没有 .gram）"));
    for (what, p) in [
        ("中文词库", &dict_path),
        ("简拼副索引", &abbrev_path),
        ("语言模型", &lm_path),
        ("英文词库", &english_path),
        ("语料", &corpus),
    ] {
        if !p.is_file() {
            return Err(format!("缺{what}: {}", p.display()).into());
        }
    }

    let mut engine = ImeEngine::new_with_lm(Some(&dict_path), Some(&lm_path))?;
    engine.dict = engine.dict.with_abbrev(&abbrev_path);
    engine.dict = engine.dict.with_syllable_graph(data_dir.join("syllable_graph.bin"));

    let en_dict = EnglishDict::open(&english_path)?;
    let words = en_dict.word_count();
    let prefixes = en_dict.prefix_count();
    let en_bytes = en_dict.file_len();
    let bigram_path = arg("--bigram", false).map(PathBuf::from);
    if mode == "on" {
        // 默认英文/中文用户层都是内存实例：**不读真实个人用户文件**。
        let mut en = EnglishEngine::new(EnglishDict::open(&english_path)?)
            // 零输入下一词面板必须打开，否则 next_word 行会全部落空。
            .with_predict(true);
        if let Some(p) = bigram_path.as_ref().filter(|p| p.is_file()) {
            en = en.with_bigram(EnglishBigram::open(p)?);
        }
        engine = engine.with_english(en);
    }
    let rows = read_corpus(&corpus);

    println!("== 装配 ==");
    println!("中文数据目录: {}", data_dir.display());
    println!("语言模型    : {}", lm_path.display());
    println!(
        "英文词库    : {} ({} 词 / {} 前缀 / {} 字节)",
        english_path.display(),
        words,
        prefixes,
        en_bytes
    );
    println!("英文开关    : {mode}");
    println!("语料        : {} ({} 行)", corpus.display(), rows.len());

    let out = evaluate(&engine, Some(&en_dict), &rows);

    println!("\n== 指标（mode={mode}）==");
    let (h1, t1) = rate(&out, Category::Exact, 1);
    let (h3, _) = rate(&out, Category::Exact, 3);
    let (c3, tc) = rate(&out, Category::Completion, 3);
    let (g1, tg) = rate(&out, Category::ChineseGuard, 1);
    let (g9, _) = rate(&out, Category::ChineseGuard, FIRST_SCREEN);
    let (n1, tn) = rate(&out, Category::NextWord, 1);
    let (n3, _) = rate(&out, Category::NextWord, 3);
    println!("精确词 Top-1 : {h1}/{t1} = {}", pct(h1, t1));
    println!("精确词 Top-3 : {h3}/{t1} = {}", pct(h3, t1));
    println!("补全   Top-3 : {c3}/{tc} = {}", pct(c3, tc));
    println!("下一词 Top-1 : {n1}/{tn} = {}", pct(n1, tn));
    println!("下一词 Top-3 : {n3}/{tn} = {}", pct(n3, tn));
    println!("中文保护 Top-1: {g1}/{tg} = {}", pct(g1, tg));
    println!("中文保护 Top-9: {g9}/{tg} = {}", pct(g9, tg));
    let (hit_all, total_all) = (out.iter().filter(|o| o.hit_rank.is_some()).count(), out.len());
    println!("整体命中     : {hit_all}/{total_all} = {}", pct(hit_all, total_all));

    let oov: Vec<&Outcome> = out.iter().filter(|o| o.oov).collect();
    println!(
        "\nOOV（目标不在英文词表里，仍计入整体分母）: {}",
        oov.len()
    );
    for o in &oov {
        println!("  {} {}", o.id, o.note);
    }

    let lang_bad: Vec<&Outcome> = out
        .iter()
        .filter(|o| o.first_lang_ok == Some(false))
        .collect();
    println!("\n首语言不符（expected_first_language 单独断言）: {}", lang_bad.len());
    for o in &lang_bad {
        println!(
            "  {} 期望首个={} 实际={} 实际候选={:?}",
            o.id, o.note, o.first_lang, o.actual
        );
    }

    println!("\n== 失败明细（含实际候选与意图证据）==");
    let mut fails = 0;
    for (row, o) in rows.iter().zip(out.iter()) {
        let over_rank = o.hit_rank.is_none_or(|r| r > row.max_rank);
        if !over_rank && o.first_lang_ok != Some(false) {
            continue;
        }
        fails += 1;
        println!(
            "  {} input={:?} target={:?} intent={} prev={:?} max_rank={} → rank={:?}",
            o.id, row.input, row.target, row.intent, row.previous_english, row.max_rank, o.hit_rank
        );
        println!("      实际: {:?}", o.actual);
        println!("      说明: {}", o.note);
    }
    println!("失败条数: {fails}");

    // 补全 Top-3 未达标的**根因拆分**：召回没进来 vs 混排没给位。
    let comp: Vec<(&Row, &Outcome)> = rows
        .iter()
        .zip(out.iter())
        .filter(|(_, o)| o.category == Category::Completion)
        .collect();
    let in_en_top3 = comp.iter().filter(|(_, o)| o.en_rank.is_some_and(|r| r <= 3)).count();
    let recalled = comp.iter().filter(|(_, o)| o.en_rank.is_some()).count();
    println!(
        "\n补全行根因：英文通道内部 Top-3 = {in_en_top3}/{}；进过英文召回池 = {recalled}/{}（其余被有界近似挡在池外）",
        comp.len(),
        comp.len()
    );
    let weak = comp
        .iter()
        .filter(|(_, o)| o.e0_zipf.is_none_or(|q| q < 300))
        .count();
    println!(
        "  其中 E0 静态频率 < 300（或没有 E0）⇒ good_en 为假、落到 ChineseFirst 的行: {weak}/{}",
        comp.len()
    );
    let mut e0_hist: BTreeMap<String, usize> = BTreeMap::new();
    for (_, o) in &comp {
        let tag = match o.e0_zipf {
            Some(q) if q >= 300 => "E0 q>=300",
            Some(q) if q >= 250 => "E0 250<=q<300",
            Some(_) => "E0 q<250",
            None => "无 E0",
        };
        *e0_hist.entry(tag.to_string()).or_default() += 1;
    }
    for (k, v) in &e0_hist {
        println!("    {k}: {v} 行");
    }

    // 分类计数（供报告引用）。
    let mut by_intent: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &rows {
        *by_intent.entry(r.intent.as_str()).or_default() += 1;
    }
    println!("\n意图分布: {by_intent:?}");
    println!("  补全行 E0 例（key[静态分]）: {:?}", comp.iter().take(8).map(|(_, o)| o.e0.clone()).collect::<Vec<_>>());
    let _ = EnglishMatchKind::Exact; // 元数据判据在 hit_rank 里已用到
    Ok(())
}
