# 英文词库 english.bin（运行时只读二进制）

上级：[数据文件说明](README.md)。

英文前缀补全(M1)的独立数据产品,**与中文词库完全正交**:它不参与中文切分、
不改中文 LM、也不进 `MixedLM`。引擎只在输入是纯 ASCII 字母时另开一条通道
(见 `docs/english-input/02-engine.md`)。

**生成**:

    python3 scripts/build_english_dict.py --wordfreq --limit 50000 \
        --overrides data/english/overrides.tsv --out data/english.bin

`--wordfreq` 需要 `pip install wordfreq==3.1.1`(构建期依赖,**运行时不依赖
Python**);没有网络时可用 `--entries <词表.tsv>` 走纯本地条目表,格式见
`tests/fixtures/english/words.tsv`(`key<TAB>display<TAB>zipf_centi`)。

**格式(v1,全小端)**:

    64 B 头:magic "XWXENG1\0" | version u32 | header_len u32=64
             | word_count u32 | prefix_count u32
             | words_offset u64 | prefixes_offset u64
             | ids_offset u64 | strings_offset u64 | file_len u64
    words:    16 B/条:key_off u32 | display_off u32 | key_len u16 | display_len u16
                      | zipf_centi u16 | reserved u16=0
    prefixes: 16 B/条:prefix_off u32 | prefix_len u16 | id_count u16(1..=32)
                      | ids_start u32 | reserved u32=0
    ids:      u32 数组,每项是一个 word 下标(0 基)
    strings:  UTF-8 字符串池(按 offset+length 寻址,不写 NUL)

约定:word 按 key 严格升序(二分查找的前提)、prefix 按字节严格升序、每个
prefix 的引用列表按「静态分降序、key 升序」排好、`id_count` 1..=32。静态分
是 `zipf_centi - 15 × (词长 - 前缀长)`。

**加载即全量校验**:魔数/版本/头长/`file_len` 与实际长度一致/区段次序与上界
(全程 `checked_*`)/word 与 prefix 严格升序/字符串引用不越界/`reserved` 为 0
/`zipf_centi ≤ 900`/引用列表无重复且被引用词确实以该前缀开头且序正确。
任何一条不成立就返回错误,**装配层只关闭英文并告警**:中文候选、中文用户
模型与按键行为一律不变。

**可选数据,缺了能活**:`data/english.bin` 不进 git(与 `dict.bin` 同等看待),
由发布物分发到 `$XDG_DATA_HOME/xwx/` 或 `/usr/share/xwx/`。文件缺失时英文
通道整体关闭,`Ctrl+空格`、中文候选、造词与屏蔽都不受影响。

**署名与许可**:词频底座是 wordfreq 3.1.1,其**数据**按 CC BY-SA 4.0 再分发
(软件许可是 Apache-2.0,不覆盖数据)。派生产品的署名与许可全文见
[english/NOTICE.md](english/NOTICE.md) 与 [english/LICENSES/](english/LICENSES/);
deb 包会把它们装到 `/usr/share/doc/xwx-ime/`。

