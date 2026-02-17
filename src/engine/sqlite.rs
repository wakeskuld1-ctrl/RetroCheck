use async_trait::async_trait;
// ### 修改记录 (2026-02-17)
// - 原因: anyhow 未被使用导致编译警告
// - 目的: 清理无用依赖保持构建干净
use anyhow::Result;
use crate::engine::StorageEngine;
use crate::actor::DbHandle;
use crate::config::NodeRole;

/// SQLite Storage Engine Implementation
/// 
/// This struct implements the `StorageEngine` trait for SQLite.
/// It acts as an adapter layer, bridging the generic `StorageEngine` interface
/// to the specific `DbHandle` (Actor) implementation.
/// 
/// Why use an Actor?
/// SQLite connections are not thread-safe and must be accessed sequentially.
/// The `DbHandle` spawns a dedicated thread (Actor) to manage the connection,
/// and we communicate with it via message passing (channels).
pub struct SqliteEngine {
    /// Handle to the database actor.
    handle: DbHandle,
}

impl SqliteEngine {
    /// Creates a new SQLite engine instance.
    /// 
    /// This spawns a background thread for the SQLite connection.
    /// 
    /// # Arguments
    /// 
    /// * `path` - Path to the SQLite database file.
    /// 
    /// # Returns
    /// 
    /// * `Result<Self>` - A new `SqliteEngine` instance or error if spawn fails.
    pub fn new(path: &str) -> Result<Self> {
        // We use NodeRole::Master as a placeholder here.
        // In a more complex setup, we might pass the actual role (Master/Slave)
        // to enable/disable certain features (e.g., read-only mode for slaves).
        let handle = DbHandle::spawn(path.to_string(), NodeRole::Master)?;
        Ok(Self { handle })
    }
}

#[async_trait]
impl StorageEngine for SqliteEngine {
    /// Executes a raw SQL statement via the actor.
    async fn execute(&self, sql: &str) -> Result<usize> {
        self.handle.execute(sql.to_string()).await
    }

    /// Prepares a distributed transaction (Phase 1).
    /// Delegates to the actor's `prepare` message.
    async fn prepare(&self, tx_id: &str, sql: &str, args: Vec<String>) -> Result<()> {
        self.handle.prepare(sql.to_string(), args, tx_id.to_string()).await
    }

    /// Commits a prepared transaction (Phase 2).
    /// Delegates to the actor's `commit` message.
    async fn commit(&self, tx_id: &str) -> Result<()> {
        self.handle.commit(tx_id.to_string()).await
    }

    /// Rolls back a prepared transaction.
    /// Delegates to the actor's `rollback` message.
    async fn rollback(&self, tx_id: &str) -> Result<()> {
        self.handle.rollback(tx_id.to_string()).await
    }

    /// Retrieves the current version of a table.
    async fn get_version(&self, table: &str) -> Result<i32> {
        self.handle.get_max_version(table.to_string()).await
    }

    /// Checks if the actor is responsive.
    async fn check_health(&self) -> Result<()> {
        self.handle.check_health().await
    }
}
