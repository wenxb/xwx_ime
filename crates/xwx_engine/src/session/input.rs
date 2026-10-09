//! 输入事件的**分发**:`handle` 与 UI 刷新(`refresh`)。

use super::*;

impl InputSession {
    /// 处理一个按键事件。
    ///
    /// 出错时返回 `Err`（词典/模型查询失败等），会话状态不变。
    pub fn handle(&mut self, event: InputEvent) -> Result<SessionOutput, EngineError> {
        // —— M2 预测态优先（预测分册第 9 节）——
        // 预测处理若给出完整输出就直接返回；否则（Closed）说明面板已关，
        // 同一个事件继续走**一次**普通处理——绝不递归重放事件。
        let mut closed_prediction = false;
        if self.predicting() {
            match self.handle_prediction(&event) {
                super::prediction::PredictionStep::Handled(out) => return Ok(out),
                super::prediction::PredictionStep::Closed => {
                    closed_prediction = true;
                    // ToggleAscii 是例外：它本身是输入法命令，预测关闭后
                    // 要返回**空视图 Updated**（继续消费该键），不能把
                    // 「切模式」误解释成把 Ctrl+空格透传给应用。
                    if matches!(event, InputEvent::ToggleAscii) {
                        self.toggle_ascii();
                        return Ok(SessionOutput::Updated(self.update()));
                    }
                }
            }
        }
        // 「英文跟随」只在连续字母里活着：任何别的按键都结束它（空格、标点、
        // 回车、退格、Esc、模式切换……）。字母本身由下面 Verbatim 分支与
        // `Char` 分支接住。
        if !matches!(event, InputEvent::Char(_) | InputEvent::Verbatim(_)) {
            self.english_run = false;
        }
        // 英文上文失效判定必须看**事件种类 + 输出**：Noop 才是「这个键
        // 交给应用了」。组合内的编辑（退格/方向键/翻页）返回 Updated，
        // 不算边界——否则打英文单词时按一次左键就把上文抹了。
        let was_composing = self.composing();
        let is_space = matches!(event, InputEvent::Space);
        let passthrough = matches!(
            event,
            InputEvent::Char(_)
                | InputEvent::Enter
                | InputEvent::Escape
                | InputEvent::Backspace
                | InputEvent::Up
                | InputEvent::Down
                | InputEvent::Left
                | InputEvent::Right
                | InputEvent::PageUp
                | InputEvent::PageDown
                | InputEvent::DeleteCandidate
        ) || (matches!(event, InputEvent::Select(_)) && !was_composing);
        let out = self.handle_composition(event)?;
        if matches!(out, SessionOutput::Noop) {
            if is_space {
                // 空闲时透传的 ASCII 空格：保留上一词，但要记住「已经隔开」，
                // 下一个英文候选不再补空格（会话分册第 5 节）。
                self.english.on_passthrough_space();
            } else if passthrough {
                self.english.clear();
            }
        }
        // 预测面板刚关掉、而普通处理把按键交给了应用：必须把「需要隐藏 UI」
        // 这个事实报出去（否则面板会僵在屏幕上），但不能提交任何字符。
        if closed_prediction && matches!(out, SessionOutput::Noop) {
            return Ok(SessionOutput::RefreshAndForward(self.update()));
        }
        Ok(out)
    }

    /// 原有按键分发（本包装只负责英文边界与后续英文上下文的处理）。
    ///
    /// 拆成两层是为了**不复制第二份完整按键 match**：英文上文失效与预测
    /// 处理挂在薄包装上，空格/回车/退格等语义仍只有这一处实现。
    fn handle_composition(&mut self, event: InputEvent) -> Result<SessionOutput, EngineError> {
        match event {
            // —— 英文模式：字母等按键透传给应用 ——
            // 英文模式下拼音串必为空，方向键/空格/回车等自然落入相应 Noop；
            // 只有字符在这里被挡掉，不进拼音串。
            InputEvent::Char(c) if self.english_mode && c.is_ascii() => {
                Ok(SessionOutput::Noop)
            }

            // —— 英文跟随：大写起头的英文单词，后面的字母直接上屏 ——
            // 见 `english_run` 的文档。与 Verbatim 同一条路：不进候选、
            // 不学习，逐字交给应用（旧实现里 `H` 之后的 `ello` 会回到拼音）。
            InputEvent::Char(c) if self.english_run && c.is_ascii_alphabetic() => {
                Ok(SessionOutput::Commit(self.commit_raw(c.to_string())))
            }

            // —— Tab：只在预测态有意义（上面已经拦掉） ——
            // 组合态/空闲态保持 Noop，不抢应用的焦点切换（预测分册第 8 节）。
            InputEvent::Tab => Ok(SessionOutput::Noop),

            // —— Ctrl+空格：切换中英文模式（引擎自己的逻辑） ——
            InputEvent::ToggleAscii => Ok(self.toggle_ascii()),

            // —— recognizer 识别态(邮箱/网址):字符直通进串 ——
            // 串已被识别为原文(`zhangsan@gmail`、`www.e`…)时,**任何**
            // 可打印字符都直接进串——`.`/`@`/`:`/`/` 不能被标点/翻页
            // 吃掉(`www.` 的点、`http://` 的斜杠都是网址的一部分),
            // 字母继续延长原文。进串后用完整串重判(见 refresh);
            // 识别态下候选窗只有一条「原样上屏」回声候选,拼音候选
            // 消失——这是 recognizer 生效的可观测标志。
            //
            // **数字与大写字母也要在这里接住**:ibus 层
            // 先把它们折成 `Select`/`Verbatim`(见 `translate_keyval`),
            // 若只挡 `Char`,逐键打 `http://x1.com` 时 `1` 会变成「选第 1 个
            // 候选」(就是那条回声)从而**提前提交地址**;`http://x/A` 的 `A`
            // 会当成「大写结束组合」。识别态下它们只是原文里的字符。
            InputEvent::Char(c) if self.recognized.is_some() && c.is_ascii() && !c.is_ascii_control() => {
                self.insert_recognized_char(c)?;
                Ok(SessionOutput::Updated(self.update()))
            }
            InputEvent::Select(n) if self.recognized.is_some() => {
                // `Select(0)` 是键盘 `0`(第十个候选只在正常拼音态有意义),
                // `Select(k)` 是键盘 `k`。
                let c = if n == 0 { '0' } else { (b'0' + n) as char };
                self.insert_recognized_char(c)?;
                Ok(SessionOutput::Updated(self.update()))
            }
            InputEvent::Verbatim(c)
                if self.recognized.is_some() && c.is_ascii() && !c.is_ascii_control() =>
            {
                self.insert_recognized_char(c)?;
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
                    match c {
                        '.' | '=' => {
                            self.goto_next_page();
                            // 记下「真的敲过这个点」(位置 = 当前串长)。
                            // `www.` 的点被翻页吃掉,下一个字母到来时
                            // refresh 只有凭这条记录才允许把它补回串里
                            // ——不靠串形状猜。
                            if c == '.' {
                                self.pending_url_dot = Some(self.pinyin.len());
                            }
                            return Ok(SessionOutput::Updated(self.update()));
                        }
                        ',' | '-' if self.page() > 0 => {
                            self.goto_prev_page();
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
                    // **标点/大写是造词段的边界**:逐字选
                    // 「张」「三」后打标点、再选「丰」,不结算的话三段的单字会
                    // 跨标点粘成「张三丰」——标点两侧不是同一段。
                    // 位置在提交之前:标点这一刻的候选属于**新一段**(它跟着
                    // 标点一起上屏),不该并进上一段再一起结算。
                    self.break_coin_run();
                    let out = match self.candidates.get(self.cursor).cloned() {
                        Some(c) => {
                            // **后缀守恒**:候选只覆盖输入的一部分时,
                            // 没被解释的字母必须**原样跟上**,不能被随后的 `reset()`
                            // 静默丢掉——`womenjintian` 高亮「我们」时,旧实现只上屏
                            // 「我们，」,`jintian` 人间蒸发。这里**不**替用户选余串
                            // 候选:自动选错比原样保留更糟。
                            let rest = self.uncovered_suffix(&c);
                            let mut text = self.commit_selection(&c);
                            if !rest.is_empty() {
                                self.push_tail(&rest);
                                text.push_str(&rest);
                            }
                            SessionOutput::Commit(text)
                        }
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
                    // 大写字母**起头**（身上没有未上屏的组合）＝ 用户明说这是
                    // 英文单词的第一个字母，后面的字母跟着走英文。
                    if c.is_ascii_uppercase() {
                        self.english_run = true;
                    }
                    Ok(SessionOutput::Commit(self.commit_raw(c.to_string())))
                } else {
                    // 标点/英文字符结束这次组合:已确认段 + 当前候选(或原样
                    // 拼音)**一次**上屏,再把该字符拼在末尾。不能走
                    // `commit_candidate`——它在「只覆盖前缀」时会返回 Updated
                    // (段留在组合里),标点就没地方落地了。
                    // 同标点分支:大写结束组合也是造词边界,
                    // 且在提交之前划。
                    self.break_coin_run();
                    let out = match self.candidates.get(self.cursor).cloned() {
                        Some(c) => {
                            // 后缀守恒:与标点分支同一条规则(见那里的注释)。
                            let rest = self.uncovered_suffix(&c);
                            let mut text = self.commit_selection(&c);
                            if !rest.is_empty() {
                                self.push_tail(&rest);
                                text.push_str(&rest);
                            }
                            SessionOutput::Commit(text)
                        }
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
                if c == '=' {
                    self.goto_next_page();
                } else {
                    self.goto_prev_page();
                }
                Ok(SessionOutput::Updated(self.update()))
            }
            // —— 其余可打印字符:透传给应用 ——
            InputEvent::Char(_) => Ok(SessionOutput::Noop),

            // —— 数字选词 ——
            // 数字是**当前页内**的序号:全局下标 = 页首 + 页内偏移
            // (`0` 仍是第 10 项,librime selector 的口径)。旧实现直接
            // 用 `candidates[n-1]`,翻到第 2 页后按 `1` 上屏的是**第 1 页**
            // 的首项——面板显示与上屏文本分叉。
            InputEvent::Select(n) if !self.pinyin.is_empty() => {
                let offset = if n == 0 { 9 } else { (n - 1) as usize };
                let idx = self.page_start() + offset;
                match self.candidates.get(idx).cloned() {
                    Some(c) => self.commit_candidate(&c),
                    None => Ok(SessionOutput::Noop), // 该数字在本页无对应候选
                }
            }
            InputEvent::Select(_) => Ok(SessionOutput::Noop),

            // —— 候选面板点击(页内下标) ——
            // 面板看到的表是**当前页**,所以它回传的是页内序号;映射回全局
            // 与数字选词同一条规则(`page_start + index`,D5)。
            // 与空格/数字同一出口:分段上屏语义、失败回滚都在
            // `commit_candidate` 里,IBus 层不需要另写一遍。
            InputEvent::CandidateClicked(index) if !self.pinyin.is_empty() => {
                let idx = self.page_start() + index;
                match self.candidates.get(idx).cloned() {
                    Some(c) => self.commit_candidate(&c),
                    None => Ok(SessionOutput::Noop), // 本页没有这一条
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
                // Escape 也是**显式结束本次英文上下文**(会话分册第 2 节)。
                self.english.clear();
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
                    self.pending_url_dot = None;
                    let seg = self.confirmed.pop().expect("上面判过非空");
                    self.pinyin.insert_str(0, &seg.letters);
                    // 回到「选之前那一步」的光标位置:串尾(继续打字就在后面接)。
                    self.caret = self.pinyin.len();
                    self.cursor = 0; // 页码是下标的函数,自动回首页
                    self.refresh()?;
                    return Ok(SessionOutput::Updated(self.update()));
                }
                // 删 caret 前一个字符(文本编辑惯例)。caret 在串首时
                // 无前字符可删,吃掉按键、状态不变——越界删除或透传
                // 都会破坏用户预期(透传会让应用删掉自己文本框里的字)。
                if self.caret > 0 {
                    self.pending_url_dot = None;
                    // caret-1 必为 char 边界(串内只有单字节 ASCII)。
                    let removed = self.pinyin.remove(self.caret - 1);
                    self.caret -= removed.len_utf8();
                    self.cursor = 0; // 页码是下标的函数,自动回首页
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
            // 翻页 = 移动**权威全局高亮下标**到目标页首项;页码是它的函数
            // (见 `InputSession::page`),这样面板高亮、空格、数字、点击
            // 四个出口永远指同一条候选。末页/首页不 wrap,
            // 与既有「多按一下无效果」一致。
            InputEvent::PageUp | InputEvent::PageDown if !self.pinyin.is_empty() => {
                if matches!(event, InputEvent::PageUp) {
                    self.goto_prev_page();
                } else {
                    self.goto_next_page();
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
                    // 按**元数据**分流：英文屏蔽的是目标完整 key，
                    // 中文屏蔽的是 (拼音, 词)——两者的撤销槽位是同一个。
                    match c.english_key() {
                        Some(key) => self.engine.delete_english_candidate(key),
                        None => self.engine.delete_candidate(&c.word, &c.pinyin),
                    }
                    self.refresh()?;
                }
                Ok(SessionOutput::Updated(self.update()))
            }
            InputEvent::DeleteCandidate => Ok(SessionOutput::Noop),

            // —— Ctrl+Shift+Delete:撤销最近一次屏蔽 ——
            // 被屏蔽的候选本来就不在列表里,所以这个动作不看高亮、也不要求
            // 拼音串非空:删错了随时能撤回。撤到了就重查一次,让列表多回来一条。
            InputEvent::UnblockCandidate => {
                if self.engine.unblock_last_candidate() {
                    self.refresh()?;
                    Ok(SessionOutput::Updated(self.update()))
                } else {
                    Ok(SessionOutput::Noop)
                }
            }
        }
    }
}
