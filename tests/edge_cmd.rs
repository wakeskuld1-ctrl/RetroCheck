//! ### 修改记录 (2026-02-25)
//! - 原因: 需要新增 cmd 前缀断电重放测试
//! - 目的: 覆盖前缀扫描顺序与断点续扫

// ### 修改记录 (2026-02-25)
// - 原因: 需要使用 EdgeStore
// - 目的: 调用 cmd 接口
use check_program::edge::store::EdgeStore;
// ### 修改记录 (2026-02-25)
// - 原因: 需要临时目录
// - 目的: 测试隔离
use tempfile::tempdir;
// ### 修改记录 (2026-02-25)
// - 原因: 需要时间边界测试
// - 目的: 生成过期时间戳
use std::time::{SystemTime, UNIX_EPOCH};

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要验证 cmd 前缀扫描
/// - 目的: 保证排序与断点过滤正确
#[test]
fn cmd_prefix_scan_respects_order_and_breakpoint() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要隔离目录
    // - 目的: 保证测试独立
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要打开存储
    // - 目的: 写入 cmd 数据
    let store = EdgeStore::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要写入乱序 cmd
    // - 目的: 验证排序恢复
    store.put_cmd(1, 2, 0, b"b".to_vec()).unwrap();
    store.put_cmd(1, 1, 0, b"a".to_vec()).unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要扫描全部 cmd
    // - 目的: 验证排序顺序
    let keys = store.scan_cmd_keys(None).unwrap();
    assert_eq!(keys, vec![store.cmd_key(1, 1), store.cmd_key(1, 2)]);

    // ### 修改记录 (2026-02-25)
    // - 原因: 需要断点过滤
    // - 目的: 验证断点续扫
    let last = store.cmd_key(1, 1);
    let keys = store.scan_cmd_keys(Some(last)).unwrap();
    assert_eq!(keys, vec![store.cmd_key(1, 2)]);
}

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要验证 cmd 过期与坏包丢弃
/// - 目的: 防止执行过期/异常指令
#[test]
fn cmd_payload_expired_or_broken_is_dropped() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要隔离目录
    // - 目的: 保证测试独立
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要打开存储
    // - 目的: 写入 cmd 数据
    let store = EdgeStore::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要写入过期 cmd
    // - 目的: 验证过期丢弃
    store.put_cmd(2, 1, 0, b"dead".to_vec()).unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要读取 payload
    // - 目的: 过期应返回 None
    let payload = store.get_cmd_payload(&store.cmd_key(2, 1)).unwrap();
    assert!(payload.is_none());

    // ### 修改记录 (2026-02-25)
    // - 原因: 需要构造坏包
    // - 目的: 验证坏包丢弃
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要验证重复读取
    // - 目的: 删除后仍返回 None
    let broken_key = store.cmd_key(2, 2);
    store
        .put_cmd_raw(broken_key.clone(), vec![1, 2, 3])
        .unwrap();
    let payload = store.get_cmd_payload(&broken_key).unwrap();
    assert!(payload.is_none());
    let payload = store.get_cmd_payload(&broken_key).unwrap();
    assert!(payload.is_none());
}

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要验证 cmd 断点持久化
/// - 目的: 断电恢复后续扫
#[test]
fn cmd_replay_resumes_from_last_key() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要隔离目录
    // - 目的: 保证测试独立
    let dir = tempdir().unwrap();
    {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要打开存储
        // - 目的: 写入 cmd 数据
        let store = EdgeStore::open(dir.path()).unwrap();
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要写入 cmd
        // - 目的: 构造断点重放场景
        store.put_cmd(3, 1, 0, b"c1".to_vec()).unwrap();
        store.put_cmd(3, 2, 0, b"c2".to_vec()).unwrap();
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要写入断点
        // - 目的: 断电后续扫从此处开始
        store.set_cmd_last_key(store.cmd_key(3, 1)).unwrap();
    }
    {
        // ### 修改记录 (2026-02-25)
        // - 原因: 模拟断电后重启
        // - 目的: 恢复断点并续扫
        let store = EdgeStore::open(dir.path()).unwrap();
        let keys = store
            .scan_cmd_keys(store.get_cmd_last_key().unwrap())
            .unwrap();
        assert_eq!(keys, vec![store.cmd_key(3, 2)]);
    }
}

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要验证 cmd 过期时间边界
/// - 目的: 防止时钟漂移误判
#[test]
fn cmd_payload_respects_future_and_boundary_expire() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要隔离目录
    // - 目的: 保证测试独立
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要打开存储
    // - 目的: 写入 cmd 数据
    let store = EdgeStore::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要当前时间戳
    // - 目的: 构造边界与未来过期
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要未来过期
    // - 目的: 应当可读
    store
        .put_cmd(4, 1, now_ms + 1000, b"live".to_vec())
        .unwrap();
    let payload = store.get_cmd_payload(&store.cmd_key(4, 1)).unwrap();
    assert!(payload.is_some());
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要边界过期
    // - 目的: 应当不可读
    store.put_cmd(4, 2, now_ms, b"edge".to_vec()).unwrap();
    let payload = store.get_cmd_payload(&store.cmd_key(4, 2)).unwrap();
    assert!(payload.is_none());
}

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要验证非法断点前缀容错
/// - 目的: 断点异常不阻塞重放
#[test]
fn cmd_scan_ignores_invalid_last_key_prefix() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要隔离目录
    // - 目的: 保证测试独立
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要打开存储
    // - 目的: 写入 cmd 数据
    let store = EdgeStore::open(dir.path()).unwrap();
    store.put_cmd(5, 1, 0, b"x".to_vec()).unwrap();
    store.put_cmd(5, 2, 0, b"y".to_vec()).unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要非法断点
    // - 目的: 触发容错逻辑
    let invalid_last = store.log_key(9, 9);
    let keys = store.scan_cmd_keys(Some(invalid_last)).unwrap();
    assert_eq!(keys, vec![store.cmd_key(5, 1), store.cmd_key(5, 2)]);
}
