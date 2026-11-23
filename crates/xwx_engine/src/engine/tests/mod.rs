//! 引擎单测与共享测试 fixture。

use super::*;
use crate::engine::rank::{
    effective_layer, leading_abbrev_input, pure_abbrev_input, tail_only, unexplained_letters,
    unreachable_tail,
};

/// 排序首键：候选终点处的不可避免未解释字母数（u_min 桶，与生产 `rank::sort_key` 同源）。
fn sort_bucket(inp: &str, c: &Candidate) -> usize {
    let cp_len = c.pinyin.chars().filter(|ch| *ch != ' ').count();
    let umin = unreachable_tail(inp, None);
    if tail_only(inp, c) && inp.len() - cp_len >= MAX_TYPO_GAP {
        unexplained_letters(inp, c)
    } else if c.covered >= inp.len() || (c.spelling == SpellingKind::Exact && cp_len > inp.len()) {
        0
    } else if c.covered > 0 && c.covered < inp.len() {
        if c.spelling == SpellingKind::Abbrev {
            (inp.len() - c.covered).max(umin[c.covered])
        } else {
            umin[c.covered]
        }
    } else {
        umin[cp_len.min(inp.len())]
    }
}

/// 排序第二键：有效层序（与生产 `effective_layer` 同源）。
fn sort_layer(inp: &str, c: &Candidate, seg: &Segmenter, has_abbrev: bool) -> SpellingKind {
    effective_layer(
        c.spelling,
        c.freq,
        c.covered,
        inp.len(),
        entry_tier_exception(inp, seg, has_abbrev),
    )
}

/// 层序收编的输入类别判据：不可达时纯缩写 ∪ 开头即缩写。
fn entry_tier_exception(inp: &str, seg: &Segmenter, has_abbrev: bool) -> bool {
    if unreachable_tail(inp, None)[0] == 0 {
        return false;
    }
    pure_abbrev_input(inp, has_abbrev) || leading_abbrev_input(seg, inp, has_abbrev)
}

fn data_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data")
}

/// 万象 `.gram` 的常规位置。
fn gram_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("XWX_LM_PATH") {
        let p = std::path::PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })?;
    let p = base.join("xwx/wanxiang-lts-zh-hans.gram");
    p.is_file().then_some(p)
}

/// 构造独立真实词库引擎（无 gram；可选挂副索引与音节图），供需要独占或挂 `UserLM` 的测试使用。
fn fresh_real_dict_engine() -> Option<ImeEngine> {
    let root = data_dir();
    let dict = root.join("dict.bin");
    if !dict.is_file() {
        return None;
    }
    let mut engine = ImeEngine::new(Some(&dict)).ok()?;
    let abbrev = root.join("abbrev.bin");
    if abbrev.is_file() {
        engine.dict = engine.dict.with_abbrev(&abbrev);
    }
    let graph = root.join("syllable_graph.bin");
    if graph.is_file() {
        engine.dict = engine.dict.with_syllable_graph(&graph);
    }
    Some(engine)
}

/// 进程级只读缓存：真实词库 + 可选副索引/音节图（无 gram）。
fn real_dict_engine() -> Option<&'static ImeEngine> {
    static ENGINE: std::sync::OnceLock<Option<ImeEngine>> = std::sync::OnceLock::new();
    ENGINE.get_or_init(fresh_real_dict_engine).as_ref()
}

/// 进程级只读缓存：真实词库 + 副索引 + 可选音节图 + 万象 `.gram`。
fn real_full_engine() -> Option<&'static ImeEngine> {
    static ENGINE: std::sync::OnceLock<Option<ImeEngine>> = std::sync::OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let root = data_dir();
            let dict = root.join("dict.bin");
            let abbrev = root.join("abbrev.bin");
            if !dict.is_file() || !abbrev.is_file() {
                return None;
            }
            let gram = gram_path()?;
            let mut engine = ImeEngine::new_with_lm(Some(&dict), Some(&gram)).ok()?;
            engine.dict = engine.dict.with_abbrev(&abbrev);
            let graph = root.join("syllable_graph.bin");
            if graph.is_file() {
                engine.dict = engine.dict.with_syllable_graph(&graph);
            }
            Some(engine)
        })
        .as_ref()
}

/// 构造独立双语真实引擎（供 `InputSession` 持有或挂 `UserLM` 的测试使用）。
fn fresh_real_bilingual_engine() -> Option<ImeEngine> {
    let root = data_dir();
    let dict = root.join("dict.bin");
    let abbrev = root.join("abbrev.bin");
    let english = root.join("english.bin");
    if !dict.is_file() || !abbrev.is_file() || !english.is_file() {
        return None;
    }
    let mut engine = match gram_path() {
        Some(gram) => ImeEngine::new_with_lm(Some(&dict), Some(&gram)).ok()?,
        None => ImeEngine::new(Some(&dict)).ok()?,
    };
    engine.dict = engine.dict.with_abbrev(&abbrev);
    let graph = root.join("syllable_graph.bin");
    if graph.is_file() {
        engine.dict = engine.dict.with_syllable_graph(&graph);
    }
    let en = crate::english::EnglishDict::open(&english).ok()?;
    Some(engine.with_english(crate::english::EnglishEngine::new(en)))
}

/// 进程级只读缓存：双语真实引擎（词库 + 副索引 + 可选音节图 + 可选 gram + 英文词库）。
fn real_bilingual_engine() -> Option<&'static ImeEngine> {
    static ENGINE: std::sync::OnceLock<Option<ImeEngine>> = std::sync::OnceLock::new();
    ENGINE.get_or_init(fresh_real_bilingual_engine).as_ref()
}

mod abbrev;
mod abbrev_correct;
mod beam_lm;
mod bilingual;
mod blend_matrix;
mod collocation;
mod completion;
mod fuzzy;
mod indexed_mixed;
mod input;
mod literal_tail;
mod lookup;
mod personalization;
mod sentence_ranking;
mod tail_completion;
mod typo_budget;
mod typo_window;
