//! ### 修改记录 (2026-02-17)
//! - 原因: 需要提供 RaftNode 最小封装
//! - 目的: 为 Raft 核心落地提供可测试入口

use crate::raft::state_machine::SqliteStateMachine;
use anyhow::{Result, anyhow};
use std::path::PathBuf;
use uuid::Uuid;

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要在测试网络中共享节点实例
/// - 目的: 允许 TestNetwork 保存节点副本
#[derive(Clone)]
pub struct RaftNode {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要唯一节点标识
    /// - 目的: 与 Raft Core 的 NodeId 对齐
    node_id: u64,
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要保存存储目录
    /// - 目的: 让 Raft 日志与快照有固定落盘位置
    base_dir: PathBuf,
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要执行 SQL apply
    /// - 目的: 提供最小的状态机落地能力
    state_machine: SqliteStateMachine,
}

impl RaftNode {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要测试环境最小启动入口
    /// - 目的: 驱动 RaftNode 的基本骨架实现
    pub async fn start_for_test(node_id: u64, base_dir: PathBuf) -> Result<Self> {
        // ### 修改记录 (2026-02-17)
        // - 原因: 测试环境需要确保目录存在
        // - 目的: 避免后续落盘失败
        std::fs::create_dir_all(&base_dir)?;

        // ### 修改记录 (2026-02-17)
        // - 原因: 需要为测试生成独立 SQLite 文件
        // - 目的: 让 apply_sql_for_test 有实际落地目标
        let db_path = base_dir.join("node.db");
        let state_machine = SqliteStateMachine::new(db_path.to_string_lossy().to_string())?;

        // ### 修改记录 (2026-02-17)
        // - 原因: OpenRaft 逻辑尚未接入
        // - 目的: 先返回最小结构以通过测试驱动
        Ok(Self {
            node_id,
            base_dir,
            state_machine,
        })
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要本地服务启动入口
    /// - 目的: 让 Router 能使用明确的 db 路径
    pub async fn start_local(node_id: u64, db_path: String) -> Result<Self> {
        // ### 修改记录 (2026-02-17)
        // - 原因: 需要推导存储目录
        // - 目的: 为日志与快照提供落盘位置
        let path = PathBuf::from(&db_path);
        let base_dir = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::env::temp_dir().join("raft_node_local"));
        std::fs::create_dir_all(&base_dir)?;
        let state_machine = SqliteStateMachine::new(db_path)?;
        Ok(Self {
            node_id,
            base_dir,
            state_machine,
        })
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要暴露节点标识用于断言
    /// - 目的: 便于测试与日志输出
    pub fn node_id(&self) -> u64 {
        self.node_id
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要暴露目录用于调试
    /// - 目的: 支撑后续 RaftStore 初始化
    pub fn base_dir(&self) -> &PathBuf {
        &self.base_dir
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要测试专用写入入口
    /// - 目的: 驱动状态机 apply 的最小路径
    pub async fn apply_sql_for_test(&self, sql: String) -> Result<usize> {
        // ### 修改记录 (2026-02-17)
        // - 原因: 需要复用统一写入口
        // - 目的: 避免测试与生产路径分叉
        self.apply_sql(sql).await
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要测试查询入口
    /// - 目的: 验证写入是否真实落盘
    pub async fn query_scalar_for_test(&self, sql: String) -> Result<String> {
        // ### 修改记录 (2026-02-17)
        // - 原因: 需要复用统一查询入口
        // - 目的: 避免测试与生产路径分叉
        self.query_scalar(sql).await
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要统一写入口
    /// - 目的: 让 Router 与测试共享写入路径
    pub async fn apply_sql(&self, sql: String) -> Result<usize> {
        // ### 修改记录 (2026-02-17)
        // - 原因: 需要复用状态机写入逻辑
        // - 目的: 保持写入实现集中
        self.state_machine.apply_write(sql).await
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要统一查询入口
    /// - 目的: 让 Router 与测试共享查询路径
    pub async fn query_scalar(&self, sql: String) -> Result<String> {
        // ### 修改记录 (2026-02-17)
        // - 原因: 需要复用状态机查询逻辑
        // - 目的: 保持查询实现集中
        self.state_machine.query_scalar(sql).await
    }
}

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要最小集群骨架
/// - 目的: 驱动 failover 测试与后续 Raft 集群落地
pub struct TestCluster {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要记录当前 leader 索引
    /// - 目的: 支撑 failover 切换逻辑
    leader_index: usize,
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要保存节点列表
    /// - 目的: 提供最小读写入口
    nodes: Vec<RaftNode>,
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要模拟复制日志
    /// - 目的: failover 后重放写入
    applied_sqls: Vec<String>,
    applied_offsets: Vec<usize>,
}

impl TestCluster {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要创建指定节点数量的集群
    /// - 目的: 为集群测试提供统一入口
    pub async fn new(node_count: usize) -> Result<Self> {
        if node_count == 0 {
            return Err(anyhow!("node_count must be > 0"));
        }
        let cluster_id = Uuid::new_v4().to_string();
        let mut nodes = Vec::with_capacity(node_count);
        for idx in 0..node_count {
            let base_dir =
                std::env::temp_dir().join(format!("raft_cluster_{}_node_{}", cluster_id, idx));
            let node = RaftNode::start_for_test((idx + 1) as u64, base_dir).await?;
            nodes.push(node);
        }
        Ok(Self {
            leader_index: 0,
            nodes,
            applied_sqls: Vec::new(),
            applied_offsets: vec![0; node_count],
        })
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要写入当前 leader
    /// - 目的: 为测试提供最小写路径
    pub async fn write(&mut self, sql: String) -> Result<usize> {
        let leader = self
            .nodes
            .get(self.leader_index)
            .ok_or_else(|| anyhow!("Leader not found"))?;
        // ### 修改记录 (2026-02-17)
        // - 原因: 需要保持写入记录
        // - 目的: 用于 failover 时重放
        let rows = leader.apply_sql(sql.clone()).await?;
        self.applied_sqls.push(sql);
        self.applied_offsets[self.leader_index] = self.applied_sqls.len();
        Ok(rows)
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要模拟 leader 下线
    /// - 目的: 驱动 failover 测试
    pub async fn fail_leader(&mut self) -> Result<()> {
        if self.nodes.is_empty() {
            return Err(anyhow!("No nodes in cluster"));
        }
        let new_leader_index = (self.leader_index + 1) % self.nodes.len();
        // ### 修改记录 (2026-02-17)
        // - 原因: 需要模拟复制
        // - 目的: 让新 leader 具备历史数据
        let new_leader = self
            .nodes
            .get(new_leader_index)
            .ok_or_else(|| anyhow!("Leader not found"))?;
        let start = self
            .applied_offsets
            .get(new_leader_index)
            .copied()
            .unwrap_or(0);
        for sql in self.applied_sqls.iter().skip(start) {
            let _ = new_leader.apply_sql(sql.clone()).await?;
        }
        self.applied_offsets[new_leader_index] = self.applied_sqls.len();
        self.leader_index = new_leader_index;
        Ok(())
    }

    pub async fn restart_leader(&mut self) -> Result<()> {
        let leader = self
            .nodes
            .get(self.leader_index)
            .ok_or_else(|| anyhow!("Leader not found"))?;
        let node_id = leader.node_id();
        let base_dir = leader.base_dir().clone();
        let new_node = RaftNode::start_for_test(node_id, base_dir).await?;
        self.nodes[self.leader_index] = new_node;
        Ok(())
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要查询 leader 状态
    /// - 目的: 验证写入结果
    pub async fn query_leader_scalar(&self, sql: String) -> Result<String> {
        let leader = self
            .nodes
            .get(self.leader_index)
            .ok_or_else(|| anyhow!("Leader not found"))?;
        leader.query_scalar(sql).await
    }
}
