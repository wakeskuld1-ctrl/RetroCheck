# Code Audit TODO (2026-03-13)

本文记录当前代码仓需要修改/改进的点，按优先级排序，便于后续跟进。

## 高优先级
- **Leader 发现逻辑有缺陷**  
  follower 上 `ensure_leader_or_redirect` 依赖 `current_leader_id`，但 `current_leader_id` 只有在“本节点是 leader”时才返回 leader_id，导致 follower 端可能一直等待超时，无法正确重定向。  
  建议：优先使用 `metrics.current_leader`（即使本节点不是 leader 也返回），或直接从本地节点 metrics 推断 leader。  
  位置：`src/management/cluster_admin_service.rs`（`current_leader_id` 相关）

- **FlatBuffers 解码安全风险**  
  网络输入路径使用 `root_unchecked` + 部分手工校验，仍可能在恶意输入下触发 UB/崩溃。  
  建议：使用 `flatbuffers::Verifier` 或生成类型的安全 `root` 校验路径，减少 `unsafe` 解析。  
  位置：`src/hub/edge_schema.rs`、`src/hub/edge_session_schema.rs`

## 中优先级
- **会话过期逻辑可能不符合预期**  
  当前过期判断用 `created_at`，`last_seen` 只更新不参与过期判定，导致活跃会话也会在固定 TTL 后被清理。  
  若期望“空闲超时”，应改为基于 `last_seen`。  
  位置：`src/hub/edge_gateway.rs`（SessionManager 过期判断）

- **Cluster 管理缓存无限增长**  
  `add_request_cache`、`add_edge_request_cache`、`remove_request_cache`、`edges` 等集合无上限，存在内存膨胀风险。  
  建议：增加容量上限、TTL 或 LRU 淘汰策略。  
  位置：`src/management/cluster_admin_service.rs`

## 低优先级/清理
- **遗留文件含 compile_error**  
  `src/management/cluster_admin_service_outer.rs` 中存在 `compile_error!("I AM HERE")`，虽未被引用但容易误用。  
  建议：删除或迁移到测试/草稿目录。  

- **异步环境下使用 std::sync::Mutex**  
  `EdgeGateway` 中多个共享状态使用 `std::sync::Mutex`，可能阻塞 Tokio 运行时线程。  
  建议：考虑 `tokio::sync::Mutex` 或 `parking_lot`。  
  位置：`src/hub/edge_gateway.rs`

