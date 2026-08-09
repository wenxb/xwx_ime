# 交接备忘：只存在于对话里、其他文档没写的东西

> 写于 2026-08-09。`PLAN.md` 记进度、`design-batch1.md` 记设计、
> `gear-audit.md` 记组件清单。**这份记的是「为什么」和「差点走错的路」**——
> 这些原本只在对话上下文里，压缩就没了。

---

## 0. 先看这个：仓库状态有风险

```
git log --oneline    →  只有 1 个提交（b746b45 初始提交）
git status           →  14 项未提交，1020 insertions
未跟踪：PLAN.md docs/ tools/ scripts/abbrev_to_bin.py
        examples/{probe,abbrev_bench}.rs
```

**这轮全部工作都没进 git**。测试当前**全绿**（101 passed / 1 ignored），
是个天然的存档点——继续往下做之前值得先落一个提交。

`data/dict.bin`（80.1 MB）与 `data/abbrev.bin`（68.9 MB）都在 `.gitignore` 里，
不进仓库，靠 `scripts/*.py` 重新生成。

---

## 1. 定过又推翻的判断（别再走一遍）

### ① 「A 和 B 必须一起做，否则返工」——**错的**
我最初认为 Block A（候选管线）和 Block B（输入缓冲）都要动 `lookup_candidates`，
分两次做会返工。**重读代码后不成立**：A 消费的是「格 + 后向 DP」，
B 改变的是「格里有哪些边」和「输入串形状」，B 后做不会迫使 A 重写。
真正强耦合的只在 **B 内部**（B1/B2/B3 共享输入缓冲与切分层）。

### ② 「简拼要给 SYLLABLES 加单字母音节」——**绕开了**
一度以为简拼必须让 `zgr` 能被切成 `['z','g','r']`，就得往
`segmenter.rs:SYLLABLES` 加单字母音节——而那里的注释明确说了不能加
（DP 会产生大量无意义切分）。
**最终方案绕过了这个两难**：`lookup_abbrev`/`lookup_mixed` 返回的词
**本来就覆盖整个输入串**（用户打前缀、拿到整词），所以它们是整串候选，
直接合并进候选列表即可，**根本不进 lattice**。

### ③ 「调排序那 39 个词收益最快」——**夸大了一个数量级**
按字/词拆开后：33 个是单字同音字排序差异（rime-ice 与 luna 的词频数据差别，
**去对齐 librime 不是目标**），多字词只有 6 个，其中 5 个是生僻词
（明体/钟鼓/中古/公族/终古），**真正常用又排得离谱的只有「西安」（第 18 名）**。
教训已写进记忆「判据要在测量前钉死」。

### ④ 「音节表要构建期写进 abbrev.bin（v2 格式）」——**不必要**
实测 `segmenter.rs` 的静态 `SYLLABLES` 有 410 个，词库实际用到 412 个，
差集只有 `fiao`/`tei`，反向差 0。**直接用静态表**即可，
不用引入 v2 文件格式。详见 `PLAN.md` 第三节。

---

## 2. 已知的「刻意代价」（不是 bug，别去"修"）

### A3 后向 DP 的 LM 近似
候选打分用后向 DP 算「从 edge.end 到串尾的最优补全分」，
但 **LM 是左到右的**，后向拿不到精确 LM 状态，每条边只能从「无上文」起评
（bigram 下等价于只拿 unigram 分）。

**已知后果**：`xian` 的「西安」排第 18 名——双音节词拿不到 bigram 增益，
竞争不过高频单字。测试 `whole_path_beam_beats_greedy` 的断言
（`nimen` 首选从「泥」变「你」）也是这个近似的直接体现。

**为什么接受**：它只影响候选**之间**的相对排序，不影响 beam 选出的整句首选
（后者走前向精确 LM）。消除它只能回到「从 beam 幸存路径反查」
（=回到被 `BEAM_WIDTH` 钳死的老路）或引入双向 LM，都不值得。

代码注释在 `lib.rs` 的 `backward_completion` 函数级文档里，写得比这里详细。

### 回声候选的语义
照搬 librime `echo_translator`：**仅当全场一个候选都没有时才冒头**，
不是「总在末尾追加兜底项」。上屏时**不学习**（`c.echo` 时给
`learn_committed` 传 `None`），否则用户每打一次打不出字的乱码串，
引擎就学一个垃圾"词"。

### `fiao`/`tei` 不支持混合简拼切分
刻意不加进 `segmenter::SYLLABLES`——那是主切分路径，动它影响全拼行为。

---

## 3. 验收指标的坑

### `TOP1_AGREE` **不能**当验收门槛
两边词库/LM 不同（我们 rime-ice，librime 用 luna + 八股文 essay.txt），
对齐它等于让实现去迎合对照方的词库。只作参考。

### `NCAND_RATIO` **不能**当门槛
`MAX_CANDIDATES = 100` 截断我方候选，而 librime 的 `guo` 有 336、`shi` 有 351，
比例天花板只有 ~0.3，追它是徒劳。只用于观察趋势。

### `ZERO_ONLY_XWX` 12 → 0 是**症状**修好，不是**能力**
回声候选只让用户看见自己敲了什么、知道按空格能上屏。
简拼真正打出字要等 B3 集成完。别看着这个 0 以为简拼好了。

### 验收线要按**职责范围**画，不能按 overall
我犯过：给 Block A 画了「overall `TOP10_OVERLAP` ≥ 6.0」，
但 Block A 只管 9 组里的 4 组，另外那些零分组（简拼/混合简拼/纯字母）
结构上就不可能靠 Block A 改善。**Block A 按其真实职责是达标的**
（四组均值 4.95 → 6.83），是我的门槛画错了。

### 解读 `RECALL_MISS` 要记得两个截断
`MAX_CANDIDATES = 100`（`lib.rs:61`）、`MAX_LOOKUP_RESULTS = 500`（`dict.rs:27`）。
本轮有 14 个输入的 NCAND 恰好 =100，即被截断。

### 判断「词库真没这个词」的方法与其局限
```bash
# 在 dict.bin 里搜 长度前缀 + UTF-8 词
grep -a -c -P '\x06西安' data/dict.bin
```
**局限**：这只证明词存在，**不证明它挂在匹配的拼音键上**。
我用它算过一个「上界 8.56」去反驳子 agent 的结论，后来发现界太松——
那 11 个「词库有却没召回」的词（蜗/肋/垃/偿/榜）读音根本不是那个输入。
更严谨的做法是解析 `dict.bin` 的 key→词条结构再判断。

---

## 4. 环境与工具的既有事实

- **librime 1.10.0 已装**（`librime-bin`），`fcitx5-rime` 是日用输入法。
  **不需要 `librime-dev`**：头文件按 tag 单独下，直接链 `.so`，全程免 sudo。
- **oracle 必须用 `luna_pinyin_simp`**，默认 `luna_pinyin` 出繁体（妳好/中國人），
  和我们的简体词库没法比对。
- **`user_data_dir` 硬编码在 `tools/rime_oracle/.rime_user`**——
  绝不能指向 `~/.local/share/fcitx5/rime`（日常打字在用）。
  每次跑完我都用 `find ~/.local/share/fcitx5/rime -newermt '-5 minutes'` 验证过没被碰。
- **`.cargo/config.toml` 指定 `target-dir = /mnt/hdd/target/xwx_ime`**。
  不要覆盖 `CARGO_TARGET_DIR`——在别处验证等于没验证（这是记忆里的教训之一）。
- **`cargo build --example probe` 必须加 `-p xwx_engine`**，
  否则报 "no example target named probe in default-run packages"。
- `tools/spike_abbrev/` 是**用完即弃**的测量脚本，不是生产代码。
  里面的 `abbrev_index.bin`（72 MB）是原型产物，可以删。

---

## 5. 协作方式：什么有效

### 有效的做法
- **判据写进任务书再派活**，包括「X < 5ms 要有实测数字，不接受『应该更快』这类论证」
- **明确写「达不到就如实报，但调整标准是我的事、不是你的事」**——
  防止执行者为了凑指标去改设计或动常量
- **允许改测试断言，但必须逐条说明「原来断言什么/改成什么/为什么新行为对」**，
  不许删除或改名。Block A 那三处改动因此可复核，且断言是**加强**不是放宽
- **禁止越界**：「本任务只做存储层，不要接进 lib.rs」——出问题时定位范围小一半
- **子 agent 的报告必须自己复核**：本轮实际抓到过「把推测写成事实」
  （根目录 installation.yaml 的时间归因）和「结论对但举例错」
  （说中古/钟鼓不在词库里，实际在）

### 一个意外收获
两次 503 中断反而帮了忙。如果那个 agent 一路跑完，它会交付一个
**看起来完全达标**的 ①（预热 840ms → 0.9ms、测试全绿——因为它正打算去改
那两个失败的测试）。而里面藏着一个我任务书没覆盖的耦合：
**混合简拼从此依赖 `abbrev.bin` 存在**，而 `lookup_mixed` 的文档注释还写着
「副索引缺失不影响本查询」——注释和行为已经不一致。

是那个失败的测试（`real_abbrev_mixed_lookup` 用 `MmapDict::open()` 不挂副索引）
把耦合顶出来的。**这就是为什么不许改测试凑绿。**

---

## 6. 下一步的完整路径

1. ~~恢复红状态~~ **已完成**：静态音节表替代 v2 文件格式，`abbrev.bin` 回退 v1
   并重新生成，预热 839.9ms → 0.7ms，测试回到 101 全绿
2. ② 简拼接进引擎 + ③ B4 分级排序（设计判断见 `design-batch1.md`）
3. B1 输入缓冲加光标（navigator）
4. 全拼排序调优（就「西安」那一类，范围已收窄）
5. gear 清单里的 P0/P1：punctuator、ascii_composer 的 Shift、造词、
   `,`/`.` 翻页、删候选、recognizer、charset_filter

**最硬的一条约束**：全拼四组的 `TOP10_OVERLAP` = **6.83，不许退化**。
简拼是新增能力，把已经跑通的全拼弄坏就是净亏。
