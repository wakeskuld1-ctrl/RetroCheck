# Raft Router 读一致性与错误处理设计

**目标**
- 修复非 Leader 读一致性问题：非 Leader 读统一转发到 Leader。
- 修复写转发错误吞咽问题：保留 gRPC 原始错误信息。

**背景**
- 当前 [router.rs](file:///d:/Rust/check_program/src/raft/router.rs) 中非 Leader 读路径直接返回默认值，存在陈旧读风险。
- 非 Leader 写路径在 gRPC 错误处理时丢弃原始错误，排障困难。

---

## 设计方案对比

**方案 A：读请求统一转发到 Leader（推荐）**
- 优点：改动小；与现有写转发逻辑一致；可快速消除陈旧读。
- 缺点：读负载集中在 Leader。

**方案 B：实现 ReadIndex / LeaseRead**
- 优点：一致性读更规范；Follower 可承担读。
- 缺点：实现复杂，需要与 Raft Core 深度集成并扩展测试。

**方案 C：新增强读/弱读模式**
- 优点：灵活选择一致性与性能。
- 缺点：接口变更，使用方需理解与配置。

**结论**
- 采用方案 A：非 Leader 读统一转发到 Leader；并修复 gRPC 错误吞咽。

---

## 设计细节

### 读路径
- 非 Leader 的 `get_version` 改为通过 gRPC 访问 Leader。
- Leader 读仍走本地状态机。

### 写路径
- 保留现有转发逻辑。
- gRPC 返回错误时保留原始错误上下文，便于排查。

### 错误处理
- 转发超时：维持现有超时语义。
- gRPC 错误：保留原始错误信息，统一在错误前缀中体现。

---

## 影响范围

**修改文件**
- [router.rs](file:///d:/Rust/check_program/src/raft/router.rs)

**潜在行为变化**
- 非 Leader 读取将通过 Leader 返回结果，读一致性提升。
- 错误消息更清晰，便于定位 gRPC 失败原因。

---

## 风险与应对

**风险**
- Leader 读流量上升导致压力集中。

**应对**
- 作为短期修复先保证一致性，后续再评估 ReadIndex / LeaseRead。

---

## 测试建议

**读一致性**
- 非 Leader `get_version` 应通过 Leader 返回与 Leader 一致的结果。

**错误处理**
- 模拟 Leader gRPC 失败时，错误信息应保留原始内容（包含具体原因）。

**超时**
- 触发转发超时后，应返回超时错误且不产生写入。
