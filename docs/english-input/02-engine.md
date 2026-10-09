# 02：候选模型、召回、英文排序与中英混排

上级：[实施任务书](../ENGLISH_INPUT_PLAN.md)。先读 [数据分册](01-data.md)。
本分册给出的阈值是首版起点，不是已经测出的最优参数。

## 1. 模块和 API 边界

建议结构：

    crates/xwx_engine/src/english/
      mod.rs          聚合 EnglishEngine、公开接口
      dict.rs         精确/前缀索引
      format.rs       二进制校验与读取原语
      rank.rs         英文内部打分
      context.rs      英文上文值类型
      bigram.rs       M2 静态二元数据
      tests/          按主题拆分
    crates/xwx_engine/src/engine/
      bilingual.rs    双通道查询编排
      blend.rs        意图判定与稳定混排

不要创建一套通用插件框架。EnglishEngine 只需持有：

- EnglishDict：只读词库。
- UserLM：独立的英文用户实例，缺省为 in_memory，可显式指定持久化实例。
- M2 可选 EnglishBigram。
- 零输入预测开关及必要的固定参数。

ImeEngine 增加可选英文组件。原有 new/new_with_lm 不读环境、不自动搜索英文文件。
提供 with_english 和只读 english 访问器；生产入口显式装配，裸测试引擎不变。

新增查询入口，命名固定为 lookup_composition：

    lookup_composition(
        raw_input: &str,
        chinese_context: &LmContext,
        english_context: &EnglishContext,
    ) -> Result<Vec<Candidate>, EngineError>

现有 lookup_candidates 继续表示纯中文查询，作为回归基线和旧调用方入口。
InputSession::refresh 改调 lookup_composition。
新评测必须调用 lookup_composition 或真实会话，不能测旧入口后宣称英文生效。

## 2. Candidate 的最小兼容扩展

不改 SpellingKind 的 Exact/Fuzzy/Abbrev 语义。
在 Candidate 增加可选 english 元数据；推荐定义如下：

    enum EnglishMatchKind {
        Exact,
        Completion,
        NextWord,
    }

    struct EnglishCandidateMeta {
        key: String,                   // 目标完整单词的小写键，不是已输入前缀
        match_kind: EnglishMatchKind,
    }

    Candidate 新字段：
        english: Option<EnglishCandidateMeta>

元数据类型采用 serde snake_case 枚举编码。
新字段序列化使用 skip_serializing_if = Option::is_none；旧候选 JSON 不增加空字段。
Candidate 当前有手写 Deserialize，必须同步扩展内部 Repr，缺省 english = None。
保留旧 fuzzy/spelling 的兼容顺序，不能只给结构体加 derive 后删掉旧解析。

### 2.1 三种来源的字段契约

| 项目 | 中文 | 原文 echo | 英文 |
|---|---|---|---|
| english | None | None | Some(meta) |
| echo | false | true | false |
| pinyin | 原有规范拼音 | 原有原文 | 空字符串 |
| spelling | 原语义 | 原语义 | Exact 占位，禁止进入中文排序器 |
| freq | 中文原有频次 | 原有值 | 0；不能伪造中文频次 |
| score | 中文分 | 原有值 | 英文通道分，不与中文比较 |
| covered | 原有原文坐标 | 原有原文坐标 | Exact/Completion 为 raw_input.len；NextWord 为 0 |
| matched_len | 原有语义 | 原有语义 | 0 |

NextWord 的 covered=0 不是让通用提交函数处理它的理由；它只允许在预测态选择。
Exact/Completion 不能用补全后单词长度填写 covered。
例如 pyth → Python：word=Python、key=python、covered=4，不是 6。

新增构造辅助和 is_english/is_next_word 等小方法，避免在多处手写字段组合。
反序列化英文时检查 echo=false、pinyin 为空、key 合法、display 小写等于 key。
冲突数据报错，不通过“看起来像英文”去修复字段。
搜索所有 Candidate 字面量，显式补 english: None；不要漏掉例子和测试夹具。

## 3. 英文查询的输入门槛

按顺序判断：

1. 没装英文组件：直接调用并返回旧 lookup_candidates，零英文工作。
2. raw_input 为空：返回旧空结果。下一词预测不从这里绕进去。
3. 原文不是纯 ASCII 字母，或长度 >48：只走旧中文路径。
4. 英文查询 key = raw_input.to_ascii_lowercase()；绝不先删除撇号、标点、数字。
5. 无英文上文时：1 个字母不出英文；2 个字母只查精确词，不展开前缀。
6. 有可信英文上文时：允许从 1 个字母开始补全；a、i 的完整词仍可精确召回。
7. 3～48 个字母正常精确查询和前缀召回。

会话中的大写直出和 recognizer 在调用本入口之前已处理，必须继续保持该优先级。
词库自身查询可支持大小写归一化，不表示 Shift+P 已自动改成了组合输入。

## 4. 一次查询只取一代英文用户快照

顺序：

    zh = lookup_candidates(raw_input, chinese_context)?
    snapshot = english.user.snapshot()
    hits = recall_english(key, english_context, snapshot)
    hits = filter_blocked_and_dedupe(hits, snapshot)
    en = rank_english(hits, english_context, snapshot)
    band = classify_intent(raw_input, zh, en, english_context, snapshot)
    return stable_blend(zh, en, band)

如果最终英文列表为空，直接返回原 zh，不能把它重排一遍。
不额外调用中文查询，不重新构建中文 lattice。
同一个英文查询的召回、屏蔽、打分、意图判定必须使用同一个 snapshot。

## 5. 英文召回池

去重前的上界为 129；M1 不含静态二元来源时为 97：

| 来源 | 上限 | 目的 |
|---|---|---|
| 完整 key 精确查询 | 1 | 不受前缀截断影响 |
| 静态 prefix_top | 32 | 高频基础候选 |
| 用户前缀索引 | 32 | 学过的长尾词不能永远被静态 Top-32 排除 |
| 用户上一词 Top-32 中匹配当前前缀的词 | 32 | 词对证据必须能召回，不能只给已入池词加分 |
| M2 静态上文 Top-32 中匹配当前前缀的词 | 32 | 上下文召回补充 |

用户前缀索引的实现见 [学习分册](03-session-learning.md)。
无英文上文时，不查两种上文列表；无静态二元数据时第五项为空。
用户统计中的 key 必须仍能在当前英文词库中找到；升级后已移除的词不凭计数复活。
第 3 节不允许补全的短串，对所有来源统一过滤 key != input_key 的条目，
不能让用户索引或上文列表绕过“两个字母仅精确”的门槛。

按目标完整 key 去重。精确关系由 key == input_key 判定，不按“来自哪个召回列表”判定。
M1/M2 不做编辑距离、子串、任意位置匹配，不扫描整个词库补漏。
任意被屏蔽目标，在所有召回源合并后再统一过滤一次。

这是有界近似召回：不承诺联合个性化分数的全词库数学 Top-K。
但静态前缀索引和用户计数前缀索引各自必须与各自的全扫描 oracle 一致。
不能把有界近似当成借口，使“刚学过一个长尾词”连召回池都进不来。

## 6. M1 英文内部排序

设：

- z = zipf_centi / 100.0。
- extra = word_key.len - input_key.len，只对前缀候选使用，不允许无符号下溢。
- u = 当前词的一元用户选择次数。
- b = 可信英文上一词 → 当前词的用户二元选择次数；无上文时为 0。

初始公式：

    U = 0.6 × log10(1 + min(u, 1000))
    B = 0.9 × log10(1 + min(b, 1000))
    S = z - 0.15 × extra + U + B

实现时先用有符号整数计算 base_centi = q - 15×extra，再除以 100.0 并加 U/B。
这样无学习时的同分关系与离线 Top-32 缓存一致，不受两次浮点舍入影响。
不要用 epsilon 比较器制造非传递排序；使用有限值的全序比较和确定性收尾键。

排序键依次为：

1. Exact 在 Completion 前。
2. S 降序。
3. extra 升序。
4. zipf_centi 降序。
5. key 字节序升序。

分数必须有限，不允许 NaN/Infinity 进入比较器。
零学习计数严格加 0；不拿用户语料总量归一化，不借用中文 USER_COUNT_GAIN_R=1000。
学习增益有上界，避免一个词学多了永久压住全部英文。
排序后最多保留 8 个英文展示候选；内部 pool 最多 129 与展示 8 是两个不同上限。
M2 的条件频率如何替换基础 z 项，见 [预测分册](04-prediction.md)。

## 7. 中文证据的定义

混排器不使用中文 score 的绝对大小。
首版 strong_zh 的定义：zh 列表中至少有一个候选同时满足：

- 非 echo、非英文。
- spelling == Exact。
- covered == 原始输入长度。
- 删除 pinyin 中的音节空格后，逐字等于当前小写输入。
- freq > 现有 FILLER_FREQ_TIER，也就是当前的 100。

这表示“存在带词条频次证据的完整全拼命中”，不是“存在某种中文解释”。
补全、简拼、纠错、freq=0 的拼装句都不能仅靠覆盖整串伪造该证据。
沿用现有 FILLER_FREQ_TIER 的来源，不到处复制数字 100。

这只是保守的初版证据，不声称识别了所有真实中文意图。
若真实验收发现用户造词等场景需保护，应显式扩展来源证据并加测试，
不能将“任何完整候选”直接改成 strong_zh，导致英文永远被简拼压住。

## 8. 意图分档：使用这张决策表

先把 zh 中非 echo 候选记为 Z。第一英文候选为 E0。

定义 good_en：E0 的 zipf_centi >= 300，或其用户一元计数 >= 3。
定义 strong_completion：输入长度 >=3、extra <=4、2×输入长度 >= 目标词长度。
定义 recent_en：会话传入的英文上一词有效，且没有语言/应用操作边界。

按以下顺序匹配，命中即停止：

| 条件 | 分档 |
|---|---|
| 英文列表空 | 不混排，原样返回 zh |
| Z 为空 | EnglishFirst；真实英文在原文兜底前 |
| recent_en 且 good_en | EnglishFirst |
| 无 recent_en，输入长度 <=2 | Mixed，仅精确英文能到这里 |
| strong_zh 且 E0 为 Exact 且 good_en | Mixed，中文仍首选 |
| strong_zh | ChineseFirst |
| good_en 且 E0 为 Exact | EnglishFirst |
| good_en 且 strong_completion | EnglishFirst |
| good_en | Mixed |
| 其他 | ChineseFirst |

用户全局计数可以改善英文内部排序，但不能在无英文上文时绕过 strong_zh 抢中文首选。
recent_en 只影响有实际英文词条/前缀证据的查询；nihao 没有英文召回时仍是中文。
不使用“包含某几个字母”“无法全拼切分”“输入很长”作为单独的英文判据。

## 9. 稳定混排的具体算法

分离 zh 中的原文 echo 为 R，其余候选为 Z；英文候选为 E。
这一步只在 E 非空时执行。
默认页大小仍为 9，不改变 IBus 协议或页内点击坐标。

首屏采用以下来源模板，从相应序列头部依次取一个；该来源耗尽则跳过槽位：

    EnglishFirst: E E Z E Z Z Z Z Z
    Mixed:        Z E Z E Z Z Z Z Z
    ChineseFirst: Z Z Z Z Z Z Z Z E

模板执行完后依次追加剩余 Z、剩余 E、剩余 R。
不丢弃中文长尾；上限为原中文候选上限加 8，而不是再次 truncate(512)。
每个通道内部相对次序不变，不按混合 score 二次排序。

如果一个 R 与真实英文的 word 和 covered 都相同，移除该重复 R。
Python 与原文 python 大小写不同，不当作相同文本；回车始终保留原文出口。
不要泛化为“所有 word 相同就合并”，中文拼音、语言类型和消费范围有各自语义。

关掉英文时：整个 Vec 及旧 JSON 序列化应与旧入口相同。
开着英文时：非 echo 中文候选的字段与相对次序不变；原文兜底允许后移或上述去重。

## 10. 调试与性能要求

复用 engine/trace.rs 的惰性日志模式，记录以下证据：

    input、英文池大小、Exact/Completion、z/U/B/extra/S
    strong_zh、good_en、recent_en、band、最终前 9 个候选的来源

日志关闭时不要先拼接这些字符串，不要在按键路径无条件打印。
普通编辑重新查询，翻页/上下选择只移动现有列表，不重新打分。
不要把每次查询的 snapshot 存到会话直到下一次提交，避免延长快照寿命造成写时大拷贝。
英文开关关闭的路径不打开文件、不访问英文索引、不获取英文用户锁。

### 必须能手算的例子

fixture 中 Python 的 q=410，输入 pyth，无用户计数：

    extra = 6 - 4 = 2
    S = 4.10 - 0.15×2 = 3.80
    类型 = Completion，covered = 4

hello 的 q=450，输入 hello，u=b=0：S=4.50，类型 Exact，covered=5。
输入 you，有高频“有”的全拼词条、无英文上文：strong_zh=true，分档 Mixed。
同样输入 you，但可信上一词为 hello：满足 good_en 时分档 EnglishFirst。
这些例子必须有对应测试，不能只写在注释里。
