# 打字热路径等价优化验收

## 范围与边界

基线是优化开始前的完整工作区（包含当时已有改动），不是 Git HEAD。
本轮不改变候选数量、搜索预算、剪枝规则、排序键或分段确认规则。
不改系统输入法安装和用户数据。

已落实的改动：

| 路径 | 改动与等价条件 |
|---|---|
| `crates/xwx_engine/src/candidate_index.rs`、`engine/lookup_scoring.rs`、`dict/lookup.rs` | 两处线性查重加辅助索引，结果仍存原 `Vec`；首现优先、替换字段及截断时点不变，碰撞核对原字符串。 |
| `crates/xwx_engine/src/dict/mmap.rs` | `entry_freq` 借用词块取首个同词频率，避免生成整批候选；命中后仍校验完整词块。评分证据仍来自主库，不直接相信副索引的非零频率。 |
| `crates/xwx_engine/src/engine/lookup_scoring.rs` | 格边去重键借用字符串；用 `entry` 避免复制候选再替换；保留最高分与同分先到规则。 |
| `crates/xwx_engine/src/dict/top.rs`、`dict/lookup.rs` | 混拼池以原全序局部选择前 500，再排序；去重键已唯一，不改变同频边界结果。`seen.insert` 合并查重与插入。 |
| `crates/xwx_engine/src/engine/rank/compact.rs`、`rank/coverage.rs`、`rank/mod.rs` | 不再分配去空格的临时拼音字符串；UTF-8 字节比较与原 `String` 比较等价。 |
| `crates/xwx_engine/src/engine/decode.rs` | 复用最长可切分前缀已经算出的切法；超编辑预算的 beam 边先拒绝，再算 LM。 |
| `crates/xwx_engine/src/engine/lookup_scoring.rs` | 尾声母分支先判断廉价后缀条件，仍用原带预算的切分器。 |
| `crates/xwx_engine/src/english/recall.rs` | 静态前缀复用已取得的词条；用户来源保留 `exact` 校验；短串不读必被门槛拒绝的补全池；上文行每查询懒读一次。 |
| `crates/xwx_engine/src/lm/prepared.rs`、`wanxiang/scoring.rs` | 同一状态批量评分复用上文编码与 trie 节点，不改词侧查询、命中次序与浮点运算顺序；大窗口走原评分路径。beam 状态推进不改。 |
| `crates/xwx_engine/src/user_lm/mod.rs`、`user_lm/file.rs` | 空屏蔽表快退；序列化完成后释放快照，再做磁盘同步，缩短并发学习的整表写时复制窗口。 |

拒绝或撤回：用布尔 DP 全局替换枚举切分、直接用非零候选词频跳过主库校验、beam 状态结构重构、固定 512 项栈容量、展示折叠的新哈希索引及其 32/64 阈值版本。展示折叠的新索引实际引入回退，已经撤掉；随后单独验证标准库 `entry` 写法，完整输出仍一致，但性能未通过验收，也未采用。带来源的召回折叠保留 `entry` 小改动。

## 等价验证

`crates/xwx_engine/examples/equivalence_probe.rs` 对中文与中英混合查询打印全部候选字段，另外打印 `score.to_bits()` 和 `matched_len`，避免十进制舍入及序列化跳过字段漏检。

覆盖 35 个输入、空/非空中文上文、空/非空英文上文，以及无学习、学习与屏蔽、学习加模糊音三种配置。另有逐键会话、光标编辑、退格、翻页、上屏与确认脚本。

基线和修正版输出共 **124,951 行**，逐字节相同，SHA-256：

```text
10ccf0c2dec16f9a58399c4930dee278ee808de9c87fb81c024cb2d59f21b88b
```

原有 `regression_probe --format v2` 的 **5,395 行**输出也一致：比对时只移除 ` min_ms=...` 字段，保留同行的候选数量。稳定输出 SHA-256 为 `a03da6ee6f1aa8726448441aa8c6ab758060e7f36ff00b9355f93893ef539cac`。

新增单测另覆盖：哈希碰撞与首现匹配、覆盖相等/更大的折叠规则、旧副索引频率、零频重复词条、命中后词块损坏、500 项截断边界、Unicode 拼音比较、带预算尾声母判断、英文旧召回 oracle、LM 位级分数和快照代际、并发学习/落盘。

全工作区测试：**611 项通过，0 失败，2 项既有忽略**；包含私有 D-Bus 集成测试，不代表真实桌面应用验收。`cargo clippy --workspace --all-targets --offline -- -D warnings` 已通过。

## 性能口径与已完成结果

release、真实数据、同核串行六轮（before/after/after/before/after/before），汇总各版本三轮分位数的中位数。查询/会话调用内部计时，格式化和写文件不计入。不是 D-Bus 或 GNOME 候选窗的端到端延迟。

逐键使用现有 `perf_probe`，每轮 `--repeat 45`。16 组场景 P95 均下降，各轮通过既有 12ms P95 门槛。代表结果，单位 ms：

| 逐键脚本 | 原 P95 | 修正版 P95 | 降幅 |
|---|---:|---:|---:|
| 全拼 `nihao` | 0.541 | 0.465 | 14.0% |
| 全拼 `weishenme` | 4.621 | 3.746 | 18.9% |
| 长全拼 `womenjintianquxian` | 6.373 | 6.175 | 3.1% |
| 混拼 `zhongg` | 9.191 | 4.915 | 46.5% |
| 混拼 `jint` | 4.727 | 2.275 | 51.9% |
| 分隔符 `xi'anq` | 3.272 | 1.931 | 41.0% |

最终源码重新构建后，逐键复核共 **5,609 个稳态样本**：整体 P50 **0.307ms**、P95 **4.346ms**、max **10.313ms**，超过 12ms 的样本 **0 个**。最终全字段对拍也再次通过。

另外量了 78 组完整查询配置。空学习、非模糊、空中文上文时，`zh` 的 P50 为 8.2961 → 4.3877ms，`xx` 为 3.8846 → 2.2414ms。它们不是逐键延迟。

对少数尾部上扬的查询加测八轮，每轮 200 样本。`nihao`、`devel` 的 P50/P95 均改善。模糊音、空中文上文的长查询仍不能宣称严格单调改善：

| 查询 | 原 P50 | 修正版 P50 | 原 P95 | 修正版 P95 |
|---|---:|---:|---:|---:|
| `womenjintianquxian` | 14.9517 | 14.9561 | 17.0467 | 17.5325 |
| `world` | 59.4772 | 59.2786 | 65.6056 | 65.8935 |

这些尾部波动保留原样，不当作收益，也不能据此保证所有输入、负载和机器均只快不慢。

## 重跑

```sh
cargo test --workspace --offline
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo build --release --offline -p xwx_engine --examples
# 二进制目录以 cargo metadata 的 target_directory 为准。
<target_directory>/release/examples/equivalence_probe data > equivalence.txt
<target_directory>/release/examples/perf_probe --data data --repeat 45
```

本轮临时基线、双版本二进制、原始样本及 A/B 脚本保存在 `/tmp/xwx-perf-safe-before` 与 `/tmp/xwx-perf-results`，系统清理 `/tmp` 后不保证留存。真实桌面 IBus/应用/候选窗协作仍需按 `docs/VERIFY.md` 人工验收。

## 2026-10-28：积压保序与格构建分配优化

### 保序修复

旧路径由 zbus 为每次方法调用并发起任务。会话互斥锁只能串行处理已经到达锁的
任务，不能还原消息接收顺序；处理变慢、请求积压后，预编辑、上屏和回复都可能乱序。
降低单键耗时只能减少触发机会，不能修正这个顺序错误。

`XwxEngineServer::register` 为 Engine 和 `org.freedesktop.DBus.Properties`
共同使用禁止并发派发的 `Ordered<T>`，在接收循环中完成一次调用及其输出、回复，
再处理后续调用。Properties 也必须纳入：密码框的 `ContentType` 不能越过此前按键。
保留 zbus 原有属性查找、错误和 introspection；启动预导出实例和 Factory 后续实例
均经同一注册入口，在服务名发布或对象路径返回前完成注册。

`tests/queued_input.rs` 暂停私有引擎，连续发送消息后恢复，验证：

- 两段输入的每次预编辑、最终上屏与方法回复保持发送顺序。
- 积压中的 `Reset`、`FocusOut`、`Disable` 仍正确分隔会话。
- 密码框切换丢弃旧组合、敏感按键透传、恢复普通输入后不混入敏感字符。

### 等价热路径优化

`engine/lattice/build.rs` 的音节链 DFS 拆到 `engine/lattice/chain.rs`，保持单文件
不超过 600 行。只删去可避免的中间对象：窗口长度直接迭代；关闭模糊音时借用
已有 key 缓冲；缓存候选直接借用，仅复制最终格边持有的词与拼音；工作队列直接
遍历边的终点。不改枚举次序、预算、候选集合、评分、排序或截断。

基线为本轮优化前的当前源码，包含已有中英排序改动；不是旧编译产物或 Git HEAD。
重新构建两版 `equivalence_probe`，全部候选字段、分数位模式和会话输出共
**124,953 行**逐字节一致，SHA-256：

```text
98ae6fd04631fa96655ec7cba4da858be1061a2a9194e7e73e8def552a39320b
```

release、真实数据、固定 CPU 2，同核串行六轮
before/after/after/before/after/before；每轮 `perf_probe --data data --repeat 45`，
5,609 个稳态样本。以下为各版本三轮 P95 的中位数，单位 ms：

| 逐键场景 | 优化前 P95 | 优化后 P95 | 降幅 |
|---|---:|---:|---:|
| 全部场景合计 | 3.977 | 3.198 | 19.6% |
| 全拼 `nihao` | 0.564 | 0.453 | 19.7% |
| 全拼 `weishenme` | 3.586 | 2.840 | 20.8% |
| 长全拼 `womenjintianquxian` | 6.950 | 5.628 | 19.0% |
| 混拼 `zhongg` | 5.650 | 4.230 | 25.1% |
| 混拼 `jint` | 2.412 | 1.840 | 23.7% |
| 混拼 `womenjintqubeij` | 5.565 | 4.604 | 17.3% |

16 组场景的 P95 中位数均下降；该计时覆盖 `InputSession::handle`，不包含
D-Bus、应用响应或桌面候选窗渲染，不代表高负载时的最坏延迟上界。

临时全局分配计数器另测预热后单次完整中英查询（不是单次按键）：
`womenjintqubeij` 的分配次数从 30,886 降至 23,493；8 个被测输入均减少。
计数探针和临时基线模块已移除，不引入生产计数开销。

### 最终源码验收

- 全工作区测试：**623 passed、2 ignored**；新增积压回归 3 项通过。
  `cargo clippy --workspace --all-targets --offline -- -D warnings` 通过。
- 最终 release 重新构建后，全字段对拍仍为上述 124,953 行及同一 SHA-256。
- 最终 release 单独复核 5,609 个稳态逐键样本：P50 **0.275ms**、P95
  **3.573ms**、max **6.332ms**，超过 12ms 的样本为 **0**。
  该轮是独立复核，不替换前述六轮 A/B 中位数。
- 私有总线加载真实中英词库、万象 gram 和英文二元表；暂停引擎形成积压后恢复，
  三段长输入的预编辑、Enter 原文上屏和回复均按顺序。
- 实际调用 Factory 创建首个与第二个实例；两条对象路径均验证
  `nihao` 空格上屏“你好”、三种会话边界、密码框切换，以及各自 62 个顺序回复。
- 未安装或重启桌面输入法，未改用户配置与学习数据；真实应用和候选窗渲染
  不在本次私有总线验收范围内。

## 2026-11-01：整串简拼同分重排

`limq` 的“里面去”原排第 12；它与前面九个人名都使用填充词频 100，
主分同为 −7.883139610290527，原先按词面顺序打破平局。

在简拼格的最终排序中，仅重排原数值键、词频完全相同，且恰好覆盖整串的
`Abbrev` 填充词条（freq = 100）。内部证据取万象模型各字符切分点的最大搭配增益，
不累计切分点分数，不改候选主分。混合模型仅取通用万象层，避免把用户学过的
片段当作整词证据；没有字符模型时保持原序。只重排符合条件的候选位，前缀候选、
其他层及不同主分的候选保持原有优先级。评分借用词面，不为切分点分配字符串。

真实 dict + abbrev + syllable_graph + 万象 gram，内存用户层与英文通道验证：

- `limq`：第 12 → 第 3，首屏可见，数字键上屏“里面去”；“黎明前”“利民桥”仍居前。
- `limianq` 与 `limianqu` 仍为首选“里面去”。20 种输入分别在空上文与“往”下
  共 40 次全候选对照，候选集合、主分、词频及覆盖范围均不变；12 次查询的
  填充档平局顺序改变，其余 28 次全表不变。
- 固定 CPU 2、release、预热后各 150 次逐键输入：`limq` 每版 600 个键位样本，
  P95 为 1.364ms → 1.357ms；单轮计时，不据此推断微秒级增减。
- 新增 5 项回归覆盖首屏选词上屏、主分/词频/层序/覆盖范围边界、前缀候选保位，
  以及无字符模型时的原序。全工作区 628 passed、2 ignored。
- `cargo clippy --workspace --all-targets --offline -- -D warnings` 与
  `cargo build --release --offline -p xwx_ime` 通过。
  `perf_probe --data data --repeat 15 --prewarm` 固定 CPU 2，共 1,859 个稳态样本：
  总体 P95 3.977ms；16 个场景的 P95 均低于 12ms。另有 4 次单键超过 12ms，
  max 27.503ms；该轮不能证明最坏延迟低于 12ms。计时不含 D-Bus 与桌面渲染。
