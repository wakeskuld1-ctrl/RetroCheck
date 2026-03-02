# 2026-03-01 Prioritized Roadmap: "The Blind Edge" Architecture Realization

This roadmap addresses the critical gaps between the "Schema-less Edge Synchronization Base" architectural blueprint and the current implementation.

## 🚨 Priority 1: Critical Architecture Fixes (The "Heart")
*Must be completed before any load testing or deployment.*

### 1. Hub "Smart Batcher" (Micro-batching)
**Gap:** The current `EdgeGateway` performs a direct `router.write()` for every incoming request. In the "8 o'clock I/O flood" scenario, this will saturate the Raft engine with thousands of tiny log entries, leading to consensus timeout and cluster collapse.
**Goal:** Implement the "Dam" to protect the Raft engine.
**Tasks:**
- [ ] Create `SmartBatcher` struct (Actor-like) with `mpsc::channel(1024)`.
- [ ] Implement buffering logic: Flush on `5ms` timeout OR `500` items count.
- [ ] Compress buffered requests into a single `RaftRequest::Proposal`.
- [ ] Implement `oneshot` fan-out mechanism to notify waiting TCP connections after the batch is committed.

### 2. Network "Ruthless Statelessness" (Active Close)
**Gap:** The current TCP server (`handle_connection`) keeps the connection open and waits for the next request (`while let Some(result) = framed.next().await`). This violates the "Edge is a Blind/Undead" philosophy where connections should be ephemeral to prevent state oscillation in weak networks.
**Goal:** Force connection termination after transaction completion.
**Tasks:**
- [ ] Modify `handle_connection` to `break` the loop immediately after sending `MSG_TYPE_RESPONSE`.
- [ ] Ensure the TCP stream is shutdown gracefully but firmly.
- [ ] Add configuration flag `enforce_stateless_interaction` (default: true) to allow debugging but enforce production rigor.

## 🟠 Priority 2: Resilience & Correctness (The "Immune System")
*Must be completed to verify the distributed system guarantees.*

### 3. Chaos Laboratory (Deterministic Simulation)
**Gap:** Current tests are standard `tokio::test` happy paths. They cannot reproduce "Split Brain", "Packet Loss", or "Time Skew" bugs which are guaranteed to occur in edge environments.
**Goal:** Introduce deterministic simulation.
**Tasks:**
- [ ] Introduce `turmoil` dependency (dev-dependency).
- [ ] Port `raft_integration.rs` to run inside a `turmoil::Builder` simulation.
- [ ] Create specific "Network Poison" scenarios:
    - Partition the Leader from Followers.
    - Drop 50% of packets during Snapshot transfer.
    - Delay packets by 500ms to trigger election timeouts.

## 🟡 Priority 3: Optimization & Isolation (The "Muscle")
*Performance enhancements for scale.*

### 4. Edge "Physical TTL" (Binary Envelope)
**Gap:** `CmdRunner` currently deserializes the full payload before checking the timestamp. This wastes I/O and memory on expired packets.
**Goal:** "Zero Schema" rejection.
**Tasks:**
- [ ] Refactor `EdgeStore` to support "Header-only" reads (using `sled`'s lazy retrieval or separate key layout if necessary, but `sled` loads pages, so optimization might be at the `bincode`/parsing level).
- [ ] *Optimization:* Store Expiry as a fixed-width prefix in the Value, or part of the Key.
- [ ] Implement `scan_headers_only` to check TTL without loading the 1MB payload.

### 5. Hub Read/Write Isolation
**Gap:** Complex AI/Reporting queries on SQLite will block the single Writer (Raft applier), causing backpressure on the Edge Gateway.
**Goal:** Physical isolation of heavy reads.
**Tasks:**
- [ ] Enable WAL mode on SQLite (if not already).
- [ ] Implement "DuckDB Read-Only Mount": Use DuckDB to attach to the SQLite file for analytical queries (`ATTACH 'hub.db' AS hub (TYPE SQLITE)`).
- [ ] OR: Use SQLite `Expression Indexes` to extract JSON fields into "Generated Columns" for fast filtering without full JSON parsing.
