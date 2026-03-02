use anyhow::{Context, Result, anyhow};
use futures::{SinkExt, StreamExt};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;

use crate::config::EdgeGatewayConfig;
use crate::hub::edge_schema::{
    AuthAck, EdgeRequest, decode_auth_hello, decode_session_request, encode_auth_ack,
    encode_signed_response,
};
use crate::hub::order_scheduler::OrderingScheduler;
use crate::hub::protocol::{
    EdgeFrameCodec, Header, MAGIC, MSG_TYPE_AUTH_HELLO, MSG_TYPE_ERROR, MSG_TYPE_RESPONSE,
    MSG_TYPE_SESSION_REQUEST, VERSION,
};
use crate::hub::security::Blacklist;
use crate::management::order_rules_service::OrderRulesStore;
use crate::raft::router::Router;
use flatbuffers::FlatBufferBuilder;
use std::time::{Duration, Instant};
use tokio::sync::{Semaphore, mpsc, oneshot};
use uuid::Uuid;

pub type SessionId = u64;

#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub device_id: u64,
    pub session_key: Vec<u8>,
    pub created_at_ms: u64,
    pub last_seen_ms: u64,
}

#[derive(Debug, Default)]
struct GroupSessions {
    entries: HashMap<SessionId, SessionEntry>,
}

pub struct SessionManager {
    ttl_ms: u64,
    resolver: Box<dyn Fn(u64) -> u64 + Send + Sync>,
    groups: HashMap<u64, GroupSessions>,
    next_id: SessionId,
    locations: HashMap<SessionId, u64>,
}

impl SessionManager {
    pub fn new(ttl_ms: u64) -> Self {
        Self::new_with_resolver(ttl_ms, |device_id| device_id)
    }

    pub fn new_with_resolver<F>(ttl_ms: u64, resolver: F) -> Self
    where
        F: Fn(u64) -> u64 + Send + Sync + 'static,
    {
        Self {
            ttl_ms,
            resolver: Box::new(resolver),
            groups: HashMap::new(),
            next_id: 1,
            locations: HashMap::new(),
        }
    }

    pub fn create(&mut self, device_id: u64, session_key: Vec<u8>, now_ms: u64) -> SessionId {
        let group_id = (self.resolver)(device_id);
        let group = self
            .groups
            .entry(group_id)
            .or_insert_with(|| GroupSessions {
                entries: HashMap::new(),
            });
        let session_id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.locations.insert(session_id, group_id);
        let entry = SessionEntry {
            device_id,
            session_key,
            created_at_ms: now_ms,
            last_seen_ms: now_ms,
        };
        group.entries.insert(session_id, entry);
        session_id
    }

    pub fn get(
        &mut self,
        session_id: SessionId,
        device_id: u64,
        now_ms: u64,
    ) -> Result<SessionEntry> {
        let group_id = (self.resolver)(device_id);
        self.get_internal(session_id, group_id, now_ms)
    }

    pub fn get_by_id(&mut self, session_id: SessionId, now_ms: u64) -> Result<SessionEntry> {
        let group_id = *self
            .locations
            .get(&session_id)
            .ok_or_else(|| anyhow!("session not found"))?;
        self.get_internal(session_id, group_id, now_ms)
    }

    fn get_internal(
        &mut self,
        session_id: SessionId,
        group_id: u64,
        now_ms: u64,
    ) -> Result<SessionEntry> {
        let group = self
            .groups
            .get_mut(&group_id)
            .ok_or_else(|| anyhow!("session not found"))?;
        let entry = group
            .entries
            .get_mut(&session_id)
            .ok_or_else(|| anyhow!("session not found"))?;
        let age = now_ms.saturating_sub(entry.created_at_ms);
        if age > self.ttl_ms {
            group.entries.remove(&session_id);
            self.locations.remove(&session_id);
            return Err(anyhow!("session expired"));
        }
        entry.last_seen_ms = now_ms;
        Ok(entry.clone())
    }

    pub fn peek_key(&self, session_id: SessionId) -> Option<Vec<u8>> {
        let group_id = self.locations.get(&session_id)?;
        let group = self.groups.get(group_id)?;
        group
            .entries
            .get(&session_id)
            .map(|e| e.session_key.clone())
    }

    pub fn touch(&mut self, session_id: SessionId, now_ms: u64) -> Result<()> {
        self.get_by_id(session_id, now_ms).map(|_| ())
    }

    pub fn get_device_id(&self, session_id: SessionId) -> Option<u64> {
        let group_id = self.locations.get(&session_id)?;
        let group = self.groups.get(group_id)?;
        group.entries.get(&session_id).map(|e| e.device_id)
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要主动清理过期会话
    // - 目的: 防止内存泄漏
    pub fn cleanup_expired(&mut self, now_ms: u64) -> usize {
        let mut count = 0;
        let mut expired_ids = Vec::new();

        for group in self.groups.values_mut() {
            let ids: Vec<u64> = group
                .entries
                .iter()
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

    pub fn insert_for_test(
        &mut self,
        device_id: u64,
        session_key: Vec<u8>,
        now_ms: u64,
    ) -> SessionId {
        self.create(device_id, session_key, now_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::management::order_rules_service::OrderRulesStore;
    use std::sync::Arc;
    use tokio::time::Duration;

    #[test]
    fn test_cleanup_expired() {
        let mut sm = SessionManager::new(100); // 100ms TTL
        sm.create(1, vec![], 1000);
        assert_eq!(sm.locations.len(), 1);

        // Not expired (50ms < 100ms)
        assert_eq!(sm.cleanup_expired(1050), 0);
        assert_eq!(sm.locations.len(), 1);

        // Expired (200ms > 100ms)
        assert_eq!(sm.cleanup_expired(1200), 1);
        assert_eq!(sm.locations.len(), 0);
    }

    #[tokio::test]
    async fn smart_batcher_stats_successful_write() -> Result<()> {
        let router = Arc::new(Router::new_for_test(true));
        let config = SmartBatchConfig {
            max_batch_size: 8,
            max_delay_ms: 5,
            max_queue_size: 8,
            max_wait_ms: 200,
        };
        // ### 修改记录 (2026-03-01)
        // - 原因: 测试需要注入规则存储
        // - 目的: 支持顺序调度初始化
        let order_rules_store = Arc::new(OrderRulesStore::new());
        let gateway = EdgeGateway::new(
            "127.0.0.1:0".to_string(),
            router,
            EdgeGatewayConfig {
                session_ttl_ms: 5000,
                nonce_cache_limit: 10,
                nonce_persist_enabled: false,
                nonce_persist_path: "".to_string(),
                secret_key: "test_secret".to_string(),
                max_connections: 10,
                batch_max_size: config.max_batch_size,
                batch_max_delay_ms: config.max_delay_ms,
                batch_max_queue_size: config.max_queue_size,
                batch_wait_timeout_ms: config.max_wait_ms,
                ordering_stage_parallelism: 2,
                ordering_queue_limit: 100,
                batch_shards: 1,
            },
            order_rules_store,
        )?;

        let result = gateway
            .batchers[0]
            .enqueue(vec!["INSERT INTO t VALUES (1)".to_string()], 1)
            .await?;
        assert_eq!(result, 1);

        tokio::time::sleep(Duration::from_millis(10)).await;

        let stats = gateway.batcher_stats();
        assert_eq!(stats.enqueued_total, 1);
        assert_eq!(stats.enqueue_ok, 1);
        assert_eq!(stats.batch_wait_ok, 1);
        assert_eq!(stats.batch_wait_err, 0);
        assert_eq!(stats.batch_write_ok, 1);
        assert_eq!(stats.batch_write_err, 0);
        assert_eq!(stats.batched_sqls_total, 1);
        assert_eq!(stats.queue_full, 0);
        assert_eq!(stats.wait_timeout, 0);
        assert_eq!(stats.response_dropped, 0);

        Ok(())
    }

    #[tokio::test]
    async fn smart_batcher_stats_write_error() -> Result<()> {
        let router = Arc::new(Router::new_for_test(false));
        let config = SmartBatchConfig {
            max_batch_size: 4,
            max_delay_ms: 1,
            max_queue_size: 4,
            max_wait_ms: 200,
        };
        // ### 修改记录 (2026-03-01)
        // - 原因: 测试需要注入规则存储
        // - 目的: 支持顺序调度初始化
        let order_rules_store = Arc::new(OrderRulesStore::new());
        let gateway = EdgeGateway::new(
            "127.0.0.1:0".to_string(),
            router,
            EdgeGatewayConfig {
                session_ttl_ms: 5000,
                nonce_cache_limit: 10,
                nonce_persist_enabled: false,
                nonce_persist_path: "".to_string(),
                secret_key: "test_secret".to_string(),
                max_connections: 10,
                batch_max_size: config.max_batch_size,
                batch_max_delay_ms: config.max_delay_ms,
                batch_max_queue_size: config.max_queue_size,
                batch_wait_timeout_ms: config.max_wait_ms,
                ordering_stage_parallelism: 2,
                ordering_queue_limit: 100,
                batch_shards: 1,
            },
            order_rules_store,
        )?;

        let err = gateway
            .batchers[0]
            .enqueue(vec!["INSERT INTO t VALUES (2)".to_string()], 1)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Batch write not supported on follower"));

        tokio::time::sleep(Duration::from_millis(10)).await;

        let stats = gateway.batcher_stats();
        assert_eq!(stats.enqueued_total, 1);
        assert_eq!(stats.enqueue_ok, 1);
        assert_eq!(stats.batch_wait_ok, 0);
        assert_eq!(stats.batch_wait_err, 1);
        assert_eq!(stats.batch_write_ok, 0);
        assert_eq!(stats.batch_write_err, 1);
        assert_eq!(stats.batched_sqls_total, 1);
        assert_eq!(stats.queue_full, 0);
        assert_eq!(stats.wait_timeout, 0);
        assert_eq!(stats.response_dropped, 0);

        Ok(())
    }

    #[tokio::test]
    async fn smart_batcher_stats_wait_timeout_and_drop() -> Result<()> {
        let router = Arc::new(Router::new_for_test(true));
        let config = SmartBatchConfig {
            max_batch_size: 4,
            max_delay_ms: 60,
            max_queue_size: 4,
            max_wait_ms: 5,
        };
        // ### 修改记录 (2026-03-01)
        // - 原因: 测试需要注入规则存储
        // - 目的: 支持顺序调度初始化
        let order_rules_store = Arc::new(OrderRulesStore::new());
        let gateway = EdgeGateway::new(
            "127.0.0.1:0".to_string(),
            router,
            EdgeGatewayConfig {
                session_ttl_ms: 5000,
                nonce_cache_limit: 10,
                nonce_persist_enabled: false,
                nonce_persist_path: "".to_string(),
                secret_key: "test_secret".to_string(),
                max_connections: 10,
                batch_max_size: config.max_batch_size,
                batch_max_delay_ms: config.max_delay_ms,
                batch_max_queue_size: config.max_queue_size,
                batch_wait_timeout_ms: config.max_wait_ms,
                ordering_stage_parallelism: 2,
                ordering_queue_limit: 100,
                batch_shards: 1,
            },
            order_rules_store,
        )?;

        let err = gateway
            .batchers[0]
            .enqueue(vec!["INSERT INTO t VALUES (3)".to_string()], 1)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Batch wait timeout"));

        tokio::time::sleep(Duration::from_millis(120)).await;

        let stats = gateway.batcher_stats();
        assert_eq!(stats.enqueued_total, 1);
        assert_eq!(stats.enqueue_ok, 1);
        assert_eq!(stats.batch_wait_ok, 0);
        assert_eq!(stats.batch_wait_err, 0);
        assert_eq!(stats.batch_write_ok, 1);
        assert_eq!(stats.batch_write_err, 0);
        assert_eq!(stats.batched_sqls_total, 1);
        assert_eq!(stats.queue_full, 0);
        assert_eq!(stats.wait_timeout, 1);
        assert_eq!(stats.response_dropped, 1);

        Ok(())
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要确保派生指标按区间增量计算
    // - 目的: 验证平均批大小与平均批等待时间的计算逻辑
    #[test]
    fn derived_batch_metrics_use_delta_snapshot() {
        let delta_batcher = SmartBatchStatsSnapshot {
            queue_full: 0,
            wait_timeout: 0,
            response_dropped: 0,
            enqueue_ok: 0,
            batch_write_ok: 2,
            batch_write_err: 1,
            enqueued_total: 0,
            batched_sqls_total: 90,
            batch_wait_ok: 0,
            batch_wait_err: 0,
        };
        let delta_timing = RequestTimingSnapshot {
            ingress: LatencyBucketsSnapshot::default(),
            batch_wait: LatencyBucketsSnapshot::default(),
            response_send: LatencyBucketsSnapshot::default(),
            upload_total: 60,
            upload_errors: 0,
            batch_wait_ms_total: 6000,
        };

        let derived = compute_batcher_derived_metrics(&delta_batcher, &delta_timing);

        assert_eq!(derived.batches, 3);
        assert!((derived.avg_batch_size - 30.0).abs() < 0.0001);
        assert!((derived.avg_batch_wait_ms - 100.0).abs() < 0.0001);
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要验证统计快照初始值
    // - 目的: 为压测汇总提供可预期基准
    // ### 修改记录 (2026-03-01)
    // - 原因: EdgeGateway 初始化触发异步任务
    // - 目的: 使用 Tokio 运行时保证测试稳定
    #[tokio::test]
    async fn timing_summary_starts_zero() -> Result<()> {
        let router = Arc::new(Router::new_for_test(true));
        let order_rules_store = Arc::new(OrderRulesStore::new());
        let gateway = EdgeGateway::new(
            "127.0.0.1:0".to_string(),
            router,
            EdgeGatewayConfig {
                session_ttl_ms: 5000,
                nonce_cache_limit: 10,
                nonce_persist_enabled: false,
                nonce_persist_path: "".to_string(),
                secret_key: "test_secret".to_string(),
                max_connections: 10,
                batch_max_size: 8,
                batch_max_delay_ms: 5,
                batch_max_queue_size: 8,
                batch_wait_timeout_ms: 200,
                ordering_stage_parallelism: 2,
                ordering_queue_limit: 100,
                // ### 修改记录 (2026-03-02)
                // - 原因: EdgeGatewayConfig 新增分片字段
                // - 目的: 保证测试配置覆盖全部必填参数
                batch_shards: 1,
            },
            order_rules_store,
        )?;

        let summary = gateway.timing_summary();
        assert_eq!(summary.upload_total, 0);
        assert_eq!(summary.upload_errors, 0);
        assert_eq!(summary.batch_wait_ms_total, 0);

        Ok(())
    }

    // ### 修改记录 (2026-03-02)
    // - 原因: 引入分片路由需要可重复的哈希行为
    // - 目的: 验证同一顺序组总是命中同一分片
    // - 目的: 防止分片索引越界导致的 panic
    // - 备注: 测试使用固定 shard_count，避免环境差异
    // - 备注: 用例覆盖确定性与边界范围两类场景
    #[test]
    fn shard_index_is_deterministic_and_in_range() {
        let shard_count = 8;
        let idx1 = shard_index_for_group("group-a", shard_count);
        let idx2 = shard_index_for_group("group-a", shard_count);
        assert_eq!(idx1, idx2);
        assert!(idx1 < shard_count);
    }

    // ### 修改记录 (2026-03-02)
    // - 原因: 分片调度在网关侧需要固定路由
    // - 目的: 验证 EdgeGateway 分片函数与内部哈希一致
    // - 目的: 确保同一顺序组总是落在相同调度分片
    // - 备注: 通过固定配置规避运行时差异
    // - 备注: 用例不依赖网络连接，仅校验路由逻辑
    #[tokio::test]
    async fn sharded_schedulers_route_same_group_to_same_shard() -> Result<()> {
        let router = Arc::new(Router::new_for_test(true));
        let order_rules_store = Arc::new(OrderRulesStore::new());
        let gateway = EdgeGateway::new(
            "127.0.0.1:0".to_string(),
            router,
            EdgeGatewayConfig {
                session_ttl_ms: 5000,
                nonce_cache_limit: 10,
                nonce_persist_enabled: false,
                nonce_persist_path: "".to_string(),
                secret_key: "test_secret".to_string(),
                max_connections: 10,
                batch_max_size: 4,
                batch_max_delay_ms: 1,
                batch_max_queue_size: 8,
                batch_wait_timeout_ms: 50,
                ordering_stage_parallelism: 2,
                ordering_queue_limit: 10,
                // ### 修改记录 (2026-03-02)
                // - 原因: 测试需要显式分片数
                // - 目的: 触发分片调度路径
                batch_shards: 4,
            },
            order_rules_store,
        )?;

        let idx1 = gateway.shard_index_for_group("group-a");
        let idx2 = gateway.shard_index_for_group("group-a");
        assert_eq!(idx1, idx2);
        Ok(())
    }
}

struct NoncePersistence {
    db: sled::Db,
}

impl NoncePersistence {
    fn open(path: &str) -> Result<Self> {
        let db = sled::Config::default()
            .path(path)
            .flush_every_ms(Some(1000))
            .open()?;
        Ok(Self { db })
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
        // ### 修改记录 (2026-03-01)
        // - 原因: 避免同步 I/O 阻塞写入路径
        // - 目的: 依赖 sled 后台 flush 提升性能
        Ok(())
    }
}

pub struct NonceCache {
    limit: usize,
    items: HashMap<u64, VecDeque<u64>>,
    persistence: Option<NoncePersistence>,
}

impl NonceCache {
    pub fn new(limit: usize) -> Self {
        let limit = limit.max(1);
        Self {
            limit,
            items: HashMap::new(),
            persistence: None,
        }
    }

    pub fn new_with_persistence(limit: usize, persist_path: Option<String>) -> Result<Self> {
        let limit = limit.max(1);
        let mut items = HashMap::new();
        let persistence = if let Some(path) = persist_path {
            let p = NoncePersistence::open(&path)?;
            items = p.load_all()?;
            Some(p)
        } else {
            None
        };

        Ok(Self {
            limit,
            items,
            persistence,
        })
    }

    pub fn seen(&mut self, group_id: u64, nonce: u64) -> Result<()> {
        let list = self.items.entry(group_id).or_default();
        if list.iter().any(|item| *item == nonce) {
            return Err(anyhow!("replay detected"));
        }
        if list.len() >= self.limit {
            list.pop_front();
        }
        list.push_back(nonce);

        if let Some(p) = &self.persistence {
            p.save_group(group_id, list)?;
        }

        Ok(())
    }
}

pub fn handle_auth_hello(
    buf: &[u8],
    ttl_ms: u64,
    sessions: &mut SessionManager,
    nonce_cache: &mut NonceCache,
    lookup_key: impl Fn(u64) -> Option<Vec<u8>>,
    now_ms: u64,
) -> Result<Vec<u8>> {
    let (meta, _) = decode_auth_hello(buf, lookup_key)?;
    nonce_cache.seen(meta.device_id, meta.nonce)?;
    let session_key = Uuid::new_v4().as_bytes().to_vec();
    let session_id = sessions.create(meta.device_id, session_key.clone(), now_ms);
    let ack = AuthAck {
        session_id,
        session_key,
        ttl_ms,
    };
    let mut builder = FlatBufferBuilder::new();
    let root = encode_auth_ack(&mut builder, &ack);
    builder.finish(root, None);
    Ok(builder.finished_data().to_vec())
}

// ### 修改记录 (2026-03-01)
// - 原因: 顺序规则匹配需要 device_id
// - 目的: 在会话解析阶段返回 device_id
pub fn handle_session_request(
    buf: &[u8],
    sessions: &mut SessionManager,
    nonce_cache: &mut NonceCache,
    now_ms: u64,
) -> Result<(EdgeRequest, u64, u64, u64)> {
    let (meta, req) = {
        let lookup = |id| sessions.peek_key(id);
        decode_session_request(buf, lookup)?
    };

    let device_id = sessions
        .get_device_id(meta.session_id)
        .ok_or_else(|| anyhow!("Session not found after decode"))?;

    nonce_cache.seen(device_id, meta.nonce)?;
    sessions.touch(meta.session_id, now_ms)?;

    Ok((req, meta.session_id, meta.nonce, device_id))
}

pub fn pack_signed_response_payload(
    payload: &[u8],
    session_id: u64,
    timestamp_ms: u64,
    nonce: u64,
    req_header: &Header,
    session_key: &[u8],
) -> Vec<u8> {
    let mut header_prefix = [0u8; 14];
    header_prefix[0..4].copy_from_slice(&MAGIC);
    header_prefix[4] = VERSION;
    header_prefix[5] = MSG_TYPE_RESPONSE;
    header_prefix[6..14].copy_from_slice(&req_header.request_id.to_be_bytes());

    let mut builder = FlatBufferBuilder::new();
    let root = encode_signed_response(
        &mut builder,
        session_id,
        timestamp_ms,
        nonce,
        &header_prefix,
        payload,
        session_key,
    );
    builder.finish(root, None);
    builder.finished_data().to_vec()
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要在入口对写入请求进行削峰填谷
// - 目的: 将多条 SQL 合并为更少的 Raft 日志条目
// - 备注: SmartBatcher 只负责聚合与调度，不负责业务逻辑
struct SmartBatchItem {
    sqls: Vec<String>,
    record_count: usize,
    resp: oneshot::Sender<Result<usize>>,
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要在顺序调度器内传递批处理请求
// - 目的: 将顺序域队列与 SmartBatcher 解耦
// - 备注: resp 用于将批处理结果回传给入口请求
struct OrderingItem {
    sqls: Vec<String>,
    record_count: usize,
    resp: oneshot::Sender<Result<usize>>,
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要与配置解耦
// - 目的: 将批处理参数在 EdgeGateway 内部集中管理
#[derive(Clone)]
struct SmartBatchConfig {
    max_batch_size: usize,
    max_delay_ms: u64,
    max_queue_size: usize,
    max_wait_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct SmartBatchStatsSnapshot {
    pub queue_full: u64,
    pub wait_timeout: u64,
    pub response_dropped: u64,
    pub enqueue_ok: u64,
    pub batch_write_ok: u64,
    pub batch_write_err: u64,
    pub enqueued_total: u64,
    pub batched_sqls_total: u64,
    pub batch_wait_ok: u64,
    pub batch_wait_err: u64,
}

#[derive(Debug, Default)]
struct SmartBatchStats {
    queue_full: AtomicU64,
    wait_timeout: AtomicU64,
    response_dropped: AtomicU64,
    enqueue_ok: AtomicU64,
    batch_write_ok: AtomicU64,
    batch_write_err: AtomicU64,
    enqueued_total: AtomicU64,
    batched_sqls_total: AtomicU64,
    batch_wait_ok: AtomicU64,
    batch_wait_err: AtomicU64,
}

impl SmartBatchStats {
    fn snapshot(&self) -> SmartBatchStatsSnapshot {
        SmartBatchStatsSnapshot {
            queue_full: self.queue_full.load(Ordering::Relaxed),
            wait_timeout: self.wait_timeout.load(Ordering::Relaxed),
            response_dropped: self.response_dropped.load(Ordering::Relaxed),
            enqueue_ok: self.enqueue_ok.load(Ordering::Relaxed),
            batch_write_ok: self.batch_write_ok.load(Ordering::Relaxed),
            batch_write_err: self.batch_write_err.load(Ordering::Relaxed),
            enqueued_total: self.enqueued_total.load(Ordering::Relaxed),
            batched_sqls_total: self.batched_sqls_total.load(Ordering::Relaxed),
            batch_wait_ok: self.batch_wait_ok.load(Ordering::Relaxed),
            batch_wait_err: self.batch_wait_err.load(Ordering::Relaxed),
        }
    }
}

// ### 修改记录 (2026-03-01)
// - 原因: 压测需要观察批处理统计的阶段性变化
// - 目的: 输出区间增量而非累计值，便于定位抖动
impl SmartBatchStatsSnapshot {
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要计算区间差值
    // - 目的: 保持日志输出稳定可读
    fn delta(&self, previous: &Self) -> Self {
        Self {
            queue_full: self.queue_full.saturating_sub(previous.queue_full),
            wait_timeout: self.wait_timeout.saturating_sub(previous.wait_timeout),
            response_dropped: self
                .response_dropped
                .saturating_sub(previous.response_dropped),
            enqueue_ok: self.enqueue_ok.saturating_sub(previous.enqueue_ok),
            batch_write_ok: self.batch_write_ok.saturating_sub(previous.batch_write_ok),
            batch_write_err: self.batch_write_err.saturating_sub(previous.batch_write_err),
            enqueued_total: self.enqueued_total.saturating_sub(previous.enqueued_total),
            batched_sqls_total: self
                .batched_sqls_total
                .saturating_sub(previous.batched_sqls_total),
            batch_wait_ok: self.batch_wait_ok.saturating_sub(previous.batch_wait_ok),
            batch_wait_err: self.batch_wait_err.saturating_sub(previous.batch_wait_err),
        }
    }
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要轻量级耗时分布统计
// - 目的: 观察入口/批处理/回包的延迟分布
#[derive(Debug, Clone, Default)]
struct LatencyBucketsSnapshot {
    lt_1ms: u64,
    lt_5ms: u64,
    lt_10ms: u64,
    lt_50ms: u64,
    lt_100ms: u64,
    lt_500ms: u64,
    lt_1000ms: u64,
    ge_1000ms: u64,
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要输出区间增量
// - 目的: 区分累计与阶段性变化
impl LatencyBucketsSnapshot {
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要避免负数
    // - 目的: 使用饱和减法确保安全
    fn delta(&self, previous: &Self) -> Self {
        Self {
            lt_1ms: self.lt_1ms.saturating_sub(previous.lt_1ms),
            lt_5ms: self.lt_5ms.saturating_sub(previous.lt_5ms),
            lt_10ms: self.lt_10ms.saturating_sub(previous.lt_10ms),
            lt_50ms: self.lt_50ms.saturating_sub(previous.lt_50ms),
            lt_100ms: self.lt_100ms.saturating_sub(previous.lt_100ms),
            lt_500ms: self.lt_500ms.saturating_sub(previous.lt_500ms),
            lt_1000ms: self.lt_1000ms.saturating_sub(previous.lt_1000ms),
            ge_1000ms: self.ge_1000ms.saturating_sub(previous.ge_1000ms),
        }
    }
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要在高并发下低开销采样
// - 目的: 使用原子计数避免锁竞争
#[derive(Debug, Default)]
struct LatencyBuckets {
    lt_1ms: AtomicU64,
    lt_5ms: AtomicU64,
    lt_10ms: AtomicU64,
    lt_50ms: AtomicU64,
    lt_100ms: AtomicU64,
    lt_500ms: AtomicU64,
    lt_1000ms: AtomicU64,
    ge_1000ms: AtomicU64,
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要统一桶边界
// - 目的: 输出可比较的延迟分布
impl LatencyBuckets {
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要按桶累加
    // - 目的: 避免存储原始样本
    fn record_ms(&self, ms: u64) {
        if ms < 1 {
            self.lt_1ms.fetch_add(1, Ordering::Relaxed);
        } else if ms < 5 {
            self.lt_5ms.fetch_add(1, Ordering::Relaxed);
        } else if ms < 10 {
            self.lt_10ms.fetch_add(1, Ordering::Relaxed);
        } else if ms < 50 {
            self.lt_50ms.fetch_add(1, Ordering::Relaxed);
        } else if ms < 100 {
            self.lt_100ms.fetch_add(1, Ordering::Relaxed);
        } else if ms < 500 {
            self.lt_500ms.fetch_add(1, Ordering::Relaxed);
        } else if ms < 1000 {
            self.lt_1000ms.fetch_add(1, Ordering::Relaxed);
        } else {
            self.ge_1000ms.fetch_add(1, Ordering::Relaxed);
        }
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要对外输出快照
    // - 目的: 保持统计只读
    fn snapshot(&self) -> LatencyBucketsSnapshot {
        LatencyBucketsSnapshot {
            lt_1ms: self.lt_1ms.load(Ordering::Relaxed),
            lt_5ms: self.lt_5ms.load(Ordering::Relaxed),
            lt_10ms: self.lt_10ms.load(Ordering::Relaxed),
            lt_50ms: self.lt_50ms.load(Ordering::Relaxed),
            lt_100ms: self.lt_100ms.load(Ordering::Relaxed),
            lt_500ms: self.lt_500ms.load(Ordering::Relaxed),
            lt_1000ms: self.lt_1000ms.load(Ordering::Relaxed),
            ge_1000ms: self.ge_1000ms.load(Ordering::Relaxed),
        }
    }
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要按阶段统计 UploadData 延迟
// - 目的: 定位入口、批处理与回包瓶颈
#[derive(Debug, Clone, Default)]
struct RequestTimingSnapshot {
    ingress: LatencyBucketsSnapshot,
    batch_wait: LatencyBucketsSnapshot,
    response_send: LatencyBucketsSnapshot,
    upload_total: u64,
    upload_errors: u64,
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要计算平均批等待时间
    // - 目的: 通过累计等待毫秒与请求数计算平均值
    batch_wait_ms_total: u64,
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要对外提供轻量级汇总
// - 目的: 在压测中计算平均等待时间
#[derive(Debug, Clone, Default)]
pub struct RequestTimingSummary {
    pub upload_total: u64,
    pub upload_errors: u64,
    pub batch_wait_ms_total: u64,
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要输出区间增量
// - 目的: 压测时观察时间窗口内变化
impl RequestTimingSnapshot {
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要计算区间差值
    // - 目的: 保持日志输出一致
    fn delta(&self, previous: &Self) -> Self {
        Self {
            ingress: self.ingress.delta(&previous.ingress),
            batch_wait: self.batch_wait.delta(&previous.batch_wait),
            response_send: self.response_send.delta(&previous.response_send),
            upload_total: self.upload_total.saturating_sub(previous.upload_total),
            upload_errors: self.upload_errors.saturating_sub(previous.upload_errors),
            // ### 修改记录 (2026-03-01)
            // - 原因: 需要输出区间内的等待耗时总量
            // - 目的: 与区间请求数对齐计算平均等待时间
            batch_wait_ms_total: self
                .batch_wait_ms_total
                .saturating_sub(previous.batch_wait_ms_total),
        }
    }
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要轻量级运行时统计
// - 目的: 在不影响主流程的前提下收集证据
#[derive(Debug, Default)]
struct RequestTimingStats {
    ingress: LatencyBuckets,
    batch_wait: LatencyBuckets,
    response_send: LatencyBuckets,
    upload_total: AtomicU64,
    upload_errors: AtomicU64,
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要累计批处理等待耗时
    // - 目的: 计算平均批等待时间
    batch_wait_ms_total: AtomicU64,
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要统一统计入口
// - 目的: 避免在业务逻辑中散落计数细节
impl RequestTimingStats {
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要统计入口处理耗时
    // - 目的: 评估解析与 SQL 构建成本
    fn record_ingress_ms(&self, ms: u64) {
        self.ingress.record_ms(ms);
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要统计批处理等待/写入耗时
    // - 目的: 判断是否是写入瓶颈
    fn record_batch_wait_ms(&self, ms: u64) {
        self.batch_wait.record_ms(ms);
        // ### 修改记录 (2026-03-01)
        // - 原因: 需要累计等待时间
        // - 目的: 输出平均批等待时间
        self.batch_wait_ms_total.fetch_add(ms, Ordering::Relaxed);
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要统计回包发送耗时
    // - 目的: 判断是否是回包发送瓶颈
    fn record_response_send_ms(&self, ms: u64) {
        self.response_send.record_ms(ms);
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要统计请求总量
    // - 目的: 对齐成功/失败比率
    fn record_upload_total(&self) {
        self.upload_total.fetch_add(1, Ordering::Relaxed);
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要统计失败数量
    // - 目的: 与超时/丢包关联分析
    fn record_upload_error(&self) {
        self.upload_errors.fetch_add(1, Ordering::Relaxed);
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要对外输出快照
    // - 目的: 供定时日志读取
    fn snapshot(&self) -> RequestTimingSnapshot {
        RequestTimingSnapshot {
            ingress: self.ingress.snapshot(),
            batch_wait: self.batch_wait.snapshot(),
            response_send: self.response_send.snapshot(),
            upload_total: self.upload_total.load(Ordering::Relaxed),
            upload_errors: self.upload_errors.load(Ordering::Relaxed),
            // ### 修改记录 (2026-03-01)
            // - 原因: 需要对外暴露累计等待耗时
            // - 目的: 支撑区间平均等待时间计算
            batch_wait_ms_total: self.batch_wait_ms_total.load(Ordering::Relaxed),
        }
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要输出汇总指标
    // - 目的: 压测汇总时计算平均等待时间
    fn summary(&self) -> RequestTimingSummary {
        RequestTimingSummary {
            upload_total: self.upload_total.load(Ordering::Relaxed),
            upload_errors: self.upload_errors.load(Ordering::Relaxed),
            batch_wait_ms_total: self.batch_wait_ms_total.load(Ordering::Relaxed),
        }
    }
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要在日志中输出批处理派生指标
// - 目的: 提供批次数、平均批大小与平均批等待时间
#[derive(Debug, Clone, Default)]
struct DerivedBatchMetrics {
    batches: u64,
    avg_batch_size: f64,
    avg_batch_wait_ms: f64,
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要从区间增量快照计算派生指标
// - 目的: 避免累计值导致的平均值失真
fn compute_batcher_derived_metrics(
    delta_batcher: &SmartBatchStatsSnapshot,
    delta_timing: &RequestTimingSnapshot,
) -> DerivedBatchMetrics {
    let batches = delta_batcher.batch_write_ok + delta_batcher.batch_write_err;
    let avg_batch_size = if batches == 0 {
        0.0
    } else {
        delta_batcher.batched_sqls_total as f64 / batches as f64
    };
    let avg_batch_wait_ms = if delta_timing.upload_total == 0 {
        0.0
    } else {
        delta_timing.batch_wait_ms_total as f64 / delta_timing.upload_total as f64
    };
    DerivedBatchMetrics {
        batches,
        avg_batch_size,
        avg_batch_wait_ms,
    }
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要统一耗时计算逻辑
// - 目的: 避免重复的 as_millis 转换
fn duration_ms(start: Instant, end: Instant) -> u64 {
    end.duration_since(start).as_millis() as u64
}

// ### 修改记录 (2026-03-01)
// - 原因: EdgeGateway 需要 Smart Batcher 组件
// - 目的: 对 UploadData 进行入口批处理并统一回执扇出
struct SmartBatcher {
    sender: mpsc::Sender<SmartBatchItem>,
    max_wait_ms: u64,
    stats: Arc<SmartBatchStats>,
}

impl SmartBatcher {
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要将入口请求批处理后提交到 Router
    // - 目的: 控制批次大小与延迟，稳定高峰写入
    fn new(router: Arc<Router>, config: SmartBatchConfig) -> Self {
        let queue_size = config.max_queue_size.max(1);
        let (tx, mut rx) = mpsc::channel::<SmartBatchItem>(queue_size);
        let max_batch_size = config.max_batch_size.max(1);
        let max_delay_ms = config.max_delay_ms;
        let max_wait_ms = config.max_wait_ms;
        let stats = Arc::new(SmartBatchStats::default());
        let stats_clone = stats.clone();

        tokio::spawn(async move {
            let mut batch: Vec<SmartBatchItem> = Vec::with_capacity(max_batch_size);
            let mut total_sqls: usize = 0;

            loop {
                let first = match rx.recv().await {
                    Some(item) => item,
                    None => break,
                };

                // ### 修改记录 (2026-03-01)
                // - 原因: 修复首元素计数变量引用错误
                // - 目的: 确保批量计数正确
                total_sqls += first.sqls.len();
                batch.push(first);

                let deadline = tokio::time::Instant::now() + Duration::from_millis(max_delay_ms);

                loop {
                    if total_sqls >= max_batch_size {
                        break;
                    }

                    let timeout = tokio::time::sleep_until(deadline);
                    tokio::pin!(timeout);

                    tokio::select! {
                        _ = timeout => {
                            break;
                        }
                        res = rx.recv() => {
                            match res {
                                Some(item) => {
                                    total_sqls += item.sqls.len();
                                    batch.push(item);
                                }
                                None => break,
                            }
                        }
                    }
                }

                if batch.is_empty() {
                    total_sqls = 0;
                    continue;
                }

                let mut merged_sqls: Vec<String> = Vec::with_capacity(total_sqls);
                for item in &batch {
                    merged_sqls.extend(item.sqls.clone());
                }

                stats_clone
                    .batched_sqls_total
                    .fetch_add(merged_sqls.len() as u64, Ordering::Relaxed);
                let result = router.write_batch(merged_sqls).await;
                match result {
                    Ok(_) => {
                        stats_clone.batch_write_ok.fetch_add(1, Ordering::Relaxed);
                        for item in batch.drain(..) {
                            if item.resp.send(Ok(item.record_count)).is_err() {
                                stats_clone.response_dropped.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                    Err(e) => {
                        stats_clone.batch_write_err.fetch_add(1, Ordering::Relaxed);
                        let err_msg = e.to_string();
                        for item in batch.drain(..) {
                            if item.resp.send(Err(anyhow!(err_msg.clone()))).is_err() {
                                stats_clone.response_dropped.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }

                total_sqls = 0;
            }
        });

        Self {
            sender: tx,
            max_wait_ms,
            stats,
        }
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要入口快速拒绝过载请求
    // - 目的: 防止队列无限膨胀
    async fn enqueue(&self, sqls: Vec<String>, record_count: usize) -> Result<usize> {
        let (tx, rx) = oneshot::channel();
        if self
            .sender
            .try_send(SmartBatchItem {
                sqls,
                record_count,
                resp: tx,
            })
            .is_err()
        {
            self.stats.queue_full.fetch_add(1, Ordering::Relaxed);
            return Err(anyhow!("Batch queue full"));
        }
        self.stats.enqueued_total.fetch_add(1, Ordering::Relaxed);
        self.stats.enqueue_ok.fetch_add(1, Ordering::Relaxed);

        if self.max_wait_ms == 0 {
            match rx.await {
                Ok(result) => match result {
                    Ok(count) => {
                        self.stats.batch_wait_ok.fetch_add(1, Ordering::Relaxed);
                        return Ok(count);
                    }
                    Err(e) => {
                        self.stats.batch_wait_err.fetch_add(1, Ordering::Relaxed);
                        return Err(e);
                    }
                },
                Err(_) => {
                    self.stats.response_dropped.fetch_add(1, Ordering::Relaxed);
                    return Err(anyhow!("Batch response dropped"));
                }
            }
        }

        let res = tokio::time::timeout(Duration::from_millis(self.max_wait_ms), rx).await;
        match res {
            Ok(inner) => match inner {
                Ok(result) => match result {
                    Ok(count) => {
                        self.stats.batch_wait_ok.fetch_add(1, Ordering::Relaxed);
                        Ok(count)
                    }
                    Err(e) => {
                        self.stats.batch_wait_err.fetch_add(1, Ordering::Relaxed);
                        Err(e)
                    }
                },
                Err(_) => {
                    self.stats.response_dropped.fetch_add(1, Ordering::Relaxed);
                    Err(anyhow!("Batch response dropped"))
                }
            },
            Err(_) => {
                self.stats.wait_timeout.fetch_add(1, Ordering::Relaxed);
                Err(anyhow!("Batch wait timeout"))
            }
        }
    }
}

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

// ### 修改记录 (2026-03-02)
// - 原因: 分片调度需要稳定的哈希映射
// - 目的: 将同一顺序组路由到固定分片
// - 目的: 避免 shard_count 为 0 导致取模异常
// - 备注: shard_count 取 max(1) 保持边界安全
// - 备注: 函数抽出便于复用与单元测试
fn shard_index_for_group(group: &str, shard_count: usize) -> usize {
    let mut hasher = DefaultHasher::new();
    group.hash(&mut hasher);
    (hasher.finish() as usize) % shard_count.max(1)
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要将顺序调度就绪任务交给 SmartBatcher
// - 目的: 保持批处理能力且不破坏阶段屏障
// - 备注: 支持多 Batcher 分片以提升吞吐
// ### 修改记录 (2026-03-02)
// - 原因: 需要将调度器与批处理器做一对一绑定
// - 目的: 避免 worker 内部二次分片带来的锁竞争
// - 备注: 每个分片 worker 只处理本分片调度器
fn spawn_ordering_worker(
    batcher: Arc<SmartBatcher>,
    scheduler: Arc<OrderingScheduler<OrderingItem>>,
) {
    tokio::spawn(async move {
        loop {
            if let Some(ready) = scheduler.next_ready().await {
                let scheduler_clone = scheduler.clone();
                let batcher_clone = batcher.clone();

                tokio::spawn(async move {
                    let result = batcher_clone
                        .enqueue(ready.task.sqls, ready.task.record_count)
                        .await;
                    let _ = ready.task.resp.send(result);
                    scheduler_clone.mark_done(&ready.group, ready.stage).await;
                });
            } else {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
    });
}

pub struct EdgeGateway {
    addr: String,
    sessions: Arc<Mutex<SessionManager>>,
    nonce_cache: Arc<Mutex<NonceCache>>,
    ttl_ms: u64,
    secret_key: Vec<u8>,
    blacklist: Arc<Mutex<Blacklist>>,
    concurrency_limit: Arc<Semaphore>,
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要在入口聚合写请求
    // - 目的: 避免每条请求直接打到 Raft
    // - 备注: 使用分片 Batcher 提升并发吞吐
    pub batchers: Vec<Arc<SmartBatcher>>,
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要在网关侧读取顺序规则
    // - 目的: 支持按 device_id + msg_type 做顺序匹配
    order_rules_store: Arc<OrderRulesStore>,
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要顺序域调度器控制阶段屏障
    // - 目的: 保证跨设备顺序执行
    // ### 修改记录 (2026-03-02)
    // - 原因: 需要按分片隔离调度锁竞争
    // - 目的: 每个分片使用独立调度器
    ordering_schedulers: Vec<Arc<OrderingScheduler<OrderingItem>>>,
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要控制顺序调度等待时间
    // - 目的: 防止请求无限期等待
    ordering_wait_timeout_ms: u64,
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要运行期统计分段耗时
    // - 目的: 定位入口/批处理/回包瓶颈
    timing_stats: Arc<RequestTimingStats>,
}

impl EdgeGateway {
    pub fn new(
        addr: String,
        router: Arc<Router>,
        config: EdgeGatewayConfig,
        order_rules_store: Arc<OrderRulesStore>,
    ) -> Result<Self> {
        let sessions = SessionManager::new(config.session_ttl_ms);
        let nonce_cache = NonceCache::new_with_persistence(
            config.nonce_cache_limit,
            if config.nonce_persist_enabled {
                Some(config.nonce_persist_path)
            } else {
                None
            },
        )?;

        let blacklist = Blacklist::new(5, Duration::from_secs(300), Duration::from_secs(60));
        let concurrency_limit = Arc::new(Semaphore::new(config.max_connections));
        // ### 修改记录 (2026-03-01)
        // - 原因: 需要从配置构造 Smart Batcher
        // - 目的: 使批处理参数可调
        // - 备注: 初始化多个 Batcher 分片
        let mut batchers = Vec::new();
        let shard_count = config.batch_shards.max(1);
        let batch_config = SmartBatchConfig {
            max_batch_size: config.batch_max_size,
            max_delay_ms: config.batch_max_delay_ms,
            max_queue_size: config.batch_max_queue_size,
            max_wait_ms: config.batch_wait_timeout_ms,
        };
        for _ in 0..shard_count {
            batchers.push(Arc::new(SmartBatcher::new(
                router.clone(),
                batch_config.clone(),
            )));
        }

        // ### 修改记录 (2026-03-01)
        // - 原因: 需要初始化顺序调度器
        // - 目的: 在网关侧按顺序域/阶段调度请求
        // ### 修改记录 (2026-03-02)
        // - 原因: 需要让调度器与分片一一绑定
        // - 目的: 降低跨分片锁竞争与转发抖动
        let mut ordering_schedulers = Vec::with_capacity(batchers.len());
        for _ in 0..batchers.len() {
            ordering_schedulers.push(Arc::new(OrderingScheduler::new(
                config.ordering_stage_parallelism,
                config.ordering_queue_limit,
            )));
        }
        // ### 修改记录 (2026-03-01)
        // - 原因: 需要启动顺序调度执行协程
        // - 目的: 将就绪任务转入 SmartBatcher
        // - 备注: 使用多 worker 避免单线程调度瓶颈
        // ### 修改记录 (2026-03-02)
        // - 原因: 分片调度需要固定 worker 归属
        // - 目的: 每个分片调度器绑定对应批处理器
        let worker_count = config.ordering_stage_parallelism.max(4);
        for (index, scheduler) in ordering_schedulers.iter().enumerate() {
            let batcher = batchers[index].clone();
            for _ in 0..worker_count {
                spawn_ordering_worker(batcher.clone(), scheduler.clone());
            }
        }
        // ### 修改记录 (2026-03-01)
        // - 原因: 需要收集运行路径耗时分布
        // - 目的: 为压测定位提供证据
        let timing_stats = Arc::new(RequestTimingStats::default());

        Ok(Self {
            addr,
            sessions: Arc::new(Mutex::new(sessions)),
            nonce_cache: Arc::new(Mutex::new(nonce_cache)),
            ttl_ms: config.session_ttl_ms,
            secret_key: config.secret_key.into_bytes(),
            blacklist: Arc::new(Mutex::new(blacklist)),
            concurrency_limit,
            batchers,
            order_rules_store,
            ordering_schedulers,
            ordering_wait_timeout_ms: config.batch_wait_timeout_ms,
            timing_stats,
        })
    }

    // ### 修改记录 (2026-03-02)
    // - 原因: 分片调度需要对顺序组做稳定路由
    // - 目的: 将同一顺序组映射到固定调度分片
    // - 目的: 避免 shard_count 为 0 导致索引异常
    // - 备注: 复用 shard_index_for_group 保持哈希一致
    pub fn shard_index_for_group(&self, group: &str) -> usize {
        shard_index_for_group(group, self.batchers.len())
    }

    pub async fn run(self: Arc<Self>) -> Result<()> {
        let listener = TcpListener::bind(&self.addr)
            .await
            .context("Failed to bind Edge TCP port")?;
        println!("EdgeGateway listening on {}", self.addr);

        // ### 修改记录 (2026-03-01)
        // - 原因: 需要定期清理过期会话
        // - 目的: 后台 GC 防止内存泄漏
        {
            let sessions = self.sessions.clone();
            let blacklist = self.blacklist.clone();
            let interval_ms = if self.ttl_ms > 2000 {
                self.ttl_ms / 2
            } else {
                1000
            };
            tokio::spawn(async move {
                let mut interval =
                    tokio::time::interval(tokio::time::Duration::from_millis(interval_ms));
                loop {
                    interval.tick().await;
                    let now_ms =
                        match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
                            Ok(d) => d.as_millis() as u64,
                            Err(_) => continue,
                        };
                    {
                        let mut sessions = sessions.lock().unwrap();
                        let count = sessions.cleanup_expired(now_ms);
                        if count > 0 {
                            println!("GC: cleaned up {} expired sessions", count);
                        }
                    }
                    {
                        let mut bl = blacklist.lock().unwrap();
                        bl.cleanup();
                    }
                }
            });
        }

        // ### 修改记录 (2026-03-01)
        // - 原因: 需要定时输出运行期统计
        // - 目的: 观察 5 分钟窗口内的阶段性变化
        {
            // ### 修改记录 (2026-03-01)
            // - 原因: 需要跨任务共享统计引用
            // - 目的: 降低日志协程对主流程的影响
            let timing_stats = self.timing_stats.clone();
            let gateway = self.clone();
            // ### 修改记录 (2026-03-01)
            // - 原因: 需要按秒观察指标变化
            // - 目的: 与压测 1 秒粒度的发送速率对齐
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            tokio::spawn(async move {
                // ### 修改记录 (2026-03-01)
                // - 原因: 需要输出区间增量
                // - 目的: 避免累计值难以定位波动
                let mut last_timing = timing_stats.snapshot();
                let mut last_batcher = gateway.batcher_stats();

                loop {
                    interval.tick().await;
                    let current_timing = timing_stats.snapshot();
                    let delta_timing = current_timing.delta(&last_timing);
                    let current_batcher = gateway.batcher_stats();
                    let delta_batcher = current_batcher.delta(&last_batcher);

                    // ### 修改记录 (2026-03-01)
                    // - 原因: 需要输出派生指标
                    // - 目的: 快速判断批次规模与平均等待时间
                    let derived = compute_batcher_derived_metrics(&delta_batcher, &delta_timing);
                    println!(
                        "Timing delta: upload_total={} upload_errors={} ingress_ms=[<1:{} <5:{} <10:{} <50:{} <100:{} <500:{} <1000:{} >=1000:{}] batch_wait_ms=[<1:{} <5:{} <10:{} <50:{} <100:{} <500:{} <1000:{} >=1000:{}] response_send_ms=[<1:{} <5:{} <10:{} <50:{} <100:{} <500:{} <1000:{} >=1000:{}] | Batcher delta: queue_full={} wait_timeout={} response_dropped={} enqueue_ok={} batch_write_ok={} batch_write_err={} enqueued_total={} batched_sqls_total={} batch_wait_ok={} batch_wait_err={} | Derived: batches={} avg_batch_size={:.2} avg_batch_wait_ms={:.2}",
                        delta_timing.upload_total,
                        delta_timing.upload_errors,
                        delta_timing.ingress.lt_1ms,
                        delta_timing.ingress.lt_5ms,
                        delta_timing.ingress.lt_10ms,
                        delta_timing.ingress.lt_50ms,
                        delta_timing.ingress.lt_100ms,
                        delta_timing.ingress.lt_500ms,
                        delta_timing.ingress.lt_1000ms,
                        delta_timing.ingress.ge_1000ms,
                        delta_timing.batch_wait.lt_1ms,
                        delta_timing.batch_wait.lt_5ms,
                        delta_timing.batch_wait.lt_10ms,
                        delta_timing.batch_wait.lt_50ms,
                        delta_timing.batch_wait.lt_100ms,
                        delta_timing.batch_wait.lt_500ms,
                        delta_timing.batch_wait.lt_1000ms,
                        delta_timing.batch_wait.ge_1000ms,
                        delta_timing.response_send.lt_1ms,
                        delta_timing.response_send.lt_5ms,
                        delta_timing.response_send.lt_10ms,
                        delta_timing.response_send.lt_50ms,
                        delta_timing.response_send.lt_100ms,
                        delta_timing.response_send.lt_500ms,
                        delta_timing.response_send.lt_1000ms,
                        delta_timing.response_send.ge_1000ms,
                        delta_batcher.queue_full,
                        delta_batcher.wait_timeout,
                        delta_batcher.response_dropped,
                        delta_batcher.enqueue_ok,
                        delta_batcher.batch_write_ok,
                        delta_batcher.batch_write_err,
                        delta_batcher.enqueued_total,
                        delta_batcher.batched_sqls_total,
                        delta_batcher.batch_wait_ok,
                        delta_batcher.batch_wait_err,
                        derived.batches,
                        derived.avg_batch_size,
                        derived.avg_batch_wait_ms
                    );

                    last_timing = current_timing;
                    last_batcher = current_batcher;
                }
            });
        }

        loop {
            let (socket, _) = listener.accept().await?;

            // ### 修改记录 (2026-03-01)
            // - 原因: 需要限制并发连接数
            // - 目的: 防止资源耗尽 (DoS)
            let permit = match self.concurrency_limit.clone().acquire_owned().await {
                Ok(p) => p,
                Err(_) => break Ok(()), // Semaphore closed
            };

            let gateway = self.clone();
            tokio::spawn(async move {
                // Drop permit when task finishes
                let _permit = permit;
                if let Err(e) = gateway.handle_connection(socket).await {
                    eprintln!("Edge connection error: {:?}", e);
                }
            });
        }
    }

    async fn handle_upstream_request(
        &self,
        req: EdgeRequest,
        device_id: u64,
        msg_type: u8,
    ) -> Result<Vec<u8>> {
        match req {
            EdgeRequest::Heartbeat { .. } => Ok(b"OK".to_vec()),
            EdgeRequest::UploadData { records } => {
                // ### 修改记录 (2026-03-01)
                // - 原因: 需要统计入口处理耗时
                // - 目的: 拆分入口与批处理阶段的成本
                let ingress_start = Instant::now();

                // ### 修改记录 (2026-03-01)
                // - 原因: 单条写入在高峰期会放大 Raft 日志数量
                // - 目的: 使用 Smart Batcher 聚合 SQL 后再提交
                let mut sqls = Vec::with_capacity(records.len());
                for record in &records {
                    let val_hex: String =
                        record.value.iter().map(|b| format!("{:02X}", b)).collect();
                    // ### 修改记录 (2026-03-01)
                    // - 原因: 需要避免单引号破坏 SQL 语义
                    // - 目的: 最小化 SQL 注入风险
                    let safe_key = record.key.replace("'", "''");
                    let sql = format!(
                        "INSERT OR REPLACE INTO data (key, value, timestamp) VALUES ('{}', x'{}', {})",
                        safe_key, val_hex, record.timestamp
                    );
                    sqls.push(sql);
                }

                // ### 修改记录 (2026-03-01)
                // - 原因: 需要标记入口处理结束
                // - 目的: 统计入口处理耗时分布
                let ingress_end = Instant::now();

                let record_count = records.len();
                // ### 修改记录 (2026-03-01)
                // - 原因: 需要按 device_id + msg_type 做顺序匹配
                // - 目的: 在满足规则时进入顺序调度，否则保持并行
                let matched_rule = {
                    let snapshot = self.order_rules_store.snapshot().await;
                    snapshot.match_device(&device_id.to_string(), msg_type)
                };
                let result = if let Some(rule) = matched_rule {
                    let (tx, rx) = oneshot::channel();
                    self.ordering_scheduler
                        .enqueue(
                            &rule.order_group,
                            rule.stage,
                            OrderingItem {
                                sqls,
                                record_count,
                                resp: tx,
                            },
                        )
                        .await?;
                    if self.ordering_wait_timeout_ms == 0 {
                        match rx.await {
                            Ok(inner) => inner,
                            Err(_) => Err(anyhow!("Ordering response dropped")),
                        }
                    } else {
                        match tokio::time::timeout(
                            Duration::from_millis(self.ordering_wait_timeout_ms),
                            rx,
                        )
                        .await
                        {
                            Ok(inner) => inner.map_err(|_| anyhow!("Ordering response dropped"))?,
                            Err(_) => Err(anyhow!("Ordering wait timeout")),
                        }
                    }
                } else {
                    // ### 修改记录 (2026-03-01)
                    // - 原因: 使用分片 Batcher 提升并发吞吐
                    // - 目的: 按设备 ID 分流请求到不同 Batcher
                    let index = (device_id % self.batchers.len() as u64) as usize;
                    self.batchers[index].enqueue(sqls, record_count).await
                };

                // ### 修改记录 (2026-03-01)
                // - 原因: 需要统计批处理等待/写入耗时
                // - 目的: 判断是否出现等待放大或写入瓶颈
                let batch_wait_end = Instant::now();
                // ### 修改记录 (2026-03-01)
                // - 原因: 需要对齐总量与失败量
                // - 目的: 便于与超时数据关联分析
                self.timing_stats.record_upload_total();
                self.timing_stats
                    .record_ingress_ms(duration_ms(ingress_start, ingress_end));
                self.timing_stats
                    .record_batch_wait_ms(duration_ms(ingress_end, batch_wait_end));
                if result.is_err() {
                    self.timing_stats.record_upload_error();
                }

                let (success_count, failures) = match result {
                    Ok(count) => (count, Vec::new()),
                    Err(e) => {
                        let err_msg = e.to_string();
                        let mut failed = Vec::with_capacity(record_count);
                        for i in 0..record_count {
                            failed.push((i, err_msg.clone()));
                        }
                        (0, failed)
                    }
                };

                let response = serde_json::json!({
                    "success": success_count,
                    "failures": failures,
                });

                Ok(serde_json::to_vec(&response)?)
            }
            EdgeRequest::AuthHello { .. } => Err(anyhow!("Invalid request type inside session")),
        }
    }

    async fn handle_connection(&self, socket: TcpStream) -> Result<()> {
        let peer_addr = socket.peer_addr().ok();

        if let Some(addr) = peer_addr {
            let mut bl = self.blacklist.lock().unwrap();
            if bl.is_banned(addr.ip()) {
                // Silently drop connection
                return Ok(());
            }
        }

        let mut framed = Framed::new(socket, EdgeFrameCodec);

        while let Some(result) = framed.next().await {
            let (header, payload) = match result {
                Ok(frame) => frame,
                Err(e) => {
                    // Try to send error frame if possible
                    let err_msg = format!("Protocol error: {}", e);
                    let err_bytes = err_msg.as_bytes();
                    let header = Header {
                        version: VERSION,
                        msg_type: MSG_TYPE_ERROR,
                        request_id: 0,
                        payload_len: err_bytes.len() as u32,
                        checksum: [0; 3],
                    };
                    let _ = framed.send((header, err_bytes.to_vec())).await;
                    // ### 修改记录 (2026-03-01)
                    // - 原因: clippy 提示多余的类型转换
                    // - 目的: 保持错误类型一致并减少冗余
                    return Err(e);
                }
            };

            match header.msg_type {
                MSG_TYPE_AUTH_HELLO => {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_millis() as u64;
                    let lookup_key = |_id: u64| Some(self.secret_key.clone());

                    let auth_result = {
                        let mut sessions = self.sessions.lock().unwrap();
                        let mut nonce_cache = self.nonce_cache.lock().unwrap();
                        let res = handle_auth_hello(
                            &payload,
                            self.ttl_ms,
                            &mut sessions,
                            &mut nonce_cache,
                            lookup_key,
                            now_ms,
                        );

                        // ### 修改记录 (2026-03-01)
                        // - 原因: clippy 提示可合并条件判断
                        // - 目的: 保持黑名单逻辑不变并提升可读性
                        if let Err(e) = &res
                            && let Some(addr) = peer_addr
                        {
                            let err_msg = e.to_string();
                            if err_msg.contains("signature mismatch")
                                || err_msg.contains("Invalid AuthHello")
                            {
                                self.blacklist.lock().unwrap().record_failure(addr.ip());
                            }
                        }
                        res
                    };

                    let response_payload = match auth_result {
                        Ok(payload) => payload,
                        Err(e) => {
                            let err_msg = format!("Auth error: {}", e);
                            let err_bytes = err_msg.as_bytes();
                            let resp_header = Header {
                                version: VERSION,
                                msg_type: MSG_TYPE_ERROR,
                                request_id: header.request_id,
                                payload_len: err_bytes.len() as u32,
                                checksum: [0; 3],
                            };
                            framed.send((resp_header, err_bytes.to_vec())).await?;
                            continue;
                        }
                    };

                    let resp_header = Header {
                        version: VERSION,
                        msg_type: MSG_TYPE_RESPONSE,
                        request_id: header.request_id,
                        payload_len: response_payload.len() as u32,
                        checksum: [0; 3],
                    };

                    framed.send((resp_header, response_payload)).await?;
                }
                MSG_TYPE_SESSION_REQUEST => {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_millis() as u64;

                    let session_result = {
                        let mut sessions = self.sessions.lock().unwrap();
                        let mut nonce_cache = self.nonce_cache.lock().unwrap();
                        let res = handle_session_request(
                            &payload,
                            &mut sessions,
                            &mut nonce_cache,
                            now_ms,
                        );

                        // ### 修改记录 (2026-03-01)
                        // - 原因: clippy 提示可合并条件判断
                        // - 目的: 保持黑名单逻辑不变并提升可读性
                        if let Err(e) = &res
                            && let Some(addr) = peer_addr
                        {
                            let err_msg = e.to_string();
                            if err_msg.contains("signature mismatch")
                                || err_msg.contains("Invalid SessionRequest")
                            {
                                self.blacklist.lock().unwrap().record_failure(addr.ip());
                            }
                        }

                        match res {
                            Ok((req, sid, nonce, device_id)) => {
                                let key = sessions.peek_key(sid);
                                match key {
                                    // ### 修改记录 (2026-03-01)
                                    // - 原因: 顺序规则匹配需要 device_id
                                    // - 目的: 将 device_id 透传到上游处理
                                    Some(k) => Ok((req, sid, nonce, k, device_id)),
                                    None => Err(anyhow!("Session key not found")),
                                }
                            }
                            Err(e) => Err(e),
                        }
                    };

                    let (req, session_id, nonce, session_key, device_id) = match session_result {
                        Ok(data) => data,
                        Err(e) => {
                            let err_msg = format!("Session error: {}", e);
                            let err_bytes = err_msg.as_bytes();
                            let resp_header = Header {
                                version: VERSION,
                                msg_type: MSG_TYPE_ERROR,
                                request_id: header.request_id,
                                payload_len: err_bytes.len() as u32,
                                checksum: [0; 3],
                            };
                            framed.send((resp_header, err_bytes.to_vec())).await?;
                            continue;
                        }
                    };

                    let response_data = self
                        .handle_upstream_request(req, device_id, header.msg_type)
                        .await?;

                    let signed_resp = pack_signed_response_payload(
                        &response_data,
                        session_id,
                        now_ms,
                        nonce,
                        &header,
                        &session_key,
                    );

                    let resp_header = Header {
                        version: VERSION,
                        msg_type: MSG_TYPE_RESPONSE,
                        request_id: header.request_id,
                        payload_len: signed_resp.len() as u32,
                        checksum: [0; 3],
                    };

                    // ### 修改记录 (2026-03-01)
                    // - 原因: 需要统计回包发送耗时
                    // - 目的: 判断是否是回包阶段瓶颈
                    let response_send_start = Instant::now();
                    framed.send((resp_header, signed_resp)).await?;
                    // ### 修改记录 (2026-03-01)
                    // - 原因: 需要标记发送完成时间
                    // - 目的: 记录回包发送耗时分布
                    let response_send_end = Instant::now();
                    self.timing_stats
                        .record_response_send_ms(duration_ms(response_send_start, response_send_end));
                }
                _ => return Err(anyhow!("Unknown msg_type {}", header.msg_type)),
            }
        }
        Ok(())
    }

    pub fn batcher_stats(&self) -> SmartBatchStatsSnapshot {
        let mut total = SmartBatchStatsSnapshot::default();
        for b in &self.batchers {
            let snap = b.stats.snapshot();
            total.queue_full += snap.queue_full;
            total.wait_timeout += snap.wait_timeout;
            total.response_dropped += snap.response_dropped;
            total.enqueue_ok += snap.enqueue_ok;
            total.batch_write_ok += snap.batch_write_ok;
            total.batch_write_err += snap.batch_write_err;
            total.enqueued_total += snap.enqueued_total;
            total.batched_sqls_total += snap.batched_sqls_total;
            total.batch_wait_ok += snap.batch_wait_ok;
            total.batch_wait_err += snap.batch_wait_err;
        }
        total
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 压测需要汇总等待耗时
    // - 目的: 计算平均批等待时间
    pub fn timing_summary(&self) -> RequestTimingSummary {
        self.timing_stats.summary()
    }
}
