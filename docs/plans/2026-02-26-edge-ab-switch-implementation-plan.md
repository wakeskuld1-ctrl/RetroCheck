# Edge A/B 交替抹除实现计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 落地 Edge A/B 双实例交替抹除，支持组合阈值触发与断电恢复。

**Architecture:** 使用 `slot_0/slot_1` 两个 sled 实例作为数据槽位，`meta` 作为独立元数据槽位；写入仅落 `active_slot`，切换时更新元数据并抹除旧槽位。

**Tech Stack:** Rust, sled, anyhow, tempfile, tokio (tests)

---

### Task 1: 编写切换触发的失败测试

**Files:**
- Modify: `d:/Rust/check_program/tests/edge_ab.rs`

**Step 1: 写失败测试**

```rust
#[test]
fn ab_switch_triggers_on_count_or_time_or_bytes() {
    let dir = tempdir().unwrap();
    let store = EdgeStore::open(dir.path()).unwrap();
    store.set_switch_policy(EdgeSwitchPolicy {
        max_write_count: 2,
        max_bytes_written: 10,
        max_duration_ms: 1,
    }).unwrap();
    store.put_log(1, 1, vec![0u8; 4]).unwrap();
    store.put_log(1, 2, vec![0u8; 4]).unwrap();
    assert_ne!(store.get_active_slot().unwrap(), 0);
}
```

**Step 2: 运行测试确认失败**

Run: `cargo test edge_ab::ab_switch_triggers_on_count_or_time_or_bytes -q`  
Expected: FAIL (缺少 set_switch_policy 与切换逻辑)

**Step 3: 暂不实现**

**Step 4: 再次运行测试确认仍失败**

Run: `cargo test edge_ab::ab_switch_triggers_on_count_or_time_or_bytes -q`  
Expected: FAIL

---

### Task 2: 编写断电恢复相关失败测试

**Files:**
- Modify: `d:/Rust/check_program/tests/edge_ab.rs`

**Step 1: 写失败测试**

```rust
#[test]
fn ab_recovery_handles_next_active_slot_only() {
    let dir = tempdir().unwrap();
    {
        let store = EdgeStore::open(dir.path()).unwrap();
        store.debug_set_next_active_slot(1).unwrap();
    }
    let store = EdgeStore::open(dir.path()).unwrap();
    assert_eq!(store.get_active_slot().unwrap(), 0);
}
```

**Step 2: 运行测试确认失败**

Run: `cargo test edge_ab::ab_recovery_handles_next_active_slot_only -q`  
Expected: FAIL (缺少 debug_set_next_active_slot 与恢复逻辑)

**Step 3: 暂不实现**

**Step 4: 再次运行测试确认仍失败**

Run: `cargo test edge_ab::ab_recovery_handles_next_active_slot_only -q`  
Expected: FAIL

---

### Task 3: 实现 A/B 元数据与双实例管理

**Files:**
- Modify: `d:/Rust/check_program/src/edge/store.rs`

**Step 1: 最小实现 meta_db + slot_db 管理**

```rust
pub struct EdgeStore {
    meta: sled::Db,
    slot_0: sled::Db,
    slot_1: sled::Db,
}
```

**Step 2: 添加元数据键与默认恢复逻辑**

```rust
const ACTIVE_SLOT_KEY: &str = "edge_active_slot";
const NEXT_ACTIVE_SLOT_KEY: &str = "edge_next_active_slot";
const WRITE_COUNT_KEY: &str = "edge_write_count";
const BYTES_WRITTEN_KEY: &str = "edge_bytes_written";
const LAST_SWITCH_MS_KEY: &str = "edge_last_switch_ms";
```

**Step 3: 写入时只落 active_slot**

```rust
fn active_db(&self) -> &sled::Db { /* ... */ }
```

**Step 4: 实现 set_switch_policy / 计数器更新**

```rust
pub struct EdgeSwitchPolicy { /* ... */ }
pub fn set_switch_policy(&self, policy: EdgeSwitchPolicy) -> Result<()> { /* ... */ }
```

**Step 5: 运行失败测试确认仍失败**

Run: `cargo test edge_ab::ab_switch_triggers_on_count_or_time_or_bytes -q`  
Expected: FAIL (切换逻辑尚未实现)

---

### Task 4: 实现切换流程与旧槽抹除

**Files:**
- Modify: `d:/Rust/check_program/src/edge/store.rs`

**Step 1: 切换流程实现**

```rust
fn maybe_switch_slot(&self, incoming_bytes: usize) -> Result<bool> { /* ... */ }
```

**Step 2: 物理抹除旧槽**

```rust
fn wipe_slot(&self, slot: u8) -> Result<()> { /* 删除目录并重建 */ }
```

**Step 3: 运行切换触发测试**

Run: `cargo test edge_ab::ab_switch_triggers_on_count_or_time_or_bytes -q`  
Expected: PASS

---

### Task 5: 实现断电恢复语义与调试入口

**Files:**
- Modify: `d:/Rust/check_program/src/edge/store.rs`

**Step 1: 启动恢复逻辑**

```rust
fn recover_ab_state(&self) -> Result<()> { /* 处理 next_active_slot */ }
```

**Step 2: 测试调试入口**

```rust
pub fn debug_set_next_active_slot(&self, slot: u8) -> Result<()> { /* 仅测试使用 */ }
```

**Step 3: 运行恢复测试**

Run: `cargo test edge_ab::ab_recovery_handles_next_active_slot_only -q`  
Expected: PASS

---

### Task 6: 回归现有 Edge 测试

**Files:**
- Test: `d:/Rust/check_program/tests/edge_ab.rs`
- Test: `d:/Rust/check_program/tests/edge_scan.rs`
- Test: `d:/Rust/check_program/tests/edge_ttl.rs`
- Test: `d:/Rust/check_program/tests/edge_hwm.rs`
- Test: `d:/Rust/check_program/tests/edge_cmd.rs`

**Step 1: 运行 Edge 测试全集**

Run: `cargo test edge_ -q`  
Expected: PASS

---

### Task 7: 质量检查

**Step 1: 运行 lint/typecheck（若存在命令）**

Run: `cargo clippy -q`  
Expected: PASS

Run: `cargo check -q`  
Expected: PASS

---

**备注**
- 由于未收到提交指令，本计划不包含 `git commit` 步骤。
