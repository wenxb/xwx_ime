//! 用户配置：`$XDG_CONFIG_HOME/xwx/config.toml`。
//!
//! 引擎进程与首选项面板共用本 crate：面板写、引擎读，两边对文件名、
//! 字段名和默认值的理解只有这一份，不会各自漂移。
//!
//! 配置**只放用户可调项**。词库/模型路径这类部署参数仍走环境变量
//! （见 `src/main.rs`），面板不该让用户误以为可以乱改。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 配置文件名（`~/.config/xwx/config.toml`）。
pub const CONFIG_FILE_NAME: &str = "config.toml";
/// 配置目录名。
pub const CONFIG_DIR_NAME: &str = "xwx";

/// 引擎启动时的默认语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// 中文：敲字母出候选。
    Zh,
    /// 英文：字母直接给应用（等同开着 Ctrl+空格 的英文模式）。
    En,
}

impl Language {
    /// 序列化用的稳定字面量，也是面板 `ComboRow` 的下标顺序依据。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zh => "zh",
            Self::En => "en",
        }
    }

    /// 面板下拉框的显示名。
    pub fn label(self) -> &'static str {
        match self {
            Self::Zh => "中文",
            Self::En => "英文",
        }
    }

    /// 全部取值，顺序即面板下拉框顺序。
    pub const ALL: [Language; 2] = [Language::Zh, Language::En];

    /// 下拉框下标 → 取值。
    pub fn from_index(index: u32) -> Option<Self> {
        Self::ALL.get(index as usize).copied()
    }

    /// 取值 → 下拉框下标。
    pub fn index(self) -> u32 {
        Self::ALL.iter().position(|l| *l == self).unwrap_or(0) as u32
    }
}

/// `config.toml` 的完整内容。
///
/// `#[serde(default)]` 让**缺字段**的旧文件照常可读：新增配置项时
/// 老用户的文件不必重写。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 引擎启动时的默认语言。默认英文。
    pub default_language: Language,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_language: Language::En,
        }
    }
}

impl Config {
    /// 从 TOML 文本解析。语法错误返回原文错误，调用方决定告警还是降级。
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// 序列化成 TOML 文本（写盘前用）。
    pub fn to_toml(&self) -> String {
        // 只有两个字段、无嵌套表，手写比引 toml 的序列化路径更可控：
        // 输出稳定、无多余表头，diff 友好。
        format!("default_language = \"{}\"\n", self.default_language.as_str())
    }

    /// 读取配置。文件不存在或解析失败时返回默认值并告警——
    /// **配置坏掉不能让输入法起不来**，与词库/模型一律优雅退化同口径。
    pub fn load_or_default(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(path = %path.display(), "配置文件不存在，使用默认值");
                return Self::default();
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "配置文件读取失败，使用默认值"
                );
                return Self::default();
            }
        };
        match Self::from_toml(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "配置文件解析失败，使用默认值"
                );
                Self::default()
            }
        }
    }

    /// 写入配置文件，必要时创建目录。
    ///
    /// 先写同目录临时文件再 `rename`：中途失败不会留下半截配置把
    /// 引擎带进降级路径。
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, self.to_toml())?;
        std::fs::rename(&tmp, path)
    }
}

/// 配置文件默认路径：`$XDG_CONFIG_HOME/xwx/config.toml`，
/// 未设 `XDG_CONFIG_HOME` 时退回 `$HOME/.config/xwx/config.toml`。
pub fn config_path() -> PathBuf {
    config_path_in(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

/// [`config_path`] 的纯函数版本，便于测试注入。
pub fn config_path_in(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> PathBuf {
    let base = xdg_config_home
        .map(PathBuf::from)
        .or_else(|| home.map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME)
}

#[cfg(test)]
mod tests;
