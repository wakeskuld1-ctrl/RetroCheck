//! ### 修改记录 (2026-02-25)
//! - 原因: 需要覆盖 HWM 推进与幂等重放语义
//! - 目的: 确保物理状态落盘后才推进水位线

// ### 修改记录 (2026-02-25)
// - 原因: 需要 EdgeStore 类型
// - 目的: 使用最小接口完成测试驱动
use check_program::edge::store::EdgeStore;
// ### 修改记录 (2026-02-25)
// - 原因: 测试需要临时目录
// - 目的: 避免污染真实数据目录
use tempfile::tempdir;

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要验证 HWM 推进依赖物理状态
/// - 目的: 防止断电导致半联动
#[test]
fn hwm_advances_only_after_state_persisted() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 测试需要隔离目录
    // - 目的: 保证每次测试独立
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要打开最小存储
    // - 目的: 执行持久化与 HWM 推进
    let store = EdgeStore::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要先落物理状态
    // - 目的: 作为 HWM 推进前置条件
    store.persist_state(10).unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要推进 HWM
    // - 目的: 验证推进后可读
    store.set_hwm(10).unwrap();
    assert_eq!(store.get_hwm().unwrap(), 10);
}

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要验证 ACK 幂等与单调
/// - 目的: 重放时不会回退 HWM
#[test]
fn hwm_ack_is_idempotent_and_monotonic() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 测试需要隔离目录
    // - 目的: 保证每次测试独立
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要打开最小存储
    // - 目的: 执行 ACK 推进
    let store = EdgeStore::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要先落物理状态
    // - 目的: 作为 HWM 推进前置条件
    store.persist_state(10).unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要首次 ACK
    // - 目的: 推进 HWM 到 5
    store.ack_hwm(5).unwrap();
    assert_eq!(store.get_hwm().unwrap(), 5);
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要幂等重放
    // - 目的: 重复 ACK 不改变 HWM
    store.ack_hwm(5).unwrap();
    assert_eq!(store.get_hwm().unwrap(), 5);
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要验证回退无效
    // - 目的: HWM 不能倒退
    store.ack_hwm(3).unwrap();
    assert_eq!(store.get_hwm().unwrap(), 5);
}

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要验证 ACK 超过物理状态被拒绝
/// - 目的: 防止断电半联动
#[test]
fn hwm_rejects_ack_above_state() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 测试需要隔离目录
    // - 目的: 保证每次测试独立
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要打开最小存储
    // - 目的: 执行 ACK 校验
    let store = EdgeStore::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要先落物理状态
    // - 目的: 作为 HWM 推进前置条件
    store.persist_state(5).unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要越界 ACK
    // - 目的: 验证错误被返回
    assert!(store.ack_hwm(6).is_err());
    assert_eq!(store.get_hwm().unwrap(), 0);
}
