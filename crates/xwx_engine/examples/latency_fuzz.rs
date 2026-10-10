//! 延迟模糊测试：**自己找出能把引擎卡几秒的输入**。
//!
//! 为什么需要它：用户遇到「偶尔卡死几秒、卡完还丢字」。丢字说明那些按键没被
//! 处理——要么引擎崩了重启，要么它活着但慢到 ibus-daemon 超时把键扔掉。
//! 后者能在代码里找到，前提是先**找到那个慢输入**。逐键延迟探针只测了几种
//! 「典型」输入（全拼/简拼），没覆盖到病态形态；这里系统地扫。
//!
//! 生成的输入形态（都来自真实打字会遇到的情况）：
//! - 越来越长的连续拼音（不打空格、不提交）——组合串只增不减；
//! - 纯辅音简拼（候选最少的形态，但格最贵）；
//! - 同一个字母重复（`aaaa…`/`nnnn…`，把音节切分的组合爆炸打满）；
//! - 随机字母串（覆盖没人在意的角落）；
//! - 真实中文句子的拼音（长句不打断）。
//!
//! 每个输入在**共享会话**里逐键打（不重置），因为真实用户也是在一个会话里
//! 连续打字的——`committed_tail`/`last_word` 的状态会影响后续查询的代价。
//!
//! 用法：
//! ```text
//! cargo run --release -p xwx_engine --example latency_fuzz -- \
//!   --data data --lm <gram> [--rounds 200] [--seed 1]
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use xwx_engine::{ImeEngine, InputEvent, InputSession};

/// 一条超时记录。
struct Slow {
    input: String,
    key: usize,
    ms: f64,
    candidates: usize,
}

const PINYIN_LETTERS: &[u8] = b"abcdefghjklmnopqrstuvwxyz";

/// 线性同余伪随机：测试输入要**可复现**，不能用真随机。
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // Numerical Recipes 的 LCG（64 位），够用且确定。
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn range(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn build(data: &PathBuf, lm: &PathBuf) -> Result<ImeEngine, Box<dyn std::error::Error>> {
    let mut engine = ImeEngine::new_with_lm(Some(&data.join("dict.bin")), Some(lm))?;
    engine.dict = engine.dict.with_abbrev(data.join("abbrev.bin"));
    engine.dict = engine.dict.with_syllable_graph(data.join("syllable_graph.bin"));
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
    let rounds: usize = get("--rounds", "150").parse().unwrap_or(150);
    let seed: u64 = get("--seed", "1").parse().unwrap_or(1);
    let threshold_ms: f64 = get("--threshold", "50").parse().unwrap_or(50.0);
    if lm.as_os_str().is_empty() {
        return Err("--lm 必填".into());
    }

    let engine = Arc::new(build(&data, &lm)?);
    println!("== 延迟模糊测试 ==");
    println!("data={data:?} lm={lm:?} rounds={rounds} seed={seed} 门槛={threshold_ms}ms");

    let mut slow: Vec<Slow> = Vec::new();
    let mut checked = 0usize;
    let mut rng = Rng(seed);

    // 共享会话：模拟真实用户连续打字，不重置。
    let mut session: Option<InputSession> = None;

    for round in 0..rounds {
        // 轮换四种形态，权重均匀。
        let (input, kind): (String, &str) = match round % 4 {
            0 => {
                // 越来越长的连续拼音（不打断）
                let n = 8 + rng.range(40);
                let s: String = (0..n)
                    .map(|_| PINYIN_LETTERS[rng.range(PINYIN_LETTERS.len())] as char)
                    .collect();
                (s, "连续拼音")
            }
            1 => {
                // 纯辅音简拼（格最贵）
                let cons: &[u8] = b"bpmfdtnlgkhjqxzhchshrzcsyw";
                let n = 4 + rng.range(14);
                let s: String = (0..n).map(|_| cons[rng.range(cons.len())] as char).collect();
                (s, "纯辅音")
            }
            2 => {
                // 同一字母重复（音节切分组合爆炸）
                let c = PINYIN_LETTERS[rng.range(PINYIN_LETTERS.len())];
                let n = 4 + rng.range(20);
                (std::iter::repeat(c as char).take(n).collect(), "重复字母")
            }
            _ => {
                // 随机字母串
                let n = 3 + rng.range(30);
                let s: String = (0..n)
                    .map(|_| PINYIN_LETTERS[rng.range(PINYIN_LETTERS.len())] as char)
                    .collect();
                (s, "随机")
            }
        };

        let sess = session.get_or_insert_with(|| engine.create_session());
        for (i, ch) in input.chars().enumerate() {
            let t = Instant::now();
            let out = sess.handle(InputEvent::Char(ch)).expect("按键");
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            checked += 1;
            if ms >= threshold_ms {
                let n = match &out {
                    xwx_engine::SessionOutput::Updated(u) => u.candidates.len(),
                    _ => 0,
                };
                slow.push(Slow {
                    input: input.clone(),
                    key: i + 1,
                    ms,
                    candidates: n,
                });
            }
        }
        // 每 8 轮换一个会话：既积累上文，又不至于让组合态无限变长。
        if round % 8 == 7 {
            session = None;
        }
    }

    slow.sort_by(|a, b| b.ms.partial_cmp(&a.ms).unwrap_or(std::cmp::Ordering::Equal));
    println!("\n按键总数 {checked}，超过 {threshold_ms}ms 的 {}/{} 次（{:.3}%）",
        slow.len(), checked, 100.0 * slow.len() as f64 / checked.max(1) as f64);

    if slow.is_empty() {
        println!("没有发现超阈值输入。提高 --rounds 或降低 --threshold 再试。");
        return Ok(());
    }

    println!("\n== 最慢的 20 次 ==");
    for s in slow.iter().take(20) {
        println!(
            "  {:>9.2}ms  第{}键/共{}键  候选={:<5} 输入={:?} ({})",
            s.ms,
            s.key,
            s.input.chars().count(),
            s.candidates,
            s.input,
            if s.input.chars().all(|c| !"aeiou".contains(c)) { "纯辅音" } else { "混合" }
        );
    }

    // 按形态聚合：哪个形态最贵。
    println!("\n== 按输入形态 ==");
    let mut by_kind: Vec<(&str, f64, usize)> = Vec::new();
    for kind in ["连续拼音", "纯辅音", "重复字母", "随机"] {
        let rows: Vec<&Slow> = slow
            .iter()
            .filter(|s| {
                let k = if s.input.chars().all(|c| !"aeiou".contains(c)) {
                    if s.input.chars().collect::<Vec<_>>().windows(2).all(|w| w[0] == w[1]) {
                        "重复字母"
                    } else {
                        "纯辅音"
                    }
                } else {
                    "混合"
                };
                // 形态名与生成时的标签对应，这里用可观测特征近似归类。
                k == kind || (kind == "连续拼音" && k == "混合")
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        let worst = rows.iter().map(|s| s.ms).fold(0.0, f64::max);
        by_kind.push((kind, worst, rows.len()));
    }
    for (k, worst, n) in by_kind {
        println!("  {k:<10} 最慢 {worst:>9.2}ms   超阈值 {n} 次");
    }
    Ok(())
}
