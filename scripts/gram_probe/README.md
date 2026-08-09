# gram_probe — 验证 Rust 能否读取 Rime octagram `.gram` 语言模型

纯可行性验证，不做任何集成。独立 Cargo 项目（`[workspace]` 空表，
已从主 workspace 排除，不污染主项目构建）。

## 运行

```bash
cargo build --release --offline          # memmap2 =0.9.11 已在本地 registry，无需联网
./target/release/gram_probe [可选: .gram 路径]
./target/release/gram_probe enum [gram] [--full]      # 键结构分析（默认快扫）
./target/release/gram_probe coverage [gram] [dict] [--top=N] [--per=M]  # 词对覆盖率
```

默认读取 `/home/youngwen/.local/share/xwx/wanxiang-lts-zh-hans.gram`
（420,253,740 字节，sha256 `051a55abb3...`，来自 RIME-LMDG 的 LTS release，
CC-BY-4.0）。也支持自定义查询：

```bash
./target/debug/gram_probe <gram 路径> <context> <word>
```

程序打印：10 万次查询性能粗测、文件头解析、double-array 抽查、
8 组常见搭配查询（含还原频率 exp(value/10000)）、6 组对照组、结论。
实测 debug 构建约 67 万次 lookup/秒（内存 1 拷，页缓存命中后为纯内存访问）。

## 结论（2026-08-08 实测）

能读通。Rust + memmap2 可直接读取并查询该 .gram 文件，
不需要任何第三方 double-array 解析 crate。

依据：

1. 文件头 `Rime::Grammar/1.0`：db_checksum=0，
   double_array_size=105,063,424（单元数，来自 GramDb::Build() 的
   array_size = trie_->size()）；OffsetPtr 的偏移量为 4，按 librime
   mapped_file.h 的 get() = &offset_ + offset_ 语义，
   数组实际起始于文件偏移 44。

2. 大小自洽：44 + 105,063,424 * 4 = 420,253,740 恰好等于文件大小，
   数组占满文件剩余全部字节，尾部无多余数据段。

3. 单元宽度 4 字节、位布局与 librime vendored
   include/darts.h（DARTS_VERSION "0.32"，darts-clone 0.32）一致：
   label() = unit & 0x800000FF；offset() = (unit >> 10) << ((unit & 0x200) >> 6)；
   has_leaf() = (unit >> 8) & 1；value() = unit & 0x7FFFFFFF。

4. 查询语义对齐 GramDb::Lookup：context 经遍历后再对 word 做
   commonPrefixSearch（最多 8 项）。常见搭配返回有序、量级合理的值
   （exp(value/10000) 从几十万到约一亿，与 ARPA log 概率 * total_count
   的生成方式吻合）。

5. 对照组（生僻字组合等不在词典中的路径）正确返回「不存在/无匹配」。

## 上一轮 782,336 字节对不上的真相

是读错了 double_array_size：正确值 0x06432400 = 105,063,424 单元，
与文件大小完全契合。不存在尾部多余数据。

## 生产级实现的坑（将来要集成时）

- parse value 时叶子单元高 31 位才是数值，记得先 mask。
- traverse()/commonPrefixSearch() 的 node_pos ^= offset ^ byte 可能越界，
  必须做边界检查（本程序的 DoubleArray::unit() 已带检查，不会 panic）。
- 编码是逐码点的定制编码（gram_encoding.cc），不是 UTF-8 原样；
  context 取末尾 N 个字符、word 取开头 N 个字符，kMaxEncodedUnicode=8，
  且受 collocation_max_length-1 截断。
- 若只要查询，DoubleArray + encode + traverse + commonPrefixSearch
  约 200 行；加上 octagram 的 penalty / min-max length 决策约再 100 行。

## 遍历公式（实测校准，勿用有缺陷的简化式）

darts-clone（本文件）的节点迁移不是 `child = offset XOR byte`，而是：

```
child = node_pos ^ offset(node) ^ byte      // offset(node) = (unit>>10)<<((unit&0x200)>>6)
```

即父节点自己的索引也参与异或（offset 字段对父索引做了复用压缩）。这一点
容易踩坑：只写 `offset ^ byte` 的话，首字节层或许能碰巧对上，第二字节起
全部失配。本程序 traverse()/commonPrefixSearch() 均按上式实现并已实测。

## 子命令说明

- `gram_probe <gram路径> [context word]`：文件头 + 抽查 + 自定义查询
  （默认文件名见上）。
- `gram_probe coverage <gram> <dict.bin> --top=N --per=M`：用主项目
  dict.bin 的高频词对测双字搭配覆盖率与精确命中率，输出分类统计。
- `gram_probe enum [gram] [--full]`：键结构分析。默认快路径：Pass A 单遍
  全量扫描（叶单元数=键总数，label 字节直方图）+ Pass B 50 万次随机游走
  （真实键采样：长度分布 / 空格与控制字节检查 / `$` 尾哨兵 / 解码样例 /
  跨词边界探针）。`--full` 才做完整 DFS（每节点 256 扇出，十几分钟级，
  已带环与深度剪枝；Rust 侧 unit() 越界当结束条件）。

## 键的结构（2026-08-08 实测，决定性实验）

方法：Pass A 单遍扫描（精确） + Pass B 50 万次随机游走采真实键 +
跨词边界探针。

1. **键总数 = 6,245,324 条**（叶单元精确计数；叶单元 label() 的 MSB 恒为 1，
  见 darts.h 注释）。
2. **键里没有空格、没有分隔符**：全量边字节直方图里 0x20 出现 157,202 处，
  但逐字节验证全部是汉字两字节编码的第二字节（例：张=9f 20、素=bd 20、
  倠=90 20）；随机游走采集的 45 万条真实键解码后也不存在独立空格。
  「词级 n-gram（空格拼接后编码）」被证伪。
3. **字符长度分布**（采样 45 万条）：1 字 0.6%、2 字 2.2%、3 字 39.9%、
  4 字 38.1%、5 字 18.0%、6 字 1.1%——3~6 字占 ~97%，与
  collocation_min_length=3 / max=6 吻合。另有 `$` 尾哨兵键（rear 模型，
  如「绩啦$」，对应用户 octagram.cc 里 `Lookup(word_query, "$")`）。
4. **键跨词边界**：存在「天天气」「天气很」「天天气很」这类不可能落在
  两个合法词边界上的键 → 索引的是字符级滑动窗口，不是词对。
5. 查询语义（gram_db.cc Lookup）= `traverse(context)` 成功后
  `commonPrefixSearch(word)`，字节级直接拼接、无分隔符；与建库端
  （build_grammar.cc 对整串文本 encode）完全一致。

结论：**键是字级（连续字符序列）**，与词表是否对齐无关；任何
「上文末 ≤5 字 + 候选词首 ≤5 字」的连续字符窗，只要在训练语料中出现过
就能取到分。

## dict.bin 覆盖率（2026-08-08 实测）

186 万词按频次取 top 8000，按词长分组（1字/2字/3字/4字/4字+），
组内确定性随机配对（每组 300 对，4字+ 组 39 对，共 5151 对），
查询语义对齐 octagram.cc Query（上下文取末尾 ≤5 字、候选取开头 ≤5 字、
后缀从长到短逐级 lookup）：

| 上文×候选 | 任意窗口命中 | 整词精确 |
|-----------|-------------|----------|
| 1字 × 1字 | 3.7%        | 3.7%     |
| 1字 × 2字 | 13.0%       | 13.0%    |
| 1字 × 3字 | 21.3%       | 21.3%    |
| 2字 × 2字 | 70.7%       | 66.3%    |
| 2字 × 3字 | 78.0%       | 71.7%    |
| 2字 × 4字 | 79.7%       | 75.3%    |
| 3字 × 2字 | 67.0%       | 28.3%    |
| 3字 × 3字 | 78.3%       | 34.3%    |
| 3字 × 4字 | 72.3%       | 26.3%    |
| 4字 × 4字 | 83.3%       | 3.0%     |
| 全部      | 50.0%       | 24.0%    |

解读：上下文 ≥2 字的常用搭配约 70%~80% 能拿到上下文分；单字上文 × 单字
候选最差（3.7%）。上下文越长整词命中越低（键最长 6 字，只能靠 suffix
窗口），但任意窗口命中率反而更高——模型设计如此。真实场景对照
（前 400 高频词作上文 × 前 600 高频词作候选，24 万组）任意命中 11.1%，
因为组合里多为「单字上文 × 四字词」。

结论：接入该模型**不需要词库与其对齐**；拿不拿得到上下文分只取决于
「上文尾若干字 + 候选首若干字」这个字符串在模型训练语料里出现过没有。
