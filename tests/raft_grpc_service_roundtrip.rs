//! ### 修改记录 (2026-03-15)
//! - 原因: 需要通过测试驱动新增 Raft gRPC 服务端
//! - 目的: 验证 Vote RPC 的编解码与路由闭环

use check_program::pb::raft_service_client::RaftServiceClient;
/// ### 修改记录 (2026-03-15)
/// - 原因: 需要直接调用 RaftService trait 方法
/// - 目的: 在测试中走非 gRPC 的直连路径排查问题
use check_program::pb::raft_service_server::RaftService;
use check_program::pb::RaftPayload;
use check_program::raft::grpc_codec::{decode_payload, encode_payload};
use check_program::raft::grpc_service::RaftServiceImpl;
use check_program::raft::raft_node::RaftNode;
use openraft::raft::VoteRequest;
use openraft::Vote;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, transport::{Channel, Server}};

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要验证 gRPC RaftService 的 vote 链路
/// - 目的: 确保 payload 编解码正确并能返回结果
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raft_service_vote_roundtrip() {
    // ### 修改记录 (2026-03-15)
    // - 原因: 需要避免测试目录冲突
    // - 目的: 使用唯一临时目录确保并发安全
    let base_dir = std::env::temp_dir()
        .join(format!("raft_grpc_vote_roundtrip_{}", uuid::Uuid::new_v4()));

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要一个可调用的 Raft 核心实例
    // - 目的: 作为 gRPC 服务端的实际处理对象
    let node = RaftNode::start_local(1, base_dir).await.unwrap();

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要绑定临时端口启动 gRPC
    // - 目的: 避免端口冲突，提升测试稳定性
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要把 RaftNode 注入到 gRPC 服务层
    // - 目的: 提供 Vote/AppendEntries/InstallSnapshot 入口
    let service = RaftServiceImpl::new(node);
    // ### 修改记录 (2026-03-15)
    // - 原因: 需要在不经过 gRPC 的情况下验证编解码
    // - 目的: 判断问题是否来自传输层
    let service_for_direct = service.clone();

    // ### 修改记录 (2026-03-15)
    // - 原因: 测试中需要后台启动 gRPC 服务
    // - 目的: 客户端可以连接并发起请求
    tokio::spawn(async move {
        let server = Server::builder()
            .add_service(check_program::pb::raft_service_server::RaftServiceServer::new(service));
        server
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要构造 gRPC 客户端
    // - 目的: 发起 Vote RPC 并验证响应
    let mut client = connect_with_retry(addr).await;

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要最小化的 Vote 请求体
    // - 目的: 验证服务端能正确解码并处理
    let req = VoteRequest::new(Vote::new(1, 2), None);

    // ### 修改记录 (2026-03-15)
    // - 原因: gRPC 传输载荷为 bytes
    // - 目的: 使用统一编解码逻辑构造 payload
    let payload = encode_payload(&req).unwrap();
    // ### 修改记录 (2026-03-15)
    // - 原因: 需要确认序列化结果非空
    // - 目的: 排除空载荷导致的解码失败
    assert!(!payload.is_empty());
    // ### 修改记录 (2026-03-15)
    // - 原因: 需要验证同进程解码可行
    // - 目的: 确认 bincode 编解码本身是可用的
    let local_roundtrip: VoteRequest<u64> = decode_payload(&payload).unwrap();
    assert_eq!(local_roundtrip.vote, req.vote);
    // ### 修改记录 (2026-03-15)
    // - 原因: 需要验证服务端方法本身不报错
    // - 目的: 将问题定位到传输层或服务层
    let direct_resp = service_for_direct
        .vote(Request::new(RaftPayload {
            payload: payload.clone(),
        }))
        .await
        .unwrap()
        .into_inner();
    let direct_decoded: Result<
        openraft::raft::VoteResponse<u64>,
        openraft::error::RaftError<u64>,
    > = decode_payload(&direct_resp.payload).unwrap();
    assert!(direct_decoded.is_ok());

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要通过 gRPC 调用 vote
    // - 目的: 验证服务端能返回可解码的结果
    let resp = client
        .vote(RaftPayload { payload })
        .await
        .unwrap()
        .into_inner();

    // ### 修改记录 (2026-03-15)
    // - 原因: 响应体同样被包装为 bytes
    // - 目的: 确认服务端返回 RaftError 或 VoteResponse
    let decoded: Result<
        openraft::raft::VoteResponse<u64>,
        openraft::error::RaftError<u64>,
    > = decode_payload(&resp.payload).unwrap();

    // ### 修改记录 (2026-03-15)
    // - 原因: 该测试只关心编解码与链路可用
    // - 目的: 允许返回 Ok 即可通过
    assert!(decoded.is_ok());
}

/// ### 修改记录 (2026-03-15)
/// - 原因: gRPC 服务启动存在竞态
/// - 目的: 用重试等待代替固定 sleep，提升稳定性
async fn connect_with_retry(addr: SocketAddr) -> RaftServiceClient<Channel> {
    // ### 修改记录 (2026-03-15)
    // - 原因: 需要可配置的重试目标地址
    // - 目的: 避免多处重复拼接字符串
    let endpoint = format!("http://{}", addr);
    // ### 修改记录 (2026-03-15)
    // - 原因: 需要有限次重试避免测试挂起
    // - 目的: 稳定等待服务端就绪
    let mut last_err: Option<tonic::transport::Error> = None;
    for _ in 0..20 {
        match RaftServiceClient::connect(endpoint.clone()).await {
            Ok(client) => return client,
            Err(err) => {
                last_err = Some(err);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }

    // ### 修改记录 (2026-03-15)
    // - 原因: 多次重试后仍失败
    // - 目的: 提供清晰的失败原因
    panic!(
        "RaftServiceClient connect failed after retries: {:?}",
        last_err
    );
}
