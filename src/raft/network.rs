//! ### 修改记录 (2026-02-17)
//! - 原因: 需要最小网络适配用于测试
//! - 目的: 先验证复制路径，再逐步替换为 OpenRaft 网络层

use crate::raft::types::{NodeId, TypeConfig};
use anyhow::Result;
use async_trait::async_trait;
use openraft::error::{Fatal, InstallSnapshotError, RPCError, RaftError, RemoteError};
use openraft::network::RPCOption;
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use openraft::{BasicNode, RaftNetwork, RaftNetworkFactory};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// ### 修改记录 (2026-02-26)
/// - 原因: 解耦 Raft 实例与网络层
/// - 目的: 避免循环依赖，支持多种实现
#[async_trait]
pub trait RaftNetworkTarget: Send + Sync {
    async fn append_entries(
        &self,
        req: AppendEntriesRequest<TypeConfig>,
    ) -> Result<AppendEntriesResponse<NodeId>, RaftError<NodeId>>;
    async fn install_snapshot(
        &self,
        req: InstallSnapshotRequest<TypeConfig>,
    ) -> Result<InstallSnapshotResponse<NodeId>, RaftError<NodeId, InstallSnapshotError>>;
    async fn vote(
        &self,
        req: VoteRequest<NodeId>,
    ) -> Result<VoteResponse<NodeId>, RaftError<NodeId>>;
}



/// ### 修改记录 (2026-02-26)
/// - 原因: 需要一个内存中的路由器
/// - 目的: 模拟网络传输，查找目标节点
#[derive(Clone)]
pub struct RaftRouter {
    targets: Arc<Mutex<HashMap<NodeId, Arc<dyn RaftNetworkTarget>>>>,
}

impl RaftRouter {
    pub fn new() -> Self {
        Self {
            targets: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn register(&self, node_id: NodeId, target: Arc<dyn RaftNetworkTarget>) {
        let mut targets = self.targets.lock().unwrap();
        targets.insert(node_id, target);
    }

    pub async fn send_append_entries(
        &self,
        target: NodeId,
        req: AppendEntriesRequest<TypeConfig>,
    ) -> Result<AppendEntriesResponse<NodeId>, RPCError<NodeId, <TypeConfig as openraft::RaftTypeConfig>::Node, RaftError<NodeId>>> {
        let target_node = {
            let targets = self.targets.lock().unwrap();
            targets.get(&target).cloned()
        };

        if let Some(t) = target_node {
            t.append_entries(req).await.map_err(|e| RPCError::RemoteError(RemoteError::new(target, e)))
        } else {
             Err(RPCError::RemoteError(RemoteError::new(target, RaftError::Fatal(Fatal::Panicked))))
        }
    }

    pub async fn send_install_snapshot(
        &self,
        target: NodeId,
        req: InstallSnapshotRequest<TypeConfig>,
    ) -> Result<
        InstallSnapshotResponse<NodeId>,
        RPCError<NodeId, <TypeConfig as openraft::RaftTypeConfig>::Node, RaftError<NodeId, InstallSnapshotError>>,
    > {
        let target_node = {
            let targets = self.targets.lock().unwrap();
            targets.get(&target).cloned()
        };

        if let Some(t) = target_node {
            t.install_snapshot(req).await.map_err(|e| RPCError::RemoteError(RemoteError::new(target, e)))
        } else {
            Err(RPCError::RemoteError(RemoteError::new(target, RaftError::Fatal(Fatal::Panicked))))
        }
    }

    pub async fn send_vote(
        &self,
        target: NodeId,
        req: VoteRequest<NodeId>,
    ) -> Result<VoteResponse<NodeId>, RPCError<NodeId, <TypeConfig as openraft::RaftTypeConfig>::Node, RaftError<NodeId>>> {
        let target_node = {
            let targets = self.targets.lock().unwrap();
            targets.get(&target).cloned()
        };

        if let Some(t) = target_node {
            t.vote(req).await.map_err(|e| RPCError::RemoteError(RemoteError::new(target, e)))
        } else {
            Err(RPCError::RemoteError(RemoteError::new(target, RaftError::Fatal(Fatal::Panicked))))
        }
    }
}

impl Default for RaftRouter {
    fn default() -> Self {
        Self::new()
    }
}

pub struct NetworkConnection {
    target: NodeId,
    router: RaftRouter,
}

//
impl RaftNetwork<TypeConfig> for NetworkConnection {
    async fn append_entries(
        &mut self,
        req: AppendEntriesRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<AppendEntriesResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        self.router.send_append_entries(self.target, req).await
    }

    async fn install_snapshot(
        &mut self,
        req: InstallSnapshotRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<
        InstallSnapshotResponse<NodeId>,
        RPCError<NodeId, BasicNode, RaftError<NodeId, InstallSnapshotError>>,
    > {
        self.router.send_install_snapshot(self.target, req).await
    }

    async fn vote(
        &mut self,
        req: VoteRequest<NodeId>,
        _option: RPCOption,
    ) -> Result<VoteResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        self.router.send_vote(self.target, req).await
    }
}

#[derive(Clone)]
pub struct RaftNetworkFactoryImpl {
    router: RaftRouter,
}

impl RaftNetworkFactoryImpl {
    pub fn new(router: RaftRouter) -> Self {
        Self { router }
    }
}

impl RaftNetworkFactory<TypeConfig> for RaftNetworkFactoryImpl {
    type Network = NetworkConnection;

    async fn new_client(&mut self, target: NodeId, _node: &BasicNode) -> Self::Network {
        NetworkConnection {
            target,
            router: self.router.clone(),
        }
    }
}

