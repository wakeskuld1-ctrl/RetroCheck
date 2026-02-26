//! ### 修改记录 (2026-02-25)
//! - 原因: 需要覆盖 A/B 切换的断电恢复语义
//! - 目的: 确保活动指针可持久化且重启可读
//! ### 修改记录 (2026-02-26)
//! - 原因: 需要验证 A/B 延迟清理与配置驱动
//! - 目的: 覆盖切换不清理与 50% 阈值清理

// ### 修改记录 (2026-02-25)
// - 原因: 需要 EdgeStore 类型
// - 目的: 使用最小接口完成测试驱动
// ### 修改记录 (2026-02-26)
// - 原因: 新增配置文件驱动的 A/B 行为
// - 目的: 使用扩展接口验证清理阈值
use check_program::edge::store::EdgeStore;
// ### 修改记录 (2026-02-26)
// - 原因: 需要写入配置文件
// - 目的: 模拟配置驱动的阈值管理
use std::fs;
// ### 修改记录 (2026-02-26)
// - 原因: 需要构造配置文件路径
// - 目的: 让测试独立生成配置
use std::path::Path;
// ### 修改记录 (2026-02-25)
// - 原因: 测试需要临时目录
// - 目的: 避免污染真实数据目录
use tempfile::tempdir;

// ### 修改记录 (2026-02-26)
// - 原因: 需要复用配置文件生成逻辑
// - 目的: 简化多个测试的配置准备
fn write_edge_config(path: &Path, cleanup_ratio: f64) {
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要用 JSON 写入配置
    // - 目的: 与 EdgeConfig 的加载格式一致
    let text = format!(r#"{{"ab_cleanup_ratio":{}}}"#, cleanup_ratio);
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要落盘配置
    // - 目的: 供 EdgeStore 从文件加载
    fs::write(path, text).unwrap();
}

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要验证 A/B 活动指针持久化
/// - 目的: 断电重启后仍可确定活动实例
#[test]
fn ab_switch_persists_active_slot() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 测试需要隔离目录
    // - 目的: 保证每次测试独立
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要模拟运行期写入
    // - 目的: 持久化活动指针
    {
        let store = EdgeStore::open(dir.path()).unwrap();
        store.set_active_slot(1).unwrap();
    }
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要模拟重启后读取
    // - 目的: 验证持久化结果可恢复
    {
        let store = EdgeStore::open(dir.path()).unwrap();
        assert_eq!(store.get_active_slot().unwrap(), 1);
    }
}

/// ### 修改记录 (2026-02-26)
/// - 原因: 需要验证切换不触发清理
/// - 目的: 确保仅切指针不会丢数据
#[test]
fn ab_switch_keeps_previous_slot_data_before_threshold() {
    // ### 修改记录 (2026-02-26)
    // - 原因: 测试需要隔离目录
    // - 目的: 保证配置与数据互不污染
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要配置文件路径
    // - 目的: 写入 A/B 阈值配置
    let config_path = dir.path().join("edge_config.json");
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要阈值为 50%
    // - 目的: 与方案 A 的默认值一致
    write_edge_config(&config_path, 0.5);
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要以配置文件打开存储
    // - 目的: 验证配置驱动逻辑
    let store = EdgeStore::open_with_config_file(dir.path(), &config_path).unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要在 A 槽写入数据
    // - 目的: 构造切换前已有数据的场景
    store.put_log(1, 1, b"a".to_vec()).unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要切换到 B 槽
    // - 目的: 仅更新活动指针
    store.set_active_slot(1).unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要切回 A 槽检查数据
    // - 目的: 验证切换不触发清理
    store.set_active_slot(0).unwrap();
    let keys = store.scan_log_keys(None).unwrap();
    assert_eq!(keys, vec![store.log_key(1, 1)]);
}

/// ### 修改记录 (2026-02-26)
/// - 原因: 需要验证达到 50% 时清理旧槽位
/// - 目的: 满足延迟清理策略
#[test]
fn ab_cleanup_triggers_when_active_reaches_ratio() {
    // ### 修改记录 (2026-02-26)
    // - 原因: 测试需要隔离目录
    // - 目的: 保证配置与数据互不污染
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要配置文件路径
    // - 目的: 写入 A/B 阈值配置
    let config_path = dir.path().join("edge_config.json");
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要阈值为 50%
    // - 目的: 触发 B 槽达到 50% 时清理
    write_edge_config(&config_path, 0.5);
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要以配置文件打开存储
    // - 目的: 验证配置驱动逻辑
    let store = EdgeStore::open_with_config_file(dir.path(), &config_path).unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要在 A 槽写入 10 条
    // - 目的: 形成清理对比基准
    for seq in 0..10 {
        store.put_log(1, seq, b"a".to_vec()).unwrap();
    }
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要切换到 B 槽
    // - 目的: 让 B 槽开始累积
    store.set_active_slot(1).unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要写入 5 条
    // - 目的: 达到 50% 阈值触发清理
    for seq in 0..5 {
        store.put_log(2, seq, b"b".to_vec()).unwrap();
    }
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要切回 A 槽验证清理结果
    // - 目的: 旧槽位应被清空
    store.set_active_slot(0).unwrap();
    let keys = store.scan_log_keys(None).unwrap();
    assert!(keys.is_empty());
}

/// ### 修改记录 (2026-02-26)
/// - 原因: 需要验证非法配置回退为默认值
/// - 目的: 确保 ratio=0 时清理仍在 50% 触发
#[test]
fn ab_invalid_ratio_zero_falls_back_to_default() {
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要复用非法 ratio 测试逻辑
    // - 目的: 复用写入/切换/清理步骤
    assert_cleanup_with_ratio(0.0);
}

/// ### 修改记录 (2026-02-26)
/// - 原因: 需要验证非法配置回退为默认值
/// - 目的: 确保 ratio>1 时清理仍在 50% 触发
#[test]
fn ab_invalid_ratio_above_one_falls_back_to_default() {
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要复用非法 ratio 测试逻辑
    // - 目的: 复用写入/切换/清理步骤
    assert_cleanup_with_ratio(1.2);
}

/// ### 修改记录 (2026-02-26)
/// - 原因: 需要验证非法配置回退为默认值
/// - 目的: 确保 ratio<0 时清理仍在 50% 触发
#[test]
fn ab_invalid_ratio_negative_falls_back_to_default() {
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要复用非法 ratio 测试逻辑
    // - 目的: 复用写入/切换/清理步骤
    assert_cleanup_with_ratio(-1.0);
}

/// ### 修改记录 (2026-02-26)
/// - 原因: 需要验证清理后计数重置
/// - 目的: 确保再次写入触发逻辑正确
#[test]
fn ab_cleanup_resets_counts_and_retriggers_after_switch() {
    // ### 修改记录 (2026-02-26)
    // - 原因: 测试需要隔离目录
    // - 目的: 保证配置与数据互不污染
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要配置文件路径
    // - 目的: 写入 A/B 阈值配置
    let config_path = dir.path().join("edge_config.json");
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要阈值为 50%
    // - 目的: 触发清理并验证计数归零
    write_edge_config(&config_path, 0.5);
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要以配置文件打开存储
    // - 目的: 验证配置驱动逻辑
    let store = EdgeStore::open_with_config_file(dir.path(), &config_path).unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要在 A 槽写入 4 条
    // - 目的: 为清理触发提供基准
    for seq in 0..4 {
        store.put_log(1, seq, b"a".to_vec()).unwrap();
    }
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要切换到 B 槽
    // - 目的: 让 B 槽达到阈值触发清理
    store.set_active_slot(1).unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 写入 2 条达到 50%
    // - 目的: 触发对 A 槽清理
    for seq in 0..2 {
        store.put_log(2, seq, b"b".to_vec()).unwrap();
    }
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要读取计数快照
    // - 目的: 验证 A 槽计数被清零
    let stats = store.debug_stats().unwrap();
    assert_eq!(stats.slot0_count, 0);
    assert_eq!(stats.slot1_count, 2);
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要切回 A 槽写入
    // - 目的: 验证再次触发清理逻辑
    store.set_active_slot(0).unwrap();
    store.put_log(3, 1, b"c".to_vec()).unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要切回 B 槽检查清理结果
    // - 目的: 确保 B 槽被清理
    store.set_active_slot(1).unwrap();
    let keys = store.scan_log_keys(None).unwrap();
    assert!(keys.is_empty());
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要读取计数快照
    // - 目的: 验证 B 槽计数被清零
    let stats = store.debug_stats().unwrap();
    assert_eq!(stats.slot1_count, 0);
}

/// ### 修改记录 (2026-02-26)
/// - 原因: 需要验证清理耗时统计
/// - 目的: 覆盖大批量清理性能冒烟
#[test]
fn ab_cleanup_records_elapsed_ms_for_large_batches() {
    // ### 修改记录 (2026-02-26)
    // - 原因: 测试需要隔离目录
    // - 目的: 保证配置与数据互不污染
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要配置文件路径
    // - 目的: 写入 A/B 阈值配置
    let config_path = dir.path().join("edge_config.json");
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要阈值为 50%
    // - 目的: 触发清理并记录耗时
    write_edge_config(&config_path, 0.5);
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要以配置文件打开存储
    // - 目的: 验证配置驱动逻辑
    let store = EdgeStore::open_with_config_file(dir.path(), &config_path).unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要构造大批量数据
    // - 目的: 触发清理并记录耗时
    let payload = vec![0u8; 128];
    for seq in 0..1000 {
        store.put_log(10, seq, payload.clone()).unwrap();
    }
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要切换到 B 槽
    // - 目的: 写入达到阈值触发清理
    store.set_active_slot(1).unwrap();
    for seq in 0..500 {
        store.put_log(11, seq, payload.clone()).unwrap();
    }
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要读取耗时统计
    // - 目的: 验证清理耗时被记录
    let stats = store.debug_stats().unwrap();
    assert!(stats.last_cleanup_ms > 0);
    assert!(stats.last_cleanup_ms <= 10_000);
}

/// ### 修改记录 (2026-02-26)
/// - 原因: 需要复用非法 ratio 测试逻辑
/// - 目的: 降低重复代码
fn assert_cleanup_with_ratio(ratio: f64) {
    // ### 修改记录 (2026-02-26)
    // - 原因: 测试需要隔离目录
    // - 目的: 保证配置与数据互不污染
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要配置文件路径
    // - 目的: 写入 A/B 阈值配置
    let config_path = dir.path().join("edge_config.json");
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要写入指定 ratio
    // - 目的: 验证非法 ratio 回退逻辑
    write_edge_config(&config_path, ratio);
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要以配置文件打开存储
    // - 目的: 验证配置驱动逻辑
    let store = EdgeStore::open_with_config_file(dir.path(), &config_path).unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要在 A 槽写入 10 条
    // - 目的: 形成清理对比基准
    for seq in 0..10 {
        store.put_log(1, seq, b"a".to_vec()).unwrap();
    }
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要切换到 B 槽
    // - 目的: 让 B 槽开始累积
    store.set_active_slot(1).unwrap();
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要写入 5 条
    // - 目的: 达到 50% 阈值触发清理
    for seq in 0..5 {
        store.put_log(2, seq, b"b".to_vec()).unwrap();
    }
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要切回 A 槽验证清理结果
    // - 目的: 旧槽位应被清空
    store.set_active_slot(0).unwrap();
    let keys = store.scan_log_keys(None).unwrap();
    assert!(keys.is_empty());
}
