//! 启动配置接线、缺数据/坏数据降级与退出信号落盘持久化。

use std::time::Duration;

use crate::support::{
    english_fixture, keyval, last_commit, last_preedit, last_table, shared_fixture, Harness,
};

fn write_config(h: &Harness, body: &str) {
    let dir = h.root.join("home/.config/xwx");
    std::fs::create_dir_all(&dir).expect("建配置目录");
    std::fs::write(dir.join("config.toml"), body).expect("写配置");
}

#[test]
fn config_default_language_zh_and_en() {
    // zh：启动即中文组合
    let mut zh = Harness::new("cfg-zh");
    write_config(&zh, "default_language = \"zh\"\n");
    zh.start_engine(shared_fixture());
    assert!(zh.key(keyval::ascii('n'), 0), "default_language=zh 应消费字母");
    let sigs = zh.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert!(
        last_table(&sigs).is_some_and(|t| !t.candidates.is_empty()),
        "默认中文应弹出候选窗"
    );

    // en：启动即英文透传，无候选窗和预编辑
    let mut en = Harness::new("cfg-en");
    write_config(&en, "default_language = \"en\"\n");
    en.start_engine(shared_fixture());
    assert!(!en.key(keyval::ascii('n'), 0), "default_language=en 应透传字母");
    let sigs = en.settle_default();
    assert!(last_table(&sigs).is_none(), "默认英文不该弹候选窗");
    assert!(
        last_preedit(&sigs).as_deref().unwrap_or("").is_empty(),
        "默认英文不该有预编辑串"
    );
}

#[test]
fn config_missing_or_malformed_starts_in_english() {
    let mut absent = Harness::new("cfg-absent").without_config();
    absent.start_engine(shared_fixture());
    assert!(
        !absent.key(keyval::ascii('n'), 0),
        "无配置文件时应按默认英文透传"
    );

    let mut broken = Harness::new("cfg-broken");
    write_config(&broken, "default_language = \n");
    broken.start_engine(shared_fixture());
    assert!(
        !broken.key(keyval::ascii('n'), 0),
        "配置语法损坏时应正常启动并回退英文透传"
    );
}

#[test]
fn missing_data_and_missing_abbrev_degrade_gracefully() {
    // (a) 全部数据文件缺失：进程正常启动并只提供回声候选
    let mut no_data = Harness::new("degrade-no-data");
    no_data.start_engine_with_no_data();
    no_data.focus_out().expect("FocusOut");
    assert!(no_data.toggle_ascii());
    assert!(!no_data.key(keyval::ascii('n'), 0));
    assert!(no_data.toggle_ascii());
    assert!(no_data.key(keyval::ascii('n'), 0));
    no_data.reset_session().expect("Reset");
    no_data.clear_backlog();
    no_data.type_ascii("ni");
    let sigs = no_data.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    let table = last_table(&sigs).expect("空词库应弹回声候选窗");
    assert_eq!(table.candidates, vec!["ni".to_string()]);

    // (b) 缺失 abbrev.bin：简拼退化为回声，精确全拼不受影响
    let mut no_abbrev = Harness::new("degrade-no-abbrev");
    no_abbrev.start_engine_without_abbrev(shared_fixture());
    no_abbrev.focus_out().expect("FocusOut");
    no_abbrev.clear_backlog();
    no_abbrev.type_ascii("ni");
    let sigs = no_abbrev.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(
        last_table(&sigs).expect("精确查询应弹窗").candidates,
        ["你", "泥", "尼", "逆", "匿", "腻", "拟", "妮", "倪"]
    );
    no_abbrev.reset_session().expect("Reset");
    no_abbrev.clear_backlog();
    no_abbrev.type_ascii("zgr");
    let sigs = no_abbrev.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(
        last_table(&sigs).expect("缺简拼索引应退化为回声").candidates,
        vec!["zgr".to_string()]
    );
}

#[test]
fn english_switches_and_corrupt_bigram_degrade_cleanly() {
    // XWX_ENGLISH=0：即使词库路径合法也不开启英文候选
    let mut disabled = Harness::new("en-switch-off")
        .with_env("XWX_ENGLISH", "0")
        .with_env("XWX_ENGLISH_DICT_PATH", english_fixture("words.bin"));
    disabled.start_engine(shared_fixture());
    disabled.type_ascii("pyth");
    let sigs = disabled.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    let cands = last_table(&sigs).map(|t| t.candidates).unwrap_or_default();
    assert!(!cands.iter().any(|c| c == "Python"), "关闭总开关不应有英文候选: {cands:?}");

    // XWX_ENGLISH_PREDICT=0：选词照常，不弹预测面板
    let mut no_predict = Harness::new("en-predict-off")
        .with_env("XWX_ENGLISH", "1")
        .with_env("XWX_ENGLISH_DICT_PATH", english_fixture("words.bin"))
        .with_env("XWX_ENGLISH_BIGRAM_PATH", english_fixture("bigram.bin"))
        .with_env("XWX_ENGLISH_PREDICT", "0");
    no_predict.start_engine(shared_fixture());
    no_predict.type_ascii("thank");
    no_predict.settle_default();
    no_predict.clear_backlog();
    assert!(no_predict.key(keyval::SPACE, 0));
    let sigs = no_predict.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(last_commit(&sigs).as_deref(), Some("thank"));
    assert!(last_table(&sigs).is_none(), "关闭预测后不得弹出下一词面板");

    // 损坏的 XWX_ENGLISH_BIGRAM_PATH：仅降级静态二元预测，英文前缀补全照常
    let mut bad_bigram = Harness::new("en-bad-bigram")
        .with_env("XWX_ENGLISH", "1")
        .with_env("XWX_ENGLISH_DICT_PATH", english_fixture("words.bin"))
        .with_env("XWX_ENGLISH_BIGRAM_PATH", english_fixture("words.bin"))
        .with_env("XWX_ENGLISH_PREDICT", "1");
    bad_bigram.start_engine(shared_fixture());
    bad_bigram.type_ascii("pyth");
    let sigs = bad_bigram.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    let cands = last_table(&sigs).map(|t| t.candidates).unwrap_or_default();
    assert!(
        cands.iter().any(|c| c == "Python"),
        "坏二元表不得破坏英文补全: {cands:?}"
    );
    bad_bigram.reset_session().expect("Reset");
    bad_bigram.clear_backlog();
    bad_bigram.type_ascii("thank");
    bad_bigram.settle_default();
    bad_bigram.clear_backlog();
    assert!(bad_bigram.key(keyval::SPACE, 0));
    let sigs = bad_bigram.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(last_commit(&sigs).as_deref(), Some("thank"));
    assert!(last_table(&sigs).is_none(), "坏二元表不应弹出静态预测面板");
}

#[test]
fn missing_or_corrupt_english_dict_degrades_with_english_enabled() {
    // 显式开启 XWX_ENGLISH=1，确保真正执行到 EnglishDict::open 降级路径
    let mut missing = Harness::new("en-missing-dict")
        .with_env("XWX_ENGLISH", "1")
        .with_env("XWX_ENGLISH_DICT_PATH", "/nonexistent/english.bin");
    missing.start_engine(shared_fixture());
    missing.type_ascii("pyth");
    let sigs = missing.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    let cands = last_table(&sigs).map(|t| t.candidates).unwrap_or_default();
    assert!(!cands.iter().any(|c| c == "Python"), "缺英文词库不应召回 Python: {cands:?}");
    missing.reset_session().expect("Reset");
    missing.clear_backlog();
    missing.type_ascii("nihao");
    let sigs = missing.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert!(
        last_table(&sigs).is_some_and(|t| t.candidates.iter().any(|c| c == "你好")),
        "英文词库缺失不得影响中文候选"
    );

    let corrupt_harness = Harness::new("en-corrupt-dict");
    let bad_path = corrupt_harness.root.join("corrupt-english.bin");
    std::fs::write(&bad_path, b"not an english.bin at all").expect("写损坏词库");
    let mut corrupt = corrupt_harness
        .with_env("XWX_ENGLISH", "1")
        .with_env("XWX_ENGLISH_DICT_PATH", &bad_path);
    corrupt.start_engine(shared_fixture());
    corrupt.type_ascii("pyth");
    let sigs = corrupt.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    let cands = last_table(&sigs).map(|t| t.candidates).unwrap_or_default();
    assert!(!cands.iter().any(|c| c == "Python"), "损坏词库不应召回 Python: {cands:?}");
    corrupt.reset_session().expect("Reset");
    corrupt.clear_backlog();
    corrupt.type_ascii("nihao");
    let sigs = corrupt.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert!(
        last_table(&sigs).is_some_and(|t| t.candidates.iter().any(|c| c == "你好")),
        "损坏英文词库不得影响中文候选"
    );
}

#[test]
fn sigint_flushes_chinese_and_sigterm_flushes_english_user_lm() {
    // SIGINT：中文学习尾巴落盘到隔离目录 cfg/user_bigram.txt
    let mut zh = Harness::new("persist-sigint-zh");
    zh.start_engine(shared_fixture());
    zh.focus_out().expect("FocusOut");
    zh.type_ascii("nihao");
    zh.settle_default();
    zh.clear_backlog();
    assert!(zh.key(keyval::SPACE, 0));
    let sigs = zh.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(last_commit(&sigs).as_deref(), Some("你好"));
    zh.shutdown_engine_with("-INT");

    let zh_file = zh.root.join("cfg/user_bigram.txt");
    let zh_text = std::fs::read_to_string(&zh_file)
        .unwrap_or_else(|e| panic!("SIGINT 后中文用户模型应落盘 {}: {e}", zh_file.display()));
    assert!(
        zh_text.contains("你好"),
        "SIGINT 落盘内容应包含「你好」:\n{zh_text}"
    );

    // SIGTERM：英文学习尾巴落盘到隔离目录 cfg/user_english.txt
    let mut en = Harness::new("persist-sigterm-en")
        .with_env("XWX_ENGLISH", "1")
        .with_env("XWX_ENGLISH_DICT_PATH", english_fixture("words.bin"));
    en.start_engine(shared_fixture());
    en.type_ascii("hello");
    let sigs = en.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert!(last_table(&sigs).is_some_and(|t| t.candidates.iter().any(|c| c == "hello")));
    en.clear_backlog();
    assert!(en.key(keyval::SPACE, 0));
    let sigs = en.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(last_commit(&sigs).as_deref(), Some("hello"));
    en.shutdown_engine_with("-TERM");

    let en_file = en.root.join("cfg/user_english.txt");
    let en_text = std::fs::read_to_string(&en_file)
        .unwrap_or_else(|e| panic!("SIGTERM 后英文用户模型应落盘 {}: {e}", en_file.display()));
    assert!(
        en_text.contains("hello"),
        "SIGTERM 落盘内容应包含 hello:\n{en_text}"
    );
}
