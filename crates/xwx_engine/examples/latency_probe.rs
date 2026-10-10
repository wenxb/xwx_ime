//! 逐键延迟探针：**按真实会话**（拼音串 + 上文 + 候选窗）量每一次按键。
//!
//! `regression_probe` 报的是 30 次里的**最小值**（用来对拍稳定性），
//! 这里要的是用户实际感受到的分布：p50 / p95 / max，以及**哪一类按键慢**。
//!
//! 用法：
//!
//! ```text
//! cargo run --release -p xwx_engine --example latency_probe -- \
//!   --data data --lm ~/.local/share/xwx/wanxiang-lts-zh-hans.gram \
//!   [--english data/english.bin]
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use xwx_engine::{EnglishDict, EnglishEngine, ImeEngine, InputEvent, InputSession};

/// 一次按键样本：标签 + 毫秒。
struct Sample {
    label: String,
    ms: f64,
}

struct Stats {
    samples: Vec<f64>,
}

impl Stats {
    fn of(s: &[&Sample]) -> Self {
        let mut v: Vec<f64> = s.iter().map(|x| x.ms).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        Self { samples: v }
    }

    fn q(&self, q: f64) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let i = ((self.samples.len() - 1) as f64 * q).round() as usize;
        self.samples[i]
    }

    fn line(&self, label: &str) -> String {
        format!(
            "{label:<26} n={:<5} p50={:>8.3}ms p95={:>8.3}ms p99={:>8.3}ms max={:>8.3}ms",
            self.samples.len(),
            self.q(0.50),
            self.q(0.95),
            self.q(0.99),
            self.samples.last().copied().unwrap_or(0.0)
        )
    }
}

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

/// 打一段中文（逐键），返回每一步的耗时。
fn type_chinese(session: &mut InputSession, text: &str, out: &mut Vec<Sample>, tag: &str) {
    for (i, ch) in text.chars().enumerate() {
        let t = Instant::now();
        let _ = session.handle(InputEvent::Char(ch)).expect("按键");
        out.push(Sample {
            label: format!("{tag}:{}", i + 1),
            ms: t.elapsed().as_secs_f64() * 1000.0,
        });
    }
}

/// 混合序列：字母 + 空格选词 + 退格 + 翻页，覆盖真实编辑动作。
fn mix(session: &mut InputSession, out: &mut Vec<Sample>) {
    let steps: &[(&str, InputEvent)] = &[
        ("space", InputEvent::Space),
        ("backspace", InputEvent::Backspace),
        ("pagedown", InputEvent::PageDown),
        ("pageup", InputEvent::PageUp),
        ("down", InputEvent::Down),
        ("up", InputEvent::Up),
        ("select1", InputEvent::Select(1)),
        ("reset", InputEvent::Escape),
    ];
    for (name, ev) in steps {
        let t = Instant::now();
        let _ = session.handle(*ev).expect("按键");
        out.push(Sample {
            label: format!("mix:{name}"),
            ms: t.elapsed().as_secs_f64() * 1000.0,
        });
    }
}

fn build(data: &Path, lm: &Path, english: Option<&Path>) -> Result<ImeEngine, Box<dyn std::error::Error>> {
    let mut engine = ImeEngine::new_with_lm(Some(&data.join("dict.bin")), Some(lm))?;
    engine.dict = engine.dict.with_abbrev(data.join("abbrev.bin"));
    engine.dict = engine.dict.with_syllable_graph(data.join("syllable_graph.bin"));
    if let Some(p) = english {
        engine = engine.with_english(EnglishEngine::new(EnglishDict::open(p)?).with_predict(true));
    }
    Ok(engine)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data = PathBuf::from(arg("--data", true).expect("--data 必填"));
    let lm = PathBuf::from(arg("--lm", true).expect("--lm 必填"));
    let english = arg("--english", false).map(PathBuf::from);
    if !lm.is_file() {
        return Err(format!("语言模型不存在: {}", lm.display()).into());
    }

    let engine = Arc::new(build(&data, &lm, english.as_deref())?);
    println!("== 逐键延迟（真实会话，release）==");
    println!("data = {}", data.display());
    println!(
        "lm   = {} ({} 字节)",
        lm.display(),
        std::fs::metadata(&lm)?.len()
    );

    // 诊断模式：给一个输入串，打印引擎内部的阶段耗时（需要 XWX_TRACE=1）。
    if let Some(input) = arg("--trace", false) {
        for s in input.split(',') {
            let mut sess = engine.create_session();
            for ch in s.chars() {
                let _ = sess.handle(InputEvent::Char(ch));
            }
            let lines = xwx_engine::engine::trace::take();
            println!("--- {s} ---");
            for l in lines.iter() {
                println!("   {l}");
            }
        }
        return Ok(());
    }

    let mut all: Vec<Sample> = Vec::new();

    // 1) 短文：常见句子的逐键
    let mut s = engine.create_session();
    type_chinese(&mut s, "jintiantianqibucuo", &mut all, "short");
    // 2) 长文：连续打 40 个字母（候选池最大、上文最长）
    let mut s = engine.create_session();
    type_chinese(&mut s, "womenmingtianyaoqubeijingkanshijiebei", &mut all, "long");
    // 3) 简拼（2 字母 → 候选爆炸）
    let mut s = engine.create_session();
    type_chinese(&mut s, "nhsytx", &mut all, "abbrev");
    // 4) 编辑动作
    let mut s = engine.create_session();
    let _ = s.handle(InputEvent::Char('n'));
    let _ = s.handle(InputEvent::Char('i'));
    let _ = s.handle(InputEvent::Char('h'));
    let _ = s.handle(InputEvent::Char('a'));
    let _ = s.handle(InputEvent::Char('o'));
    mix(&mut s, &mut all);

    // 分类汇总
    let cats = ["short", "long", "abbrev", "mix"];
    for c in cats {
        let col: Vec<&Sample> = all.iter().filter(|s| s.label.starts_with(c)).collect();
        if !col.is_empty() {
            println!("{}", Stats::of(&col).line(c));
        }
    }
    let col: Vec<&Sample> = all.iter().collect();
    println!("{}", Stats::of(&col).line("全部"));
    let over: Vec<&Sample> = all.iter().filter(|s| s.ms > 12.0).collect();
    println!("\n超 12ms 的按键: {}/{}", over.len(), all.len());
    for s in over.iter().take(15) {
        println!("   {:<16} {:.2}ms", s.label, s.ms);
    }
    // 最慢的 10 个（不限门槛）
    let mut sorted: Vec<&Sample> = all.iter().collect();
    sorted.sort_by(|a, b| b.ms.partial_cmp(&a.ms).unwrap_or(std::cmp::Ordering::Equal));
    println!("\n最慢的 10 次按键:");
    for s in sorted.iter().take(10) {
        println!("   {:<16} {:.2}ms", s.label, s.ms);
    }

    // —— 热态逐输入分布：先热身，再看每一种输入形态的真实开销 ——
    println!("\n== 热态：每种输入形态的**最后一次按键**耗时（热身 3 轮后 30 轮）==");
    let inputs: &[(&str, &str)] = &[
        ("单字母", "n"),
        ("双字母全拼", "ni"),
        ("三字母全拼", "nih"),
        ("五字母全拼", "nihao"),
        ("九字母全拼", "jintiantian"),
        ("双字母简拼", "nh"),
        ("三字母简拼", "nhs"),
        ("六字母简拼", "nhsytx"),
        ("长简拼", "wmjtwmyqbj"),
        ("词库无解", "zzzzz"),
        ("中英混合", "python"),
    ];
    // 热身：把所有输入都跑一遍，把页缓存与内部分支都捂热
    for _ in 0..3 {
        for (_, input) in inputs {
            let mut sess = engine.create_session();
            for ch in input.chars() {
                let _ = sess.handle(InputEvent::Char(ch));
            }
        }
    }
    for (name, input) in inputs {
        let mut times = Vec::new();
        let mut n_cands = 0usize;
        for _ in 0..30 {
            let mut sess = engine.create_session();
            // 前面的字母不计时，只量**最后一次**按键（用户感受到的那一下）
            let chars: Vec<char> = input.chars().collect();
            for ch in &chars[..chars.len() - 1] {
                let _ = sess.handle(InputEvent::Char(*ch));
            }
            let last = *chars.last().expect("非空");
            let t = Instant::now();
            let _ = sess.handle(InputEvent::Char(last));
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            n_cands = sess.candidate_count();
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let q = |p: f64| times[((times.len() - 1) as f64 * p).round() as usize];
        println!(
            "   {name:<12} {input:<12} p50={:>7.3}ms p95={:>7.3}ms max={:>7.3}ms 候选={n_cands}",
            q(0.50),
            q(0.95),
            times[times.len() - 1]
        );
    }
    Ok(())
}
