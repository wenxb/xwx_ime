//! 词库夹具(`Fixture`)与隔离探针(`fingerprint`):每个测试用的小而确定的
//! 词库/缩写表/语言模型,以及「用户数据一个字节都不许变」的指纹检查。

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

/// A small, deterministic dictionary built at test time.
///
/// `data/dict.bin` (80 MB) and `data/abbrev.bin` (69 MB) are gitignored and may
/// be absent, so the core tests must never depend on them. This fixture is
/// built from an in-memory SQLite table with a handful of controlled entries
/// and converted with the project's own `scripts/dict_to_bin.py`
/// (`SELECT pinyin, word, freq FROM entries` — the schema is read by that
/// script, see its line 33).
///
/// The entries are chosen so that the expectations are *exact*:
///
/// | pinyin key  | words (freq desc)                                        |
/// |-------------|----------------------------------------------------------|
/// | `ni`        | 你 泥 尼 逆 匿 腻 拟 妮 倪 霓 溺 昵  (12 → 2 pages)      |
/// | `hao`       | 好 号 毫 豪 浩 皓 耗 郝 嚎 壕 濠 蚝 颢 昊 (14 → 2 pages)  |
/// | `ni hao`    | 你好                                                     |
/// | `bei jing`  | 北京                                                     |
/// | `zhong guo` | 中国                                                     |
/// | `xian`      | 先                                                       |
/// | `xi an`     | 西安                                                     |
///
/// Because the engine sorts by frequency when the language model is empty, the
/// expected candidate order is exactly the frequency order above.
pub struct Fixture {
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    dict_bin: PathBuf,
    abbrev_bin: PathBuf,
    sqlite: PathBuf,
}

/// One fixture per test *process*, built once and shared read-only.
///
/// Tests run on parallel threads; building a fresh SQLite + running two Python
/// converters per test would double the suite's wall time for no benefit. The
/// fixture is immutable after construction.
pub fn shared_fixture() -> &'static Fixture {
    static FIX: std::sync::OnceLock<Fixture> = std::sync::OnceLock::new();
    FIX.get_or_init(Fixture::build)
}

impl Fixture {
    /// Build the fixture (requires `python3`, like the rest of the project's
    /// data pipeline).
    pub fn build() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir for fixture");
        let root = tmp.path().to_path_buf();
        let sqlite = root.join("dict.sqlite3");
        let dict_bin = root.join("dict.bin");

        let rows: &[(&str, &str, u32)] = &[
            ("ni hao", "你好", 9000),
            ("bei jing", "北京", 8000),
            ("zhong guo", "中国", 7000),
            ("xian", "先", 6000),
            ("ni", "你", 5000),
            ("ni", "泥", 4900),
            ("ni", "尼", 4800),
            ("ni", "逆", 4700),
            ("ni", "匿", 4600),
            ("ni", "腻", 4500),
            ("ni", "拟", 4400),
            ("ni", "妮", 4300),
            ("ni", "倪", 4200),
            ("ni", "霓", 4100),
            ("ni", "溺", 3900),
            ("ni", "昵", 3800),
            ("hao", "好", 4000),
            ("hao", "号", 3900),
            ("hao", "毫", 3800),
            ("hao", "豪", 3700),
            ("hao", "浩", 3600),
            ("hao", "皓", 3500),
            ("hao", "耗", 3400),
            ("hao", "郝", 3300),
            ("hao", "嚎", 3200),
            ("hao", "壕", 3100),
            ("hao", "濠", 3000),
            ("hao", "蚝", 2900),
            ("hao", "颢", 2800),
            ("hao", "昊", 2700),
            ("xi an", "西安", 3000),
            // Entries whose *keys* cover the prefixes used by the per-keystroke
            // end-to-end sequences (`zhang` covers z/zh/zha/zhan/zhang, `wang`
            // covers w/wa/wan/wang, `hao` covers h/ha/hao for `http:`).
            // Without them the fallback candidate list is empty for `www`, `.`
            // would fall through to the punctuation arm, and the URL sequence
            // could not be typed at all.
            ("zhang", "张", 2600),
            ("san", "三", 2400),
            ("feng", "风", 2200),
            ("wang", "王", 2000),
            ("ren", "人", 1900),
            // Needed by the abbreviation side index: `zgr` is its initials.
            ("zhong guo ren", "中国人", 5000),
        ];

        // Build the SQLite source with python3 (stdlib sqlite3; no new crates).
        let mut script = String::from(
            "import sqlite3,sys\n\
             c=sqlite3.connect(sys.argv[1])\n\
             c.execute('CREATE TABLE entries (pinyin TEXT, word TEXT, freq INTEGER)')\n\
             rows=[\n",
        );
        for (p, w, f) in rows {
            script.push_str(&format!("({p:?},{w:?},{f}),\n"));
        }
        script.push_str(
            "]\n\
             c.executemany('INSERT INTO entries VALUES (?,?,?)',rows)\n\
             c.commit()\n",
        );
        run_python(&script, &[sqlite.as_os_str()], "build fixture sqlite");

        let script_path = repo_root().join("scripts/dict_to_bin.py");
        let out = Command::new("python3")
            .arg(&script_path)
            .arg(&sqlite)
            .arg(&dict_bin)
            .output()
            .unwrap_or_else(|e| panic!("运行 {} 失败: {e}", script_path.display()));
        assert!(
            out.status.success(),
            "dict_to_bin.py 失败: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );

        // The abbreviation side index is built from the same dictionary so the
        // abbreviation tests exercise the real `MmapDict::with_abbrev` path.
        // `abbrev_to_bin.py` refuses to overwrite its input, hence the copy.
        let scratch = root.join("abbrev-src");
        std::fs::create_dir_all(&scratch).expect("mkdir abbrev-src");
        let src = scratch.join("dict.bin");
        std::fs::copy(&dict_bin, &src).expect("copy dict.bin");
        let abbrev_bin = scratch.join("abbrev.bin");
        let script = repo_root().join("scripts/abbrev_to_bin.py");
        let out = Command::new("python3")
            .arg(&script)
            .arg(&src)
            .arg(&abbrev_bin)
            .output()
            .unwrap_or_else(|e| panic!("运行 {} 失败: {e}", script.display()));
        assert!(
            out.status.success(),
            "abbrev_to_bin.py 失败: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(abbrev_bin.is_file(), "abbrev.bin 未生成");

        Self {
            _tmp: tmp,
            root,
            dict_bin,
            abbrev_bin,
            sqlite,
        }
    }

    pub fn dict_bin(&self) -> &Path {
        &self.dict_bin
    }

    pub fn abbrev_bin(&self) -> &Path {
        &self.abbrev_bin
    }

    /// The SQLite source (kept around for debugging fixture failures).
    pub fn sqlite(&self) -> &Path {
        &self.sqlite
    }
}

/// Workspace root, derived from this crate's manifest dir (`<root>/tests/..`).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The binary under test, built by the same cargo invocation (so it is not
/// stale), located via the `.cargo/config.toml` target dir.
pub fn engine_bin() -> PathBuf {
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(repo_root())
        .output()
        .expect("cargo metadata");
    assert!(out.status.success(), "cargo metadata 失败");
    let json = String::from_utf8_lossy(&out.stdout);
    let key = "\"target_directory\":";
    let at = json.find(key).expect("target_directory 不在 cargo metadata 输出里") + key.len();
    let rest = &json[at..];
    let rest = rest.trim_start();
    let start = rest.find('"').expect("target_directory 格式异常") + 1;
    let end = rest[start..].find('"').expect("target_directory 格式异常") + start;
    let target_dir = PathBuf::from(&rest[start..end]);
    let is_debug = cfg!(debug_assertions);
    let bin = target_dir
        .join(if is_debug { "debug" } else { "release" })
        .join("xwx_ime");
    assert!(
        bin.is_file(),
        "找不到被测二进制 {}（先 cargo build --bin xwx_ime）",
        bin.display()
    );
    bin
}

fn run_python(script: &str, args: &[&std::ffi::OsStr], what: &str) {
    let mut cmd = Command::new("python3");
    cmd.arg("-c").arg(script);
    for a in args {
        cmd.arg(a);
    }
    let out = cmd.output().unwrap_or_else(|e| panic!("{what}: python3 失败: {e}"));
    assert!(
        out.status.success(),
        "{what}: python3 退出码 {:?}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A cheap fingerprint of a path that must not change during a test run.
#[derive(Debug, PartialEq, Eq)]
pub struct PathFingerprint {
    pub exists: bool,
    pub len: u64,
    pub modified: Option<std::time::SystemTime>,
}

pub fn fingerprint(p: &Path) -> PathFingerprint {
    match std::fs::metadata(p) {
        Ok(m) => PathFingerprint {
            exists: true,
            len: m.len(),
            modified: m.modified().ok(),
        },
        Err(_) => PathFingerprint {
            exists: false,
            len: 0,
            modified: None,
        },
    }
}

/// The real user data paths this suite must never touch.
pub fn real_user_paths() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/nonexistent"));
    vec![
        home.join(".local/share/xwx/user_bigram.txt"),
        home.join(".local/share/xwx/dict.bin"),
        home.join(".local/share/xwx"),
        PathBuf::from("/usr/share/ibus/component/xwx.xml"),
    ]
}
