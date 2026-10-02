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
