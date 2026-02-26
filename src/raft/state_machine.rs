//! ### 修改记录 (2026-02-17)
//! - 原因: 需要实现 Raft StateMachine 的最小落地能力
//! - 目的: 将 Raft apply 与 SQLite 执行对接

use crate::actor::DbHandle;
use crate::config::NodeRole;
use crate::raft::types::{NodeId, Request, Response, TypeConfig};
use anyhow::Result;
use openraft::storage::{RaftSnapshotBuilder, RaftStateMachine};
use openraft::{EntryPayload, LogId, Snapshot, SnapshotMeta, StorageError, StorageIOError, StoredMembership};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要封装 SQLite 访问
/// - 目的: 为 Raft apply 提供稳定执行入口
#[derive(Clone)]
pub struct SqliteStateMachine {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要串行执行 SQLite
    /// - 目的: 复用 Actor 线程模型
    handle: DbHandle,
    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要临时目录存放快照
    /// - 目的: 避免文件冲突
    root_dir: String,
}

impl SqliteStateMachine {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要通过路径初始化状态机
    /// - 目的: 支持不同节点各自数据库文件
    pub fn new(path: String) -> Result<Self> {
        let root_dir = std::path::Path::new(&path)
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_string_lossy()
            .to_string();
        let handle = DbHandle::spawn(path, NodeRole::Master)?;
        Ok(Self { handle, root_dir })
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

    async fn get_meta<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Result<Option<T>> {
        let sql = format!("SELECT value FROM _raft_meta WHERE key = '{}'", key);
        let value_str = self.handle.query_scalar(sql).await?;
        if value_str.is_empty() {
            Ok(None)
        } else {
            let value = serde_json::from_str(&value_str)?;
            Ok(Some(value))
        }
    }
}

impl RaftSnapshotBuilder<TypeConfig> for SqliteStateMachine {
    async fn build_snapshot(
        &mut self,
    ) -> Result<Snapshot<TypeConfig>, StorageError<NodeId>> {
        let last_applied_log_id = self
            .get_meta::<LogId<NodeId>>("last_applied")
            .await
            .map_err(|e| StorageError::IO {
                source: StorageIOError::read(openraft::AnyError::error(e.to_string())),
            })?;
        
        let last_membership = self
            .get_meta::<StoredMembership<NodeId, openraft::BasicNode>>("last_membership")
            .await
            .map_err(|e| StorageError::IO {
                source: StorageIOError::read(openraft::AnyError::error(e.to_string())),
            })?;

        if let (Some(last_log_id), Some(membership)) = (last_applied_log_id, last_membership) {
            let snapshot_id = format!("{}-{}-{}", last_log_id.leader_id.term, last_log_id.index, Uuid::new_v4());
            let snapshot_path = std::path::Path::new(&self.root_dir).join(format!("snapshot-{}.db", snapshot_id));
            let snapshot_path_str = snapshot_path.to_string_lossy().to_string();

            // 1. Create snapshot via VACUUM INTO
            self.handle.create_snapshot(snapshot_path_str.clone())
                .await
                .map_err(|e| StorageError::IO {
                    source: StorageIOError::write_snapshot(None, openraft::AnyError::error(e.to_string())),
                })?;

            // 2. Read snapshot content
            let data = tokio::fs::read(&snapshot_path).await.map_err(|e| StorageError::IO {
                source: StorageIOError::read_snapshot(None, openraft::AnyError::error(e.to_string())),
            })?;

            // 3. Clean up temporary file
            let _ = tokio::fs::remove_file(&snapshot_path).await;

            let meta = SnapshotMeta {
                last_log_id: Some(last_log_id),
                last_membership: membership,
                snapshot_id: snapshot_id.clone(),
            };

            Ok(Snapshot {
                meta,
                snapshot: Box::new(std::io::Cursor::new(data)),
            })
        } else {
             Err(StorageError::IO {
                source: StorageIOError::read(openraft::AnyError::error("No state to snapshot")),
            })
        }
    }
}

impl RaftStateMachine<TypeConfig> for SqliteStateMachine {
    type SnapshotBuilder = Self;

    async fn applied_state(
        &mut self,
    ) -> Result<(Option<LogId<NodeId>>, StoredMembership<NodeId, openraft::BasicNode>), StorageError<NodeId>> {
        let last_applied = self
            .get_meta::<LogId<NodeId>>("last_applied")
            .await
            .map_err(|e| StorageError::IO {
                source: StorageIOError::read(openraft::AnyError::error(e.to_string())),
            })?;
        
        let last_membership = self
            .get_meta::<StoredMembership<NodeId, openraft::BasicNode>>("last_membership")
            .await
            .map_err(|e| StorageError::IO {
                source: StorageIOError::read(openraft::AnyError::error(e.to_string())),
            })?;
        
        let effective_membership = last_membership.unwrap_or_else(|| StoredMembership::default());

        Ok((last_applied, effective_membership))
    }

    async fn apply<I>(&mut self, entries: I) -> Result<Vec<Response>, StorageError<NodeId>>
    where
        I: IntoIterator<Item = openraft::Entry<TypeConfig>> + openraft::OptionalSend,
        I::IntoIter: openraft::OptionalSend,
    {
        let mut sqls = Vec::new();
        let mut entry_map = Vec::new();
        let mut last_log_id = None;

        for (_i, entry) in entries.into_iter().enumerate() {
            last_log_id = Some(entry.log_id);
            match entry.payload {
                EntryPayload::Normal(req) => {
                    match req {
                        Request::Write { sql } => {
                            sqls.push(sql);
                            entry_map.push(Some(sqls.len() - 1)); // Map to the current SQL index
                        }
                        Request::Read { sql: _ } => {
                            // Read requests in log are ignored for state modification
                            entry_map.push(None);
                        }
                    }
                }
                EntryPayload::Membership(ref mem) => {
                    let stored_mem = StoredMembership::new(
                        Some(entry.log_id),
                        mem.clone(),
                    );
                    let mem_json = serde_json::to_string(&stored_mem).map_err(|e| StorageError::IO {
                         source: StorageIOError::write(openraft::AnyError::error(e.to_string())),
                    })?;
                    let sql = format!(
                        "INSERT INTO _raft_meta(key, value) VALUES ('last_membership', '{}')
                         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                        mem_json
                    );
                    sqls.push(sql);
                    // Membership updates don't produce a Response value usually
                    entry_map.push(None);
                }
                EntryPayload::Blank => {
                    entry_map.push(None);
                }
            }
        }

        // Append last_applied update
        if let Some(log_id) = last_log_id {
            let log_id_json = serde_json::to_string(&log_id).map_err(|e| StorageError::IO {
                source: StorageIOError::write(openraft::AnyError::error(e.to_string())),
            })?;
            let sql = format!(
                "INSERT INTO _raft_meta(key, value) VALUES ('last_applied', '{}')
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                log_id_json
            );
            sqls.push(sql);
        }

        if sqls.is_empty() {
             return Ok(Vec::new());
        }

        let results = self.handle.execute_batch(sqls).await.map_err(|e| StorageError::IO {
            source: StorageIOError::write(openraft::AnyError::error(e.to_string())),
        })?;

        // Map results back to responses
        let mut responses = Vec::new();
        // The results vector corresponds to the sqls vector.
        // entry_map maps entry index to sql index (if any).
        // But sql indices in entry_map are relative to the start of sqls vector.
        // Wait, I constructed sqls linearly.
        // So entry_map[k] = Some(idx) means the k-th entry corresponds to sqls[idx].
        // However, `execute_batch` returns results for ALL sqls.
        // So `results[idx]` is the result for `sqls[idx]`.
        
        // One issue: `entry_map` indices need to be correct.
        // In the loop, I did `entry_map.push(Some(i))`? No.
        // I need to track the index in `sqls`.
        
        // Let's correct the loop logic in the actual code below.
        // I will use `sqls.len()` to get current index.
        
        for item in entry_map {
            if let Some(idx) = item {
                if idx < results.len() {
                    responses.push(Response {
                        value: Some(results[idx].to_string()),
                    });
                } else {
                    // Should not happen
                    responses.push(Response { value: None });
                }
            } else {
                responses.push(Response { value: None });
            }
        }
        
        Ok(responses)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.clone()
    }

    async fn begin_receiving_snapshot(&mut self) -> Result<Box<std::io::Cursor<Vec<u8>>>, StorageError<NodeId>> {
        Ok(Box::new(std::io::Cursor::new(Vec::new())))
    }

    async fn install_snapshot(
        &mut self,
        _meta: &SnapshotMeta<NodeId, openraft::BasicNode>,
        snapshot: Box<std::io::Cursor<Vec<u8>>>,
    ) -> Result<(), StorageError<NodeId>> {
        let snapshot_id = Uuid::new_v4().to_string();
        let snapshot_path = std::path::Path::new(&self.root_dir).join(format!("snapshot-{}.db", snapshot_id));
        let snapshot_path_str = snapshot_path.to_string_lossy().to_string();

        let mut file = tokio::fs::File::create(&snapshot_path).await.map_err(|e| StorageError::IO {
            source: StorageIOError::write_snapshot(None, openraft::AnyError::error(e.to_string())),
        })?;
        
        file.write_all(snapshot.get_ref()).await.map_err(|e| StorageError::IO {
            source: StorageIOError::write_snapshot(None, openraft::AnyError::error(e.to_string())),
        })?;
        
        file.sync_all().await.map_err(|e| StorageError::IO {
            source: StorageIOError::write_snapshot(None, openraft::AnyError::error(e.to_string())),
        })?;

        self.handle.install_snapshot(snapshot_path_str.clone()).await.map_err(|e| StorageError::IO {
            source: StorageIOError::write_snapshot(None, openraft::AnyError::error(e.to_string())),
        })?;

        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<TypeConfig>>, StorageError<NodeId>> {
        Ok(None)
    }
}
