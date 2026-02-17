# Raft Linearizable Read Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 通过 OpenRaft `ensure_linearizable()` 落地强一致读，并让所有 SELECT 走 read_index，失败时回退本地读。

**Architecture:** 在 RaftNode 内接入 OpenRaft 实例并暴露 `ensure_linearizable()`，Router::read 先走 read_index，再读本地状态机；一致性错误回退本地读，无 leader 则统一报错。gRPC Execute 对 SELECT 调用 Router::read。

**Tech Stack:** Rust 2024, OpenRaft 0.9, tokio, tonic, rusqlite, sled

---

### Task 1: 强一致读测试用例

**Files:**
- Create: `d:/Rust/check_program/.worktrees/raft-a2/tests/raft_read.rs`

**Step 1: Write the failing test**

```rust
use check_program::raft::raft_node::RaftNode;
use check_program::raft::router::Router;

#[tokio::test]
async fn read_is_linearizable() {
    let base_dir = std::env::temp_dir().join("raft_read_linearizable");
    let raft_node = RaftNode::start_for_test(1, base_dir).await.unwrap();
    let router = Router::new_with_raft(raft_node.clone());

    router.write("CREATE TABLE IF NOT EXISTS t(x INT)".to_string()).await.unwrap();
    router.write("INSERT INTO t(x) VALUES (1)".to_string()).await.unwrap();

    let value = router.read("SELECT COUNT(*) FROM t".to_string()).await.unwrap();
    assert_eq!(value, "1");
}

#[tokio::test]
async fn read_fallbacks_on_consistency_error() {
    let base_dir = std::env::temp_dir().join("raft_read_fallback");
    let mut raft_node = RaftNode::start_for_test(1, base_dir).await.unwrap();
    raft_node.set_force_read_index_error_for_test(true);
    let router = Router::new_with_raft(raft_node.clone());

    router.write("CREATE TABLE IF NOT EXISTS t(x INT)".to_string()).await.unwrap();
    router.write("INSERT INTO t(x) VALUES (1)".to_string()).await.unwrap();

    let value = router.read("SELECT COUNT(*) FROM t".to_string()).await.unwrap();
    assert_eq!(value, "1");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q read_is_linearizable`  
Expected: FAIL with "no method named read" or missing Raft read_index path.

**Step 3: Write minimal implementation**

```rust
// 先为 Router 添加 read 接口，后续补 RaftNode::ensure_linearizable
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q read_is_linearizable`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/tests/raft_read.rs
git commit -m "test: add linearizable read cases"
```

---

### Task 2: Raft 类型配置（OpenRaft 基础类型）

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/types.rs`

**Step 1: Write the failing test**

Use the tests from Task 1 to drive this change.

**Step 2: Run test to verify it fails**

Run: `cargo test -q read_is_linearizable`  
Expected: FAIL with missing RaftTypeConfig or openraft usage.

**Step 3: Write minimal implementation**

```rust
use openraft::{BasicNode, RaftTypeConfig};

pub struct RaftConfig;

impl RaftTypeConfig for RaftConfig {
    type D = Request;
    type R = Response;
    type NodeId = u64;
    type Node = BasicNode;
    type Entry = openraft::Entry<RaftConfig>;
    type SnapshotData = std::io::Cursor<Vec<u8>>;
    type AsyncRuntime = openraft::TokioRuntime;
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q read_is_linearizable`  
Expected: still FAIL, but type errors resolved.

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/src/raft/types.rs
git commit -m "feat: add openraft type config"
```

---

### Task 3: RaftStateMachine 绑定 SQLite

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/state_machine.rs`

**Step 1: Write the failing test**

Use `read_is_linearizable` test to drive.

**Step 2: Run test to verify it fails**

Run: `cargo test -q read_is_linearizable`  
Expected: FAIL with missing RaftStateMachine impl.

**Step 3: Write minimal implementation**

```rust
use openraft::RaftStateMachine;
use openraft::{StorageError, StorageIOError, StoredMembership};
use crate::raft::types::{RaftConfig, Request, Response};

#[async_trait::async_trait]
impl RaftStateMachine<RaftConfig> for SqliteStateMachine {
    async fn applied_state(&mut self) -> Result<(openraft::LogId<RaftConfig>, StoredMembership<RaftConfig>), StorageError<RaftConfig>> {
        Ok((openraft::LogId::new(0, 0), StoredMembership::default()))
    }

    async fn apply(&mut self, entries: &[openraft::Entry<RaftConfig>]) -> Result<Vec<Response>, StorageError<RaftConfig>> {
        let mut results = Vec::with_capacity(entries.len());
        for entry in entries {
            match &entry.payload {
                openraft::EntryPayload::Normal(req) => {
                    match req.data.clone() {
                        Request::Write { sql } => {
                            let _ = self.apply_write(sql).await.map_err(|e| StorageIOError::read_state_machine(e))?;
                            results.push(Response { value: None });
                        }
                        Request::Read { sql } => {
                            let value = self.query_scalar(sql).await.map_err(|e| StorageIOError::read_state_machine(e))?;
                            results.push(Response { value: Some(value) });
                        }
                    }
                }
                _ => results.push(Response { value: None }),
            }
        }
        Ok(results)
    }

    async fn get_snapshot_builder(&mut self) -> Result<Self::SnapshotBuilder, StorageError<RaftConfig>> {
        Err(StorageError::IO(StorageIOError::read_state_machine(anyhow::anyhow!("snapshot not implemented"))))
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q read_is_linearizable`  
Expected: still FAIL, but state machine compile errors resolved.

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/src/raft/state_machine.rs
git commit -m "feat: implement raft state machine for sqlite"
```

---

### Task 4: RaftLogStorage 最小实现

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/store.rs`

**Step 1: Write the failing test**

Use `read_is_linearizable` test to drive.

**Step 2: Run test to verify it fails**

Run: `cargo test -q read_is_linearizable`  
Expected: FAIL with missing RaftLogStorage impl.

**Step 3: Write minimal implementation**

```rust
use openraft::{RaftLogStorage, RaftLogReader, StorageError, StorageIOError};
use crate::raft::types::RaftConfig;

#[async_trait::async_trait]
impl RaftLogReader<RaftConfig> for RaftStore {
    async fn try_get_log_entries(&mut self, _range: std::ops::Range<u64>) -> Result<Vec<openraft::Entry<RaftConfig>>, StorageError<RaftConfig>> {
        Ok(vec![])
    }

    async fn get_log_state(&mut self) -> Result<openraft::LogState<RaftConfig>, StorageError<RaftConfig>> {
        Ok(openraft::LogState::default())
    }
}

#[async_trait::async_trait]
impl RaftLogStorage<RaftConfig> for RaftStore {
    type LogReader = RaftStore;

    async fn get_log_reader(&mut self) -> Self::LogReader {
        RaftStore { db: self.db.clone() }
    }

    async fn append<I>(&mut self, _entries: I) -> Result<(), StorageError<RaftConfig>>
    where
        I: IntoIterator<Item = openraft::Entry<RaftConfig>> + Send,
    {
        Ok(())
    }

    async fn truncate(&mut self, _log_id: openraft::LogId<RaftConfig>) -> Result<(), StorageError<RaftConfig>> {
        Ok(())
    }

    async fn purge(&mut self, _log_id: openraft::LogId<RaftConfig>) -> Result<(), StorageError<RaftConfig>> {
        Ok(())
    }

    async fn save_committed(&mut self, _log_id: openraft::LogId<RaftConfig>) -> Result<(), StorageError<RaftConfig>> {
        Ok(())
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q read_is_linearizable`  
Expected: still FAIL, but storage compile errors resolved.

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/src/raft/store.rs
git commit -m "feat: add minimal raft log storage"
```

---

### Task 5: RaftNode 接入 OpenRaft 与 read_index

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/raft_node.rs`

**Step 1: Write the failing test**

Use `read_is_linearizable` test to drive.

**Step 2: Run test to verify it fails**

Run: `cargo test -q read_is_linearizable`  
Expected: FAIL with missing ensure_linearizable.

**Step 3: Write minimal implementation**

```rust
use openraft::{Config, Raft};
use crate::raft::types::RaftConfig;
use crate::raft::store::RaftStore;
use crate::raft::network::RaftNetworkImpl;

pub struct RaftNode {
    raft: Raft<RaftConfig>,
    // existing fields...
    force_read_index_error_for_test: bool,
}

impl RaftNode {
    async fn build_raft(node_id: u64, store: RaftStore, sm: SqliteStateMachine) -> Raft<RaftConfig> {
        let config = Config::build("check_program".to_string()).validate().unwrap();
        let network = RaftNetworkImpl::new();
        Raft::new(node_id, Arc::new(config), network, store, sm)
    }

    pub async fn ensure_linearizable(&self) -> Result<()> {
        if self.force_read_index_error_for_test {
            return Err(anyhow!("forced read_index error"));
        }
        self.raft.ensure_linearizable().await.map_err(|e| anyhow!(e.to_string()))
    }

    pub fn set_force_read_index_error_for_test(&mut self, enabled: bool) {
        self.force_read_index_error_for_test = enabled;
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q read_is_linearizable`  
Expected: still FAIL, but read_index path compiles.

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/src/raft/raft_node.rs
git commit -m "feat: wire openraft read_index in raft node"
```

---

### Task 6: Router 读路径接入 read_index + 回退

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/router.rs`

**Step 1: Write the failing test**

Use `read_is_linearizable` and `read_fallbacks_on_consistency_error` tests.

**Step 2: Run test to verify it fails**

Run: `cargo test -q read_is_linearizable`  
Expected: FAIL with missing Router::read.

**Step 3: Write minimal implementation**

```rust
pub async fn read(&self, sql: String) -> Result<String> {
    if let Some(raft_node) = &self.raft_node {
        let linearizable = raft_node.ensure_linearizable().await;
        match linearizable {
            Ok(_) => raft_node.query_scalar(sql).await,
            Err(e) => {
                if e.to_string().contains("leader") {
                    Err(e)
                } else {
                    raft_node.query_scalar(sql).await
                }
            }
        }
    } else if let Some(state_machine) = &self.state_machine {
        state_machine.query_scalar(sql).await
    } else {
        Ok("0".to_string())
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q read_is_linearizable`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/src/raft/router.rs
git commit -m "feat: add router read path with read_index fallback"
```

---

### Task 7: gRPC Execute 对 SELECT 走 Router::read

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/bin/server.rs`

**Step 1: Write the failing test**

Reuse `read_is_linearizable` test as validation, no新增测试。

**Step 2: Run test to verify it fails**

Run: `cargo test -q read_is_linearizable`  
Expected: FAIL until Execute 路径切换为读。

**Step 3: Write minimal implementation**

```rust
fn is_select(sql: &str) -> bool {
    let trimmed = sql.trim_start().to_ascii_uppercase();
    trimmed.starts_with("SELECT")
}

async fn execute(&self, sql: &str) -> anyhow::Result<usize> {
    if is_select(sql) {
        let _ = self.router.read(sql.to_string()).await?;
        Ok(0)
    } else {
        self.router.write(sql.to_string()).await
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q read_is_linearizable`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/src/bin/server.rs
git commit -m "feat: route select through linearizable read"
```

---

### Task 8: 统一验证与清理

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/raft_node.rs`
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/router.rs`
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/bin/server.rs`

**Step 1: Run tests**

Run: `cargo test -q read_is_linearizable read_fallbacks_on_consistency_error`  
Expected: PASS

**Step 2: Run lint/typecheck (if configured)**

Run: `cargo check`  
Expected: PASS
