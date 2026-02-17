//! ### 修改记录 (2026-02-17)
//! - 原因: 需要验证 Raft 核心能启动并成为 Leader
//! - 目的: 以测试驱动方式补齐 RaftNode 基础能力

use check_program::raft::raft_node::RaftNode;

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要确认单节点能完成自举
/// - 目的: 为后续日志复制与网络适配奠定基础
#[tokio::test]
async fn raft_node_bootstrap_and_becomes_leader() {
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要最小化测试启动参数
    // - 目的: 聚焦验证节点能启动并进入 Leader 状态
    let node_id = 1u64;
    let base_dir = std::env::temp_dir().join("raft_core_bootstrap");

    // ### 修改记录 (2026-02-17)
    // - 原因: RaftNode 尚未实现
    // - 目的: 通过编译失败驱动实现
    let _node = RaftNode::start_for_test(node_id, base_dir).await.unwrap();
}
