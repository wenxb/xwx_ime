//! crate 级单测:启动预热(`ImeEngine::prewarm`)。

use super::*;

/// 预热必须**只读**:查询结果不变、用户模型一个字段都不动。
///
/// 这条不变式比「预热有用」更重要——`src/main.rs` 在启动后无条件调它,
/// 一旦它偷偷学习/造词,用户模型里就会多出没人打过的词。
#[test]
fn prewarm_is_read_only() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你好", "ni hao", 900).expect("插入失败");
    engine.dict.upsert("我们", "wo men", 880).expect("插入失败");
    engine.dict.upsert("为", "wei", 700).expect("插入失败");
    engine.dict.upsert("是", "shi", 600).expect("插入失败");
    engine.dict.upsert("你", "ni", 500).expect("插入失败");
    let engine = Arc::new(engine.with_user_lm(UserLM::in_memory()));

    let ctx = LmContext::empty();
    let snapshot = |engine: &Arc<ImeEngine>| -> Vec<Vec<String>> {
        ["nihao", "weis", "wo", "ns", "nihoa"]
            .iter()
            .map(|p| {
                engine
                    .lookup_candidates(p, &ctx)
                    .expect("查询失败")
                    .into_iter()
                    .map(|c| c.word)
                    .collect()
            })
            .collect()
    };

    let before = snapshot(&engine);
    engine.prewarm();
    engine.prewarm(); // 可重复调用
    let after = snapshot(&engine);
    assert_eq!(before, after, "预热改变了查询结果");

    let user = engine.user_lm().expect("用户层没接上");
    assert_eq!(user.dirty_count(), 0, "预热不应该学习");
    assert_eq!(user.unigram_len(), 0, "预热不应该写一元计数");
    assert_eq!(user.entry_len(), 0, "预热不应该造词");
    assert_eq!(user.blocklist_len(), 0, "预热不应该改屏蔽表");
}

/// 启动期初始化(`prepare_startup`)同样必须只读、且可重复调用。
///
/// 它在 `src/main.rs` 里是无条件同步执行的(发布服务名之前),
/// 一旦它改了什么,或第二次调用就出问题,整机上的输入法都起不来。
#[test]
fn prepare_startup_is_read_only_and_repeatable() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你好", "ni hao", 900).expect("插入失败");
    engine.dict.upsert("拟好", "ni hao", 500).expect("插入失败");
    let engine = Arc::new(engine.with_user_lm(UserLM::in_memory()));

    let ctx = LmContext::empty();
    let snapshot = |engine: &Arc<ImeEngine>| -> Vec<String> {
        engine
            .lookup_candidates("nihao", &ctx)
            .expect("查询失败")
            .into_iter()
            .map(|c| c.word)
            .collect()
    };
    let before = snapshot(&engine);
    let total_before = engine.dict.total_freq().expect("词频总量失败");

    engine.prepare_startup();
    engine.prepare_startup();

    assert_eq!(before, snapshot(&engine), "启动期初始化改变了查询结果");
    assert_eq!(
        total_before,
        engine.dict.total_freq().expect("词频总量失败"),
        "词频总量在初始化前后必须一致"
    );
    let user = engine.user_lm().expect("用户层没接上");
    assert_eq!(user.dirty_count(), 0, "启动期初始化不应该学习");
    assert_eq!(user.unigram_len(), 0, "启动期初始化不应该写一元计数");
}
