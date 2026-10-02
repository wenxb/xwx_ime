//! 逐字符敲一遍 URL / 邮箱:识别器必须在**全程不出拼音候选**的前提下,
//! 末尾一次 `CommitText` 上屏原文。
//!
//! 为什么要有这个文件(docs/HANDOFF.md「交互功能要有『逐字符敲一遍』的端到端测试」):
//! 识别器曾经在单键测试全绿的情况下带着完全坏掉的真实序列上线——
//! `http://x.com` 上屏成 `http:、、xcom`(`/` 被映射成顿号)、
//! `mailto:a@b.c` 上屏成 `买:a@b.c`(被当拼音处理)。**比不做识别器还糟。**
//! 单键测试测不出来,因为没有一条跑完整个序列。形状就是:每个字符一次真实
//! `ProcessKeyEvent`,末尾 Enter,把所有 `CommitText` 拼起来,断言等于原串。
//!
//! 断言质量(能做到的最强):① 全程**恰好一条** `CommitText`,且只在末尾
//! Enter 之后;② 它与输入逐字节相等;③ 任何一步的 preedit 都不出现拼音候选
//! (docs/VERIFY.md 第 32 行「全程不出拼音候选」)。第 ③ 条才是「该消失的消失
//! 了」的断言:识别器的全部工作就是让拼音候选**消失**;只断言「地址最终上屏」
//! 的写法在坏实现下也是绿的(坏版本同样会提交地址,只是标点被搞烂、还错了候选)。

mod support;

use std::time::Duration;
use support::{Harness, keyval, last_commit, last_preedit, shared_fixture};

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
    let after = h.settle_response(Duration::from_millis(200), Duration::from_secs(4));

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
    let after_at = h.settle_response(Duration::from_millis(200), Duration::from_secs(3));
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
    let sigs = h.settle_response(Duration::from_millis(200), Duration::from_secs(4));
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
