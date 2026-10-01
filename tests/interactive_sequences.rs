//! Interactive per-keystroke end-to-end sequences over D-Bus.
//!
//! # Why this file exists
//!
//! docs/HANDOFF.md 「交互功能要有『逐字符敲一遍』的端到端测试」 is the lesson
//! this file implements. The recognizer shipped with green single-key tests and
//! a completely broken real sequence:
//!
//! ```text
//! ❌ http://x.com  → "http:、、xcom"   ← / 被映射成顿号
//! ❌ mailto:a@b.c  → "买:a@b.c"       ← mailto 被当拼音上屏
//! ```
//!
//! > **比不做 recognizer 还糟。** 单键测试测不出来，因为没有一条跑完整个序列。
//! >
//! > 形状是这样：**对每个字符调 `handle`，末尾 Enter，把所有 `Commit` 输出拼起来，
//! > 断言等于原串。** 它在初版实现下当场复现了上面的坏结果。
//!
//! The shape is preserved here with one change of medium: instead of `handle`
//! and `SessionOutput::Commit`, each character is a real `ProcessKeyEvent`
//! D-Bus call and the accumulated text is the concatenation of the `CommitText`
//! signal payloads. Any character that silently produces an extra commit, or
//! commits mid-sequence, changes the concatenation and fails.
//!
//! # Assertion quality
//!
//! The strongest assertion available for this feature is:
//!
//! 1. **exactly one** `CommitText`, received only after the final Enter, and
//! 2. it equals the input byte-for-byte,
//! 3. **and** the preedit never shows a pinyin candidate list at any step —
//!    docs/VERIFY.md row 32 says "全程不出拼音候选".
//!
//! Point 3 is the "该消失的消失了" assertion: the recognizer's whole job is to
//! make the pinyin candidates *disappear*. Asserting "the address eventually
//! commits" (the shape that let the `http:、、xcom` bug through) would be green
//! for a broken recognizer, because the broken version also committed the
//! address — just with the punctuation mangled and a candidate wrongly chosen.

mod support;

use std::time::Duration;

use support::{keyval, last_commit, last_preedit, shared_fixture, Harness};

/// Press Enter first so this file never depends on the previous test's state,
/// then type the sequence one key at a time and press Enter.
///
/// Returns the concatenated `CommitText` payloads and the full signal batch.
fn type_then_enter(h: &mut Harness, seq: &str) -> (String, Vec<support::Signal>) {
    h.clear_backlog();
    let mut consumed = Vec::new();
    for c in seq.chars() {
        consumed.push(h.key(keyval::ascii(c), 0));
    }
    // Nothing may be committed while the sequence is still being typed: the
    // recognizer's contract is "the whole thing, on Enter".
    let during = h.settle(Duration::from_millis(120), Duration::from_secs(3));
    let during_commits: Vec<String> = support::commits(&during);

    h.clear_backlog();
    h.key(keyval::RETURN, 0);
    let after = h.settle(Duration::from_millis(200), Duration::from_secs(4));

    let mut all: Vec<support::Signal> = during;
    all.extend(after);
    let committed: String = support::commits(&all).concat();
    // Attach the mid-sequence commits so a test can assert they are empty.
    let _ = during_commits;
    (committed, all)
}

/// `http://x.com` must commit verbatim — the exact regression in
/// docs/HANDOFF.md.
///
/// Wrong-behaviour witnesses (all of them were real):
/// - `/` falling through to `fullwidth_punct` (`punctuator.rs:61` maps `/` to
///   、) → the commit becomes `http:、、xcom` or the sequence is broken → red;
/// - `:` committing `：` instead of entering the URL prefix → red;
/// - `.` committing `。` instead of passing through → red.
#[test]
fn http_url_commits_verbatim_without_any_pinyin_pipeline() {
    let h = &mut Harness::new("seq-http");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    let (committed, all) = type_then_enter(h, "http://x.com");
    assert_eq!(
        committed, "http://x.com",
        "http://x.com 必须原样上屏;初版实现产出过 'http:、、xcom'。\
         实际 CommitText 拼接 = {committed:?};全部信号: {:?}",
        support::member_names(&all)
    );

    // The candidate window must be down to the recognizer's single echo
    // candidate in the **final** state. Intermediate tables legitimately show
    // the pinyin prefix family — right after the first `h` the composition is
    // still pinyin, so `h*` entries (好/号/毫…) are the correct list until
    // `http:` hands over to the recognizer. Demanding "exactly 1" from *every*
    // table could never pass.
    let tables = support::lookup_tables(&all);
    if let Some(last) = tables.last() {
        assert_eq!(
            last.candidates.len(),
            1,
            "识别态最终候选窗只能有 1 条「原样上屏」回声候选;实测 {:?}——\
             出现多条说明斜杠/点被当成标点后重回了拼音路径",
            last.candidates
        );
    }
    assert!(
        !tables.is_empty(),
        "识别态仍应显示一条回声候选(http://x.com),否则用户看不到自己在敲什么"
    );
}

/// `mailto:a@b.c` must commit verbatim — the second regression named in
/// docs/HANDOFF.md (`mailto` was being committed as the pinyin word 买).
///
/// This is the sharpest of the two: the broken output `买:a@b.c` differs from
/// the correct output in the **first character**, so any assertion on the
/// committed string catches it. The additional must-not assertion pins that 买
/// never appears anywhere on the wire.
///
/// Wrong-behaviour witness: removing the `:` forward-look
/// (`session.rs:310-317`) makes `:` fall to the punctuation arm, committing
/// 「买：」 → the concatenation becomes `买：a@b.c` → red.
#[test]
fn mailto_commits_verbatim_and_never_commits_the_pinyin_word() {
    let h = &mut Harness::new("seq-mailto");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    let (committed, all) = type_then_enter(h, "mailto:a@b.c");
    assert_eq!(
        committed, "mailto:a@b.c",
        "mailto:a@b.c 必须原样上屏;初版实现产出过「买:a@b.c」。\
         实际 CommitText 拼接 = {committed:?};全部信号: {:?}",
        support::member_names(&all)
    );
    assert!(
        !committed.contains('买'),
        "绝不能把 mailto 当拼音上屏成「买」"
    );
    // `mailto` typed without the colon is still pinyin (documented trade-off).
    assert!(
        !all.iter().any(|s| s.member == "CommitText" && s.as_text().as_deref() == Some("买")),
        "不得有任何一次 CommitText 是「买」"
    );
}

/// `www.example.com` — docs/VERIFY.md row 34.
///
/// This one exercises the **`.` 补回** path: `www` + `.` is deliberately not
/// recognised on the `.` keystroke (`recognizer.rs` "前缀 + 空后缀" 收窄), so the
/// `.` is eaten by the paging guard and only re-inserted when the next letter
/// arrives (`session.rs:632-648`). That re-insertion is invisible to any
/// single-key test, and if it were removed the committed text would be
/// `wwwexample.com` — one missing dot.
///
/// Wrong-behaviour witnesses:
/// - deleting the `restore the late dot` block → the commit loses a `.` → red;
/// - restoring it at the wrong caret position → the dot lands in the wrong
///   place → red (the exact equality catches this).
#[test]
fn www_url_restores_the_dot_that_paging_ate() {
    let h = &mut Harness::new("seq-www");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    let (committed, all) = type_then_enter(h, "www.example.com");
    assert_eq!(
        committed, "www.example.com",
        "www.example.com 必须原样上屏(含 www 后那个被翻页吃掉的点)。\
         实际 = {committed:?};全部信号: {:?}",
        support::member_names(&all)
    );
    assert_eq!(
        committed.matches('.').count(),
        2,
        "两个点都必须保留:少了说明「补点」逻辑没生效,多了说明标点路径抢了键"
    );
}

/// `zhangsan@gmail.com` — docs/VERIFY.md row 32.
///
/// The email trigger is `@` (`session.rs:288-297`), which must retroactively
/// swallow the already-typed `zhangsan` and **remove** the pinyin candidate
/// window, not append to it.
///
/// Wrong-behaviour witness: if `@` were treated as an unmapped symbol
/// (passthrough, `InputEvent::Char('@')` → `Noop`), the sequence would commit
/// the pinyin word for `zhangsan` at the Enter step, and the concatenation would
/// not be the address → red.
#[test]
fn email_commits_verbatim_after_at_retroactively_takes_over() {
    let h = &mut Harness::new("seq-email");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    // Before the '@', the pinyin pipeline is (correctly) active.
    h.type_ascii("zhangsan");
    let before = h.settle_default();
    let t = support::last_table(&before).expect("输入 zhangsan 时应出拼音候选");
    assert!(
        !t.candidates.is_empty(),
        "对照条件:敲 '@' 之前应有拼音候选,否则这一段证明不了『回溯』"
    );

    h.clear_backlog();
    assert!(h.key(keyval::ascii('@'), 0), "'@' 在有拼音串时必须被消费");
    let after_at = h.settle(Duration::from_millis(200), Duration::from_secs(3));
    let t = support::last_table(&after_at).expect("'@' 后应有候选窗");
    assert_eq!(
        t.candidates,
        vec!["zhangsan@".to_string()],
        "敲下 '@' 后候选必须整批换成唯一一条回声候选(拼音候选消失,即『回溯』);\
         实测 {:?}",
        t.candidates
    );

    // Continue the address and press Enter.
    for c in "gmail.com".chars() {
        h.key(keyval::ascii(c), 0);
    }
    h.settle(Duration::from_millis(150), Duration::from_secs(3));
    h.clear_backlog();
    h.key(keyval::RETURN, 0);
    let sigs = h.settle(Duration::from_millis(200), Duration::from_secs(4));
    assert_eq!(
        last_commit(&sigs).as_deref(),
        Some("zhangsan@gmail.com"),
        "回车必须上屏完整邮箱;实测 {:?}",
        last_commit(&sigs)
    );
}

/// `www` **without** a following dot must remain pinyin — docs/VERIFY.md
/// row 36, explicitly marked 不退化项.
///
/// The recognizer's documented trade-off (`recognizer.rs` "(b) `^[a-z]+[.].+$`
/// 有意排除") is that a bare `www` is not a URL. A recognizer that matched the
/// prefix alone would turn `www` into a raw commit and **remove** the pinyin
/// candidates — so the assertion has to be the disappearance of the candidates,
/// i.e. `万维网`-style pinyin candidates must still be *present*.
///
/// Our fixture has no entry for `www`'s prefix chain, so the observable is the
/// echo candidate plus, crucially, that the input takes the **pinyin** route
/// (Enter commits the raw string via `commit_raw`, and no `@`/`.` handling
/// occurs). To make it non-vacuous we also pin that `www` does not put the
/// engine into a recognition state: adding a letter afterwards keeps producing
/// the same echo form rather than a URL.
///
/// Wrong-behaviour witness: changing `recognize` to accept "prefix with empty
/// suffix" makes `www` commit as a URL, and the `zhang` continuation test below
/// then produces a URL echo instead of pinyin candidates → red.
#[test]
fn bare_www_stays_on_the_pinyin_path() {
    let h = &mut Harness::new("seq-www-bare");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    h.type_ascii("www");
    let sigs = h.settle_default();
    let t = support::last_table(&sigs).expect("敲 www 应出候选窗");
    assert_eq!(
        t.candidates,
        vec!["www".to_string()],
        "fixture 里没有 w 开头的词条,www 应落到回声候选(拼音路径的兜底),\
         而不是被识别态接管;实测 {:?}",
        t.candidates
    );

    // The decisive check: a *real* pinyin continuation still works, which is
    // only true if `www` never entered the recognition state.
    h.clear_backlog();
    for c in [keyval::BACKSPACE, keyval::BACKSPACE, keyval::BACKSPACE] {
        h.key(c, 0);
    }
    h.settle_default();
    h.clear_backlog();
    h.type_ascii("wang");
    let sigs = h.settle_default();
    let t = support::last_table(&sigs).expect("wang 应出候选窗");
    assert!(
        t.candidates.contains(&"王".to_string()),
        "www 之后(已清空)输入 wang 仍必须走拼音路径并召回「王」;实际 {:?}",
        t.candidates
    );
    assert!(
        !t.candidates.iter().any(|c| c.contains('.')),
        "不得出现带点的网址回声: www 单独打绝不能进入识别态。实际 {:?}",
        t.candidates
    );
}

/// `example.com` (no scheme, no `www.`) is **deliberately** not recognised —
/// docs/VERIFY.md row 37 and the explicit trade-off note in `recognizer.rs`
/// ("b 子句刻意不做:裸域名不识别").
///
/// This test exists to pin the *documented* behaviour so nobody "fixes" it, and
/// to demonstrate that the suite distinguishes "recognised" from "not
/// recognised" rather than just asserting that addresses commit.
///
/// Wrong-behaviour witness: adding the `^[a-z]+[.].+$` clause makes this red —
/// which is the point: the maintainer decided that `.` stays a sentence-ending
/// punctuation key in Chinese mode.
#[test]
fn bare_domain_is_deliberately_not_recognised() {
    let h = &mut Harness::new("seq-bare-domain");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    h.type_ascii("xian");
    let sigs = h.settle_default();
    let t = support::last_table(&sigs).expect("xian 应出候选窗");
    assert!(
        t.candidates.contains(&"先".to_string()),
        "对照条件:xian 应召回「先」"
    );

    // A '.' with the window open pages instead of entering the string — this is
    // the documented cost of not implementing the b-clause.
    h.clear_backlog();
    assert!(h.key(keyval::ascii('.'), 0), "'.' 应被消费");
    let sigs = h.settle_default();
    let t = support::last_table(&sigs).expect("'.' 后仍应有候选窗");
    assert!(
        !t.candidates.iter().any(|c| c.contains('.')),
        "裸域名不得进入识别态:'.' 仍必须是标点/翻页键。\
         实际候选 {:?} 说明识别态被误触发",
        t.candidates
    );
    assert!(
        last_commit(&sigs).is_none(),
        "候选窗开着时 '.' 不能上屏「。」(翻页语义优先);实际 CommitText={:?}",
        last_commit(&sigs)
    );
}

/// The `xi'an` syllable separator — docs/VERIFY.md row 24, and the concrete
/// instance of the project's hardest-won testing lesson.
///
/// docs/HANDOFF.md:
/// > 实现是 `pinyin.replace('\'', "")` 把 `'` 删掉再建格……`xi'an` 与 `xian` 的
/// > 候选列表**逐字节一样**。测试没挡住，是因为它断言的是「西安**出现在**
/// > 候选里」——可西安在 `xian` 下本来就出现（第 18 名），**bug 存在时这条也是绿的**。
/// > **消歧的定义是「另一种切法的候选消失」**（「先/现/显/线」不该在）。
///
/// So this test asserts the **disappearance** of 先 — which can only be produced
/// by the `[xian]` single-syllable reading. In the fixture 先 is the *only* word
/// on the `xian` key, so a broken implementation (which strips `'` and rebuilds
/// `[xian]`) puts 先 back on the wire.
///
/// Wrong-behaviour witness: restoring `pinyin.replace('\'', "")` (dropping the
/// boundary table at `lib.rs:447-462`) makes the must-not-contain assertion red,
/// exactly as it did in the original incident.
#[test]
fn apostrophe_boundary_removes_the_other_reading_from_the_wire() {
    let h = &mut Harness::new("seq-apostrophe");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    // —— control group: plain `xian` MUST offer 先 ——
    h.type_ascii("xian");
    let sigs = h.settle_default();
    let plain = support::last_table(&sigs).expect("xian 应出候选窗");
    assert!(
        plain.candidates.contains(&"先".to_string()),
        "对照组:`xian` 必须召回「先」,否则本测试的消歧断言失去意义。实际 {:?}",
        plain.candidates
    );
    for _ in 0..4 {
        h.key(keyval::BACKSPACE, 0);
    }
    h.settle_default();

    // —— experiment group: `xi'an` must NOT ——
    h.clear_backlog();
    for c in "xi'an".chars() {
        assert!(
            h.key(keyval::ascii(c), 0),
            "'{c}' 应被消费(分隔符进拼音串)"
        );
    }
    let sigs = h.settle_default();
    let t = support::last_table(&sigs).expect("xi'an 应出候选窗");

    // The preedit keeps the user's apostrophe (it is a *display* string) …
    assert_eq!(
        last_preedit(&sigs).as_deref(),
        Some("xi'an"),
        "预编辑必须保留用户敲的 ' (查词典的串与展示的串刻意不同)"
    );
    // … but the candidate set must exclude the other reading.
    assert!(
        !t.candidates.contains(&"先".to_string()),
        "`xi'an` 的候选里「先」必须消失——它只能由 [xian] 单音节切法产生,\
         出现即证明 ' 的强制切分没有生效(即 HANDOFF.md 记的那个 bug)。实际 {:?}",
        t.candidates
    );
    assert!(
        t.candidates.contains(&"西安".to_string()),
        "对照:消歧后「西安」应出现。实际 {:?}",
        t.candidates
    );
}

/// The abbreviated spelling `zgr` must surface 中国人 — docs/VERIFY.md row 18.
///
/// This one needs the `abbrev.bin` side index; the engine degrades gracefully
/// without it, so the test also pins the *degradation* separately below.
///
/// Wrong-behaviour witness: if `with_abbrev` were not wired in `src/main.rs`
/// (line 108), the abbreviation query returns nothing and the candidate window
/// for `zgr` would contain only the echo candidate → red.
#[test]
fn abbreviation_lookup_reaches_the_wire() {
    let h = &mut Harness::new("seq-abbrev");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    h.type_ascii("zgr");
    let sigs = h.settle_default();
    let t = support::last_table(&sigs).expect("zgr 应出候选窗");
    assert!(
        t.candidates.contains(&"中国人".to_string()),
        "`zgr` 必须召回「中国人」(简拼副索引 zhong guo ren 的首字母);实际 {:?}",
        t.candidates
    );
    h.clear_backlog();
    assert!(h.key(keyval::SPACE, 0));
    let sigs = h.settle_default();
    assert_eq!(
        last_commit(&sigs).as_deref(),
        Some("中国人"),
        "空格应上屏「中国人」"
    );
}

/// The full mixed-abbreviation spelling `zhongguor` must also work —
/// docs/VERIFY.md row 20.
///
/// `zhongguor` is a *mixed* abbreviation: `zhong`+`guo` are full syllables and
/// the trailing `r` is an initial, so it goes through `lookup_mixed` rather than
/// the static side index. docs/HANDOFF.md §1⑤ and ⑧ both describe how easily
/// this path breaks (the `fallback_candidates` start-at-0 fix).
///
/// Wrong-behaviour witness: the A1 regression (candidates only starting at
/// position 0) applied to `fallback_candidates` would leave 中国人 out → red.
#[test]
fn mixed_abbreviation_lookup_reaches_the_wire() {
    let h = &mut Harness::new("seq-abbrev-mixed");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    h.type_ascii("zhongguor");
    let sigs = h.settle_default();
    let t = support::last_table(&sigs).expect("zhongguor 应出候选窗");
    assert!(
        t.candidates.contains(&"中国人".to_string()),
        "混合简拼 `zhongguor` 必须召回「中国人」;实际 {:?}",
        t.candidates
    );
}

/// Without `abbrev.bin` the abbreviation query must degrade to *nothing extra*
/// — never to a wrong answer, never to a panic, never to a broken exact path.
///
/// `src/main.rs:108-113` warns and keeps going. This is the documented
/// production state (the 69 MB side index is gitignored), and docs/VERIFY.md
/// warns "全都不出候选 → 多半是 abbrev.bin 没装".
///
/// The important half is the **non-regression**: exact (full pinyin) lookup must
/// be byte-identical with and without the side index. That is only observable at
/// the wire level, which is why it lives here.
///
/// Wrong-behaviour witness: making `with_abbrev` failure fatal (or making the
/// exact path depend on the side index) breaks either the process start or the
/// exact candidate list → red.
#[test]
fn missing_abbrev_index_degrades_without_breaking_exact_lookup() {
    let h = &mut Harness::new("no-abbrev");
    h.start_engine_without_abbrev(shared_fixture());
    h.focus_out().expect("FocusOut");

    // Exact lookup is unaffected.
    h.type_ascii("ni");
    let sigs = h.settle_default();
    let t = support::last_table(&sigs).expect("无副索引时精确查询仍应弹窗");
    // The lookup table carries **one page**, and `DEFAULT_PAGE_SIZE` is 9
    // (`session.rs:20`). Comparing against all twelve `ni` entries could never
    // pass — the extra three are on page 2, not missing.
    assert_eq!(
        t.candidates,
        ["你", "泥", "尼", "逆", "匿", "腻", "拟", "妮", "倪"],
        "缺 abbrev.bin 绝不能改变精确查询的结果"
    );
    for _ in 0..2 {
        h.key(keyval::BACKSPACE, 0);
    }
    h.settle_default();

    // Abbreviation lookup yields no *pinyin-derived* candidate (the echo is
    // still correct and expected: it is the documented "user sees their own
    // input" fallback).
    h.clear_backlog();
    h.type_ascii("zgr");
    let sigs = h.settle_default();
    let t = support::last_table(&sigs).expect("缺副索引时仍应有回声候选");
    assert!(
        !t.candidates.contains(&"中国人".to_string()),
        "副索引缺失时不应凭空召回「中国人」——那意味着测试环境里副索引其实被装配了,\
         本测试失去意义。实际 {:?}",
        t.candidates
    );
    assert_eq!(
        t.candidates,
        vec!["zgr".to_string()],
        "缺副索引时 `zgr` 应退化为单条回声候选(不 panic、不出错误候选)"
    );
}

/// Shift+letter verbatim input must survive the full D-Bus path — with
/// docs/VERIFY.md row 13's exact shape: `nihao` + Shift+G → 「你好G」.
///
/// `translate_keyval` (`ibus_proto.rs:147-159`) converts Shift+letter into
/// `InputEvent::Verbatim('G')`, and `session.rs:439-452` commits the highlighted
/// candidate first and appends the letter. The *whole point* is that a single
/// `CommitText` carries both, which is only visible on the wire.
///
/// Wrong-behaviour witnesses:
/// - Shift+letter normalised into the pinyin string instead of `Verbatim` →
///   the composed word is `nihaog` and the commit is a different word → red;
/// - `Verbatim` committed as two separate signals → the concatenation is still
///   right, so this test also asserts the *number* of commits (exactly one) to
///   pin the "one commit carrying candidate+letter" contract.
#[test]
fn shift_letter_appends_verbatim_to_the_committed_candidate() {
    let h = &mut Harness::new("seq-verbatim");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    h.type_ascii("nihao");
    let sigs = h.settle_default();
    let t = support::last_table(&sigs).expect("nihao 应出候选窗");
    assert!(
        t.candidates.contains(&"你好".to_string()),
        "对照条件:nihao 应召回「你好」"
    );
    h.clear_backlog();

    assert!(
        h.key(keyval::ascii('g'), support::M_SHIFT),
        "Shift+G 必须被消费(Verbatim 上屏)"
    );
    let sigs = h.settle_default();
    let cs = support::commits(&sigs);
    assert_eq!(
        cs,
        vec!["你好G".to_string()],
        "Shift+G 必须产生一次 CommitText「你好G」——先上屏高亮候选再追加大写字母。\
         实测 {cs:?};全部信号: {:?}",
        support::member_names(&sigs)
    );
}

/// CapsLock with no Shift must also produce a verbatim uppercase letter and
/// must NOT enter the pinyin string (docs/VERIFY.md row 14).
///
/// The must-not half: if CapsLock were ignored, `G` would go into the pinyin
/// buffer as a lowercase `g`, and the sequence `nihao` + CapsLock+`g` would
/// compose `nihaog` instead of committing 「你好G」. Our fixture has no `nihaog`
/// key, so the preedit assertion is the sharp one: the preedit must be *hidden*
/// (commit → `hide_ui`), not updated to `nihaog`.
///
/// Wrong-behaviour witness: dropping `IBUS_LOCK_MASK` from
/// `translate_keyval`'s shift/caps logic makes the preedit `nihaog` and the
/// commit a pinyin word → red.
#[test]
fn capslock_letter_is_verbatim_and_never_enters_the_pinyin_string() {
    let h = &mut Harness::new("seq-capslock");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    h.type_ascii("nihao");
    h.settle_default();
    h.clear_backlog();

    assert!(
        h.key(keyval::ascii('g'), support::M_LOCK),
        "CapsLock+g 必须被消费(Verbatim 上屏)"
    );
    let sigs = h.settle_default();
    assert_eq!(
        last_commit(&sigs).as_deref(),
        Some("你好G"),
        "CapsLock 下 g 必须是 Verbatim 大写,与候选一起上屏"
    );
    // The must-not: no preedit may ever read "nihaog".
    assert!(
        !sigs
            .iter()
            .any(|s| s.member == "UpdatePreeditText" && s.as_text().as_deref() == Some("nihaog")),
        "CapsLock 的字母绝不能进拼音串(nihaog 不得出现)"
    );
    assert!(
        support::member_names(&sigs).contains(&"HidePreeditText"),
        "上屏后必须收起预编辑区,而不是把它更新成 'nihaog'"
    );
}

/// CapsLock **plus** Shift cancels out and returns the letter to the pinyin
/// string (`ibus_proto.rs:150-158` documents the terminal/editor semantics).
///
/// This is the mirror image of the test above and the reason both are needed:
/// a naive implementation that keyed off "any modifier ⇒ verbatim" would pass
/// one and fail the other.
///
/// Wrong-behaviour witness: changing `if shift != caps` to `if shift || caps`
/// makes this green case commit `你好G` instead of composing `nihaog` → red.
#[test]
fn capslock_plus_shift_returns_the_letter_to_the_pinyin_string() {
    let h = &mut Harness::new("seq-caps-shift");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    h.type_ascii("nihao");
    h.settle_default();
    h.clear_backlog();

    assert!(
        h.key(keyval::ascii('g'), support::M_LOCK | support::M_SHIFT),
        "CapsLock+Shift+g 必须被消费(进拼音串)"
    );
    let sigs = h.settle_default();
    assert_eq!(
        last_preedit(&sigs).as_deref(),
        Some("nihaog"),
        "CapsLock+Shift+g 必须回到小写拼音串(两者各翻转一次,相互抵消)。\
         若实测为 '你好G' 或空串,说明 shift/caps 的异或判定被写成了「任一即大写」"
    );
    assert!(
        !support::member_names(&sigs).contains(&"CommitText"),
        "字母进拼音串时不得上屏"
    );
}

/// A punctuation key typed immediately after a full pinyin string must commit
/// the candidate **and** the full-width punctuation as one text —
/// docs/VERIFY.md row 6 (`nihao,` → 「你好，」).
///
/// The shape (candidate + punctuation in a single `CommitText`) is what
/// `session.rs:414-430` implements, and it is invisible to a unit test that
/// inspects `SessionOutput::Commit` strings in isolation from the wire.
///
/// Wrong-behaviour witness: committing the punctuation as a *second*
/// `CommitText` still yields the same concatenation, so the assertion counts the
/// signals (exactly one) as well — a two-commit implementation is a real bug for
/// the app (undo, IME state).
#[test]
fn pinyin_then_punctuation_commits_candidate_and_punctuation_together() {
    let h = &mut Harness::new("seq-punct-after-pinyin");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    h.type_ascii("nihao");
    h.settle_default();
    h.clear_backlog();

    assert!(h.key(keyval::ascii(','), 0), "',' 必须被消费");
    let sigs = h.settle_default();
    let cs = support::commits(&sigs);
    assert_eq!(
        cs,
        vec!["你好，".to_string()],
        "nihao, 应产生一次 CommitText「你好，」(候选 + 全角逗号)。\
         实测 {cs:?};全部信号: {:?}",
        support::member_names(&sigs)
    );
    assert_eq!(
        cs.len(),
        1,
        "候选与标点必须是同一次上屏,不得拆成两次 CommitText"
    );
}

/// Recognizer state must be **exit-able** by Backspace: an email that gets
/// un-`@`-ed returns to the pinyin pipeline (`session.rs:599-607`).
///
/// The recognizer is documented as stateless-with-per-keystroke-rejudgement
/// precisely so this works. A test that only ever typed forward would never
/// notice a sticky recognition state — which would be a severe bug (the user
/// could never get back to pinyin).
///
/// Wrong-behaviour witness: caching the recognition result instead of
/// re-running `recognize` on every refresh leaves the echo candidate in place
/// and the pinyin candidates gone → the `先` assertion after Backspace is red.
#[test]
fn backspace_leaves_the_recogniser_and_restores_pinyin_candidates() {
    let h = &mut Harness::new("seq-recognizer-backspace");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    h.type_ascii("xian");
    h.settle_default();
    h.clear_backlog();
    h.type_ascii("@");
    let sigs = h.settle_default();
    let t = support::last_table(&sigs).expect("'@' 后应有候选窗");
    assert_eq!(
        t.candidates,
        vec!["xian@".to_string()],
        "敲 '@' 后应进入邮箱识别态(拼音候选消失);实测 {:?}",
        t.candidates
    );

    h.clear_backlog();
    assert!(h.key(keyval::BACKSPACE, 0), "Backspace 应被消费");
    let sigs = h.settle_default();
    let t = support::last_table(&sigs).expect("退格后应有候选窗");
    assert!(
        t.candidates.contains(&"先".to_string()),
        "删掉 '@' 后必须退出识别态并恢复拼音候选「先」;实际 {:?} —— \
         识别态是粘的(没有每次重判),用户将永远回不到拼音输入",
        t.candidates
    );
    assert_eq!(
        last_preedit(&sigs).as_deref(),
        Some("xian"),
        "退格后拼音串应回到 'xian'"
    );
}

/// The whole typed sequence must be *observed* to be one composition: the
/// preedit must accumulate exactly the characters typed, in order, and must
/// never lose one.
///
/// This is the generic form of the docs/HANDOFF.md lesson (「没有一条跑完整个
/// 序列」). It is deliberately redundant with `signal_payloads.rs`'s
/// per-keystroke test but runs on a *pinyin* string that takes the fallback path
/// at every step, so a bug that only affects partial keys still shows up.
///
/// Wrong-behaviour witness: any dropped keystroke (e.g. an early return for a
/// partial syllable) changes the observed preedit sequence → red.
#[test]
fn observed_preedit_sequence_equals_the_typed_sequence() {
    let h = &mut Harness::new("seq-preedit-accumulate");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    let seq = "zhongguo";
    let mut expected = String::new();
    let mut observed = Vec::new();
    for c in seq.chars() {
        expected.push(c);
        h.key(keyval::ascii(c), 0);
        let sigs = h.settle(Duration::from_millis(150), Duration::from_secs(3));
        if let Some(p) = last_preedit(&sigs) {
            observed.push(p);
        }
    }
    let expected_seq: Vec<String> = (1..=seq.len()).map(|i| seq[..i].to_string()).collect();
    assert_eq!(
        observed, expected_seq,
        "逐字符观察到的预编辑序列必须与输入序列完全一致(每个前缀各出现一次);\
         少一步说明有按键被丢弃,顺序不同说明状态被重置"
    );
}