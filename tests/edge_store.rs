//! ### 修改记录 (2026-02-25)
//! - 原因: 需要为 edge 模块建立最小测试入口
//! - 目的: 以 TDD 驱动 EdgeStore 的最小可用骨架

// ### 修改记录 (2026-02-25)
// - 原因: 测试需要临时目录
// - 目的: 避免污染真实数据目录
use tempfile::tempdir;

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要验证 EdgeStore 可打开与关闭
/// - 目的: 作为后续断电与重放测试的基础
#[test]
fn edge_store_can_open_and_close() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 测试需要隔离目录
    // - 目的: 确保每次测试环境干净
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: EdgeStore 尚未实现
    // - 目的: 先以测试定义目标接口
    let store = check_program::edge::store::EdgeStore::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要显式释放
    // - 目的: 验证关闭时不触发异常
    drop(store);
}
