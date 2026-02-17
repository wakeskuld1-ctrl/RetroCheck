//! ### 修改记录 (2026-02-17)
//! - 原因: 需要最小集成测试覆盖多节点场景
//! - 目的: 验证多节点独立写入与重启恢复

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要覆盖三节点写入与重启
/// - 目的: 为后续真正 Raft 集成预留测试结构
#[tokio::test]
async fn three_nodes_write_and_failover() {
    let dir = tempfile::tempdir().unwrap();
    let node1 = dir.path().join("node1.db");
    let node2 = dir.path().join("node2.db");
    let node3 = dir.path().join("node3.db");

    let router1 =
        check_program::raft::router::Router::new_local_leader(node1.to_string_lossy().to_string())
            .unwrap();
    let router2 =
        check_program::raft::router::Router::new_local_leader(node2.to_string_lossy().to_string())
            .unwrap();
    let router3 =
        check_program::raft::router::Router::new_local_leader(node3.to_string_lossy().to_string())
            .unwrap();

    let create_sql = "CREATE TABLE IF NOT EXISTS accounts (id TEXT, version INTEGER)".to_string();
    let insert_sql = "INSERT INTO accounts (id, version) VALUES ('u1', 1)".to_string();

    let _ = router1.write(create_sql.clone()).await.unwrap();
    let _ = router1.write(insert_sql.clone()).await.unwrap();
    let _ = router2.write(create_sql.clone()).await.unwrap();
    let _ = router2.write(insert_sql.clone()).await.unwrap();
    let _ = router3.write(create_sql.clone()).await.unwrap();
    let _ = router3.write(insert_sql.clone()).await.unwrap();

    let v1 = router1.get_version("accounts".to_string()).await.unwrap();
    let v2 = router2.get_version("accounts".to_string()).await.unwrap();
    let v3 = router3.get_version("accounts".to_string()).await.unwrap();

    assert_eq!(v1, 1);
    assert_eq!(v2, 1);
    assert_eq!(v3, 1);

    drop(router1);
    let router1_restart =
        check_program::raft::router::Router::new_local_leader(node1.to_string_lossy().to_string())
            .unwrap();
    let v1_restart = router1_restart
        .get_version("accounts".to_string())
        .await
        .unwrap();
    assert_eq!(v1_restart, 1);
}
