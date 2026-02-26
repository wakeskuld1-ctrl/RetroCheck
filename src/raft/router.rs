//! ### 修改记录 (2026-02-17)
//! - 原因: 需要提供最小 Router 入口
//! - 目的: 支持 Leader 判断与转发路径

use crate::pb::ExecuteRequest;
use crate::pb::database_service_client::DatabaseServiceClient;
use crate::raft::raft_node::RaftNode;
use crate::raft::state_machine::SqliteStateMachine;
use anyhow::{Result, anyhow};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

#[derive(Clone)]
pub struct BatchConfig {
    pub max_batch_size: usize,
    pub max_delay_ms: u64,
    pub max_queue_size: usize,
    pub max_wait_ms: u64,
}

struct BatchItem {
    sql: String,
    resp: oneshot::Sender<Result<usize>>,
    cancelled: Arc<AtomicBool>,
}

struct BatchWriter {
    sender: mpsc::Sender<BatchItem>,
}

impl BatchWriter {
    fn new(state_machine: SqliteStateMachine, config: BatchConfig) -> Self {
        let queue_size = config.max_queue_size.max(1);
        let (tx, mut rx) = mpsc::channel::<BatchItem>(queue_size);
        let max_batch_size = config.max_batch_size.max(1);
        let max_delay_ms = config.max_delay_ms;
        tokio::spawn(async move {
            loop {
                let first = match rx.recv().await {
                    Some(item) => item,
                    None => break,
                };
                let mut batch: Vec<BatchItem> = vec![first];
                let mut closed = false;
                let delay = tokio::time::sleep(Duration::from_millis(max_delay_ms));
                tokio::pin!(delay);
                loop {
                    tokio::select! {
                        _ = &mut delay => {
                            break;
                        }
                        msg = rx.recv() => {
                            match msg {
                                Some(item) => {
                                    batch.push(item);
                                    if batch.len() >= max_batch_size {
                                        break;
                                    }
                                }
                                None => {
                                    closed = true;
                                    break;
                                }
                            }
                        }
                    }
                }

                let mut active = Vec::new();
                for item in batch {
                    if item.cancelled.load(Ordering::SeqCst) {
                        let _ = item.resp.send(Err(anyhow!("Batch cancelled")));
                    } else {
                        active.push(item);
                    }
                }
                if active.is_empty() {
                    if closed {
                        break;
                    }
                    continue;
                }
                let sqls: Vec<String> = active.iter().map(|item| item.sql.clone()).collect();
                let res = state_machine.apply_batch(sqls).await;
                match res {
                    Ok(rows) => {
                        if rows.len() == active.len() {
                            for (idx, item) in active.into_iter().enumerate() {
                                let _ = item.resp.send(Ok(rows[idx]));
                            }
                        } else {
                            let err = anyhow!("Batch result size mismatch");
                            for item in active {
                                let _ = item.resp.send(Err(anyhow!(err.to_string())));
                            }
                        }
                    }
                    Err(e) => {
                        let err_text = e.to_string();
                        for item in active {
                            let _ = item.resp.send(Err(anyhow!(err_text.clone())));
                        }
                    }
                }

                if closed {
                    break;
                }
            }
        });
        Self { sender: tx }
    }

    async fn enqueue(&self, sql: String, max_wait_ms: u64) -> Result<usize> {
        let (tx, rx) = oneshot::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.sender
            .try_send(BatchItem {
                sql,
                resp: tx,
                cancelled: cancelled.clone(),
            })
            .map_err(|_| anyhow!("Batch queue full"))?;
        if max_wait_ms == 0 {
            return rx.await.map_err(|_| anyhow!("Batch response dropped"))?;
        }
        let res = tokio::time::timeout(Duration::from_millis(max_wait_ms), rx)
            .await
            .map_err(|_| {
                cancelled.store(true, Ordering::SeqCst);
                anyhow!("Batch wait timeout")
            })?;
        res.map_err(|_| anyhow!("Batch response dropped"))?
    }
}

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要封装路由判断
/// - 目的: 统一 write/read 的入口
pub struct Router {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 测试需要控制 Leader 状态
    /// - 目的: 简化最小实现验证
    is_leader: bool,
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要在本地执行写入
    /// - 目的: 在单节点或 leader 场景复用状态机
    state_machine: Option<SqliteStateMachine>,
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要接入 Raft 写路径
    /// - 目的: 统一 Router 写入到 RaftNode
    raft_node: Option<RaftNode>,
    /// ### 修改记录 (2026-02-25)
    /// - 原因: 非 Leader 需要转发到 Leader
    /// - 目的: 保存 Leader 地址以便执行 gRPC 转发
    leader_addr: Option<String>,
    /// ### 修改记录 (2026-02-25)
    /// - 原因: 转发需要超时保护
    /// - 目的: 避免转发请求无限等待
    forward_timeout_ms: Option<u64>,
    batcher: Option<BatchWriter>,
    batch_max_wait_ms: Option<u64>,
}

impl Router {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 测试需要构造最小 Router
    /// - 目的: 避免依赖真实 Raft 实例
    pub fn new_for_test(is_leader: bool) -> Self {
        Self {
            is_leader,
            state_machine: None,
            raft_node: None,
            leader_addr: None,
            forward_timeout_ms: None,
            batcher: None,
            batch_max_wait_ms: None,
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要统一写入口
    /// - 目的: 根据 Leader 状态选择路径
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要返回 rows_affected
    /// - 目的: 保持 SQLite 风格的返回值
    /// ### 修改记录 (2026-02-25)
    /// - 原因: 非 Leader 需要转发到 Leader
    /// - 目的: 统一由 Router 承担转发职责
    pub async fn write(&self, sql: String) -> Result<usize> {
        if self.is_leader {
            if let Some(raft_node) = &self.raft_node {
                // ### 修改记录 (2026-02-17)
                // - 原因: 需要优先走 Raft 写路径
                // - 目的: 避免状态机直写造成分叉
                raft_node.apply_sql(sql).await
            } else if let Some(batcher) = &self.batcher {
                let wait_ms = self.batch_max_wait_ms.unwrap_or(0);
                batcher.enqueue(sql, wait_ms).await
            } else if let Some(state_machine) = &self.state_machine {
                state_machine.apply_write(sql).await
            } else {
                Ok(0)
            }
        } else {
            // ### 修改记录 (2026-02-25)
            // - 原因: 非 Leader 写入需要转发
            // - 目的: 让客户端保持相同写语义
            if let Some(leader_addr) = &self.leader_addr {
                // ### 修改记录 (2026-02-25)
                // - 原因: 需要动态连接 Leader
                // - 目的: 避免长连接管理复杂度
                let mut client = DatabaseServiceClient::connect(leader_addr.clone())
                    .await
                    .map_err(|e| anyhow!("Leader connect failed: {}", e))?;
                // ### 修改记录 (2026-02-25)
                // - 原因: 需要透传 SQL 写请求
                // - 目的: 保持与对外 Execute 接口一致
                let req = ExecuteRequest { sql };
                // ### 修改记录 (2026-02-25)
                // - 原因: 需要控制转发等待时间
                // - 目的: 防止转发阻塞调用方
                let timeout_ms = self.forward_timeout_ms.unwrap_or(2000);
                let res =
                    tokio::time::timeout(Duration::from_millis(timeout_ms), client.execute(req))
                        .await
                        .map_err(|_| anyhow!("Leader forward timeout"))?;
                // ### 修改记录 (2026-02-25)
                // - 原因: 需要将 gRPC 结果映射为 Router 结果
                // - 目的: 统一错误风格与返回值
                // ### 修改记录 (2026-02-25)
                // - 原因: 错误信息可能包含敏感内容
                // - 目的: 执行错误统一脱敏处理
                let resp = res.map_err(|_| anyhow!("Leader execute failed"))?;
                Ok(resp.into_inner().rows_affected as usize)
            } else {
                // ### 修改记录 (2026-02-25)
                // - 原因: 缺少 Leader 地址无法转发
                // - 目的: 返回明确错误便于上层处理
                Err(anyhow!("Not leader and leader address unknown"))
            }
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要提供最小读路径
    /// - 目的: 支持一致性校验读取
    pub async fn get_version(&self, table: String) -> Result<i32> {
        if self.is_leader {
            if let Some(state_machine) = &self.state_machine {
                state_machine.get_max_version(table).await
            } else {
                Ok(0)
            }
        } else {
            Ok(0)
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要在本地 leader 场景执行写入
    /// - 目的: 为 gRPC 服务提供最小可用写入路径
    pub fn new_local_leader(path: String) -> Result<Self> {
        let state_machine = SqliteStateMachine::new(path)?;
        Ok(Self {
            is_leader: true,
            state_machine: Some(state_machine),
            raft_node: None,
            leader_addr: None,
            forward_timeout_ms: None,
            batcher: None,
            batch_max_wait_ms: None,
        })
    }

    pub fn new_local_leader_with_batch(path: String, config: BatchConfig) -> Result<Self> {
        let state_machine = SqliteStateMachine::new(path)?;
        let batcher = BatchWriter::new(state_machine.clone(), config.clone());
        Ok(Self {
            is_leader: true,
            state_machine: Some(state_machine),
            raft_node: None,
            leader_addr: None,
            forward_timeout_ms: None,
            batcher: Some(batcher),
            batch_max_wait_ms: Some(config.max_wait_ms),
        })
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要 Router 绑定 RaftNode
    /// - 目的: 让写入路径走 RaftNode 封装
    pub fn new_with_raft(raft_node: RaftNode) -> Self {
        Self {
            is_leader: true,
            state_machine: None,
            raft_node: Some(raft_node),
            leader_addr: None,
            forward_timeout_ms: None,
            batcher: None,
            batch_max_wait_ms: None,
        }
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要明确构造非 Leader Router
    /// - 目的: 支持转发到指定 Leader 地址
    pub fn new_follower_with_leader_addr(leader_addr: String) -> Self {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要默认超时参数
        // - 目的: 保持旧构造函数行为不变
        Self::new_follower_with_leader_addr_and_timeout(leader_addr, 2000)
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要可配置转发超时
    /// - 目的: 让测试可稳定触发超时路径
    pub fn new_follower_with_leader_addr_and_timeout(leader_addr: String, timeout_ms: u64) -> Self {
        Self {
            // ### 修改记录 (2026-02-25)
            // - 原因: 非 Leader 构造需要固定角色
            // - 目的: 确保进入转发逻辑
            is_leader: false,
            // ### 修改记录 (2026-02-25)
            // - 原因: 非 Leader 不持有状态机
            // - 目的: 避免误走本地写路径
            state_machine: None,
            // ### 修改记录 (2026-02-25)
            // - 原因: 非 Leader 不持有 RaftNode
            // - 目的: 防止绕过转发
            raft_node: None,
            // ### 修改记录 (2026-02-25)
            // - 原因: 转发需要 Leader 地址
            // - 目的: 作为 gRPC 目标
            leader_addr: Some(leader_addr),
            // ### 修改记录 (2026-02-25)
            // - 原因: 需要精确控制超时
            // - 目的: 让超时测试可控
            forward_timeout_ms: Some(timeout_ms),
            // ### 修改记录 (2026-02-25)
            // - 原因: 非 Leader 不使用批处理
            // - 目的: 保持行为简单
            batcher: None,
            // ### 修改记录 (2026-02-25)
            // - 原因: 非 Leader 不使用批处理
            // - 目的: 避免无效配置
            batch_max_wait_ms: None,
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要更新元数据
    /// - 目的: 为快照调度提供写入口
    pub async fn update_meta(&self, key: String, value: String) -> Result<()> {
        if self.is_leader {
            if let Some(state_machine) = &self.state_machine {
                state_machine.update_meta(key, value).await
            } else {
                Ok(())
            }
        } else {
            Ok(())
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要读取元数据
    /// - 目的: 支持快照调度判断
    pub async fn get_meta_u64(&self, key: &str) -> Result<u64> {
        if self.is_leader {
            if let Some(state_machine) = &self.state_machine {
                let sql = format!("SELECT value FROM _raft_meta WHERE key='{}'", key);
                let value = state_machine.query_scalar(sql).await?;
                Ok(value.parse::<u64>().unwrap_or(0))
            } else {
                Ok(0)
            }
        } else {
            Ok(0)
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要读取 SQLite 时间
    /// - 目的: 统一以数据库时间为基准
    pub async fn current_unix_timestamp(&self) -> Result<u64> {
        if self.is_leader {
            if let Some(state_machine) = &self.state_machine {
                let value = state_machine
                    .query_scalar("SELECT strftime('%s','now')".to_string())
                    .await?;
                Ok(value.parse::<u64>().unwrap_or(0))
            } else {
                Ok(0)
            }
        } else {
            Ok(0)
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要执行 VACUUM INTO
    /// - 目的: 生成快照文件
    pub async fn vacuum_into(&self, snapshot_path: String) -> Result<()> {
        if self.is_leader {
            if let Some(state_machine) = &self.state_machine {
                let safe_path = snapshot_path.replace('\'', "''");
                let sql = format!("VACUUM INTO '{}'", safe_path);
                let _ = state_machine.apply_write(sql).await?;
                Ok(())
            } else {
                Ok(())
            }
        } else {
            Ok(())
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 快照后需要轮转 WAL
    /// - 目的: 控制日志膨胀
    pub async fn rotate_wal(&self) -> Result<()> {
        if self.is_leader {
            if let Some(state_machine) = &self.state_machine {
                state_machine.rotate_wal().await
            } else {
                Ok(())
            }
        } else {
            Ok(())
        }
    }
}
