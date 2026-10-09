# 01：词源、构建、索引与加载

上级：[实施任务书](../ENGLISH_INPUT_PLAN.md)。本分册只定义数据，不实现中英意图判断。

## 1. 数据源的固定选择

### 1.1 主词频：wordfreq 3.1.1

- 项目：https://github.com/rspeer/wordfreq
- 版本信息：https://pypi.org/pypi/wordfreq/3.1.1/json
- wheel：wordfreq-3.1.1-py3-none-any.whl
- SHA-256：4b1c6ecffc6198be3396d5cf871c4423ca71c907c231348d352dd54d62b97473
- Python 要求：>= 3.8、< 4。只用于构建，不进入 Rust 运行时。
- 使用英文 large 词表的 iter_wordlist 与 zipf_frequency，不调用随机词接口。
- 数据是约截至 2021 年的使用快照，上游明确表示很可能不再更新数据。
- 软件 Apache 许可与数据许可不同；数据包含 CC BY-SA 4.0 许可内容。

选择它是为了多领域词频，不是因为它保证每个 token 都是正确英文。
不可把它的多词查询频率当成真实 bigram 统计。

### 1.2 技术词和显示形式

维护小型 data/english/overrides.tsv，字段为：

    key<TAB>display<TAB>zipf_centi_override
    python<TAB>Python<TAB>
    github<TAB>GitHub<TAB>
    docker<TAB>Docker<TAB>
    kubernetes<TAB>Kubernetes<TAB>350
    chatgpt<TAB>ChatGPT<TAB>350
    i<TAB>I<TAB>

- key 是小写完整单词，display 的 ASCII 小写结果必须等于 key。
- 第三列为空表示使用 wordfreq 的该词频率；无频率时默认 300。
- 明确填写的第三列是百分之一 Zipf 单位，350 表示 3.50，不是出现 350 次。
- 普通新增技术词不要全部设置为极高频；没有证据时先用 300～350。
- 实现模型可以维护少量真实技术词，不能按验收题目逐项写排序特例。
- 同一个 key 只能有一个 display，重复人工覆盖视为构建错误。

可选导入雾凇英文表作为补充，但不得硬编码本机的绝对路径：

    ~/.local/share/fcitx5/rime/en_dicts/en.dict.yaml
    ~/.local/share/fcitx5/rime/en_dicts/en_ext.dict.yaml

导入规则：只看词面；不搬运 Rime 的拼写派生规则，不导入 Jan → January 之类编码别名。
仅接纳 ASCII 字母词面。补充词没有 wordfreq 频率时用 250，人工覆盖优先。
Rime 的整数权重不等于 Zipf，禁止直接拿来填 zipf_centi。
已有 wordfreq 条目的 display 默认仍为小写，只有人工覆盖可以替换；
不要把 US/May 等大写词面盲目套到常用 us/may 上。

导入上游词表前记录具体来源版本和许可。许可归属没确认的补充文件先不合并，
用独立维护的小型技术词表完成首版；不能把所有下载资源笼统标成 MIT。

### 1.3 不并用第二套未经校准的一元频次

SymSpell 一元表可作为离线对照，不与 wordfreq 的 Zipf 数值直接相加。
SCOWL/ESDB 可供人工清洗参考，首版不增加硬交集过滤，避免删掉技术词、缩写和专名。
M2 所用的二元表是不同用途，见 [04-prediction.md](04-prediction.md)。

## 2. 清洗和选词算法

按以下顺序执行，输出必须确定：

1. 按 wordfreq 英文频率降序遍历；频率并列按规范化 key 的字节序排列。
2. 原词必须全为 ASCII 字母，长度 1～48；单字母仅保留 a 和 i。
3. key = 原词的 ASCII 小写；不是删除符号或去掉重音后的结果。
4. 不接纳空串、数字、带空白、网址、控制字符、撇号、连字符和非 ASCII token。
5. 合并相同 key，保留最高基础频率；默认 display = key。
6. 取清洗后的前 50,000 个唯一 key，而不是先取 50,000 再过滤。
7. 合入可选补充词，再应用人工覆盖；覆盖词不受前 50,000 截断影响。
8. 词库最终按 key 升序，分配从 0 开始的 word_id。

如果基础词不足 50,000，就如实使用全部有效词并报告数量，不能补随机字符串凑数。
不要删除 an/he/you/can/women 等歧义词；它们由混排算法处理。
不要把 don't 变成 dont，也不要把 C++ 变成 c。

Zipf 量化：

    zipf_centi = round_half_up(Zipf × 100)
    合法范围：0～900

构建端可用 Decimal(str(value)) 实现量化，拒绝 NaN/Infinity。
使用 q/100.0 还原排序值；q 不表示真实语料计数。
序列化只保留 q，不在二进制中保存浮点值。

## 3. 构建程序与产物

新增 scripts/build_english_dict.py；超过 600 行时把格式和导入器拆到 scripts/english/。
不要修改现有 build_dict.py，把英文混进中文 dict.bin。

规划的命令接口：

    # 生产构建，需要构建环境中有 wordfreq
    python3 scripts/build_english_dict.py --wordfreq --limit 50000 \
      --overrides data/english/overrides.tsv --out data/english.bin

    # 测试构建：完全离线，只用标准库，不要求安装 wordfreq
    python3 scripts/build_english_dict.py \
      --entries tests/fixtures/english/words.tsv --out /tmp/test-english.bin

两个来源参数互斥。fixture 的列是 key、display、zipf_centi，使用实际 TAB 分隔。
读 --entries 分支时不能在模块顶层提前 import wordfreq。
无效人工输入、重复 key、非法 q、越界长度必须失败并给出文件和行号。
生产语料中按清洗规则跳过的 token 要分类计数；不能把解析异常当普通脏词吞掉。

产物：

- english.bin：运行时只读数据。
- english.meta.json：格式版本、词数、前缀数、参数、源版本/哈希、产物哈希。
- data/english/NOTICE.md 与 LICENSES/：随安装分发的署名、许可和修改说明。

meta 不写当前时间、随机数、机器绝对路径；相同输入必须生成相同 bin 和 meta。
不提交大型中间文本词表；构建在内存中处理或采用临时流式数据。
先写同目录临时产物，校验成功后原子替换。不要原地截断可能正在 mmap 的文件。

## 4. english.bin v1 格式

全部为 little-endian。字符串不带 NUL，使用长度字段。不得直接把 Rust struct 内存写入文件。

### 4.1 64 字节头

| 偏移 | 类型 | 内容 |
|---|---|---|
| 0 | 8 bytes | XWXENG1 后接一个零字节 |
| 8 | u32 | version = 1 |
| 12 | u32 | header_len = 64 |
| 16 | u32 | word_count |
| 20 | u32 | prefix_count |
| 24 | u64 | words_offset = 64 |
| 32 | u64 | prefixes_offset |
| 40 | u64 | ids_offset |
| 48 | u64 | strings_offset |
| 56 | u64 | file_len，必须等于实际文件长度 |

布局：header → words → prefixes → ids → strings。
words、prefixes 都是定长记录；ids 是 u32 word_id 数组；strings 是共用字节区。

### 4.2 每条 word 记录 16 字节

| 相对偏移 | 类型 | 内容 |
|---|---|---|
| 0 | u32 | key 在 strings 内的相对偏移 |
| 4 | u32 | display 在 strings 内的相对偏移 |
| 8 | u16 | key 长度 |
| 10 | u16 | display 长度 |
| 12 | u16 | zipf_centi |
| 14 | u16 | reserved = 0 |

word 记录按 key 严格升序。key 和 display 都满足第 2 节的字母/长度约束。

### 4.3 每条 prefix 记录 16 字节

| 相对偏移 | 类型 | 内容 |
|---|---|---|
| 0 | u32 | prefix 在 strings 内的相对偏移 |
| 4 | u16 | prefix 长度 |
| 6 | u16 | id_count，1～32 |
| 8 | u32 | 在 ids 数组内的起始下标，不是字节偏移 |
| 12 | u32 | reserved = 0 |

prefix 按字节序严格升序。为所有非空前缀建立记录，包括完整单词本身。
词只存一份，前缀记录存 word_id 引用，不复制词面与词频。

## 5. 前缀 Top-32 如何生成

禁止“字典序找到前 32 个就返回”，否则高频词会被拼写顺序截掉。
首版静态补全分固定为：

    static_score_centi(word, prefix)
      = zipf_centi(word) - 15 × (word.len - prefix.len)

同一个 prefix 的长度是常数，所以可按以下等价算法构建：

1. 将所有 word_id 按 zipf_centi - 15 × word.len 降序排序，并列按 key 升序。
2. 按该顺序遍历单词，枚举它的所有非空前缀。
3. 某前缀的列表不足 32 个时追加该 word_id，否则跳过。
4. 最后按 prefix 升序输出索引，列表内部维持追加顺序。

精确命中在运行时另外二分查询、强制召回，因此不依赖该词是否挤进 Top-32。
算法复杂度为 O(N log N + 所有词长之和)，不为每个前缀扫描全词库。
32 和 15 属于格式 v1 的排序约定；改变它们必须重建数据并更新兼容版本，
不能只改 Rust 常数继续读取旧缓存。

## 6. Rust 读取端

新增 english/dict.rs、english/format.rs，复用 memmap2，不增加运行时服务。
建议接口：

    EnglishDict::open(path) -> Result<EnglishDict, EnglishError>
    EnglishDict::exact(key) -> Option<EnglishEntryRef>
    EnglishDict::prefix_top(key) -> bounded entries，最多 32 个
    EnglishDict::word_count() -> usize

借用数据能覆盖一次查询即可，不需要为每个候选复制整套词库。
测试用小二进制走相同 reader；可提供内存构造辅助，但不能只测试内存替身。
映射本身便宜，完整校验是线性工作，必须在启动期做，不能声称整个加载过程 O(1)。

加载校验必须覆盖：

- 魔数、版本、头长、实际长度、区段次序及所有区段边界。
- words_offset + word_count × 16 等运算使用 checked_add/checked_mul。
- u64 转 usize 必须可表示，不使用无检查的 as 截断。
- ids 区长度是 4 的倍数；所有引用都在 words、ids、strings 合法范围内。
- key/prefix 严格升序且唯一；字符、长度、display 与 key 的关系合法。
- reserved 为零；q ≤ 900；引用列表无重复 ID；被引用词确实 starts_with(prefix)。
- 列表按静态分和 key 排序；精确查询和前缀查询的测试 oracle 必须独立实现。

防御上限：文件 ≤ 256 MiB、word_count ≤ 200,000、prefix_count ≤ 4,000,000。
这些是拒绝异常资源消耗的上界，不是要求生成这么大的词库。
读取损坏文件返回错误；进程装配层只关闭英文并告警，不能 panic 或连带清空中文。

## 7. 配置和实际接线

新增 src/english_config.rs，纯库不读取环境变量。

| 配置 | 缺省 | 作用 |
|---|---|---|
| XWX_ENGLISH | 1 | 总开关；0 时不打开英文词库和英文用户文件 |
| XWX_ENGLISH_DICT_PATH | 用户数据目录 english.bin，之后系统目录 | 只读词库 |
| XWX_ENGLISH_USER_LM_PATH | 用户目录 user_english.dev.txt 或 user_english.txt | 可写英文学习数据 |
| XWX_ENGLISH_PREDICT | 0 | M2 零输入下一词预测开关 |
| XWX_ENGLISH_BIGRAM_PATH | 用户数据目录 english_bigram.bin，之后系统目录 | M2 静态二元数据 |

布尔解析统一支持 1/true/on/yes 和 0/false/off/no，忽略 ASCII 大小写及两端空白。
非法布尔值警告并回到缺省，不把文件路径字符串 off 偷偷解释成特殊文件模式。
显式路径不存在时不自动改读其他路径；让缺失/损坏降级可观察。
可写用户路径永不回落 /usr/share/xwx；需要单独路径解析函数和纯函数测试。
英文词库没成功加载时，不打开或创建英文用户文件。

在 Arc::new(engine) 之前装配英文组件；ImeEngine::new(None) 等原有构造器默认不装英文。
新增显式 with_english(EnglishEngine) 装配接口，测试和探针也通过它接线。
安装任务只拷贝已经构建的资源，不联网；NOTICE/meta/许可必须与数据一起安装。
deb 的英文数据版应明确列出资源；缺文件时打包失败并提示构建命令，不能静默生成残缺包。
运行时仍允许缺少可选英文文件，保留原中文能力。
