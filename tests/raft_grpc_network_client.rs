//! ### 修改记录 (2026-03-15)
//! - 原因: 需要通过 TDD 驱动 Raft gRPC 网络客户端
//! - 目的: 验证网络层在超时约束下返回错误而非崩溃

use check_program::raft::grpc_network::RaftGrpcNetworkFactory;
use openraft::network::RPCOption;
use openraft::raft::VoteRequest;
/// ### 修改记录 (2026-03-15)
/// - 原因: 需要调用 RaftNetworkFactory trait 方法
/// - 目的: 在测试中使用 new_client
/// ### 修改记录 (2026-03-15)
/// - 原因: 需要调用 RaftNetwork trait 方法
/// - 目的: 使用 vote 接口触发 RPC
use openraft::{BasicNode, RaftNetwork, RaftNetworkFactory, Vote};
use std::time::Duration;

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要验证 gRPC 网络客户端可构造并处理错误
/// - 目的: 确认超时参数能限制调用时间
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grpc_network_client_can_call_vote_with_timeout() {
    // ### 修改记录 (2026-03-15)
    // - 原因: 需要创建 gRPC 网络工厂
    // - 目的: 生成与目标节点绑定的网络实例
    let mut factory = RaftGrpcNetworkFactory::new();

    // ### 修改记录 (2026-03-15)
    // - 原因: 使用一个不存在的地址模拟失败
    // - 目的: 验证返回 Err 而不是 panic
    let mut client = factory
        .new_client(2, &BasicNode::new("http://127.0.0.1:59999".to_string()))
        .await;

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要最小化的 Vote 请求
    // - 目的: 验证网络调用链路能返回错误
    let req = VoteRequest::new(Vote::new(1, 1), None);

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要显式超时限制
    // - 目的: 防止测试在网络异常时挂起
    let result = client
        .vote(req, RPCOption::new(Duration::from_millis(50)))
        .await;

    // ### 修改记录 (2026-03-15)
    // - 原因: 该地址不可达
    // - 目的: 确认返回错误
    assert!(result.is_err());
}
