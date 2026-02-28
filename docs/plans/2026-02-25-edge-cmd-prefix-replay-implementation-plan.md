# Edge CMD Prefix Replay Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 为 edge 增加 cmd 前缀与断电重放能力，支持可排序 cmd 键、断点续扫、过期丢弃与坏包清理。

**Architecture:** 在现有 EdgeStore 中新增 cmd 命名空间（cmd 前缀 + cmd_last_key 元数据），key 使用 (ts, seq) 大端编码保证字典序等于时间序；扫描与重放使用断点续扫；TTL 以 value 前 8 字节为过期时间戳，过期则直接物理删除并跳过。

**Tech Stack:** Rust 2024, sled, tempfile, anyhow

---

### Task 1: Cmd 前缀扫描与断点语义测试

**Files:**
- Create: `d:\Rust\check_program\tests\edge_cmd.rs`
- Modify: `d:\Rust\check_program\src\edge\store.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn cmd_prefix_scan_respects_order_and_breakpoint() {
    let dir = tempfile::tempdir().unwrap();
    let store = check_program::edge::store::EdgeStore::open(dir.path()).unwrap();
    store.put_cmd(1, 2, 0, b"b".to_vec()).unwrap();
    store.put_cmd(1, 1, 0, b"a".to_vec()).unwrap();
    let keys = store.scan_cmd_keys(None).unwrap();
    assert_eq!(keys, vec![store.cmd_key(1, 1), store.cmd_key(1, 2)]);

    let last = store.cmd_key(1, 1);
    let keys = store.scan_cmd_keys(Some(last)).unwrap();
    assert_eq!(keys, vec![store.cmd_key(1, 2)]);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q cmd_prefix_scan_respects_order_and_breakpoint`  
Expected: FAIL with missing methods `put_cmd/cmd_key/scan_cmd_keys`

**Step 3: Write minimal implementation**

```rust
pub fn cmd_key(&self, ts: u64, seq: u64) -> Vec<u8> { ... }
pub fn put_cmd(&self, ts: u64, seq: u64, expire_at_ms: u64, payload: Vec<u8>) -> Result<()> { ... }
pub fn scan_cmd_keys(&self, last_key: Option<Vec<u8>>) -> Result<Vec<Vec<u8>>> { ... }
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q cmd_prefix_scan_respects_order_and_breakpoint`  
Expected: PASS

**Step 5: Commit**

跳过（未被要求提交）

---

### Task 2: Cmd TTL 与坏包丢弃测试

**Files:**
- Modify: `d:\Rust\check_program\tests\edge_cmd.rs`
- Modify: `d:\Rust\check_program\src\edge\store.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn cmd_payload_expired_or_broken_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let store = check_program::edge::store::EdgeStore::open(dir.path()).unwrap();
    store.put_cmd(2, 1, 0, b"dead".to_vec()).unwrap();
    let payload = store.get_cmd_payload(&store.cmd_key(2, 1)).unwrap();
    assert!(payload.is_none());

    let broken_key = store.cmd_key(2, 2);
    store.put_cmd_raw(broken_key.clone(), vec![1, 2, 3]).unwrap();
    let payload = store.get_cmd_payload(&broken_key).unwrap();
    assert!(payload.is_none());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q cmd_payload_expired_or_broken_is_dropped`  
Expected: FAIL with missing methods `get_cmd_payload/put_cmd_raw`

**Step 3: Write minimal implementation**

```rust
pub fn put_cmd_raw(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> { ... }
pub fn get_cmd_payload(&self, key: &[u8]) -> Result<Option<Vec<u8>>> { ... } // delete if expired/broken
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q cmd_payload_expired_or_broken_is_dropped`  
Expected: PASS

**Step 5: Commit**

跳过（未被要求提交）

---

### Task 3: Cmd 断点持久化与重放测试

**Files:**
- Modify: `d:\Rust\check_program\tests\edge_cmd.rs`
- Modify: `d:\Rust\check_program\src\edge\store.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn cmd_replay_resumes_from_last_key() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = check_program::edge::store::EdgeStore::open(dir.path()).unwrap();
        store.put_cmd(3, 1, 0, b"c1".to_vec()).unwrap();
        store.put_cmd(3, 2, 0, b"c2".to_vec()).unwrap();
        store.set_cmd_last_key(store.cmd_key(3, 1)).unwrap();
    }
    {
        let store = check_program::edge::store::EdgeStore::open(dir.path()).unwrap();
        let keys = store.scan_cmd_keys(store.get_cmd_last_key().unwrap()).unwrap();
        assert_eq!(keys, vec![store.cmd_key(3, 2)]);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q cmd_replay_resumes_from_last_key`  
Expected: FAIL with missing methods `set_cmd_last_key/get_cmd_last_key`

**Step 3: Write minimal implementation**

```rust
pub fn set_cmd_last_key(&self, key: Vec<u8>) -> Result<()> { ... }
pub fn get_cmd_last_key(&self) -> Result<Option<Vec<u8>>> { ... }
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q cmd_replay_resumes_from_last_key`  
Expected: PASS

**Step 5: Commit**

跳过（未被要求提交）

---

### Task 4: 验证

**Files:**
- Test: `d:\Rust\check_program\tests\edge_cmd.rs`

**Step 1: Run cmd tests**

Run: `cargo test -q edge_cmd`  
Expected: PASS

**Step 2: Run check**

Run: `cargo check -q`  
Expected: PASS

**Step 3: Run clippy**

Run: `cargo clippy -q`  
Expected: PASS (若有既有警告，记录但不阻塞)

**Step 4: Commit**

跳过（未被要求提交）
