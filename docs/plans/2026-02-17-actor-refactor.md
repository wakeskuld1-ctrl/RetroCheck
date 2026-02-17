# Distributed Transaction Actor Model Refactoring Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:** Refactor the existing synchronous SQLite coordinator into an asynchronous Actor model using Tokio to support parallel execution and future network capabilities.

**Architecture:** 
- **DbActor:** Encapsulates a `rusqlite::Connection` in a dedicated thread/task, communicating via MPSC channels.
- **Coordinator:** Manages `DbActor` handles and orchestrates 2PC using `join_all` for parallelism.
- **Message Protocol:** Define `Prepare`, `Commit`, `Rollback` messages with `oneshot` response channels.

**Tech Stack:** Rust, tokio (full features), rusqlite, anyhow.

---

### Task 1: Add Tokio Dependencies

**Files:**
- Modify: `Cargo.toml`

**Step 1: Update Cargo.toml**

Add `tokio` with `full` features.

```toml
[dependencies]
tokio = { version = "1.43", features = ["full"] }
# ... existing dependencies ...
```

---

### Task 2: Define Actor Messages and Structs

**Files:**
- Create: `src/actor.rs`
- Modify: `src/main.rs` (to register module)

**Step 1: Create src/actor.rs with Message Enum**

```rust
use tokio::sync::{mpsc, oneshot};
use rusqlite::{Connection, Result, params};
use std::path::PathBuf;
use crate::config::NodeRole;
use anyhow::anyhow;

#[derive(Debug)]
pub enum DbMessage {
    CheckHealth {
        resp: oneshot::Sender<anyhow::Result<()>>,
    },
    Execute {
        sql: String,
        resp: oneshot::Sender<anyhow::Result<usize>>, // Returns affected rows
    },
    Prepare {
        sql: String,
        // We need to pass args. For simplicity, let's serialize args to JSON or use a specific struct?
        // rusqlite Params are tricky to send.
        // Let's stick to string params for this demo or use a simplified enum.
        // Or better: pass a closure? No, closure not Send.
        // Let's use `Vec<String>` for params for now to prove the concept.
        args: Vec<String>, 
        tx_id: String,
        resp: oneshot::Sender<anyhow::Result<()>>,
    },
    Commit {
        tx_id: String,
        resp: oneshot::Sender<anyhow::Result<()>>,
    },
    Rollback {
        tx_id: String,
        resp: oneshot::Sender<anyhow::Result<()>>,
    },
    GetVersion {
        table: String,
        resp: oneshot::Sender<anyhow::Result<i32>>,
    }
}

pub struct DbActor {
    receiver: mpsc::Receiver<DbMessage>,
    conn: Connection,
    path: String,
    role: NodeRole,
    node_id: String,
    // We need to store the active transaction.
    // Since `Transaction` borrows `conn`, we can't store it easily in the struct if `conn` is also there.
    // We have to use "unsafe" or better: Ouroboros / Self-referential? 
    // OR: Just keep the `Transaction` in the loop scope?
    // But Prepare and Commit are separate messages.
    // SOLUTION: Use `conn.unchecked_transaction()` (if available) or raw pointers?
    // CLEANER SOLUTION: Don't hold `rusqlite::Transaction` object across await points.
    // Instead, use raw SQL `BEGIN`, `COMMIT`, `ROLLBACK` for the manual 2PC control.
    // SQLite supports `BEGIN`, `COMMIT`. We just need to ensure no other statements intervene.
    // Since Actor processes messages sequentially, this is safe!
}

impl DbActor {
    pub fn new(path: String, role: NodeRole, node_id: String, receiver: mpsc::Receiver<DbMessage>) -> anyhow::Result<Self> {
        let conn = Connection::open(&path)?;
        // Set WAL mode
        let _ = conn.execute("PRAGMA journal_mode=WAL;", []);
        let _ = conn.execute("PRAGMA busy_timeout=5000;", []);
        
        Ok(Self {
            receiver,
            conn,
            path,
            role,
            node_id,
        })
    }

    pub async fn run(mut self) {
        while let Some(msg) = self.receiver.recv().await {
            match msg {
                DbMessage::CheckHealth { resp } => {
                    let res = self.conn.execute("SELECT 1", []).map(|_| ()).map_err(|e| e.into());
                    let _ = resp.send(res);
                },
                DbMessage::Execute { sql, resp } => {
                    let res = self.conn.execute(&sql, []).map_err(|e| e.into());
                    let _ = resp.send(res);
                },
                DbMessage::Prepare { sql, args, tx_id: _, resp } => {
                    // Manual Transaction Control
                    // 1. BEGIN IMMEDIATE
                    let begin_res = self.conn.execute("BEGIN IMMEDIATE", []);
                    if let Err(e) = begin_res {
                        let _ = resp.send(Err(e.into()));
                        continue;
                    }

                    // 2. Execute SQL
                    // Convert String args to dyn ToSql
                    let params: Vec<&dyn rusqlite::ToSql> = args.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
                    let exec_res = self.conn.execute(&sql, &*params);
                    
                    match exec_res {
                        Ok(_) => {
                            let _ = resp.send(Ok(()));
                        },
                        Err(e) => {
                            // Auto rollback on failure
                            let _ = self.conn.execute("ROLLBACK", []);
                            let _ = resp.send(Err(e.into()));
                        }
                    }
                },
                DbMessage::Commit { tx_id: _, resp } => {
                    let res = self.conn.execute("COMMIT", []).map(|_| ()).map_err(|e| e.into());
                    let _ = resp.send(res);
                },
                DbMessage::Rollback { tx_id: _, resp } => {
                    let res = self.conn.execute("ROLLBACK", []).map(|_| ()).map_err(|e| e.into());
                    let _ = resp.send(res);
                },
                DbMessage::GetVersion { table, resp } => {
                    let sql = format!("SELECT MAX(version) FROM {}", table);
                    let res: Result<i32, rusqlite::Error> = self.conn.query_row(&sql, [], |row| row.get(0));
                    // Handle NULL (empty table) as 0
                    let val = match res {
                        Ok(v) => Ok(v),
                        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
                        Err(e) => Err(e.into()),
                    };
                    let _ = resp.send(val);
                }
            }
        }
    }
}
```

**Step 2: Register in main.rs**

Add `mod actor;` to `src/main.rs`.

---

### Task 3: Create Actor Handle for Client Usage

**Files:**
- Modify: `src/actor.rs`

**Step 1: Implement DbHandle**

Create a wrapper struct `DbHandle` that holds `mpsc::Sender<DbMessage>`.
Implement methods like `prepare`, `commit`, `rollback` that send messages and await responses.

```rust
#[derive(Clone)]
pub struct DbHandle {
    sender: mpsc::Sender<DbMessage>,
    pub node_id: String,
    pub role: NodeRole,
}

impl DbHandle {
    pub async fn prepare(&self, sql: String, args: Vec<String>, tx_id: String) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender.send(DbMessage::Prepare { sql, args, tx_id, resp: tx }).await?;
        rx.await?
    }
    
    pub async fn commit(&self, tx_id: String) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender.send(DbMessage::Commit { tx_id, resp: tx }).await?;
        rx.await?
    }

    // ... implement others ...
}
```

---

### Task 4: Refactor Coordinator to Use Actors

**Files:**
- Modify: `src/coordinator.rs`
- Modify: `src/db_node.rs` (Remove DbNode logic or replace with DbHandle creation)

**Step 1: Update MultiDbCoordinator**

Replace `DbNode` fields with `DbHandle`.
Use `tokio::spawn` to start actors in `new()`.

```rust
pub struct MultiDbCoordinator {
    config: ClusterConfig,
    master: DbHandle,
    backups: Vec<DbHandle>,
    wal: WalManager,
}
```

**Step 2: Rewrite atomic_write to be async**

```rust
pub async fn atomic_write(&mut self, sql: &str, params: Vec<String>) -> Result<()> {
    // ...
    // Parallel Prepare
    let mut futures = Vec::new();
    futures.push(self.master.prepare(sql.to_string(), params.clone(), tx_id.clone()));
    for b in &self.backups {
        futures.push(b.prepare(sql.to_string(), params.clone(), tx_id.clone()));
    }
    
    let results = futures::future::join_all(futures).await;
    
    // ... Process results ...
}
```

---

### Task 5: Async Main and Test

**Files:**
- Modify: `src/main.rs`

**Step 1: Make main async**

```rust
#[tokio::main]
async fn main() -> Result<()> {
    // ...
}
```

**Step 2: Verify functionality**

Run the same test flow but with `await` calls.
