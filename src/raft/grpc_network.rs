//! ### 修改记录 (2026-03-15)
//! - 原因: 需要将 Raft 网络层从内存模拟迁移为 gRPC
//! - 目的: 为 OpenRaft 提供可用的 gRPC 网络客户端实现

use crate::pb::raft_service_client::RaftServiceClient;
use crate::pb::RaftPayload;
use crate::raft::grpc_codec::{decode_payload, encode_payload};
use crate::raft::types::{NodeId, TypeConfig};
use openraft::error::{InstallSnapshotError, NetworkError, RPCError, RaftError, RemoteError};
use openraft::network::RPCOption;
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use openraft::{BasicNode, RaftNetwork, RaftNetworkFactory};
use std::time::Duration;
use tokio::time::sleep;
use tonic::transport::{Channel, Endpoint};
use tonic::Request;

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要提供 gRPC 网络工厂
/// - 目的: 为 Raft 节点创建 gRPC 连接实例
#[derive(Clone)]
pub struct RaftGrpcNetworkFactory;

impl RaftGrpcNetworkFactory {
    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要显式构造网络工厂
    /// - 目的: 统一初始化入口
    pub fn new() -> Self {
        Self
    }
}

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要与单一目标节点通信
/// - 目的: 实现 OpenRaft 的 RaftNetwork 接口
pub struct RaftGrpcNetwork {
    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要记录目标节点 ID
    /// - 目的: 构造 RemoteError 时携带 target
    target: NodeId,
    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要保留目标节点信息
    /// - 目的: 支持 RemoteError 的 target_node
    target_node: BasicNode,
    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要 gRPC 客户端
    /// - 目的: 执行 Vote/AppendEntries/InstallSnapshot RPC
    client: RaftServiceClient<Channel>,
}

impl RaftGrpcNetwork {
    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要将错误统一映射为 NetworkError
    /// - 目的: 避免不一致的错误处理路径
    fn to_network_error(message: String) -> NetworkError {
        let io_err = std::io::Error::new(std::io::ErrorKind::Other, message);
        NetworkError::new(&io_err)
    }
}

impl RaftNetwork<TypeConfig> for RaftGrpcNetwork {
    async fn append_entries(
        &mut self,
        req: AppendEntriesRequest<TypeConfig>,
        option: RPCOption,
    ) -> Result<AppendEntriesResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        // ### 修改记录 (2026-03-15)
        // - 原因: gRPC 载荷需要 bytes
        // - 目的: 使用统一 bincode 编码
        let payload = encode_payload(&req)
            .map_err(|e| RPCError::Network(Self::to_network_error(e.to_string())))?;

        // ### 修改记录 (2026-03-15)
        // - 原因: 需要设置 gRPC 请求超时
        // - 目的: 使用 RPCOption 的 hard_ttl 约束调用时间
        let mut request = Request::new(RaftPayload { payload });
        request.set_timeout(option.hard_ttl());

        // ### 修改记录 (2026-03-15)
        // - 原因: 需要执行 gRPC 调用
        // - 目的: 转发到 AppendEntries RPC
        let response = self
            .client
            .append_entries(request)
            .await
            .map_err(|e| RPCError::Network(Self::to_network_error(e.to_string())))?;

        // ### 修改记录 (2026-03-15)
        // - 原因: 响应体也为 bytes
        // - 目的: 反序列化为 OpenRaft 结构
        let result: Result<AppendEntriesResponse<NodeId>, RaftError<NodeId>> =
            decode_payload(&response.into_inner().payload)
                .map_err(|e| RPCError::Network(Self::to_network_error(e.to_string())))?;

        match result {
            Ok(resp) => Ok(resp),
            Err(err) => Err(RPCError::RemoteError(RemoteError::new_with_node(
                self.target,
                self.target_node.clone(),
                err,
            ))),
        }
    }

    async fn install_snapshot(
        &mut self,
        req: InstallSnapshotRequest<TypeConfig>,
        option: RPCOption,
    ) -> Result<
        InstallSnapshotResponse<NodeId>,
        RPCError<NodeId, BasicNode, RaftError<NodeId, InstallSnapshotError>>,
    > {
        // ### 修改记录 (2026-03-15)
        // - 原因: gRPC 载荷需要 bytes
        // - 目的: 使用统一 bincode 编码
        let payload = encode_payload(&req)
            .map_err(|e| RPCError::Network(Self::to_network_error(e.to_string())))?;

        // ### 修改记录 (2026-03-15)
        // - 原因: 需要设置 gRPC 请求超时
        // - 目的: 使用 RPCOption 的 hard_ttl 约束调用时间
        let mut request = Request::new(RaftPayload { payload });
        request.set_timeout(option.hard_ttl());

        // ### 修改记录 (2026-03-15)
        // - 原因: 需要执行 gRPC 调用
        // - 目的: 转发到 InstallSnapshot RPC
        let response = self
            .client
            .install_snapshot(request)
            .await
            .map_err(|e| RPCError::Network(Self::to_network_error(e.to_string())))?;

        // ### 修改记录 (2026-03-15)
        // - 原因: 响应体也为 bytes
        // - 目的: 反序列化为 OpenRaft 结构
        let result: Result<InstallSnapshotResponse<NodeId>, RaftError<NodeId, InstallSnapshotError>> =
            decode_payload(&response.into_inner().payload)
                .map_err(|e| RPCError::Network(Self::to_network_error(e.to_string())))?;

        match result {
            Ok(resp) => Ok(resp),
            Err(err) => Err(RPCError::RemoteError(RemoteError::new_with_node(
                self.target,
                self.target_node.clone(),
                err,
            ))),
        }
    }

    async fn vote(
        &mut self,
        req: VoteRequest<NodeId>,
        option: RPCOption,
    ) -> Result<VoteResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        // ### 修改记录 (2026-03-15)
        // - 原因: gRPC 载荷需要 bytes
        // - 目的: 使用统一 bincode 编码
        let payload = encode_payload(&req)
            .map_err(|e| RPCError::Network(Self::to_network_error(e.to_string())))?;

        // ### 修改记录 (2026-03-15)
        // - 原因: 需要设置 gRPC 请求超时
        // - 目的: 使用 RPCOption 的 hard_ttl 约束调用时间
        let mut request = Request::new(RaftPayload { payload });
        request.set_timeout(option.hard_ttl());

        // ### 修改记录 (2026-03-15)
        // - 原因: 需要执行 gRPC 调用
        // - 目的: 转发到 Vote RPC
        let response = self
            .client
            .vote(request)
            .await
            .map_err(|e| RPCError::Network(Self::to_network_error(e.to_string())))?;

        // ### 修改记录 (2026-03-15)
        // - 原因: 响应体也为 bytes
        // - 目的: 反序列化为 OpenRaft 结构
        let result: Result<VoteResponse<NodeId>, RaftError<NodeId>> =
            decode_payload(&response.into_inner().payload)
                .map_err(|e| RPCError::Network(Self::to_network_error(e.to_string())))?;

        match result {
            Ok(resp) => Ok(resp),
            Err(err) => Err(RPCError::RemoteError(RemoteError::new_with_node(
                self.target,
                self.target_node.clone(),
                err,
            ))),
        }
    }
}

impl RaftNetworkFactory<TypeConfig> for RaftGrpcNetworkFactory {
    type Network = RaftGrpcNetwork;

    async fn new_client(&mut self, target: NodeId, node: &BasicNode) -> Self::Network {
        // ### 修改记录 (2026-03-15)
        // - 原因: 需要规范化目标地址
        // - 目的: 确保 tonic 能正确解析 endpoint
        let endpoint = normalize_endpoint(&node.addr);

        // ### 修改记录 (2026-03-15)
        // - 原因: 连接可能短暂失败
        // - 目的: 提供一次重试以提高稳定性
        let client = match connect_with_retry(endpoint.clone(), 2, Duration::from_millis(50)).await
        {
            Ok(client) => client,
            Err(_err) => {
                // ### 修改记录 (2026-03-15)
                // - 原因: gRPC 连接失败仍需返回 Network 实例
                // - 目的: 让后续 RPC 在超时内返回 Err
                RaftServiceClient::new(build_lazy_channel(&endpoint))
            }
        };

        RaftGrpcNetwork {
            target,
            target_node: node.clone(),
            client,
        }
    }
}

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要统一处理地址补全
/// - 目的: 支持传入 "host:port" 或完整 URL
fn normalize_endpoint(addr: &str) -> String {
    if addr.starts_with("http://") || addr.starts_with("https://") {
        addr.to_string()
    } else {
        format!("http://{}", addr)
    }
}

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要连接重试
/// - 目的: 避免瞬时连接失败导致创建 Network 失败
async fn connect_with_retry(
    endpoint: String,
    attempts: usize,
    delay: Duration,
) -> Result<RaftServiceClient<Channel>, tonic::transport::Error> {
    // ### 修改记录 (2026-03-15)
    // - 原因: 避免 0 次尝试导致无错误返回
    // - 目的: 保证 last_err 有值
    let attempts = attempts.max(1);
    let mut last_err: Option<tonic::transport::Error> = None;
    for _ in 0..attempts {
        match RaftServiceClient::connect(endpoint.clone()).await {
            Ok(client) => return Ok(client),
            Err(err) => {
                last_err = Some(err);
                sleep(delay).await;
            }
        }
    }

    Err(last_err.expect("grpc connect retry exhausted"))
}

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要在连接失败时提供惰性通道
/// - 目的: 保证 RaftNetworkFactory 不因连接失败而 panic
fn build_lazy_channel(endpoint: &str) -> Channel {
    Endpoint::from_shared(endpoint.to_string())
        .unwrap_or_else(|_| Endpoint::from_static("http://127.0.0.1:0"))
        .connect_lazy()
}
