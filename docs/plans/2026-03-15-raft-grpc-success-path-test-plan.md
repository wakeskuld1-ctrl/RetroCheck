# Raft gRPC 成功链路最小日志复制测试 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 新增一条两节点 gRPC 日志复制的最小成功链路测试。

**Architecture:** 测试内自建 Raft 初始化逻辑，使用 RaftGrpcNetworkFactory，测试内实现 RaftService 代理，启动两个 gRPC 服务端后完成成员变更与写入验证。

**Tech Stack:** Rust 2024, OpenRaft 0.9, tonic gRPC, bincode/serde, tokio

---

### Task 1: 新增最小 gRPC 复制成功链路测试

**Files:**
- Create: `tests/raft_grpc_replication_minimal.rs`

**Step 1: Write the failing test**

```rust
// tests/raft_grpc_replication_minimal.rs
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grpc_replication_minimal_success_path() {
    // TODO: 构造两个 Raft 节点并启动 gRPC 服务端
    // TODO: 初始化集群、添加 learner、变更成员
    // TODO: 写入一条 SQL 并在 follower 查询验证
    assert!(false, "test not implemented");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q --test raft_grpc_replication_minimal`  
Expected: FAIL（断言触发或未实现）

**Step 3: Write minimal implementation**

实现测试内 helper：
- 启动 gRPC 服务端并返回地址。
- 复刻最小 Raft 初始化逻辑（RaftStore + SqliteStateMachine + Config + RaftGrpcNetworkFactory）。
- leader 写入 1~2 条 SQL，follower 轮询查询结果。

**Step 4: Run test to verify it passes**

Run: `cargo test -q --test raft_grpc_replication_minimal`  
Expected: PASS

**Step 5: Commit**

```bash
git add tests/raft_grpc_replication_minimal.rs
git commit -m "test: add grpc replication minimal success path"
```
