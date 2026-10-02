//! 本模块的单测(从同一个文件里拆出,2026-10-02:`mod.rs` 只管实现,
//! 测试单独一个文件;`use super::*` 让它们照旧能碰私有条目)。

use super::*;

#[test]
fn learn_produces_counts_for_committed_word() {
    // 计数口径(2026-10-02 起):这一层只回计数,不回概率。
    let lm = UserLM::in_memory();
    assert_eq!(
        lm.counts_with(&lm.snapshot(), None, "斯巴克"),
        (0, 0),
        "没学过就没有任何计数"
    );
    for _ in 0..5 {
        lm.learn(None, "斯巴克");
    }
    assert_eq!(
        lm.counts_with(&lm.snapshot(), None, "斯巴克"),
        (5, 0),
        "5 次上屏 ⇒ 一元计数 5"
    );
    // 二元单独记:中→国 学 5 次,一元也各 +5
    for _ in 0..5 {
        lm.learn(Some("中"), "国");
    }
    let snap = lm.snapshot();
    assert_eq!(
        lm.counts_with(&snap, Some("中"), "国"),
        (5, 5),
        "有上文「中」时:一元 5 + 二元(中→国) 5"
    );
    assert_eq!(
        lm.counts_with(&snap, None, "国"),
        (5, 0),
        "没有上文时只有一元计数,二元不给分"
    );
    assert_eq!(
        lm.counts_with(&snap, Some("今天"), "国"),
        (5, 0),
        "上文不匹配时不产生二元计数"
    );
}

#[test]
fn persistence_roundtrip_keeps_effect() {
    let dir = std::env::temp_dir().join(format!("xwx_userlm_test_{}", std::process::id()));
    let path = dir.join("user_bigram.txt");
    let _ = std::fs::remove_dir_all(&dir);
    {
        let lm = UserLM::open_or_create(&path);
        assert!(!path.exists(), "首次运行不应创建文件");
        for _ in 0..3 {
            lm.learn(Some("今天"), "天气");
        }
        // 强制落盘(未达阈值也应能写:退出兜底路径)
        lm.flush().expect("flush 不应失败");
        assert!(path.is_file(), "flush 后文件应存在");
    }
    // 模拟崩溃残留:临时文件不应影响正式文件
    std::fs::write(dir.join("user_bigram.txt.tmp.99999"), b"garbage").unwrap();
    {
        let lm = UserLM::open_or_create(&path);
        let snap = lm.snapshot();
        let with_prev = lm.counts_with(&snap, Some("今天"), "天气");
        let unknown = lm.counts_with(&snap, None, "没学过的词");
        assert!(
            with_prev.0 > 0 && with_prev.1 > 0 && unknown == (0, 0),
            "重载后学习效果应仍在: {with_prev:?} vs {unknown:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_and_corrupt_files_degrade_gracefully() {
    let dir = std::env::temp_dir().join(format!("xwx_userlm_bad_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // 缺失:不 panic、空模型
    let missing = dir.join("nope.txt");
    let lm = UserLM::open_or_create(&missing);
    assert_eq!(lm.unigram_len(), 0);
    // 损坏:格式错误的行 → 告警后空模型,不 panic
    std::fs::create_dir_all(&dir).unwrap();
    let bad = dir.join("bad.txt");
    std::fs::write(&bad, "好\t不是数字\n两\t个\t字段\t太多\n").unwrap();
    let lm = UserLM::open_or_create(&bad);
    assert_eq!(lm.unigram_len(), 0, "损坏文件应退化为空模型");
    // 损坏文件仍可被下次 flush 覆盖重写
    lm.learn(None, "好");
    lm.flush().expect("损坏后 flush 应能重写");
    let lm = UserLM::open_or_create(&bad);
    assert_eq!(lm.unigram_len(), 1, "重写后应能正常加载");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn entry_cap_triggers_decay() {
    let lm = UserLM::in_memory();
    // 一个高频词 + 顶满上限的一次性词
    for _ in 0..100 {
        lm.learn(None, "常用词");
    }
    for i in 0..MAX_USER_UNIGRAMS {
        lm.learn(None, &format!("长尾词{i}"));
    }
    // 再学一次 → 超过上限 → 衰减
    lm.learn(None, "触发衰减");
    let n = lm.unigram_len();
    assert!(n <= MAX_USER_UNIGRAMS, "衰减后条数应回到上限内: {n}");
    let snap = lm.snapshot();
    let (hot, _) = lm.counts_with(&snap, None, "常用词");
    let (cold, _) = lm.counts_with(&snap, None, "长尾词0");
    assert!(hot > cold, "高频词应在衰减后仍高于长尾: {hot} vs {cold}");
}

#[test]
fn dirty_threshold_gates_flush() {
    let dir = std::env::temp_dir().join(format!("xwx_userlm_dirty_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("u.txt");
    let lm = UserLM::open_or_create(&path);
    assert!(!lm.needs_flush());
    lm.learn(None, "甲");
    assert_eq!(lm.dirty_count(), 1);
    assert!(!lm.needs_flush(), "未达阈值不应要求落盘");
    for _ in 0..FLUSH_LEARN_THRESHOLD {
        lm.learn(None, "乙");
    }
    assert!(lm.needs_flush(), "达到阈值应要求落盘");
    lm.flush().expect("flush 失败");
    assert_eq!(lm.dirty_count(), 0);
    assert!(!lm.needs_flush());
    // 无新学习时 flush 是 no-op(文件 mtime 不变)
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));
    lm.flush().expect("无学习 flush 应直接跳过");
    let mtime2 = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(mtime, mtime2, "无学习不应重写文件");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn legacy_file_without_entries_section_loads_as_empty_entries() {
    // 向后兼容(验收判据 A6):造词功能引入**之前**的旧格式文件
    // (只有 bigram 计数行,没有造词分节标记)必须仍能正常加载,
    // 且词条表为空——与「那时还没有造词」的语义一致。
    let dir = std::env::temp_dir().join(format!("xwx_userlm_legacy_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("old.txt");
    // 手写一份旧格式(分节标记绝不出现):与引入造词前的 flush
    // 产物逐字节同构。
    std::fs::write(&path, "# xwx 用户词频学习模型\n你\t3\n你\t好\t2\n").unwrap();
    let lm = UserLM::open_or_create(&path);
    // bigram 行(你,好)的「好」不单独计 unigram:BigramLM 的
    // unigrams 只收「词<TAB>次数」行,去重后只有「你」一个。
    assert_eq!(lm.unigram_len(), 1, "旧文件的计数区应正常加载");
    assert_eq!(lm.entry_len(), 0, "旧文件没有造词区,词条表应为空");
    assert!(lm.snapshot().lookup_entries("ni hao").is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn entries_section_survives_roundtrip() {
    // 新格式:bigram 区 + 造词区同一份文件,写出去再读回来两段都在。
    let dir = std::env::temp_dir().join(format!("xwx_userlm_rt_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("rt.txt");
    {
        let lm = UserLM::open_or_create(&path);
        lm.learn(Some("今天"), "天气");
        lm.add_entry("张三丰", "zhang san feng");
        lm.add_entry("张叁丰", "zhang san feng"); // 同 key 不同词
        lm.flush().expect("flush 不应失败");
    }
    {
        let lm = UserLM::open_or_create(&path);
        // learn(Some("今天"), "天气") 记 unigram(天气) + bigram(今天,天气):
        // unigram key 去重后只有「天气」一个。
        assert_eq!(lm.unigram_len(), 1, "计数区应读回");
        let snap = lm.snapshot();
        let got = snap.lookup_entries("zhang san feng");
        assert_eq!(got.len(), 2, "同一 key 的两个词都应读回");
        assert!(got.contains(&"张三丰".to_string()) && got.contains(&"张叁丰".to_string()),
            "两个词都应在: {got:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn add_entry_is_idempotent_per_word_key() {
    // 同一 (word, key) 重复记不重复:用户反复上屏同一自造词
    // 只留一份(排序增益由 unigram 计数表达,不靠词条重复)。
    let lm = UserLM::in_memory();
    for _ in 0..3 {
        lm.add_entry("张三丰", "zhang san feng");
    }
    assert_eq!(lm.entry_len(), 1);
    assert_eq!(lm.snapshot().lookup_entries("zhang san feng").len(), 1);
}

// —— 屏蔽表(Ctrl+Delete 删候选)——

#[test]
fn block_candidate_marks_blocked_and_removes_user_entry() {
    // 核心判据:屏蔽一条候选后,该 (key, word) 被标记;若它是用户
    // 造词条目,词条同时被删(连根拔);若是词库词(entries 里没有),
    // 只屏蔽、不影响任何词条。
    let lm = UserLM::in_memory();
    lm.add_entry("自造短语", "zi zao duan yu");
    assert_eq!(lm.entry_len(), 1);

    // 词库词:entries 里没有,屏蔽只进 blocklist,不动词条
    lm.block_candidate("词库词", "ci ku ci");
    assert!(lm.snapshot().is_blocked("ci ku ci", "词库词"));
    assert_eq!(lm.entry_len(), 1, "词库词的屏蔽不应动造词条目");

    // 用户造词:屏蔽 + 删词条
    lm.block_candidate("自造短语", "zi zao duan yu");
    assert!(lm.snapshot().is_blocked("zi zao duan yu", "自造短语"));
    assert_eq!(lm.entry_len(), 0, "用户造词应连同词条一起删掉");
    assert!(
        lm.snapshot().lookup_entries("zi zao duan yu").is_empty(),
        "删掉后按 key 查不到该词条"
    );
    assert_eq!(lm.blocklist_len(), 2);
}

#[test]
fn block_candidate_is_idempotent() {
    // 重复屏蔽同一条不重复计(幂等,与 add_entry 同一取舍)。
    let lm = UserLM::in_memory();
    lm.block_candidate("词", "ci");
    lm.block_candidate("词", "ci");
    assert_eq!(lm.blocklist_len(), 1);
    assert!(lm.snapshot().is_blocked("ci", "词"));
}

#[test]
fn block_candidate_clears_counts_too() {
    // **语义变更（2026-10-02）**：旧断言是「屏蔽不动计数」，
    // 现在改成「屏蔽连计数一起清」，理由见 `block_candidate` 文档：
    // ① 屏蔽键是 (拼音, 词)，而计数按**词**记——不清的话同一个词
    //    在别的拼音路径上仍带着计数被加分（多音字串味）；
    // ② 计数就是排名的燃料，燃料不清等于没删。
    let lm = UserLM::in_memory();
    lm.learn(None, "某词");
    lm.learn(Some("某词"), "下一词");
    assert_eq!(lm.counts_with(&lm.snapshot(), None, "某词"), (1, 0));
    assert_eq!(lm.counts_with(&lm.snapshot(), Some("某词"), "下一词"), (1, 1));

    lm.block_candidate("某词", "mou ci");

    assert!(lm.snapshot().is_blocked("mou ci", "某词"), "仍要进屏蔽表");
    assert_eq!(
        lm.counts_with(&lm.snapshot(), None, "某词"),
        (0, 0),
        "被删的词一元计数应清零"
    );
    assert_eq!(
        lm.counts_with(&lm.snapshot(), Some("某词"), "下一词"),
        (1, 0),
        "指向它的二元计数应清掉(只剩「下一词」自己的一元计数)"
    );
    assert_eq!(
        lm.counts_with(&lm.snapshot(), Some("旁词"), "某词"),
        (0, 0),
        "任何上文下它都不该再有计数"
    );
}

#[test]
fn blocklist_survives_roundtrip() {
    // 落盘 + 读回:屏蔽表经 flush 写入 `# xwx-user-blocklist` 区,
    // 重开后仍生效;与计数区、造词区三段共存。
    let dir = std::env::temp_dir().join(format!("xwx_userlm_bl_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("bl.txt");
    {
        let lm = UserLM::open_or_create(&path);
        lm.learn(None, "今天");
        lm.add_entry("自造", "zi zao");
        lm.block_candidate("词库词", "ci ku ci");
        lm.block_candidate("自造", "zi zao"); // 同时屏蔽 + 删词条
        lm.flush().expect("flush 不应失败");
    }
    {
        let lm = UserLM::open_or_create(&path);
        assert_eq!(lm.unigram_len(), 1, "计数区应读回");
        assert_eq!(lm.entry_len(), 0, "自造词条已被删(屏蔽连根拔)");
        assert!(lm.snapshot().is_blocked("ci ku ci", "词库词"), "屏蔽表应读回");
        assert!(lm.snapshot().is_blocked("zi zao", "自造"), "屏蔽表应读回");
        assert!(!lm.snapshot().is_blocked("ci ku ci", "别的词"), "键含词,不误伤");
        assert!(!lm.snapshot().is_blocked("别的拼音", "词库词"), "键含拼音,不误伤");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn legacy_file_with_blocklist_section_loads() {
    // 向后兼容:含造词区但无屏蔽表区的「次新」文件(引入屏蔽表之前)
    // 正常加载,屏蔽表为空;手写一份该形态验证分节解析不回退。
    let dir = std::env::temp_dir().join(format!("xwx_userlm_leg2_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mid.txt");
    std::fs::write(
        &path,
        "# xwx 用户词频学习模型\n你\t3\n# xwx-user-entries\n自造\tzi zao\n",
    )
    .unwrap();
    let lm = UserLM::open_or_create(&path);
    assert_eq!(lm.unigram_len(), 1, "计数区应加载");
    assert_eq!(lm.entry_len(), 1, "造词区应加载");
    assert_eq!(lm.blocklist_len(), 0, "无屏蔽表区时屏蔽表为空");
    assert!(!lm.snapshot().is_blocked("zi zao", "自造"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 写路径必须是「取表—改—发布」的**原子**操作(codex 审查 D1,P0)。
///
/// 旧实现用 `mem::take` 把共享表换成空表再释放锁:窗口里别的线程读到空模型
/// (屏蔽项、计数全消失),两个写者还会互相覆盖。这里让 4 个写者并发 learn
/// (词有重叠,丢更新会体现为总数变少),读者同时断言屏蔽项从不消失。
#[test]
fn concurrent_writes_never_publish_an_empty_table() {
    use std::sync::{Arc as StdArc, Barrier};

    let lm = StdArc::new(UserLM::in_memory());
    lm.block_candidate("测试词", "ce shi ci");
    assert_eq!(lm.blocklist_len(), 1);

    let writers = 4usize;
    let per_writer = 500usize;
    let words = 32usize;
    let barrier = StdArc::new(Barrier::new(writers + 2));
    let mut handles = Vec::new();

    for w in 0..writers {
        let lm = StdArc::clone(&lm);
        let barrier = StdArc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            for i in 0..per_writer {
                lm.learn(None, &format!("词{}", (w * 7 + i) % words));
            }
        }));
    }
    {
        let lm = StdArc::clone(&lm);
        let barrier = StdArc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..200_000 {
                assert_eq!(
                    lm.blocklist_len(),
                    1,
                    "写路径的窗口里发布了空表:屏蔽项短暂消失"
                );
            }
        }));
    }
    barrier.wait();
    for h in handles {
        h.join().expect("线程 panic");
    }

    let snap = lm.snapshot();
    let total: u64 = (0..words)
        .map(|i| lm.counts_with(&snap, None, &format!("词{i}")).0 as u64)
        .sum();
    assert_eq!(
        total,
        (writers * per_writer) as u64,
        "并发学习丢了更新:计数总数少于学习次数"
    );
    assert_eq!(lm.blocklist_len(), 1);
}

/// 落盘后再载入必须完整(codex 审查 D4 的基本面)。
#[test]
fn flush_then_reload_keeps_every_count() {
    let dir = std::env::temp_dir().join(format!("xwx_userlm_rt2_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录失败");
    let path = dir.join("user_bigram.txt");

    let lm = UserLM::open_or_create(&path);
    for _ in 0..3 {
        lm.learn(None, "我们");
        lm.learn(Some("我们"), "今天");
    }
    lm.add_entry("我们今天", "wo men jin tian");
    lm.block_candidate("测试词", "ce shi ci");
    lm.flush().expect("落盘失败");
    assert_eq!(lm.dirty_count(), 0, "落盘成功后应清零");

    let reloaded = UserLM::open_or_create(&path);
    let snap = reloaded.snapshot();
    assert_eq!(reloaded.counts_with(&snap, None, "我们"), (3, 0));
    // 一元计数对 prev/word 各自加(learn 给词本身记频,bigram 另记)
    assert_eq!(reloaded.counts_with(&snap, Some("我们"), "今天"), (3, 3));
    assert_eq!(reloaded.blocklist_len(), 1);

    // 没有新学习时 flush 是空操作(空写只徒增 SSD 磨损)。
    reloaded.flush().expect("空 flush 不应失败");
    assert_eq!(reloaded.dirty_count(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 落盘**失败**时这一代必须还是脏的(不能静默丢):
/// 把路径的父目录指成一个普通文件,`create_dir_all` 必失败。
#[test]
fn failed_flush_keeps_the_generation_dirty() {
    let dir = std::env::temp_dir().join(format!("xwx_userlm_fail_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录失败");
    let blocker = dir.join("not-a-dir");
    std::fs::write(&blocker, b"x").expect("写文件失败");
    let path = blocker.join("user_bigram.txt");

    let lm = UserLM::open_or_create(&path);
    lm.learn(None, "我们");
    assert_eq!(lm.dirty_count(), 1);
    assert!(lm.flush().is_err(), "父目录是文件,落盘必须失败");
    assert_eq!(lm.dirty_count(), 1, "失败后这一代必须还回去,不能丢");

    let _ = std::fs::remove_dir_all(&dir);
}
