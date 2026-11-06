//! 默认语言配置走到 D-Bus 线上：`config.toml` → 引擎 → 会话初始模式。
//!
//! 面板写的文件与引擎读的文件必须是同一份、同一套解析规则（都在
//! `xwx_config`）。单元测试钉住解析，这一层钉住**接线**：
//! 文件里写 `en`，敲字母就该原样进应用（`ProcessKeyEvent` 返回 false，
//! 没有 `UpdatePreeditText`）；写 `zh` 或没有文件，敲字母该出候选。

mod support;

use std::path::Path;

use support::{keyval, last_preedit, last_table, Harness};

/// 在 harness 的临时根目录下写 `config.toml`。
///
/// `Harness` 已把 `XDG_CONFIG_HOME` 指到 `root/home/.config`，所以这里
/// 写的就是被测引擎会读到的那个文件。
fn write_config(h: &Harness, body: &str) {
    let dir = h.root.join("home/.config/xwx");
    std::fs::create_dir_all(&dir).expect("建配置目录");
    std::fs::write(dir.join("config.toml"), body).expect("写配置");
}

#[test]
fn english_default_passes_letters_through() {
    let mut h = Harness::new("config-default-en");
    write_config(&h, "default_language = \"en\"\n");
    h.start_engine(support::shared_fixture());

    let consumed = h.key(keyval::ascii('n'), 0);
    assert!(
        !consumed,
        "默认英文时字母必须透传给应用，实际被输入法吃掉了"
    );

    let signals = h.settle_default();
    assert!(
        last_table(&signals).is_none(),
        "默认英文不该弹候选窗"
    );
    let preedit = last_preedit(&signals);
    assert!(
        preedit.is_none() || preedit.as_deref() == Some(""),
        "默认英文不该有预编辑串，实际 {preedit:?}"
    );
}

#[test]
fn chinese_default_composes_letters() {
    let mut h = Harness::new("config-default-zh");
    write_config(&h, "default_language = \"zh\"\n");
    h.start_engine(support::shared_fixture());

    let consumed = h.key(keyval::ascii('n'), 0);
    assert!(consumed, "默认中文时字母应被输入法消费");

    let signals = h.settle_default();
    let table = last_table(&signals).expect("默认中文应有候选窗");
    assert!(!table.candidates.is_empty(), "候选表不该为空");
}

#[test]
fn missing_config_starts_in_english() {
    // 去掉预置配置：`Config::default()` 的默认语言是英文。
    // 这条同时证明「没有配置」不等于「从中文起」。
    let mut h = Harness::new("config-absent").without_config();
    h.start_engine(support::shared_fixture());

    let consumed = h.key(keyval::ascii('n'), 0);
    assert!(!consumed, "无配置文件时默认英文，字母应透传");
}

#[test]
fn broken_config_does_not_break_startup() {
    // 语法错误：引擎必须照常起来并按默认值（英文）工作，不能崩。
    let mut h = Harness::new("config-broken");
    write_config(&h, "default_language = \n");
    h.start_engine(support::shared_fixture());

    let consumed = h.key(keyval::ascii('n'), 0);
    assert!(!consumed, "配置损坏时退回默认英文，字母应透传");
}

/// 面板那条路径写出的字节，引擎必须读得懂。
///
/// 两条路径共用 `xwx_config`，这条测试防的是「有人手改了其中一个的
/// 字段名/字面量」——那种改动编译照样过，行为却对不上。
#[test]
fn panel_written_file_matches_engine_expectations() {
    let cfg = xwx_config::Config {
        default_language: xwx_config::Language::Zh,
    };
    let text = cfg.to_toml();
    assert_eq!(
        text.trim(),
        "default_language = \"zh\"",
        "配置写法变了，面板与引擎的契约要同步更新"
    );
    assert_eq!(
        xwx_config::Config::from_toml(&text).expect("引擎必须能读回面板写的文件"),
        cfg
    );

    // 引擎侧读的就是这个文件名，路径由同一函数给出。
    let path = xwx_config::config_path_in(None, Some("/home/u".into()));
    assert_eq!(path, Path::new("/home/u/.config/xwx/config.toml"));
}
