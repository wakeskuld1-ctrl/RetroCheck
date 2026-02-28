//! ### 修改记录 (2026-02-17)
//! - 原因: 需要验证 Raft 日志应用能写入 SQLite
//! - 目的: 以测试驱动方式补齐状态机 apply 路径

use check_program::raft::raft_node::RaftNode;
use uuid::Uuid;

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要验证 SQL 写入能被应用
/// - 目的: 确保 Raft apply 与 SQLite 连接正常
#[tokio::test]
async fn raft_applies_log_to_sqlite() {
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要隔离测试目录
    // - 目的: 避免污染其他测试数据
    let base_dir = std::env::temp_dir().join(format!("raft_apply_sqlite_{}", Uuid::new_v4()));
    let node = RaftNode::start_for_test(1, base_dir).await.unwrap();

    // ### 修改记录 (2026-02-28)
    // - 原因: OpenRaft 节点启动后默认为 Learner，必须显式初始化才能成为 Leader
    // - 目的: 确保节点能处理写请求
    let mut members = std::collections::BTreeSet::new();
    members.insert(1);
    node.raft.initialize(members).await.unwrap();
    
    // 等待成为 Leader
    loop {
        let metrics = node.raft.metrics().borrow().clone();
        if metrics.state == openraft::ServerState::Leader {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要最小写入场景
    // - 目的: 验证写入与查询都可工作
    node.apply_sql_for_test("CREATE TABLE IF NOT EXISTS t(x INT)".to_string())
        .await
        .unwrap();
    node.apply_sql_for_test("INSERT INTO t(x) VALUES (1)".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要验证写入结果
    // - 目的: 确认 apply 路径落地
    let value = node
        .query_scalar_for_test("SELECT COUNT(*) FROM t".to_string())
        .await
        .unwrap();
    assert_eq!(value, "1");
}
