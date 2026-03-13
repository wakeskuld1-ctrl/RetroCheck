# Code Audit 修复设计

**目标**：修复 leader 识别/重定向、FlatBuffers 安全解码、Session 固定 TTL 语义、NonceCache 每组上限、compile_error 清理、EdgeGateway Mutex（async 友好）问题。

---

## 架构概述
- **FlatBuffers**：新增 `.fbs` schema 并通过 `flatc` 生成 Rust 类型，解码路径统一改为 `Verifier + root_as_*`，移除 `root_unchecked` 与手工结构校验。
- **Session TTL**：以 `created_at_ms` 计算固定 TTL；`last_seen_ms` 仅用于观测，不参与续期。
- **NonceCache**：`nonce_cache_limit` 语义为“每个 group 的上限”，超出上限淘汰最旧 nonce。
- **Mutex**：`EdgeGateway` 内部共享状态替换为 `tokio::sync::Mutex`，避免 async 阻塞。
- **Leader Redirect**：leader 识别逻辑统一，`leader_hint` 优先返回可用的 gRPC 地址，必要时提供可解析的回退信息。
- **compile_error**：清理遗留 `compile_error!` 或以 feature gate + 文档替代。

## 组件与边界
- `proto/` 或 `schemas/`：新增 `.fbs`（请求/会话/响应）。
- `build.rs`：集成 `flatc` 生成 Rust 代码至 `src/hub/edge_fbs/`。
- `src/hub/edge_schema.rs`：仅保留高层 encode/decode 入口。
- `src/hub/edge_session_schema.rs`：集中处理签名验证与 payload 解析。
- `src/hub/edge_gateway.rs`：替换锁类型并收缩锁持有范围。
- `src/management/cluster_admin_service.rs`：统一 leader 判断与 redirect。

## 数据流
- **AuthHello**：TCP → 解码 → 验签 → `NonceCache::seen` → `SessionManager::create` → `AuthAck`。
- **SessionRequest**：解码 → 验签 → `NonceCache::seen` → `SessionManager::touch`（不延长 TTL）→ 业务处理。
- **UploadData**：安全解码 → 调度/批处理 → 回包签名。

## 错误处理与安全
- FlatBuffers 校验失败返回 `Invalid FlatBuffer`。
- MAC 不匹配返回签名错误。
- Nonce 重放返回 `replay detected`。
- 非 leader 返回 `NOT_LEADER` + `leader_hint`。

## 测试策略（TDD）
- FlatBuffers：非法 buffer 必须返回错误且不 panic。
- Session TTL：过期后访问报错，`last_seen_ms` 不续期。
- NonceCache：达到每组上限后淘汰最旧 nonce，重放检测有效。
- Leader Redirect：follower 正确返回 `leader_hint`，失效地址可回退。
- Mutex：并发请求下无阻塞卡死。

## 非目标
- 大规模重构协议或引入全新业务语义。
- 改动网关业务逻辑以外的功能扩展。
