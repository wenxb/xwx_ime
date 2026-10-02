//! gram_probe —— 验证 Rust 能否读取 Rime octagram `.gram` 语言模型（纯可行性验证）
//!
//! 子命令：
//!   gram_probe <gram> [context word]   原有行为：文件头 + 抽查 + 查询
//!   gram_probe enum [gram] [--full] [--dump file]
//!       默认：单遍全量扫描（键总数、叶节点、字节标签直方图）
//!             + 20 万次随机游走采样真实键（长度分布、分隔符检查、解码样例）
//!       --full：再从 root 做 DFS 枚举全部键（慢，默认不启用）
//!   gram_probe coverage [gram] [dict] [--top=N] [--per=N]  词对覆盖率
//!
//! 格式依据（均为 BSD-3-Clause 上游源码）：
//! - lotem/librime-octagram src/gram_db.h: Metadata 结构（44 字节）
//! - librime 仓库 src/rime/dict/mapped_file.h: OffsetPtr 的相对偏移语义
//! - librime vendored 的 darts-clone: include/darts.h（DARTS_VERSION "0.32"）:
//!   DoubleArrayUnit 位布局、traverse() 与 commonPrefixSearch()
//! - lotem/librime-octagram src/gram_db.cc: Build()/Lookup()、octagram.cc: Query()

mod commands;
mod gram;


use commands::*;

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("enum") => run_enum(&mut args),
        Some("coverage") => run_coverage(&mut args),
        _ => {
            // run_default 需要拿到“文件路径 上下文 候选词”完整参数；
            // 这里重新从 argv 构造迭代器，避免第一个参数被上面的 next() 吃掉。
            run_default(std::env::args().skip(1))
        }
    }
}
