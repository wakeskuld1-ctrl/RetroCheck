//! ### 修改记录 (2026-03-15)
//! - 原因: 需要验证 gRPC 网络下的最小日志复制成功链路
//! - 目的: 确保 leader 写入后 follower 可读

use anyhow::{Result, anyhow};
use check_program::pb::RaftPayload;
use check_program::pb::raft_service_server::{RaftService, RaftServiceServer};
use check_program::raft::grpc_codec::{decode_payload, encode_payload};
use check_program::raft::grpc_network::RaftGrpcNetworkFactory;
use check_program::raft::state_machine::SqliteStateMachine;
use check_program::raft::store::RaftStore;
use check_program::raft::types::{NodeId, Request as RaftRequest, TypeConfig};
use openraft::error::{InstallSnapshotError, RaftError};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use openraft::{BasicNode, Config, Raft, ServerState};
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::sleep;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status, transport::Server};
use uuid::Uuid;

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要与生产代码对齐 Raft 类型别名
/// - 目的: 简化测试中的类型标注
type RaftCore = Raft<TypeConfig>;

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要测试内的最小 Raft 节点封装
/// - 目的: 复用 RaftCore 与状态机查询能力
#[derive(Clone)]
struct TestRaftNode {
    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要识别节点
    /// - 目的: 与 OpenRaft NodeId 对齐
    node_id: NodeId,
    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要 Raft 核心实例
    /// - 目的: 执行共识协议与日志复制
    raft: RaftCore,
    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要读取 follower 数据
    /// - 目的: 验证日志应用结果
    state_machine: SqliteStateMachine,
}

impl TestRaftNode {
    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要最小化初始化 Raft 节点
    /// - 目的: 在测试中复刻生产启动流程
    async fn start_grpc(node_id: NodeId, base_dir: PathBuf) -> Result<Self> {
        // ### 修改记录 (2026-03-15)
        // - 原因: 需要确保目录存在
        // - 目的: 避免存储初始化失败
        if !base_dir.exists() {
            std::fs::create_dir_all(&base_dir)?;
        }

        // ### 修改记录 (2026-03-15)
        // - 原因: 需要区分 Raft 与 SQLite 的存储路径
        // - 目的: 避免日志与数据混用
        let raft_db_path = base_dir.join("raft_db");
        let sqlite_path = base_dir.join("node.db");

        // ### 修改记录 (2026-03-15)
        // - 原因: 需要初始化 RaftStore
        // - 目的: 提供日志与元数据持久化
        let store = RaftStore::open(&raft_db_path)?;

        // ### 修改记录 (2026-03-15)
        // - 原因: 需要初始化状态机
        // - 目的: 接收 Raft apply 的 SQL
        let state_machine = SqliteStateMachine::new(sqlite_path.to_string_lossy().to_string())?;

        // ### 修改记录 (2026-03-15)
        // - 原因: 需要与生产一致的 Raft 配置
        // - 目的: 保持选举与心跳行为一致
        let config = Config {
            heartbeat_interval: 100,
            election_timeout_min: 200,
            election_timeout_max: 300,
            ..Default::default()
        };
        let config = build_validated_raft_config(config)?;

        // ### 修改记录 (2026-03-15)
        // - 原因: 需要 gRPC 网络工厂
        // - 目的: 让 Raft RPC 通过 gRPC 发送
        let network = RaftGrpcNetworkFactory::new();

        // ### 修改记录 (2026-03-15)
        // - 原因: 需要创建 Raft 核心实例
        // - 目的: 启动复制与选举
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
            raft,
            state_machine,
        })
    }

    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要在 leader 上写入 SQL
    /// - 目的: 触发日志复制
    async fn apply_sql(&self, sql: String) -> Result<()> {
        let req = RaftRequest::Write { sql };
        let _ = self.raft.client_write(req).await?;
        Ok(())
    }

    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要在 follower 上读取结果
    /// - 目的: 验证日志应用完成
    async fn query_scalar(&self, sql: String) -> Result<String> {
        self.state_machine.query_scalar(sql).await
    }

    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要等待 leader 选举完成
    /// - 目的: 避免初始化后立即写入导致 NOT_LEADER
    async fn wait_for_leader(&self) -> Result<NodeId> {
        for _ in 0..200 {
            let metrics = self.raft.metrics().borrow().clone();
            if metrics.state == ServerState::Leader
                && matches!(metrics.current_leader, Some(id) if id == self.node_id)
            {
                return Ok(self.node_id);
            }
            sleep(Duration::from_millis(50)).await;
        }
        Err(anyhow!("leader not elected"))
    }
}

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要测试内的 gRPC 服务实现
/// - 目的: 将 RaftCore 暴露为 RaftService
#[derive(Clone)]
struct TestRaftService {
    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要访问 Raft 核心
    /// - 目的: 执行 AppendEntries/Vote/InstallSnapshot
    raft: RaftCore,
}

impl TestRaftService {
    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要构造服务实例
    /// - 目的: 注入 RaftCore
    fn new(raft: RaftCore) -> Self {
        Self { raft }
    }
}

#[tonic::async_trait]
impl RaftService for TestRaftService {
    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要 gRPC AppendEntries 入口
    /// - 目的: 支持日志复制 RPC
    async fn append_entries(
        &self,
        request: Request<RaftPayload>,
    ) -> Result<Response<RaftPayload>, Status> {
        let req: AppendEntriesRequest<TypeConfig> =
            decode_payload(&request.into_inner().payload)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let resp: Result<AppendEntriesResponse<NodeId>, RaftError<NodeId>> =
            self.raft.append_entries(req).await;
        let payload = encode_payload(&resp).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(RaftPayload { payload }))
    }

    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要 gRPC InstallSnapshot 入口
    /// - 目的: 支持快照同步 RPC
    async fn install_snapshot(
        &self,
        request: Request<RaftPayload>,
    ) -> Result<Response<RaftPayload>, Status> {
        let req: InstallSnapshotRequest<TypeConfig> =
            decode_payload(&request.into_inner().payload)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let resp: Result<InstallSnapshotResponse<NodeId>, RaftError<NodeId, InstallSnapshotError>> =
            self.raft.install_snapshot(req).await;
        let payload = encode_payload(&resp).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(RaftPayload { payload }))
    }

    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要 gRPC Vote 入口
    /// - 目的: 支持选举 RPC
    async fn vote(
        &self,
        request: Request<RaftPayload>,
    ) -> Result<Response<RaftPayload>, Status> {
        let req: VoteRequest<NodeId> = decode_payload(&request.into_inner().payload)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let resp: Result<VoteResponse<NodeId>, RaftError<NodeId>> = self.raft.vote(req).await;
        let payload = encode_payload(&resp).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(RaftPayload { payload }))
    }
}

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要统一 Raft 配置校验逻辑
/// - 目的: 避免使用未校验的配置
fn build_validated_raft_config(config: Config) -> Result<Arc<Config>> {
    Ok(Arc::new(
        config
            .validate()
            .map_err(|e| anyhow!("invalid raft config: {e}"))?,
    ))
}

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要启动 gRPC 服务端
/// - 目的: 让 Raft 节点可被远端调用
async fn spawn_raft_grpc_server(node: &TestRaftNode) -> Result<SocketAddr> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let service = TestRaftService::new(node.raft.clone());
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
/// - 原因: 需要等待 follower 应用完成
/// - 目的: 避免竞态导致读不到数据
async fn wait_for_scalar(
    node: &TestRaftNode,
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
/// - 原因: 需要最小成功链路集成测试入口
/// - 目的: 覆盖 gRPC 日志复制与状态机应用
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grpc_replication_minimal_success_path() {
    // ### 修改记录 (2026-03-15)
    // - 原因: 需要隔离测试目录
    // - 目的: 避免并发测试冲突
    let base_dir_1 = std::env::temp_dir()
        .join(format!("raft_grpc_replication_node1_{}", Uuid::new_v4()));
    let base_dir_2 = std::env::temp_dir()
        .join(format!("raft_grpc_replication_node2_{}", Uuid::new_v4()));

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要两个 Raft 节点实例
    // - 目的: 构造最小复制场景
    let node1 = TestRaftNode::start_grpc(1, base_dir_1).await.unwrap();
    let node2 = TestRaftNode::start_grpc(2, base_dir_2).await.unwrap();

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要启动 gRPC 服务端
    // - 目的: 提供 RPC 通信入口
    let _addr1 = spawn_raft_grpc_server(&node1).await.unwrap();
    let addr2 = spawn_raft_grpc_server(&node2).await.unwrap();

    // ### 修改记录 (2026-03-15)
    // - 原因: 初始化集群需要成员集合
    // - 目的: 让 node1 成为初始 leader
    let mut members = BTreeSet::new();
    members.insert(1);
    node1.raft.initialize(members).await.unwrap();
    node1.wait_for_leader().await.unwrap();

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要添加 follower 节点
    // - 目的: 让 node2 加入复制链路
    node1
        .raft
        .add_learner(2, BasicNode::new(format!("http://{}", addr2)), true)
        .await
        .unwrap();

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要提升 learner 为 voter
    // - 目的: 进入正式成员集群
    node1
        .raft
        .change_membership(BTreeSet::from([1, 2]), true)
        .await
        .unwrap();

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要最小写入
    // - 目的: 触发日志复制并验证应用
    node1
        .apply_sql("CREATE TABLE IF NOT EXISTS t(v INTEGER);".to_string())
        .await
        .unwrap();
    node1
        .apply_sql("INSERT INTO t(v) VALUES (42);".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-03-15)
    // - 原因: follower 应用存在异步延迟
    // - 目的: 轮询等待数据可见
    let value = wait_for_scalar(
        &node2,
        "SELECT COUNT(*) FROM t;",
        "1",
        50,
        Duration::from_millis(100),
    )
    .await
    .unwrap();

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要确认复制生效
    // - 目的: 验证最小成功链路
    assert_eq!(value.trim(), "1");
}
