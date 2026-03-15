# add_hub 启动 Raft gRPC 服务设计

**日期:** 2026-03-15  
**目标:** 修复 add_hub 在 gRPC 网络下的阻塞问题，并强制 `raft_addr` 与 `grpc_addr` 同端口约束。

## 背景
- 当前 `add_hub` 已切换为 `RaftNode::start_grpc`，但并未启动新增节点的 gRPC 服务。
- 当 leader 使用 gRPC 网络时，`add_learner` 会尝试与新节点进行 RPC；若新节点未对外提供 gRPC 服务则会阻塞。
- 用户要求 `raft_addr` 与 `grpc_addr` 必须完全一致（同一地址、同一端口）。

## 设计范围
- 修改 `ClusterNodeManager::add_hub`：启动新节点的 Raft gRPC 服务。
- 在 `precheck_add` 中增加同端口校验。
- 更新 `tests/cluster_admin_add_hub.rs`：所有 add_hub 请求使用同端口地址，优先使用动态端口避免冲突。

## 关键约束
- `raft_addr` 与 `grpc_addr` 必须完全一致（同地址同端口）。
- gRPC 服务共用同一端口，新增节点绑定在 `grpc_addr` 指定的端口。
- 维持最小行为改变：仅针对 add_hub 相关路径。

## 设计要点
### 1) 校验同端口
- 在 `precheck_add` 中规范化地址后比较。
- 不一致时返回 `INVALID_ARGUMENT`，提示修正地址配置。

### 2) 启动 Raft gRPC 服务
- `add_hub` 中创建新节点后，使用 `req.grpc_addr` 绑定 `TcpListener` 并启动 `RaftServiceServer`。
- 使用 `tokio::spawn` 后台启动服务，确保 `add_learner` 可建立连接。

### 3) 测试调整
- add_hub 相关测试改用动态端口（`127.0.0.1:0` 获取可用端口）。
- 保证每个测试用例内 `raft_addr == grpc_addr`。

## 风险与缓解
- **端口占用**：若地址冲突将返回明确错误，避免隐性阻塞。
- **并行测试冲突**：改为动态端口减少冲突概率。

## 测试策略
- 运行 `cluster_admin_add_hub` 测试文件验证新增链路。
- 重点关注 `remove_hub_commit_auto_transfers_leader_and_succeeds` 通过。
