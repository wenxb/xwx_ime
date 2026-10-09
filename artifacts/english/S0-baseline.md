# S0 基线资产（英文输入任务）

日期：2026-10-09。本节只记录**改动前**实测，不是目标值。

## 1. 命令与退出码

| 命令 | 退出码 | 产物 |
|---|---:|---|
| `git status --short` | 0 | `S0-git-status.txt`（3 个未跟踪 docs 条目，无改动） |
| `git diff --stat` | 0 | `S0-git-diff-stat.txt`（空） |
| `cargo test --workspace --offline` | 0 | `S0-cargo-test.txt`（**419 passed / 0 failed**） |
| `cargo clippy --workspace --all-targets --offline -- -D warnings` | 0 | `S0-cargo-clippy.txt`（0 warning） |

已有失败：**无**。基线全绿。

## 2. 中文排序基线（release，真实词库 + 万象 gram）

装配：`XWX_DICT_PATH=data/dict.bin`、`XWX_ABBREV_PATH=data/abbrev.bin`、
`XWX_LM_PATH=$HOME/.local/share/xwx/wanxiang-lts-zh-hans.gram`、
`syllable_graph.bin` 与 abbrev 同目录自动装配。

命令见 `S0-eval-rank.txt`。`eval_rank` 输出**召回 / MRR / 首屏(前 9)**，
它不直接输出 Top-1 表；下表的 Top-1 由「MRR = 1.0 ⇔ 全部首位命中」推得，
不做改名冒充。

| 语料 | 条数 | 召回 | MRR | 首屏(前 9) | 平均选择 |
|---|---:|---|---:|---|---:|
| daily.jsonl（全拼日常） | 12 | 12/12 | 1.000 | 12/12 | 1.00 |
| mixed.jsonl（简拼/混拼） | 14 | 13/14 | 0.929 | 13/14 | 0.93 |
| edits.jsonl（打错/半成品） | 6 | 6/6 | 0.841 | 5/6 | 1.17 |
| keystrokes.jsonl（逐键） | 12 | 12/12 | 0.877 | 11/12 | 1.00 |
| tuning.jsonl（调参集） | 5 | 5/5 | 1.000 | 5/5 | 1.00 |

已知未召回（基线即存在，不得隐藏）：
`mixed.jsonl` 的 `jintqbj → 今天请病假`，实际前 3 为
`["今天去北京", "今天去北京"...]`（原文：`["今天去北京", "今天起北京", "今天骑兵军"]`）。

**Top-1 口径补齐**：新增 `examples/english_eval` 时同时补一个中文 Top-1 计数，
不从 `eval_rank` 的首屏指标改名。

## 3. 超长文件与拆分边界（本任务触及的文件）

| 文件 | 2026-10-09 行数 | 拆分动作（S1） |
|---|---:|---|
| `crates/xwx_engine/src/engine/lookup.rs` | 951 | 词条证据/格候选/句子辅助 → `lookup_scoring.rs`；整句追加块 → `lookup_sentence.rs` |
| `crates/xwx_engine/src/engine/rank.rs` | 597 | **不动**（本任务不往这里加英文逻辑） |
| `crates/xwx_engine/src/engine/mod.rs` | 578 | 引擎聚合结构 → `engine/types.rs` |
| `crates/xwx_engine/src/session/input.rs` | 595 | `refresh`/`insert_recognized_char` → `session/refresh.rs`；`handle` 拆薄包装 + `handle_composition` |
| `crates/xwx_ibus/src/engine.rs` | 542 | UI 辅助（commit/commit_text_only/refresh_ui/hide_ui）→ `engine/ui.rs` |

硬门槛：任何改动后的文件 ≤ 600 行，不用 `include!` 拼接、不压行。

## 4. 验收语料现状

`tests/corpora/` 已有 daily / mixed / edits / keystrokes / tuning 五套中文语料。
`mixed.jsonl` 指**简拼/混拼**，不是中英混输语料——新英文语料另建，
计划为 `english_tuning.jsonl`(≥60) 与 `english_acceptance.jsonl`(≥120)。

`tests/corpus.rs` 用的是字符串截取式解析器；新英文语料必须走 `serde_json`，
不复用它。

## 5. 尚未完成（S0 遗留）

- release 冷启动与稳态逐键 p50/p95/max 还未测（`examples/perf_probe` 基线）
  —— 排到 S6 与英文性能探针一起测，避免重复占用 release 构建时间。
- 真实用户统计不参与基线；用户层一律内存实例（本基线用 `LmContext::empty()`）。
