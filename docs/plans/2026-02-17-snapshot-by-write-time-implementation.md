# Write-Time Snapshot Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 在不改变对外协议的前提下，实现基于“数据库写入时间”的快照切割与日志压缩基础设施。

**Architecture:**  
在 SQLite 内维护 `_raft_meta` 作为权威写入时间来源，写入成功后更新 `last_write_at`。后台快照调度器周期性检查触发条件，执行 `VACUUM INTO` 生成快照文件，并更新 `last_snapshot_at` 与 `last_snapshot_index`。RaftStore 保持日志索引元数据。

**Tech Stack:** Rust, Tokio, Rusqlite, OpenRaft(预留), sled

---

### Task 1: 元数据表与读写接口（SQLite 侧）

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/actor.rs`
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/engine/sqlite.rs`
- Test: `d:/Rust/check_program/.worktrees/raft-a2/tests/state_machine.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn meta_last_write_at_is_updated() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");
    let sm = check_program::raft::state_machine::SqliteStateMachine::new(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap();

    let _ = sm.apply_write("CREATE TABLE t(x INT)".to_string()).await.unwrap();
    let last_write_at = sm.query_scalar("SELECT value FROM _raft_meta WHERE key='last_write_at'".to_string())
        .await
        .unwrap();
    assert!(!last_write_at.is_empty());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q meta_last_write_at_is_updated`  
Expected: FAIL（表不存在或 key 不存在）

**Step 3: Write minimal implementation**

```rust
// in actor.rs: 初始化时创建 _raft_meta 表
self.conn.execute(
    "CREATE TABLE IF NOT EXISTS _raft_meta (key TEXT PRIMARY KEY, value TEXT)",
    [],
)?;

// in actor.rs: 新增 DbMessage::UpdateMeta 与 DbHandle::update_meta
// in DbActor::handle_message: 执行 UPSERT
self.conn.execute(
    "INSERT INTO _raft_meta(key, value) VALUES (?1, ?2)
     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
    rusqlite::params![key, value],
)?;
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q meta_last_write_at_is_updated`  
Expected: PASS

**Step 5: Commit**

```bash
git add src/actor.rs src/engine/sqlite.rs tests/state_machine.rs
git commit -m "feat: add raft meta table and update hook"
```

---

### Task 2: 写入后更新 last_write_at（数据库时间）

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/actor.rs`
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/state_machine.rs`
- Test: `d:/Rust/check_program/.worktrees/raft-a2/tests/state_machine.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn last_write_at_uses_sqlite_time() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");
    let sm = check_program::raft::state_machine::SqliteStateMachine::new(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap();

    let _ = sm.apply_write("CREATE TABLE t(x INT)".to_string()).await.unwrap();
    let v1 = sm.query_scalar("SELECT value FROM _raft_meta WHERE key='last_write_at'".to_string())
        .await
        .unwrap();
    let _ = sm.apply_write("INSERT INTO t(x) VALUES (1)".to_string()).await.unwrap();
    let v2 = sm.query_scalar("SELECT value FROM _raft_meta WHERE key='last_write_at'".to_string())
        .await
        .unwrap();
    assert!(v2 >= v1);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q last_write_at_uses_sqlite_time`  
Expected: FAIL（未更新或值为空）

**Step 3: Write minimal implementation**

```rust
// in actor.rs: 新增获取 SQLite 时间的消息
// SELECT strftime('%s','now') as unix_time;

// in state_machine.rs: apply_write 后调用 update_meta("last_write_at", sqlite_time)
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q last_write_at_uses_sqlite_time`  
Expected: PASS

**Step 5: Commit**

```bash
git add src/actor.rs src/raft/state_machine.rs tests/state_machine.rs
git commit -m "feat: update last_write_at after apply_write"
```

---

### Task 3: RaftStore 快照索引记录

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/store.rs`
- Test: `d:/Rust/check_program/.worktrees/raft-a2/tests/raft_store.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn raft_store_persists_snapshot_index() {
    let dir = tempfile::tempdir().unwrap();
    let store = check_program::raft::store::RaftStore::open(dir.path()).unwrap();
    store.set_last_snapshot_index(42).unwrap();
    let reopened = check_program::raft::store::RaftStore::open(dir.path()).unwrap();
    assert_eq!(reopened.get_last_snapshot_index().unwrap(), 42);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q raft_store_persists_snapshot_index`  
Expected: FAIL（方法不存在）

**Step 3: Write minimal implementation**

```rust
pub fn set_last_snapshot_index(&self, value: u64) -> Result<()> { ... }
pub fn get_last_snapshot_index(&self) -> Result<u64> { ... }
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q raft_store_persists_snapshot_index`  
Expected: PASS

**Step 5: Commit**

```bash
git add src/raft/store.rs tests/raft_store.rs
git commit -m "feat: persist last snapshot index in raft store"
```

---

### Task 4: 快照调度器与 VACUUM INTO

**Files:**
- Create: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/snapshot.rs`
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/mod.rs`
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/src/raft/router.rs`
- Test: `d:/Rust/check_program/.worktrees/raft-a2/tests/raft_snapshot.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn snapshot_created_when_time_exceeds_window() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");
    let snapshot_dir = dir.path().join("snapshots");
    std::fs::create_dir_all(&snapshot_dir).unwrap();

    let router = check_program::raft::router::Router::new_local_leader(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap();

    let _ = router.write("CREATE TABLE t(x INT)".to_string()).await.unwrap();
    // 手动设置 last_snapshot_at 为更早的时间，模拟 24h 窗口
    let _ = router.write("INSERT INTO _raft_meta(key,value) VALUES('last_snapshot_at','0')".to_string())
        .await
        .unwrap();

    let scheduler = check_program::raft::snapshot::SnapshotScheduler::new(
        router,
        snapshot_dir.to_string_lossy().to_string(),
        60 * 60 * 24,
        1000,
    );

    scheduler.tick_once().await.unwrap();

    let entries = std::fs::read_dir(&snapshot_dir).unwrap().count();
    assert!(entries > 0);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q snapshot_created_when_time_exceeds_window`  
Expected: FAIL（模块/类型不存在）

**Step 3: Write minimal implementation**

```rust
pub struct SnapshotScheduler { ... }
impl SnapshotScheduler {
    pub async fn tick_once(&self) -> Result<()> {
        // 读取 last_write_at/last_snapshot_at/last_snapshot_index
        // 判断是否触发
        // VACUUM INTO snapshot_*.db
        // 更新 _raft_meta 与 raft store index
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q snapshot_created_when_time_exceeds_window`  
Expected: PASS

**Step 5: Commit**

```bash
git add src/raft/snapshot.rs src/raft/mod.rs src/raft/router.rs tests/raft_snapshot.rs
git commit -m "feat: add write-time snapshot scheduler"
```

---

### Task 5: 端到端校验

**Files:**
- Test: `d:/Rust/check_program/.worktrees/raft-a2/tests/raft_integration.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn snapshot_persists_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("node.db");
    let snapshot_dir = dir.path().join("snapshots");
    std::fs::create_dir_all(&snapshot_dir).unwrap();

    let router = check_program::raft::router::Router::new_local_leader(
        db_path.to_string_lossy().to_string(),
    )
    .unwrap();
    let _ = router.write("CREATE TABLE t(x INT)".to_string()).await.unwrap();
    // 强制触发快照
    // ...
    drop(router);

    let entries = std::fs::read_dir(&snapshot_dir).unwrap().count();
    assert!(entries > 0);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -q snapshot_persists_after_restart`  
Expected: FAIL（逻辑未完成或未触发）

**Step 3: Write minimal implementation**

```rust
// 补足 snapshot 触发条件与更新路径
```

**Step 4: Run test to verify it passes**

Run: `cargo test -q snapshot_persists_after_restart`  
Expected: PASS

**Step 5: Commit**

```bash
git add tests/raft_integration.rs
git commit -m "test: cover snapshot persistence"
```

