# EdgeGateway Draining Regression Tests Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 覆盖排空期间 connect 失败回归、并发 inflight 统计一致性，并清理 edge_generated.rs 的 Rust 2024 警告与 ANSI 乱码备注。

**Architecture:** 在 EdgeGateway 的 run 循环中引入排空态监听关闭/重绑；测试侧通过新增 spawn helper 暴露网关句柄；通过 allow 包裹生成文件解决 2024 警告。

**Tech Stack:** Rust, tokio, flatbuffers, cargo test

---

### Task 1: 为测试暴露可控的 EdgeGateway 句柄

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/edge-gateway-drain-tests-2026-03-15/tests/edge_tcp_common.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn edge_tcp_draining_rejects_connect_then_allows_after_resume() -> Result<()> {
    // placeholder for new helper: spawn_gateway_with_handle
    let (_addr, _gateway, _handle) = spawn_gateway_with_handle().await?;
    Ok(())
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test edge_tcp_concurrency -- edge_tcp_draining_rejects_connect_then_allows_after_resume --test-threads=1`
Expected: FAIL (unresolved function spawn_gateway_with_handle)

**Step 3: Write minimal implementation**

```rust
pub async fn spawn_gateway_with_handle()
    -> Result<(String, Arc<EdgeGateway>, tokio::task::JoinHandle<Result<()>>)> {
    // returns addr + Arc<EdgeGateway> + JoinHandle
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test edge_tcp_concurrency -- edge_tcp_draining_rejects_connect_then_allows_after_resume --test-threads=1`
Expected: still FAIL (connect behavior not implemented yet)

**Step 5: Commit**

```bash
git add tests/edge_tcp_common.rs
git commit -m "test: add gateway spawn helper for drain control"
```

---

### Task 2: 排空期间 connect 失败回归测试

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/edge-gateway-drain-tests-2026-03-15/tests/edge_tcp_concurrency.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn edge_tcp_draining_rejects_connect_then_allows_after_resume() -> Result<()> {
    let (addr, gateway, handle) = spawn_gateway_with_handle().await?;
    gateway.set_draining(true);

    for _ in 0..5 {
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            TcpStream::connect(&addr),
        )
        .await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    gateway.set_draining(false);
    let _stream = connect_with_retry(&addr).await?;

    handle.abort();
    Ok(())
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test edge_tcp_concurrency -- edge_tcp_draining_rejects_connect_then_allows_after_resume --test-threads=1`
Expected: FAIL (connect still succeeds during draining)

**Step 3: Write minimal implementation**

(见 Task 3，修复 run 循环使 draining 时停止监听)

**Step 4: Run test to verify it passes**

Run: `cargo test --test edge_tcp_concurrency -- edge_tcp_draining_rejects_connect_then_allows_after_resume --test-threads=1`
Expected: PASS

**Step 5: Commit**

```bash
git add tests/edge_tcp_concurrency.rs src/hub/edge_gateway.rs
git commit -m "feat: reject connect during draining"
```

---

### Task 3: 排空时关闭监听并可恢复

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/edge-gateway-drain-tests-2026-03-15/src/hub/edge_gateway.rs`

**Step 1: Write the failing test**

(复用 Task 2 的测试，不新增)

**Step 2: Run test to verify it fails**

Run: `cargo test --test edge_tcp_concurrency -- edge_tcp_draining_rejects_connect_then_allows_after_resume --test-threads=1`
Expected: FAIL

**Step 3: Write minimal implementation**

```rust
// add to EdgeGateway struct
// drain_notify: Arc<Notify>

pub fn set_draining(&self, reject_new: bool) {
    self.draining.store(reject_new, Ordering::SeqCst);
    self.drain_notify.notify_waiters();
}

pub async fn run(self: Arc<Self>) -> Result<()> {
    let mut listener: Option<TcpListener> = Some(TcpListener::bind(&self.addr).await?);

    loop {
        if self.draining.load(Ordering::SeqCst) {
            listener.take();
            self.drain_notify.notified().await;
            continue;
        }

        if listener.is_none() {
            listener = Some(TcpListener::bind(&self.addr).await?);
        }

        tokio::select! {
            accepted = listener.as_ref().unwrap().accept() => {
                let (socket, _) = accepted?;
                // existing accept handling...
            }
            _ = self.drain_notify.notified() => {
                continue;
            }
        }
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test edge_tcp_concurrency -- edge_tcp_draining_rejects_connect_then_allows_after_resume --test-threads=1`
Expected: PASS

**Step 5: Commit**

```bash
git add src/hub/edge_gateway.rs
git commit -m "feat: close listener when draining"
```

---

### Task 4: inflight_requests 并发一致性测试

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/edge-gateway-drain-tests-2026-03-15/tests/edge_tcp_concurrency.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn edge_tcp_inflight_requests_tracks_concurrent_connections() -> Result<()> {
    let (addr, gateway, handle) = spawn_gateway_with_handle().await?;
    let mut streams = Vec::new();

    for _ in 0..8 {
        streams.push(TcpStream::connect(&addr).await?);
    }

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(gateway.inflight_requests() >= 8);

    drop(streams);
    for _ in 0..10 {
        if gateway.inflight_requests() == 0 { break; }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(gateway.inflight_requests(), 0);

    handle.abort();
    Ok(())
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test edge_tcp_concurrency -- edge_tcp_inflight_requests_tracks_concurrent_connections --test-threads=1`
Expected: FAIL (if inflight not tracked or timing issues)

**Step 3: Write minimal implementation**

(若失败，微调测试同步手段或 inflight 计数点，保持计数与连接生命周期一致)

**Step 4: Run test to verify it passes**

Run: `cargo test --test edge_tcp_concurrency -- edge_tcp_inflight_requests_tracks_concurrent_connections --test-threads=1`
Expected: PASS

**Step 5: Commit**

```bash
git add tests/edge_tcp_concurrency.rs

git commit -m "test: verify inflight count on concurrent connects"
```

---

### Task 5: 清理 edge_generated.rs 的 Rust 2024 警告

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/edge-gateway-drain-tests-2026-03-15/src/hub/edge_fbs.rs`

**Step 1: Write the failing test**

(编译警告类问题，使用构建验证)

**Step 2: Run build to verify it warns**

Run: `cargo build`
Expected: WARN `unsafe_op_in_unsafe_fn`, `mismatched_lifetime_syntaxes`

**Step 3: Write minimal implementation**

```rust
#![allow(
    dead_code,
    unused_imports,
    non_camel_case_types,
    non_snake_case,
    unsafe_op_in_unsafe_fn,
    mismatched_lifetime_syntaxes,
)]
```

**Step 4: Run build to verify warnings are gone**

Run: `cargo build`
Expected: no edge_generated.rs warnings

**Step 5: Commit**

```bash
git add src/hub/edge_fbs.rs
git commit -m "chore: silence flatbuffers 2024 warnings"
```

---

### Task 6: ANSI 乱码备注恢复为 UTF-8

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/edge-gateway-drain-tests-2026-03-15/src/hub/edge_gateway.rs`
- Modify: `d:/Rust/check_program/.worktrees/edge-gateway-drain-tests-2026-03-15/tests/edge_tcp_common.rs`
- Modify: `d:/Rust/check_program/.worktrees/edge-gateway-drain-tests-2026-03-15/build.rs`

**Step 1: Identify garbled comments**

Run: `rg "修改|乱码" -n src tests build.rs`
Expected: finds garbled blocks

**Step 2: Replace with UTF-8 Chinese**

(仅修复乱码，不改变含义)

**Step 3: Run build/test to ensure no behavior change**

Run: `cargo test -- --test-threads=1`
Expected: PASS

**Step 4: Commit**

```bash
git add src/hub/edge_gateway.rs tests/edge_tcp_common.rs build.rs
git commit -m "chore: fix ansi garbled comments"
```

---

### Task 7: 验证与汇总

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/edge-gateway-drain-tests-2026-03-15/.trae/CHANGELOG_TASK.md`

**Step 1: Run full tests**

Run: `cargo test -- --test-threads=1`
Expected: PASS

**Step 2: Record task journal**

Use skill: `task-journal`

**Step 3: Summarize risks & suggestions**

- 列出可能问题与补充测试建议

**Step 4: Commit**

```bash
git add .trae/CHANGELOG_TASK.md
git commit -m "docs: update task journal for drain tests"
```

