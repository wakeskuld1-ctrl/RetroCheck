use crate::config::NodeRole;
use crate::wal::WalLogger;
use anyhow::{Result, anyhow};
use rusqlite::Connection;
use std::thread;
use tokio::sync::{mpsc, oneshot};

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
        resp: oneshot::Sender<Result<usize>>,
    },
    ExecuteBatch {
        sqls: Vec<String>,
        resp: oneshot::Sender<Result<Vec<usize>>>,
    },
    /// Prepare a distributed transaction (Phase 1).
    Prepare {
        tx_id: String,
        sql: String,
        args: Vec<String>,
        resp: oneshot::Sender<Result<()>>,
    },
    /// Commit a prepared transaction (Phase 2).
    Commit {
        tx_id: String,
        resp: oneshot::Sender<Result<()>>,
    },
    /// Rollback a prepared transaction.
    Rollback {
        tx_id: String,
        resp: oneshot::Sender<Result<()>>,
    },
    /// Get the maximum version number from a table (for consistency checks).
    GetMaxVersion {
        table: String,
        resp: oneshot::Sender<Result<i32>>,
    },
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要支持状态机读取验证
    /// - 目的: 增加最小化的标量查询能力
    QueryScalar {
        /// ### 修改记录 (2026-02-17)
        /// - 原因: 需要执行查询语句
        /// - 目的: 使用 SQL 文本驱动查询
        sql: String,
        /// ### 修改记录 (2026-02-17)
        /// - 原因: 需要返回标量结果
        /// - 目的: 支持状态机读取验证
        resp: oneshot::Sender<Result<String>>,
    },
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要维护元数据
    /// - 目的: 支持快照时间与索引持久化
    UpdateMeta {
        /// ### 修改记录 (2026-02-17)
        /// - 原因: 需要指定元数据键
        /// - 目的: 支持按键更新
        key: String,
        /// ### 修改记录 (2026-02-17)
        /// - 原因: 需要写入元数据值
        /// - 目的: 统一存储字符串值
        value: String,
        /// ### 修改记录 (2026-02-17)
        /// - 原因: 需要返回更新结果
        /// - 目的: 保持调用方可感知错误
        resp: oneshot::Sender<Result<()>>,
    },
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 快照后需要轮转 WAL
    /// - 目的: 控制日志膨胀
    RotateWal {
        /// ### 修改记录 (2026-02-17)
        /// - 原因: 需要返回轮转结果
        /// - 目的: 让调用方感知失败
        resp: oneshot::Sender<Result<()>>,
    },
    /// Check if the actor is alive and processing messages.
    CheckHealth { resp: oneshot::Sender<Result<()>> },
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
                let mut actor = DbActor {
                    conn,
                    wal,
                    role,
                    active_tx: None,
                };
                // ### 修改记录 (2026-02-17)
                // - 原因: 需要初始化元数据表
                // - 目的: 为快照切割提供基础
                let _ = actor.init_meta();

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
        self.sender
            .send(DbMessage::Execute { sql, resp: tx })
            .await
            .map_err(|_| anyhow!("Actor died"))?;
        rx.await?
    }

    pub async fn execute_batch(&self, sqls: Vec<String>) -> Result<Vec<usize>> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(DbMessage::ExecuteBatch { sqls, resp: tx })
            .await
            .map_err(|_| anyhow!("Actor died"))?;
        rx.await?
    }

    pub async fn prepare(&self, sql: String, args: Vec<String>, tx_id: String) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(DbMessage::Prepare {
                tx_id,
                sql,
                args,
                resp: tx,
            })
            .await
            .map_err(|_| anyhow!("Actor died"))?;
        rx.await?
    }

    pub async fn commit(&self, tx_id: String) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(DbMessage::Commit { tx_id, resp: tx })
            .await
            .map_err(|_| anyhow!("Actor died"))?;
        rx.await?
    }

    pub async fn rollback(&self, tx_id: String) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(DbMessage::Rollback { tx_id, resp: tx })
            .await
            .map_err(|_| anyhow!("Actor died"))?;
        rx.await?
    }

    pub async fn get_max_version(&self, table: String) -> Result<i32> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(DbMessage::GetMaxVersion { table, resp: tx })
            .await
            .map_err(|_| anyhow!("Actor died"))?;
        rx.await?
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 状态机测试需要标量查询能力
    /// - 目的: 提供最小读取接口
    pub async fn query_scalar(&self, sql: String) -> Result<String> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(DbMessage::QueryScalar { sql, resp: tx })
            .await
            .map_err(|_| anyhow!("Actor died"))?;
        rx.await?
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要写入元数据
    /// - 目的: 供快照调度器与状态机复用
    pub async fn update_meta(&self, key: String, value: String) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(DbMessage::UpdateMeta {
                key,
                value,
                resp: tx,
            })
            .await
            .map_err(|_| anyhow!("Actor died"))?;
        rx.await?
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 快照后需要轮转 WAL
    /// - 目的: 控制日志膨胀
    pub async fn rotate_wal(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(DbMessage::RotateWal { resp: tx })
            .await
            .map_err(|_| anyhow!("Actor died"))?;
        rx.await?
    }

    pub async fn check_health(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(DbMessage::CheckHealth { resp: tx })
            .await
            .map_err(|_| anyhow!("Actor died"))?;
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
                // ### 修改记录 (2026-02-17)
                // - 原因: 需要写入后记录数据库时间
                // - 目的: 支撑按写入时间切割快照
                if res.is_ok() {
                    let _ = self.conn.execute(
                        "INSERT INTO _raft_meta(key, value) VALUES ('last_write_at', strftime('%s','now'))
                         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                        [],
                    );
                    let _ = self.bump_write_seq(1);
                }
                let _ = resp.send(res.map_err(|e| anyhow!(e)));
            }
            DbMessage::ExecuteBatch { sqls, resp } => {
                if self.active_tx.is_some() {
                    let _ = resp.send(Err(anyhow!("Transaction already in progress")));
                    return;
                }
                let mut rows: Vec<usize> = Vec::new();
                let mut batch_error: Option<anyhow::Error> = None;
                let begin_res = self.conn.execute("BEGIN", []);
                if begin_res.is_err() {
                    let _ = resp.send(Err(anyhow!("Failed to begin batch transaction")));
                    return;
                }
                for sql in sqls.iter() {
                    match self.conn.execute(sql, []) {
                        Ok(count) => rows.push(count),
                        Err(e) => {
                            batch_error = Some(anyhow!(e));
                            break;
                        }
                    }
                }
                if batch_error.is_none() && self.conn.execute("COMMIT", []).is_err() {
                    batch_error = Some(anyhow!("Failed to commit batch transaction"));
                }
                if let Some(err) = batch_error {
                    let _ = self.conn.execute("ROLLBACK", []);
                    let _ = resp.send(Err(err));
                } else {
                    let _ = self.conn.execute(
                        "INSERT INTO _raft_meta(key, value) VALUES ('last_write_at', strftime('%s','now'))
                         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                        [],
                    );
                    let _ = self.bump_write_seq(sqls.len() as i32);
                    let _ = resp.send(Ok(rows));
                }
            }
            DbMessage::Prepare {
                tx_id,
                sql,
                args,
                resp,
            } => {
                // Log to WAL for durability
                // ### 修改记录 (2026-02-17)
                // - 原因: role 字段未被使用导致告警
                // - 目的: 在两阶段提交日志中标注节点角色
                let _ = self.wal.log(&format!(
                    "[{:?}] PREPARE {} {} {:?}",
                    self.role, tx_id, sql, args
                ));

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
                    self.conn.execute(
                        "INSERT INTO _raft_meta(key, value) VALUES ('last_write_at', strftime('%s','now'))
                         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                        [],
                    )?;
                    self.bump_write_seq(1)?;
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
                let _ = self
                    .wal
                    .log(&format!("[{:?}] ROLLBACK {}", self.role, tx_id));

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
                let _ = table;
                let res = (|| -> Result<i32> {
                    let mut stmt = self.conn.prepare(
                        "SELECT CAST(value AS INTEGER) FROM _raft_meta WHERE key='last_write_seq'",
                    )?;
                    let version: Option<i32> = stmt.query_row([], |row| row.get(0)).unwrap_or(None);
                    Ok(version.unwrap_or(0))
                })();
                let _ = resp.send(res);
            }
            DbMessage::QueryScalar { sql, resp } => {
                // ### 修改记录 (2026-02-17)
                // - 原因: 需要支持状态机读取验证
                // - 目的: 执行最小化标量查询
                let res = (|| -> Result<String> {
                    let mut stmt = self.conn.prepare(&sql)?;
                    // ### 修改记录 (2026-02-17)
                    // - 原因: 需要兼容文本与数字类型
                    // - 目的: 避免读取 TEXT 时失败
                    let value: rusqlite::types::Value = stmt.query_row([], |row| row.get(0))?;
                    match value {
                        rusqlite::types::Value::Null => Ok(String::new()),
                        rusqlite::types::Value::Integer(v) => Ok(v.to_string()),
                        rusqlite::types::Value::Real(v) => Ok(v.to_string()),
                        rusqlite::types::Value::Text(v) => Ok(v),
                        rusqlite::types::Value::Blob(v) => {
                            Ok(String::from_utf8_lossy(&v).to_string())
                        }
                    }
                })();
                let _ = resp.send(res);
            }
            DbMessage::UpdateMeta { key, value, resp } => {
                // ### 修改记录 (2026-02-17)
                // - 原因: 需要持久化元数据
                // - 目的: 支持快照时间与索引更新
                let res = self.conn.execute(
                    "INSERT INTO _raft_meta(key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    rusqlite::params![key, value],
                );
                let _ = resp.send(res.map(|_| ()).map_err(|e| anyhow!(e)));
            }
            DbMessage::RotateWal { resp } => {
                // ### 修改记录 (2026-02-17)
                // - 原因: 快照后需要切换日志文件
                // - 目的: 控制 WAL 增长
                let res = self.wal.rotate();
                let _ = resp.send(res);
            }
            DbMessage::CheckHealth { resp } => {
                let _ = resp.send(Ok(()));
            }
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要初始化元数据表
    /// - 目的: 确保元数据查询可用
    fn init_meta(&mut self) -> Result<()> {
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS _raft_meta (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )?;
        // ### 修改记录 (2026-02-17)
        // - 原因: 需要默认键值
        // - 目的: 避免查询不存在时失败
        self.conn.execute(
            "INSERT OR IGNORE INTO _raft_meta(key, value) VALUES
             ('last_write_at', '0'),
             ('last_snapshot_at', '0'),
             ('last_snapshot_index', '0'),
             ('last_write_seq', '0')",
            [],
        )?;
        Ok(())
    }

    fn bump_write_seq(&mut self, delta: i32) -> Result<()> {
        self.conn.execute(
            "UPDATE _raft_meta SET value = CAST(value AS INTEGER) + ?1 WHERE key='last_write_seq'",
            rusqlite::params![delta],
        )?;
        Ok(())
    }
}
