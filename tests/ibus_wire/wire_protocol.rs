//! IBus D-Bus 线协议：GVariant 签名、信号顺序、面板 RPC 与英文 M2 面板契约。

use std::time::Duration;

use crate::support::{
    keyval, last_commit, last_preedit, last_table, member_names, shared_fixture, Harness,
    LookupTable,
};

const NI_ORDER: &[&str] = &[
    "你", "泥", "尼", "逆", "匿", "腻", "拟", "妮", "倪", "霓", "溺", "昵",
];

fn compose_ni(h: &mut Harness) -> Vec<String> {
    h.focus_out().expect("FocusOut 复位");
    h.clear_backlog();
    h.type_ascii("ni");
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    last_table(&sigs).expect("应弹出候选窗").candidates
}

#[test]
fn gvariant_wire_shapes_and_preedit_lookup_geometry() {
    let mut h = Harness::new("wire-shapes");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");
    h.clear_backlog();

    assert!(h.key(keyval::ascii('n'), 0), "'n' 应开始组合");
    let p1 = h
        .wait_signal("UpdatePreeditText", Duration::from_secs(3))
        .expect("应收到 UpdatePreeditText");
    let strings = p1.strings();
    assert_eq!(strings.first().map(String::as_str), Some("IBusText"));
    assert_eq!(strings.get(1).map(String::as_str), Some("n"));
    assert_eq!(
        p1.structure_for_test().full_signature().as_str(),
        "(sa{sv}sv)"
    );
    assert_eq!(p1.as_u32_field(1), Some(1), "caret 应为 1");
    assert_eq!(p1.as_bool_field(2), Some(true), "visible 应为 true");
    assert_eq!(p1.as_u32_field(3), Some(0), "mode 应为 0");

    // 排干 'n' 附带的候选表，再敲 'i' 检查完整双信号
    h.settle_default();
    h.clear_backlog();

    assert!(h.key(keyval::ascii('i'), 0), "'i' 应继续组合");
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    let p2 = sigs
        .iter()
        .rev()
        .find(|s| s.member == "UpdatePreeditText")
        .expect("应收到第二个 UpdatePreeditText");
    assert_eq!(p2.as_text().as_deref(), Some("ni"));
    assert_eq!(p2.as_u32_field(1), Some(2), "caret 应为 2");
    assert_eq!(p2.as_bool_field(2), Some(true));
    assert_eq!(p2.as_u32_field(3), Some(0));

    let raw_table = sigs
        .iter()
        .rev()
        .find(|s| s.member == "UpdateLookupTable")
        .expect("应收到 UpdateLookupTable");
    assert_eq!(
        raw_table.structure_for_test().full_signature().as_str(),
        "(sa{sv}uubbiavav)"
    );
    let table = raw_table.as_lookup_table().expect("解析 IBusLookupTable");
    assert_eq!(table.page_size, 9);
    assert_eq!(table.cursor, 0);
    assert!(table.cursor_visible);
    assert!(!table.round);
    assert_eq!(table.orientation, 0);
    assert_eq!(table.labels, 0);
    assert_eq!(table.candidates, NI_ORDER[..9]);
}

#[test]
fn commit_precedes_hides_and_empty_composition_hides_ui() {
    let mut h = Harness::new("wire-commit-hide");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");
    h.clear_backlog();

    h.type_ascii("nihao");
    let pre = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    let table = last_table(&pre).expect("应弹出候选窗");
    assert!(
        table.candidates.iter().any(|c| c == "你好"),
        "对照条件：候选应含「你好」，实测 {:?}",
        table.candidates
    );
    h.clear_backlog();

    assert!(h.key(keyval::SPACE, 0), "空格应选词");
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(last_commit(&sigs).as_deref(), Some("你好"));
    let names = member_names(&sigs);
    let commit_pos = names.iter().position(|m| *m == "CommitText").unwrap();
    let hide_preedit_pos = names
        .iter()
        .position(|m| *m == "HidePreeditText")
        .expect("上屏后应发 HidePreeditText");
    assert!(
        names.contains(&"HideLookupTable"),
        "上屏后应发 HideLookupTable: {names:?}"
    );
    assert!(
        commit_pos < hide_preedit_pos,
        "CommitText 应先于 HidePreeditText: {names:?}"
    );

    // Escape 清空：发 Hide*，不得发空 UpdatePreeditText 或候选表
    h.type_ascii("ni");
    h.settle_default();
    h.clear_backlog();
    assert!(h.key(keyval::ESCAPE, 0));
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    let names = member_names(&sigs);
    assert!(names.contains(&"HidePreeditText"), "Esc 应发 HidePreeditText: {names:?}");
    assert!(names.contains(&"HideLookupTable"), "Esc 应发 HideLookupTable: {names:?}");
    assert!(last_preedit(&sigs).is_none(), "Esc 不得用空预编辑冒充收起");
    assert!(last_table(&sigs).is_none(), "Esc 后不得再发候选表");

    // Backspace 删空：同样发 Hide*，不得只更新为空串
    h.clear_backlog();
    h.type_ascii("n");
    h.settle_default();
    h.clear_backlog();
    assert!(h.key(keyval::BACKSPACE, 0));
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    let names = member_names(&sigs);
    assert!(names.contains(&"HidePreeditText"), "退格删空应发 HidePreeditText: {names:?}");
    assert!(names.contains(&"HideLookupTable"), "退格删空应发 HideLookupTable: {names:?}");
    assert!(last_preedit(&sigs).is_none(), "退格删空不得只发空预编辑");
}

#[test]
fn panel_paging_and_cursor_navigation() {
    let mut h = Harness::new("wire-panel-nav");
    h.start_engine(shared_fixture());
    assert_eq!(compose_ni(&mut h), NI_ORDER[..9]);

    let read_table = |h: &mut Harness, after: &str| -> LookupTable {
        let s = h
            .wait_signal("UpdateLookupTable", Duration::from_secs(3))
            .unwrap_or_else(|e| panic!("{after}: {e}"));
        last_table(std::slice::from_ref(&s)).unwrap_or_else(|| panic!("{after}: 候选表解析失败"))
    };

    // PageDown 翻到第 2 页，再按钳位在末页
    h.clear_backlog();
    h.page_down().expect("PageDown");
    let page2 = read_table(&mut h, "PageDown");
    assert_eq!(page2.candidates, NI_ORDER[9..]);
    assert_eq!(page2.cursor, 0);

    h.clear_backlog();
    h.page_down().expect("末页 PageDown");
    assert_eq!(read_table(&mut h, "末页 PageDown").candidates, NI_ORDER[9..]);

    // PageUp 回第 1 页，再按钳位在首页（不 wrap）
    h.clear_backlog();
    h.page_up().expect("PageUp");
    let page1 = read_table(&mut h, "PageUp");
    assert_eq!(page1.candidates, NI_ORDER[..9]);
    assert_eq!(page1.cursor, 0);

    h.clear_backlog();
    h.page_up().expect("首页 PageUp");
    assert_eq!(read_table(&mut h, "首页 PageUp").candidates, NI_ORDER[..9]);

    // CursorDown / CursorUp 页内移动与到顶钳位
    h.clear_backlog();
    h.cursor_down().expect("CursorDown");
    assert_eq!(read_table(&mut h, "CursorDown").cursor, 1);

    h.clear_backlog();
    h.cursor_down().expect("CursorDown x2");
    assert_eq!(read_table(&mut h, "CursorDown x2").cursor, 2);

    h.clear_backlog();
    h.cursor_up().expect("CursorUp");
    assert_eq!(read_table(&mut h, "CursorUp").cursor, 1);

    h.clear_backlog();
    h.cursor_up().expect("CursorUp");
    h.cursor_up().expect("CursorUp clamp");
    let top = last_table(&h.settle_response(Duration::from_millis(300), Duration::from_secs(5)))
        .expect("到顶应有候选表");
    assert_eq!(top.cursor, 0);
    assert_eq!(top.candidates, NI_ORDER[..9]);

    // 越过第 1 页边界：切换第 2 页且页内 cursor 同步
    h.clear_backlog();
    for _ in 0..12 {
        h.cursor_down().expect("CursorDown");
    }
    let crossed = last_table(&h.settle_response(Duration::from_millis(300), Duration::from_secs(5)))
        .expect("跨页应有候选表");
    assert_eq!(crossed.candidates, NI_ORDER[9..]);
    assert_eq!(crossed.cursor as usize, NI_ORDER.len() - 1 - 9);

    // PageUp 回首页首项，空格上屏与面板高亮一致
    h.clear_backlog();
    h.page_up().expect("PageUp");
    let back = read_table(&mut h, "回首页 PageUp");
    assert_eq!(back.candidates, NI_ORDER[..9]);
    assert_eq!(back.cursor, 0);
    h.clear_backlog();
    assert!(h.key(keyval::SPACE, 0));
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(last_commit(&sigs).as_deref(), Some(NI_ORDER[0]));
}

#[test]
fn candidate_clicked_maps_page_local_index_on_page_one_and_two() {
    // 第 1 页点击页内下标 1 -> "泥"
    {
        let mut h = Harness::new("wire-click-p1");
        h.start_engine(shared_fixture());
        assert_eq!(compose_ni(&mut h), NI_ORDER[..9]);
        h.clear_backlog();
        h.candidate_clicked(1).expect("CandidateClicked(1)");
        let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
        assert_eq!(last_commit(&sigs).as_deref(), Some(NI_ORDER[1]));
    }

    // 第 2 页点击页内下标 1 -> 全局下标 10 ("溺")，绝不能错成第 1 页的 "泥"
    let mut h = Harness::new("wire-click-p2");
    h.start_engine(shared_fixture());
    assert_eq!(compose_ni(&mut h), NI_ORDER[..9]);
    h.clear_backlog();
    h.page_down().expect("PageDown");
    let p2 = last_table(&h.settle_response(Duration::from_millis(300), Duration::from_secs(5)))
        .expect("第 2 页候选表");
    assert_eq!(p2.candidates, NI_ORDER[9..]);
    h.clear_backlog();

    h.candidate_clicked(1).expect("第 2 页 CandidateClicked(1)");
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(
        last_commit(&sigs).as_deref(),
        Some(NI_ORDER[10]),
        "第 2 页页内下标 1 必须映射到全局 10({})",
        NI_ORDER[10]
    );
    assert!(
        last_table(&sigs).is_some() || member_names(&sigs).contains(&"HideLookupTable"),
        "点选上屏后必须刷新或收起候选窗: {:?}",
        member_names(&sigs)
    );
}

#[test]
fn english_m2_prediction_panel_and_refresh_and_forward_wire_shapes() {
    let mut h = Harness::new("wire-m2").with_english_prediction();
    h.start_engine(shared_fixture());

    // CommitAndUpdate：选 "thank" 后发出空且不可见的 preedit + 下一词候选表
    h.type_ascii("thank");
    h.settle_default();
    h.clear_backlog();
    assert!(h.key(keyval::SPACE, 0), "空格应选词并弹出预测面板");
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(last_commit(&sigs).as_deref(), Some("thank"));
    let preedit = sigs
        .iter()
        .rev()
        .find(|s| s.member == "UpdatePreeditText")
        .expect("预测面板应发出 UpdatePreeditText");
    assert_eq!(preedit.as_text().as_deref(), Some(""));
    assert_eq!(
        preedit.as_bool_field(2),
        Some(false),
        "预测态空 preedit 必须不可见"
    );
    let table = last_table(&sigs).expect("预测态应弹出候选窗");
    assert_eq!(table.candidates.first().map(String::as_str), Some("you"));
    let names = member_names(&sigs);
    assert!(
        !names.contains(&"HideLookupTable"),
        "CommitAndUpdate 不得隐藏刚弹出的预测面板: {names:?}"
    );

    // RefreshAndForward：预测面板上按未映射字符 '+' 透传（false）、零 CommitText 并隐藏面板
    h.clear_backlog();
    assert!(!h.key(keyval::ascii('+'), 0), "未映射字符 '+' 必须透传");
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert!(
        last_commit(&sigs).is_none(),
        "RefreshAndForward 不得发出 CommitText"
    );
    assert!(
        member_names(&sigs).contains(&"HideLookupTable"),
        "RefreshAndForward 必须发 HideLookupTable 收起面板: {:?}",
        member_names(&sigs)
    );

    // 预测面板上按空格：透传（false）且零 CommitText（不产出双空格）
    h.focus_out().expect("FocusOut");
    h.type_ascii("thank");
    h.settle_default();
    h.clear_backlog();
    assert!(h.key(keyval::SPACE, 0));
    h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    h.clear_backlog();
    assert!(!h.key(keyval::SPACE, 0), "预测面板上的空格必须透传");
    let sigs = h.settle_default();
    assert!(
        last_commit(&sigs).is_none(),
        "空格透传不得额外发出 CommitText"
    );

    // 对照：Tab 接受预测词并带一个前导空格
    h.focus_out().expect("FocusOut");
    h.type_ascii("thank");
    h.settle_default();
    h.clear_backlog();
    assert!(h.key(keyval::SPACE, 0));
    h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    h.clear_backlog();
    assert!(h.key(keyval::TAB, 0), "Tab 应接受预测词");
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(last_commit(&sigs).as_deref(), Some(" you"));
}
