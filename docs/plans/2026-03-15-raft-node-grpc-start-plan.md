# RaftNode gRPC Start Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a gRPC-based RaftNode startup path (`start_grpc`) and switch production entrypoints to it while preserving in-memory paths for tests.

**Architecture:** Introduce a generic `start_with_network` that accepts any `RaftNetworkFactory`, implement `start_grpc` using `RaftGrpcNetworkFactory`, and keep `start`/`start_local` on the in-memory router. Update server and cluster admin to use the gRPC startup path; add a minimal gRPC startup test.

**Tech Stack:** Rust, OpenRaft, tonic gRPC, Tokio, SQLite state machine.

---

### Task 1: Add failing gRPC startup test

**Files:**
- Create: `tests/raft_node_start_grpc.rs`

**Step 1: Write the failing test**

```rust
use check_program::raft::grpc_service::RaftServiceImpl;
use check_program::raft::raft_node::RaftNode;
use check_program::pb::raft_service_server::RaftServiceServer;
use openraft::BasicNode;
use tempfile::TempDir;
use tonic::transport::Server;

#[tokio::test]
async fn start_grpc_can_bootstrap_and_replicate_minimally() {
    let dir1 = TempDir::new().expect("tempdir 1");
    let dir2 = TempDir::new().expect("tempdir 2");

    let node1 = RaftNode::start_grpc(1, dir1.path().to_path_buf())
        .await
        .expect("start_grpc node1");
    let node2 = RaftNode::start_grpc(2, dir2.path().to_path_buf())
        .await
        .expect("start_grpc node2");

    let addr1 = "127.0.0.1:0".parse().unwrap();
    let addr2 = "127.0.0.1:0".parse().unwrap();

    let svc1 = RaftServiceImpl::new(node1.clone());
    let svc2 = RaftServiceImpl::new(node2.clone());

    let listener1 = tokio::net::TcpListener::bind(addr1).await.unwrap();
    let listener2 = tokio::net::TcpListener::bind(addr2).await.unwrap();
    let addr1 = listener1.local_addr().unwrap();
    let addr2 = listener2.local_addr().unwrap();

    let server1 = Server::builder()
        .add_service(RaftServiceServer::new(svc1))
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener1));
    let server2 = Server::builder()
        .add_service(RaftServiceServer::new(svc2))
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener2));

    tokio::spawn(server1);
    tokio::spawn(server2);

    let mut members = std::collections::BTreeSet::new();
    members.insert(1);
    node1.raft.initialize(members).await.expect("init");

    node1
        .raft
        .add_learner(2, BasicNode::new(format!("http://{}", addr2)), true)
        .await
        .expect("add_learner");

    node1
        .raft
        .change_membership(std::collections::BTreeSet::from([1, 2]), true)
        .await
        .expect("promote");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q --test raft_node_start_grpc`

Expected: FAIL with error like `no function or associated item named 'start_grpc' found for struct 'RaftNode'`.

**Step 3: Commit the failing test**

```bash
git add tests/raft_node_start_grpc.rs
git commit -m "test: add failing start_grpc integration"
```

---

### Task 2: Implement `start_with_network` + `start_grpc`

**Files:**
- Modify: `src/raft/raft_node.rs`

**Step 1: Write minimal implementation**

```rust
pub async fn start_with_network<N>(
    node_id: NodeId,
    base_dir: PathBuf,
    network: N,
) -> Result<Self>
where
    N: openraft::RaftNetworkFactory<TypeConfig> + Send + Sync + 'static,
{
    // reuse storage/config/state machine init, but inject network
}

pub async fn start_grpc(node_id: NodeId, base_dir: PathBuf) -> Result<Self> {
    let network = crate::raft::grpc_network::RaftGrpcNetworkFactory::new();
    Self::start_with_network(node_id, base_dir, network).await
}

pub async fn start(node_id: NodeId, base_dir: PathBuf, router: RaftRouter) -> Result<Self> {
    let network = RaftNetworkFactoryImpl::new(router.clone());
    Self::start_with_network(node_id, base_dir, network).await
}
```

**Step 2: Run test to verify it passes**

Run: `cargo test -q --test raft_node_start_grpc`

Expected: PASS.

**Step 3: Commit**

```bash
git add src/raft/raft_node.rs
git commit -m "feat: add start_grpc for raft node"
```

---

### Task 3: Switch server startup to gRPC path

**Files:**
- Modify: `src/bin/server.rs`

**Step 1: Update startup**

```rust
let raft_node = RaftNode::start_grpc(1, std::path::PathBuf::from(db_path)).await?;
```

**Step 2: Run targeted tests**

Run: `cargo test -q --test raft_node_start_grpc`

Expected: PASS.

**Step 3: Commit**

```bash
git add src/bin/server.rs
git commit -m "feat: use start_grpc in server bootstrap"
```

---

### Task 4: Switch dynamic node startup to gRPC path

**Files:**
- Modify: `src/management/cluster_admin_service.rs`

**Step 1: Update add_hub startup**

```rust
let node = RaftNode::start_grpc(req.node_id, node_base_dir).await.map_err(...)?;
```

**Step 2: Run targeted tests**

Run: `cargo test -q --test cluster_admin_add_hub`

Expected: PASS.

**Step 3: Commit**

```bash
git add src/management/cluster_admin_service.rs
git commit -m "feat: use start_grpc for add_hub"
```

---

### Task 5: Update task journal

**Files:**
- Modify: `.trae/CHANGELOG_TASK.md`

**Step 1: Update task journal via skill**

- Invoke `task-journal` skill to append the Task 5 summary and tests run.

**Step 2: Commit**

```bash
git add .trae/CHANGELOG_TASK.md
git commit -m "docs: update task journal for grpc start"
```
