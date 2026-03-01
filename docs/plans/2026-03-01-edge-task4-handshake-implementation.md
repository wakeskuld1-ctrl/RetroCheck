# Edge Task4 握手处理 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 在 Hub 侧完成 AuthHello 握手处理，创建会话并返回携带 session_id/session_key/ttl 的响应。

**Architecture:** 新增握手响应（AuthAck）编解码函数，EdgeGateway 中添加 AuthHello 处理入口：解码并校验签名、创建会话（使用 SessionManager）、生成 session_key，并返回 AuthAck 字节。测试通过模拟 device_key 与 AuthHello 请求验证会话创建与响应字段。

**Tech Stack:** Rust 2024, FlatBuffers, HMAC-SHA256, UUID v4, anyhow

---

### Task 1: 增加 AuthAck 响应编解码

**Files:**
- Modify: `d:\Rust\check_program\.worktrees\edge-session-gateway\src\hub\edge_session_schema.rs:1-240`
- Modify: `d:\Rust\check_program\.worktrees\edge-session-gateway\src\hub\edge_schema.rs:1-200`
- Test: `d:\Rust\check_program\.worktrees\edge-session-gateway\tests\edge_gateway_test.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn auth_hello_creates_session() {
    // build AuthHello using encode_auth_hello
    // call handle_auth_hello and decode_auth_ack
    // assert session_id > 0, session_key not empty, ttl_ms == expected
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test edge_gateway_test auth_hello_creates_session`  
Expected: FAIL with missing AuthAck encode/decode or handler

**Step 3: Write minimal implementation**

```rust
pub struct AuthAck {
    pub session_id: u64,
    pub session_key: Vec<u8>,
    pub ttl_ms: u64,
}

pub fn encode_auth_ack<'b>(
    builder: &mut FlatBufferBuilder<'b>,
    ack: &AuthAck,
) -> WIPOffset<Table<'b>> { /* build table with session_id, session_key vec, ttl_ms */ }

pub fn decode_auth_ack(buf: &[u8]) -> anyhow::Result<AuthAck> { /* parse table */ }
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test edge_gateway_test auth_hello_creates_session`  
Expected: PASS

**Step 5: Commit**

跳过（未被要求提交）

---

### Task 2: AuthHello 处理与会话创建

**Files:**
- Modify: `d:\Rust\check_program\.worktrees\edge-session-gateway\src\hub\edge_gateway.rs:1-260`
- Test: `d:\Rust\check_program\.worktrees\edge-session-gateway\tests\edge_gateway_test.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn auth_hello_creates_session() {
    // create SessionManager::new(ttl)
    // use device_key resolver closure
    // assert that session is stored and ack contains session_id
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test edge_gateway_test auth_hello_creates_session`  
Expected: FAIL with missing handler

**Step 3: Write minimal implementation**

```rust
pub fn handle_auth_hello(
    buf: &[u8],
    ttl_ms: u64,
    sessions: &mut SessionManager,
    lookup_key: impl Fn(u64) -> Option<Vec<u8>>,
    now_ms: u64,
) -> anyhow::Result<Vec<u8>> {
    let (meta, _) = decode_auth_hello(buf, lookup_key)?;
    let session_key = uuid::Uuid::new_v4().as_bytes().to_vec();
    let session_id = sessions.create(meta.device_id, session_key.clone(), now_ms);
    let ack = AuthAck { session_id, session_key, ttl_ms };
    let mut builder = FlatBufferBuilder::new();
    let root = encode_auth_ack(&mut builder, &ack);
    builder.finish(root, None);
    Ok(builder.finished_data().to_vec())
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test edge_gateway_test auth_hello_creates_session`  
Expected: PASS

**Step 5: Commit**

跳过（未被要求提交）

---

### Task 3: 验证与质量门禁

**Step 1: Run tests**

Run: `cargo test --test edge_gateway_test auth_hello_creates_session`  
Expected: PASS

**Step 2: Run lint/typecheck**

Run: `cargo clippy --all-targets --all-features -- -D warnings`  
Expected: PASS

Run: `cargo check`  
Expected: PASS

**Step 3: Commit**

跳过（未被要求提交）
