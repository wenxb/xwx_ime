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
//!   用户层命中时按 [`crate::lm::USER_COUNT_GAIN_R`] 给计数增益。
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

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use tracing::info;

mod file;

use file::{block_key, UserTables};

use crate::lm::MixedLM;

/// 用户模型默认文件名(落在 `$XDG_DATA_HOME/xwx/` 下)。
pub const USER_LM_FILE_NAME: &str = "user_bigram.txt";

/// **开发期**用户模型文件名(与上面同目录,后缀 `.dev`)。
///
/// `debug_assertions` 打开的构建默认用它(见 `src/main.rs`),这样
/// `just dev` 起的调试引擎**不会把开发时打的字混进真实用户模型**——
/// 这个文件里装着三段状态(一元/二元计数、造词条目、Ctrl+Delete 屏蔽表),
/// 混进去会污染真实使用时的排序,而且很难清干净。
///
/// 与真实文件相邻(`$XDG_DATA_HOME/xwx/user_bigram.dev.txt`)是为了好对比、
/// 好手工删除;`XWX_USER_LM_PATH` 仍然优先于本默认值(集成测试靠它隔离)。
pub const USER_LM_DEV_FILE_NAME: &str = "user_bigram.dev.txt";
/// 距上次落盘累计多少次学习上屏后,值得再写一次盘。
///
/// 落盘代价是「序列化全表 + write + rename」(数千条时约 1MB、毫秒级),
/// 不 fsync、不在上屏路径上同步调用;64 次的阈值让正常使用大约几分钟
/// 批量回写一次,崩溃丢失上限就是这 64 次上屏——可接受(词频学习是
/// 统计量,丢几十次样本只让「越用越顺手」晚来一点)。
pub const FLUSH_LEARN_THRESHOLD: u32 = 64;
/// 用户模型一元词条数硬上限(超限触发全局衰减,见模块文档)。
pub const MAX_USER_UNIGRAMS: usize = 20_000;
/// 造词条目数硬上限(与一元词条同一衰减策略,见模块文档)。
pub const MAX_USER_ENTRIES: usize = 10_000;



// 用户层的打分口径,经历过一次「学习反而降权」事故(2026-10-02),
// 结论是:**这一层只提供计数,不提供概率**。
//
// 事故形态:旧实现把用户层当概率模型,按**用户自己的语料规模**归一化,
// 再与通用层做概率域插值。真实 `~/.local/share/xwx/user_bigram.txt`
// (Σc+vocab ≈ 175)下,学过 3 次的「可以」算出 log10(4/175) = -1.64,
// 比「没学过」的中性分 -1.0 还低——学过的词反而被压到没学过的词后面
// (简拼 `ky` 里「可以」第 11 名,跨串的「跨越」白拿 -1.15)。
// 更糟的是这个量纲随用户打字量漂移:实测同一句「学 1 次目标词」的加分
// 从 +11.45(语料 1 条)掉到 +7.45(语料 10001 条),重度用户的个人化
// 比新用户弱 4 个数量级。
//
// 现在的口径:`UserLmSnapshot::counts` 只回**计数**(一元/二元),
// 混合层按 [`crate::lm::USER_COUNT_GAIN_R`] 把它换算成加法项
// (`log10(1 + c·R)`)。计数与语料规模无关,0 次就是 +0(逐位回退通用层),
// 学习因此**永不降权**,也不可能出现「用户用得越多、个人化越弱」。


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
    /// 用户层对该词的**计数证据**:`(一元计数, 二元计数)`。
    ///
    /// 0 表示没有证据——混合层据此逐位回退通用层(见文件头说明)。
    /// **只给计数、不给概率**:用户层是「稀疏观察表」,它的概率分母是
    /// 用户自己的语料规模,与通用层量纲不可比(「学习反而降权」事故的
    /// 根因);计数是纯增量证据,换算成加分的比例由
    /// [`crate::lm::USER_COUNT_GAIN_R`] 单点决定,且与语料规模无关。
    fn counts(&self, prev: Option<&str>, word: &str) -> (u32, u32) {
        let t = &*self.tables;
        let to_u32 = |c: u64| c.min(u32::MAX as u64) as u32;
        let uni = to_u32(t.unigrams.get(word).copied().unwrap_or(0));
        let bi = to_u32(
            prev.and_then(|p| t.bigrams.get(p).and_then(|m| m.get(word)).copied())
                .unwrap_or(0),
        );
        (uni, bi)
    }

    /// 按拼音 key(音节空格分隔,与词典 key 同格式)查造词条目。
    ///
    /// 引擎的格构建把它当作词库之外的第二召回源合并(见
    /// `lib.rs` 的 `collect_chain_edges`);返回的词列表是「用户亲手
    /// 逐字选出来过的短语」,词库里可能根本没有。查询零锁、零拷贝
    /// (快照是 `Arc` 共享的整表)。
    pub fn lookup_entries(&self, pinyin_key: &str) -> &[String] {
        self.tables
            .entries
            .get(pinyin_key)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// 该候选是否被用户屏蔽(Ctrl+Delete 删过)。
    ///
    /// 排序前的统一过滤点(`lib.rs` 候选收口)在**每次查询**对每个候选
    /// 调用一次,因此它必须是 O(1) 哈希查找、不加锁(快照 `Arc` 共享)。
    /// 拼接键一次分配、`contains(&str)` 零拷贝(见 `blocklist` 字段注释)。
    pub fn is_blocked(&self, pinyin_key: &str, word: &str) -> bool {
        let mut k = String::with_capacity(pinyin_key.len() + 1 + word.len());
        k.push_str(pinyin_key);
        k.push('\u{1}');
        k.push_str(word);
        self.tables.blocklist.contains(&k)
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



    /// 记录一次候选词上屏:`word` 是当前词,`prev` 是上一个上屏的词
    /// (来自会话的 `last_word`;原始字母/标点上屏后该值为 `None`,
    /// 此时只记一元计数)。返回距上次落盘的净增学习次数。
    pub fn learn(&self, prev: Option<&str>, word: &str) -> u32 {
        self.mutate_tables(|t| Self::learn_into(t, prev, word))
    }

    /// 写路径的唯一入口:在**写锁之内**完成「取得可写表 → 修改 → 发布」。
    ///
    /// 旧实现(`clone_tables_for_write` + `commit_tables`)先用 `mem::take`
    /// 把共享表换成**空表**、随即释放锁,再改、再提交,于是:
    /// 1. 窗口里其它线程(查询打分、学习、flush)读到的是**空模型**——
    ///    屏蔽项、计数、造词条目全部短暂消失;
    /// 2. 两个写者各拿一份拷贝,后提交的覆盖先提交的(丢更新)。
    ///
    /// (codex 审查 D1,P0。)
    ///
    /// 现在:独占时 `Arc::make_mut` **原地**改(零拷贝),只有打分线程还持有
    /// 旧快照时才深拷贝一份;占位空表从不发布。闭包内不得回调 `self`
    /// (会自己等自己的写锁)。
    fn mutate_tables(&self, f: impl FnOnce(&mut UserTables)) -> u32 {
        {
            let mut guard = self.tables.write().unwrap_or_else(|p| p.into_inner());
            let t = Arc::make_mut(&mut *guard);
            f(t);
        }
        self.dirty.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
    }

    /// 把一次学习写进计数表(内部;衰减/测试也走这里)。
    fn learn_into(t: &mut UserTables, prev: Option<&str>, word: &str) {
        *t.unigrams.entry(word.to_string()).or_insert(0) += 1;
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

    /// 记一条造词条目:`(词, 拼音 key)` 可被后续格构建查到。
    ///
    /// 由会话层的造词缓冲在「连续候选上屏段 ≥ 2 字」时调用(见
    /// `session.rs` 的 `coin_buffer`;单字不造词——它要么已在词库,
    /// 要么靠 unigram 计数调频就够了,造出词条也不会改变它的候选
    /// 地位)。幂等:同 `(word, key)` 重复记不会产生重复条目(用户
    /// 反复上屏同一自造词只留一份,排序增益靠 unigram 计数表达,
    /// 不靠词条重复)。
    ///
    /// 返回距上次落盘的净增学习次数(与 [`Self::learn`] 共用同一
    /// dirty 计数与回写调度——造词与调频是同一份用户数据的两个面)。
    pub fn add_entry(&self, word: &str, pinyin_key: &str) -> u32 {
        self.mutate_tables(|t| {
            let words = t.entries.entry(pinyin_key.to_string()).or_default();
            if !words.iter().any(|w| w == word) {
                words.push(word.to_string());
            }
            if t.entries.len() > MAX_USER_ENTRIES {
                Self::decay_entries(t);
            }
        })
    }

    /// 删候选(editor 的 Ctrl+Delete,gear-audit 第 6 节):
    /// 「屏蔽 + 连根拔」一体完成。
    ///
    /// 对一条候选 `(word, pinyin_key)` 做三件事:
    /// 1. **屏蔽**:插入 blocklist——无论它是词库词还是用户造词,此后
    ///    该候选在候选收口处被滤掉(`lib.rs` 的统一过滤点),重查同
    ///    拼音不再出现。词库只读(mmap),这是词库词唯一可行的删除形态;
    /// 2. **能删则删**:若它同时是用户造词条目(`entries` 里有),把那条
    ///    词条也删掉——留着词条会让它继续占 `MAX_USER_ENTRIES` 名额;
    /// 3. **计数一起清**(2026-10-02 起):删掉该词的一元计数、所有
    ///    「某词 → 它」的二元计数、以及「它 → 某词」的上下文计数。
    ///
    /// 第 3 条为什么必须有(旧实现刻意**不**动计数,当时的理由是
    /// 「删候选 ≠ 抹掉频次」):屏蔽键是 `(拼音, 词)`,而计数是按**词**
    /// 记的——只屏蔽的话,同一个词在**别的拼音路径**上照样带着计数被
    /// 加分(多音字串味),用户看到「删了它,换个打法又冒出来还更靠前」。
    /// 更直接的是:计数正是排名的燃料,燃料不清就等于没删。
    /// 「误删后重新上屏会丢已积累的调频」这点代价明确接受——用户按下
    /// Ctrl+Delete 就是明确表态,而误删的恢复成本只是再选几次。
    ///
    /// 幂等:重复屏蔽同一条不产生变化。返回 dirty(与 learn/add_entry
    /// 共用同一回写调度,屏蔽表随同一份文件落盘)。
    pub fn block_candidate(&self, word: &str, pinyin_key: &str) -> u32 {
        self.mutate_tables(|t| {
            t.blocklist.insert(block_key(pinyin_key, word));
            if let Some(words) = t.entries.get_mut(pinyin_key) {
                words.retain(|w| w != word);
                if words.is_empty() {
                    t.entries.remove(pinyin_key);
                }
            }
            // 计数连根拔:一元 + 所有指向它的二元 + 它作为上文的二元。
            t.unigrams.remove(word);
            t.bigrams.retain(|_, inner| {
                inner.remove(word);
                !inner.is_empty()
            });
            t.bigrams.remove(word);
        })
    }

    /// 屏蔽表条数(测试/日志用)。
    pub fn blocklist_len(&self) -> usize {
        self.tables
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .blocklist
            .len()
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
        info!(
            unigrams = t.unigrams.len(),
            "用户模型条数超限,已执行全局计数衰减(÷2)"
        );
    }

    /// 造词条目超限时的淘汰:按「词的上屏次数」保高频(与 unigram
    /// 衰减的「记忆随使用淡化」同一取向——造词条目若对应的 unigram
    /// 计数已衰减到 0,说明用户早就没再用它,先丢它)。
    ///
    /// 与 [`Self::decay_tables`] 的分工:计数表可以整体减半(值是
    /// 统计量),词条是有无问题不能减半,只能按条驱逐;判据借用
    /// unigram 计数,两张表因此共享同一条「用得少的先消失」曲线。
    fn decay_entries(t: &mut UserTables) {
        // 每个词条的留存分 = 其 unigram 计数(没学过/已衰减到 0 的
        // 词条得 0 分,最先被淘汰)。
        let mut scored: Vec<(String, String, u64)> = Vec::new();
        for (key, words) in &t.entries {
            for w in words {
                let c = t.unigrams.get(w).copied().unwrap_or(0);
                scored.push((key.clone(), w.clone(), c));
            }
        }
        // 按分升序排,丢前一半(同分按 (key, word) 字典序保证确定性)。
        scored.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)).then_with(|| a.1.cmp(&b.1)));
        let drop_n = scored.len() / 2;
        let drop: std::collections::HashSet<(String, String)> =
            scored.into_iter().take(drop_n).map(|(k, w, _)| (k, w)).collect();
        t.entries.retain(|key, words| {
            words.retain(|w| !drop.contains(&(key.clone(), w.clone())));
            !words.is_empty()
        });
        info!(
            entries = t.entries.len(),
            "造词条目超限,已按上屏频次淘汰低频一半"
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


    /// 一元词条数(测试/日志用)。
    pub fn unigram_len(&self) -> usize {
        self.tables
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .unigrams
            .len()
    }

    /// 造词条目数(拼音 key 的条数;测试/日志用)。
    pub fn entry_len(&self) -> usize {
        self.tables
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .entries
            .len()
    }

    /// 内部查询计数(供 [`crate::lm::MixedLM`] 调用):`(一元, 二元)`,
    /// 0 = 无证据(混合层据此逐位回退通用层)。
    pub(crate) fn counts_with(
        &self,
        snap: &UserLmSnapshot,
        prev: Option<&str>,
        word: &str,
    ) -> (u32, u32) {
        let _ = self; // 打分只依赖快照,不需要锁
        snap.counts(prev, word)
    }
}

/// 从引擎聚合的 LM 里取用户层造词条目快照(若有用户层)。
///
/// 引擎的格构建(`lib.rs` 的 `collect_chain_edges`)借此把造词条目
/// 作为词库之外的第二召回源;引擎没挂用户层(测试里 `new(None)`
/// 的裸引擎、或 probe 对拍进程)时返回 `None`,召回源与引入造词
/// 之前完全一致——这是「空用户词典下候选列表逐字节不变」的结构性
/// 保证:无快照 ⇒ 一次都不查。
pub(crate) fn user_entries_snapshot(lm: &crate::lm::LmStore) -> Option<UserLmSnapshot> {
    match lm {
        crate::lm::LmStore::Mixed(m) => Some(m.user().snapshot()),
        _ => None,
    }
}

impl MixedLM {
    /// 用户层造词条目的打分快照(查询开始时取一次,沿本次查询传递)。
    pub fn user_entries_snapshot(&self) -> UserLmSnapshot {
        self.user().snapshot()
    }
}

impl From<Arc<UserTables>> for UserLmSnapshot {
    fn from(tables: Arc<UserTables>) -> Self {
        UserLmSnapshot { tables }
    }
}

#[cfg(test)]
mod tests;
