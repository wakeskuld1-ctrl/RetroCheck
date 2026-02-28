//! ### 修改记录 (2026-02-17)
//! - 原因: 需要提供 RaftNode 最小封装
//! - 目的: 为 Raft 核心落地提供可测试入口

use crate::raft::network::{RaftNetworkFactoryImpl, RaftNetworkTarget, RaftRouter};
use crate::raft::state_machine::SqliteStateMachine;
use crate::raft::store::RaftStore;
use crate::raft::types::{NodeId, TypeConfig};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use openraft::error::{InstallSnapshotError, RaftError};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
// ### 修改记录 (2026-02-27)
// - 原因: 需要读取节点状态
// - 目的: 选举与领导者判断
use openraft::{Config, Raft, ServerState};
use std::path::PathBuf;
use std::sync::Arc;
// ### 修改记录 (2026-02-27)
// - 原因: 需要等待选举
// - 目的: 支持 failover 测试稳定
use std::time::Duration;
// ### 修改记录 (2026-02-27)
// - 原因: 需要异步等待
// - 目的: 轮询 leader 状态
use tokio::time::sleep;
use uuid::Uuid;

/// ### 修改记录 (2026-02-26)
/// - 原因: 定义 Raft 核心类型别名
/// - 目的: 简化代码引用，明确泛型参数
pub type RaftCore = Raft<TypeConfig>;

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要在测试网络中共享节点实例
/// - 目的: 允许 TestNetwork 保存节点副本
#[derive(Clone)]
pub struct RaftNode {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要唯一节点标识
    /// - 目的: 与 Raft Core 的 NodeId 对齐
    node_id: NodeId,
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要保存存储目录
    /// - 目的: 让 Raft 日志与快照有固定落盘位置
    base_dir: PathBuf,
    /// ### 修改记录 (2026-02-26)
    /// - 原因: 集成 OpenRaft 核心
    /// - 目的: 提供完整的 Raft 功能
    pub raft: RaftCore,
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要执行 SQL apply
    /// - 目的: 提供最小的状态机落地能力，保留句柄用于本地查询
    pub state_machine: SqliteStateMachine,
}

#[async_trait]
impl RaftNetworkTarget for RaftNode {
    async fn append_entries(
        &self,
        req: AppendEntriesRequest<TypeConfig>,
    ) -> Result<AppendEntriesResponse<NodeId>, RaftError<NodeId>> {
        self.raft.append_entries(req).await
    }

    async fn install_snapshot(
        &self,
        req: InstallSnapshotRequest<TypeConfig>,
    ) -> Result<InstallSnapshotResponse<NodeId>, RaftError<NodeId, InstallSnapshotError>> {
        self.raft.install_snapshot(req).await
    }

    async fn vote(
        &self,
        req: VoteRequest<NodeId>,
    ) -> Result<VoteResponse<NodeId>, RaftError<NodeId>> {
        self.raft.vote(req).await
    }
}

impl RaftNode {
    /// ### 修改记录 (2026-02-26)
    /// - 原因: 启动完整的 Raft 节点
    /// - 目的: 初始化存储、网络、状态机并启动 Raft 任务
    pub async fn start(node_id: NodeId, base_dir: PathBuf, router: RaftRouter) -> Result<Self> {
        // 1. 准备目录
        if !base_dir.exists() {
            std::fs::create_dir_all(&base_dir)?;
        }
        let raft_db_path = base_dir.join("raft_db");
        let sqlite_path = base_dir.join("node.db");

        // 2. 初始化存储 (Sled)
        let store = RaftStore::open(&raft_db_path)?;

        // 3. 初始化状态机 (SQLite)
        let state_machine = SqliteStateMachine::new(sqlite_path.to_string_lossy().to_string())?;

        // 4. 初始化配置
        let config = Config {
            heartbeat_interval: 100,
            election_timeout_min: 200,
            election_timeout_max: 300,
            ..Default::default()
        };
        let config = Arc::new(config.validate().unwrap());

        // 5. 初始化网络
        let network = RaftNetworkFactoryImpl::new(router.clone());

        // 6. 启动 Raft
        let raft = Raft::new(
            node_id,
            config,
            network,
            store.clone(),
            state_machine.clone(),
        )
        .await?;

        Ok(Self {
            node_id,
            base_dir,
            raft,
            state_machine,
        })
    }

    pub async fn start_local(node_id: NodeId, base_dir: PathBuf) -> Result<Self> {
        let router = RaftRouter::new();
        let node = Self::start(node_id, base_dir, router.clone()).await?;
        router.register(node_id, Arc::new(node.clone()));
        Ok(node)
    }

    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要提供测试专用启动入口
    /// - 目的: 复用最小启动逻辑
    pub async fn start_for_test(node_id: NodeId, base_dir: PathBuf) -> Result<Self> {
        Self::start_local(node_id, base_dir).await
    }

    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    pub fn base_dir(&self) -> &PathBuf {
        &self.base_dir
    }

    // 保留旧的测试辅助方法，但转发给 Raft 或 StateMachine
    pub async fn apply_sql(&self, sql: String) -> Result<usize> {
        // 在真实 Raft 中，应该通过 raft.client_write 写入
        // 这里为了兼容旧测试，或者提供本地写入入口
        use crate::raft::types::Request;

        let req = Request::Write { sql };
        let _resp = self.raft.client_write(req).await?;
        // 解析 Response
        // 目前 Response 只是 Option<String>
        Ok(1) // 简化返回
    }

    /// ### 修改记录 (2026-02-28)
    /// - 原因: 提供 Smart Batcher 批量写入入口
    /// - 目的: 将多条 SQL 打包为单一 Raft 日志提交
    pub async fn apply_sql_batch(&self, sqls: Vec<String>) -> Result<usize> {
        use crate::raft::types::Request;

        if sqls.is_empty() {
            return Ok(0);
        }

        let count = sqls.len();
        let req = Request::WriteBatch { sqls };
        let _resp = self.raft.client_write(req).await?;
        // 批次提交成功，返回批次大小
        Ok(count)
    }

    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要测试用写入入口
    /// - 目的: 与测试用例保持命名一致
    pub async fn apply_sql_for_test(&self, sql: String) -> Result<usize> {
        self.apply_sql(sql).await
    }

    pub async fn query_scalar(&self, sql: String) -> Result<String> {
        // 本地查询可以直接查 StateMachine (如果是 Read Index 则需要走 Raft)
        // 这里简化为直接查 StateMachine
        self.state_machine.query_scalar(sql).await
    }

    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要测试用查询入口
    /// - 目的: 与测试用例保持命名一致
    pub async fn query_scalar_for_test(&self, sql: String) -> Result<String> {
        self.query_scalar(sql).await
    }

    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要支持节点关闭
    /// - 目的: 释放资源并支持测试中的重启场景
    pub async fn shutdown(&self) -> Result<()> {
        self.raft.shutdown().await?;
        Ok(())
    }
}

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要最小集群骨架
/// - 目的: 驱动 failover 测试与后续 Raft 集群落地
pub struct TestCluster {
    pub router: RaftRouter,
    pub nodes: Vec<RaftNode>,
}

impl TestCluster {
    pub async fn new(node_count: usize) -> Result<Self> {
        if node_count == 0 {
            return Err(anyhow!("node_count must be > 0"));
        }
        let cluster_id = Uuid::new_v4().to_string();
        let mut nodes = Vec::with_capacity(node_count);
        let router = RaftRouter::new();

        for idx in 0..node_count {
            let base_dir =
                std::env::temp_dir().join(format!("raft_cluster_{}_node_{}", cluster_id, idx));
            let node_id = (idx + 1) as u64;
            let node = RaftNode::start(node_id, base_dir, router.clone()).await?;
            // ### 修改记录 (2026-02-26)
            // - 原因: 节点启动后需要注册到路由表
            // - 目的: 让网络层能找到目标节点
            router.register(node_id, Arc::new(node.clone()));
            nodes.push(node);
        }

        // 初始化集群 (OpenRaft 需要显式初始化)
        // 让第一个节点作为 Leader 初始化
        let leader = &nodes[0];
        let mut members = std::collections::BTreeSet::new();
        for node in &nodes {
            members.insert(node.node_id());
        }

        // 只有在集群第一次启动时需要 initialize
        // 这里假设是全新的集群
        leader.raft.initialize(members).await?;

        Ok(Self { router, nodes })
    }

    pub fn get_node(&self, node_id: NodeId) -> Option<&RaftNode> {
        self.nodes.iter().find(|n| n.node_id == node_id)
    }

    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要等待 leader 选举
    /// - 目的: 支撑写入与故障切换
    async fn wait_for_leader(&self, exclude: Option<NodeId>) -> Result<NodeId> {
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要设置超时轮询
        // - 目的: 避免无限等待
        // ### 修改记录 (2026-02-28)
        // - 原因: 增加轮询次数
        // - 目的: 适应慢速环境或多次 failover 后的状态收敛
        for _ in 0..200 {
            for node in &self.nodes {
                if exclude == Some(node.node_id()) {
                    continue;
                }
                // ### 修改记录 (2026-02-28)
                // - 原因: 节点可能已经被关闭，导致 metrics 获取失败或 channel 关闭
                // - 目的: 避免在节点关闭后 panic (如果 watch channel 关闭)
                // 增加更多的重试和状态检查
                let metrics_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                     node.raft.metrics().borrow().clone()
                }));

                if let Ok(metrics) = metrics_result {
                     if metrics.state == ServerState::Leader {
                        // 额外检查: 确保不是在 Candidate 状态下被误判
                        if metrics.current_leader.is_some() && metrics.current_leader.unwrap() == node.node_id() {
                            return Ok(node.node_id());
                        }
                    }
                }
            }
            // ### 修改记录 (2026-02-27)
            // - 原因: 需要等待状态收敛
            // - 目的: 降低忙等压力
            sleep(Duration::from_millis(100)).await;
        }
        
        // 尝试最后一次全面扫描，输出调试信息
        for node in &self.nodes {
            if exclude == Some(node.node_id()) { continue; }
            if let Ok(metrics) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| node.raft.metrics().borrow().clone())) {
                println!("Node {} state: {:?}, current_leader: {:?}", node.node_id(), metrics.state, metrics.current_leader);
                
                // 尝试手动触发选举 (如果是 Candidate)
                if metrics.state == ServerState::Candidate {
                    let _ = node.raft.trigger().elect().await;
                }
            }
        }
        
        // 再试一次
         sleep(Duration::from_millis(1000)).await;
         for node in &self.nodes {
                if exclude == Some(node.node_id()) {
                    continue;
                }
                let metrics_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                     node.raft.metrics().borrow().clone()
                }));

                if let Ok(metrics) = metrics_result {
                     if metrics.state == ServerState::Leader {
                        if metrics.current_leader.is_some() && metrics.current_leader.unwrap() == node.node_id() {
                            return Ok(node.node_id());
                        }
                    }
                }
            }

        Err(anyhow!("Leader not found"))
    }

    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要集群写入入口
    /// - 目的: 统一走 leader 写入路径
    pub async fn write(&self, sql: String) -> Result<()> {
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要确定 leader
        // - 目的: 保证写入走主节点
        let leader_id = self.wait_for_leader(None).await?;
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要定位 leader 实例
        // - 目的: 执行写入
        let leader = self
            .get_node(leader_id)
            .ok_or_else(|| anyhow!("Leader not found"))?;
        leader.apply_sql(sql).await?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要 leader 查询入口
    /// - 目的: 读取一致性数据
    pub async fn query_leader_scalar(&self, sql: String) -> Result<String> {
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要确定 leader
        // - 目的: 保证查询走主节点
        let leader_id = self.wait_for_leader(None).await?;
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要定位 leader 实例
        // - 目的: 执行查询
        let leader = self
            .get_node(leader_id)
            .ok_or_else(|| anyhow!("Leader not found"))?;
        leader.query_scalar(sql).await
    }

    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要支持模拟节点故障
    /// - 目的: 测试故障场景下的集群行为
    pub async fn stop_node(&self, node_id: NodeId) -> Result<()> {
        if let Some(node) = self.nodes.iter().find(|n| n.node_id == node_id) {
            node.shutdown().await?;
        }
        Ok(())
    }

    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要模拟 leader 故障
    /// - 目的: 驱动 failover 场景
    pub async fn fail_leader(&mut self) -> Result<()> {
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要当前 leader
        // - 目的: 定位待故障节点
        let leader_id = self.wait_for_leader(None).await?;
        self.stop_node(leader_id).await?;
        
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要等待新 leader
        // - 目的: 确保 failover 完成
        let _ = self.wait_for_leader(Some(leader_id)).await?;
        
        // ### 修改记录 (2026-02-28)
        // - 原因: 需要恢复故障节点
        // - 目的: 避免节点耗尽，模拟真实故障恢复
        self.start_node(leader_id).await?;
        
        Ok(())
    }

    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要支持节点重启
    /// - 目的: 测试持久化恢复与重新加入集群
    pub async fn start_node(&mut self, node_id: NodeId) -> Result<()> {
        let (idx, base_dir) = {
            let (idx, node) = self
                .nodes
                .iter()
                .enumerate()
                .find(|(_, n)| n.node_id == node_id)
                .ok_or_else(|| anyhow!("Node not found"))?;
            (idx, node.base_dir.clone())
        };

        // 重新启动节点
        let new_node = RaftNode::start(node_id, base_dir, self.router.clone()).await?;

        // 更新路由表
        self.router.register(node_id, Arc::new(new_node.clone()));

        // 更新节点列表
        self.nodes[idx] = new_node;
        Ok(())
    }

    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要模拟 leader 重启
    /// - 目的: 覆盖恢复路径
    pub async fn restart_leader(&mut self) -> Result<()> {
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要当前 leader
        // - 目的: 定位待重启节点
        let leader_id = self.wait_for_leader(None).await?;
        self.stop_node(leader_id).await?;
        self.start_node(leader_id).await?;
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要等待选举恢复
        // - 目的: 确保集群稳定
        let _ = self.wait_for_leader(None).await?;
        Ok(())
    }
}
