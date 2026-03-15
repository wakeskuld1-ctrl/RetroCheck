# add_hub 启动 Raft gRPC 服务 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 解决 add_hub 在 gRPC 网络下阻塞的问题，确保新增节点 gRPC 服务启动并强制同端口约束。

**Architecture:** 在 `precheck_add` 中校验 `raft_addr`/`grpc_addr` 一致；在 `add_hub` 中启动 Raft gRPC 服务；测试用例统一改为动态同端口地址。

**Tech Stack:** Rust, tokio, tonic gRPC, openraft

---

### Task 1: 编写失败测试（RED）

**Files:**
- Modify: `D:\Rust\check_program\.worktrees\raft-grpc-readindex\tests\cluster_admin_add_hub.rs`

**Step 1: 新增“地址不一致”失败用例**

```rust
#[tokio::test]
async fn add_hub_rejects_mismatched_grpc_addr() {
    // setup leader + manager
    // build AddHubRequest where raft_addr != grpc_addr
    // expect reason_code == "INVALID_ARGUMENT"
}
```

**Step 2: 运行该用例，确认失败原因正确**

Run: `cargo test -q --test cluster_admin_add_hub add_hub_rejects_mismatched_grpc_addr -- --test-threads=1`
Expected: 失败且提示尚未校验一致性。

---

### Task 2: 实现生产逻辑修复（GREEN）

**Files:**
- Modify: `D:\Rust\check_program\.worktrees\raft-grpc-readindex\src\management\cluster_admin_service.rs`

**Step 1: precheck_add 中增加同端口校验**

- 规范化地址后比较；不一致则返回 INVALID_ARGUMENT。
- 写入“修改记录”注释（原因/目的/日期）。

**Step 2: add_hub 中启动 Raft gRPC 服务**

- 解析 `req.grpc_addr` 获取 `SocketAddr` 并绑定 `TcpListener`。
- 使用 `RaftServiceImpl` + `RaftServiceServer` 启动服务（`tokio::spawn`）。
- 错误处理返回 INVALID_ARGUMENT。
- 写入“修改记录”注释（原因/目的/日期）。

**Step 3: 运行单测确认通过**

Run: `cargo test -q --test cluster_admin_add_hub add_hub_rejects_mismatched_grpc_addr -- --test-threads=1`
Expected: PASS

---

### Task 3: 调整 add_hub 测试端口与一致性（GREEN）

**Files:**
- Modify: `D:\Rust\check_program\.worktrees\raft-grpc-readindex\tests\cluster_admin_add_hub.rs`

**Step 1: 增加动态端口辅助函数**

```rust
async fn allocate_local_addr() -> String {
    // bind 127.0.0.1:0 and return formatted addr
}
```

**Step 2: 所有 AddHubRequest 使用同端口地址**

- 替换所有静态 `raft_addr`/`grpc_addr` 为 `allocate_local_addr()` 结果。
- 每处改动写“修改记录”注释（原因/目的/日期）。

**Step 3: 运行目标用例**

Run: `cargo test -q --test cluster_admin_add_hub remove_hub_commit_auto_transfers_leader_and_succeeds -- --test-threads=1`
Expected: PASS

---

### Task 4: 全量回归 + 提交

**Step 1: 全量运行该测试文件**

Run: `cargo test -q --test cluster_admin_add_hub -- --test-threads=1`
Expected: PASS

**Step 2: 提交**

```bash
git add D:\Rust\check_program\.worktrees\raft-grpc-readindex\src\management\cluster_admin_service.rs \
        D:\Rust\check_program\.worktrees\raft-grpc-readindex\tests\cluster_admin_add_hub.rs

git commit -m "fix: start raft grpc on add_hub"
```

---

### Task 5: 更新任务日志

**Files:**
- Modify: `D:\Rust\check_program\.worktrees\raft-grpc-readindex\.trae\CHANGELOG_TASK.md`

**Step 1: 追加任务记录**

- 按模板追加本次修改内容、原因、遗留项、潜在问题。

**Step 2: （可选）提交日志**

- 若仓库流程要求，提交日志文件。
