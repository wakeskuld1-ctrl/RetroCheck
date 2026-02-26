//! ### 修改记录 (2026-02-17)
//! - 原因: 需要验证最小读路径
//! - 目的: 为后续强一致读打下基础
//! ### 修改记录 (2026-02-17)
//! - 原因: 需要记录快照相关潜在问题
//! - 目的: 为后续验证提供清单
//! - **问题**: VACUUM INTO 可能受锁影响失败
//! - **建议测试**: 并发读写时触发快照，验证错误可被捕获与重试
//! - **问题**: 秒级时间戳可能导致同秒重复快照
//! - **建议测试**: 连续触发两次调度，检查文件名与元数据一致性
//! - **问题**: last_snapshot_index 依赖外部输入
//! - **建议测试**: 注入不同索引值，验证元数据与恢复流程一致

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要验证 get_version
/// - 目的: 确保读路径能够访问本地 SQLite
#[tokio::test]
async fn read_get_version_uses_state_machine() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");
    let router = check_program::raft::router::Router::new_local_leader(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap();

    let _ = router
        .write("CREATE TABLE IF NOT EXISTS accounts (id TEXT, balance INTEGER)".to_string())
        .await
        .unwrap();
    let _ = router
        .write("INSERT INTO accounts (id, balance) VALUES ('u1', 100)".to_string())
        .await
        .unwrap();

    let version = router.get_version("accounts".to_string()).await.unwrap();
    assert_eq!(version, 2);
}

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要验证快照调度触发
/// - 目的: 覆盖 VACUUM INTO 路径
#[tokio::test]
async fn snapshot_created_when_time_exceeds_window() {
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要隔离临时目录
    // - 目的: 避免污染本地数据
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");
    let snapshot_dir = dir.path().join("snapshots");
    std::fs::create_dir_all(&snapshot_dir).unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要本地 leader 路由
    // - 目的: 触发写入与快照逻辑
    let router = check_program::raft::router::Router::new_local_leader(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要创建表
    // - 目的: 触发写入时间更新
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要满足 clippy let_unit_value
    // - 目的: 移除无意义绑定并保持语义
    router
        .write("CREATE TABLE t(x INT)".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要模拟超时
    // - 目的: 强制触发快照
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要满足 clippy let_unit_value
    // - 目的: 移除无意义绑定并保持语义
    router
        .update_meta("last_snapshot_at".to_string(), "0".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要创建调度器
    // - 目的: 执行一次触发检查
    let scheduler = check_program::raft::snapshot::SnapshotScheduler::new(
        router,
        snapshot_dir.to_string_lossy().to_string(),
        60 * 60 * 24,
        1000,
    );

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要执行一次调度
    // - 目的: 生成快照文件
    scheduler.tick_once().await.unwrap();

    let entries = std::fs::read_dir(&snapshot_dir).unwrap().count();
    assert!(entries > 0);
}

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要端到端验证快照元数据
/// - 目的: 确保快照生成后元数据更新
#[tokio::test]
async fn snapshot_updates_meta_after_creation() {
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要隔离临时目录
    // - 目的: 避免污染本地数据
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");
    let snapshot_dir = dir.path().join("snapshots");
    std::fs::create_dir_all(&snapshot_dir).unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要本地 leader 路由
    // - 目的: 触发写入与快照逻辑
    let router = check_program::raft::router::Router::new_local_leader(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要触发写入
    // - 目的: 生成 last_write_at
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要满足 clippy let_unit_value
    // - 目的: 移除无意义绑定并保持语义
    router
        .write("CREATE TABLE t(x INT)".to_string())
        .await
        .unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要满足 clippy let_unit_value
    // - 目的: 移除无意义绑定并保持语义
    router
        .write("INSERT INTO t(x) VALUES (1)".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要模拟超时
    // - 目的: 强制触发快照
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要满足 clippy let_unit_value
    // - 目的: 移除无意义绑定并保持语义
    router
        .update_meta("last_snapshot_at".to_string(), "0".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要创建调度器
    // - 目的: 执行一次触发检查
    let scheduler = check_program::raft::snapshot::SnapshotScheduler::new(
        router,
        snapshot_dir.to_string_lossy().to_string(),
        60 * 60 * 24,
        12,
    );

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要执行一次调度
    // - 目的: 生成快照并更新元数据
    scheduler.tick_once().await.unwrap();

    let entries = std::fs::read_dir(&snapshot_dir).unwrap().count();
    assert!(entries > 0);
    let last_snapshot_at = scheduler.get_meta_u64("last_snapshot_at").await.unwrap();
    let last_snapshot_index = scheduler.get_meta_u64("last_snapshot_index").await.unwrap();
    assert!(last_snapshot_at > 0);
    assert_eq!(last_snapshot_index, 12);
}

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要验证 WAL 轮转
/// - 目的: 控制日志膨胀
#[tokio::test]
async fn wal_rotates_after_snapshot() {
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要隔离临时目录
    // - 目的: 避免污染本地数据
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");
    let snapshot_dir = dir.path().join("snapshots");
    std::fs::create_dir_all(&snapshot_dir).unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要本地 leader 路由
    // - 目的: 触发写入与快照逻辑
    let router = check_program::raft::router::Router::new_local_leader(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要触发写入
    // - 目的: 生成 WAL 文件
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要满足 clippy let_unit_value
    // - 目的: 移除无意义绑定并保持语义
    router
        .write("CREATE TABLE t(x INT)".to_string())
        .await
        .unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要满足 clippy let_unit_value
    // - 目的: 移除无意义绑定并保持语义
    router
        .write("INSERT INTO t(x) VALUES (1)".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要模拟超时
    // - 目的: 强制触发快照
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要满足 clippy let_unit_value
    // - 目的: 移除无意义绑定并保持语义
    router
        .update_meta("last_snapshot_at".to_string(), "0".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要创建调度器
    // - 目的: 执行一次触发检查
    let scheduler = check_program::raft::snapshot::SnapshotScheduler::new(
        router,
        snapshot_dir.to_string_lossy().to_string(),
        60 * 60 * 24,
        1,
    );

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要执行一次调度
    // - 目的: 生成快照并触发 WAL 轮转
    scheduler.tick_once().await.unwrap();

    let wal_path = dir.path().join("node.db.wal");
    let rotated_path = dir.path().join("node.db.wal.rotated");
    assert!(wal_path.exists());
    assert!(rotated_path.exists());
}

#[tokio::test]
async fn batch_write_is_atomic_on_error() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");
    let sm = check_program::raft::state_machine::SqliteStateMachine::new(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap();
    sm.apply_write("CREATE TABLE t(x INT)".to_string())
        .await
        .unwrap();

    let router = check_program::raft::router::Router::new_local_leader_with_batch(
        db_path.to_string_lossy().to_string(),
        check_program::raft::router::BatchConfig {
            max_batch_size: 2,
            max_delay_ms: 1000,
            max_queue_size: 10,
            max_wait_ms: 5000,
        },
    )
    .unwrap();

    let f1 = router.write("INSERT INTO t(x) VALUES (1)".to_string());
    let f2 = router.write("INSERT INTO t(not_a_col) VALUES (2)".to_string());
    let (r1, r2) = tokio::join!(f1, f2);
    assert!(r1.is_err());
    assert!(r2.is_err());

    let count = sm
        .query_scalar("SELECT COUNT(*) FROM t".to_string())
        .await
        .unwrap();
    assert_eq!(count, "0");
}

#[tokio::test]
async fn batch_queue_overflow_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");
    let _ = check_program::raft::state_machine::SqliteStateMachine::new(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap()
    .apply_write("CREATE TABLE t(x INT)".to_string())
    .await
    .unwrap();

    let router = check_program::raft::router::Router::new_local_leader_with_batch(
        db_path.to_string_lossy().to_string(),
        check_program::raft::router::BatchConfig {
            max_batch_size: 10,
            max_delay_ms: 1000,
            max_queue_size: 1,
            max_wait_ms: 5000,
        },
    )
    .unwrap();

    let f1 = router.write("INSERT INTO t(x) VALUES (1)".to_string());
    let f2 = router.write("INSERT INTO t(x) VALUES (2)".to_string());
    let (r1, r2) = tokio::join!(f1, f2);
    assert!(r1.is_ok());
    assert!(r2.is_err());
}

#[tokio::test]
async fn batch_wait_timeout_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");
    let _ = check_program::raft::state_machine::SqliteStateMachine::new(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap()
    .apply_write("CREATE TABLE t(x INT)".to_string())
    .await
    .unwrap();

    let router = check_program::raft::router::Router::new_local_leader_with_batch(
        db_path.to_string_lossy().to_string(),
        check_program::raft::router::BatchConfig {
            max_batch_size: 100,
            max_delay_ms: 1000,
            max_queue_size: 10,
            max_wait_ms: 1,
        },
    )
    .unwrap();

    let res = router
        .write("INSERT INTO t(x) VALUES (1)".to_string())
        .await;
    assert!(res.is_err());
}

#[tokio::test]
async fn batch_size_threshold_flushes_once() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");
    let sm = check_program::raft::state_machine::SqliteStateMachine::new(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap();
    sm.apply_write("CREATE TABLE t(x INT)".to_string())
        .await
        .unwrap();

    let router = check_program::raft::router::Router::new_local_leader_with_batch(
        db_path.to_string_lossy().to_string(),
        check_program::raft::router::BatchConfig {
            max_batch_size: 2,
            max_delay_ms: 10_000,
            max_queue_size: 10,
            max_wait_ms: 5000,
        },
    )
    .unwrap();

    let f1 = router.write("INSERT INTO t(x) VALUES (1)".to_string());
    let f2 = router.write("INSERT INTO t(x) VALUES (2)".to_string());
    let (r1, r2) = tokio::join!(f1, f2);
    assert!(r1.is_ok());
    assert!(r2.is_ok());

    let count = sm
        .query_scalar("SELECT COUNT(*) FROM t".to_string())
        .await
        .unwrap();
    assert_eq!(count, "2");
}

#[tokio::test]
async fn batch_timeout_cancels_and_skips_write() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");
    let sm = check_program::raft::state_machine::SqliteStateMachine::new(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap();
    sm.apply_write("CREATE TABLE t(x INT)".to_string())
        .await
        .unwrap();

    let router = check_program::raft::router::Router::new_local_leader_with_batch(
        db_path.to_string_lossy().to_string(),
        check_program::raft::router::BatchConfig {
            max_batch_size: 10,
            max_delay_ms: 200,
            max_queue_size: 10,
            max_wait_ms: 1,
        },
    )
    .unwrap();

    let res = router
        .write("INSERT INTO t(x) VALUES (1)".to_string())
        .await;
    assert!(res.is_err());

    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let count = sm
        .query_scalar("SELECT COUNT(*) FROM t".to_string())
        .await
        .unwrap();
    assert_eq!(count, "0");
}

#[tokio::test]
async fn non_leader_write_returns_error() {
    let router = check_program::raft::router::Router::new_for_test(false);
    let res = router
        .write("INSERT INTO t(x) VALUES (1)".to_string())
        .await;
    assert!(res.is_err());
}
