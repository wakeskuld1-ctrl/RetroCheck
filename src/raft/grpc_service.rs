//! ### 修改记录 (2026-03-15)
//! - 原因: 需要提供 Raft gRPC 服务端实现
//! - 目的: 将 OpenRaft RPC 映射为可注册的 gRPC 服务

use crate::pb::raft_service_server::RaftService;
use crate::pb::RaftPayload;
use crate::raft::grpc_codec::{decode_payload, encode_payload};
use crate::raft::raft_node::RaftNode;
use crate::raft::types::{NodeId, TypeConfig};
use openraft::error::{InstallSnapshotError, RaftError};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use tonic::{Request, Response, Status};

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要一个可克隆的 gRPC 服务实例
/// - 目的: 支持 tonic 在多线程环境下复用服务
#[derive(Clone)]
pub struct RaftServiceImpl {
    /// ### 修改记录 (2026-03-15)
    /// - 原因: gRPC 层需要访问 Raft 核心
    /// - 目的: 直接调用 OpenRaft 的 RPC 接口
    node: RaftNode,
}

impl RaftServiceImpl {
    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要构造 gRPC 服务端实例
    /// - 目的: 注入 RaftNode 以处理请求
    pub fn new(node: RaftNode) -> Self {
        Self { node }
    }
}

#[tonic::async_trait]
impl RaftService for RaftServiceImpl {
    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要接入 AppendEntries gRPC
    /// - 目的: 让 Raft 日志复制走 gRPC 链路
    async fn append_entries(
        &self,
        request: Request<RaftPayload>,
    ) -> Result<Response<RaftPayload>, Status> {
        // ### 修改记录 (2026-03-15)
        // - 原因: gRPC 载荷为 bytes
        // - 目的: 反序列化为 OpenRaft 请求
        let req: AppendEntriesRequest<TypeConfig> = decode_payload(&request.into_inner().payload)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;

        // ### 修改记录 (2026-03-15)
        // - 原因: 需要调用 OpenRaft append_entries
        // - 目的: 执行日志复制与一致性处理
        let resp: Result<AppendEntriesResponse<NodeId>, RaftError<NodeId>> =
            self.node.raft.append_entries(req).await;

        // ### 修改记录 (2026-03-15)
        // - 原因: gRPC 响应需要 bytes
        // - 目的: 统一编码结果
        let payload = encode_payload(&resp).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(RaftPayload { payload }))
    }

    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要接入 InstallSnapshot gRPC
    /// - 目的: 让快照安装走 gRPC 链路
    async fn install_snapshot(
        &self,
        request: Request<RaftPayload>,
    ) -> Result<Response<RaftPayload>, Status> {
        // ### 修改记录 (2026-03-15)
        // - 原因: gRPC 载荷为 bytes
        // - 目的: 反序列化为 OpenRaft 请求
        let req: InstallSnapshotRequest<TypeConfig> =
            decode_payload(&request.into_inner().payload)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        // ### 修改记录 (2026-03-15)
        // - 原因: 需要调用 OpenRaft install_snapshot
        // - 目的: 处理快照安装与状态同步
        let resp: Result<InstallSnapshotResponse<NodeId>, RaftError<NodeId, InstallSnapshotError>> =
            self.node.raft.install_snapshot(req).await;

        // ### 修改记录 (2026-03-15)
        // - 原因: gRPC 响应需要 bytes
        // - 目的: 统一编码结果
        let payload = encode_payload(&resp).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(RaftPayload { payload }))
    }

    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要接入 Vote gRPC
    /// - 目的: 让投票 RPC 走 gRPC 链路
    async fn vote(
        &self,
        request: Request<RaftPayload>,
    ) -> Result<Response<RaftPayload>, Status> {
        // ### 修改记录 (2026-03-15)
        // - 原因: gRPC 载荷为 bytes
        // - 目的: 反序列化为 OpenRaft 请求
        let req: VoteRequest<NodeId> = decode_payload(&request.into_inner().payload)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;

        // ### 修改记录 (2026-03-15)
        // - 原因: 需要调用 OpenRaft vote
        // - 目的: 完成投票流程
        let resp: Result<VoteResponse<NodeId>, RaftError<NodeId>> = self.node.raft.vote(req).await;

        // ### 修改记录 (2026-03-15)
        // - 原因: gRPC 响应需要 bytes
        // - 目的: 统一编码结果
        let payload = encode_payload(&resp).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(RaftPayload { payload }))
    }
}
