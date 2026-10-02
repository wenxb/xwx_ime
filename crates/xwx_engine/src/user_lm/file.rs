//! 用户模型的**磁盘格式**(从 `user_lm/mod.rs` 拆出,2026-10-02):
//! 分节标记、屏蔽键构造、计数表本体,以及「加载/落盘」两个方向。
//!
//! 拆开的分界是「什么进内存 vs 什么上磁盘」:内存语义(计数增益、
//! 衰减、造词判据)留在 `mod.rs`,字节格式全部在这里。

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use tracing::{info, warn};

use crate::lm::{BigramLM, LmError};

use super::UserLM;

/// 落盘文件里造词节的分节标记行(加载时据此切换解析;旧文件没有
/// 这一行,整文件按 bigram 格式解析 → 词条表为空,向后兼容)。
pub(super) const ENTRIES_SECTION_MARK: &str = "# xwx-user-entries";

/// 落盘文件里屏蔽表节的分节标记行(同样向后兼容:旧文件没有这一行,
/// 屏蔽表为空;`#` 前缀让它落在旧版解析器的注释行分支里——旧版本
/// 读新文件也只会把它当注释跳过,双向兼容)。
pub(super) const BLOCKLIST_SECTION_MARK: &str = "# xwx-user-blocklist";

/// 屏蔽表的存储键:`"拼音 key\u{1}词"`(U+0001 分隔,拼音是 ASCII、
/// 词是 UTF-8 文本都不会含它)。集中一处构造,保证写入/查询/落盘
/// 三处的键形一致。
pub(super) fn block_key(pinyin_key: &str, word: &str) -> String {
    let mut k = String::with_capacity(pinyin_key.len() + 1 + word.len());
    k.push_str(pinyin_key);
    k.push('\u{1}');
    k.push_str(word);
    k
}

enum Section {
    /// bigram 计数区(旧格式本体,默认)。
    Bigram,
    /// 造词区(`# xwx-user-entries` 之后)。
    Entries,
    /// 屏蔽表区(`# xwx-user-blocklist` 之后)。
    Blocklist,
}

/// 计数表本体(不跨线程共享;共享由 [`UserLM`] 的 `RwLock` 承担)。
#[derive(Debug, Default)]
pub(super) struct UserTables {
    /// 一元计数:词 -> 上屏次数。
    pub(super) unigrams: HashMap<String, u64>,
    /// 二元计数:前词 -> (后词 -> 上屏次数)。
    pub(super) bigrams: HashMap<String, HashMap<String, u64>>,
    /// 造词条目:拼音 key(音节空格分隔,与词典 key 同格式)-> 词列表。
    ///
    /// 与 unigrams/bigrams 的分工:计数表管「学过的词该排多靠前」
    /// (LM 打分),entries 管「词库里根本没有的短语能被查到」(召回)。
    /// 造出的词同时会进 unigrams(上屏即学),因此它既有候选召回、
    /// 又有用户层打分增益,两条通道一体衰减。
    pub(super) entries: HashMap<String, Vec<String>>,
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
    pub(super) blocklist: HashSet<String>,
}

impl UserLM {
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
}
