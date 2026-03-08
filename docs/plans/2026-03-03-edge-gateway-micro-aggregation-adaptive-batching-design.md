# Edge Gateway 主动微聚合与自适应批处理设计方案

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 在 500x5 压测场景下，将批次碎片化导致的超时断崖转为可控退化，同时不破坏既有“顺序规则/优先级”执行语义。  

**Architecture:** 在现有分片 SmartBatcher 前增加短窗口 Global Buffer（主动微聚合），将“碎小请求”先合并再分发；在 SmartBatcher 中加入自适应批大小控制（仅调 max_batch_size，不改 max_wait 语义）；在 SQLite Actor 启动路径显式设置 PRAGMA（WAL + synchronous=NORMAL + busy_timeout）以提高串行写吞吐。  

**Tech Stack:** Rust, tokio, rusqlite, OpenRaft, existing EdgeGateway/OrderingScheduler/DbActor

---

## 1. 背景与问题定义

### 1.1 观测到的问题
- `batch_shards` 升到 4 时，`wait_timeout` 与 `response_dropped` 显著上升，客户端成功率断崖下降。
- 当前链路为“先按设备分片，再各分片独立攒批”，在高分片场景中更易出现小批次高频 flush。
- 下游写入路径存在串行上限（SQLite actor 单线程顺序执行），容易触发排队尾延迟放大。

### 1.2 已确认的关键约束
- 入口存在固定 `batch_wait_timeout_ms` 的硬超时语义，不能被破坏。
- 顺序规则链路必须保持阶段屏障与优先级匹配语义不变。
- 当前系统仍处于内存网络模拟，真实网络接入后尾延迟风险只会增大。

---

## 2. 方案对比（多方案+优缺点）

### 方案 A：仅调参（不改架构）
- 做法：继续人工扫描 `batch_shards / max_delay / max_batch_size / wait_timeout`。
- 优点：实现成本低，变更最小。
- 缺点：对负载变化敏感，参数迁移性差；无法根治“碎批+串行写入”结构性矛盾。
- 结论：仅作短期兜底，不建议作为主方案。

### 方案 B：主动微聚合 + PRAGMA 优化（推荐）
- 做法：新增 Global Buffer（5~10ms）在分片前聚合；SQLite 显式设置 WAL + NORMAL + busy_timeout。
- 优点：直接减少碎批，降低 proposal 频率；对当前瓶颈命中率最高；实现风险可控。
- 缺点：入口增加微小等待；`synchronous=NORMAL` 会降低单机最强 durability。
- 结论：作为第一阶段主落地方案。

### 方案 C：方案 B + 自适应批处理
- 做法：在方案 B 基础上，按窗口统计动态调节 `max_batch_size`。
- 优点：负载变化下更稳，降低手工调参成本。
- 缺点：控制环路复杂，若无滞回与冷却机制可能振荡。
- 结论：作为第二阶段增量方案，需严格保护边界。

---

## 3. “是否影响既有执行优先级”专项结论

### 3.1 现有优先级语义（基线）
- 规则匹配优先级在 `OrderingRules::match_device` 内决定：优先 `msg_type` 精确匹配，再按 `priority`、再按规则顺序。
- 命中规则的请求进入 `OrderingScheduler`，由阶段屏障控制执行顺序（stage barrier）。
- 未命中规则请求走“设备分片 -> SmartBatcher”并行路径。

### 3.2 变更后的保护策略（必须满足）
- **保护点 P1：** `match_device` 逻辑不改动（优先级判定语义 100% 保持）。
- **保护点 P2：** 命中顺序规则的请求默认旁路 Global Buffer，直接走现有调度链路。
- **保护点 P3：** 即便后续允许“命中规则请求进入 Global Buffer”，也必须按 `(order_group, stage)` 分桶，禁止跨组跨阶段合并。

### 3.3 影响判断
- 采用 P1+P2 时：不会影响之前“批量影响执行优先级”的修复结果（语义不变，仅优化非顺序路径吞吐）。
- 风险场景仅在未做分桶的“全量混合微聚合”下出现；本设计明确禁止。

---

## 4. 目标架构设计（详细）

### 4.1 写入链路（目标）
1. 入口解析请求并生成 SQL 列表。  
2. 先执行规则匹配：  
   - 命中规则：进入 OrderingScheduler（保持现状）。  
   - 未命中规则：进入 Global Buffer（新）。  
3. Global Buffer 在 `flush_delay_ms` 或 `flush_max_sqls` 达阈值后批量下发。  
4. 下发阶段按原分片规则分发到 SmartBatcher。  
5. SmartBatcher 合并后写入 `router.write_batch`。  
6. `router.write_batch -> raft.client_write -> state_machine(actor)` 完成提交。  

### 4.2 Global Buffer 设计
- 组件：`GlobalPreAggregator`（单独协程 + mpsc 队列）。
- 输入：`PreAggItem { device_id, sqls, record_count, resp }`。
- 刷新条件：
  - 时间阈值：默认 5ms（可配置 5~10ms）。
  - 大小阈值：默认 256~512 SQL（可配置）。
  - 队列阈值：达到上限立即 flush（背压保护）。
- 输出：`Vec<PreAggItem>`，按设备分片索引二次分桶后批量调用对应分片 `SmartBatcher.enqueue_batch(...)`（新增批量入队接口）。
- 错误语义：任一子批失败时按 item 粒度回传；超时统计沿用现有 wait_timeout 口径。

### 4.3 自适应批处理设计（阶段 2）
- 调节对象：仅 `max_batch_size`（不直接调 `max_wait_ms`，避免破坏超时语义）。
- 控制周期：1 秒滑动窗口。
- 观测指标：`wait_timeout_rate`, `avg_batch_size`, `batch_write_ok`。
- 控制策略（带滞回）：
  - 若 `wait_timeout_rate > high_watermark` 连续 N 窗口：`max_batch_size += step_up`（上限 cap）。
  - 若 `wait_timeout_rate < low_watermark` 且 `avg_batch_size` 偏高：`max_batch_size -= step_down`（下限 floor）。
  - 每次调整后进入 cooldown（例如 5 秒）避免振荡。
- 兜底：任何窗口 `batch_write_err` 异常升高，回退到默认静态值。

### 4.4 SQLite PRAGMA 设计
- 在 `DbHandle::spawn` 初始化连接后执行：
  - `PRAGMA journal_mode=WAL;`
  - `PRAGMA synchronous=NORMAL;`
  - `PRAGMA busy_timeout=5000;`
- 失败策略：
  - `journal_mode` 设置失败：记录错误并中止启动（高优先级失败）。
  - `synchronous` 设置失败：降级到默认并告警（可运行但需告警）。

---

## 5. 配置设计（新增字段）

在 `EdgeGatewayConfig` 增加：
- `pre_agg_enabled: bool`（默认 `false`）
- `pre_agg_flush_delay_ms: u64`（默认 `5`）
- `pre_agg_flush_max_sqls: usize`（默认 `256`）
- `pre_agg_queue_size: usize`（默认 `65536`）
- `adaptive_batch_enabled: bool`（默认 `false`）
- `adaptive_window_secs: u64`（默认 `1`）
- `adaptive_step_up: usize`（默认 `32`）
- `adaptive_step_down: usize`（默认 `16`）
- `adaptive_batch_floor: usize`（默认 `64`）
- `adaptive_batch_cap: usize`（默认 `1024`）

---

## 6. 细粒度实施计划（日本公司风格）

### Task 1: 固化基线与防回归测试

**Files:**
- Modify: `tests/edge_tcp_ordering_rules.rs`
- Modify: `tests/edge_tcp_stress.rs`

**Step 1: 写失败测试（优先级不变）**
- 新增：规则冲突下 `msg_type + priority` 选择不变测试。

**Step 2: 跑失败测试验证红灯**
- Run: `cargo test --test edge_tcp_ordering_rules ordering_rules_match_group_and_stage -- --nocapture`

**Step 3: 写失败测试（阶段屏障不变）**
- 新增：启用预聚合开关时，stage 2 不能早于 stage 1 执行。

**Step 4: 跑失败测试验证红灯**
- Run: `cargo test --test edge_tcp_ordering_rules ordering_scheduler_enforces_stage_barrier -- --nocapture`

---

### Task 2: 接入 SQLite PRAGMA 优化

**Files:**
- Modify: `src/actor.rs`
- Test: `tests/state_machine.rs`（如无则在 `tests` 下补最小连接初始化测试）

**Step 1: 写失败测试**
- 验证启动后 `PRAGMA journal_mode` 为 WAL。

**Step 2: 跑失败测试**
- Run: `cargo test --test state_machine sqlite_pragma_wal_enabled -- --nocapture`

**Step 3: 最小实现**
- 在连接初始化处执行 WAL/NORMAL/busy_timeout。

**Step 4: 通过验证**
- Run: `cargo test --test state_machine sqlite_pragma_wal_enabled -- --nocapture`

---

### Task 3: 实现 Global Buffer（仅非顺序路径）

**Files:**
- Modify: `src/hub/edge_gateway.rs`
- Modify: `src/config.rs`
- Test: `tests/edge_tcp_stress.rs`

**Step 1: 写失败测试**
- 断言：开启 pre_agg 后 `avg_batch_size` 提升，`batches` 下降。

**Step 2: 跑失败测试**
- Run: `cargo test --test edge_tcp_stress stress_scan_500clients_5reports_find_bottleneck -- --nocapture`

**Step 3: 最小实现**
- 增加 `GlobalPreAggregator` 组件；
- 非顺序请求进入 pre-agg；
- 顺序请求继续走原有调度链路（旁路 pre-agg）。

**Step 4: 通过验证**
- Run: 同 Step 2，比较新增指标列。

---

### Task 4: 实现自适应批处理（可开关）

**Files:**
- Modify: `src/hub/edge_gateway.rs`
- Modify: `src/config.rs`
- Test: `tests/edge_tcp_stress.rs`

**Step 1: 写失败测试**
- 模拟高超时窗口，断言 `max_batch_size` 自动上调且不超上限。

**Step 2: 跑失败测试**
- Run: `cargo test --test edge_tcp_stress adaptive_batching_adjusts_up_on_high_timeout -- --nocapture`

**Step 3: 最小实现**
- 引入 window 统计 + hysteresis + cooldown。

**Step 4: 通过验证**
- Run: 同 Step 2；再加低负载回调测试。

---

### Task 5: 端到端回归与发布门禁

**Files:**
- Modify: `tests/edge_tcp_stress.rs`（如需新增场景）

**Step 1: 压测回归**
- Run: `RUN_STRESS=1 cargo test --test edge_tcp_stress stress_scan_500clients_5reports_find_bottleneck -- --nocapture`

**Step 2: 优先级回归**
- Run: `cargo test --test edge_tcp_ordering_rules -- --nocapture`

**Step 3: 全量质量门禁**
- Run: `cargo fmt --all -- --check`
- Run: `cargo clippy --all-targets --all-features -- -D warnings`
- Run: `cargo check`

---

## 7. 验证指标与验收标准

### 7.1 性能验收
- `wait_timeout` 比 baseline 下降 >= 50%（500x5，shards=4）。
- `avg_batch_size` 提升 >= 30%。
- `client_success_rate` 提升到 >= 90%（同窗口参数下）。

### 7.2 语义验收（优先级/顺序）
- 规则优先级选择结果与基线一致（0 差异）。
- 阶段屏障（stage barrier）行为与基线一致（0 违规）。

### 7.3 稳定性验收
- 自适应调整无高频振荡（单位时间调整次数受 cooldown 限制）。
- 发生异常时可回退静态参数并保持服务可用。

---

## 8. 风险清单与缓解

- 风险 R1：Global Buffer 引入额外等待，低负载延迟上升。  
  缓解：提供开关 + 5ms 默认短窗口 + 小流量自动旁路策略（可选）。

- 风险 R2：Adaptive 控制振荡。  
  缓解：高低水位 + 连续窗口判定 + cooldown + 上下限钳制。

- 风险 R3：`synchronous=NORMAL` 引发 durability 认知差异。  
  缓解：文档明确“依赖 Raft 副本安全”，并保留配置化回退路径。

---

## 9. 结论与执行建议

- 建议采用 **B -> C 分阶段**：
  1) 先做 PRAGMA + Global Buffer（稳定收益、低风险）。  
  2) 再上 Adaptive（可开关灰度）。  
- 对你关心的“之前批量影响执行优先级问题”：
  - 在本设计约束（顺序链路旁路 pre-agg + 不改 match_device 语义）下，**不会影响**既有修复结果。
  - 本计划已把该点设为硬性回归门禁。

