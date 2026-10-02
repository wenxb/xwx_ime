//! Signal payloads, checked as real GVariant on the wire.
//!
//! # Why this file exists
//!
//! The unit tests in `crates/xwx_ibus/src/ibus_proto.rs` check the *shape* of
//! the serialized objects (`gvariant_signatures_match_ibus_1529`) and that a
//! freshly built `IBusText` round-trips. They never check what the engine
//! actually **emits** for a real keystroke sequence: whether the candidate the
//! user sees is the word the engine claims it ranked first, whether the preedit
//! text is the pinyin string, whether `CommitText` carries the Chinese text.
//!
//! docs/VERIFY.md (§ intro) lists exactly this gap:
//! > `UpdatePreeditText` / `UpdateLookupTable` / `CommitText` 三个信号
//!
//! # Assertion quality
//!
//! Payload assertions are the strongest kind here, because they are
//! **content**-pinned rather than shape-pinned: a signal that still arrives with
//! the wrong word fails. Each test names the breakage it detects.
//!
//! The parsing is done with zvariant (already a dependency), not by scanning
//! `dbus-monitor` text, so the type-name literals `"IBusText"` /
//! `"IBusAttrList"` cannot be mistaken for the payload (a real trap: they are
//! decoded, not scanned).

mod support;

use support::{keyval, last_commit, last_preedit, last_table, lookup_tables, shared_fixture, Harness};

/// The fixture's exact candidate order for `ni`, highest frequency first.
///
/// The engine has no language model here (`XWX_LM_PATH` points at a missing
/// file → empty bigram), so ranking is pure frequency
/// (`wanxiang.rs::score_word` early-returns a constant on empty context —
/// docs/HANDOFF.md §1⑥). That is what makes these expectations exact.
const NI_ORDER: &[&str] = &[
    "你", "泥", "尼", "逆", "匿", "腻", "拟", "妮", "倪", "霓", "溺", "昵",
];

/// `UpdatePreeditText` must carry the pinyin string and a caret that follows it.
///
/// Wrong-behaviour witnesses:
/// - sending the *translated* text (candidates) instead of the pinyin → the
///   `"ni"` assertion goes red;
/// - hard-coding `cursor_pos` to 0 instead of `u.caret` (`engine.rs:95`) → the
///   cursor assertion goes red; that is the B1 "insert in the middle" feature
///   (`session.rs:980`, docs/VERIFY.md row 22) which is invisible otherwise.
#[test]
fn update_preedit_text_carries_pinyin_and_caret() {
    let h = &mut Harness::new("preedit");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");
    h.clear_backlog();

    assert!(h.key(keyval::ascii('n'), 0), "'n' 应开始组合");
    let s1 = h.wait_signal("UpdatePreeditText", std::time::Duration::from_secs(3));
    let s1 = s1.expect("应收到 UpdatePreeditText");
    assert_eq!(
        s1.as_text().as_deref(),
        Some("n"),
        "预编辑文本必须是拼音串 'n';实测 {:?}",
        s1.strings()
    );
    // Field indices are of the BODY `(v u b u)` (`UpdatePreeditText(v, u, b, u)`,
    // engine.rs:319): 1 = cursor_pos, 2 = visible, 3 = mode. Field 0 is the
    // variant holding the IBusText, which `as_text` unwraps.
    // Assert the wire value rather than trusting the session.
    let fields = support::Signal::as_u32_field(&s1, 1);
    assert_eq!(fields, Some(1), "caret 应为 1(＝'n' 的长度);实测 {:?}", s1.strings());

    // 'i' → preedit "ni"
    assert!(h.key(keyval::ascii('i'), 0));
    let s2 = h
        .wait_signal("UpdatePreeditText", std::time::Duration::from_secs(3))
        .expect("应收到第二个 UpdatePreeditText");
    assert_eq!(s2.as_text().as_deref(), Some("ni"), "预编辑应增长为 'ni'");
    assert_eq!(support::Signal::as_u32_field(&s2, 1), Some(2), "caret 应为 2");

    // The same signal also carries visible=true and mode=0 — the engine always
    // shows the preedit while composing (`engine.rs:96`).
    assert_eq!(
        support::Signal::as_bool_field(&s2, 2),
        Some(true),
        "visible 必须为 true(否则用户看不到自己打的拼音)"
    );
    assert_eq!(support::Signal::as_u32_field(&s2, 3), Some(0), "mode 为 0");
}

/// `UpdateLookupTable` must contain the expected candidates, in order, for the
/// current page, with the page-local `cursor` and the page geometry.
///
/// 线上坐标系(codex 审查 D5 定稿):表 = **当前页**切片(≤ page_size),
/// `cursor` = **页内**下标;面板点击回传的也是页内下标,会话用
/// `page_start + index` 映射回全局。旧缺陷不是这个坐标系本身,而是会话
/// 少做了那条映射(见 `tests/paging.rs::candidate_clicked_on_page_two_...`)。
/// 不采用「整表 + 绝对 cursor」的原因:实测整表序列化 500 条 = 7.5ms/键。
///
/// Wrong-behaviour witnesses:
/// - `page_size` hard-coded to something other than `DEFAULT_PAGE_SIZE` (9) →
///   the page_size assertion goes red;
/// - candidates emitted unsorted → the ordered-equality assertion goes red;
/// - `cursor` emitted as the global index (> page_size) → the cursor assertion
///   goes red (and the panel would highlight the wrong row);
/// - labels filled in → the `labels == 0` assertion goes red (the panel is
///   supposed to auto-number 1..9).
#[test]
fn update_lookup_table_carries_ordered_candidates_and_page_geometry() {
    let h = &mut Harness::new("lookup-table");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");
    h.clear_backlog();

    h.type_ascii("ni");
    let sigs = h.settle_default();
    let table = last_table(&sigs).expect("应收到 UpdateLookupTable");

    assert_eq!(
        table.page_size, 9,
        "每页候选数应为 DEFAULT_PAGE_SIZE=9(session.rs:20)"
    );
    assert_eq!(table.cursor, 0, "初始高亮在第 0 条(页内下标)");
    assert!(table.cursor_visible, "cursor_visible 应为 true");
    assert!(
        !table.round,
        "round(循环翻页)应为 false:与会话「末页/首页钳位、不 wrap」的导航一致"
    );
    assert_eq!(table.orientation, 0, "orientation 应为 IBUS_ORIENTATION_HORIZONTAL");
    assert_eq!(table.labels, 0, "labels 应为空,交给面板自动编号");
    assert_eq!(
        table.candidates,
        NI_ORDER[..9],
        "第 1 页候选必须严格按词频降序;实测 {:?}",
        table.candidates
    );

    // Page 2 of the same composition is driven by PageDown, so it is asserted in
    // paging.rs. Here we only pin the page geometry that makes the cursor field
    // meaningful: the cursor is an in-page index, so it must stay below
    // `page_size` and inside the emitted slice.
    assert!(
        (table.cursor as usize) < table.page_size as usize,
        "页内 cursor({})必须 < page_size({})",
        table.cursor,
        table.page_size
    );
    assert!(
        (table.cursor as usize) < table.candidates.len(),
        "页内 cursor 必须指向表内一条"
    );

    // Page 2 of the same composition is driven by PageDown, so it is asserted in
    // paging.rs. Here we only pin the geometry that makes `cursor` meaningful:
    // the table carries the entire list, and `page_size` is the panel's window
    // width over it (the panel computes `page = cursor / page_size`).
    assert!(
        table.candidates.len() >= table.page_size as usize,
        "整表不该比一页还窄(page_size={} 只描述面板一页);实测 {} 条",
        table.page_size,
        table.candidates.len()
    );
}

/// `CommitText` must carry the Chinese word, and `HidePreeditText` /
/// `HideLookupTable` must follow it (the window closes).
///
/// This is docs/VERIFY.md row 1 (`nihao` + space → 你好) end to end.
///
/// Wrong-behaviour witnesses:
/// - committing the pinyin (`"nihao"`) instead of the candidate → the text
///   assertion goes red;
/// - committing the *first* candidate when selection is elsewhere → red (the
///   paging/selection file covers the non-first case);
/// - forgetting `hide_ui` after a commit (`engine.rs:79`) → the
///   HidePreeditText/HideLookupTable assertions go red and the candidate window
///   would stay on screen after the text is already in the app.
#[test]
fn commit_text_carries_the_word_and_closes_the_window() {
    let h = &mut Harness::new("commit");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");
    h.clear_backlog();

    // Precondition (non-vacuous): the window must be open before space.
    h.type_ascii("nihao");
    let pre = h.settle_default();
    let table = last_table(&pre).expect("打字过程中应弹出候选窗");
    assert!(
        table.candidates.contains(&"你好".to_string()),
        "对照条件:'nihao' 的候选里必须有「你好」,否则本测试证明不了上屏内容。实测 {:?}",
        table.candidates
    );
    h.clear_backlog();

    assert!(h.key(keyval::SPACE, 0), "空格应被消费(选词)");
    let sigs = h.settle_default();

    assert_eq!(
        last_commit(&sigs).as_deref(),
        Some("你好"),
        "CommitText 必须携带「你好」——不是拼音 'nihao'、也不是别的候选。\
         实际信号: {:?}",
        support::member_names(&sigs)
    );

    let names = support::member_names(&sigs);
    assert!(
        names.contains(&"HidePreeditText"),
        "上屏后必须收起预编辑区。实际: {names:?}"
    );
    assert!(
        names.contains(&"HideLookupTable"),
        "上屏后必须收起候选窗。实际: {names:?}"
    );
    // Ordering: the commit comes before the hides (engine.rs:70-80).
    let commit_immediately = names.iter().position(|m| *m == "CommitText").unwrap();
    let hide_at = names.iter().position(|m| *m == "HidePreeditText").unwrap();
    assert!(
        commit_immediately < hide_at,
        "CommitText 必须先于 HidePreeditText 发出(先给应用文字,再收窗)"
    );
}

/// The IBus serialization shape survives the wire, not just construction.
///
/// The unit test at `ibus_proto.rs:400` checks `full_signature()` on a locally
/// built value. This checks the same contract **after** zbus marshalled it into
/// a `v` and the bus decoded it back — the step where a wrong field order or a
/// missing `a{sv}` would actually break ibus-daemon.
///
/// Wrong-behaviour witnesses: dropping the `a{sv}` props slot, swapping
/// `text`/`attr_list`, or emitting the lookup table as `av` instead of the
/// 9-field struct makes the type-name / arity assertions red.
#[test]
fn on_the_wire_gvariant_shape_matches_ibus_1529() {
    let h = &mut Harness::new("gvariant-shape");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");
    h.clear_backlog();

    // IBusText: ("IBusText", a{sv}, text, v) — the *first* field is the type
    // name, the *second* is text; nothing else may appear.
    assert!(h.key(keyval::ascii('n'), 0));
    let p = h
        .wait_signal("UpdatePreeditText", std::time::Duration::from_secs(3))
        .expect("UpdatePreeditText");
    let strings = p.strings();
    assert_eq!(
        strings.first().map(String::as_str),
        Some("IBusText"),
        "预编辑负载的第 1 个字段必须是类型名 \"IBusText\"(ibus-daemon 靠它分派 GType)"
    );
    assert_eq!(
        strings.get(1).map(String::as_str),
        Some("n"),
        "第 2 个字段必须是文本本身;实测 {strings:?}"
    );
    let st = p.structure_for_test();
    assert_eq!(
        st.full_signature().as_str(),
        "(sa{sv}sv)",
        "IBusText 在总线上必须序列化成 (sa{{sv}}sv)"
    );

    // IBusLookupTable: ("IBusLookupTable", a{sv}, u,u,b,b,i, av, av)
    h.type_ascii("i");
    let sigs = h.settle_default();
    let t = last_table(&sigs).expect("UpdateLookupTable");
    assert_eq!(t.labels, 0, "labels 必须是空 av(若被填成候选文本,面板会重复编号)");
    let raw = sigs
        .iter()
        .find(|s| s.member == "UpdateLookupTable")
        .expect("UpdateLookupTable");
    assert_eq!(
        raw.structure_for_test().full_signature().as_str(),
        "(sa{sv}uubbiavav)",
        "IBusLookupTable 在总线上必须序列化成 (sa{{sv}}uubbiavav),字段顺序逐字节对齐 ibus 1.5.29"
    );
}

/// The preedit must be *hidden* — not merely updated to empty — when the input
/// is cleared by Escape or Backspace.
///
/// This is the "assert what must DISAPPEAR" rule from docs/HANDOFF.md applied to
/// signals: `refresh_ui` (`engine.rs:85`) chooses between
/// `hide_ui` and `update_preedit_text`, and an implementation that always sent
/// `UpdatePreeditText("")` would look identical to a user but is a different
/// wire contract (the panel would keep an empty preedit visible).
///
/// Wrong-behaviour witness: deleting the
/// `if u.preedit.is_empty() && u.candidates.is_empty()` early return in
/// `refresh_ui` makes `HidePreeditText` disappear from the batch.
#[test]
fn empty_composition_hides_the_ui_instead_of_sending_empty_text() {
    let h = &mut Harness::new("hide-vs-empty");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    // Escape path (session.rs:507-513).
    h.type_ascii("ni");
    h.settle_default();
    h.clear_backlog();
    assert!(h.key(keyval::ESCAPE, 0), "组合中 Esc 应被消费(清空输入)");
    let sigs = h.settle_default();
    let names = support::member_names(&sigs);
    assert!(
        names.contains(&"HidePreeditText"),
        "Esc 清空后必须发 HidePreeditText。实际: {names:?}"
    );
    assert!(
        names.contains(&"HideLookupTable"),
        "Esc 清空后必须发 HideLookupTable。实际: {names:?}"
    );
    assert!(
        !sigs.iter().any(|s| s.member == "UpdatePreeditText" && s.as_text().as_deref() == Some("")),
        "不得用 UpdatePreeditText(\"\") 冒充收起:那是另一个契约"
    );
    assert!(
        last_table(&sigs).is_none(),
        "清空后不得再发候选表(候选必须整批消失)"
    );

    // Backspace-to-empty path (session.rs:531).
    h.clear_backlog();
    h.type_ascii("n");
    h.settle_default();
    h.clear_backlog();
    assert!(h.key(keyval::BACKSPACE, 0), "有内容时退格应被消费");
    let sigs = h.settle_default();
    let names = support::member_names(&sigs);
    assert!(
        names.contains(&"HidePreeditText"),
        "退格删空后必须发 HidePreeditText。实际: {names:?}"
    );
    assert!(
        names.contains(&"HideLookupTable"),
        "退格删空后必须发 HideLookupTable。实际: {names:?}"
    );
    assert!(
        !last_preedit(&sigs).is_some_and(|t| t.is_empty() && !names.contains(&"HidePreeditText")),
        "预编辑不得只被置空而不收起"
    );
}

/// A pinyin string that matches nothing must still produce a visible echo
/// candidate rather than a silent empty window.
///
/// docs/HANDOFF.md §2「回声候选的语义」: echo appears **only** when the whole
/// field has no candidate at all. This is the wire-level proof, and it is
/// doubly useful because it is the exact candidate the tile hint in
/// `engine.rs` relies on for the "no candidate" UX.
///
/// Wrong-behaviour witness: if the fallback/echo path stops merging, `zzzz`
/// yields an empty candidate list and `UpdateLookupTable` is replaced by
/// `HideLookupTable` (a window that flashes and vanishes) — both assertions go
/// red.
#[test]
fn untypable_input_yields_an_echo_candidate_not_an_empty_window() {
    let h = &mut Harness::new("echo");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");
    h.clear_backlog();

    // 'q' alone has no fixture key, but sub-prefix fallback still finds
    // nothing; use a string that cannot be a syllable chain prefix either.
    h.type_ascii("qqqq");
    let sigs = h.settle_default();
    let table = last_table(&sigs).expect(
        "无法切分的输入也必须弹出候选窗(回声候选),不得只有 HideLookupTable。\
         实际: ",
    );
    assert!(
        !table.candidates.is_empty(),
        "候选窗不得为空"
    );
    assert_eq!(
        table.candidates,
        vec!["qqqq".to_string()],
        "全场无候选时应只有一条「原样上屏」的回声候选(= 用户敲的串)"
    );

    // And space commits exactly that raw string (commit_raw semantics: not
    // learned, not coined — but it must still reach the app).
    h.clear_backlog();
    assert!(h.key(keyval::SPACE, 0));
    let sigs = h.settle_default();
    assert_eq!(
        last_commit(&sigs).as_deref(),
        Some("qqqq"),
        "回声候选上屏的必须是原串"
    );
}

/// Every keystroke of a composition must refresh **both** the preedit and the
/// lookup table, with matching text.
///
/// docs/HANDOFF.md「交互功能要有『逐字符敲一遍』的端到端测试」: single-step
/// assertions pass while the real sequence is broken. This walks `nihao` one
/// character at a time and asserts the preedit *grows character by character*
/// (so a dropped keystroke cannot hide) and that a lookup table accompanies
/// every step (so a "candidate window flickers off mid-word" regression is
/// caught — the reason `fallback_candidates` exists, `session.rs:477-478`).
///
/// Wrong-behaviour witness: swallowing every second keystroke (e.g. by
/// resetting the session on each refresh) makes the preedit sequence
/// `n, ni, nih, ...` collapse; skipping the fallback for partial input makes
/// the per-step lookup table assertion red.
#[test]
fn per_keystroke_preedit_grows_and_lookup_table_follows_every_step() {
    let h = &mut Harness::new("per-keystroke");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    let mut expected = String::new();
    let mut tables = 0usize;
    for c in "nihao".chars() {
        expected.push(c);
        assert!(h.key(keyval::ascii(c), 0), "'{c}' 应被消费(进拼音串)");
        let sigs = h.settle_response(std::time::Duration::from_millis(200), std::time::Duration::from_secs(4));
        assert_eq!(
            last_preedit(&sigs).as_deref(),
            Some(expected.as_str()),
            "敲下 '{c}' 后预编辑必须逐字符增长为 {expected:?};实测 {:?}",
            support::member_names(&sigs)
        );
        let t = last_table(&sigs).unwrap_or_else(|| {
            panic!(
                "每一步都必须伴随 UpdateLookupTable(否则候选窗在打字中途闪一下就没)。\
                 步 {expected:?} 实际: {:?}",
                support::member_names(&sigs)
            )
        });
        tables += 1;
        // The window must never claim to show candidates while offering none.
        assert!(
            !t.candidates.is_empty(),
            "步 {expected:?}: 候选窗开着却没有候选"
        );
    }
    assert_eq!(tables, 5, "五步各一次候选表刷新");
    assert!(
        lookup_tables(&h.backlog()).len() >= 5,
        "累计应至少 5 次候选表刷新(留档观测)"
    );
}