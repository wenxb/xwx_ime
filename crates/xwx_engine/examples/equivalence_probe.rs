//! 全字段候选与逐键会话对拍；stdout 只含确定性结果，不含耗时。
use std::{path::Path, sync::Arc};
use xwx_engine::{
    Candidate, EnglishBigram, EnglishContext, EnglishDict, EnglishEngine, ImeEngine, InputEvent,
    LmContext, LmStore, SessionOutput, UserLM,
};

fn dump(tag: &str, candidates: &[Candidate]) {
    println!("{tag}: {}", candidates.len());
    for c in candidates {
        println!(
            "{:08x}\tmatched_len={}\t{}",
            c.score.to_bits(),
            c.matched_len,
            serde_json::to_string(c).unwrap()
        );
    }
}

fn engine(root: &Path, learned: bool, fuzzy: bool) -> ImeEngine {
    for name in [
        "dict.bin",
        "abbrev.bin",
        "syllable_graph.bin",
        "wanxiang-lts-zh-hans.gram",
        "english.bin",
        "english_bigram.bin",
    ] {
        assert!(root.join(name).is_file(), "missing {name}");
    }
    let mut e = ImeEngine::new_with_lm(
        Some(&root.join("dict.bin")),
        Some(&root.join("wanxiang-lts-zh-hans.gram")),
    )
    .unwrap();
    assert!(matches!(&e.lm, LmStore::Wanxiang(lm) if lm.num_units() > 0));
    e.dict = e
        .dict
        .with_abbrev(root.join("abbrev.bin"))
        .with_syllable_graph(root.join("syllable_graph.bin"));
    assert!(e.dict.has_abbrev() && e.dict.has_syllable_graph());
    let user = UserLM::in_memory();
    let en_user = UserLM::in_memory();
    if learned {
        for _ in 0..3 {
            user.learn(Some("我们"), "今天");
        }
        user.add_entry("张三丰", "zhang san feng");
        user.block_candidate("中国", "zhong guo");
        en_user.learn(Some("hello"), "world");
        en_user.learn(Some("hello"), "python");
        en_user.block_candidate("help", "__english__");
    }
    e = e.with_user_lm(user).with_english(
        EnglishEngine::new(EnglishDict::open(root.join("english.bin")).unwrap())
            .with_bigram(EnglishBigram::open(root.join("english_bigram.bin")).unwrap())
            .with_predict(true)
            .with_user_lm(en_user),
    );
    e.fuzzy_enabled = fuzzy;
    e
}

fn benchmark(e: &ImeEngine) {
    use std::{hint::black_box, time::Instant};
    for ctx in [
        LmContext::empty(),
        LmContext {
            tail: "我们今天",
            last_word: Some("今天"),
        },
    ] {
        for input in [
            "nihao",
            "zh",
            "xx",
            "zgr",
            "zhongg",
            "shh",
            "wmjtqbj",
            "daishix",
            "womenjintianquxian",
            "pyt",
            "devel",
            "wor",
            "world",
        ] {
            for _ in 0..5 {
                black_box(
                    e.lookup_composition(input, &ctx, &EnglishContext::with_previous("hello"))
                        .unwrap(),
                );
            }
            let mut samples = Vec::new();
            for _ in 0..70 {
                let started = Instant::now();
                let result = e
                    .lookup_composition(input, &ctx, &EnglishContext::with_previous("hello"))
                    .unwrap();
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
                black_box(result);
            }
            samples.sort_by(f64::total_cmp);
            println!(
                "{input}\tctx={}\tp50={:.4}\tp95={:.4}",
                !ctx.tail.is_empty(),
                samples[35],
                samples[66]
            );
        }
    }
}

fn main() {
    let root_arg = std::env::args().nth(1).unwrap_or_else(|| "data".into());
    let root = Path::new(&root_arg);
    let inputs = [
        "",
        "a",
        "an",
        "ni",
        "nihao",
        "zh",
        "xx",
        "zgr",
        "zhongg",
        "shh",
        "smsh",
        "wmjtqbj",
        "daishix",
        "buyy",
        "yiwanch",
        "shanghaai",
        "womenjintianquxian",
        "nihaoshijie",
        "xi'an",
        "zh'on",
        "lue",
        "nue",
        "zzzz",
        "aaaaaaaa",
        "zhzhzhzh",
        "nihoa",
        "nihaoo",
        "nihso",
        "zhangsanfeng",
        "pyt",
        "devel",
        "wor",
        "world",
        "hello",
        "A",
    ];
    for (learned, fuzzy) in [(false, false), (true, false), (true, true)] {
        println!("mode learned={learned} fuzzy={fuzzy}");
        let e = Arc::new(engine(root, learned, fuzzy));
        if std::env::args().any(|a| a == "--bench") {
            benchmark(&e);
            continue;
        }
        for ctx in [
            LmContext::empty(),
            LmContext {
                tail: "我们今天",
                last_word: Some("今天"),
            },
        ] {
            for input in inputs {
                dump(
                    &format!("zh {input:?} ctx={ctx:?}"),
                    &e.lookup_candidates(input, &ctx).unwrap(),
                );
                for en in [
                    EnglishContext::empty(),
                    EnglishContext::with_previous("hello"),
                ] {
                    dump(
                        &format!("mix {input:?} ctx={ctx:?} en={en:?}"),
                        &e.lookup_composition(input, &ctx, &en).unwrap(),
                    );
                }
            }
        }
        for body in [
            "nihao",
            "zhongguor",
            "womenjintianquxian",
            "xi'an",
            "pyt",
            "world",
            "zhangsanfeng",
        ] {
            let mut session = e.create_session();
            let mut events: Vec<_> = body.chars().map(InputEvent::Char).collect();
            events.extend([
                InputEvent::Left,
                InputEvent::Backspace,
                InputEvent::Char('a'),
                InputEvent::Right,
                InputEvent::PageDown,
                InputEvent::PageUp,
                InputEvent::Space,
                InputEvent::Backspace,
                InputEvent::Enter,
            ]);
            for event in events {
                println!("event {body} {event:?}");
                let output = session.handle(event).unwrap();
                println!("{output:?}");
                match &output {
                    SessionOutput::Updated(u)
                    | SessionOutput::RefreshAndForward(u)
                    | SessionOutput::CommitAndUpdate { update: u, .. } => {
                        dump("update", &u.candidates)
                    }
                    _ => {}
                }
            }
        }
    }
}
