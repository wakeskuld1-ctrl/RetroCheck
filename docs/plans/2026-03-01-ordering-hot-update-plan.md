# 顺序规则热更新 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 在网关侧实现“顺序域 + 阶段”的调度与规则热更新，并覆盖 5000 设备压力下的顺序与超时验证。

**Architecture:** 请求入站后通过规则引擎按 device_prefix + 可选 msg_type 匹配顺序域与阶段，顺序域内按阶段推进、阶段内并行；规则通过管理接口动态更新并原子切换，仅影响新请求。

**Tech Stack:** Rust, tokio, tonic, serde, existing EdgeGateway & Router modules

---

### Task 1: 建立顺序规则模型与内存引擎

**Files:**
- Create: `src/hub/order_rules.rs`
- Modify: `src/hub/mod.rs`
- Test: `tests/edge_tcp_ordering_rules.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn ordering_rules_match_group_and_stage() {
    let rules = OrderingRules::from_json(r#"{
        "rules":[{"device_prefix":"A","msg_type":3,"order_group":"g1","stage":1,"priority":10}]
    }"#).unwrap();
    let matched = rules.match_device("A-001", 3).unwrap();
    assert_eq!(matched.order_group, "g1");
    assert_eq!(matched.stage, 1);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test tests::edge_tcp_ordering_rules::ordering_rules_match_group_and_stage -v`

Expected: FAIL with "use of undeclared type `OrderingRules`"

**Step 3: Write minimal implementation**

```rust
pub struct OrderRule { pub device_prefix: String, pub msg_type: Option<u8>, pub order_group: String, pub stage: u32, pub priority: i32 }
pub struct OrderingRules { rules: Vec<OrderRule> }
impl OrderingRules {
    pub fn from_json(raw: &str) -> Result<Self> { /* serde_json parse */ }
    pub fn match_device(&self, device_id: &str, msg_type: u8) -> Option<OrderRule> { /* prefix + optional msg_type match */ }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test tests::edge_tcp_ordering_rules::ordering_rules_match_group_and_stage -v`

Expected: PASS

**Step 5: Commit**

```bash
git add src/hub/order_rules.rs src/hub/mod.rs tests/edge_tcp_ordering_rules.rs
git commit -m "feat: add ordering rules model"
```

---

### Task 2: 引入顺序域队列与阶段推进调度

**Files:**
- Modify: `src/hub/edge_gateway.rs`
- Create: `src/hub/order_scheduler.rs`
- Test: `tests/edge_tcp_ordering_rules.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn ordering_scheduler_enforces_stage_barrier() {
    let scheduler = OrderingScheduler::new(/* stage_parallelism */ 2, /* queue_limit */ 100);
    let g = "g1";
    scheduler.enqueue(g, 1, "t1").await.unwrap();
    scheduler.enqueue(g, 2, "t2").await.unwrap();
    let first = scheduler.next_ready().await.unwrap();
    assert_eq!(first.stage, 1);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test tests::edge_tcp_ordering_rules::ordering_scheduler_enforces_stage_barrier -v`

Expected: FAIL with "use of undeclared type `OrderingScheduler`"

**Step 3: Write minimal implementation**

```rust
pub struct OrderingScheduler { /* per group queues + stage state */ }
impl OrderingScheduler {
    pub async fn enqueue(&self, group: &str, stage: u32, task: TaskToken) -> Result<()> { /* ... */ }
    pub async fn next_ready(&self) -> Option<ReadyTask> { /* only current stage */ }
    pub async fn mark_done(&self, group: &str, stage: u32) { /* advance */ }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test tests::edge_tcp_ordering_rules::ordering_scheduler_enforces_stage_barrier -v`

Expected: PASS

**Step 5: Commit**

```bash
git add src/hub/order_scheduler.rs src/hub/edge_gateway.rs tests/edge_tcp_ordering_rules.rs
git commit -m "feat: add ordering scheduler with stage barrier"
```

---

### Task 3: 管理接口实现规则热更新

**Files:**
- Modify: `proto/transaction.proto`
- Modify: `build.rs`
- Modify: `src/bin/server.rs`
- Modify: `src/lib.rs` (or gRPC module entry)
- Create: `src/management/order_rules_service.rs`
- Test: `tests/edge_tcp_ordering_rules.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn ordering_rules_hot_reload_only_affects_new_requests() {
    // start management service, apply v1 rules, enqueue old request
    // apply v2 rules, enqueue new request
    // assert old uses v1, new uses v2
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test tests::edge_tcp_ordering_rules::ordering_rules_hot_reload_only_affects_new_requests -v`

Expected: FAIL with "management service not found"

**Step 3: Write minimal implementation**

```rust
service OrderRulesAdmin {
  rpc UpdateRules(UpdateRulesRequest) returns (UpdateRulesResponse);
  rpc GetRulesVersion(GetRulesVersionRequest) returns (GetRulesVersionResponse);
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test tests::edge_tcp_ordering_rules::ordering_rules_hot_reload_only_affects_new_requests -v`

Expected: PASS

**Step 5: Commit**

```bash
git add proto/transaction.proto build.rs src/bin/server.rs src/management/order_rules_service.rs tests/edge_tcp_ordering_rules.rs
git commit -m "feat: add order rules management api"
```

---

### Task 4: 接入 EdgeGateway 请求流与批处理边界

**Files:**
- Modify: `src/hub/edge_gateway.rs`
- Modify: `src/config.rs`
- Test: `tests/edge_tcp_ordering_rules.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn edge_gateway_ordering_applies_stage_and_group() {
    // enqueue three devices in same group with stage 1/2/3
    // assert batcher only processes stage 1 before stage 2
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test tests::edge_tcp_ordering_rules::edge_gateway_ordering_applies_stage_and_group -v`

Expected: FAIL with "no ordering integration"

**Step 3: Write minimal implementation**

```rust
// resolve device -> ordering rule
// enqueue into ordering scheduler
// only merge sqls within same stage
```

**Step 4: Run test to verify it passes**

Run: `cargo test tests::edge_tcp_ordering_rules::edge_gateway_ordering_applies_stage_and_group -v`

Expected: PASS

**Step 5: Commit**

```bash
git add src/hub/edge_gateway.rs src/config.rs tests/edge_tcp_ordering_rules.rs
git commit -m "feat: integrate ordering scheduler into edge gateway"
```

---

### Task 5: 5000 设备顺序压力与超时验证

**Files:**
- Modify: `tests/edge_tcp_stress.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn stress_5000_devices_ordering_no_timeout() {
    // configure order rules for a subset of devices with stage barriers
    // run stress and assert no wait_timeout, order maintained
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test tests::edge_tcp_stress::stress_5000_devices_ordering_no_timeout -v`

Expected: FAIL with "ordering enforcement missing"

**Step 3: Write minimal implementation**

```rust
// reuse existing stress harness, add ordering assertions
```

**Step 4: Run test to verify it passes**

Run: `cargo test tests::edge_tcp_stress::stress_5000_devices_ordering_no_timeout -v`

Expected: PASS

**Step 5: Commit**

```bash
git add tests/edge_tcp_stress.rs
git commit -m "test: add 5000 device ordering stress"
```
