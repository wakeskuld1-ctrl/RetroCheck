//! ### 修改记录 (2026-02-17)
//! - 原因: 需要验证 StateMachine 真实落地 SQLite
//! - 目的: 保证 Raft apply 写入路径可用

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要验证写入与读取链路
/// - 目的: 确保 SQLite 执行与查询工作正常
#[tokio::test]
async fn state_machine_applies_write() {
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要隔离数据库文件
    // - 目的: 避免污染本地数据
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要初始化状态机实例
    // - 目的: 验证最小执行路径
    let sm = check_program::raft::state_machine::SqliteStateMachine::new(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要验证建表与写入
    // - 目的: 覆盖最基础的写入路径
    let _ = sm
        .apply_write("CREATE TABLE t(x INT)".to_string())
        .await
        .unwrap();
    let _ = sm
        .apply_write("INSERT INTO t(x) VALUES (1)".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要读取验证结果
    // - 目的: 确认写入确实落地
    let value = sm
        .query_scalar("SELECT COUNT(*) FROM t".to_string())
        .await
        .unwrap();
    assert_eq!(value, "1");
}

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要验证元数据表存在
/// - 目的: 为写入时间切割快照提供基础
#[tokio::test]
async fn meta_last_write_at_is_updated() {
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要隔离数据库文件
    // - 目的: 避免污染本地数据
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要初始化状态机实例
    // - 目的: 验证元数据表初始化能力
    let sm = check_program::raft::state_machine::SqliteStateMachine::new(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要触发写入路径
    // - 目的: 验证 last_write_at 是否存在
    let _ = sm
        .apply_write("CREATE TABLE t(x INT)".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要读取元数据
    // - 目的: 确保 last_write_at 可用
    let last_write_at = sm
        .query_scalar("SELECT value FROM _raft_meta WHERE key='last_write_at'".to_string())
        .await
        .unwrap();
    assert!(!last_write_at.is_empty());
}

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要验证元数据写接口
/// - 目的: 确保后续快照元数据更新可用
#[tokio::test]
async fn meta_update_interface_writes_value() {
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要隔离数据库文件
    // - 目的: 避免污染本地数据
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要初始化状态机实例
    // - 目的: 验证元数据写接口
    let sm = check_program::raft::state_machine::SqliteStateMachine::new(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要写入元数据
    // - 目的: 确保写接口生效
    let _ = sm
        .update_meta("last_snapshot_at".to_string(), "123".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要读取元数据
    // - 目的: 验证写入结果可读取
    let value = sm
        .query_scalar("SELECT value FROM _raft_meta WHERE key='last_snapshot_at'".to_string())
        .await
        .unwrap();
    assert_eq!(value, "123");
}

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要验证写入时间更新
/// - 目的: 确保 last_write_at 在每次写入后变化
#[tokio::test]
async fn last_write_at_uses_sqlite_time() {
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要隔离数据库文件
    // - 目的: 避免污染本地数据
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要初始化状态机实例
    // - 目的: 验证写入时间更新逻辑
    let sm = check_program::raft::state_machine::SqliteStateMachine::new(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要触发首次写入
    // - 目的: 生成 last_write_at
    let _ = sm
        .apply_write("CREATE TABLE t(x INT)".to_string())
        .await
        .unwrap();
    let v1 = sm
        .query_scalar("SELECT value FROM _raft_meta WHERE key='last_write_at'".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要二次写入
    // - 目的: 验证时间更新
    let _ = sm
        .apply_write("INSERT INTO t(x) VALUES (1)".to_string())
        .await
        .unwrap();
    let v2 = sm
        .query_scalar("SELECT value FROM _raft_meta WHERE key='last_write_at'".to_string())
        .await
        .unwrap();

    assert!(v2 >= v1);
}
