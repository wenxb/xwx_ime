//! SQLite 词库实现(`rusqlite`):开发/测试用的小词库后端,
//! 生产走 mmap 二进制(见 `mmap.rs`)。

use std::io::BufRead;
use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};

use crate::candidate::Candidate;

use super::{DictError, Dictionary, MAX_LOOKUP_RESULTS};

/// 基于 SQLite 的词库。
///
/// `rusqlite::Connection` 不是 `Sync`，这里用 `Mutex` 包一层，词库对象
/// 因此可以在多线程间共享（`ImeEngine` 随之变成 `Sync`，可供 IBus 适配层
/// 的多个输入会话并发查词；每次查词短暂持锁，命中率足够）。
/// TODO: 若查询成为瓶颈，换成连接池或只读 mmap 实现。
#[derive(Debug)]
pub struct SqliteDict {
    conn: Mutex<Connection>,
}

/// 词库建表语句。
///
/// TODO: 后续需要支持：
/// - `user_entries`（用户自造词，与系统词库分离，允许增删）；
/// - 分词/词性等附加列（`data/README.md` 中有规划）。
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS entries (
    id     INTEGER PRIMARY KEY AUTOINCREMENT,
    pinyin TEXT    NOT NULL,            -- 拼音，多音节空格分隔，如 'ni hao'
    word   TEXT    NOT NULL,            -- 词语
    freq   INTEGER NOT NULL DEFAULT 0   -- 静态词频
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_entries_pinyin_word ON entries(pinyin, word);
CREATE INDEX IF NOT EXISTS idx_entries_pinyin ON entries(pinyin);
";

impl SqliteDict {
    /// 打开（必要时创建）指定路径的词库文件；父目录不存在时自动创建。
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, DictError> {
        let path = path.as_ref();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let conn = Connection::open(path)?;
        let dict = Self { conn: Mutex::new(conn) };
        dict.create_schema()?;
        Ok(dict)
    }

    /// 打开内存词库（测试 / 无持久化场景）。
    pub fn open_in_memory() -> Result<Self, DictError> {
        let conn = Connection::open_in_memory()?;
        let dict = Self { conn: Mutex::new(conn) };
        dict.create_schema()?;
        Ok(dict)
    }

    /// 建表（幂等）。数据变更/迁移统一放这里，TODO: 引入 schema 版本号。
    pub fn create_schema(&self) -> Result<(), DictError> {
        let conn = self.conn.lock().map_err(|_| DictError::LockPoisoned)?;
        conn.execute_batch(SCHEMA)?;
        Ok(())
    }

    /// 插入（或累加）一条词条。同一 (pinyin, word) 已存在时累加词频。
    pub fn upsert(&self, word: &str, pinyin: &str, freq: u64) -> Result<(), DictError> {
        let conn = self.conn.lock().map_err(|_| DictError::LockPoisoned)?;
        conn.execute(
            "INSERT INTO entries (pinyin, word, freq) VALUES (?1, ?2, ?3)
             ON CONFLICT(pinyin, word) DO UPDATE SET freq = freq + excluded.freq",
            params![pinyin, word, freq],
        )?;
        Ok(())
    }

    /// 从 TSV 流批量导入词条。
    ///
    /// 每行格式：`拼音<TAB>词语<TAB>频率`，空行与 `#` 注释行跳过。
    /// 整个导入在一个事务里完成，中途出错则整体回滚。
    pub fn import_tsv(&self, reader: impl BufRead) -> Result<u64, DictError> {
        let conn = self.conn.lock().map_err(|_| DictError::LockPoisoned)?;
        let tx = conn.unchecked_transaction()?;
        let mut imported = 0u64;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO entries (pinyin, word, freq) VALUES (?1, ?2, ?3)
                 ON CONFLICT(pinyin, word) DO UPDATE SET freq = freq + excluded.freq",
            )?;
            for (i, line) in reader.lines().enumerate() {
                let line_no = (i + 1) as u64;
                let line = line?;
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let mut parts = line.split('\t');
                let (Some(pinyin), Some(word), Some(freq)) =
                    (parts.next(), parts.next(), parts.next())
                else {
                    return Err(DictError::BadTsvLine {
                        line: line_no,
                        detail: format!("期望 3 个制表符分隔的字段，实际: {line:?}"),
                    });
                };
                let freq: u64 = freq.trim().parse().map_err(|e| DictError::BadTsvLine {
                    line: line_no,
                    detail: format!("频率不是整数: {freq:?} ({e})"),
                })?;
                stmt.execute(params![pinyin.trim(), word.trim(), freq])?;
                imported += 1;
            }
        }
        tx.commit()?;
        Ok(imported)
    }

    /// 词条总数（调试/统计用）。
    pub fn count(&self) -> Result<u64, DictError> {
        let conn = self.conn.lock().map_err(|_| DictError::LockPoisoned)?;
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))?;
        Ok(n as u64)
    }
}

impl Dictionary for SqliteDict {
    fn lookup(&self, pinyin: &str) -> Result<Vec<Candidate>, DictError> {
        let conn = self.conn.lock().map_err(|_| DictError::LockPoisoned)?;
        let mut stmt = conn.prepare(
            "SELECT word, pinyin, freq FROM entries
             WHERE pinyin = ?1
             ORDER BY freq DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![pinyin, MAX_LOOKUP_RESULTS], |row| {
            Ok(Candidate {
                word: row.get(0)?,
                pinyin: row.get(1)?,
                score: 0.0, // 得分由引擎统一计算
                freq: row.get(2)?,
                spelling: crate::candidate::SpellingKind::Exact, // 词典条目是精确匹配
                echo: false,  // 词典条目是真实候选
                covered: 0,   // 未知/整串:由引擎按查询来源回填(见 Candidate::covered)
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn lookup_prefix(&self, pinyin: &str) -> Result<Vec<Candidate>, DictError> {
        self.lookup_prefix_limited(pinyin, MAX_LOOKUP_RESULTS as usize)
    }

    fn lookup_prefix_limited(
        &self,
        pinyin: &str,
        limit: usize,
    ) -> Result<Vec<Candidate>, DictError> {
        let conn = self.conn.lock().map_err(|_| DictError::LockPoisoned)?;
        let mut stmt = conn.prepare(
            "SELECT word, pinyin, freq FROM entries
             WHERE pinyin LIKE (?1 || '%') ESCAPE '\\'
             ORDER BY freq DESC
             LIMIT ?2",
        )?;
        // 转义 LIKE 通配符，防止用户输入 %/_ 影响查询。
        let escaped = pinyin.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        let limit = (limit as i64).min(MAX_LOOKUP_RESULTS);
        let rows = stmt.query_map(
            params![escaped, limit],
            |row| {
                Ok(Candidate {
                    word: row.get(0)?,
                    pinyin: row.get(1)?,
                    score: 0.0,
                    freq: row.get(2)?,
                    spelling: crate::candidate::SpellingKind::Exact, // 词典条目是精确匹配
                    echo: false,  // 词典条目是真实候选
                    covered: 0,   // 未知/整串:由引擎按查询来源回填(见 Candidate::covered)
                })
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn total_freq(&self) -> Result<u64, DictError> {
        let conn = self.conn.lock().map_err(|_| DictError::LockPoisoned)?;
        Ok(conn.query_row("SELECT COALESCE(SUM(freq), 0) FROM entries", [], |r| r.get(0))?)
    }
}
