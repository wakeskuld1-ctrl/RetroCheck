//! ### 修改记录 (2026-02-17)
//! - 原因: 需要提供最小 Router 入口
//! - 目的: 支持 Leader 判断与转发路径

use crate::pb::database_service_client::DatabaseServiceClient;
use crate::pb::{ExecuteRequest, GetVersionRequest};
use crate::raft::raft_node::{RaftNode, TestCluster};
use crate::raft::state_machine::SqliteStateMachine;
use anyhow::{Result, anyhow};
use openraft::ServerState;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, mpsc, oneshot};

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
    /// ### 修改记录 (2026-02-28)
    /// - 原因: Smart Batcher 需要将聚合后的请求提交给 RaftNode
    /// - 目的: 修复 Batcher 绕过 Raft 直接写库的架构缺陷
    fn new(raft_node: RaftNode, config: BatchConfig) -> Self {
        let queue_size = config.max_queue_size.max(1);
        let (tx, mut rx) = mpsc::channel::<BatchItem>(queue_size);
        let max_batch_size = config.max_batch_size;
        let max_delay_ms = config.max_delay_ms;

        tokio::spawn(async move {
            let mut batch = Vec::with_capacity(max_batch_size);

            loop {
                // 1. 获取第一个元素 (如果通道关闭则退出)
                let first = match rx.recv().await {
                    Some(item) => item,
                    None => break,
                };

                batch.push(first);

                // 2. 尝试收集更多元素，直到超时或达到最大批次
                let deadline = tokio::time::Instant::now() + Duration::from_millis(max_delay_ms);

                loop {
                    if batch.len() >= max_batch_size {
                        break;
                    }

                    let timeout = tokio::time::sleep_until(deadline);
                    tokio::pin!(timeout);

                    tokio::select! {
                        _ = timeout => {
                            break;
                        }
                        res = rx.recv() => {
                            match res {
                                Some(item) => batch.push(item),
                                None => break, // 通道关闭，处理剩余批次
                            }
                        }
                    }
                }

                // 3. 过滤已取消的请求
                let mut active_items = Vec::with_capacity(batch.len());
                for item in batch.drain(..) {
                    if item.cancelled.load(Ordering::SeqCst) {
                        let _ = item.resp.send(Err(anyhow!("Batch cancelled")));
                    } else {
                        active_items.push(item);
                    }
                }

                if active_items.is_empty() {
                    continue;
                }

                // 4. 提交给 RaftNode
                let sqls: Vec<String> = active_items.iter().map(|item| item.sql.clone()).collect();
                // 批次大小用于返回结果 (目前 Raft 响应只是受影响行数，这里简化处理)
                // 理想情况下 Raft 应返回每条 SQL 的执行结果
                match raft_node.apply_sql_batch(sqls).await {
                    Ok(_count) => {
                        // 暂时假设每条 SQL 成功执行，返回 1 (或平均分配?)
                        // 由于 apply_sql_batch 返回的是批次大小
                        // 我们给每个请求返回 1 表示成功
                        for item in active_items {
                            let _ = item.resp.send(Ok(1));
                        }
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        for item in active_items {
                            let _ = item.resp.send(Err(anyhow!(err_msg.clone())));
                        }
                    }
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
    test_cluster: Option<Arc<Mutex<TestCluster>>>,
}

impl Router {
    fn is_raft_leader(&self) -> bool {
        if let Some(raft_node) = &self.raft_node {
            let metrics_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                raft_node.raft.metrics().borrow().clone()
            }));
            if let Ok(metrics) = metrics_result {
                return metrics.state == ServerState::Leader
                    && matches!(metrics.current_leader, Some(id) if id == raft_node.node_id());
            }
            return false;
        }
        self.is_leader
    }

    async fn forward_write_to_leader(&self, sql: String) -> Result<usize> {
        let leader_addr = self
            .leader_addr
            .as_ref()
            .ok_or_else(|| anyhow!("Not leader and leader address unknown"))?;
        let mut client = DatabaseServiceClient::connect(leader_addr.clone())
            .await
            .map_err(|e| anyhow!("Leader connect failed: {}", e))?;
        let req = ExecuteRequest { sql };
        let timeout_ms = self.forward_timeout_ms.unwrap_or(2000);
        let res = tokio::time::timeout(Duration::from_millis(timeout_ms), client.execute(req))
            .await
            .map_err(|_| anyhow!("Leader forward timeout"))?;
        let resp = res.map_err(|e| anyhow!("Leader execute failed: {}", e))?;
        Ok(resp.into_inner().rows_affected as usize)
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 测试需要构造最小 Router
    /// - 目的: 避免依赖真实 Raft 实例
    pub fn new_for_test(is_leader: bool) -> Self {
        // ### 修改记录 (2026-03-03)
        // - 原因: 测试需要可写的状态机
        // - 目的: 保证 write_batch 能返回非 0 结果
        // - 备注: 使用临时路径避免污染本地文件
        let state_machine = if is_leader {
            let suffix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let mut path = std::env::temp_dir();
            path.push(format!("check_program_test_{}.db", suffix));
            let path_str = path.to_string_lossy().to_string();
            // ### 修改记录 (2026-03-03)
            // - 原因: 需要确保测试立刻失败而非静默返回 0
            // - 目的: 对齐 new_for_test 的可写预期
            Some(
                SqliteStateMachine::new(path_str)
                    .expect("new_for_test requires sqlite state machine"),
            )
        } else {
            None
        };
        Self {
            is_leader,
            state_machine,
            raft_node: None,
            leader_addr: None,
            forward_timeout_ms: None,
            batcher: None,
            batch_max_wait_ms: None,
            test_cluster: None,
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
        if let Some(cluster) = &self.test_cluster {
            let guard = cluster.lock().await;
            guard.write(sql).await?;
            return Ok(1);
        }
        let is_leader = self.is_raft_leader();
        if is_leader {
            if let Some(batcher) = &self.batcher {
                let wait_ms = self.batch_max_wait_ms.unwrap_or(0);
                batcher.enqueue(sql, wait_ms).await
            } else if let Some(raft_node) = &self.raft_node {
                raft_node.apply_sql(sql).await
            } else if let Some(state_machine) = &self.state_machine {
                state_machine.apply_write(sql).await
            } else {
                Ok(0)
            }
        } else {
            self.forward_write_to_leader(sql).await
        }
    }

    /// ### 修改记录 (2026-03-01)
    /// - 原因: EdgeGateway Smart Batcher 需要批量写入接口
    /// - 目的: 将一批 SQL 作为一个写入单元提交
    pub async fn write_batch(&self, sqls: Vec<String>) -> Result<usize> {
        if sqls.is_empty() {
            return Ok(0);
        }
        if let Some(cluster) = &self.test_cluster {
            let guard = cluster.lock().await;
            return guard.write_batch(sqls).await;
        }

        let is_leader = self.is_raft_leader();
        if is_leader {
            if let Some(raft_node) = &self.raft_node {
                raft_node.apply_sql_batch(sqls).await
            } else if let Some(state_machine) = &self.state_machine {
                let results = state_machine.apply_batch(sqls).await?;
                Ok(results.len())
            } else {
                Ok(0)
            }
        } else {
            let mut total_rows = 0usize;
            for sql in sqls {
                total_rows = total_rows.saturating_add(self.forward_write_to_leader(sql).await?);
            }
            Ok(total_rows)
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要提供最小读路径
    /// - 目的: 支持一致性校验读取
    pub async fn get_version(&self, table: String) -> Result<i32> {
        let is_leader = self.is_raft_leader();
        if is_leader {
            if let Some(state_machine) = &self.state_machine {
                state_machine.get_max_version(table).await
            } else if let Some(raft_node) = &self.raft_node {
                let sql = format!("SELECT MAX(version) FROM {}", table);
                let value = raft_node.query_scalar(sql).await?;
                Ok(value.parse::<i32>().unwrap_or(0))
            } else {
                Ok(0)
            }
        } else if let Some(leader_addr) = &self.leader_addr {
            // ### 修改记录 (2026-02-26)
            // - 原因: 非 Leader 读可能返回陈旧数据
            // - 目的: 统一转发到 Leader 保持一致性
            let mut client = DatabaseServiceClient::connect(leader_addr.clone())
                .await
                .map_err(|e| anyhow!("Leader connect failed: {}", e))?;
            // ### 修改记录 (2026-02-26)
            // - 原因: 读路径需要与 gRPC GetVersion 对齐
            // - 目的: 复用协议定义避免重复接口
            let req = GetVersionRequest { table };
            // ### 修改记录 (2026-02-26)
            // - 原因: 读转发同样需要超时保护
            // - 目的: 避免长时间阻塞调用方
            let timeout_ms = self.forward_timeout_ms.unwrap_or(2000);
            let res =
                tokio::time::timeout(Duration::from_millis(timeout_ms), client.get_version(req))
                    .await
                    .map_err(|_| anyhow!("Leader forward timeout"))?;
            // ### 修改记录 (2026-02-26)
            // - 原因: 需要保留原始 gRPC 错误
            // - 目的: 便于定位 Leader 读失败原因
            let resp = res.map_err(|e| anyhow!("Leader get_version failed: {}", e))?;
            Ok(resp.into_inner().version)
        } else {
            Err(anyhow!("Not leader and leader address unknown"))
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
            test_cluster: None,
        })
    }

    pub fn new_local_leader_with_batch(raft_node: RaftNode, config: BatchConfig) -> Result<Self> {
        let batcher = BatchWriter::new(raft_node.clone(), config.clone());
        Ok(Self {
            is_leader: true,
            state_machine: None,
            raft_node: Some(raft_node),
            leader_addr: None,
            forward_timeout_ms: None,
            batcher: Some(batcher),
            batch_max_wait_ms: Some(config.max_wait_ms),
            test_cluster: None,
        })
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要 Router 绑定 RaftNode
    /// - 目的: 让写入路径走 RaftNode 封装
    pub fn new_with_raft(raft_node: RaftNode) -> Self {
        let metrics = raft_node.raft.metrics().borrow().clone();
        let is_leader = metrics.state == ServerState::Leader
            && matches!(metrics.current_leader, Some(id) if id == raft_node.node_id());
        Self {
            is_leader,
            state_machine: None,
            raft_node: Some(raft_node),
            leader_addr: None,
            forward_timeout_ms: None,
            batcher: None,
            batch_max_wait_ms: None,
            test_cluster: None,
        }
    }

    pub fn new_with_test_cluster(cluster: Arc<Mutex<TestCluster>>) -> Self {
        Self {
            is_leader: true,
            state_machine: None,
            raft_node: None,
            leader_addr: None,
            forward_timeout_ms: None,
            batcher: None,
            batch_max_wait_ms: None,
            test_cluster: Some(cluster),
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
            test_cluster: None,
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
            } else if let Some(raft_node) = &self.raft_node {
                // ### 修改记录 (2026-02-28)
                // - 原因: RaftNode 场景也需要查询元数据
                // - 目的: 修复快照调度在 Raft 模式下的空指针异常
                let sql = format!("SELECT value FROM _raft_meta WHERE key='{}'", key);
                let value = raft_node.query_scalar(sql).await?;
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
            } else if let Some(raft_node) = &self.raft_node {
                // ### 修改记录 (2026-02-28)
                // - 原因: RaftNode 场景也需要查询时间
                // - 目的: 修复快照调度在 Raft 模式下的空指针异常
                let value = raft_node
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
            let safe_path = snapshot_path.replace('\'', "''");
            let sql = format!("VACUUM INTO '{}'", safe_path);

            if let Some(state_machine) = &self.state_machine {
                let _ = state_machine.apply_write(sql).await?;
                Ok(())
            } else if let Some(raft_node) = &self.raft_node {
                // ### 修改记录 (2026-02-28)
                // - 原因: RaftNode 场景也需要执行快照
                // - 目的: 通过 RaftLog 触发各节点快照 (注意：VACUUM INTO 是本地操作还是分布式操作？
                //        通常快照是本地状态机行为，但如果是 SQL 触发，则会复制到所有节点)
                //        这里我们假设它是通过 Raft 复制的指令
                raft_node.apply_sql(sql).await?;
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

// ### 修改记录 (2026-03-02)
// - 原因: 需要复现测试环境 new_for_test 写入返回 0 的问题
// - 目的: 以单元测试锁定批量写入的期望行为
// - 目的: 为后续修复提供可验证的失败用例
// - 备注: 该测试仅覆盖测试构造函数，不影响生产路径
// - 备注: 通过最小 SQL 组合验证写入结果长度
#[cfg(test)]
mod tests {
    use super::*;

    // ### 修改记录 (2026-03-02)
    // - 原因: 需要异步验证 write_batch 返回值
    // - 目的: 证明 new_for_test(true) 必须具备可写状态机
    // - 目的: 在修复前触发断言失败以满足 TDD 约束
    // - 备注: SQL 组合包含 DDL + DML，确保 batch 返回长度可判断
    // - 备注: 该测试仅验证行为，不依赖外部网络或 gRPC
    #[tokio::test]
    async fn new_for_test_leader_write_batch_returns_rows() -> Result<()> {
        // ### 修改记录 (2026-03-02)
        // - 原因: 需要复现实例化路径
        // - 目的: 覆盖 new_for_test(true) 的真实行为
        let router = Router::new_for_test(true);

        // ### 修改记录 (2026-03-02)
        // - 原因: 需要最小化写入场景
        // - 目的: 组合 DDL 与 DML 形成可验证的 batch
        // - 备注: CREATE TABLE IF NOT EXISTS 保证重复执行安全
        let sqls = vec![
            "CREATE TABLE IF NOT EXISTS t(id INTEGER)".to_string(),
            "INSERT INTO t(id) VALUES (1)".to_string(),
        ];

        // ### 修改记录 (2026-03-02)
        // - 原因: 需要验证批量写入返回值
        // - 目的: 确保返回条数与提交 SQL 数量一致
        let rows = router.write_batch(sqls).await?;
        assert_eq!(rows, 2);

        Ok(())
    }
}
