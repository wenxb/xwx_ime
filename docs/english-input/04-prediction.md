# 04：下一词模型、预测状态与 IBus 协议

上级：[实施任务书](../ENGLISH_INPUT_PLAN.md)。**M1 验收完成后再实施本分册。**
本分册不是把空串传给 lookup_composition，也不是调用大模型生成一句话。

## 1. 明确两个不同的功能

- 有输入字母：例如 hello 后输入 w，用英文上文帮助补全 world。
- 无输入字母：提交 thank 后展示 you，等待用户主动选择。

前者继续使用组合态；后者必须使用独立预测态。
XWX_ENGLISH_PREDICT 只控制无输入面板，默认 0。
已有静态二元数据时，前缀补全仍可利用它；关闭预测面板不等于关闭英文上下文排序。

## 2. 静态二元数据的首版来源

使用 SymSpell 提供的 bigram 文件作为可复现基线，不运行 SymSpell 的纠错算法。

- 仓库：https://github.com/wolfgarbe/SymSpell
- 固定提交：736034a4f0436da6a98c94f83f54ec6de2ec25c6
- 文件：SymSpell/frequency_bigramdictionary_en_243_342.txt
- 下载：https://raw.githubusercontent.com/wolfgarbe/SymSpell/736034a4f0436da6a98c94f83f54ec6de2ec25c6/SymSpell/frequency_bigramdictionary_en_243_342.txt
- SHA-256：fd892a160184101dd7ae807ac5a302d01fcea1c47304181a8ed7ed9c94545bcd
- 实际字节数：5,137,213；实际行数：242,342。不要根据文件名猜行数。

已核对的数据示例：

    hello world 7852480
    looking forward 271821504
    thank you 733376704

这是一份静态、偏书面语的词对频率表，不代表现代聊天或技术英语的最优模型。
保留来源、许可和加工说明；仓库代码的 MIT 不能代替对数据来源条款的记录。
后续可替换语料，但首版先有真实二元统计，不手写几十个词对冒充通用预测。

## 3. 离线构建

新增 scripts/build_english_bigram.py，规划命令为：

    python3 scripts/build_english_bigram.py \
      --input /path/to/pinned-bigram.txt --lexicon data/english.bin \
      --out data/english_bigram.bin

测试用同一脚本读取小型人工 bigram.tsv，不能联网。
解析输入为 prev、next、count 三列，允许空格或 TAB；count 是正 u64。
空行和明确的注释行跳过，其他格式错误必须带行号报错。

处理顺序：

1. prev/next 都按 M1 英文 key 规则校验、转小写。
2. 两个词都必须存在于构建时的英文词库；统计过滤数量。
3. 归一化后重复词对按 checked_add 合并，溢出报错。
4. 对每个 prev 汇总所有保留词对的 count，得到 observed_total。
5. 按 count 降序、next key 升序，每个 prev 只保留 Top-32。
6. observed_total 必须在 Top-32 截断之前计算，不能把截断后的部分伪装成完整行。

产物包含二进制、meta、来源/许可说明。meta 记录构建时 english.bin 的 SHA-256，
便于检测数据过期；运行时不使用跨文件 word_id，不会把旧编号解释成另一词。
静态模型更新和词库更新都要跑完整验收，不能只看文件是否可打开。

## 4. english_bigram.bin v1 布局

little-endian；header → contexts → edges → strings。

### 4.1 64 字节头

| 偏移 | 类型 | 内容 |
|---|---|---|
| 0 | 8 bytes | XWXENB1 后接零字节 |
| 8 | u32 | version=1 |
| 12 | u32 | header_len=64 |
| 16 | u32 | context_count |
| 20 | u32 | edge_count |
| 24 | u64 | contexts_offset=64 |
| 32 | u64 | edges_offset |
| 40 | u64 | strings_offset |
| 48 | u64 | file_len |
| 56 | u64 | reserved=0 |

### 4.2 context 记录：24 字节

    +0  u32 prev 字符串相对偏移
    +4  u16 prev 长度
    +6  u16 该行 edge 数，1～32
    +8  u32 第一条 edge 的数组下标
    +12 u32 reserved=0
    +16 u64 observed_total，必须 >0

context 按 prev 严格升序。

### 4.3 edge 记录：16 字节

    +0 u32 next 字符串相对偏移
    +4 u16 next 长度
    +6 u16 reserved=0
    +8 u64 count，必须 >0

行内按 count 降序、next 升序，无重复 next。
每个 count 以及行内已存 count 总和均不得超过 observed_total。
所有字符串偏移相对于 strings 起点，不带 NUL。
复用 M1 的边界、溢出、字符、排序、资源上限校验模式，文件上限 256 MiB。
读失败只关闭静态二元增强，不关闭已有英文词库、补全或用户学习。
运行时遇到当前词库已无的目标词要过滤，不把它当随机词 ID 使用。

## 5. 英文条件排序

无可信上一词，或静态模型没有该 prev 行时，基础项直接使用 M1 的 z。
不要为了“统一公式”做无意义的指数/对数往返，避免无数据时分数漂移。

存在该行时，设 c 为该词对 count，未存的词对 c=0，T=observed_total：

    p0 = 10^(z - 9)
    p  = 0.8 × c/T + 0.2 × p0
    lexical = 9 + log10(p)
    S = lexical - 0.15 × extra + U + B

U/B 沿用 M1 的有界用户计数增益。Exact 仍在 Completion 前。
这是一种供候选排序使用的平滑频率估计，不宣称覆盖完整语言概率分布。
所有运算保持有限；不能 log10(0)，不能把未知词对直接记成负无穷。
0.8/0.2 是待调参起点，验收前不得称其最优。

## 6. 零输入下一词召回

单独提供 next_candidates(english_context)，不要复用空串组合查询。
候选池 = 静态上一词 Top-32 ∪ 用户上一词 Top-32，最大 64。
按 key 去重，过滤当前词库不存在的词与全局英文屏蔽词，再用上式排序，extra=0。
最多显示 5 个 NextWord 候选。

- 没有静态模型时，仍可用真实学到的词对预测。
- 两种来源都没有时，不展示面板；不要用全局 the/and 等高频词硬填。
- 展示不学习，不改变英文 last_word，不提前生成前导空格。
- 每次预测计算取一次新用户快照，整个计算使用同一代。

只有 Space/数字/点击等明确选择英文词的入口，才在提交成功后尝试开始预测。
标点、Enter 原文、Verbatim 大写、模式切换、焦点清算不触发预测。
不要在低层 record_commit 中无条件开预测，否则标点提交路径也会错误弹窗。

## 7. 预测状态的不变量

新增 CandidateMode::{Composition, Prediction}，默认 Composition。
进入 Prediction 时必须同时满足：

- pinyin 为空，confirmed 为空，caret=0。
- recognized=None，english_mode=false，english_run=false。
- 存在可信英文上一词；列表非空，所有项都是英文 NextWord。
- cursor=0，page 由 cursor/page_size 推导。

不要把 composing() 改成“有预测候选也算正在拼音组合”。
预测选择有专用入口；禁止用 covered=0 的旧语义绕进通用 commit_candidate。
没有下一词候选就返回普通空闲态，不维持一个空的 Prediction 状态。

## 8. 预测态按键表

此表必须变成测试，不允许自行猜空格和回车应该干什么。

| 按键/动作 | 行为 | 是否消费原按键 |
|---|---|---|
| 普通字母 | 退出预测面板，保留英文上文，同一个事件进入普通组合查询 | 是 |
| Space | 关闭预测，保留上一词并标记显式空格，原空格交给应用 | 否 |
| Enter | 关闭预测、清空英文上文，回车交给应用 | 否 |
| Backspace/Delete/Left/Right | 关闭预测、清空英文上文，应用正常编辑 | 否 |
| Escape | 关闭预测并清空英文上文 | 是 |
| 有效数字 1～候选数 | 提交对应下一词，可能继续预测 | 是 |
| 其他数字，包括无第十项时的 0 | 关闭预测和英文上文，数字交给应用 | 否 |
| 无修饰 Tab | 接受当前高亮的下一词 | 是 |
| Shift+Tab | 关闭预测、使英文上文失效，应用处理 | 否 |
| Up/Down | 移动高亮，不查询、不学习 | 是 |
| PageUp/PageDown | 走现有页坐标规则；不足一页时钳位 | 是 |
| 鼠标选择有效候选 | 提交所选下一词 | 由面板事件处理 |
| Ctrl+Delete | 屏蔽该英文目标并重算预测 | 是 |
| Ctrl+Shift+Delete | 撤销最近的真实删除；有有效英文上文时可重新生成预测 | 按是否有可撤操作 |
| Ctrl+空格 | 清预测/上文，切纯英文直通 | 是 |
| 其他标点 | 关闭预测，同一个事件继续原有中文标点逻辑 | 沿用原逻辑 |
| 应用快捷键/未识别按键 | 关闭预测、清英文上文，不改变原始键 | 否 |
| key release | 直接透传，不因释放关闭预测 | 否 |
| FocusOut/Reset/Disable | 清预测和上文，不提交未选择的预测文本 | 无按键返回值 |

Tab 需要新增 InputEvent 和 keyval 翻译，但只在预测态接受候选。
在组合态或空闲态，Tab 保持 Noop；Shift+Tab 不应被折成“接受预测”。
英文直通模式继续在 IBus 翻译层之前 return false，不因为新 Tab 支持而拦截它。

## 9. SessionOutput 的最小扩展

保留原有三个分支，新增：

    CommitAndUpdate { text: String, update: SessionUpdate }
    RefreshAndForward(SessionUpdate)

含义与 IBus 映射：

| 输出 | IBus 动作 | ProcessKeyEvent 返回值 |
|---|---|---|
| Commit | 原有 CommitText + 隐藏 UI | true |
| Updated | 刷新 UI | true |
| Noop | 不额外发 UI/提交信号 | false |
| CommitAndUpdate | CommitText 后刷新预测面板 | true |
| RefreshAndForward | 更新/隐藏 UI，不提交字符 | false |

必须复用 commit_text_only 后调用 refresh_ui，不能调用会 hide_ui 的 commit 再重复提交。
RefreshAndForward 不得 CommitText 一个空格再 return false，否则应用收到两个空格。
原有 append_to_commit 的标点路径不应收到 CommitAndUpdate；在选择入口之外不启动预测。

预测退出后，同一个事件最多走一次普通处理：

    若预测处理已返回完整输出：立即返回
    否则：清预测列表，继续处理该事件一次
    若原普通结果为 Noop 且旧预测需要隐藏：包装成 RefreshAndForward

ToggleAscii 是例外：它本身是输入法命令，预测关闭后应返回空视图 Updated，
不能把“切模式”误解释为将 Ctrl+空格透传；IBus 原有切换入口继续消费该键。
不能递归调用 process_key_event 重放事件，也不能丢掉“需要隐藏 UI”的事实。

## 10. IBus 的三个额外改动

### 10.1 空 preedit 可以有预测候选

refresh_ui 不能仅因 preedit 为空就隐藏候选。
已有“preedit 与候选都空才 hide”的总体判断可以保留。
UpdatePreeditText 的 visible 应取 !preedit.is_empty()；预测面板不显示空的活动组合。
候选仍只发当前页，页内高亮和点击坐标完全沿用现有协议。

### 10.2 焦点清理不能只看 is_active()

预测态 pinyin 为空，旧 is_active() 会返回 false。
reset_state 改用“原先存在组合、确认段或候选 UI”的检查，清空后隐藏 UI。
保持空闲 FocusOut 不乱发信号的旧约束。
不得把当前高亮的预测当成 confirmed 段，FocusOut 不提交它。

### 10.3 直接透传的按键也要使上下文失效

在应用快捷键过滤、未识别 keyval 等 return false 之前补英文失效钩子。
如果有预测就隐藏它；无预测时不发新 UI，不触碰旧中文组合。
释放事件必须先区分，不能与 Ctrl/Alt 等按下事件混在一起清理。
检查 keyval、modifier 组合的真实 D-Bus 测试，不能只测试手工 InputEvent。

## 11. 预测提交的完整例子

    中文模式下输入 thank，Space 选择 thank
      → CommitText("thank")
      → 空 preedit、预测列表 you/...
    Tab 接受高亮 you
      → CommitText(" you")，沿用 M1 延迟空格
      → unigram(you) 和 bigram(thank,you) 各加一次
      → 有后继则继续预测，无后继则正常空闲

另一条：

    thank 已提交，预测 you 正显示
    Space
      → HideLookupTable 等 UI 清理
      → ProcessKeyEvent=false，应用收到一个真实空格
      → 不提交 you，不学习 thank→you
    输入 y/o/u，再选择 you
      → CommitText("you")，不重复加空格

上述两条必须同时有会话测试和子进程 D-Bus 信号测试。
