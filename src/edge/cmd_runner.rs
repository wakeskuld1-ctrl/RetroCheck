//! ### 修改记录 (2026-02-27)
//! - 原因: 需要运行态指令执行器
//! - 目的: 管理面策略与业务执行面解耦

// ### 修改记录 (2026-02-27)
// - 原因: 需要错误处理
// - 目的: 统一返回 Result
use anyhow::Result;
// ### 修改记录 (2026-02-27)
// - 原因: 需要时间戳
// - 目的: 获取本地时间
use std::time::{SystemTime, UNIX_EPOCH};
// ### 修改记录 (2026-02-27)
// - 原因: 需要策略接口
// - 目的: 注入过期判断逻辑
use crate::edge::policy::{DefaultExpiredPolicy, ExpiredContext, ExpiredDecision, ExpiredPolicy};
// ### 修改记录 (2026-02-27)
// - 原因: 需要访问存储
// - 目的: 扫描与更新 cmd 数据
// ### 修改记录 (2026-02-28)
// - 原因: 需要写入 ACK 结果
// - 目的: 形成指令闭环
use crate::edge::store::{AckRecord, AckStatus, EdgeStore};

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要确认决策
/// - 目的: 决定过期指令是否执行
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmDecision {
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要确认后执行
    /// - 目的: 允许过期指令执行
    Execute,
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要确认后丢弃
    /// - 目的: 阻止过期指令执行
    Drop,
}

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要执行器接口
/// - 目的: 业务侧实现执行逻辑
pub trait CmdExecutor {
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要执行指令
    /// - 目的: 返回执行序号用于推进 HWM
    fn execute(&self, payload: Vec<u8>) -> Result<u64>;

    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要过期确认入口
    /// - 目的: 业务侧决定是否执行
    fn confirm_expired(
        &self,
        payload: Vec<u8>,
        expire_at_ms: u64,
        now_ms: u64,
    ) -> Result<ConfirmDecision>;
}

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要运行选项
/// - 目的: 注入时间与网络状态
#[derive(Debug, Clone, Copy)]
pub struct RunOptions {
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要可控时间
    /// - 目的: 方便测试固定时间
    pub now_ms: Option<u64>,
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要网络状态
    /// - 目的: 触发兜底策略
    pub network_ok: bool,
}

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要默认运行参数
/// - 目的: 保持调用简单
impl Default for RunOptions {
    fn default() -> Self {
        Self {
            // ### 修改记录 (2026-02-27)
            // - 原因: 默认使用系统时间
            // - 目的: 避免调用方必传
            now_ms: None,
            // ### 修改记录 (2026-02-27)
            // - 原因: 默认网络可用
            // - 目的: 保持通用默认
            network_ok: true,
        }
    }
}

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要运行器结构
/// - 目的: 封装扫描与执行流程
pub struct CmdRunner<'a, P: ExpiredPolicy, E: CmdExecutor> {
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要访问存储
    /// - 目的: 读取与更新 cmd 数据
    store: &'a EdgeStore,
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要过期策略
    /// - 目的: 统一过期判定
    policy: P,
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要执行器
    /// - 目的: 执行业务逻辑
    executor: &'a E,
}

impl<'a, P: ExpiredPolicy, E: CmdExecutor> CmdRunner<'a, P, E> {
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要构造运行器
    /// - 目的: 注入策略与执行器
    pub fn new(store: &'a EdgeStore, policy: P, executor: &'a E) -> Self {
        Self {
            store,
            policy,
            executor,
        }
    }

    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要执行一次扫描
    /// - 目的: 执行或丢弃待处理指令
    pub fn run_once(&self, options: RunOptions) -> Result<()> {
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要本地时间
        // - 目的: 与过期时间对比
        let now_ms = options.now_ms.unwrap_or_else(Self::now_ms);
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要读取断点
        // - 目的: 从上次位置继续扫描
        let last_key = self.store.get_cmd_last_key()?;
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要扫描 cmd 键
        // - 目的: 处理所有待执行指令
        let keys = self.store.scan_cmd_keys(last_key)?;
        for key in keys {
            // ### 修改记录 (2026-02-27)
            // - 原因: 需要读取信封
            // - 目的: 获取过期时间与载荷
            let envelope = match self.store.get_cmd_envelope(&key)? {
                Some(envelope) => envelope,
                None => {
                    self.store.set_cmd_last_key(key)?;
                    continue;
                }
            };
            // ### 修改记录 (2026-02-27)
            // - 原因: 需要构造策略上下文
            // - 目的: 传递过期与网络信息
            let ctx = ExpiredContext {
                expire_at_ms: envelope.expire_at_ms,
                now_ms,
                network_ok: options.network_ok,
            };
            // ### 修改记录 (2026-02-27)
            // - 原因: 需要策略决策
            // - 目的: 选择执行/确认/丢弃
            match self.policy.decide(ctx) {
                ExpiredDecision::Execute => {
                    self.execute_and_commit(&key, envelope.payload)?;
                }
                ExpiredDecision::Drop => {
                    // ### 修改记录 (2026-02-28)
                    // - 原因: 需要落盘丢弃结果
                    // - 目的: 供 Hub 对账
                    self.store.put_cmd_ack(
                        &key,
                        AckRecord {
                            status: AckStatus::Dropped,
                            exec_seq: 0,
                        },
                    )?;
                    self.store.remove_cmd(&key)?;
                    self.store.set_cmd_last_key(key)?;
                }
                ExpiredDecision::Confirm => {
                    self.confirm_and_maybe_execute(&key, envelope, now_ms)?;
                }
            }
        }
        Ok(())
    }

    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要确认过期指令
    /// - 目的: 由业务侧决定是否执行
    fn confirm_and_maybe_execute(
        &self,
        key: &[u8],
        envelope: crate::edge::store::CmdEnvelope,
        now_ms: u64,
    ) -> Result<()> {
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要确认过期
        // - 目的: 交由业务侧判断
        let decision = self.executor.confirm_expired(
            envelope.payload.clone(),
            envelope.expire_at_ms,
            now_ms,
        )?;
        match decision {
            ConfirmDecision::Execute => {
                self.execute_and_commit(key, envelope.payload)?;
            }
            ConfirmDecision::Drop => {
                // ### 修改记录 (2026-02-28)
                // - 原因: 需要落盘丢弃结果
                // - 目的: 供 Hub 对账
                self.store.put_cmd_ack(
                    key,
                    AckRecord {
                        status: AckStatus::Dropped,
                        exec_seq: 0,
                    },
                )?;
                self.store.remove_cmd(key)?;
                self.store.set_cmd_last_key(key.to_vec())?;
            }
        }
        Ok(())
    }

    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要统一执行路径
    /// - 目的: 执行后推进状态与断点
    fn execute_and_commit(&self, key: &[u8], payload: Vec<u8>) -> Result<()> {
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要执行业务逻辑
        // - 目的: 返回状态序号
        let seq = self.executor.execute(payload)?;
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要持久化物理状态
        // - 目的: 作为 HWM 推进前置
        self.store.persist_state(seq)?;
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要推进 HWM
        // - 目的: 标记已确认执行
        self.store.ack_hwm(seq)?;
        // ### 修改记录 (2026-02-28)
        // - 原因: 需要落盘执行结果
        // - 目的: 指令闭环对账
        self.store.put_cmd_ack(
            key,
            AckRecord {
                status: AckStatus::Executed,
                exec_seq: seq,
            },
        )?;
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要推进断点
        // - 目的: 避免重复执行
        self.store.set_cmd_last_key(key.to_vec())?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要时间戳
    /// - 目的: 统一时间来源
    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
}

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要默认策略构造
/// - 目的: 读取配置并支持覆盖
impl<'a, E: CmdExecutor> CmdRunner<'a, DefaultExpiredPolicy, E> {
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要默认策略运行器
    /// - 目的: 使用配置的超时宽限期
    pub fn new_with_default_policy(
        store: &'a EdgeStore,
        executor: &'a E,
        timeout_override_ms: Option<u64>,
    ) -> Self {
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要读取默认超时
        // - 目的: 与配置保持一致
        let default_timeout_ms = store.config().cmd_timeout_ms;
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要支持覆盖
        // - 目的: 允许应用侧调整
        let grace_ms = timeout_override_ms.unwrap_or(default_timeout_ms);
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要构造默认策略
        // - 目的: 传入宽限期
        let policy = DefaultExpiredPolicy::new(grace_ms);
        CmdRunner::new(store, policy, executor)
    }
}
