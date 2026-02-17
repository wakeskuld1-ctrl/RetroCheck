//! ### 修改记录 (2026-02-17)
//! - 原因: 需要验证最小集群写入与 failover
//! - 目的: 以测试驱动集群骨架能力

use check_program::raft::raft_node::TestCluster;

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要验证 leader 切换后仍可写入
/// - 目的: 为后续真实 Raft failover 打基础
#[tokio::test]
async fn three_nodes_write_and_failover() {
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要最小三节点集群
    // - 目的: 验证基础 failover 路径
    let mut cluster = TestCluster::new(3).await.unwrap();

    cluster
        .write("CREATE TABLE IF NOT EXISTS t(x INT)".to_string())
        .await
        .unwrap();
    cluster
        .write("INSERT INTO t(x) VALUES (1)".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要模拟 leader 下线
    // - 目的: 驱动 failover 逻辑
    cluster.fail_leader().await.unwrap();

    cluster
        .write("INSERT INTO t(x) VALUES (1)".to_string())
        .await
        .unwrap();

    let value = cluster
        .query_leader_scalar("SELECT COUNT(*) FROM t".to_string())
        .await
        .unwrap();
    assert_eq!(value, "2");
}
