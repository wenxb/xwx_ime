//! 回归探针:把「候选列表 + 会话行为 + 单次延迟」钉成文本,改动前后逐字节对拍。
//!
//! docs/HANDOFF.md 定的判退化方式就是它:改动前先跑一遍存成基线,改完再跑
//! 同一个探针 `diff`——**不靠「前 10 重合率」这类指标**,而是逐位比对候选
//! 列表本身(顺序、词、拼音、层、分数都要一致)。第 ① 期(分段上屏)与第 ② 期
//! (容错解码)都是用它验收的,基线留在 `/tmp/xwx_phase0_baseline.txt`。
//!
//! ## 三类输入,读结果时分开看
//!
//! - **A 段**:`lookup_candidates` 的完整候选列表(逐字节对拍的主体);
//! - **B 段**:会话级行为(打字 → 选第 1 个候选 → 上屏/剩余串);
//! - 每块的 `min_ms` 是单次查询最短耗时(30 次取最小),用于盯住延迟门槛。
//!
//! 打对的输入(A 段里没有错拼的那些)基线必须**逐字节不变**;为了容错解码
//! 而**故意**改变的输入(错拼、乱码段)在改动说明里点名列出,其余差异都算
//! 退化。
//!
//! 用法:
//! ```sh
//! cargo run --release --offline -p xwx_engine --example regression_probe -- /tmp/xwx_after.txt
//! ```
//! 不接用户层(确定性):只量词库 + 万象 LM + 切分/解码的固有行为。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use xwx_engine::{ImeEngine, InputEvent, LmContext, SessionOutput};

/// 场景矩阵:正确串 / 多打 / 少打 / 替换 / 调换 / 乱码,错在头/中/尾。
const SCENARIOS: &[&str] = &[
    // —— 回归对拍集(这些改动前后必须逐字节相同)——
    "nihao", "ni", "xian", "ky", "mei", "meiy", "zhongg", "zhongguo", "zhongguor", "women",
    "nimen", "nhao", "feicde", "sh", "zh", "zho", "niha", "beijin", "meiyo", "xianzai", "jintian",
    "gongyuan", "zzzz", "zhzhzhzh", "aaaaaaaa", "wsm", "zgr", "nih",
    // —— 短词错误矩阵 ——
    "nihaoq",    // 多打(尾)
    "qnihao",    // 乱码(头)
    "niqhao",    // 多打(中)
    "nihoa",     // 调换(用户报的 nihoa)
    "nhao",      // 少打
    "nihaoo",    // 重复字母
    "shenem",    // 调换(CHIME 例子:shenme)
    "shanghaai", // 重复字母(CHIME 例子:shanghai)
    // —— 长串错误矩阵 ——
    "womenjintianqugongyuan",  // 正确基线
    "womenjintianqugongyuann", // 多打(尾)
    "womenjintianqqugongyuan", // 多打(中)
    "womenjintianzzgongyuan",  // 乱码(中)
    "womemjintianqugongyuan",  // 替换(头,m/n)
    "womenjinxtianqugongyuan", // 多打(音节内)
    "womenjitianqugongyuan",   // 少打(音节内,jintian→jitian)
    "womenjintianqugonguan",   // 少打(尾音节,gongyuan→gonguan)
    "womenjintianqugongyuam",  // 替换(尾,n/m)
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/xwx_phase0_baseline.txt".to_owned());
    let dir = PathBuf::from(std::env::var("HOME")?).join(".local/share/xwx");
    let dict_path = dir.join("dict.bin");
    let abbrev_path = dir.join("abbrev.bin");
    let lm_path = dir.join("wanxiang-lts-zh-hans.gram");

    let mut engine = ImeEngine::new_with_lm(Some(&dict_path), Some(&lm_path))?;
    engine.dict = engine.dict.with_abbrev(&abbrev_path);
    let engine = Arc::new(engine);

    let mut out = String::new();
    out.push_str("# xwx_ime 长串容错基线(无用户层;词库+万象 LM)\n");

    // —— A. 引擎层:候选列表 + 单次耗时 ——
    out.push_str("\n## A. lookup_candidates\n");
    let ctx = LmContext::empty();
    for input in SCENARIOS {
        for _ in 0..5 {
            let _ = engine.lookup_candidates(input, &ctx)?;
        }
        let mut min_ms = f64::MAX;
        let mut cands = Vec::new();
        for _ in 0..30 {
            let t0 = Instant::now();
            cands = engine.lookup_candidates(input, &ctx)?;
            min_ms = min_ms.min(t0.elapsed().as_secs_f64() * 1000.0);
        }
        out.push_str(&format!(
            "\n### {input}\nn={} min_ms={min_ms:.3}\n",
            cands.len()
        ));
        for (i, c) in cands.iter().enumerate() {
            out.push_str(&format!(
                "{i}|{}|{}|{}|{:.4}|{:?}|echo={}\n",
                c.word, c.pinyin, c.freq, c.score, c.spelling, c.echo
            ));
        }
    }

    // —— B. 会话层:选第 1 个候选之后,剩余串还剩什么 ——
    out.push_str("\n## B. session(打字 → 选第 1 个候选)\n");
    for input in SCENARIOS {
        let mut s = engine.create_session();
        let mut before: Vec<String> = Vec::new();
        for ch in input.chars() {
            if let SessionOutput::Updated(u) = s.handle(InputEvent::Char(ch))? {
                before = u.candidates.iter().take(3).map(|c| c.word.clone()).collect();
            }
        }
        let committed = s.commit_at(0);
        out.push_str(&format!(
            "\n### {input}\ntop3_before={before:?}\ncommitted={committed:?}\npreedit_after={:?}\n",
            s.pinyin(),
        ));
    }

    std::fs::write(&out_path, &out)?;
    println!("baseline written: {out_path} ({} bytes)", out.len());
    Ok(())
}
