//! ### 修改记录 (2026-02-17)
//! - 原因: 需要验证 RaftStore 的持久化能力
//! - 目的: 确保重启后元数据仍可恢复

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要读取日志条目
/// - 目的: 允许调用 try_get_log_entries
use openraft::storage::RaftLogReader;
/// ### 修改记录 (2026-02-27)
/// - 原因: 需要引入日志存储 trait
/// - 目的: 允许使用 get_log_reader
use openraft::storage::RaftLogStorage;

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

/// ### 修改记录 (2026-02-26)
/// - 原因: 需要验证日志追加与读取能力
/// - 目的: 为 RaftLogStorage 的最小实现提供保证
#[tokio::test]
async fn raft_store_appends_and_reads_logs() {
    // ### 修改记录 (2026-02-26)
    // - 原因: 测试需要隔离的临时目录
    // - 目的: 避免污染真实数据目录
    let dir = tempfile::tempdir().unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要初始化 RaftStore
    // - 目的: 验证日志追加与读取闭环
    let mut store = check_program::raft::store::RaftStore::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要构造最小日志条目
    // - 目的: 验证 append 的读写路径
    // ### 修改记录 (2026-02-27)
    // - 原因: Entry::new 不存在
    // - 目的: 使用结构体字段构造
    let entry = openraft::Entry::<check_program::raft::types::TypeConfig> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要构造基础 log_id
        // - 目的: 让存储层具备索引信息
        log_id: openraft::LogId::new(openraft::CommittedLeaderId::new(1, 1), 1),
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要最小载荷
        // - 目的: 保证 append 可执行
        payload: openraft::EntryPayload::Blank,
    };
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要写入日志并触发回调
    // - 目的: 验证存储具备 append 能力
    store.append_for_test(vec![entry.clone()]).await.unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要获取只读日志视图
    // - 目的: 验证读取路径完整
    let mut reader = store.get_log_reader().await;
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要读取指定范围日志
    // - 目的: 确认 append 结果可见
    let entries = reader.try_get_log_entries(1..2).await.unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要断言数量
    // - 目的: 保证日志读取正确
    assert_eq!(entries.len(), 1);
}
