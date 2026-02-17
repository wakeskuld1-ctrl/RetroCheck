# gRPC Multi-Engine Refactoring Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Refactor the current local Actor-based distributed transaction system into a gRPC-based service architecture with support for multiple storage engines (SQLite, DuckDB).

**Architecture:**
- **gRPC Service**: Defines `transaction.proto` for distributed transaction operations (Prepare, Commit, Rollback).
- **Storage Engine Abstraction**: `StorageEngine` trait to decouple transaction logic from underlying database.
- **Server**: A standalone binary that loads a specific engine (SQLite/DuckDB) based on CLI args and exposes gRPC interface.
- **Client/Coordinator**: Connects to multiple remote gRPC servers to orchestrate distributed transactions using 2PC/Quorum.

**Tech Stack:** Rust, Tonic (gRPC), Prost, Tokio, Clap, Async-Trait, Rusqlite.

---

### Task 1: Project Restructuring & Dependencies

**Files:**
- Modify: `Cargo.toml`
- Create: `build.rs`
- Create: `proto/transaction.proto`
- Create: `src/lib.rs`
- Move: `src/actor.rs`, `src/wal.rs`, `src/config.rs` -> `src/` (exposed via lib)

**Step 1: Add Dependencies**
Run:
```bash
cargo add tonic prost tokio-stream clap --features clap/derive
cargo add tonic-build --build
cargo add async-trait
```

**Step 2: Create Proto Definition**
Create `proto/transaction.proto`:
```protobuf
syntax = "proto3";
package transaction;

service DatabaseService {
  rpc Execute (ExecuteRequest) returns (ExecuteResponse);
  rpc Prepare (PrepareRequest) returns (PrepareResponse);
  rpc Commit (CommitRequest) returns (CommitResponse);
  rpc Rollback (RollbackRequest) returns (RollbackResponse);
  rpc GetVersion (GetVersionRequest) returns (GetVersionResponse);
  rpc CheckHealth (Empty) returns (Empty);
}

message ExecuteRequest { string sql = 1; }
message ExecuteResponse { int32 rows_affected = 1; }

message PrepareRequest {
  string tx_id = 1;
  string sql = 2;
  repeated string args = 3;
}
message PrepareResponse {}

message CommitRequest { string tx_id = 1; }
message CommitResponse {}

message RollbackRequest { string tx_id = 1; }
message RollbackResponse {}

message GetVersionRequest { string table = 1; }
message GetVersionResponse { int32 version = 1; }

message Empty {}
```

**Step 3: Create Build Script**
Create `build.rs`:
```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::compile_protos("proto/transaction.proto")?;
    Ok(())
}
```

**Step 4: Restructure `src`**
- Move `actor.rs`, `wal.rs`, `config.rs` to `src/lib.rs` modules.
- Ensure `lib.rs` exposes `pub mod wal; pub mod config;`.
- Add `pub mod pb { tonic::include_proto!("transaction"); }` to `lib.rs`.

---

### Task 2: Storage Engine Abstraction

**Files:**
- Create: `src/engine/mod.rs`
- Create: `src/engine/sqlite.rs`
- Modify: `src/lib.rs`

**Step 1: Define Engine Trait**
In `src/engine/mod.rs`:
```rust
use async_trait::async_trait;
use anyhow::Result;

#[async_trait]
pub trait StorageEngine: Send + Sync {
    async fn execute(&self, sql: &str) -> Result<usize>;
    async fn prepare(&self, tx_id: &str, sql: &str, args: Vec<String>) -> Result<()>;
    async fn commit(&self, tx_id: &str) -> Result<()>;
    async fn rollback(&self, tx_id: &str) -> Result<()>;
    async fn get_version(&self, table: &str) -> Result<i32>;
    async fn check_health(&self) -> Result<()>;
}
```

**Step 2: Port SQLite Actor to Engine**
In `src/engine/sqlite.rs`:
- Adapt `DbActor` logic to implement `StorageEngine`.
- Use the existing `mpsc` pattern internally if needed for serialization, or use `tokio::sync::Mutex<Connection>` if `Connection` is not `Sync`.
- Since `rusqlite::Connection` is `!Sync`, we MUST keep the Actor pattern or use a Mutex.
- **Decision**: Wrap the `DbHandle` (Client-side of Actor) as the implementation of `StorageEngine`. The `DbHandle` already sends messages to the background `DbActor`.

---

### Task 3: gRPC Server Implementation

**Files:**
- Create: `src/bin/server.rs`

**Step 1: Implement Service Trait**
- Implement `transaction::database_service_server::DatabaseService` for a struct `DbServiceImpl` that holds a `Box<dyn StorageEngine>`.

**Step 2: Implement Main with Clap**
- Parse args: `--port`, `--db`, `--engine`.
- Instantiate `SqliteEngine` (which spawns `DbActor`).
- Start `Tonic` server.

---

### Task 4: Client Refactoring

**Files:**
- Create: `src/bin/client.rs` (Move `main.rs` content here)
- Modify: `src/coordinator.rs`

**Step 1: Update Coordinator**
- Replace `DbHandle` with `transaction::database_service_client::DatabaseServiceClient<tonic::transport::Channel>`.
- Update `atomic_write` to call gRPC methods.

**Step 2: Update Client Main**
- Update `ClusterConfig` to use URLs (e.g., `http://127.0.0.1:50051`).
- Connect to servers instead of spawning local actors.

---

### Task 5: Integration Verification

**Step 1: Run Servers**
- Terminal 1: `./target/debug/server --port 50051 --db data/master.db --engine sqlite`
- Terminal 2: `./target/debug/server --port 50052 --db data/backup1.db --engine sqlite`
- Terminal 3: `./target/debug/server --port 50053 --db data/backup2.db --engine sqlite`

**Step 2: Run Client**
- Terminal 4: `./target/debug/client`

**Step 3: Verify Output**
- Check consistency and logs.
