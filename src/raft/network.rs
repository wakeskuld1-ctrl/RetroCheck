//! ### 修改记录 (2026-02-17)
//! - 原因: 需要最小网络适配用于测试
//! - 目的: 先验证复制路径，再逐步替换为 OpenRaft 网络层

use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::raft::raft_node::RaftNode;

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要测试环境的节点注册表
/// - 目的: 模拟网络传输时的节点寻址
#[derive(Clone)]
pub struct TestNetwork {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要共享节点映射
    /// - 目的: 让多测试线程访问同一份注册表
    nodes: Arc<Mutex<HashMap<u64, RaftNode>>>,
}

impl TestNetwork {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要初始化空网络
    /// - 目的: 为测试提供独立上下文
    pub fn new() -> Self {
        Self {
            nodes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要注册节点以便查找
    /// - 目的: 模拟网络中的节点发现
    pub fn register_node(&self, node: RaftNode) {
        // ### 修改记录 (2026-02-17)
        // - 原因: 需要写入共享映射
        // - 目的: 允许后续复制调用定位目标
        let mut guard = self.nodes.lock().unwrap();
        guard.insert(node.node_id(), node);
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要在测试中读取节点
    /// - 目的: 验证复制结果
    pub fn get_node_for_test(&self, node_id: u64) -> Option<RaftNode> {
        // ### 修改记录 (2026-02-17)
        // - 原因: 需要从共享表中获取节点
        // - 目的: 提供只读访问能力
        let guard = self.nodes.lock().unwrap();
        guard.get(&node_id).cloned()
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要最小复制路径
    /// - 目的: 驱动 follower 应用写入
    pub async fn replicate_sql_for_test(&self, target_id: u64, sql: String) -> Result<usize> {
        // ### 修改记录 (2026-02-17)
        // - 原因: 需要查询目标节点
        // - 目的: 作为网络寻址的最小实现
        let node = {
            let guard = self.nodes.lock().unwrap();
            guard.get(&target_id).cloned()
        };
        let node = node.ok_or_else(|| anyhow!("Target node not found"))?;

        // ### 修改记录 (2026-02-17)
        // - 原因: 需要执行写入应用
        // - 目的: 模拟复制后的日志应用
        node.apply_sql_for_test(sql).await
    }
}
