# 第 1 批架构设计：候选管线重构 + 输入缓冲重构

> 状态：**待评审**。证据均来自 `tools/compare.py` 的差分对拍与源码实读，
> 每条问题都标注了复现方式。

## 证据基线

对拍语料 `tools/corpus/basic.txt`（64 条输入），对照本机 librime 1.10.0
（`luna_pinyin_simp`）。第 0 批合并后的实测：

| 指标 | 数值 |
|---|---|
| `NCAND_RATIO` 中位数（全量） | **0.0466** |
| `TOP10_OVERLAP` 均值（全量） | **3.20** |
| `TOP1_AGREE`（全量） | 31/52 (59.6%) |
| `MISSING_MULTISYL` | 277 条 |
| 简拼组 `NCAND_RATIO` 中位数 | **0.0004** |

复现：
```bash
./tools/rime_oracle/rime_oracle < tools/corpus/basic.txt > /tmp/oracle.txt
cargo run -q -p xwx_engine --example probe --offline < tools/corpus/basic.txt > /tmp/xwx.txt
python3 tools/compare.py /tmp/oracle.txt /tmp/xwx.txt tools/corpus/basic.txt
```

---

## Block A：候选管线重构

三个缺陷同源，都在 `lookup_candidates`（`lib.rs:339`），必须一起改。

### A1. 候选包含了非起始位置的词（正确性缺陷）

**现状**：
```rust
for path in &paths {
    for spot in &path.words {          // ← 路径上的所有词位
        best.entry((spot.word.clone(), spot.pinyin.clone()))
```

输入 `nihao` 的实测候选：`你好 拟好 好 你 尼 泥 号 毫 逆 拟`。
其中 `好`(#3)、`号`(#7)、`毫`(#8) 覆盖的是第二个音节 `hao`——**用户无法有意义地选中它们**。
librime 同输入的 10 个候选全部起始于位置 0（`你好 妳好 逆号 你 拟 尼 泥 呢 妳 妮`）。

**后果**：每页 10 个候选里约 3 个是废位，直接压低 `TOP10_OVERLAP`。

**改法**：候选只取路径的**首词**（`path.words[0]`），或等价地只取
`lattice[0]` 上的边。

### A2. 候选来源被 beam 宽度钳死（召回缺陷）

**现状**：`lookup_candidates` 只从 `decode_beam(pinyin, ctx, BEAM_WIDTH)` 的
幸存路径里取词，`BEAM_WIDTH = 16`（`lib.rs:84`）。

单音节输入时每条路径恰好一个词，于是**候选数 ≡ beam 宽度**。实测八个
互不相干的单音节输入 `ni hao wo zhong guo ren ta shi` 全部返回**恰好 16**
个候选；`de`=6、`le`=10 是因为词库里该键的词条本就不足 16。

**根因**：beam 同时承担了「路径剪枝」和「候选来源」两个角色。
librime 是分开的——`script_translator` 从词典枚举整段的全部词条作候选，
`poet`/beam 只决定整句首选，两个来源互不钳制。

**关键事实**：`build_lattice`（`lib.rs:407`）**已经**把 `lattice[0]` 上所有
边的词典查询做完了（`collect_chain_edges` 对 `start=0` 枚举了全部音节链
并 `dict.lookup`）。我们只是在取候选时把它们丢掉了。**修复不需要新增任何
词典查询。**

**改法**：候选来源改为 `lattice[0]` 全量边，beam 只用于计算整句路径分。

### A3. 打分口径需要统一（A1+A2 的必然结果）

A2 之后，候选里会同时存在「覆盖整串的词」（`你好` 覆盖 `nihao`）和
「只覆盖前缀的词」（`你` 只覆盖 `ni`）。两者不能直接比分：
当前一个候选的分是「包含它的最优完整路径的总分」，而前缀词没有完整路径。

**改法**：把每个候选的分定义为
**「以该候选开头的最优完整路径分」** = `边自身的分 + 从 edge.end 到串尾的最优补全分`。

后者用一次**后向 DP**（从串尾往前推的 Viterbi）在格上算，O(边数)，
与现有前向 beam 对称。这样所有候选都落在「整串完整解析」的同一量纲上，
`你好`（一个词覆盖全串）自然胜过 `你`（还需 `好/号/毫…` 补全）。

> 注意：后向 DP 的 LM 状态无法精确（LM 是左到右的），补全分只能用
> 词频项 + 近似 LM。这是**刻意的近似**：它只影响候选间的相对排序，
> 不影响 beam 选出的整句首选。实现时把这个近似写进注释，别让后人以为是 bug。

### Block A 预期收益与验收

| 指标 | 现状 | 验收线 |
|---|---|---|
| `TOP10_OVERLAP` 均值 | 3.20 | **≥ 6.0**（主指标） |
| 全拼单字/全拼词组每个输入的 `NCAND` | 恒 16 | **≥ 50**（librime 侧 ≥50 的输入） |
| `MISSING_MULTISYL` | 277 | **≤ 80** |
| 非起始位置候选 | 有 | **0**（新增单测断言） |
| 简拼组指标 | 0.0004 | **不作要求**（Block B 才管） |

> **不要拿 `NCAND_RATIO` 当验收门槛**：`MAX_CANDIDATES = 100` 会把我方候选
> 截在 100，而 librime 的 `guo` 有 336、`shi` 有 351，比例天花板只有 ~0.3。
> 该指标只用于观察趋势。本批**不改** `MAX_CANDIDATES`。

Block A **不依赖 Block B**，可独立交付。

---

## Block B：输入缓冲重构

三项功能共享同一个地基，分开做要返工三次。

### B1. 输入缓冲：从 append-only `String` 到可编辑缓冲

**现状**：`InputSession.pinyin: String`（`session.rs:106`），
`Char(c)` 只 push、Backspace 只 pop；`Left|Right` 直接 `Noop` 透传
（`session.rs:257-258`，代码里自认 TODO）。

**改法**：
```rust
struct InputBuffer {
    raw: String,     // 用户实际敲入的字符（含分隔符 '）
    caret: usize,    // 字节偏移；输入全 ASCII，等同字符位置
}
```
- `Char(c)` → 在 caret 处插入
- `Backspace` → 删 caret 前一个字符
- `Left/Right` → 移动 caret（按音节跳的 Ctrl+←/→ 可后置）
- preedit 光标位置经 `update_preedit_text` 传给 IBus
  （`engine.rs:93` 目前恒为串尾，要改成 caret）

### B2. 音节分隔符 `'`

`translate_keyval`（`ibus_proto.rs:110`）放行 `'`；`build_lattice` 把 `'`
视为**强制切分点**（`xi'an` 只有一种切法）。
注意 `Candidate.pinyin` 与词典 key 用空格分隔，`'` 不能进 `pinyin` 字段。

> 对拍数据显示 librime **不用** `'` 也能把「西安」排进 `xian` 的 top-5，
> 所以 `'` 是消歧的**兜底手段**而非必需品。优先级低于 B1/B3。

### B3. 简拼（zgr → 中国人）

这是量化最严重的缺陷：简拼组 10 个输入里 9 个我们零真实候选，
librime 给出 753~4138 个（`zgr`=4138、`bjdx`=2490、`xx`=3611）。

**难点在存储层**。当前词库是排序后的扁平 mmap 数组，key 为
`"zhong guo ren"`，`lookup_prefix` 是字节前缀二分——`zgr` 无法命中。

三条路线：

| 路线 | 做法 | 代价 |
|---|---|---|
| **(a) librime 式** | 构建期把简码写进索引（`abbrev/^([a-z]).+$/$1/`） | 零运行期开销；索引显著膨胀（1.86M 词条） |
| **(b) libime 式** | 换成音节级 trie，简拼位枚举全部韵母做 DFS | 召回最全、支持混合简拼；要重写整个存储层 |
| **(c) 声母副索引** | 额外建一张「首字母串 → 词条」的排序索引 | 改动最小；纯简拼直达，混合简拼需拆解 |

### B3 spike 结论（已完成，路线 (c) 定稿）

spike 代码与数据：`tools/spike_abbrev/`（`spike.py`、`mixed_fix.py`、
`abbrev_index.bin`、`spike_results.json`）。以下数字均已独立复跑确认。

| 判据 | 阈值 | 实测 | 结论 |
|---|---|---|---|
| (a) 索引膨胀 | > 3× 否决 | 唯一 key 数 **0.74×**（简码高度重合而**减少**）；总体积 1.46×（指针式）/ 2.01×（复制式） | 未否决 |
| (c) 副索引体积 | — | **72.2 MB**（主索引 0.90×），关联数恰为词条数 1.00×，**无组合爆炸** | 好 |
| (c) 纯简拼召回 | top-10 对不上则否决 | `zgr` → **中国人 #1**；`bjdx` → **北京大学 #1** | 通过 |
| (c) 混合简拼 | 完全无解则否决 | `zhongguor` → **中国人 #1**；`zhongg` → 中国 #1；`nih` → 你好 #4 | 通过 |

**选 (c) 而非 (a)**：(a) 体积虽可接受，但它把简拼和全拼混进同一套 key，
简拼候选无法与全拼候选走不同的打分路径，**B4 的分级 credibility 就用不上了**。
(c) 的副索引天然分离两类来源。(b) 召回最强但工作量是 (c) 的 3~5 倍（估算），
(c) 已通过全部判据，不需要付这个代价。

### ⚠ 设计更正：混合简拼方案原先是错的

本文档此前写的「混合简拼按**精确前缀 + 声母副索引**两段拼接处理」
**结构性错误，已作废**。

原因：主索引 key 是**空格分隔**的音节串（`"zhong guo ren"`），而用户输入
**不带空格**（`"zhongguor"`）。拿 `zhongguo` 去做字节前缀匹配，命中数恒为 **0**
（实测：以 `zhongguo` 开头的 key 数 = 0，以 `zhong guo` 开头的 = 20,541）。

**正确方案**——先切音节，再用空格拼回主索引前缀：
1. 从主索引扫出合法单音节集合（实测 **412 个**，可缓存）
2. 对输入做音节切分，枚举前几种切法（`zhongguor` → `['zhong','guo']+'r'`、
   `['zhong','gu','o']+'r'`）
3. 完整音节段**用空格拼接**作为主索引前缀去二分
4. 残余字符作为下一音节声母，对命中词条按「下一音节首字母 == 残余」过滤

### ⚠ 实现期必须解决的风险：混合简拼的扫描量

延迟判据原定「单次 < 5ms」。**这条在原型上没有真正通过**：

| 输入 | Python 原型延迟 | 说明 |
|---|---|---|
| `zgr` / `bjdx` | 0.62 / 0.10 ms | 好 |
| `nh` | 3.5 ms | 勉强 |
| `xx` | 15.5 ms | 命中 10,816 条 |
| `zhongg`（混合） | **210 ms** | **扫描 334,819 个 key** |

spike 主张「Rust 常数下能进 1~3ms」——那是**估算，未验证**。
缓解因素是真实引擎有 `MAX_LOOKUP_RESULTS = 500`（`dict.rs:27`，
mmap 路径在 `dict.rs:449` 应用），不会物化全部命中。

**实现时的硬性要求**：
- 扫描必须**带早停**（凑够 `MAX_LOOKUP_RESULTS` 即止），不能先全扫再截断
- 把「`zhongg` 单次查询 < 5ms」写成一条**基准测试**，作为验收门槛，
  不接受「Rust 应该更快」这类论证

### B4. 与简拼配套：`Candidate.fuzzy` 从 bool 改分级

现状 `fuzzy: bool`（`candidate.rs:37`）只能分两层。简拼必须比模糊音
**更靠后**，且简拼音节越多越不可信。参照 librime 的 `SpellingType`：
```cpp
enum SpellingType { kNormalSpelling, kFuzzySpelling, kAbbreviation,
                    kCompletion, kAmbiguousSpelling, kInvalidSpelling };
struct SpellingProperties { SpellingType type; double credibility; ... };
```
改成「类型枚举 + 连续 credibility」。序列化照 `fuzzy`/`echo` 的
`#[serde(default)]` 做法保持向后兼容。

### Block B 验收

| 指标 | 验收线 |
|---|---|
| 简拼组 `ZERO_ONLY_XWX`（真实候选，排除回声） | **0** |
| 简拼组 `TOP1_AGREE` | **≥ 6/10** |
| 长串中间改字（B1） | 新增会话单测 |
| 现有 87→91 测试 | **一个不许少** |

---

## 实施顺序（修正）

我此前说过「A 和 B 必须一起做否则返工」——**这条修正**：
重新读码后确认，Block A 消费的是「格 + 后向 DP」，Block B 改变的是
「格里有哪些边」和「输入串形状」。B 后做**不会**迫使 A 重写。

真正强耦合的是 **B 内部**（B1/B2/B3 共享输入缓冲与切分层）。

因此建议：

1. **Block A 先做，独立交付** — 价值最高、风险最低、不需要碰存储层
2. **B3 的路线 spike** — 只做测量，不写实现
3. **Block B 整体** — 拿 spike 结论定稿后一次做完 B1+B3+B4，B2 可再往后

## 风险

- **A3 的后向 DP 近似**：LM 状态在反向不精确。若实测排序质量不达标，
  退路是「补全分只用词频项」，更粗但完全可解释。
- **B3 索引膨胀**：若 spike 显示 (c) 的混合简拼覆盖不足，需回到 (b)，
  那是存储层重写，工作量跳一个量级——**spike 必须在动手前做**。
- **对拍口径**：两边词库/LM 不同，`TOP1_AGREE` 只作参考，
  **不得作为验收门槛**。验收看 `TOP10_OVERLAP`、`NCAND_RATIO`、
  `ZERO_ONLY_XWX` 这类结构性指标。
