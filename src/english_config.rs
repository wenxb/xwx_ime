//! 英文通道的**配置解析与路径解析**（生产入口专用）。
//!
//! 纯库（`xwx_engine`）不读环境变量：所有 `XWX_ENGLISH*` 的解析都在这里，
//! 由 `src/main.rs` 显式调用并把结果装进引擎。测试与探针走显式的
//! `with_english(...)`，不靠进程环境。
//!
//! 配置表见 `docs/english-input/01-data.md` 第 7 节。
//!
//! TODO(S5): 生产装配接上 `with_english` 后删除下面这条临时豁免——
//! 阶段性接口「写了还没接线」不是给它永久开绿灯的理由。
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// 总开关；`0` 时不打开英文词库，也不打开英文用户文件。
pub(crate) const ENGLISH_ENV: &str = "XWX_ENGLISH";
/// 只读英文词库路径覆盖。
pub(crate) const ENGLISH_DICT_ENV: &str = "XWX_ENGLISH_DICT_PATH";
/// 可写英文用户数据路径覆盖。
pub(crate) const ENGLISH_USER_LM_ENV: &str = "XWX_ENGLISH_USER_LM_PATH";
/// M2 零输入下一词预测开关（默认 0）。
pub(crate) const ENGLISH_PREDICT_ENV: &str = "XWX_ENGLISH_PREDICT";
/// M2 静态二元数据路径覆盖。
pub(crate) const ENGLISH_BIGRAM_ENV: &str = "XWX_ENGLISH_BIGRAM_PATH";

/// 系统级数据目录（deb 的 asset 目标）。**只给只读资源用**。
pub(crate) const SYSTEM_DATA_DIR: &str = "/usr/share/xwx";

/// 布尔取值的解析结果：`(取值, 这个取值认不认得)`。
///
/// 第二项让调用方能对「写了但没认出来」的值记一条 warning——`XWX_ENGLISH=of`
/// 应当被察觉，而不是静默当成默认值。不认得时取值回落到 `default`。
///
/// 认得的取值：`1/true/on/yes` 与 `0/false/off/no`（忽略 ASCII 大小写与两端空白），
/// 以及「未设置」。**空串不认**：它不是任何一个合法字面量。
pub(crate) fn parse_bool_env(value: Option<&str>, default: bool) -> (bool, bool) {
    let Some(raw) = value else {
        return (default, true); // 未设置不是错误
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" => (true, true),
        "0" | "false" | "off" | "no" => (false, true),
        _ => (default, false),
    }
}

/// 英文用户文件的默认文件名：debug 走 `.dev`，release 走真实文件。
pub(crate) fn english_user_file_name() -> &'static str {
    if cfg!(debug_assertions) {
        xwx_engine::ENGLISH_USER_LM_DEV_FILE_NAME
    } else {
        xwx_engine::ENGLISH_USER_LM_FILE_NAME
    }
}

/// 只读英文词库路径：显式 > 用户数据目录 > 系统数据目录。
///
/// 三者都不存在时返回**用户路径**，由调用方按「缺文件」处理并告警——
/// 与中文词库 [`crate::lm_path::resolve_data_path`] 同一口径。
pub(crate) fn resolve_english_dict_in(explicit: Option<&str>, xdg_data_home: &Path) -> PathBuf {
    if let Some(p) = explicit {
        // 显式路径**原样尊重**，即使不存在也不偷偷换别的文件：否则用户
        // 排查不出自己指定的路径为何没生效。
        return PathBuf::from(p);
    }
    let user = xdg_data_home.join("xwx").join("english.bin");
    let system = PathBuf::from(SYSTEM_DATA_DIR).join("english.bin");
    if user.is_file() {
        user
    } else if system.is_file() {
        system
    } else {
        user
    }
}

/// M2 静态二元数据路径：与词库同规则（用户目录优先，其次系统目录）。
pub(crate) fn resolve_english_bigram_in(explicit: Option<&str>, xdg_data_home: &Path) -> PathBuf {
    if let Some(p) = explicit {
        return PathBuf::from(p);
    }
    let user = xdg_data_home.join("xwx").join("english_bigram.bin");
    let system = PathBuf::from(SYSTEM_DATA_DIR).join("english_bigram.bin");
    if user.is_file() {
        user
    } else if system.is_file() {
        system
    } else {
        user
    }
}

/// **可写**英文用户文件路径：显式 > 用户数据目录。
///
/// 与只读资源分开：可写路径**永不回落 `/usr/share/xwx`**——那是个只读目录，
/// 落进去只会让回写静默失败，还会把用户数据混进系统资源。
pub(crate) fn resolve_english_user_in(
    explicit: Option<&str>,
    xdg_data_home: &Path,
    file_name: &str,
) -> PathBuf {
    match explicit {
        Some(p) => PathBuf::from(p),
        None => xdg_data_home.join("xwx").join(file_name),
    }
}

/// 生产入口解析出的英文配置（`enabled == false` 时其余字段不参与装配）。
#[derive(Debug, Clone)]
pub(crate) struct EnglishConfig {
    /// `XWX_ENGLISH`（默认 1）。
    pub(crate) enabled: bool,
    /// `XWX_ENGLISH_PREDICT`（默认 0，M2 零输入面板）。
    pub(crate) predict: bool,
    /// 只读词库路径。
    pub(crate) dict_path: PathBuf,
    /// 可写英文用户文件路径。
    pub(crate) user_path: PathBuf,
    /// M2 静态二元数据路径。
    pub(crate) bigram_path: PathBuf,
}

impl EnglishConfig {
    /// 从进程环境解析（唯一读环境变量的地方）。
    pub(crate) fn from_env() -> Self {
        let xdg = crate::lm_path::xdg_data_home_for_config();
        let (enabled, ok) = parse_bool_env(std::env::var(ENGLISH_ENV).ok().as_deref(), true);
        if !ok {
            tracing::warn!(
                value = ?std::env::var(ENGLISH_ENV).ok(),
                env = ENGLISH_ENV,
                "无法识别的布尔值，回落到默认(开启)"
            );
        }
        let (predict, ok) = parse_bool_env(
            std::env::var(ENGLISH_PREDICT_ENV).ok().as_deref(),
            false,
        );
        if !ok {
            tracing::warn!(
                value = ?std::env::var(ENGLISH_PREDICT_ENV).ok(),
                env = ENGLISH_PREDICT_ENV,
                "无法识别的布尔值，回落到默认(关闭)"
            );
        }
        Self {
            enabled,
            predict,
            dict_path: resolve_english_dict_in(
                std::env::var(ENGLISH_DICT_ENV).ok().as_deref(),
                &xdg,
            ),
            user_path: resolve_english_user_in(
                std::env::var(ENGLISH_USER_LM_ENV).ok().as_deref(),
                &xdg,
                english_user_file_name(),
            ),
            bigram_path: resolve_english_bigram_in(
                std::env::var(ENGLISH_BIGRAM_ENV).ok().as_deref(),
                &xdg,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_literals_are_recognized_case_insensitively() {
        for v in ["1", "true", "TRUE", "On", "yes", " yes "] {
            assert_eq!(parse_bool_env(Some(v), false), (true, true), "{v}");
        }
        for v in ["0", "false", "FALSE", "Off", "no", " no "] {
            assert_eq!(parse_bool_env(Some(v), true), (false, true), "{v}");
        }
    }

    #[test]
    fn unset_returns_default_and_counts_as_recognized() {
        assert_eq!(parse_bool_env(None, true), (true, true));
        assert_eq!(parse_bool_env(None, false), (false, true), "未设置不是错误");
    }

    #[test]
    fn unrecognized_value_falls_back_and_is_flagged() {
        // `off` 是认得的；`of`/空串/路径字符串不是。不能把文件路径偷偷
        // 解释成特殊文件模式——这正是「是否认得」要单独回报的原因。
        for v in ["", "  ", "of", "2", "/tmp/english.bin", "enabled"] {
            let (val, ok) = parse_bool_env(Some(v), false);
            assert!(!ok, "{v:?} 不应被当成合法布尔值");
            assert!(!val, "不认得时回落到缺省");
        }
    }

    #[test]
    fn explicit_dict_path_is_returned_verbatim_even_if_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let got = resolve_english_dict_in(Some("/nonexistent/custom.bin"), tmp.path());
        assert_eq!(got, PathBuf::from("/nonexistent/custom.bin"));
    }

    #[test]
    fn dict_path_prefers_user_then_system() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let xdg = tmp.path().join("xdg");
        let dir = xdg.join("xwx");
        std::fs::create_dir_all(&dir).expect("mkdir");
        // 都不存在 ⇒ 用户路径
        assert_eq!(resolve_english_dict_in(None, &xdg), dir.join("english.bin"));
        // 用户文件存在 ⇒ 用户路径
        std::fs::write(dir.join("english.bin"), b"x").expect("写");
        assert_eq!(resolve_english_dict_in(None, &xdg), dir.join("english.bin"));
    }

    #[test]
    fn bigram_path_prefers_user_then_system() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let xdg = tmp.path().join("xdg");
        std::fs::create_dir_all(xdg.join("xwx")).expect("mkdir");
        assert_eq!(
            resolve_english_bigram_in(None, &xdg),
            xdg.join("xwx").join("english_bigram.bin")
        );
    }

    #[test]
    fn writable_user_path_never_falls_back_to_system_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let xdg = tmp.path().join("xdg");
        let got = resolve_english_user_in(None, &xdg, "user_english.txt");
        assert_eq!(got, xdg.join("xwx").join("user_english.txt"));
        assert!(
            !got.starts_with(SYSTEM_DATA_DIR),
            "可写路径绝不能落到只读系统目录"
        );
        let explicit = resolve_english_user_in(Some("/tmp/e.txt"), &xdg, "user_english.txt");
        assert_eq!(explicit, PathBuf::from("/tmp/e.txt"));
    }

    #[test]
    fn english_user_file_name_is_profile_separated() {
        assert_ne!(
            xwx_engine::ENGLISH_USER_LM_FILE_NAME,
            xwx_engine::ENGLISH_USER_LM_DEV_FILE_NAME,
            "开发文件名与真实文件名不能相同，否则隔离形同虚设"
        );
        let expected = if cfg!(debug_assertions) {
            xwx_engine::ENGLISH_USER_LM_DEV_FILE_NAME
        } else {
            xwx_engine::ENGLISH_USER_LM_FILE_NAME
        };
        assert_eq!(english_user_file_name(), expected);
        assert_eq!(
            expected.ends_with(".dev.txt"),
            cfg!(debug_assertions),
            "debug 构建必须用 .dev 文件名"
        );
    }
}
