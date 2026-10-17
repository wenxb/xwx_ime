# 03：会话、上屏、空格、学习与持久化

上级：[实施任务书](../ENGLISH_INPUT_PLAN.md)。依赖 [候选与排序定义](02-engine.md)。
本分册的 M1 行为先落地，下一词面板的额外状态见 [04-prediction.md](04-prediction.md)。

## 1. 三件事必须分开

1. 组合：当前还没交给应用的字母与已确认段。
2. 英文上文：本会话已经实际提交、仍可信的上一个英文词。
3. 预测面板：没有组合字母时展示的建议，不是已经输入的文字。

不要复用 english_mode 表示新英文上文。
它是 Ctrl+空格模式，已有独立行为和测试。

新增 session/english.rs，持有最小状态：

    struct EnglishRunContext {
        last_word: Option<String>,     // 已提交词的规范小写 key
        separator_seen: bool,          // 之后已透传过 ASCII 空格
    }

初始 last_word=None、separator_seen=false。
传给查询器的是只读借用视图，不让查询器改会话：

    struct EnglishContext<'a> {
        previous_word: Option<&'a str>,
    }

提供 EnglishContext::empty()，表示无可信英文上文。
无需保存完整英文句子，也不从 committed_tail 的 16 字符窗口反推完整单词。
reset() 只清组合、候选和预测模式，不清此处的已提交英文上文；
否则刚提交英文后 consume_covered/reset 就会把下一词需要的上文抹掉。
原文、取消、模式切换和焦点等语义边界调用单独的英文上下文清理函数。

## 2. 英文上文何时有效

| 事件 | 对英文上文的处理 |
|---|---|
| 选择并提交英文候选 | 更新为所选完整 key，separator_seen=false |
| 输入、编辑尚未提交的字母 | 保留实际已提交上文 |
| 在组合内部移动光标/翻页/高亮 | 保留 |
| 空闲时透传一个 ASCII 空格 | 保留上一词，separator_seen=true |
| 提交中文候选 | 清空英文上文 |
| 提交原文、Enter 原文、echo、大写组合原样上屏 | 清空，不从原文提取英文学习 |
| 实际输入标点 | 词提交处理完成后清空 |
| 进入邮箱/网址识别态 | 清空，不把地址当英文上下文 |
| Escape 取消组合 | 清空，作为显式结束本次英文上下文 |
| 切换中英模式、FocusOut、Reset、Disable | 清空 |
| 空闲时 Backspace/Delete/方向键/Home/End 等交给应用 | 清空 |
| 应用快捷键、未识别但交给应用的按键按下 | 清空 |
| 按键释放事件 | 不改变上文；否则每个 release 都会毁掉连续英文 |

最后两项必须在 IBus 层补钩子，因为这些事件可能在进入 InputSession 前就 return false。
该钩子只作英文上下文失效；不得清掉原有中文组合或发出不必要的 UI 信号。
M2 有预测面板时还需要隐藏预测，详见预测分册。

边界判断是基于可观察事件的保守策略，不是声称知道应用全部编辑动作。
当前没有 SurroundingText：同一输入框内未通知输入法的鼠标重定位无法完全检测。
不要用剪贴板、应用进程读取或猜测光标位置来掩盖这个限制。

## 3. 已确认段与临时上下文

当前 confirmed 段留在 preedit，最后才真正上屏和学习。
M1 英文候选覆盖完整的剩余原文，因此英文选择会结束本次组合，不新增英文半段确认。

有中文 confirmed 段时：

- 查询剩余英文串时，effective EnglishContext 必须为空；前面的中文是语言边界。
- 不能提前写英文用户计数，也不能提前清掉已提交上文的持久状态。
- 退格撤回 confirmed 后，根据实际剩余 confirmed 重新计算有效上下文。
- 最终提交时按旧顺序 replay_confirmed，再记当前英文词；中文重放自然清掉英文链。

不要把临时预编辑文字复制到新的英文 last_word。
不要从已有 push_tail 临时更新推断“这一段已经上屏”。

## 4. 提交函数的修改位置

以 session/commit.rs::record_commit 为来源分流点：

    if candidate.english.is_some():
        提交英文词，走本分册流程
    else if candidate.echo:
        保持原文提交，不学习，打断英文上下文和中文造词
    else:
        保持中文提交，清空英文上下文，走原有中文学习/造词

不能让英文落入现有“非 echo 都 push_coin”的分支。
所有新增判断都读元数据，不靠 word.is_ascii() 猜候选来源。

### 英文 Exact/Completion 提交顺序

1. 校验当前候选不是 NextWord，覆盖长度符合完整剩余串。
2. 在修改英文上文之前保存 prev_key。
3. 根据第 5 节计算实际提交字符串，候选本身的 word 不含前导空格。
4. break_coin_run，结束前面的中文造词段；这不是把中文已确认段丢掉。
5. 对英文 UserLM 执行 learn(prev_key, target_key)，只调用一次。
6. 将实际提交字符串追加到 committed_tail；中文 last_word 设为 None。
7. 英文 last_word 更新为 target_key，separator_seen=false。
8. 消耗原始输入，按原有组合收尾流程复位；M2 再考虑展示下一词。

不要再调用会重复学习或把英文上下文清掉的通用 raw 提交函数。
返回值是实际提交文本，调用方必须使用它，而不是再次直接使用 candidate.word。
这里“提交”指会话生成确定的提交输出，沿用项目既有记账边界。
IBus 没有应用最终接收文本的确认协议；不把学习搬到协议层或新增虚假的接收 ACK。

### 必须检查的调用方

- commit_selection：确认段文本 + record_commit 的返回文本。
- commit_word / commit_immediately：同样保留返回文本和正确消费长度。
- Space、数字选词、CandidateClicked：统一走真实选择路径。
- 标点/Verbatim 结束组合：先提交选择，再追加标点/字符，最后使英文上文失效。
- finish_with_raw / toggle_ascii：严格保留原文，不自动补全、不自动添加英文空格。

commit_immediately 仍是探针快捷接口，不是确认段时序的验收证据。
生命周期测试必须走 handle 和实际按键序列。

## 5. 延迟空格：固定规则，不让实现模型自行发挥

只在提交一个新的英文候选时考虑添加空格：

    needs_space = last_word.is_some() && !separator_seen
    actual_text = (needs_space ? " " : "") + candidate.word

随后 separator_seen=false，因为应用中的最后一个字符又是新词末尾。

约束：

- 不在英文候选后追加尾随空格。
- 不在中文与英文之间自动插空格。
- 不在 raw/Enter/echo 的原文前添加空格。
- 用户已经显式输入空格时，不重复补。
- 不回删用户已经输入的空格；连续两次空闲空格就是两个空格。
- 标点、应用操作、焦点等一旦使上下文失效，就不能继续补旧英文链的空格。

完整例子，竖线表示一次提交边界：

    选择 hello | 输入 world 并选择它  => "hello" + " world"
    选择 hello | 空闲 Space | 选择 world => "hello" + 应用收到空格 + "world"
    选择 Python | 选择中文“开发” => "Python" + "开发"
    选择中文“使用” | 选择 Python => "使用" + "Python"
    选择 hello | 输入 wrold 后 Enter => "hello" + "wrold"

最后一个例子刻意不修正也不补空格：Enter 是原文逃生通道，必须可预测。
要原样拼接英文标识符，可使用 Enter 或 Ctrl+空格直通；不要擅自把本规则扩成代码补全。

## 6. 独立英文用户实例，复用 UserLM

EnglishEngine 持有单独 UserLM，不把它装进中文 MixedLM。
词键统一使用完整小写 key，Python/python 的选择共用 python 的统计。
主词库外的 raw 不学习；本任务不支持从任意原文创建新英文词条。

复用现有能力：

- UserLM::learn 与 snapshot 的写时复制发布。
- 一元/二元计数及现有衰减策略。
- block_candidate、unblock 的计数清理规则。
- flush 的 dirty 认领、失败回补、临时文件和原子 rename。
- 损坏文件只读恢复态，不自动覆盖唯一原件。

不复制一套新的 JSON 覆盖写、逐键 fsync 或 Mutex<HashMap> 统计器。
不调用 add_entry 给英文造“拼音词条”，英文 UserLM 的 entries 区保持不用。
历史英文选择留下的计数即使用户随后用应用退格编辑也不反学习；输入法不知道应用编辑结果。

## 7. 必须补用户前缀/下一词派生索引

仅给静态 Top-32 内的词加分不够：被裁掉的个性化长尾根本回不来。
建议在 UserTables 中增加两个不序列化的派生索引，代码拆到 user_lm/english_index.rs：

    english_prefix_top: prefix -> 最多 32 个规范英文 key
    english_next_top:   prev   -> 最多 32 个规范英文 key

只索引 1～48 个小写 ASCII 字母构成的词键；中文表里的汉字不建立这些索引。
分数分别使用一元计数、指定 prev 的二元计数降序，并列按 key 升序。
索引保存 key，不保存指向另一代快照的引用，不写入用户文件。

更新算法：

1. learn 增加某词计数后，枚举该词前缀，把该词与原 Top-32 合并后重排并截断。
2. 若有合法英文 prev，对它的 next_top 作相同更新。
3. 这个增量算法只用于计数增加；其他词不变，所以不需要逐次全表重建。
4. 删除、全局衰减、加载完成时重建派生索引，避免删除后缺位或衰减后并列顺序错误。
5. 重建按计数降序遍历词，向尚未满的前缀列表追加；下一词索引按每个 prev 处理。
6. 重建和计数变更在同一个 UserTables 可写代内完成，再一起发布。

给 UserLmSnapshot 增加 crate 内部的计数、前缀词和下一词访问器。
现有 counts_with 可以复用；不要为了英文公开整个内部 HashMap。
加载器的所有成功出口都要构建索引；未识别或不合法的英文键不进入英文索引。
旧文本文件格式不变，旧计数照常读取；没有索引不是新文件损坏。

测试必须验证：旧快照的计数和索引都不变，新快照的二者同时改变。
不能先发布新计数，再在另一个 Mutex 里慢慢修索引。
不得把长寿命用户快照挂在会话上，否则每次选词都可能复制整张表。

## 8. 屏蔽与撤销

英文屏蔽是“屏蔽目标词”，不是只屏蔽当前前缀。
在独立英文 UserLM 中固定使用：

    block_candidate(target_key, "__english__")
    snapshot.is_blocked("__english__", target_key)

因此在 pyth 下屏蔽 Python 后，python、pytho、下一词预测都不能重新召回该英文词。
同时清理该词的一元计数，以及进入和离开它的二元计数；沿用现有 UserLM 行为。
原文 Enter 不受屏蔽影响；必要的原文 echo 也不是“英文词复活”。
中文屏蔽仍按原来的 (pinyin, word)，不能改成英文全词语义。

### 两个用户实例的统一撤销

不能“先撤中文，失败再撤英文”，也不能按当前高亮的语言决定撤谁。
在 ImeEngine 维护共享的最近一次删除记录，包含语言、键和词。
所有会话通过同一个删除入口写该记录；旧中文 delete_candidate 入口也要接入。
Ctrl+Shift+Delete 撤销真正最近的一次删除，不恢复已经清掉的学习计数。

实现可用 Mutex<Option<UndoRecord>>；删除与撤销串行化，锁顺序固定为 undo → UserLM。
查询不获取该锁；UserLM 回调不得反向获取 undo 锁。
无可用用户实例、删除未实际发生时，不覆盖上一条有效撤销记录。

## 9. 路径与回写接线

默认英文用户文件：

    release: $XDG_DATA_HOME/xwx/user_english.txt
    debug:   $XDG_DATA_HOME/xwx/user_english.dev.txt

XDG_DATA_HOME 未设置时按 HOME/.local/share 解析；测试必须覆盖 HOME/XDG 注入。
XWX_ENGLISH_USER_LM_PATH 显式值优先，不改写为中文路径，不回落系统目录。
英文组件关闭或词库加载失败时不打开英文用户文件。

扩展 src/main.rs 的回写调度：

- 30 秒 tick 分别检查中文、英文 dirty >0。
- 阻塞线程中分别 flush；一个失败不能阻止另一个。
- SIGINT、SIGTERM 都保存两个模型的尾部增量。
- 不在选词路径直接 flush，不启动每词一个后台线程。
- 英文文件损坏沿用只读恢复机制；日志明确指出具体路径和降级。

确认关闭功能后没有新的路径访问与写文件副作用。
新统计索引不能改变中文用户文件的字段语义；不要为本任务迁移或清空已有用户数据。

## 10. 一次完整时序

    输入 h/e/l/l/o
      → 多次查询，只读快照，不学习
    Space 选择 hello
      → 提交 "hello"
      → 英文 unigram(hello) 加 1，无中文学习
      → 英文 last_word=hello，separator_seen=false
    输入 w/o/r
      → 用 hello 作为英文上文查询，仍不学习
    数字或点击选择 world
      → 提交 " world"，消耗 3 个输入字节
      → unigram(world) 加 1，bigram(hello,world) 加 1
      → 英文 last_word=world
    FocusOut
      → 清空英文会话上文，隐藏相关 UI
      → 已提交统计不撤销；下一会话不学习跨焦点词对

每个箭头都应有会话测试；至少一条完整时序必须用 D-Bus 验证提交文本和信号。
