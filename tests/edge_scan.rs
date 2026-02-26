//! ### 修改记录 (2026-02-25)
//! - 原因: 需要覆盖前缀扫描顺序与断点语义
//! - 目的: 确保字典序等于时间序

// ### 修改记录 (2026-02-25)
// - 原因: 需要 EdgeStore 类型
// - 目的: 使用最小接口完成测试驱动
use check_program::edge::store::EdgeStore;
// ### 修改记录 (2026-02-25)
// - 原因: 测试需要临时目录
// - 目的: 避免污染真实数据目录
use tempfile::tempdir;

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要验证前缀扫描的字典序
/// - 目的: 防止乱序导致上传流水错乱
#[test]
fn prefix_scan_returns_sorted_order() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 测试需要隔离目录
    // - 目的: 保证每次测试独立
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要打开最小存储
    // - 目的: 写入两条乱序记录
    let store = EdgeStore::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要乱序写入
    // - 目的: 验证扫描结果排序
    store.put_log(1, 2, b"a".to_vec()).unwrap();
    store.put_log(1, 1, b"b".to_vec()).unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要扫描日志键
    // - 目的: 断言字典序稳定
    let keys = store.scan_log_keys(None).unwrap();
    assert_eq!(keys, vec![store.log_key(1, 1), store.log_key(1, 2)]);
}
