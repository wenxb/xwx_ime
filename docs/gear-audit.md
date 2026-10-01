# librime gear 组件逐项对照审计(xwx_engine 现状)

> ⚠ **本文件是 2026-08-09 的历史快照,其中 P0/P1 清单已全部清零。**
> 表里的「现状」列**不代表今天的状态**——读它请配合 `PLAN.md` 的
> 第二、三节(已完成改动)与本文件末尾的「清零记录」。
> **保留原文是为了留住「当初缺什么、为什么缺」的推理**,不是现状清单。
>
> 已于 2026-08-10 清零的项:punctuator、ascii_composer(Shift/CapsLock)、
> memory(接线 + 造词 + 删候选)、fallback_segmentor × echo_translator、
> selector 的 `,`/`.` 翻页、editor 的 Ctrl+Delete、recognizer。
> charset_filter 经实测**销掉**(收益为零)。
>
> **2026-10-01 追加的溯源更正**:本文件第 3 节把
> `unity_table_encoder` 与 `memory` 一起挂在 `table_translator` 内部
> ——这是**对的**,而且很关键。后续实现造词时忽略了这一点,
> 在 `MAX_COIN_CHARS` 的注释里把它当成拼音(script_translator)的
> 对应物,导致溯源错误。**读这张表要连「挂在哪个 translator 下」
> 一起读**:librime 有 table / script / reverse_lookup 等多个 translator,
> 组件清单是按 translator 分组的,只对组件名会搬错特性。
>
> 目的:把 librime `src/rime/gear/` 的 36 个组件(`.h` 清单取自
> `https://api.github.com/repos/rime/librime/contents/src/rime/gear`)
> 逐项对照 xwx 现状,列出「还没发现的问题」。
>
> 对照基准不是「组件名有没有」,而是「它解决的用户可见问题在 xwx 里有没有被解决」。
> 很多 librime 组件在 xwx 的单引擎架构里**不需要同名实现**,其功能已被
> beam 解码/会话状态机吸收,这类标为「有(等价)」。

## 参照系:luna_pinyin 的真实挂载顺序

`/usr/share/rime-data/luna_pinyin.schema.yaml` 的 `engine:` 段(本机实读):

```yaml
engine:
  processors:    [ascii_composer, recognizer, key_binder, speller,
                  punctuator, selector, navigator, express_editor]
  segmentors:    [ascii_segmentor, matcher, abc_segmentor,
                  punct_segmentor, fallback_segmentor]
  translators:   [punct_translator, table_translator@custom_phrase,
                  reverse_lookup_translator, script_translator]
  filters:       [simplifier, uniquifier]
```

顺序本身是语义:
- `fallback_segmentor` 挂在 segmentors **最后**——前面所有切分器都切不动时才补一个
  单字符 raw 段,保证切分永不为空;
- `punct_translator` 挂在 translators **最前**——标点候选先于汉字候选产出;
- `simplifier` 在 `uniquifier` **之前**——先繁转简,再去重(简繁同形词才能合并);
- `express_editor`(editor.h 的拼写模式特化,名为 `express_editor`/`fluid_editor`)
  挂在 processors **最后**——前面组件都不认的键才落到编辑兜底。

本机 `/usr/share/rime-data/default.yaml` 实读确认:它只有 schema_list /
switcher / punctuator / key_binder / recognizer / ascii_composer 几个段,
**没有 `engine:` 段**——默认引擎组件表在 librime 代码里内置
(`gear/switches.h` 等),luna_pinyin 这份就是完整可参照的真实配置。

## 主表(36 行,与 GitHub API 返回的 .h 清单一一对应)

「xwx 现状」列的判断依据均在表后「读过的 xwx 源码清单」中标注了文件与行号。

| 组件 | 类型 | 它解决什么问题 | xwx 现状 | 缺失的用户可见后果 | 优先级 |
|---|---|---|---|---|---|
| punctuator | processor(+ punct_segmentor / punct_translator) | 中文标点:`,`→`,`,`/`→`、`翻页选符号,数字后 `.`→`.` | **无** | 中文模式下打逗号句号出英文半角,中文段落里夹半角标点;`/` 不能翻页选 `、` `÷`;数字后打 `.` 无法智能出小数点 | **P0** |
| ascii_composer | processor | Shift 单击临时切英文/提交、CapsLock 策略、ascii 模式下 Shift+字母出大写 | **部分**(仅 Ctrl+空格切换;`translate_keyval` 把大小写都归一为小写) | 想临时输入大写字母(如 `GPU`)做不到:Shift+G 变成小写 `g` 进拼音串;句中想敲个英文单词必须 Ctrl+空格切走再切回 | **P0** |
| selector | processor | 空格上屏首选;`-`/`=`、`,`/`.` 翻页;自定义选词键 | **部分**(空格=确认高亮项;数字选词;上下/翻页键有;`,`/`.` 翻页和 select_keys 无) | 翻页只能伸手够 PageUp/PageDown 或 `=` 不存在的键位;`,` `.` 这两个最顺手的翻页键不可用 | **P1** |
| editor(express_editor) | processor | 空格确认、回车原样上屏、Esc 清屏、**Backspace 逐字母删编码、Ctrl+Delete 删候选** | **部分**(空格/回车/Esc/单字母 Backspace 都有;`Delete` 键、`Ctrl+Delete` 删候选无) | 无法删掉用户自造错的词/不想再见到的候选,错词每次打字都出现,只能忍 | **P1** |
| navigator | processor | 预编辑串内光标移动(按字/按音节跳,Ctrl+←/→) | **无**(session.rs:257-258 `Left\|Right` 直接 Noop 透传) | 长拼音串中间打错一个字母,只能 Backspace 全删到错误点重打;无法按音节回跳改字 | **P1** |
| key_binder | processor | 可配置键位绑定(toggle 简繁/全半角、模式开关等热键) | **部分**(空格/回车/方向/数字是硬编码等价物;无任何 option 切换热键) | 简繁切换、全半角切换没有键可按(功能本身也不存在,见 simplifier/shape) | **P1** |
| speller | processor | 拼写归一:分隔符 `'`、纠错、简拼(auto_select/顶功类方案用) | **部分**(模糊音纠错=corrector.rs 的 algebra 等价物;**分隔符 `'` 未处理**) | 打 `xi'an` 想消歧「西安」做不到——`'` 被 translate_keyval 丢弃,`xian` 只能在不想要的切分里翻页找 | **P1** |
| recognizer | processor(+ matcher) | 正则识别 email/URL/大写英文整段,直接上屏不切分 | **无** | 中文模式里输邮箱/网址,逐字母被切成拼音,得切英文模式重打一遍 | **P1** |
| echo_translator | translator | 全场无候选时把原始输入本身作为候选(`raw`,quality=-100),让用户始终能回车上屏 | **无**(Space 无候选时有 commit_raw,但候选窗里是空的) | 候选窗显示空,用户不知道按空格/回车能上屏原始字母,视觉上像「输入法卡死/丢了输入」 | **P1** |
| history_translator | translator | 特定输入(如 `u……`)调出最近上屏历史重复输入 | **无** | 刚上屏过的一句话想原样再打一遍,只能从头逐字重打 | **P2** |
| schema_list_translator | translator | F4 方案选单候选 | **无**(xwx 单方案,IBus 面板引擎切换可替代) | 无明显后果(单一拼音方案) | **P2** |
| switch_translator | translator | 方案选单里切换简繁/全半角等 option | **无** | 同 key_binder:option 体系不存在,无可切 | **P2** |
| simplifier | filter | 繁→简转换(opencc),`simplification` 开关 | **无**(词库本身是简体,无繁体源可转) | 需要打繁体的用户(对台港联系人)完全无法输出繁体 | **P2** |
| opencc | (库封装,非组件) | simplifier 的底层转换库 | **不适用** | — | **P2** |
| charset_filter | filter | 过滤 CJK 扩展 B~J 区生僻字 | **无** | 打 `yi` 翻页会看到「𠄎」「𪚥」式生僻字占候选位(rime-ice 词库含扩展区字) | **P2** |
| reverse_lookup_filter | filter | 给候选注五笔/笔画等反查码注释 | **无** | 候选旁看不到形码提示(仅形码用户需要) | **P2** |
| reverse_lookup_translator | translator | `` ` `` 前缀进入笔画反查 | **无** | 不会读的字无法用 `` `hspnz `` 笔画反查出来 | **P2** |
| affix_segmentor | segmentor | 前后缀派生切分(特定方案) | **不适用**(拼音方案不挂载) | — | **P2** |
| ascii_segmentor | segmentor | ascii 模式下整段输入标 raw 段 | **部分**(english_mode 下字母直接透传,不进切分,等价且更简) | 无 | **P2** |
| chord_composer | processor | 并击和弦输入(速录) | **不适用** | — | **P2** |
| contextual_translation | filter | 组句时用 Grammar 重排短语候选权重 | **有(等价)**:beam 解码天然做整句重排(lib.rs `decode_beam`);万象 .gram 即 grammar 数据源 | 无 | **P2** |
| filter_commons | (公共代码,非组件) | 各 filter 的公共基类 | **不适用** | — | **P2** |
| key_binding_processor(+ _impl) | (基类,非组件) | selector/navigator/key_binder 的按键分派框架 | **不适用**(Rust 直接用 match 分派) | — | **P2** |
| memory | (table_translator 内部) | 用户词典:造词、调频、删词、commit 历史记忆 | **部分**(UserLM 学一元/二元调频;**不会造词**;且未接线,见 P0 节) | 上屏 N 次的自造短语永远学不会:每次都只能逐字选;调频只在单测里生效(生产进程根本没挂 UserLM) | **P0** |
| unity_table_encoder | (table_translator 内部) | 上屏历史编码成用户词条(造词落盘) | **无** | 同上:无造词 | **P1** |
| table_translator | translator | 码表固定编码查词 + custom_phrase 自定义短语库 | **无**(xwx 只有音节码查词;custom_phrase 无) | 用户自定义短语(`;mail`→邮箱地址这类)无处可配 | **P2** |
| script_translator | translator | 音节表逐字查词 + 整句造句(朙月拼音主力) | **有(等价)**:build_lattice 查词 + decode_beam 整句 | 无 | — |
| abc_segmentor | segmentor | 按音节表贪心前切,打 `abc` tag | **有(等价)**:SYLLABLES + segmenter.rs;lattice 枚举全部歧义切分,优于贪心 | 无 | — |
| fallback_segmentor | segmentor | 切不动时补单字符 raw 段并逐格后延,保证切分不为空(**不产候选**) | **部分(等价方向不同)**:fallback_candidates(lib.rs:629)保证「切不动也有候选」;无 raw tag 段机制 | 见 P0 节:无候选时**无可见候选可上屏**(echo 缺失的组合症状) | **P0** |
| grammar | (poet 内部) | n-gram 语法模型打分接口 | **有(等价)**:WanxiangLM(字符级窗口,.gram 同族数据) | 无 | — |
| poet | (table_translator 内部) | 造句器:句内路径按 weight+左结合比较选最优 | **有(等价)**:beam search | 无 | — |
| single_char_filter | filter | 单字候选提到词组前(仅 table/user_table 类型,遇其他类型 break) | **无**(刻意决策:beam 整句打分让词组天然优先,见 lib.rs 测试 `phrase_path_outranks_split_words`) | 想快速选单字时要多翻页——当前设计取词组优先,此为**方向相反的刻意选择**,不算缺陷 | **P2** |
| uniquifier | filter | 同文本候选去重合并 | **有(等价)**:lookup_candidates 按 (word,pinyin) 合并取最优路径分(lib.rs:358-368);fallback 同(lib.rs:679-687) | 无 | — |
| shape | (模块,非 gear 组件) | 全角/半角西文与数字转换 | **无** | 财务/排版场景打不出全角数字字母(`ABC`);依赖全角标点的 punctuator 缺失后此问题被掩盖 | **P2** |
| translator_commons | (公共代码,非组件) | preedit 格式化、候选公共件 | **不适用** | — | **P2** |

**主表行数:36**(含 key_binding_processor 与其 _impl 合一行;opencc / filter_commons /
translator_commons / memory / unity_table_encoder / grammar / poet / shape 为
非独立挂载组件或公共代码,均保留一行并注明性质)。

**P0 组件(4 个)**:punctuator、ascii_composer(部分)、memory(部分,未接线+不造词)、
fallback_segmentor × echo_translator 组合(无候选时无可上屏候选)。

---

## P0/P1 缺失项逐项细节

### 1. punctuator(P0,无)

**librime 实现要点**(`punctuator.cc`,实读):

```cpp
ProcessResult Punctuator::ProcessKeyEvent(const KeyEvent& key_event) {
  if (key_event.release() || key_event.ctrl() || key_event.alt() ||
      key_event.super())
    return kNoop;
  int ch = key_event.keycode();
  if (ch < 0x20 || ch >= 0x7f)
    return kNoop;
  Context* ctx = engine_->context();
  if (ctx->get_option("ascii_punct")) {
    return kNoop;
  }
  ...
  if (!use_space_ && ch == XK_space && ctx->IsComposing()) {
    return kNoop;
  }
```

标点键被 processor 拦下 → 进输入串由 punct_segmentor 切段 → punct_translator
按 `punctuator/half_shape` 映射表(`/usr/share/rime-data/punctuation.yaml`,
default.yaml 里 `punctuator: __include: punctuation:/half_shape`)产出中文标点候选。
另有数字例外:`is_after_number` 时 `.` `:` `,` 保持半角(3.14、12:30)。

**xwx 落点**:
- `crates/xwx_ibus/src/ibus_proto.rs` `translate_keyval`(约 110-130 行):
  `match` 只认空格/回车/Esc/方向/翻页/数字/字母,标点键返回 `None` 透传;
- `crates/xwx_engine/src/session.rs:192` `InputEvent::Char(_) => Noop`——
  即使 ibus 层放行进来的非标点字母字符也被丢弃;
- 改动点:`translate_keyval` 放行 ASCII 标点为 `InputEvent::Char(c)`;session.rs
  的 `Char(_)` 分支加一张半角→全角映射表(中文模式下直接 commit 全角标点),
  并保留 session.rs:191 已有的 TODO(拼音串非空时标点上屏)。数字例外规则可后置。

### 2. ascii_composer(P0,部分)

**librime 实现要点**(`ascii_composer.cc`,实读):Shift 单击 500ms 内释放触发
inline_ascii/commit_text;ascii 模式下字母键:

```cpp
  bool ascii_mode = ctx->get_option("ascii_mode");
  if (ascii_mode) {
    if (!ctx->IsComposing()) {
      return kRejected;  // direct commit
    }
    // edit inline ascii string
    if (!key_event.release() && ch >= 0x20 && ch < 0x80) {
      ctx->PushInput(ch);
      return kAccepted;
    }
  }
```

CapsLock 态下字母大小写翻转直接上屏(`if (islower(ch)) ch = toupper(ch)` 段)。

**xwx 落点**:
- `crates/xwx_ibus/src/ibus_proto.rs:125-127`:大写 keyval 直接
  `to_ascii_lowercase()` 成 `InputEvent::Char` 进拼音串——Shift+字母没有独立事件;
- `crates/xwx_engine/src/session.rs:184-188` 注释自认 TODO:
  「Shift+字母目前按小写处理;可配置为"直接上屏大写"」;
- `crates/xwx_engine/src/session.rs:176-178`:english_mode 下 `Char(c)` Noop
  透传,而 ibus 层把大写归小写后透传——**英文模式下 Shift+G 上屏的也是小写 g**
  (归一化发生在透传之前);
- 改动点:`translate_keyval` 把 SHIFT_MASK 状态或大小写 keyval 区分传入
  (需要把 `state` 里的 shift 位透到事件,或在 ibus_proto 直接保留大写 keyval
  生成 `Char('G')`),session.rs 在 english_mode 分支允许 Shift+字母以
  原样 Commit 而非 Noop。Shift 单击切换(按下+快速释放无其他键)需要新增
  Shift 键的 press/release 事件,改动较大,可只先做「Shift+字母出大写」。

### 3. memory × unity_table_encoder(P0/P1,部分且未接线)

**librime 实现要点**(`memory.cc` + `table_translator.cc`,实读):

```cpp
bool CommitEntry::Save() const {
  if (memory && !empty()) {
    DLOG(INFO) << "memorize commit entry: " << text;
    return memory->Memorize(*this);
  }
  return false;
}
```

`table_translator.cc:323-350`:上屏时 `encoder_->EncodePhrase(commit_entry.text, "1")`
——把上屏的**整段文本**(可能跨多个词)反查编码后写入用户词典,这就是「造词」:
下次打同样拼音,整段短语直接作为用户词条出现。xwx 的 UserLM 只学词级
unigram/bigram 计数(user_lm.rs `learn`),短语没有被编码成词条,beam 查词时
永远查不到它——多字新词只能靠 bigram 概率在逐字路径上「间接变快」,不会作为
整体候选出现。

**xwx 落点**:
- 调频接线:`src/main.rs` 装配引擎处(约 86-124 行)从未调用
  `with_user_lm`(grep 全仓库仅 session.rs:526 单测调用)——`engine.rs:227`
  `Enable` 的 TODO「加载用户词频」与此是同一个洞。改动点:main.rs
  `ImeEngine::new(...)` 之后 `engine = engine.with_user_lm(UserLM::open_or_create(...))`,
  并在退出前 flush(user_lm.rs:257 已有 `flush`)。
- 造词:`crates/xwx_engine/src/lib.rs` `learn_committed`(305 行)处增加
  「把上屏的未确认整段(多次逐字上屏拼出的串)写入用户词库」的机制;
  需要先能在 commit 路径拿到「本次上屏串对应的拼音」,session.rs
  `commit_word`/`commit_raw` 目前只传 word,不传拼音串。

### 4. fallback_segmentor × echo_translator(P0/P1,组合问题)

librime 两者配合的完整闭环:fallback_segmentor 保证切分永不为空(raw 段逐格后延,
`last.end = k + 1`),echo_translator 在 raw 段上产出「原始输入本身」的候选,
且 `EchoTranslation::Compare` 里「只要别人有候选就 `set_exhausted(true)`」——
回声候选**仅在全场无候选时冒头**,quality=-100 垫底。

**xwx 现状**(实读 session.rs:205-216):Space 时 `candidates.get(cursor)`
为 `None` 则 `commit_raw(pinyin)`——功能上能原样上屏,但**候选窗是空的**,
用户看不到「空格将上屏 ni」这个候选。GNOME Shell 的候选窗收到空表直接不显示,
用户视角是「打了一串字母,什么都没出现,输入法死了」。

**xwx 落点**:`crates/xwx_engine/src/lib.rs` `fallback_candidates`(629 行)
末尾:`merged` 为空或全部候选过滤完后,追加一个 `Candidate::new(pinyin, pinyin, 0)`
式的回声候选(词=拼音串本身),并保证其排在最后(score 取最低或加 `echo` 分层
标志,参照 fuzzy 分层做法);`commit_word` 路径对它天然兼容(词上屏=原字母上屏,
但需注意 learn_committed 里它会被当成「词」学习——应在 commit_word 处特判,
参照 echo_translator 的 raw 类型不学)。

### 5. selector 的 `,`/`.` 翻页(P1,部分)

**librime 实现要点**(`selector.cc`,实读):

```cpp
  const string& select_keys(engine_->schema()->select_keys());
  if (!select_keys.empty() && !key_event.ctrl() && ch >= 0x20 && ch < 0x7f) {
    size_t pos = select_keys.find((char)ch);
    ...
    SelectCandidateAt(ctx, index);
```

`,`/`.` 翻页由 default.yaml 的 `key_binder/bindings` 配置驱动(本机
default.yaml 实读含 `key_bindings:/paging_with_comma_period`)。

**xwx 落点**:`crates/xwx_engine/src/session.rs` `handle`(171 行)增加
`InputEvent::Char(',')`/`Char('.')` 在 `!pinyin.is_empty()` 时映射为
PageUp/PageDown 的分支(注意与 punctuator 落地的冲突:`,` 既翻页又是标点,
rime 的解法是 has_menu 时才翻页——xwx 对应条件即 `!candidates.is_empty()`)。

### 6. editor 的 Ctrl+Delete 删候选(P1,无)

**librime 实现要点**(`editor.cc`,实读):

```cpp
bool Editor::DeleteCandidate(Context* ctx) {
  ctx->DeleteCurrentSelection();   // → 通知 user_dict 删词条/降权
  ...
}
// keymap.Bind({XK_Delete, kControlMask}, &Editor::DeleteCandidate);
```

**xwx 落点**:`crates/xwx_engine/src/session.rs` 增加 `InputEvent::DeleteCandidate`;
`crates/xwx_engine/src/user_lm.rs` `UserLM` 需增加「负学习/删除条目」接口
(当前只有 `learn` 正向计数,user_lm.rs:178);ibus 层 `translate_keyval`
需要识别 Delete 键 + Ctrl 修饰位(目前 Ctrl 组合在 engine.rs:194-203 被
整体透传,要把 Ctrl+Delete 单拎出来,参照 is_ctrl_space 的写法)。

### 7. navigator(P1,无)

**librime 实现要点**(`navigator.cc`,实读):

```cpp
    keymap.Bind({XK_Left, 0}, &Navigator::Rewind);
    keymap.Bind({XK_Left, kControlMask}, &Navigator::LeftBySyllable);
    ...
bool Navigator::LeftBySyllable(Context* ctx) {
  BeginMove(ctx);
  size_t confirmed_pos = ctx->composition().GetConfirmedPosition();
  JumpLeft(ctx, confirmed_pos, true);
  return true;
}
```

**xwx 落点**:`crates/xwx_engine/src/session.rs:257-258` 注释自认
「TODO 移动拼音串内光标(当前直接透传)」。需要:InputSession 增加
`caret: usize` 字段(拼音串内光标),`Char` 分支从 push 改为在 caret 处插入,
Backspace 删 caret 前字符;preedit 光标位置经 `update_preedit_text` 的
cursor_pos 传给 IBus(engine.rs:93 目前恒为串尾 `u.preedit.chars().count()`)。
按音节跳(Ctrl+←/→)可后置,先实现按字符移动。

### 8. speller 的分隔符 `'`(P1,部分)

**librime 实现要点**(`speller.cc`,实读):

```cpp
  config->GetString("speller/delimiter", &delimiters_);   // luna: " '"
  ...
  if (!belongs_to(ch, alphabet_) && !belongs_to(ch, delimiters_))
    return kNoop;
  ...
  ctx->PushInput(ch);
```

分隔符进输入串,abc_segmentor 切分时按 `'` 硬切段——`xi'an` 只有一种切法。

**xwx 落点**:`crates/xwx_engine/src/lib.rs` `build_lattice`(407 行)/
`lookup_candidates`(339 行)入口:把含 `'` 的输入按 `'` 拆成多段分别建格
解码(或建格时把 `'` 位置视为强制切分点);`translate_keyval` 放行 `'`。
注意 `Candidate.pinyin` 与词典 key 用空格分隔音节,`'` 不要进 pinyin 字段。

### 9. recognizer(P1,无)

**librime 实现要点**(`recognizer.cc`,实读):

```cpp
    // pattern matching against the input string plus the incoming character
    Context* ctx = engine_->context();
    string input = ctx->input();
    input += ch;
    auto match = patterns_.GetMatch(input, ctx->composition());
    if (match.found()) {
      ctx->PushInput(ch);
      return kAccepted;
    }
```

default.yaml 的 patterns(本机实读):email / uppercase / url 三条正则;
luna_pinyin 另加 punct 与 reverse_lookup 两条。匹配上的整段标 tag 不切分,
直接作为英文/URL 候选上屏。

**xwx 落点**:`crates/xwx_engine/src/session.rs` `Char` 分支前加正则预判:
当前 `pinyin` + 新字符整体匹配 email/URL 模式时,转入「raw 段」状态
(可复用 fallback 的 commit_raw 路径,或新增一个 verbatim 模式标志);
或更简方案:检测 `:` `/` `@` `.` 等字符出现时若前缀像 URL/邮箱则整串直通。
正则 crate 依赖与状态机改动需设计,标 P1。

### 10. key_binder(P1,部分)

librime 的 key_binder 把「when(composing/paging/has_menu)+ accept + toggle_option/
send_sequence」做成配置驱动(default.yaml `key_binder/bindings` 挂
`key_bindings:/numbered_mode_switch` 等)。xwx 的空格确认/回车上屏/Esc 清空/
数字选词已硬编码等价(session.rs `handle`),**已覆盖日常编辑动作**;缺的是
option 切换热键——而 option 体系(简繁/全半角)本身不存在,所以该项实际是
simplifier/shape 的附属,不单列优先级。

---

## 读过的 xwx 源码清单(判定依据)

| 文件 | 读取范围 | 支撑的判断 |
|---|---|---|
| `crates/xwx_engine/src/lib.rs` | 全文 1-1383 | lookup_candidates:339、build_lattice:407、decode_beam:494、fallback_candidates:629、sort_candidates:698(精确/模糊分层)、learn_committed:305 |
| `crates/xwx_engine/src/session.rs` | 全文 1-642 | InputEvent 全集:29-53;english_mode:176-178;Char 透传:192;Space commit_raw:205-216;Left/Right TODO:257-258;commit_word/raw:313-326;reset 不清上文:366-371 |
| `crates/xwx_ibus/src/engine.rs` | 全文 1-364 | Ctrl+空格拦截:176-191;Ctrl/Alt 组合透传:194-203;FocusIn/Enable TODO:211-229;cursor_pos 恒为串尾:93 |
| `crates/xwx_ibus/src/ibus_proto.rs` | 100-130(translate_keyval)、137-167、400-417 | 大小写归一:125-127;无标点/Delete 键映射 |
| `crates/xwx_engine/src/segmenter.rs` | 全文 1-237 | SYLLABLES:21-95(排除单字母音节注释:17-19);segment DP:132-140 |
| `crates/xwx_engine/src/candidate.rs` | 全文 1-75 | Candidate 字段;fuzzy bool:37;无 comment/weight/type 字段(→ reverse_lookup_filter、simplifier tips 无落点) |
| `crates/xwx_engine/src/dict.rs` | 公开函数清单(grep)+ 65-77 | lookup/lookup_prefix/total_freq;无按词文本反查、无用户词条接口 |
| `crates/xwx_engine/src/user_lm.rs` | 公开函数清单(grep) | learn:178 仅正向计数;无删除/负学习 |
| `crates/xwx_engine/src/lm.rs` | 公开函数清单 + 89-114、256-268 | LmContext(tail+last_word);MixedLM 混合 |
| `crates/xwx_engine/src/corrector.rs` | 公开函数清单(grep) | expand_fuzzy_scored——speller algebra 的等价物 |
| `crates/xwx_engine/src/wanxiang.rs` | 公开函数清单(grep) | WanxiangLM——grammar 的等价物 |
| `src/main.rs` | 1-140 | 引擎装配:86-124(**未调用 with_user_lm**,UserLM 仅单测接线,已 grep 全仓库确认) |

## 读过的 librime 源码

实读 .cc 原文(贴代码的组件):punctuator、recognizer、speller、selector、
navigator、key_binder、ascii_composer、echo_translator、fallback_segmentor、
single_char_filter、uniquifier、history_translator、memory、table_translator、
reverse_lookup_filter、switch_translator、ascii_segmentor、editor、abc_segmentor、
matcher、charset_filter、contextual_translation、poet、unity_table_encoder。
script_translator、simplifier、opencc、chord_composer、schema_list_translator、
reverse_lookup_translator 为 grep 定位关键行(未逐行通读,相关结论只引用到
grep 命中的行,不超出其含义)。
`grammar.cc` 在 librime master 上不存在(grammar 是插件组件,实现在
librime-octagram 仓),poet.cc 里 `Grammar::Require("grammar")` 为动态加载——
已验证结论「万象 .gram 是字符级窗口模型」与此对应。

## 显式标注的推测

- **推测**:GNOME Shell 对空 lookup table 不显示候选窗(依据 engine.rs:103-105
  空表时主动 HideLookupTable 的写法反推,未实测)。
- **推测**:rime-ice 词库含 CJK 扩展区字(charset_filter 行的后果依据
  data/README 提到 rime-ice 全量词表,未逐字核查词库内容)。
- **推测**:xwx 词库为纯简体(simplifier 行「无繁体源可转」的依据是
  data 目录词表文件名与测试用词均为简体,未统计词库中的繁体字)。
- 主表中所有「有(等价)」的判断均有上文源码清单中的直接依据,非推测。
