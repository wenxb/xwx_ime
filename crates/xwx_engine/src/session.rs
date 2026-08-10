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
    /// Ctrl+空格：切换中英文模式。
    ToggleAscii,
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
            // —— 中文标点(librime punctuator 的等价物;映射表及成对标点不交替、
            //    `'` 不进表、`,`/`.` 翻页留待与 selector 一起设计等取舍,
            //    见 punctuator.rs 模块文档) ——
            // 拼音串非空时,先按「空格上屏」的同一语义提交当前高亮候选
            // (无候选则原样上屏拼音串,与 Space 分支逐字对齐),紧接着上屏
            // 全角标点——用户打 `nihao,` 得到「你好,」;拼音串为空时直接
            // 上屏全角标点。英文模式不到这里(上面第一个 Char 分支已拦截,
            // 返回 Noop 沿透传链让应用收到原始半角字符)。
            InputEvent::Char(c) if fullwidth_punct(c).is_some() => {
                let punct = fullwidth_punct(c).expect("守卫已检查");
                if self.pinyin.is_empty() {
                    Ok(SessionOutput::Commit(self.commit_raw(punct.to_string())))
                } else {
                    let head = match self.candidates.get(self.cursor).cloned() {
                        Some(c) => self.commit_word(&c),
                        None => self.commit_raw(self.pinyin.clone()),
                    };
                    // 与 Space 分支的唯一差别:commit_* 已把会话复位,
                    // 这里只把标点追加进上屏尾部并拼到上屏文本后面。
                    // 标点用 push_tail 而非 record_committed:既不触发学习
                    // (标点不是词,不进 UserLM——对应 librime 标点不进 memory),
                    // 也不动 last_word(刚上屏的「你好」仍是后续打分的有效
                    // bigram 上文,不被标点抹掉)。
                    self.push_tail(punct);
                    Ok(SessionOutput::Commit(format!("{head}{punct}")))
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
                if self.pinyin.is_empty() {
                    Ok(SessionOutput::Commit(self.commit_raw(c.to_string())))
                } else {
                    let head = match self.candidates.get(self.cursor).cloned() {
                        Some(c) => self.commit_word(&c),
                        None => self.commit_raw(self.pinyin.clone()),
                    };
                    // 单字母不是词:不进 UserLM 学习(与标点同一取舍),
                    // 也不动 last_word(刚上屏的候选仍是有效 bigram 上文)。
                    self.push_tail(c.encode_utf8(&mut [0u8; 4]));
                    Ok(SessionOutput::Commit(format!("{head}{c}")))
                }
            }
            // —— 其余可打印字符(含 `'`——B2 音节分隔符预留,见
            //    punctuator.rs 模块文档):透传给应用 ——
            InputEvent::Char(_) => Ok(SessionOutput::Noop),

            // —— 数字选词 ——
            InputEvent::Select(n) if !self.pinyin.is_empty() => {
                let idx = if n == 0 { 9 } else { (n - 1) as usize };
                match self.candidates.get(idx).cloned() {
                    Some(c) => Ok(SessionOutput::Commit(self.commit_word(&c))),
                    None => Ok(SessionOutput::Noop), // 该数字无对应候选
                }
            }
            InputEvent::Select(_) => Ok(SessionOutput::Noop),

            // —— 空格：选高亮候选；无候选则原样上屏拼音串 ——
            InputEvent::Space if !self.pinyin.is_empty() => {
                match self.candidates.get(self.cursor) {
                    // 选中真实候选词：走 commit_word，词要进 last_word
                    Some(c) => {
                        let c = c.clone();
                        Ok(SessionOutput::Commit(self.commit_word(&c)))
                    }
                    // 没有候选（如只打了一半前缀）：拼音串原样上屏，是
                    // 原始字母，不算词
                    None => Ok(SessionOutput::Commit(self.commit_raw(self.pinyin.clone()))),
                }
            }
            InputEvent::Space => Ok(SessionOutput::Noop),

            // —— 回车：拼音串原样上屏（同主流输入法） ——
            InputEvent::Enter if !self.pinyin.is_empty() => {
                let raw = self.pinyin.clone();
                Ok(SessionOutput::Commit(self.commit_raw(raw)))
            }
            InputEvent::Enter => Ok(SessionOutput::Noop),

            // —— Esc：清空 ——
            InputEvent::Escape if !self.pinyin.is_empty() => {
                self.reset();
                Ok(SessionOutput::Updated(self.update()))
            }
            InputEvent::Escape => Ok(SessionOutput::Noop),

            // —— 退格 ——
            InputEvent::Backspace if !self.pinyin.is_empty() => {
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
        }
    }

    /// 重新计算候选列表，并把高亮/页码收敛到合法范围。
    ///
    /// TODO: 这里目前是"全量重查"；性能优化方向是增量更新
    /// （追加一个字母时只查新增前缀 + 复用旧候选）。
    fn refresh(&mut self) -> Result<(), EngineError> {
        if self.pinyin.is_empty() {
            self.candidates.clear();
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
    fn update(&self) -> SessionUpdate {
        SessionUpdate {
            preedit: self.pinyin.clone(),
            candidates: self.candidates.clone(),
            cursor: self.cursor,
            caret: self.caret,
            page: self.page,
            page_size: self.page_size,
        }
    }

    /// 上屏指定候选词并复位会话。
    ///
    /// 回声候选(`c.echo`,词库解不出的原始输入串)不是词:按
    /// `commit_raw` 的语义处理——不学习、`last_word` 置 `None`
    /// (对应 librime 里 raw 类型不进 memory 的行为)。
    fn commit_word(&mut self, c: &Candidate) -> String {
        let word = c.word.clone();
        let as_word = if c.echo { None } else { Some(word.as_str()) };
        self.record_committed(&word, as_word);
        self.reset();
        word
    }

    /// 上屏任意文本并复位会话。原文计入上屏尾部；
    /// 原始字母/标点不是词，`last_word` 置 `None`。
    fn commit_raw(&mut self, text: String) -> String {
        self.record_committed(&text, None);
        self.reset();
        text
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
    /// 供候选面板点击、IBus `CandidateClicked` 等场景使用；
    /// 下标越界或没有候选时返回 `None`，会话状态不变。
    pub fn commit_at(&mut self, index: usize) -> Option<String> {
        let candidate = self.candidates.get(index)?.clone();
        Some(self.commit_word(&candidate))
    }

    /// 复位到空闲状态。
    ///
    /// 只清「当前输入」（拼音串/候选/高亮/页码），**不清已上屏上下文**
    /// （`committed_tail` / `last_word`）：上文要跨候选保留，这正是它的
    /// 意义。上屏路径（[`Self::commit_word`] / [`Self::commit_raw`]）先
    /// 更新上文再调用本方法；「新句子/焦点切换」的显式清空留给后续步骤
    /// （IBus `focus-out`）接入。
    pub fn reset(&mut self) {
        self.pinyin.clear();
        self.caret = 0;
        self.candidates.clear();
        self.cursor = 0;
        self.page = 0;
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
            let text = self.pinyin.clone();
            self.reset();
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
        let learned = user.score_with(&snap, None, "测试");
        let unknown = user.score_with(&snap, None, "没学过的词");
        assert!(learned > unknown, "候选词上屏应学习: {learned} vs {unknown}");

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
        // 再追加全角句号。
        let engine = Arc::new(ImeEngine::new(None).expect("内存引擎创建失败"));
        let mut s = InputSession::new(engine);
        for c in "zgr".chars() {
            s.handle(InputEvent::Char(c)).expect("不应失败");
        }
        let out = s.handle(InputEvent::Char('.')).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == "zgr\u{3002}"),
            "无真实候选时应上屏「原始串 + 。」: {out:?}"
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
    fn apostrophe_passthrough_reserved_for_b2() {
        // `'` 是 B2 音节分隔符(xi'an 消歧)的预留键,本轮明确不进
        // 标点表,保持既有透传行为——中文模式下也是 Noop。
        let mut s = session_with_dict();
        let out = s.handle(InputEvent::Char('\'')).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Noop),
            "' 应透传(B2 预留): {out:?}"
        );
        // 拼音串非空时同样透传(不进拼音串、不触发标点上屏)
        s.handle(InputEvent::Char('x')).expect("不应失败");
        s.handle(InputEvent::Char('i')).expect("不应失败");
        let out = s.handle(InputEvent::Char('\'')).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Noop),
            "拼音串非空时 ' 仍应透传: {out:?}"
        );
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
        // `=` `+` `-` `#` `%` `&` `*` `@` `|` 没有全角映射:
        // translate_keyval 的 0x21..=0x7e 整段放行会把它们送进
        // Char(c),会话层必须落到 catch-all 透传(Noop)——键不能被
        // 吃掉,也不能被塞进拼音串。本轮动了 Char 分支的守卫,
        // 这条测试钉住这个行为不被破坏。
        let unmapped = ['=', '+', '-', '#', '%', '&', '*', '@', '|'];
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
}
