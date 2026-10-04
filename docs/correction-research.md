# 智能纠错研究报告（本地证据部分）

> 用户需求（2026-10-04）：「实现智能纠错功能，按照大厂的水准去做，先去研究；
> 比如 "xaing 想"，包括简拼」。本文件是研究阶段的证据与结论沉淀；
> 外部（大厂/开源）调研结论见文末「外部研究」一节（待补）。
> 实测口径：真实词库 `data/dict.bin`（1,864,966 条）+ 万象 gram +
> `data/abbrev.bin`，`--release`，`examples/probe` / `recall_trace`（XWX_USER_LM_PATH 指向临时文件，未动真实用户模型）。

## 一、现状盘点：第③期容错解码已经做到哪一步

引擎已有完整的「音节级容错解码」管线（`engine/lattice/build.rs` 的
`build_tolerant_lattice` + `segmenter.rs::TypoSyllables`）：

- **变体生成**：`TypoSyllables` 把每个音节的 1-编辑变体预展开成静态 map
  （替换/插入/删除/调换四类，Cooper 1983 分类；约 10 万条，启动预热
  `prepare_typo_table`），代价用噪声信道 `-log10 P` 口径：
  替换默认 2.0（命中混淆对则按其 weight）、插入 2.3（方言型 h/g 1.3）、
  删除 2.4、**调换 1.52**（`TYPO_P_TRANSPOSE = 0.03`）。
- **注入位置**：容错格以精确格为底，只在「走不动位置」（`first_stalls`）与
  叠字母位置附近补容错弧；窗口还需通过两道闸：**含元音**（`window_has_vowel`，
  保护 `zgr` 这类纯缩写）与**不能切成 ≥2 个音节**（`window_splits_into_multiple_syllables`，
  保护 `womenjintianquxian` 的 `xian` 不被读成 `xiang`）。
- **预算**：`MAX_TYPO_EDITS = 1`（整条路径）、`MAX_TYPO_WINDOW = 4`、
  `MAX_TYPO_READINGS_PER_WINDOW = 1`（每窗口只留最便宜一条读法）、
  `MAX_TYPO_GAP = 2`、`MIN_TYPO_WINDOW = 2`、`MIN_TOLERANT_INPUT_LEN = 4`。
- **门槛**：①精确路解不出整串/切不开/有叠字母；②分数闸（容错读法要打得过
  现有覆盖整串的答案才整批放入，`lookup.rs` 第二个门槛）。
- **排序**：`(u_min 桶, 层序 Exact<Fuzzy<Abbrev, 分−覆盖罚, 词频)`；
  容错边与模糊音同层（`Fuzzy`）。另外 `entry_tier_exception` 把「覆盖整串 +
  freq>0」的简拼词条**收编进 Exact 层**（`rank.rs::effective_layer`）。

### 现状实测（哪些已经对了）

| 输入 | 意图 | 现状 | 判定 |
|---|---|---|---|
| `nihoa` | 你好 | 你好 **#1**（oa↔ao 调换，3 字母窗口） | ✅ |
| `nhao` | 你好/您好 | 您好 #1、你好 #2（漏打 i） | ✅ |
| `shanghaai` | 上海 | 上海 **#1**（叠字母，结构性信号） | ✅ |
| `nihaoo` | 你好 | 你好 #1（多打 o） | ✅ |
| `beijin` | 北京 | 北京 #1（尾半音节） | ✅ |
| `meiyo` | 没有 | 没有 #1 | ✅ |
| `jainshe` | 建设 | 建设 **#1**（4 字母窗口内调换） | ✅ |
| `wagnzhe` | 望着 | 望着 **#1** | ✅ |
| `zahnshi` | 展示 | 展示 **#1** | ✅ |
| `nih` | 你好 | 你好 #4（残缺尾类，够用） | ✅ |

**结论：双音节/跨音节、窗口 ≤4 字母的换位、漏打、多打、叠字母，已经是大厂水准。**

## 二、缺口清单（附实测与根因）

### 缺口 1：≥5 字母的音节窗口换位彻底失效（用户点名的 `xaing`）

实测 `xaing`（想）：首位「喜爱那个」，**想完全不在候选里**（想爱 #10）。
`xiang` 基线 #1 正常。

- 根因：`MAX_TYPO_WINDOW = 4`——`xaing` 是 5 字母窗口，整个窗口的调换弧
  （swap i,a → `xiang`，代价 1.52）根本不开；引擎退而求其次把串拆成
  `xa`→`xi`(替换 2.0)+`i`→`a`(替换 2.0)+`ng`→`ang`(插入 2.3) 之类的碎读法，
  拼出「喜爱那个」垃圾。
- `xiang` 本体 6 字母（zh 起手的 `zhuagn` 类同理 6 字母），窗口上限 4 把
  「单音节打错一处」里最长的一类全排除了。注释里「替换/漏打/多打/调换的窗口
  都很短」的论断对 `xiang`/`zhuang` 级音节不成立。

### 缺口 2：每窗口只留「最便宜」一条读法，正确的读法被等代价垃圾挤掉

实测 `xang`（想）：首位「轩昂」，丧/桑/嗓 #6~8，**想不在候选里**。

- 根因：窗口 `xang` 的 1-编辑读法里，「sang」(替换 x→s, 2.0)、「kang/hang/…」
  (各 2.0)、「xiang」(插入 i, 2.3)、「ang」(删 x, 2.4) 同时可达；
  `MAX_TYPO_READINGS_PER_WINDOW = 1` 按**代价升序只留一条**——插入读法
  （恰恰是打字行为上最常见的「漏打一个字母」）永远轮不上。
- spike 实证：把该常量放宽到 2，`xaing` 立刻**想 #1**（见下文 spike 一节）；
  `xang` 仍不修——因为替补的是另一条 2.0 替换读法（帮/膀 进了候选），
  「想」需要第 3 条读法或更聪明的选择规则。

### 缺口 3：替换代价无键盘模型，邻键误触与毫不相干字母同价

实测 `nohao`（你好，i 被邻键 o 打掉）：只有「好/号/毫…」前缀候选，
**你好完全缺席**；spike 后还冒出「熬好」#2（`no`→`ao` 替换读法）。

- 根因：`substitution_penalty` 对不在混淆对表里的字母对一律 2.0——
  `o→i`（QWERTY 相邻）与 `n→a`（不相干）同价。噪声信道模型里
  邻键误触概率应显著高于随机替换（大厂键盘纠错的标配）。
- 现有混淆对表只覆盖方言音系混淆（n/l、平翘舌），**不含键盘几何**。

### 缺口 4：纠错读法排序败给「词条级简拼收编」的填充档垃圾

实测 `xain`（先）：喜爱你/心爱你/想爱你 #1~3（freq=100 填充档！），
先 #4；`huanygin`（欢迎）：换衣/环艺 #1~2，欢迎（正确纠错读法）不在前列。

- 根因：`rank.rs::effective_layer` 的收编谓词是 `freq > 0`——freq=100
  是词库的「未知/极低频」约定档（`FILLER_FREQ_TIER = 100`，70% 词条在此档），
  它们不是「词条级证据」，却被收编进 Exact 层，永远压住 Fuzzy 层的纠错读法。
- 这是**层内谓词收紧**（把「词条级证据」的定义做真），不是另起打分口径——
  与 HANDOFF「打分口径只能整层改」的教训不冲突，但必须全量对拍验证。

### 缺口 5：简拼纠错完全不存在（用户点名「包括简拼」）

实测：简拼错字串几乎总能被「别的读法」解释掉（`zgf`→最高峰、`wrsm`→我认识吗、
`wsmj`→我是买家），**没有任何机制**在原串读不出好候选时尝试纠正；
纯缩写窗口也无容错弧（元音闸，防 `zgr` 冲垮是有意设计，正确）。

- 结构困境：3 字母简拼串打错一个字母后仍能覆盖整串（每个辅音字母都能起音节），
  「打错了」没有无歧义的结构信号；大厂靠云端 LM + 用户历史判断，
  单机引擎只能在**证据极强**时才纠（见方案第三节的范围界定）。

## 三、Spike 实证（已验证的修复假设，改动已恢复）

临时改两个常量（`MAX_TYPO_WINDOW 4→6`、`MAX_TYPO_READINGS_PER_WINDOW 1→2`），
`--release` 重测，**结论**：

| 项 | 结果 |
|---|---|
| `xaing` | 想 **#1**（原：喜爱那个 #1，想缺席） |
| 44 输入对拍（`regression_probe --format v1`） | 32 条冻结输入**逐字节零漂移**；漂移仅 `niqhao`/`womenjintianzzgongyuan`/`womemjintianqugongyuan` 三条「故意容错场景」且 top10 不变 |
| 测试 | `cargo test -p xwx_engine` **324 passed / 0 failed**（无常量冻结测试） |
| 性能（`lookup_bench`） | `womenjintianquxian` 0.97ms、`zhongg` 4.2ms、`zgr` 2.8ms——无变化，远在 12ms 线内 |
| 未修 | `xang`（需缺口 2 的读法选择/缺口 3 键盘模型）、`nohao`（缺口 3）、`xain`/`huanygin`（缺口 4） |

⇒ **缺口 1 的修复是「两个常量松绑」，低风险、已被本机证据验证。**

## 四、外部研究：大厂与开源引擎是怎么做的

### 4.1 学术原点：MS 拼音（Chen & Lee, ACL 2000, [P00-1031](https://aclanthology.org/P00-1031/)）

「拼写格 + 混淆概率 + Viterbi」噪声信道框架的完整原型，是所有现代实现的共同祖先：

- 框架：`Ĥ = argmax Pr(P|H)·Pr(H)`；trigram 词级 SLM 与打字模型同进 beam 解码。
- **错误类型学**（引 William 1983 键盘实验）：替换错误里**同行邻键 43%、同列 15%、
  同指异手 10%**；其余为插入/删除/相邻换位——本报告缺口 3 的键盘模型的直接依据。
- 从真实打字语料（100 人 × 8h）训出 `Pr(输入串|音节)`（27 个共享字母状态），
  纠错使真实输入错误率 20.84% → 13.3%，对完美输入的副作用仅 6.82% → 7.45%。
- 后续：[Jia & Zhao 2014, P14-1142](https://aclanthology.org/P14-1142/)（联合图模型 + typo correction）；
  Google [PinyinGPT, arXiv:2203.00249](https://arxiv.org/abs/2203.00249)（假设输入干净，纠错列为 future work）。

**对本项目的意义**：现有「容错格 + typo_penalty + LM 排序」正是这个框架的工程化形态——
架构方向不需要动，差距只在变体覆盖面与排序竞争（见第五节方案）。

### 4.2 libime / fcitx5（一手源码，克隆至 /tmp/xwx/libime）

一句话：**纠错 = 启动期把 QWERTY 同行邻键替换枚举成带 `Correction` 标志的
PinyinMap 变体条目，运行期只查表**，与模糊音同层进同一解码图。

- `pinyincorrectionprofile.cpp`：`mappingFromRows({"qwertyuiop","asdfghjkl","zxcvbnm"})`
  ——只取**同行左右邻键**；对每个音节的每个字符替换成邻键，生成
  `PinyinEntry(flags | Correction)` 补充条目。
- 计价（`pinyindictionary.cpp`）：`fuzzyCost = log10(0.5)` ≈ 0.301/档；
  **Correction 每次 +10 档（≈3.01）、AdvancedTypo +5 档、普通模糊音 +1 档**；
  `isCorrection = fuzzies ≥ 10` 供 UI 提示（`anyCorrectionOnPath`）。
- **AdvancedTypo 五阶段换位表**（`pinyindata.cpp:1126-1285`，就是我们缺口 1 的同类）：
  zh/sh/ch↔hz/sz/cs（reversed initial）、un↔nu、
  双字母尾 {ai,ia,ei,ie,ao,uo,ou,iu,an,en,in} 相邻换位、
  三字母尾 {ang,eng,ing,ong,iao,ian} 换位、四字母尾 {iang,iong} 换位、zhe↔zeh。
- 防爆：beam=20 / frame=40 分层剪枝、每音节 initial 变体 ≤2 / final ≤10、
  nbest 池 1e4、fcitx5 `setScoreFilter(1)`（与最优分差 ≤1 才展示）。
- 产品决策（`fcitx5-chinese-addons/im/pinyin/pinyin.h:158`）：**correction 布局
  Android/iOS 默认 Qwerty（开），桌面默认 None（关）**——移动端虚拟键盘才需要。

### 4.3 librime（一手源码，克隆至 /tmp/xwx/librime；子代理报告的两处错误已修正）

生效的纠错 = **编译期 derive 变体 + NearSearchCorrector（邻键替换 BFS）**。
报告过的「SymSpell 式 EditDistanceCorrector」在 `CorrectorComponent::Create` 里被
`#if 0` 整段停用（corrector.cc:304 注释原话 "Don't use edit distance based
correction for now"）——**上游没在用**，机制可参考但不是 librime 的生产行为。

- **编译期**（`algo/calculus.cc`，常数逐行核实）：`derive/正则/替换/correction`
  生成变体，`credibility` 累加惩罚：模糊音 `log(0.5)`、**correction `log(0.01)`**、
  简拼 `log(0.5)`；补全 `log(0.05)`（syllabifier.cc:28）。变体与正写共存于
  同一张拼写表（`SpellingProperties{type,credibility,tips,is_correction}`，
  `tips` 记原拼写）。
- **key_correction 规则恰好 5 条**（本机 `/usr/share/rime-data/pinyin.yaml:139-146`）：
  `([aeiou])ng$→$1gn`、`([dtngkhrzcs])o(u|ng)$→$1o`、`ong$→on`、`ao$→oa`、
  `([iu])a(o|ng?)$→a$1$2`——即 librime 的「打错容错」就是 5 条手写规则，
  不是通用编辑距离。
- **运行期**（`algo/syllabifier.cc:93-96`）：每个音节段 `ToleranceSearch(..., 5)`
  ——**每段纠错预算 5 个候选**；只对「未被精确拼写命中」的段加 correction 边
  （credibility=log(0.01)）；反向剪枝时 **correction 边免疫**（不被精确路径挤掉，
  syllabifier.cc:194-196）。
- **NearSearchCorrector**（corrector.cc:247-289，生效的那条）：对输入串做 BFS，
  每步把一个字符替换成 `keyboard_map` 邻键，trie 上试走，距离 ≤ threshold 收。
  `keyboard_map` = **同行左右邻键**（corrector.cc:19-66 逐行核实：q↔w、a↔s、
  x↔z,c……**没有上下行邻键**）。替换代价：邻键 1、非邻键 4、删/插 2、换位 2
  （`RestrictedDistance`，被停用的那套里的量化口径，留作参考）。

### 4.4 Gboard（公开论文）

噪声信道统一框架：按键触点 2D 高斯空间模型 `P(touch|word)` × LM × FST 解码
（[arXiv:2410.15575](https://arxiv.org/abs/2410.15575)、[arXiv:2209.11311](https://arxiv.org/abs/2209.11311)）；
联邦学习端上个性化（[research.google blog](https://research.google/blog/federated-learning-for-mobile-keyboard-next-word-prediction/)）。
中文拼音方向 Google 只到 PinyinGPT，纠错明确列为 future work。

### 4.5 大厂产品行为（全量调研完成，证据分级：【官方】/【媒体】/【社区】/【未能核实】）

#### 错误类型覆盖矩阵（●=有明确证据 ◐=笼统提及 ○=查不到）

| 错误类型 | 搜狗 | 百度 | 讯飞 | 微信 | Gboard | iOS 拼音 |
|---|---|---|---|---|---|---|
| 邻键错字母 | ● guanglong→光荣（官方实例） | ●「按键错误自动纠错」 | ◐ 按键纠错 | ● 智能拼写 | ● 空间模型口径 | ◐ 自动改正 |
| 换位/颠倒 | ● AI 纠错「顺序打错」（shoduao→收到） | ◐ | ● gn=ng 实例 | ○ | ● swaps 在口径 | ○ |
| 漏字母 | ●「字母漏输」 | ◐ | ○ | ◐ | ● deletions | ◐ |
| 多字母/多按一键 | ● v5.0「多按一个键的纠错」 | ◐ | ○ | ◐ | ● insertions | ◐ |
| 简拼串内纠错 | ○（简拼=吸收平翘舌，属容错） | ○ | ○ | ○ | ○ | ○ |
| 模糊音 | ● 自定义+智能+错音提示 | ● 含 l=r | ● sh↔s | ● 模糊拼音+易错音 | ○ | ● 可自定义 |
| 整句谐音错字（上屏后，云端） | ● 智写 | ● AI 助聊·校对 | ○ | ● 拼写检查一键修改 | ● Proofread（美英） | ○ |

#### 产品策略共识

- **呈现**：串级纠错候选与正常候选**并列排序**（搜狗 v7.9「支持显示多个纠错候选」、
  右上方纠错专区提示），**不是直接替换**；整句纠错才是「提示 chip→确认」（百度键盘
  右上方、Gboard Fix it chip、iOS 下划线内联）。
- **记忆（搜狗官方明确）**：选中纠错候选后，**下次同一误串直接出该词**
  （guanglong→光荣 选一次即记住）。
- **撤销通道是标配**：iOS 下划线轻点、Gboard 退格撤销、百度「待上屏文字直接编辑/撤销」；
  Gboard 还把「撤销自动纠正」作为**负训练信号**回传。
- **默认开关**：iOS 自动改正默认开【官方】；Gboard 自动更正默认开【第三方一致】；
  微信本地纠错默认开、云端拼写 Plus 默认关【官方隐私政策原文】；
  **搜狗手机版「拼写纠错」是独立开关，且双拼模式下灰色禁用**（官方 FAQ 原文：
  「开启双拼功能，拼写纠错按钮灰色不能开启或关闭」）；讯飞官网核实无任何纠错文案；
  Gboard 无中文拼音模糊音/纠错的官方专项文档（已证伪存在性）。
- **离线/云端分工**：串级拼音纠错**全端侧**（微信官方原文「本地字符串匹配」、Gboard 端侧
  空间模型）；整句/语义词级纠错走云端且可整体关闭。
- **风险对冲**：全部有总开关；事故集中在纠错过激（百度 26 键≈14 键化【社区】）、
  关闭后仍纠（iOS 社区）、双拼场景纠错失效（搜狗/Gboard 双拼【社区】）。

#### 关键来源

搜狗官方更新日志 [pinyin.sogou.com/changelog.php](https://pinyin.sogou.com/changelog.php)（v4.2 邻键+记忆、
v5.0 多按一键、v7.9 多纠错候选）；百度官方帮助/历程 [srf.baidu.com](https://srf.baidu.com/?c=index&e=help)；
微信官方隐私政策 [z.weixin.qq.com/web/privacy](https://z.weixin.qq.com/web/privacy)（本地/云端分工原文）、
官方更新日志；Gboard 官方博客 [The Machine Intelligence Behind Gboard](https://research.google/blog/the-machine-intelligence-behind-gboard/)
（insertions/deletions/swaps 口径、撤销负信号、坏纠正 -15%）；iOS 官方
[support.apple.com/zh-cn/104995](https://support.apple.com/zh-cn/104995)（自动改正默认开、iOS 17 下划线撤销）；
简拼歧义难点佐证 [ACL 2022 PinyinGPT](https://aclanthology.org/2022.acl-long.133/)。
各家纠错候选与合法候选的排序边界公式、纠错候选数量上限、延迟指标——**均无公开资料**。

**对本方案的直接影响**：①串级纠错走「并列排序」正好是本项目现有
lattice 排序形态，方向再次确认；②「选择纠错候选→记忆映射」是搜狗官方明确的产品机制，
可挂在现有用户学习层（user LM/造词）上；③简拼串内纠错大厂无公开实现——阶段 B 的
保守范围有依据；④默认开 + 总开关 + 撤销通道是产品基线。

### 4.6 技术实现机制对照（核心结论）

| 机制维度 | libime（fcitx5） | librime | MS Pinyin / Gboard | 本项目现状与差距 |
|---|---|---|---|---|
| 变体生成时机 | 启动期物化进 PinyinMap | 编译期 derive 进 prism | 运行期模型 | ✅ 同 libime/librime：`TypoSyllables` 启动预热静态表 |
| 变体类型集 | Correction=同行邻键替换（每音节每字符）；AdvancedTypo=系统换位表（zh↔hz、un↔nu、双/三/四字母尾互换、zhe↔zeh）；CommonTypo=jv、din、gn↔ng 等常见笔误 | 仅 5 条手写 derive 规则（ng↔gn、漏 o、ong→on、ao↔oa、ia↔ai）+ NearSearch 邻键替换 | 完整四类（sub/ins/del/swap），数据训练 | ✅ 四类全有且比开源更全（≤1 编辑邻域枚举）；❌ 差「变体覆盖窗口」与「读法选择」（缺口 1/2） |
| 代价公式 | 计数×单价：每档 fuzzyCost=log10(0.5)≈0.301；correction 10 档≈3.01；AdvancedTypo 5 档≈1.51 | 对数惩罚直接记 credibility：模糊/简拼 log(0.5)、correction log(0.01)；距离口径邻键 1/非邻 4/删插 2/换位 2 | 噪声概率训练（27 态 HMM / 2D 高斯空间模型） | ✅ 噪声信道同构：替换 2.0=log10(0.01)、换位 1.52、插 2.3、删 2.4、方言 h/g 1.3；❌ 替换无键盘分档（缺口 3） |
| 防爆 | beam=20/frame=40、每音节 initial≤2/final≤10、scoreFilter(1) | 每段纠错候选≤5、逐行早退、correction 边免疫剪枝 | beam/FST 剪枝 | ✅ 预算五件套（EDITS=1/WINDOW=4/READINGS=1/CHAIN=64/GAP=2）；❌ WINDOW/READINGS 卡太死（缺口 1/2） |
| 排序整合 | 变体与正写同图同价竞争，LM 定胜负 | credibility+词频统一排序式 | 噪声信道×LM 联合 argmax | ✅ typo_penalty 进同域分数；❌ 填充档简拼收编进 Exact 层抢跑（缺口 4） |
| 纠错元数据 | `anyCorrectionOnPath()`（UI 提示） | `tips=origin` + `is_correction` | — | ❌ 无（阶段 C 补 `corrected_from`） |
| 默认开关 | 桌面关、Android/iOS 开（产品裁决） | schema 显式配 derive 规则 | 默认开+撤销负信号 | 待裁决：已裁决 A3 邻接代价默认开 |

**共识（全部一手核实）**：①纠错变体在**索引构建期物化、运行期只查表**；②纠错代价
**显著重于模糊音**（librime log(0.01) vs log(0.5)，libime 10 档 vs 1 档）；③变体枚举
必带预算与免疫规则；④「每窗口/每段只留极少数变体」（libime initial≤2、librime 每段≤5）
靠代价排序选，不靠规则硬编码；⑤候选携带可纠正元数据供 UI/学习。

## 五、方案（待用户裁决后实施）

### 阶段 A：全拼纠错补洞（全部有本地 spike 或大厂依据）

| # | 改动 | 修复目标 | 依据与已验证性 |
|---|---|---|---|
| A1 | `MAX_TYPO_WINDOW 4→6` | `xaing`→想（5-6 字母窗口换位） | **spike 已验证**：想#1；44 对拍冻结输入零漂移；324 测试过；perf 无变化 |
| A2 | `MAX_TYPO_READINGS_PER_WINDOW 1→2`，且两条必须来自**不同编辑类别**（替换类最便宜一条 + 非替换类最便宜一条） | `xang`→想（插入读法不再被替换读法淹没）、`nohao` 铺路 | spike 已验证 1→2 修 `xaing`；「分类各取一条」比放宽到 3 的分支少、且直击 xang 病灶 |
| A3 | 键盘邻接代价表：替换代价分两档——**邻键 1.0~1.3、非邻键保持 2.0**（标定值，A/B 定）。邻接表对齐开源双先例：**同行左右邻键**（libime/librime 一致）；上下同列是否并入由标定实验裁决（MS Pinyin 数据：同列误触占 15%）。**用户已裁决：默认开** | `nohao`→你好（邻键 i↔o 同行误触） | libime `mappingFromRows` 同款邻接表；librime `SubstCost` 邻键 1/非邻 4 的比率共识 |
| A4 | `effective_layer` 收编谓词加 `freq != FILLER_FREQ_TIER`（freq=100 填充档不得收编进 Exact 层） | `xain`→先#1；`huanygin` 类纠错读法浮出 | **spike 已验证**：先#1；44 对拍仅 feicde 一条漂移且方向是改善（费城的→飞的）；20 条纯缩写首位全部不变 |
| A5 | （可选，默认不做）`MIN_TOLERANT_INPUT_LEN 4→3` | `hoa`→好 | 会搅动 `zho`/`sh` 类声母前缀输入，需单独 A/B 证明零害后才考虑 |

**验收线（阶段 A）**：`xaing`→想 top3、`xang`→想 top3、`nohao`→你好 top3、`xain`→先 #1；
`nihao/nhao/nihoa/shanghaai/beijin/meiyo/jainshe/wagnzhe/zahnshi` 不退化；
44 输入对拍 32 条冻结输入逐字节零漂移；daily/mixed/keystrokes/edits 四语料不低于现值
（1.000/0.929/0.877/0.728）；`lookup_bench` A/B 同条件不可区分（12ms 线）；clippy 0；
全测试绿。每步一次提交（A1+A2 → A4 → A3）。

### 阶段 B：简拼纠错（用户点名；开源引擎都没做，范围必须窄）

> 完整论证与一手证据见第六节；这里是实现口径。

现状：简拼错字串几乎总能被别的读法解释（`zgf`→最高峰、`wrsm`→我认识吗），
**没有无歧义的「打错了」信号**——libime/librime 的 correction 都只作用于音节内字符，
不作用于首字母串；大厂在这里靠云端 LM + 用户历史。

设计（**证据闸 + 渐进披露**，两级升级）：

1. **触发闸**（三条同时满足才进入纠错路径，强读法路径零成本）：
   ①纯缩写输入（复用 `pure_abbrev_input` 判据）；②输入 ≥3 字母；
   ③**原串读法弱**：整串候选全为 FILLER 档（freq==100）/回声，或整串读法缺席。
2. **变体生成**：对首字母串做 1 次编辑——**邻键替换**（A3 键盘模型复用，同行左右邻）、
   相邻换位、删一个字母；4-7 字母串变体上限 ~20 个。
3. **渐进披露（两级，成本阶梯）**：
   - 一级（便宜）：每个变体只查 `abbrev.bin` 副索引二分（O(log n)）；
   - 二级（贵）：只有副索引命中 **freq>FILLER 档**词条的变体才升级到**整串简拼解码**
     （`build_abbrev_lattice` 同款链路——`wmjtqbj` 型读法不在副索引里，是链拼出来的），
     升级上限 ≤2 个变体、独立小预算（不与主解码共享 2048）。
4. **呈现**：Abbrev 层内垫底 + 邻键/编辑代价进 typo_penalty 同域 + **分数闸**
   （纠正串最佳候选要明显强于原串最佳，与全拼门槛②同形）+ 小配额（2~3 条，
   复用 `merge_layer`）+ `corrected_from` 元数据（阶段 C 补）。
4. **验收**：20 条纯缩写清单首位逐条不变（冻结）；用 `recall_trace` 扫描产出
   「原串读不出好候选」用例清单作为验收输入；四语料不降；perf 无感。
5. **范围声明**：不做「有读法但可能不是本意」的激进替换（无信号、必砸精度）；
   云端能力（整句谐音纠错）不做——离线单机定位。

### 阶段 C：产品层收尾（小）

- `Candidate` 加 `corrected_from: Option<String>`（librime `tips=origin` 同款），
  先进内部结构/trace；IBus 层「已纠正」提示待实机验证后定。
- `XWX_TRACE` 门槛迹补纠错路径字段（默认关）。
- **纠错记忆映射（讨论中）**：搜狗官方机制——选中纠错候选后，同一误串下次直出该词。
  可挂在现有用户学习层：候选带 `corrected_from` 时，上屏学习除常规词频外，
  记录「误串 → 该词」的拼写映射（可选小文件），lookup 时先查此表直取。
  待裁决：做/不做；做的话数据落 `user_bigram.txt` 新段还是独立文件。

### 不做清单（写明理由）

- 云端/神经网络纠错（讯飞/百度整句、PinyinGPT 方向）：离线单机定位，不引入网络依赖。
- MS Pinyin 式自适应打分权重重训：HANDOFF「打分口径只能整层改」教训 + 需要训练数据。
- 简拼激进替换纠错（见 B5）。
- 3 字母门槛放宽（见 A5，单独 A/B 不通过就不做）。
## 六、简拼专题研究（2026-10-04 深查，一手源码）

> 用户要求：简拼单独深查——大厂/开源怎么做（一手源码优先），本机现状机制，
> 然后出自己的方案。外部机制**只作参考**，方案以本机约束为准。

### 6.1 本机现状机制（一手代码，全链路）

| 环节 | 落点 | 机制 |
|---|---|---|
| 副索引 | `data/abbrev.bin`（72MB，`scripts/abbrev_to_bin.py` 构建） | 每词条取「音节首字母串」为 key（`zhong guo ren`→`zgr`），与主索引同构的排序 mmap 数组，词条区多带全拼字段；运行期 `MmapDict::lookup_abbrev`（`dict/lookup.rs:122`）直接二分 |
| 写法变体 | `dict/abbrev.rs:60` `abbrev_variant_reads` | 每个 `zh/ch/sh` 的 h 保留或删除（2^k 枚举），折叠时带 digraph 真值记录（该音节必须真以 Xh 开头）——**「简拼吸收平翘舌」的自研等价物**（搜狗官方同款语义） |
| 混合简拼 | `dict/lookup.rs:164` `lookup_mixed` | 交错 DFS：每个位置消耗字面音节或一个首字母；要求 ≥1 字面音节（`used_literal`） |
| 静态音节索引 | `segmenter.rs:106` `abbrev_syllables` | 按声母 1~2 字母前缀 O(1) 切片查音节表；**短音节优先**（实测 `w`→`wang` 排前会挤掉 `wo`，`wmjtqbj` 连格子都进不去） |
| 简拼弧 | `engine/lattice/build.rs:100` `build_abbrev_lattice` + `build_lattice_flags` 的 `abbrev` 参数 | 简拼弧 `j`→`ji`/`jin`/…；`ABBREV_PENALTY = 0.5`/弧（log10 域，计入 typo_penalty，**不占容错编辑预算**）；链预算 2048（起点）/768（尾部）共享工作队列 |
| 排序与层 | `engine/rank.rs` | 简拼候选恒 0 桶；桶内层序 Exact→Fuzzy→Abbrev；`effective_layer` 收编「freq>0 且覆盖整串」的简拼词条进 Exact（缺口 4 的病灶）；`ABBREV_QUOTA` 保底配额（不是排序）；`pure_abbrev_input`（≥2，每字母可起音节）/`leading_abbrev_input`（≥3，前两字母不成音节）双判据 |
| 门槛 | `engine/mod.rs` | `MIN_ABBREV_INPUT_LEN=4`（整串解码）、`MIN_ABBREV_TAIL_CLASS_LEN=3`（残缺尾）、`ABBREV_CHAIN_NBEST=4`、`ABBREV_PER_WORD_PENALTY=1.2`（整串读法逐词扣分） |
| 通道 | `DEMOTE_OLD_ABBREV_CHANNEL=true` | 旧 `abbrev_candidates` 通道已随第 8 步删除，简拼/混拼/残缺尾全部由简拼格+条目边供给，**无回退点** |

### 6.2 外部机制（一手源码核实，仅作参考）

**librime** ——「简拼是有代价的派生拼写」：
- 简拼不是查询期行为，是**构建期 algebra 派生**：`data/minimal/luna_pinyin.schema.yaml:75-76`
  `abbrev/^([a-z]).+$/$1/` + `abbrev/^([zcs]h).+$/$1/`，为每词条物化简拼拼写进 prism；
- 代价：`algo/calculus.cc:14` `kAbbreviationPenalty = -0.693…` = **log(0.5)**——与模糊音
  （kFuzzySpellingPenalty）**同价**；`calculus.cc:241-242` 派生时设
  `type = kAbbreviation` + `credibility += penalty`；
- 层级：`algo/spelling.h:18` `kNormalSpelling < kFuzzySpelling < kAbbreviation < kCompletion`；
- **单字词禁令**：`syllabifier.cc:132` strict_spelling 下模糊/简拼拼写不得作为单词的唯一命中
  （防低质量洪泛）；简拼边也不触发重叠拼写检查（`:209`）与前缀补全展开（`:245` 只对
  `type < kAbbreviation` 的拼写做 completion）；
- 用户词典防错学：`dict/user_dictionary.cc:205-214` 注释——`shsh` 这类双折叠歧义串
  **不参与用户词典预查询**（歧义时宁可不学）。

**libime/fcitx5** ——「简拼无代价，全靠 LM」：
- 整个仓库（含 fcitx5-chinese-addons 配置）**没有任何简拼开关**，简拼是内建行为；
- `pinyinencoder.cpp:197` `parseUserPinyin`：`longestMatch` 返回 `(valid, str, isCompletePinyin)`，
  **不完整拼写照样开边**（单字母 `n` 也是一条图边）；
- 声母-only 匹配是一等公民：`MatchedPinyinSyllables` 里 `PinyinFinal::Invalid` 表示
  「只有声母」（`pinyinencoder.cpp:760` 注释：要 `{M,N,R}/Invalid` 而不是 `/Zero`）；
- **无专门简拼代价**——简拼路径与全拼路径在同一格子里靠 LM 分胜负；
- 修正与级联：flagsToTry 三级（Correction→去 Correction→去 AdvancedTypo）逐级退化建图。

**搜狗（官方帮助，产品侧）**：`zly`→张靓颖（首字母简拼）；「简拼天然吸收平翘舌，
即使不开模糊音 zly 也能打出张靓颖」；**双拼模式下拼写纠错灰色禁用**。

**ACL 2022（PinyinGPT 论文）**：简拼输入候选空间爆炸、全拼冻结模型直接迁移性能大跌——
简拼歧义是公认硬问题，靠强上下文/强 LM 才能压住。

### 6.3 对照结论

- **光谱**：libime（零代价、LM 兜底、无开关）←→ librime（有代价、层级、单字词禁令）。
  本机（0.5/弧 + 配额 + 恒 0 桶 + Abbrev 层垫底）在中间偏 librime——**方向正确**。
- **三家共识三件事**：①简拼候选有代价；②防低质量洪泛（librime 禁令 / 本机配额+桶）；
  ③模糊吸收内置（搜狗平翘舌 = librime `[zcs]h` 规则 = 本机 `abbrev_variant_reads`）。
  本机三条都有，不需要补。
- **本机独有优势**：专门简拼副索引（O(log n) 词条级命中）。librime 靠 algebra 物化进
  spelling trie（等价物存在但混在主拼写空间），libime 连索引都没有（靠部分拼写边 +
  trie 前缀）。`wmjtqbj` 型多词链读法两家都要全量解码，我们有共享预算的工作队列。
- **缺口不变**：简拼串内纠错不存在（第二节缺口 5）——两家开源引擎也没有，属本机可
  领先项，但必须窄范围。

### 6.4 简拼纠错——我的方案与论证

**核心判断：全拼「何时可纠」有结构信号（切不开/走不动→容错格），简拼没有——
任何辅音字母串都能被解释掉，打错一个字母后串仍「合法」。所以简拼纠错的难点
不在生成读法，在**触发时机**；时机错了洪泛与误纠同时爆发。**

**为什么不是 librime 路线（构建期物化简拼变体）**：librime 物化的是音节级拼写变体
（每音节常数个派生）；首字母串的纠错变体 = 每个字母 × 邻键数（平均 ~2.5 个），
7 字母串 2.5^7 ≈ 600 条组合，物化不动，**只能查询期生成**。

**为什么不是 libime 路线（零代价边 + LM 兜底）**：零代价纠错边会让每个单字母窗口
开出邻键分支，`wmjtqbj` 的链预算（2048）被乘 2.5^7 直接爆炸；且洪泛候选会冲垮
第一页。**必须在闸外生成、带代价入层。**

**设计（两级渐进披露，对应第五节阶段 B）**：
1. 触发闸（强读法路径零成本）：纯缩写判据复用 + ≥3 字母 + **原串弱**（整串候选全
   FILLER 档/回声，或整串读法缺席——从现有 ranked 结果派生，不加查询）；
2. 变体：1 次编辑 = 邻键替换（A3 表）/相邻换位/删 1 字母，上限 ~20；
3. 一级：逐变体副索引二分（O(log n)）；二级：命中 freq>FILLER 词条的变体 ≤2 个
   升级到整串简拼解码（链路，独立预算）；
4. 呈现：Abbrev 层内垫底 + 编辑代价同域 + 分数闸 + 小配额 2~3 + `corrected_from`。

**验收线**：正例（邻键误触 3~7 字母）如 `wnjtqbj`（m↔n 邻键）→我们今天去北京、
`gshj`（j↔h）→公司简介、`dhj`（y↔h）→打印机、`zrg`（换位）→中国人、`zgrm`（多打）
→中国人 进 top3；负例 `wsm`/`zgr`/`nh`/`wrsm`（原串读法强）不纠；32 冻结对拍零漂移；
mixed MRR 不退；p95 无感（闸在强路径零成本）。

**用户裁决（2026-10-04）**：①先保守——原串词条级证据弱才纠 + 分数闸兜底仲裁；
②换位进变体集；③纠错候选上限 2 条。

## 七、实施记录

### 7.2 阶段 A 第二批（A4 填充档不收编进 Exact 层）

**改动**：`engine/rank.rs::effective_layer` 的收编谓词 `freq > 0` →
`entry_has_evidence(freq) = freq > FILLER_FREQ_TIER`。

**三条实测依据**（第二条是本批调出来的关键教训）：

1. `freq = 100`（词库填充档，约 70% 词条）享受「词条级证据」待遇被收编，
   `xain` 前三名全是填充档「喜爱你/心爱你/想爱你」，「先」排第 4；
2. **`freq == 0`（多词拼装候选/用户造词）不是词条级证据**——第一版谓词写成
   `freq == 0 || freq > 100`，结果 `xaing` 的拼装候选「喜爱那个」(freq=0,
   分 -31.4) 被收编进 `Exact` 层抢在「想」(`Fuzzy`，分 -16.2)之前，首位掉到
   第 5。收编的本义是「整串命中**词库条目**」（`effective_layer` 文档原话），
   拼装候选没有词条资格；
3. `feicde`：「费城的/飞驰的」（freq=100 填充档）退出前三，「飞的」上浮——
   与 spike2 预测一致。

**实测**：`xain`→先 #1、`xaing`→想 #1（不变）、`xang`→想 #3（从 #7 改善）、
`womenjtqbj`→我们今天去北京 #1（不变）。

**对拍**：对 A1+A2 结果只漂移 1 块（`feicde` #3 费城的→飞的，改善）；
对原始基线累计 38/44 相同。

**测试**：330 passed；clippy 干净。

**范围外记录**：`huanygin`→欢迎 需要 3 字母轮换（`gin`→`ing`，两次编辑），
超出 `MAX_TYPO_EDITS = 1`，属后续阶段问题，不是本批回归。

### 7.3 阶段 A 第三批（A3 键盘邻接代价 + 名额 3 + 漏打型保底）

**改动**：

| 位置 | 改动 | 依据 |
|---|---|---|
| `segmenter.rs` | `substitution_penalty` 增加第二优先级证据：**QWERTY 同行左右邻键**（`is_qwerty_neighbor`，`TYPO_P_ADJACENT = 0.05` → 代价 1.3），混淆对 > 邻键 > 默认 | MS Pinyin(ACL 2000) 类型学：同行邻键占替换错误 ~43%；libime `mappingFromRows`/librime `keyboard_map` 均只收同行 |
| `engine/mod.rs` | `MAX_TYPO_READINGS_PER_WINDOW` 2 → **3** | 见下「两条实测共同压出来」 |
| `engine/lattice/build.rs` | 选取规则改为：**代价并列组** → **漏打型保底一个名额**（音节比窗口长的读法）→ 其余按排序补 | 同下 |

**「两条实测方向相反」的教训**（本批的核心设计冲突）：

- `xang`(想)：需要**漏打型** `xiang`(2.40)进格子，但它按代价排在十几条替换读法
  (2.00)之后——纯按代价取永远轮不上 ⇒ 需要「类别/形态多样性」；
- `nohao`(你好)：窗口 `no` 的最便宜读法是混淆对 `lo`(0.10)，邻键 `ni`(1.30)
  排第二 ⇒ 纯按代价取 2 条时 `ni` 在格子里 ✓，但「类别优先」规则会把名额送给
  漏打型 `nao`(2.40)、漏打型名额再送 `nou`(2.40)，`ni` 反而出局。

两者同时成立的取法：**名额 3 = 代价并列组 + 漏打型保底一个名额 + 其余按代价**。
漏打型（音节比窗口长 = 用户少敲一个字母）是打字行为上最常见的一类，保底进格子；
「谁对」交给打分器。窗口子序列读法（窗口只多打字母）不再需要专门让位——漏打型
名额天然只收「比窗口长」的读法，多打形态仍由空词弧表达。

**实测**：

| 输入 | A4 后 | A3 后 |
|---|---|---|
| `nohao` | 好 #1，**你好整个缺席** | **你好 #1** |
| `xaing` | 想 #1 | 想 #1（不变） |
| `xang` | 想 #3 | 想 #3（不变） |
| `xain` | 先 #1 | 先 #1（不变） |
| `womem` | 我们 #1 | 我们 #1（不变） |
| `nihoa`/`nhao`/`shanghaai`/`beijin`/`meiyo`/`jainshe` | 达标 | 不变 |
| 正确输入（`xiang`/`women`/`womenjintianqugongyuan` 等） | #1 | 不变 |

**对拍**（44 块）：A4 → A3 增量漂移 **4 块**，全部是故意打错场景的 #3 名次重排
（top1/top2 不变）：`qnihao` #3 拟好→七号、`niqhao` #3 拟好→你瞧、`nihoa` #3
你好啊→你花、`shenem` #3 神峨眉→深恩。对原始基线累计 **36/44 相同**，8 块变化
全部在打错场景。

**性能**（`lookup_bench` p95，基线 → A3 后）：`womenjintianquxian` 1.03→0.95ms、
`zhongg` 4.23→4.03ms、`zgr` 3.16→2.80ms、`xx` 4.33→4.29ms、`zh` 9.32→9.24ms——
全部同档或更好。容错场景 `min_ms`：打错串 +3~12%（最重 `womenjintianqugonguan`
2.04→2.28ms），正确 22 字母串 1.390→1.378ms（无变化）。

**测试**：`cargo test -p xwx_engine` **329 passed / 0 failed**（`typo_window` 增至
6 条，新增 `nohao→你好 #1`）；clippy 干净。
（勘误：此前记录的「330 passed」把临时 dump 测试算进去了，A1+A2 提交
cebc2fb 里残留过一个 `typo_dump_tmp` 模块，本批起移除，真实测试数为 329。）

### 7.1 阶段 A 第一批（A1 窗口上限 + A2 每窗口读法数）

**改动**（`xwx_engine`）：

| 位置 | 之前 | 之后 | 依据 |
|---|---|---|---|
| `engine/mod.rs` `MAX_TYPO_WINDOW` | 4 | **6** | 单音节打错一处里最长的一类是 5~6 字母音节；上限 4 时 `xaing`(5)/`zhuagn`(6) 整个窗口不开弧 |
| `engine/mod.rs` `MAX_TYPO_READINGS_PER_WINDOW` | 1 | **2** | 同上一格只留最便宜一条时，正确读法被同价/更便宜的假读法挤掉 |
| `engine/mod.rs` `MAX_TOLERANT_CHAINS_PER_START` | 64 | **128** | 窗口变宽 + 读法变多让容错 DFS 的共享预算更快耗尽：22 字母打错串 `womemjintianqugongyuan` 在 64 下**丢掉整条容错链**（`我没/我们` 从候选消失），128 恢复 |
| `segmenter.rs` `TypoSyllables` | 表值 `(音节, 代价)` | `(音节, 代价, 是否替换类)` | 供调用方按编辑类别选取 |
| `engine/lattice/build.rs` 读法选取 | 全表按代价排序后 `truncate(n)` | ①代价升序、同价**首字母一致**优先、再字典序定序；②**代价并列的读法一起留**；③名额有剩补一条**不同编辑类别**且**不是窗口子序列**的读法 | 见下三条实测 |

**选取规则的三条实测依据**（都是这一批调出来的）：

1. **同价不该由字典序决定**：`mem` 窗口的 `mei`/`men` 同价 2.0，只留字典序第一条
   会把「我们」丢成「我没/我妹」→ 代价并列者一起留（`womem` → 我们 #1）。
2. **类别多样性**：`xang` 窗口的 `sang`(0.70，x/s 混淆对)与十几条替换读法(2.00)
   占满名额，漏打中间 `i` 的 `xiang`(2.40) 进不来 → 第二个名额必须来自不同编辑类别。
3. **窗口子序列的读法让位**：`xang`→`ang`(2.30，多打首字母)这类读法已由
   「吃掉一段字母」的空词弧 + 精确音节表达（代价同域），不该再占容错名额；
   让位后名额落到 `xiang`。实测 `bnihao` → 你好 #2 证明空词弧覆盖这类形态。

**实测**（真词库 1,864,966 条 + 万象 gram + 副索引；`--release`）：

| 输入 | 之前 | 之后 |
|---|---|---|
| `xaing` | 喜爱那个 #1，**想缺席** | **想 #1** |
| `xang` | 想缺席（96 条里没有） | **想 #7** |
| `zhuagn` | — | 装 #2 |
| `womem` | — | 我们 #1 |
| `womemjintian` | — | 我们今天 #1 |
| `nihoa`/`nhao`/`shanghaai`/`jainshe`/`beijin`/`meiyo` | 已达标 | 不变 |
| 正确输入（`xiang`/`zhuang`/`women`/`nihao`/`womenjintianqugongyuan`） | #1 | **不变** |

**对拍**（`regression_probe --format v1`，44 个输入块，过滤 `min_ms=`）：
**39 块逐字节相同**，5 块变化，全部在「故意打错」的容错场景内、逐条点名：

| 输入 | 字段 | 变化 | 原因 |
|---|---|---|---|
| `niqhao` | 候选序 | 你好/倪超/拟好 → **您好/你好/拟好** | 窗口读法数放开后 `hao`/`nhao` 两条读法都能进格子，首位由 LM 定 |
| `nihoa` | 候选序 | #3 你啊 → **你好啊** | 同上（多音节读法进格子） |
| `womenjintianqqugongyuan` | #2/#3 | 我们/我么 → **我盟/我们** | 容错链预算 64→128 |
| `womemjintianqugongyuan` | #1 | 我没 → **我们今天**（我们 #2） | 同上 + `mem` 并列读法一起留 |
| `womenjintianqugonguan` | #2 | 我盟 → **我们** | 同上 |

正确输入的块**零漂移**（含 `womenjintianqugongyuan` → 我们今天去公园 #1）。

**性能**（`lookup_bench`，同机三次；改动前同法测得基线）：

| 输入 | 基线 | 现在 |
|---|---|---|
| `womenjintianquxian` p95 | 1.025ms | 0.95~1.07ms |
| `zhongg` p95 | 4.23ms | 4.20ms |
| `zgr` p95 | 3.16ms | 3.02ms |
| `xx` p95 | 4.33ms | 4.31~4.45ms |
| `zh` p95 | 9.32ms | 9.43~17.7ms（**同构建多次抖动**，与改动无关） |

容错场景的耗时（`regression_probe` 的 `min_ms`）：`womem…` 1.09→1.44ms、
`womenjintianqugonguan` 1.49→2.07ms、`womenjintianqqugongyuan` 1.46→1.83ms——
打错的长串 +20~40%，仍远在 12ms 线内；**打对的 22 字母串 1.393→1.396ms（无变化）**。

**测试**：`cargo test -p xwx_engine` **330 passed / 0 failed**（新增
`engine/tests/typo_window.rs` 5 条钉住上面这张实测表）；clippy `-D warnings` 干净。
注意：`cargo test --release` 会因 `mixed_rejects_nested_mixed` 依赖 `debug_assert`
而假红，测试要按默认（debug）跑。

### 7.4 阶段 B：简拼纠错（2026-10-04，本节是唯一权威实施记录）

**落点**：新文件 `engine/abbrev_correct.rs`（变体生成/两级披露/呈现，298 行）；
`engine/mod.rs` 新常量组；`dict/abbrev.rs` 两个证据探针
（`MmapDict::abbrev_entry_evidence` 词条级、`MmapDict::abbrev_chain_evidence` 链级，
共用新的 `AbbrevIndex::exact_key_best_freq` 精确 key 二分；折叠校验
`syllables_carry_digraphs` 从 `dict/lookup.rs` 移入）；`segmenter.rs` 开放噪声信道
代价口径（`confusion_pair_penalty`/`qwerty_row_neighbors`/`typo_penalty_*`，概率表
仍只有一份）；`engine/lookup.rs` needs_abbrev 尾部一行冷调用
`try_abbrev_correction`。

**机制（与 §6.4 设计的对应与偏差）**：
- 触发闸①纯缩写复用 `pure_abbrev_input`、②`MIN_ABBREV_CORRECTION_INPUT_LEN=3`、
  ③原串弱＝整串候选 freq>FILLER 一个都没有（拼装 freq=0/填充档/回声都不算）；
- 变体 1 编辑 = 换位(1.523) → 删字母(2.398,方言型 1.301) → 同行替换(1.301) →
  **同列替换(1.70,新档,常量 `TYPO_P_ADJACENT_COL=0.02`)**，生成序即代价序，
  去重排除原串，上限 `MAX_ABBREV_CORRECTION_VARIANTS=20`（7 字母串的 `n→m`
  排第 17，恰好保住）；
- 一级披露：逐变体副索引精确二分。词条级＝变体本身是真实词条的首字母串
  （freq>FILLER 才算证据）；链级＝真实词条的首字母串是变体的 ≥4 真前缀
  （`ABBREV_CORRECTION_CHAIN_PREFIX=4`：≤3 字母串在 186 万词条下处处命中，
  无判别力），取**最深**命中，覆盖率＝前缀长/变体长；
- 二级披露：词条级升级 ≤2（零建格成本，直接 `lookup_abbrev` 取条目）；
  链级升级 **≤1**（每条一次简拼格构建，只解证据最强的；实测第二条链级变体
  建格 4.5ms 而其读法在配额竞争中全部落选）——与 §6.4 写的「≤2」不同，
  是实测收敛；
- 呈现：纠正候选 covered:=原串长（删除型变体短 1 字母，多打的字母由编辑代价
  吸收）、只取整串读法、编辑代价扣在词条口径重算之后的分数上（同域相加）、
  层序照走 Abbrev（真实词条经 effective_layer 收编竞争）、配额
  `ABBREV_CORRECTION_QUOTA=2`（用户裁决③）、分数闸
  `ABBREV_CORRECTION_MARGIN=1.0`（纠正最佳领先原串最佳一个数量级；
  原串弱是闸③已判事实，余量拦「垃圾纠正险胜垃圾原串」）。

**验收实测（真实词库+万象 gram）**：
- 正例：`wnjtqbj`（m↔n 同行邻键）→我们今天去北京 **#1**（原：五年级提前布局）；
  `zgrr`（多打 r）→中国人 **#1**（词条级 45 万词频）；
- 负例（原串强读法，闸③挡住，首位逐条不变）：`wsm`→为什么、`zgr`→中国人、
  `nh`→女孩、`wrsm`→我认识吗(freq=205,刚过填充档线)、`zgrm`→中国人民、
  `gshj`→工商局(126360,sh 双字母声母字面读法)、`dhj`→电话机、`zrg`→责任感；
- **验收正例的勘误**：§6.4 曾把 `gshj`→公司简介、`dhj`→打印机、`zrg`→中国人、
  `zgrm`→中国人 列为正例——probe 实测这四条的原串本身就有强字面读法
  （工商局/打火机/责任感/中国人民），保守裁决下**本来就不该纠**，这也是大厂
  同款行为（不跟强字面读法抢）。它们转为负例钉进测试；正例换成实测可触发的
  `wnjtqbj`（链级替换）与 `zgrr`（词条级多打）。

**回归**：44 块冻结对拍 vs 阶段 A 末态**零漂移**；20 条纯缩写清单首位 vs HEAD
**逐条一致**；四语料 MRR 与 HEAD 逐字节同值（1.000/0.860/0.877/0.841——
0.860/0.841 是 HEAD 现值，旧的 0.929/0.728 参考线已过时）；335 tests 全绿
（新增 `engine/tests/abbrev_correct.rs` 5 条）；clippy -D warnings 干净。

**性能**：触发闸挡住时纠错路径零执行；触发时的成本 = 证据探针（20 次
O(log n) 二分，微秒级）+ ≤1 次纠正格构建（实测 1.45~1.69ms）+ 链解码。
`wnjtqbj` 端到端 ≈5.2ms。**一个已查明的布局税**：纠错代码进入二进制后，
纯缩写输入（闸③挡住、纠错一次都不执行）端到端仍 +0.2~0.3ms（zgr 2.8→3.0，
zhongg 4.0→4.4）——bisect 铁证：探针函数体换成空 stub 依旧慢、执行路径逐字节
相同（对拍零漂移），是调用图变化引发 LLVM 布局/内联决策改变的确定性代码生成
效应，不是算法开销；`inline(never)`+`#[cold]` 都挡不住，12ms 线内余量巨大，
接受并记录（后续若要消除：profile-guided 或 codegen-units 调参，另立卡）。

**已知局限**（漏纠不是误纠）：删除型链纠正的平局集——原串多打一个字母、目标
读法是多词链（`wmjtqbjj`→`wmjtqbj`=我们今天去北京）时，各删除变体共享同一条
首词证据（词频与覆盖率全同），先验上无法判别谁对；实测即便把正确变体升级进
解码，跨变体比较时「词数更少的垃圾链」（逐词罚分更轻）仍可能压过真读法。
后果是呈现次优读法（我们今天不仅仅 #1），不产生错误纠正（分数闸守门）。
若将来要补：平局集预算减半筛查解码、按分选优后再全量解码——需给
`build_abbrev_lattice` 加预算参数，另立一步。

**附带发现（不在本阶段修）**：长弱缩写串（8 字母 `wmjtqbjj`、4 字母 `sgjj` 类）
在 HEAD 的建简拼格就要 11~13ms（mixed DFS 步数预算耗尽 + 全预算链枚举），
本来就贴/超 12ms 线——与本阶段无关的既有病理，建议另立性能卡。
