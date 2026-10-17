//! 候选刷新与识别态字符插入。
//!
//! 从 `input.rs` 拆出：翻页/识别态直通/候选重算都是「刷新」这件事的不同入口。

use super::*;

impl InputSession {
    /// 翻到下一页:把**权威下标**移到下一页首项;已是末页则不动(不 wrap)。
    ///
    /// 「下一页首项 ≥ 候选总数」即没有下一页——末页(可能不足一页)与
    /// 「最后一页再按下一页」都落在这里,下标不会被推出范围。
    pub(super) fn goto_next_page(&mut self) {
        let next = (self.page() + 1) * self.page_size;
        if next < self.candidates.len() {
            self.cursor = next;
        }
    }

    /// 回上一页:首页不动(不 wrap)。
    pub(super) fn goto_prev_page(&mut self) {
        let page = self.page();
        if page > 0 {
            self.cursor = (page - 1) * self.page_size;
        }
    }

    /// 识别态(邮箱/网址)的**原文直通**:把字符插到 caret 处并重判。
    ///
    /// `Char`/`Select`(数字)/`Verbatim`(大写)三条入口共用,保证
    /// 「识别态下任何可打印字符都只是原文的一部分」。
    pub(super) fn insert_recognized_char(&mut self, c: char) -> Result<(), EngineError> {
        self.pinyin.insert(self.caret, c);
        self.caret += c.len_utf8();
        self.refresh()
    }

    /// 字母进串(组合的常规入口):`Char` 归一小写进拼音;`Verbatim` 是
    /// 用户明敲的大写(Shift+字母 / CapsLock),按原样进串——**进组合,
    /// 不直出**。CapsLock+Shift 的抵消在 ibus 层已折成 `Char` 小写
    /// (见 `translate_keyval`)。
    pub(super) fn insert_letter(
        &mut self,
        c: char,
        verbatim: bool,
    ) -> Result<SessionOutput, EngineError> {
        // 在 caret 处插入(不是追加):长串中间打错字母可以左移光标
        // 补插,不用全删重打。caret 处必为 char 边界(串内只有单字节
        // ASCII,caret 由本结构体独占维护)。
        let c = if verbatim { c } else { c.to_ascii_lowercase() };
        self.pinyin.insert(self.caret, c);
        self.caret += c.len_utf8();
        self.refresh()?;
        Ok(SessionOutput::Updated(self.update()))
    }

    /// 重新计算候选列表，并把高亮/页码收敛到合法范围。
    ///
    /// TODO: 这里目前是"全量重查"；性能优化方向是增量更新
    /// （追加一个字母时只查新增前缀 + 复用旧候选）。
    pub(super) fn refresh(&mut self) -> Result<(), EngineError> {
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
        // 补点**必须凭真实敲过的那个点**:`pending_url_dot` 只能是 `.` 键
        // 被翻页吃掉时留下的,且位置要正好落在 `www`/`ftp` 之后。旧实现
        // 只看串形状,于是 `wwwx`(没敲点)也被补成 `www.x`。
        if self.recognized.is_none() && !self.pinyin.contains('.') {
            for bare in ["www", "ftp"] {
                if self.pending_url_dot != Some(bare.len()) {
                    continue;
                }
                if let Some(rest) = self.pinyin.strip_prefix(bare) {
                    if !rest.is_empty() {
                        let restored = format!("{bare}.{rest}");
                        if crate::recognize(&restored).is_some() {
                            self.pinyin = restored;
                            self.pending_url_dot = None; // 只补一次
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
        // 识别态(邮箱/网址)是**原文世界**:地址不是英文上下文,不能拿
        // 它当上一词去召回/打分（会话分册第 2 节）。
        if self.recognized.is_some() {
            self.english.clear();
        }
        if self.pinyin.is_empty() {
            std::sync::Arc::make_mut(&mut self.candidates).clear();
        } else if self.recognized.is_some() {
            self.candidates = std::sync::Arc::new(vec![Self::recognized_echo_candidate(
                &self.pinyin,
            )]);
        } else {
            let ctx = self.lm_context();
            let en_ctx = self.english_context();
            self.candidates = std::sync::Arc::new(
                self.engine.lookup_composition(&self.pinyin, &ctx, &en_ctx)?,
            );
        }
        // 只收敛**那一个**权威下标:页码是它的函数,不再有第二份状态
        // 需要各自钳位。
        self.cursor = self.cursor.min(self.candidates.len().saturating_sub(1));
        Ok(())
    }
}
