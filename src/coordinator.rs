// ### 修改记录 (2026-02-17)
// - 原因: 在库 crate 内使用包名路径导致模块无法解析
// - 目的: 使用 crate:: 路径明确指向当前 crate
use crate::pb::database_service_client::DatabaseServiceClient;
// ### 修改记录 (2026-02-17)
// - 原因: Empty 未使用导致编译警告
// - 目的: 使用 crate:: 路径明确指向当前 crate                                                                                                                                                                                                                                                               // ### 修改记录 (2026-02-17)
// - 原因: 需要使用一致性模式计算所需法定人数
// - 目的: 清理无用依赖保持构建干净
use crate::pb::{
    ExecuteRequest, PrepareRequest, CommitRequest, RollbackRequest, GetVersionRequest
};
// ### 修改记录 (2026-02-17)
// - 原因: 在库 crate 内使用包名路径导致模块无法解析
// - 目的: 将 ConsistencyMode 引入以消除未使用字段告警
use crate::config::{ClusterConfig, ConsistencyMode};
use tonic::transport::Channel;
use anyhow::{Result, anyhow};
use futures::future::join_all;
use uuid::Uuid;
// ### 修改记录 (2026-02-17)
// - 原因: HashMap 未使用导致编译警告
// - 目的: 清理无用依赖保持构建干净

/// MultiDbCoordinator: Distributed Transaction Coordinator
/// 
/// This struct is responsible for orchestrating distributed transactions across
/// a cluster of database nodes (Master and Slaves). It implements a Quorum-based
/// consistency protocol to ensure data integrity even in the presence of node failures.
/// 
/// Key Responsibilities:
/// 1. Connection Management: Maintains gRPC connections to all cluster nodes.
/// 2. Transaction Orchestration: Manages the Two-Phase Commit (2PC) protocol.
/// 3. Consistency Enforcement: Enforces Quorum (N/2 + 1) rules for commits.
/// 4. Failure Handling: Rollbacks transactions if Quorum is not met.
/// 
/// Architecture Note:
/// We use the Actor model implicitly here by treating each remote node as an actor
/// that we send messages to (Prepare, Commit, Rollback). The coordinator itself
/// acts as the state machine driver for the transaction.
pub struct MultiDbCoordinator {
    /// Client connection to the Master node (Primary write target)
    master_client: DatabaseServiceClient<Channel>,
    /// List of client connections to Slave nodes (Replicas)
    slave_clients: Vec<DatabaseServiceClient<Channel>>,
    /// Cluster configuration including addresses and consistency mode
    config: ClusterConfig,
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要支持在 Prepare 与 Commit 之间注入故障
    // - 目的: 增加可选提交前暂停以便脚本控制
    pause_before_commit_ms: Option<u64>,
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要记录已提交事务用于故障后补齐
    // - 目的: 提供最小可行的事务回放能力
    committed_history: Vec<CommittedTx>,
}

// ### 修改记录 (2026-02-17)
// - 原因: 节点重启后缺乏补齐依据导致版本回落
// - 目的: 存储已提交事务的 SQL 与参数供回放使用
#[derive(Clone)]
struct CommittedTx {
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要保留事务内容以支持回放
    // - 目的: 记录原始 SQL 字符串
    sql: String,
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要保留绑定参数以确保回放一致
    // - 目的: 记录 SQL 参数列表
    args: Vec<String>,
}

impl MultiDbCoordinator {
    /// Creates a new Coordinator instance and establishes connections to all nodes.
    /// 
    /// This is an async constructor because establishing network connections (gRPC)
    /// requires awaiting the handshake.
    /// 
    /// # Arguments
    /// 
    /// * `config` - Cluster configuration containing Master/Slave addresses.
    /// 
    /// # Returns
    /// 
    /// * `Result<Self>` - Connected coordinator or error if connections fail.
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要从调用端透传提交前暂停参数
    // - 目的: 允许验证脚本在 2PC 间隙插入宕机
    pub async fn new(config: ClusterConfig, pause_before_commit_ms: Option<u64>) -> Result<Self> {
        // Connect to Master node. 
        // We fail fast if the Master is unreachable as it's critical for the cluster.
        // ### 修改记录 (2026-02-17)
        // - 原因: gRPC Client 泛型类型推断失败导致编译错误
        // - 目的: 明确指定 Channel 类型，稳定类型推断
        let master_client: DatabaseServiceClient<Channel> = DatabaseServiceClient::connect(config.master_addr.clone()).await
            .map_err(|e| anyhow!("Failed to connect to master at {}: {}", config.master_addr, e))?;

        // Connect to all Slave nodes.
        // In a more robust implementation, we might allow partial startup if Quorum is met,
        // but for now, we expect all configured nodes to be reachable at startup.
        let mut slave_clients = Vec::new();
        for addr in &config.slave_addrs {
            // ### 修改记录 (2026-02-17)
            // - 原因: 循环内 Client 泛型类型推断失败
            // - 目的: 明确指定 Channel 类型，避免编译器无法推断
            let client: DatabaseServiceClient<Channel> = DatabaseServiceClient::connect(addr.clone()).await
                .map_err(|e| anyhow!("Failed to connect to slave at {}: {}", addr, e))?;
            slave_clients.push(client);
        }

        Ok(Self {
            master_client,
            slave_clients,
            config,
            // ### 修改记录 (2026-02-17)
            // - 原因: 初始状态下没有可回放事务
            // - 目的: 初始化为空列表以便后续追加
            committed_history: Vec::new(),
            // ### 修改记录 (2026-02-17)
            // - 原因: 默认不开启提交前暂停
            // - 目的: 只有显式指定时才进行等待
            pause_before_commit_ms,
        })
    }

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要根据一致性模式计算所需法定人数
    // - 目的: 让 Quorum/Strong 模式都能正确判断提交阈值
    fn required_quorum(&self, total_nodes: usize) -> usize {
        // ### 修改记录 (2026-02-17)
        // - 原因: 使用模式决定成功阈值
        // - 目的: Strong 需要全部节点，Quorum 需要 N/2+1
        match self.config.mode {
            ConsistencyMode::Strong => total_nodes,
            ConsistencyMode::Quorum => (total_nodes / 2) + 1,
        }
    }

    // ### 修改记录 (2026-02-17)
    // - 原因: 节点恢复后缺乏可重放事务
    // - 目的: 记录已提交事务以支持补齐
    fn record_committed_tx(&mut self, sql: &str, args: &[String]) {
        // ### 修改记录 (2026-02-17)
        // - 原因: 需要控制历史记录长度避免无限增长
        // - 目的: 保持固定窗口以平衡内存与恢复能力
        let history_limit = 32;
        self.committed_history.push(CommittedTx {
            sql: sql.to_string(),
            args: args.to_vec(),
        });
        if self.committed_history.len() > history_limit {
            self.committed_history.remove(0);
        }
    }

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要定位落后节点并进行事务回放
    // - 目的: 在一致性检测失败后进行一次补齐尝试
    async fn replay_committed_txs_to_nodes(
        &self,
        all_clients: &[DatabaseServiceClient<Channel>],
        lagging_indices: &[usize],
    ) -> Result<()> {
        // ### 修改记录 (2026-02-17)
        // - 原因: 没有历史事务时无法补齐
        // - 目的: 提前返回并保留错误信息
        if self.committed_history.is_empty() {
            return Err(anyhow!("No committed history available for replay"));
        }
        for idx in lagging_indices {
            let mut client = all_clients
                .get(*idx)
                .cloned()
                .ok_or_else(|| anyhow!("Invalid node index {}", idx))?;
            for tx in &self.committed_history {
                let tx_id = Uuid::new_v4().to_string();
                let prepare_req = PrepareRequest {
                    tx_id: tx_id.clone(),
                    sql: tx.sql.clone(),
                    args: tx.args.clone(),
                };
                let prepare_res = client.prepare(prepare_req).await;
                if prepare_res.is_err() {
                    let _ = client
                        .rollback(RollbackRequest { tx_id: tx_id.clone() })
                        .await;
                    return Err(anyhow!("Replay prepare failed on node {}", idx));
                }
                let commit_res = client.commit(CommitRequest { tx_id }).await;
                if commit_res.is_err() {
                    return Err(anyhow!("Replay commit failed on node {}", idx));
                }
            }
        }
        Ok(())
    }

    /// Executes a schema migration (DDL) across the cluster.
    /// 
    /// Schema changes are particularly risky in distributed systems because they can't
    /// always be easily rolled back. This implementation uses a "Best Effort" approach:
    /// it tries to apply the change to Master first, then all Slaves.
    /// 
    /// # Arguments
    /// 
    /// * `sql` - The DDL SQL statement (e.g., CREATE TABLE).
    /// 
    /// # Returns
    /// 
    /// * `Result<()>` - Ok if at least Master succeeded, but logs errors for Slaves.
    pub async fn execute_schema_migration(&mut self, sql: &str) -> Result<()> {
        // Schema changes are tricky in distributed systems.
        // For simplicity, we try to execute on all nodes. If one fails, we report error.
        // In production, this should be more robust (e.g. idempotent scripts, version tracking).
        
        println!("Migration: Executing on Master...");
        // Critical: If Master fails, we abort immediately.
        self.master_client.execute(ExecuteRequest { sql: sql.to_string() }).await?;

        println!("Migration: Executing on {} Slaves...", self.slave_clients.len());
        // We execute on slaves sequentially here for simplicity, but parallel is also possible.
        // Parallel execution would speed up large clusters.
        for (i, client) in self.slave_clients.iter_mut().enumerate() {
            match client.execute(ExecuteRequest { sql: sql.to_string() }).await {
                Ok(_) => println!("  Slave {} migrated.", i),
                Err(e) => println!("  Slave {} migration failed: {}", i, e),
            }
        }
        Ok(())
    }

    /// Executes an Atomic Write Transaction across the cluster.
    /// 
    /// This is the core of the distributed system. It implements a Two-Phase Commit (2PC)
    /// protocol with Quorum consistency checks.
    /// 
    /// Steps:
    /// 1. Generate a global Transaction ID (UUID).
    /// 2. Phase 1 (Prepare): Send SQL to all nodes. Nodes lock resources and validate.
    /// 3. Quorum Check: Ensure (N/2 + 1) nodes successfully Prepared.
    /// 4. Phase 2 (Commit/Rollback): 
    ///    - If Quorum reached: Send Commit to prepared nodes.
    ///    - If Quorum failed: Send Rollback to prepared nodes.
    /// 
    /// # Arguments
    /// 
    /// * `sql` - The SQL INSERT/UPDATE/DELETE statement.
    /// * `args` - Parameters for the SQL statement (to avoid SQL injection).
    pub async fn atomic_write(&mut self, sql: &str, args: Vec<String>) -> Result<()> {
        let tx_id = Uuid::new_v4().to_string();
        println!("Starting transaction {} with Quorum check...", tx_id);

        // Phase 1: Prepare
        // We construct the request once and clone it for each node.
        let prepare_req = PrepareRequest {
            tx_id: tx_id.clone(),
            sql: sql.to_string(),
            args: args.clone(),
        };

        // We need to send prepare to ALL nodes (Master + Slaves)
        // Let's gather all clients into a single list for uniform processing.
        // Note: We need mutable access to clients for Tonic request methods.
        // Since `self.master_client` and `self.slave_clients` are separate fields,
        // and we can't borrow `self` mutably twice, we clone the clients.
        // Tonic clients are cheap to clone (they are just handles to a channel).
        
        let mut all_clients = vec![self.master_client.clone()];
        all_clients.extend(self.slave_clients.iter().cloned());
        
        let total_nodes = all_clients.len();
        // ### 修改记录 (2026-02-17)
        // - 原因: 原逻辑固定使用 Quorum 计算
        // - 目的: 根据配置的模式计算所需法定人数
        let required_quorum = self.required_quorum(total_nodes);
        
        println!("  Phase 1: Prepare (Target Quorum: {}/{})", required_quorum, total_nodes);

        // Execute Prepare in PARALLEL using Tokio tasks.
        // This significantly reduces latency compared to sequential execution.
        let mut prepare_futures = Vec::new();
        for mut client in all_clients.clone() {
            let req = prepare_req.clone();
            prepare_futures.push(async move {
                client.prepare(req).await
            });
        }

        // Wait for all Prepare responses
        // ### 修改记录 (2026-02-17)
        // - 原因: 编译器无法推断 join_all 返回的具体类型
        // - 目的: 显式标注结果为 Vec 以稳定类型推断
        let results: Vec<_> = join_all(prepare_futures).await;
        
        let mut success_count = 0;
        // We track which nodes succeeded so we know who to Commit/Rollback later.
        let mut prepared_indices = Vec::new();

        for (i, res) in results.into_iter().enumerate() {
            if res.is_ok() {
                success_count += 1;
                prepared_indices.push(i);
            } else {
                println!("    Node {} Prepare Failed: {:?}", i, res.err());
            }
        }

        // Decision Point: Quorum Check
        if success_count < required_quorum {
            println!("  Quorum failed ({} < {}). Rolling back prepared nodes...", success_count, required_quorum);
            
            // Rollback only prepared nodes.
            // Nodes that failed prepare presumably didn't lock anything or already failed.
            // (In a stricter impl, we might try to rollback everyone just in case).
            let mut rollback_futures = Vec::new();
            for i in prepared_indices {
                let mut client = all_clients[i].clone();
                let tid = tx_id.clone();
                rollback_futures.push(async move {
                    client.rollback(RollbackRequest { tx_id: tid }).await
                });
            }
            join_all(rollback_futures).await;
            return Err(anyhow!("Quorum not reached during Prepare phase"));
        }

        println!("  Phase 2: Commit (Quorum Reached: {})", success_count);
        // ### 修改记录 (2026-02-17)
        // - 原因: 需要在 Prepare 与 Commit 之间留出故障注入窗口
        // - 目的: 允许脚本在暂停期内终止节点进程
        if let Some(ms) = self.pause_before_commit_ms {
            if ms > 0 {
                println!("  Pause Before Commit: {} ms", ms);
                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            }
        }

        // Phase 2: Commit
        // We only send Commit to nodes that successfully Prepared.
        // Nodes that failed Prepare are assumed to be out of sync or offline for this Tx.
        // They will need catch-up/replication later (not implemented here).
        
        let mut commit_futures = Vec::new();
        for i in prepared_indices {
            let mut client = all_clients[i].clone();
            let tid = tx_id.clone();
            commit_futures.push(async move {
                client.commit(CommitRequest { tx_id: tid }).await
            });
        }

        // ### 修改记录 (2026-02-17)
        // - 原因: 编译器无法推断 join_all 返回的具体类型
        // - 目的: 显式标注结果为 Vec 以稳定类型推断
        let commit_results: Vec<_> = join_all(commit_futures).await;
        
        // Verify Commit Success
        let mut commit_success = 0;
        for res in commit_results {
            if res.is_ok() {
                commit_success += 1;
            }
        }

        // Final Safety Check
        // If we lose Quorum during Commit (e.g., nodes crash after Prepare but before Commit),
        // we are in a dangerous state (Partial Commit).
        if commit_success < required_quorum {
             // This is a critical failure state (Inconsistent state).
             // In production, we'd need manual intervention or complex recovery (WAL replay).
             return Err(anyhow!("Critical: Quorum lost during Commit phase! Data may be inconsistent."));
        }

        println!("Transaction {} Committed Successfully on {} nodes.", tx_id, commit_success);
        // ### 修改记录 (2026-02-17)
        // - 原因: 节点重启后缺乏补齐依据
        // - 目的: 在提交成功后记录可回放的事务
        self.record_committed_tx(sql, &args);
        Ok(())
    }

    /// Verifies data consistency across the cluster.
    /// 
    /// It queries the `version` column (or equivalent) from all nodes and checks
    /// if they match. This is useful for monitoring and verifying the system state.
    /// 
    /// # Arguments
    /// 
    /// * `table` - The table to check consistency for.
    /// 
    /// # Returns
    /// 
    /// * `Result<()>` - Ok if all reachable nodes have the same version.
    pub async fn verify_consistency(&mut self, table: &str) -> Result<()> {
        let req = GetVersionRequest { table: table.to_string() };
        
        let mut all_clients = vec![self.master_client.clone()];
        all_clients.extend(self.slave_clients.iter().cloned());
        
        // Parallel fetch of versions
        let mut futures = Vec::new();
        for mut client in all_clients.clone() {
            let r = req.clone();
            futures.push(async move { client.get_version(r).await });
        }

        // ### 修改记录 (2026-02-17)
        // - 原因: 编译器无法推断 join_all 返回的具体类型
        // - 目的: 显式标注结果为 Vec 以稳定类型推断
        let results: Vec<_> = join_all(futures).await;
        let mut versions_by_index: Vec<Option<i32>> = Vec::new();

        for (i, res) in results.into_iter().enumerate() {
            match res {
                Ok(resp) => {
                    let v = resp.into_inner().version;
                    println!("  Node {}: Version {}", i, v);
                    versions_by_index.push(Some(v));
                }
                Err(e) => {
                    println!("  Node {}: Error fetching version: {}", i, e);
                    versions_by_index.push(None);
                }
            }
        }

        // Simple check: are all available versions the same?
        let available_versions: Vec<i32> = versions_by_index.iter().flatten().copied().collect();
        if available_versions.is_empty() {
            return Err(anyhow!("No versions retrieved"));
        }
        let max_version = *available_versions.iter().max().unwrap();
        let all_match = available_versions.iter().all(|&v| v == max_version);

        if all_match {
            println!("Consistency Check Passed: All reachable nodes at version {}", max_version);
            Ok(())
        } else {
            // ### 修改记录 (2026-02-17)
            // - 原因: 节点重启后可能缺失已提交事务
            // - 目的: 针对落后节点执行一次事务回放再验证
            let lagging_indices: Vec<usize> = versions_by_index
                .iter()
                .enumerate()
                .filter_map(|(i, v)| v.filter(|vv| *vv < max_version).map(|_| i))
                .collect();
            if lagging_indices.is_empty() {
                return Err(anyhow!(
                    "Consistency Check Failed: Versions mismatch {:?}",
                    available_versions
                ));
            }
            self.replay_committed_txs_to_nodes(&all_clients, &lagging_indices)
                .await?;
            let mut recheck_futures = Vec::new();
            for mut client in all_clients {
                let r = req.clone();
                recheck_futures.push(async move { client.get_version(r).await });
            }
            let recheck_results: Vec<_> = join_all(recheck_futures).await;
            let mut recheck_versions = Vec::new();
            for (i, res) in recheck_results.into_iter().enumerate() {
                match res {
                    Ok(resp) => {
                        let v = resp.into_inner().version;
                        println!("  Node {}: Version {}", i, v);
                        recheck_versions.push(v);
                    }
                    Err(e) => println!("  Node {}: Error fetching version: {}", i, e),
                }
            }
            if recheck_versions.is_empty() {
                return Err(anyhow!("No versions retrieved after replay"));
            }
            let recheck_first = recheck_versions[0];
            let recheck_all_match = recheck_versions.iter().all(|&v| v == recheck_first);
            if recheck_all_match {
                println!(
                    "Consistency Check Passed: All reachable nodes at version {}",
                    recheck_first
                );
                Ok(())
            } else {
                Err(anyhow!(
                    "Consistency Check Failed: Versions mismatch {:?}",
                    recheck_versions
                ))
            }
        }
    }
}
