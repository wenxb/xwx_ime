//! # xwx_engine：拼音输入法纯算法核心
//!
//! 不依赖任何 GUI / DBus 框架，只做输入法该做的"智力活"：
//!
//! 1. [`segmenter`]：拼音音节表 + 切分；
//! 2. [`dict`]：词库查询（SQLite）；
//! 3. [`lm`]：语言模型打分；
//! 4. [`corrector`]：模糊拼音展开；
//! 5. [`session`]：按键状态机；
//! 6. [`ImeEngine`]：以上所有组件的聚合入口。
//!
//! 典型的调用方式（由 xwx_ibus 适配层驱动）：
//!
//! ```ignore
//! let engine = Arc::new(ImeEngine::new(Some("data/dict.db".as_ref()))?);
//! let mut session = engine.create_session();
//! match session.handle(InputEvent::Char('n'))? {
//!     SessionOutput::Updated(u) => { /* 刷新候选窗 */ }
//!     _ => {}
//! }
//! ```

pub use engine::trace;
pub mod candidate;
pub mod corrector;
pub mod dict;
pub mod english;
pub mod lm;
pub mod punctuator;
pub mod recognizer;
pub mod segmenter;
pub mod session;
pub mod user_lm;
pub mod wanxiang;

mod engine;

pub use engine::*;

pub use candidate::{Candidate, SpellingKind};
pub use corrector::{expand_fuzzy, expand_fuzzy_scored, ConfusionPair, COMMON_CONFUSION_PAIRS};
pub use dict::{DictError, DictStore, Dictionary, MmapDict, SqliteDict};
pub use english::{
    EnglishBigram, EnglishContext, EnglishDict, EnglishEngine, EnglishEntryRef, EnglishError,
    ENGLISH_USER_LM_DEV_FILE_NAME, ENGLISH_USER_LM_FILE_NAME,
};
pub use lm::{BigramLM, LanguageModel, LmContext, LmError, LmState, LmStore};
pub use lm::{MixedLM, MixedState, USER_COUNT_GAIN_R};
pub use recognizer::{recognize, Recognized, URL_PREFIXES};
pub use user_lm::{
    UserLM, FLUSH_LEARN_THRESHOLD, MAX_USER_UNIGRAMS, USER_LM_DEV_FILE_NAME, USER_LM_FILE_NAME,
};
pub use segmenter::{Segmenter, SYLLABLES};
pub use session::{InputEvent, InputSession, SessionOutput, SessionUpdate};
pub use wanxiang::{WanxiangConfig, WanxiangLM, WanxiangState};
