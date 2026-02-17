# Raft/2PC 混沌补充用例 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 同时补齐 Raft 连续重启恢复一致性、2PC 提交窗口交替故障重放一致性、客户端超时重试幂等性三类用例。

**Architecture:** Rust 集成测试覆盖 Raft WAL/Snapshot 与幂等逻辑；verify.ps1 负责端到端故障注入与重放验证。新增逻辑严格遵循 TDD，且在每处改动旁添加“原因/目的/时间”备注，注释比例 ≥ 6:4。

**Tech Stack:** Rust (tokio, anyhow), PowerShell verify.ps1, SQLite

---

### Task 1: Raft 连续重启 WAL/Snapshot 一致性用例（Rust）

**Files:**
- Modify: `d:\Rust\check_program\.worktrees\raft-a2\tests\chaos_raft.rs`
- Modify: `d:\Rust\check_program\.worktrees\raft-a2\src\raft\raft_node.rs`（如需暴露恢复细节）
- Test: `d:\Rust\check_program\.worktrees\raft-a2\tests\chaos_raft.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn chaos_leader_restart_preserves_wal_snapshot_state() {
    let mut cluster = TestCluster::new(3).await.unwrap();
    cluster.write("CREATE TABLE IF NOT EXISTS t(x INT)".to_string()).await.unwrap();
    for i in 0..20 {
        cluster.write(format!("INSERT INTO t(x) VALUES ({})", i)).await.unwrap();
        if i % 3 == 0 {
            cluster.restart_leader().await.unwrap();
        }
    }
    let value = cluster.query_leader_scalar("SELECT COUNT(*) FROM t".to_string()).await.unwrap();
    assert_eq!(value, "20");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test chaos_raft -- chaos_leader_restart_preserves_wal_snapshot_state`  
Expected: FAIL（若恢复路径不完整或重复应用导致计数偏差）

**Step 3: Write minimal implementation**

- 若失败源自重复应用：在 TestCluster 维护已应用偏移，重启时仅回放增量。
- 若失败源自数据目录复用：确保 base_dir 唯一化或重启复用同目录但不重复回放。

**Step 4: Run test to verify it passes**

Run: `cargo test --test chaos_raft -- chaos_leader_restart_preserves_wal_snapshot_state`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:\Rust\check_program\.worktrees\raft-a2\tests\chaos_raft.rs d:\Rust\check_program\.worktrees\raft-a2\src\raft\raft_node.rs
git commit -m "test: add raft restart wal/snapshot consistency case"
```

---

### Task 2: 2PC 提交窗口交替故障重放一致性（verify.ps1）

**Files:**
- Modify: `d:\Rust\check_program\.worktrees\raft-a2\verify.ps1`

**Step 1: Write the failing test (scenario)**

新增场景 `chaos_prepare_commit_alternate`，逻辑为：
- 第1轮：pause-before-commit，kill slave1，恢复后 verify-only
- 第2轮：pause-before-commit，kill slave2，恢复后 verify-only
- 循环 N 轮

**Step 2: Run scenario to verify it fails**

Run: `.\verify.ps1 -Scenario chaos_prepare_commit_alternate -PauseBeforeCommitMs 4000`  
Expected: FAIL（当前场景不存在）

**Step 3: Write minimal implementation**

- 新增 `Run-ScenarioChaosPrepareCommitAlternate`
- 复用 `Wait-ForClientPauseLog` 与 `Run-ClientVerifyOnly`

**Step 4: Run scenario to verify it passes**

Run: `.\verify.ps1 -Scenario chaos_prepare_commit_alternate -PauseBeforeCommitMs 4000`  
Expected: PASS（一致性校验通过）

**Step 5: Commit**

```bash
git add d:\Rust\check_program\.worktrees\raft-a2\verify.ps1
git commit -m "test: add alternating 2pc commit-window chaos scenario"
```

---

### Task 3: 客户端超时重试幂等性（Rust + 脚本）

**Files:**
- Modify: `d:\Rust\check_program\.worktrees\raft-a2\src\coordinator.rs`
- Modify: `d:\Rust\check_program\.worktrees\raft-a2\src\bin\client.rs`
- Modify: `d:\Rust\check_program\.worktrees\raft-a2\verify.ps1`
- Test: `d:\Rust\check_program\.worktrees\raft-a2\tests\2pc_idempotency.rs`（如无可复用测试文件）

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn idempotent_retry_does_not_double_apply() {
    // 构造固定 tx_id，重复调用提交路径
    // 断言版本/行数只增加一次
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test 2pc_idempotency`  
Expected: FAIL（重复提交导致二次写入）

**Step 3: Write minimal implementation**

- 在 coordinator 侧引入“已提交 tx_id 集合”（内存级即可），重复 tx_id 直接返回 OK。
- client 增加 `--fixed-tx-id` 或 `--retry-same-tx` 参数以触发重复提交。
- verify.ps1 新增 `chaos_retry_idempotent` 场景调用 client。

**Step 4: Run test to verify it passes**

Run: `cargo test --test 2pc_idempotency`  
Expected: PASS

**Step 5: Commit**

```bash
git add d:\Rust\check_program\.worktrees\raft-a2\src\coordinator.rs d:\Rust\check_program\.worktrees\raft-a2\src\bin\client.rs d:\Rust\check_program\.worktrees\raft-a2\tests\2pc_idempotency.rs d:\Rust\check_program\.worktrees\raft-a2\verify.ps1
git commit -m "test: add 2pc idempotent retry protection"
```
