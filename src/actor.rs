use tokio::sync::{mpsc, oneshot};
use rusqlite::Connection;
use anyhow::{Result, anyhow};
use crate::wal::WalLogger;
use crate::config::NodeRole;
use std::thread;

/// Database Actor
/// 
/// Represents the internal state of the database actor.
/// It holds the actual SQLite connection and WAL logger.
/// 
/// Thread Safety:
/// This struct is NOT thread-safe and is designed to live on a single thread.
/// All interactions must go through the `DbHandle` via message passing.
pub struct DbActor {
    /// The raw SQLite connection. `rusqlite::Connection` is `!Sync`.
    conn: Connection,
    /// Write-Ahead Log for durability and crash recovery.
    wal: WalLogger,
    /// The role of this node (Master/Slave).
    role: NodeRole,
    /// The ID of the currently active transaction, if any.
    /// Used to ensure we commit/rollback the correct transaction.
    active_tx: Option<String>,
}

/// Message types for the Actor
/// 
/// These enum variants represent the operations that can be requested of the actor.
/// Each variant typically includes a `oneshot::Sender` to return the result.
pub enum DbMessage {
    /// Execute a raw SQL statement immediately (outside of 2PC).
    Execute { 
        sql: String, 
        resp: oneshot::Sender<Result<usize>> 
    },
    /// Prepare a distributed transaction (Phase 1).
    Prepare { 
        tx_id: String, 
        sql: String, 
        args: Vec<String>, 
        resp: oneshot::Sender<Result<()>> 
    },
    /// Commit a prepared transaction (Phase 2).
    Commit { 
        tx_id: String, 
        resp: oneshot::Sender<Result<()>> 
    },
    /// Rollback a prepared transaction.
    Rollback { 
        tx_id: String, 
        resp: oneshot::Sender<Result<()>> 
    },
    /// Get the maximum version number from a table (for consistency checks).
    GetMaxVersion { 
        table: String, 
        resp: oneshot::Sender<Result<i32>> 
    },
    /// Check if the actor is alive and processing messages.
    CheckHealth { 
        resp: oneshot::Sender<Result<()>> 
    },
}

/// Handle to the Database Actor
/// 
/// This is the public interface for interacting with the database.
/// It is `Clone` and `Send`, allowing it to be shared across Tokio tasks.
#[derive(Clone)]
pub struct DbHandle {
    /// Channel to send messages to the actor thread.
    sender: mpsc::Sender<DbMessage>,
}

impl DbHandle {
    /// Spawns the database actor in a dedicated background thread.
    /// 
    /// Why a dedicated thread?
    /// SQLite blocking I/O operations can block the Tokio runtime if run on a
    /// standard worker thread. A dedicated thread ensures isolation and performance.
    /// 
    /// # Arguments
    /// 
    /// * `path` - Path to the database file.
    /// * `role` - Role of this node.
    pub fn spawn(path: String, role: NodeRole) -> Result<Self> {
        let (tx, mut rx) = mpsc::channel(32);
        
        // Spawn a dedicated thread for rusqlite (blocking I/O)
        thread::spawn(move || {
            let conn_res = Connection::open(&path);
            let wal_res = WalLogger::new(&format!("{}.wal", path));

            if let (Ok(conn), Ok(wal)) = (conn_res, wal_res) {
                // Initialize actor state
                let mut actor = DbActor { conn, wal, role, active_tx: None };
                
                // We use a minimal Tokio runtime on this thread to support async message receiving.
                // However, the actor logic itself is synchronous (blocking SQLite calls).
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                    
                // Enter the message processing loop
                rt.block_on(async move {
                    while let Some(msg) = rx.recv().await {
                        actor.handle_message(msg);
                    }
                });
            } else {
                eprintln!("Failed to open DB or WAL for {}", path);
            }
        });

        Ok(Self { sender: tx })
    }

    // --- Public Async Methods ---
    // These methods wrap the message passing boilerplate.

    pub async fn execute(&self, sql: String) -> Result<usize> {
        let (tx, rx) = oneshot::channel();
        self.sender.send(DbMessage::Execute { sql, resp: tx }).await.map_err(|_| anyhow!("Actor died"))?;
        rx.await?
    }

    pub async fn prepare(&self, sql: String, args: Vec<String>, tx_id: String) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender.send(DbMessage::Prepare { tx_id, sql, args, resp: tx }).await.map_err(|_| anyhow!("Actor died"))?;
        rx.await?
    }

    pub async fn commit(&self, tx_id: String) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender.send(DbMessage::Commit { tx_id, resp: tx }).await.map_err(|_| anyhow!("Actor died"))?;
        rx.await?
    }

    pub async fn rollback(&self, tx_id: String) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender.send(DbMessage::Rollback { tx_id, resp: tx }).await.map_err(|_| anyhow!("Actor died"))?;
        rx.await?
    }

    pub async fn get_max_version(&self, table: String) -> Result<i32> {
        let (tx, rx) = oneshot::channel();
        self.sender.send(DbMessage::GetMaxVersion { table, resp: tx }).await.map_err(|_| anyhow!("Actor died"))?;
        rx.await?
    }

    pub async fn check_health(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender.send(DbMessage::CheckHealth { resp: tx }).await.map_err(|_| anyhow!("Actor died"))?;
        rx.await?
    }
}

impl DbActor {
    /// The core message handler loop.
    /// This runs on the dedicated thread and executes operations sequentially.
    fn handle_message(&mut self, msg: DbMessage) {
        match msg {
            DbMessage::Execute { sql, resp } => {
                // ### 修改记录 (2026-02-17)
                // - 原因: role 字段未被使用导致告警
                // - 目的: 在执行日志中标注节点角色便于追踪来源
                let _ = self.wal.log(&format!("[{:?}] EXECUTE {}", self.role, sql));
                let res = self.conn.execute(&sql, []);
                let _ = resp.send(res.map_err(|e| anyhow!(e)));
            }
            DbMessage::Prepare { tx_id, sql, args, resp } => {
                // Log to WAL for durability
                // ### 修改记录 (2026-02-17)
                // - 原因: role 字段未被使用导致告警
                // - 目的: 在两阶段提交日志中标注节点角色
                let _ = self.wal.log(&format!("[{:?}] PREPARE {} {} {:?}", self.role, tx_id, sql, args));
                
                // Manual transaction control to avoid Send issues with rusqlite::Transaction
                // We use explicit SQL BEGIN/COMMIT/ROLLBACK.
                
                if self.active_tx.is_some() {
                    let _ = resp.send(Err(anyhow!("Transaction already in progress")));
                    return;
                }

                let res = (|| -> Result<()> {
                    // Start the transaction
                    self.conn.execute("BEGIN", [])?;
                    
                    // Execute the SQL within the transaction scope.
                    // Note: This modifies the database state, but it is not visible to others
                    // until COMMIT is called. This effectively locks the rows.
                    
                    // We need to bind args dynamically.
                    // Rusqlite's `params_from_iter` is useful here.
                    let params = rusqlite::params_from_iter(args.iter());
                    self.conn.execute(&sql, params)?;
                    
                    self.active_tx = Some(tx_id.clone());
                    Ok(())
                })();
                
                let _ = resp.send(res);
            }
            DbMessage::Commit { tx_id, resp } => {
                // ### 修改记录 (2026-02-17)
                // - 原因: role 字段未被使用导致告警
                // - 目的: 在提交日志中标注节点角色
                let _ = self.wal.log(&format!("[{:?}] COMMIT {}", self.role, tx_id));
                
                let res = (|| -> Result<()> {
                    // Ensure we are committing the correct transaction
                    if self.active_tx.as_ref() != Some(&tx_id) {
                        return Err(anyhow!("No matching active transaction"));
                    }
                    self.conn.execute("COMMIT", [])?;
                    self.active_tx = None;
                    Ok(())
                })();
                let _ = resp.send(res);
            }
            DbMessage::Rollback { tx_id, resp } => {
                // ### 修改记录 (2026-02-17)
                // - 原因: role 字段未被使用导致告警
                // - 目的: 在回滚日志中标注节点角色
                let _ = self.wal.log(&format!("[{:?}] ROLLBACK {}", self.role, tx_id));
                
                let res = (|| -> Result<()> {
                    // If the transaction matches, roll it back.
                    // If it doesn't match (e.g., already rolled back or never started), 
                    // we usually just return Ok to be idempotent, unless we want strict checking.
                    if self.active_tx.as_ref() != Some(&tx_id) {
                         return Ok(());
                    }
                    self.conn.execute("ROLLBACK", [])?;
                    self.active_tx = None;
                    Ok(())
                })();
                let _ = resp.send(res);
            }
            DbMessage::GetMaxVersion { table, resp } => {
                let res = (|| -> Result<i32> {
                    let mut stmt = self.conn.prepare(&format!("SELECT MAX(version) FROM {}", table))?;
                    let version: Option<i32> = stmt.query_row([], |row| row.get(0)).unwrap_or(None);
                    Ok(version.unwrap_or(0))
                })();
                let _ = resp.send(res);
            }
            DbMessage::CheckHealth { resp } => {
                let _ = resp.send(Ok(()));
            }
        }
    }
}
