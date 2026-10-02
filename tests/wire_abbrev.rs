//! 简拼/混合简拼走到 D-Bus 线上:会话层测过的召回,在**线上**也要成立
//! (候选表 payload、隔音符号边界、缺缩写表时的降级、preedit 序列)。
//!
//! 断言质量:不只看「有候选」,而是逐条比对 `UpdateLookupTable` 里的词面
//! ——候选表是用户**看得见**的东西,拼音对但词面错同样是 bug。

mod support;

use std::time::Duration;
use support::{Harness, keyval, last_commit, last_preedit, shared_fixture};

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
/// `src/main.rs:104-110` warns and keeps going. This is the documented
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
        let sigs = h.settle_response(Duration::from_millis(150), Duration::from_secs(3));
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
