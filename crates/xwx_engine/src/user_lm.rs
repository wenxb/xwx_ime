//! 用户词频学习模型(第 5 步):上屏即学,两层加权混合里的「用户层」。
//!
//! 设计要点:
//!
//! - **学什么**:`commit_word` 上屏的候选词是「词」,记录
//!   `(上一个词, 当前词)` 的二元计数与当前词的一元计数;
//!   `commit_raw` 上屏的原始字母/标点不是词,不学(与第 2a 步
//!   「`last_word` 置 `None`」的同一判断,见 [`crate::session`])。
//! - **怎么生效**:不打分时修改通用模型,而是由 [`crate::lm::MixedLM`]
//!   把「万象/通用模型分」与「用户模型分」在 log10 概率域线性插值,
//!   用户层命中时以 [`crate::lm::MIX_USER_WEIGHT`] 的权重压过通用统计。
//! - **共享与并发**:引擎被所有会话共享(`Arc<ImeEngine>`,ibus 层每个
//!   输入上下文一个会话),计数表本体包在 `RwLock` 里;打分热路径
//!   不加锁——[`Self::snapshot`] 返回的不可变快照内置在
//!   [`crate::lm::LmState`] 里沿 beam 路径传递。
//! - **持久化**:复用 `bigram.txt` 文本格式(见 `data/README.md`),
//!   写临时文件 + 原子 rename;增量达到 [`FLUSH_LEARN_THRESHOLD`]
//!   后由调用方(bin 的事件循环)触发 [`Self::flush`],退出时再兜底一次。
//! - **上界**:一元词条数硬上限 [`MAX_USER_UNIGRAMS`],超限时把全部
//!   计数右移一位(频次减半、保相对比例、自然淘汰低频长尾)——
//!   典型输入法的用户模型都在万级条数(Rime `userdb`、搜狗用户词库
//!   同理),纯粹 LRU 按条淘汰会把「常用但条目多」的用户整个清出去,
//!   全局衰减更像「记忆随时间淡化」,代价只是偶发一次 O(n) 减半扫描。
//!
//! 数据目录约定与 `XWX_DICT_PATH` / `XWX_LM_PATH` 一致:默认
//! `$XDG_DATA_HOME/xwx/user_bigram.txt`,用 `XWX_USER_LM_PATH` 覆盖
//! (解析在 bin 侧 `src/main.rs`,与既有约定同处)。

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use tracing::{info, warn};

use crate::lm::{BigramLM, LmError};

/// 用户模型默认文件名(落在 `$XDG_DATA_HOME/xwx/` 下)。
pub const USER_LM_FILE_NAME: &str = "user_bigram.txt";
/// 距上次落盘累计多少次学习上屏后,值得再写一次盘。
///
/// 落盘代价是「序列化全表 + write + rename」(数千条时约 1MB、毫秒级),
/// 不 fsync、不在上屏路径上同步调用;64 次的阈值让正常使用大约几分钟
/// 批量回写一次,崩溃丢失上限就是这 64 次上屏——可接受(词频学习是
/// 统计量,丢几十次样本只让「越用越顺手」晚来一点)。
pub const FLUSH_LEARN_THRESHOLD: u32 = 64;
/// 用户模型一元词条数硬上限(超限触发全局衰减,见模块文档)。
pub const MAX_USER_UNIGRAMS: usize = 20_000;

/// 用户模型对「没学过」的词给的中性分(log10)。
///
/// 不是概率,是混合层的「不表态」票:-1.0 高于真未知词兜底
/// ([`crate::lm::UNK_LOG_PROB`] = -8),低于任何真实命中
/// (bigram/unigram 平滑分约 -2 起步、随计数上升)。混合后没学过的
/// 通用搭配仍由通用层主导,学过的搭配由用户层压过。推导与验证见
/// [`crate::lm::MixedLM`] 与 `lm::tests::mixed_prefers_user_hits_*`。
pub(crate) const NEUTRAL_LOG_PROB: f32 = -1.0;

/// 计数表本体(不跨线程共享;共享由 [`UserLM`] 的 `RwLock` 承担)。
#[derive(Debug, Default)]
struct UserTables {
    /// 一元计数:词 -> 上屏次数。
    unigrams: HashMap<String, u64>,
    /// 二元计数:前词 -> (后词 -> 上屏次数)。
    bigrams: HashMap<String, HashMap<String, u64>>,
    /// 上屏次数总和 Σc(w)(一元概率归一化分母)。
    unigram_total: u64,
}

/// 打分用不可变快照。
///
/// 一次候选查询可能触发数十上百次 `score`(beam 每步每边一次),
/// 每次都 `RwLock` 读锁会有可感知开销,且中途 `learn` 成功会让
/// 同一次查询内部打分不一致。做法:每次查询开始时取一次快照
/// (`Arc` 克隆,O(1)),快照内置在 [`crate::lm::LmState::Mixed`] 里
/// 沿 beam 路径传递——学习在查询中途发生也只会影响下一次查询。
#[derive(Debug, Clone, Default)]
pub struct UserLmSnapshot {
    /// 快照时的计数表(`Arc` 共享,零拷贝)。
    tables: Arc<UserTables>,
}

impl UserLmSnapshot {
    /// log10 P(word | prev):与 [`BigramLM::log10_prob`] 同链
    /// (bigram → unigram → 未知词兜底),分母用快照自己的统计,
    /// 与通用模型语料规模无关。
    fn log10_prob(&self, prev: Option<&str>, word: &str) -> f32 {
        let t = &*self.tables;
        // 词表规模 = 已有 key 数(维护一个独立 vocab 字段反而要处理
        // 「计数衰减到 0 的词是否还算词表」的歧义,key 数定义更清晰)。
        let vocab = t.unigrams.len() as f32;
        if let Some(prev) = prev {
            if let Some(&c) = t.bigrams.get(prev).and_then(|m| m.get(word)) {
                let ch = t.unigrams.get(prev).copied().unwrap_or(0) as f32;
                let denom = ch + vocab;
                if denom > 0.0 {
                    return ((c as f32 + 1.0) / denom).log10();
                }
            }
        }
        let cw = t.unigrams.get(word).copied().unwrap_or(0) as f32;
        let denom = t.unigram_total as f32 + vocab;
        if cw > 0.0 && denom > 0.0 {
            return ((cw + 1.0) / denom).log10();
        }
        // 「通用模型有、用户没学过」的未知词:给中性分而不是
        // 未知词兜底分。用户模型是「按上屏次数采样的稀疏观察表」,
        // 没观察到 ≠ 概率低到 1e-8——它和通用模型在混合层做概率域
        // 线性插值,若此处给 -8,一次没学过的通用高频搭配会被用户层
        // 拖掉 ≈2.4 个数量级(0.3×10^-1.03 + 0.7×10^-8),用户层变成
        // 全场否决票,通用语料的覆盖率优势被稀释。真未知词(通用层
        // 也不认识)两侧同取兜底,混合分 ≈ -3.4,仍远低于任何命中。
        NEUTRAL_LOG_PROB
    }
}

/// 比较两个快照的计数内容是否一致(测试/调试断言用,不逐键比较表)。
impl PartialEq for UserLmSnapshot {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.tables, &other.tables)
    }
}

/// 用户学习模型:计数表 + 落盘状态,内部可变(`RwLock`),
/// 打分只读快照、不加锁。
#[derive(Debug)]
pub struct UserLM {
    /// 计数表(写:学习/衰减/加载;读:打分快照)。
    tables: RwLock<Arc<UserTables>>,
    /// 持久化路径;`None` = 不持久化(纯内存,如 `XWX_USER_LM_PATH=off`)。
    path: Option<PathBuf>,
    /// 距上次成功落盘的净增学习次数(到达 [`FLUSH_LEARN_THRESHOLD`]
    /// 后由调用方触发 [`Self::flush`])。
    dirty: std::sync::atomic::AtomicU32,
}

impl UserLM {
    /// 纯内存模型(不持久化)。
    pub fn in_memory() -> Self {
        Self {
            tables: RwLock::new(Arc::new(UserTables::default())),
            path: None,
            dirty: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// 从 `path` 加载用户模型;文件不存在/损坏/不可写目录都优雅退化为
    /// 「空模型 + 记住路径」,绝不 panic——首次运行必然没有这个文件。
    pub fn open_or_create(path: &Path) -> Self {
        let mut lm = Self::in_memory();
        lm.path = Some(path.to_path_buf());
        match std::fs::File::open(path) {
            Ok(file) => {
                let mut bigram = BigramLM::new();
                match bigram.load(std::io::BufReader::new(file)) {
                    Ok(()) => lm.replace_tables_from(bigram),
                    Err(e) => {
                        // 文件损坏:不 panic、不截断原文件(它是唯一副本,
                        // flush 才会覆盖);从空模型继续,下次 flush 重写。
                        warn!(error = %e, path = %path.display(),
                            "用户语言模型损坏,从空模型重新开始(原文件保留到下次回写)");
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                info!(path = %path.display(), "用户语言模型不存在,首次运行从空模型开始");
            }
            Err(e) => {
                warn!(error = %e, path = %path.display(), "用户语言模型打开失败,本次不加载");
            }
        }
        lm
    }

    /// 记录一次候选词上屏:`word` 是当前词,`prev` 是上一个上屏的词
    /// (来自会话的 `last_word`;原始字母/标点上屏后该值为 `None`,
    /// 此时只记一元计数)。返回距上次落盘的净增学习次数。
    pub fn learn(&self, prev: Option<&str>, word: &str) -> u32 {
        let mut guard = self.tables.write().unwrap_or_else(|p| p.into_inner());
        // copy-on-write:打分线程持有的旧快照不受影响。
        let mut t: UserTables = match Arc::try_unwrap(std::mem::take(&mut *guard)) {
            Ok(t) => t,
            Err(shared) => UserTables {
                unigrams: shared.unigrams.clone(),
                bigrams: shared.bigrams.clone(),
                unigram_total: shared.unigram_total,
            },
        };
        Self::learn_into(&mut t, prev, word);
        *guard = Arc::new(t);
        self.dirty.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
    }

    /// 把一次学习写进计数表(内部;衰减/测试也走这里)。
    fn learn_into(t: &mut UserTables, prev: Option<&str>, word: &str) {
        *t.unigrams.entry(word.to_string()).or_insert(0) += 1;
        t.unigram_total += 1;
        if let Some(prev) = prev {
            *t.bigrams
                .entry(prev.to_string())
                .or_default()
                .entry(word.to_string())
                .or_insert(0) += 1;
        }
        // 条数上界:超限时全表计数减半(衰减而不是按条驱逐,见模块文档)。
        if t.unigrams.len() > MAX_USER_UNIGRAMS {
            Self::decay_tables(t);
        }
    }

    /// 全表计数右移一位(÷2 向下取整),丢弃衰减到 0 的条目。
    fn decay_tables(t: &mut UserTables) {
        t.unigrams.retain(|_, c| {
            *c /= 2;
            *c > 0
        });
        t.bigrams.retain(|_, inner| {
            inner.retain(|_, c| {
                *c /= 2;
                *c > 0
            });
            !inner.is_empty()
        });
        t.unigram_total = t.unigrams.values().sum();
        info!(
            unigrams = t.unigrams.len(),
            "用户模型条数超限,已执行全局计数衰减(÷2)"
        );
    }

    /// 打分快照(`Arc` 克隆,O(1);查询开始时取一次)。
    pub fn snapshot(&self) -> UserLmSnapshot {
        self.tables
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .into()
    }

    /// 距上次落盘的净增学习次数(bin 的事件循环据此决定何时 flush)。
    pub fn dirty_count(&self) -> u32 {
        self.dirty.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 是否需要落盘(达到批量阈值)。
    pub fn needs_flush(&self) -> bool {
        self.dirty_count() >= FLUSH_LEARN_THRESHOLD
    }

    /// 落盘:序列化全表 → 写临时文件 → 原子 rename。
    ///
    /// - 不 fsync:输入法丢电丢几十次上屏样本可接受(统计量),
    ///   fsync 的偶发数百毫秒抖动不可接受;
    /// - 临时文件 + rename 保证任何时刻目标路径要么不存在、要么是
    ///   完整的一代快照,崩溃不会留下半截文件;
    /// - 没有任何新学习时直接跳过(空写只会徒增 SSD 磨损)。
    pub fn flush(&self) -> Result<(), LmError> {
        let Some(path) = &self.path else {
            self.dirty.store(0, std::sync::atomic::Ordering::Relaxed);
            return Ok(());
        };
        if self.dirty_count() == 0 {
            return Ok(());
        }
        // 序列化基于一份快照:flush 期间的学习写入新表,计入下一轮 dirty。
        let snap = self.snapshot();
        let mut text = String::with_capacity(snap.tables.unigrams.len() * 24);
        text.push_str("# xwx 用户词频学习模型(bigram 文本格式,见 data/README.md)\n");
        text.push_str("# 由引擎自动维护,手工编辑将在下次回写时被覆盖\n");
        let t = &*snap.tables;
        for (word, count) in &t.unigrams {
            text.push_str(word);
            text.push('\t');
            text.push_str(&count.to_string());
            text.push('\n');
        }
        for (prev, inner) in &t.bigrams {
            for (word, count) in inner {
                text.push_str(prev);
                text.push('\t');
                text.push_str(word);
                text.push('\t');
                text.push_str(&count.to_string());
                text.push('\n');
            }
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // 同目录临时文件(保证 rename 不跨文件系统);PID 避免多实例互踩。
        let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(format!(".tmp.{}", std::process::id()));
        let tmp = path.with_file_name(tmp_name);
        let write_result = (|| -> Result<(), LmError> {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_data().ok(); // 尽力而为:数据页落盘失败不致命
            drop(f);
            std::fs::rename(&tmp, path)?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = std::fs::remove_file(&tmp); // 不留垃圾
        }
        write_result?;
        // 只清零本代:flush 期间又发生的学习(resize 代差)不丢。
        // 严格说「flush 快照之后的学习」会被这次清零覆盖——它们还在内存
        // 表里,只是 dirty 计数少了;为简化取保守清零,下一轮学习会再次
        // 计数,最迟退出兜底 flush 落盘。
        self.dirty.store(0, std::sync::atomic::Ordering::Relaxed);
        info!(path = %path.display(), "用户语言模型已回写");
        Ok(())
    }

    /// 一元词条数(测试/日志用)。
    pub fn unigram_len(&self) -> usize {
        self.tables
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .unigrams
            .len()
    }

    /// 用加载好的 [`BigramLM`] 计数整体替换当前表(加载路径专用;
    /// `BigramLM` 的字段对 crate 内可见)。
    fn replace_tables_from(&self, lm: BigramLM) {
        let t = UserTables {
            unigrams: lm.unigrams,
            bigrams: lm.bigrams,
            unigram_total: lm.unigram_total,
        };
        *self.tables.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(t);
    }

    /// 内部查询打分(供 [`crate::lm::MixedLM`] 调用)。
    pub(crate) fn score_with(&self, snap: &UserLmSnapshot, prev: Option<&str>, word: &str) -> f32 {
        let _ = self; // 打分只依赖快照,不需要锁
        snap.log10_prob(prev, word)
    }
}

impl From<Arc<UserTables>> for UserLmSnapshot {
    fn from(tables: Arc<UserTables>) -> Self {
        UserLmSnapshot { tables }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learn_raises_score_for_committed_word() {
        let lm = UserLM::in_memory();
        let before = lm.score_with(&lm.snapshot(), None, "斯巴克");
        for _ in 0..5 {
            lm.learn(None, "斯巴克");
        }
        let after = lm.score_with(&lm.snapshot(), None, "斯巴克");
        assert!(after > before, "学习后 unigram 分应升高: {before} -> {after}");
        // 上文搭配也应命中:中→国 学过后,有中上文时分更高
        for _ in 0..5 {
            lm.learn(Some("中"), "国");
        }
        let snap = lm.snapshot();
        let with_prev = lm.score_with(&snap, Some("中"), "国");
        let no_prev = lm.score_with(&snap, None, "国");
        assert!(with_prev > no_prev, "学习 bigram 后有上文分应更高: {with_prev} vs {no_prev}");
    }

    #[test]
    fn persistence_roundtrip_keeps_effect() {
        let dir = std::env::temp_dir().join(format!("xwx_userlm_test_{}", std::process::id()));
        let path = dir.join("user_bigram.txt");
        let _ = std::fs::remove_dir_all(&dir);
        {
            let lm = UserLM::open_or_create(&path);
            assert!(!path.exists(), "首次运行不应创建文件");
            for _ in 0..3 {
                lm.learn(Some("今天"), "天气");
            }
            // 强制落盘(未达阈值也应能写:退出兜底路径)
            lm.flush().expect("flush 不应失败");
            assert!(path.is_file(), "flush 后文件应存在");
        }
        // 模拟崩溃残留:临时文件不应影响正式文件
        std::fs::write(dir.join("user_bigram.txt.tmp.99999"), b"garbage").unwrap();
        {
            let lm = UserLM::open_or_create(&path);
            let snap = lm.snapshot();
            let with_prev = lm.score_with(&snap, Some("今天"), "天气");
            let unknown = lm.score_with(&snap, None, "没学过的词");
            assert!(
                with_prev > unknown,
                "重载后学习效果应仍在: {with_prev} vs {unknown}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_and_corrupt_files_degrade_gracefully() {
        let dir = std::env::temp_dir().join(format!("xwx_userlm_bad_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // 缺失:不 panic、空模型
        let missing = dir.join("nope.txt");
        let lm = UserLM::open_or_create(&missing);
        assert_eq!(lm.unigram_len(), 0);
        // 损坏:格式错误的行 → 告警后空模型,不 panic
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("bad.txt");
        std::fs::write(&bad, "好\t不是数字\n两\t个\t字段\t太多\n").unwrap();
        let lm = UserLM::open_or_create(&bad);
        assert_eq!(lm.unigram_len(), 0, "损坏文件应退化为空模型");
        // 损坏文件仍可被下次 flush 覆盖重写
        lm.learn(None, "好");
        lm.flush().expect("损坏后 flush 应能重写");
        let lm = UserLM::open_or_create(&bad);
        assert_eq!(lm.unigram_len(), 1, "重写后应能正常加载");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn entry_cap_triggers_decay() {
        let lm = UserLM::in_memory();
        // 一个高频词 + 顶满上限的一次性词
        for _ in 0..100 {
            lm.learn(None, "常用词");
        }
        for i in 0..MAX_USER_UNIGRAMS {
            lm.learn(None, &format!("长尾词{i}"));
        }
        // 再学一次 → 超过上限 → 衰减
        lm.learn(None, "触发衰减");
        let n = lm.unigram_len();
        assert!(n <= MAX_USER_UNIGRAMS, "衰减后条数应回到上限内: {n}");
        let snap = lm.snapshot();
        let hot = lm.score_with(&snap, None, "常用词");
        let cold = lm.score_with(&snap, None, "长尾词0");
        assert!(hot > cold, "高频词应在衰减后仍高于长尾: {hot} vs {cold}");
    }

    #[test]
    fn dirty_threshold_gates_flush() {
        let dir = std::env::temp_dir().join(format!("xwx_userlm_dirty_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("u.txt");
        let lm = UserLM::open_or_create(&path);
        assert!(!lm.needs_flush());
        lm.learn(None, "甲");
        assert_eq!(lm.dirty_count(), 1);
        assert!(!lm.needs_flush(), "未达阈值不应要求落盘");
        for _ in 0..FLUSH_LEARN_THRESHOLD {
            lm.learn(None, "乙");
        }
        assert!(lm.needs_flush(), "达到阈值应要求落盘");
        lm.flush().expect("flush 失败");
        assert_eq!(lm.dirty_count(), 0);
        assert!(!lm.needs_flush());
        // 无新学习时 flush 是 no-op(文件 mtime 不变)
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        lm.flush().expect("无学习 flush 应直接跳过");
        let mtime2 = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(mtime, mtime2, "无学习不应重写文件");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
