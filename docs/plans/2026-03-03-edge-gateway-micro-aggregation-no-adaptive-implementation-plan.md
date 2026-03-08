# Edge Gateway 主动微聚合（不含自适应）Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 在 500x5 压测下优先落地可控低风险优化（PRAGMA + Global Buffer 非顺序路径），缓解碎批导致的超时断崖，并保持顺序规则语义不变。

**Architecture:** 先用测试固化“优先级匹配与阶段屏障”基线，再接入 SQLite PRAGMA（WAL/NORMAL/busy_timeout）提升串行写吞吐；随后在 EdgeGateway 分片前增加 GlobalPreAggregator，仅处理未命中顺序规则请求，命中规则请求继续旁路至 OrderingScheduler。全流程按 TDD 执行，并在每一步附带 progressive 压测验证。

**Tech Stack:** Rust, tokio, rusqlite, OpenRaft, EdgeGateway/OrderingScheduler/DbActor

---

### Task 1: 固化顺序语义防回归（优先级 + 阶段屏障）

**Files:**
- Modify: `d:\Rust\check_program\tests\edge_tcp_ordering_rules.rs`
- Modify: `d:\Rust\check_program\tests\edge_tcp_stress.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn ordering_rules_priority_stable_under_preagg_flag() -> Result<()> {
    let rules = OrderingRules::from_json(
        r#"{
        "rules":[
            {"device_prefix":"A","msg_type":3,"order_group":"g_precise","stage":1,"priority":1},
            {"device_prefix":"A","order_group":"g_wild","stage":2,"priority":99}
        ]
    }"#,
    )?;
    let matched = rules.match_device("A-101", 3).expect("rule should match");
    assert_eq!(matched.order_group, "g_precise");
    assert_eq!(matched.stage, 1);
    Ok(())
}

#[tokio::test]
async fn ordering_stage_barrier_unchanged_when_preagg_enabled() -> Result<()> {
    let scheduler = OrderingScheduler::<String>::new(1, 10);
    scheduler.enqueue("g1", 1, "task-1".to_string()).await?;
    scheduler.enqueue("g1", 2, "task-2".to_string()).await?;
    let first = scheduler.next_ready().await.expect("first");
    assert_eq!(first.stage, 1);
    scheduler.mark_done("g1", 1).await;
    let second = scheduler.next_ready().await.expect("second");
    assert_eq!(second.stage, 2);
    Ok(())
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test edge_tcp_ordering_rules ordering_rules_priority_stable_under_preagg_flag -- --nocapture`  
Expected: FAIL（新增测试尚未存在）

Run: `cargo test --test edge_tcp_ordering_rules ordering_stage_barrier_unchanged_when_preagg_enabled -- --nocapture`  
Expected: FAIL（新增测试尚未存在）

**Step 3: Write minimal implementation**

```rust
// 测试实现，无生产代码改动。
// 保持 match_device 与 stage barrier 的行为锁定。
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test edge_tcp_ordering_rules -- --nocapture`  
Expected: PASS

**Step 5: Progressive reliability check**

Run: `RUN_STRESS=1 cargo test --test edge_tcp_stress progressive_c500_r1_d20_timeout200_delay50_baseline -- --nocapture`  
Expected: PASS，并产出 baseline 指标（wait_timeout / response_dropped / avg_batch_size）

**Step 6: Commit**

```bash
# 跳过提交，除非用户明确要求
```

---

### Task 2: 接入 SQLite PRAGMA（WAL + NORMAL + busy_timeout）

**Files:**
- Modify: `d:\Rust\check_program\src\actor.rs`
- Modify: `d:\Rust\check_program\tests\state_machine.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn sqlite_pragma_wal_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");
    let sm = check_program::raft::state_machine::SqliteStateMachine::new(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap();
    let mode = sm
        .query_scalar("PRAGMA journal_mode;".to_string())
        .await
        .unwrap()
        .to_lowercase();
    assert_eq!(mode, "wal");
}

#[tokio::test]
async fn sqlite_pragma_busy_timeout_applied() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");
    let sm = check_program::raft::state_machine::SqliteStateMachine::new(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap();
    let timeout = sm
        .query_scalar("PRAGMA busy_timeout;".to_string())
        .await
        .unwrap();
    assert_eq!(timeout, "5000");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test state_machine sqlite_pragma_wal_enabled -- --nocapture`  
Expected: FAIL（当前未显式设置 PRAGMA）

Run: `cargo test --test state_machine sqlite_pragma_busy_timeout_applied -- --nocapture`  
Expected: FAIL（当前未显式设置 busy_timeout）

**Step 3: Write minimal implementation**

```rust
// DbHandle::spawn 连接建立后执行：
// PRAGMA journal_mode=WAL;
// PRAGMA synchronous=NORMAL;
// PRAGMA busy_timeout=5000;
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test state_machine sqlite_pragma_ -- --nocapture`  
Expected: PASS

**Step 5: Progressive reliability check**

Run: `RUN_STRESS=1 cargo test --test edge_tcp_stress progressive_c500_r1_d20_timeout200_delay50_pragma -- --nocapture`  
Expected: PASS，且相较 baseline 至少在 wait_timeout 或 success_rate 上不劣化

**Step 6: Commit**

```bash
# 跳过提交，除非用户明确要求
```

---

### Task 3: 实现 Global Buffer（仅非顺序路径）

**Files:**
- Modify: `d:\Rust\check_program\src\config.rs`
- Modify: `d:\Rust\check_program\src\hub\edge_gateway.rs`
- Modify: `d:\Rust\check_program\tests\edge_tcp_stress.rs`
- Modify: `d:\Rust\check_program\tests\edge_tcp_ordering_rules.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn preagg_bypasses_ordering_path() -> Result<()> {
    // 命中顺序规则请求不进入 pre-agg，stage barrier 行为不变
    Ok(())
}

#[tokio::test]
async fn preagg_improves_batch_fragmentation_metrics() -> Result<()> {
    // 对照 pre_agg_enabled=false/true，断言 avg_batch_size 上升且 batches 下降
    Ok(())
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test edge_tcp_stress preagg_improves_batch_fragmentation_metrics -- --nocapture`  
Expected: FAIL（功能尚未实现）

Run: `cargo test --test edge_tcp_ordering_rules preagg_bypasses_ordering_path -- --nocapture`  
Expected: FAIL（功能尚未实现）

**Step 3: Write minimal implementation**

```rust
// EdgeGatewayConfig 新增：
// pre_agg_enabled / pre_agg_flush_delay_ms / pre_agg_flush_max_sqls / pre_agg_queue_size
// 
// EdgeGateway 新增 GlobalPreAggregator：
// - 命中规则: 直接走 OrderingScheduler
// - 未命中规则: 进入 pre-agg，再按 shard 下发 SmartBatcher.enqueue_batch(...)
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test edge_tcp_ordering_rules -- --nocapture`  
Expected: PASS

Run: `cargo test --test edge_tcp_stress preagg_ -- --nocapture`  
Expected: PASS

**Step 5: Progressive reliability check**

Run: `RUN_STRESS=1 cargo test --test edge_tcp_stress progressive_c500_r1_d20_timeout200_delay50_preagg -- --nocapture`  
Expected: PASS，且相较 pragma 阶段 avg_batch_size 提升、wait_timeout 下降

**Step 6: Commit**

```bash
# 跳过提交，除非用户明确要求
```

---

### Task 4: 端到端回归与门禁

**Files:**
- Modify: `d:\Rust\check_program\tests\edge_tcp_stress.rs`（如需补充场景）

**Step 1: Run progressive suite**

Run: `RUN_STRESS=1 cargo test --test edge_tcp_stress progressive_c500_r1_d20_timeout200_delay50_ -- --nocapture`  
Expected: PASS，三阶段结果可对比

**Step 2: Run semantics regression**

Run: `cargo test --test edge_tcp_ordering_rules -- --nocapture`  
Expected: PASS

**Step 3: Run quality gates**

Run: `cargo fmt --all -- --check`  
Expected: PASS

Run: `cargo clippy --all-targets --all-features -- -D warnings`  
Expected: PASS

Run: `cargo check --all-targets --all-features`  
Expected: PASS

**Step 4: Commit**

```bash
# 跳过提交，除非用户明确要求
```

---

### Task 5: 风险兜底与回退策略（不含自适应）

**Files:**
- Modify: `d:\Rust\check_program\src\config.rs`
- Modify: `d:\Rust\check_program\src\hub\edge_gateway.rs`

**Step 1: Add fallback controls**

```rust
// pre_agg_enabled 默认 false
// pre_agg_queue_size 达阈值立即 flush（避免长尾堆积）
// 失败时保持原有非顺序路径可运行
```

**Step 2: Verify fallback behavior**

Run: `cargo test --test edge_tcp_stress preagg_ -- --nocapture`  
Expected: PASS（启停开关均可运行）

**Step 3: Progressive safety check**

Run: `RUN_STRESS=1 cargo test --test edge_tcp_stress progressive_c500_r1_d20_timeout200_delay50_preagg -- --nocapture`  
Expected: PASS，异常时可通过关闭 pre_agg 回退

**Step 4: Commit**

```bash
# 跳过提交，除非用户明确要求
```
