//! 诊断：打印若干词/词链在万象 LM 下的分数。
//!
//! 用途（2026-10-03 定下的根因证据）：**空上下文下万象对所有词返回同一个常数**
//! （实测 -11.605767，连生造字「龘」也一样）。而路径打分是逐词累加 LM 项的，
//! 于是「单词覆盖整串」只付一次常数、「N 个词拼成的整串」要付 N 次——
//! 实测「杵在那」-11.6058 vs 「出→在→哪」链 -31.06，差 19.45 个 log10。
//! 这就是长串读法系统性输给单字词的原因（与词频先验、调度门槛都无关）。

use std::path::Path;

use xwx_engine::{LanguageModel, LmContext, WanxiangLM};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::var("XWX_LM_PATH").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/.local/share/xwx/wanxiang-lts-zh-hans.gram")
    });
    let lm = WanxiangLM::open(Path::new(&path))?;
    let empty = LmContext::empty();

    println!("== 单词（空上文）==");
    for w in ["杵在那", "杵", "在", "那", "出", "哪", "你好", "龘"] {
        let st = lm.state_from_context(&empty);
        let (s, _) = lm.score(&st, w);
        println!("  {w:<6} {s:+.6}");
    }

    println!("== 词链 出 → 在 → 哪 ==");
    let mut st = lm.state_from_context(&empty);
    let mut sum = 0.0f32;
    for w in ["出", "在", "哪"] {
        let (s, next) = lm.score(&st, w);
        println!("  {w:<4} {s:+.6}");
        sum += s;
        st = next;
    }
    println!("  链合计 {sum:+.6}");

    let st0 = lm.state_from_context(&empty);
    let (whole, _) = lm.score(&st0, "杵在那");
    println!("== 对比：杵在那 单词 {whole:+.6}  vs  出+在+哪 链 {sum:+.6} ==");
    Ok(())
}
