// ### 修改记录 (2026-02-18)
// - 原因: 补齐连续重启与故障注入覆盖
// - 目的: 强化混沌测试的行为解释性
use check_program::raft::raft_node::TestCluster;

#[tokio::test]
async fn chaos_failover_and_restart_preserves_row_count() {
    // ### 修改记录 (2026-02-18)
    // - 原因: 补充测试说明以提升可维护性
    // - 目的: 标注用例关注点与流程边界
    let mut cluster = TestCluster::new(3).await.unwrap();
    // ### 修改记录 (2026-02-18)
    // - 原因: 需要确保写入路径存在
    // - 目的: 为计数断言提供基础表
    cluster
        .write("CREATE TABLE IF NOT EXISTS t(x INT)".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-02-18)
    // - 原因: 需要明确预期的累计写入数
    // - 目的: 在循环中稳定验证结果
    let mut expected = 0;
    for round in 0..6 {
        // ### 修改记录 (2026-02-18)
        // - 原因: 需要制造稳定写入负载
        // - 目的: 驱动后续故障切换验证
        cluster
            .write("INSERT INTO t(x) VALUES (1)".to_string())
            .await
            .unwrap();
        expected += 1;
        if round % 2 == 0 {
            // ### 修改记录 (2026-02-18)
            // - 原因: 需要覆盖故障切换
            // - 目的: 验证 failover 后计数一致
            cluster.fail_leader().await.unwrap();
        } else {
            // ### 修改记录 (2026-02-18)
            // - 原因: 需要覆盖重启路径
            // - 目的: 验证重启后计数一致
            cluster.restart_leader().await.unwrap();
        }
        // ### 修改记录 (2026-02-18)
        // - 原因: 需要读取 leader 状态
        // - 目的: 与预期计数对齐
        let value = cluster
            .query_leader_scalar("SELECT COUNT(*) FROM t".to_string())
            .await
            .unwrap();
        assert_eq!(value, expected.to_string());
    }
}

#[tokio::test]
async fn chaos_repeated_failover_keeps_history_applied() {
    // ### 修改记录 (2026-02-18)
    // - 原因: 补充测试说明以提升可维护性
    // - 目的: 标注用例关注点与流程边界
    let mut cluster = TestCluster::new(3).await.unwrap();
    // ### 修改记录 (2026-02-18)
    // - 原因: 需要确保写入路径存在
    // - 目的: 为计数断言提供基础表
    cluster
        .write("CREATE TABLE IF NOT EXISTS t(x INT)".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-02-18)
    // - 原因: 需要明确预期的累计写入数
    // - 目的: 在循环中稳定验证结果
    let mut expected = 0;
    for _ in 0..4 {
        // ### 修改记录 (2026-02-18)
        // - 原因: 需要制造稳定写入负载
        // - 目的: 驱动后续故障切换验证
        cluster
            .write("INSERT INTO t(x) VALUES (1)".to_string())
            .await
            .unwrap();
        expected += 1;
        // ### 修改记录 (2026-02-18)
        // - 原因: 需要覆盖多次 failover
        // - 目的: 验证历史写入未丢失
        cluster.fail_leader().await.unwrap();
        // ### 修改记录 (2026-02-18)
        // - 原因: 需要读取 leader 状态
        // - 目的: 与预期计数对齐
        let value = cluster
            .query_leader_scalar("SELECT COUNT(*) FROM t".to_string())
            .await
            .unwrap();
        assert_eq!(value, expected.to_string());
    }
}

#[tokio::test]
async fn chaos_leader_restart_preserves_wal_snapshot_state() {
    // ### 修改记录 (2026-02-18)
    // - 原因: 需要覆盖连续重启恢复一致性
    // - 目的: 验证 WAL/Snapshot 路径具备稳定恢复能力
    let mut cluster = TestCluster::new(3).await.unwrap();
    // ### 修改记录 (2026-02-18)
    // - 原因: 需要确保写入路径存在
    // - 目的: 为计数断言提供基础表
    cluster
        .write("CREATE TABLE IF NOT EXISTS t(x INT)".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-02-18)
    // - 原因: 需要积累足够的写入量
    // - 目的: 触发 WAL/Snapshot 的典型落盘路径
    let mut expected = 0;
    for idx in 0..20 {
        // ### 修改记录 (2026-02-18)
        // - 原因: 需要制造持续写入负载
        // - 目的: 增加重启恢复压力
        cluster
            .write(format!("INSERT INTO t(x) VALUES ({})", idx))
            .await
            .unwrap();
        expected += 1;
        if idx % 3 == 0 {
            // ### 修改记录 (2026-02-18)
            // - 原因: 需要模拟连续重启 leader
            // - 目的: 覆盖恢复后读一致性
            cluster.restart_leader().await.unwrap();
        }
    }

    // ### 修改记录 (2026-02-18)
    // - 原因: 需要验证恢复后数据
    // - 目的: 确认写入未丢失且未重复
    let value = cluster
        .query_leader_scalar("SELECT COUNT(*) FROM t".to_string())
        .await
        .unwrap();
    assert_eq!(value, expected.to_string());
}
