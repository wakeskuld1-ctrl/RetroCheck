# 集群管理 add_hub gRPC 回归测试 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 修复 `remove_hub_commit_auto_transfers_leader_and_succeeds` 编译错误与 gRPC 回归，确保测试在同端口约束下通过。

**Architecture:** 在测试内显式启动 leader/node2 的 Raft gRPC server，并确保 `raft_addr`/`grpc_addr` 同端口；修复重复 `#[tokio::test]` 与注释格式问题。

**Tech Stack:** Rust, tokio, tonic gRPC, openraft

---

### Task 1: 复现失败并锁定根因（RED）

**Files:**
- Modify: `D:\Rust\check_program\.worktrees\raft-grpc-readindex\tests\cluster_admin_add_hub.rs`

**Step 1: 运行失败用例（确认 RED）**

Run: `cargo test -q --test cluster_admin_add_hub -- --test-threads=1`
Expected: 编译失败，提示 `second test attribute is supplied`。

**Step 2: 记录根因证据**

Action: 检查 `remove_hub_commit_auto_transfers_leader_and_succeeds` 前是否存在重复 `#[tokio::test]`。
Expected: 发现连续两个 `#[tokio::test]` 标注。

**Step 3: 提交记录（可选）**

Action: 将根因记录在任务说明中（无需代码变更）。

### Task 2: 修复测试并保持 gRPC 同端口约束（GREEN）

**Files:**
- Modify: `D:\Rust\check_program\.worktrees\raft-grpc-readindex\tests\cluster_admin_add_hub.rs`
- Modify: `D:\Rust\check_program\.worktrees\raft-grpc-readindex\src\management\cluster_admin_service.rs`

**Step 1: 修复重复标注与注释格式**

Action:
- 删除多余 `#[tokio::test]`。
- 修正新增/修改区域的“修改记录”注释为 Markdown 格式，包含原因/目的/日期，并追加记录而非覆盖。

**Step 2: 确认 gRPC 端口一致**

Action:
- 确保 `AddHubRequest` 的 `raft_addr` 与 `grpc_addr` 使用同一 `node2_addr`。
- `ChangeMembers::SetNodes` 使用同一端口的地址映射。

**Step 3: 运行用例（验证 GREEN）**

Run: `cargo test -q --test cluster_admin_add_hub -- --test-threads=1`
Expected: 用例通过，无编译错误。

**Step 4: Commit**

```bash
git add D:\Rust\check_program\.worktrees\raft-grpc-readindex\tests\cluster_admin_add_hub.rs \
        D:\Rust\check_program\.worktrees\raft-grpc-readindex\src\management\cluster_admin_service.rs

git commit -m "test: fix add_hub grpc remove_hub transfer"
```

---

### Task 3: 更新任务日志

**Files:**
- Modify: `D:\Rust\check_program\.worktrees\raft-grpc-readindex\.trae\CHANGELOG_TASK.md`

**Step 1: 写入任务记录**

Action: 记录本次修复的范围与测试结果。

**Step 2: Commit（如需要）**

Action: 若仓库要求，可单独提交任务日志（否则保留未提交）。
