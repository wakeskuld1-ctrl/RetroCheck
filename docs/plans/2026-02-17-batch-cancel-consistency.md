# Batch Cancel & Consistency Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 在批处理写入中实现“超时即取消”、非 Leader 明确报错，并补齐一致性与并发测试。

**Architecture:** Router 侧批处理队列新增取消感知与超时取消机制；非 Leader 写入直接返回错误；Coordinator 侧历史回放窗口可配置并增强错误可观测性。

**Tech Stack:** Rust 2024, tokio, rusqlite, anyhow, tonic

---

### Task 1: 批处理超时取消与不落库测试

**Files:**
- Test: `d:/Rust/check_program/.worktrees/raft-a2/tests/raft_read.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn batch_timeout_cancels_and_skips_write() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");
    let sm = check_program::raft::state_machine::SqliteStateMachine::new(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap();
    sm.apply_write("CREATE TABLE t(x INT)".to_string()).await.unwrap();

    let router = check_program::raft::router::Router::new_local_leader_with_batch(
        db_path.to_string_lossy().to_string(),
        check_program::raft::router::BatchConfig {
            max_batch_size: 10,
            max_delay_ms: 200,
            max_queue_size: 10,
            max_wait_ms: 1,
        },
    )
    .unwrap();

    let res = router.write("INSERT INTO t(x) VALUES (1)".to_string()).await;
    assert!(res.is_err());

    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let count = sm.query_scalar("SELECT COUNT(*) FROM t".to_string()).await.unwrap();
    assert_eq!(count, "0");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q batch_timeout_cancels_and_skips_write`  
Expected: FAIL with count == "1"

**Step 3: Write minimal implementation**

```rust
// src/raft/router.rs: add cancel flag per batch item and skip canceled items.
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q batch_timeout_cancels_and_skips_write`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/tests/raft_read.rs d:/Rust/check_program/.worktrees/raft-a2/src/raft/router.rs
git commit -m "test: cancel batch on timeout"
```

---

### Task 2: 非 Leader 写入明确报错测试

**Files:**
- Test: `d:/Rust/check_program/.worktrees/raft-a2/tests/raft_read.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn non_leader_write_returns_error() {
    let router = check_program::raft::router::Router::new_for_test(false);
    let res = router.write("INSERT INTO t(x) VALUES (1)".to_string()).await;
    assert!(res.is_err());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q non_leader_write_returns_error`  
Expected: FAIL because write returns Ok

**Step 3: Write minimal implementation**

```rust
// src/raft/router.rs: return Err(anyhow!("Not leader")) when is_leader == false
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q non_leader_write_returns_error`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/tests/raft_read.rs d:/Rust/check_program/.worktrees/raft-a2/src/raft/router.rs
git commit -m "feat: return error on non-leader writes"
```

---

### Task 3: 回放历史窗口可配置与错误可观测性

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/coordinator.rs`
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/config.rs`
- Test: `d:/Rust/check_program/.worktrees/raft-a2/tests/coord_replay.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn committed_history_respects_limit() {
    let mut cfg = check_program::config::ClusterConfig::default();
    cfg.committed_history_limit = 2;
    let mut coord = check_program::coordinator::MultiDbCoordinator::new(cfg).await.unwrap();
    coord.record_committed_tx("A", &[]);
    coord.record_committed_tx("B", &[]);
    coord.record_committed_tx("C", &[]);
    assert_eq!(coord.committed_history_len(), 2);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q committed_history_respects_limit`  
Expected: FAIL missing config / helper methods

**Step 3: Write minimal implementation**

```rust
// config.rs: add committed_history_limit with default 32
// coordinator.rs: use config limit instead of hard-coded 32
// coordinator.rs: expose helper for test only (feature-gated if needed)
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q committed_history_respects_limit`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/src/config.rs d:/Rust/check_program/.worktrees/raft-a2/src/coordinator.rs d:/Rust/check_program/.worktrees/raft-a2/tests/coord_replay.rs
git commit -m "feat: configurable committed history limit"
```

---

### Task 4: Clippy 告警修复

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/coordinator.rs`
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/bin/server.rs`

**Step 1: Write the failing test**

```bash
cargo clippy -q
```

**Step 2: Run to verify it fails**

Expected: clippy warnings about collapsible if / doc indentation

**Step 3: Write minimal implementation**

```rust
// coordinator.rs: collapse nested if for pause_before_commit_ms
// server.rs: fix doc comment indentation
```

**Step 4: Run clippy to verify it passes**

Run: `cargo clippy -q`  
Expected: no new warnings

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/src/coordinator.rs d:/Rust/check_program/.worktrees/raft-a2/src/bin/server.rs
git commit -m "chore: resolve clippy warnings"
```

---

### Task 5: 全量验证

**Files:**
- Test: `d:/Rust/check_program/.worktrees/raft-a2/tests/raft_read.rs`

**Step 1: Run verification**

Run: `cargo test -q batch_timeout_cancels_and_skips_write`  
Run: `cargo test -q non_leader_write_returns_error`  
Run: `cargo test -q batch_queue_overflow_returns_error`  
Run: `cargo test -q batch_wait_timeout_returns_error`  
Run: `cargo test -q batch_size_threshold_flushes_once`  
Run: `cargo check -q`  
Run: `cargo clippy -q`

**Step 2: Commit**

```bash
git status
git commit -m "test: batch consistency and safeguards" || true
```
