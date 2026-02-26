//! ### 修改记录 (2026-02-25)
//! - 原因: 需要覆盖 TTL 信封过期丢弃
//! - 目的: 过期数据不惊动应用层

// ### 修改记录 (2026-02-25)
// - 原因: 需要 EdgeStore 类型
// - 目的: 使用最小接口完成测试驱动
use check_program::edge::store::EdgeStore;
// ### 修改记录 (2026-02-25)
// - 原因: 测试需要临时目录
// - 目的: 避免污染真实数据目录
use tempfile::tempdir;

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要验证过期即丢弃
/// - 目的: TTL 逻辑可独立工作
#[test]
fn ttl_expired_value_is_dropped() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 测试需要隔离目录
    // - 目的: 保证每次测试独立
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要打开最小存储
    // - 目的: 写入并读取 TTL 值
    let store = EdgeStore::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 过期时间为 0
    // - 目的: 触发立即过期
    store.put_with_ttl(0, b"dead".to_vec()).unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 读取后应丢弃
    // - 目的: 确保应用层不会收到过期值
    let val = store.get_with_ttl(0).unwrap();
    assert!(val.is_none());
}
