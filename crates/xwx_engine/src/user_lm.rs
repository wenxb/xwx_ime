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

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use tracing::{info, warn};

use crate::lm::{BigramLM, LmError, MixedLM};

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
/// 造词条目数硬上限(与一元词条同一衰减策略,见模块文档)。
pub const MAX_USER_ENTRIES: usize = 10_000;
/// 落盘文件里造词节的分节标记行(加载时据此切换解析;旧文件没有
/// 这一行,整文件按 bigram 格式解析 → 词条表为空,向后兼容)。
const ENTRIES_SECTION_MARK: &str = "# xwx-user-entries";
/// 落盘文件里屏蔽表节的分节标记行(同样向后兼容:旧文件没有这一行,
/// 屏蔽表为空;`#` 前缀让它落在旧版解析器的注释行分支里——旧版本
/// 读新文件也只会把它当注释跳过,双向兼容)。
const BLOCKLIST_SECTION_MARK: &str = "# xwx-user-blocklist";

/// 屏蔽表的存储键:`"拼音 key\u{1}词"`(U+0001 分隔,拼音是 ASCII、
/// 词是 UTF-8 文本都不会含它)。集中一处构造,保证写入/查询/落盘
/// 三处的键形一致。
fn block_key(pinyin_key: &str, word: &str) -> String {
    let mut k = String::with_capacity(pinyin_key.len() + 1 + word.len());
    k.push_str(pinyin_key);
    k.push('\u{1}');
    k.push_str(word);
    k
}

/// 落盘文件的分节(加载时按标记行切换,见 [`UserLM::load_tables`])。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    /// bigram 计数区(旧格式本体,默认)。
    Bigram,
    /// 造词区(`# xwx-user-entries` 之后)。
    Entries,
    /// 屏蔽表区(`# xwx-user-blocklist` 之后)。
    Blocklist,
}

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

/// 计数表本体(不跨线程共享;共享由 [`UserLM`] 的 `RwLock` 承担)。
#[derive(Debug, Default)]
struct UserTables {
    /// 一元计数:词 -> 上屏次数。
    unigrams: HashMap<String, u64>,
    /// 二元计数:前词 -> (后词 -> 上屏次数)。
    bigrams: HashMap<String, HashMap<String, u64>>,
    /// 造词条目:拼音 key(音节空格分隔,与词典 key 同格式)-> 词列表。
    ///
    /// 与 unigrams/bigrams 的分工:计数表管「学过的词该排多靠前」
    /// (LM 打分),entries 管「词库里根本没有的短语能被查到」(召回)。
    /// 造出的词同时会进 unigrams(上屏即学),因此它既有候选召回、
    /// 又有用户层打分增益,两条通道一体衰减。
    entries: HashMap<String, Vec<String>>,
    /// 屏蔽表:被用户 Ctrl+Delete 删掉的候选,存 `"拼音 key\u{1}词"` 的
    /// 拼接字符串(Ctrl+Delete 删候选)。
    ///
    /// 与 entries 的对偶:entries 是「词库没有、用户要它出现」,
    /// blocklist 是「词库有、用户不想再见到」。键带拼音:同一个词
    /// (如多音字「长」zhang/chang)只屏蔽用户当时打的那一路读音,
    /// 不误伤另一路。排序前的统一过滤在 `lib.rs` 的候选收口处,
    /// 查询零锁(快照 `Arc` 共享)。
    ///
    /// 用拼接串而非 `(String, String)` 元组:热路径 `is_blocked` 每次
    /// 查询对每个候选调一次,拼接键只需一次 `String` 分配就能
    /// `HashSet<String>::contains(&str)` 零拷贝哈希查找;元组键要么
    /// 两次分配、要么退化成 O(n) 线性扫描(实测 500 条时把长串从
    /// 1.5ms 拉到 3.0ms)。分隔用 U+0001:拼音是 ASCII、词是 UTF-8
    /// 文本,都不会含它。
    blocklist: HashSet<String>,
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

    /// 从 `path` 加载用户模型;文件不存在/损坏/不可写目录都优雅退化为
    /// 「空模型 + 记住路径」,绝不 panic——首次运行必然没有这个文件。
    ///
    /// 格式向后兼容:文件由 bigram 计数区(旧格式本体)与可选的造词区
    /// 两段组成,造词区以 [`ENTRIES_SECTION_MARK`] 行开头。旧版本写出的
    /// 文件没有这一行,整文件按 bigram 解析、词条表为空——与造词功能
    /// 引入前的语义一致(那时没有用户词条)。
    pub fn open_or_create(path: &Path) -> Self {
        let mut lm = Self::in_memory();
        lm.path = Some(path.to_path_buf());
        match std::fs::File::open(path) {
            Ok(file) => match Self::load_tables(std::io::BufReader::new(file)) {
                Ok(t) => {
                    *lm.tables.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(t);
                }
                Err(e) => {
                    // 文件损坏:不 panic、不截断原文件(它是唯一副本,
                    // flush 才会覆盖);从空模型继续,下次 flush 重写。
                    warn!(error = %e, path = %path.display(),
                        "用户语言模型损坏,从空模型重新开始(原文件保留到下次回写)");
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                info!(path = %path.display(), "用户语言模型不存在,首次运行从空模型开始");
            }
            Err(e) => {
                warn!(error = %e, path = %path.display(), "用户语言模型打开失败,本次不加载");
            }
        }
        lm
    }

    /// 从文本流加载计数表 + 造词条目(`open_or_create` 的核心,可测)。
    ///
    /// 解析复用 [`BigramLM::load`] 处理计数区(bigram 格式细节只有它
    /// 知道);造词区在分节标记之后逐行解析(`词<TAB>拼音 key`)。
    /// 词条表大小不参与 bigram 的 vocab 统计(那是 LM 平滑用的,
    /// 造词区是召回用的,两者职责不同)。
    fn load_tables(reader: impl std::io::BufRead) -> Result<UserTables, LmError> {
        let mut bigram_text = String::new();
        let mut entries: HashMap<String, Vec<String>> = HashMap::new();
        let mut blocklist: HashSet<String> = HashSet::new();
        // 分节解析:bigram 计数区(默认)→ 造词区 / 屏蔽表区(标记行切换)。
        let mut section = Section::Bigram;
        for (i, line) in reader.lines().enumerate() {
            let line = line?;
            let t = line.trim();
            if t == ENTRIES_SECTION_MARK {
                section = Section::Entries;
                continue;
            }
            if t == BLOCKLIST_SECTION_MARK {
                section = Section::Blocklist;
                continue;
            }
            match section {
                Section::Entries => {
                    if t.is_empty() || t.starts_with('#') {
                        continue;
                    }
                    let fields: Vec<&str> = t.split('\t').collect();
                    let [word, key] = fields.as_slice() else {
                        return Err(LmError::BadLine {
                            line: (i + 1) as u64,
                            detail: format!("造词区期望「词<TAB>拼音」两字段,实际: {t:?}"),
                        });
                    };
                    entries.entry((*key).to_string()).or_default().push((*word).to_string());
                }
                Section::Blocklist => {
                    if t.is_empty() || t.starts_with('#') {
                        continue;
                    }
                    let fields: Vec<&str> = t.split('\t').collect();
                    let [word, key] = fields.as_slice() else {
                        return Err(LmError::BadLine {
                            line: (i + 1) as u64,
                            detail: format!("屏蔽表区期望「词<TAB>拼音」两字段,实际: {t:?}"),
                        });
                    };
                    blocklist.insert(block_key(key, word));
                }
                Section::Bigram => {
                    bigram_text.push_str(t);
                    bigram_text.push('\n');
                }
            }
        }
        let mut bigram = BigramLM::new();
        bigram.load(bigram_text.as_bytes())?;
        Ok(UserTables {
            unigrams: bigram.unigrams,
            bigrams: bigram.bigrams,
            entries,
            blocklist,
        })
    }

    /// 记录一次候选词上屏:`word` 是当前词,`prev` 是上一个上屏的词
    /// (来自会话的 `last_word`;原始字母/标点上屏后该值为 `None`,
    /// 此时只记一元计数)。返回距上次落盘的净增学习次数。
    pub fn learn(&self, prev: Option<&str>, word: &str) -> u32 {
        let mut t = self.clone_tables_for_write();
        Self::learn_into(&mut t, prev, word);
        self.commit_tables(t)
    }

    /// copy-on-write 取出一份可写的计数表(写路径统一入口):
    /// 独占时直接接管,共享时深拷贝——打分线程持有的旧快照不受影响。
    fn clone_tables_for_write(&self) -> UserTables {
        let mut guard = self.tables.write().unwrap_or_else(|p| p.into_inner());
        match Arc::try_unwrap(std::mem::take(&mut *guard)) {
            Ok(t) => t,
            Err(shared) => UserTables {
                unigrams: shared.unigrams.clone(),
                bigrams: shared.bigrams.clone(),
                entries: shared.entries.clone(),
                blocklist: shared.blocklist.clone(),
            },
        }
        // 注意:guard 在这里已释放给调用方继续 mutate 之前必须重新
        // 拿锁的场景不存在——commit_tables 会重新拿锁写回。
    }

    /// 把改完的计数表写回并累加 dirty(与 [`Self::learn`] 原路径一致)。
    fn commit_tables(&self, t: UserTables) -> u32 {
        let mut guard = self.tables.write().unwrap_or_else(|p| p.into_inner());
        *guard = Arc::new(t);
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
        let mut t = self.clone_tables_for_write();
        let words = t.entries.entry(pinyin_key.to_string()).or_default();
        if !words.iter().any(|w| w == word) {
            words.push(word.to_string());
        }
        if t.entries.len() > MAX_USER_ENTRIES {
            Self::decay_entries(&mut t);
        }
        self.commit_tables(t)
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
        let mut t = self.clone_tables_for_write();
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
        self.commit_tables(t)
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
        // 造词区:分节标记之后的「词<TAB>拼音 key」行。旧版本读到这里
        // 之前的文件内容即可正常加载(bigram 区在上),旧文件没有本节
        // 则词条表为空——双向兼容。排序写出保证同一表内容字节级稳定
        // (测试与 diff 友好;HashMap 遍历序不定)。
        if !t.entries.is_empty() {
            text.push_str(ENTRIES_SECTION_MARK);
            text.push('\n');
            let mut rows: Vec<(&str, &str)> = t
                .entries
                .iter()
                .flat_map(|(key, words)| words.iter().map(move |w| (w.as_str(), key.as_str())))
                .collect();
            rows.sort_unstable();
            for (word, key) in rows {
                text.push_str(word);
                text.push('\t');
                text.push_str(key);
                text.push('\n');
            }
        }
        // 屏蔽表区:与造词区同格式、同兼容策略(标记行 `#` 前缀,
        // 旧版解析器把它当注释跳过;旧文件没有本节则屏蔽表为空)。
        // 存储键是拼接串「key\u{1}词」,写出时拆回「词<TAB>key」两字段。
        if !t.blocklist.is_empty() {
            text.push_str(BLOCKLIST_SECTION_MARK);
            text.push('\n');
            let mut rows: Vec<(&str, &str)> = t
                .blocklist
                .iter()
                .map(|k| {
                    let (key, word) = k
                        .split_once('\u{1}')
                        .expect("屏蔽键由 block_key 构造,必含分隔符");
                    (word, key)
                })
                .collect();
            rows.sort_unstable();
            for (word, key) in rows {
                text.push_str(word);
                text.push('\t');
                text.push_str(key);
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
mod tests {
    use super::*;

    #[test]
    fn learn_produces_counts_for_committed_word() {
        // 计数口径(2026-10-02 起):这一层只回计数,不回概率。
        let lm = UserLM::in_memory();
        assert_eq!(
            lm.counts_with(&lm.snapshot(), None, "斯巴克"),
            (0, 0),
            "没学过就没有任何计数"
        );
        for _ in 0..5 {
            lm.learn(None, "斯巴克");
        }
        assert_eq!(
            lm.counts_with(&lm.snapshot(), None, "斯巴克"),
            (5, 0),
            "5 次上屏 ⇒ 一元计数 5"
        );
        // 二元单独记:中→国 学 5 次,一元也各 +5
        for _ in 0..5 {
            lm.learn(Some("中"), "国");
        }
        let snap = lm.snapshot();
        assert_eq!(
            lm.counts_with(&snap, Some("中"), "国"),
            (5, 5),
            "有上文「中」时:一元 5 + 二元(中→国) 5"
        );
        assert_eq!(
            lm.counts_with(&snap, None, "国"),
            (5, 0),
            "没有上文时只有一元计数,二元不给分"
        );
        assert_eq!(
            lm.counts_with(&snap, Some("今天"), "国"),
            (5, 0),
            "上文不匹配时不产生二元计数"
        );
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
            let with_prev = lm.counts_with(&snap, Some("今天"), "天气");
            let unknown = lm.counts_with(&snap, None, "没学过的词");
            assert!(
                with_prev.0 > 0 && with_prev.1 > 0 && unknown == (0, 0),
                "重载后学习效果应仍在: {with_prev:?} vs {unknown:?}"
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
        let (hot, _) = lm.counts_with(&snap, None, "常用词");
        let (cold, _) = lm.counts_with(&snap, None, "长尾词0");
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

    #[test]
    fn legacy_file_without_entries_section_loads_as_empty_entries() {
        // 向后兼容(验收判据 A6):造词功能引入**之前**的旧格式文件
        // (只有 bigram 计数行,没有造词分节标记)必须仍能正常加载,
        // 且词条表为空——与「那时还没有造词」的语义一致。
        let dir = std::env::temp_dir().join(format!("xwx_userlm_legacy_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.txt");
        // 手写一份旧格式(分节标记绝不出现):与引入造词前的 flush
        // 产物逐字节同构。
        std::fs::write(&path, "# xwx 用户词频学习模型\n你\t3\n你\t好\t2\n").unwrap();
        let lm = UserLM::open_or_create(&path);
        // bigram 行(你,好)的「好」不单独计 unigram:BigramLM 的
        // unigrams 只收「词<TAB>次数」行,去重后只有「你」一个。
        assert_eq!(lm.unigram_len(), 1, "旧文件的计数区应正常加载");
        assert_eq!(lm.entry_len(), 0, "旧文件没有造词区,词条表应为空");
        assert!(lm.snapshot().lookup_entries("ni hao").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn entries_section_survives_roundtrip() {
        // 新格式:bigram 区 + 造词区同一份文件,写出去再读回来两段都在。
        let dir = std::env::temp_dir().join(format!("xwx_userlm_rt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("rt.txt");
        {
            let lm = UserLM::open_or_create(&path);
            lm.learn(Some("今天"), "天气");
            lm.add_entry("张三丰", "zhang san feng");
            lm.add_entry("张叁丰", "zhang san feng"); // 同 key 不同词
            lm.flush().expect("flush 不应失败");
        }
        {
            let lm = UserLM::open_or_create(&path);
            // learn(Some("今天"), "天气") 记 unigram(天气) + bigram(今天,天气):
            // unigram key 去重后只有「天气」一个。
            assert_eq!(lm.unigram_len(), 1, "计数区应读回");
            let snap = lm.snapshot();
            let got = snap.lookup_entries("zhang san feng");
            assert_eq!(got.len(), 2, "同一 key 的两个词都应读回");
            assert!(got.contains(&"张三丰".to_string()) && got.contains(&"张叁丰".to_string()),
                "两个词都应在: {got:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_entry_is_idempotent_per_word_key() {
        // 同一 (word, key) 重复记不重复:用户反复上屏同一自造词
        // 只留一份(排序增益由 unigram 计数表达,不靠词条重复)。
        let lm = UserLM::in_memory();
        for _ in 0..3 {
            lm.add_entry("张三丰", "zhang san feng");
        }
        assert_eq!(lm.entry_len(), 1);
        assert_eq!(lm.snapshot().lookup_entries("zhang san feng").len(), 1);
    }

    // —— 屏蔽表(Ctrl+Delete 删候选)——

    #[test]
    fn block_candidate_marks_blocked_and_removes_user_entry() {
        // 核心判据:屏蔽一条候选后,该 (key, word) 被标记;若它是用户
        // 造词条目,词条同时被删(连根拔);若是词库词(entries 里没有),
        // 只屏蔽、不影响任何词条。
        let lm = UserLM::in_memory();
        lm.add_entry("自造短语", "zi zao duan yu");
        assert_eq!(lm.entry_len(), 1);

        // 词库词:entries 里没有,屏蔽只进 blocklist,不动词条
        lm.block_candidate("词库词", "ci ku ci");
        assert!(lm.snapshot().is_blocked("ci ku ci", "词库词"));
        assert_eq!(lm.entry_len(), 1, "词库词的屏蔽不应动造词条目");

        // 用户造词:屏蔽 + 删词条
        lm.block_candidate("自造短语", "zi zao duan yu");
        assert!(lm.snapshot().is_blocked("zi zao duan yu", "自造短语"));
        assert_eq!(lm.entry_len(), 0, "用户造词应连同词条一起删掉");
        assert!(
            lm.snapshot().lookup_entries("zi zao duan yu").is_empty(),
            "删掉后按 key 查不到该词条"
        );
        assert_eq!(lm.blocklist_len(), 2);
    }

    #[test]
    fn block_candidate_is_idempotent() {
        // 重复屏蔽同一条不重复计(幂等,与 add_entry 同一取舍)。
        let lm = UserLM::in_memory();
        lm.block_candidate("词", "ci");
        lm.block_candidate("词", "ci");
        assert_eq!(lm.blocklist_len(), 1);
        assert!(lm.snapshot().is_blocked("ci", "词"));
    }

    #[test]
    fn block_candidate_clears_counts_too() {
        // **语义变更（2026-10-02）**：旧断言是「屏蔽不动计数」，
        // 现在改成「屏蔽连计数一起清」，理由见 `block_candidate` 文档：
        // ① 屏蔽键是 (拼音, 词)，而计数按**词**记——不清的话同一个词
        //    在别的拼音路径上仍带着计数被加分（多音字串味）；
        // ② 计数就是排名的燃料，燃料不清等于没删。
        let lm = UserLM::in_memory();
        lm.learn(None, "某词");
        lm.learn(Some("某词"), "下一词");
        assert_eq!(lm.counts_with(&lm.snapshot(), None, "某词"), (1, 0));
        assert_eq!(lm.counts_with(&lm.snapshot(), Some("某词"), "下一词"), (1, 1));

        lm.block_candidate("某词", "mou ci");

        assert!(lm.snapshot().is_blocked("mou ci", "某词"), "仍要进屏蔽表");
        assert_eq!(
            lm.counts_with(&lm.snapshot(), None, "某词"),
            (0, 0),
            "被删的词一元计数应清零"
        );
        assert_eq!(
            lm.counts_with(&lm.snapshot(), Some("某词"), "下一词"),
            (1, 0),
            "指向它的二元计数应清掉(只剩「下一词」自己的一元计数)"
        );
        assert_eq!(
            lm.counts_with(&lm.snapshot(), Some("旁词"), "某词"),
            (0, 0),
            "任何上文下它都不该再有计数"
        );
    }

    #[test]
    fn blocklist_survives_roundtrip() {
        // 落盘 + 读回:屏蔽表经 flush 写入 `# xwx-user-blocklist` 区,
        // 重开后仍生效;与计数区、造词区三段共存。
        let dir = std::env::temp_dir().join(format!("xwx_userlm_bl_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("bl.txt");
        {
            let lm = UserLM::open_or_create(&path);
            lm.learn(None, "今天");
            lm.add_entry("自造", "zi zao");
            lm.block_candidate("词库词", "ci ku ci");
            lm.block_candidate("自造", "zi zao"); // 同时屏蔽 + 删词条
            lm.flush().expect("flush 不应失败");
        }
        {
            let lm = UserLM::open_or_create(&path);
            assert_eq!(lm.unigram_len(), 1, "计数区应读回");
            assert_eq!(lm.entry_len(), 0, "自造词条已被删(屏蔽连根拔)");
            assert!(lm.snapshot().is_blocked("ci ku ci", "词库词"), "屏蔽表应读回");
            assert!(lm.snapshot().is_blocked("zi zao", "自造"), "屏蔽表应读回");
            assert!(!lm.snapshot().is_blocked("ci ku ci", "别的词"), "键含词,不误伤");
            assert!(!lm.snapshot().is_blocked("别的拼音", "词库词"), "键含拼音,不误伤");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_file_with_blocklist_section_loads() {
        // 向后兼容:含造词区但无屏蔽表区的「次新」文件(引入屏蔽表之前)
        // 正常加载,屏蔽表为空;手写一份该形态验证分节解析不回退。
        let dir = std::env::temp_dir().join(format!("xwx_userlm_leg2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mid.txt");
        std::fs::write(
            &path,
            "# xwx 用户词频学习模型\n你\t3\n# xwx-user-entries\n自造\tzi zao\n",
        )
        .unwrap();
        let lm = UserLM::open_or_create(&path);
        assert_eq!(lm.unigram_len(), 1, "计数区应加载");
        assert_eq!(lm.entry_len(), 1, "造词区应加载");
        assert_eq!(lm.blocklist_len(), 0, "无屏蔽表区时屏蔽表为空");
        assert!(!lm.snapshot().is_blocked("zi zao", "自造"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
