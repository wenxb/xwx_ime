//! 英文通道性能探针（M1）。
//!
//! 报告分开记录（见 `docs/english-input/05-execution-tests.md` 第 16 节）：
//! 数据版本/大小/加载词数、冷启动与首次查询、稳态逐键 p50/p95/max、
//! 超过 12ms 的样本、英文开/关在同脚本上的差值、用户模型空与受控较大
//! 模型各一组。**缺数据直接失败**——不退回空模型后输出漂亮延迟。
//!
//! 用法：
//! ```text
//! cargo run --release --offline -p xwx_engine --example english_perf -- \
//!   --data /path/to/chinese-data --english data/english.bin --repeat 20
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use xwx_engine::english::{EnglishContext, EnglishDict, EnglishEngine};
use xwx_engine::{ImeEngine, LmContext};

/// 门槛：单次按键超过它就单独列出来（与项目既有 12ms 口径一致）。
const SLOW_MS: f64 = 12.0;

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
    let mut grams: Vec<PathBuf> = std::fs::read_dir(xdg.join("xwx"))
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "gram"))
        .collect();
    grams.sort();
    grams.into_iter().next()
}

/// 一个常量输入序列。
struct Scenario {
    name: &'static str,
    keys: &'static [&'static str],
    /// 是否带上英文上文（连续英文场景）。
    previous: Option<&'static str>,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "英文前缀",
        keys: &["pyt", "pyth", "pytho", "dock", "docke", "kuber", "devel"],
        previous: None,
    },
    Scenario {
        name: "英文完整词",
        keys: &["hello", "world", "thank", "there", "docker"],
        previous: None,
    },
    Scenario {
        name: "英文连续(有上文)",
        keys: &["w", "wo", "wor", "world"],
        previous: Some("hello"),
    },
    Scenario {
        name: "中英同形",
        keys: &["you", "can", "he", "an", "wo"],
        previous: None,
    },
    Scenario {
        name: "中文拼音(回归)",
        keys: &["nihao", "zhongguo", "shurufa", "jintiantianqi"],
        previous: None,
    },
];

struct Stats {
    samples: Vec<f64>,
}

impl Stats {
    fn new() -> Self {
        Self { samples: Vec::new() }
    }

    fn push(&mut self, ms: f64) {
        self.samples.push(ms);
    }

    fn quantile(&self, q: f64) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let mut v = self.samples.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((v.len() - 1) as f64 * q).round() as usize;
        v[idx]
    }

    fn max(&self) -> f64 {
        self.samples.iter().copied().fold(0.0, f64::max)
    }

    fn line(&self, label: &str) -> String {
        format!(
            "{label:<22} n={:<5} p50={:>8.3}ms p95={:>8.3}ms max={:>8.3}ms",
            self.samples.len(),
            self.quantile(0.50),
            self.quantile(0.95),
            self.max()
        )
    }

    /// 超过门槛的样本数。
    fn slow(&self) -> usize {
        self.samples.iter().filter(|v| **v > SLOW_MS).count()
    }
}

/// 跑一轮场景，返回每键耗时。
fn run(engine: &ImeEngine, sc: &Scenario, repeat: usize) -> Stats {
    let ctx = match sc.previous {
        Some(p) => EnglishContext::with_previous(p),
        None => EnglishContext::empty(),
    };
    let mut stats = Stats::new();
    for _ in 0..repeat {
        for key in sc.keys {
            let t = Instant::now();
            let _ = engine
                .lookup_composition(key, &LmContext::empty(), &ctx)
                .expect("查询失败");
            stats.push(t.elapsed().as_secs_f64() * 1000.0);
        }
    }
    stats
}

fn build(data_dir: &Path, lm: &Path, english: Option<&Path>) -> Result<ImeEngine, Box<dyn std::error::Error>> {
    let mut engine = ImeEngine::new_with_lm(Some(&data_dir.join("dict.bin")), Some(lm))?;
    engine.dict = engine.dict.with_abbrev(data_dir.join("abbrev.bin"));
    engine.dict = engine.dict.with_syllable_graph(data_dir.join("syllable_graph.bin"));
    if let Some(p) = english {
        engine = engine.with_english(EnglishEngine::new(EnglishDict::open(p)?));
    }
    Ok(engine)
}

/// 受控「较大」用户模型：学 300 个词的二元计数，用来暴露索引更新的隐藏退化。
fn seed_user_model(engine: &ImeEngine, dict: &EnglishDict) {
    let Some(en) = engine.english() else {
        return;
    };
    let mut prev: Option<String> = None;
    for id in 0..dict.word_count().min(300) as u32 {
        let Ok(e) = dict.entry_at(id) else { continue };
        en.user().learn(prev.as_deref(), e.key);
        prev = Some(e.key.to_string());
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = PathBuf::from(arg("--data", true).expect("--data 必填"));
    let english_path = PathBuf::from(arg("--english", true).expect("--english 必填"));
    let repeat: usize = arg("--repeat", false)
        .map(|v| v.parse().expect("--repeat 必须是整数"))
        .unwrap_or(20);
    let lm_path = find_lm(&data_dir)
        .ok_or("找不到语言模型（--data/$XWX_LM_PATH/XDG 都没有 .gram）")?;
    for (what, p) in [
        ("中文词库", &data_dir.join("dict.bin")),
        ("简拼副索引", &data_dir.join("abbrev.bin")),
        ("语言模型", &lm_path),
        ("英文词库", &english_path),
    ] {
        if !p.is_file() {
            return Err(format!("缺{what}: {}", p.display()).into());
        }
    }

    let dict_meta = EnglishDict::open(&english_path)?;
    let (words, prefixes, bytes) = (
        dict_meta.word_count(),
        dict_meta.prefix_count(),
        dict_meta.file_len(),
    );
    println!("== 数据 ==");
    println!("中文数据 : {}", data_dir.display());
    println!("语言模型 : {} ({} 字节)", lm_path.display(), std::fs::metadata(&lm_path)?.len());
    println!(
        "英文词库 : {} ({} 词 / {} 前缀 / {} 字节)",
        english_path.display(),
        words,
        prefixes,
        bytes
    );
    println!("repeat   : {repeat}");

    // —— 冷启动与首次查询（英文开）——
    let t = Instant::now();
    let engine_on = build(&data_dir, &lm_path, Some(&english_path))?;
    let cold_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let _ = engine_on.lookup_composition("pyth", &LmContext::empty(), &EnglishContext::empty())?;
    let first_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!("\n== 冷启动 ==");
    println!("引擎装配(含 mmap 与英文词库完整校验) : {cold_ms:.1}ms");
    println!("首次查询                              : {first_ms:.1}ms");

    // —— 英文关 ——
    let engine_off = build(&data_dir, &lm_path, None)?;

    println!("\n== 稳态（英文关 / 英文开 / 英文开+受控用户模型）==");
    // **先热身**:引擎刚装配完时页缓存是冷的(上面「英文关」那组的第一轮
    // 会带 78ms 的冷页缺页),不热身的话「开/关差值」测的是缓存而不是代码。
    for sc in SCENARIOS {
        let _ = run(&engine_off, sc, 1);
        let _ = run(&engine_on, sc, 1);
    }
    seed_user_model(&engine_on, &dict_meta);
    for sc in SCENARIOS {
        let _ = run(&engine_on, sc, 1);
    }
    let mut over: Vec<(String, f64)> = Vec::new();
    for sc in SCENARIOS {
        let off = run(&engine_off, sc, repeat);
        let on = run(&engine_on, sc, repeat);
        seed_user_model(&engine_on, &dict_meta);
        let seeded = run(&engine_on, sc, repeat);
        println!("{}", off.line(&format!("{} [关]", sc.name)));
        println!("{}", on.line(&format!("{} [开]", sc.name)));
        println!("{}", seeded.line(&format!("{} [开+模型]", sc.name)));
        let delta = on.quantile(0.95) - off.quantile(0.95);
        println!(
            "{:<22} 开-关 p95 差值 = {delta:+.3}ms；超 {SLOW_MS}ms 样本 {}/{}",
            "",
            on.slow() + seeded.slow(),
            on.samples.len() + seeded.samples.len()
        );
        for v in on.samples.iter().chain(seeded.samples.iter()) {
            if *v > SLOW_MS {
                over.push((sc.name.to_string(), *v));
            }
        }
    }

    println!("\n== 超过 {SLOW_MS}ms 的逐键样本 ==");
    if over.is_empty() {
        println!("(无)");
    } else {
        for (name, ms) in &over {
            println!("  {name}: {ms:.3}ms");
        }
    }
    println!("\n注：这里是**纯会话/引擎**时间；D-Bus 往返另测，不把两者混为一谈。");
    Ok(())
}
