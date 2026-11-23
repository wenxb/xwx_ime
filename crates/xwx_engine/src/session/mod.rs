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
use std::time::{Duration, Instant};

use crate::punctuator::fullwidth_punct;
use crate::{Candidate, EngineError, ImeEngine, LmContext};
use english::EnglishRunContext;

/// 候选窗口每页显示的候选数（搜狗等主流输入法默认 9 个）。
pub const DEFAULT_PAGE_SIZE: usize = 9;
/// 已上屏文本尾部的最大保留字符数（按 `char` 计，非字节）。
///
/// 上界 16：够覆盖将来万象（Rime octagram `.gram`）的 5 字字符窗口，
/// 也装得下 bigram 的一个词；既不随会话无限膨胀，也不会让候选打分
/// 丢失必要上文。
pub const COMMITTED_TAIL_LIMIT: usize = 16;

/// 含单字片段的分段造词最大字数（如 `张+三+丰`、`刘+慈欣`、`千问+三`）。
pub const MAX_COIN_CHARS: usize = 8;

/// 纯多字词拼接的分段造词最大字数（如 `万象+模型`、`大语言+模型`；更长多为整句，交给 bigram 与整句解码）。
pub const MAX_COIN_MULTI_WORD_CHARS: usize = 5;

/// 造词最少片段数：同一次预编辑内至少分 2 段选出才记为自造词。
pub const MIN_COIN_SEGMENTS: usize = 2;

/// 一次按键的抽象。由上层把 IBus/GDK 键值翻译成这个枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    /// 可打印字符（字母/数字/标点等）。字母会进入拼音串。
    Char(char),
    /// 原样进串的字符(不归一、不做全角标点映射)。
    ///
    /// 与 [`InputEvent::Char`] 的差别:`Char` 是「交给输入法处理的
    /// 原始字符」——字母归一小写进拼音串、标点按模式映射全角;`Verbatim`
    /// 携带用户明敲的大小写,目前只有两个来源(ibus 层 `translate_keyval`):
    ///
    /// - **Shift+字母**(中文模式):大写进串(`G`),
    ///   `GPU`/`API`/`CPU` 这类大写缩写能打出来;
    /// - **CapsLock 开**:字母出大写,进串组合。
    ///
    /// 会话层对它的处理与普通字母**同一条组合路**:进串、出预编辑、
    /// 可退格修改,由空格/回车/选词统一上屏——**不直出、不切英文**
    /// (旧实现逐字直出并点亮「英文跟随」,把大写之后接着打的中文整段
    /// 吞成英文,已移除)。含大写的串查不出覆盖整串的中文读法,候选里
    /// 有「原样上屏」逃生口;回声不进 UserLM 学习、不进造词缓冲。
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
    /// Tab：**只在预测态**接受当前高亮的下一词。
    ///
    /// 组合态与空闲态下 Tab 仍然是 `Noop`（不抢应用的焦点切换）；
    /// Shift+Tab 由 IBus 层直接按「关闭预测 + 使上文失效」处理，
    /// **不折成**这个事件。
    Tab,
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
    /// 确认本段前的 `committed_tail`，用于退格回退时还原 LM 字符级窗口。
    prev_tail: String,
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
    /// 上屏文本，**并且**刷新界面（M2 预测面板）。
    ///
    /// IBus 必须走「先 `CommitText`、再 `refresh_ui`」，**不能**调会
    /// `hide_ui` 的 `commit`——否则刚算好的下一词面板会被自己藏掉。
    CommitAndUpdate {
        /// 需要提交给应用的文本。
        text: String,
        /// 提交后要展示的界面数据。
        update: SessionUpdate,
    },
    /// 只刷新/隐藏界面，**不提交任何字符**（`ProcessKeyEvent` 返回 false）。
    ///
    /// 用于「预测面板需要消失、但按键本身要交给应用」的情形。绝不能
    /// 为了这个去 `CommitText(" ")` 再返回 false——应用会收到两个空格。
    RefreshAndForward(SessionUpdate),
}

/// 把后缀字符(标点)拼到一次上屏的文本末尾。
///
/// 标点落在「输入串非空」的分支里:「先结束当前组合(已确认段 + 当前
/// 候选/原样拼音),再追加这个字符」,以 `Commit` 收尾(方案 b 之后不再
/// 有「上屏一段还留着组合」的变体),这里只负责拼串。Verbatim 大写
/// 原样保留、不再提交候选,自己拼串,不经这里。
fn append_to_commit(out: SessionOutput, suffix: &str) -> SessionOutput {
    match out {
        SessionOutput::Commit(text) => SessionOutput::Commit(format!("{text}{suffix}")),
        // 本函数只被上屏分支调用,`Updated`/`Noop` 与两个 M2 变体都不可达
        // (标点会先关掉预测态);原样返回,不制造新的语义。
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
    /// 最近一次透传数字的时刻，仅供紧随其后的句点使用。
    last_digit_at: Option<Instant>,
    /// 「用户真的敲过一个点、但它被翻页语义吃掉了」的位置（= 串长）。
    ///
    /// `www` 后敲 `.` 时,候选窗开着 → `.` 走翻页、不进串;下一个字母
    /// (`www.e`) 只有凭这个记录才允许把点补回串里。旧实现靠**串形状**
    /// 猜(`www`/`ftp` 开头 + 无点 + 补点后命中 url),于是 `wwwx` 也被
    /// 补成 `www.x`——凭空造出用户没输入的字符。
    /// 任何编辑/选词/复位都会清掉它(见 `reset`、Backspace 分支)。
    pending_url_dot: Option<usize>,
    /// 已确认、但还没交给应用的段(方案 b)。见 [`ConfirmedSegment`]。
    ///
    /// 组合期间预编辑区显示 = 这些段的文本 + 剩余拼音串;整串选完 / 回车 /
    /// 标点 / 切英文 / 焦点切换时才一次性上屏。
    confirmed: Vec<ConfirmedSegment>,
    /// 英文上文(已提交、仍可信的上一个英文词)+ 延迟空格状态。
    ///
    /// 与 `english_mode` **刻意分开**（见
    /// [`english::EnglishRunContext`] 的模块文档）。`reset()` 不清它。
    english: EnglishRunContext,
    /// M2 **下一词预测面板**的候选（最多 [`crate::english::ENGLISH_NEXT_LIMIT`] 条）。
    ///
    /// 与 `candidates` 刻意分开：预测项全是 `NextWord`、`covered` 恒 0，
    /// 既不参与 `composing()`，也不走 `commit_candidate` 的消费语义。
    /// 空 = 普通空闲态（不维持一个空的 Prediction 状态）。
    prediction: std::sync::Arc<Vec<Candidate>>,
    /// 预测面板的高亮下标（相对 `prediction`）。
    prediction_cursor: usize,
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
pub mod english;
mod input_recognized;
mod input;
mod prediction;
mod refresh;
mod view;

#[cfg(test)]
mod tests;
