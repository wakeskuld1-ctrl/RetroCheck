//! ### 修改记录 (2026-02-17)
//! - 原因: 需要定义 Raft 日志的基础请求/响应结构
//! - 目的: 为 Router 与 StateMachine 提供稳定的数据模型

use serde::{Deserialize, Serialize};

/// ### 修改记录 (2026-02-17)
/// - 原因: Raft 复制需要结构化请求
/// - 目的: 统一读写命令的封装
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 写操作是强一致复制的核心
    /// - 目的: 将 SQL 作为日志内容传递到各节点
    Write {
        /// ### 修改记录 (2026-02-17)
        /// - 原因: SQL 文本需要完整复现
        /// - 目的: 保证日志重放语义一致
        sql: String,
    },
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 读操作需要统一抽象
    /// - 目的: 为强一致读路径提供入口
    Read {
        /// ### 修改记录 (2026-02-17)
        /// - 原因: SQL 文本决定读取逻辑
        /// - 目的: 允许保持 SQLite 风格协议
        sql: String,
    },
}

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要统一返回结构以便序列化
/// - 目的: 让上层对返回值有稳定格式
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 读写结果可能为空
    /// - 目的: 允许返回可选结果与错误处理解耦
    pub value: Option<String>,
}
