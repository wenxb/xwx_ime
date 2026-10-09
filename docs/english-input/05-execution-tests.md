# 05：执行顺序、精确验收与交付清单

上级：[实施任务书](../ENGLISH_INPUT_PLAN.md)。每个阶段都应能说明“改变了什么、怎么证明”。
本文件中的 english_eval、english_perf、Python 测试等新命令均为待实现接口，不是现成工具。

## 1. 开始前

在项目根目录执行：

    git status --short
    git diff --stat
    cargo test --workspace --offline
    cargo clippy --workspace --all-targets --offline -- -D warnings

记录已有失败，不能将其隐藏，也不能声称全绿。不要凭 README 的旧测试数量做验收。
保留别人未提交和未跟踪的文件，不能使用 reset/clean 等方式制造“干净基线”。
基线实测若需要模型，明确记录所用中文词库、简拼索引、gram 路径和版本。
不读取真实用户统计用于基准；用户层使用内存实例。

禁止自动运行 just dev、just install、ibus restart；它们可能接管或重启真实桌面输入法。
集成测试使用 tests/support/harness 的私有总线，释放资源使用持有的 Child/PID。
不使用 pkill。

## 2. S0：建立基线资产

产物建议放 artifacts/english/，生成结果可忽略，不把大型数据报告塞进源码。

- 记录上述命令及退出码。
- 记录当前纯中文日常、简拼、编辑语料的 Top-1/Top-9 结果。
- 记录 release 性能基线，不用 debug 时间推断回归。
- 列出要触及的超长文件，先确定拆分边界。
- 保存固定验收语料，调参集与验收集分开，同一单词的不同前缀归同一集合。

现有中文统计命令可先使用：

    XWX_DICT_PATH=/path/to/dict.bin XWX_ABBREV_PATH=/path/to/abbrev.bin \
    XWX_LM_PATH=/path/to/wanxiang.gram \
    cargo run --release --offline -p xwx_engine --example eval_rank -- \
      tests/corpora/daily.jsonl tests/corpora/mixed.jsonl tests/corpora/edits.jsonl

旧 eval_rank 输出召回、MRR、首屏等指标，不直接输出完整 Top-1 表；
Top-1 必须从候选记录补统计或由新 A/B 工具收集，不能把首屏指标改名为 Top-1。
确认日志和模型类型证明真正加载了 gram；仅判断文件存在不够。
候选稳定性比较逻辑字段，不把 min_ms、运行时间和机器元数据也当成必须相等的文本。

当前几个特别需要处理的文件长度：

| 文件 | 2026-10-09 行数 | 处理 |
|---|---:|---|
| engine/lookup.rs | 951 | 先拆查询辅助和整句候选处理 |
| engine/rank.rs | 597 | 本任务不往这里加英文逻辑 |
| engine/mod.rs | 578 | 引擎结构体可先移出，为新字段留空间 |
| session/input.rs | 595 | 抽出 refresh/识别字符插入和新英文分发 |
| xwx_ibus/src/engine.rs | 542 | 抽出 UI 辅助，再加新输出与失效钩子 |

只处理本任务触及文件的结构，不顺手全仓重写。
新改动不能使文件超过 600 行，也不能用 include! 文本拼接、压行来规避限制。

## 3. S1：纯结构拆分，不改变行为

### 3.1 中文查询

- 将 lookup.rs 约第 658 行之后的词条证据/格候选/句子构造辅助移到 lookup_scoring.rs。
- EntryEvidence 可留在 lookup.rs，以保持 abbrev_correct.rs 的导入路径。
- 移走后被 lookup.rs 调用的私有方法，如 push_rescored、ends_one_letter_short，
  调整到最小可见范围 pub(super)，不要直接改成全库 public。
- 将原 lookup_candidates 内“reachable 时追加整句并处理首选”的整块逻辑
  抽为 append_and_promote_sentence，放到 lookup_sentence.rs。
- 该辅助接收 pinyin、ctx、选好的 sentence_lattice 和 &mut ranked；
  原 reachable/长度条件和 tolerant_lattice 选择仍由调用点控制。
- 保持排序、截断、sentence_keys、填充词判断、原文兜底、屏蔽和覆盖映射顺序。

不能因为抽函数就改变打分、重复计算 lattice 或重新获取用户快照。

### 3.2 会话与 IBus

- session/input.rs 的 refresh、insert_recognized_char 可移至 session/refresh.rs。
- handle 可保留为薄包装，原分发成为 handle_composition；新英文边界/预测处理放单独模块。
- 不复制第二份完整按键 match，避免两份空格/回车逻辑漂移。
- xwx_ibus 的 commit、commit_text_only、refresh_ui、hide_ui 移到 engine/ui.rs。
- 引擎聚合结构可移至 engine/types.rs，原公开导入路径用 re-export 保持。
- 位于子模块但被父/同级调用的辅助使用 pub(super)，新字段使用需要的最小可见范围。

通过门槛：旧测试全过；纯中文固定输入结果和上屏序列不变。
这一步不夹带新功能或大范围格式化。

## 4. S2：英文构建器和读取器

只读 [01-data.md](01-data.md) 实现，先用小 fixture 打通 Python 写、Rust 读。

完成：

- 固定格式、清洗、量化、Top-32 前缀索引、meta 与许可产物。
- --entries 离线构建，不依赖 wordfreq。
- --wordfreq 的隔离构建依赖文件，锁定 wordfreq 3.1.1，记录构建依赖版本。
- 英文二进制 reader、损坏文件测试、静态前缀 oracle。

通过门槛：同输入两次输出相同；损坏文件全部返回错误；不是只能查到前几个字典序词。

## 5. S3：候选元数据和英文用户索引

按 [02-engine.md](02-engine.md)、[03-session-learning.md](03-session-learning.md) 实现：

- Candidate 可选英文元数据、构造辅助、旧 JSON 兼容。
- UserLM 派生英文前缀/下一词索引，随同一快照代发布，不改文本格式。
- 一元/二元计数、删除/衰减/加载后的索引一致性测试。

不能给旧 JSON 增加一堆默认字段，也不能为了让编译器安静删掉旧测试。
阶段性接口未调用不等于可以永久添加大范围 allow(dead_code)；M1 验收前必须接线并通过 clippy。

## 6. S4：双通道引擎

完成 lookup_composition、英文有界召回、M1 英文打分、意图表和首屏模板。
原 lookup_candidates 保留为纯中文入口。
先使用合成词频逐个证明分数和分档，后用真实词库调参。

通过门槛：关闭英文完全回退；英文不进中文排序；英语内部和中文内部各自稳定。
新增 trace 证据，便于解释“为什么 EnglishFirst/Mixed/ChineseFirst”。

## 7. S5：M1 会话、生产装配与学习

完成：

- session refresh 实际调用双通道接口。
- 英文提交分流、正确消费长度、延迟空格、英文上文边界。
- 独立英文 UserLM、统一最近删除记录、30 秒及退出回写。
- src/english_config.rs 配置接线；纯库和测试不偷偷读全局环境。
- 测试 harness 显式隔离新增数据与用户路径。

特别检查所有 Noop 出口：

- 空闲 Space 保留英文上文并标记显式空格。
- 会交给应用修改文本/焦点的其他事件要使英文上文失效。
- 包括无候选时 Ctrl+Delete、无撤销记录时 Ctrl+Shift+Delete、空闲数字/Tab 等。
- key release 不进入上述失效路径。

建议在薄 handle 包装中统一检查普通分发结果，避免原 match 内的提前 return 漏掉处理。
识别态、标点、raw 提交仍有各自更明确的边界；不要只依赖 Noop 清理。

## 8. S6：M1 验收与打包资源

- 完成下方 D/C/U/S/I 组测试。
- 新建真实中英混输语料，不覆盖已有 mixed.jsonl。
- 完成英文评测与性能探针，确认生产装配真的加载了英文词库。
- 更新 README/data 文档及安装、deb 资源清单。
- 验证临时 staging 目录内容，不运行会重启真实 IBus 的安装任务。
- 交付 M1 结果、数据哈希、中文回归表和已知限制。

M1 未验收通过不继续堆 M2 状态机。

## 9. S7～S9：M2

S7：二元构建器/读取器、条件打分、真实用户词对召回；先做纯函数测试。

S8：Prediction 模式、两个新增 SessionOutput、Tab、透传清理、焦点隐藏；
先证明状态和信号正确，再开放预测开关。

S9：N 组会话/D-Bus 测试，静态模型缺失/损坏降级，M1 全部回归，报告预测指标。
新输出必须检查所有匹配点，包括 examples、tests/support 中的辅助代码。

## 10. 最小可复用 fixture

新增 tests/fixtures/english/words.tsv，以下为建议的精确数据；实际文件使用 TAB：

| key | display | zipf_centi |
|---|---|---:|
| a | a | 700 |
| i | I | 690 |
| an | an | 600 |
| he | he | 600 |
| you | you | 640 |
| can | can | 620 |
| women | women | 500 |
| hello | hello | 450 |
| help | help | 520 |
| python | Python | 410 |
| docker | Docker | 410 |
| dock | dock | 400 |
| docket | docket | 330 |
| github | GitHub | 400 |
| developer | developer | 440 |
| development | development | 500 |
| word | word | 600 |
| world | world | 510 |
| there | there | 620 |
| thank | thank | 450 |
| them | them | 600 |
| forward | forward | 480 |
| looking | looking | 500 |

中文在原有 fixture 上补充带确定频次的：

    有 / you / 8000
    和 / he / 7000
    餐 / can / 6500
    安 / an / 6000
    我们 / wo men / 5500

不要改旧 shared_fixture 使所有测试默认带英文；新增独立英文 fixture 或显式扩展入口。
前缀截断、两页候选、相同分数等测试另外生成受控词条，不用真实大词表充当夹具。

M2 fixture：

    hello world 80
    hello there 20
    hello you 10
    thank you 90
    thank them 10
    looking forward 100

这些小词对用于精确测试，不代替 M2 的真实二元数据。

## 11. 测试矩阵：数据 D、候选 C

| ID | 必须断言 |
|---|---|
| D01 | 相同来源/参数生成逐字节相同 bin 和 meta |
| D02 | 过滤在基础词数截断之前；大小写去重；人工覆盖优先；非法人工输入报错 |
| D03 | --entries 在没有 wordfreq、没有网络时仍能构建 |
| D04 | 前缀高频词位于字典序末尾，也能进入 Top-32 |
| D05 | 40 个同前缀词的缓存与独立全扫描静态 oracle 一致 |
| D06 | 低频精确词未在前缀 Top-32 中，仍精确召回 |
| D07 | 截短头、坏 magic/version、u64 极值、重叠区段、坏 ID、乱序键都返回错误 |
| D08 | 非 ASCII key、非法 display、重复前缀 ID、q>900、非零 reserved 被拒绝 |
| D09 | 只读数据显式路径缺失不偷偷回退；坏英文不破坏中文查询 |
| C01 | 旧 JSON 无 english 可读，新中文 JSON 不增加空字段；旧 fuzzy 兼容仍在 |
| C02 | English 元数据往返；english+echo 等冲突数据报错 |
| C03 | pyth→Python：Completion、covered=4、S≈3.80 |
| C04 | hello：Exact、covered=5、S≈4.50；精确优先于更高频补全 |
| C05 | 无英文上文 h 不出英文；he 只出精确 he，不展开 hello/help |
| C06 | 有英文上文 h 可以补全；大写直出路径仍不进新候选 |
| C07 | an/he/you/can/women 无英文上文时中文首选，有英文上文时允许英文首选 |
| C08 | hello/python/pyth/docker/docke 按规则 EnglishFirst，不靠单词名单特判 |
| C09 | xi'an、地址、数字/符号、非 ASCII、超长输入不进入新英文查询 |
| C10 | English None 或召回为空时，结果等于旧中文入口，包括旧 JSON |
| C11 | 混排模板精确符合来源顺序；中文非 echo 候选字段和相对序不变 |
| C12 | 中文满 512 项时不因混排丢掉最后 8 个中文候选 |
| C13 | 相同 raw echo 与英文去重；不同大小写原文不错误合并 |
| C14 | 所有查询结果确定，所有英文分数有限，查询不改变用户模型 |

## 12. 测试矩阵：用户 U、会话 S

| ID | 必须断言 |
|---|---|
| U01 | 英文 learn 不增加中文 UserLM 的一元、二元和 entries |
| U02 | 无学习增益为 0；增加目标计数不会使其英文分数降低 |
| U03 | 新旧快照的计数和派生索引成对变化，无半更新代 |
| U04 | 加载、删除、衰减后前缀/下一词索引与独立 oracle 一致 |
| U05 | 静态第 33 名目标学过后可经用户索引召回，不被旧 Top-32 永久挡住 |
| U06 | Python/python 共用 python 计数，文本显示仍使用规范大小写 |
| U07 | 在 pyth 屏蔽 Python 后所有英文前缀、精确、预测均过滤 python |
| U08 | 中文→英文→中文删除交错后，撤销的确是最后一次，不按当前语言猜 |
| U09 | 英文用户文件损坏不覆盖；回写失败保留 dirty；文件不存在可后续正常保存 |
| S01 | pyth 的空格/数字/点击选择都提交 Python，输入完全清空，不残留 on |
| S02 | pyth Enter 严格提交 pyth，不学习、不补空格 |
| S03 | hello 后选择 world 的提交片段为 hello 和前导一个空格的 world |
| S04 | hello 后一个空闲 Space 返回不消费，再选 world 不带额外前导空格 |
| S05 | 两个空闲 Space 不被压成一个，后续也不补第三个 |
| S06 | 中文→英文、英文→中文均不自动添加边界空格 |
| S07 | 查询、翻页、移动高亮、Escape 取消没有新学习 |
| S08 | 有中文 confirmed 时英文查询不沿用更早英文；撤回确认不产生学习 |
| S09 | 最终组合重放时每段仅记账一次，英文不进入中文造词缓冲 |
| S10 | 邮箱/网址、Ctrl+空格、Shift/CapsLock 旧路径不退化 |
| S11 | 空闲应用编辑、快捷键、焦点边界清英文上文，不学习跨边界 bigram |
| S12 | 同一英文序列中的 release 事件不清空英文上文 |
| S13 | 组合内退格删空字母不删除已提交文本；再按一次空闲退格才交给应用 |
| S14 | 中文选择和英文选择都使用当前页/当前高亮，翻页后数字与点击同源 |

U05 可构造 33 个同长度同前缀词：前 32 个 q=382，目标 q=381。
目标一元学习 1 次后，其 U 增益足以超过 0.01 的静态差，且由用户索引进入候选池。
不要用相差 5 个 Zipf 单位的夹具，要求“学 1 次就第一”而违背既定公式。

## 13. D-Bus I 组与下一词 N 组

| ID | 必须断言 |
|---|---|
| I01 | 真子进程加载小英文 bin，UpdateLookupTable 确实有英文，不只是库函数能查 |
| I02 | Space/数字/点击各自仅发一次正确 CommitText，预编辑按期清理 |
| I03 | 英文关闭、无数据、坏数据仍可输入夹具中文 |
| I04 | harness 新增所有路径都指向临时目录，不能回落用户/系统英文词库 |
| I05 | 子进程 SIGINT 和 SIGTERM 都能保存英文尾部计数，中文保存仍正常 |
| I06 | Ctrl+空格英文直通无候选、无 CommitText，原按键不进翻译层 |
| I07 | Shift+H 后 ello 仍直出，真实 modifiers 而非伪造 Char 事件 |
| I08 | 空闲 Ctrl+Delete/Tab 等被应用接管时，新英文上下文确实失效 |
| N01 | thank 后预测 you；hello 后预测 world，生成与展示不学习 |
| N02 | 无静态行/无用户词对就不显示，不补全局高频假预测 |
| N03 | 缺静态模型但已有用户词对，仍能预测；坏静态模型不关闭 M1 |
| N04 | M2 上文使 w 的 world 排在原先高频 word 前，证明条件排序生效 |
| N05 | Tab/有效数字/点击选预测，正确补空格、只学习一次，再决定是否预测 |
| N06 | 预测中 Space：隐藏 UI、返回 false、无 CommitText，不选择预测词 |
| N07 | 预测中 Enter/Backspace/左右/应用快捷键同样透传，无重复事件 |
| N08 | Escape 关闭预测并消费；Shift+Tab 不接受预测 |
| N09 | 输入字母退出预测并只插入一次字母，普通前缀补全继续工作 |
| N10 | 预测时 FocusOut/Reset/Disable 无未选词提交，候选窗一定隐藏 |
| N11 | 预测中 Ctrl+空格隐藏 UI 并切直通，原组合/模式语义不被替换 |
| N12 | CommitAndUpdate 顺序是 CommitText 后显示预测；没有多余 Hide 抹掉新面板 |
| N13 | 空 preedit 的预测 UI 不显示活动预编辑；当前页坐标仍正确 |
| N14 | 预测删除、全部删除、撤销、无效数字、释放事件均按事件表处理 |
| N15 | M2 关闭时无零输入面板，M1 全部行为仍通过 |

N05～N14 必须包含真实 D-Bus 检查，不能只用 SessionOutput 类型作为协议证据。
信号解析错误与“没有收到信号”分别报错，不使用 let Ok(...) else { continue } 吞异常。
所有等待和 D-Bus 调用有超时，失败时保留日志和最近信号用于定位。

## 14. harness 接线要求

在 start_engine_with 的默认分支显式设置：

    XWX_ENGLISH=0
    XWX_ENGLISH_PREDICT=0
    XWX_ENGLISH_DICT_PATH=<临时目录中的缺失路径>
    XWX_ENGLISH_BIGRAM_PATH=<临时目录中的缺失路径>
    XWX_ENGLISH_USER_LM_PATH=<临时目录>/user_english.txt

新英文测试显式启用并传入其 fixture 文件。
不能只改 HOME：系统目录回退仍可能读到 /usr/share/xwx/english.bin。
需要给 harness 增加显式选项/专用启动入口，不能用测试线程修改全局进程环境。
指纹保护扩展到英文用户文件；测试不得自动恢复/覆盖真实损坏文件。

## 15. 真实词库验收

新增独立 JSONL 语料，单文件保持 <600 行：

- english_tuning.jsonl：至少 60 条，只用来调阈值。
- english_acceptance.jsonl：至少 120 条，冻结后用于验收。

固定字段：id、kind、input、target、intent、previous_english、max_rank、expected_first_language、note。
kind 取 query 或 next_word；intent 取 english、chinese、ambiguous。
previous_english 是 null 或规范英文 key；expected_first_language 是 null、english 或 chinese。
max_rank 使用从 1 开始的名次。kind=next_word 时 input 必须为空且 previous_english 非空。
JSON 必须用 serde_json 正式解析，不能复制旧 corpus.rs 的字符串截取解析器。

样例（每行一个对象）：

    {"id":"en-001","kind":"query","input":"pyth","target":"Python","intent":"english","previous_english":null,"max_rank":3,"expected_first_language":"english","note":"前缀补全"}
    {"id":"amb-001","kind":"query","input":"you","target":"you","intent":"ambiguous","previous_english":null,"max_rank":3,"expected_first_language":"chinese","note":"有/you 并存，保护中文首选"}
    {"id":"next-001","kind":"next_word","input":"","target":"you","intent":"english","previous_english":"thank","max_rank":3,"expected_first_language":"english","note":"真实词对预测"}

intent 是人工标注，不能由被测算法输出的 band 反推。
query 行调用组合查询，next_word 行调用独立预测入口，不以空串组合查询代替。
覆盖日常词、技术词、长词、短词、英文/拼音同形、简拼和上下文变化。
大小写按键、地址、原文逃生和完整提交时序由真实会话/D-Bus 测试覆盖，不能用 query 行冒充。
单词分组隔离，不能把 python 放调参集、pyth 放验收集。

评测工具必须报告：

- 词表覆盖率、精确词 Top-1/Top-3、补全 Top-3、中文保护集 Top-1/Top-9。
- 每条失败的实际候选和意图证据，不能只打印一个平均分。
- OOV 单列并仍保留在整体结果，不从分母悄悄删除。
- 学习关闭、学习后的受控场景分开；不使用真实个人用户文件。

英文或 ambiguous 行的目标命中要求 Candidate 有英文元数据，且完整 key 匹配目标小写。
next_word 行还要求 match_kind=NextWord；显示大小写另按规范显示表检查。
仅 word 文本相等的原文 echo 不能算英文候选命中，否则会把旧原文兜底误报为已支持英文。
chinese 行要求非 echo、非英文，且中文 word 等于目标。
expected_first_language 非空时单独断言，不因为目标在 Top-3 就忽略中文首选被抢。

初始发布门槛：固定核心例子全部通过；冻结英文精确集 Top-1 ≥90%、Top-3 ≥95%；
补全集 Top-3 ≥90%；中文保护集 Top-1 与原 Top-9 命中不得退化。
这些是产品目标，不是保证首版自动达到；达不到就报告失败和原因，不能改标注凑通过。

## 16. 验证命令

新增 Python unittest 目录后：

    python3 -m unittest discover -s scripts/tests -p 'test_english*.py'
    cargo test --workspace --offline
    cargo clippy --workspace --all-targets --offline -- -D warnings

使用项目风格格式化改动文件；不要为一次 fmt 把无关文件全部改掉。
模块重排可能触发递归格式化，执行后检查 git diff。

现有中文性能探针：

    cargo run --release --offline -p xwx_engine --example perf_probe -- \
      --repeat 20 --prewarm

新工具约定如下，实施时必须提供对应参数或同步修改此处和测试：

    cargo run --release --offline -p xwx_engine --example english_eval -- \
      --data /path/to/chinese-data --english data/english.bin \
      --corpus tests/corpora/english_acceptance.jsonl --mode on

    cargo run --release --offline -p xwx_engine --example english_eval -- \
      --data /path/to/chinese-data --english data/english.bin \
      --corpus tests/corpora/english_acceptance.jsonl --mode off

    cargo run --release --offline -p xwx_engine --example english_perf -- \
      --data /path/to/chinese-data --english data/english.bin --repeat 20

工具默认英文/中文用户层均为内存实例。M2 增加显式 --english-bigram 参数。
--data 是真正装配中文 dict/abbrev/gram 的目录，不是仅含英文文件的目录。
要求数据的评测发现缺数据必须失败，不能退回空模型后输出漂亮延迟。

性能报告分开记录：

- 数据版本、文件大小、实际加载词数、模型种类。
- 冷启动和首次查询。
- 稳态逐键 p50/p95/max，按场景列出超过 12ms 的样本。
- 英文开/关在相同脚本上的差值，新增部分的计时。
- 用户模型空与受控较大模型各一组，防止索引更新造成隐藏退化。
- D-Bus 场景另测，不把纯会话时间称作完整 IPC 时间。

旧 perf_probe 默认构造器不会自动装英文，因此它本身不是新英文性能的证明。
不要在普通单元测试中用极短机器时间硬断言；release 基准负责性能门槛。

## 17. 最终交付检查

- [ ] M1/M2 完成状态分别列出，不把前缀补全叫作已完成下一词预测。
- [ ] 源码、测试、文档均按结构拆分，改动文件没有超过 600 行。
- [ ] 明确数据源、固定版本、哈希、词数、格式版本和许可文件。
- [ ] 中文关闭对照、英文真实语料结果、失败列表和性能表完整。
- [ ] 所有配置有解析测试，生产入口、安装清单与测试工具实际接上。
- [ ] 英文用户数据独立、debug 隔离、定时/退出回写和损坏恢复均验证。
- [ ] 预测场景有 CommitText/LookupTable/按键返回值的真实协议证据。
- [ ] 没运行真实桌面接管/安装操作，没动真实用户文件，没回滚他人修改。
- [ ] 记录剩余限制：大写直出不预测、无任意中英串切分、无应用全文上下文。

交付报告可写 docs/english-input/IMPLEMENTATION_REPORT.md，超过 600 行时按阶段拆分。
报告必须填实际运行结果；本任务书的目标、样例和预期不能复制成“已通过”的证据。
