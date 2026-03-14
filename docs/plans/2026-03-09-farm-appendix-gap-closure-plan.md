# RetroCheck 底座能力收敛计划（去业务化）

## 0. 目标与边界（仅底座）

- 目标：把规划聚焦到 RetroCheck 数据底座自身能力，不绑定任何农场或行业业务模型。
- 范围：仅覆盖 Raft 提交语义、状态机 apply、故障恢复、可观测性与一致性验证。
- 约束：不引入业务表、不引入业务编排状态机、不把业务幂等逻辑硬塞进底座。

## 1. 方案对比

### 方案 A：继续按业务闭环扩展

- 优点：叙事完整，业务可展示性高。
- 缺点：与底座职责耦合过深，容易引入非通用能力。

### 方案 B：底座能力优先（推荐）

- 优点：职责清晰，通用性强，回归风险可控。
- 缺点：对业务方看起来“功能不多”，但技术债更少。

### 方案 C：仅保留现状，不继续收敛

- 优点：短期零改动。
- 缺点：缺少明确底座能力边界与验证基线。

### 选型结论

- 推荐方案：**B（底座能力优先）**。
- 原因：当前仓库核心价值是一致性与恢复能力，应先把底座能力做实、做稳、做可验证。

## 2. 分阶段实施

### Phase 1：固化底座能力缺口测试（先红灯）

**目标**
- 用失败测试锁定“提交语义、故障恢复、状态一致性”的缺口边界，防止后续漂移。

**涉及文件**
- 修改：`d:\Rust\check_program\tests\edge_cmd.rs`
- 修改：`d:\Rust\check_program\tests\edge_tcp_stress.rs`
- 视情况修改：`d:\Rust\check_program\tests\raft_apply.rs`

**测试要点**
- Leader failover 后写入语义保持一致（不丢、不重、可继续提交）。
- 提交后状态机查询结果与提交顺序一致。
- 节点重启后可通过日志/快照恢复到一致状态。

---

### Phase 2：收敛底座状态模型（Raft 提交后可查询）

**目标**
- 在状态机路径完善底座必需元信息结构（版本、提交位点、恢复位点、快照元数据）。
- 保持所有写入通过 Router 写路径进入 Raft 提交，不引入旁路写。

**涉及文件**
- 修改：`d:\Rust\check_program\src\raft\state_machine.rs`
- 修改：`d:\Rust\check_program\src\actor.rs`
- 修改：`d:\Rust\check_program\src\raft\router.rs`

**关键约束**
- 不引入业务域表。
- 不引入仅服务于单业务的幂等字段要求。
- 迁移逻辑保持幂等（`CREATE TABLE IF NOT EXISTS` + `CREATE INDEX IF NOT EXISTS`）。

---

### Phase 3：统一底座写入约束与错误语义

**目标**
- 统一 Router/StateMachine/网关到 Raft 写入口的错误语义与重试边界，避免分支行为漂移。

**涉及文件**
- 修改：`d:\Rust\check_program\src\hub\edge_gateway.rs`
- 修改：`d:\Rust\check_program\src\hub\edge_schema.rs`
- 修改：`d:\Rust\check_program\src\management\order_rules_service.rs`

**关键约束**
- 只保留底座相关通用约束，不新增业务闭环协议。
- 错误分层清晰（可重试/不可重试/需切主后重试）。

---

### Phase 4：补齐底座可观测与审计事件

**目标**
- 对关键底座动作（选主、提交、apply、快照、恢复）记录统一事件，支持问题追踪。

**涉及文件**
- 修改：`d:\Rust\check_program\src\management\cluster_admin_service.rs`
- 修改：`d:\Rust\check_program\src\hub\edge_gateway.rs`
- 可选新增：审计写入辅助模块（若复用现有模块困难）

**关键约束**
- 审计事件写失败不能吞掉，应有明确错误分支。
- 事件字段保持通用，不绑定业务对象类型。

---

### Phase 5：回归与验收

**目标**
- 用现有质量门禁验证“新增能力 + 原有能力”同时稳定。

**命令**
```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --test edge_cmd -- --nocapture
cargo test --test edge_tcp_stress -- --nocapture
cargo test
./verify.ps1 -Scenario verify-only
```

## 3. 里程碑验收标准

- M1：存在失败测试，且能稳定复现当前缺口。
- M2：底座元信息结构完善，DDL 可重复执行且不破坏旧数据。
- M3：写入路径错误语义统一，故障切换与恢复边界清晰。
- M4：关键底座事件可查询且可用于故障复盘。
- M5：格式化、静态检查、测试与 verify 脚本全部通过。

## 4. 风险与缓解

- 风险：`EdgeGateway` 职责过重导致改动冲突。
- 缓解：按 Phase 拆分 PR，优先提取纯函数和小接口，再接入主流程。

- 风险：不同入口写路径错误语义不一致。
- 缓解：通过统一错误映射和回归测试收敛。

- 风险：新增 DDL 与旧测试数据不兼容。
- 缓解：全部使用 IF NOT EXISTS，测试中显式清理临时数据。

## 5. 本轮执行顺序建议

1. 先做 Phase 1（测试红灯）。
2. 再做 Phase 2（底座状态模型）。
3. 接着做 Phase 3（写入语义与错误边界）。
4. 然后做 Phase 4（可观测与事件）。
5. 最后做 Phase 5（全量回归）。

## 6. 计划归类

### A. 验证基线类

- 对应阶段：Phase 1、Phase 5
- 目标：先用失败测试锁定缺口，再用全量门禁验证收敛结果。
- 产出：可稳定复现的测试样例、最终通过的 fmt/clippy/test/verify 结果。

### B. 底座数据模型类

- 对应阶段：Phase 2
- 目标：完善底座元信息结构并保证写入只走 Raft 提交路径。
- 产出：可重复执行且不破坏旧数据的 DDL/迁移逻辑。

### C. 写入语义收敛类

- 对应阶段：Phase 3
- 目标：统一网关/Router/StateMachine 的错误语义与重试边界。
- 产出：一致的错误分层（可重试/不可重试/切主后重试）与回归用例。

### D. 可观测与审计类

- 对应阶段：Phase 4
- 目标：覆盖选主、提交、apply、快照、恢复等关键底座事件。
- 产出：可查询、可复盘、字段通用的事件记录能力。
