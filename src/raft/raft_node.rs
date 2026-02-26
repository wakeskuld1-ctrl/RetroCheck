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
use openraft::{Config, Raft};
use std::path::PathBuf;
use std::sync::Arc;
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
    pub async fn start(
        node_id: NodeId,
        base_dir: PathBuf,
        router: RaftRouter,
    ) -> Result<Self> {
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
        let raft = Raft::new(node_id, config, network, store.clone(), state_machine.clone()).await?;

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

    pub async fn query_scalar(&self, sql: String) -> Result<String> {
        // 本地查询可以直接查 StateMachine (如果是 Read Index 则需要走 Raft)
        // 这里简化为直接查 StateMachine
        self.state_machine.query_scalar(sql).await
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

        Ok(Self {
            router,
            nodes,
        })
    }

    pub fn get_node(&self, node_id: NodeId) -> Option<&RaftNode> {
        self.nodes.iter().find(|n| n.node_id == node_id)
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

    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要支持节点重启
    /// - 目的: 测试持久化恢复与重新加入集群
    pub async fn start_node(&mut self, node_id: NodeId) -> Result<()> {
        let (idx, base_dir) = {
             let (idx, node) = self.nodes.iter().enumerate()
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
}
