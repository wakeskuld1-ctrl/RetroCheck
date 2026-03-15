# 集群管理 add_hub gRPC 回归测试设计

**日期:** 2026-03-15  
**目标:** 修复 `remove_hub_commit_auto_transfers_leader_and_succeeds` 在 gRPC 网络路径下的回归，使测试用例稳定通过并满足“raft_addr 与 grpc_addr 同端口”约束。

## 背景
- 近期将 `add_hub` 迁移为 gRPC 网络路径后，该用例仍沿用内存网络启动，导致 leader transfer 阶段出现 `NOT_LEADER` 与超时。
- 当前测试文件还存在重复 `#[tokio::test]` 标注，引发编译错误。

## 设计范围
- 仅调整 `tests/cluster_admin_add_hub.rs` 的该用例与测试内 gRPC 启动辅助逻辑。
- 维持 `ClusterAdminService` 现有语义，仅补充测试读取节点句柄的入口（已存在）。
- 不引入额外生产逻辑改动，不扩大测试覆盖范围。

## 关键约束
- `raft_addr` 与 `grpc_addr` **必须完全一致（同一地址、同一端口）**。
- 所有 gRPC 服务共用同一端口。
- 仍保持最小改动、最小行为变更。

## 设计要点
### 组件
- `RaftNode::start_grpc`：用于 leader 与新节点启动。
- `spawn_raft_grpc_server`：测试内启动 Raft gRPC server。
- `ClusterNodeManager::node_for_test`：测试内获取新增节点句柄并启动 gRPC server。

### 数据流
1. leader 使用 `start_grpc` 启动并绑定 gRPC 监听端口。
2. `add_hub` 请求中 `raft_addr`/`grpc_addr` 均使用新节点的同一监听端口。
3. 通过 `node_for_test` 取回 node2 句柄并启动 gRPC server。
4. 使用 `ChangeMembers::SetNodes` 更新 membership，地址映射为同端口。
5. 触发 `remove_hub`，验证 leader transfer 成功。

### 错误处理
- gRPC 监听失败直接 `expect` 失败，确保测试暴露问题。
- 保留现有超时重试逻辑，不改变业务语义。

### 测试策略
- 仅运行 `cluster_admin_add_hub` 测试文件，关注该用例回归。
- 不新增额外测试用例，保持改动最小。
