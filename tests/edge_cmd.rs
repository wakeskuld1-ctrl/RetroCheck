//! ### 修改记录 (2026-02-25)
//! - 原因: 需要新增 cmd 前缀断电重放测试
//! - 目的: 覆盖前缀扫描顺序与断点续扫

// ### 修改记录 (2026-02-25)
// - 原因: 需要使用 EdgeStore
// - 目的: 调用 cmd 接口
// ### 修改记录 (2026-02-28)
// - 原因: 需要读取 ACK 记录
// - 目的: 校验过期确认与丢弃落盘
use check_program::edge::store::{AckRecord, AckStatus, EdgeStore};
// ### 修改记录 (2026-02-27)
// - 原因: 需要执行指令过期策略与运行器测试
// - 目的: 引入 CmdRunner 与执行器接口
use check_program::edge::cmd_runner::{CmdExecutor, CmdRunner, ConfirmDecision, RunOptions};
// ### 修改记录 (2026-02-27)
// - 原因: 需要测试过期策略判定
// - 目的: 引入默认策略与策略接口
use check_program::edge::policy::{
    DefaultExpiredPolicy, ExpiredContext, ExpiredDecision, ExpiredPolicy,
};
// ### 修改记录 (2026-02-25)
// - 原因: 需要临时目录
// - 目的: 测试隔离
use tempfile::tempdir;
// ### 修改记录 (2026-02-25)
// - 原因: 需要时间边界测试
// - 目的: 生成过期时间戳
use std::time::{SystemTime, UNIX_EPOCH};
// ### 修改记录 (2026-02-27)
// - 原因: 需要统计执行调用次数
// - 目的: 验证运行器是否调用执行器
use std::sync::atomic::{AtomicUsize, Ordering};
// ### 修改记录 (2026-02-27)
// - 原因: 需要记录执行载荷
// - 目的: 校验执行调用发生
use std::sync::Mutex;
// ### 修改记录 (2026-02-28)
// - 原因: 需要写入配置文件
// - 目的: 驱动 A/B 清理阈值
use std::fs;
// ### 修改记录 (2026-02-28)
// - 原因: 需要配置文件路径类型
// - 目的: 复用配置写入逻辑
use std::path::Path;
/// ### 修改记录 (2026-02-28)
/// - 原因: 需要从 cmd_key 派生 ack_key
/// - 目的: 生成非法 ACK 数据写入键
fn ack_key_from_cmd_key(cmd_key: &[u8]) -> Vec<u8> {
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要定位 cmd 前缀
    // - 目的: 复用尾部排序语义
    let cmd_prefix = b"cmd:";
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要替换为 ack 前缀
    // - 目的: 构造 ACK 命名空间
    let ack_prefix = b"ack:";
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要找到 cmd 前缀位置
    // - 目的: 避免误替换
    let pos = cmd_key
        .windows(cmd_prefix.len())
        .position(|window| window == cmd_prefix)
        .expect("cmd prefix not found");
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要拼接新键
    // - 目的: 保留槽位前缀与尾部
    let mut buf = Vec::with_capacity(cmd_key.len());
    buf.extend_from_slice(&cmd_key[..pos]);
    buf.extend_from_slice(ack_prefix);
    buf.extend_from_slice(&cmd_key[pos + cmd_prefix.len()..]);
    buf
}

/// ### 修改记录 (2026-02-28)
/// - 原因: 需要复用配置文件生成
/// - 目的: 控制 A/B 清理阈值
fn write_edge_config(path: &Path, cleanup_ratio: f64) {
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要用 JSON 写入配置
    // - 目的: 与 EdgeConfig 的加载格式一致
    let text = format!(r#"{{"ab_cleanup_ratio":{}}}"#, cleanup_ratio);
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要落盘配置
    // - 目的: 供 EdgeStore 从文件加载
    fs::write(path, text).unwrap();
}

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

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要验证默认过期策略
/// - 目的: 明确执行/确认/丢弃判定边界
#[test]
fn default_expired_policy_decisions() {
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要 100ms 宽限期
    // - 目的: 构造确认窗口
    let policy = DefaultExpiredPolicy::new(100);
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要未过期场景
    // - 目的: 应直接执行
    let ctx = ExpiredContext {
        expire_at_ms: 200,
        now_ms: 100,
        network_ok: true,
    };
    assert_eq!(policy.decide(ctx), ExpiredDecision::Execute);
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要过期但在宽限期内
    // - 目的: 应进入确认
    let ctx = ExpiredContext {
        expire_at_ms: 100,
        now_ms: 150,
        network_ok: true,
    };
    assert_eq!(policy.decide(ctx), ExpiredDecision::Confirm);
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要过期且超过宽限期
    // - 目的: 应丢弃
    let ctx = ExpiredContext {
        expire_at_ms: 100,
        now_ms: 300,
        network_ok: true,
    };
    assert_eq!(policy.decide(ctx), ExpiredDecision::Drop);
}

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要验证过期丢弃路径
/// - 目的: 过期时不执行且推进断点
#[test]
fn cmd_runner_drops_expired_and_skips_execute() {
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要隔离目录
    // - 目的: 保证测试独立
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要打开存储
    // - 目的: 写入过期指令
    let store = EdgeStore::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要过期时间戳
    // - 目的: 触发过期逻辑
    store.put_cmd(10, 1, 0, b"expired".to_vec()).unwrap();
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要自定义策略
    // - 目的: 强制走丢弃路径
    struct AlwaysDropPolicy;
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要策略接口实现
    // - 目的: 返回丢弃决策
    impl ExpiredPolicy for AlwaysDropPolicy {
        fn decide(&self, _ctx: ExpiredContext) -> ExpiredDecision {
            ExpiredDecision::Drop
        }
    }
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要统计执行次数
    // - 目的: 确认未调用执行
    struct CountingExecutor {
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要记录执行次数
        // - 目的: 断言未执行
        executed: AtomicUsize,
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要记录确认次数
        // - 目的: 断言未确认
        confirmed: AtomicUsize,
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要记录执行载荷
        // - 目的: 调试断言使用
        payloads: Mutex<Vec<Vec<u8>>>,
    }
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要执行器接口实现
    // - 目的: 便于统计调用
    impl CmdExecutor for CountingExecutor {
        fn execute(&self, payload: Vec<u8>) -> anyhow::Result<u64> {
            self.executed.fetch_add(1, Ordering::SeqCst);
            self.payloads.lock().unwrap().push(payload);
            Ok(1)
        }

        fn confirm_expired(
            &self,
            _payload: Vec<u8>,
            _expire_at_ms: u64,
            _now_ms: u64,
        ) -> anyhow::Result<ConfirmDecision> {
            self.confirmed.fetch_add(1, Ordering::SeqCst);
            Ok(ConfirmDecision::Drop)
        }
    }
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要执行器实例
    // - 目的: 记录调用次数
    let executor = CountingExecutor {
        executed: AtomicUsize::new(0),
        confirmed: AtomicUsize::new(0),
        payloads: Mutex::new(Vec::new()),
    };
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要运行器实例
    // - 目的: 驱动过期处理
    let runner = CmdRunner::new(&store, AlwaysDropPolicy, &executor);
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要固定时间
    // - 目的: 确保过期触发
    let options = RunOptions {
        now_ms: Some(10),
        network_ok: true,
    };
    runner.run_once(options).unwrap();
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要断言未执行
    // - 目的: 过期应丢弃
    assert_eq!(executor.executed.load(Ordering::SeqCst), 0);
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要断言未确认
    // - 目的: 丢弃路径不确认
    assert_eq!(executor.confirmed.load(Ordering::SeqCst), 0);
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要断言断点推进
    // - 目的: 避免重复扫描
    let last_key = store.get_cmd_last_key().unwrap();
    assert_eq!(last_key, Some(store.cmd_key(10, 1)));
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要断言过期被物理删除
    // - 目的: 防止重复积压
    let keys = store.scan_cmd_keys(None).unwrap();
    assert!(keys.is_empty());
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要读取 ACK 结果
    // - 目的: 确认丢弃已落盘
    let ack = store.get_cmd_ack(&store.cmd_key(10, 1)).unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要断言丢弃状态
    // - 目的: 保证 ACK 记录一致
    assert_eq!(
        ack,
        Some(AckRecord {
            status: AckStatus::Dropped,
            exec_seq: 0,
        })
    );
}

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要验证过期确认与执行路径
/// - 目的: 过期指令可被业务确认执行
#[test]
fn cmd_runner_confirms_and_executes_expired() {
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要隔离目录
    // - 目的: 保证测试独立
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要打开存储
    // - 目的: 写入过期指令
    let store = EdgeStore::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要过期时间戳
    // - 目的: 触发确认路径
    store.put_cmd(11, 1, 0, b"confirm".to_vec()).unwrap();
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要自定义策略
    // - 目的: 强制进入确认
    struct AlwaysConfirmPolicy;
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要策略接口实现
    // - 目的: 返回确认决策
    impl ExpiredPolicy for AlwaysConfirmPolicy {
        fn decide(&self, _ctx: ExpiredContext) -> ExpiredDecision {
            ExpiredDecision::Confirm
        }
    }
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要统计执行次数
    // - 目的: 校验执行发生
    struct ConfirmExecuteExecutor {
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要记录执行次数
        // - 目的: 断言执行发生
        executed: AtomicUsize,
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要记录确认次数
        // - 目的: 断言确认发生
        confirmed: AtomicUsize,
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要固定执行序号
        // - 目的: 校验 HWM 推进
        seq: u64,
    }
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要执行器接口实现
    // - 目的: 触发确认后执行
    impl CmdExecutor for ConfirmExecuteExecutor {
        fn execute(&self, _payload: Vec<u8>) -> anyhow::Result<u64> {
            self.executed.fetch_add(1, Ordering::SeqCst);
            Ok(self.seq)
        }

        fn confirm_expired(
            &self,
            _payload: Vec<u8>,
            _expire_at_ms: u64,
            _now_ms: u64,
        ) -> anyhow::Result<ConfirmDecision> {
            self.confirmed.fetch_add(1, Ordering::SeqCst);
            Ok(ConfirmDecision::Execute)
        }
    }
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要执行器实例
    // - 目的: 记录调用次数
    let executor = ConfirmExecuteExecutor {
        executed: AtomicUsize::new(0),
        confirmed: AtomicUsize::new(0),
        seq: 7,
    };
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要运行器实例
    // - 目的: 驱动确认执行逻辑
    let runner = CmdRunner::new(&store, AlwaysConfirmPolicy, &executor);
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要固定时间
    // - 目的: 确保过期触发
    let options = RunOptions {
        now_ms: Some(10),
        network_ok: true,
    };
    runner.run_once(options).unwrap();
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要断言确认次数
    // - 目的: 确认发生
    assert_eq!(executor.confirmed.load(Ordering::SeqCst), 1);
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要断言执行次数
    // - 目的: 确认后执行
    assert_eq!(executor.executed.load(Ordering::SeqCst), 1);
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要断言 HWM 推进
    // - 目的: 保证状态一致性
    assert_eq!(store.get_hwm().unwrap(), 7);
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要断言断点推进
    // - 目的: 避免重复扫描
    let last_key = store.get_cmd_last_key().unwrap();
    assert_eq!(last_key, Some(store.cmd_key(11, 1)));
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要读取 ACK 结果
    // - 目的: 确认确认执行已落盘
    let ack = store.get_cmd_ack(&store.cmd_key(11, 1)).unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要断言执行状态
    // - 目的: 确保 ACK 记录了执行序号
    assert_eq!(
        ack,
        Some(AckRecord {
            status: AckStatus::Executed,
            exec_seq: 7,
        })
    );
}

/// ### 修改记录 (2026-02-28)
/// - 原因: 需要验证坏 ACK 自动清理
/// - 目的: 防止损坏数据长期残留
#[test]
fn ack_invalid_value_is_dropped_and_cleared() {
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要隔离目录
    // - 目的: 保证测试独立
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要生成 cmd_key
    // - 目的: 派生 ack_key
    let store = EdgeStore::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要构造 cmd_key
    // - 目的: 复用槽位前缀
    let cmd_key = store.cmd_key(20, 1);
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要释放 store
    // - 目的: 避免重复打开 sled
    drop(store);
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要构造 ack_key
    // - 目的: 直接写入坏 ACK
    let ack_key = ack_key_from_cmd_key(&cmd_key);
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要直接操作 sled
    // - 目的: 写入非法 ACK value
    let db = sled::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要长度不足的 value
    // - 目的: 触发坏包清理
    db.insert(ack_key.clone(), vec![9]).unwrap();
    db.flush().unwrap();
    drop(db);
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要重新打开存储
    // - 目的: 读取并触发清理
    let store = EdgeStore::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要读取 ACK
    // - 目的: 坏包应返回 None
    let ack = store.get_cmd_ack(&cmd_key).unwrap();
    assert!(ack.is_none());
    drop(store);
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要确认坏包被清理
    // - 目的: 验证自动清理生效
    let db = sled::open(dir.path()).unwrap();
    assert!(db.get(ack_key).unwrap().is_none());
}

/// ### 修改记录 (2026-02-28)
/// - 原因: 需要验证非活跃槽 ACK 写入
/// - 目的: 避免误触发清理或计数
#[test]
fn ack_write_on_inactive_slot_is_readable_and_counts_unchanged() {
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要隔离目录
    // - 目的: 保证测试独立
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要打开存储
    // - 目的: 获取 cmd_key
    let store = EdgeStore::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要固定槽位为 0
    // - 目的: 生成 slot0 的 cmd_key
    store.set_active_slot(0).unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要构造 cmd_key
    // - 目的: 用于 slot0 写 ACK
    let cmd_key = store.cmd_key(30, 1);
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要切到 slot1
    // - 目的: 模拟非活跃槽 ACK 写入
    store.set_active_slot(1).unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要记录写入前计数
    // - 目的: 验证不触发计数变化
    let stats_before = store.debug_stats().unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要写入 ACK
    // - 目的: 模拟非活跃槽确认
    store
        .put_cmd_ack(
            &cmd_key,
            AckRecord {
                status: AckStatus::Executed,
                exec_seq: 9,
            },
        )
        .unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要读取写入后计数
    // - 目的: 验证未误触发清理
    let stats_after = store.debug_stats().unwrap();
    assert_eq!(stats_after.slot0_count, stats_before.slot0_count);
    assert_eq!(stats_after.slot1_count, stats_before.slot1_count);
    assert_eq!(stats_after.last_cleanup_ms, stats_before.last_cleanup_ms);
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要读取 ACK
    // - 目的: 确认仍可读回
    let ack = store.get_cmd_ack(&cmd_key).unwrap();
    assert_eq!(
        ack,
        Some(AckRecord {
            status: AckStatus::Executed,
            exec_seq: 9,
        })
    );
}

/// ### 修改记录 (2026-02-28)
/// - 原因: 需要评估清理对 ACK 的影响
/// - 目的: 模拟仍需 ACK 的清理场景
#[test]
fn ack_is_cleared_after_cleanup_trigger_in_current_policy() {
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要隔离目录
    // - 目的: 保证测试独立
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要配置文件路径
    // - 目的: 设置清理阈值
    let config_path = dir.path().join("edge_config.json");
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要阈值为 50%
    // - 目的: 触发 A/B 清理
    write_edge_config(&config_path, 0.5);
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要以配置文件打开存储
    // - 目的: 走配置驱动清理逻辑
    let store = EdgeStore::open_with_config_file(dir.path(), &config_path).unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要在 slot0 写入基准日志
    // - 目的: 形成清理触发基线
    for seq in 0..10 {
        store.put_log(40, seq, b"a".to_vec()).unwrap();
    }
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要在 slot0 写入 ACK
    // - 目的: 模拟仍需 ACK 的场景
    let cmd_key = store.cmd_key(40, 99);
    store
        .put_cmd_ack(
            &cmd_key,
            AckRecord {
                status: AckStatus::Executed,
                exec_seq: 99,
            },
        )
        .unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要切换到 slot1
    // - 目的: 触发对 slot0 的清理
    store.set_active_slot(1).unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要写入超过 50% 数据
    // - 目的: 触发清理逻辑
    for seq in 0..6 {
        store.put_log(41, seq, b"b".to_vec()).unwrap();
    }
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要确认清理发生
    // - 目的: 避免假阳性
    let stats = store.debug_stats().unwrap();
    assert!(stats.last_cleanup_ms > 0);
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要读取 ACK
    // - 目的: 评估当前清理策略影响
    let ack = store.get_cmd_ack(&cmd_key).unwrap();
    assert!(ack.is_none());
}

/// ### 修改记录 (2026-02-28)
/// - 原因: 需要覆盖非法状态分支
/// - 目的: status=3 时应清理
#[test]
fn ack_invalid_status_is_dropped_and_cleared() {
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要隔离目录
    // - 目的: 保证测试独立
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要生成 cmd_key
    // - 目的: 派生 ack_key
    let store = EdgeStore::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要构造 cmd_key
    // - 目的: 复用槽位前缀
    let cmd_key = store.cmd_key(50, 1);
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要释放 store
    // - 目的: 避免重复打开 sled
    drop(store);
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要构造 ack_key
    // - 目的: 直接写入坏 ACK
    let ack_key = ack_key_from_cmd_key(&cmd_key);
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要直接操作 sled
    // - 目的: 写入非法状态 ACK
    let db = sled::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要 9 字节长度
    // - 目的: 覆盖非法状态分支
    let mut value = Vec::with_capacity(9);
    value.push(3);
    value.extend_from_slice(&7u64.to_be_bytes());
    db.insert(ack_key.clone(), value).unwrap();
    db.flush().unwrap();
    drop(db);
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要重新打开存储
    // - 目的: 读取并触发清理
    let store = EdgeStore::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要读取 ACK
    // - 目的: 非法状态应返回 None
    let ack = store.get_cmd_ack(&cmd_key).unwrap();
    assert!(ack.is_none());
    drop(store);
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要确认坏包被清理
    // - 目的: 验证自动清理生效
    let db = sled::open(dir.path()).unwrap();
    assert!(db.get(ack_key).unwrap().is_none());
}

/// ### 修改记录 (2026-02-28)
/// - 原因: 需要覆盖 ACK 覆盖行为
/// - 目的: 同一 cmd_key 以最后写入为准
#[test]
fn ack_overwrites_with_latest_status() {
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要隔离目录
    // - 目的: 保证测试独立
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要打开存储
    // - 目的: 写入 ACK 记录
    let store = EdgeStore::open(dir.path()).unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要构造 cmd_key
    // - 目的: 作为 ACK 键基准
    let cmd_key = store.cmd_key(60, 2);
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要先写入 Dropped
    // - 目的: 覆盖前置状态
    store
        .put_cmd_ack(
            &cmd_key,
            AckRecord {
                status: AckStatus::Dropped,
                exec_seq: 0,
            },
        )
        .unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要再写入 Executed
    // - 目的: 验证覆盖行为
    store
        .put_cmd_ack(
            &cmd_key,
            AckRecord {
                status: AckStatus::Executed,
                exec_seq: 42,
            },
        )
        .unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要读取 ACK
    // - 目的: 断言最后写入生效
    let ack = store.get_cmd_ack(&cmd_key).unwrap();
    assert_eq!(
        ack,
        Some(AckRecord {
            status: AckStatus::Executed,
            exec_seq: 42,
        })
    );
}

/// ### 修改记录 (2026-02-28)
/// - 原因: 需要验证多次清理后 ACK 范围
/// - 目的: 避免清理扩大误删
#[test]
fn ack_persists_in_active_slot_after_two_cleanups() {
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要隔离目录
    // - 目的: 保证测试独立
    let dir = tempdir().unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要配置文件路径
    // - 目的: 设置清理阈值
    let config_path = dir.path().join("edge_config.json");
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要阈值为 50%
    // - 目的: 触发 A/B 清理
    write_edge_config(&config_path, 0.5);
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要以配置文件打开存储
    // - 目的: 走配置驱动清理逻辑
    let store = EdgeStore::open_with_config_file(dir.path(), &config_path).unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要在 slot0 写入基准日志
    // - 目的: 形成清理触发基线
    for seq in 0..10 {
        store.put_log(70, seq, b"a".to_vec()).unwrap();
    }
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要切到 slot1 并触发清理
    // - 目的: 清理 slot0
    store.set_active_slot(1).unwrap();
    for seq in 0..6 {
        store.put_log(71, seq, b"b".to_vec()).unwrap();
    }
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要切回 slot0 并触发清理
    // - 目的: 清理 slot1
    store.set_active_slot(0).unwrap();
    for seq in 0..10 {
        store.put_log(72, seq, b"c".to_vec()).unwrap();
    }
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要在活跃槽写入 ACK
    // - 目的: 作为保留基准
    let cmd_key = store.cmd_key(72, 100);
    store
        .put_cmd_ack(
            &cmd_key,
            AckRecord {
                status: AckStatus::Executed,
                exec_seq: 100,
            },
        )
        .unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要再切到 slot1 并触发清理
    // - 目的: 二次清理后仍需保留活跃槽 ACK
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要避免再次清理 slot0
    // - 目的: 验证误删范围是否扩大
    store.set_active_slot(1).unwrap();
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要写入低于阈值的数据量
    // - 目的: 避免触发清理
    for seq in 0..4 {
        store.put_log(73, seq, b"d".to_vec()).unwrap();
    }
    // ### 修改记录 (2026-02-28)
    // - 原因: 需要读取 ACK
    // - 目的: 验证未被误删
    let ack = store.get_cmd_ack(&cmd_key).unwrap();
    assert_eq!(
        ack,
        Some(AckRecord {
            status: AckStatus::Executed,
            exec_seq: 100,
        })
    );
}
