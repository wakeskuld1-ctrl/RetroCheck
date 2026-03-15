# Raft gRPC + ReadIndex 一致性闭环 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 用 gRPC 替换 Raft 内存通信，并补齐线性一致读与成员变更闭环，达到可部署的一致性路径。

**Architecture:** 增加 RaftService gRPC 与 RaftGrpcNetworkFactory；生产路径通过 gRPC 进行 Raft RPC；读路径在 leader 上使用 ensure_linearizable，并对 follower 读做 leader 转发；成员变更保留现有流程并补齐地址规范化。

**Tech Stack:** Rust 2024, OpenRaft 0.9, tonic gRPC, bincode/serde, tokio

---

> 约束提醒（实现时必须遵守）
> - 每次修改需在代码旁边写 **Markdown 修改记录**（原因/目的/日期），如重复修改同一段则在原注释后追加。
> - 备注与代码比例 ≥ 6:4。
> - 发现 BUG 先写可复现测试，再修复直到通过。

### Task 1: Proto 定义 RaftService 与生成验证

**Files:**
- Modify: `proto/transaction.proto`
- Test: `tests/raft_proto_generated.rs`

**Step 1: Write the failing test**

```rust
// tests/raft_proto_generated.rs
#[test]
fn raft_service_proto_is_generated() {
    // 只要编译能找到类型就算通过
    let _ = std::any::type_name::<check_program::pb::raft_service_client::RaftServiceClient<tonic::transport::Channel>>();
    let _ = std::any::type_name::<check_program::pb::raft_service_server::RaftServiceServer<()>>();
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q --test raft_proto_generated`
Expected: FAIL（找不到 raft_service_* 模块）

**Step 3: Write minimal implementation**

```proto
// proto/transaction.proto
service RaftService {
  rpc AppendEntries (RaftPayload) returns (RaftPayload);
  rpc InstallSnapshot (RaftPayload) returns (RaftPayload);
  rpc Vote (RaftPayload) returns (RaftPayload);
}

message RaftPayload { bytes payload = 1; }
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q --test raft_proto_generated`
Expected: PASS

**Step 5: Commit**

```bash
git add proto/transaction.proto tests/raft_proto_generated.rs
git commit -m "feat: add raft grpc proto"
```

### Task 2: bincode 编解码辅助模块

**Files:**
- Create: `src/raft/grpc_codec.rs`
- Modify: `src/raft/mod.rs`
- Test: `tests/raft_grpc_codec.rs`

**Step 1: Write the failing test**

```rust
// tests/raft_grpc_codec.rs
use check_program::raft::grpc_codec::{decode_payload, encode_payload};
use check_program::raft::types::TypeConfig;
use openraft::raft::{AppendEntriesRequest, AppendEntriesResponse};
use openraft::Vote;

#[test]
fn encode_decode_append_entries_roundtrip() {
    let req = AppendEntriesRequest::<TypeConfig> {
        vote: Vote::new(1, 1),
        prev_log_id: None,
        entries: vec![],
        leader_commit: None,
    };
    let payload = encode_payload(&req).expect("encode req");
    let decoded: AppendEntriesRequest<TypeConfig> = decode_payload(&payload).expect("decode req");
    assert_eq!(decoded.vote, req.vote);

    let resp: Result<AppendEntriesResponse<u64>, openraft::error::RaftError<u64>> =
        Ok(AppendEntriesResponse::Success);
    let payload = encode_payload(&resp).expect("encode resp");
    let decoded: Result<AppendEntriesResponse<u64>, openraft::error::RaftError<u64>> =
        decode_payload(&payload).expect("decode resp");
    assert!(decoded.is_ok());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q --test raft_grpc_codec`
Expected: FAIL（模块/函数不存在）

**Step 3: Write minimal implementation**

```rust
// src/raft/grpc_codec.rs
use anyhow::{Result, anyhow};
use serde::{Serialize, de::DeserializeOwned};

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要统一 Raft gRPC 载荷序列化
/// - 目的: 让请求/响应通过 bytes 传输
pub fn encode_payload<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    bincode::serialize(value).map_err(|e| anyhow!(e))
}

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要统一 Raft gRPC 载荷反序列化
/// - 目的: 将 bytes 还原为 OpenRaft 结构
pub fn decode_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T> {
    bincode::deserialize(payload).map_err(|e| anyhow!(e))
}
```

并在 `src/raft/mod.rs` 增加 `pub mod grpc_codec;`。

**Step 4: Run test to verify it passes**

Run: `cargo test -q --test raft_grpc_codec`
Expected: PASS

**Step 5: Commit**

```bash
git add src/raft/grpc_codec.rs src/raft/mod.rs tests/raft_grpc_codec.rs
git commit -m "feat: add raft grpc codec"
```

### Task 3: RaftService gRPC 服务端

**Files:**
- Create: `src/raft/grpc_service.rs`
- Modify: `src/raft/mod.rs`
- Modify: `src/bin/server.rs`
- Test: `tests/raft_grpc_service_roundtrip.rs`

**Step 1: Write the failing test**

```rust
// tests/raft_grpc_service_roundtrip.rs
use check_program::pb::raft_service_client::RaftServiceClient;
use check_program::pb::RaftPayload;
use check_program::raft::grpc_codec::{encode_payload, decode_payload};
use check_program::raft::raft_node::RaftNode;
use check_program::raft::grpc_service::RaftServiceImpl;
use check_program::raft::types::TypeConfig;
use openraft::raft::VoteRequest;
use openraft::Vote;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tonic::transport::Server;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raft_service_vote_roundtrip() {
    let base_dir = std::env::temp_dir().join("raft_grpc_vote_roundtrip");
    let node = RaftNode::start_local(1, base_dir).await.unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let service = RaftServiceImpl::new(node);

    tokio::spawn(async move {
        Server::builder()
            .add_service(check_program::pb::raft_service_server::RaftServiceServer::new(service))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    let mut client = RaftServiceClient::connect(format!("http://{}", addr)).await.unwrap();
    let req = VoteRequest::new(Vote::new(1, 2), None);
    let payload = encode_payload(&req).unwrap();
    let resp = client.vote(RaftPayload { payload }).await.unwrap().into_inner();
    let decoded: Result<openraft::raft::VoteResponse<u64>, openraft::error::RaftError<u64>> =
        decode_payload(&resp.payload).unwrap();
    assert!(decoded.is_ok());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q --test raft_grpc_service_roundtrip`
Expected: FAIL（服务未实现）

**Step 3: Write minimal implementation**

```rust
// src/raft/grpc_service.rs
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

#[derive(Clone)]
pub struct RaftServiceImpl {
    node: RaftNode,
}

impl RaftServiceImpl {
    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要在 gRPC 层封装 RaftNode
    /// - 目的: 暴露 AppendEntries/Vote/InstallSnapshot 入口
    pub fn new(node: RaftNode) -> Self {
        Self { node }
    }
}

#[tonic::async_trait]
impl RaftService for RaftServiceImpl {
    async fn append_entries(
        &self,
        request: Request<RaftPayload>,
    ) -> Result<Response<RaftPayload>, Status> {
        let req: AppendEntriesRequest<TypeConfig> =
            decode_payload(&request.into_inner().payload).map_err(|e| Status::invalid_argument(e.to_string()))?;
        let resp: Result<AppendEntriesResponse<NodeId>, RaftError<NodeId>> = self.node.raft.append_entries(req).await;
        let payload = encode_payload(&resp).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(RaftPayload { payload }))
    }

    async fn install_snapshot(
        &self,
        request: Request<RaftPayload>,
    ) -> Result<Response<RaftPayload>, Status> {
        let req: InstallSnapshotRequest<TypeConfig> =
            decode_payload(&request.into_inner().payload).map_err(|e| Status::invalid_argument(e.to_string()))?;
        let resp: Result<InstallSnapshotResponse<NodeId>, RaftError<NodeId, InstallSnapshotError>> =
            self.node.raft.install_snapshot(req).await;
        let payload = encode_payload(&resp).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(RaftPayload { payload }))
    }

    async fn vote(
        &self,
        request: Request<RaftPayload>,
    ) -> Result<Response<RaftPayload>, Status> {
        let req: VoteRequest<NodeId> =
            decode_payload(&request.into_inner().payload).map_err(|e| Status::invalid_argument(e.to_string()))?;
        let resp: Result<VoteResponse<NodeId>, RaftError<NodeId>> = self.node.raft.vote(req).await;
        let payload = encode_payload(&resp).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(RaftPayload { payload }))
    }
}
```

并在 `src/raft/mod.rs` 增加 `pub mod grpc_service;`。
`src/bin/server.rs` 注册 `RaftServiceServer` 与现有 gRPC 服务共端口。

**Step 4: Run test to verify it passes**

Run: `cargo test -q --test raft_grpc_service_roundtrip`
Expected: PASS

**Step 5: Commit**

```bash
git add src/raft/grpc_service.rs src/raft/mod.rs src/bin/server.rs tests/raft_grpc_service_roundtrip.rs
git commit -m "feat: add raft grpc service"
```

### Task 4: Raft gRPC 网络客户端实现

**Files:**
- Create: `src/raft/grpc_network.rs`
- Modify: `src/raft/mod.rs`
- Test: `tests/raft_grpc_network_client.rs`

**Step 1: Write the failing test**

```rust
// tests/raft_grpc_network_client.rs
use check_program::raft::grpc_network::RaftGrpcNetworkFactory;
use check_program::raft::types::TypeConfig;
use openraft::raft::VoteRequest;
use openraft::Vote;
use openraft::BasicNode;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grpc_network_client_can_call_vote() {
    // 仅验证能够创建 client 并调用 vote，具体响应由服务端测试覆盖
    let mut factory = RaftGrpcNetworkFactory::new();
    let mut client = factory
        .new_client(2, &BasicNode::new("http://127.0.0.1:59999".to_string()))
        .await;
    let req = VoteRequest::new(Vote::new(1, 1), None);
    let result = client.vote(req, openraft::network::RPCOption::new(std::time::Duration::from_millis(50))).await;
    assert!(result.is_err());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q --test raft_grpc_network_client`
Expected: FAIL（模块不存在）

**Step 3: Write minimal implementation**

```rust
// src/raft/grpc_network.rs
use crate::pb::raft_service_client::RaftServiceClient;
use crate::pb::RaftPayload;
use crate::raft::grpc_codec::{decode_payload, encode_payload};
use crate::raft::types::{NodeId, TypeConfig};
use openraft::error::{NetworkError, RPCError, RaftError, RemoteError, InstallSnapshotError};
use openraft::network::RPCOption;
use openraft::raft::{AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse, VoteRequest, VoteResponse};
use openraft::{BasicNode, RaftNetwork, RaftNetworkFactory};
use tonic::transport::Channel;
use tonic::Status;

#[derive(Clone)]
pub struct RaftGrpcNetworkFactory;

impl RaftGrpcNetworkFactory {
    /// ### 修改记录 (2026-03-15)
    /// - 原因: 需要提供 gRPC 网络工厂
    /// - 目的: 让 Raft 节点走真实网络通信
    pub fn new() -> Self {
        Self
    }
}

pub struct RaftGrpcNetwork {
    target: NodeId,
    target_node: BasicNode,
    client: RaftServiceClient<Channel>,
}

impl RaftGrpcNetwork {
    async fn call_payload<Req, Resp, E>(
        &mut self,
        payload: &Req,
        f: impl FnOnce(&mut RaftServiceClient<Channel>, RaftPayload) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<tonic::Response<RaftPayload>, Status>> + Send>>,
    ) -> Result<Resp, RPCError<NodeId, BasicNode, RaftError<NodeId, E>>>
    where
        Req: serde::Serialize,
        Resp: serde::de::DeserializeOwned,
        E: std::error::Error,
    {
        let payload = encode_payload(payload).map_err(|e| RPCError::Network(NetworkError::new(&e)))?;
        let response = f(&mut self.client, RaftPayload { payload }).await.map_err(|e| RPCError::Network(NetworkError::new(&e)))?;
        let result: Result<Resp, RaftError<NodeId, E>> =
            decode_payload(&response.into_inner().payload).map_err(|e| RPCError::Network(NetworkError::new(&e)))?;
        match result {
            Ok(resp) => Ok(resp),
            Err(err) => Err(RPCError::RemoteError(RemoteError::new_with_node(self.target, self.target_node.clone(), err))),
        }
    }
}

#[tonic::async_trait]
impl RaftNetwork<TypeConfig> for RaftGrpcNetwork {
    async fn append_entries(
        &mut self,
        req: AppendEntriesRequest<TypeConfig>,
        option: RPCOption,
    ) -> Result<AppendEntriesResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        let _ = option; // 先保留，后续接入 gRPC timeout
        self.call_payload(&req, |client, payload| Box::pin(client.append_entries(payload))).await
    }

    async fn install_snapshot(
        &mut self,
        req: InstallSnapshotRequest<TypeConfig>,
        option: RPCOption,
    ) -> Result<InstallSnapshotResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId, InstallSnapshotError>>> {
        let _ = option;
        self.call_payload(&req, |client, payload| Box::pin(client.install_snapshot(payload))).await
    }

    async fn vote(
        &mut self,
        req: VoteRequest<NodeId>,
        option: RPCOption,
    ) -> Result<VoteResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        let _ = option;
        self.call_payload(&req, |client, payload| Box::pin(client.vote(payload))).await
    }
}

impl RaftNetworkFactory<TypeConfig> for RaftGrpcNetworkFactory {
    type Network = RaftGrpcNetwork;

    async fn new_client(&mut self, target: NodeId, node: &BasicNode) -> Self::Network {
        let addr = if node.addr.starts_with("http") { node.addr.clone() } else { format!("http://{}", node.addr) };
        let client = RaftServiceClient::connect(addr)
            .await
            .expect("raft grpc connect failed");
        RaftGrpcNetwork {
            target,
            target_node: node.clone(),
            client,
        }
    }
}
```

并在 `src/raft/mod.rs` 增加 `pub mod grpc_network;`。

**Step 4: Run test to verify it passes**

Run: `cargo test -q --test raft_grpc_network_client`
Expected: PASS（连接失败也应返回 Err）

**Step 5: Commit**

```bash
git add src/raft/grpc_network.rs src/raft/mod.rs tests/raft_grpc_network_client.rs
git commit -m "feat: add raft grpc network"
```

### Task 5: RaftNode 启动路径切换为 gRPC 网络

**Files:**
- Modify: `src/raft/raft_node.rs`
- Modify: `src/bin/server.rs`

**Step 1: Write the failing test**

```rust
// tests/raft_node_start_grpc.rs
use check_program::raft::raft_node::RaftNode;

#[tokio::test]
async fn raft_node_start_grpc_exists() {
    let base_dir = std::env::temp_dir().join("raft_node_grpc_start");
    let _ = RaftNode::start_grpc(1, base_dir).await.unwrap();
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q --test raft_node_start_grpc`
Expected: FAIL（start_grpc 不存在）

**Step 3: Write minimal implementation**

```rust
// src/raft/raft_node.rs
pub async fn start_grpc(node_id: NodeId, base_dir: PathBuf) -> Result<Self> {
    let network = crate::raft::grpc_network::RaftGrpcNetworkFactory::new();
    Self::start_with_network(node_id, base_dir, network).await
}

pub async fn start_with_network<N>(
    node_id: NodeId,
    base_dir: PathBuf,
    network: N,
) -> Result<Self>
where
    N: openraft::RaftNetworkFactory<TypeConfig>,
{
    // 与 start() 共享初始化逻辑，只替换 network
}
```

并在 `src/bin/server.rs` 中将主节点初始化改为 `RaftNode::start_grpc`，同时注册 `RaftServiceServer`。

**Step 4: Run test to verify it passes**

Run: `cargo test -q --test raft_node_start_grpc`
Expected: PASS

**Step 5: Commit**

```bash
git add src/raft/raft_node.rs src/bin/server.rs tests/raft_node_start_grpc.rs
git commit -m "feat: start raft node with grpc network"
```

### Task 6: 线性一致读（ReadIndex/等价机制）

**Files:**
- Modify: `src/raft/raft_node.rs`
- Modify: `src/raft/router.rs`
- Test: `tests/raft_linearizable_read.rs`

**Step 1: Write the failing test**

```rust
// tests/raft_linearizable_read.rs
use check_program::raft::raft_node::RaftNode;

#[tokio::test]
async fn leader_linearizable_read_succeeds() {
    let base_dir = std::env::temp_dir().join("raft_linearizable_read");
    let node = RaftNode::start_local(1, base_dir).await.unwrap();
    let mut members = std::collections::BTreeSet::new();
    members.insert(1);
    node.raft.initialize(members).await.unwrap();
    node.ensure_linearizable_read().await.unwrap();
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q --test raft_linearizable_read`
Expected: FAIL（ensure_linearizable_read 不存在）

**Step 3: Write minimal implementation**

```rust
// src/raft/raft_node.rs
/// ### 修改记录 (2026-03-15)
/// - 原因: 需要线性一致读的统一入口
/// - 目的: leader 读前确保 ReadIndex 完成
pub async fn ensure_linearizable_read(&self) -> Result<()> {
    self.raft.ensure_linearizable().await.map_err(|e| anyhow!(e))?;
    Ok(())
}
```

在 `Router::get_version` 的 leader 分支中调用 `ensure_linearizable_read` 后再读。

**Step 4: Run test to verify it passes**

Run: `cargo test -q --test raft_linearizable_read`
Expected: PASS

**Step 5: Commit**

```bash
git add src/raft/raft_node.rs src/raft/router.rs tests/raft_linearizable_read.rs
git commit -m "feat: add linearizable read guard"
```

### Task 7: gRPC 复制链路集成测试

**Files:**
- Test: `tests/raft_grpc_replication.rs`
- (使用已有 `RaftServiceImpl` 与 `RaftGrpcNetworkFactory`)

**Step 1: Write the failing test**

```rust
// tests/raft_grpc_replication.rs
use check_program::raft::grpc_network::RaftGrpcNetworkFactory;
use check_program::raft::grpc_service::RaftServiceImpl;
use check_program::raft::raft_node::RaftNode;
use check_program::raft::types::TypeConfig;
use openraft::BasicNode;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tonic::transport::Server;

async fn spawn_raft_server(node: RaftNode) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let service = RaftServiceImpl::new(node);
    tokio::spawn(async move {
        Server::builder()
            .add_service(check_program::pb::raft_service_server::RaftServiceServer::new(service))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    addr
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grpc_replication_applies_sql_on_follower() {
    let node1 = RaftNode::start_grpc(1, std::env::temp_dir().join("raft_grpc_n1")).await.unwrap();
    let node2 = RaftNode::start_grpc(2, std::env::temp_dir().join("raft_grpc_n2")).await.unwrap();
    let addr1 = spawn_raft_server(node1.clone()).await;
    let addr2 = spawn_raft_server(node2.clone()).await;

    let mut members = std::collections::BTreeSet::new();
    members.insert(1);
    node1.raft.initialize(members).await.unwrap();

    node1.raft.add_learner(2, BasicNode::new(format!("http://{}", addr2)), true).await.unwrap();
    node1.raft.change_membership(std::collections::BTreeSet::from([1, 2]), true).await.unwrap();

    node1.apply_sql("CREATE TABLE t(v INTEGER);".to_string()).await.unwrap();
    node1.apply_sql("INSERT INTO t(v) VALUES (42);".to_string()).await.unwrap();

    let value = node2.query_scalar("SELECT COUNT(*) FROM t;".to_string()).await.unwrap();
    assert_eq!(value.trim(), "1");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q --test raft_grpc_replication`
Expected: FAIL（gRPC 网络未接通）

**Step 3: Write minimal implementation**

使用 Task 3/4/5 的代码即可通过。

**Step 4: Run test to verify it passes**

Run: `cargo test -q --test raft_grpc_replication`
Expected: PASS

**Step 5: Commit**

```bash
git add tests/raft_grpc_replication.rs
git commit -m "test: add grpc replication coverage"
```

### Task 8: 版本记录与变更文档

**Files:**
- Create: `docs/changes/2026-03-15-raft-grpc-readindex.md`
- Modify: `Cargo.toml`

**Step 1: Write the failing test**

N/A（文档变更）

**Step 2: Apply changes**

```markdown
# 2026-03-15 Raft gRPC + ReadIndex
- 新增 Raft gRPC 服务与网络客户端
- 读路径引入 ensure_linearizable
- 增加 gRPC 复制链路测试
```

`Cargo.toml` 版本号从 `0.1.0` → `0.2.0-dev`。

**Step 3: Commit**

```bash
git add docs/changes/2026-03-15-raft-grpc-readindex.md Cargo.toml
git commit -m "docs: record raft grpc readindex change"
```

### Task 9: PR/合并准备

**Files:**
- Modify: `README_ZH.md`（当前实现状态/roadmap 简述）

**Step 1: Apply changes**

更新 “当前实现状态” 与 Roadmap：
- 标注 Raft gRPC 已完成
- 线性一致读已落地（leader ensure_linearizable + follower 转发）
- 成员变更支持 gRPC 网络

**Step 2: Commit**

```bash
git add README_ZH.md
git commit -m "docs: update status for raft grpc"
```

---

Plan complete and saved to `docs/plans/2026-03-15-raft-grpc-readindex-plan.md`. Two execution options:

1. Subagent-Driven (this session) — I dispatch a fresh subagent per task, review between tasks, fast iteration
2. Parallel Session (separate) — Open new session with executing-plans, batch execution with checkpoints

Which approach?
