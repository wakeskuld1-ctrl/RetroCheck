# ReadIndex Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement linearizable reads via ReadIndex in RaftNode and route Router reads through it.

**Architecture:** Add a ReadIndex entry point in RaftNode that requests a read index from Raft core and waits for local apply before allowing reads. Router uses this for leader reads; followers still forward to leader.

**Tech Stack:** Rust, tokio, openraft (existing), gRPC (tonic), sqlite state machine

---

### Task 1: Add failing ReadIndex test for leader reads

**Files:**
- Modify: `d:\Rust\check_program\tests\router.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn router_leader_read_requires_readindex() {
    let temp_dir = TempDir::new().unwrap();
    let leader_db = temp_dir.path().join("leader_readindex.db");
    let leader_router = Router::new_local_leader(leader_db.to_string_lossy().to_string()).unwrap();
    let _ = leader_router
        .write("CREATE TABLE IF NOT EXISTS t(x INT)".to_string())
        .await
        .unwrap();
    let _ = leader_router
        .write("INSERT INTO t VALUES (1)".to_string())
        .await
        .unwrap();
    let version = leader_router.get_version("t".to_string()).await.unwrap();
    assert!(version >= 2);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test router`
Expected: FAIL with missing ReadIndex path (assertion or placeholder error)

**Step 3: Commit**

```bash
git add tests/router.rs
git commit -m "test: add readindex expectation for leader reads"
```

---

### Task 2: Add ReadIndex API to RaftNode and Router

**Files:**
- Modify: `d:\Rust\check_program\src\raft\raft_node.rs`
- Modify: `d:\Rust\check_program\src\raft\router.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn raftnode_readindex_returns_timeout_on_missing_core() {
    let temp_dir = TempDir::new().unwrap();
    let leader_db = temp_dir.path().join("leader_readindex_timeout.db");
    let raft_node = RaftNode::start_local(1, leader_db.to_string_lossy().to_string())
        .await
        .unwrap();
    let err = raft_node.read_index(Duration::from_millis(10)).await.unwrap_err();
    assert!(err.to_string().contains("read index not available"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test router`
Expected: FAIL with function not found or unimplemented

**Step 3: Write minimal implementation**

```rust
impl RaftNode {
    pub async fn read_index(&self, _timeout: Duration) -> Result<()> {
        Err(anyhow!("read index not available"))
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test router`
Expected: PASS (test now checks explicit error)

**Step 5: Commit**

```bash
git add src/raft/raft_node.rs tests/router.rs
git commit -m "feat: add raftnode read_index stub and tests"
```

---

### Task 3: Wire Router leader read through ReadIndex

**Files:**
- Modify: `d:\Rust\check_program\src\raft\router.rs`
- Modify: `d:\Rust\check_program\tests\router.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn router_leader_read_fails_when_readindex_unavailable() {
    let temp_dir = TempDir::new().unwrap();
    let leader_db = temp_dir.path().join("leader_readindex_unavailable.db");
    let raft_node = RaftNode::start_local(1, leader_db.to_string_lossy().to_string())
        .await
        .unwrap();
    let router = Router::new_with_raft(raft_node);
    let err = router.get_version("t".to_string()).await.unwrap_err();
    assert!(err.to_string().contains("read index not available"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test router`
Expected: FAIL because Router does not call read_index

**Step 3: Write minimal implementation**

```rust
if let Some(raft_node) = &self.raft_node {
    raft_node.read_index(Duration::from_millis(2000)).await?;
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test router`
Expected: PASS

**Step 5: Commit**

```bash
git add src/raft/router.rs tests/router.rs
git commit -m "feat: gate leader reads on read_index"
```

---

### Task 4: Tighten error messages and timeouts

**Files:**
- Modify: `d:\Rust\check_program\src\raft\router.rs`
- Modify: `d:\Rust\check_program\tests\router.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn router_leader_read_propagates_readindex_timeout() {
    let temp_dir = TempDir::new().unwrap();
    let leader_db = temp_dir.path().join("leader_readindex_timeout_msg.db");
    let raft_node = RaftNode::start_local(1, leader_db.to_string_lossy().to_string())
        .await
        .unwrap();
    let router = Router::new_with_raft(raft_node);
    let err = router.get_version("t".to_string()).await.unwrap_err();
    assert!(err.to_string().contains("read index"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test router`
Expected: FAIL if error message is too generic

**Step 3: Write minimal implementation**

```rust
raft_node
    .read_index(Duration::from_millis(2000))
    .await
    .map_err(|e| anyhow!("ReadIndex failed: {}", e))?;
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test router`
Expected: PASS

**Step 5: Commit**

```bash
git add src/raft/router.rs tests/router.rs
git commit -m "feat: improve readindex error propagation"
```

---

### Task 5: Verification

**Files:**
- None

**Step 1: Run unit tests**

Run: `cargo test --test router`
Expected: PASS

**Step 2: Run formatting**

Run: `cargo fmt --all -- --check`
Expected: PASS

**Step 3: Run lint**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS

**Step 4: Run type check**

Run: `cargo check`
Expected: PASS

**Step 5: Commit**

```bash
git add -A
git commit -m "chore: verify readindex gating"
```
