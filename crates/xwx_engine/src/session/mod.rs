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
    /// 候选面板点击(`CandidateClicked`):`index` 是**页内**下标
    /// (与 `UpdateLookupTable` 里那个页内 `cursor` 同一坐标系)。
    ///
    /// D5:面板只知道「表里的第几条」——我们发的是当前页,
    /// 所以它回传的也是页内序号。会话把它映射回全局
    /// (`page_start + index`,与 `Select` 同一条映射);旧实现直接当全局
    /// 下标用,于是第 2 页点第 1 条会点上屏第 1 页第 1 条。
    ///
    /// 走 `handle` 而不是直接调 [`InputSession::commit_immediately`],是为了让
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
    /// 撤销最近一次屏蔽（Ctrl+Shift+Delete）。
    UnblockCandidate,
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
    ///
    /// 共享而非克隆:候选表带 `String`,翻页/数字选词
    /// 只移动光标时旧实现每次刷新都整表克隆。会话持有唯一那份 `Arc`,
    /// 更新包只传引用计数;会话真要改候选时走 `Arc::make_mut`(无人共享
    /// 就原地改,还有人拿着就写时复制),语义词、零额外拷贝。
    pub candidates: std::sync::Arc<Vec<Candidate>>,
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
    candidates: std::sync::Arc<Vec<Candidate>>,
    /// 高亮候选下标——**唯一权威的选中坐标**（全局下标，相对 `candidates`）。
    ///
    /// 页码、面板高亮、数字选词、点击选词全部由它推导：
    /// 这里一旦成为唯一来源，「显示的是谁」与「上屏的是谁」就不可能分叉。
    cursor: usize,
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
    /// **英文跟随态**：大写字母起头的英文单词（`Shift+H` 上屏 `H`）之后，
    /// 后续字母继续按英文逐字上屏，直到出现非字母事件（空格/标点/回车/
    /// 退格/焦点切换）。这是「受控中英混输」里的**显式控制**：敲大写字母
    /// 就是明说要打英文，不必先按 Ctrl+空格 切模式再切回来。
    english_run: bool,
    /// 「用户真的敲过一个点、但它被翻页语义吃掉了」的位置（= 串长）。
    ///
    /// `www` 后敲 `.` 时,候选窗开着 → `.` 走翻页、不进串;下一个字母
    /// (`www.e`) 只有凭这个记录才允许把点补回串里。旧实现靠**串形状**
    /// 猜(`www`/`ftp` 开头 + 无点 + 补点后命中 url),于是 `wwwx` 也被
    /// 补成 `www.x`——凭空造出用户没输入的字符。
    /// 任何编辑/选词/复位都会清掉它(见 `reset`、Backspace 分支)。
    pending_url_dot: Option<usize>,
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

mod commit;
mod input;
mod view;

#[cfg(test)]
mod tests;
