# Task Plan: "The Blind Edge" Architecture Realization

## Goal
Implement the critical missing components of the "Schema-less Edge Synchronization Base" to ensure stability, resilience, and performance.

**Detailed Roadmap:** [docs/plans/2026-03-01-prioritized-roadmap.md](docs/plans/2026-03-01-prioritized-roadmap.md)

## Phases

### Phase 1: Critical Architecture Fixes (The "Heart")
- [ ] **Hub Smart Batcher**: Implement `SmartBatcher` with `mpsc::channel(1024)`, 5ms/500 item flush, and Raft proposal compression.
- [ ] **Network Statelessness**: Enforce active connection closure after response in `EdgeGateway`.

### Phase 2: Resilience & Correctness (The "Immune System")
- [ ] **Chaos Testing**: Introduce `turmoil` and port integration tests to deterministic simulation.
- [ ] **Network Poison Scenarios**: Implement partition, drop, and delay tests.

### Phase 3: Optimization & Isolation (The "Muscle")
- [ ] **Edge Physical TTL**: Refactor `EdgeStore` for header-only TTL checks.
- [ ] **Hub R/W Isolation**: Implement DuckDB read-only mount or SQLite Expression Indexes.

## Current Status
- Roadmap created and prioritized.
- Ready to start Phase 1.
