//! 「`score_only` 真的省下分配」的确定性验证。
//!
//! 「记录每查询分配次数/字节」是为了让「省了一次 `String`/`Box`」这种
//! 断言靠毫秒数是测不出来的(在真实查询里它淹没在噪声里)。这里用**计数
//! 分配器**直接量字节:同样次数的 `score` 与 `score_only` 各跑一遍,
//! 后者必须明显更少——这是结构性的,和机器快慢无关。
//!
//! 不引第三方依赖:`GlobalAlloc` 包一层 `System`,只数不改行为。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use xwx_engine::lm::{LanguageModel, LmContext, LmStore, MixedLM};
use xwx_engine::{BigramLM, UserLM};

static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATED.fetch_add(new_size, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn bytes_for<F: FnMut()>(mut f: F) -> usize {
    let before = ALLOCATED.load(Ordering::Relaxed);
    f();
    ALLOCATED.load(Ordering::Relaxed) - before
}

#[test]
fn score_only_allocates_less_than_score() {
    let mut base = BigramLM::new();
    for word in ["你", "好", "可以", "天气", "西安", "斯巴克", "今天"] {
        base.add_unigram(word, 100);
    }
    base.add_bigram("你", "好", 90);
    let user = UserLM::in_memory();
    for word in ["你好", "可以", "斯巴克"] {
        user.learn(None, word);
    }
    user.learn(Some("你"), "好");
    let store = LmStore::Mixed(MixedLM::new(LmStore::Bigram(base), user));

    let words = ["你好", "可以", "天气", "西安", "斯巴克", "龘"];
    let state = store.state_from_context(&LmContext {
        tail: "你",
        last_word: Some("你"),
    });
    // 预热一次,避免把首次分配(HashMap 扩容之类)算进任何一边。
    for word in words {
        let _ = store.score(&state, word);
        let _ = store.score_only(&state, word);
    }

    let full = bytes_for(|| {
        for _ in 0..100 {
            for word in words {
                let (score, _next) = store.score(&state, word);
                std::hint::black_box(score);
            }
        }
    });
    let only = bytes_for(|| {
        for _ in 0..100 {
            for word in words {
                let score = store.score_only(&state, word);
                std::hint::black_box(score);
            }
        }
    });

    println!("600 次打分:score 分配 {full} 字节,score_only 分配 {only} 字节");
    assert!(
        only < full,
        "score_only 没有省下分配:score={full} 字节,score_only={only} 字节"
    );
    assert!(
        only * 2 <= full,
        "省得不够明显(要求至少减半):score={full} 字节,score_only={only} 字节"
    );
}
