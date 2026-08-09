//! 对拍探针:从 stdin 逐行读 ASCII 拼音串,输出固定格式的解码结果,
//! 供与 librime 侧工具逐行 diff(输出契约见下,逐字节遵守)。
//!
//! 输出契约(每个输入串一条记录,`\t` 为真实制表符):
//! ```text
//! IN\t<原始输入串>
//! PREEDIT\t<preedit 文本;本引擎无 preedit 概念,输出原始输入串>
//! NCAND\t<候选总数>
//! CAND\t1\t<候选文本>
//! ...
//! END
//! ```
//! - `CAND` 行最多前 100 条;`NCAND` 是全量候选数(不受 100 条截断影响);
//! - `END` 之后无空行;空行与 `#` 开头的行跳过(不产出记录)。
//!
//! 用法:
//! ```sh
//! printf 'nihao\n' | cargo run --example probe --offline -- [dict.bin] [.gram]
//! ```
//! 路径解析(与 eval_rank.rs 同一套):命令行参数 > 环境变量
//! `XWX_DICT_PATH` / `XWX_LM_PATH` > 默认仓库 `data/dict.bin` 与
//! `~/.local/share/xwx/wanxiang-lts-zh-hans.gram`。
//! 文件缺失即报错退出并打印期望路径——绝不静默降级成空词库/空 LM
//! (引擎的 new_with_lm 对缺失文件会优雅退回空实现,那对拍会全是
//! 假阴性,所以这里在加载前先自己检查)。

use std::io::{BufRead, Write};
use std::path::PathBuf;

use xwx_engine::{ImeEngine, LmContext};

/// 命令行 > 环境变量 > 仓库 data/ 默认;不存在时给 fallback(供 .gram
/// 这种仓库里没有、在 XDG 数据目录下的文件)。
fn resolve(cli: Option<&str>, env_key: &str, default: &str, fallback: Option<PathBuf>) -> PathBuf {
    if let Some(p) = cli {
        return PathBuf::from(p);
    }
    if let Some(p) = std::env::var_os(env_key) {
        return PathBuf::from(p);
    }
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(default);
    if p.is_file() || fallback.is_none() {
        return p;
    }
    fallback.unwrap()
}

fn xdg_xwx(file: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("xwx").join(file))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dict = resolve(
        args.first().map(String::as_str),
        "XWX_DICT_PATH",
        "data/dict.bin",
        xdg_xwx("dict.bin"),
    );
    let gram = resolve(
        args.get(1).map(String::as_str),
        "XWX_LM_PATH",
        "data/wanxiang-lts-zh-hans.gram",
        xdg_xwx("wanxiang-lts-zh-hans.gram"),
    );

    if !dict.is_file() {
        eprintln!("缺词库文件:{}(期望 dict.bin)", dict.display());
        std::process::exit(1);
    }
    if !gram.is_file() {
        eprintln!("缺语言模型文件:{}(期望 wanxiang .gram)", gram.display());
        std::process::exit(1);
    }

    let mut engine = ImeEngine::new_with_lm(Some(&dict), Some(&gram)).expect("引擎创建失败");
    // 简拼副索引:优先 XWX_ABBREV_PATH,其次与 dict.bin 同目录的
    // abbrev.bin。缺失时引擎退回精确路径(优雅退化),对拍不因此挂掉。
    let abbrev = std::env::var_os("XWX_ABBREV_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| dict.with_file_name("abbrev.bin"));
    engine.dict = engine.dict.with_abbrev(&abbrev);
    // 保险:引擎对坏文件会静默退回空实现,这里核对加载结果不是空壳。
    if engine.dict_count() == 0 {
        eprintln!("词库加载后词条数为 0:{}(疑似损坏)", dict.display());
        std::process::exit(1);
    }
    eprintln!(
        "probe: dict={}({} 条) gram={} abbrev={}",
        dict.display(),
        engine.dict_count(),
        gram.display(),
        if engine.dict.has_abbrev() {
            abbrev.display().to_string()
        } else {
            "(未装配)".to_string()
        }
    );

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let ctx = LmContext::empty();

    for line in stdin.lock().lines() {
        let line = line.expect("读取 stdin 失败");
        let input = line.trim_end_matches(['\r', '\n']);
        if input.is_empty() || input.starts_with('#') {
            continue;
        }
        let cands = engine
            .lookup_candidates(input, &ctx)
            .expect("候选查询失败");
        // 本引擎没有 preedit 概念,按契约输出原始输入串。
        write!(out, "IN\t{input}\nPREEDIT\t{input}\nNCAND\t{}", cands.len())
            .expect("写入 stdout 失败");
        for (i, c) in cands.iter().take(100).enumerate() {
            write!(out, "\nCAND\t{}\t{}", i + 1, c.word).expect("写入 stdout 失败");
        }
        writeln!(out, "\nEND").expect("写入 stdout 失败");
    }
}
