# M2 验收报告（下一词预测）

日期：2026-10-09。范围：`docs/english-input/04-prediction.md` 与分册 05 的 S7-S9。
M1 报告见 [`S6-m1-report.md`](S6-m1-report.md)。

## 1. 交付状态

| 能力 | 状态 |
|---|---|
| `english_bigram.bin` 构建脚本（SymSpell 固定提交） | ✅ 241,616 词对 → 16,315 context / 114,890 edge / 2.36 MB |
| Rust reader（加载即全量校验、失败只降级） | ✅ `crates/xwx_engine/src/english/bigram.rs` |
| 英文条件排序（`p = 0.8c/T + 0.2p0`） | ✅ 无该行时**逐位**退回 M1 的 `z` |
| 静态上文 Top-32 作为第 5 个前缀召回来源 | ✅ 去重前上界 104 |
| 零输入下一词召回（静态 ∪ 用户，最多 5 条） | ✅ 独立入口 `EnglishEngine::next_candidates` |
| 预测态（`CandidateMode` 的不变量、按键表 20 行） | ✅ `crates/xwx_engine/src/session/prediction.rs` |
| `SessionOutput::{CommitAndUpdate, RefreshAndForward}` + IBus 映射 | ✅ 不调会 `hide_ui` 的 `commit` |
| Tab 新增按键 + Shift+Tab 不折成接受 | ✅ `ibus_proto::keyval::TAB` |
| FocusOut / 透传按键收面板 | ✅ `has_visible_ui()` + 透传钩子 |
| 打包、来源说明、文档 | ✅ deb / justfile / NOTICE / README / data/ENGLISH.md |

## 2. 验证命令与结果

```
cargo test --workspace --offline                                 → 561 passed / 0 failed
cargo clippy --workspace --all-targets --offline -- -D warnings  → 0 error / 0 warning
python3 -m unittest discover -s scripts/tests -p 'test_english*.py' → 39 tests OK
中文排序 vs S0 基线                                                → diff 为空
```

新增测试文件：`crates/xwx_engine/tests/english_bigram.rs`（6 条，含坏文件矩阵）、
`crates/xwx_engine/src/session/tests/prediction_flow.rs`（18 条 N 组）、
`tests/wire_english.rs` 增 8 条 M2 线级验收、`scripts/tests/test_english_bigram.py`（12 条）。

## 3. 真实数据（`S9-hashes.txt`）

```
上游文件 SHA-256 : fd892a160184101dd7ae807ac5a302d01fcea1c47304181a8ed7ed9c94545bcd ✓
字节数 / 行数     : 5,137,213 / 242,342 ✓（与规格一致，不按文件名猜）
data/english_bigram.bin : e5761d8353fa737251f5bbcc88cd89bae60d4beb2b63ba92eb635301589c5b83
```

规格点名的三个词对实测一致：`hello world 7852480`、`looking forward 271821504`、
`thank you 733376704`。Top-32 截断后的真实行（计数降序）：

```
thank     T=1064706816   you 733376704 · the 135289600 · all 44178240 · my 21105088
looking   T=6210413824   for · at · to · forward · statements
hello     T=177132480    all · to · everyone · from · kitty · there
```

## 4. 英文评测（`S9-english-eval-acceptance.txt`，217 行）

| 指标 | 实测 | 门槛 | 结论 |
|---|---:|---:|---|
| 精确词 Top-1 | 88/98 = 89.8% | ≥90% | ❌ 差 1 条（同 M1） |
| 精确词 Top-3 | 97/98 = 99.0% | ≥95% | ✅ |
| 补全 Top-3 | 48/90 = 53.3% | ≥90% | ❌（同 M1，根因见 S6 报告 5.1） |
| **下一词 Top-1** | **10/11 = 90.9%** | — | — |
| **下一词 Top-3** | **11/11 = 100%** | — | ✅ |
| 中文保护 Top-1 | 17/18 = 94.4% | 不退化 | ✅ 与 mode=off 逐条相同 |
| 中文保护 Top-9 | 18/18 = 100% | 不退化 | ✅ |
| 整体命中 | 211/217 = 97.2% | — | — |
| OOV | 0 | — | — |

下一词唯一没进 Top-1 的是 `thank → the`（`you` 的计数是它的 5.4 倍，但
`you` 被调参集的词汇隔离占用）；规格点名的 `thank → you` 作为调参集样例存在，
调参集下一词 Top-3 = 5/5。

> **实施中发现并修掉的一处规格偏差**：零输入预测最初把 `input_len` 传成 `0`，
> 于是 `extra = 词长`，长词被凭空惩罚（`word` 行里 `for` 掉到第 6 名）。
> 规格第 6 节要求预测时 **`extra = 0`**。现在用 `InputLen::{Chars, None}` 把
> 「组合态的实际串长」与「零输入没有串长」彻底分开，并有回归测试
> `zero_input_never_applies_a_length_penalty` 钉住两者恰好差 0.15。
> 修复后：下一词 Top-1 从 81.8% → **90.9%**，调参集 Top-3 从 4/5 → **5/5**。

## 5. 性能（`S9-english-perf.txt`，release，repeat=24）

| 项 | 实测 |
|---|---|
| 引擎装配（含英文词库完整校验） | 30.8 ms |
| 引擎装配 **+ english_bigram.bin 完整校验** | **37.2 ms**（+6.4 ms，一次性） |
| **零输入下一词（召回 + 条件排序）** | p50 **0.066 ms** / p95 **0.078 ms** / max **0.083 ms** |
| 超 12 ms 样本 | **0/144** |
| 有英文上文的补全：无静态二元 vs 带静态二元 | p95 差值 **+0.027 ms** |
| 英文开-关 p95 差值（M1 场景） | -0.014 ~ +0.057 ms |

预测面板比一次组合查询**快两个数量级**：它只查 16,315 行的二分索引，
不碰中文词库与语言模型。

## 6. 打包与文档

- `Cargo.toml` 的 deb assets 加 `english_bigram.bin` → `/usr/share/xwx/english_bigram.bin`，
  以及 `data/english/LICENSES/SymSpell-NOTICE.md` → `/usr/share/doc/xwx-ime/english-bigram-NOTICE.md`；
- staging 用 `cargo deb --no-build` + `dpkg-deb -c` 核对（`S9-deb-contents-english.txt`）：
  两个 `usr/share/xwx/english*.bin` 与三份 NOTICE 都在包里；
- `justfile` 安装步骤检查该文件（**可选**，缺了只提示）；
- `README.md` 的 M2 条目改成「已实现」，并写明 `XWX_ENGLISH_PREDICT` 关掉的
  只是零输入面板——**有输入时的条件排序照常使用静态二元数据**；
- `data/ENGLISH.md` 记两份英文数据产品的规模与降级行为；
- **`SymSpell-NOTICE.md` 单独存在**：上游代码是 MIT，但不能代替数据条款记录。

**没有运行任何会重启真实 IBus 的安装任务。**

## 7. 已知限制与未完成项

1. **补全 Top-3 仍未达标（53.3%）**，根因是 M1 就存在并已报告的
   `good_en` 阈值与模板第三槽问题（S6 报告 5.1），修法属规格改动，**等你拍板**。
2. **精确词 Top-1 89.8%**，差 1 条（`cache`）。
3. 零输入面板**只有 11 条验收行**——受静态表规模限制（16,315 个 prev，
   但语料词表只有 50 个词，交集有限）。增加覆盖需要扩充语料词表。
4. `Tab` 在组合态与空闲态是 `Noop`（不抢应用焦点切换）；`Shift+Tab`
   在 IBus 翻译层直接返回 `None` 透传。
5. 前向 `Delete` 没有会话事件，由 IBus 层按「关闭预测 + 清上文 + 透传」处理，
   **没有**专门的线级断言（`Backspace`/`Left`/`Right` 有）。
6. 预测面板的鼠标点击走 `CandidateClicked`（页内下标），已有会话测试，
   但**没有**真实 D-Bus 点击事件测试（IBus 的候选点击是面板信号，harness 未模拟）。
7. `english_bigram.bin` 与 `english.bin` 的**版本配套**靠 meta 里的 SHA-256 人工核对；
   运行时不校验两者一致性（跨文件不共享 word_id，所以错配只会让条件分偏低，
   不会把旧编号解释成另一个词）。
