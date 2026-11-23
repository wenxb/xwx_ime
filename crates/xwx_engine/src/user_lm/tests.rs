//! `user_lm` 模块单测。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Barrier;

use super::*;

#[test]
fn learn_produces_counts_for_committed_word() {
    let lm = UserLM::in_memory();
    assert_eq!(lm.counts_with(&lm.snapshot(), None, "斯巴克"), (0, 0));
    for _ in 0..5 {
        lm.learn(None, "斯巴克");
        lm.learn(Some("中"), "国");
    }
    let snap = lm.snapshot();
    assert_eq!(lm.counts_with(&snap, None, "斯巴克"), (5, 0));
    assert_eq!(lm.counts_with(&snap, Some("中"), "国"), (5, 5));
    assert_eq!(lm.counts_with(&snap, None, "国"), (5, 0));
    assert_eq!(lm.counts_with(&snap, Some("今天"), "国"), (5, 0));
}

#[test]
fn persistence_roundtrip_keeps_all_three_sections() {
    let dir = std::env::temp_dir().join(format!("xwx_userlm_rt_{}", std::process::id()));
    let path = dir.join("user_bigram.txt");
    let _ = std::fs::remove_dir_all(&dir);
    {
        let lm = UserLM::open_or_create(&path);
        assert!(!path.exists(), "首次运行不应创建文件");
        for _ in 0..3 {
            lm.learn(None, "我们");
            lm.learn(Some("我们"), "今天");
        }
        lm.add_entry("张三丰", "zhang san feng");
        lm.add_entry("张叁丰", "zhang san feng");
        lm.add_entry("自造", "zi zao");
        lm.block_candidate("词库词", "ci ku ci");
        lm.block_candidate("自造", "zi zao"); // 屏蔽同时删造词条目
        lm.flush().expect("flush 不应失败");
        assert!(path.is_file());
        assert_eq!(lm.dirty_count(), 0);
    }
    // 崩溃残留 .tmp 不影响正式文件重载
    std::fs::write(dir.join("user_bigram.txt.tmp.99999"), b"garbage").unwrap();
    {
        let lm = UserLM::open_or_create(&path);
        let snap = lm.snapshot();
        assert_eq!(lm.counts_with(&snap, None, "我们"), (3, 0));
        assert_eq!(lm.counts_with(&snap, Some("我们"), "今天"), (3, 3));
        assert_eq!(lm.counts_with(&snap, None, "没学过的词"), (0, 0));

        let got = snap.lookup_entries("zhang san feng");
        assert_eq!(got.len(), 2);
        assert!(got.contains(&"张三丰".to_string()) && got.contains(&"张叁丰".to_string()));
        assert!(snap.lookup_entries("zi zao").is_empty());

        assert_eq!(lm.blocklist_len(), 2);
        assert!(snap.is_blocked("ci ku ci", "词库词"));
        assert!(snap.is_blocked("zi zao", "自造"));
        assert!(!snap.is_blocked("ci ku ci", "别的词"));
        assert!(!snap.is_blocked("别的拼音", "词库词"));

        // 无新学习时 flush 是空操作
        lm.flush().expect("空 flush 不应失败");
        assert_eq!(lm.dirty_count(), 0);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn entry_cap_triggers_decay() {
    let lm = UserLM::in_memory();
    for _ in 0..100 {
        lm.learn(None, "常用词");
    }
    for i in 0..MAX_USER_UNIGRAMS {
        lm.learn(None, &format!("长尾词{i}"));
    }
    lm.learn(None, "触发衰减");
    let n = lm.unigram_len();
    assert!(n <= MAX_USER_UNIGRAMS, "衰减后条数应回到上限内: {n}");
    let snap = lm.snapshot();
    let (hot, _) = lm.counts_with(&snap, None, "常用词");
    let (cold, _) = lm.counts_with(&snap, None, "长尾词0");
    assert!(hot > cold, "高频词衰减后仍应高于长尾: {hot} vs {cold}");
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
    assert!(!lm.needs_flush());
    for _ in 0..FLUSH_LEARN_THRESHOLD {
        lm.learn(None, "乙");
    }
    assert!(lm.needs_flush());
    lm.flush().expect("flush 失败");
    assert_eq!(lm.dirty_count(), 0);
    assert!(!lm.needs_flush());

    // 无新学习时 flush 是 no-op（文件 mtime 不变）
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));
    lm.flush().expect("无学习 flush 应直接跳过");
    assert_eq!(mtime, std::fs::metadata(&path).unwrap().modified().unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn legacy_files_without_newer_sections_load_cleanly() {
    let dir = std::env::temp_dir().join(format!("xwx_userlm_legacy_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 仅计数区（引入造词前）
    let v1 = dir.join("v1.txt");
    std::fs::write(&v1, "# xwx 用户词频学习模型\n你\t3\n你\t好\t2\n").unwrap();
    let lm1 = UserLM::open_or_create(&v1);
    assert_eq!(lm1.unigram_len(), 1);
    assert_eq!(lm1.entry_len(), 0);
    assert!(lm1.snapshot().lookup_entries("ni hao").is_empty());

    // 计数区 + 造词区，无屏蔽表区（引入屏蔽表前）
    let v2 = dir.join("v2.txt");
    std::fs::write(&v2, "# xwx 用户词频学习模型\n你\t3\n# xwx-user-entries\n自造\tzi zao\n").unwrap();
    let lm2 = UserLM::open_or_create(&v2);
    assert_eq!(lm2.unigram_len(), 1);
    assert_eq!(lm2.entry_len(), 1);
    assert_eq!(lm2.blocklist_len(), 0);
    assert!(!lm2.snapshot().is_blocked("zi zao", "自造"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn entries_and_block_candidate_behavior_and_idempotency() {
    let lm = UserLM::in_memory();
    for _ in 0..3 {
        lm.add_entry("自造短语", "zi zao duan yu");
    }
    assert_eq!(lm.entry_len(), 1);
    assert_eq!(lm.snapshot().lookup_entries("zi zao duan yu").len(), 1);

    // 词库词：只进屏蔽表、不动造词条目，且重复屏蔽幂等
    lm.block_candidate("词库词", "ci ku ci");
    lm.block_candidate("词库词", "ci ku ci");
    assert!(lm.snapshot().is_blocked("ci ku ci", "词库词"));
    assert_eq!(lm.entry_len(), 1);
    assert_eq!(lm.blocklist_len(), 1);

    // 用户造词：屏蔽并连同造词条目一起删除
    lm.block_candidate("自造短语", "zi zao duan yu");
    assert!(lm.snapshot().is_blocked("zi zao duan yu", "自造短语"));
    assert_eq!(lm.entry_len(), 0);
    assert!(lm.snapshot().lookup_entries("zi zao duan yu").is_empty());
    assert_eq!(lm.blocklist_len(), 2);
}

#[test]
fn block_candidate_clears_counts_too() {
    let lm = UserLM::in_memory();
    lm.learn(None, "某词");
    lm.learn(Some("某词"), "下一词");
    lm.learn(Some("旁词"), "某词");
    assert_eq!(lm.counts_with(&lm.snapshot(), None, "某词"), (2, 0));
    assert_eq!(lm.counts_with(&lm.snapshot(), Some("某词"), "下一词"), (1, 1));

    lm.block_candidate("某词", "mou ci");

    let snap = lm.snapshot();
    assert!(snap.is_blocked("mou ci", "某词"));
    assert_eq!(lm.counts_with(&snap, None, "某词"), (0, 0));
    assert_eq!(lm.counts_with(&snap, Some("某词"), "下一词"), (1, 0));
    assert_eq!(lm.counts_with(&snap, Some("旁词"), "某词"), (0, 0));
}

#[test]
fn unblock_and_unblock_last_behavior() {
    let lm = UserLM::in_memory();
    assert!(!lm.unblock_last());

    lm.add_entry("自造短语", "zi zao duan yu");
    lm.block_candidate("自造短语", "zi zao duan yu");
    lm.block_candidate("词库词", "ci ku ci");
    assert_eq!(lm.blocklist_len(), 2);

    // unblock_last 只撤销最近一条，槽位用完后返回 false
    assert!(lm.unblock_last());
    assert!(!lm.snapshot().is_blocked("ci ku ci", "词库词"));
    assert!(lm.snapshot().is_blocked("zi zao duan yu", "自造短语"));
    assert!(!lm.unblock_last());
    assert_eq!(lm.blocklist_len(), 1);

    // unblock 指名解屏蔽且幂等
    lm.unblock("自造短语", "zi zao duan yu");
    lm.unblock("自造短语", "zi zao duan yu");
    assert!(!lm.snapshot().is_blocked("zi zao duan yu", "自造短语"));
    assert_eq!(lm.blocklist_len(), 0);
}

#[test]
fn empty_blocklist_fast_path_tracks_the_snapshot_generation() {
    let user = UserLM::in_memory();
    let empty = user.snapshot();
    assert!(!empty.is_blocked("ni", "你"));
    user.block_candidate("你", "ni");
    let blocked = user.snapshot();
    assert!(!empty.is_blocked("ni", "你"));
    assert!(blocked.is_blocked("ni", "你"));
    assert!(!blocked.is_blocked("ni", "妮"));
    user.unblock("你", "ni");
    assert!(!user.snapshot().is_blocked("ni", "你"));
    assert!(blocked.is_blocked("ni", "你"));
}

#[test]
fn missing_and_corrupt_user_files_never_overwrite_original() {
    let dir = std::env::temp_dir().join(format!("xwx_userlm_corrupt_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let missing = UserLM::open_or_create(&dir.join("nope.txt"));
    assert_eq!(missing.unigram_len(), 0);
    assert!(!missing.is_read_only());

    std::fs::create_dir_all(&dir).expect("建目录失败");
    let cases = [
        ("counts", "# xwx\n我们\tnot-a-number\n"),
        ("entries", "# xwx\n# xwx-user-entries\n坏行没有制表符\n"),
        ("blocklist", "# xwx\n# xwx-user-blocklist\n坏行没有制表符\n"),
    ];
    for (tag, content) in cases {
        let path = dir.join(format!("{tag}.txt"));
        std::fs::write(&path, content).expect("写坏文件失败");
        let before = std::fs::read(&path).expect("读坏文件失败");

        let lm = UserLM::open_or_create(&path);
        assert_eq!(lm.unigram_len(), 0, "{tag}: 损坏文件应退化为空模型");
        assert!(lm.is_read_only(), "{tag}: 损坏文件必须进只读恢复态");

        lm.learn(None, "新词");
        lm.add_entry("新词", "xin ci");
        lm.block_candidate("新词", "xin ci");
        lm.flush().expect("只读态 flush 不该报错");
        assert_eq!(std::fs::read(&path).unwrap(), before, "{tag}: 只读态不得覆盖原文件");

        // 显式恢复后留备份并允许重新落盘
        let backup = lm.recover_corrupt_file().expect("恢复失败").expect("应留下备份");
        assert_eq!(std::fs::read(&backup).unwrap(), before, "{tag}: 备份必须是原件字节");
        assert!(!lm.is_read_only());
        lm.flush().expect("恢复后应能回写");
        assert!(std::fs::read_to_string(&path).unwrap().contains("新词"));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn failed_flush_keeps_the_generation_dirty() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("xwx_userlm_fail_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录失败");
    let path = dir.join("user_bigram.txt");
    let lm = UserLM::open_or_create(&path);
    lm.learn(None, "我们");
    assert_eq!(lm.dirty_count(), 1);

    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).expect("chmod 失败");
    let result = lm.flush();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod 恢复失败");
    if result.is_ok() {
        let _ = std::fs::remove_dir_all(&dir);
        return; // root 无视权限位
    }
    assert_eq!(lm.dirty_count(), 1, "失败后这一代必须还回去");
    lm.flush().expect("恢复可写后应能补上");
    assert_eq!(lm.dirty_count(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unreadable_user_file_is_not_truncated() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("xwx_userlm_perm_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录失败");
    let path = dir.join("user_bigram.txt");
    std::fs::write(&path, "我们\t3\n").expect("写文件失败");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod 失败");
    if std::fs::read(&path).is_err() {
        let lm = UserLM::open_or_create(&path);
        assert!(lm.is_read_only(), "读不出来必须进只读态");
        lm.learn(None, "新词");
        lm.flush().expect("只读态 flush 不该报错");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), "我们\t3\n".len() as u64);
    }
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn concurrent_writes_never_publish_an_empty_table() {
    let lm = Arc::new(UserLM::in_memory());
    lm.block_candidate("测试词", "ce shi ci");
    assert_eq!(lm.blocklist_len(), 1);

    let (writers, per_writer, words) = (4usize, 500usize, 32usize);
    let barrier = Arc::new(Barrier::new(writers + 2));
    let done = Arc::new(AtomicBool::new(false));
    let mut writer_handles = Vec::with_capacity(writers);

    for w in 0..writers {
        let lm = Arc::clone(&lm);
        let barrier = Arc::clone(&barrier);
        writer_handles.push(std::thread::spawn(move || {
            barrier.wait();
            for i in 0..per_writer {
                lm.learn(None, &format!("词{}", (w * 7 + i) % words));
            }
        }));
    }
    let reader = {
        let lm = Arc::clone(&lm);
        let barrier = Arc::clone(&barrier);
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            barrier.wait();
            while !done.load(Ordering::Acquire) {
                assert_eq!(lm.blocklist_len(), 1, "写路径窗口不得发布空表");
                std::hint::spin_loop();
            }
        })
    };

    barrier.wait();
    for h in writer_handles {
        h.join().expect("写线程 panic");
    }
    done.store(true, Ordering::Release);
    reader.join().expect("读线程 panic");

    let snap = lm.snapshot();
    let total: u64 = (0..words)
        .map(|i| lm.counts_with(&snap, None, &format!("词{i}")).0 as u64)
        .sum();
    assert_eq!(total, (writers * per_writer) as u64, "并发学习不得丢更新");
    assert_eq!(lm.blocklist_len(), 1);
}

#[test]
fn flush_with_concurrent_learning_preserves_all_counts_after_final_flush() {
    let path = std::env::temp_dir().join(format!(
        "xwx_userlm_concurrent_flush_{}.txt",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let user = Arc::new(UserLM::open_or_create(&path));
    let barrier = Arc::new(Barrier::new(2));
    let writer = Arc::clone(&user);
    let writer_barrier = Arc::clone(&barrier);
    let thread = std::thread::spawn(move || {
        writer_barrier.wait();
        for _ in 0..2000 {
            writer.learn(Some("我们"), "今天");
        }
    });
    barrier.wait();
    for _ in 0..20 {
        user.flush().unwrap();
    }
    thread.join().unwrap();
    user.flush().unwrap();
    assert_eq!(user.dirty_count(), 0);
    let loaded = UserLM::open_or_create(&path);
    assert_eq!(loaded.counts_with(&loaded.snapshot(), Some("我们"), "今天"), (2000, 2000));
    std::fs::remove_file(path).unwrap();
}
