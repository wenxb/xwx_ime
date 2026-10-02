# XWX 输入法改造计划与进度

> **总方针**：不重复造轮子，拿 librime 当参照。两条腿并行——① 读 librime 源码补
> 组件清单（**缺什么**）② 差分对拍量化行为偏差（**现有的偏了多少**）。缺一不可：
> `BEAM_WIDTH=16` 把候选钳死、候选里混入非起始位置的词，都是对拍逼出来的。
>
> **入口**：[`docs/HANDOFF.md`](docs/HANDOFF.md) 接手先读（定过又推翻的判断、
> 刻意代价、指标的坑）· [`docs/VERIFY.md`](docs/VERIFY.md) 实机验证清单 37 条 ·
> [`docs/PLAN-history.md`](docs/PLAN-history.md) 已完成轮次的过程留档 ·
> [`docs/gear-audit.md`](docs/gear-audit.md) 组件审计 ·
> [`docs/design-batch1.md`](docs/design-batch1.md) 早期设计。
>
> **当前状态**：测试 **284 passed / 1 ignored**（235 单元 + 49 D-Bus 集成），
> clippy `-D warnings` **全绿**；仓库无远端。gear 审计已清零——简拼（纯 + 混合）、
> 中文标点、可编辑输入缓冲、Shift 大写、造词、`,`/`.` 翻页、`'` 分隔符、
> Ctrl+Delete、邮箱/网址识别全部可用，全拼行为无退化。
>
> ⚠ **但这是引擎与会话层的绿，不是实机可用的证据**——IBus 那几条路径
> （英文模式透传、`UpdateLookupTable` 等信号）在 D-Bus 边界上，**真实
> `ibus-daemon` + 真实应用**仍要按 `docs/VERIFY.md` 手工敲一遍。
>
> **本文件只放结论、待做与验收口径。** 已完成轮次的完整过程（含走错的弯路、
> 被推翻的归因）在 [`docs/PLAN-history.md`](docs/PLAN-history.md)，需要细节时
> 按标题 grep；**别把它当必读。**
>
> 最后更新：2026-10-02（本轮：PLAN 瘦身 28.4K → 10.7K 字符，过程留档外迁）。

## 一、基础设施

### 差分对拍台
- [x] `tools/rime_oracle/` — 驱动本机已装的 librime 1.10.0（免 sudo，直接链 `librime.so.1`）
- [x] 方案切到 `luna_pinyin_simp`（默认 `luna_pinyin` 出繁体，没法和我们的简体词库比对）
- [x] `crates/xwx_engine/examples/probe.rs` — 我方同格式对端
- [x] `tools/compare.py` — 六项指标 + `RECALL_MISS`（真召回缺失）/ `RANK_GAP`（召回了但排名靠后）
- [x] `tools/corpus/basic.txt` — 81 行、9 组分类语料
- [x] 双侧输出深度 10 → 100（否则分不清「没召回」和「排名靠后」）

跑法：
```bash
./tools/rime_oracle/rime_oracle < tools/corpus/basic.txt > /tmp/oracle.txt
XWX_DICT_PATH=$PWD/data/dict.bin \
  cargo run -q -p xwx_engine --example probe --offline < tools/corpus/basic.txt > /tmp/xwx.txt
python3 tools/compare.py /tmp/oracle.txt /tmp/xwx.txt tools/corpus/basic.txt
```

> ⚠ oracle 的 `user_data_dir` 硬编码在 `tools/rime_oracle/.rime_user`，
> **绝不能指向 `~/.local/share/fcitx5/rime`**（那是日常打字在用的配置）。

### 组件清单
- [x] `docs/gear-audit.md` — librime `src/rime/gear/` 全部 36 个组件逐项对照 xwx 现状

---

## 二、已完成的改动

### 第 0 批：零架构风险项
- [x] **UserLM 接线** — `learn_committed` 整条学习链路此前在生产进程里是死代码
      （`main.rs` 从未调用 `with_user_lm`）。已接线 + 30s 定时 flush + Ctrl+C 兜底
- [x] **回声候选** — 候选列表永不为空。照搬 librime `echo_translator`：
      仅当全场无任何候选时冒头，`quality` 垫底，不污染正常候选

### Block A：候选管线重构
- [x] **A1 候选只能起始于位置 0** — 此前 `for spot in &path.words` 取了路径上所有词位，
      导致 `nihao` 的候选里混进只覆盖 `hao` 的「好/号/毫」，用户选中毫无意义
- [x] **A2 候选来源改为 `lattice[0]` 全量** — 此前只从 beam 幸存路径取词，
      被 `BEAM_WIDTH = 16` 钳死（八个不同单音节输入全部恰好返回 16 个候选）。
      修复**未新增任何词典查询**（`build_lattice` 早就查完了）
- [x] **A3 后向 DP 统一打分** — 让「覆盖整串的词」与「只覆盖前缀的词」落在同一量纲。
      含一处**刻意的近似**（后向拿不到精确 LM 状态，丢 bigram），已写进代码注释

### B3：简拼（首字母输入）
- [x] **路线选型 spike** — `tools/spike_abbrev/`，实测三条路线后**路线 (c) 声母副索引**定稿
- [x] spike 推翻了设计文档原方案（主索引 key 空格分隔、用户输入不带空格，
      前缀匹配命中恒为 0），已改为「先切音节 → 空格拼回前缀 → 残余字符当声母过滤」
- [x] **存储层** — `scripts/abbrev_to_bin.py` → `data/abbrev.bin`（68.9 MB，主索引 0.90×）；
      `dict.rs` 新增 `AbbrevIndex` / `lookup_abbrev` / `lookup_mixed`
- [x] 早停已验证是**真早停**（凑够 `MAX_LOOKUP_RESULTS=500` 即 `break 'outer`，
      不是全扫后截断）
- [x] 副索引缺失时**优雅退化**（实测挪走文件后不 panic、词库不变空、精确查询正常）

---

## 三、已完成轮次（结论速查）

> 每轮一句话；过程与数字见 [`docs/PLAN-history.md`](docs/PLAN-history.md) 同名小节。

| 轮次 | 结论 |
|---|---|
| B3 引擎集成（三步一体） | 音节表预热 **839.9ms → 0.7ms**（从按键路径上挪走）；简拼副索引接入引擎 |
| 让简拼真的能被看见（四轮才挖到根） | 对拍 **0.40 → 51.30**；此前是「查得到、排得对，就是没机会露面」 |
| 排序：严格分层 → 可达性分桶 | **走过一次弯路**：裸「未解释字母数」当硬首键会淹掉高频单字；改为 u_min 桶 |
| 中文标点（gear P0） | 新增 `punctuator.rs`，映射逐字节核对自 `rime-data/symbols.yaml` |
| 工程卫生 | clippy 基线清零，门槛固定为 `-D warnings` 必须过 |
| B1 可编辑缓冲 + ascii_composer 的 Shift | 输入缓冲从 append-only 改成带光标可编辑；Shift 选择补齐 |
| 造词 + 键位三件套 | 逐字选出的短语会造词条；`,`/`.` 翻页；Ctrl+Delete 删候选 |
| recognizer（gear 审计最后一项） | 邮箱/网址不再被切拼音（`www` 不再出「万维网」） |
| 2026-10-01 三个「文档宣称 ≠ 实际」 | 模糊拼音接成 `XWX_FUZZY`（**默认关**）；造词判据收紧；README 重写 |
| 2026-10-02 用户层「学习反而降权」 | 用户层改 **backoff**，学过的词不再被绝对中性分压低；`ky` 里「可以」第 11 → 第 1 |
| 2026-10-02 个人化改**计数增益** | `score = 通用层分 + log10(1 + c·1000)`；与语料规模无关，一次上屏 +3.0（同类候选登顶），选得越多越高，且翻不过「整词 vs 逐字」的结构墙 |
| 2026-10-02 T1 召回截断改按词频 | 混合简拼收集期不再「先到先得」，改为排序后按词频截断；`meiy` 起回「没有」（词频 504185 却曾被 500 条早停丢掉） |

### 别再试（否定性结论，逐条保留）

- 空上下文查 unigram 修排序 —— **结构性走不通**（通用 gram 层的值不跨键长可比；
  用户层 unigram 是另一回事，别一起砍）。
- 「`xian` 第 18 名是 A3 后向 DP 丢了 bigram」 —— **归因是错的**。
- 按「未解释输入字母数」当硬首键 —— **规则本身有缺陷**，已推翻。
- 「简拼要给 `SYLLABLES` 加单字母音节」 —— 绕开了问题。
- 「音节表构建期写进 `abbrev.bin`」 —— 不必要。
- 跨模型混合用**绝对**中性常数 —— 2026-10-02 那次事故的根因。
- 其余 ①–⑭ 见 [`docs/HANDOFF.md`](docs/HANDOFF.md) 第一节（内含「测试设施会撒谎」
  「先占名字后挂对象」等）。

## 四、待做

### ⚠ 实机验证（现在最该做的一件事）

**284 passed 里已含 49 条 D-Bus 集成测试**（私有总线 + 真实引擎进程），
所以下面这些路径**不再是「结构上摸不到」**——已被自动化覆盖：
英文模式透传（`engine.rs` 的 `process_key_event` 直接 `return false`）、
`UpdatePreeditText` / `UpdateLookupTable` / `CommitText` 三个信号的内容与字段
顺序、`FocusOut`、候选点击、翻页、Ctrl+Delete、可编辑缓冲的逐字符序列、
隔离性（不碰真实用户数据）。

**仍未自动化的只有「真实 daemon + 真实应用」那一段**：真实 `ibus-daemon`
的激活时序（走 `CreateEngine` 而非预导出实例）、GNOME Shell candidate popup
的**实际渲染**、跨应用 `FocusOut` 的真实到达时机、与系统输入源切换 /
fcitx5 抢总线名的交互——集成层用的是**私有总线**，这些验不了。

功能已经堆了一大摞（标点、大写、光标、造词、翻页、分隔符、删候选、识别），
**全都在会话/协议层**——越堆越该实机跑一遍。

**→ 清单在 [`docs/VERIFY.md`](docs/VERIFY.md)**：37 条，每条独立，
含不动系统、不碰 fcitx5 的跑法（`just install` 要 sudo 且会 `ibus restart`，
而 fcitx5 会占用 IBus 总线名）。

最该先验的四条：**翻页 8~10**（`.`/`,` 键位抢占）、**造词 27**（跨进程，
靠 `FocusOut`）、**网址 33**（端到端序列，初版真的产出过 `http:、、xcom`）、
**英文模式 16**（ibus 层直接 `return false`，单测覆盖不到）。

- [x] ~~**B2 音节分隔符 `'`**~~ — 已完成，见第三节

### 拼音容错：打快了打错字母（如 `nihao` 打成 `nihoa`）（待做，用户 2026-10-02 提）

<!-- 恢复稿：此条原由另一个会话写入、处于未提交状态，被我在还原本地改动时
     误还原（未 git add 过，对象库里没有，无法从 git 恢复）。以下按当时
     读到的 diff 逐字重建，**尾部可能缺失**；若原会话仍在，请以其版本为准。 -->

- [ ] **探究拼音纠错通常怎么做，并让相邻字母打反/漏字母仍能出目标词。**
      实测（真实词库、无用户层）：**`nihoa` 前 3 是「你 / 尼 / 泥」——「你好」完全
      没出现**；`nohao` 直接只剩回声。而 `nihaoo`（多打一个 `o`）反而正常，
      说明**多字母能容忍、换位不能**。

  **主流做法（已核对本机 `/usr/share/rime-data/pinyin.yaml`）**：librime 用一组
  **`derive/` 正则规则给词典条目派生「常见笔误拼写」**，变体与正写一起进同一个
  音节网格、靠词频与 LM 排序——**不是通用编辑距离搜索**。真实规则（`key_correction` 段）：
  `derive/([aeiou])ng$/$1gn/`（dagn⇒dang）、**`derive/ao$/oa/`（hoa⇒hao，正是
  `nihoa` 这条）**、`derive/ong$/on/`（zhonguo⇒zhong guo）、
  `derive/([dtngkhrzcs])o(u|ng)$/$1o/`、`derive/([iu])a(o|ng?)$/a$1$2/`（tain⇒tian）。
  同一份 `pinyin.yaml` 里还有 `spelling_correction`（正写规范，如 `jv`⇒`ju`）。

  **落地位置**：现有 `crates/xwx_engine/src/corrector.rs`（13 组模糊音、带权重 +
  `MAX_EXPANSIONS = 64`/`MAX_DEPTH = 4` 防爆炸）就是同形状的钩子，容错可作为一种
  新的拼写变体加进去，带自己的惩罚项与预算；**别新造一条并行管线**。
  **验收线**：`nihoa` 能出「你好」且在前列；合法全拼**逐字节不变**（冻结约束）；
  延迟不得破 12ms（模糊音那次实测 6.44× 的教训）；病态输入有预算钳制。
  **待定的设计问题**：容错是「默认开」还是像 `XWX_FUZZY` 一样开关（打错规则越多，
  同音竞争越乱）；以及只做 `derive` 式定点规则，还是补一个受预算约束的编辑距离 1。

### 全拼排序调优（已诊断，暂不改，范围极窄）

- [ ] **`xian` 的「西安」排第 18 名** —— 已完整诊断，**暂时接受现状**。
      根因**不是** A3 后向 DP 近似，是 `wanxiang.rs::score_word()` 的空上下文早退
      （实测 `examples/lm_ctx_probe.rs`：空上下文对任何词返回同一常数）。
      真要解需要跨候选归一化，比试过的那条路大。
      绕开方式：`xi'an` → 西安第 1。细节见 history 与 HANDOFF ⑥。

### 中英混排：明显是英文的串，英文候选要前置（待做，用户 2026-10-02 提）

- [ ] **拼音串明显是英文时，英文候选（或原串）应排在中文候选前面。**
      现在的行为是硬转中文：`abc` 出「安伯尘」、`mailto` 出「买/卖/埋」、
      `http` 出「好调皮」——打网址/命令行得先切英文模式重打。
      这条已写进 README 的已知症状，但**待做清单里一直没有条目**。

  **主流做法（待选型，别直接照搬一个启发式）**：按「整串是否落在英文词表里 +
  该串能否被拼音完整切分」两条证据打分，而不是按「有没有元音」这类形状判断。
  可选路线：(a) 内置小型英文词表（几千常用词）做整串命中，命中则把原串
  作为一个候选插到最前；(b) 只在**整串无法切分成合法音节**时才前置原串
  （最保守，零词表）；(c) 英文词表 + 音节可切分性联合判定（最接近大厂，
  需要一份词表数据与一条优先级策略）。
  **验收线**：`mailto`/`http`/`vim`/`abc` 这类串原串在前；`nihao`/`women`
  这类合法全拼**一点都不能变**（冻结约束）；`abc` 这类既是合法音节串又是
  英文缩写的要给出明确选择并写进测试。

---

### gear 清单

- [x] **已清零**（punctuator / ascii_composer 的 Shift / selector 翻页 /
  Ctrl+Delete / 造词 / charset_filter / recognizer）。明细与「为什么砍掉
  charset_filter」见 [`docs/PLAN-history.md`](docs/PLAN-history.md)。


---

## 五、指标与基线

### ⚠ 先读：`TOP10_OVERLAP` 这个名字是骗人的

`compare.py:169` 实际算的是 **`len(set(rime_cands) & set(xwx_cands))`——
两边完整候选列表（各上限 100）的交集**，不是 top-10 交集。
打印出来那句 `mean |rime_top10 ∩ xwx_top10|` 是**陈旧标签**：
当初把双侧深度从 10 提到 100 时，改了取数深度却没改指标名和标签。

**后果**：本表 Block A 那一列的 `4.95 → 6.83` 是**深度 10 时代**的数，
和现在动辄 20~30 的数**不是同一个单位，不能纵向比**。
「全拼四组 ≥6.83 不许退化」这条曾被写成硬门槛，但它写的是一个已经不存在的单位。

**以后怎么用**：判断「有没有退化」**不要比这个数的绝对值**，
而是拿改动前后的 probe 输出跑同一个 oracle 做对比（下表就是这么测的）。
名字暂不改，改了历史记录更难对上；这段说明就是解药。

### 当前状态（可达性分桶 + 标点落地后，2026-08-10）

**目标词名次**（这是比聚合指标更该看的东西）：

| 输入 | 目标词 | 名次 |
|---|---|---|
| `zgr` / `bjdx` | 中国人 / 北京大学 | **1** |
| `zhongguor` / `zhonggr` / `renm` / `beijdx` | 中国人 / 中国人 / 人们 / 北京大学 | **1** |
| `nih` | 你好 | **2**（第 1 是「你还」，词频更高） |
| `nihao` / `woshizhongguoren` / `woaibeijingtiananmen` / `jintiantianqihenhao` | 长词 | **1** |
| `ni` / `zhon` / `zhongg` | 你·尼·泥 / 中·种·重 / 中国 | **1·2·3** / **1·2·3** / **1** |

**`TOP1_AGREE`**（参考项，不是门槛）：全拼单字 10/10、全拼词 10/10、
整句 3/4、歧义切分 7/8——与分桶改动前**完全一致**；混合简拼组 0/5 → **4/5**。

| 旧口径指标（深度 10，仅存档） | 起始 | 第 0 批后 | Block A 后 |
|---|---|---|---|
| `TOP10_OVERLAP` overall | 3.20 | 3.20 | 4.02 |
| ↳ 全拼四组均值 | 4.95 | 4.95 | 6.83 |

### 其他指标
| 指标 | 起始 | 现在 |
|---|---|---|
| `ZERO_ONLY_XWX`（含回声） | 12 | **0** |
| 纯简拼组真实候选（排除回声） | 0/10 | **10/10** |
| 纯简拼组 `TOP1_AGREE` | 0/10 | 5/10（参考项） |

> `ZERO_ONLY_XWX` 归零是**症状**修好（候选窗不再空白），不是**能力**修好——
> 回声候选只让用户看见自己敲了什么。所以要单独看「排除回声」那一行。
>
> **`TOP1_AGREE` 不能当验收门槛**（简拼组也不行）：两边词库/LM 不同，对齐它
> 等于让实现去迎合 librime 的词库。
>
> 口径更正记录：本表原先给简拼组写的是「`TOP1_AGREE` ≥6/10」当验收线，
> 与上面这条自相矛盾（`design-batch1.md` 的 Block B 验收表同）。已降为参考项。

### 测试基线
| 阶段 | 数量 |
|---|---|
| 起始 | 87 passed / 1 ignored（76 / 3 / 8） |
| 第 0 批后 | 91（80 / 3 / 8） |
| Block A 后 | 94（83 / 3 / 8） |
| B3 存储层后 | 101（90 / 3 / 8） |
| B3 ① 后 | 101（90 / 3 / 8）全绿，0 failed |
| B3 ②③ 后 | 111（100 / 3 / 8） |
| 配额 + 残余字母 + A1 兜底后 | 117（106 / 3 / 8） |
| 可达性分桶 + 标点后 | 131（119 / 4 / 8） |
| B1 可编辑缓冲后 | 141（129 / 4 / 8） |
| ascii_composer Shift 后 | 149（136 / 5 / 8） |
| 造词后 | 160（147 / 5 / 8） |
| 翻页 + `'` 分隔符 + 删候选后 | 181（168 / 5 / 8） |
| recognizer 后 | 207（194 / 5 / 8） |
| 声母前缀枚举后 | 212（199 / 5 / 8） |
| 模糊 + 造词 + LM 探查后 | 222（209 / 5 / 8） |
| D-Bus 集成层修好后 | 271（209 / 5 / 8 + 集成 49） |
| 混合简拼任意位置缩写后 | **275（213 / 5 / 8 + 集成 49）** |
| 用户层 backoff 修复后（2026-10-02） | 278（216 / 5 / 8 + 集成 49） |
| 个人化改计数增益后（2026-10-02） | 283（221 / 5 / 8 + 集成 49） |
| 混合简拼召回截断改按词频后（2026-10-02） | **284（222 / 5 / 8 + 集成 49）** |

**集成层 49 条 = 5 个文件**：`passthrough_contract` 7、
`focus_paging_delete` 14、`signal_payloads` 7、`interactive_sequences` 16、
`hermeticity` 5。它们跑**真实 `xwx_ime --ibus` 进程 + 私有 `dbus-daemon`**，
覆盖 `cargo test` 结构上摸不到的 D-Bus 边界（见 `README.md` 的「测试」一节）。
集成层曾经**整体不可信**（信号 100% 被丢弃、多数用例红、部分永久挂起），
修法见 `docs/HANDOFF.md` 第 ⑫ 条——那里记的六个缺陷全部是**测试设施**的
问题，引擎侧只有一条真 bug（第 ⑬ 条）。

```bash
just verify                                                   # check + test
cargo test --workspace --offline                              # 必须在项目根跑
cargo clippy --workspace --all-targets --offline -- -D warnings
```
> `.cargo/config.toml` 指定 `target-dir = /mnt/hdd/target/xwx_ime`。
> **不要覆盖 `CARGO_TARGET_DIR`** —— 在别处验证等于没验证。

### 性能预算

**端到端**（`examples/lookup_bench.rs`，装了副索引，走完整 `lookup_candidates`）：

| 输入 | min / avg / p95 | 上限 |
|---|---|---|
| `womenjintianquxian`（17 字符） | 1.534 / — / 1.738 ms | 12ms |
| `zhongg` | 1.097 / — / 1.242 ms | 8ms |
| `zgr` | 0.359 / — / 0.410 ms | 8ms |
| `nihao` | 0.166 / — / 0.183 ms | 8ms |

> **必须用 `--release` 测**。debug 构建下同一份代码是 9.6ms 量级，
> 曾有人据此误判「退化 7 倍」。

**dict 层单点**（`examples/abbrev_bench.rs`）：

| 项目 | 现状 | 上限 |
|---|---|---|
| 简拼单次查询 `zgr` | 0.070ms（p95 0.097） | — |
| 混合简拼单次 `zhongg` | 0.592ms（p95 0.676） | 5ms |
| 混合简拼单次 `zhongguor` | 0.635ms（p95 0.731） | 5ms |
| 音节表预热 | **0.7ms**（原 839.9ms） | 5ms ✅ |

> `decode_bench` **不装副索引**，测不到简拼路径，所以另加了 `lookup_bench`。
> 看延迟要看 `lookup_bench`——IBus 侧是同步的，每次按键都付这个代价。

### 引擎内部截断常量（解读指标时必须记得）
- `MAX_CANDIDATES = 100` — 候选总数上限
- `MAX_LOOKUP_RESULTS = 500`（`dict.rs`）— 单次词典查询上限
- `BEAM_WIDTH = 16` — 现在只影响整句路径，不再钳制候选数
- `ABBREV_QUOTA = 5` — 简拼层保底名额（**保底不是封顶**）
- 覆盖罚分系数 `0.2` — 翻转点实测 0.25，留 20% 余量

> 前三个是**解读全部历史指标的锚点**，动了纵向对比就断了。
> 派活时把它们写进「不许改」清单，需要改由我裁决。

---

### 三个「拆开看过」的指标（结论）

- **`RECALL_MISS` 208 条与 `RANK_GAP` 84 条不同源**——聚合数字必须按维度拆开；
  三类里只有一类是真缺口。
- **`RANK_GAP` 89 条**：拆完**没有一条值得再追**。
- 差分对拍的逐组演进表（深度 100 口径）见
  [`docs/PLAN-history.md`](docs/PLAN-history.md)。


---

## 六、工作方式约定

- **用户提需求先给方案，不要直接开干**（用户 2026-10-02 明确要求）。
  先写清楚：打算怎么做、改动落在哪些文件、验收线是什么、有哪些备选；
  等用户点头再动手。**例外**：用户明确说「这次直接做」。
- **判据在测量前钉死**。已踩三次：拿「词库内容差异」解释未达标、
  拿「Rust 应该更快」的估算过延迟门槛、以及把「39 个词」夸大一个数量级。
  任务书里要写明「X < 5ms 要有实测数字，不接受论证」。
- **达不到验收线要如实报**，但**调整标准不是执行者的事**。
- **不许删除或改名已有测试来凑绿**。因语义变化需要改断言的，
  必须逐条说明「原来断言什么 / 改成什么 / 为什么新行为对」。
- **聚合数字先拆开看**是不是同一类问题——聚合指标最容易掩盖异质性。
