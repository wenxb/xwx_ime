//! 崩溃模糊测试：**找出能让引擎 panic 的输入序列**。
//!
//! 为什么需要它：用户遇到「偶尔卡死几秒 + 丢字」。丢字说明那些按键没被处理；
//! 引擎中途死掉、被 ibus-daemon 重新拉起，正好同时解释「卡几秒」「丢字」
//! 引擎的 stderr 被 ibus-daemon 接到 /dev/null，panic 是**无声**的：死了也不会
//! 留下任何痕迹，所以只能靠主动打出来。
//!
//! 所以与其猜，不如直接打：把**整个按键事件空间**（不只是字母，还有退格/
//! 回车/空格/翻页/选词/屏蔽候选/中英切换/原文直出）按随机顺序、在随机长度的
//! 拼音串上打出去，看会不会 panic。逐键延迟模糊测试（`latency_fuzz`）只打
//! `Char`，覆盖不到这些路径。
//!
//! panic 会让本进程直接退出——那**就是**找到的证据（`--` 后面的进度行
//! 会停在出错的那一步）。
//!
//! 用法：
//! ```text
//! cargo run --release -p xwx_engine --example crash_fuzz -- \
//!   --data data --lm <gram> [--events 20000] [--seed 7]
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use xwx_engine::{ImeEngine, InputEvent};

/// 线性同余伪随机：输入要**可复现**，不能用真随机。
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn range(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const LETTERS: &[u8] = b"abcdefghjklmnopqrstuvwxyz";
/// 会进入拼音串/标点映射的真实键面。
const PUNCT: &[char] = &['.', ',', ';', '?', '!', '\'', '/', '-', '0', '1', '2'];

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
    let total: usize = get("--events", "20000").parse().unwrap_or(20_000);
    let seed: u64 = get("--seed", "7").parse().unwrap_or(7);
    let batch: usize = get("--batch", "400").parse().unwrap_or(400);
    if lm.as_os_str().is_empty() {
        return Err("--lm 必填".into());
    }

    let mut engine = ImeEngine::new_with_lm(Some(&data.join("dict.bin")), Some(&lm))?;
    engine.dict = engine.dict.with_abbrev(data.join("abbrev.bin"));
    engine.dict = engine.dict.with_syllable_graph(data.join("syllable_graph.bin"));
    let engine = Arc::new(engine);

    println!("== 崩溃模糊测试 == events={total} seed={seed} batch={batch}");
    let mut rng = Rng(seed);
    let mut session = engine.create_session();
    let mut done = 0usize;
    let mut worst = 0.0f64;

    while done < total {
        // 一批一批打，每批结束换一个新会话并打进度（panic 时能定位到哪一批）。
        for _ in 0..batch.min(total - done) {
            let ev = match rng.range(20) {
                0..=9 => InputEvent::Char(LETTERS[rng.range(LETTERS.len())] as char),
                10 => InputEvent::Char(PUNCT[rng.range(PUNCT.len())]),
                11 => InputEvent::Verbatim(PUNCT[rng.range(PUNCT.len())]),
                12 => InputEvent::Backspace,
                13 => InputEvent::Space,
                14 => InputEvent::Enter,
                15 => InputEvent::Escape,
                16 => InputEvent::PageDown,
                17 => InputEvent::Select(rng.range(10) as u8),
                18 => InputEvent::DeleteCandidate,
                _ => InputEvent::ToggleAscii,
            };
            let t = Instant::now();
            let out = session.handle(ev).expect("按键不应 panic");
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            if ms > worst {
                worst = ms;
            }
            let _ = out;
            done += 1;
        }
        session = engine.create_session();
        eprintln!("  … {done}/{total}  最慢单键 {worst:.1}ms");
    }

    println!("✅ {total} 个事件无 panic；最慢单键 {worst:.1}ms");
    Ok(())
}
