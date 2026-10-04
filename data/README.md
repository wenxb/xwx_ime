# 数据文件说明

本目录存放输入法的数据文件:词库(`dict.bin`,运行时 mmap 二进制)与
语言模型(万象 `.gram` / 文本 `bigram.txt`)。运行时文件默认落在
`$XDG_DATA_HOME/xwx/`(通常 `~/.local/share/xwx/`)。环境变量方向不同:

- `XWX_DICT_PATH` / `XWX_LM_PATH` 是引擎**读**词库 / 语言模型的路径,
  显式设置时原样使用;`XWX_LM_PATH` 未设置时按优先级自动探测数据目录
  下的 `.gram`(首选 `wanxiang-lts-zh-hans.gram`),最后回落 `bigram.txt`;
- `XWX_BIGRAM_OUT` 是第五节训练脚本**写** bigram 输出的路径,默认
  `$XDG_DATA_HOME/xwx/bigram.txt`,与 `XWX_LM_PATH` 独立——避免脚本把
  训练产出覆盖到 `.gram` 模型文件上。

词库管线：

```text
rime-ice 词表(dict.yaml) → build_dict.py → dict.sqlite3 → dict_to_bin.py → dict.bin → MmapDict（mmap 只读）
```

`dict.sqlite3` 只是构建**中间格式**；运行时引擎只读 `dict.bin`，
不再触碰 SQLite。

**词表来源**：`build_dict.py` 自动优先读 fcitx5-rime 的
`rime_ice.dict.yaml`（雾凇拼音），按 `import_tables` 展开其词表——
主体是**腾讯词向量词库**（`cn_dicts/tencent`，约 98 万条）加字表
（8105）/基础（base）/扩展（ext）表。Rime 官方自带的
`luna_pinyin` 只是脚本找不到 rime_ice 时的第二优先级兜底。
「Rime」在这里只是词表的**打包格式与渠道**，词库内容不是 Rime 的。

## 一、词库 dict.bin（运行时只读二进制）

由 `scripts/dict_to_bin.py` 从 `dict.sqlite3` 转换生成；`MmapDict`
（`crates/xwx_engine/src/dict.rs`）启动时 mmap 映射，查询=内存二分，
零 SQL、零锁、无逐击键磁盘 IO。

### 二进制布局（v1，little-endian）

```text
偏移      内容
0         magic: "XWXIDM1\0"（8 字节）
8         u32 LE: 版本号（当前 1）
12        u64 LE: 拼音 key 数 P
20        u64 LE: 词条总数
28        u64 LE: 拼音索引区偏移（P × 12 字节）
36        u64 LE: 拼音字符串区偏移
44        u64 LE: 词条区偏移

拼音索引区（P 条，按拼音字节序排序 → 二分查找）:
  u32 LE 拼音串偏移（相对字符串区）+ u16 LE 长度
  + u32 LE 词块偏移（相对词条区）+ u16 LE 词块长度

词条区（每拼音一个块）:
  word_len u8 + word UTF-8 + freq u32 LE，块内按 freq 降序
```

约定：

- `pinyin` 字段多音节用**空格**分隔（`"ni hao"`），与 `Candidate::pinyin`
  格式一致；`ü` 写作 `v`（`lv` / `nve`）；
- 同一 `(pinyin, word)` 唯一；
- 前缀联想（`lookup_prefix`）靠拼音排序数组的区间扫描，替代旧
  SQLite `LIKE` 全表扫描（90ms → 微秒级）。
- rime-ice 的 `tencent` 词表条目只有 `词<TAB>权重` **无注音**（由 Rime
  自动注音）；`build_dict.py` 按同样语义从单字表取**权重最高的读音**
  逐字补全（如 `召` shao=1 / zhao=128448 → 取 zhao），查不到读音的
  字对应词条跳过。实测 98 万条 tencent 全部可注音进库。

## 一点五、简拼副索引 abbrev.bin(运行时只读二进制)

简拼(`zgr` → 中国人)的存储层,路线 (c)(B3 spike 实测结论)。由 `scripts/abbrev_to_bin.py`
**只读** `dict.bin` 生成,产物是**独立文件 `data/abbrev.bin`**(不改动主索引)。

key 是「拼音各音节首字母串」(`"zhong guo ren"` → `"zgr"`);每个词条只进
一个首字母 key,因此关联总数恒等于词条数(实测 1.00×,无组合爆炸)。
`MmapDict::with_abbrev` 装配后,`lookup_abbrev` 对纯简拼直接二分这张表。

### 二进制布局(v1,little-endian)

与 `dict.bin` v1 **完全同构**:同样的 52 字节头、12 字节索引记录、词条区
按 freq 降序。差别只有两处:魔数换成 `XWXABR1\0`,词条区每条多带一个
「全拼」字段(查出的词要回送打分流程,必须带回全拼)。

```text
偏移      内容
0         magic: "XWXABR1\0"(8 字节)
8         u32 LE: 版本号(当前 1)
12        u64 LE: 首字母 key 数 K
20        u64 LE: 关联总数(== 词条数)
28        u64 LE: 首字母索引区偏移(K × 12 字节)
36        u64 LE: 首字母字符串区偏移
44        u64 LE: 词条区偏移

首字母索引区(K 条,按首字母串字节序排序 → 二分查找):
  u32 LE 首字母串偏移(相对字符串区)+ u16 LE 长度
  + u32 LE 词块偏移(相对词条区)+ u16 LE 词块长度

词条区(每首字母 key 一个块,块内按 freq 降序):
  word_len u8 + word UTF-8 + pinyin_len u8 + pinyin UTF-8 + freq u32 LE
```

构建:

```sh
python3 scripts/abbrev_to_bin.py            # data/dict.bin → data/abbrev.bin
python3 scripts/abbrev_to_bin.py in.bin out.bin   # 自定义路径
```

实测(rime-ice 全量词表):唯一首字母 key 569,250 个,产物 68.9 MB
(主索引的 0.90×),构建约 16s。混合简拼(`zhongguor`)不走这张表,
由 `MmapDict::lookup_mixed` 用「音节切分 + 空格拼回主索引前缀」实现,
(「设计更正」)。

## 一点六、音节级词典转移索引 syllable_graph.bin(运行时只读二进制)

缩写弧建边的加速索引(`scripts/build_syllable_graph.py` 生成,读取端
`crates/xwx_engine/src/dict/syllable_graph.rs`)。建边要在「当前已拼到哪个
音节链」上反复问两件事——① 哪些音节能接在后面;② 这条链本身是不是完整
词条。没有索引时只能拿整张音节表逐个去主索引二分(实测 `womenjintqubeij`
一次输入约 1.2 万次二分 ≈ 10.5ms,曾是简拼格的全部成本);索引把这两件事
变成内存树 walk:从根按音节走,子节点就是词库里**真实存在**的后继音节。

由脚本**只读** `dict.bin` 生成:把主索引的拼音 key(字典序是单趟建树的
前提)按最长公共前缀建 trie,0 号节点是根,一条 key 的音节链对应一条树
路径;`MmapDict::with_syllable_graph` 装配。

**可选数据,缺了能活**:文件缺失/损坏/与当前词库**不同代**(判据:头部
第 52 字节的 key 数 ≠ 当前 `dict.bin` 记录数)时引擎**回退旧匹配器**、
不报错——换词库后忘了重建它只会变慢,不会错。也因此不进 git,本机重新
生成即可。

### 二进制布局(v1,little-endian)

```text
偏移      内容
0         magic: "XWXSYL1\0"(8 字节)
8         u32 LE: 版本号(当前 1)
12        u64 LE: 节点数 N
20        u64 LE: 音节名条数 S
28        u64 LE: 节点区偏移
36        u64 LE: 边表区偏移
44        u64 LE: 音节名表偏移
52        u64 LE: 生成时词库的拼音 key 数(同代判据)

节点区(N × 16 字节,下标即节点号,0 号是根):
  u32 LE 子表起始边号(相对边表区)+ u16 LE 子表长度
  + u8 flags(b0 = 完整词条)+ u8 填充
  + u32 LE 词条区间起始(dict.bin 记录号;非完整词条 = 0xFFFFFFFF)
  + u16 LE 词条区间长度 + u16 填充

边表区(每条 6 字节;节点子表按音节号升序 → 读取端二分):
  u16 LE 音节号 + u32 LE 子节点号

音节名表(S 条变长,按名字升序,音节号 = 下标):
  u16 LE 名字长度 + 名字 UTF-8
```

约定:

- **音节号定义为名字升序的下标**,子表按音节号升序落盘,`child()` 直接
  二分;半音节展开 `children_starting_with()` 用线性过滤——根扇出 412、
  深层通常个位数,线性比二分还快;
- **完整词条节点**存该 key 在 `dict.bin` 的**记录号区间**(一个 key 的
  记录在主索引里连续);词条本体不重复存,区间扫描交给主索引;
- 读取端只报错、不 panic(损坏文件在 `open` 阶段被
  `DictError::Corrupt` 拒绝),要不要降级由调用方决定;查询面就是
  `node / child / children_starting_with / walk`。

构建:

```sh
python3 scripts/build_syllable_graph.py                 # data/dict.bin → data/syllable_graph.bin
python3 scripts/build_syllable_graph.py in.bin out.bin  # 自定义路径
```

实测(rime-ice 全量词表,1,730,725 个拼音 key):节点 2,819,021(每个 key
的末节点都是完整词条)、边 2,819,020(严格树:边 = 节点 − 1)、音节 412;
产物 62,020,673 字节(62.0 MB:节点区 45.1 + 边区 16.9 + 音节名表 ~2KB);
脚本内建产物自检(头自洽 + 抽 400 条 key 走通到底)。

## 二、中间格式 dict.sqlite3（构建/迁移用）

引擎运行时不使用，仅作为 `dict_to_bin.py` 的输入中间格式
（`build_dict.py` 从 Rime 构建、TSV 手工导入均产出此格式）。

### Schema

`SqliteDict`（`crates/xwx_engine/src/dict.rs`）在打开词库时幂等建表：

```sql
CREATE TABLE IF NOT EXISTS entries (
    id     INTEGER PRIMARY KEY AUTOINCREMENT,
    pinyin TEXT    NOT NULL,            -- 拼音，多音节空格分隔，如 'ni hao'
    word   TEXT    NOT NULL,            -- 词语
    freq   INTEGER NOT NULL DEFAULT 0   -- 静态词频
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_entries_pinyin_word ON entries(pinyin, word);
CREATE INDEX IF NOT EXISTS idx_entries_pinyin ON entries(pinyin);
```

约定：

- `pinyin` 字段多音节用**空格**分隔（`"ni hao"`），与 `Candidate::pinyin`
  格式一致；`ü` 写作 `v`（`lv` / `nve`）；
- 同一 `(pinyin, word)` 唯一，重复导入时词频**累加**。

### TSV 导入格式

每行三个制表符分隔字段：`拼音<TAB>词语<TAB>频率`，空行与 `#` 开头
的注释行跳过：

```text
ni	你	946028
ni hao	你好	12623
# 注释行会被跳过
```

库内 `SqliteDict::import_tsv` 以单事务导入。命令行没有专门的导入工具，
可用 sqlite3 或 Python 直接导入（见下文 Rime 教程）。

## 三、语言模型 bigram.txt / bigram.bin

### 当前文本格式（bigram.txt）

`BigramLM::load`（`crates/xwx_engine/src/lm.rs`）从文本流加载：

- 两个制表符字段：`词<TAB>次数` → 一元计数（unigram）；
- 三个制表符字段：`前词<TAB>后词<TAB>次数` → 二元计数（bigram）；
- `#` 开头为注释，空行跳过。

```text
# 一元计数
你	946028
你好	12623
# 二元计数
你	好	12109
```

打分方式：log10 概率，加一平滑 + unigram/未知词两级回退，任何输入都
返回有限分数（详见 `lm.rs` 的 `log10_prob` 注释）。

### 二进制格式规划（bigram.bin，已废弃，未实现）

这份二进制规划**不再做**：通用语言模型走了**万象 `.gram`**（mmap
字符级窗口模型，本文件开头与 `lm/` 模块文档），加载与打分都比自研
bigram 二进制更省事，效果也更好。

`bigram.txt` 文本格式至今仍有用武之地——它现在是**用户学习模型**的
存储格式（会话上屏回写 `user_bigram.txt`，`UserLM`/`MixedLM` 把用户层
叠在通用模型之上），上文的一元/二元计数语义就是它的格式。

## 四、从 Rime 词库导入词库（rime_ice 为主，luna_pinyin 为兜底教程）

**实际构建走的是 rime_ice**（见开头「词表来源」）：`build_dict.py`
自动优先读 fcitx5-rime 的 `rime_ice.dict.yaml`，词表主体是腾讯词向量
词库。本节保留作为 **luna_pinyin 兜底教程**——机器上没有 rime_ice
（也没装 `rime-data-luna-pinyin` 时）的替代路径；不装 Rime 的话，
直接按第二节的 TSV 格式手工导入亦可。

Rime 的 `luna_pinyin.dict.yaml` 是公开的现代汉语词库（几十万词条），
可以转成我们的 TSV 格式导入。

### 1. 找到 luna_pinyin.dict.yaml

按输入法框架，文件可能位于：

| 框架 | 路径 |
| --- | --- |
| ibus-rime | `~/.config/ibus/rime/luna_pinyin.dict.yaml` |
| fcitx5-rime | `~/.local/share/fcitx5/rime/luna_pinyin.dict.yaml` |
| 系统预装（Ubuntu 装 `rime-data-luna-pinyin` 后） | `/usr/share/rime-data/luna_pinyin.dict.yaml` |

都没有的话：`sudo apt install rime-data-luna-pinyin`，或
`rime-install luna-pinyin`。

### 2. 格式说明

Rime 词库是 YAML，头部是 `---` 开头的元信息块，正文每行
`词语<TAB>拼音<TAB>频率`（拼音音节用空格分隔，与我们的格式一致，
但**字段顺序相反**）：

```yaml
---
name: luna_pinyin
version: "2021.11.29"
sort: by_weight
use_preset_vocabulary: true
...
你	ni	946028
你好	ni hao	12623
...
```

### 3. 转换并导入

```bash
python3 << 'EOF'
import os, sqlite3, re

SRC = "/usr/share/rime-data/luna_pinyin.dict.yaml"  # 按上文找到的路径改
DB  = f"{os.environ.get('XDG_DATA_HOME', os.path.expanduser('~/.local/share'))}/xwx/dict.sqlite3"
os.makedirs(os.path.dirname(DB), exist_ok=True)

conn = sqlite3.connect(DB)
conn.execute("""CREATE TABLE IF NOT EXISTS entries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    pinyin TEXT NOT NULL, word TEXT NOT NULL, freq INTEGER NOT NULL DEFAULT 0)""")
conn.execute("CREATE UNIQUE INDEX IF NOT EXISTS idx_entries_pinyin_word ON entries(pinyin, word)")

imported = 0
with open(SRC, encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if not line or line.startswith(("#", "---", "...")):
            continue
        if "\t" not in line:
            continue  # 跳过元信息行
        word, pinyin, freq = line.split("\t")[:3]
        conn.execute(
            "INSERT INTO entries (pinyin, word, freq) VALUES (?,?,?) "
            "ON CONFLICT(pinyin, word) DO UPDATE SET freq = freq + excluded.freq",
            (pinyin.strip(), word.strip(), int(freq or 1)),
        )
        imported += 1
conn.commit()
print(f"导入 {imported} 条")
EOF
```

### 4. 转换为运行时二进制 dict.bin

导入完成后必须转成运行时格式（引擎不再读 SQLite）：

```bash
python3 scripts/dict_to_bin.py ~/.local/share/xwx/dict.sqlite3 ~/.local/share/xwx/dict.bin
```

### 5. 验证

```bash
sqlite3 ~/.local/share/xwx/dict.sqlite3 "SELECT COUNT(*) FROM entries"
# 应该输出几十万
sqlite3 ~/.local/share/xwx/dict.sqlite3 \
  "SELECT word, freq FROM entries WHERE pinyin='ni hao' ORDER BY freq DESC LIMIT 5"
# 你好 / 拟好 / 尼豪 ...
```

验证通过后 SQLite 文件即可删除（`~/.local/share/xwx/dict.sqlite3`），
运行时只依赖 `dict.bin`。

## 五、训练语言模型（可选）

从中文语料统计一元/二元词频，输出 `bigram.txt`。

> **必须先分词，且分词词表必须和候选词表是同一套。** 这是本节最容易
> 功亏一篑的地方，两种典型错误都让产出完全不可用：
>
> 1. 用正则把连续汉字串当「词」——`re.findall(r"[一-鿿]+", "今天天气很好")`
>    得到的是 `["今天天气很好"]` 这一个 6 字整串，而不是
>    `["今天", "天气", "很", "好"]`。这样统计出的 unigram 键是整句长串，
>    与词库里的候选词（多为 1–4 字）几乎无交集，LM 查询 `log10_prob`
>    每次都 miss、全退回未知词兜底值，训练等于白跑。
>
> 2. 用 jieba 默认词典分词——**只要分词词表和候选词表不是同一套，
>    统计出的「词」就和实际候选对不上**，LM 查询照样大面积 miss、全退回
>    unigram，bigram 依然白做。
>
> 3. 全量词表直接拿来构造分词词表不过滤——rime-ice 词条最长 29 字，
>    「今天天气很好」「去西安」这类整串/短语本身就是词条。DP 的 Σlog P
>    只认「少切一刀 = 少一份 −log(总频)」，整串/长词条几乎必赢，实测
>    `今天天气很好 → ['今天天气很好']` 整串一个词，bigram 照样统计不出
>    东西。解决：分词词表只保留 ≤4 字且词频 ≥ 下限的词条（见下文）。
>
> 下面的脚本直接从 `dict.bin` 的词条区线性扫出词频表，用同一套词表做
> DP 切分，与引擎的候选集天然一致。若坚持用 jieba，需
> `jieba.set_dictionary(自己导出的词表)`（导出格式 `词<TAB>频次`，词频
> 用下面从 `dict.bin` 扫出的累计值），并调用 `jieba.cut(s, HMM=False)`
> 关掉新词发现，否则会造出词库里没有的词。

**DP 目标函数必须是 Σlog P(词)，不是 Σlog(词频)。** P(词) = 词频/总频，
词频取 `dict.bin` 词条区内同一词跨拼音的累计值，未登录字按词频 1 兜底。
Σlog(词频) 有个隐蔽的坑：词频动辄上万，log(词频) ≈ 9–13 是**正数**，
每多切一刀就凭空多加一个正的 log(词频)——DP 为了最大化目标会把一切都
切成单字。实测在 186 万词的词表下，「今天天气很好，我们去西安玩」被切成
`今 天 天 气 很 好 我 们 去 西 安 玩` 全单字，`今天`/`天气`/`西安` 一个
都没切出来，bigram 里只剩 `天+天`、`天+气` 这种相邻单字对，模型照旧白做。

正确目标必须对每个词都减掉 `log(总频)`：

```text
score(整句) = Σ log P(词) = Σ [ log(词频) − log(总频) ]
```

减去 `log(总频)` 后每多切一个词就多一份 `−log(总频)` 惩罚（本库总词频
7.64×10⁹，ln 底 ≈ −22.76/词），多字词只有词频高到值得这几刀才切得出来。
以下用本库真实词频在**自然对数（ln）**下的数值验证（换成 log10 只是
整体缩放，胜负不变）：

| 候选 | Σln P(整词) | Σln P(拆成单字) | 胜出 |
| --- | --- | --- | --- |
| 今天 | ln 501272 − 22.76 ≈ **−9.6** | (13.11+15.73) − 2×22.76 ≈ −15.7 | 整词 |
| 天气 | ln 492855 − 22.76 ≈ **−9.6** | (15.73+14.99) − 45.5 ≈ −14.8 | 整词 |
| 很好 | ln 500592 − 22.76 ≈ **−9.6** | (15.20+15.80) − 45.5 ≈ −14.5 | 整词 |
| 西安 | ln 55003 − 22.76 ≈ **−11.8** | (14.39+13.80) − 45.5 ≈ −17.3 | 整词 |

注意 `西安` 词频只有 5.5 万，但拆单字要付两个 penalty，照样合不回来；
词频越高、切得越少越稳——**这正是第二道坑的根源**：词表是候选集没错，
但 rime-ice 候选集里混着大量超长、长尾词条（最长 29 字），「今天天气
很好」这种整句串、「去西安」这种动宾短语都是词条。Σlog P 不认词语粒度，
只认「少切一刀 = 少付一份 −log(总频)」：只要整串在词表里且词频 ≥ 1，
胜出门槛就低到离谱。用本库真实词频（T = 总词频 ≈ 7.64×10⁹）验证：

| 词条 | 累计词频 | 整词胜出的门槛 | 实测结果 |
| --- | --- | --- | --- |
| 今天天气很好（6 字） | 100 | 今天×天气×很×好 / T³ ≈ 1.6×10⁻⁵ | 100 ≫ 门槛，整串赢 |
| 去西安（3 字） | 100 | 去×西安 / T ≈ 51.7 | 100 > 51.7，合并成「去西安」 |
| 今天天气（4 字） | 10240 | 今天×天气 / T ≈ 32.3 | 10240 ≫ 32.3，合并成「今天天气」 |

第一行门槛低到 10⁻⁵ 量级：任何词频 ≥ 1 的长串词条都是「必赢」；
而后两行说明词频下限也不够——「今天天气」4 字词频一万照样并，「去西安」
3 字词频才 100 也照并。**所以过滤必须两道同时上（缺一都验不过）**：

1. `MAX_SEG_WORD_LEN = 4`：参与切分的词最多 4 字。>4 字的长串词条
   （今天天气很好这类）根本不进分词词表，只能被拆成 ≤4 字的碎片；
2. `MIN_SEG_FREQ = 20000`：1–4 字的词条累计词频低于下限也不参与分词
   （单字永远保留，作未登录兜底）。默认 20000 是为了盖过「今天天气」
   （10240）这类组合词；换词典、换语料量级时按同量级调整。

过滤只影响分词粒度，`dict.bin` 本身不变，引擎候选集不受影响。实测过滤后：

```text
今天天气很好 → ['今天', '天气', '很好']
我们去西安玩 → ['我们', '去', '西安', '玩']
```

「今天」「天气」「西安」各自成词，整串不再被吞。用下面脚本跑三行语料
（今天天气很好，我们去西安玩。 / 今天天气很好，我们去西安。 /
今天天气很好，去西安玩。），`MIN_BIGRAM=3` 时仍统计出 `(今天,天气)`、
`(天气,很好)`、`(去,西安)` **3 条 bigram（阈值 3 共 5 条候选）**；而不带
过滤的同一脚本只有 `(我们,去西安)`、`(去西安,玩)`（阈值 3 全剪光）——
这就是必须加过滤的唯一判据。

输出格式与本文第三节「当前文本格式」逐字段对应：unigram 是
`词<TAB>次数`（2 字段），bigram 是 `前词<TAB>后词<TAB>次数`（3 字段）。
**剪枝**：二元计数低于 `MIN_BIGRAM` 一律不写入（默认 3，语料更大可调到
5）；不剪枝的话 bigram 轻松几千万条，纯文本加载要几十秒，直接拖慢启动。

**输出路径用独立变量 `XWX_BIGRAM_OUT`,不是 `XWX_LM_PATH`。** 两个变量方向
相反:`XWX_LM_PATH` 是引擎**读**哪个语言模型的路径(默认指向
`wanxiang-lts-zh-hans.gram` 这类 `.gram` 文件);`XWX_BIGRAM_OUT` 是本脚本
**写**训练结果到哪儿(默认 `bigram.txt`)。曾用同一个变量名是坑:设过
`XWX_LM_PATH=~/.local/share/xwx/wanxiang-lts-zh-hans.gram` 让引擎读万象之后,
跑本脚本会把训练产出**覆盖到那个 400 MB 的 .gram 模型文件上**,模型直接毁掉。

```bash
python3 << 'EOF'
import os, re, struct, math, collections

XDG = os.environ.get("XDG_DATA_HOME", os.path.expanduser("~/.local/share"))
DICT = os.environ.get("XWX_DICT_PATH", f"{XDG}/xwx/dict.bin")
OUT  = os.environ.get("XWX_BIGRAM_OUT", f"{XDG}/xwx/bigram.txt")
MIN_BIGRAM = 3          # 二元剪枝阈值；语料大（>1GB）时调到 5

# --- 从 dict.bin 词条区线性扫出词表 ---
# 布局见本文第一节；词条区是连续的 word_len u8 + word UTF-8 + freq u32 LE
blob = open(DICT, "rb").read()
assert blob[:8] == b"XWXIDM1\0", "dict.bin magic 不匹配"
pos = struct.unpack_from("<Q", blob, 44)[0]      # 44: 词条区偏移
freq = collections.Counter()                     # 同一词跨拼音重复 → 累加词频
max_len = 1
while pos < len(blob):
    n = blob[pos]; pos += 1
    w = blob[pos:pos + n].decode("utf-8"); pos += n
    freq[w] += struct.unpack_from("<I", blob, pos)[0]; pos += 4
    max_len = max(max_len, len(w))
total = sum(freq.values())                       # 总词频 = log 分式的分母

# --- 分词词表过滤（为什么，见上文「第二道坑」）---
# 不加这两条：6 字/整串词条必赢，今天天气很好 → ['今天天气很好']，bigram 白做
MAX_SEG_WORD_LEN = 4            # 参与切分的词最长 4 字（>4 字长串不参与）
MIN_SEG_FREQ     = 20000        # 参与切分的最小词频（单字不限，作兜底）
logf = {}
for w, c in freq.items():
    if len(w) > MAX_SEG_WORD_LEN:
        continue
    if len(w) > 1 and c < MIN_SEG_FREQ:
        continue
    logf[w] = math.log(c)       # 预存 log(词频)
print(f"词表 {len(freq)} 词（最长 {max_len} 字），总频 {total}，来自 {DICT}")
print(f"参与切分的词 {len(logf)} 个（≤{MAX_SEG_WORD_LEN} 字 且 词频≥{MIN_SEG_FREQ}；单字不限）")

def segment(text):
    """Viterbi 切分：最大化 Σlog P(词) = Σ[log(词频) − log(总频)]。

    ⚠ 千万别只用 Σlog(词频)：词频上万时每个都是正数（log ≈ +9~+13），
    每多切一刀反而加连正分，DP 必然把一切都切成单字；必须每词减 log(总频)。
    分词词表本身已按 MAX_SEG_WORD_LEN / MIN_SEG_FREQ 过滤（见上文），
    长串/长尾词不参与。
    """
    n = len(text)
    lt = math.log(total)
    dp = [0.0] * (n + 1)           # dp[i]: text[i:] 的最优 ΣlogP
    pick = [1] * (n + 1)           # pick[i]: i 处切出的词长
    for i in range(n - 1, -1, -1):
        dp[i] = logf.get(text[i], 0.0) + dp[i + 1] - lt   # 单字兜底（未登录当频 1，log1=0）
        pick[i] = 1
        for L in range(2, min(MAX_SEG_WORD_LEN, n - i) + 1):
            p = logf.get(text[i:i + L])
            if p is not None:
                v = p + dp[i + L] - lt
                if v > dp[i]:
                    dp[i], pick[i] = v, L        # ΣlogP 更大的切法胜出
    out, i = [], 0
    while i < n:
        out.append(text[i:i + pick[i]]); i += pick[i]
    return out

WORD = re.compile(r"[一-鿿]+")     # 只负责按连续汉字切块（跨标点不成搭配）
uni, bi = collections.Counter(), collections.Counter()
with open("corpus.txt", encoding="utf-8") as f:   # 语料：每行一段文本
    for line in f:
        for run in WORD.findall(line):            # 真正的分词交给 segment()
            words = segment(run)
            for w in words:
                uni[w] += 1
            for a, b in zip(words, words[1:]):
                bi[(a, b)] += 1

# 输出格式见本文第三节「当前文本格式」：
#   2 字段 = 词<TAB>次数（unigram）；3 字段 = 前词<TAB>后词<TAB>次数（bigram）
with open(OUT, "w", encoding="utf-8") as f:
    f.write("# 一元计数\n")
    for w, c in uni.most_common():
        f.write(f"{w}\t{c}\n")
    f.write("# 二元计数\n")
    kept = 0
    for (a, b), c in bi.most_common():
        if c < MIN_BIGRAM:
            break                                 # most_common 已降序，可提前停
        f.write(f"{a}\t{b}\t{c}\n")
        kept += 1
print(f"unigram {len(uni)}，bigram {kept}/{len(bi)}（阈值 {MIN_BIGRAM}）")
EOF
```

规模建议：词汇总量在 10 万级时文件约几十 MB，加载需数秒；
当前文本加载是一次性的，之后的查询都是 O(1) HashMap 查找。
