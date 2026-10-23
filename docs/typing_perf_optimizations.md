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
