# 接续说明（xwx_ime，2026-10-03 收尾时写）

## 目标（本会话的 objective，仍 active）
按 codex 的分阶段方案把简拼/整串的「召回-匹配-搜索-投影」收敛成统一架构：
阶段 0 补失效证据 → 1 RecallHit 成为真实数据 → 2 统一词匹配（先落 A：单缩写音节 +
字面尾链的可穷尽约束匹配）→ 3 统一路径搜索（先落 B：删掉展示表对调度的否决）→
4 展示投影与全局接纳 → 5 删旧通道各自独立语义。

## 已完成
阶段 0、1、2（含 A、含 2B 的 `dict/syllable_graph.rs` 音节转移索引）、3（含 B 与投影规则）、
4 全部落地，各有提交与文档。此外修掉一批实测缺口：叠字母纠错（shanghaai→上海）、
末尾半音节补全进格子（nihaobeijin→你好北京、womenjintianqugongyua→我们今天去公园）、
投影层填充档判断改按词串口径（**daily MRR 0.958→1.000**）、儿化音 r 别名
（zaiyikuair→在一块儿）、尾补全（daishix→待实现）。

## 当前状态（接手时先核对）
- HEAD `784b5e4` 之后若有新提交以 `git log --oneline -3` 为准；工作区应干净，
  只剩未跟踪的 `data/syllable_graph.bin`（阶段 2B 生成的可选索引，功能等价回退；入库或 .gitignore 待用户定）。
- 门槛基线：`cargo test --workspace` **401 passed / 0 failed**（单元 327）、clippy 0、
  `regression_probe --format v1` 对拍 44 输入 0 变化、perf 需同条件 A/B（本机 load 12~18，12ms 线对两版都不可达）。
- 语料：daily 12/12（MRR **1.000**）、mixed 13/14（余下 `jintqbj` 是模型判断，保留人工期望）、
  keystrokes 12/12（0.877）、edits 6/6（0.728）。

## 下一步（唯一未完成项，规格已写死）
阶段 3 的「按类整层切换」第一类 = **纯缩写输入**。施工图与实现规格见
`docs/HANDOFF.md` 的 **⑤㉚**（含三次附录：第 1 步已执行的基线/成本实测、第 2 步前置实验、
第 2 步实现规格）。要点：
1. 现状：`lookup_abbrev` 是这类输入唯一有效来源（关掉后 20 条只剩 2 条正确）；
   整句层产的候选是 `Exact`，不是简拼层要的单词条 `Abbrev`；`reachable == false`
   的纯缩写输入（wsm/zgr/xxn/dbq/mgx）连简拼格都不建。
2. 做法：判定纯缩写输入 → **只对这一类**放宽建格门槛 → 从格上抽候选
   （标 `Abbrev`、`covered = 拼串长度`、分数用 `decode.rs` 边分公式）→ 新通道常量开关、
   旧通道保留作回退点、(word,pinyin) 并集去重。
3. 验收：20 条纯缩写清单逐条点名（基线 `/tmp/xwx/abbrev_inputs.txt`、
   `/tmp/xwx/probe_abbrev_before.txt`；注意 /tmp 可能被清，必要时重跑 recall_trace 重建）；
   四套语料不得低于上面数值；ztai/ztailan/wmjtqbj/womenjtqbj/zmgai/zguor 不退化；
   perf 同条件 A/B 不可区分（wmjtqbj 的简拼格已 6.167ms）；clippy 0；非纯缩写部分对拍 0 变化。
4. 做完之后再谈阶段 5（删旧通道各自独立语义）。

## 硬约束（每次都要过）
`.cargo/config.toml` 固定 target-dir=/mnt/hdd/target/xwx_ime（勿覆盖 CARGO_TARGET_DIR）；
clippy `-D warnings` 全绿；单文件 ≤600 行；AGENTS.md 禁止 pkill（用
`for pid in $(pgrep -x 名); do kill "$pid"; done`）；不动 ~/.local/share/xwx/；
说中文、注释简明；诊断代码默认关闭；**已跑通的行为不许退化**（对拍差异逐条点名）。

## 教训（别重复踩）
- **打分口径只能整层改**：四次局部尝试（投影层守卫、投影层词串口径、全局相对基线、
  简拼层落底换口径）都会破坏层内可比性，证据在 HANDOFF ⑤㉔~⑤㉚。
- 「填充档词条」不等于「差读法」：`FILLER_FREQ_TIER` 只说明词频缺失，
  真短语（早点休息/我马上到）就在这一档里。
- 词面必须取候选自己的汉字：只按拼音切词会让 key 相同的候选比较退化恒等。
- 万象 `.gram` 对个别 (上文,词) 读出 ≈2e9 垃圾值：扫 200 配对只 1 例，属**模型数据异常**，
  现有「折成兜底分」处理正确，不必改代码（卡 t-mur4twht-j2pd2j 已结案）。
- 委托产物必须自己重验；诊断代码用完即删；回退源码后必须重建再测。
