//! 数据与语言模型的**路径解析**(从 `main.rs` 拆出,2026-10-02)。
//!
//! 这一段与引擎装配无关,只回答两个问题:「数据目录在哪」和「用哪个
//! 语言模型文件」。之所以单独成文件:它带着一整套**可测**的纯函数
//! (`resolve_lm_path_in` / `first_gram_in` / `xdg_data_home` 注入路径),
//! 而且隐式默认值必须被断言钉住——debug 构建不许污染真实用户模型
//! 这件事,只写在注释里是不够的(见本文件末尾的测试)。

use std::path::PathBuf;

/// 解析数据文件路径：环境变量覆盖 > `$XDG_DATA_HOME/xwx/<name>` >
/// `~/.local/share/xwx/<name>`。
/// 用户模型**默认**文件名(不含环境变量覆盖):debug 构建走开发文件。
///
/// 抽成独立函数是为了可测——隐式默认值必须能被一条断言钉住,
/// 否则「debug 不污染真实数据」这件事就只剩注释里的一句话。
pub(crate) fn user_lm_file_name() -> &'static str {
    if cfg!(debug_assertions) {
        xwx_engine::USER_LM_DEV_FILE_NAME
    } else {
        xwx_engine::USER_LM_FILE_NAME
    }
}

/// 当前进程的 XDG 数据根目录（`english_config` 用它解析英文资源）。
pub(crate) fn xdg_data_home_for_config() -> PathBuf {
    xdg_data_home(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
}

pub(crate) fn resolve_data_path(env_key: &str, file_name: &str) -> PathBuf {
    if let Ok(p) = std::env::var(env_key) {
        return PathBuf::from(p);
    }
    let base = xdg_data_home(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"));
    let user = base.join("xwx").join(file_name);
    // **系统级回退**：deb 把词库/副索引装到 `/usr/share/xwx/`，而用户目录里没有
    // 的时候必须能读到——否则装了包也只退化成内置小词库（Q5 离线交付）。
    let system = PathBuf::from(SYSTEM_DATA_DIR).join(file_name);
    pick_existing(user, system)
}

/// 系统级数据目录（deb 的 asset 目标）。
const SYSTEM_DATA_DIR: &str = "/usr/share/xwx";

/// 用户目录优先、其次系统目录；两者都不存在时返回用户路径
/// （调用方按「缺文件」处理并告警，保持原有错误路径不变）。
fn pick_existing(user: PathBuf, system: PathBuf) -> PathBuf {
    if user.is_file() {
        user
    } else if system.is_file() {
        system
    } else {
        user
    }
}

/// 由 `XDG_DATA_HOME` / `HOME` 算出 XDG 数据根目录。
///
/// 抽成独立函数主要是让测试可以注入路径,避免依赖进程级环境变量
/// (Rust 测试多线程并发,`std::env::set_var` 会互相干扰)。
fn xdg_data_home(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> PathBuf {
    xdg.map(PathBuf::from)
        .or_else(|| home.map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 语言模型路径来源(用于日志区分显式指定与自动探测)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LmSource {
    /// `XWX_LM_PATH` 显式指定(原样使用,不做存在性检查)
    Explicit,
    /// 未显式指定时,在数据目录里探测到的首选 `.gram`
    ProbedGram,
    /// 未显式指定时,回落到数据目录里的 `bigram.txt`(可能存在也可能不存在)
    FallbackBigramTxt,
}

impl LmSource {
    pub(crate) fn label(self) -> &'static str {
        match self {
            LmSource::Explicit => "显式 XWX_LM_PATH",
            LmSource::ProbedGram => "自动探测(.gram)",
            LmSource::FallbackBigramTxt => "自动回落(bigram.txt)",
        }
    }
}

/// 语言模型路径解析结果。
pub(crate) struct LmChoice {
    pub(crate) path: PathBuf,
    pub(crate) source: LmSource,
}

/// 解析语言模型路径。
///
/// `explicit` 是 `XWX_LM_PATH` 的当前值(`Some` 即用户显式设置):
/// 显式设置时**原样尊重**——即使指向不存在的文件,也由调用方按现有
/// 逻辑优雅退化为空模型,**绝不**偷偷回落到别的文件,否则用户排查
/// 不出自己指定的路径为何没生效。
///
/// 未显式设置时按以下优先级探测 `$XDG_DATA_HOME/xwx/`:
///
/// 1. `wanxiang-lts-zh-hans.gram`(简体万象,最常见);
/// 2. 目录下任意 `.gram` 文件(例如繁体 `wanxiang-lts-zh-hant.gram`);
///    多个时按文件名字典序取第一个,保证行为确定可复现,不依赖
///    目录遍历顺序;
/// 3. `bigram.txt`(文本 bigram,回落到旧行为;不存在也返回此路径,
///    由调用方走"未找到"分支)。
pub(crate) fn resolve_lm_path(explicit: Option<&str>) -> LmChoice {
    resolve_lm_path_with_system(
        explicit,
        &xdg_data_home(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME")),
        Some(std::path::Path::new(SYSTEM_DATA_DIR)),
    )
}

/// 在指定 XDG 数据根目录下解析语言模型路径(可测试的核心逻辑)。
#[cfg(test)]
fn resolve_lm_path_in(explicit: Option<&str>, xdg_data_home: &std::path::Path) -> LmChoice {
    resolve_lm_path_with_system(explicit, xdg_data_home, None)
}

fn resolve_lm_path_with_system(
    explicit: Option<&str>,
    xdg_data_home: &std::path::Path,
    system_dir: Option<&std::path::Path>,
) -> LmChoice {
    if let Some(p) = explicit {
        return LmChoice {
            path: PathBuf::from(p),
            source: LmSource::Explicit,
        };
    }
    let xwx_dir = xdg_data_home.join("xwx");

    for dir in [Some(xwx_dir.as_path()), system_dir].into_iter().flatten() {
        let preferred = dir.join("wanxiang-lts-zh-hans.gram");
        if preferred.is_file() {
            return LmChoice {
                path: preferred,
                source: LmSource::ProbedGram,
            };
        }
        if let Some(gram) = first_gram_in(dir) {
            return LmChoice {
                path: gram,
                source: LmSource::ProbedGram,
            };
        }
    }

    LmChoice {
        path: xwx_dir.join("bigram.txt"),
        source: LmSource::FallbackBigramTxt,
    }
}

/// 列出 `dir` 下所有 `.gram` 文件,按文件名字典序返回第一个。
///
/// 读目录失败(不存在/无权限)一律返回 `None`,让上层走 bigram.txt 回落。
fn first_gram_in(dir: &std::path::Path) -> Option<PathBuf> {
    let mut grams: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "gram"))
        .collect();
    grams.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    grams.into_iter().next()
}

/// 为已打开的词库装配 `abbrev.bin` 与 `syllable_graph.bin`；
/// 若未显式指定环境变量且本地缺失/异代，则自动从 `dict.bin` 生成至用户数据目录。
pub(crate) fn attach_or_build_indexes(
    mut dict: xwx_engine::DictStore,
    dict_path: &std::path::Path,
) -> xwx_engine::DictStore {
    let user_dir = xdg_data_home_for_config().join("xwx");
    let sibling_abbrev = dict_path.with_file_name("abbrev.bin");
    let user_abbrev = user_dir.join("abbrev.bin");
    if let Some(exp) = std::env::var_os("XWX_ABBREV_PATH") {
        dict = dict.with_abbrev(PathBuf::from(exp));
    } else {
        for cand in [&sibling_abbrev, &user_abbrev] {
            if cand.is_file() {
                dict = dict.with_abbrev(cand);
                if dict.has_abbrev() {
                    break;
                }
            }
        }
        if !dict.has_abbrev() {
            let target = if sibling_abbrev
                .parent()
                .is_some_and(|p| p != std::path::Path::new(SYSTEM_DATA_DIR))
            {
                &sibling_abbrev
            } else {
                &user_abbrev
            };
            if dict.build_companion_indexes(Some(target), None).is_ok() {
                dict = dict.with_abbrev(target);
            }
        }
    }

    let sibling_graph = dict_path.with_file_name("syllable_graph.bin");
    let user_graph = user_dir.join("syllable_graph.bin");
    if let Some(exp) = std::env::var_os("XWX_SYLLABLE_GRAPH_PATH") {
        dict = dict.with_syllable_graph(PathBuf::from(exp));
    } else {
        for cand in [&sibling_graph, &user_graph] {
            if cand.is_file() {
                dict = dict.with_syllable_graph(cand);
                if dict.has_syllable_graph() {
                    break;
                }
            }
        }
        if !dict.has_syllable_graph() && std::env::var_os("XWX_ABBREV_PATH").is_none() {
            let target = if sibling_graph
                .parent()
                .is_some_and(|p| p != std::path::Path::new(SYSTEM_DATA_DIR))
            {
                &sibling_graph
            } else {
                &user_graph
            };
            if dict.build_companion_indexes(None, Some(target)).is_ok() {
                dict = dict.with_syllable_graph(target);
            }
        }
    }
    dict
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_xdg(files: &[&str]) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let xdg = tmp.path().join("xdg");
        let xwx_dir = xdg.join("xwx");
        fs::create_dir_all(&xwx_dir).expect("mkdir xwx");
        for name in files {
            fs::write(xwx_dir.join(name), b"").expect("touch file");
        }
        (tmp, xdg)
    }

    #[test]
    fn resolve_lm_path_priority_and_gram_selection() {
        // 显式路径即使不存在也原样尊重
        let (_t0, xdg0) = setup_xdg(&["wanxiang-lts-zh-hans.gram", "bigram.txt"]);
        let exp = resolve_lm_path_in(Some("/nonexistent/custom.gram"), &xdg0);
        assert_eq!(exp.path, PathBuf::from("/nonexistent/custom.gram"));
        assert_eq!(exp.source, LmSource::Explicit);

        // 优先简体万象 > 任意 .gram > 多个 .gram 按字典序首位
        for (files, want) in [
            (
                &["wanxiang-lts-zh-hans.gram", "wanxiang-lts-zh-hant.gram", "bigram.txt"][..],
                "wanxiang-lts-zh-hans.gram",
            ),
            (&["wanxiang-lts-zh-hant.gram", "bigram.txt"][..], "wanxiang-lts-zh-hant.gram"),
            (
                &["zzz-custom.gram", "aaa-custom.gram", "mmm-custom.gram", "bigram.txt"][..],
                "aaa-custom.gram",
            ),
        ] {
            let (_t, xdg) = setup_xdg(files);
            let choice = resolve_lm_path_in(None, &xdg);
            assert_eq!(choice.path, xdg.join("xwx").join(want));
            assert_eq!(choice.source, LmSource::ProbedGram);
        }
    }

    #[test]
    fn resolve_lm_path_fallbacks_system_dir_and_ignores_subdirs() {
        // 无 .gram / 空目录 / xwx 目录不存在 / 仅子目录有 .gram → 回落 bigram.txt
        let (_t1, xdg1) = setup_xdg(&["bigram.txt", "dict.bin"]);
        let sub = xdg1.join("xwx").join("subdir");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("nested.gram"), b"").unwrap();
        let (_t2, xdg2) = setup_xdg(&[]);
        let t3 = tempfile::tempdir().unwrap();
        let xdg3 = t3.path().join("xdg");
        for xdg in [&xdg1, &xdg2, &xdg3] {
            let c = resolve_lm_path_in(None, xdg);
            assert_eq!(c.path, xdg.join("xwx").join("bigram.txt"));
            assert_eq!(c.source, LmSource::FallbackBigramTxt);
        }

        // system_dir 回退：用户目录空时命中系统目录；用户目录有则优先用户目录
        let sys_tmp = tempfile::tempdir().unwrap();
        let sys_gram = sys_tmp.path().join("wanxiang-lts-zh-hans.gram");
        fs::write(&sys_gram, b"").unwrap();
        let c = resolve_lm_path_with_system(None, &xdg2, Some(sys_tmp.path()));
        assert_eq!(c.path, sys_gram);
        assert_eq!(c.source, LmSource::ProbedGram);

        let user_gram = xdg2.join("xwx").join("custom.gram");
        fs::write(&user_gram, b"").unwrap();
        let c = resolve_lm_path_with_system(None, &xdg2, Some(sys_tmp.path()));
        assert_eq!(c.path, user_gram);
        assert_eq!(c.source, LmSource::ProbedGram);
    }

    #[test]
    fn pick_existing_prefers_user_then_system_then_user_default() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user.bin");
        let system = tmp.path().join("system.bin");
        assert_eq!(pick_existing(user.clone(), system.clone()), user);
        fs::write(&system, b"x").unwrap();
        assert_eq!(pick_existing(user.clone(), system.clone()), system);
        fs::write(&user, b"x").unwrap();
        assert_eq!(pick_existing(user.clone(), system.clone()), user);
    }

    #[test]
    fn debug_builds_default_to_the_dev_user_model_file() {
        assert_ne!(xwx_engine::USER_LM_DEV_FILE_NAME, xwx_engine::USER_LM_FILE_NAME);
        let expected = if cfg!(debug_assertions) {
            xwx_engine::USER_LM_DEV_FILE_NAME
        } else {
            xwx_engine::USER_LM_FILE_NAME
        };
        assert_eq!(user_lm_file_name(), expected);
        assert_eq!(expected.ends_with(".dev.txt"), cfg!(debug_assertions));
    }
}
