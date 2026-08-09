//! 用户词频学习效果评测（learn_eval）。
//!
//! 回答一个问题：**「上屏即学」真的让排序变好了吗？**
//!
//! 单元测试已证明「机制能跑」（分数会变、能持久化、上界生效），
//! 但机制正确 ≠ 有效果。本评测把「学习」和「评测」严格分开，
//! 用一颗**有真实提升空间**的受控词典，衡量「学了之后排序是否更好」。
//!
//! ## 防作弊设计（本评测唯一的价值所在）
//!
//! - **训练走真实链路**：`InputSession` 收键 → `commit_word` 上屏 →
//!   引擎 `learn_committed` → `UserLM` 计数。绝不直接往 `UserLM` 里塞数据。
//! - **训练/测试分离（方案 a）+ 泛化（方案 b）双保险**：
//!   - 训练集 A：一组「上文→目标词」搭配 `(甲_i, 乙_i)`，重复上屏多次；
//!   - 测试集 B：**同一批目标词** `乙_i`，但上文换成**训练从未出现过的
//!     丙_i** ，查询 `(丙_i, 乙_i)` 的候选排序。
//!   - 学了 `(甲, 乙)` 之后 `(丙, 乙)` 也好转，说明模型学到的不只是这条
//!     记录，而是「乙是用户常用词」——unigram 跨上文泛化生效。
//!     这**不是作弊**：`(丙, 乙)` 这个搭配从未进过训练，排序改善只能
//!     来自 unigram 计数，而不是「把答案写进去再读出来」。
//! - **更严格的负对照（方案 c）**：另有一组目标词**完全没学过**
//!   （held-out），它们的排序**应几乎不变**——从而证明前面训练组的
//!   改善来自学习本身，而不是词典/引擎的全局偏移。
//!
//! ## 为什么用内存可控词典（不用真实 dict.bin / .gram）
//!
//! 真实词典里「目标词本就是 top1」的样本没有提升空间；评测也不能依赖
//! 80MB 的 dict.bin 或 400MB 的 .gram 在场。因此本评测在**内存词典**
//! 里手工构造一个「同音竞争场」：每个目标词配**多个同拼音、词频更高**
//! 的干扰词。这样：
//! - 学习前目标词既不在 top1、也排不进 top5（有真实提升空间）；
//! - 学习后是否翻盘，只看用户模型本身；
//! - 全流程不落盘（用户模型用 [`UserLM::in_memory`]），可复现、
//!   不碰 `$XDG_DATA_HOME/xwx/` 真实用户数据。
//!
//! 另附一个**持久化冒烟**子检查：把用户模型写进 `/tmp` 临时目录、
//! 重载后重测，验证「学会的效果跨持久化保留」（写临时目录）。
//!
//! ## 用法
//!
//! ```sh
//! cargo run --offline -p xwx_engine --example learn_eval
//! ```
//!
//! 不需要任何真实数据文件，一条命令出全部数字。

use std::sync::Arc;

use xwx_engine::{ImeEngine, InputEvent, InputSession, LmContext, UserLM};

/// 一组训练/测试样本：目标词 + 一组同音高词频干扰词 + 训练/测试上文。
struct Sample {
    /// 目标词（用户想打的词，词典里低频，未学习时排不进 top1）。
    word: &'static str,
    /// 目标词拼音（所有干扰词同拼音）。
    pinyin: &'static str,
    /// 干扰词+词频（同拼音、词频都高于目标词，制造早期竞争）。
    distractors: &'static [(&'static str, u64)],
    /// 目标词词频。
    word_freq: u64,
    /// 训练上文（训练时反复「训练上文 + 目标词」上屏）。
    train_prev: &'static str,
    /// 训练上文拼音。
    train_prev_py: &'static str,
    /// 测试上文（训练里从未出现；评测时查「测试上文 + 目标词」）。
    test_prev: &'static str,
    /// 测试上文拼音。
    test_prev_py: &'static str,
    /// 是否参与训练（false = 负对照 held-out）。
    trained: bool,
}

const SAMPLES: &[Sample] = &[
    // ── 训练组（学了 (甲,乙)，测 (丙,乙)） ──
    Sample {
        word: "评", pinyin: "ping",
        distractors: &[("瓶", 118), ("平", 114), ("苹", 110), ("凭", 106), ("砰", 103)],
        word_freq: 100,
        train_prev: "批", train_prev_py: "pi",
        test_prev: "差", test_prev_py: "cha",
        trained: true,
    },
    Sample {
        word: "账", pinyin: "zhang",
        distractors: &[("张", 120), ("长", 116), ("掌", 112), ("障", 108), ("胀", 104)],
        word_freq: 100,
        train_prev: "记", train_prev_py: "ji",
        test_prev: "查", test_prev_py: "cha",
        trained: true,
    },
    Sample {
        word: "篮", pinyin: "lan",
        distractors: &[("兰", 122), ("拦", 118), ("烂", 112), ("缆", 107), ("澜", 103)],
        word_freq: 100,
        train_prev: "打", train_prev_py: "da",
        test_prev: "观", test_prev_py: "guan",
        trained: true,
    },
    // ── 负对照（完全不上屏训练） ──
    Sample {
        word: "辙", pinyin: "zhe",
        distractors: &[("这", 120), ("者", 115), ("遮", 110), ("折", 105), ("蜇", 102)],
        word_freq: 100,
        train_prev: "覆", train_prev_py: "fu",
        test_prev: "兴", test_prev_py: "xing",
        trained: false,
    },
    Sample {
        word: "溃", pinyin: "kui",
        distractors: &[("亏", 125), ("盔", 115), ("窥", 110), ("奎", 105), ("傀", 102)],
        word_freq: 100,
        train_prev: "崩", train_prev_py: "beng",
        test_prev: "幸", test_prev_py: "xing",
        trained: false,
    },
];

/// 每个训练样本重复上屏次数（模拟用户反复用这个词）。
const TRAIN_REPEATS: usize = 12;

/// 构建可控内存词典：插入所有目标词、干扰词、训练/测试上文词。
fn build_dict(engine: &ImeEngine) {
    for s in SAMPLES {
        engine.dict.upsert(s.word, s.pinyin, s.word_freq).expect("插入目标词失败");
        for (d, freq) in s.distractors {
            engine.dict.upsert(d, s.pinyin, *freq).expect("插入干扰词失败");
        }
        engine
            .dict
            .upsert(s.train_prev, s.train_prev_py, 100)
            .expect("插入训练上文失败");
        engine
            .dict
            .upsert(s.test_prev, s.test_prev_py, 100)
            .expect("插入测试上文失败");
    }
}

/// 在会话里敲一串拼音（模拟键盘事件流）。
fn type_pinyin(session: &mut InputSession, pinyin: &str) {
    for ch in pinyin.chars() {
        session.handle(InputEvent::Char(ch)).expect("打字事件不应失败");
    }
}

/// 上屏指定词，走完整真实链路：打字 → 查候选 → `commit_at`。
///
/// `commit_at` 与真实 IBus 适配层「点候选 / 数字选词」同一条路
/// （都收敛到 `commit_word` → 会话 `record_committed` → 引擎
/// `learn_committed` → `UserLM::learn`）。
fn commit_word(engine: &Arc<ImeEngine>, session: &mut InputSession, pinyin: &str, word: &str) {
    type_pinyin(session, pinyin);
    let cands = engine
        .lookup_candidates(pinyin, &session.lm_context())
        .expect("查询候选失败");
    let idx = cands
        .iter()
        .position(|c| c.word == word)
        .unwrap_or_else(|| panic!("「{word}」({pinyin}) 不在候选列表"));
    let got = session.commit_at(idx).expect("上屏失败");
    assert_eq!(got, word, "上屏的词应与目标一致");
}

/// 训练：对每个 `trained` 样本，用它的训练上文反复上屏目标词。
///
/// 只学 `train_prev`；评测用的 `test_prev` 全程没出现，保证
/// (丙,乙) 这个搭配零训练数据。——这就是泛化测试的由来。
fn train(engine: &Arc<ImeEngine>) {
    for s in SAMPLES.iter().filter(|s| s.trained) {
        for _ in 0..TRAIN_REPEATS {
            let mut session = engine.create_session();
            commit_word(engine, &mut session, s.train_prev_py, s.train_prev);
            commit_word(engine, &mut session, s.pinyin, s.word);
        }
    }
}

/// 评测一个样本：在「测试上文 + 目标拼音」下，目标词排在候选里的第几名。
/// 只查询（`lookup_candidates` 与 `session.refresh` 同一条管线），不学习。
fn rank_of(engine: &ImeEngine, s: &Sample) -> Option<usize> {
    let ctx = LmContext { tail: s.test_prev, last_word: Some(s.test_prev) };
    let cands = engine
        .lookup_candidates(s.pinyin, &ctx)
        .expect("评测查询失败");
    cands.iter().position(|c| c.word == s.word)
}

/// 一组样本的 top1/top5 命中统计。
#[derive(Default)]
struct HitStats {
    n: usize,
    top1: usize,
    top5: usize,
}

impl HitStats {
    fn add(&mut self, rank: Option<usize>) {
        self.n += 1;
        if let Some(r) = rank {
            if r == 0 {
                self.top1 += 1;
            }
            if r < 5 {
                self.top5 += 1;
            }
        }
    }
    fn pct(&self, x: usize) -> f64 {
        if self.n == 0 { 0.0 } else { x as f64 / self.n as f64 * 100.0 }
    }
    fn top1(&self) -> f64 { self.pct(self.top1) }
    fn top5(&self) -> f64 { self.pct(self.top5) }
}

/// 测一组样本的命中率（不触发任何学习）。
fn eval_group(engine: &ImeEngine, trained: bool) -> HitStats {
    let mut h = HitStats::default();
    for s in SAMPLES.iter().filter(|s| s.trained == trained) {
        h.add(rank_of(engine, s));
    }
    h
}

/// 打印一颗引擎的训练组 + 负对照命中率。
fn report(engine: &ImeEngine, title: &str) {
    let trained_hit = eval_group(engine, true);
    let heldout_hit = eval_group(engine, false);
    println!("[{title}]");
    println!(
        "  训练组(泛化)  top1 {:5.1}%  top5 {:5.1}%  (n={})",
        trained_hit.top1(), trained_hit.top5(), trained_hit.n
    );
    println!(
        "  负对照        top1 {:5.1}%  top5 {:5.1}%  (n={})",
        heldout_hit.top1(), heldout_hit.top5(), heldout_hit.n
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // —— 1. 学习前基线 ——
    let plain = ImeEngine::new(None)?;
    build_dict(&plain);
    report(&plain, "学习前基线（无用户模型）");
    println!();

    // —— 2. 训练（真实会话链路），再评测 ——
    let engine = Arc::new(ImeEngine::new(None)?.with_user_lm(UserLM::in_memory()));
    build_dict(&engine);
    train(&engine);
    report(&engine, "学习后（训练跑完）");
    println!();

    println!(
        "说明：训练只喂了 {} 组搭配 (甲,乙)；评测用 (丙,乙)，丙 从未出现。",
        SAMPLES.iter().filter(|s| s.trained).count(),
    );
    println!("      → 训练组若改善，来自 unigram 跨上文泛化；负对照应几乎不动。");

    // —— 3. 持久化冒烟：写临时目录、重载、重测 ——
    let tmp = std::env::temp_dir().join("xwx_learn_eval_persist");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let path = tmp.join("user_bigram.txt");

    let p1 = Arc::new(ImeEngine::new(None)?.with_user_lm(UserLM::open_or_create(&path)));
    build_dict(&p1);
    train(&p1);
    p1.user_lm().expect("引擎应有用户层").flush().expect("flush 失败");
    drop(p1);

    let p2 = Arc::new(ImeEngine::new(None)?.with_user_lm(UserLM::open_or_create(&path)));
    build_dict(&p2);
    println!("[持久化冒烟] 重载临时目录用户模型后：");
    report(&p2, "重载后");
    let _ = std::fs::remove_dir_all(&tmp);
    println!("临时目录已删除；真实 $XDG_DATA_HOME/xwx/ 全程未触碰。");

    Ok(())
}
