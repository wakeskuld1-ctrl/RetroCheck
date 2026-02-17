use async_trait::async_trait;
use anyhow::Result;

#[async_trait]
pub trait StorageEngine: Send + Sync {
    async fn execute(&self, sql: &str) -> Result<usize>;
    async fn prepare(&self, tx_id: &str, sql: &str, args: Vec<String>) -> Result<()>;
    async fn commit(&self, tx_id: &str) -> Result<()>;
    async fn rollback(&self, tx_id: &str) -> Result<()>;
    async fn get_version(&self, table: &str) -> Result<i32>;
    async fn check_health(&self) -> Result<()>;
}

pub mod sqlite;
