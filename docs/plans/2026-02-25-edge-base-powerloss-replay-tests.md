# 边缘底座断电与重放测试 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 以 TDD 方式落地边缘底座的断电安全与重放幂等测试，并实现最小可测的 KV 逻辑骨架。  

**Architecture:** 新增 edge 模块（KV + 元数据 + 扫描 + TTL + HWM + ACK），测试驱动实现最小功能；每个测试围绕“断电恢复、幂等重放、前缀扫描、TTL 丢弃”构建。  

**Tech Stack:** Rust 2024, sled, tokio, tempfile

---

### Task 1: 建立 edge 模块骨架与基础类型

**Files:**
- Create: `d:\Rust\check_program\src\edge\mod.rs`
- Create: `d:\Rust\check_program\src\edge\store.rs`
- Modify: `d:\Rust\check_program\src\lib.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn edge_store_can_open_and_close() {
    let dir = tempfile::tempdir().unwrap();
    let store = check_program::edge::store::EdgeStore::open(dir.path()).unwrap();
    drop(store);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q edge_store_can_open_and_close`  
Expected: FAIL with unresolved module `edge`

**Step 3: Write minimal implementation**

```rust
// src/edge/mod.rs
pub mod store;

// src/edge/store.rs
pub struct EdgeStore {
    db: sled::Db,
}

impl EdgeStore {
    pub fn open(path: &std::path::Path) -> anyhow::Result<Self> {
        Ok(Self { db: sled::open(path)? })
    }
}

// src/lib.rs
pub mod edge;
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q edge_store_can_open_and_close`  
Expected: PASS

**Step 5: Commit**

跳过（未被要求提交）

---

### Task 2: A/B 切换断电恢复测试

**Files:**
- Test: `d:\Rust\check_program\tests\edge_ab.rs`
- Modify: `d:\Rust\check_program\src\edge\store.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn ab_switch_persists_active_slot() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = EdgeStore::open(dir.path()).unwrap();
        store.set_active_slot(1).unwrap();
    }
    {
        let store = EdgeStore::open(dir.path()).unwrap();
        assert_eq!(store.get_active_slot().unwrap(), 1);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q ab_switch_persists_active_slot`  
Expected: FAIL with missing methods

**Step 3: Write minimal implementation**

```rust
// store.rs: use single-key metadata
pub fn set_active_slot(&self, slot: u8) -> Result<()> { ... }
pub fn get_active_slot(&self) -> Result<u8> { ... }
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q ab_switch_persists_active_slot`  
Expected: PASS

**Step 5: Commit**

跳过（未被要求提交）

---

### Task 3: 前缀扫描顺序与断点恢复测试

**Files:**
- Test: `d:\Rust\check_program\tests\edge_scan.rs`
- Modify: `d:\Rust\check_program\src\edge\store.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn prefix_scan_returns_sorted_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = EdgeStore::open(dir.path()).unwrap();
    store.put_log(1, 2, b"a".to_vec()).unwrap();
    store.put_log(1, 1, b"b".to_vec()).unwrap();
    let keys = store.scan_log_keys(None).unwrap();
    assert_eq!(keys, vec![store.log_key(1,1), store.log_key(1,2)]);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q prefix_scan_returns_sorted_order`  
Expected: FAIL with missing methods

**Step 3: Write minimal implementation**

```rust
pub fn log_key(&self, ts: u64, seq: u64) -> Vec<u8> { ... } // big-endian
pub fn put_log(&self, ts: u64, seq: u64, value: Vec<u8>) -> Result<()> { ... }
pub fn scan_log_keys(&self, last_key: Option<Vec<u8>>) -> Result<Vec<Vec<u8>>> { ... }
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q prefix_scan_returns_sorted_order`  
Expected: PASS

**Step 5: Commit**

跳过（未被要求提交）

---

### Task 4: TTL 信封丢弃测试

**Files:**
- Test: `d:\Rust\check_program\tests\edge_ttl.rs`
- Modify: `d:\Rust\check_program\src\edge\store.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn ttl_expired_value_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let store = EdgeStore::open(dir.path()).unwrap();
    store.put_with_ttl(0, b"dead".to_vec()).unwrap();
    let val = store.get_with_ttl(0).unwrap();
    assert!(val.is_none());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q ttl_expired_value_is_dropped`  
Expected: FAIL with missing methods

**Step 3: Write minimal implementation**

```rust
pub fn put_with_ttl(&self, expire_at_ms: u64, payload: Vec<u8>) -> Result<()> { ... }
pub fn get_with_ttl(&self, key: u64) -> Result<Option<Vec<u8>>> { ... } // drop if expired
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q ttl_expired_value_is_dropped`  
Expected: PASS

**Step 5: Commit**

跳过（未被要求提交）

---

### Task 5: HWM 与 ACK 幂等重放测试

**Files:**
- Test: `d:\Rust\check_program\tests\edge_hwm.rs`
- Modify: `d:\Rust\check_program\src\edge\store.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn hwm_advances_only_after_state_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let store = EdgeStore::open(dir.path()).unwrap();
    store.persist_state(10).unwrap();
    store.set_hwm(10).unwrap();
    assert_eq!(store.get_hwm().unwrap(), 10);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q hwm_advances_only_after_state_persisted`  
Expected: FAIL with missing methods

**Step 3: Write minimal implementation**

```rust
pub fn persist_state(&self, seq: u64) -> Result<()> { ... }
pub fn set_hwm(&self, seq: u64) -> Result<()> { ... }
pub fn get_hwm(&self) -> Result<u64> { ... }
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q hwm_advances_only_after_state_persisted`  
Expected: PASS

**Step 5: Commit**

跳过（未被要求提交）

---

### Task 6: 全量验证（含 lint/typecheck）

**Files:**
- Test: `tests/edge_*.rs`

**Step 1: Run verification**

Run: `cargo test -q edge_store_can_open_and_close`  
Run: `cargo test -q ab_switch_persists_active_slot`  
Run: `cargo test -q prefix_scan_returns_sorted_order`  
Run: `cargo test -q ttl_expired_value_is_dropped`  
Run: `cargo test -q hwm_advances_only_after_state_persisted`  
Run: `cargo check -q`  
Run: `cargo clippy -q`

**Step 2: Commit**

跳过（未被要求提交）
