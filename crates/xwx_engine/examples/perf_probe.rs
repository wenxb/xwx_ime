//! 性能验收入口:逐键 p50/p95/max + 超线输入点名。
//!
//! codex 审查第二批第一项:在**生产装配**上量真实按键延迟,而不是在
//! 合成/降级装配上量。这里与 `src/main.rs` 的接线顺序一致(真词库 +
//! 简拼副索引 + 万象 `.gram` + 用户层),按键走**真实会话**路径
//! `InputSession::handle`(与 `regression_probe` 的 C 段同一条),
//! 所以一个样本 = 「一次按键里引擎做的全部工作」,**不含 D-Bus IPC**。
//!
//! ```text
//! cargo build --release --offline -p xwx_engine --examples
//! /mnt/hdd/target/xwx_ime/release/examples/perf_probe [--data DIR] [--repeat N]
//!                                                     [--limit-ms 12] [--user-model PATH]
//! ```
//!
//! 口径(写清楚是因为性能数字最容易各说各话):
//! - **冷启动首键**:新装配的第一个键(含 mmap 缺页)单独报,不参与判定;
//! - **稳态**:其余按键按场景给 p50/p95/max(最近秩,样本量小的时候
//!   比插值更能反映「用户真的会遇到多慢的一次」);
//! - **判定**:退出码在任一场景的**稳态 p95** 超过 `--limit-ms` 时非零;
//!   `max` 与全部超线样本(场景/第几键/毫秒)照实打印,便于定位;
//! - 用户层默认 `UserLM::in_memory()`,**不碰** `~/.local/share/xwx/`;
//!   要量真文件后端用 `--user-model PATH`(写盘在定时器里,不在热路径)。
//!
//! 12ms 这个门槛来自项目既有约定(逐键端到端预算),不是本探针新定的。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use xwx_engine::{ImeEngine, InputEvent, LmStore, SessionOutput, UserLM};

/// 默认数据目录:与生产 `resolve_data_path` 的回落一致。
fn default_data_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("xwx");
    }
    let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
    PathBuf::from(home).join(".local/share/xwx")
}

struct Args {
    data: PathBuf,
    repeat: usize,
    limit_ms: f64,
    user_model: Option<PathBuf>,
    /// 量之前先空跑一遍所有脚本(样本丢弃):用来验证「首次触碰」成本
    /// 到底是一次性的、还是每次都会付。
    prewarm: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut args = Self {
            data: default_data_dir(),
            repeat: 5,
            limit_ms: 12.0,
            user_model: None,
            prewarm: false,
        };
        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--data" => {
                    args.data = it
                        .next()
                        .ok_or_else(|| "--data 需要目录参数".to_owned())?
                        .into();
                }
                "--repeat" => {
                    args.repeat = it
                        .next()
                        .ok_or_else(|| "--repeat 需要数字".to_owned())?
                        .parse()
                        .map_err(|e| format!("--repeat 不是数字:{e}"))?;
                }
                "--limit-ms" => {
                    args.limit_ms = it
                        .next()
                        .ok_or_else(|| "--limit-ms 需要数字".to_owned())?
                        .parse()
                        .map_err(|e| format!("--limit-ms 不是数字:{e}"))?;
                }
                "--user-model" => {
                    args.user_model = Some(
                        it.next()
                            .ok_or_else(|| "--user-model 需要路径".to_owned())?
                            .into(),
                    );
                }
                "--prewarm" => args.prewarm = true,
                other => return Err(format!("未知参数:{other}")),
            }
        }
        if args.repeat == 0 {
            return Err("--repeat 至少为 1".to_owned());
        }
        Ok(args)
    }
}

/// 装配与生产一致,并**验证数据真的加载了**(引擎对缺失/损坏文件是静默
/// 退回空库的,只看 `Ok(engine)` 会把空库的漂亮延迟当成绩)。
fn open_engine(
    dir: &Path,
    user_model: Option<&Path>,
) -> Result<(Arc<ImeEngine>, usize), String> {
    let dict = dir.join("dict.bin");
    let abbrev = dir.join("abbrev.bin");
    let gram = dir.join("wanxiang-lts-zh-hans.gram");
    let mut missing = Vec::new();
    for p in [&dict, &abbrev, &gram] {
        if !p.is_file() {
            missing.push(p.display().to_string());
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "数据文件缺失(性能入口不做降级,请先准备或改 --data):{}",
            missing.join("、")
        ));
    }

    let mut engine =
        ImeEngine::new_with_lm(Some(&dict), Some(&gram)).map_err(|e| format!("打开引擎失败:{e}"))?;
    engine.dict = engine.dict.with_abbrev(&abbrev);
    // 注意:规模要在**挂用户层之前**读——`with_user_lm` 之后 lm 变成
    // `LmStore::Mixed`,从外面再 `match` 只会得到「不是万象」。
    let lm_units = match &engine.lm {
        LmStore::Wanxiang(lm) if lm.num_units() > 0 => lm.num_units(),
        LmStore::Wanxiang(_) => return Err(format!("万象模型是空表:{}", gram.display())),
        _ => return Err("万象模型没加载(退回空 bigram/混合后端)".to_owned()),
    };
    if engine.dict_count() == 0 {
        return Err(format!("词库没加载:{}", dict.display()));
    }
    if !engine.dict.has_abbrev() {
        return Err(format!("简拼副索引没装上:{}", abbrev.display()));
    }

    let user = match user_model {
        Some(path) => UserLM::open_or_create(path),
        None => UserLM::in_memory(),
    };
    Ok((Arc::new(engine.with_user_lm(user)), lm_units))
}

/// 按键脚本:覆盖全拼/混拼/简拼/容错/分隔符/翻页/数字/高亮/退格/清空。
fn scripts() -> Vec<(&'static str, Vec<InputEvent>)> {
    let letters = |s: &str| -> Vec<InputEvent> { s.chars().map(InputEvent::Char).collect() };
    let mut out: Vec<(&'static str, Vec<InputEvent>)> = Vec::new();
    let mut add = |name: &'static str, body: &str, tail: &[InputEvent]| {
        let mut keys = letters(body);
        keys.extend(tail.iter().copied());
        out.push((name, keys));
    };

    // 全拼(长串是本项目已知的最慢形状)
    add("full:nihao", "nihao", &[InputEvent::Space]);
    add("full:weishenme", "weishenme", &[InputEvent::Space]);
    add("full:womenjintianquxian", "womenjintianquxian", &[InputEvent::Space]);
    // 混合简拼(整串跨词:确认段 + 剩余串)
    add("mixed:zhongg", "zhongg", &[InputEvent::Space]);
    add("mixed:jint", "jint", &[InputEvent::Space]);
    add(
        "mixed:womenjintqubeij",
        "womenjintqubeij",
        &[InputEvent::Space, InputEvent::Space],
    );
    // 纯简拼(首字母缩写)
    add("abbrev:ns", "ns", &[InputEvent::Space]);
    add("abbrev:nh", "nh", &[InputEvent::Space]);
    // 容错与分隔符
    add("typo:nihoa", "nihoa", &[InputEvent::Space]);
    add("sep:xi'anq", "xi'anq", &[InputEvent::Space]);
    // 交互路径
    add(
        "page:nihao",
        "nihao",
        &[InputEvent::PageDown, InputEvent::PageDown, InputEvent::Space],
    );
    add("digit:nihao", "nihao", &[InputEvent::Select(3)]);
    add("click:nihao", "nihao", &[InputEvent::CandidateClicked(2)]);
    add(
        "cursor:nihao",
        "nihao",
        &[InputEvent::Down, InputEvent::Down, InputEvent::Up, InputEvent::Space],
    );
    add("backspace:nihao", "nihao", &[InputEvent::Backspace, InputEvent::Space]);
    add("escape:nihao", "nihao", &[InputEvent::Escape]);
    out
}

/// 单次按键样本。
struct Sample {
    scenario: &'static str,
    key_index: usize,
    ms: f64,
    cold: bool,
}

/// 最近秩分位(样本量小时比插值更贴近「真会碰到多慢的一次」)。
fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 * q).ceil() as usize).saturating_sub(1);
    sorted[idx.min(sorted.len() - 1)]
}

/// 跑一遍脚本,返回「有没有任何一键真的产出了会话输出」——全 `Noop` 的
/// 脚本是没被验证到的假样本(比如修饰键名写错),必须报出来。
fn run_keys(
    engine: &Arc<ImeEngine>,
    scenario: &'static str,
    keys: &[InputEvent],
    cold_session: bool,
    samples: &mut Vec<Sample>,
) -> Result<bool, String> {
    let mut session = engine.create_session();
    let mut any_output = false;
    for (i, key) in keys.iter().enumerate() {
        let t0 = Instant::now();
        let out = session
            .handle(*key)
            .map_err(|e| format!("{scenario} 第 {} 键失败:{e}", i + 1))?;
        any_output |= !matches!(out, SessionOutput::Noop);
        samples.push(Sample {
            scenario,
            key_index: i,
            ms: t0.elapsed().as_secs_f64() * 1000.0,
            // 只有「新会话的第一键」算冷样本:它含会话初始化。
            cold: cold_session && i == 0,
        });
    }
    Ok(any_output)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = match Args::parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("参数错误:{e}");
            std::process::exit(2);
        }
    };

    // 真冷启动:装配完成后第一个键(含 mmap 缺页/首次分配)。
    let (cold_engine, lm_units) = open_engine(&args.data, args.user_model.as_deref())
        .map_err(|e| format!("装配失败:{e}"))?;
    let scripts = scripts();
    let mut samples: Vec<Sample> = Vec::new();
    {
        let (name, keys) = &scripts[0];
        let t0 = Instant::now();
        let mut session = cold_engine.create_session();
        for key in keys {
            session.handle(*key)?;
        }
        samples.push(Sample {
            scenario: name,
            key_index: 0,
            ms: t0.elapsed().as_secs_f64() * 1000.0,
            cold: true,
        });
    }
    let cold_start_ms = samples[0].ms;

    let engine = Arc::clone(&cold_engine);
    if args.prewarm {
        // 走**生产同一条**预热路径(`ImeEngine::prewarm`,由 `src/main.rs`
        // 在启动后后台调用),而不是探针自己另造一套:这样这里的数字就是
        // 生产首键的数字。
        engine.prewarm();
        println!("(已调用 ImeEngine::prewarm 预热;样本不计入统计)");
    }
    let mut activity: Vec<(&'static str, bool)> =
        scripts.iter().map(|(name, _)| (*name, false)).collect();
    for round in 0..args.repeat {
        for (idx, (name, keys)) in scripts.iter().enumerate() {
            let any = run_keys(&engine, name, keys, round == 0, &mut samples)?;
            activity[idx].1 |= any;
        }
    }
    let idle: Vec<&str> = activity
        .iter()
        .filter(|(_, any)| !any)
        .map(|(name, _)| *name)
        .collect();
    if !idle.is_empty() {
        eprintln!(
            "警告:以下脚本没有任何一键产出会话输出(全 Noop),延迟数据无意义:{}",
            idle.join("、")
        );
    }

    // —— 汇总 ——
    let steady: Vec<&Sample> = samples.iter().filter(|s| !s.cold).collect();
    let mut all_ms: Vec<f64> = steady.iter().map(|s| s.ms).collect();
    all_ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    println!("## 逐键延迟(handle 路径,含会话内全部工作;不含 D-Bus IPC)");
    println!("数据目录 {}", args.data.display());
    println!(
        "装配:词库 {} 条 / 万象 {} 单元 / 简拼副索引 已装 / 用户层 {}",
        engine.dict_count(),
        lm_units,
        if args.user_model.is_some() { "文件" } else { "内存" }
    );
    println!("重复 {} 轮,稳态样本 {} 个\n", args.repeat, steady.len());
    println!(
        "{:<28} {:>5} {:>9} {:>9} {:>9}",
        "场景", "键数", "p50", "p95", "max"
    );
    for (name, keys) in &scripts {
        let mut ms: Vec<f64> = steady
            .iter()
            .filter(|s| s.scenario == *name)
            .map(|s| s.ms)
            .collect();
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        println!(
            "{:<28} {:>5} {:>8.3}ms {:>8.3}ms {:>8.3}ms",
            name,
            keys.len(),
            quantile(&ms, 0.50),
            quantile(&ms, 0.95),
            quantile(&ms, 1.0)
        );
    }
    println!(
        "\n总表(稳态):p50={:.3}ms p95={:.3}ms max={:.3}ms",
        quantile(&all_ms, 0.50),
        quantile(&all_ms, 0.95),
        quantile(&all_ms, 1.0)
    );
    println!("冷启动首键(装配后的第一个键,不参与判定):{cold_start_ms:.3}ms");

    let over: Vec<&Sample> = steady.iter().filter(|s| s.ms > args.limit_ms).copied().collect();
    if over.is_empty() {
        println!("超线(> {:.2}ms):无", args.limit_ms);
    } else {
        println!("超线(> {:.2}ms):{} 个样本", args.limit_ms, over.len());
        let mut sorted = over.clone();
        sorted.sort_by(|a, b| b.ms.partial_cmp(&a.ms).unwrap_or(std::cmp::Ordering::Equal));
        for s in sorted.iter().take(10) {
            println!(
                "  {:<28} 第 {} 键 {:.3}ms",
                s.scenario,
                s.key_index + 1,
                s.ms
            );
        }
    }

    // 判定按场景 p95:max 受调度抖动影响,单次尖峰不该让门槛红。
    let mut failed = Vec::new();
    for (name, _) in &scripts {
        let mut ms: Vec<f64> = steady
            .iter()
            .filter(|s| s.scenario == *name)
            .map(|s| s.ms)
            .collect();
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p95 = quantile(&ms, 0.95);
        if p95 > args.limit_ms {
            failed.push(format!("{name} p95={p95:.3}ms"));
        }
    }
    if failed.is_empty() {
        println!(
            "判定:全部场景稳态 p95 ≤ {:.2}ms,通过(本次 max={:.3}ms、超线 {} 个样本,\n      与 p95 一起看:门槛卡的是「常慢」而不是单次调度抖动)",
            args.limit_ms,
            quantile(&all_ms, 1.0),
            over.len()
        );
        Ok(())
    } else {
        eprintln!(
            "判定:以下场景稳态 p95 超过 {:.2}ms —— {}",
            args.limit_ms,
            failed.join("、")
        );
        std::process::exit(1);
    }
}
