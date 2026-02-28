//! ### 修改记录 (2026-02-17)
//! - 原因: 需要验证 Raft 核心能启动并成为 Leader
//! - 目的: 以测试驱动方式补齐 RaftNode 基础能力

use check_program::raft::raft_node::RaftNode;
use uuid::Uuid;

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要确认单节点能完成自举
/// - 目的: 为后续日志复制与网络适配奠定基础
#[tokio::test]
async fn raft_node_bootstrap_and_becomes_leader() {
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要最小化测试启动参数
    // - 目的: 聚焦验证节点能启动并进入 Leader 状态
    let node_id = 1u64;
    let base_dir = std::env::temp_dir().join(format!("raft_core_bootstrap_{}", Uuid::new_v4()));

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要验证节点能自举
    // - 目的: 通过测试驱动实现
    let node = RaftNode::start_local(node_id, base_dir).await.unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 单节点需要显式初始化才能成为 Leader
    // - 目的: 触发 election 并成为 Leader
    let mut members = std::collections::BTreeSet::new();
    members.insert(node_id);
    node.raft.initialize(members).await.unwrap();

    // Wait for leader state
    let mut metrics = node.raft.metrics().borrow().clone();
    loop {
        if metrics.state == openraft::ServerState::Leader {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        metrics = node.raft.metrics().borrow().clone();
    }
}
