# 代码审计报告：Raft Router 与批处理实现（复核更新版）

## 摘要

本报告是对之前审计结果的复核与更新。经仔细查阅最新提交的代码（`src/raft/raft_node.rs`, `src/raft/router.rs`, `src/raft/network/grpc.rs`, `src/raft/store.rs`），确认之前指出的重大架构缺陷已在当前版本中得到修复。系统已从单机原型进化为集成 `openraft` 的分布式共识系统。

---

## 缺陷修复状态复核

### 1. 虚假的 RaftNode (The Hollow RaftNode) -> **[已修复]**

**原指控：** `RaftNode` 只是 `SqliteStateMachine` 的包装，无共识逻辑。
**当前状态：**
- `RaftNode` 现已集成 `openraft::Raft<TypeConfig>`。
- `apply_sql` 和 `apply_sql_batch` 方法通过 `self.raft.client_write(req)` 提交提案，走完整的 Raft 共识流程（Log -> Consensus -> Apply）。
- `RaftStore` 实现了 `RaftLogStorage` 和 `RaftLogReader`，使用 `sled` 的 `log_tree` 和 `meta_tree` 进行持久化。

### 2. 致命的批处理旁路 (The Deadly Batch Bypass) -> **[已修复]**

**原指控：** `BatchWriter` 直接操作状态机，绕过 Raft。
**当前状态：**
- 引入了 `BatchExecutor` trait，`RaftNode` 实现了该接口。
- `BatchWriter` 现在持有 `Arc<dyn BatchExecutor>`，其内部逻辑是将批量 SQL 转发给 `RaftNode::apply_sql_batch`。
- 数据流向修正为：`Client -> BatchWriter -> RaftNode (Propose Batch) -> Consensus -> StateMachine (Apply Batch)`。

### 3. 互斥的路由逻辑 (The Mutually Exclusive Router) -> **[已修复]**

**原指控：** 开启 Batch 导致 Raft 失效。
**当前状态：**
- 路由逻辑优化为优先级链：`BatchWriter` > `RaftNode` > `StateMachine`。
- 由于 `BatchWriter` 底层已接入 Raft，因此“走 Batch 路径”等同于“走高效的 Raft 批量提交路径”，不再是互斥关系，而是优化关系。

### 4. 掩耳盗铃的测试 (The Fabricated Tests) -> **[已修复]**

**原指控：** 测试代码手动重放 SQL 模拟同步。
**当前状态：**
- `TestCluster` 移除了手动重放逻辑。
- 新增 `stop_node` 和 `start_node` 方法，通过 Raft 协议自身的快照安装（InstallSnapshot）和日志复制（AppendEntries）来恢复节点状态。
- `RaftStore` 的持久化实现支持了节点重启后的状态恢复。

### 5. 网络层缺失 -> **[已修复]**

**原指控：** 缺乏 gRPC 网络实现。
**当前状态：**
- `src/raft/network/grpc.rs` 已恢复，提供了 `GrpcNetworkFactory` 和 `GrpcNetworkConnection`。
- 实现了 `RaftNetwork` trait，支持跨节点 RPC 通信。

---

## 遗留问题与建议 (Pending Issues)

尽管核心架构已修正，但以下细节问题仍需关注：

### 1. 错误吞咽 (Error Swallowing) -> **[仍存在]**

**位置：** `src/raft/router.rs:285`
```rust
let resp = res.map_err(|_| anyhow!("Leader execute failed"))?;
```
**问题：** 依然丢弃了 gRPC 调用的具体错误信息（如超时、权限拒绝等），不利于排查。
**建议：** 保留原始错误信息，或将其封装为自定义错误类型返回。

### 2. 非 Leader 读一致性 (Read Consistency) -> **[待优化]**

**位置：** `src/raft/router.rs:207`
```rust
pub async fn query_scalar(&self, sql: String) -> Result<String> {
    // 本地查询可以直接查 StateMachine (如果是 Read Index 则需要走 Raft)
    // 这里简化为直接查 StateMachine
    self.state_machine.query_scalar(sql).await
}
```
**问题：** 目前的读操作直接查询本地状态机，如果在发生网络分区时，旧 Leader（或 Follower）可能会返回陈旧数据（Stale Read）。
**建议：** 实现 `ReadIndex` 或 `LeaseRead` 机制，确保线性一致性读取。

v
### 4. ReadIndex 说明 (ReadIndex Notes)

**目的：** ReadIndex 用于线性一致读，避免 Follower 或旧 Leader 返回陈旧数据。

**基本流程：**
1. 客户端或 Router 向 Leader 请求 ReadIndex。
2. Leader 在多数派确认其任期有效后返回 read index。
3. 本地节点等待日志应用到该 index，再执行读取，保证读与已提交日志一致。

**与当前架构的结合点：**
- Router 可在非 Leader 场景转发到 Leader 获取 ReadIndex。
- RaftNode 需提供 ReadIndex 接口，并在状态机应用到指定 index 后放行读请求。

---

## 结论

GPT-5.2 在最新版本中已成功修复了所有关于分布式共识的核心指控。当前代码架构正确，Raft 协议集成完整，批处理机制设计合理。

**后续重点：**
1.  完善错误处理机制。
2.  实现强一致性读取（ReadIndex）。
