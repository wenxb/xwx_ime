//! 只读视图与访问器:拼音串/光标/模式,以及回送给 IBus 适配层的
//! [`SessionUpdate`](super::SessionUpdate)。

use super::*;

impl InputSession {
    /// 新建会话（初始为空闲状态）。
    pub fn new(engine: Arc<ImeEngine>) -> Self {
        Self {
            engine,
            pinyin: String::new(),
            caret: 0,
            candidates: std::sync::Arc::new(Vec::new()),
            cursor: 0,
            page_size: DEFAULT_PAGE_SIZE,
            committed_tail: String::new(),
            last_word: None,
            english_mode: false,
            english_run: false,
            coin_buffer: Vec::new(),
            confirmed: Vec::new(),
            recognized: None,
            pending_url_dot: None,
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

    /// 当前候选页码(0 起)——**由唯一权威的全局高亮下标推导**,不是独立状态。
    ///
    /// D5:旧实现把 `page` 当独立字段(翻页只动 `page`、不动
    /// `cursor`,数字选词又直接取 `candidates[n-1]`),于是「面板显示第 2 页
    /// 首项 / 空格上屏第 1 页首项」这类不一致无法靠单点修补消除。现在
    /// 翻页 = 把 `cursor` 移到目标页首项,页码只剩这一个来源。
    pub fn page(&self) -> usize {
        self.page_of(self.cursor)
    }

    /// `cursor` 落在第几页(0 起)。`page_size` 恒 ≥ 1(见 [`DEFAULT_PAGE_SIZE`])。
    pub(super) fn page_of(&self, cursor: usize) -> usize {
        cursor / self.page_size
    }

    /// 当前页第一个候选的全局下标。
    pub(super) fn page_start(&self) -> usize {
        self.page() * self.page_size
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

    /// 取当前界面快照。
    /// 组合还活着吗——**剩余串与已确认段任一非空**都算。
    ///
    /// 不能只看 `pinyin`:选完一段后剩余串可能被退格删空(已确认段还在),
    /// 此时回车/空格/Esc/退格都必须按「组合仍在」处理,否则用户选过的段
    /// 既上不了屏、也退不回。
    pub(super) fn composing(&self) -> bool {
        !self.pinyin.is_empty() || !self.confirmed.is_empty()
    }

    pub(super) fn update(&self) -> SessionUpdate {
        // 预编辑区 = **已确认的段 + 剩余拼音串**(方案 b:确认段留在组合里,
        // 不交给应用)。光标要在剩余串里的插入点上,所以偏移已确认段的
        // **字符数**(IBus 的 preedit cursor 是字符下标;已确认段是中文,
        // 不能按字节加)。
        let mut preedit = self.confirmed_text();
        preedit.push_str(&self.pinyin);
        let caret = self.confirmed_chars() + self.caret;
        SessionUpdate {
            preedit,
            candidates: std::sync::Arc::clone(&self.candidates),
            cursor: self.cursor,
            caret,
            page: self.page(),
            page_size: self.page_size,
        }
    }

    /// 已确认段拼成的文本(预编辑区显示 + 最终上屏都要用)。
    pub(super) fn confirmed_text(&self) -> String {
        self.confirmed.iter().map(|s| s.candidate.word.as_str()).collect()
    }

    /// 已确认段的**字符**数(预编辑光标偏移用)。
    pub(super) fn confirmed_chars(&self) -> usize {
        self.confirmed
            .iter()
            .map(|s| s.candidate.word.chars().count())
            .sum()
    }

    /// 把已确认段的账补上(造词缓冲 / 上屏尾部 / UserLM),按用户选中的顺序。
    ///
    /// 记账推迟到组合结束是方案 b 的关键:中途确认的段还没交给应用,用户
    /// 随时可以退格回退——若在确认当时就写 UserLM,回退就得做「反学习」。
    pub(super) fn replay_confirmed(&mut self) {
        let held = std::mem::take(&mut self.confirmed);
        for seg in &held {
            self.record_commit(&seg.candidate);
        }
    }
}
