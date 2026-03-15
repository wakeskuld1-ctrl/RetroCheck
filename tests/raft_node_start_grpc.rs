//! ### 修改记录 (2026-03-15)
//! - 原因: 需要补齐 RaftNode gRPC 启动路径的最小化集成测试
//! - 目的: 验证 start_grpc 可完成基础选举与日志复制链路

use anyhow::{Result, anyhow};
use check_program::pb::raft_service_server::RaftServiceServer;
use check_program::raft::grpc_service::RaftServiceImpl;
use check_program::raft::raft_node::RaftNode;
use openraft::BasicNode;
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::time::Duration;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::time::sleep;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要独立启动 gRPC 服务端
/// - 目的: 为 gRPC 网络提供 Raft RPC 入口
async fn spawn_raft_grpc_server(node: &RaftNode) -> Result<SocketAddr> {
    // ### 修改记录 (2026-03-15)
    // - 原因: 需要随机端口避免端口冲突
    // - 目的: 允许并行测试稳定运行
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要将 RaftNode 暴露为 gRPC 服务
    // - 目的: 使 RaftGrpcNetworkFactory 可访问目标节点
    let service = RaftServiceImpl::new(node.clone());

    // ### 修改记录 (2026-03-15)
    // - 原因: gRPC 服务需要在后台运行
    // - 目的: 避免阻塞测试主流程
    tokio::spawn(async move {
        let server = Server::builder().add_service(RaftServiceServer::new(service));
        server
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .expect("raft grpc server failed");
    });

    Ok(addr)
}

/// ### 修改记录 (2026-03-15)
/// - 原因: 初始化后需要等待 leader 选举完成
/// - 目的: 避免后续操作遇到 NOT_LEADER
async fn wait_for_leader(node: &RaftNode, retries: usize, delay: Duration) -> Result<()> {
    for _ in 0..retries {
        let metrics = node.raft.metrics().borrow().clone();
        if metrics.state == openraft::ServerState::Leader
            && matches!(metrics.current_leader, Some(id) if id == node.node_id())
        {
            return Ok(());
        }
        sleep(delay).await;
    }
    Err(anyhow!("leader not elected"))
}

/// ### 修改记录 (2026-03-15)
/// - 原因: follower 应用日志存在异步延迟
/// - 目的: 轮询直到读到预期值
async fn wait_for_scalar(
    node: &RaftNode,
    sql: &str,
    expected: &str,
    retries: usize,
    delay: Duration,
) -> Result<String> {
    for _ in 0..retries {
        match node.query_scalar(sql.to_string()).await {
            Ok(value) if value.trim() == expected => return Ok(value),
            _ => sleep(delay).await,
        }
    }
    Err(anyhow!("follower did not reach expected value"))
}

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要覆盖 start_grpc 最小成功路径
/// - 目的: 验证 gRPC 选举与复制链路可用
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_grpc_can_bootstrap_and_replicate_minimally() {
    // ### 修改记录 (2026-03-15)
    // - 原因: 需要临时目录隔离节点数据
    // - 目的: 避免测试间互相污染
    let dir1 = TempDir::new().expect("tempdir 1");
    let dir2 = TempDir::new().expect("tempdir 2");

    // ### 修改记录 (2026-03-15)
    // - 原因: 使用 gRPC 网络启动两个节点
    // - 目的: 覆盖 start_grpc 初始化路径
    let node1 = RaftNode::start_grpc(1, dir1.path().to_path_buf())
        .await
        .expect("start_grpc node1");
    let node2 = RaftNode::start_grpc(2, dir2.path().to_path_buf())
        .await
        .expect("start_grpc node2");

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要启动 gRPC 服务端
    // - 目的: 提供 Raft RPC 通信入口
    let _addr1 = spawn_raft_grpc_server(&node1).await.unwrap();
    let addr2 = spawn_raft_grpc_server(&node2).await.unwrap();

    // ### 修改记录 (2026-03-15)
    // - 原因: 初始化集群成员
    // - 目的: 确保 node1 成为 leader
    let mut members = BTreeSet::new();
    members.insert(1);
    node1.raft.initialize(members).await.expect("init");
    wait_for_leader(&node1, 200, Duration::from_millis(50))
        .await
        .expect("leader ready");

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要将 node2 加入集群
    // - 目的: 建立复制链路
    node1
        .raft
        .add_learner(2, BasicNode::new(format!("http://{}", addr2)), true)
        .await
        .expect("add_learner");

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要将 learner 提升为 voter
    // - 目的: 覆盖完整成员变更路径
    node1
        .raft
        .change_membership(BTreeSet::from([1, 2]), true)
        .await
        .expect("promote");

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要写入 SQL 触发日志复制
    // - 目的: 验证 follower 可见数据
    node1
        .apply_sql("CREATE TABLE IF NOT EXISTS t(v INTEGER);".to_string())
        .await
        .expect("create table");
    node1
        .apply_sql("INSERT INTO t(v) VALUES (7);".to_string())
        .await
        .expect("insert");

    // ### 修改记录 (2026-03-15)
    // - 原因: follower 应用有延迟
    // - 目的: 等待复制完成
    let value = wait_for_scalar(
        &node2,
        "SELECT COUNT(*) FROM t;",
        "1",
        50,
        Duration::from_millis(100),
    )
    .await
    .expect("wait follower");

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要断言复制结果
    // - 目的: 确认 gRPC 复制链路有效
    assert_eq!(value.trim(), "1");
}
