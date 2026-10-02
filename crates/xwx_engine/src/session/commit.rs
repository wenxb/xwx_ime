//! 上屏与「已确认段」的状态机:提交候选/原样串、造词计数、剩余串出路、
//! 焦点切换复位、中英模式开关。

use super::*;

impl InputSession {
    /// 组合结束:已确认段 + 这次选中的整串候选**一次**交给应用。
    pub(super) fn commit_selection(&mut self, c: &Candidate) -> String {
        let mut text = self.confirmed_text();
        self.replay_confirmed();
        text.push_str(&self.record_commit(c));
        self.reset();
        text
    }

    /// 候选 `c` **没解释到**的那部分原始输入(标点/大写结束组合时要原样跟上)。
    ///
    /// 空串 = 候选覆盖整串。`c.covered` 已由 `fill_covered` 映射回**原文
    /// 坐标**,与 `self.pinyin` 是同一坐标系(含用户敲的 `'`)。
    pub(super) fn uncovered_suffix(&self, c: &Candidate) -> String {
        let n = c.covered.min(self.pinyin.len());
        self.pinyin[n..].to_string()
    }

    /// 组合以「原样拼音」结束(回车 / 切英文 / 标点 / 无候选时的空格):
    /// 已确认段照样上屏,尾部原始字母跟着一起。
    pub(super) fn finish_with_raw(&mut self, raw: String) -> String {
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
    pub(super) fn commit_word(&mut self, c: &Candidate) -> String {
        let word = self.record_commit(c);
        self.consume_covered(c);
        word
    }

    /// 上屏的**记账**部分:造词缓冲、上屏尾部(bigram 上文)、UserLM 学习。
    ///
    /// 与「消耗输入串」拆开是因为分段上屏要先搬走前缀、再刷新剩余串的
    /// 候选,刷新失败必须能原样回滚——那时**不能**留下任何上屏副作用。
    pub(super) fn record_commit(&mut self, c: &Candidate) -> String {
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
    pub(super) fn covered_len(&self, c: &Candidate) -> usize {
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
    pub(super) fn consume_covered(&mut self, c: &Candidate) {
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
    pub(super) fn commit_candidate(&mut self, c: &Candidate) -> Result<SessionOutput, EngineError> {
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
    pub(super) fn commit_raw(&mut self, text: String) -> String {
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
    pub(super) fn push_coin(&mut self, c: &Candidate) {
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
    pub(super) fn break_coin_run(&mut self) {
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
    pub(super) fn is_reusable_phrase(&self) -> bool {
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
    pub(super) fn record_committed(&mut self, text: &str, word: Option<&str>) {
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
    pub(super) fn push_tail(&mut self, text: &str) {
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
        // 「被翻页吃掉的那个点」是**当前串**的属性,串都没了就作废
        // (codex 审查 D8)。
        self.pending_url_dot = None;
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
    pub(super) fn toggle_ascii(&mut self) -> SessionOutput {
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
