//! 确定性小词库夹具（每测试进程构建一次，只读共享）。

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Fixture {
    _tmp: tempfile::TempDir,
    dict_bin: PathBuf,
    abbrev_bin: PathBuf,
}

pub fn shared_fixture() -> &'static Fixture {
    static FIX: std::sync::OnceLock<Fixture> = std::sync::OnceLock::new();
    FIX.get_or_init(Fixture::build)
}

impl Fixture {
    pub fn build() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir for fixture");
        let root = tmp.path();
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
            ("zhang", "张", 2600),
            ("san", "三", 2400),
            ("feng", "风", 2200),
            ("wang", "王", 2000),
            ("ren", "人", 1900),
            ("zhong guo ren", "中国人", 5000),
        ];

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

        let dict_script = repo_root().join("scripts/dict_to_bin.py");
        let out = Command::new("python3")
            .arg(&dict_script)
            .arg(&sqlite)
            .arg(&dict_bin)
            .output()
            .unwrap_or_else(|e| panic!("运行 {} 失败: {e}", dict_script.display()));
        assert!(
            out.status.success(),
            "dict_to_bin.py 失败: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );

        // `abbrev_to_bin.py` 拒绝覆盖输入，先拷一份到独立子目录
        let scratch = root.join("abbrev-src");
        std::fs::create_dir_all(&scratch).expect("mkdir abbrev-src");
        let src = scratch.join("dict.bin");
        std::fs::copy(&dict_bin, &src).expect("copy dict.bin");
        let abbrev_bin = scratch.join("abbrev.bin");
        let abbrev_script = repo_root().join("scripts/abbrev_to_bin.py");
        let out = Command::new("python3")
            .arg(&abbrev_script)
            .arg(&src)
            .arg(&abbrev_bin)
            .output()
            .unwrap_or_else(|e| panic!("运行 {} 失败: {e}", abbrev_script.display()));
        assert!(
            out.status.success(),
            "abbrev_to_bin.py 失败: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(abbrev_bin.is_file(), "abbrev.bin 未生成");

        Self {
            _tmp: tmp,
            dict_bin,
            abbrev_bin,
        }
    }

    pub fn dict_bin(&self) -> &Path {
        &self.dict_bin
    }

    pub fn abbrev_bin(&self) -> &Path {
        &self.abbrev_bin
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn english_fixture(name: &str) -> PathBuf {
    repo_root().join("tests/fixtures/english").join(name)
}

pub(super) fn engine_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_xwx_ime"))
}

fn run_python(script: &str, args: &[&std::ffi::OsStr], what: &str) {
    let mut cmd = Command::new("python3");
    cmd.arg("-c").arg(script);
    for a in args {
        cmd.arg(a);
    }
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("{what}: python3 失败: {e}"));
    assert!(
        out.status.success(),
        "{what}: python3 退出码 {:?}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}
