//! 输入会话（InputSession）：把"按键流"翻译成"上屏/更新候选"的状态机。
//!
//! 这是引擎对外的核心入口：上层（IBus 适配层）把每次按键翻译成
//! [`InputEvent`] 交给会话，会话维护拼音串与候选列表，产出一个
//! [`SessionOutput`]：
//!
//! - [`SessionOutput::Commit`]：需要上屏文本（选词、回车、空格）；
//! - [`SessionOutput::Updated`]：候选/拼音串变化，需要刷新界面；
//! - [`SessionOutput::Noop`]：按键不归本输入法管，交给应用（如拼音
//!   为空时的空格键）。
//!
//! 本模块刻意不依赖任何框架（DBus/GTK 都不沾），方便单测与复用。

use std::sync::Arc;

use crate::punctuator::fullwidth_punct;
use crate::{Candidate, EngineError, ImeEngine, LmContext};

/// 候选窗口每页显示的候选数（搜狗等主流输入法默认 9 个）。
pub const DEFAULT_PAGE_SIZE: usize = 9;
/// 已上屏文本尾部的最大保留字符数（按 `char` 计，非字节）。
///
/// 上界 16：够覆盖将来万象（Rime octagram `.gram`）的 5 字字符窗口，
/// 也装得下 bigram 的一个词；既不随会话无限膨胀，也不会让候选打分
/// 丢失必要上文。
pub const COMMITTED_TAIL_LIMIT: usize = 16;

/// 一次造词片段的最大字符数(按 `char` 计,非字节)。
///
/// 长度上限:超过这个长度的连续上屏段不再粘成一个用户词——越长
/// 越不像是「一个想反复复用的短语」,且造出来的词条要占候选位。
/// 24 字覆盖常见人名/短语/短句(「张三丰」3 字、七言诗句 14 字),
/// 又不至于让一次失控的长段上屏永久占位。
///
/// > **溯源更正(2026-10-01)**:本常量原先的注释称它「对应 librime
/// > `unity_table_encoder` 对单次编码片段的长度上限」。实读 librime
/// > 头文件与 `gear-audit.md` 的对照后发现这个溯源是错的:
/// > `UnityTableEncoder` 是 **`table_translator`**(码表输入法)的组件,
/// > 挂在 `/usr/include/rime/gear/unity_table_encoder.h`,而拼音走的是
/// > **`script_translator`**(`gear-audit.md` 第 3 节确认 script_translator
/// > 没有 encoder)。也就是说 xwx 在拼音引擎上移植了一个**码表特性**。
/// > 24 这个数值本身仍是合理的工程选择(覆盖人名/诗句),保留;
/// > 但「librime 也这么做」不再是它的理由——见
/// > [`InputSession::is_reusable_phrase`] 的说明。
pub const MAX_COIN_CHARS: usize = 24;

/// 造词最少片段数:一次「完整词上屏」(缓冲长度 1)不造词。
///
/// 单片段要么词库本来就有它(造了是重复条目),要么是单字——
/// 单字的候选地位由 unigram 调频表达就够了,造成词条也不会改变
/// 它的召回(它本来就以单音节 key 查得到)。
///
/// ≥2 片段才造,这是 `unity_table_encoder` 语义的正确落点:它把
/// **一次编码**反查出的整段短语写进用户词典,而「一次编码」在码表
/// 里对应一次完整上屏;在拼音里对应「用户逐字/逐词挑了一串」。
pub const MIN_COIN_SEGMENTS: usize = 2;

/// 造词的判据开关:要求造词段里**每一个片段都是单字**(逐字组词)。
///
/// **为什么需要这道门槛(实测依据)**:造词缓冲对**每一次非回声候选
/// 上屏**都入队(`push_coin`),只在被打断时结算。于是「把候选当空格
/// 用」的连续打字——选「你好」→ 选「世界」→ 选下一个词——会被粘成
/// 一个用户词条。这在真实用户数据里已经发生
/// (`~/.local/share/xwx/user_bigram.txt` 的造词段):
///
/// ```text
/// 当前主线以及已经完了是吧进度  dang qian zhu xian yi ji yi jing wan le shi ba jin du
/// 设置为说着玩的说说中文        she zhi wei shuo zhe wan de shuo shuo zhong wen
/// ```
///
/// 这些是**句子**,不是用户想复用的词条;而且用户对此毫无感知
/// (没有确认步骤、没有提示),词条却会永久占用候选位——正是
/// `MAX_COIN_CHARS` 想避免的「用户词典被一次性大段文本塞满」,
/// 只是 24 字上限拦不住 11 字的句子。
///
/// **判据的演化(记下来,别再走一遍)**:第一版取「缓冲里**存在**
/// 至少一个单字片段」。那是错的——上面第二条 `设置为说着玩的说说中文`
/// 的片段里有单字「为」「说」,会被误判为够格。改成「**全部**片段
/// 都是单字」后两条真实句子都被拦住:
///
/// | 段 | 片段切分 | 全单字? | 判定 |
/// |---|---|---|---|
/// | `当前主线以及已经完了是吧进度` | 当前/主线/以及/已经/完了/是吧/进度 | 否 | 不造 ✓ |
/// | `设置为说着玩的说说中文` | 设置/为/说着玩的/说/说中文 | 否 | 不造 ✓ |
/// | `张三丰`(验收) | 张/三/丰 | **是** | 造 ✓ |
/// | `西双版纳` | 西/双/版/纳 | **是** | 造 ✓ |
///
/// 这也正是这个特性的原始定义:**「用户逐字选出来的整段短语」**。
/// 只要用户选了**一个整词**作为片段,就说明他是在组句/正常打字,
/// 不是在造词——判据与语义因此完全对齐,不是权宜之计。
///
/// 权衡:极端情况下「逐词拼一个真正想复用的短语」不再自动造词
/// (用户仍可逐字选一遍来造)。这是刻意的保守取舍——
/// **宁可不造,不可乱造**:乱造的词条会污染候选,而且用户删不掉
/// (它只在候选里出现,没有「管理用户词典」界面)。
///
/// **残留局限(如实记下,没解决)**:反过来那种情况仍会误造——
/// 用户**全部用单字**连续打字组句时(如逐个选「我」「的」「是」),
/// 片段全是单字,判据为真,仍会造出「我的是」。这条判据分不开
/// 「逐字组一个想复用的词」与「逐字打一句话的巧合」,因为两者在
/// **片段形状上完全同构**——这既是判据能成立的原因,也是它的边界。
///
/// 要彻底分开需要额外信号,都超出当前改动范围:
/// - 「同一个字被反复重打过」的观察(可隔开句子与词汇);
/// - 用户显式确认(弹窗/按键)后才落盘;
/// - 给用户一个「管理用户词典」的界面来删(更该做,目前没有)。
///
/// 当前取舍:**优先挡住高频且明显错误的场景**(把候选当空格用的
/// 连续打字——已实测发生在真实用户数据里),接受低频的逐字组句误造。
pub const COIN_ALL_SEGMENTS_SINGLE_CHAR: bool = true;

/// 一次按键的抽象。由上层把 IBus/GDK 键值翻译成这个枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    /// 可打印字符（字母/数字/标点等）。字母会进入拼音串。
    Char(char),
    /// 直接上屏的字符(不进拼音串、不做全角标点映射)。
    ///
    /// 与 [`InputEvent::Char`] 的差别:`Char` 是「交给输入法处理的
    /// 原始字符」——字母进拼音串、标点按模式映射全角;`Verbatim`
    /// 是「已经定稿、原样上屏」的字符,目前只有两个来源
    /// (ibus 层 `translate_keyval`):
    ///
    /// - **Shift+字母**(中文模式):归一出大写(`G`),
    ///   `GPU`/`API`/`CPU` 这类大写缩写能打出来;
    /// - **CapsLock 开**:字母直接出大写,不进拼音串。
    ///
    /// 会话层对它的处理与标点分支同构:拼音串非空时先按空格语义
    /// 上屏当前高亮候选,再追加该字符(`nihao` 后 Shift+G →
    /// 「你好G」);为空时直接上屏。字符只进上屏尾部、不进 UserLM
    /// 学习(单字母不是词),与标点一致。
    Verbatim(char),
    /// 数字选词：`Select(1)` 选第 1 个候选；`Select(0)` 选第 10 个。
    Select(u8),
    /// 候选面板点击(`CandidateClicked`):`index` 是**全局**下标(跨页)。
    ///
    /// 走 `handle` 而不是直接调 [`InputSession::commit_at`],是为了让
    /// 「点击选词」与「空格/数字选词」共用同一条分段上屏路径(同一个
    /// `SessionOutput` 契约):否则点击路径要么漏掉「剩余串继续组合」,
    /// 要么得在 IBus 层再实现一遍。
    CandidateClicked(usize),
    /// 退格键。
    Backspace,
    /// 回车键：拼音串原样上屏。
    Enter,
    /// 空格键：拼音串非空时选词（高亮候选），为空时透传。
    Space,
    /// Esc：清空当前输入。
    Escape,
    /// 上/下方向键：移动候选高亮。
    Up,
    Down,
    /// 左/右方向键:移动拼音串内光标(B1 已实现,见 `handle`
    /// 的对应分支;拼音串为空时透传给应用)。
    Left,
    Right,
    /// 翻页。
    PageUp,
    PageDown,
    /// Ctrl+Delete:删除当前高亮候选(屏蔽它,此后同拼音查询不再出现;
    /// 若是用户造词条目则连同词条删掉)。见 [`crate::UserLM::block_candidate`];
    /// 仅在候选窗有候选时有意义。
    DeleteCandidate,
    /// Ctrl+空格：切换中英文模式。
    ToggleAscii,
}

/// 已确认、但**尚未交给应用**的一段(方案 b:确认段留在组合里)。
///
/// 用户选了一段之后,这一段进预编辑区显示、并留在会话里,直到整个组合
/// 结束才随 `CommitText` 一次交给应用。两个好处:
/// - 退格可以**回退上一次确认**(把 `letters` 原样还回输入串,重新出候选);
/// - 学习/记账推迟到组合结束(`commit_selection` / `finish_with_raw`),
///   中途回退不需要撤销任何已写入的 UserLM 计数。
#[derive(Debug, Clone)]
struct ConfirmedSegment {
    /// 用户选中的候选本身(上屏文本、是否回声、以及记账要用的信息)。
    candidate: Candidate,
    /// 这一段消耗掉的原始字母——退格回退时原样还回输入串。
    letters: String,
}

/// 一次按键处理后的界面刷新数据。
#[derive(Debug, Clone, Default)]
pub struct SessionUpdate {
    /// 当前拼音串（未上屏部分），空串表示无输入。
    pub preedit: String,
    /// 全部候选（按得分降序）。
    pub candidates: Vec<Candidate>,
    /// 高亮候选的全局下标（相对 `candidates`）。
    pub cursor: usize,
    /// 预编辑串(拼音串)内的光标位置:字节偏移,也等于字符位置
    /// (拼音串只含 ASCII 字母)。**不是候选下标**——那是 `cursor`:
    /// `cursor` 高亮候选窗里的第几条,`caret` 定位预编辑串里的插入点。
    pub caret: usize,
    /// 当前页码（0 起）。
    pub page: usize,
    /// 每页候选数。
    pub page_size: usize,
}

impl SessionUpdate {
    /// 当前页第一个候选的全局下标。
    pub fn page_start(&self) -> usize {
        self.page * self.page_size
    }

    /// 当前页的候选切片。
    pub fn page_candidates(&self) -> &[Candidate] {
        let start = self.page_start();
        if start >= self.candidates.len() {
            return &[];
        }
        &self.candidates[start..(start + self.page_size).min(self.candidates.len())]
    }
}

/// 一次按键处理的结果。
#[derive(Debug, Clone)]
pub enum SessionOutput {
    /// 需要上屏的文本。上屏后会话已自动复位。
    Commit(String),
    /// 界面需要刷新（拼音串或候选变化）。
    Updated(SessionUpdate),
    /// 本输入法不处理该按键。
    Noop,
}

/// 把后缀字符(标点 / Verbatim 字母)拼到一次上屏的文本末尾。
///
/// 标点与 Shift+字母落在「输入串非空」的分支里,都是「先结束当前组合
/// (已确认段 + 当前候选/原样拼音),再追加这个字符」;两者都以 `Commit`
/// 收尾(方案 b 之后不再有「上屏一段还留着组合」的变体),这里只负责拼串。
fn append_to_commit(out: SessionOutput, suffix: &str) -> SessionOutput {
    match out {
        SessionOutput::Commit(text) => SessionOutput::Commit(format!("{text}{suffix}")),
        // 本函数只被上屏分支调用,`Updated`/`Noop` 不可达;原样返回,
        // 不制造新的语义。
        other => other,
    }
}

/// 输入会话状态机。
///
/// 状态：拼音串 + 候选列表 + 高亮/页码。所有状态变化都通过
/// [`InputSession::handle`] 驱动，出错返回 `Err`（此时状态保持不变，
/// 上层应把按键透传给应用并记日志）。
pub struct InputSession {
    /// 引擎（切分器/词典/模型/纠错的聚合），会话只通过它查询候选。
    engine: Arc<ImeEngine>,
    /// 当前拼音串。
    pinyin: String,
    /// 拼音串内的编辑光标:字节偏移,也等于字符位置(串内只会有
    /// ASCII 字母——见 `handle` 的 `Char` 分支,只有 `is_ascii_alphabetic`
    /// 的字符能进来)。不变量:`0 <= caret <= pinyin.len()`,
    /// 由所有 mutating 路径维护,`reset()` 归 0。
    caret: usize,
    /// 当前候选（刷新时重新计算）。
    candidates: Vec<Candidate>,
    /// 高亮候选下标。
    cursor: usize,
    /// 页码。
    page: usize,
    /// 每页候选数。
    page_size: usize,
    /// 已上屏文本的末尾（有界，≤ `COMMITTED_TAIL_LIMIT` 个字符）。
    ///
    /// 供语言模型查询上文用（`tail`：字符级窗口；`last_word`：词级
    /// bigram 前词）。**跨候选保留**：上屏一个词后继续打字，下一轮
    /// 候选打分要能看见它——这是第 2a 步「把已上屏上文接进打分」的
    /// 落点。`reset()` 刻意不清它（见 [`InputSession::reset`]）。
    committed_tail: String,
    /// 最后一个上屏的词（词级 bigram 上文）。`commit_raw` 上屏的是
    /// 原始字母/标点，不是词，此时置 `None`。
    last_word: Option<String>,
    /// 英文模式（中英切换）：开启时除 Shift 外的按键直接透传给应用。
    english_mode: bool,
    /// 造词缓冲:连续的「候选词上屏」暂存区(word + 其拼音)。
    ///
    /// 用户逐字/逐词选出来的整段(如 `zhangsanfeng` 分三次选
    /// 「张」「三」「丰」)在这里累积;段被打断(标点/回车/原始字母/
    /// Esc/模式切换——它们都经 `commit_raw` 或 `reset`)或缓冲超长时,
    /// 若段长 ≥ 2 字就造出一条用户词条写入 [`crate::UserLM`]
    /// (对应 librime `unity_table_encoder` 把上屏整段反查编码落盘)。
    ///
    /// 只收非回声的候选词上屏(`commit_word`):回声候选、原始字母、
    /// 标点、Verbatim 大写都没有合法拼音,不进缓冲(与它们不进
    /// UserLM 学习是同一判断)。
    coin_buffer: Vec<(String, String)>,
    /// 已确认、但还没交给应用的段(方案 b)。见 [`ConfirmedSegment`]。
    ///
    /// 组合期间预编辑区显示 = 这些段的文本 + 剩余拼音串;整串选完 / 回车 /
    /// 标点 / 切英文 / 焦点切换时才一次性上屏。
    confirmed: Vec<ConfirmedSegment>,
    /// recognizer 识别态(gear 审计最后一项:邮箱/网址识别)。
    ///
    /// `Some(_)` 表示当前拼音串整体被 [`crate::recognize`] 判定为
    /// 邮箱/网址原文(如 `zhangsan@gmail.com`、`www.example.com`):
    /// 候选列表被一条「原样上屏」的回声候选取代(拼音候选消失——
    /// 「万维网/呜呜呜」不再出现在 `www.` 之后),`.`/`@`/`:` 等
    /// 符号键直通进串(不被标点/翻页吃掉)。**每接纳/回删一个字符
    /// 后用完整串重判一次**,判定是纯函数、状态只在会话层——
    /// 「继续打字不反悔」与「Backspace 退回正常拼音」都由这个
    /// 重判天然给出(见 `handle` 的 Char/Backspace 分支)。
    recognized: Option<crate::Recognized>,
}

impl InputSession {
    /// 新建会话（初始为空闲状态）。
    pub fn new(engine: Arc<ImeEngine>) -> Self {
        Self {
            engine,
            pinyin: String::new(),
            caret: 0,
            candidates: Vec::new(),
            cursor: 0,
            page: 0,
            page_size: DEFAULT_PAGE_SIZE,
            committed_tail: String::new(),
            last_word: None,
            english_mode: false,
            coin_buffer: Vec::new(),
            confirmed: Vec::new(),
            recognized: None,
        }
    }

    /// 当前拼音串。
    pub fn pinyin(&self) -> &str {
        &self.pinyin
    }

    /// 拼音串内的编辑光标(字节偏移 = 字符位置,串内只有 ASCII)。
    pub fn caret(&self) -> usize {
        self.caret
    }

    /// 是否处于输入状态（拼音串非空）。
    pub fn is_active(&self) -> bool {
        !self.pinyin.is_empty()
    }

    /// 是否处于英文模式（中英切换）。
    pub fn english_mode(&self) -> bool {
        self.english_mode
    }

    /// 当前 LM 查询上下文（已上屏文本，有界）。
    pub fn lm_context(&self) -> LmContext<'_> {
        LmContext {
            tail: &self.committed_tail,
            last_word: self.last_word.as_deref(),
        }
    }

    /// 处理一个按键事件。
    ///
    /// 出错时返回 `Err`（词典/模型查询失败等），会话状态不变。
    pub fn handle(&mut self, event: InputEvent) -> Result<SessionOutput, EngineError> {
        match event {
            // —— 英文模式：字母等按键透传给应用 ——
            // 英文模式下拼音串必为空，方向键/空格/回车等自然落入相应 Noop；
            // 只有字符在这里被挡掉，不进拼音串。
            InputEvent::Char(c) if self.english_mode && c.is_ascii() => {
                Ok(SessionOutput::Noop)
            }

            // —— Ctrl+空格：切换中英文模式（引擎自己的逻辑） ——
            InputEvent::ToggleAscii => Ok(self.toggle_ascii()),

            // —— recognizer 识别态(邮箱/网址):字符直通进串 ——
            // 串已被识别为原文(`zhangsan@gmail`、`www.e`…)时,**任何**
            // 可打印字符都直接进串——`.`/`@`/`:`/`/` 不能被标点/翻页
            // 吃掉(`www.` 的点、`http://` 的斜杠都是网址的一部分),
            // 字母继续延长原文。进串后用完整串重判(见 refresh);
            // 识别态下候选窗只有一条「原样上屏」回声候选,拼音候选
            // 消失——这是 recognizer 生效的可观测标志。
            InputEvent::Char(c) if self.recognized.is_some() && c.is_ascii() && !c.is_ascii_control() => {
                self.pinyin.insert(self.caret, c);
                self.caret += c.len_utf8();
                self.refresh()?;
                Ok(SessionOutput::Updated(self.update()))
            }

            // —— 字母：进入拼音串 ——
            InputEvent::Char(c) if c.is_ascii_alphabetic() => {
                // 在 caret 处插入(不是追加):长串中间打错字母可以
                // 左移光标补插,不用全删重打。caret 处必为 char 边界
                // (串内只有单字节 ASCII,caret 由本结构体独占维护)。
                let c = c.to_ascii_lowercase();
                self.pinyin.insert(self.caret, c);
                self.caret += c.len_utf8();
                self.refresh()?;
                Ok(SessionOutput::Updated(self.update()))
            }
            // —— recognizer 入口 1:`@`(邮箱识别) ——
            // `@` 不在标点表、空闲时照旧透传(应用收到原字符);
            // 拼音串非空时进串——若此刻整串命中 email pattern
            // (`[字母][-_.0-9A-Za-z]*@`),refresh 会把会话带入识别态,
            // 此前被切成拼音的那几个字母(abc)原样留在串里成为原文,
            // 候选窗从「安伯尘/安保处…」换成唯一一条「abc@」回声候选
            // ——这就是「回溯」:不需要把已进串的内容搬出去,识别态只是
            // 「别再切它」。
            InputEvent::Char('@') => {
                if self.pinyin.is_empty() {
                    Ok(SessionOutput::Noop)
                } else {
                    self.pinyin.insert(self.caret, '@');
                    self.caret += 1;
                    self.refresh()?;
                    Ok(SessionOutput::Updated(self.update()))
                }
            }
            // —— recognizer 入口 2:`:`(协议前缀 url:http:/https:/mailto:/file:/ftp:)——
            // 前向判定(与 `.` 的触发臂同构,先于标点臂):「串 + `:`」
            // 命中某个 url 前缀时,`:` 直接进串——**不**落到标点臂
            // 上屏「mailto:」。`http:` 落键后串就是 url 前缀,下一键
            // (`/`、字母)使「前缀 + 非空后缀」命中,识别成立,
            // 此后 `//` 等全部走识别态直通臂(`/` 不会被 fullwidth_punct
            // 映射成顿号——这是本轮修掉的坏结果「http:、、xcom」)。
            // `:` 不与翻页键位冲突(`,`/`.`/`-`/`=` 才有翻页语义),
            // 唯一让渡:候选窗开着时对 `nihao:` 这类串敲 `:`,出的
            // 是识别态 `http:` 前缀而不是「拼音首选 + `:`」——
            // 只影响四个协议名前缀,可接受(见 recognizer.rs 模块文档)。
            // `mailto` 单独打(不接 `:`)永不触发,拼音候选不受影响。
            InputEvent::Char(':') if !self.pinyin.is_empty() => {
                let prospective = format!("{}:", self.pinyin);
                if crate::URL_PREFIXES.contains(&prospective.as_str()) {
                    self.pinyin.insert(self.caret, ':');
                    self.caret += 1;
                    self.refresh()?;
                    return Ok(SessionOutput::Updated(self.update()));
                }
                // 非协议前缀:落到标点臂(全角 `:`),行为不变
                let punct = fullwidth_punct(':').expect("`:` 在标点表内");
                let head = match self.candidates.get(self.cursor).cloned() {
                    Some(c) => self.commit_selection(&c),
                    None => self.finish_with_raw(self.pinyin.clone()),
                };
                self.push_tail(punct);
                Ok(SessionOutput::Commit(format!("{head}{punct}")))
            }
            // —— B2 音节分隔符 `'`(librime speller 的 delimiters)——
            // 进拼音串、作为强制切分点(`xi'an` 消歧出「西安」)。
            // 切分在引擎层做:`lookup_candidates` 把 `'` 归一化成
            // 「干净串 + 边界字节表」——`'` 不进词典 key、不进
            // `Candidate.pinyin`,但它的位置作为「不许任何边跨越」的
            // 强制切分点贯穿格构建/beam/后向 DP(详见 lib.rs 的
            // `lookup_candidates` 与 `build_lattice` 注释)。
            // 空闲时(拼音串为空)打 `'` 没有意义:透传给应用。
            InputEvent::Char('\'') => {
                if self.pinyin.is_empty() {
                    Ok(SessionOutput::Noop)
                } else {
                    self.pinyin.insert(self.caret, '\'');
                    self.caret += 1; // `'` 是单字节 ASCII
                    self.refresh()?;
                    Ok(SessionOutput::Updated(self.update()))
                }
            }
            // —— 中文标点(librime punctuator 的等价物;映射表及成对标点不交替、
            //    `'` 不进表、`,`/`.` 翻页留待与 selector 一起设计等取舍,
            //    见 punctuator.rs 模块文档) ——
            // 拼音串非空时,先按「空格上屏」的同一语义提交当前高亮候选
            // (无候选则原样上屏拼音串,与 Space 分支逐字对齐),紧接着上屏
            // 全角标点——用户打 `nihao,` 得到「你好,」;拼音串为空时直接
            // 上屏全角标点。英文模式不到这里(上面第一个 Char 分支已拦截,
            // 返回 Noop 沿透传链让应用收到原始半角字符)。
            InputEvent::Char(c) if fullwidth_punct(c).is_some() => {
                // —— librime `paging_with_comma_period` / `paging_with_minus_equal` 的状态机 ——
                // 在「上屏标点」之前先判翻页(照 `/usr/share/rime-data/key_bindings.yaml`):
                //   `.` 用 has_menu:候选窗开着(非空)就翻下一页;
                //   `,` 用 paging:仅「已翻过页」(page>0)才回翻上一页——
                //     第一页时打 `,` 仍出中文逗号,这是它与 `.` 的非对称点,
                //     也是「中文标点和翻页不冲突」的关键(还没翻页时 `,` 必须是标点);
                //   `-`/`=`(key_bindings.yaml:27-30,都是 has_menu)无标点含义、
                //     不冲突,一并做:候选窗开着时分别上/下翻页。
                // 末页再按 `.`、首页再按 `,`:`page` 用 min/saturating 钳在
                // 合法范围(不 wrap),与既有 PageUp/PageDown 分支同一行为——
                // 多按一下无效果,不报错、不上屏、不丢候选。
                //
                // recognizer 入口(`.`,网址识别):`.` 是「翻页/标点」
                // 与「url 字符」唯一正面冲突的键。判定顺序照 librime
                // recognizer(recognizer 排在 selector 之前,匹配成功
                // PushInput 且不再落到后续 processor):**先问「串 + `.`
                // 会不会命中 url pattern」,命中则 `.` 进串、不翻页、
                // 不出标点**。
                //
                // 落到这里的 `c` 含全部标点键,但能把整串判成 url 的
                // 只有 `.`(`recognize` 的 url 判定只看前缀字面量与
                // 首字符,`@`/`:` 等其余键在 url 语义下恒不命中;
                // email 的触发键 `@` 走上面专属臂,到不了这里)。
                //
                // 注意收窄语义:`recognize` 对「前缀 + 空后缀」不判成立
                // (见 recognizer.rs 模块文档),所以 `www` + `.` 在落键
                // 瞬间**不**进识别态——`.` 照旧翻页/出「。」;用户在
                // `.` 后再敲一个字母(`www.e`)才识别。也因此,下面的
                // 代码路径在现在的 pattern 集下**不可达**(没有任何
                // 「加 `.` 后 newly 命中」的串):它防的是将来放宽
                // 「前缀后必须有内容」这条收窄时,`.` 被翻页/标点
                // 抢走。保留它是有意的——去掉它,那个未来改动会
                // 静默退化成「`.` 被吃掉」。
                if !self.candidates.is_empty()
                    && c == '.'
                    && crate::recognize(&format!("{}{}", self.pinyin, c)).is_some()
                {
                    self.pinyin.insert(self.caret, '.');
                    self.caret += 1;
                    self.refresh()?;
                    return Ok(SessionOutput::Updated(self.update()));
                }
                if !self.candidates.is_empty() {
                    let max_page = self
                        .candidates
                        .len()
                        .div_ceil(self.page_size)
                        .saturating_sub(1);
                    match c {
                        '.' | '=' => {
                            self.page = (self.page + 1).min(max_page);
                            return Ok(SessionOutput::Updated(self.update()));
                        }
                        ',' | '-' if self.page > 0 => {
                            self.page = self.page.saturating_sub(1);
                            return Ok(SessionOutput::Updated(self.update()));
                        }
                        _ => {}
                    }
                }
                let punct = fullwidth_punct(c).expect("守卫已检查");
                if self.pinyin.is_empty() && self.confirmed.is_empty() {
                    Ok(SessionOutput::Commit(self.commit_raw(punct.to_string())))
                } else {
                    // 标点/英文字符结束这次组合:已确认段 + 当前候选(或原样
                    // 拼音)**一次**上屏,再把该字符拼在末尾。不能走
                    // `commit_candidate`——它在「只覆盖前缀」时会返回 Updated
                    // (段留在组合里),标点就没地方落地了。
                    let out = match self.candidates.get(self.cursor).cloned() {
                        Some(c) => SessionOutput::Commit(self.commit_selection(&c)),
                        None => SessionOutput::Commit(self.finish_with_raw(self.pinyin.clone())),
                    };
                    // 与 Space 分支的唯一差别:commit_* 已把该段消耗掉,
                    // 这里只把标点追加进上屏尾部并拼到上屏文本后面。
                    // 标点用 push_tail 而非 record_committed:既不触发学习
                    // (标点不是词,不进 UserLM——对应 librime 标点不进 memory),
                    // 也不动 last_word(刚上屏的「你好」仍是后续打分的有效
                    // bigram 上文,不被标点抹掉)。**分段上屏时标点跟着那一段
                    // 一起上屏,剩余串继续组合**(组合内的标点同样是上下文)。
                    self.push_tail(punct);
                    Ok(append_to_commit(out, punct))
                }
            }
            // —— Verbatim:直接上屏的字符(Shift+字母 / CapsLock,
            //    见 InputEvent::Verbatim 的文档) ——
            // 处理与上面标点分支同一形状:拼音串非空时先按空格语义
            // 上屏高亮候选(无候选则原样上屏拼音串),再追加该字符;
            // 为空时直接上屏。差别只有两点:字符是原样半角(不做
            // 全角映射),且语义上是「敲了一个英文字母」不是标点——
            // 但两者都只用 push_tail 进上屏尾部、不学、不动 last_word。
            InputEvent::Verbatim(c) => {
                if self.pinyin.is_empty() && self.confirmed.is_empty() {
                    Ok(SessionOutput::Commit(self.commit_raw(c.to_string())))
                } else {
                    // 标点/英文字符结束这次组合:已确认段 + 当前候选(或原样
                    // 拼音)**一次**上屏,再把该字符拼在末尾。不能走
                    // `commit_candidate`——它在「只覆盖前缀」时会返回 Updated
                    // (段留在组合里),标点就没地方落地了。
                    let out = match self.candidates.get(self.cursor).cloned() {
                        Some(c) => SessionOutput::Commit(self.commit_selection(&c)),
                        None => SessionOutput::Commit(self.finish_with_raw(self.pinyin.clone())),
                    };
                    // 单字母不是词:不进 UserLM 学习(与标点同一取舍),
                    // 也不动 last_word(刚上屏的候选仍是有效 bigram 上文)。
                    self.push_tail(c.encode_utf8(&mut [0u8; 4]));
                    Ok(append_to_commit(out, c.encode_utf8(&mut [0u8; 4])))
                }
            }
            // —— `-`/`=` 翻页(librime `paging_with_minus_equal`,has_menu)——
            // 这两个键**没有全角标点映射**(上面 punctuator 分支够不着),
            // 也无标点含义冲突,故候选窗开着时一律翻页:`=` 下一页(has_menu),
            // `-` 上一页(同 `,` 的 paging 边界:仅 page>0 回翻,首页钳 0)。
            // 空闲(无候选窗)时落回 catch-all 透传,键不被吃掉。
            InputEvent::Char(c @ ('-' | '=')) if !self.candidates.is_empty() => {
                let max_page = self
                    .candidates
                    .len()
                    .div_ceil(self.page_size)
                    .saturating_sub(1);
                if c == '=' {
                    self.page = (self.page + 1).min(max_page);
                } else if self.page > 0 {
                    self.page = self.page.saturating_sub(1);
                }
                Ok(SessionOutput::Updated(self.update()))
            }
            // —— 其余可打印字符:透传给应用 ——
            InputEvent::Char(_) => Ok(SessionOutput::Noop),

            // —— 数字选词 ——
            InputEvent::Select(n) if !self.pinyin.is_empty() => {
                let idx = if n == 0 { 9 } else { (n - 1) as usize };
                match self.candidates.get(idx).cloned() {
                    Some(c) => self.commit_candidate(&c),
                    None => Ok(SessionOutput::Noop), // 该数字无对应候选
                }
            }
            InputEvent::Select(_) => Ok(SessionOutput::Noop),

            // —— 候选面板点击(全局下标) ——
            // 与空格/数字同一条出口:分段上屏语义、失败回滚都在
            // `commit_candidate` 里,IBus 层不需要另写一遍。
            InputEvent::CandidateClicked(index) if !self.pinyin.is_empty() => {
                match self.candidates.get(index).cloned() {
                    Some(c) => self.commit_candidate(&c),
                    None => Ok(SessionOutput::Noop),
                }
            }
            InputEvent::CandidateClicked(_) => Ok(SessionOutput::Noop),

            // —— 空格：选高亮候选；无候选则原样上屏拼音串 ——
            InputEvent::Space if self.composing() => {
                if self.pinyin.is_empty() {
                    // 只剩已确认段:空格就是「到此为止」。
                    return Ok(SessionOutput::Commit(self.finish_with_raw(String::new())));
                }
                match self.candidates.get(self.cursor) {
                    // 选中真实候选词：走 commit_word，词要进 last_word
                    Some(c) => {
                        let c = c.clone();
                        self.commit_candidate(&c)
                    }
                    // 没有候选（如只打了一半前缀）：拼音串原样上屏，是
                    // 原始字母，不算词（已确认段一并带上）
                    None => Ok(SessionOutput::Commit(self.finish_with_raw(self.pinyin.clone()))),
                }
            }
            InputEvent::Space => Ok(SessionOutput::Noop),

            // —— 回车：拼音串原样上屏（同主流输入法） ——
            InputEvent::Enter if self.composing() => {
                // 原样上屏:已确认段 + 剩余字母一次交给应用(回车就是
                // 「这次输入到此为止」;剩余串可能已被删空,只剩确认段)。
                let raw = self.pinyin.clone();
                Ok(SessionOutput::Commit(self.finish_with_raw(raw)))
            }
            InputEvent::Enter => Ok(SessionOutput::Noop),

            // —— Esc：清空 ——
            InputEvent::Escape if self.composing() => {
                // 取消当前输入:打断造词缓冲(结算已累积的段),
                // 保证缓冲不跨「用户明确取消」的边界泄漏。
                self.break_coin_run();
                self.reset();
                Ok(SessionOutput::Updated(self.update()))
            }
            InputEvent::Escape => Ok(SessionOutput::Noop),

            // —— 退格 ——
            InputEvent::Backspace if self.composing() => {
                // **回退上一次确认**(方案 b 的心智模型):只要还有已确认的段,
                // 退格就先退回**上一步的选择**——把那段消耗掉的字母原样还回
                // 输入串、重新出候选;而不是「按已经选好的去删输入末尾」。
                // 全部退回之后(没有已确认段了)才回到文本编辑惯例:删 caret
                // 前一个字符。
                //
                // 为什么不看 caret:选完一段后 caret 落在剩余串的**末尾**
                // (用户还在继续打字),若要求 caret 在分界处才回退,用户就得
                // 先把剩余串一个字一个字删光——那正是要修掉的手感。
                if !self.confirmed.is_empty() {
                    let seg = self.confirmed.pop().expect("上面判过非空");
                    self.pinyin.insert_str(0, &seg.letters);
                    // 回到「选之前那一步」的光标位置:串尾(继续打字就在后面接)。
                    self.caret = self.pinyin.len();
                    self.cursor = 0;
                    self.page = 0;
                    self.refresh()?;
                    return Ok(SessionOutput::Updated(self.update()));
                }
                // 删 caret 前一个字符(文本编辑惯例)。caret 在串首时
                // 无前字符可删,吃掉按键、状态不变——越界删除或透传
                // 都会破坏用户预期(透传会让应用删掉自己文本框里的字)。
                if self.caret > 0 {
                    // caret-1 必为 char 边界(串内只有单字节 ASCII)。
                    let removed = self.pinyin.remove(self.caret - 1);
                    self.caret -= removed.len_utf8();
                    self.cursor = 0;
                    self.page = 0;
                    self.refresh()?;
                }
                Ok(SessionOutput::Updated(self.update()))
            }
            InputEvent::Backspace => Ok(SessionOutput::Noop),

            // —— 候选导航 ——
            InputEvent::Up | InputEvent::Down if !self.pinyin.is_empty() => {
                let len = self.candidates.len();
                if len > 0 {
                    if matches!(event, InputEvent::Up) {
                        self.cursor = self.cursor.saturating_sub(1);
                    } else {
                        self.cursor = (self.cursor + 1).min(len - 1);
                    }
                }
                Ok(SessionOutput::Updated(self.update()))
            }
            InputEvent::Up | InputEvent::Down => Ok(SessionOutput::Noop),

            // 左右方向键:移动拼音串内光标(编辑语义)。拼音串非空时
            // 吃掉按键——即使 caret 已到边界(不再移动)也不透传:此时
            // 用户焦点在编辑拼音串,透传给应用会移动应用文本框的光标,
            // 与「左右键归输入法管」的心智模型冲突。拼音串为空时透传,
            // 让应用正常移动自己的光标。
            InputEvent::Left | InputEvent::Right if !self.pinyin.is_empty() => {
                if matches!(event, InputEvent::Left) {
                    self.caret = self.caret.saturating_sub(1);
                } else {
                    self.caret = (self.caret + 1).min(self.pinyin.len());
                }
                Ok(SessionOutput::Updated(self.update()))
            }
            InputEvent::Left | InputEvent::Right => Ok(SessionOutput::Noop),

            // —— 翻页 ——
            InputEvent::PageUp | InputEvent::PageDown if !self.pinyin.is_empty() => {
                let max_page = self
                    .candidates
                    .len()
                    .div_ceil(self.page_size)
                    .saturating_sub(1);
                if matches!(event, InputEvent::PageUp) {
                    self.page = self.page.saturating_sub(1);
                } else {
                    self.page = (self.page + 1).min(max_page);
                }
                Ok(SessionOutput::Updated(self.update()))
            }
            InputEvent::PageUp | InputEvent::PageDown => Ok(SessionOutput::Noop),

            // —— Ctrl+Delete:删当前高亮候选(librime editor 的 DeleteCandidate)——
            // 有候选窗且高亮有效:把该 (pinyin key, word) 屏蔽(用户造词
            // 同时删词条,词库词只屏蔽——词库只读),随后重查候选让列表
            // 立刻少一条。无候选(拼音串空或列表空)时透传,让应用收到
            // 正常的 Ctrl+Delete(删词)。删到列表空也不 panic:refresh
            // 对空候选收敛 cursor/page,update 照常返回。
            InputEvent::DeleteCandidate if !self.pinyin.is_empty() => {
                if let Some(c) = self.candidates.get(self.cursor).cloned() {
                    self.engine.delete_candidate(&c.word, &c.pinyin);
                    self.refresh()?;
                }
                Ok(SessionOutput::Updated(self.update()))
            }
            InputEvent::DeleteCandidate => Ok(SessionOutput::Noop),
        }
    }

    /// 重新计算候选列表，并把高亮/页码收敛到合法范围。
    ///
    /// TODO: 这里目前是"全量重查"；性能优化方向是增量更新
    /// （追加一个字母时只查新增前缀 + 复用旧候选）。
    fn refresh(&mut self) -> Result<(), EngineError> {
        // recognizer 重判(每次重查候选前):整串命中邮箱/网址 pattern
        // 时进入(或保持)识别态——候选列表**只有一条**「原样上屏」的
        // 回声候选(词 = 原始串本身,`echo = true`:上屏走 commit_raw
        // 语义,不学习、不进造词缓冲),拼音候选整批消失;不再命中时
        // 退出识别态、恢复查词典。这条重判同时给出「继续打字不反悔」
        // (每多一个字符整串仍命中)与「Backspace 退回」(`abc@` 删掉
        // `@` 变回 `abc` 不再命中,候选立刻恢复)两个语义,不需要
        // 单独的滞回状态机。
        //
        // **`.` 触发键补回**:`recognize` 对「url 前缀 + 空后缀」
        // 刻意不判成立(见 recognizer.rs 模块文档),于是 `www` + `.`
        // 落键瞬间不识别,`.` 被既有「候选窗非空 → 翻页」语义吃掉、
        // 不进串。下一个字母到来时(`wwwe`),识别要先试「把那个被
        // 翻页吃掉的点补回去」的形状(`www.e`)——命中则补点进串、
        // 进识别态。「串以 www/ftp 开头、串内还没有点、补点后命中
        // url」三个条件凑齐,唯一来源就是「用户刚在该前缀后敲过一个
        // 被翻页吃掉的 `.`」;对正常拼音不可达(`wwwe` 之类本身
        // 不是合法切分,只有回声候选)。caret 跟随右移一位。
        //
        // `:` 触发的前缀(`http:`/`https:`/`mailto:`/`file:`/`ftp:`)
        // **不走这条路**:`:` 落键时已由 handle 的 `:` 前向臂直接进串
        // (见「recognizer 入口 2」),不存在「被吃掉再补回」的形状。
        // 但串此刻**就是**一条前缀(`http:`),「前缀 + 空后缀」的
        // 收窄会让 recognize 返回 None、识别态立不住、`/` 落进标点
        // 分支被映射成顿号(坏结果「http:、、xcom」)——所以「串
        // 本身等于某条 url 前缀」也算识别成立:它只能来自 `:` 前向臂
        // (`.`,` 触发的 `www.`/`ftp.` 进不了串,见上文),用户在
        // 协议串中途,直通必须生效。
        self.recognized = crate::recognize(&self.pinyin);
        if self.recognized.is_none() && crate::URL_PREFIXES.contains(&self.pinyin.as_str()) {
            self.recognized = Some(crate::Recognized::Url);
        }
        if self.recognized.is_none() && !self.pinyin.contains('.') {
            for bare in ["www", "ftp"] {
                if let Some(rest) = self.pinyin.strip_prefix(bare) {
                    if !rest.is_empty() {
                        let restored = format!("{bare}.{rest}");
                        if crate::recognize(&restored).is_some() {
                            self.pinyin = restored;
                            if self.caret >= bare.len() {
                                self.caret += 1;
                            }
                            self.recognized = crate::recognize(&self.pinyin);
                        }
                        break;
                    }
                }
            }
        }
        if self.pinyin.is_empty() {
            self.candidates.clear();
        } else if self.recognized.is_some() {
            // 识别态:候选 = 原文本身(回声形态)。借回声候选的既有
            // 语义:它是「词库解不出的原始输入串」,上屏按 commit_raw
            // 处理——邮箱/网址同样不是词、不该进 UserLM/造词。
            self.candidates = vec![Candidate {
                word: self.pinyin.clone(),
                pinyin: self.pinyin.clone(),
                score: 0.0,
                freq: 0,
                spelling: crate::SpellingKind::Exact,
                echo: true,
                // 识别态(邮箱/网址)的回声候选整串上屏:它就是要原样
                // 上屏整串输入,剩余串按定义为空。
                covered: self.pinyin.len(),
            }];
        } else {
            self.candidates = self
                .engine
                .lookup_candidates(&self.pinyin, &self.lm_context())?;
        }
        let max_cursor = self.candidates.len().saturating_sub(1);
        self.cursor = self.cursor.min(max_cursor);
        let max_page = self
            .candidates
            .len()
            .div_ceil(self.page_size)
            .saturating_sub(1);
        self.page = self.page.min(max_page);
        Ok(())
    }

    /// 取当前界面快照。
    /// 组合还活着吗——**剩余串与已确认段任一非空**都算。
    ///
    /// 不能只看 `pinyin`:选完一段后剩余串可能被退格删空(已确认段还在),
    /// 此时回车/空格/Esc/退格都必须按「组合仍在」处理,否则用户选过的段
    /// 既上不了屏、也退不回。
    fn composing(&self) -> bool {
        !self.pinyin.is_empty() || !self.confirmed.is_empty()
    }

    fn update(&self) -> SessionUpdate {
        // 预编辑区 = **已确认的段 + 剩余拼音串**(方案 b:确认段留在组合里,
        // 不交给应用)。光标要在剩余串里的插入点上,所以偏移已确认段的
        // **字符数**(IBus 的 preedit cursor 是字符下标;已确认段是中文,
        // 不能按字节加)。
        let mut preedit = self.confirmed_text();
        preedit.push_str(&self.pinyin);
        let caret = self.confirmed_chars() + self.caret;
        SessionUpdate {
            preedit,
            candidates: self.candidates.clone(),
            cursor: self.cursor,
            caret,
            page: self.page,
            page_size: self.page_size,
        }
    }

    /// 已确认段拼成的文本(预编辑区显示 + 最终上屏都要用)。
    fn confirmed_text(&self) -> String {
        self.confirmed.iter().map(|s| s.candidate.word.as_str()).collect()
    }

    /// 已确认段的**字符**数(预编辑光标偏移用)。
    fn confirmed_chars(&self) -> usize {
        self.confirmed
            .iter()
            .map(|s| s.candidate.word.chars().count())
            .sum()
    }

    /// 把已确认段的账补上(造词缓冲 / 上屏尾部 / UserLM),按用户选中的顺序。
    ///
    /// 记账推迟到组合结束是方案 b 的关键:中途确认的段还没交给应用,用户
    /// 随时可以退格回退——若在确认当时就写 UserLM,回退就得做「反学习」。
    fn replay_confirmed(&mut self) {
        let held = std::mem::take(&mut self.confirmed);
        for seg in &held {
            self.record_commit(&seg.candidate);
        }
    }

    /// 组合结束:已确认段 + 这次选中的整串候选**一次**交给应用。
    fn commit_selection(&mut self, c: &Candidate) -> String {
        let mut text = self.confirmed_text();
        self.replay_confirmed();
        text.push_str(&self.record_commit(c));
        self.reset();
        text
    }

    /// 组合以「原样拼音」结束(回车 / 切英文 / 标点 / 无候选时的空格):
    /// 已确认段照样上屏,尾部原始字母跟着一起。
    fn finish_with_raw(&mut self, raw: String) -> String {
        let mut text = self.confirmed_text();
        self.replay_confirmed();
        self.break_coin_run();
        self.record_committed(&raw, None);
        text.push_str(&raw);
        self.reset();
        text
    }

    /// 上屏指定候选词并消耗它覆盖的那段输入。
    ///
    /// 覆盖范围未标注(`0`)或已达串尾时按**整串**处理并复位;只覆盖前缀时
    /// 把这一段**收进已确认段**(方案 b),不交给应用、也不记账。
    ///
    /// 本方法只做状态变更、**不刷新候选**(见 [`Self::commit_candidate`]
    /// 的三段式:先消耗 + 刷新,成功后才记账)。
    fn commit_word(&mut self, c: &Candidate) -> String {
        let word = self.record_commit(c);
        self.consume_covered(c);
        word
    }

    /// 上屏的**记账**部分:造词缓冲、上屏尾部(bigram 上文)、UserLM 学习。
    ///
    /// 与「消耗输入串」拆开是因为分段上屏要先搬走前缀、再刷新剩余串的
    /// 候选,刷新失败必须能原样回滚——那时**不能**留下任何上屏副作用。
    fn record_commit(&mut self, c: &Candidate) -> String {
        let word = c.word.clone();
        if c.echo {
            self.break_coin_run();
        } else {
            self.push_coin(c);
        }
        let as_word = if c.echo { None } else { Some(word.as_str()) };
        self.record_committed(&word, as_word);
        word
    }

    /// 候选要消耗的输入字节数(`covered == 0` = 未知/整串)。
    ///
    /// 串内理论上只有 ASCII(字母 + `'`),但这里仍按字符边界回退——
    /// 消费量越界或落在半个字符上时宁可少消耗,也绝不 panic。
    fn covered_len(&self, c: &Candidate) -> usize {
        let raw = if c.covered == 0 {
            self.pinyin.len()
        } else {
            c.covered.min(self.pinyin.len())
        };
        let mut n = raw;
        while n > 0 && !self.pinyin.is_char_boundary(n) {
            n -= 1;
        }
        n
    }

    /// 消耗候选覆盖的前缀:够整串就复位(旧语义),否则只搬走前缀。
    ///
    /// 编辑光标按**同一个位置**平移(`caret -= covered`),于是「在串尾打字」
    /// 的用户上屏一段后光标仍在剩余串末尾——与 librime `OnSelect` 里
    /// `set_caret_pos(input.length())` 的语义一致;光标原本落在被消耗的
    /// 前缀里时钳到串首(`saturating_sub`),不会指到串外。
    fn consume_covered(&mut self, c: &Candidate) {
        let covered = self.covered_len(c);
        if covered >= self.pinyin.len() {
            self.reset();
            return;
        }
        self.pinyin.drain(..covered);
        self.caret = self.caret.saturating_sub(covered);
        self.cursor = 0;
        self.page = 0;
    }

    /// 上屏候选并决定「界面接下来怎么办」——空格/数字/点击/标点的统一出口。
    ///
    /// - 覆盖整串 ⇒ [`SessionOutput::Commit`](整个组合一次上屏 + 收起);
    /// - 只覆盖前缀 ⇒ [`SessionOutput::Updated`](这一段收进已确认段,
    ///   留在预编辑区,剩余串继续出候选——方案 b)。
    ///
    /// **失败回滚**:剩余串刷新候选失败时把刚搬走的前缀原样塞回去,并
    /// 保持会话不变量——「已上屏文本 + 剩余输入串」的并集恒等于用户敲过的
    /// 全部字母。这条不变量正是分段上屏要修的东西:改动前选一段会把后面
    /// 的字母**静默丢掉**(既不进文档也不留在串里)。
    fn commit_candidate(&mut self, c: &Candidate) -> Result<SessionOutput, EngineError> {
        let covered = self.covered_len(c);
        if covered >= self.pinyin.len() {
            // 整串选完 = 整个组合结束:已确认段与这一条**一次**上屏。
            return Ok(SessionOutput::Commit(self.commit_selection(c)));
        }
        let prefix: String = self.pinyin[..covered].to_owned();
        self.consume_covered(c);
        if let Err(e) = self.refresh() {
            // 回滚:字母一个都不能少(候选查询失败不该吞掉用户输入)。
            self.pinyin.insert_str(0, &prefix);
            self.caret += prefix.len();
            self.cursor = 0;
            self.page = 0;
            return Err(e);
        }
        // **方案 b**:这一段留在组合里(预编辑区显示为已确认文本),不交给
        // 应用;记账也推迟到组合结束(见 `replay_confirmed`)——于是退格
        // 回退不需要撤销任何学习。界面照常刷新:剩余串的候选按剩余串重算。
        self.confirmed.push(ConfirmedSegment {
            candidate: c.clone(),
            letters: prefix,
        });
        Ok(SessionOutput::Updated(self.update()))
    }

    /// 上屏任意文本并复位会话。原文计入上屏尾部；
    /// 原始字母/标点不是词，`last_word` 置 `None`。
    fn commit_raw(&mut self, text: String) -> String {
        // 原始上屏打断造词缓冲(标点/回车/Verbatim 大写都经此路径,
        // 即「不跨标点造词」「不跨模式切换造词」的落点)。
        self.break_coin_run();
        self.record_committed(&text, None);
        self.reset();
        text
    }

    /// 把一次非回声候选上屏推进造词缓冲。
    ///
    /// 候选自带的 `pinyin` 字段就是「空格分隔的音节串」(与词典 key
    /// 同格式),直接入缓冲;段在 [`Self::break_coin_run`] 时才结算。
    fn push_coin(&mut self, c: &Candidate) {
        self.coin_buffer.push((c.word.clone(), c.pinyin.clone()));
        // 长度上限:超限即把当前缓冲结算后清空(防失控长段粘成
        // 一个词;见 MAX_COIN_CHARS 注释)。结算发生在「打断」语义
        // 之前——超限本身就是一种打断。
        //
        // 注意:这里用的是**字符**总数,与 is_reusable_phrase 的
        // 「片段数/单字」判据是两件事,不要混:本上限拦的是「单次
        // 编码片段过长」,那条拦的是「这一串到底像不像一个想复用的
        // 短语」。两者各自独立生效。
        let chars: usize = self
            .coin_buffer
            .iter()
            .map(|(w, _)| w.chars().count())
            .sum();
        if chars > MAX_COIN_CHARS {
            self.break_coin_run();
        }
    }

    /// 打断造词缓冲:缓冲够格时把整段造成一条用户词条
    /// (词 = 各片段拼接,拼音 key = 各片段拼音空格连接),随后清空。
    ///
    /// 「够格」= [`Self::is_reusable_phrase`](见那里的实测依据与权衡)。
    /// 不够格(连续打字的句子)时**只清空、不造词**——缓冲本身仍要
    /// 清掉,否则下一次打断会把上一句的尾巴也拼进来。
    fn break_coin_run(&mut self) {
        if self.is_reusable_phrase() {
            let word: String = self.coin_buffer.iter().map(|(w, _)| w.as_str()).collect();
            let key: String = self
                .coin_buffer
                .iter()
                .map(|(_, p)| p.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            self.engine.coin_entry(&word, &key);
        }
        self.coin_buffer.clear();
    }

    /// 当前造词缓冲是否够格造成一条用户词条。
    ///
    /// 判据(依据见 [`COIN_ALL_SEGMENTS_SINGLE_CHAR`] 的完整注释):
    /// 1. 片段数 ≥ [`MIN_COIN_SEGMENTS`]——一次完整词上屏不造词;
    /// 2. **全部**片段都是单字——逐字组词(「张三丰」「西双版纳」)
    ///    满足;只要有**一个**整词片段(连续打字的形状)就不满足,
    ///    因而「设置为说着玩的说说中文」这类句子不会被粘成词条。
    ///
    /// 抽成独立方法是为了让它**可被直接单测**:造词是「用户无感知地
    /// 写入持久数据」的行为,判据必须能被逐条钉住,而不是埋在
    /// `break_coin_run` 里只能通过整体副作用观察。
    fn is_reusable_phrase(&self) -> bool {
        if self.coin_buffer.len() < MIN_COIN_SEGMENTS {
            return false;
        }
        if !COIN_ALL_SEGMENTS_SINGLE_CHAR {
            return true;
        }
        self.coin_buffer
            .iter()
            .all(|(w, _)| w.chars().count() == 1)
    }

    /// 把上屏文本追加进有界尾部并更新 `last_word`。
    ///
    /// 截断必须按 `char` 边界（汉字/emoji 都是多字节，按字节切会
    /// 切坏 UTF-8 序列），只保留末尾 ≤ `COMMITTED_TAIL_LIMIT` 个字符。
    fn record_committed(&mut self, text: &str, word: Option<&str>) {
        // 第 5 步学习钩子:候选词上屏(Some)学 (旧 last_word, word)
        // 词对;原始字母/标点(None)不学——它不是词,与下方
        // `last_word` 置 `None` 是同一判断。
        self.engine.learn_committed(self.last_word.as_deref(), word);
        self.push_tail(text);
        self.last_word = word.map(str::to_string);
    }

    /// 只把文本追加进有界上屏尾部,不触发学习、不动 `last_word`。
    ///
    /// 用于中文标点(见 `handle` 的标点分支):标点要被后续打分的
    /// 字符级窗口(`committed_tail`)看见,但它不是词——不进 UserLM
    /// 学习,也不该把刚上屏的词从 `last_word` 里抹掉。
    fn push_tail(&mut self, text: &str) {
        self.committed_tail.push_str(text);
        let excess = self
            .committed_tail
            .chars()
            .count()
            .saturating_sub(COMMITTED_TAIL_LIMIT);
        if excess > 0 {
            // 从头丢 `excess` 个完整字符(`char` 边界,不碰字节)。
            self.committed_tail = self.committed_tail.chars().skip(excess).collect();
        }
    }

    /// 直接上屏第 `index` 个候选（全局下标，跨页有效）。
    ///
    /// 供已有的调用方(测试、以及任何不走 `handle` 的路径)使用；
    /// 下标越界或没有候选时返回 `None`，会话状态不变。**分段上屏语义
    /// 与 `handle` 一致**:只消耗候选覆盖的那段,剩余串继续组合;这里
    /// 顺手把剩余串的候选刷新掉,让调用方看到的会话始终自洽(刷新失败
    /// 只记日志——本函数的返回类型无法表达错误;需要错误语义的调用方
    /// 请走 [`InputEvent::CandidateClicked`])。
    pub fn commit_at(&mut self, index: usize) -> Option<String> {
        let candidate = self.candidates.get(index)?.clone();
        let text = self.commit_word(&candidate);
        if !self.pinyin.is_empty() {
            if let Err(e) = self.refresh() {
                tracing::warn!(error = %e, "分段上屏后刷新剩余串候选失败,候选窗仍是旧列表");
            }
        }
        Some(text)
    }

    /// 复位到空闲状态。
    ///
    /// 只清「当前输入」（拼音串/候选/高亮/页码），**不清已上屏上下文**
    /// （`committed_tail` / `last_word`）：上文要跨候选保留，这正是它的
    /// 意义。上屏路径（[`Self::commit_word`] / [`Self::commit_raw`]）先
    /// 更新上文再调用本方法；「新句子/焦点切换」的显式清空留给后续步骤
    /// （IBus `focus-out`）接入。
    pub fn reset(&mut self) {
        // 注意:本方法**不清造词缓冲**——上屏路径(commit_word /
        // commit_raw)每次都会调用本方法,而造词缓冲恰恰要「跨多次
        // 上屏累积」才有意义。缓冲的取消由 Escape 分支与 toggle_ascii
        // 显式打断(见对应分支),不是这里的职责。
        self.pinyin.clear();
        self.caret = 0;
        self.candidates.clear();
        self.cursor = 0;
        self.page = 0;
        self.confirmed.clear();
        self.recognized = None;
    }

    /// 焦点切换（IBus `FocusOut` / `Reset` / `Disable`）的复位。
    ///
    /// 与 [`Self::reset`] 的差别：**先结算并打断造词缓冲**。
    /// 换了输入焦点就是换了上下文，跨焦点的上屏片段不该粘成一个词——
    /// 在微信里逐字选「张」「三」、切到浏览器逐字选「李」「四」，
    /// 不该造出「张三李四」。librime 的 memory 同样把焦点切换当作
    /// 真正的边界（见 `docs/gear-audit.md` 第 3 节）。
    pub fn reset_for_focus_change(&mut self) -> String {
        // 已确认的段是用户明确选过的,焦点切换不能把它们丢掉——返回给
        // 调用方(xwx_ibus)去 `CommitText`;未确认的剩余字母按原有语义丢弃
        // (那是「还没选」的内容,不该凭空进文档)。
        let text = self.confirmed_text();
        self.replay_confirmed();
        self.break_coin_run();
        self.reset();
        text
    }

    /// 切换中英文模式。
    ///
    /// 由 [`InputEvent::ToggleAscii`]（Ctrl+空格）触发：
    /// - 中文 → 英文：若拼音串仍未上屏，先原样上屏（不丢字）；
    /// - 英文 → 中文：直接切回（英文模式下拼音串必然为空）。
    ///
    /// `reset` 不清除模式：中/英状态跨焦点切换保持，直到再次按 Shift。
    fn toggle_ascii(&mut self) -> SessionOutput {
        if !self.pinyin.is_empty() && !self.english_mode {
            // 切英文时已确认段一起上屏(`finish_with_raw` 拼上并记账)。
            let text = self.finish_with_raw(self.pinyin.clone());
            self.english_mode = true;
            SessionOutput::Commit(text)
        } else {
            self.english_mode = !self.english_mode;
            self.reset();
            SessionOutput::Noop
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ImeEngine;
    use crate::LmStore;
    use crate::Dictionary as _; // DictStore.lookup(测试里直查词库用)

    fn session_with_dict() -> InputSession {
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        // 手工塞几条词（内存词库）
        engine
            .dict
            .upsert("你好", "ni hao", 100)
            .expect("插入失败");
        engine.dict.upsert("你", "ni", 200).expect("插入失败");
        engine.dict.upsert("好", "hao", 150).expect("插入失败");
        InputSession::new(Arc::new(engine))
    }

    /// 带「中→国」词对的会话：上文注入后 bigram 分要能改变候选排序。
    ///
    /// 词频：过(20) > 国(10)，无上文时排前面的是「过」；有上文「中」
    /// 时 bigram(中,国) 分应把「国」抬到最前。
    fn session_with_lm() -> InputSession {
        let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("中", "zhong", 100).expect("插入失败");
        engine.dict.upsert("国", "guo", 10).expect("插入失败");
        engine.dict.upsert("过", "guo", 20).expect("插入失败");
        let LmStore::Bigram(lm) = &mut engine.lm else {
            panic!("默认引擎应为 bigram（测试 helper 用 new(None) 创建）");
        };
        lm.add_unigram("中", 100);
        lm.add_unigram("国", 100);
        lm.add_unigram("过", 100);
        lm.add_bigram("中", "国", 90);
        InputSession::new(Arc::new(engine))
    }

    #[test]
    fn typing_produces_updated() {
        let mut s = session_with_dict();
        let out = s
            .handle(InputEvent::Char('n'))
            .expect("不应失败");
        match out {
            SessionOutput::Updated(u) => {
                assert_eq!(u.preedit, "n");
            }
            other => panic!("期望 Updated，实际 {other:?}"),
        }
    }

    #[test]
    fn space_commits_candidate() {
        let mut s = session_with_dict();
        for c in ['n', 'i'] {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let out = s.handle(InputEvent::Space).expect("不应失败");
        match out {
            SessionOutput::Commit(text) => {
                assert_eq!(text, "你", "词频最高的 '你' 应被选中");
                assert!(!s.is_active(), "上屏后会话应复位");
            }
            other => panic!("期望 Commit，实际 {other:?}"),
        }
    }

    #[test]
    fn escape_clears() {
        let mut s = session_with_dict();
        s.handle(InputEvent::Char('n')).expect("不应失败");
        let out = s.handle(InputEvent::Escape).expect("不应失败");
        match out {
            SessionOutput::Updated(u) => assert!(u.preedit.is_empty()),
            other => panic!("期望 Updated，实际 {other:?}"),
        }
    }

    // —— B1:可编辑输入缓冲(caret)——

    #[test]
    fn left_right_move_caret_within_pinyin() {
        let mut s = session_with_dict();
        for c in "nihao".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        assert_eq!(s.caret(), 5, "追加输入后 caret 应在串尾");

        s.handle(InputEvent::Left).expect("不应失败");
        s.handle(InputEvent::Left).expect("不应失败");
        assert_eq!(s.caret(), 3, "左移两位后 caret 应在 3");

        s.handle(InputEvent::Right).expect("不应失败");
        assert_eq!(s.caret(), 4);

        // 右移到串尾后不再越界;按键仍被吃掉(Updated)
        s.handle(InputEvent::Right).expect("不应失败");
        let out = s.handle(InputEvent::Right).expect("不应失败");
        assert_eq!(s.caret(), 5, "caret 不应越过串尾");
        assert!(matches!(out, SessionOutput::Updated(_)), "串非空时左右键应被吃掉");

        // 左移出界同理
        for _ in 0..6 {
            s.handle(InputEvent::Left).expect("不应失败");
        }
        assert_eq!(s.caret(), 0, "caret 不应越过串首");
        let out = s.handle(InputEvent::Left).expect("不应失败");
        assert_eq!(s.caret(), 0);
        assert!(matches!(out, SessionOutput::Updated(_)), "串非空时左右键应被吃掉");
    }

    #[test]
    fn left_right_passthrough_when_idle() {
        let mut s = session_with_dict();
        // 拼音串为空:左右键透传给应用(让应用移动自己的光标)
        assert!(matches!(
            s.handle(InputEvent::Left).expect("不应失败"),
            SessionOutput::Noop
        ));
        assert!(matches!(
            s.handle(InputEvent::Right).expect("不应失败"),
            SessionOutput::Noop
        ));
    }

    #[test]
    fn insert_in_middle_refreshes_candidates_for_whole_string() {
        // 输入 nihao,caret 左移 2 位(停在 3,nih|ao),插入 x →
        // 缓冲 nihxao,候选必须基于完整新串 nihxao 而不是 caret 前的 nih。
        let mut s = session_with_dict();
        for c in "nihao".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        s.handle(InputEvent::Left).expect("不应失败");
        s.handle(InputEvent::Left).expect("不应失败");
        assert_eq!(s.caret(), 3);

        let out = s.handle(InputEvent::Char('x')).expect("不应失败");
        assert_eq!(s.pinyin(), "nihxao");
        assert_eq!(s.caret(), 4, "插入后 caret 应越过新字符");
        match out {
            SessionOutput::Updated(u) => {
                assert_eq!(u.preedit, "nihxao", "preedit 应为完整新串");
                assert_eq!(u.caret, 4);
                assert!(
                    u.candidates.iter().all(|c| c.pinyin != "你好"),
                    "nihxao 不是合法拼音,不应再出现「你好」候选"
                );
            }
            other => panic!("期望 Updated,实际 {other:?}"),
        }
    }

    #[test]
    fn backspace_deletes_char_before_caret() {
        // 输入 nihao,caret 左移 2 位(停在 3),Backspace 删的是 h → niao。
        let mut s = session_with_dict();
        for c in "nihao".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        s.handle(InputEvent::Left).expect("不应失败");
        s.handle(InputEvent::Left).expect("不应失败");
        let out = s.handle(InputEvent::Backspace).expect("不应失败");
        assert_eq!(s.pinyin(), "niao", "应删掉 caret 前的 h");
        assert_eq!(s.caret(), 2);
        match out {
            SessionOutput::Updated(u) => {
                assert_eq!(u.preedit, "niao");
                assert_eq!(u.caret, 2);
            }
            other => panic!("期望 Updated,实际 {other:?}"),
        }

        // caret 停在 2,Backspace 只删 caret 前的字符:再删 i、n 后
        // caret 到串首,caret 后的 "ao" 不受影响
        s.handle(InputEvent::Backspace).expect("不应失败");
        assert_eq!(s.pinyin(), "nao");
        assert_eq!(s.caret(), 1);
        let out = s.handle(InputEvent::Backspace).expect("不应失败");
        assert_eq!(s.pinyin(), "ao");
        assert_eq!(s.caret(), 0);
        match out {
            SessionOutput::Updated(u) => assert!(u.preedit == "ao" && u.caret == 0),
            other => panic!("期望 Updated,实际 {other:?}"),
        }
        // caret 已在串首:吃掉按键,"ao" 不动(串首无前字符可删)
        let out = s.handle(InputEvent::Backspace).expect("不应失败");
        assert_eq!(s.pinyin(), "ao", "caret 在串首时 Backspace 不应删任何字符");
        assert!(matches!(out, SessionOutput::Updated(_)), "按键应被吃掉");
    }

    #[test]
    fn backspace_to_empty_resets_caret() {
        // caret 在串尾连续删到空:串空后 caret 归 0,再按 Backspace 透传
        let mut s = session_with_dict();
        for c in "ni".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        s.handle(InputEvent::Backspace).expect("不应失败");
        let out = s.handle(InputEvent::Backspace).expect("不应失败");
        assert!(s.pinyin().is_empty());
        assert_eq!(s.caret(), 0, "删空后 caret 应归 0");
        match out {
            SessionOutput::Updated(u) => assert!(u.preedit.is_empty() && u.caret == 0),
            other => panic!("期望 Updated,实际 {other:?}"),
        }
        // 串已空:Backspace 透传给应用(Noop)
        assert!(matches!(
            s.handle(InputEvent::Backspace).expect("不应失败"),
            SessionOutput::Noop
        ));
    }

    #[test]
    fn backspace_at_caret_zero_is_safe_noop() {
        // caret 在串首按 Backspace:串不变、不 panic、按键被吃掉
        // (不透传给应用,否则应用会删掉自己文本框里的字)。
        let mut s = session_with_dict();
        for c in "ni".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        s.handle(InputEvent::Left).expect("不应失败");
        s.handle(InputEvent::Left).expect("不应失败");
        assert_eq!(s.caret(), 0);

        let out = s.handle(InputEvent::Backspace).expect("不应失败");
        assert_eq!(s.pinyin(), "ni", "caret 在串首时 Backspace 不应删任何字符");
        assert_eq!(s.caret(), 0);
        assert!(
            matches!(out, SessionOutput::Updated(ref u) if u.preedit == "ni"),
            "按键应被吃掉且状态不变: {out:?}"
        );
    }

    #[test]
    fn commit_uses_whole_string_not_prefix_before_caret() {
        // caret 不在串尾时按空格/数字/回车:上屏的都是完整串的结果。
        let mut s = session_with_dict();
        for c in "nihao".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        s.handle(InputEvent::Left).expect("不应失败");
        s.handle(InputEvent::Left).expect("不应失败");
        assert_eq!(s.caret(), 3);
        // 空格:完整串 nihao 的首选是「你好」,不是前缀 nih 的任何结果
        let out = s.handle(InputEvent::Space).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "你好"),
            "应上屏完整串的首选「你好」: {out:?}"
        );

        // 数字选词同上
        let mut s = session_with_dict();
        for c in "nihao".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        s.handle(InputEvent::Left).expect("不应失败");
        s.handle(InputEvent::Left).expect("不应失败");
        let out = s.handle(InputEvent::Select(1)).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "你好"),
            "数字选词应上屏完整串的第 1 个候选「你好」: {out:?}"
        );

        // 回车:完整串原样上屏
        let mut s = session_with_dict();
        for c in "nihao".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        s.handle(InputEvent::Left).expect("不应失败");
        s.handle(InputEvent::Left).expect("不应失败");
        let out = s.handle(InputEvent::Enter).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "nihao"),
            "回车应上屏完整串 nihao,不是前缀 nih: {out:?}"
        );
    }

    #[test]
    fn caret_resets_on_commit_and_escape() {
        // 上屏后 caret 归 0
        let mut s = session_with_dict();
        for c in "nihao".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        s.handle(InputEvent::Left).expect("不应失败");
        let out = s.handle(InputEvent::Space).expect("不应失败");
        assert!(matches!(out, SessionOutput::Commit(_)));
        assert_eq!(s.caret(), 0, "上屏后 caret 应归 0");
        assert!(!s.is_active());

        // Escape 后 caret 归 0
        let mut s = session_with_dict();
        for c in "ni".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        s.handle(InputEvent::Left).expect("不应失败");
        assert_eq!(s.caret(), 1);
        let out = s.handle(InputEvent::Escape).expect("不应失败");
        match out {
            SessionOutput::Updated(u) => {
                assert!(u.preedit.is_empty());
                assert_eq!(u.caret, 0, "Escape 清空后 caret 应归 0");
            }
            other => panic!("期望 Updated,实际 {other:?}"),
        }
        assert_eq!(s.caret(), 0);
    }

    #[test]
    fn commit_at_after_caret_move_still_uses_whole_string() {
        // commit_at(面板点击路径)在 caret 移动后也应基于完整串的候选。
        let mut s = session_with_dict();
        for c in "nihao".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        s.handle(InputEvent::Left).expect("不应失败");
        s.handle(InputEvent::Left).expect("不应失败");
        let word = s.commit_at(0).expect("下标 0 应有候选");
        assert_eq!(word, "你好", "应提交完整串的首选,不是前缀 nih 的结果");
        assert_eq!(s.caret(), 0, "上屏后 caret 应归 0");
    }

    #[test]
    fn caret_visible_in_session_update() {
        // SessionUpdate.caret 跟随编辑光标,engine.rs 拿它做 preedit 光标。
        let mut s = session_with_dict();
        for c in "nihao".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let out = s.handle(InputEvent::Left).expect("不应失败");
        match out {
            SessionOutput::Updated(u) => {
                assert_eq!(u.preedit, "nihao");
                assert_eq!(u.caret, 4, "左移后 update 里的 caret 应为 4");
                assert_eq!(u.cursor, 0, "移动编辑光标不应动候选高亮");
            }
            other => panic!("期望 Updated,实际 {other:?}"),
        }
    }

    #[test]
    fn noop_when_idle() {
        let mut s = session_with_dict();
        assert!(matches!(
            s.handle(InputEvent::Space).expect("不应失败"),
            SessionOutput::Noop
        ));
    }

    #[test]
    fn ctrl_space_toggles_chinese_english() {
        let mut s = session_with_dict();
        assert!(!s.english_mode(), "初始应为中文模式");

        // Ctrl+空格：切到英文模式
        s.handle(InputEvent::ToggleAscii).expect("切换不应失败");
        assert!(s.english_mode(), "Ctrl+空格后应进入英文模式");

        // 英文模式下字母不进入拼音，直接透传
        let out = s.handle(InputEvent::Char('h')).expect("不应失败");
        assert!(matches!(out, SessionOutput::Noop));

        // 再按一次 Ctrl+空格：切回中文
        s.handle(InputEvent::ToggleAscii).expect("切换不应失败");
        assert!(!s.english_mode(), "再次 Ctrl+空格应回到中文模式");

        // 回到中文后字母恢复进入拼音串
        let out = s.handle(InputEvent::Char('h')).expect("不应失败");
        assert!(matches!(out, SessionOutput::Updated(ref u) if u.preedit == "h"));
    }

    #[test]
    fn toggle_during_composition_commits_pinyin_first() {
        let mut s = session_with_dict();
        for c in ['n', 'i'] {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        // 输入到一半切英文：未上屏的拼音原样上屏，不丢字
        let out = s.handle(InputEvent::ToggleAscii).expect("切换不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "ni"),
            "切换为英文时应先把拼音上屏"
        );
        assert!(s.english_mode(), "应已进入英文模式");
        assert!(!s.is_active(), "上屏后会话应复位");
    }

    #[test]
    fn commit_word_learns_but_commit_raw_does_not() {
        // 端到端学习钩子:引擎挂用户层后,候选词上屏应学习、
        // 原始字母/标点上屏不应学习(后者不是词)。
        let engine = Arc::new(
            crate::ImeEngine::new(None)
                .expect("内存引擎创建失败")
                .with_user_lm(crate::UserLM::in_memory()),
        );
        engine.dict.upsert("测试", "ce shi", 10).expect("插入失败");
        let mut s = InputSession::new(Arc::clone(&engine));

        for ch in "ceshi".chars() {
            s.handle(InputEvent::Char(ch)).expect("按键失败");
        }
        let out = s.handle(InputEvent::Space).expect("空格选词失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "测试"),
            "应上屏候选词「测试」: {out:?}"
        );
        let user = engine.user_lm().expect("引擎应有用户层");
        let snap = user.snapshot();
        let learned = user.counts_with(&snap, None, "测试");
        let unknown = user.counts_with(&snap, None, "没学过的词");
        assert!(
            learned.0 > 0 && unknown == (0, 0),
            "候选词上屏应产生计数,没学过的词不应有计数: {learned:?} vs {unknown:?}"
        );

        let before = user.unigram_len();
        s.handle(InputEvent::Char('a')).expect("按键失败");
        let out = s.handle(InputEvent::Enter).expect("回车原样上屏失败");
        assert!(matches!(out, SessionOutput::Commit(ref t) if t == "a"));
        assert_eq!(
            user.unigram_len(),
            before,
            "commit_raw 原始字母上屏不应产生学习"
        );
    }

    #[test]
    fn echo_candidate_commit_does_not_learn() {
        // 回声候选(词库解不出的原始输入串)上屏不应被当成词学习——
        // 它不是词,语义等同 commit_raw(对应 librime raw 不进 memory)。
        // 引擎挂内存用户层,断言上屏回声候选后 unigram_len() 不变。
        let engine = Arc::new(
            crate::ImeEngine::new(None)
                .expect("内存引擎创建失败")
                .with_user_lm(crate::UserLM::in_memory()),
        );
        // 空词库:输入 "zgr" 解不出任何真实候选,只会产出回声候选
        let mut s = InputSession::new(Arc::clone(&engine));

        for ch in "zgr".chars() {
            s.handle(InputEvent::Char(ch)).expect("按键失败");
        }
        let user = engine.user_lm().expect("引擎应有用户层");
        let before = user.unigram_len();

        // 空格选中唯一候选(即回声候选)上屏
        let out = s.handle(InputEvent::Space).expect("空格选词失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "zgr"),
            "回声候选上屏应提交原始输入串: {out:?}"
        );
        assert_eq!(
            user.unigram_len(),
            before,
            "回声候选上屏不应产生学习(等同 commit_raw)"
        );
        assert_eq!(s.last_word, None, "回声候选上屏后 last_word 应为 None");
    }

    #[test]
    fn commit_at_commits_global_index() {
        let mut s = session_with_dict();
        for c in ['n', 'i', 'h', 'a', 'o'] {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        // 第 2b 步起候选按「整条路径」打分：
        // 路径[你好]（一个词，-700）压过路径[你,好]（两个词，-1250），
        // 所以首选是整串解码出的「你好」，而不是逐字拆开的「你」——
        // 这正是 beam search 解码要的可见行为（短语整体概率 > 逐字累加）。
        // 次选仍是单字候选：你(-1250) > 好(-1250)，可以只上屏一个字。
        let word = s.commit_at(0).expect("下标 0 应有候选");
        assert_eq!(word, "你好");
        assert!(!s.is_active(), "上屏后会话应复位");
        assert!(s.commit_at(0).is_none(), "复位后不应再取到候选");
    }

    /// 分段上屏用的会话:词库里有「我们」「今天」两个整词与它们的单字,
    /// 输入 `womenjintian` 整串可解(格边候选覆盖各自那一段)。
    fn session_with_segments() -> InputSession {
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("我们", "wo men", 300).expect("插入失败");
        engine.dict.upsert("我", "wo", 500).expect("插入失败");
        engine.dict.upsert("今天", "jin tian", 300).expect("插入失败");
        InputSession::new(Arc::new(engine))
    }

    /// 选中只覆盖前缀的候选之后,**剩下的字母必须留在串里**,并且面板
    /// 立刻显示剩余串的候选——这是「一长串拼音连不起来时,正确的先选,
    /// 选完接着后面的」的最小可验证形状。
    ///
    /// 定向破坏(逐条都能让它变红):
    /// - `consume_covered` 改成 `reset()`(改动前的行为)→ `pinyin()` 为空、
    ///   输出变回 `Commit` → 红;
    /// - `covered_len` 忽略 `Candidate::covered` 一律整串 → 同上 → 红;
    /// - `commit_candidate` 忘了 `refresh()` → 剩余串有 preedit 但候选窗
    ///   是空的(或还是旧列表)→ `update.candidates` 断言红;
    /// - 少扣一次 caret → 下面的 caret 断言红。
    #[test]
    fn committing_a_prefix_candidate_keeps_the_rest_of_the_composition() {
        let mut s = session_with_segments();
        for c in "womenjintian".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let idx = s
            .candidates
            .iter()
            .position(|c| c.word == "我们")
            .expect("候选里应有「我们」");
        assert_eq!(s.candidates[idx].covered, 5, "「我们」覆盖 wo men");

        let out = s.handle(InputEvent::CandidateClicked(idx)).expect("不应失败");
        // 方案 b:这一段**不交给应用**,只进预编辑区(于是退格能退回来)。
        match out {
            SessionOutput::Updated(update) => {
                assert_eq!(update.preedit, "我们jintian", "确认段 + 剩余串都在组合里");
                // 光标按**字符**偏移:已确认段是中文,不能按字节加。
                // 消耗前缀后光标平移到剩余串末尾(7 个字母),加确认段 2 字。
                assert_eq!(update.caret, 2 + 7, "光标落在剩余串末尾");
                assert!(
                    !update.candidates.is_empty(),
                    "剩余串必须有候选(面板不能变成空的)"
                );
            }
            other => panic!("期望 Updated(确认段留在组合里),实际 {other:?}"),
        }
        assert_eq!(s.pinyin(), "jintian", "剩余串仍在会话里");
        assert!(!s.candidates.is_empty(), "剩余串的候选已刷新");
    }

    /// **退格回退上一次确认**(方案 b 的退格心智模型)。
    ///
    /// 定向破坏:去掉 Backspace 分支里的 `confirmed.pop()`(退回到「只删
    /// 剩余串末位字母」)→ 下面的 pinyin/确认段断言红;`letters` 存成空串 →
    /// 恢复出来的输入串不完整 → 红。
    #[test]
    fn backspace_at_the_segment_boundary_undoes_the_last_confirmation() {
        let mut s = session_with_segments();
        for c in "womenjintian".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let idx = s
            .candidates
            .iter()
            .position(|c| c.word == "我们")
            .expect("候选里应有「我们」");
        s.handle(InputEvent::CandidateClicked(idx)).expect("不应失败");
        assert_eq!(s.pinyin(), "jintian");

        // 光标正停在「已确认段 | 剩余串」的分界处(选完一段的默认状态)。
        let out = s.handle(InputEvent::Backspace).expect("不应失败");
        match out {
            SessionOutput::Updated(update) => {
                assert_eq!(update.preedit, "womenjintian", "确认段退回,输入串原样恢复");
                // 光标回到「选之前那一步」:整串末尾(继续打字就在后面接)。
                assert_eq!(update.caret, "womenjintian".len());
            }
            other => panic!("期望 Updated(退回上一步),实际 {other:?}"),
        }
        assert_eq!(s.pinyin(), "womenjintian", "字母一个都不能少");
        assert!(!s.candidates.is_empty(), "重新出候选(按恢复后的整串算)");
        // 再退一次:没有已确认段了,回到文本编辑惯例(删末位字母)。
        let out = s.handle(InputEvent::Backspace).expect("不应失败");
        match out {
            SessionOutput::Updated(update) => {
                assert_eq!(update.preedit, "womenjintia", "无确认段时退格删末位字母");
            }
            other => panic!("期望 Updated(删字符),实际 {other:?}"),
        }
    }

    /// 每一步都确认,最后整串结束时**一次**交给应用(方案 b 的落点)。
    #[test]
    fn confirmed_segments_are_committed_once_at_the_end() {
        let mut s = session_with_segments();
        for c in "womenjintian".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let idx = s
            .candidates
            .iter()
            .position(|c| c.word == "我们")
            .expect("候选里应有「我们」");
        s.handle(InputEvent::CandidateClicked(idx)).expect("不应失败");
        // 剩余串是 jintian,选「今天」(覆盖整串)→ 组合结束。
        let idx = s
            .candidates
            .iter()
            .position(|c| c.word == "今天")
            .expect("剩余串应有「今天」");
        let out = s.handle(InputEvent::CandidateClicked(idx)).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "我们今天"),
            "确认段与最后一段一起上屏: {out:?}"
        );
        assert!(!s.is_active(), "组合结束");
    }

    /// 覆盖整串的候选仍走**旧路径**(上屏 + 复位 + 收起界面):这条保证
    /// 改动对「选一个词就打完了」的常规输入逐字节不变。
    #[test]
    fn committing_a_whole_input_candidate_still_uses_the_old_path() {
        let mut s = session_with_dict();
        for c in "nihao".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let out = s.handle(InputEvent::Space).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "你好"),
            "覆盖整串时应保持旧的 Commit 语义: {out:?}"
        );
        assert!(!s.is_active(), "上屏后会话复位");
    }

    /// 不变量:**用户敲下的字母一个都不能少**。
    ///
    /// 「已消耗的字母 + 剩余输入串」恒等于敲过的字母数——改动前选一段会
    /// 把后面的字母静默丢掉(既不进文档、也不留在串里),这条断言就是钉
    /// 那个缺陷。
    #[test]
    fn segmented_commits_never_drop_typed_letters() {
        let mut s = session_with_segments();
        let typed = "womenjintian";
        for c in typed.chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let mut consumed = 0usize;
        for step in 0..8 {
            if s.pinyin().is_empty() {
                break;
            }
            let c = s.candidates.first().cloned().unwrap_or_else(|| {
                panic!("第 {step} 步:非空输入串必须有候选(回声兜底是最后一道)")
            });
            let covered = if c.covered == 0 {
                s.pinyin().len()
            } else {
                c.covered
            };
            s.handle(InputEvent::Space).expect("不应失败");
            consumed += covered;
            assert_eq!(
                consumed + s.pinyin().len(),
                typed.len(),
                "第 {step} 步:消耗 {consumed} + 剩余 {} 应等于 {}(字母丢失)",
                s.pinyin().len(),
                typed.len()
            );
            assert!(
                typed.ends_with(s.pinyin()),
                "剩余串必须是原串的后缀,实际 {:?}",
                s.pinyin()
            );
        }
    }

    /// 逐段上屏**整词**不是造词场景:只有「全部片段都是单字」才够格造成
    /// 用户词条(见 [`COIN_ALL_SEGMENTS_SINGLE_CHAR`] 的判据)。
    ///
    /// 分段上屏会让「一次组合里连续上屏多个整词」变得常见,这条测试保证
    /// 它不会把整句粘进用户词库——那正是之前修掉的「造词失控」的新入口。
    #[test]
    fn segmented_commits_of_whole_words_do_not_coin_a_phrase() {
        let (engine, mut s) = session_for_coin(&[
            ("我们", "wo men", 300),
            ("我", "wo", 500),
            ("今天", "jin tian", 300),
        ]);
        let user = engine.user_lm().expect("应有用户层");
        assert_eq!(user.entry_len(), 0, "初始无造词条目");

        for c in "womenjintian".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let idx = s
            .candidates
            .iter()
            .position(|c| c.word == "我们")
            .expect("候选里应有「我们」");
        s.handle(InputEvent::CandidateClicked(idx)).expect("不应失败");
        let idx = s
            .candidates
            .iter()
            .position(|c| c.word == "今天")
            .expect("剩余串的候选里应有「今天」");
        s.handle(InputEvent::CandidateClicked(idx)).expect("不应失败");
        s.break_coin_run();
        assert_eq!(
            user.entry_len(),
            0,
            "连续上屏两个整词不该造词(两个片段都不是单字)"
        );
    }

    /// 标点跟在分段上屏之后:标点跟着那一段走,剩余串继续组合。
    #[test]
    fn punctuation_after_a_segment_keeps_the_rest_composing() {
        let mut s = session_with_segments();
        for c in "womenjintian".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let idx = s
            .candidates
            .iter()
            .position(|c| c.word == "我们")
            .expect("候选里应有「我们」");
        s.cursor = idx;
        let out = s.handle(InputEvent::Char(',')).expect("不应失败");
        match out {
            // 方案 b:标点结束这次组合——已确认段 + 当前候选 + 标点一次上屏。
            SessionOutput::Commit(text) => {
                assert_eq!(text, "我们，", "标点跟在已确认段后面一起上屏");
            }
            other => panic!("期望 Commit(标点结束组合),实际 {other:?}"),
        }
        assert!(!s.is_active(), "标点之后组合结束");
    }

    /// `commit_at`(不经 `handle` 的低层出口)也要把剩余串的候选刷成新的,
    /// 否则调用方读到的是「剩余串 + 旧候选」这种自相矛盾的会话。
    #[test]
    fn commit_at_refreshes_candidates_for_the_remaining_input() {
        let mut s = session_with_segments();
        for c in "womenjintian".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let idx = s
            .candidates
            .iter()
            .position(|c| c.word == "我们")
            .expect("候选里应有「我们」");
        let word = s.commit_at(idx).expect("应有候选");
        assert_eq!(word, "我们");
        assert_eq!(s.pinyin(), "jintian");
        let expected = s
            .engine
            .lookup_candidates("jintian", &s.lm_context())
            .expect("查询失败");
        assert_eq!(
            s.candidates.first().map(|c| c.word.clone()),
            expected.first().map(|c| c.word.clone()),
            "剩余串的候选应与引擎对该串的查询一致"
        );
    }

    /// 第 ③ 期「剩余串的出路」:解不完整(这里 `zz` 解释不了)时,候选表里
    /// 有一条**覆盖整串**的「原样上屏」逃生口;选中它就把敲过的字母原样交给
    /// 应用,不必退格删光重打。
    ///
    /// 定向破坏:把逃生口那条 `if !ranked.iter().any(covered >= len)` 去掉 →
    /// `position` 找不到 → 红;把它改成 `spelling = Abbrev`(进简拼层)→ 会排在
    /// 真实候选之前、把「你好」挤下去 → 上面那条 `candidates[0] == 你好` 的
    /// 断言会红。
    #[test]
    fn escape_hatch_commits_the_unexplained_remainder_verbatim() {
        let (engine, mut s) = session_for_coin(&[("你好", "ni hao", 900)]);
        let user = engine.user_lm().expect("应有用户层");
        for c in "nihaozzzzz".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        assert_eq!(
            s.candidates.first().map(|c| c.word.clone()).as_deref(),
            Some("你好"),
            "真实候选仍在最前: {:?}",
            s.candidates.iter().map(|c| &c.word).collect::<Vec<_>>()
        );
        let idx = s
            .candidates
            .iter()
            .position(|c| c.echo && c.word == "nihaozzzzz")
            .unwrap_or_else(|| {
                panic!(
                    "应有原样上屏的逃生口: {:?}",
                    s.candidates
                        .iter()
                        .map(|c| (&c.word, c.echo, c.covered))
                        .collect::<Vec<_>>()
                )
            });
        s.cursor = idx;
        let out = s.handle(InputEvent::Space).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "nihaozzzzz"),
            "逃生口应把剩余字母原样上屏: {out:?}"
        );
        s.break_coin_run();
        assert_eq!(user.entry_len(), 0, "原样上屏不是词,不该造词");
    }

    /// 点击越界/无候选时不做任何事(不 panic、不动状态)。
    #[test]
    fn clicking_an_out_of_range_candidate_is_a_noop() {
        let mut s = session_with_segments();
        for c in "women".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let before = s.pinyin().to_owned();
        let out = s
            .handle(InputEvent::CandidateClicked(999))
            .expect("不应失败");
        assert!(matches!(out, SessionOutput::Noop), "越界点击应无事发生");
        assert_eq!(s.pinyin(), before);
    }

    #[test]
    fn committed_text_feeds_next_candidates() {
        // 端到端：上屏「中」后继续打 guo，上文必须真的进到打分里
        // （bigram(中,国) 命中 → 「国」压过词频更高的「过」）。
        let mut s = session_with_lm();
        for c in ['z', 'h', 'o', 'n', 'g'] {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let out = s.handle(InputEvent::Space).expect("不应失败");
        assert!(matches!(out, SessionOutput::Commit(ref t) if t == "中"));
        assert_eq!(s.last_word.as_deref(), Some("中"), "上屏词应成为词级上文");
        assert_eq!(s.committed_tail, "中");

        for c in ['g', 'u', 'o'] {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let out = s.handle(InputEvent::Space).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "国"),
            "上文「中」应让「国」压过「过」成为首选"
        );
        assert_eq!(s.committed_tail, "中国", "上下文应跨候选累积");
    }

    #[test]
    fn raw_commit_resets_last_word_but_keeps_tail() {
        let mut s = session_with_dict();
        for c in ['n', 'i'] {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        // 选词上屏：last_word = 该词
        let out = s.handle(InputEvent::Space).expect("不应失败");
        assert!(matches!(out, SessionOutput::Commit(ref t) if t == "你"));
        assert_eq!(s.last_word.as_deref(), Some("你"));

        // 回车：原始字母上屏（不是词）→ last_word 置 None，尾部仍累积
        for c in ['h', 'a', 'o'] {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let out = s.handle(InputEvent::Enter).expect("不应失败");
        assert!(matches!(out, SessionOutput::Commit(ref t) if t == "hao"));
        assert_eq!(s.last_word, None, "原始字母上屏后 last_word 应为 None");
        assert_eq!(s.committed_tail, "你hao", "上屏文本应累积进尾部");
    }

    #[test]
    fn committed_tail_is_bounded_on_char_boundary() {
        let mut s = session_with_dict();

        // 20 个汉字（40 字节）超出上限：只留末尾 16 个完整字符
        s.commit_raw("一二三四五六七八九十一二三四五六七八九十".to_string());
        assert_eq!(s.committed_tail, "五六七八九十一二三四五六七八九十");
        assert_eq!(
            s.committed_tail.chars().count(),
            COMMITTED_TAIL_LIMIT,
            "尾部应有界"
        );

        // 多字节字符（emoji 4 字节/个）：截断必须按 char，不能按字节
        // （按字节会切坏 UTF-8 序列或 panic）
        s.commit_raw("😀".repeat(20));
        s.commit_raw("尾".to_string());
        assert_eq!(s.committed_tail.chars().count(), COMMITTED_TAIL_LIMIT);
        assert_eq!(s.committed_tail.chars().last(), Some('尾'), "最新上屏字符应在尾部");
        assert!(
            std::str::from_utf8(s.committed_tail.as_bytes()).is_ok(),
            "截断后必须是合法 UTF-8（char 边界截断）"
        );
    }

    // —— punctuator(中文标点)——

    #[test]
    fn punct_commits_fullwidth_when_idle() {
        // 中文模式、拼音串为空:标点键直接上屏全角标点。
        let mut s = session_with_dict();
        let out = s.handle(InputEvent::Char(',')).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "\u{FF0C}"),
            "空闲时 `,` 应上屏全角逗号: {out:?}"
        );
        assert!(!s.is_active(), "上屏后会话应保持空闲");
        assert_eq!(s.last_word, None, "标点不是词,last_word 应为 None");
        assert_eq!(s.committed_tail, "\u{FF0C}", "标点应计入上屏尾部");
    }

    #[test]
    fn punct_commits_highlighted_candidate_then_fullwidth() {
        // 中文模式、拼音串非空(`nihao`):`,` 先按空格语义上屏高亮候选
        // (首选整词「你好」,与 commit_at_commits_global_index 同一词典),
        // 再追加全角逗号——用户得到「你好,」。
        let mut s = session_with_dict();
        for c in "nihao".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let out = s.handle(InputEvent::Char(',')).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "你好\u{FF0C}"),
            "`nihao,` 应上屏「你好,」: {out:?}"
        );
        assert!(!s.is_active(), "上屏后会话应复位");
        // 「你好」是词(last_word),标点紧随之后不覆盖它
        assert_eq!(s.last_word.as_deref(), Some("你好"));
        assert_eq!(s.committed_tail, "你好\u{FF0C}");
    }

    #[test]
    fn punct_with_no_real_candidate_commits_raw_then_fullwidth() {
        // 拼音串非空但没有真实候选:与 Space 分支同一兜底语义——
        // 上屏原始输入串(空词库下唯一候选是回声候选,词即原始串),
        // 再追加全角标点。
        //
        // 三段式(`selector` 的 `,`/`.` 翻页落地后改写):
        // - 原断言用 `.`:空词库下 `zgr` 仍有 1 个回声候选(候选窗开着),
        //   而 `.` 的翻页条件是 has_menu(候选非空)——按 librime 语义它
        //   此刻该翻页,不再上屏「zgr。」。那条断言与「`.` 有候选就翻页」
        //   的新行为正面冲突,不可能同时成立;
        // - 改为用 `;`(分号,无翻页键语义)走同一条「无真实候选 →
        //   原始串 + 全角标点」路径;`. 的翻页行为由 paging 专项测试覆盖;
        // - 为什么新行为对:librime `key_bindings.yaml` 的 `.` 就是
        //   has_menu 翻页,回声候选也是候选(用户在候选窗里看得见它),
        //   「有候选就翻页」与 librime 逐字一致;要验证的本是「无真实
        //   候选时标点先上屏原始串」,换一个不翻页的标点键,语义不变。
        let engine = Arc::new(ImeEngine::new(None).expect("内存引擎创建失败"));
        let mut s = InputSession::new(engine);
        for c in "zgr".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let out = s.handle(InputEvent::Char(';')).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "zgr\u{FF1B}"),
            "无真实候选时应上屏「原始串 + ;」: {out:?}"
        );
        assert!(!s.is_active(), "上屏后会话应复位");
        assert_eq!(s.last_word, None, "回声/原始串不是词");
    }

    #[test]
    fn punct_table_driven_session_level() {
        // 会话层面把整张标点表跑一遍(映射本身在 punctuator.rs 单测,
        // 这里断言「中文模式空闲时,每个标点键都上屏对应全角文本」)。
        let cases: &[(char, &str)] = &[
            (',', "\u{FF0C}"),
            ('.', "\u{3002}"),
            (';', "\u{FF1B}"),
            (':', "\u{FF1A}"),
            ('?', "\u{FF1F}"),
            ('!', "\u{FF01}"),
            ('\\', "\u{3001}"),
            ('<', "\u{300A}"),
            ('>', "\u{300B}"),
            ('(', "\u{FF08}"),
            (')', "\u{FF09}"),
            ('~', "\u{FF5E}"),
            ('$', "\u{FFE5}"),
            ('_', "\u{2014}\u{2014}"),
            ('^', "\u{2026}\u{2026}"),
            ('/', "\u{3001}"),
            ('"', "\u{201C}"),
            ('[', "\u{300C}"),
            (']', "\u{300D}"),
            ('{', "\u{300E}"),
            ('}', "\u{300F}"),
            ('`', "\u{FF40}"),
        ];
        for (input, expected) in cases {
            let mut s = session_with_dict();
            let out = s.handle(InputEvent::Char(*input)).expect("不应失败");
            assert!(
                matches!(out, SessionOutput::Commit(ref t) if t == expected),
                "标点 {input:?} 应上屏 {expected:?},实际 {out:?}"
            );
        }
    }

    #[test]
    fn punct_passthrough_in_english_mode() {
        // 英文模式:标点键返回 Noop——IBus 层对 Noop 返回 false,daemon
        // 把按键透传给应用,应用收到原始半角字符(键没有被吃掉)。
        let mut s = session_with_dict();
        s.handle(InputEvent::ToggleAscii).expect("切换不应失败");
        assert!(s.english_mode());
        for c in [',', '.', ';', '$', '~'] {
            let out = s.handle(InputEvent::Char(c)).expect("不应失败");
            assert!(
                matches!(out, SessionOutput::Noop),
                "英文模式下 {c:?} 应透传(Noop): {out:?}"
            );
        }
        // 切回中文模式后标点恢复全角上屏
        s.handle(InputEvent::ToggleAscii).expect("切换不应失败");
        let out = s.handle(InputEvent::Char(',')).expect("不应失败");
        assert!(matches!(out, SessionOutput::Commit(ref t) if t == "\u{FF0C}"));
    }

    #[test]
    fn apostrophe_enters_pinyin_as_syllable_separator() {
        // B2 音节分隔符落地(本轮兑现 punctuator.rs 里的预留):
        // `'` 在拼音串非空时**进拼音串**当强制切分点(`xi'an` 消歧),
        // 空闲时(拼音串为空)打 `'` 没有意义,仍透传给应用。
        //
        // 三段式(B2 落地后改写,原测试名 apostrophe_passthrough_reserved_for_b2):
        // - 原断言:「中文模式下 `'` 透传(Noop),拼音串非空时也透传」
        //   ——那是 B2 未做时的过渡行为(预留键,不冲突先透传);
        // - 改为:「空闲时 `'` 透传(Noop);拼音串非空时 `'` 进拼音串
        //   (preedit 含 `'`)」——B2 的核心语义;
        // - 为什么新行为对:librime speller 的 delimiters 就是「`'` 进
        //   输入串、abc_segmentor 按它硬切」;引擎层把 `'` 当归一化掉的
        //   强制断点(任何含 `'` 的子串都不是合法音节),`xi'an` 只有
        //   [xi, an] 一种切法——消歧成立。进拼音串(而不进词典 key、
        //   不进 `Candidate.pinyin`)的边界由引擎层归一化保证。
        let mut s = session_with_dict();
        let out = s.handle(InputEvent::Char('\'')).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Noop),
            "空闲时 ' 应透传(拼音串为空,无分隔对象): {out:?}"
        );
        // 拼音串非空时:`'` 进拼音串,preedit 为 xi'
        s.handle(InputEvent::Char('x')).expect("不应失败");
        s.handle(InputEvent::Char('i')).expect("不应失败");
        let out = s.handle(InputEvent::Char('\'')).expect("不应失败");
        match out {
            SessionOutput::Updated(u) => {
                assert_eq!(u.preedit, "xi'", "' 应进拼音串: {u:?}");
            }
            other => panic!("拼音串非空时 ' 应进拼音串(Updated),实际 {other:?}"),
        }
    }

    #[test]
    fn digits_still_select_candidates_not_punct() {
        // 数字键仍是选词,不被标点逻辑劫持(数字经 `translate_keyval`
        // 翻成 Select(n),会话层行为不应因标点分支的引入而变化)。
        let mut s = session_with_dict();
        for c in "nihao".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let out = s.handle(InputEvent::Select(1)).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "你好"),
            "数字 1 应选第 1 个候选「你好」: {out:?}"
        );
        // 拼音串为空时数字键照旧 Noop(无候选可选)
        let out = s.handle(InputEvent::Select(1)).expect("不应失败");
        assert!(matches!(out, SessionOutput::Noop));
    }

    // —— ascii_composer 的 Shift / CapsLock(InputEvent::Verbatim)——

    #[test]
    fn shift_letter_commits_uppercase_when_idle() {
        // 中文模式、拼音串为空:Shift+G → 上屏 `G`,不进拼音串。
        let mut s = session_with_dict();
        let out = s.handle(InputEvent::Verbatim('G')).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "G"),
            "空闲时 Shift+G 应直接上屏 G: {out:?}"
        );
        assert!(!s.is_active(), "上屏后会话应保持空闲");
        assert_eq!(s.last_word, None, "单字母不是词,last_word 应为 None");
        assert_eq!(s.committed_tail, "G", "应计入上屏尾部(供后续打分的字符窗口)");
    }

    #[test]
    fn shift_letters_spell_gpu_across_commits() {
        // 连打 Shift+G、Shift+P、Shift+U:每次上屏一个字母,
        // committed_tail 逐次累积成 GPU(应用文本框里就是 GPU)。
        let mut s = session_with_dict();
        for (ev, expect) in [
            (InputEvent::Verbatim('G'), "G"),
            (InputEvent::Verbatim('P'), "P"),
            (InputEvent::Verbatim('U'), "U"),
        ] {
            let out = s.handle(ev).expect("不应失败");
            assert!(
                matches!(out, SessionOutput::Commit(ref t) if t == expect),
                "应上屏 {expect}: {out:?}"
            );
        }
        assert_eq!(s.committed_tail, "GPU", "三次上屏应累积成 GPU");
    }

    #[test]
    fn shift_letter_mid_composition_commits_candidate_then_letter() {
        // 中文模式、拼音串非空(`nihao`):Shift+G 先按空格语义上屏
        // 高亮候选(首选整词「你好」),再追加大写字母——得到「你好G」,
        // 与 `nihao,` → 「你好,」的 punctuator 既定做法一致。
        let mut s = session_with_dict();
        for c in "nihao".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let out = s.handle(InputEvent::Verbatim('G')).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "你好G"),
            "`nihao` + Shift+G 应上屏「你好G」: {out:?}"
        );
        assert!(!s.is_active(), "上屏后会话应复位");
        // 「你好」是词(last_word),大写字母紧随其后不覆盖它(与标点一致)
        assert_eq!(s.last_word.as_deref(), Some("你好"));
        assert_eq!(s.committed_tail, "你好G");
    }

    #[test]
    fn shift_letter_mid_composition_no_candidate_commits_raw_then_letter() {
        // 拼音串非空但没有真实候选(空词库,只有回声候选):与 Space
        // 分支同一兜底语义——上屏原始输入串,再追加大写字母。
        let engine = Arc::new(ImeEngine::new(None).expect("内存引擎创建失败"));
        let mut s = InputSession::new(engine);
        for c in "zgr".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let out = s.handle(InputEvent::Verbatim('R')).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "zgrR"),
            "无真实候选时应上屏「原始串 + R」: {out:?}"
        );
        assert!(!s.is_active());
        assert_eq!(s.last_word, None, "回声/原始串不是词");
    }

    #[test]
    fn lowercase_letter_still_enters_pinyin() {
        // 最关键的不许退化项:小写字母仍然进拼音串(别为了修大写
        // 把正常拼音输入弄坏)。`g` 进拼音串、出候选、空格能上屏。
        let mut s = session_with_dict();
        let out = s.handle(InputEvent::Char('g')).expect("不应失败");
        match out {
            SessionOutput::Updated(u) => {
                assert_eq!(u.preedit, "g", "小写 g 应进拼音串");
                assert!(!u.candidates.is_empty(), "应有候选(至少是回声)");
            }
            other => panic!("期望 Updated,实际 {other:?}"),
        }
        assert_eq!(s.pinyin(), "g");
        assert_eq!(s.caret(), 1);
    }

    #[test]
    fn verbatim_does_not_learn() {
        // 大写字母上屏不是词:不进 UserLM 学习(与标点同一取舍,
        // 对应 librime 标点/原始输入不进 memory)。
        let engine = Arc::new(
            crate::ImeEngine::new(None)
                .expect("内存引擎创建失败")
                .with_user_lm(crate::UserLM::in_memory()),
        );
        let mut s = InputSession::new(Arc::clone(&engine));
        let user = engine.user_lm().expect("引擎应有用户层");
        let before = user.unigram_len();
        let out = s.handle(InputEvent::Verbatim('G')).expect("不应失败");
        assert!(matches!(out, SessionOutput::Commit(ref t) if t == "G"));
        assert_eq!(
            user.unigram_len(),
            before,
            "Verbatim 单字母上屏不应产生学习"
        );
    }

    #[test]
    fn unmapped_ascii_symbols_still_passthrough() {
        // `+` `#` `%` `&` `*` `|` 没有全角映射:
        // translate_keyval 的 0x21..=0x7e 整段放行会把它们送进
        // Char(c),会话层必须落到 catch-all 透传(Noop)——键不能被
        // 吃掉,也不能被塞进拼音串。本轮动了 Char 分支的守卫,
        // 这条测试钉住这个行为不被破坏。
        //
        // 三段式(`selector` 的 `-`/`=` 翻页落地后改写):
        // - 原断言的字符表含 `=` 与 `-`:它们「无全角映射 ⇒ catch-all
        //   透传」。本轮给它们加了「候选窗开着时翻页」(librime
        //   `paging_with_minus_equal`,has_menu)——候选窗非空时不再透传,
        //   与「透传」断言正面冲突;
        // - 改为:把 `=`/`-` 从这张表移出,它们的翻页/空闲透传行为由
        //   `minus_equal_page_when_menu_open` 专项钉住;表里只留下
        //   「无映射、无翻页语义」的其余符号,透传断言逐字不变;
        // - 为什么新行为对:librime 默认就绑了 `-`/`=` 翻页
        //   (key_bindings.yaml:27-30),它们不是「无映射透传」键而是
        //   翻页键——该测试钉的本是「无映射符号不被劫持」,把两个
        //   已被赋予翻页语义的键留在表里,断言的就不再是那个不变量。
        //
        // 三段式(recognizer 落地后再改写):
        // - 原断言的字符表还含 `@`:它「无全角映射 ⇒ 任何状态下
        //   透传」。本轮 recognizer 给 `@` 加了「拼音串非空时进串
        //   尝试邮箱识别」(librime recognizer 的 email pattern
        //   `^[A-Za-z][-_.0-9A-Za-z]*@.*$` 要求 `@` 能进输入串)——
        //   串非空时 `@` 不再透传,与「透传」断言正面冲突;
        // - 改为:把 `@` 从这张表移出,它的「空闲透传 / 串中非空时
        //   进串识别」行为由 recognized_state_char_fallthrough_when_not_recognized
        //   与 email_* 用例专项钉住;表里只留下「无映射、无识别语义、
        //   无翻页语义」的其余符号,透传断言逐字不变;
        // - 为什么新行为对:librime 的 recognizer 就排在 speller 之前、
        //   把 `@` 视为输入串的合法成分(email pattern 的成分),它
        //   不是「无映射透传」键而是「识别触发」键——该测试钉的本是
        //   「无映射符号不被劫持」,把一个已被赋予识别语义的键留在
        //   表里,断言的就不再是那个不变量。空闲时(拼音串为空)`@`
        //   照旧透传,这一半行为不变。
        let unmapped = ['+', '#', '%', '&', '*', '|'];
        for c in unmapped {
            assert!(
                fullwidth_punct(c).is_none(),
                "{c:?} 应无全角映射(有映射的是另一条路径)"
            );
            // 拼音串为空:透传
            let mut s = session_with_dict();
            let out = s.handle(InputEvent::Char(c)).expect("不应失败");
            assert!(
                matches!(out, SessionOutput::Noop),
                "空闲时 {c:?} 应透传(Noop): {out:?}"
            );
            assert!(!s.is_active(), "{c:?} 不应进拼音串");
            // 拼音串非空:同样透传,拼音串不受扰动
            let mut s = session_with_dict();
            s.handle(InputEvent::Char('n')).expect("不应失败");
            s.handle(InputEvent::Char('i')).expect("不应失败");
            let out = s.handle(InputEvent::Char(c)).expect("不应失败");
            assert!(
                matches!(out, SessionOutput::Noop),
                "拼音串非空时 {c:?} 仍应透传: {out:?}"
            );
            assert_eq!(s.pinyin(), "ni", "{c:?} 不应改动拼音串");
        }
    }

    // —— 造词(unity_table_encoder 的等价物)——

    /// 造词测试的引擎装配:内存引擎 + 内存用户层,词库塞入
    /// 指定的单字/词(供逐字选择),返回 (engine, session)。
    fn session_for_coin(entries: &[(&str, &str, u64)]) -> (Arc<ImeEngine>, InputSession) {
        let engine = Arc::new(
            crate::ImeEngine::new(None)
                .expect("内存引擎创建失败")
                .with_user_lm(crate::UserLM::in_memory()),
        );
        for (w, py, f) in entries {
            engine.dict.upsert(w, py, *f).expect("插入失败");
        }
        let s = InputSession::new(Arc::clone(&engine));
        (engine, s)
    }

    /// 逐个打出 `input` 并选中「按词精确匹配目标」的候选上屏。
    ///
    /// 候选列表里同拼音可能有多个字,按 word 精确定位再上屏
    /// (模拟用户逐字选择)。目标词必须在候选里,否则 panic。
    fn type_and_commit(s: &mut InputSession, input: &str, target: &str) {
        for ch in input.chars() {
            s.handle(InputEvent::Char(ch)).expect("按键失败");
        }
        // 用引擎直接查候选列表定位目标词的全局下标(与会话内部
        // 一致:同一引擎、同一输入、空上下文),再 commit_at 上屏
        // (等价于用户翻到该候选并选中)。
        let cands = s
            .engine
            .lookup_candidates(input, &crate::LmContext::empty())
            .expect("查询失败");
        let idx = cands
            .iter()
            .position(|c| c.word == target)
            .unwrap_or_else(|| panic!("{input} 的候选里应有 {target}"));
        let committed = s.commit_at(idx).expect("目标下标应有候选");
        assert_eq!(committed, target, "应上屏 {target}");
    }

    #[test]
    fn char_by_char_selection_coins_phrase() {
        // 验收判据 A1:逐字选「张」「三」「丰」上屏后,再打同样拼音,
        // 「张三丰」应作为用户词条出现在候选里。
        let (engine, mut s) = session_for_coin(&[
            ("张", "zhang", 100),
            ("三", "san", 100),
            ("丰", "feng", 100),
        ]);
        let user = engine.user_lm().expect("应有用户层");
        assert_eq!(user.entry_len(), 0, "初始无造词条目");

        type_and_commit(&mut s, "zhang", "张");
        type_and_commit(&mut s, "san", "三");
        type_and_commit(&mut s, "feng", "丰");
        // 三次上屏后缓冲仍在累积(等下个事件才知道段是否结束);
        // 这里显式结算(测试是 crate 内,可直接调 break_coin_run)。
        s.break_coin_run();
        assert_eq!(user.entry_len(), 1, "三次连续选词应造出一个词条");

        // 再打同样的整串拼音:「张三丰」应在候选里。
        let cands = engine
            .lookup_candidates("zhangsanfeng", &crate::LmContext::empty())
            .expect("查询失败");
        assert!(
            cands.iter().any(|c| c.word == "张三丰"),
            "造词后「张三丰」应能被查到: {:?}",
            cands.iter().take(8).map(|c| &c.word).collect::<Vec<_>>()
        );
        // 拼音 key 应为三段拼接。
        let c = cands.iter().find(|c| c.word == "张三丰").expect("应存在");
        assert_eq!(c.pinyin, "zhang san feng");
    }

    #[test]
    fn coining_does_not_cross_punctuation() {
        // 验收判据 A2:选「你」「好」后打逗号,再选「世」「界」,
        // 不该造出「你好世界」——标点打断造词缓冲。
        let (engine, mut s) = session_for_coin(&[
            ("你", "ni", 100),
            ("好", "hao", 100),
            ("世", "shi", 100),
            ("界", "jie", 100),
        ]);
        let user = engine.user_lm().expect("应有用户层");

        type_and_commit(&mut s, "ni", "你");
        type_and_commit(&mut s, "hao", "好");
        // 打逗号(空闲标点上屏):应打断造词缓冲,造出「你好」而非积累。
        let out = s.handle(InputEvent::Char(',')).expect("不应失败");
        assert!(matches!(out, SessionOutput::Commit(_)), "逗号应上屏");
        assert_eq!(user.entry_len(), 1, "逗号应结算「你好」词条");

        type_and_commit(&mut s, "shi", "世");
        type_and_commit(&mut s, "jie", "界");
        // 光标移出/另一个打断来结算「世界」:用回车(原始上屏)打断。
        s.handle(InputEvent::Char('x')).expect("按键失败");
        s.handle(InputEvent::Enter).expect("回车失败");
        assert_eq!(user.entry_len(), 2, "应再结算「世界」词条");

        // 验证:能查到「你好」与「世界」,查不到「你好世界」。
        let snap = user.snapshot();
        assert_eq!(snap.lookup_entries("ni hao"), &["你好".to_string()]);
        assert_eq!(snap.lookup_entries("shi jie"), &["世界".to_string()]);
        assert!(
            snap.lookup_entries("ni hao shi jie").is_empty(),
            "不应造出跨标点的「你好世界」"
        );
    }

    #[test]
    fn coining_does_not_cross_focus_change() {
        // 焦点切换（IBus FocusOut/Reset/Disable）是造词的真边界：
        // 在一个应用里逐字选「你」「好」，切到另一个应用逐字选
        // 「世」「界」，不该粘成「你好世界」。
        //
        // 注意 reset() 本身**不打断**造词缓冲（上屏路径每次都调它，
        // 打断了就永远攒不起来）——焦点切换必须走
        // reset_for_focus_change()，这个测试就是钉住这个区别的。
        let (engine, mut s) = session_for_coin(&[
            ("你", "ni", 100),
            ("好", "hao", 100),
            ("世", "shi", 100),
            ("界", "jie", 100),
        ]);
        let user = engine.user_lm().expect("应有用户层");

        type_and_commit(&mut s, "ni", "你");
        type_and_commit(&mut s, "hao", "好");
        // 焦点离开：结算「你好」并打断，后续片段另起一段。
        s.reset_for_focus_change();
        assert_eq!(user.entry_len(), 1, "焦点切换应结算「你好」");

        type_and_commit(&mut s, "shi", "世");
        type_and_commit(&mut s, "jie", "界");
        s.reset_for_focus_change();
        assert_eq!(user.entry_len(), 2, "第二段应单独结算「世界」");

        let snap = user.snapshot();
        assert_eq!(snap.lookup_entries("ni hao"), &["你好".to_string()]);
        assert_eq!(snap.lookup_entries("shi jie"), &["世界".to_string()]);
        assert!(
            snap.lookup_entries("ni hao shi jie").is_empty(),
            "不应造出跨焦点的「你好世界」"
        );
    }

    #[test]
    fn echo_and_verbatim_do_not_coin() {
        // 验收判据 A3:回声候选、Verbatim 大写字母不进造词。
        let (engine, mut s) = session_for_coin(&[("你", "ni", 100)]);
        let user = engine.user_lm().expect("应有用户层");

        // 回声候选上屏(空词库解不出的串):不进缓冲。
        for ch in "zgr".chars() {
            s.handle(InputEvent::Char(ch)).expect("按键失败");
        }
        s.handle(InputEvent::Space).expect("空格失败"); // 上屏回声 zgr
        // Verbatim 大写:不进缓冲。
        s.handle(InputEvent::Verbatim('G')).expect("不应失败");
        // 回车原始上屏:不进缓冲。
        s.handle(InputEvent::Char('a')).expect("按键失败");
        s.handle(InputEvent::Enter).expect("回车失败");
        assert_eq!(user.entry_len(), 0, "回声/Verbatim/回车都不应造词");
    }

    #[test]
    fn single_complete_word_commit_does_not_duplicate() {
        // 验收判据 A4:单次上屏的完整词不重复造词——词库已有的词
        // 上屏不应在用户词典里再造一份。
        let (engine, mut s) = session_for_coin(&[("你好", "ni hao", 100)]);
        let user = engine.user_lm().expect("应有用户层");

        type_and_commit(&mut s, "nihao", "你好");
        assert_eq!(
            user.entry_len(),
            0,
            "单次完整词上屏(缓冲长度 1)不应造词条目"
        );
        // 但调频计数照常(既有 learn 通道不受影响)。
        assert_eq!(user.unigram_len(), 1, "unigram 学习应照常");
    }

    // —— 造词判据:句子不该被粘成用户词条(真实用户数据的回归)——

    /// **回归测试,证据来自真实用户数据。**
    ///
    /// `~/.local/share/xwx/user_bigram.txt` 的造词段里出现过:
    /// ```text
    /// 当前主线以及已经完了是吧进度  dang qian zhu xian yi ji yi jing wan le shi ba jin du
    /// 设置为说着玩的说说中文        she zhi wei shuo zhe wan de shuo shuo zhong wen
    /// ```
    /// 这是「把候选当空格用」的连续打字被粘成词条——用户毫无感知,
    /// 词条却永久占候选位。根因是 `break_coin_run` 只判「片段数 ≥ 2」。
    ///
    /// 注意:第一版判据取的是「**存在**至少一个单字片段」,那版会漏掉
    // 下面第 2 条(它的片段里有单字「为」「说」),已经是错的——
    // 现在取「**全部**片段都是单字」,两条都被拦住。
    #[test]
    fn consecutive_multi_char_words_do_not_coin_a_sentence() {
        let (engine, mut s) = session_for_coin(&[
            ("当前", "dang qian", 100),
            ("主线", "zhu xian", 100),
            ("进度", "jin du", 100),
        ]);
        let user = engine.user_lm().expect("应有用户层");

        // 连续上屏三个「多字词」——都是完整词,不是逐字挑的。
        type_and_commit(&mut s, "dangqian", "当前");
        type_and_commit(&mut s, "zhuxian", "主线");
        type_and_commit(&mut s, "jindu", "进度");
        s.break_coin_run();

        assert_eq!(
            user.entry_len(),
            0,
            "连续上屏多字词不该粘成句子词条(修复前这里会造出「当前主线进度」)"
        );
        assert!(
            user.snapshot().lookup_entries("dang qian zhu xian jin du").is_empty(),
            "不该有这条拼起来的词条"
        );
        // 调频(learn)通道不受影响:它本来就是「用过就加频」的语义。
        assert_eq!(user.unigram_len(), 3, "unigram 调频应照常记录三个词");
    }

    /// **回归测试,证据是真实数据第 2 条**——它专门钉住「存在单字」
    /// 那版错误判据。
    ///
    /// 真实词条是 `设置为说着玩的说说中文`,其片段为
    /// `设置(2) / 为(1) / 说着玩的(4) / 说(1) / 说中文(3)`——
    /// **含单字但含多字词**。取「存在单字 ⇒ 够格」的实现会把它造出来;
    /// 取「全部单字 ⇒ 够格」才拦得住。
    ///
    /// 这条断言的价值正在于它**能证伪第一版实现**:把
    /// `is_reusable_phrase` 的 `.all(...)` 改回 `.any(...)`,它立刻变红。
    #[test]
    fn mixed_single_and_multi_char_segments_do_not_coin() {
        let (engine, mut s) = session_for_coin(&[
            ("设置", "she zhi", 100),
            ("为", "wei", 100),
            ("说着玩的", "shuo zhe wan de", 100),
            ("说", "shuo", 100),
            ("说中文", "shuo zhong wen", 100),
        ]);
        let user = engine.user_lm().expect("应有用户层");

        type_and_commit(&mut s, "shezhi", "设置");
        type_and_commit(&mut s, "wei", "为");
        type_and_commit(&mut s, "shuozhewande", "说着玩的");
        type_and_commit(&mut s, "shuo", "说");
        type_and_commit(&mut s, "shuozhongwen", "说中文");
        s.break_coin_run();

        assert_eq!(
            user.entry_len(),
            0,
            "含整词片段的连续上屏不该造词——即使中间夹着单字。\
             这正是真实数据「设置为说着玩的说说中文」的形状"
        );
        assert!(
            user.snapshot()
                .lookup_entries("she zhi wei shuo zhe wan de shuo shuo zhong wen")
                .is_empty(),
            "不该有这条真实数据形状的词条"
        );
    }

    /// 逐字组词的**验收不能被上面那条误伤**。
    ///
    /// 「张三丰」是 PLAN/HANDOFF 里点名的验收判据(逐字选三个单字,
    /// 再打整串拼音应能召回)。它的片段全是单字,必须仍然造词。
    ///
    /// 这条与上一条构成**双向判据**:只测「句子不造词」会被
    /// 「造词彻底关掉」的实现骗过,只测「张三丰能造」会被原来的
    /// bug 骗过。两条一起才钉住「该造的造、不该造的不造」。
    #[test]
    fn char_by_char_coining_still_satisfies_predicate() {
        let (engine, mut s) = session_for_coin(&[
            ("张", "zhang", 100),
            ("三", "san", 100),
            ("丰", "feng", 100),
        ]);
        let user = engine.user_lm().expect("应有用户层");

        type_and_commit(&mut s, "zhang", "张");
        type_and_commit(&mut s, "san", "三");
        // 缓冲里已有两个单字片段,判据此刻就应为真。
        assert!(
            s.is_reusable_phrase(),
            "两个单字片段应够格造词: {:?}",
            s.coin_buffer
        );
        type_and_commit(&mut s, "feng", "丰");
        s.break_coin_run();

        assert_eq!(user.entry_len(), 1, "逐字组词应造出「张三丰」");
        assert_eq!(
            user.snapshot().lookup_entries("zhang san feng"),
            &["张三丰".to_string()],
            "词条拼音 key 应为三段拼接"
        );
    }

    /// `is_reusable_phrase` 的**逐条判据表**:把「够格/不够格」的
    /// 各种组合钉死,而不是只测两个端到端场景。
    ///
    /// 造词是「无感知写入持久数据」的行为,判据必须能被逐条检查。
    #[test]
    fn reusable_phrase_predicate_table() {
        let (_engine, mut s) = session_for_coin(&[]);

        // 空缓冲 / 单片段:不够格(单片段是一次完整词上屏)。
        assert!(!s.is_reusable_phrase(), "空缓冲不够格");
        s.coin_buffer.push(("你".into(), "ni".into()));
        assert!(!s.is_reusable_phrase(), "单片段不够格");

        // 两个单字:够格。
        s.coin_buffer.push(("好".into(), "hao".into()));
        assert!(s.is_reusable_phrase(), "两个单字够格");

        // 全多字词:不够格(连续打字的形状)。
        s.coin_buffer.clear();
        s.coin_buffer.push(("你好".into(), "ni hao".into()));
        s.coin_buffer.push(("世界".into(), "shi jie".into()));
        assert!(!s.is_reusable_phrase(), "全多字词不够格");

        // 单字 + 多字混合:**不够格**。
        // 这是判据演化过的关键一格:第一版用 `.any(...)`(存在单字即可)
        // 时这里是够格的,于是真实数据的「设置为说着玩的说说中文」
        // (含单字「为」「说」)被误造成词条。改成 `.all(...)` 后不够格。
        s.coin_buffer.push(("张".into(), "zhang".into()));
        assert!(
            !s.is_reusable_phrase(),
            "只要有一个整词片段就不够格——整词意味着在组句/正常打字,不是在造词"
        );

        // 多字 + 单字,顺序无所谓(仍不够格)。
        s.coin_buffer.clear();
        s.coin_buffer.push(("你好".into(), "ni hao".into()));
        s.coin_buffer.push(("啊".into(), "a".into()));
        assert!(!s.is_reusable_phrase(), "顺序不影响判据:仍不够格");

        // 三个单字(张三丰/西双版纳的形状):够格。
        s.coin_buffer.clear();
        for (w, p) in [("西", "xi"), ("双", "shuang"), ("版", "ban"), ("纳", "na")] {
            s.coin_buffer.push((w.into(), p.into()));
        }
        assert!(s.is_reusable_phrase(), "全单字四个片段够格(西双版纳)");
    }

    /// 不够格时缓冲**仍须清空**——否则上一句的尾巴会被拼进下一次
    /// 结算,反而造出跨句的更大垃圾词条。
    #[test]
    fn non_reusable_run_still_clears_buffer() {
        let (engine, mut s) = session_for_coin(&[("你好", "ni hao", 100), ("世界", "shi jie", 100)]);
        let user = engine.user_lm().expect("应有用户层");

        type_and_commit(&mut s, "nihao", "你好");
        type_and_commit(&mut s, "shijie", "世界");
        assert!(!s.coin_buffer.is_empty(), "缓冲应已累积");
        s.break_coin_run();
        assert!(s.coin_buffer.is_empty(), "结算后缓冲必须清空");
        assert_eq!(user.entry_len(), 0, "不够格不造词");
    }

    #[test]
    fn coined_entry_persists_across_flush_and_reopen() {
        // 验收判据 A5:造词条目落盘后能读回。
        let dir = std::env::temp_dir().join(format!("xwx_coin_test_{}", std::process::id()));
        let path = dir.join("user_bigram.txt");
        let _ = std::fs::remove_dir_all(&dir);
        {
            let lm = crate::UserLM::open_or_create(&path);
            lm.learn(None, "张");
            lm.learn(Some("张"), "三");
            lm.learn(Some("三"), "丰");
            lm.add_entry("张三丰", "zhang san feng");
            lm.flush().expect("flush 不应失败");
            assert!(path.is_file());
        }
        {
            let lm = crate::UserLM::open_or_create(&path);
            assert_eq!(lm.entry_len(), 1, "重开后造词条目应读回");
            let snap = lm.snapshot();
            assert_eq!(
                snap.lookup_entries("zhang san feng"),
                &["张三丰".to_string()],
                "重开后按拼音 key 应能查到造词"
            );
            // 计数区也应读回(同一份文件两段)。
            assert_eq!(lm.unigram_len(), 3, "重开后 unigram 应读回");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn coined_entry_surfaces_via_lattice_like_dict_word() {
        // 造词条目与词库词条同层同式:它应参与整串解码(覆盖整串),
        // 不是候选列表末尾的孤岛。词库里没有「张三丰」整词,只有单字;
        // 造词后整串查询应把「张三丰」排到比「逐字路径」更前的位置
        // (整词候选分 = 自身 + 0 补全,优于逐字拆解的多次打分)。
        let (engine, mut s) = session_for_coin(&[
            ("张", "zhang", 100),
            ("三", "san", 90),
            ("丰", "feng", 80),
        ]);
        type_and_commit(&mut s, "zhang", "张");
        type_and_commit(&mut s, "san", "三");
        type_and_commit(&mut s, "feng", "丰");
        s.break_coin_run(); // 结算缓冲(测试 crate 内直接调)

        let cands = engine
            .lookup_candidates("zhangsanfeng", &crate::LmContext::empty())
            .expect("查询失败");
        let pos = cands
            .iter()
            .position(|c| c.word == "张三丰")
            .expect("造词后应能查到");
        assert_eq!(pos, 0, "整词造词候选应排第一(覆盖整串): {:?}",
            cands.iter().take(5).map(|c| &c.word).collect::<Vec<_>>());
        // 它是 Exact 层(精确匹配),不是回声/简拼。
        assert_eq!(cands[pos].spelling, crate::SpellingKind::Exact);
        assert!(!cands[pos].echo);
    }

    #[test]
    fn coin_buffer_cap_drops_overlong_run() {
        // 长度上限:超过 MAX_COIN_CHARS 的连续上屏段被结算后清空,
        // 不无限粘成一个巨词。逐字上屏 MAX_COIN_CHARS+2 个单字,
        // 缓冲在超限处结算一次(长度足够则造词),随后从空重新累积。
        let entries: Vec<(String, String, u64)> = (0..30u32)
            .map(|i| {
                (
                    char::from_u32(0x4E00 + i).unwrap().to_string(),
                    "a".to_string(),
                    100 - i as u64,
                )
            })
            .collect();
        let engine = Arc::new(
            crate::ImeEngine::new(None)
                .expect("内存引擎创建失败")
                .with_user_lm(crate::UserLM::in_memory()),
        );
        let mut entries_ref: Vec<(&str, &str, u64)> = Vec::new();
        for (w, py, f) in &entries {
            entries_ref.push((w.as_str(), py.as_str(), *f));
        }
        let _ = &entries_ref; // 用 engine 直接 upsert(借用生命周期)
        for (w, py, f) in &entries {
            engine.dict.upsert(w, py, *f).expect("插入失败");
        }
        let mut s = InputSession::new(Arc::clone(&engine));
        let user = engine.user_lm().expect("应有用户层");

        // 逐字上屏 MAX_COIN_CHARS + 2 个字;每个字拼音都是 "a",
        // 但逐字上屏时输入是单字母 "a",候选第一个就是当前字
        // (词频降序),逐个选「当前该选的字」太脆弱——改为直接用
        // commit_at(0) 上屏当前首选,只验证缓冲长度上限行为。
        for _ in 0..(MAX_COIN_CHARS + 2) {
            s.handle(InputEvent::Char('a')).expect("按键失败");
            s.commit_at(0).expect("应有候选");
        }
        // 超限结算:缓冲长度 ≤ MAX_COIN_CHARS(结算后重新累积)。
        let buffered: usize = s.coin_buffer.iter().map(|(w, _)| w.chars().count()).sum();
        assert!(
            buffered <= MAX_COIN_CHARS,
            "缓冲不应超过上限: {buffered}"
        );
        // 至少结算过一次超限段(造了词条或缓冲被清)。
        assert!(user.entry_len() >= 1 || buffered <= 2,
            "超限应触发结算: entry_len={} buffered={buffered}", user.entry_len());
    }

    // —— selector 的 `,`/`.` 与 `-`/`=` 翻页(librime key_bindings)——

    /// 造一个「候选超过一页」的会话:同一拼音塞 page_size+3 个同音字,
    /// 保证至少有 2 页可翻。
    fn session_with_many_candidates() -> InputSession {
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        // DEFAULT_PAGE_SIZE=9,塞 12 个同音字 → 2 页(9 + 3)
        for i in 0..12u32 {
            let w = char::from_u32(0x4E00 + i).unwrap().to_string();
            engine.dict.upsert(&w, "ni", 1000 - i as u64).expect("插入失败");
        }
        InputSession::new(Arc::new(engine))
    }

    #[test]
    fn period_pages_down_when_menu_open() {
        // 候选窗有候选、在第 1 页:`.` → 翻到第 2 页(has_menu 语义)。
        let mut s = session_with_many_candidates();
        for c in "ni".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let out = s.handle(InputEvent::Char('.')).expect("不应失败");
        match out {
            SessionOutput::Updated(u) => {
                assert_eq!(u.page, 1, "`.` 应翻到第 2 页(下标 1)");
                assert_eq!(u.preedit, "ni", "拼音串不变");
            }
            other => panic!("`.` 翻页应返回 Updated,实际 {other:?}"),
        }
    }

    #[test]
    fn comma_pages_up_only_after_paging() {
        // `,` 用 paging 语义:仅「已翻过页」(page>0)才回翻。
        let mut s = session_with_many_candidates();
        for c in "ni".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        // 第 1 页:`,` 不翻页,出中文逗号「,」(最关键的一条:
        // 中文标点与翻页不冲突的保证)
        let out = s.handle(InputEvent::Char(',')).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t.ends_with('\u{FF0C}')),
            "第 1 页的 `,` 应上屏中文逗号,不是翻页: {out:?}"
        );

        // 重新输入并翻到第 2 页,再按 `,` → 回第 1 页
        let mut s = session_with_many_candidates();
        for c in "ni".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        s.handle(InputEvent::Char('.')).expect("`.` 应翻页"); // → page 1
        let out = s.handle(InputEvent::Char(',')).expect("不应失败");
        match out {
            SessionOutput::Updated(u) => {
                assert_eq!(u.page, 0, "第 2 页的 `,` 应回翻到第 1 页(下标 0)");
            }
            other => panic!("paging 状态下 `,` 应回翻(Updated),实际 {other:?}"),
        }
    }

    #[test]
    fn idle_comma_period_still_fullwidth() {
        // 拼音串为空(无候选窗):`.` → 。,`,` → ,——标点优先,无翻页对象。
        let mut s = session_with_many_candidates();
        let out = s.handle(InputEvent::Char('.')).expect("不应失败");
        assert!(matches!(out, SessionOutput::Commit(ref t) if t == "\u{3002}"), "空闲 `.` 应出 。");
        let out = s.handle(InputEvent::Char(',')).expect("不应失败");
        assert!(matches!(out, SessionOutput::Commit(ref t) if t == "\u{FF0C}"), "空闲 `,` 应出 ,");
    }

    #[test]
    fn minus_equal_page_when_menu_open() {
        // `-`/`=`(librime paging_with_minus_equal,has_menu):候选窗开着时翻页。
        // `-` 同 `,` 的 paging 边界:第 1 页按 `-` 回翻钳在 0(saturating),
        // 与 PageUp 在首页的行为一致(不 wrap、不上屏、不丢候选)。
        let mut s = session_with_many_candidates();
        for c in "ni".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        // `=` 翻到第 2 页
        let out = s.handle(InputEvent::Char('=')).expect("不应失败");
        assert!(matches!(out, SessionOutput::Updated(ref u) if u.page == 1), "`=` 应翻下一页");
        // `-` 回第 1 页
        let out = s.handle(InputEvent::Char('-')).expect("不应失败");
        assert!(matches!(out, SessionOutput::Updated(ref u) if u.page == 0), "`-` 应回上一页");
        // 第 1 页再按 `-`:钳在 0,不出标点、不上屏、不 panic
        let out = s.handle(InputEvent::Char('-')).expect("不应失败");
        assert!(matches!(out, SessionOutput::Updated(ref u) if u.page == 0), "首页 `-` 钳在 0");

        // 空闲(无候选窗):`-`/`=` 落回 catch-all 透传(键不被吃掉),
        // 接管自 unmapped_ascii_symbols_still_passthrough 移出的两条
        let mut s = session_with_many_candidates();
        for c in ['-', '='] {
            let out = s.handle(InputEvent::Char(c)).expect("不应失败");
            assert!(matches!(out, SessionOutput::Noop), "空闲时 {c:?} 应透传(Noop): {out:?}");
        }
    }

    #[test]
    fn period_at_last_page_stays_and_comma_recovers() {
        // 末页再按 `.`:钳在末页(min(max_page)),不 wrap、不上屏;
        // 之后按 `,` 仍能回翻(page>0 满足 paging)。
        let mut s = session_with_many_candidates(); // 12 候选 → 2 页(0,1)
        for c in "ni".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        s.handle(InputEvent::Char('.')).expect("翻页"); // page 1(末页)
        let out = s.handle(InputEvent::Char('.')).expect("末页再按");
        assert!(matches!(out, SessionOutput::Updated(ref u) if u.page == 1), "末页 `.` 钳在末页");
        let out = s.handle(InputEvent::Char(',')).expect("回翻");
        assert!(matches!(out, SessionOutput::Updated(ref u) if u.page == 0), "`,` 应从末页回翻");
    }

    #[test]
    fn nihao_comma_still_commits_fullwidth() {
        // 不许退化:`nihao,` 仍出「你好,」——只有 1 个候选(你好)时
        // candidates 非空、page=0,`,` 不满足 paging(page>0),走标点上屏。
        let mut s = session_with_dict();
        for c in "nihao".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let out = s.handle(InputEvent::Char(',')).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "你好\u{FF0C}"),
            "nihao, 应仍出「你好,」: {out:?}"
        );
    }

    // —— B2 音节分隔符 `'` ——

    #[test]
    fn apostrophe_disambiguates_xian() {
        // `xi'an` → 「西安」:强制切分点让 [xi, an] 成为唯一双音节切法,
        // 西安(xi an)应出现在候选里,且其 `pinyin` 字段不含 `'`。
        //
        // 三段式(原断言选错了观测点,本轮改写):
        // - 原断言:「西安**出现在**候选里」。但西安在不带 `'` 的 `xian`
        //   下本来就会出现(实机第 18 名),所以这条断言在「`'` 根本没
        //   生效」的 bug 存在时也是绿的——它没测到消歧;
        // - 改为**消歧语义**:消歧的定义是「另一种切法的候选**消失**」。
        //   `xi'an` 的候选里**不得出现**只能由 `[xian]` 这条单音节切法
        //   产生的词(如「先」,pinyin 恰为 `xian`),同时「西安」要在;
        // - 为什么新行为对:`'` 是用户敲的强制切分点(librime 的
        //   `delimiters`),`xi'an` 把输入钉成 [xi | an] 两段,`[xian]`
        //   这条单音节边**不得**再产生——「先/现/显/线」这些只能来自
        //   `[xian]` 的字因此消失,这才是 `'` 起作用的证据。只断「西安
        //   存在」无法区分「`'` 生效」与「`'` 被静默删掉」两种实现。
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("西安", "xi an", 100).expect("插入失败");
        engine.dict.upsert("先", "xian", 50).expect("插入失败");
        let cands = engine
            .lookup_candidates("xi'an", &crate::LmContext::empty())
            .expect("查询失败");
        assert!(
            cands.iter().any(|c| c.word == "西安"),
            "xi'an 应能消歧出「西安」: {:?}",
            cands.iter().take(6).map(|c| &c.word).collect::<Vec<_>>()
        );
        let c = cands.iter().find(|c| c.word == "西安").expect("应存在");
        assert_eq!(c.pinyin, "xi an", "词典 key 不含 `'`:{:?}", c.pinyin);
        // 消歧的关键:只能由 `[xian]` 单音节切法产生的「先」必须消失。
        // 它是 `[xian]` 复活的标志——`'` 若被静默删掉,「先」会照常出现。
        assert!(
            !cands.iter().any(|c| c.word == "先" && c.pinyin == "xian"),
            "xi'an 不应再召回只能由 `[xian]` 切出的「先」(`'` 未生效回归): {:?}",
            cands.iter().take(6).map(|c| &c.word).collect::<Vec<_>>()
        );
    }

    #[test]
    fn apostrophe_never_leaks_into_candidate_pinyin() {
        // `'` 不能进任何 `Candidate.pinyin`(词典 key 是空格分隔)。
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("西安", "xi an", 100).expect("插入失败");
        engine.dict.upsert("先", "xian", 50).expect("插入失败");
        engine.dict.upsert("岸", "an", 80).expect("插入失败");
        let cands = engine
            .lookup_candidates("xi'an", &crate::LmContext::empty())
            .expect("查询失败");
        for c in &cands {
            assert!(!c.pinyin.contains('\''), "Candidate.pinyin 不应含 `'`:{:?}", c.pinyin);
        }
    }

    #[test]
    fn apostrophe_backspace_and_caret_cross_it() {
        // `'` 能被 Backspace 删掉、caret 能跨过它(B1 的可编辑缓冲不许坏)。
        let mut s = session_with_dict();
        for c in "xi'an".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        assert_eq!(s.pinyin(), "xi'an");
        assert_eq!(s.caret(), 5);

        // caret 左移跨过 `'`(从 5 到 2,停在 i 后 ' 前)
        for _ in 0..3 {
            s.handle(InputEvent::Left).expect("不应失败");
        }
        assert_eq!(s.caret(), 2, "caret 应跨过 `'` 与字母");
        // Backspace 删 caret 前的 i → x'an
        s.handle(InputEvent::Backspace).expect("不应失败");
        assert_eq!(s.pinyin(), "x'an");

        // caret 移到 `'` 后(位置 2),Backspace 删 `'` → xan
        let mut s = session_with_dict();
        for c in "x'a".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        // caret 在 3(串尾),左移 1 到 2(' 后),Backspace 删 '
        s.handle(InputEvent::Left).expect("不应失败");
        assert_eq!(s.caret(), 2);
        s.handle(InputEvent::Backspace).expect("不应失败");
        assert_eq!(s.pinyin(), "xa", "Backspace 应能删掉 `'`");
    }

    #[test]
    fn apostrophe_edge_cases_do_not_panic() {
        // 连续 `''`、串首 `'`、串尾 `'`、全 `'` 都不 panic、都返回某个
        // 合法的 SessionOutput(不断言具体候选,只钉住「不崩」)。
        let mut s = session_with_dict();
        for input in ["''", "an'", "'a", "a''n", "'''"] {
            // 每个用全新会话,避免前一个的 preedit 干扰
            let mut s = std::mem::replace(&mut s, session_with_dict());
            for c in input.chars() {
                // 串首的 `'` 会透传(Noop),后续进串;都不应 panic
                let _ = s.handle(InputEvent::Char(c)).expect("不应失败");
            }
        }
    }

    // —— editor 的 Ctrl+Delete 删候选 ——

    #[test]
    fn ctrl_delete_removes_highlighted_candidate() {
        // 删掉高亮候选后,重查同一拼音,该候选不再出现。
        let engine = Arc::new(
            crate::ImeEngine::new(None)
                .expect("内存引擎创建失败")
                .with_user_lm(crate::UserLM::in_memory()),
        );
        engine.dict.upsert("你", "ni", 200).expect("插入失败");
        engine.dict.upsert("逆", "ni", 50).expect("插入失败");
        let mut s = InputSession::new(Arc::clone(&engine));

        for c in "ni".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        // 高亮在 cursor=0(「你」,词频最高);Ctrl+Delete 删掉它
        let out = s.handle(InputEvent::DeleteCandidate).expect("不应失败");
        match out {
            SessionOutput::Updated(u) => {
                assert!(
                    u.candidates.iter().all(|c| c.word != "你"),
                    "删掉后「你」不应再出现: {:?}",
                    u.candidates.iter().map(|c| &c.word).collect::<Vec<_>>()
                );
                assert!(u.candidates.iter().any(|c| c.word == "逆"), "「逆」应还在");
            }
            other => panic!("DeleteCandidate 应返回 Updated,实际 {other:?}"),
        }
        // 重新查询同一拼音:「你」仍被屏蔽
        let cands = engine
            .lookup_candidates("ni", &crate::LmContext::empty())
            .expect("查询失败");
        assert!(cands.iter().all(|c| c.word != "你"), "重查后「你」仍应被屏蔽");
    }

    #[test]
    fn ctrl_delete_user_coined_entry_also_removed() {
        // 删掉用户造的词 → 用户词条也被删掉(连根拔)。
        let (engine, mut s) = session_for_coin(&[
            ("张", "zhang", 100),
            ("三", "san", 100),
            ("丰", "feng", 100),
        ]);
        let user = engine.user_lm().expect("应有用户层");
        type_and_commit(&mut s, "zhang", "张");
        type_and_commit(&mut s, "san", "三");
        type_and_commit(&mut s, "feng", "丰");
        s.break_coin_run();
        assert_eq!(user.entry_len(), 1, "已造出「张三丰」");

        // 打出整串,定位并删掉「张三丰」候选
        for c in "zhangsanfeng".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let cands = s.engine
            .lookup_candidates("zhangsanfeng", &crate::LmContext::empty())
            .expect("查询失败");
        let idx = cands.iter().position(|c| c.word == "张三丰").expect("应能查到造词");
        // 把高亮移到该候选,再 Ctrl+Delete
        while s.candidates.get(s.cursor).map(|c| c.word.as_str()) != Some("张三丰") {
            s.handle(InputEvent::Down).expect("下移失败");
        }
        let _ = idx;
        s.handle(InputEvent::DeleteCandidate).expect("删除失败");
        assert_eq!(user.entry_len(), 0, "删掉用户造词应连同词条删除");
        assert!(
            user.snapshot().is_blocked("zhang san feng", "张三丰"),
            "且已屏蔽"
        );
    }

    #[test]
    fn ctrl_delete_dict_word_only_blocks_not_touch_dict() {
        // 删掉词库词:屏蔽生效,但词库只读——dict.bin 不被修改。
        // 用内存词库塞一个词当「词库词」,屏蔽后词库里它仍在
        // (lookup 能查到),只是候选收口把它滤掉。
        let engine = Arc::new(
            crate::ImeEngine::new(None)
                .expect("内存引擎创建失败")
                .with_user_lm(crate::UserLM::in_memory()),
        );
        engine.dict.upsert("词库词", "ci ku ci", 100).expect("插入失败");
        let mut s = InputSession::new(Arc::clone(&engine));
        for c in "cikuci".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        // 定位并删掉
        while s.candidates.get(s.cursor).map(|c| c.word.as_str()) != Some("词库词") {
            if s.handle(InputEvent::Down).is_err() { break; }
            if s.cursor > 50 { panic!("找不到词库词候选"); }
        }
        s.handle(InputEvent::DeleteCandidate).expect("删除失败");
        // 词库里它仍在(只读词库没有被改——对内存词库而言 lookup 仍命中)
        let still_in_dict = engine.dict.lookup("ci ku ci").expect("查询失败");
        assert!(
            still_in_dict.iter().any(|c| c.word == "词库词"),
            "词库词只屏蔽,词库本体不动"
        );
        // 但候选列表里它被滤掉
        let cands = engine
            .lookup_candidates("cikuci", &crate::LmContext::empty())
            .expect("查询失败");
        assert!(cands.iter().all(|c| c.word != "词库词"), "候选里应被屏蔽");
    }

    #[test]
    fn ctrl_delete_down_to_empty_does_not_panic() {
        // 删到候选列表空:不 panic,update 照常(cursor/page 收敛到 0)。
        let engine = Arc::new(
            crate::ImeEngine::new(None)
                .expect("内存引擎创建失败")
                .with_user_lm(crate::UserLM::in_memory()),
        );
        engine.dict.upsert("你", "ni", 200).expect("插入失败");
        let mut s = InputSession::new(Arc::clone(&engine));
        for c in "ni".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        // 连续删,直到没有候选(每删一个 refresh 一次,可能冒出回声;
        // 回声也能被 Ctrl+Delete 屏蔽——它的 (key,word)=(ni,ni))
        for _ in 0..8 {
            let has = s.candidates.get(s.cursor).is_some();
            if !has { break; }
            let out = s.handle(InputEvent::DeleteCandidate).expect("删除不应失败");
            assert!(matches!(out, SessionOutput::Updated(_)), "删到空也应 Updated");
        }
        // 再按一次:无候选,吃掉按键、状态不变、不 panic
        let out = s.handle(InputEvent::DeleteCandidate).expect("不应失败");
        assert!(matches!(out, SessionOutput::Updated(_)), "空候选时 DeleteCandidate 吃掉按键不 panic");
    }

    #[test]
    fn ctrl_delete_passthrough_when_idle() {
        // 拼音串为空:Ctrl+Delete 透传给应用(应用的删词快捷键不被吃掉)。
        let mut s = session_with_dict();
        let out = s.handle(InputEvent::DeleteCandidate).expect("不应失败");
        assert!(matches!(out, SessionOutput::Noop), "空闲时 Ctrl+Delete 应透传");
    }

    // —— recognizer(gear 审计最后一项:邮箱/网址识别)——
    //
    // 断言纪律(本轮任务书特别强调):**观测点是「拼音候选消失」**——
    // 只断「回车/空格能原样上屏」没有用,`commit_raw` 在 bug 存在时
    // 本来就能原样上屏;必须断「候选列表里不再有拼音候选、只剩一条
    // 原文回声候选」。

    /// 往会话里逐字符打 `input`,返回最后一次的输出。
    fn type_str(s: &mut InputSession, input: &str) -> SessionOutput {
        let mut last = SessionOutput::Noop;
        for c in input.chars() {
            last = s.handle(InputEvent::Char(c)).expect("按键失败");
        }
        last
    }

    /// 断言识别态的三条不变量:串等于原文、候选恰一条(原文回声)、
    /// 候选窗里没有任何拼音候选(词 ≠ 原文的都不能有)。
    fn assert_recognized(s: &InputSession, raw: &str) {
        assert_eq!(s.pinyin(), raw, "拼音串应为原文 {raw:?}");
        assert!(s.recognized.is_some(), "应处于识别态: {raw:?}");
        assert_eq!(s.candidates.len(), 1, "识别态下候选应只有一条(原文回声): {raw:?}");
        let c = &s.candidates[0];
        assert_eq!(c.word, raw, "唯一候选应为原文本身: {raw:?}");
        assert!(c.echo, "识别态候选应为回声形态(上屏不学习): {raw:?}");
    }

    #[test]
    fn email_suppresses_pinyin_candidates_and_commits_raw() {
        // 验收 A:zhangsan@gmail.com 全程(自 `@` 起)不出拼音候选,
        // 回车/空格原样上屏。
        let mut s = session_with_dict();
        // `@` 之前:正常拼音态(zhangsan 可能有候选,不断言内容,只断不识别)
        type_str(&mut s, "zhangsan");
        assert_eq!(s.recognized, None, "@ 之前不应识别");
        // `@` 落键:整串命中 email,候选窗立刻只剩原文一条
        let out = s.handle(InputEvent::Char('@')).expect("不应失败");
        match out {
            SessionOutput::Updated(u) => {
                assert_eq!(u.preedit, "zhangsan@");
                assert_eq!(u.candidates.len(), 1, "@ 落键后候选应只剩原文: {:?}", u.candidates);
                assert_eq!(u.candidates[0].word, "zhangsan@");
            }
            other => panic!("@ 应返回 Updated,实际 {other:?}"),
        }
        // 继续敲 gmail.com:识别态保持(逐步断言见
        // continued_typing_after_recognition_does_not_oscillate)
        type_str(&mut s, "gmail.com");
        assert_recognized(&s, "zhangsan@gmail.com");
        // 回车原样上屏
        let out = s.handle(InputEvent::Enter).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "zhangsan@gmail.com"),
            "回车应原样上屏整个邮箱: {out:?}"
        );
        assert!(!s.is_active(), "上屏后复位");
        assert_eq!(s.recognized, None, "上屏后识别态清空");
    }

    #[test]
    fn email_space_commits_raw_via_echo_candidate() {
        // 空格路径:识别态下唯一候选是原文回声,空格选中它上屏 = 原文。
        let mut s = session_with_dict();
        type_str(&mut s, "abc@x.com");
        assert_recognized(&s, "abc@x.com");
        let out = s.handle(InputEvent::Space).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "abc@x.com"),
            "空格应上屏原文(经回声候选): {out:?}"
        );
    }

    #[test]
    fn abc_at_sign_replaces_pinyin_candidates() {
        // 「回溯」的显式断言:`abc` 三个字母已进拼音串、候选窗开着
        // (哪怕只有回声候选);`@` 出现后,候选列表里**不得再有**
        // 任何词 ≠ 原文的候选——在真实词库下「安伯尘/安保处」这类
        // 拼音候选必须消失。内存词库里 abc 切不出合法音节、只有回声
        // 候选,所以这里断的是更弱但同构的一点:候选恰一条且词随 `@`
        // 从「abc」变「abc@」——识别逻辑短路掉时,`@` 透传、候选
        // 仍是「abc」,本条即红(验证见报告第 4 节)。
        let mut s = session_with_dict();
        type_str(&mut s, "abc");
        assert_eq!(s.recognized, None);
        let before: Vec<&str> = s.candidates.iter().map(|c| c.word.as_str()).collect();
        assert_eq!(before, ["abc"], "abc 切不出音节,候选应只有回声");
        s.handle(InputEvent::Char('@')).expect("不应失败");
        assert_recognized(&s, "abc@");
    }

    #[test]
    fn url_www_dot_not_eaten_by_paging_or_punct() {
        // 验收 A 的关键边界:`www.example.com` 里第一个 `.` 是网址的
        // 一部分,不能被翻页/标点吃掉。
        //
        // 三段式(原断言基于「`www` + `.` 落键即识别」的设计,
        // 该设计在 code review 中被自己推翻,本轮改写):
        // - 原断言:「`www` 后按 `.`,点立刻进串、preedit 为 `www.`、
        //   识别态成立」。这要求「前缀 + 空后缀」就判 url 命中;
        // - 改为:**`.` 落键时不识别、进识别态的动作推迟到 `.` 后的
        //   第一个字母**(`www.e`)。`.`,在中文模式下首先是句号/翻页
        //   键,「`www` 后用户可能只想敲个句号」的意图不该被网址
        //   识别抢走;`www.` 这个点随后由识别态的「字符直通」补进
        //   串里(识别态下任何可打印字符直接进串,`.` 不会被标点
        //   分支拦),最终上屏 `www.example.com` 一个字符不缺;
        // - 为什么新行为对:与 librime 的差异只存在于「`www.` 单独
        //   出现时」这一个落键瞬间(librime 前缀 + `.*$` 立吞,
        //   我们要求后缀非空);此后行为逐键一致。而「`.` 落键不被
        //   吃掉」这一条验收判据的本意——**最终能打出一个含点的
        //   网址**——在新行为下完整成立,见本测试末尾的上屏断言。
        //
        // 另:识别态下「`.` 直通进串」这一条,由
        // url_scheme_prefixes_recognized 的 `mailto:a@b.c` 与
        // email 全程用例(`zhangsan@gmail.com` 的第二个点)覆盖——
        // 那里 `.` 若被标点吃掉,串根本长不到目标形状。
        let mut s = session_with_dict();

        type_str(&mut s, "www");
        assert_eq!(s.recognized, None, "www 无后缀时必须是拼音(不退化)");
        assert!(!s.candidates.is_empty(), "候选窗开着(回声候选也在)");

        // `.` 落键:候选窗非空 → 既有 has_menu 翻页语义,串不变。
        // (此刻「用户要敲网址」尚无证据,`.` 保持它一贯的键义。)
        let out = s.handle(InputEvent::Char('.')).expect("不应失败");
        match out {
            SessionOutput::Updated(u) => {
                assert_eq!(u.preedit, "www", "`.` 落键瞬间串不变(翻页语义): {u:?}");
                assert_eq!(s.recognized, None, "此刻仍不识别");
            }
            other => panic!("`.` 翻页应返回 Updated,实际 {other:?}"),
        }

        // 再敲一个字母:此刻「www.」+「e」构成 url,进入识别态——
        // 候选窗里的拼音候选**消失**、只剩一条原文回声(这是
        // recognizer 生效的标志;只断「能原样上屏」在 bug 存在时
        // 也成立,不算数)。
        let out = s.handle(InputEvent::Char('e')).expect("不应失败");
        match out {
            SessionOutput::Updated(u) => {
                assert_eq!(u.preedit, "www.e", "e 落键后串含点: {u:?}");
                assert_eq!(u.candidates.len(), 1, "候选应只剩原文回声: {:?}", u.candidates);
                assert_eq!(u.candidates[0].word, "www.e");
                assert!(u.candidates[0].echo, "应为回声形态");
            }
            other => panic!("识别应返回 Updated,实际 {other:?}"),
        }
        // 识别态下 `.` 直通(不被标点/翻页吃掉),打完整个网址
        type_str(&mut s, "xample.com");
        assert_recognized(&s, "www.example.com");
        let out = s.handle(InputEvent::Enter).expect("不应失败");
        assert!(matches!(out, SessionOutput::Commit(ref t) if t == "www.example.com"),
            "回车应原样上屏完整网址: {out:?}");
    }

    #[test]
    fn url_scheme_prefixes_recognized() {
        // 验收 A 点名的其余前缀,**逐字符交互**(维护者裁决 2 后全部
        // 可从零打出):http(s)://、mailto:、ftp.、file:。
        // `:`/`/`/`.` 在识别态下都必须直通进串(不被标点吃掉——
        // `/` 尤其不能走 fullwidth_punct 映射成顿号)。
        for (input, why) in [
            ("http://x.com", "http: 前缀,:// 直通"),
            ("https://x.com", "https: 前缀"),
            ("mailto:a@b.c", "mailto: 前缀;串内 @ 也直通"),
            ("ftp.x.com", "ftp. 前缀(`.` 触发键补回)"),
            ("file:/x", "file: 前缀"),
        ] {
            let mut s = session_with_dict();
            type_str(&mut s, input);
            assert_recognized(&s, input);
            let out = s.handle(InputEvent::Enter).expect("不应失败");
            assert!(
                matches!(out, SessionOutput::Commit(ref t) if t == input),
                "{why}:回车应原样上屏 {input:?},实际 {out:?}"
            );
        }
    }

    #[test]
    fn colon_enters_string_when_scheme_prefix() {
        // `:` 触发臂的前向判定(维护者裁决 2 的核心):`mailto` 后敲
        // `:` → `:` 进串、**不**上屏「mailto:」、候选窗里拼音候选
        // 消失(只剩一条原文回声)——这是「`:` 不被标点分支吃掉」
        // 的直接断言。初版的坏结果是「mailto 被当拼音上屏成某词 +
        // 全角冒号」,本条在该 bug 存在时必红。
        let mut s = session_with_dict();
        type_str(&mut s, "mailto");
        assert_eq!(s.recognized, None, "mailto 单独打不得识别");
        let out = s.handle(InputEvent::Char(':')).expect("不应失败");
        match out {
            SessionOutput::Updated(u) => {
                assert_eq!(u.preedit, "mailto:", "`:` 应进串: {u:?}");
                assert!(
                    u.candidates.iter().all(|c| c.echo),
                    "识别态下候选应只剩回声(拼音候选消失): {:?}",
                    u.candidates.iter().map(|c| &c.word).collect::<Vec<_>>()
                );
            }
            SessionOutput::Commit(t) => {
                panic!("`:` 不应触发上屏(初版 bug:上屏「拼音词+冒号」),实际 Commit({t:?})")
            }
            other => panic!("`:` 应返回 Updated,实际 {other:?}"),
        }
        // 继续打完,`@` 与 `.` 直通
        type_str(&mut s, "a@b.c");
        assert_recognized(&s, "mailto:a@b.c");
    }

    #[test]
    fn mailto_alone_keeps_pinyin_candidates() {
        // 「mailto 单独打(后面不接 `:`)仍出拼音候选」——`:` 触发臂
        // 不许污染裸前缀。玩具词库里塞一个 mailto 切得出来的词
        // ([mai, l, to] → 「买」确认可切),断言它在、且不进识别态。
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        engine.dict.upsert("买", "mai", 100).expect("插入失败");
        let mut s = InputSession::new(Arc::new(engine));
        type_str(&mut s, "mailto");
        assert_eq!(s.recognized, None, "mailto 不接 `:` 不得识别");
        assert!(
            s.candidates.iter().any(|c| c.word == "买" && !c.echo),
            "mailto 应出真实拼音候选「买」: {:?}",
            s.candidates.iter().take(8).map(|c| &c.word).collect::<Vec<_>>()
        );
    }

    #[test]
    fn bare_domain_stays_pinyin_per_ruling() {
        // 裁决 1 的既定行为钉住:`example.com`(裸域名,无显式前缀)
        // **仍走拼音路径**——`.` 落键时按既有标点/翻页语义消耗,
        // 不进串、不进识别态。这条测试防止将来有人把它当「漏了」
        // 误修。librime 的 `^[a-z]+[.].+$` 子句被有意排除,理由见
        // recognizer.rs 模块文档。
        let mut s = session_with_dict();
        type_str(&mut s, "example");
        assert_eq!(s.recognized, None);
        // `.` 落键:候选窗非空 → 翻页(has_menu),串不变、不识别
        let out = s.handle(InputEvent::Char('.')).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Updated(ref u) if u.preedit == "example"),
            "裸域名的 `.` 应按翻页消耗,串不变: {out:?}"
        );
        assert_eq!(s.recognized, None, "裸域名不得进识别态");
        // 继续敲 com:走正常拼音(字母进串)
        type_str(&mut s, "com");
        assert_eq!(s.pinyin(), "examplecom", "`.` 未进串,字母照拼");
        assert_eq!(s.recognized, None, "裸域名全程不得识别");
    }

    #[test]
    fn recognized_samples_end_to_end_commit_identity() {
        // 端到端(维护者实测发现问题的方法,表驱动):对每个样例
        // 「逐字符 handle + 末尾 Enter,拼接所有 Commit 输出」断言
        // 等于原串。单键断言覆盖不到的「中间某键偷偷上屏了一段」
        // 在这条下无处遁形——`http://` 出「http:、、xcom」就是
        // 这样被抓到的。
        for input in [
            "zhangsan@gmail.com",
            "www.example.com",
            "http://x.com",
            "https://x.com",
            "mailto:a@b.c",
            "ftp.x.com",
            "file:/x",
        ] {
            let mut s = session_with_dict();
            let mut committed = String::new();
            for ch in input.chars() {
                if let SessionOutput::Commit(t) =
                    s.handle(InputEvent::Char(ch)).expect("按键失败")
                {
                    committed.push_str(&t);
                }
            }
            if let SessionOutput::Commit(t) = s.handle(InputEvent::Enter).expect("回车失败") {
                committed.push_str(&t);
            }
            assert_eq!(committed, input, "端到端上屏应等于原串 {input:?}");
        }
    }

    #[test]
    fn continued_typing_after_recognition_does_not_oscillate() {
        // 「识别成立后继续打字不反悔」:`abc@gmail.com` 逐个字符推进,
        // 每一步都保持识别态、候选恒一条。
        let mut s = session_with_dict();
        type_str(&mut s, "abc@");
        let mut expect = String::from("abc@");
        for c in "gmail.com".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
            expect.push(c);
            assert_eq!(s.candidates.len(), 1, "第 {expect:?} 步候选应恒一条");
            assert_eq!(s.candidates[0].word, expect, "第 {expect:?} 步候选应为当前原文");
            assert!(s.recognized.is_some(), "第 {expect:?} 步应保持识别态");
        }
    }

    #[test]
    fn backspace_out_of_recognition_restores_pinyin() {
        // 验收 A:`abc@` 删掉 `@` → 回到正常拼音态,候选恢复。
        let mut s = session_with_dict();
        type_str(&mut s, "abc@");
        assert_recognized(&s, "abc@");
        let out = s.handle(InputEvent::Backspace).expect("不应失败");
        match out {
            SessionOutput::Updated(u) => {
                assert_eq!(u.preedit, "abc", "删掉 @ 后应回到 abc");
            }
            other => panic!("Backspace 应返回 Updated,实际 {other:?}"),
        }
        assert_eq!(s.recognized, None, "删掉 @ 后应退出识别态");
        // 候选恢复为拼音路径的结果(内存词库下 abc 只有回声候选;
        // 「恢复」的判据是候选词回到 abc 而不是 abc@)
        assert_eq!(s.candidates.len(), 1);
        assert_eq!(s.candidates[0].word, "abc", "候选应回到拼音态的回声");
        // 再删光:识别态同样清空
        s.handle(InputEvent::Backspace).expect("不应失败");
        s.handle(InputEvent::Backspace).expect("不应失败");
        s.handle(InputEvent::Backspace).expect("不应失败");
        assert!(!s.is_active());
        assert_eq!(s.recognized, None);
    }

    #[test]
    fn backspace_mid_email_stays_recognized() {
        // 识别态下回删但整串仍命中(`zhangsan@gmail.co` 删成
        // `zhangsan@gmail.c`):保持识别态,不回拼音。
        let mut s = session_with_dict();
        type_str(&mut s, "a@b.co");
        assert_recognized(&s, "a@b.co");
        s.handle(InputEvent::Backspace).expect("不应失败");
        assert_recognized(&s, "a@b.c");
    }

    #[test]
    fn escape_in_recognized_state_clears() {
        // 验收 A:Escape 在识别态下清空。
        let mut s = session_with_dict();
        type_str(&mut s, "abc@x");
        assert!(s.recognized.is_some(), "前置:应处于识别态");
        let out = s.handle(InputEvent::Escape).expect("不应失败");
        match out {
            SessionOutput::Updated(u) => assert!(u.preedit.is_empty(), "Escape 应清空"),
            other => panic!("Escape 应返回 Updated,实际 {other:?}"),
        }
        assert_eq!(s.recognized, None, "Escape 后识别态清空");
        assert!(!s.is_active());
    }

    #[test]
    fn ascii_toggle_during_recognition_leaves_no_residue() {
        // 验收 A:识别态中途切英文再切回来,不残留识别态。
        let mut s = session_with_dict();
        type_str(&mut s, "abc@x");
        assert!(s.recognized.is_some());
        // 切英文:未上屏串原样上屏(toggle 的既有语义),识别态随 reset 清空
        let out = s.handle(InputEvent::ToggleAscii).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "abc@x"),
            "切英文应原样上屏未确认串: {out:?}"
        );
        assert_eq!(s.recognized, None, "切英文后识别态清空");
        assert!(s.english_mode());
        // 切回中文:重新打字,正常拼音态
        s.handle(InputEvent::ToggleAscii).expect("不应失败");
        assert!(!s.english_mode());
        type_str(&mut s, "ni");
        assert_eq!(s.recognized, None, "切回中文后不应残留识别态");
    }

    #[test]
    fn recognized_commit_does_not_learn_and_does_not_coin() {
        // 识别态上屏的是原文,不是词:不进 UserLM 学习、不进造词缓冲
        // (借回声候选的既有语义,与 echo_candidate_commit_does_not_learn
        // 同构,但走的是「识别态回声」这条新通道)。
        let engine = Arc::new(
            crate::ImeEngine::new(None)
                .expect("内存引擎创建失败")
                .with_user_lm(crate::UserLM::in_memory()),
        );
        engine.dict.upsert("你", "ni", 100).expect("插入失败");
        let mut s = InputSession::new(Arc::clone(&engine));
        let user = engine.user_lm().expect("应有用户层");
        let before_uni = user.unigram_len();

        type_str(&mut s, "a@b.c");
        assert!(s.recognized.is_some());
        let out = s.handle(InputEvent::Space).expect("不应失败");
        assert!(matches!(out, SessionOutput::Commit(ref t) if t == "a@b.c"));
        assert_eq!(user.unigram_len(), before_uni, "识别态上屏不应学习");
        assert_eq!(s.last_word, None, "原文不是词");
        // 紧接着选一次真实候选词,不应与刚才的原文粘成造词
        type_str(&mut s, "ni");
        let out = s.handle(InputEvent::Space).expect("不应失败");
        assert!(matches!(out, SessionOutput::Commit(ref t) if t == "你"));
        s.break_coin_run();
        assert_eq!(user.entry_len(), 0, "识别态原文不应参与造词");
    }

    #[test]
    fn unrecognized_inputs_keep_pinyin_candidates() {
        // 不退化:「像前缀但没敲触发字符」的串仍走正常拼音。
        // `www`(缺 `.` + 后缀)不得识别;真实词库下「万维网/呜呜呜」
        // 的召回由 probe 对拍基线保证(玩具词库里 w 不是声母、
        // `www` 切不出音节,只有回声候选——判据落在「不进识别态 +
        // 候选走查词典通道」)。
        let mut s = session_with_dict();
        type_str(&mut s, "www");
        assert_eq!(s.recognized, None, "www 单独打不得识别");
        assert!(
            s.candidates.iter().all(|c| c.echo),
            "玩具词库下 www 只应有回声候选(识别态未劫持): {:?}",
            s.candidates.iter().map(|c| &c.word).collect::<Vec<_>>()
        );

        // 同构的一条:「能切出真实拼音候选」的串,识别逻辑不许碰——
        // `nini`(切得出 [ni,ni])在玩具词库里「你」应在;若
        // recognizer 误判这类串,候选会被一条原文回声换掉。
        let mut s2 = session_with_dict();
        type_str(&mut s2, "nini");
        assert_eq!(s2.recognized, None, "nini 不得识别");
        assert!(
            s2.candidates.iter().any(|c| c.word == "你" && !c.echo),
            "nini 应出真实拼音候选「你」: {:?}",
            s2.candidates.iter().take(5).map(|c| &c.word).collect::<Vec<_>>()
        );
    }

    #[test]
    fn caret_editing_in_recognized_state() {
        // 识别态与 B1 可编辑缓冲共存:caret 不在串尾时插入字符、
        // Backspace 删 caret 前字符,重判基于完整新串。
        let mut s = session_with_dict();
        type_str(&mut s, "a@b.com");
        assert_recognized(&s, "a@b.com");
        // caret 移到串首后一位,插入 x → ax@b.com,仍是 email
        for _ in 0..6 {
            s.handle(InputEvent::Left).expect("不应失败");
        }
        assert_eq!(s.caret(), 1);
        s.handle(InputEvent::Char('x')).expect("不应失败");
        assert_recognized(&s, "ax@b.com");
        assert_eq!(s.caret(), 2);
        // caret 处 Backspace 删掉 @ → axb.com 不再命中,退回拼音
        let mut s = session_with_dict();
        type_str(&mut s, "a@b");
        // caret 在 3(串尾),左移 1 到 2(@ 后),Backspace 删 @
        s.handle(InputEvent::Left).expect("不应失败");
        assert_eq!(s.caret(), 2);
        s.handle(InputEvent::Backspace).expect("不应失败");
        assert_eq!(s.pinyin(), "ab");
        assert_eq!(s.recognized, None, "删掉 @ 应退出识别态");
    }

    #[test]
    fn recognized_state_char_fallthrough_when_not_recognized() {
        // 防御:`@` 在「不可能识别」的位置(串首)仍透传,不被吃掉;
        // 数字串 + @ 不识别(首字符非字母,但数字根本进不了串——
        // 这条只测串首 @)。
        let mut s = session_with_dict();
        let out = s.handle(InputEvent::Char('@')).expect("不应失败");
        assert!(matches!(out, SessionOutput::Noop), "空闲时 @ 应透传");
        assert!(!s.is_active());
    }
}

