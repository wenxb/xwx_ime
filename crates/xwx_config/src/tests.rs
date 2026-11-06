use super::*;

#[test]
fn default_language_is_english() {
    assert_eq!(Config::default().default_language, Language::En);
}

#[test]
fn missing_fields_fall_back_to_defaults() {
    let cfg = Config::from_toml("").expect("空文件是合法 TOML");
    assert_eq!(cfg, Config::default());
}

#[test]
fn explicit_chinese_round_trips() {
    let cfg = Config {
        default_language: Language::Zh,
    };
    let text = cfg.to_toml();
    assert!(text.contains("default_language = \"zh\""), "{text}");
    assert_eq!(Config::from_toml(&text).expect("自产 TOML 必须能读回"), cfg);
}

#[test]
fn invalid_language_is_a_parse_error() {
    let err = Config::from_toml("default_language = \"fr\"").expect_err("未知取值应报错");
    assert!(err.to_string().contains("fr") || err.to_string().contains("unknown variant"));
}

#[test]
fn bad_toml_degrades_to_default_instead_of_panicking() {
    let dir = std::env::temp_dir().join(format!("xwx-config-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let path = dir.join("config.toml");
    std::fs::write(&path, "default_language = ").expect("写坏配置");
    assert_eq!(Config::load_or_default(&path), Config::default());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_file_degrades_to_default() {
    let path = std::env::temp_dir().join("xwx-config-does-not-exist/config.toml");
    assert_eq!(Config::load_or_default(&path), Config::default());
}

#[test]
fn save_then_load_round_trips() {
    let dir = std::env::temp_dir().join(format!("xwx-config-save-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("xwx").join("config.toml");
    let cfg = Config {
        default_language: Language::Zh,
    };
    cfg.save(&path).expect("保存失败");
    assert_eq!(Config::load_or_default(&path), cfg);
    // 临时文件不能留下。
    assert!(!path.with_extension("toml.tmp").exists(), "残留了临时文件");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn config_path_prefers_xdg_config_home() {
    let p = config_path_in(Some("/x".into()), Some("/home/u".into()));
    assert_eq!(p, PathBuf::from("/x/xwx/config.toml"));
}

#[test]
fn config_path_falls_back_to_home() {
    let p = config_path_in(None, Some("/home/u".into()));
    assert_eq!(p, PathBuf::from("/home/u/.config/xwx/config.toml"));
}

#[test]
fn language_index_matches_all_order() {
    for (i, lang) in Language::ALL.iter().enumerate() {
        assert_eq!(lang.index(), i as u32, "index/ALL 顺序不一致: {lang:?}");
        assert_eq!(Language::from_index(i as u32), Some(*lang));
    }
    assert_eq!(Language::from_index(99), None);
}
