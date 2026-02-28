//! ### 修改记录 (2026-02-27)
//! - 原因: 需要分离过期策略
//! - 目的: 管理面策略与业务执行面解耦

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要表达过期判定结果
/// - 目的: 驱动执行/确认/丢弃分支
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiredDecision {
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要执行决策
    /// - 目的: 直接执行指令
    Execute,
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要丢弃决策
    /// - 目的: 过期或异常时丢弃
    Drop,
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要确认决策
    /// - 目的: 交由业务确认是否执行
    Confirm,
}

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要统一过期判断输入
/// - 目的: 便于策略实现扩展
#[derive(Debug, Clone, Copy)]
pub struct ExpiredContext {
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要过期时间戳
    /// - 目的: 进行超时判断
    pub expire_at_ms: u64,
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要本地时间
    /// - 目的: 与过期时间对比
    pub now_ms: u64,
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要网络可用性
    /// - 目的: 决定兜底策略
    pub network_ok: bool,
}

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要过期策略接口
/// - 目的: 业务可替换默认策略
pub trait ExpiredPolicy {
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要统一策略入口
    /// - 目的: 输出过期决策
    fn decide(&self, ctx: ExpiredContext) -> ExpiredDecision;
}

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要默认策略
/// - 目的: 提供通用兜底行为
#[derive(Debug, Clone, Copy)]
pub struct DefaultExpiredPolicy {
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要宽限期配置
    /// - 目的: 过期后可进入确认
    grace_ms: u64,
}

impl DefaultExpiredPolicy {
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要构造默认策略
    /// - 目的: 注入宽限期配置
    pub fn new(grace_ms: u64) -> Self {
        Self { grace_ms }
    }
}

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要默认过期判断逻辑
/// - 目的: 匹配通用兜底行为
impl ExpiredPolicy for DefaultExpiredPolicy {
    fn decide(&self, ctx: ExpiredContext) -> ExpiredDecision {
        // ### 修改记录 (2026-02-27)
        // - 原因: 未过期直接执行
        // - 目的: 保证时效内的指令可执行
        if ctx.now_ms <= ctx.expire_at_ms {
            return ExpiredDecision::Execute;
        }
        // ### 修改记录 (2026-02-27)
        // - 原因: 网络不可用时兜底确认
        // - 目的: 避免弱网误执行过期指令
        if !ctx.network_ok {
            return ExpiredDecision::Confirm;
        }
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要计算过期时长
        // - 目的: 判定是否进入确认窗口
        let late_ms = ctx.now_ms.saturating_sub(ctx.expire_at_ms);
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要宽限期判断
        // - 目的: 宽限期内交由业务确认
        if late_ms <= self.grace_ms {
            return ExpiredDecision::Confirm;
        }
        // ### 修改记录 (2026-02-27)
        // - 原因: 过期且无确认条件
        // - 目的: 丢弃过期指令
        ExpiredDecision::Drop
    }
}
