# M1 验收报告（英文前缀补全 + 中英稳定混排）

日期：2026-10-09。范围：`docs/ENGLISH_INPUT_PLAN.md` 的 **M1**。
**M2（下一词预测）未实现**，见文末「未完成」。

## 1. 交付状态

| 能力 | 状态 |
|---|---|
| wordfreq 常用词频底座 + 人工技术词/大小写覆盖 | ✅ 50002 词 / 116134 前缀 |
| 独立英文精确查询与前缀补全 | ✅ 不改中文词库与中文 LM |
| 中英意图分档混排（中文内部次序保持） | ✅ 决策表逐条实现 |
| 英文选择学习 + 独立英文用户文件 | ✅ `user_english[.dev].txt` |
| 英文屏蔽与跨语言统一撤销 | ✅ 共享撤销记录（锁序 undo → UserLM） |
| 连续英文单词的延迟空格 | ✅ 固定规则，见 S03-S06 |
| 构建/加载/安装资源接线，缺数据只关英文 | ✅ deb + justfile + 装配降级 |
| 引擎/会话/D-Bus 测试 | ✅ 见第 3 节 |
| 真实数据排序与性能报告 | ✅ 见第 4/5 节（补全 Top-3 **未达标**） |
| **M2 下一词预测** | ❌ **未实现** |

## 2. 验证命令与结果

```
python3 -m unittest discover -s scripts/tests -p 'test_english*.py'   → 27 tests OK
cargo test --workspace --offline                                      → 524 passed / 0 failed
cargo clippy --workspace --all-targets --offline -- -D warnings       → 0 error / 0 warning
```

产物与日志：`artifacts/english/S6-*.txt`（中文回归、性能、评测、deb 清单、哈希）。

## 3. 测试矩阵覆盖

| 组 | 覆盖 |
|---|---|
| D01-D08 | `scripts/tests/test_english_dict.py`（21 条）+ `crates/xwx_engine/src/english/tests.rs`（15 条，含独立 oracle 与坏文件矩阵） |
| D09 | `crates/xwx_engine/tests/english_dict.rs`（真实 Python 产物）、`engine/tests/blend_matrix.rs::d09_*`、`tests/wire_english.rs::missing_english_dict_*` / `i03_bad_english_data_*` |
| C01-C02 | `candidate.rs` 单元测试（旧 JSON 兼容、冲突数据报错） |
| C03-C14 | `engine/tests/bilingual.rs` + `engine/tests/blend_matrix.rs` |
| U01-U07 | `session/tests/english_flow.rs` + `user_lm/english_index.rs`（与独立 oracle 一致） |
| U08 | `session/tests/english_flow.rs`（中英交错删除，撤销真正最近的一次） |
| U09 | `user_lm` 既有损坏只读恢复测试 + `wire_english.rs::i05_*`（SIGTERM 落盘） |
| S01-S14 | `session/tests/english_flow.rs`（含完整时序、延迟空格五条约束、组合内导航不清上文） |
| I01-I05 | `tests/wire_english.rs`（11 条真实 D-Bus 验收） |

## 4. 中文回归（`S6-eval-rank.txt`）

```
daily.jsonl      召回 12/12 · MRR 1.000 · 首屏 12/12
mixed.jsonl      召回 13/14 · MRR 0.929 · 首屏 13/14
edits.jsonl      召回  6/6  · MRR 0.841 · 首屏  5/6
keystrokes.jsonl 召回 12/12 · MRR 0.877 · 首屏 11/12
tuning.jsonl     召回  5/5  · MRR 1.000 · 首屏  5/5
```

与 S0 基线**逐字相同**（`diff` 为空）——英文改动没有动中文排序。

## 5. 英文评测（`S6-english-eval-acceptance.txt`）

固定核心例子：`pyth` → `Python`、`hello` → `hello` 通过（C03/C04 与 D-Bus I01）。

| 指标 | 实测 | 门槛 | 结论 |
|---|---:|---:|---|
| 词表覆盖率（OOV） | 0 OOV | — | ✅ |
| 精确词 Top-1 | 88/98 = **89.8%** | ≥90% | ❌ 差 1 条 |
| 精确词 Top-3 | 97/98 = **99.0%** | ≥95% | ✅ |
| 补全 Top-3 | 48/90 = **53.3%** | ≥90% | ❌ |
| 中文保护 Top-1 | 17/18 = **94.4%** | 不退化 | ✅ 与 mode=off 完全相同 |
| 中文保护 Top-9 | 18/18 = **100%** | 不退化 | ✅ 与 mode=off 完全相同 |
| 整体命中 | 200/206 = 97.1% | — | — |

**中文保护集在 mode=on / mode=off 下失败集合逐条相同**——这是本任务最硬的
非退化证据：英文通道没有抢走任何一条中文首选。

### 5.1 补全 Top-3 未达标的根因（实测拆分）

同一份语料的三层数字：

- 目标**进过英文召回池**：84/90（其余 6 条被有界近似挡在池外）；
- 目标在**英文通道内部**排 Top-3：65/90；
- 目标在**混排结果**里排 Top-3：48/90。

两个各自独立的机制：

1. **`good_en` 阈值卡住 18/90 行**。wordfreq `large` 里存在一批
   zipf 在 **250-299** 的罕见短词，而它们恰好是输入串的**精确命中**：
   按 C04「精确优先」它们成为 E0，zipf < 300 ⇒ `good_en` 为假 ⇒ 决策表落到
   「其他 → ChineseFirst」，用户想要的补全（`lunch`/`grass`/`city`/`holiday`）
   被排到第 9 名之后。实测例：
   `lun`(q=267)→lunch、`gra`(q=272)→grass、`cit`(q=289)→city、
   `bri`(q=286)→bridge、`holi`(q=271)→holiday、`bre`(q=253)→bread。
   18 行的 E0 静态频率全部落在 `[250,300)`。
2. **`EnglishFirst` 模板的第三槽是 Z**。模板是 `E E Z E Z…`，所以英文**内部
   第 3 名**落在混排第 4 名；再叠加前缀竞争（`app` → apple/application/airport/
   appointment…），内部 Top-3 只剩 65/90 能过，而其中排第 3 的那些刚好卡在
   混排第 4。

**没有为了过门槛改标注或改判据**（规格第 15 节明确禁止）。可行的修法都属于
**规格改动**，需要用户拍板，未实施：

- (a) E0 是精确命中且 zipf < 300 时，用**次优补全**的 zipf 参与 `good_en`；
- (b) `strong_completion` 命中时把模板第三槽让给英文（`E E E Z …`）；
- (c) 接受「3 字母前缀 + 密集中文简拼池 ⇒ 中文优先」这一产品取舍。

### 5.2 唯一的精确词失败

`cache`（q 低于同形中文简拼候选「擦车」）：`strong_zh` 成立 ⇒ ChineseFirst，
英文排第 3 名，`Top-1` 差这一条。同样是决策表的设计后果，不是实现偏差。

## 6. 性能（`S6-english-perf.txt`，release）

数据：中文 `data/dict.bin` 80 MB + `abbrev.bin` 72 MB + `syllable_graph.bin` 62 MB
+ 万象 `wanxiang-lts-zh-hans.gram` 420 MB；英文 `data/english.bin` 4.46 MB / 50002 词。

| 项 | 实测 |
|---|---|
| 引擎装配（含 mmap 与英文词库**完整校验**） | 31.6 ms |
| 首次查询 | 12.7 ms |
| 稳态 p50/p95/max（英文开，5 个场景） | 0.10-2.38 / 0.15-3.87 / 3.96 ms |
| 英文开-关 p95 差值 | **-0.002 ~ +0.076 ms** |
| 超过 12 ms 的逐键样本 | **0** |
| 中文拼音场景（回归） | 开/关 p95 差值 +0.000 ms |

受控用户模型（学 300 个词的二元计数）后再测：p95 与空模型组差异 < 0.05 ms，
**没有出现索引更新的隐藏退化**。

> 纯会话/引擎时间；D-Bus 往返另测（`wire_english.rs`），不把两者混为一谈。

## 7. 资源与打包

- `data/english.bin` 不进 git，由 `scripts/build_english_dict.py --wordfreq` 生成；
- `Cargo.toml` 的 deb assets 已加 `english.bin` → `/usr/share/xwx/english.bin`，
  以及 wordfreq 的署名与许可全文（CC BY-SA 4.0，见 `data/english/NOTICE.md`）→
  `/usr/share/doc/xwx-ime/`；
- `justfile` 安装步骤检查 `data/english.bin`（**可选**，缺了只提示）；
- 临时 staging 目录内容已用 `cargo deb --no-build` + `dpkg-deb -c` 核对
  （`S6-deb-contents.txt`），**没有运行任何会重启真实 IBus 的安装任务**。

**生产装配实测**（用真实 `data/english.bin` 起 release 二进制，启动日志）：

```
INFO xwx_ime: 词库已就绪 path=.../data/dict.bin entries=1864966
INFO xwx_ime: 用户学习模型已接入 path=/tmp/xwx-verify-user_bigram.txt
INFO xwx_ime: 英文通道已接入 dict=.../data/english.bin user=/tmp/xwx-verify-user_english.txt
              words=50002 prefixes=116134 predict=false bigram=.../english_bigram.bin
```

启动期不创建英文用户文件（本次没有选词），符合「没有学习就不写盘」的约定。

数据哈希（`S6-hashes.txt`）：

```
a6bfd0bcf63177f5296df3870970005204b8553bda2e500232c122ea7bde1646  data/english.bin
2d287495ce33385c4d6863853ba4a184c525c473694b0cd8c4e790de3796b5fe  data/dict.bin
adadc7b99bfec523ae6e0387397ebf1b28455c2e8730c867fd7da8a729342156  tests/fixtures/english/words.bin
```

## 8. 已知限制

1. **M2 未实现**：`XWX_ENGLISH_PREDICT` 只解析并记录，不产生任何行为；
   `next_word` 语料行由评测工具单列为「M2 未实现」，不从分母删除。
2. 补全 Top-3 未达产品门槛（53.3%），根因见 5.1；**未改标注凑通过**。
3. 精确词 Top-1 差 1 条（89.8% vs 90%）。
4. 语料是**机械派生**的前缀（对每个词砍 1-3 个字母），3 字母前缀与密集中文
   简拼池正面相撞的比例高于真实使用；这是有意的压力测试，但也说明门槛
   与语料构造强相关。
5. `in`/`is`/`it`/`us` 不是合法中文音节，词库里没有任何中文候选，因此在语料
   里标注为纯英文行（原「中英同形」标注与词库事实不符，已改正并在此说明）。
6. 英文召回是**有界近似**：静态前缀 Top-32 + 用户前缀 Top-32 + 上文 Top-32，
   6/90 的补全目标不进池。
7. `english_eval` 的 next_word 行只报告未实现；M2 落地后需要新增独立预测入口
   与 N 组测试。
8. 大小写按键路径（Shift/CapsLock 直出）沿用既有行为，M1 不产生英文候选——
   由既有 `wire_*` / 会话测试覆盖，未新增专用断言。
