//! 候选词(candidate)数据结构。
//!
//! 候选词是输入法引擎的核心数据单元:一次按键处理的结果会产出一组候选词,
//! 上屏的也是候选词。`Candidate` 同时承担两层含义:
//!
//! 1. **词典条目**:`dict.rs` 从词库查出的原始条目(此时 `score` 尚未计算,为 0.0);
//! 2. **引擎排序结果**:`lib.rs` 综合词频 + 语言模型打分后的最终候选
//!    (此时 `score` 是最终得分,`freq` 保留原始词频供调试/回退使用)。

use serde::{Deserialize, Serialize};

/// 候选的拼写来源(B4:取代旧 `fuzzy: bool` 的两层划分)。
///
/// 排序键是 **(u_min 桶, 层序, 分数 − 覆盖罚分)**:本枚举的声明序
/// (`#[derive(PartialOrd, Ord)]` 的枚举比较就是声明序,`Exact < Fuzzy <
/// Abbrev`)是**第二键**——同一 u_min 桶内仍是精确层全部 → 模糊层
/// 全部 → 简拼层全部;首键「u_min 桶」(候选终点处的不可避免未解释
/// 字母数)让「终点能精确切分到串尾」的候选跨层提前——既包括覆盖
/// 整串的简拼词(`zhongguor` 的「中国人」,恒 0 桶),也包括终点在
/// 音节边界的高频单字(`beijing` 的「被」`bei`,0 桶)——而把
/// 「拼音凑巧等长但剩下字母不可精确切分」的生僻组合词(被禁锢
/// `bei jin [g]`、狄安娜)压到段内桶。层内仍按分排序,见 `lib.rs`
/// 的 `sort_candidates`、`unreachable_tail` 与 `unexplained_letters`。
///
/// 不用「连续 credibility 混排」的理由不变:简拼候选动辄上千条(`xx`
/// 命中 10,816),一旦允许与精确候选按分混排,全拼第一页立刻被冲垮
/// (B4 与层序裁决)——所以分层
/// 语义保留,只是降为第二键,不是拆掉。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum SpellingKind {
    /// 精确匹配(输入串就是该词的完整拼音)。
    #[default]
    Exact,
    /// 模糊拼音匹配(经混淆对展开到达,带 `fuzzy_penalty` 扣分)。
    Fuzzy,
    /// 简拼/混合简拼匹配(`lookup_abbrev` / `lookup_mixed` 命中的整串候选)。
    Abbrev,
}

/// 旧 `fuzzy` 布尔字段与新 `spelling` 字段之间的映射。
///
/// serde 的 `alias` 只改「从哪个键读」,不改值的形态(旧值是 bool、
/// 新值是字符串),所以反序列化要走 `Candidate` 的自定义实现:先把整个
/// JSON 对象读成中间表示(同时认 `spelling` 与 `fuzzy` 两个键),再组装。
impl From<bool> for SpellingKind {
    /// 旧语义:true → 模糊层,false → 精确层。
    fn from(fuzzy: bool) -> Self {
        if fuzzy {
            SpellingKind::Fuzzy
        } else {
            SpellingKind::Exact
        }
    }
}

impl From<SpellingKind> for bool {
    /// 旧布尔语义的投影:只有「经模糊拼音到达」才是 true;
    /// 简拼不是模糊拼音(它走的是另一条更靠后的层),映射为 false。
    fn from(kind: SpellingKind) -> Self {
        kind == SpellingKind::Fuzzy
    }
}

impl Serialize for SpellingKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self {
            SpellingKind::Exact => "exact",
            SpellingKind::Fuzzy => "fuzzy",
            SpellingKind::Abbrev => "abbrev",
        })
    }
}

impl<'de> Deserialize<'de> for SpellingKind {
    /// 只认新格式的字符串形态(`"exact"` / `"fuzzy"` / `"abbrev"`);
    /// 旧格式的布尔值由 `Candidate` 的自定义反序列化在字段层接管
    /// (见 `Candidate` 的 `Deserialize` 实现),不会走到这里。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "exact" => Ok(SpellingKind::Exact),
            "fuzzy" => Ok(SpellingKind::Fuzzy),
            "abbrev" => Ok(SpellingKind::Abbrev),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["exact", "fuzzy", "abbrev"],
            )),
        }
    }
}

/// 英文候选的匹配方式。
///
/// 与中文的 [`SpellingKind`] 是**两套轴**：英文候选一律把 `spelling` 填
/// `Exact` 占位（见数据契约），但**绝不进入中文排序器**——中文排序按
/// `spelling` 分层，若让英文参与，它会污染那些结构判断。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnglishMatchKind {
    /// 输入串就是完整单词（`hello` → hello）。
    Exact,
    /// 输入串是完整单词的前缀（`pyth` → Python）。
    Completion,
    /// 无输入字母时的下一词建议（M2 预测面板）。
    NextWord,
}

/// 英文候选的元数据（中文与原文回声候选为 `None`）。
///
/// 有了它，会话层才能**按来源分流**而不是靠 `word.is_ascii()` 猜：
/// 提交、学习、造词、空格、屏蔽各自读这里的字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnglishCandidateMeta {
    /// 目标**完整单词**的小写键，不是已输入的前缀。
    pub key: String,
    /// 匹配方式。
    pub match_kind: EnglishMatchKind,
}

impl EnglishCandidateMeta {
    /// 构造（`key` 必须是合法小写英文键，由调用方保证）。
    pub fn new(key: impl Into<String>, match_kind: EnglishMatchKind) -> Self {
        Self {
            key: key.into(),
            match_kind,
        }
    }

    /// 是否是「无输入字母时的下一词」。
    pub fn is_next_word(&self) -> bool {
        self.match_kind == EnglishMatchKind::NextWord
    }
}

/// 一个候选词。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candidate {
    /// 候选词文本,如 "你好"。
    pub word: String,
    /// 对应的拼音串。多音节时用空格分隔,如 "ni hao"。
    ///
    /// 约定与 `data/README.md` 中词库的 `pinyin` 字段格式一致。
    pub pinyin: String,
    /// 最终得分(越大越靠前)。由引擎综合词频与语言模型打分得出;
    /// 词典直接产出的条目此字段为 0.0。
    pub score: f32,
    /// 原始词频(词典中记录的静态频率),用于展示与打分的回退基准。
    pub freq: u64,
    /// 该候选的拼写来源(分层排序用):精确 / 模糊拼音 / 简拼。
    ///
    /// 词典直接产出的条目与 `fallback_candidates` 联想路径(不经过
    /// `expand_fuzzy`)恒为 `Exact`;简拼来源(`lookup_abbrev` /
    /// `lookup_mixed`)由引擎回填 `Abbrev`。
    ///
    /// 序列化:新格式写为 `"spelling": "fuzzy"` 等字符串;读入时**同时**
    /// 兼容旧格式的 `"fuzzy": true/false` 布尔字段与连字段都没有的更旧
    /// 数据(缺省为 `Exact`)——两条旧路径都落回字段引入前的语义,
    /// 旧候选天然全在精确层。兼容逻辑在 `Candidate` 的 `Deserialize`
    /// 实现里(serde 的 `alias` 处理不了「旧值是 bool、新值是字符串」
    /// 的形态差异)。
    pub spelling: SpellingKind,
    /// 该候选是否是「回声候选」(echo candidate)。
    ///
    /// 回声候选由 `lookup_candidates` 在**全场一个候选都没有**时追加:
    /// `word` / `pinyin` 就是原始输入串本身,`freq = 0`,仅用来让候选
    /// 列表非空(用户能看见"打了一串字母至少有个东西"),语义照搬
    /// librime `echo_translator.cc` 的 `quality(-100)` 最低优先级项——
    /// 只要任何真实候选存在就绝不出现。它**不是词**,上屏时按
    /// `commit_raw` 处理(不学习、`last_word` 置 `None`)。
    ///
    /// 序列化向后兼容:旧数据没有此字段,反序列化按 `false`(真实候选)
    /// 处理——这正是字段引入前的语义,旧候选全是真实候选。
    #[serde(default)]
    pub echo: bool,
    /// 该候选**覆盖了输入串开头的多少字节**(分段上屏的依据)。
    ///
    /// 值 = `covered`,即选中它时应从输入串首部消耗掉的字节数:
    /// - 格边候选 = 该边在**原始输入串**里的终点(如 `womenjintian` 的
    ///   「我们」→ 5);
    /// - 兜底前缀联想 = 它实际匹配到的那个可切分前缀长度;整串联想、
    ///   简拼层、回声候选 = 整串长度。
    ///
    /// **`0` 的语义是「未知/整串」**:词典直接产出的条目、反序列化来的
    /// 旧数据、以及任何没被引擎回填的场景都保持 `0`,会话层遇到 `0`
    /// 按**整串消耗**处理——这正是本字段引入前的行为(上屏即复位),
    /// 因此旧数据/旧调用方的语义逐字节不变,不会出现「只消耗 0 字节、
    /// 输入串原地不动」这种把键盘卡死的退化。
    ///
    /// 序列化向后兼容:旧数据没有此字段,反序列化回落到 `0`。
    #[serde(default)]
    pub covered: usize,
    /// 副索引(`abbrev.bin`)命中时,被匹配到的那个 key **消费了多少个输入字母**。
    ///
    /// **不复用 `covered`**:`covered` 的语义是「该候选在原文串里覆盖到第几字节」,
    /// 被多处依赖(上屏消耗、分段、前缀混拼的真实覆盖),改它等于改一个共享字段的
    /// 全局含义——实测那样做会把 mixed 的召回从 13/14 打到 10/14(历史对拍记录)。本字段只回答一个问题:「这次前缀匹配吃掉了整串吗」。
    ///
    /// 值 = 被匹配到的副索引 key 的字母数(副索引 key 是「每音节一个字母」,
    /// `zh`/`ch`/`sh` 计两个字母,故 `smsh` 的 key 长 4)。与输入字母数相等
    /// 才是**整串命中**;`lookup_prefix` 是**前缀**匹配(`wmjt` 会被
    /// `wmjtqbj` 查到),所以只有这个真值能区分「整串」与「前缀」——
    /// 想从词条拼音反推(音节数/首字母串)的三次尝试全部失败(⑤⑨②/⑤⑨③/⑤⑨⑦)。
    ///
    /// `0` = 未知/非副索引来源(其余所有构造点)。只由
    /// [`crate::dict::AbbrevIndex::lookup_prefix`] 填值。
    ///
    /// **不序列化**:它纯粹是引擎内部的判据,进线格式只会白白改变
    /// 稳定输出(与 `covered` 不同——后者是上屏语义,必须跨进程保留)。
    ///
    /// **当前生产代码没有消费它**:⑤⑤④ 的「整串条目边」一度想用它筛,但
    /// `lookup_abbrev` 在收集阶段就到 `MAX_LOOKUP_RESULTS`(500 条)早停,
    /// 而筛只能发生在早停之后 ⇒ 排在 500 名以后的整串命中会被误杀
    /// (`smsh` 实测:命中的 `matched_len` 只有 {3,4},500 条窗口里的「什么是」
    /// 是 ml=4,但 20 条清单会因此从 20/20 掉到 19/20)。建边处改用候选自己的
    /// `covered`(`engine/lattice/build.rs` 的整串条目边)。本字段保留为
    /// **可测的匹配长度真值**,给后续要放宽早停、或按匹配长度做优先收集的
    /// 改动当输入。
    #[serde(skip)]
    pub matched_len: usize,
    /// 英文元数据（中文候选与原文回声候选为 `None`）。
    ///
    /// 序列化用 `skip_serializing_if`：旧候选 JSON **不增加空字段**，
    /// 关掉英文时整个 Vec 的序列化与旧入口逐字节相同。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub english: Option<EnglishCandidateMeta>,
}

impl<'de> Deserialize<'de> for Candidate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // 中间表示:新键 `spelling`(字符串)与旧键 `fuzzy`(布尔)同时
        // 可选;其余字段照常。`spelling` 优先于 `fuzzy`(新数据不可能带
        // 旧键,但防御「新旧键同时出现且冲突」时以新键为准)。
        #[derive(Deserialize)]
        struct Repr {
            word: String,
            pinyin: String,
            score: f32,
            freq: u64,
            #[serde(default)]
            spelling: Option<SpellingKind>,
            #[serde(default)]
            fuzzy: Option<bool>,
            #[serde(default)]
            echo: bool,
            #[serde(default)]
            covered: usize,
            #[serde(default)]
            english: Option<EnglishCandidateMeta>,
        }
        let r = Repr::deserialize(deserializer)?;
        if let Some(m) = &r.english {
            // 冲突数据报错，**不通过「看起来像英文」去修复字段**：
            // 回声标记、拼音、键合法性、大小写关系四者都必须自洽。
            if r.echo {
                return Err(serde::de::Error::custom("echo 候选不能带 english 元数据"));
            }
            if !r.pinyin.is_empty() {
                return Err(serde::de::Error::custom("英文候选的 pinyin 必须为空串"));
            }
            if !crate::english::is_english_key(&m.key) {
                return Err(serde::de::Error::custom("english.key 不是合法小写英文键"));
            }
            if !r.word.eq_ignore_ascii_case(&m.key) {
                return Err(serde::de::Error::custom(
                    "english.key 与候选 word 的大小写形式不一致",
                ));
            }
            if m.match_kind == EnglishMatchKind::NextWord && r.covered != 0 {
                return Err(serde::de::Error::custom("NextWord 候选的 covered 必须是 0"));
            }
        }
        Ok(Candidate {
            word: r.word,
            pinyin: r.pinyin,
            score: r.score,
            freq: r.freq,
            spelling: r
                .spelling
                .or_else(|| r.fuzzy.map(SpellingKind::from))
                .unwrap_or_default(),
            echo: r.echo,
            covered: r.covered,
            // 不参与序列化(见字段文档):反序列化一律回落到 `0`。
            matched_len: 0,
            english: r.english,
        })
    }
}

impl Candidate {
    /// 构造一个新候选词(`score` 初始为 0.0,由引擎后续打分)。
    pub fn new(word: impl Into<String>, pinyin: impl Into<String>, freq: u64) -> Self {
        Self {
            word: word.into(),
            pinyin: pinyin.into(),
            score: 0.0,
            freq,
            spelling: SpellingKind::Exact, // 词典条目是精确匹配;分层信息由引擎回填
            echo: false, // 词典条目是真实候选;回声标记由引擎追加时设置
            covered: 0,  // 未知/整串:由引擎在候选来源处回填(见字段文档)
            matched_len: 0, // 副索引匹配长度:只有 AbbrevIndex::lookup_prefix 填(见字段文档)
            english: None,  // 词典条目不是英文候选;英文通道显式构造(见 expand_english)
        }
    }

    /// 是否是英文候选（不论匹配方式）。
    pub fn is_english(&self) -> bool {
        self.english.is_some()
    }

    /// 是否是 M2 的下一词预测候选（只在预测态可被选择）。
    pub fn is_next_word(&self) -> bool {
        self.english.as_ref().is_some_and(|m| m.is_next_word())
    }

    /// 英文候选的完整小写键。
    pub fn english_key(&self) -> Option<&str> {
        self.english.as_ref().map(|m| m.key.as_str())
    }

    /// 英文候选的匹配方式（非英文候选为 `None`）。
    pub fn english_match_kind(&self) -> Option<EnglishMatchKind> {
        self.english.as_ref().map(|m| m.match_kind)
    }

    /// 构造一条英文候选（本地契约的唯一落点，避免多处手写字段组合）。
    ///
    /// 契约（见 `docs/english-input/02-engine.md` 第 2.1 节）：
    /// `echo = false`、`pinyin` 为空、`spelling = Exact` **占位**（英文
    /// 不参与中文分层排序，这个值只为满足旧字段的语义完整性）、
    /// `freq = 0`（不伪造中文词频）、`matched_len = 0`。
    ///
    /// `covered`：Exact/Completion 传**原始输入字节数**（不是补全后的单词
    /// 长度）；NextWord 必须传 0。
    pub fn new_english(
        display: impl Into<String>,
        key: impl Into<String>,
        match_kind: EnglishMatchKind,
        score: f32,
        covered: usize,
    ) -> Self {
        let key = key.into();
        debug_assert!(
            !(match_kind == EnglishMatchKind::NextWord && covered != 0),
            "NextWord 的 covered 必须是 0（它不消费任何输入字节）"
        );
        Self {
            word: display.into(),
            pinyin: String::new(),
            score,
            freq: 0,
            spelling: SpellingKind::Exact,
            echo: false,
            covered,
            matched_len: 0,
            english: Some(EnglishCandidateMeta::new(key, match_kind)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_roundtrip() {
        let c = Candidate::new("你好", "ni hao", 42);
        let json = serde_json::to_string(&c).expect("序列化不应失败");
        let back: Candidate = serde_json::from_str(&json).expect("反序列化不应失败");
        assert_eq!(c, back);
    }

    #[test]
    fn serde_legacy_without_fuzzy_defaults_exact() {
        // 字段引入前的旧格式没有 `fuzzy`/`spelling`:反序列化应回退为
        // Exact(精确层),与引入前的语义一致(旧候选全是精确匹配)。
        let legacy = r#"{"word":"你好","pinyin":"ni hao","score":-1.5,"freq":42}"#;
        let c: Candidate = serde_json::from_str(legacy).expect("旧格式反序列化不应失败");
        assert_eq!(c.spelling, SpellingKind::Exact, "缺省 spelling 应为 Exact(精确层)");
        assert_eq!(c.word, "你好");
        assert_eq!(c.freq, 42);
    }

    #[test]
    fn serde_legacy_fuzzy_true_maps_to_fuzzy_layer() {
        // 旧格式 `fuzzy: true` 的候选是模糊层:新字段应反序列化为 Fuzzy,
        // 不能掉回 Exact(否则旧数据里的模糊候选会被错误地提升进精确层)。
        let legacy = r#"{"word":"里","pinyin":"li","score":-2.0,"freq":10,"fuzzy":true}"#;
        let c: Candidate = serde_json::from_str(legacy).expect("旧格式反序列化不应失败");
        assert_eq!(c.spelling, SpellingKind::Fuzzy, "旧 fuzzy:true 应映射为 Fuzzy");
    }

    #[test]
    fn serde_legacy_fuzzy_false_maps_to_exact_layer() {
        let legacy = r#"{"word":"你","pinyin":"ni","score":-1.0,"freq":100,"fuzzy":false}"#;
        let c: Candidate = serde_json::from_str(legacy).expect("旧格式反序列化不应失败");
        assert_eq!(c.spelling, SpellingKind::Exact, "旧 fuzzy:false 应映射为 Exact");
    }

    #[test]
    fn serde_new_spelling_roundtrips_all_kinds() {
        for (kind, tag) in [
            (SpellingKind::Exact, "exact"),
            (SpellingKind::Fuzzy, "fuzzy"),
            (SpellingKind::Abbrev, "abbrev"),
        ] {
            let json = format!(
                r#"{{"word":"词","pinyin":"ci","score":0.0,"freq":1,"spelling":"{tag}"}}"#
            );
            let c: Candidate = serde_json::from_str(&json).expect("新格式反序列化不应失败");
            assert_eq!(c.spelling, kind);
        }
    }

    #[test]
    fn serde_legacy_without_echo_defaults_real() {
        // 字段引入前的旧格式没有 `echo`:反序列化应回退为 false(真实候选),
        // 与引入前的语义一致(旧候选全是真实候选,没有回声概念)。
        let legacy = r#"{"word":"你好","pinyin":"ni hao","score":-1.5,"freq":42,"fuzzy":false}"#;
        let c: Candidate = serde_json::from_str(legacy).expect("旧格式反序列化不应失败");
        assert!(!c.echo, "缺省 echo 应为 false(真实候选)");
        assert_eq!(c.word, "你好");
        assert_eq!(c.freq, 42);
    }

    #[test]
    fn serde_legacy_without_covered_defaults_whole_string() {
        // 字段引入前的旧格式没有 `covered`:反序列化回落 0,而 0 的语义是
        // 「未知/整串」——旧数据在会话层照旧整串消耗(改动前行为),
        // 不会退化成「消耗 0 字节」。
        let legacy = r#"{"word":"你好","pinyin":"ni hao","score":-1.5,"freq":42,"echo":false}"#;
        let c: Candidate = serde_json::from_str(legacy).expect("旧格式反序列化不应失败");
        assert_eq!(c.covered, 0, "缺省 covered 应为 0(=未知/整串)");
    }

    #[test]
    fn serde_covered_roundtrips() {
        let mut c = Candidate::new("我们", "wo men", 10);
        c.covered = 5;
        let json = serde_json::to_string(&c).expect("序列化不应失败");
        let back: Candidate = serde_json::from_str(&json).expect("反序列化不应失败");
        assert_eq!(back.covered, 5);
    }

    #[test]
    fn serde_spelling_wins_over_conflicting_legacy_fuzzy() {
        // 防御:新旧键同时出现且冲突时以新键为准(手写/迁移中的数据)。
        let mixed = r#"{"word":"词","pinyin":"ci","score":0.0,"freq":1,"fuzzy":true,"spelling":"exact"}"#;
        let c: Candidate = serde_json::from_str(mixed).expect("混合格式反序列化不应失败");
        assert_eq!(c.spelling, SpellingKind::Exact, "新键 spelling 应优先");
    }

    #[test]
    fn spelling_kind_declaration_order_is_layer_order() {
        // 严格分层依赖枚举声明序:Exact < Fuzzy < Abbrev(排序时精确层在
        // 最前)。若有人调整声明顺序,这个测试会先红。
        assert!(SpellingKind::Exact < SpellingKind::Fuzzy);
        assert!(SpellingKind::Fuzzy < SpellingKind::Abbrev);
    }

    #[test]
    fn serde_legacy_without_english_has_no_extra_field() {
        // C01:旧 JSON 无 english 可读,且新中文候选**不增加空字段**——
        // 关掉英文时整个 Vec 的序列化必须与旧入口逐字节相同。
        let legacy = r#"{"word":"你好","pinyin":"ni hao","score":-1.5,"freq":42}"#;
        let c: Candidate = serde_json::from_str(legacy).expect("旧格式反序列化不应失败");
        assert!(c.english.is_none());
        let json = serde_json::to_string(&Candidate::new("你好", "ni hao", 42)).expect("序列化");
        assert!(!json.contains("english"), "中文候选不得写出 english 字段: {json}");
    }

    #[test]
    fn english_metadata_roundtrips_all_match_kinds() {
        for (kind, tag) in [
            (EnglishMatchKind::Exact, "exact"),
            (EnglishMatchKind::Completion, "completion"),
            (EnglishMatchKind::NextWord, "next_word"),
        ] {
            let covered = if kind == EnglishMatchKind::NextWord { 0 } else { 4 };
            let c = Candidate::new_english("Python", "python", kind, 3.8, covered);
            let json = serde_json::to_string(&c).expect("序列化不应失败");
            assert!(json.contains(&format!("\"match_kind\":\"{tag}\"")), "{json}");
            let back: Candidate = serde_json::from_str(&json).expect("反序列化不应失败");
            assert_eq!(back, c);
            assert!(back.is_english());
            assert_eq!(back.english_key(), Some("python"));
            assert_eq!(back.pinyin, "", "英文候选的 pinyin 必须是空串");
            assert!(!back.echo);
            assert_eq!(back.freq, 0, "不伪造中文词频");
        }
    }

    #[test]
    fn english_next_word_has_zero_covered_and_exact_has_input_len() {
        let nw = Candidate::new_english("you", "you", EnglishMatchKind::NextWord, 1.0, 0);
        assert!(nw.is_next_word());
        assert_eq!(nw.covered, 0);
        let ex = Candidate::new_english("hello", "hello", EnglishMatchKind::Exact, 4.5, 5);
        assert!(!ex.is_next_word());
        assert_eq!(ex.covered, 5, "covered 是原始输入字节数,不是补全后单词长度");
    }

    #[test]
    fn conflicting_english_data_is_rejected_not_silently_repaired() {
        // echo + english
        let bad = r#"{"word":"python","pinyin":"","score":1.0,"freq":0,"echo":true,
            "english":{"key":"python","match_kind":"exact"}}"#;
        assert!(serde_json::from_str::<Candidate>(bad).is_err(), "echo+english 必须报错");
        // pinyin 非空
        let bad = r#"{"word":"python","pinyin":"py","score":1.0,"freq":0,
            "english":{"key":"python","match_kind":"exact"}}"#;
        assert!(serde_json::from_str::<Candidate>(bad).is_err(), "英文候选 pinyin 必须为空");
        // key 非小写/非法
        let bad = r#"{"word":"Python","pinyin":"","score":1.0,"freq":0,
            "english":{"key":"Python","match_kind":"exact"}}"#;
        assert!(serde_json::from_str::<Candidate>(bad).is_err(), "key 必须是小写英文键");
        // word 与 key 大小写形式不一致
        let bad = r#"{"word":"Py thon","pinyin":"","score":1.0,"freq":0,
            "english":{"key":"python","match_kind":"exact"}}"#;
        assert!(serde_json::from_str::<Candidate>(bad).is_err(), "word 小写必须等于 key");
        // NextWord 带 covered
        let bad = r#"{"word":"you","pinyin":"","score":1.0,"freq":0,"covered":3,
            "english":{"key":"you","match_kind":"next_word"}}"#;
        assert!(serde_json::from_str::<Candidate>(bad).is_err(), "NextWord 的 covered 必须是 0");
    }
}
