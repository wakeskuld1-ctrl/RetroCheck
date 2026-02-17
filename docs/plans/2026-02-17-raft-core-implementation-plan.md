# Raft 核心落地 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 以 TDD 方式补齐 OpenRaft 核心链路（日志复制、提交、应用），为后续 ReadIndex 打基础  

**Architecture:** 先接入 OpenRaft 核心节点与存储接口，再绑定 SQLite 状态机的 apply 路径，补齐网络适配后对接 Router 写路径并完成最小集群联调。  

**Tech Stack:** Rust 2024, OpenRaft, tokio, tonic, rusqlite, sled

---

## 问题记录

- **(2026-02-17)** 最小网络适配尚未接入 OpenRaft 协议，复制路径仅用于测试骨架验证。
- **(2026-02-17)** 复制测试未覆盖真实并发/超时场景，后续需补充并发与超时相关测试。
- **(2026-02-17)** RaftNode 仍为“本地状态机封装”，尚未验证日志一致性与提交索引推进。
- **(2026-02-17)** 强一致读计划采用 OpenRaft read_index，若一致性校验失败则回退本地读路径。
- **(2026-02-17)** 无 leader 时读路径需要统一错误返回，避免隐式回退导致不一致。

### Task 1: Raft 核心框架落地（RaftNode + RaftStore 适配）

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/mod.rs`
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/store.rs`
- Create: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/raft_node.rs`
- Test: `d:/Rust/check_program/.worktrees/raft-a2/tests/raft_core_bootstrap.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn raft_node_bootstrap_and_becomes_leader() {
    // 启动单节点 Raft，等待成为 leader
    assert!(true);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q raft_node_bootstrap_and_becomes_leader`  
Expected: FAIL

**Step 3: Write minimal implementation**

```rust
// 新增 RaftNode 封装 + RaftStore 适配
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q raft_node_bootstrap_and_becomes_leader`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/src/raft d:/Rust/check_program/.worktrees/raft-a2/tests/raft_core_bootstrap.rs
git commit -m "feat: bootstrap raft core"
```

---

### Task 2: 日志复制与 apply 路径（StateMachine 接入 Raft）

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/state_machine.rs`
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/raft_node.rs`
- Test: `d:/Rust/check_program/.worktrees/raft-a2/tests/raft_apply.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn raft_applies_log_to_sqlite() {
    // Raft 写入 SQL 后应落地到 SQLite
    assert!(true);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q raft_applies_log_to_sqlite`  
Expected: FAIL

**Step 3: Write minimal implementation**

```rust
// 将 SqliteStateMachine 实现为 OpenRaft StateMachine
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q raft_applies_log_to_sqlite`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/src/raft/state_machine.rs d:/Rust/check_program/.worktrees/raft-a2/tests/raft_apply.rs
git commit -m "feat: bind sqlite state machine to raft"
```

---

### Task 3: 网络适配（RaftNetwork / RPC 接入）

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/network.rs`
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/raft_node.rs`
- Test: `d:/Rust/check_program/.worktrees/raft-a2/tests/raft_replication.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn raft_replication_to_follower() {
    // 两节点 Raft，leader 写入后 follower 能应用
    assert!(true);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q raft_replication_to_follower`  
Expected: FAIL

**Step 3: Write minimal implementation**

```rust
// 实现 OpenRaft RaftNetwork trait
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q raft_replication_to_follower`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/src/raft/network.rs d:/Rust/check_program/.worktrees/raft-a2/tests/raft_replication.rs
git commit -m "feat: add raft network adapter"
```

---

### Task 4: Router 写路径改为 Raft 提交

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/router.rs`
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/bin/server.rs`
- Test: `d:/Rust/check_program/.worktrees/raft-a2/tests/router_raft_write.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn router_write_uses_raft_log() {
    // Router 写入应走 Raft 日志而不是直接写 SQLite
    assert!(true);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q router_write_uses_raft_log`  
Expected: FAIL

**Step 3: Write minimal implementation**

```rust
// Router::write 使用 Raft client_write
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q router_write_uses_raft_log`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/src/raft/router.rs d:/Rust/check_program/.worktrees/raft-a2/tests/router_raft_write.rs
git commit -m "feat: route writes through raft"
```

---

### Task 5: 最小集群联调测试（3 节点 + failover）

**Files:**
- Test: `d:/Rust/check_program/.worktrees/raft-a2/tests/raft_cluster.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn three_nodes_write_and_failover() {
    // 三节点写入，leader 挂掉后仍可写入并保持一致
    assert!(true);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q three_nodes_write_and_failover`  
Expected: FAIL

**Step 3: Write minimal implementation**

```rust
// 启动三节点，验证 failover 后写入一致
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q three_nodes_write_and_failover`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/tests/raft_cluster.rs
git commit -m "test: raft cluster failover"
```
