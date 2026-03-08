# EdgeGateway 稳定性加固 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 为 EdgeGateway 引入可配置 TTL/Nonce 上限与可选持久化能力，提升重启稳定性。

**Architecture:** 新增 EdgeGatewayConfig 统一配置来源；NonceCache 增加可选持久化（sled+序列化）并在启动时恢复；EdgeGateway 使用配置初始化 SessionManager/NonceCache 并在 AuthAck 返回 TTL。

**Tech Stack:** Rust 2024, Tokio, sled, serde_json, bincode, anyhow

---

### Task 1: 引入 EdgeGatewayConfig（TTL/Nonce/持久化开关）

**Files:**
- Modify: `d:\Rust\check_program\src\config.rs`
- Test: `d:\Rust\check_program\tests\edge_tcp_limits.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn edge_tcp_auth_ack_uses_configured_ttl() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let config_path = dir.path().join("edge_gateway_config.json");
    std::fs::write(
        &config_path,
        r#"{"session_ttl_ms": 120_000, "nonce_cache_limit": 1000, "nonce_persist_enabled": false, "nonce_persist_path": "nonce_cache"}"#,
    )?;
    let (addr, handle) = spawn_gateway_with_config(&config_path).await?;
    let mut stream = connect_with_retry(&addr).await?;
    let auth_payload = build_auth_hello(1, 1000, 1);
    send_frame(&mut stream, MSG_TYPE_AUTH_HELLO, 1, &auth_payload).await?;
    let (_header, resp) = read_frame(&mut stream).await?;
    let ack = decode_auth_ack(&resp)?;
    assert_eq!(ack.ttl_ms, 120_000);
    handle.abort();
    Ok(())
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test edge_tcp_limits edge_tcp_auth_ack_uses_configured_ttl`  
Expected: FAIL (缺少配置加载与注入)

**Step 3: Write minimal implementation**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeGatewayConfig {
    pub session_ttl_ms: u64,
    pub nonce_cache_limit: usize,
    pub nonce_persist_enabled: bool,
    pub nonce_persist_path: String,
}

impl Default for EdgeGatewayConfig {
    fn default() -> Self {
        Self {
            session_ttl_ms: 60_000,
            nonce_cache_limit: 1000,
            nonce_persist_enabled: false,
            nonce_persist_path: "edge_nonce_cache".to_string(),
        }
    }
}

impl EdgeGatewayConfig {
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let config: EdgeGatewayConfig = serde_json::from_reader(reader)?;
        Ok(config)
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test edge_tcp_limits edge_tcp_auth_ack_uses_configured_ttl`  
Expected: PASS

**Step 5: Commit**

```bash
# 跳过提交，除非用户明确要求
```

---

### Task 2: NonceCache 增加可选持久化与重启恢复

**Files:**
- Modify: `d:\Rust\check_program\src\hub\edge_gateway.rs`
- Test: `d:\Rust\check_program\tests\edge_tcp_limits.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn edge_tcp_rejects_replay_after_restart_when_nonce_persisted() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let config_path = dir.path().join("edge_gateway_config.json");
    std::fs::write(
        &config_path,
        r#"{"session_ttl_ms": 60_000, "nonce_cache_limit": 10, "nonce_persist_enabled": true, "nonce_persist_path": "nonce_cache"}"#,
    )?;
    let (addr, handle) = spawn_gateway_with_config(&config_path).await?;
    let mut stream = connect_with_retry(&addr).await?;
    let auth_payload = build_auth_hello(9, 1000, 7);
    send_frame(&mut stream, MSG_TYPE_AUTH_HELLO, 1, &auth_payload).await?;
    let (_header, _resp) = read_frame(&mut stream).await?;
    handle.abort();

    let (addr, handle) = spawn_gateway_with_config(&config_path).await?;
    let mut stream = connect_with_retry(&addr).await?;
    send_frame(&mut stream, MSG_TYPE_AUTH_HELLO, 2, &auth_payload).await?;
    let read_result = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        read_frame(&mut stream),
    )
    .await;
    assert!(read_result.is_err() || read_result.unwrap().is_err());
    handle.abort();
    Ok(())
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test edge_tcp_limits edge_tcp_rejects_replay_after_restart_when_nonce_persisted`  
Expected: FAIL (NonceCache 未持久化)

**Step 3: Write minimal implementation**

```rust
struct NoncePersistence {
    db: sled::Db,
}

impl NoncePersistence {
    fn open(path: &Path) -> Result<Self> {
        Ok(Self { db: sled::open(path)? })
    }

    fn load_all(&self) -> Result<HashMap<u64, VecDeque<u64>>> {
        let mut out = HashMap::new();
        for item in self.db.iter() {
            let (key, value) = item?;
            let group_id = u64::from_be_bytes(key.as_ref().try_into()?);
            let list: Vec<u64> = bincode::deserialize(&value)?;
            out.insert(group_id, VecDeque::from(list));
        }
        Ok(out)
    }

    fn save_group(&self, group_id: u64, list: &VecDeque<u64>) -> Result<()> {
        let key = group_id.to_be_bytes();
        let value = bincode::serialize(&list.iter().copied().collect::<Vec<u64>>())?;
        self.db.insert(key, value)?;
        self.db.flush()?;
        Ok(())
    }
}

pub struct NonceCache {
    limit: usize,
    items: HashMap<u64, VecDeque<u64>>,
    persistence: Option<NoncePersistence>,
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test edge_tcp_limits edge_tcp_rejects_replay_after_restart_when_nonce_persisted`  
Expected: PASS

**Step 5: Commit**

```bash
# 跳过提交，除非用户明确要求
```

---

### Task 3: EdgeGateway 注入配置与测试辅助函数

**Files:**
- Modify: `d:\Rust\check_program\src\hub\edge_gateway.rs`
- Modify: `d:\Rust\check_program\tests\edge_tcp_common.rs`

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn edge_tcp_spawn_uses_config_file() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let config_path = dir.path().join("edge_gateway_config.json");
    std::fs::write(
        &config_path,
        r#"{"session_ttl_ms": 90_000, "nonce_cache_limit": 1, "nonce_persist_enabled": false, "nonce_persist_path": "nonce_cache"}"#,
    )?;
    let (addr, handle) = spawn_gateway_with_config(&config_path).await?;
    let mut stream = connect_with_retry(&addr).await?;
    let auth_payload = build_auth_hello(2, 1000, 1);
    send_frame(&mut stream, MSG_TYPE_AUTH_HELLO, 1, &auth_payload).await?;
    let (_header, resp) = read_frame(&mut stream).await?;
    let ack = decode_auth_ack(&resp)?;
    assert_eq!(ack.ttl_ms, 90_000);
    handle.abort();
    Ok(())
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test edge_tcp_limits edge_tcp_spawn_uses_config_file`  
Expected: FAIL (缺少 spawn_gateway_with_config)

**Step 3: Write minimal implementation**

```rust
impl EdgeGateway {
    pub fn new_with_config(addr: String, router: Arc<Router>, config: EdgeGatewayConfig) -> Self {
        let sessions = SessionManager::new(config.session_ttl_ms);
        let nonce_cache = NonceCache::new_with_persistence(
            config.nonce_cache_limit,
            config.nonce_persist_enabled.then_some(config.nonce_persist_path),
        )?;
        Self { addr, router, sessions: Arc::new(Mutex::new(sessions)), nonce_cache: Arc::new(Mutex::new(nonce_cache)) }
    }
}

pub async fn spawn_gateway_with_config(config_path: &Path) -> Result<(String, JoinHandle<Result<()>>)> {
    let config = EdgeGatewayConfig::load_from_file(config_path)?;
    ...
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test edge_tcp_limits edge_tcp_spawn_uses_config_file`  
Expected: PASS

**Step 5: Commit**

```bash
# 跳过提交，除非用户明确要求
```

---

### Task 4: 全量验证

**Step 1: Run tests**

Run: `cargo test edge_tcp_ -q`  
Expected: PASS

**Step 2: Run lint/typecheck**

Run: `cargo check`  
Expected: PASS

Run: `cargo clippy`  
Expected: PASS（允许已有 clippy 警告但需记录）

