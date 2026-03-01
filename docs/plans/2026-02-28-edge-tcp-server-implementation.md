# Edge TCP Server Implementation Plan (Session-Based Security-First)

> **Status:** Completed
> **Last Updated:** 2026-03-01
> **Current Focus:** Done

**Goal:** Implement a Hub-side TCP server to handle Edge device connections using 21-byte header + FlatBuffers, with session-based authentication to prevent forged Hub commands or falsified responses while supporting one-time handshake and multi-request execution.

**Architecture:** Dedicated `EdgeGateway` module in `src/hub`, using Tokio for async TCP handling, RaftRouter for data persistence, and session-based authentication. Devices perform a single AuthHello handshake signed with device key; Hub issues `session_id` + `session_key` with TTL. Subsequent requests are signed using `session_key` and protected by nonce replay checks.

**Tech Stack:** Rust, Tokio, FlatBuffers, OpenRaft, HMAC-SHA256 (truncated), CRC32C, UUID v4.

---

### Task 1: 会话化协议与依赖准备 [Completed]

- [x] Update Cargo dependencies (hmac, sha2, crc32fast)
- [x] Implement sessionized envelope (`SessionMeta`, `encode/decode_auth_hello`)
- [x] Verify with `auth_hello_rejects_tamper` test

**Implementation:** `src/hub/edge_session_schema.rs`

---

### Task 2: EdgeGateway 头部解析与基础校验 [Completed]

- [x] Create `src/hub/edge_gateway.rs`
- [x] Implement header parsing (`validate_header`)
- [x] Verify with `header_rejects_invalid_magic` test

**Implementation:** `src/hub/edge_gateway.rs`

---

### Task 3: 会话管理、重放防护与过期处理 [Completed]

- [x] Implement `SessionManager` with TTL and Group isolation
- [x] Implement `NonceCache` for replay protection
- [x] Verify with `session_expired_is_rejected` test

**Implementation:** `src/hub/edge_gateway.rs`

---

### Task 4: 握手处理与会话建立 [Completed]

- [x] Implement `handle_auth_hello` logic
- [x] Implement `AuthAck` response structure
- [x] Verify with `auth_hello_creates_session` test
- [x] Verify with `auth_hello_rejects_replay` test

**Implementation:** `src/hub/edge_gateway.rs`

---

### Task 5: 会话请求鉴权与重放拒绝 [Completed]

**Files:**
- Modify: `src/hub/edge_gateway.rs`
- Test: `tests/edge_gateway_test.rs`

**Step 1: Write the failing test (会话请求处理)**

```rust
#[test]
fn session_request_is_processed() {
    // create session
    // encode session request (UploadData)
    // handle_session_request -> success
}
```

**Step 2: Implement `handle_session_request`**

- Use `decode_session_request` to verify signature.
- Check nonce with `nonce_cache`.
- Check session validity with `SessionManager`.
- Dispatch to inner `handle_edge_request` (UploadData/Heartbeat).

**Step 3: Run verification**

**Implementation:** `src/hub/edge_gateway.rs`

---

### Task 6: 响应签名与错误反馈防伪造 [Completed]

**Files:**
- Modify: `src/hub/edge_session_schema.rs`
- Modify: `src/hub/edge_gateway.rs`
- Test: `tests/edge_schema_test.rs`

**Step 1: Implement signed response**

```rust
pub struct SignedResponse {
    pub session_id: u64,
    pub timestamp_ms: u64,
    pub nonce: u64, // match request nonce
    pub payload: Vec<u8>,
    pub mac: [u8; 16],
}
pub fn encode_signed_response(...) -> Vec<u8> { ... }
pub fn decode_signed_response(...) -> anyhow::Result<SignedResponse> { ... }
```

**Step 2: Verify with tests**

**Implementation:** `src/hub/edge_gateway.rs` (`pack_signed_response`) & `src/hub/edge_session_schema.rs` (`SignedResponse`)

---

### Task 6.5: 分组映射（device_id -> group_id） [Completed]

- [x] Define group resolver logic
- [x] Wire resolver into `SessionManager`

**Implementation:** `src/hub/edge_gateway.rs` (Integrated into `SessionManager::new_with_resolver`)

---

### Task 7: Server 集成与配置 [Completed]

**Files:**
- Modify: `src/config.rs` (Not needed, using CLI arg)
- Modify: `src/bin/server.rs`
- Modify: `src/hub/edge_gateway.rs` (Implemented `EdgeGateway` struct and run loop)

**Step 1: Add CLI/Config support for Edge Port**
**Step 2: Spawn Edge TCP Server in `server.rs`**

**Implementation:** `src/bin/server.rs` (Added `--edge-port`, spawns `EdgeGateway`), `src/hub/edge_gateway.rs` (Full implementation)

---

### Task 8: Integration Test (防伪造验证) [Completed]

**Files:**
- Create: `tests/edge_tcp_common.rs`
- Create: `tests/edge_tcp_security.rs`
- Create: `tests/edge_tcp_concurrency.rs`
- Create: `tests/edge_tcp_limits.rs`
- Delete: `tests/edge_tcp_integration.rs`

- [x] End-to-end handshake and data upload test
- [x] Tampered payload rejection test
- [x] Parallel session validation
- [x] Oversized payload + invalid header rejection

---

### Task 9: 全量验证（含 lint/typecheck） [Completed]

- Run: `cargo test`
- Run: `cargo check`
- Run: `cargo clippy`

---

### 验证输出摘要

- `cargo test`: 全部通过（包含 edge_tcp_* 分拆测试）
- `cargo check`: 通过
- `cargo clippy`: 通过但有 3 个 clippy::collapsible_if 警告（`src/raft/raft_node.rs`）

---

### 当前已知问题

- `src/raft/raft_node.rs` 存在可折叠 if 的 clippy 警告（未影响功能）
