# Raft gRPC 成功链路最小日志复制测试 设计

**日期**: 2026-03-15  
**目标**: 通过集成测试验证两节点在 gRPC 网络下的最小日志复制与状态机应用闭环。

---

## 目标与范围
- **目标**: 在不修改生产代码的前提下，验证 leader 写入 1 条 SQL 后，follower 可读到结果。
- **范围内**: gRPC 网络客户端、RaftService 服务端、OpenRaft 的 add_learner/change_membership 流程。
- **范围外**: 生产启动路径切换（`start_grpc` 等）、完整快照传输、多节点容错。

## 架构方案
测试内自建 Raft 最小初始化逻辑：
- 使用 `RaftStore` + `SqliteStateMachine` + `Config` 创建 `Raft<TypeConfig>`。
- 网络层使用 `RaftGrpcNetworkFactory`，gRPC 服务端由测试内实现的 `RaftService` 代理到 `Raft`。
- 两个节点分别启动 gRPC 服务端（临时端口）。

## 数据流
1. 启动 node1/node2 的 gRPC 服务端。
2. node1 `initialize({1})`。
3. node1 `add_learner(2, BasicNode(addr2), true)`。
4. node1 `change_membership({1,2}, true)`。
5. node1 `client_write(Request::Write { sql })` 写入 1~2 条 SQL。
6. 轮询 node2 的 `SqliteStateMachine::query_scalar`，验证复制成功。

## 错误处理与稳定性
- 使用临时目录，避免并发冲突。
- gRPC 服务启动与连接采用短重试，降低竞态。
- 复制完成使用轮询 + 超时避免测试挂起。

## 测试计划
新增 `tests/raft_grpc_replication_minimal.rs`：
- 启动 2 个节点 + gRPC 服务端。
- 完成成员变更与最小写入。
- 断言 follower 可读到写入结果。

---

## 变更约束
遵循现有约束：
- 代码旁写 Markdown 修改记录（原因/目的/日期）。
- 备注与代码比例 ≥ 6:4。
- 先写失败测试再实现。
