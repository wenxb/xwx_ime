# XWX 项目认知指南

> 目的：让你（项目所有者）对「每个关键地方用了什么方法、为什么、想优化该去哪」有一个清晰的地图。
> 所有路径都可直接跳转；文中数字均来自代码注释里的实测记录。

---

## 1. 项目是什么、分几层

**从零实现的拼音输入法**：算法全部自研（纯 Rust），界面不自绘——候选窗、预编辑区都交给
GNOME Shell / IBus 自带组件画。整个项目只做「算法 + 协议」。

三个 crate，依赖单向：

```
xwx_ime (bin, src/main.rs)      进程入口：连 D-Bus、注册组件、导出对象
  └─ xwx_ibus (lib)             IBus 协议适配：按键→InputEvent，结果→IBus 信号
       └─ xwx_engine (lib)      算法核心：切分/词库/LM/纠错/会话，不沾 GUI/DBus
```

**一次按键的生命周期**：

```
键盘事件 → ibus-daemon(D-Bus) → XwxEngineServer(zbus, xwx_ibus/engine.rs)
  → InputEvent → InputSession 状态机(xwx_engine/session/)
  → ImeEngine::lookup_candidates(xwx_engine/engine/lookup.rs)  ← 核心查询
  → Vec<Candidate> → UpdateLookupTable / UpdatePreeditText 信号
  → GNOME Shell 画候选窗；用户选词 → CommitText 上屏
```

---

## 2. 核心算法链路（xwx_engine）

### 2.1 拼音切分：音节表 + DP 枚举所有合法切法

- `segmenter.rs`：内置 400+ 无声调音节表；用记忆化递归（DP）枚举一个字母串的
  **全部**合法音节切法。`xian` 同时返回 `[xian]` 和 `[xi,an]` 两种——**切分不确定
  的问题不在这里裁决，交给后面的语言模型打分决定**。
- 简拼索引 `abbrev_syllables()`：按首字母（含 zh/ch/sh）建 `OnceLock` 静态索引，
  链枚举时 O(1) 取「以某字母开头的音节」切片，顺序「短音节优先」（实测决定长串
  简拼能不能解出正确句子）。

### 2.2 切分格（lattice）：把「查词」变成「建图」

`engine/lattice/build.rs` + `engine/lattice.rs`。**这是整个引擎最重要的抽象**：
不在「切分×查词」的乘积空间里枚举，而是从每个位置做音节链 DFS，每条链的拼音串
查词库，查中的词成为图上一条「词边」（起点、终点、词、频次）。之后一切
（解码、排序、纠错、简拼）都在这张图上做。

三种格，共用同一套建边代码：

| 格 | 何时构建 | 干什么 |
|---|---|---|
| 精确格 | 每次查询 | 字面合法读法的词边 |
| 简拼格 | 精确格解不出整串时 | 额外允许「单字母 = 某音节首字母」的读法，`wmjtqbj`→我们今天去北京 |
| 容错格 | 精确解不出**或**输入有结构错误信号时 | 在「走不动的位置」附近补 ≤1 次编辑的弧 |

**关键教训（已写进注释）**：容错格不重建整张格——实测重建会把 8ms 推到 15ms。
做法是克隆精确格（几百条边，微秒级），只对错处附近重跑链枚举、只推「用到弧」的新边。

**最坏情况用配额常量钳死**（防指数爆炸，都是实测标定）：
`MAX_CHAINS_PER_START=64`（每起点链数）、`MAX_TOLERANT_CHAINS_PER_START`、
`MAX_MIXED_DFS_STEPS=20_000`（交错 DFS 索引探测步数）、`MAX_MIXED_COLLECT=4000`
（探索池上限，早了会丢正确答案，晚了超时）。

### 2.3 解码与候选收集：beam 退居二线，DP 成为主力的演进

`engine/decode.rs`、`engine/lookup.rs`。这里有一段重要的架构演化（注释里写得很清楚）：

- **旧方案**：beam search（`BEAM_WIDTH=16`）沿格推进，候选就是 beam 里的路径词
  → 缺陷：候选数量被 beam 宽度钳死，好词在被剪掉的路径上。
- **现行方案**（生产路径 `lookup_candidates`）：
  1. 建精确格；
  2. **布尔 DAG 可达性 DP** 替代 beam——只回答「存在覆盖整串的读法吗？」，
     每条边碰一次、零 LM 调用（beam 只在测试/对拍里保留）；
  3. **候选 = 格位置 0 上的全部词边**，数量不受 beam 钳制（上限 `MAX_CANDIDATES=512`）；
  4. 候选分 = **「以该候选开头的最优完整路径分」**：边自身分 + 从边终点到串尾的
     最优补全分（一次**后向 DP** 算出）。这让「覆盖整串的词（你好）」和
     「只覆盖前缀的词（你）」落在同一量纲上，前者自然胜出；
  5. 整串解不出（打字中，如 `nih`）→ `fallback_candidates` 前缀联想兜底。

**打分口径的两个补丁（都是修真实 bug 的产物，注释里有完整推导）**：
- **基线补偿**：LM 的「无证据基线分」（万象实测 -11.6）不按词数复付——否则多词组合
  「出+在+哪」白扣 N 次基线，输给生僻单词「杵在那」（实测差 12.19 个 log10）；
- **词频归一化**：词频项写成 `log10(freq/总词频)`，与 LM 的 log10 概率同量纲
  （都是 -1..-12 的负数），两者才能加权相加（`DEFAULT_LM_WEIGHT=1.0`，扫参定的）。

**性能红线**：单次查询 ≤ 12ms（IBus 按键是同步处理的）。所有常量改动都要重跑
`examples/*_bench.rs`，且**必须 `--release`**（debug 下同一代码 9.6ms vs release 1.5ms，
历史上据此误判过「退化 7 倍」）。

### 2.4 排序：多层结构键，不是单一分数

`engine/rank.rs` 的 `sort_candidates`。排序键（元组，逐级比较）：

1. **u_min 桶**：候选终点处「不可避免的未解释字母数」。0 = 终点在音节边界
   （高频单字「被/点」），>0 = 终点落在切不开的段内（生僻组合词「被禁锢」）。
   这是对旧「裸未解释字母数硬首键」的结构性修正——当年生僻组合词霸占第一页；
2. **拼写来源层序**：精确 → 模糊 → 简拼（同桶内保持结构优先，防止上千条
   `xx` 简拼候选冲垮全拼第一页）；
3. **分数 − 覆盖罚分**：`COVERAGE_PENALTY × 段内未解释字母数`，与词频/LM 同量纲竞争；
4. 频次降序、词面字典序（确定性收尾，同输入两次查询逐字节一致）。

**性能细节（可复用的模式）**：排序键**预计算**成 `PlainKey`（避免 sort_by 比较器里
反复 O(拼音长) 过滤），排序只排**下标**、按置换环就地 swap——零克隆（500 条候选
排序曾因此 0.99→1.23ms，改回 1.0ms）。

### 2.5 词库：mmap 只读二进制 + 二分，SQLite 只当构建中间格式

`dict/mmap.rs`（主索引）、`dict/abbrev.rs`（简拼副索引）、`dict/syllable_graph.rs`
（音节转移索引）。两个索引文件**同构**：52 字节头（魔数/版本/计数/三区偏移）
+ records 区（定长 12B 记录）+ strings 区 + entries 区；主索引是 60 字节头
（尾部多一个全表词频总和，见 `data/README.md`）。

- **为什么 mmap**：启动 O(1) 映射，查询 = 内存上二分 + 切片；零 SQL、零锁、
  无逐击键磁盘 IO；文件只读 → 天然线程安全，不需要 Mutex。
- **总频必须预计算**：`P(词) = freq / 总频` 的分母过去是启动时线性扫词条区
  求出来的，为拿一个 `u64` 把整段词条区读进 page cache（实测把 `dict.bin`
  常驻从 ~30MB 抬到 63MB）。v2 头部直接带这个数，`scan_total_freq` 只剩
  v1 旧文件的兼容路径。
- **mmap 一律 `MADV_RANDOM`**（`src/mmap_advice.rs`）：二分与 trie 走子节点
  都是跨页随机跳，内核默认策略把常驻放大的两个来源（64KB fault-around +
  64KB large folio）在随机访问下几乎全是浪费。冷页实测：`.gram` 5000 次
  随机页读 203MB → 107MB，`dict.bin` 2000 次 57.8MB → 7.6MB。
  **量这一项必须冷页**（先 `POSIX_FADV_DONTNEED`）——页缓存热时两种策略
  读数一模一样，据此判「madvise 没用」是错的。
- **构建链**：Rime 词表（雾凇/腾讯词向量）→ `scripts/build_dict.py` → dict.sqlite3
  → `scripts/dict_to_bin.py` → dict.bin；`scripts/abbrev_to_bin.py` 从 dict.bin
  **只读**生成 abbrev.bin。
- **同代校验**：副索引关联数必须等于主索引词条数、音节图记录数必须相等，否则
  **拒绝装配**只告警——拿错代索引会静默给出错候选，比功能缺失更糟。
- **降级哲学**：文件缺失/损坏一律 warning + 降级，绝不 panic——输入法起不来
  比模糊音没开严重得多。

### 2.6 简拼与混合简拼：副索引 + 交错 DFS

`dict/abbrev.rs` + `dict/mixed.rs`。三条读法路径：

- **纯简拼** `zgr`：副索引二分直查（首字母串→词条）；
- **混合简拼** `feicde`、`nhao`：**交错 DFS**——每一位要么吃字面音节、要么吃一个
  首字母，探索预算由上面的常量钳制；zh/ch/sh 折叠成整体声母时带**真值校验**
  （`VariantRead.digraphs`——`是(shi)` 和 `三(san)` 的 key 都是 `s`，光看 key 分不清）；
- **整串条目边**：`生日快乐` 这类「缩写展开恰好命中词条」的读法，作为位置 0 的一条
  边直接进格（20 条纯缩写输入里 18 条的正确答案属于这一类）。

### 2.7 语言模型：一个 trait，三个实现

`lm/mod.rs`（trait + BigramLM + MixedLM）、`wanxiang/mod.rs`（万象 .gram）。

```
LanguageModel trait（KenLM 风格状态化打分）
  score(state, word) → (log10 概率, 新状态)
  State: Clone  ← beam 分叉时每条路径复制一份，沿路径传递
```

| 实现 | 数据 | 原理 |
|---|---|---|
| `WanxiangLM`（首选） | 万象 `.gram`，约 420MB | Rime octagram **字符级窗口模型**：上文末 ≤5 字 + 词首 ≤5 字拼成键，在 darts-clone double-array trie 上查询；mmap 只读，不进堆 |
| `BigramLM`（回落） | `bigram.txt` 文本 | 纯内存 HashMap 词对计数 |
| `MixedLM`（混合层） | 上面任一 + 用户模型 | 通用层分 + 用户层计数增益，log10 域线性叠加 |

万象的关键细节（`wanxiang/mod.rs` 模块文档有完整推导）：
- 存储值 `v = ln(freq)×10000`，换算 `log10 P = (v/10000 + penalty_ln)/ln10 − log10_corpus`；
- `log10_corpus_size = 9` 是**占位常数**（语料规模读不出来）——只整体平移不改排序；
- 上游惩罚（collocation −14 / non_collocation −6）原样搬过来当起点，**未做评测调参**。

**已知未修的排序问题**：裸 `xian` 时「西安」排第 18。根因已诊断：空上下文下
万象返回常数（见 `examples/lm_ctx_probe.rs`），两义切分没有语境可用。
README TODO 里写明：**跨候选归一化是唯一未试的方向**。

### 2.8 用户学习：只提供计数，不提供概率

`user_lm/mod.rs`。上屏即学：`(上一个词, 当前词)` 的二元计数 + 一元计数。

- **历史事故（必读注释）**：旧实现把用户层当概率模型按用户语料归一化，结果
  「学过 3 次的词算出 -1.64，比没学过的中性分 -1.0 还低」——学习反而降权，且
  加分随打字量漂移 4 个数量级。现行口径：混合层把计数换算成**加法项**
  `log10(1 + c·R)`，与语料规模无关，0 次逐位回退通用层，学习永不降权。
- **并发**：计数表 `RwLock`，热路径不加锁——每次查询开始取一次 `Arc` 快照
  （`UserLmSnapshot`），内置进 `LmState` 沿 beam 路径传递。「一次查询一代」，
  并发学习不会让同一次查询前后看到不同的表。
- **持久化**：文本格式（bigram.txt 同款），64 次学习批量 flush + 临时文件原子
  rename，Ctrl+C 兜底；超 2 万条一元词条触发**全局减半衰减**（频次右移一位，
  保相对比例）而不是 LRU——按条淘汰会把「常用但条目多」的用户整个清出去。
- **debug/release 数据隔离**：debug 构建默认写 `user_bigram.dev.txt`，
  开发时打的字不会混进真实用户模型。

### 2.9 造词：把「用户逐字选出的短语」变成词条

`session/mod.rs` 的 `COIN_*` 常量区（注释是项目里最好读的设计文档之一）。

- 判据：造词段的**每一个片段都是单字**（逐字组词）。第一版是「存在单字片段」，
  被真实用户数据打脸——「把候选当空格用」的连续打字会把整句粘成词条；
- 已知残留局限（如实记录在注释里）：全部用单字连续打字组句仍会误造，因为
  「逐字组词」和「逐字打句」在片段形状上**完全同构**，判据层面分不开；
- 上限：单段 ≤24 字、≥2 片段才造、词条数 ≤1 万（同一减半衰减）。

### 2.10 容错/纠错：噪声信道模型 + 结构性触发门槛

**全拼**（`engine/lattice/build.rs` 容错格 + `segmenter.rs` 的 `TypoSyllables`）：
- ≤1 次编辑索引：替换（混淆对）/漏打/多打/调换四类，都能落回规范音节；
- 「空词弧」：吃掉 1~2 个无法解释的字母，扣编辑惩罚；
- **触发门槛是结构性的**（不是模糊匹配）：精确路径解不出整串、或整串切不开、
  或输入有叠字母（拼音没有叠字母，出现即多打）。打对的输入**零开销、零新边**；
- 代价按**噪声信道**进对数域：键盘同行邻键 0.05、同列 0.02（按 MS Pinyin 论文
  的 43%/15% 错误分布折算）、无关替换 2.0，不是简单一票否决。

**简拼**（`engine/abbrev_correct.rs`）：难点不同——任何辅音串都有别的读法，
「何时可纠」没有结构信号。方案 = **证据闸 + 渐进披露 + 分数闸**：
原串整串候选全无词条级证据才参与；先便宜的副索引二分，有证据的变体 ≤2 条
才升级贵的整串解码；呈现时编辑代价扣分 + 分数闸（领先原串 ≥1 个数量级）+ 配额 2。

### 2.11 会话状态机：按键流 → 上屏/刷新

`session/`（mod + input + commit 三块）。`InputSession` 把 `InputEvent` 翻译成
三种输出：`Commit`（上屏）/ `Updated`（刷候选）/ `Noop`（还给应用）。

- 可编辑缓冲：左右光标移动、光标处插入/退格；
- **分段上屏**：选只覆盖前缀的候选时，前缀收进「已确认段」，剩余串继续编辑——
  记账（造词/学习/上文）与状态变更**拆开**，刷新失败可整体回滚；
- `'` 音节分隔符：`NormalizedInput` 把原文归一化成「干净串 + 边界表」，三套坐标
  （原文/干净串/候选拼音）一处收口；格构建、beam、排序全程「不许跨 `'`」；
- 中英切换是引擎自己的会话状态（Ctrl+空格），英文模式字母原样透传。

### 2.12 邮箱/网址识别：手写匹配，不引正则库

`recognizer.rs`。librime `recognizer` 组件的最小等价物（email/url 两类 pattern），
手写匹配器替代 regex crate（「不许新增依赖」是硬约束）。**纯函数、整串重判**：
每敲一个字符都重新判定，「识别态」由会话层持有，回溯天然成立
（`abc@` 删成 `abc` 自动退出识别态）。

---

## 3. IBus 集成（xwx_ibus）

`engine.rs` + `factory.rs` + `ibus_proto.rs`。

- **zbus 4**（纯 Rust D-Bus）+ **zvariant** 的 GVariant 序列化；
- `ibus_proto.rs` 手写 GVariant 构造：IBus 对 D-Bus 类型串与字段顺序**严格敏感**，
  测试里有「与 ibus 1.5.29 类型串逐字对照」的断言——改协议前先跑它；
- 每个输入焦点一个 `XwxEngineServer` 实例（各持一个 `InputSession`），
  引擎本体 `Arc<ImeEngine>` 全进程共享；会话用 `tokio::sync::Mutex` 串行化按键；
- **不自绘候选窗**：发 `UpdateLookupTable`/`UpdatePreeditText` 信号，GNOME Shell
  的 candidate popup 负责渲染；用户翻页/点击由 daemon 折回调本接口方法；
- **组件注册双通道**：运行时 `RegisterComponent` + 安装期 component XML。XML 必须
  进 `/usr/share/ibus/component/`（sudo）——IBus 1.5.x 把「扫用户目录组件」的代码
  `#if 0` 掉了（上游 issue #2754 closed as not planned），这是硬约束不是偷懒；
- 已知冲突：fcitx5 启动时伪装占用 `org.freedesktop.IBus` 总线名，必须先停干净。

---

## 4. 测试体系（改动前先看这节）

当前基线 **290 passed / 0 failed / 1 ignored** = 241 单测 + 49 D-Bus 集成。

**三层测试，别把上层当双层的证据**：

| 层 | 位置 | 覆盖 |
|---|---|---|
| 单测 | 各 `mod tests` | 算法与状态机，最快最多 |
| D-Bus 集成 | 根 `tests/`（11 个文件） | **真实 `xwx_ime --ibus` 进程 + 私有 dbus-daemon**，覆盖单测结构上摸不到的路径（透传、信号内容、焦点生命周期、封闭性） |
| 实机 | `docs/VERIFY.md`（37 条清单） | IBus daemon + 真实应用，未自动化，**当前可信度最低的一环** |

**集成层的纪律**（README 里有专门一段，写给它自己踩过的坑）：
「没收到信号」和「断言不成立」必须能分开；不要用 `let Ok(x) = .. else { continue }`
静默吞解析失败；每个 D-Bus 调用必须有超时。

**对拍体系**（质量回归的护栏）：
- `tools/rime_oracle/`：librime 驱动的差分对拍台——用成熟输入法当 oracle；
- `tools/compare.py`：对拍指标；`docs/VERIFY.md` 存逐字节基线（90 段首位变化 0）；
- 改排序/解码后跑对拍，防止行为静默漂移。

**基准工具**（全在 `crates/xwx_engine/examples/`，一律 `--release`）：
`lookup_bench`（端到端击键延迟）、`abbrev_bench`、`decode_bench`、`fuzzy_bench`、
`lm_ctx_probe`（诊断空上下文 LM）、`probe`（对拍探针）。

---

## 5. 想优化时去哪改（按场景速查）

| 你想优化的 | 去哪 | 已知结论/坑 |
|---|---|---|
| **排序质量**（候选顺序不对） | `engine/rank.rs` 排序键；`DEFAULT_LM_WEIGHT` | 裸 `xian` 问题：空上下文 LM 是常数，跨候选归一化是唯一未试方向 |
| **查询延迟** | `engine/lattice/build.rs` 配额常量、`MAX_CANDIDATES` | 512→1024 试过并回退（召回 +1.2pp，但 `zh` 从 9.3→12.1ms 顶穿预算）；所有常量改动重跑 bench 且必须 release |
| **内存占用** | `src/mmap_advice.rs`（访问模式）、`dict_to_bin.py`（头部字段） | 常驻大头是只读 mmap 的文件页（可回收，不是泄漏）：anon 只有 ~15MB。别用「改成 `read()` 进堆」来省——那会让可回收页变成不可回收的 anon |
| **新增配置项**（现在全是硬编码/环境变量） | TODO 表里的「配置化」：页大小、模糊音按组开关、词库路径 | 目前只有 `XWX_FUZZY` 一个开关；`DEFAULT_FUZZY_ENABLED` 的注释是「默认值怎么定」的方法论范本 |
| **LM 效果** | `wanxiang/mod.rs` 的换算常数与惩罚 | `log10_corpus_size=9` 占位、句尾哨兵键未实现、惩罚未调参——三件事都在等评测集 |
| **模糊拼音** | `corrector.rs` 混淆对 + `DEFAULT_FUZZY_ENABLED` | 默认关是实测裁决（长串 6.48x 超预算）；数据一变结论可能翻转，重跑 `fuzzy_bench` |
| **用户学习行为** | `user_lm/mod.rs`、`lm/mod.rs` 的 `USER_COUNT_GAIN_R` | 用户层只回计数不回概率——别改回概率归一化（踩过坑） |
| **造词误造** | `session/mod.rs` 的 `COIN_ALL_SEGMENTS_SINGLE_CHAR` | 残留局限有记录；彻底解决需要用户确认 UI 或重打观察 |
| **词库格式** | `dict/mmap.rs` + `data/README.md` 格式文档 + `scripts/dict_to_bin.py` | 改格式要同步动版本号/魔数校验、两个生成脚本和同代校验逻辑 |
| **IBus 协议** | `xwx_ibus/ibus_proto.rs` | 先跑「与 1.5.29 类型串逐字对照」的测试 |
| **集成测试基建** | 根 `tests/` | 先读 README「集成层的正确性要求比单测更严」那段再写新用例 |

**改任何东西之前的通用流程**（项目自己沉淀的）：
1. `just verify`（check + test 一把过）+ `cargo clippy -D warnings` 全绿；
2. 行为变化跑对拍（rime_oracle + VERIFY.md 基线），性能变化跑 bench（release）；
3. 常量调参时把**实测数据写回注释**——这个项目的注释之所以可信，
   就是因为每个数字都带着当时怎么量出来的。
