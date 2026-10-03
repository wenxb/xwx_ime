//! 必需数据验收：缺词库/副索引必须**失败**。
//!
//! 既有单测（`dict/tests.rs`、`engine/tests/*`）在数据缺失时 `return` 算绿——那是为了
//! 让没有大数据的机器也能跑快速测试，代价是**正式验收也一起绿了**。本文件是那套快测
//! 之外的另一档：它存在的意义就是缺数据时红，所以这里**不做任何存在性跳过**。

use std::path::{Path, PathBuf};

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("data")
}

#[test]
fn required_data_files_are_present_and_loadable() {
    let data = data_dir();
    for f in ["dict.bin", "abbrev.bin"] {
        let p = data.join(f);
        assert!(
            p.is_file(),
            "必需数据缺失: {}（生成见 scripts/dict_to_bin.py 与 scripts/abbrev_to_bin.py）",
            p.display()
        );
    }

    let dict = xwx_engine::MmapDict::open(data.join("dict.bin")).expect("词库应能加载");
    assert!(!dict.has_abbrev(), "裸 open 不该挂副索引");
    let dict = dict.with_abbrev(data.join("abbrev.bin"));
    assert!(dict.has_abbrev(), "副索引应已装配");

    // 一次真实查询：副索引能用，而不是「文件在但内容坏」。
    let cands = dict.lookup_abbrev("zgr").expect("简拼查询应成功");
    assert!(
        cands.iter().any(|c| c.word == "中国人"),
        "zgr 应命中「中国人」，实际前 3: {:?}",
        cands.iter().take(3).map(|c| &c.word).collect::<Vec<_>>()
    );
}
