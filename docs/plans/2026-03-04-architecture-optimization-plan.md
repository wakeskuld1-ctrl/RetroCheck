# 架构稳定性与压测可信度优化 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 修复当前“路由主从角色静态化 + 批量写转发缺失 + 失败路径统计丢失”三类架构问题，保证 HA 压测结论可信且报表口径稳定。

**Architecture:** 先在 Router 层引入“基于 Raft metrics 的动态 leader 判定”，避免 `is_leader` 静态标志与实际集群状态偏离；再补齐 follower 的批量写转发，使单写与批写语义一致；最后补齐压测失败路径的请求级统计回传，移除设备报表对旧口径回退的强依赖。全流程按 TDD 推进，每步都有回归测试与质量门禁。

**Tech Stack:** Rust, tokio, OpenRaft, tonic, EdgeGateway, Router, edge_tcp_stress tests

---

### Task 1: 固化架构问题回归测试（先红灯）

**Files:**
- Modify: `d:\Rust\check_program\tests\router.rs`
- Modify: `d:\Rust\check_program\tests\edge_tcp_stress.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn router_new_with_raft_follows_current_leader_after_failover() {
    // 1) 启动 TestCluster(3)
    // 2) 用 new_with_raft 构造 Router
    // 3) 触发 fail_leader
    // 4) 断言后续 write 仍成功（不依赖静态 is_leader）
}

#[tokio::test]
async fn router_follower_can_forward_write_batch_to_leader() {
    // 构造 follower router + leader grpc
    // 调用 write_batch(vec![...]) 并断言 rows_affected > 0
}

#[test]
fn device_report_uses_request_metrics_even_when_client_failed() {
    // 构造 client_success=false 但 upload_total/success/fail 非零
    // 断言报表按请求级字段计算，不回退到全或无
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test router router_new_with_raft_follows_current_leader_after_failover -- --nocapture`  
Expected: FAIL（当前 Router 主从角色依赖静态标记）

Run: `cargo test --test router router_follower_can_forward_write_batch_to_leader -- --nocapture`  
Expected: FAIL（当前 follower write_batch 直接返回不支持）

Run: `cargo test --test edge_tcp_stress device_report_uses_request_metrics_even_when_client_failed -- --nocapture`  
Expected: FAIL（失败路径统计仍可能回退旧口径）

**Step 3: Write minimal implementation**

```rust
// 仅添加测试，不改生产代码。
// 用失败测试锁定三类架构问题边界。
```

**Step 4: Run test to verify red tests are stable**

Run: `cargo test --test router -- --nocapture`  
Expected: FAIL（仅新增目标用例失败，现有用例可通过）

Run: `cargo test --test edge_tcp_stress -- --nocapture`  
Expected: FAIL（仅新增目标用例失败）

**Step 5: Commit**

```bash
# 跳过提交，除非用户明确要求
```

---

### Task 2: Router 主从角色动态化（消除静态 is_leader 失真）

**Files:**
- Modify: `d:\Rust\check_program\src\raft\router.rs`
- Modify: `d:\Rust\check_program\tests\router.rs`

**Step 1: Write the failing test**

使用 Task 1 已新增的 `router_new_with_raft_follows_current_leader_after_failover`。

**Step 2: Run test to verify it fails**

Run: `cargo test --test router router_new_with_raft_follows_current_leader_after_failover -- --nocapture`  
Expected: FAIL

**Step 3: Write minimal implementation**

```rust
// Router 在持有 raft_node 时：
// - 每次 write/get_version 前读取 raft metrics 判断当前节点角色
// - 非 leader 时走 leader 转发或统一错误语义
// - new_with_raft 不再仅靠固定 is_leader=true 驱动分支
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test router router_new_with_raft_follows_current_leader_after_failover -- --nocapture`  
Expected: PASS

**Step 5: Run router regression**

Run: `cargo test --test router -- --nocapture`  
Expected: PASS

**Step 6: Commit**

```bash
# 跳过提交，除非用户明确要求
```

---

### Task 3: 补齐 follower 批量写转发（对齐单写语义）

**Files:**
- Modify: `d:\Rust\check_program\src\raft\router.rs`
- Modify: `d:\Rust\check_program\tests\router.rs`

**Step 1: Write the failing test**

使用 Task 1 已新增的 `router_follower_can_forward_write_batch_to_leader`。

**Step 2: Run test to verify it fails**

Run: `cargo test --test router router_follower_can_forward_write_batch_to_leader -- --nocapture`  
Expected: FAIL（当前返回 Batch write not supported on follower）

**Step 3: Write minimal implementation**

```rust
// 在 write_batch 的 follower 分支：
// - 复用 leader_addr + timeout
// - 逐条转发或批量协议转发（与当前接口能力保持一致）
// - 统一错误映射风格：connect/timeout/execute failed
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test router router_follower_can_forward_write_batch_to_leader -- --nocapture`  
Expected: PASS

**Step 5: Run router full regression**

Run: `cargo test --test router -- --nocapture`  
Expected: PASS

**Step 6: Commit**

```bash
# 跳过提交，除非用户明确要求
```

---

### Task 4: 压测失败路径保留请求级统计（报表彻底去全或无）

**Files:**
- Modify: `d:\Rust\check_program\tests\edge_tcp_stress.rs`

**Step 1: Write the failing test**

使用 Task 1 已新增的 `device_report_uses_request_metrics_even_when_client_failed`。

**Step 2: Run test to verify it fails**

Run: `cargo test --test edge_tcp_stress device_report_uses_request_metrics_even_when_client_failed -- --nocapture`  
Expected: FAIL

**Step 3: Write minimal implementation**

```rust
// run_client_with_rate 失败返回前保留已累计的 upload_total/upload_success/upload_fail
// client_statuses 收集时在 Err 分支也携带部分统计
// build_device_report_rows 优先请求级字段，回退逻辑仅保留兼容兜底
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test edge_tcp_stress device_report_uses_request_metrics_even_when_client_failed -- --nocapture`  
Expected: PASS

**Step 5: Run stress report regression**

Run: `cargo test --test edge_tcp_stress build_device_report_rows -- --nocapture`  
Expected: PASS

**Step 6: Commit**

```bash
# 跳过提交，除非用户明确要求
```

---

### Task 5: HA 压测可信度验证（故障切换窗口）

**Files:**
- Modify: `d:\Rust\check_program\tests\edge_tcp_stress.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn progressive_ha_compare_survives_midrun_failover_window() {
    // 在 run_stress_case_with_preagg_and_ha 执行过程中主动 fail_leader
    // 断言 request_total > 0 且不会全量雪崩
}
```

**Step 2: Run test to verify it fails**

Run: `RUN_STRESS=1 cargo test --test edge_tcp_stress progressive_ha_compare_survives_midrun_failover_window -- --nocapture`  
Expected: FAIL（当前用例未覆盖中途 failover）

**Step 3: Write minimal implementation**

```rust
// 在 HA 用例中注入 fail_leader 时间点
// 并记录 failover 前后窗口指标（request_success_rate_pct, wait_timeout）
```

**Step 4: Run test to verify it passes**

Run: `RUN_STRESS=1 cargo test --test edge_tcp_stress progressive_ha_compare_survives_midrun_failover_window -- --nocapture`  
Expected: PASS

**Step 5: Execute baseline HA compare**

Run: `RUN_STRESS=1 cargo test --test edge_tcp_stress progressive_c500_r1_d20_timeout200_delay50_no_preagg_ha_compare -- --nocapture`  
Expected: PASS，并输出 baseline/HA3 对照表

**Step 6: Commit**

```bash
# 跳过提交，除非用户明确要求
```

---

### Task 6: 质量门禁与收敛检查

**Files:**
- Test: `d:\Rust\check_program\tests\router.rs`
- Test: `d:\Rust\check_program\tests\edge_tcp_stress.rs`

**Step 1: Run targeted tests**

Run: `cargo test --test router -- --nocapture`  
Expected: PASS

Run: `cargo test --test edge_tcp_stress build_device_report_rows -- --nocapture`  
Expected: PASS

**Step 2: Run lint/typecheck**

Run: `cargo clippy --all-targets --all-features -- -D warnings`  
Expected: PASS

Run: `cargo check --all-targets --all-features`  
Expected: PASS

**Step 3: Commit**

```bash
# 跳过提交，除非用户明确要求
```

---

### 风险与回退

- 动态 leader 判定若引入额外 metrics 读取开销，可先加本地短缓存（毫秒级）并保留开关。
- follower 批量转发若导致单请求膨胀，可先以“逐条转发 + 限流”上线，再迭代协议批转发。
- 报表回退逻辑需分阶段收敛，先保留兼容兜底，待历史数据路径停用后再删除。
