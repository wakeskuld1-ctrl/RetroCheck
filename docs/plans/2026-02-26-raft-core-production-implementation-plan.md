 # Raft 主链路生产级骨架 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 接入 OpenRaft，完成单节点/多节点可运行的 Raft 主链路骨架并落地 SQLite 状态机。

**Architecture:** 基于 OpenRaft 的 RaftTypeConfig + RaftLogStorage/RaftStateMachine 实现，使用 sled 持久化日志/元数据，SqliteStateMachine 作为 apply 入口，RaftNetwork 以进程内注册表实现最小网络可运行闭环。

**Tech Stack:** Rust, openraft 0.9, sled, tokio, serde, anyhow

---

### Task 1: 定义 OpenRaft 类型配置与请求/响应结构

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/edge-m1/src/raft/types.rs`
- Test: `d:/Rust/check_program/.worktrees/edge-m1/tests/raft_types.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn raft_type_config_is_constructible() {
    let _req = check_program::raft::types::Request::Write { sql: "SELECT 1".to_string() };
    let _resp = check_program::raft::types::Response { value: Some("ok".to_string()) };
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test raft_types`  
Expected: FAIL（类型未暴露或未满足 OpenRaft 约束）

**Step 3: Write minimal implementation**

```rust
use openraft::{RaftTypeConfig, Entry, BasicNode, TokioRuntime};
use serde::{Deserialize, Serialize};

pub type NodeId = u64;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TypeConfig;

impl RaftTypeConfig for TypeConfig {
    type D = Request;
    type R = Response;
    type NodeId = NodeId;
    type Node = BasicNode;
    type Entry = Entry<TypeConfig>;
    type SnapshotData = std::io::Cursor<Vec<u8>>;
    type AsyncRuntime = TokioRuntime;
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test raft_types`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/edge-m1/src/raft/types.rs d:/Rust/check_program/.worktrees/edge-m1/tests/raft_types.rs
git commit -m "feat(raft): add openraft type config"
```

---

### Task 2: 实现 RaftLogStorage（sled 日志与元数据）

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/edge-m1/src/raft/store.rs`
- Test: `d:/Rust/check_program/.worktrees/edge-m1/tests/raft_store.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn raft_store_appends_and_reads_logs() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = check_program::raft::store::RaftStore::open(dir.path()).unwrap();
    let entry = openraft::Entry::<check_program::raft::types::TypeConfig>::new(
        openraft::LogId::new(openraft::CommittedLeaderId::new(1, 1), 1),
        openraft::EntryPayload::Blank
    );
    store.append(vec![entry.clone()], openraft::storage::LogFlushed::noop()).await.unwrap();
    let mut reader = store.get_log_reader().await;
    let entries = reader.try_get_log_entries(1..2).await.unwrap();
    assert_eq!(entries.len(), 1);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test raft_store`  
Expected: FAIL（RaftLogStorage 未实现）

**Step 3: Write minimal implementation**

```rust
use openraft::storage::{RaftLogStorage, RaftLogReader, LogState, LogFlushed};
use openraft::{LogId, Vote, StorageError};
use std::ops::RangeBounds;

#[derive(Clone)]
pub struct RaftStore {
    db: sled::Db,
    log_tree: sled::Tree,
    meta_tree: sled::Tree,
}

impl RaftStore {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let db = sled::open(path)?;
        let log_tree = db.open_tree("raft_log")?;
        let meta_tree = db.open_tree("raft_meta")?;
        Ok(Self { db, log_tree, meta_tree })
    }
}

impl RaftLogReader<TypeConfig> for RaftStore {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + std::fmt::Debug + openraft::OptionalSend>(
        &mut self,
        range: RB,
    ) -> Result<Vec<openraft::Entry<TypeConfig>>, StorageError<NodeId>> {
        let mut res = Vec::new();
        for item in self.log_tree.range(range) {
            let (_, value) = item.map_err(StorageError::IO)?;
            let entry: openraft::Entry<TypeConfig> = bincode::deserialize(&value).map_err(StorageError::IO)?;
            res.push(entry);
        }
        Ok(res)
    }
}

impl RaftLogStorage<TypeConfig> for RaftStore {
    type LogReader = RaftStore;
    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, StorageError<NodeId>> { /* ... */ }
    async fn get_log_reader(&mut self) -> Self::LogReader { self.clone() }
    async fn save_vote(&mut self, vote: &Vote<NodeId>) -> Result<(), StorageError<NodeId>> { /* ... */ }
    async fn read_vote(&mut self) -> Result<Option<Vote<NodeId>>, StorageError<NodeId>> { /* ... */ }
    async fn append<I>(&mut self, entries: I, callback: LogFlushed<TypeConfig>) -> Result<(), StorageError<NodeId>>
    where I: IntoIterator<Item = openraft::Entry<TypeConfig>> + openraft::OptionalSend,
          I::IntoIter: openraft::OptionalSend { /* ... */ }
    async fn truncate(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> { /* ... */ }
    async fn purge(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> { /* ... */ }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test raft_store`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/edge-m1/src/raft/store.rs d:/Rust/check_program/.worktrees/edge-m1/tests/raft_store.rs
git commit -m "feat(raft): implement raft log storage on sled"
```

---

### Task 3: 实现 RaftStateMachine（SQLite apply + snapshot 占位）

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/edge-m1/src/raft/state_machine.rs`
- Test: `d:/Rust/check_program/.worktrees/edge-m1/tests/raft_state_machine.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn raft_state_machine_applies_entries() {
    let dir = tempfile::tempdir().unwrap();
    let sm = check_program::raft::state_machine::SqliteStateMachine::new(
        dir.path().join("node.db").to_string_lossy().to_string()
    ).unwrap();
    let entry = openraft::Entry::<check_program::raft::types::TypeConfig>::new(
        openraft::LogId::new(openraft::CommittedLeaderId::new(1, 1), 1),
        openraft::EntryPayload::Normal(
            check_program::raft::types::Request::Write { sql: "CREATE TABLE t(x INT)".to_string() }
        )
    );
    let mut sm_impl = check_program::raft::state_machine::RaftSqliteStateMachine::new(sm);
    sm_impl.apply(vec![entry]).await.unwrap();
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test raft_state_machine`  
Expected: FAIL（RaftStateMachine 未实现）

**Step 3: Write minimal implementation**

```rust
use openraft::storage::{RaftStateMachine, Snapshot, SnapshotMeta, RaftSnapshotBuilder};
use openraft::{LogId, StoredMembership, StorageError};

pub struct RaftSqliteStateMachine {
    inner: SqliteStateMachine,
    last_applied: Option<LogId<NodeId>>,
    last_membership: StoredMembership<NodeId, openraft::BasicNode>,
}

impl RaftSqliteStateMachine {
    pub fn new(inner: SqliteStateMachine) -> Self { /* ... */ }
}

impl RaftStateMachine<TypeConfig> for RaftSqliteStateMachine {
    type SnapshotBuilder = RaftSnapshotBuilderNoop;
    async fn applied_state(&mut self) -> Result<(Option<LogId<NodeId>>, StoredMembership<NodeId, openraft::BasicNode>), StorageError<NodeId>> { /* ... */ }
    async fn apply<I>(&mut self, entries: I) -> Result<Vec<Response>, StorageError<NodeId>>
    where I: IntoIterator<Item = openraft::Entry<TypeConfig>> + openraft::OptionalSend,
          I::IntoIter: openraft::OptionalSend { /* ... */ }
    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder { /* ... */ }
    async fn begin_receiving_snapshot(&mut self) -> Result<Box<std::io::Cursor<Vec<u8>>>, StorageError<NodeId>> { /* ... */ }
    async fn install_snapshot(&mut self, meta: &SnapshotMeta<NodeId, openraft::BasicNode>, snapshot: Box<std::io::Cursor<Vec<u8>>>) -> Result<(), StorageError<NodeId>> { /* ... */ }
    async fn get_current_snapshot(&mut self) -> Result<Option<Snapshot<TypeConfig>>, StorageError<NodeId>> { /* ... */ }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test raft_state_machine`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/edge-m1/src/raft/state_machine.rs d:/Rust/check_program/.worktrees/edge-m1/tests/raft_state_machine.rs
git commit -m "feat(raft): implement raft state machine wrapper for sqlite"
```

---

### Task 4: 实现 RaftNetworkFactory（进程内最小网络）

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/edge-m1/src/raft/network.rs`
- Test: `d:/Rust/check_program/.worktrees/edge-m1/tests/raft_network.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn raft_network_factory_creates_client() {
    let registry = check_program::raft::network::RaftRegistry::new();
    let mut factory = check_program::raft::network::RaftNetworkFactoryImpl::new(registry);
    let node = openraft::BasicNode::new("local".to_string());
    let _client = factory.new_client(1, &node).await;
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test raft_network`  
Expected: FAIL（RaftNetworkFactory 未实现）

**Step 3: Write minimal implementation**

```rust
use openraft::network::{RaftNetwork, RaftNetworkFactory};
use openraft::raft::{AppendEntriesRequest, AppendEntriesResponse, VoteRequest, VoteResponse, InstallSnapshotRequest, InstallSnapshotResponse};

#[derive(Clone)]
pub struct RaftRegistry { /* HashMap<NodeId, RaftNodeHandle> */ }

pub struct RaftNetworkFactoryImpl {
    registry: RaftRegistry,
}

impl RaftNetworkFactory<TypeConfig> for RaftNetworkFactoryImpl {
    type Network = RaftNetworkClient;
    async fn new_client(&mut self, target: NodeId, _node: &openraft::BasicNode) -> Self::Network {
        RaftNetworkClient::new(target, self.registry.clone())
    }
}

pub struct RaftNetworkClient { /* ... */ }

impl RaftNetwork<TypeConfig> for RaftNetworkClient {
    async fn append_entries(&mut self, rpc: AppendEntriesRequest<TypeConfig>) -> Result<AppendEntriesResponse<NodeId>, openraft::error::RPCError<NodeId, openraft::error::RaftError<NodeId>>> { /* ... */ }
    async fn vote(&mut self, rpc: VoteRequest<NodeId>) -> Result<VoteResponse<NodeId>, openraft::error::RPCError<NodeId, openraft::error::RaftError<NodeId>>> { /* ... */ }
    async fn install_snapshot(&mut self, rpc: InstallSnapshotRequest<TypeConfig>) -> Result<InstallSnapshotResponse<NodeId>, openraft::error::RPCError<NodeId, openraft::error::RaftError<NodeId>>> { /* ... */ }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test raft_network`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/edge-m1/src/raft/network.rs d:/Rust/check_program/.worktrees/edge-m1/tests/raft_network.rs
git commit -m "feat(raft): add in-process raft network factory"
```

---

### Task 5: 改造 RaftNode 以接入 OpenRaft 主链路

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/edge-m1/src/raft/raft_node.rs`
- Modify: `d:/Rust/check_program/.worktrees/edge-m1/src/raft/router.rs`
- Test: `d:/Rust/check_program/.worktrees/edge-m1/tests/raft_core_bootstrap.rs`
- Test: `d:/Rust/check_program/.worktrees/edge-m1/tests/raft_cluster.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn raft_node_bootstrap_and_write_commits() {
    let dir = tempfile::tempdir().unwrap();
    let registry = check_program::raft::network::RaftRegistry::new();
    let node = check_program::raft::raft_node::RaftNode::start_local(
        1,
        dir.path().join("node.db").to_string_lossy().to_string(),
        registry.clone()
    ).await.unwrap();
    node.initialize_single_node().await.unwrap();
    let rows = node.client_write("CREATE TABLE t(x INT)".to_string()).await.unwrap();
    assert_eq!(rows, 0);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test raft_core_bootstrap`  
Expected: FAIL（RaftNode 未接入 OpenRaft）

**Step 3: Write minimal implementation**

```rust
pub struct RaftNode {
    id: NodeId,
    raft: openraft::Raft<TypeConfig>,
    state_machine: RaftSqliteStateMachine,
}

impl RaftNode {
    pub async fn start_local(id: NodeId, db_path: String, registry: RaftRegistry) -> Result<Self> {
        let config = openraft::Config::build("check_program".to_string()).validate()?;
        let storage = RaftStore::open(std::path::Path::new(&db_path).parent().unwrap())?;
        let sm = SqliteStateMachine::new(db_path)?;
        let raft_sm = RaftSqliteStateMachine::new(sm);
        let raft = openraft::Raft::new(id, config, RaftNetworkFactoryImpl::new(registry), storage).await?;
        Ok(Self { id, raft, state_machine: raft_sm })
    }

    pub async fn initialize_single_node(&self) -> Result<()> {
        let mut members = std::collections::BTreeMap::new();
        members.insert(self.id, openraft::BasicNode::new("local".to_string()));
        self.raft.initialize(members).await?;
        Ok(())
    }

    pub async fn client_write(&self, sql: String) -> Result<usize> {
        let req = Request::Write { sql };
        let resp = self.raft.client_write(req).await?;
        Ok(resp.data.value.unwrap_or_default().parse().unwrap_or(0))
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test raft_core_bootstrap`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/edge-m1/src/raft/raft_node.rs d:/Rust/check_program/.worktrees/edge-m1/src/raft/router.rs d:/Rust/check_program/.worktrees/edge-m1/tests/raft_core_bootstrap.rs d:/Rust/check_program/.worktrees/edge-m1/tests/raft_cluster.rs
git commit -m "feat(raft): wire openraft into raft node and router"
```

---

### Task 6: 多节点一致性与重启回归

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/edge-m1/tests/raft_cluster.rs`
- Modify: `d:/Rust/check_program/.worktrees/edge-m1/tests/raft_integration.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn raft_cluster_commits_across_three_nodes() {
    let registry = check_program::raft::network::RaftRegistry::new();
    let nodes = check_program::raft::raft_node::RaftNode::start_three_nodes_for_test(registry).await;
    let leader = nodes[0].clone();
    leader.initialize_single_node().await.unwrap();
    let _ = leader.client_write("CREATE TABLE t(x INT)".to_string()).await.unwrap();
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test raft_cluster`  
Expected: FAIL（多节点链路未落地）

**Step 3: Write minimal implementation**

```rust
impl RaftNode {
    pub async fn start_three_nodes_for_test(registry: RaftRegistry) -> Result<Vec<RaftNode>> { /* ... */ }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test raft_cluster`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/edge-m1/tests/raft_cluster.rs d:/Rust/check_program/.worktrees/edge-m1/tests/raft_integration.rs
git commit -m "test(raft): add multi-node raft core regression cases"
```

---

### 追加说明：方案 A（OpenRaft storage-v2 + serde）

**目标：** 解决 OpenRaft 0.9 `RaftLogStorage`/`RaftStateMachine` 封闭 trait 导致的编译失败，并统一日志序列化与错误语义。

**关键变更：**
- 调整 `Cargo.toml` 中 `openraft` 依赖，开启 `storage-v2` 与 `serde` feature，确保 v2 存储接口可实现且 `LogId/Entry` 支持序列化。
- 将 `StorageError` 的构造改为显式 `StorageIOError`（包含 `ErrorSubject` 与 `ErrorVerb`），避免 `err.into()` 造成类型或语义不完整。
- 保留现有 `RaftStore` 的 `log_tree/meta_tree` 设计与日志键序列化策略，减少对方案 A 未来演进的影响。

**适配范围：**
- 本实现以 `d:/Rust/check_program/.worktrees/raft-core-production` 为目标路径，避免混用旧计划中的 `edge-m1` 路径。
