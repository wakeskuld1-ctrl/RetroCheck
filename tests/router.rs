//! ### 修改记录 (2026-02-17)
//! - 原因: 需要验证 Router 的转发逻辑
//! - 目的: 在最小实现阶段锁定分支行为

/// ### 修改记录 (2026-02-17)
/// - 原因: 非 Leader 需要走转发路径
/// - 目的: 覆盖 Router 的条件分支
/// ### 修改记录 (2026-02-17)
/// - 原因: 非 Leader 写入改为明确报错
/// - 目的: 与当前写入语义保持一致
#[tokio::test]
async fn router_returns_error_when_not_leader() {
    // ### 修改记录 (2026-02-17)
    // - 原因: 构造非 Leader Router
    // - 目的: 验证非 Leader 明确报错
    let router = check_program::raft::router::Router::new_for_test(false);
    let res = router.write("INSERT INTO t VALUES (1)".to_string()).await;
    assert!(res.is_err());
}
