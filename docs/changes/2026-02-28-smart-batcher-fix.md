# Smart Batcher 修复与测试重构变更文档

**日期**: 2026-02-28
**作者**: Trae AI (Pair Programmer)
**状态**: 已完成

## 1. 变更背景
在审计 GPT 生成的代码时，发现 `Smart Batcher` (位于 `src/raft/router.rs`) 存在严重的架构缺陷：它直接持有 `SqliteStateMachine` 并绕过 Raft 共识协议直接执行写操作。这违反了 "写入聚合规范" 和 "分布式一致性" 的核心原则。

此外，为了验证修复后的逻辑，`tests/raft_read.rs` 中的测试用例需要进行相应的调整，暴露出测试初始化逻辑重复且脆弱的问题。

## 2. 变更内容

### 2.1 核心修复：Smart Batcher 集成 Raft
我们重构了 `BatchWriter` 及其相关组件，确保所有批量写入请求都通过 Raft 协议提交。

*   **`src/raft/types.rs`**:
    *   新增 `Request::WriteBatch { sqls: Vec<String> }` 变体。
    *   目的：支持将多条 SQL 作为一个原子性的 Raft 日志条目进行复制。

*   **`src/raft/raft_node.rs`**:
    *   新增 `apply_sql_batch(sqls: Vec<String>) -> Result<usize>` 方法。
    *   目的：作为 `BatchWriter` 提交请求的入口，内部调用 `raft.client_write(Request::WriteBatch { ... })`。

*   **`src/raft/router.rs`**:
    *   修改 `BatchWriter` 结构体，将 `SqliteStateMachine` 替换为 `RaftNode`。
    *   修改 `BatchWriter::new`，在聚合满足条件后调用 `raft_node.apply_sql_batch`。
    *   更新 `Router::new_local_leader_with_batch` 构造函数签名，要求传入 `RaftNode`。

*   **`src/raft/state_machine.rs`**:
    *   更新 `apply` 方法，增加对 `Request::WriteBatch` 的处理分支。
    *   重构结果映射逻辑：将 `entry_map` 从单纯的索引映射改为范围映射 (`Range<usize>`)，以支持一个 Raft Log 对应多条执行结果的聚合。
    *   **当前策略**：批量请求的响应值为所有 SQL 受影响行数的总和。

### 2.2 测试重构：统一初始化逻辑
针对 `tests/raft_read.rs` 中大量重复的测试设置代码，我们进行了提取和优化。

*   **`tests/raft_read.rs`**:
    *   提取 `setup_test_node() -> (TempDir, RaftNode)` 辅助函数。
    *   在辅助函数中统一处理：
        1.  临时目录创建
        2.  `RaftNode` 启动
        3.  **Raft 集群初始化** (`raft.initialize`)
        4.  **Leader 选举等待** (轮询 `metrics.state`)
        5.  基础表结构 (`CREATE TABLE t`) 的创建
    *   修复了 `batch_size_threshold_flushes_once` 和 `batch_queue_overflow_returns_error` 等测试因 Leader 状态未就绪而失败的问题。

## 3. 验证结果
执行 `cargo test --test raft_read`，结果如下：
```text
running 10 tests
test non_leader_write_returns_error ... ok
test batch_write_is_atomic_on_error ... ok
test batch_size_threshold_flushes_once ... ok
test batch_wait_timeout_returns_error ... ok
test read_get_version_uses_state_machine ... ok
test snapshot_created_when_time_exceeds_window ... ok
test snapshot_updates_meta_after_creation ... ok
test wal_rotates_after_snapshot ... ok
test batch_timeout_cancels_and_skips_write ... ok
test batch_queue_overflow_returns_error ... ok

test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.22s
```
所有相关测试均通过，证明 Smart Batcher 已正确集成 Raft，且测试环境稳定。

## 4. 潜在风险与后续建议 (Potential Issues)
1.  **Batch 响应粒度**：目前 `WriteBatch` 的响应是聚合后的受影响行数。如果业务需要知道批次中每一条 SQL 的具体执行结果（如 ID 生成），当前的 `Response` 结构（`Option<String>`）可能不够用。
    *   **建议**: 将 `Response` 升级为支持列表或更复杂的结构结果。
2.  **测试中的忙等待**：`setup_test_node` 使用 `sleep(50ms)` 轮询 Leader 状态。在极高负载或慢速机器上可能导致测试变慢。
    *   **建议**: 引入事件通知机制或使用更高级的测试框架工具来等待状态变更。
3.  **大事务限制**：`WriteBatch` 将所有 SQL 放在一个 Raft Log 中。如果批次过大（超过 Raft 日志大小限制或网络包限制），可能会失败。
    *   **建议**: 在 `BatchWriter` 中增加对序列化后大小的检查，超过阈值提前切分。

## 5. 架构规范符合度检查
- [x] **Hub-Hub 通信**: 保持 gRPC 接口（Router 转发逻辑保留）。
- [x] **写入聚合**: Smart Batcher 逻辑保留并增强了 Raft 一致性。
- [x] **代码变更规范**: 关键变更点已添加中文注释，包含原因、目的和时间。
