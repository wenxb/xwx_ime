//! 打字模拟器（tools/typing_sim/DESIGN.md）：按真实按键语义驱动
//! [`InputSession`] 状态机（进程内，无系统键盘事件），量化候选质量与
//! 逐键延迟，产出 JSONL 结果交 `tools/typing_sim/report.py` 聚合。
//!
//! 两种模式：
//! - `word`  ：词级简拼/混合样本（gen_abbrev.py 产出）→ 逐条查候选，
//!   测召回与名次（含 zh/ch/sh 两种写法、混合缩写模式）；
//! - `session`：打字计划（gen_plans.py 产出）→ 逐词敲入 + 数字/空格
//!   选词 + 标点收尾，走真实状态机；测逐键前缀稳定性、
//!   选字正确性、残留串、整句零干预字转换（mode=whole）。
//!
//! 装配纪律与 eval_rank.rs 一致：生产同款（dict+abbrev+syllable_graph+LM），
//! 缺件即报错退出；多线程共享 `Arc<ImeEngine>`，每计划独立会话。
//! 候选/名次一律从 `handle` 返回的 `SessionUpdate` 读取（会话不暴露内部态）。

use std::collections::VecDeque;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

use xwx_engine::candidate::Candidate;
use xwx_engine::{ImeEngine, InputEvent, InputSession, LmContext, SessionOutput, UserLM};

const FIRST_SCREEN: usize = 9; // page_size=9：rank 0..=8 在首屏，9 = 第 10 项
const SLOW_MS: f64 = 12.0; // just perf 的逐键门槛
const MAX_LEFTOVER_SPACES: usize = 3;

// —— 极简 JSONL 字段提取（与 eval_rank.rs 同一手法；值不含裸引号） ——
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":");
    let start = line.find(&pat)? + pat.len();
    let rest = line[start..].trim_start().strip_prefix('"')?;
    Some(&rest[..rest.find('"').expect("字段未闭合")])
}

fn unescape(s: &str) -> String {
    s.replace("\\\\", "\u{0}").replace("\\\"", "\"").replace('\u{0}', "\\")
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

struct WordPlan {
    text: String,
    style: String,
    input: String,
    /// 全拼（style 非 full 时简拼失败后的重打串——真实用户的兜底习惯）。
    full: String,
}

struct Plan {
    id: String,
    mode: String,
    sent: String,
    punct: String,
    words: Vec<WordPlan>,
}

fn parse_plan(line: &str) -> Plan {
    let words_raw = unescape(field(line, "words").unwrap_or_default());
    let words = words_raw
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|w| {
            let mut it = w.split('|');
            WordPlan {
                text: it.next().unwrap_or_default().to_string(),
                style: it.next().unwrap_or_default().to_string(),
                input: it.next().unwrap_or_default().to_string(),
                full: it.next().unwrap_or_default().to_string(),
            }
        })
        .collect();
    Plan {
        id: field(line, "id").unwrap_or("?").to_string(),
        mode: field(line, "mode").unwrap_or("word").to_string(),
        sent: unescape(field(line, "sent").unwrap_or_default()),
        punct: unescape(field(line, "punct").unwrap_or_default()),
        words,
    }
}

/// `--user <path>` 挂真实用户模型（**只读口径**：本例程不调 flush，不污染
/// 用户的 `user_bigram.txt`）；`--user-mem` 挂空的内存用户层，让会话在跑的过程中学
/// ——这两档与不挂用户层（冷启动）一起，用来分离「个性化」与「引擎排序」。
fn load_engine(user: Option<&str>, user_mem: bool) -> Result<Arc<ImeEngine>, Box<dyn std::error::Error>> {
    let dict: PathBuf = std::env::var("XWX_DICT_PATH").unwrap_or_else(|_| "data/dict.bin".into()).into();
    let abbrev: PathBuf = std::env::var("XWX_ABBREV_PATH").unwrap_or_else(|_| "data/abbrev.bin".into()).into();
    let lm: PathBuf = std::env::var("XWX_LM_PATH")
        .map_err(|_| "未设置 XWX_LM_PATH：必须在生产同款装配下跑，缺 LM 不能出数")?
        .into();
    for (what, p) in [("词库", &dict), ("副索引", &abbrev), ("语言模型", &lm)] {
        if !p.is_file() {
            return Err(format!("缺{what}: {}", p.display()).into());
        }
    }
    let mut engine = ImeEngine::new_with_lm(Some(&dict), Some(&lm))?;
    engine.dict = engine.dict.with_abbrev(&abbrev);
    engine.dict = engine.dict.with_syllable_graph(abbrev.with_file_name("syllable_graph.bin"));
    if let Some(path) = user {
        let p = PathBuf::from(path);
        if !p.is_file() {
            return Err(format!("缺用户模型: {}", p.display()).into());
        }
        engine = engine.with_user_lm(UserLM::open_or_create(&p));
        println!("用户层: {}（只读口径：本例程不触发回写）", p.display());
    } else if user_mem {
        engine = engine.with_user_lm(UserLM::in_memory());
        println!("用户层: 空的内存层（会话内学习）");
    }
    Ok(Arc::new(engine))
}

/// 逐键计时：慢键样本（>SLOW_MS，至多 3 条）随累计值一起返回。
struct Timer {
    ms: f64,
    ms_max: f64,
    slow: Vec<String>,
}

impl Timer {
    fn new() -> Self {
        Self { ms: 0.0, ms_max: 0.0, slow: Vec::new() }
    }
    fn absorb(&mut self, other: &Timer) {
        self.ms += other.ms;
        self.ms_max = self.ms_max.max(other.ms_max);
        if self.slow.len() < 6 {
            self.slow.extend(other.slow.iter().cloned());
        }
    }
    fn measure<T>(&mut self, tag: &str, f: impl FnOnce() -> T) -> T {
        let t = Instant::now();
        let r = f();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        self.ms += ms;
        self.ms_max = self.ms_max.max(ms);
        if ms > SLOW_MS && self.slow.len() < 3 {
            self.slow.push(format!("{tag}|{ms:.1}"));
        }
        r
    }
}

fn rank_of(cands: &[Candidate], expect: &str) -> Option<usize> {
    cands.iter().position(|c| c.word == expect)
}

/// 敲入一串字母。track=true 时逐键盯目标词：记录最终名次与前缀抖动。
/// 返回 (最终名次, 计时, 抖动标记, 是否中途被自动上屏)。
fn type_input(
    s: &mut InputSession,
    input: &str,
    expect: &str,
    track: bool,
) -> (Option<usize>, Timer, &'static str, bool) {
    let mut t = Timer::new();
    let mut seen_fs = false;
    let mut ever_seen = false;
    let mut mid_commit = false;
    let mut last: Option<xwx_engine::SessionUpdate> = None;
    for ch in input.chars() {
        match t.measure("key", || s.handle(InputEvent::Char(ch))) {
            Ok(SessionOutput::Updated(u)) => {
                if track {
                    match rank_of(&u.candidates, expect) {
                        Some(r) => {
                            ever_seen = true;
                            if r < FIRST_SCREEN {
                                seen_fs = true;
                            }
                        }
                        None if ever_seen => {} // 目标曾可见、现在没了 → lost
                        None => {}
                    }
                }
                last = Some(u);
            }
            Ok(SessionOutput::Commit(_)) => mid_commit = true, // 字母敲着敲着自己上屏了
            _ => {}
        }
    }
    let rank = last.as_ref().and_then(|u| rank_of(&u.candidates, expect));
    let churn = if rank.is_none() && ever_seen {
        "lost"
    } else if seen_fs && rank.is_some_and(|r| r >= FIRST_SCREEN) {
        "fs_churn"
    } else {
        "none"
    };
    (rank, t, churn, mid_commit)
}

/// 按名次选字（空格/数字键），残留串继续空格。返回 (结果标记, 上屏片段)。
fn select_expect(s: &mut InputSession, rank: Option<usize>, t: &mut Timer) -> (&'static str, String) {
    let Some(rank) = rank else {
        // 无候选：Esc 清空，句子继续（真实用户会换写法重打）
        t.measure("esc", || { let _ = s.handle(InputEvent::Escape); });
        return ("no_cand", String::new());
    };
    let (key, base) = match rank {
        0 => (InputEvent::Space, "space"),
        9 => (InputEvent::Select(0), "select10"),
        r => (InputEvent::Select(r as u8), "select"),
    };
    let mut got = String::new();
    let mut outcome = base;
    if rank >= 10 {
        outcome = "deep_fallback"; // 超首屏：记失败，按空格将就首位
    }
    if let Ok(SessionOutput::Commit(text)) = t.measure("sel", || s.handle(key)) {
        got.push_str(&text);
    }
    for _ in 0..MAX_LEFTOVER_SPACES {
        if s.pinyin().is_empty() {
            break;
        }
        if let Ok(SessionOutput::Commit(text)) = t.measure("left", || s.handle(InputEvent::Space)) {
            got.push_str(&text);
            outcome = "leftover"; // 目标候选只覆盖了输入串前缀
        }
    }
    (outcome, got)
}

// —— word 模式：查候选，测召回/名次/延迟 ——
fn run_word(engine: &Arc<ImeEngine>, line: &str, threads: usize) -> String {
    let input = field(line, "input").unwrap_or_default().to_string();
    let word = unescape(field(line, "word").unwrap_or_default());
    let style = field(line, "style").unwrap_or("?").to_string();
    let mut t = Timer::new();
    let (rank, ncand, top3) = t.measure("lookup", || {
        let cands = engine.lookup_candidates(&input, &LmContext::empty()).unwrap_or_default();
        let r = rank_of(&cands, &word);
        let top3: Vec<String> = cands.iter().take(3).map(|c| c.word.clone()).collect();
        (r, cands.len(), top3.join("/"))
    });
    format!(
        "{{\"input\":\"{}\",\"word\":\"{}\",\"style\":\"{}\",\"found\":{},\"rank\":\"{}\",\
         \"ncand\":{},\"top3\":\"{}\",\"ms\":{:.3},\"ms_max\":{:.3},\"slow\":\"{}\",\
         \"threads\":{}}}",
        input, esc(&word), style, rank.is_some() as u8,
        rank.map(|r| r.to_string()).unwrap_or_else(|| "-".into()),
        ncand, esc(&top3), t.ms, t.ms_max, t.slow.join(","), threads,
    )
}

// —— session 模式：逐词敲入+选字，走真实状态机 ——
fn run_session(engine: &Arc<ImeEngine>, plan: &Plan, threads: usize) -> String {
    let mut s = engine.create_session();
    let mut got = String::new();
    let mut total_keys = 0usize;
    let mut t_all = Timer::new();
    let mut words_json: Vec<String> = Vec::new();

    if plan.mode == "whole" {
        // 整句全拼连打、零干预空格（字转换正确率口径）
        let full: String = plan.words.iter().map(|w| w.input.as_str()).collect();
        let (_, t, _, _) = type_input(&mut s, &full, "", false);
        t_all.absorb(&t);
        total_keys += full.chars().count();
        for _ in 0..(plan.words.len() * 2 + 2) {
            if s.pinyin().is_empty() {
                break;
            }
            if let Ok(SessionOutput::Commit(text)) = t_all.measure("space", || s.handle(InputEvent::Space)) {
                got.push_str(&text);
            }
            total_keys += 1;
        }
    } else {
        for w in &plan.words {
            let (rank, mut t, churn, mid) = type_input(&mut s, &w.input, &w.text, true);
            total_keys += w.input.chars().count();
            let found = rank.is_some();
            let can_retry = !w.full.is_empty() && w.full != w.input;
            let (outcome, pieces) =
                if (rank.is_none() || rank.is_some_and(|r| r >= 10)) && can_retry {
                    // 简拼失败后按真实习惯重打全拼（不将就首位——将就是错误级联之源）
                    t.measure("esc", || { let _ = s.handle(InputEvent::Escape); });
                    let (rank2, mut t2, _, _) = type_input(&mut s, &w.full, &w.text, false);
                    total_keys += w.full.chars().count();
                    let (out2, pieces2) = select_expect(&mut s, rank2, &mut t2);
                    let o = if out2 == "deep_fallback" || out2 == "no_cand" {
                        "retry_fail"
                    } else {
                        "full_retry"
                    };
                    t.absorb(&t2);
                    (o, pieces2)
                } else {
                    select_expect(&mut s, rank, &mut t)
                };
            let outcome = if mid { "mid_commit" } else { outcome };
            got.push_str(&pieces);
            total_keys += 1;
            t_all.absorb(&t);
            words_json.push(format!(
                "{{\"text\":\"{}\",\"style\":\"{}\",\"input\":\"{}\",\"found\":{},\"rank\":\"{}\",\
                 \"outcome\":\"{}\",\"churn\":\"{}\",\"ms\":{:.3},\"ms_max\":{:.3}}}",
                esc(&w.text), esc(&w.style), w.input, found as u8,
                rank.map(|r| r.to_string()).unwrap_or_else(|| "-".into()),
                outcome, churn, t.ms, t.ms_max,
            ));
        }
    }

    // 句末标点：候选窗应已关闭并映射全角；被翻页吞掉/原样上屏都要记
    if !plan.punct.is_empty() {
        let key = plan.punct.chars().next().unwrap_or('.');
        let mut flag = "ok";
        if !s.pinyin().is_empty() {
            flag = "punct_with_preedit";
        }
        match t_all.measure("punct", || s.handle(InputEvent::Char(key))) {
            Ok(SessionOutput::Commit(text)) => {
                got.push_str(&text);
                if text.contains(key) {
                    flag = if flag == "ok" { "punct_raw" } else { flag };
                }
            }
            Ok(_) => flag = "punct_paged", // 标点被当成翻页/无操作
            Err(_) => flag = "punct_err",
        }
        total_keys += 1;
        words_json.push(format!("{{\"text\":\"<punct:{}>\",\"flag\":\"{}\"}}", esc(&plan.punct), flag));
    }

    format!(
        "{{\"id\":\"{}\",\"mode\":\"{}\",\"keys\":{},\"ms\":{:.3},\"ms_max\":{:.3},\
         \"slow\":\"{}\",\"got\":\"{}\",\"sent\":\"{}\",\"words\":[{}],\"threads\":{}}}",
        esc(&plan.id), plan.mode, total_keys, t_all.ms, t_all.ms_max,
        t_all.slow.join(","), esc(&got), esc(&plan.sent),
        words_json.join(","), threads,
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().ok_or("用法: typing_sim word|session --in F --out F [--threads N] [--limit N] [--user F|--user-mem]")?.clone();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let input = get("--in").ok_or("缺 --in")?;
    let output = get("--out").ok_or("缺 --out")?;
    let threads: usize = get("--threads").and_then(|v| v.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4))
        .min(16);
    let limit: usize = get("--limit").and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);

    let user = get("--user");
    let user_mem = args.iter().any(|a| a == "--user-mem");
    let engine = load_engine(user.as_deref(), user_mem)?;
    let file = std::fs::read_to_string(&input)?;
    let mut lines: VecDeque<String> = file.lines().map(String::from).filter(|l| !l.trim().is_empty()).collect();
    lines.truncate(limit);
    let total = lines.len();
    println!("mode={mode} 计划 {total} threads={threads}");

    let (tx, rx) = mpsc::channel::<String>();
    for line in &lines {
        tx.send(line.clone()).expect("send");
    }
    drop(tx); // 先灌满再收工：无界信道不阻塞，worker 收尽即退出

    let rx = Arc::new(Mutex::new(rx));
    let out = Arc::new(Mutex::new(BufWriter::new(std::fs::File::create(&output)?)));
    let done = Arc::new(AtomicUsize::new(0));

    std::thread::scope(|sc| {
        for _ in 0..threads {
            let (engine, rx, out, done, mode) = (engine.clone(), rx.clone(), out.clone(), done.clone(), mode.clone());
            sc.spawn(move || {
                // 预热：首击键的冷页故障不计入样本
                let _ = engine.lookup_candidates("nihao", &LmContext::empty());
                let mut s = engine.create_session();
                for ch in "zhongguoren".chars() {
                    let _ = s.handle(InputEvent::Char(ch));
                }
                loop {
                let Ok(line) = rx.lock().expect("rx").recv() else { break };
                let result = match mode.as_str() {
                    "word" => run_word(&engine, &line, threads),
                    "session" => run_session(&engine, &parse_plan(&line), threads),
                    other => format!("{{\"error\":\"unknown mode {other}\"}}"),
                };
                {
                    let mut w = out.lock().expect("out");
                    writeln!(w, "{result}").expect("write");
                }
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n % 5000 == 0 {
                    eprintln!("  {n}/{total}");
                }
                }
            });
        }
    });

    out.lock().expect("out").flush()?;
    println!("完成 {total} → {output}");
    Ok(())
}