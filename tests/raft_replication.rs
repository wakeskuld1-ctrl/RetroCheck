//! ### 修改记录 (2026-02-17)
//! - 原因: 需要验证最小复制路径
//! - 目的: 为网络适配提供测试驱动入口

use check_program::raft::network::RaftRouter;
use check_program::raft::raft_node::RaftNode;
use uuid::Uuid;
use std::sync::Arc;
use std::time::Duration;
use openraft::BasicNode;

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

    // ### 修改记录 (2026-02-28)
    // - 原因: 需要共享路由器
    // - 目的: 让节点能够互相发现
    let router = RaftRouter::new();

    // Start nodes with shared router
    let leader = RaftNode::start(1, base_dir_leader, router.clone()).await.unwrap();
    let follower = RaftNode::start(2, base_dir_follower, router.clone()).await.unwrap();

    // Register nodes
    router.register(1, Arc::new(leader.clone()));
    router.register(2, Arc::new(follower.clone()));

    // Initialize leader
    let mut members = std::collections::BTreeSet::new();
    members.insert(1);
    leader.raft.initialize(members).await.unwrap();

    // Wait for leader
    loop {
        if leader.raft.metrics().borrow().state == openraft::ServerState::Leader {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Add follower as learner
    leader.raft.add_learner(2, BasicNode::new("127.0.0.1:0"), true).await.unwrap();
    // Add follower as voter
    leader.raft.change_membership(std::collections::BTreeSet::from([1, 2]), true).await.unwrap();

    // Wait for follower to become Follower
    loop {
        if follower.raft.metrics().borrow().state == openraft::ServerState::Follower {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要在 follower 上应用写入
    // - 目的: 验证复制后数据落地
    leader.apply_sql_for_test("CREATE TABLE IF NOT EXISTS t(x INT)".to_string()).await.unwrap();
    leader.apply_sql_for_test("INSERT INTO t(x) VALUES (1)".to_string()).await.unwrap();

    // Verify on follower
    // We need to wait for replication to apply to state machine
    // Wait for last applied index to match leader's last log index
    // Using a simple retry loop for query result consistency
    let mut value = "0".to_string();
    for _ in 0..50 {
         let res = follower.query_scalar_for_test("SELECT COUNT(*) FROM t".to_string()).await;
         if let Ok(v) = res {
             if v == "1" {
                 value = v;
                 break;
             }
         }
         tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert_eq!(value, "1");
}
