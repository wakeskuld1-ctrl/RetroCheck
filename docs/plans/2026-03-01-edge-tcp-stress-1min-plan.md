# Edge TCP 1分钟压测用例 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 新增 1 分钟版 5000 终端 5 rps 压测用例，并以 `--nocapture` 输出日志用于定位超时问题。

**Architecture:** 复用现有 5 分钟压测用例的参数与逻辑，仅新增一个独立测试函数，将时长调整为 60 秒，保持其他配置一致以便对比。

**Tech Stack:** Rust、tokio、anyhow、flatbuffers、现有 edge_tcp_stress 测试框架。

---

### Task 1: 新增 1 分钟压测用例

**Files:**
- Modify: `d:\Rust\check_program\tests\edge_tcp_stress.rs`
- Test: `cargo test tests::edge_tcp_stress::stress_test_edge_gateway_5000_clients_5rps_1min -- --nocapture`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn stress_test_edge_gateway_5000_clients_5rps_1min() -> Result<()> {
    // 复制 5 分钟压测逻辑，仅将 duration 调整为 60 秒
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test tests::edge_tcp_stress::stress_test_edge_gateway_5000_clients_5rps_1min -- --nocapture`  
Expected: FAIL（若仍存在 wait_timeout/response_dropped，将在输出中体现）

**Step 3: Write minimal implementation**

```rust
// 复制 5 分钟压测函数并调整 duration
let duration = Duration::from_secs(60);
```

**Step 4: Run test to verify it passes**

Run: `cargo test tests::edge_tcp_stress::stress_test_edge_gateway_5000_clients_5rps_1min -- --nocapture`  
Expected: PASS（若仍失败，进入定位流程）

**Step 5: Commit**

```bash
git add tests/edge_tcp_stress.rs
git commit -m "test: add 1min edge tcp stress test"
```

> 仅在用户明确要求提交时执行提交步骤。
