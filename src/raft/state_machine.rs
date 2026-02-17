//! ### 修改记录 (2026-02-17)
//! - 原因: 需要实现 Raft StateMachine 的最小落地能力
//! - 目的: 将 Raft apply 与 SQLite 执行对接

use crate::actor::DbHandle;
use crate::config::NodeRole;
use anyhow::Result;

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要封装 SQLite 访问
/// - 目的: 为 Raft apply 提供稳定执行入口
#[derive(Clone)]
pub struct SqliteStateMachine {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要串行执行 SQLite
    /// - 目的: 复用 Actor 线程模型
    handle: DbHandle,
}

impl SqliteStateMachine {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要通过路径初始化状态机
    /// - 目的: 支持不同节点各自数据库文件
    pub fn new(path: String) -> Result<Self> {
        let handle = DbHandle::spawn(path, NodeRole::Master)?;
        Ok(Self { handle })
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要在 Raft apply 中执行写入
    /// - 目的: 让 SQL 通过唯一写入口落地
    /// ### 修改记录 (2026-02-17)
    /// - 原因: gRPC Execute 需要返回 rows_affected
    /// - 目的: 保持 SQLite 风格的返回语义
    pub async fn apply_write(&self, sql: String) -> Result<usize> {
        let rows = self.handle.execute(sql).await?;
        Ok(rows)
    }

    pub async fn apply_batch(&self, sqls: Vec<String>) -> Result<Vec<usize>> {
        self.handle.execute_batch(sqls).await
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 测试需要验证写入结果
    /// - 目的: 提供最小标量查询
    pub async fn query_scalar(&self, sql: String) -> Result<String> {
        self.handle.query_scalar(sql).await
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要写入元数据
    /// - 目的: 供快照调度与测试使用
    pub async fn update_meta(&self, key: String, value: String) -> Result<()> {
        self.handle.update_meta(key, value).await
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 快照后需要轮转 WAL
    /// - 目的: 控制日志膨胀
    pub async fn rotate_wal(&self) -> Result<()> {
        self.handle.rotate_wal().await
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要读取版本号用于一致性校验
    /// - 目的: 提供最小化版本读取能力
    pub async fn get_max_version(&self, table: String) -> Result<i32> {
        self.handle.get_max_version(table).await
    }
}
