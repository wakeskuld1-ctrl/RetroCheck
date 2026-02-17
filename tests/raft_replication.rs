//! ### 修改记录 (2026-02-17)
//! - 原因: 需要验证最小复制路径
//! - 目的: 为网络适配提供测试驱动入口

use check_program::raft::network::TestNetwork;
use check_program::raft::raft_node::RaftNode;
use uuid::Uuid;

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要验证 follower 能应用 leader 写入
/// - 目的: 驱动最小复制能力的实现
#[tokio::test]
async fn raft_replication_to_follower() {
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要隔离各节点数据目录
    // - 目的: 避免测试间互相干扰
    let base_dir_leader =
        std::env::temp_dir().join(format!("raft_replication_leader_{}", Uuid::new_v4()));
    let base_dir_follower =
        std::env::temp_dir().join(format!("raft_replication_follower_{}", Uuid::new_v4()));
    let _leader = RaftNode::start_for_test(1, base_dir_leader).await.unwrap();
    let follower = RaftNode::start_for_test(2, base_dir_follower)
        .await
        .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要最小网络注册
    // - 目的: 模拟 leader 找到 follower
    let network = TestNetwork::new();
    network.register_node(follower);

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要在 follower 上应用写入
    // - 目的: 验证复制后数据落地
    network
        .replicate_sql_for_test(2, "CREATE TABLE IF NOT EXISTS t(x INT)".to_string())
        .await
        .unwrap();
    network
        .replicate_sql_for_test(2, "INSERT INTO t(x) VALUES (1)".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要验证 follower 状态
    // - 目的: 证明复制路径生效
    let follower_view = network
        .get_node_for_test(2)
        .expect("Follower should be registered");
    let value = follower_view
        .query_scalar_for_test("SELECT COUNT(*) FROM t".to_string())
        .await
        .unwrap_or_else(|_| "0".to_string());
    assert_eq!(value, "1");
}
