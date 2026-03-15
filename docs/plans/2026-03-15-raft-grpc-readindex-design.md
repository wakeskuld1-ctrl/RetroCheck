# Raft gRPC + ReadIndex 一致性闭环设计

- **日期**：2026-03-15
- **范围**：一致性与可用性闭环（Raft 网络迁移到 gRPC、线性一致读、成员变更闭环）
- **状态**：已确认方案A

## 背景与目标
当前 Raft 节点间通信依赖内存 `RaftRouter/TestNetwork`，读路径直接走本地 SQLite，成员变更流程缺少真实网络与一致性闭环。目标是让集群具备可部署的网络通信能力、线性一致读、可靠的成员变更流程，为生产可用奠定基础。

### 目标
- 用 gRPC 替代内存网络层实现 Raft RPC（AppendEntries/Vote/InstallSnapshot）。
- 补齐线性一致读（ReadIndex 或等价机制），支持 follower 读场景。
- 完善成员变更闭环（AddHub/RemoveHub/Leader 转移/重入）。
- 保留内存网络用于测试，不影响现有测试路径。

### 非目标
- 不在本次引入快照压缩与日志裁剪。
- 不涉及权限控制、TLS 与多租户隔离。
- 不在本次扩展 AI/Edge 业务功能。

## 方案对比与结论
### 方案A（采用）
- **描述**：新增 Raft gRPC 服务；生产路径切换到 gRPC 网络层；实现 ReadIndex 读一致性；成员变更闭环落地。
- **优点**：最贴近生产部署；可验证一致性闭环；后续扩展清晰。
- **缺点**：改动面较大，需新增 RPC 与测试。

### 方案B
- **描述**：gRPC 网络层 + follower 读转发 leader，不做本地 ReadIndex。
- **优点**：实现更快。
- **缺点**：读性能与可用性不如方案A；不满足“本地一致读”目标。

### 方案C
- **描述**：仍使用内存网络，只做外层 gRPC 封装。
- **优点**：改动最小。
- **缺点**：不满足生产可用要求。

**结论**：采用方案A。

## 架构与组件
### 新增/调整组件
- **RaftService (gRPC)**：新增 AppendEntries/Vote/InstallSnapshot RPC。
- **RaftGrpcNetworkFactory**：生产网络层实现，使用 gRPC client 与节点通信。
- **RaftNetworkServer**：在现有 gRPC 服务器中注册 RaftService（与 DatabaseService 共端口）。
- **拓扑映射维护**：在 AddHub/RemoveHub 中维护 `node_id -> raft_addr`。
- **ReadIndex 读入口**：封装“ReadIndex + 本地 SQLite 查询”的一致读路径。

### 保留组件
- **RaftRouter/TestNetwork**：仅用于测试与本地模拟，不进入生产路径。

## 数据流
### 写入
Client Execute → Router → RaftNode `client_write` → Raft gRPC 复制 → SQLite 状态机 apply。

### 线性一致读（follower 读）
1) Follower 接收读请求。
2) 通过 Raft ReadIndex 获取一致读屏障。
3) 本地 SQLite 查询并返回。
4) ReadIndex 超时返回可重试错误。

### 成员变更
- **AddHub**：注册 `raft_addr/grpc_addr` → `add_learner` → 可选 `change_membership`。
- **RemoveHub**：MARK_DRAINING → 排空请求 & 需要时 leader 转移 → `change_membership` → 清理拓扑映射。

## 错误处理与回退
- **Raft gRPC 失败**：映射为 `RPCError`，保留 node_id 与原因，记录日志，必要时重试。
- **ReadIndex 超时**：返回可重试错误（reason_code），不返回可能陈旧读。
- **成员变更失败**：保留幂等缓存，允许重试；COMMIT 错误不阻断同 request_id 重试。
- **拓扑缺失**：返回 `TARGET_NOT_FOUND/TARGET_METADATA_MISSING` 指示修复方向。

## 测试与验证
- **Raft gRPC 复制**：多节点启动后 AppendEntries 落地 SQLite；断开单节点仍能多数派提交。
- **ReadIndex 一致读**：leader 写入后，follower 读可见；ReadIndex 超时返回可重试。
- **成员变更闭环**：AddHub→promote→复制一致；RemoveHub→leader 转移→集群可写。
- **回归**：内存 TestNetwork 相关测试继续通过。

## 版本管理与发布
- 合并主干前更新 `Cargo.toml` 版本号（未发布状态，但可追踪）。
- 新增 `docs/changes/YYYY-MM-DD-raft-grpc-readindex.md` 记录本次变更。
- 准备 PR 说明（范围、风险、测试清单）。

## 风险与缓解
- **风险**：RPC 失败或超时导致选举抖动。
  - **缓解**：合理的超时与重试策略；记录网络指标。
- **风险**：ReadIndex 调用引入额外延迟。
  - **缓解**：只在 follower 读路径启用；leader 读仍可直接返回。
- **风险**：成员变更期间短暂不可用。
  - **缓解**：先 MARK_DRAINING，必要时转移 leader 再提交变更。
