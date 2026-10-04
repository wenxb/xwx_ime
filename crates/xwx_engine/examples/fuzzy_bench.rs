//! 模糊拼音开关的代价/收益基准。
//!
//! 模糊拼音(`corrector.rs` 的 13 组混淆对)在本次改动前**从未在生产进程里
//! 启用过**——`fuzzy_enabled` 只在单测里被置 true(见 `lib.rs` 测试),`main.rs`
//! 从不设置它。这个基准回答两个必须用实测数字回答的问题,作为
//! `DEFAULT_FUZZY_ENABLED` 取值的裁决依据:
//!
//! 1. **代价(延迟)**:开启后每条音节链的拼音串要经 `expand_fuzzy_scored`
//!    展开成多个变体键、逐个查词,格边数量变多,直接放大击键延迟
//!    (IBus 侧同步处理每次按键)。必须给出 `--release` 实测数字。
//! 2. **代价(排序)**:模糊边带 `-log10(weight)` 惩罚、且落在
//!    `SpellingKind::Fuzzy` 层(严格排在 Exact 层之后)。要确认它
//!    **不会**把精确候选挤出第一页。
//! 3. **收益(召回)**:方言输入(如 `lansheng` = n/l 混的「男生」)在关闭时
//!    出不来、开启后能出来——没有收益就不能默认开。
//!
//! 判据在测量前钉死(项目约定):
//! - 延迟:开模糊后 `zhongg` 端到端 < 8ms、最长全拼串 < 12ms(既有上限);
//! - 排序:**全拼精确候选的前 10 名逐位不变**——这是「不许退化」硬约束;
//! - 召回:至少一个方言输入从「目标词不在候选里」变为「在候选里」。
//!
//! 用法:
//! ```sh
//! XWX_DICT_PATH=$PWD/data/dict.bin XWX_ABBREV_PATH=$PWD/data/abbrev.bin \
//!   cargo run --release --offline -p xwx_engine --example fuzzy_bench
//! ```

use std::path::Path;
use std::time::Instant;

use xwx_engine::{ImeEngine, LmContext};

/// 方言/常见误输入样本:(输入串, 期望能召回的目标词)。
///
/// 每组都对应 `COMMON_CONFUSION_PAIRS` 里的一条混淆对;目标词是「按标准
/// 拼音打不出来、只有开启模糊才可能命中」的那一个。权重低的组(0.2/0.3)
/// 展开后惩罚大,不一定进前 10,这里只要求**出现在候选里**(召回口径)。
const FUZZY_PROBES: &[(&str, &str, &str)] = &[
    // (输入, 目标词, 对应混淆对)
    ("lansheng", "男生", "n/l"),
    ("nansheng", "男生", "基准:标准拼音应已能出"),
    ("zhongguo", "中国", "基准"),
    ("zongguo", "中国", "zh/z"),
    ("suozi", "桌子", "zh/z"),
    ("zhuozi", "桌子", "基准"),
    ("huanghe", "黄河", "基准"),
    ("fanhual", "繁华", "f/h"),
];

/// 全拼输入:开模糊后前 10 名必须逐位不变。
const EXACT_GUARD: &[&str] = &[
    "nihao",
    "zhongguo",
    "beijing",
    "diannao",
    "shanghai",
    "gongzuo",
    "xuexi",
    "jintiantianqihenhao",
    "woshizhongguoren",
    "womenjintianquxian",
];

fn engine(fuzzy: bool, lm: Option<&Path>) -> Result<ImeEngine, Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let dict_path = std::env::var_os("XWX_DICT_PATH")
        .map(Into::into)
        .unwrap_or_else(|| root.join("dict.bin"));
    let abbrev_path = std::env::var_os("XWX_ABBREV_PATH")
        .map(Into::into)
        .unwrap_or_else(|| root.join("abbrev.bin"));

    let mut e = ImeEngine::new_with_lm(Some(&dict_path), lm)?;
    e.dict = e.dict.with_abbrev(&abbrev_path);
    e.fuzzy_enabled = fuzzy;
    Ok(e)
}

fn top10(e: &ImeEngine, input: &str) -> Vec<String> {
    e.lookup_candidates(input, &LmContext::empty())
        .map(|v| v.into_iter().take(10).map(|c| c.word).collect())
        .unwrap_or_default()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 语言模型可选:仓库里没有 .gram,自动探测 XDG 目录;缺失时按
    // 仅词频打分(i.e. 与用户首次启动、还没下载模型时同构)。
    let gram = std::env::var_os("XWX_LM_PATH")
        .map(Into::into)
        .or_else(|| {
            let p = Path::new(&std::env::var("HOME").ok()?)
                .join(".local/share/xwx/wanxiang-lts-zh-hans.gram");
            p.is_file().then_some(p)
        });
    println!("LM: {}", gram.as_ref().map_or("(无,仅词频)".to_string(), |p| p.display().to_string()));

    let off = engine(false, gram.as_deref())?;
    let on = engine(true, gram.as_deref())?;
    println!("dict entries: {}\n", off.dict_count());

    // —— 1. 延迟(端到端 lookup_candidates) ——
    let lat_inputs: &[&str] = &["nihao", "zhongguo", "zhongg", "zgr", "womenjintianquxian"];
    println!("== 端到端延迟 关/开 模糊(release, 预热 20 次后取 100 次 min/avg/p95)==");
    println!("{:<22} {:>22} {:>22} {:>9}", "输入", "关(ms)", "开(ms)", "倍率");
    let mut worst_ratio = 0.0f64;
    for &inp in lat_inputs {
        let a = bench(&off, inp);
        let b = bench(&on, inp);
        let ratio = b.0 / a.0.max(1e-9);
        worst_ratio = worst_ratio.max(ratio);
        println!(
            "{:<22} {:>7.3}/{:>7.3}/{:>7.3} {:>7.3}/{:>7.3}/{:>7.3} {:>8.2}x",
            inp,
            a.0, a.1, a.2,
            b.0, b.1, b.2,
            ratio
        );
    }
    println!("最差倍率: {worst_ratio:.2}x\n");

    // —— 2. 排序退化守卫(精确候选前 10 逐位对比) ——
    println!("== 全拼前 10 逐位对比(关 vs 开) ==");
    let mut regressions = 0;
    for &inp in EXACT_GUARD {
        let a = top10(&off, inp);
        let b = top10(&on, inp);
        let same = a == b;
        if !same {
            regressions += 1;
            println!("  ✗ {inp}\n      关: {a:?}\n      开: {b:?}");
        } else {
            println!("  ✓ {inp}  前10逐位一致");
        }
    }
    println!("退化输入数: {regressions}/{}\n", EXACT_GUARD.len());

    // —— 3. 召回收益 ——
    println!("== 召回(目标词是否出现在候选里) ==");
    println!("{:<14} {:<8} {:>12} {:>12}", "输入", "目标词", "关:名次", "开:名次");
    let mut gains = 0;
    for &(inp, target, tag) in FUZZY_PROBES {
        let ra = rank(&off, inp, target);
        let rb = rank(&on, inp, target);
        if ra.is_none() && rb.is_some() {
            gains += 1;
        }
        println!(
            "{:<14} {:<8} {:>12} {:>12}   ({tag})",
            inp,
            target,
            ra.map_or("未召回".to_string(), |r| format!("#{r}")),
            rb.map_or("未召回".to_string(), |r| format!("#{r}"))
        );
    }
    println!("新召回(关未召回 → 开召回)数: {gains}\n");

    // —— 结论 ——
    println!("== 结论 ==");
    println!("延迟最差倍率 {worst_ratio:.2}x;前 10 退化 {regressions} 组;新召回 {gains} 组");
    Ok(())
}

/// 某候选在结果里的名次(1-based);不在候选里返回 None。
fn rank(e: &ImeEngine, input: &str, target: &str) -> Option<usize> {
    e.lookup_candidates(input, &LmContext::empty())
        .ok()?
        .iter()
        .position(|c| c.word == target)
        .map(|i| i + 1)
}

/// (min, avg, p95),毫秒。预热后测量,排除冷路径。
fn bench(e: &ImeEngine, input: &str) -> (f64, f64, f64) {
    for _ in 0..20 {
        let _ = e.lookup_candidates(input, &LmContext::empty());
    }
    let mut samples = Vec::with_capacity(100);
    for _ in 0..100 {
        let t = Instant::now();
        let _ = e.lookup_candidates(input, &LmContext::empty());
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let min = samples[0];
    let avg = samples.iter().sum::<f64>() / samples.len() as f64;
    let p95 = samples[(samples.len() as f64 * 0.95) as usize];
    (min, avg, p95)
}