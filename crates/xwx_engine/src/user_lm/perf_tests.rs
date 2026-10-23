use super::*;

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
fn flush_with_concurrent_learning_preserves_all_counts_after_final_flush() {
    let path =
        std::env::temp_dir().join(format!("xwx_flush_concurrent_{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let user = Arc::new(UserLM::open_or_create(&path));
    let barrier = Arc::new(std::sync::Barrier::new(2));
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
    assert_eq!(
        loaded.counts_with(&loaded.snapshot(), Some("我们"), "今天"),
        (2000, 2000)
    );
    std::fs::remove_file(path).unwrap();
}
