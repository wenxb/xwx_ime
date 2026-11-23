//! 英文通道的**配置解析与路径解析**（生产入口专用）。
//!
//! 纯库（`xwx_engine`）不读环境变量：所有 `XWX_ENGLISH*` 的解析都在这里，
//! 由 `src/main.rs` 显式调用并把结果装进引擎。测试与探针走显式的
//! `with_english(...)`，不靠进程环境。
//!
//! 配置表见 `docs/english-input/01-data.md` 第 7 节。

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

/// 只读英文资源路径解析：显式 > 用户数据目录 > 系统数据目录；都不存在返回用户路径。
fn resolve_readonly_in(
    explicit: Option<&str>,
    xdg_data_home: &Path,
    system_dir: &Path,
    file_name: &str,
) -> PathBuf {
    if let Some(p) = explicit {
        return PathBuf::from(p);
    }
    let user = xdg_data_home.join("xwx").join(file_name);
    let system = system_dir.join(file_name);
    if user.is_file() {
        user
    } else if system.is_file() {
        system
    } else {
        user
    }
}

/// 只读英文词库路径：显式 > 用户数据目录 > 系统数据目录。
pub(crate) fn resolve_english_dict_in(explicit: Option<&str>, xdg_data_home: &Path) -> PathBuf {
    resolve_readonly_in(explicit, xdg_data_home, Path::new(SYSTEM_DATA_DIR), "english.bin")
}

/// M2 静态二元数据路径：与词库同规则（用户目录优先，其次系统目录）。
pub(crate) fn resolve_english_bigram_in(explicit: Option<&str>, xdg_data_home: &Path) -> PathBuf {
    resolve_readonly_in(
        explicit,
        xdg_data_home,
        Path::new(SYSTEM_DATA_DIR),
        "english_bigram.bin",
    )
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
    fn parse_bool_env_handles_literals_unset_and_invalid() {
        for v in ["1", "true", "TRUE", "On", "yes", " yes "] {
            assert_eq!(parse_bool_env(Some(v), false), (true, true), "{v}");
        }
        for v in ["0", "false", "FALSE", "Off", "no", " no "] {
            assert_eq!(parse_bool_env(Some(v), true), (false, true), "{v}");
        }
        assert_eq!(parse_bool_env(None, true), (true, true));
        assert_eq!(parse_bool_env(None, false), (false, true));
        for v in ["", "  ", "of", "2", "/tmp/english.bin", "enabled"] {
            assert_eq!(parse_bool_env(Some(v), false), (false, false), "{v:?}");
        }
    }

    #[test]
    fn path_resolution_priority_is_hermetic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let xdg = tmp.path().join("xdg");
        let user_dir = xdg.join("xwx");
        let sys_dir = tmp.path().join("sys");
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::create_dir_all(&sys_dir).unwrap();

        for file in ["english.bin", "english_bigram.bin"] {
            // 显式路径原样尊重（即使不存在）
            assert_eq!(
                resolve_readonly_in(Some("/nonexistent/custom.bin"), &xdg, &sys_dir, file),
                PathBuf::from("/nonexistent/custom.bin")
            );
            // 都不存在 → 用户路径；仅系统存在 → 系统路径；用户也存在 → 用户优先
            assert_eq!(resolve_readonly_in(None, &xdg, &sys_dir, file), user_dir.join(file));
            std::fs::write(sys_dir.join(file), b"s").unwrap();
            assert_eq!(resolve_readonly_in(None, &xdg, &sys_dir, file), sys_dir.join(file));
            std::fs::write(user_dir.join(file), b"u").unwrap();
            assert_eq!(resolve_readonly_in(None, &xdg, &sys_dir, file), user_dir.join(file));
        }
        // 可写用户路径只认显式或用户目录，永不回落系统目录
        let writable = resolve_english_user_in(None, &xdg, "user_english.txt");
        assert_eq!(writable, user_dir.join("user_english.txt"));
        assert!(!writable.starts_with(SYSTEM_DATA_DIR));
        assert_eq!(
            resolve_english_user_in(Some("/tmp/e.txt"), &xdg, "user_english.txt"),
            PathBuf::from("/tmp/e.txt")
        );
    }

    #[test]
    fn english_user_file_name_is_profile_separated() {
        assert_ne!(
            xwx_engine::ENGLISH_USER_LM_FILE_NAME,
            xwx_engine::ENGLISH_USER_LM_DEV_FILE_NAME
        );
        let expected = if cfg!(debug_assertions) {
            xwx_engine::ENGLISH_USER_LM_DEV_FILE_NAME
        } else {
            xwx_engine::ENGLISH_USER_LM_FILE_NAME
        };
        assert_eq!(english_user_file_name(), expected);
        assert_eq!(expected.ends_with(".dev.txt"), cfg!(debug_assertions));
    }
}
