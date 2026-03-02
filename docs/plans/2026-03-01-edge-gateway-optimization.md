# Edge Gateway Optimization Plan

> **For Trae:** REQUIRED SUB-SKILL: Use subagent-driven-development to implement this plan task-by-task.

**Goal:** Implement critical optimizations for EdgeGateway: configurable security, non-blocking persistence, and active resource cleanup.

**Architecture:**
- **Config:** Extend `EdgeGatewayConfig` to include security parameters.
- **Persistence:** Switch `sled` to asynchronous background flushing to remove I/O blocking from the hot path.
- **Resource Management:** Implement a background Garbage Collection (GC) task for expired sessions.

**Tech Stack:** Rust, Tokio, Sled, Serde.

---

### Task 1: Configurable Secret Key

**Files:**
- Modify: `src/config.rs`
- Modify: `src/hub/edge_gateway.rs`
- Modify: `tests/edge_tcp_common.rs`

**Step 1: Update Config Structure**

In `src/config.rs`, add `secret_key` to `EdgeGatewayConfig`.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeGatewayConfig {
    // ... existing fields ...
    #[serde(default = "default_secret_key")]
    pub secret_key: String,
}

fn default_secret_key() -> String {
    "device_secret".to_string()
}
```

**Step 2: Update EdgeGateway to use Config**

In `src/hub/edge_gateway.rs`:
1.  Update `EdgeGateway` struct to store `secret_key: Vec<u8>`.
2.  Update `EdgeGateway::new` to read `config.secret_key`.
3.  Update `handle_connection` to use `self.secret_key` in the `lookup_key` closure.

**Step 3: Verify with Tests**

Run `cargo test --test edge_tcp_common` and `cargo test --test edge_tcp_limits` to ensure no regression.
(Existing tests use the default key, so they should pass if default is "device_secret").

**Step 4: Add Negative Test**

In `tests/edge_tcp_common.rs`, add a test case `test_handshake_fails_with_wrong_key` that configures a gateway with key "A" and client uses key "B".

---

### Task 2: Optimized Persistence (Async Flush)

**Files:**
- Modify: `src/hub/edge_gateway.rs`

**Step 1: Configure Sled Flush**

In `src/hub/edge_gateway.rs`, modify `NoncePersistence::open`:

```rust
    fn open(path: &str) -> Result<Self> {
        let db = sled::Config::default()
            .path(path)
            .flush_every_ms(1000) // Flush every 1s
            .open()?;
        Ok(Self { db })
    }
```

**Step 2: Remove Synchronous Flush**

In `NoncePersistence::save_group`, remove `self.db.flush()?`.

**Step 3: Verify Persistence**

Run `cargo test --test edge_tcp_limits` (specifically `edge_tcp_rejects_replay_after_restart_when_nonce_persisted`).
*Note:* The test might need a `tokio::time::sleep(Duration::from_millis(1100))` before restarting the gateway to ensure `sled` had time to flush in the background.

---

### Task 3: Active Session Cleanup (GC)

**Files:**
- Modify: `src/hub/edge_gateway.rs`
- Test: `tests/edge_tcp_limits.rs`

**Step 1: Implement Cleanup Method**

In `SessionManager`:

```rust
    pub fn cleanup_expired(&mut self, now_ms: u64) -> usize {
        let mut count = 0;
        let mut expired_ids = Vec::new();
        
        // iterate all groups
        for group in self.groups.values_mut() {
            // collect expired
            let ids: Vec<u64> = group.entries.iter()
                .filter(|(_, entry)| now_ms.saturating_sub(entry.created_at_ms) > self.ttl_ms)
                .map(|(id, _)| *id)
                .collect();
            
            for id in ids {
                group.entries.remove(&id);
                expired_ids.push(id);
                count += 1;
            }
        }
        
        for id in expired_ids {
            self.locations.remove(&id);
        }
        
        count
    }
```

**Step 2: Spawn GC Task**

In `EdgeGateway::run`:

```rust
        let sessions_clone = self.sessions.clone();
        let ttl_ms = self.ttl_ms;
        // Check every 60s, or ttl_ms if it's large, but 60s is safe default
        let interval_ms = 60_000;
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(interval_ms));
            loop {
                interval.tick().await;
                let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                let mut sessions = sessions_clone.lock().unwrap();
                let count = sessions.cleanup_expired(now_ms);
                if count > 0 {
                    println!("Cleaned up {} expired sessions", count);
                }
            }
        });
```

**Step 3: Unit Test GC**

Add a unit test in `src/hub/edge_gateway.rs` module to verify `cleanup_expired` works.
