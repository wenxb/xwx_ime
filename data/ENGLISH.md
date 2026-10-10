# 英文词库 english.bin（运行时只读二进制）

上级：[数据文件说明](README.md)。

本页讲英文通道的**两份**数据产品：

1. `english.bin` —— 词频底座 + 人工覆盖（M1：精确查询与前缀补全）；
2. `english_bigram.bin` —— 静态二元词频表（M2：条件排序与零输入下一词）。

两份都是**可选**的：缺 `english.bin` 整个英文通道关闭；只缺
`english_bigram.bin` 则英文补全照常，只是没有条件排序与预测面板。

英文前缀补全(M1)的独立数据产品,**与中文词库完全正交**:它不参与中文切分、
不改中文 LM、也不进 `MixedLM`。引擎只在输入是纯 ASCII 字母时另开一条通道
(见 `docs/english-input/02-engine.md`)。

**生成**:

    python3 scripts/build_english_dict.py --wordfreq --limit 50000 \
        --reference /tmp/symspell-unigram.txt \
        --reference /usr/share/hunspell/en_US.dic \
        --overrides data/english/overrides.tsv --out data/english.bin

`--wordfreq` 需要 `pip install wordfreq==3.1.1`(构建期依赖,**运行时不依赖
Python**);没有网络时可用 `--entries <词表.tsv>` 走纯本地条目表,格式见
`tests/fixtures/english/words.tsv`(`key<TAB>display<TAB>zipf_centi`)。

### 质量过滤(`--reference`,不给就不过滤)

wordfreq 的 large 表是**语料统计**,不是词典:除了真词还带一批碎片——
脱掉撇号的缩合(`dont`)、社媒拼写(`lol`)、被切碎的词头(`gra`/`lun`/`bri`/
`holi`)、两三个字母的 URL 残渣(`ve`/`im`/`gc`)。中文模式下打的是拼音缩写,
这些碎片正在同一个键盘区:输入 `ve` 连中文候选都没有,碎片直接排第一位,
按空格就把**一个不是单词的字符串**上屏了。

`--reference` 给参考词表(可重复,取并集),规则见 `scripts/english/quality.py`:
人工清单(`--entries`/`--overrides`)永远保留 → 精确命中参考表保留 →
1~2 字母走白名单 → 去词尾(含双写辅音)后词干命中保留 → `zipf>=400` 保留 →
其余丢弃。**没给 `--reference` 时是恒等变换**,旧产物逐字节不变。

参考词表只是**输入**(和 `--wordfreq` 一样),不进仓库:调用方给路径,
`meta.json` 的 `quality_filter` 记下路径、SHA-256 与逐条原因统计,构建可复现。

用到的两份(都不是仓库资产,按需下载):

| 词表 | 来源 | 形态 |
|---|---|---|
| `unigram.txt` | SymSpell 固定提交 `736034a4…` 的 `frequency_dictionary_en_82_765.txt` | `word count` |
| `en_US.dic` | 发行版 `hunspell-en-us`(`/usr/share/hunspell/en_US.dic`) | `word/FLAGS` |

hunspell 那行**必须先把 `/FLAGS` 剥掉**:`github/SM` 过不了「纯字母」检查,
不剥的话整份文件里带标志的词(绝大多数)会被当成无效行丢掉,参考表从
7.6 万缩到 1.5 万,过滤随之过激(`github`/`center` 都被误删)——踩过一次。

实测(2026-10):50 002 词 → **42 770 词 / 103 819 前缀 / 3.94 MB**,
丢弃 7 219 条(原因分布见 `meta.json`)。技术词与专名靠 `overrides.tsv`
兜底(`api`/`css`/`sql`/`npm`/`nginx`/`redis`/…),它们不受过滤与截断影响。

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

## 二、english_bigram.bin（M2 静态二元数据）

| 项 | 值 |
|---|---|
| 构建 | `python3 scripts/build_english_bigram.py --input <pinned.txt> --lexicon data/english.bin --out data/english_bigram.bin` |
| 上游 | SymSpell `736034a4f0436da6a98c94f83f54ec6de2ec25c6` 的 `frequency_bigramdictionary_en_243_342.txt` |
| 上游 SHA-256 | `fd892a160184101dd7ae807ac5a302d01fcea1c47304181a8ed7ed9c94545bcd` |
| 规模 | 242,342 行 → 241,616 词对 → **16,315 context / 114,890 edge / 2.36 MB** |
| 运行期文件 | `$XDG_DATA_HOME/xwx/english_bigram.bin`，其次 `/usr/share/xwx/english_bigram.bin` |
| 缺省行为 | 只降级条件排序与零输入预测 |

布局（little-endian）：64 字节头 → context(24 B/条) → edge(16 B/条) → strings。
`meta.json` 记录构建时 `english.bin` 的 SHA-256（用来发现数据过期）；
**运行时不用跨文件 word_id**，所以旧编号永远不会被解释成另一个词。

来源与许可单独记在 [english/LICENSES/SymSpell-NOTICE.md](english/LICENSES/SymSpell-NOTICE.md)：
**上游代码是 MIT，不代表数据条款**，所以那份说明必须随包分发。
