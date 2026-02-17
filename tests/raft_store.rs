//! ### 修改记录 (2026-02-17)
//! - 原因: 需要验证 RaftStore 的持久化能力
//! - 目的: 确保重启后元数据仍可恢复

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要验证 last_applied 的持久化
/// - 目的: 避免 Raft 重启后日志回放错位
#[test]
fn raft_store_persists_after_restart() {
    // ### 修改记录 (2026-02-17)
    // - 原因: 测试需要隔离的临时目录
    // - 目的: 避免污染真实数据目录
    let dir = tempfile::tempdir().unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 第一次打开用于写入
    // - 目的: 模拟节点运行期间持久化
    {
        let store = check_program::raft::store::RaftStore::open(dir.path()).unwrap();
        store.set_last_applied(42).unwrap();
    }

    // ### 修改记录 (2026-02-17)
    // - 原因: 第二次打开用于读取
    // - 目的: 验证持久化结果可恢复
    {
        let store = check_program::raft::store::RaftStore::open(dir.path()).unwrap();
        assert_eq!(store.get_last_applied().unwrap(), 42);
    }
}

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要验证快照索引持久化
/// - 目的: 确保重启后可恢复快照边界
#[test]
fn raft_store_persists_snapshot_index() {
    // ### 修改记录 (2026-02-17)
    // - 原因: 测试需要隔离的临时目录
    // - 目的: 避免污染真实数据目录
    let dir = tempfile::tempdir().unwrap();

    // ### 修改记录 (2026-02-17)
    // - 原因: 第一次打开用于写入
    // - 目的: 模拟节点运行期间持久化
    {
        let store = check_program::raft::store::RaftStore::open(dir.path()).unwrap();
        store.set_last_snapshot_index(7).unwrap();
    }

    // ### 修改记录 (2026-02-17)
    // - 原因: 第二次打开用于读取
    // - 目的: 验证持久化结果可恢复
    {
        let store = check_program::raft::store::RaftStore::open(dir.path()).unwrap();
        assert_eq!(store.get_last_snapshot_index().unwrap(), 7);
    }
}
