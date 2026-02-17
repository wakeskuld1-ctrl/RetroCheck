//! ### 修改记录 (2026-02-17)
//! - 原因: 需要验证 Router 写入走 Raft 路径
//! - 目的: 以测试驱动 Router 接入 RaftNode

use check_program::raft::raft_node::RaftNode;
use check_program::raft::router::Router;
use uuid::Uuid;

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要确保 Router 写入走 RaftNode
/// - 目的: 避免直接写 SQLite 造成路径分叉
#[tokio::test]
async fn router_write_uses_raft_log() {
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要独立测试目录
    // - 目的: 避免污染其他测试数据
    let base_dir = std::env::temp_dir().join(format!("router_raft_write_{}", Uuid::new_v4()));
    let raft_node = RaftNode::start_for_test(1, base_dir).await.unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要 Router 绑定 RaftNode
    // - 目的: 让写入走统一 Raft 写路径
    let router = Router::new_with_raft(raft_node.clone());

    router
        .write("CREATE TABLE IF NOT EXISTS t(x INT)".to_string())
        .await
        .unwrap();
    router
        .write("INSERT INTO t(x) VALUES (1)".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要验证写入效果
    // - 目的: 证明 Router 写入已落地
    let value = raft_node
        .query_scalar_for_test("SELECT COUNT(*) FROM t".to_string())
        .await
        .unwrap();
    assert_eq!(value, "1");
}
