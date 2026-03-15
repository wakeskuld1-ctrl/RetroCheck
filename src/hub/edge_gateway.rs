use anyhow::{Context, Result, anyhow};
use futures::{SinkExt, StreamExt};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;

use crate::config::EdgeGatewayConfig;
use crate::hub::edge_schema::{
    AuthAck, DataRecord, EdgeRequest, decode_auth_hello, decode_session_request, encode_auth_ack,
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
use tokio::sync::{Mutex, Notify, Semaphore, mpsc, oneshot};
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

    // ### 修改记录 (2026-03-15)
    // - 原因: DrainSignal 需要切换排空状态
    // - 目的: 为 remove_hub 等流程提供拒绝新连接能力
    pub fn set_draining(&self, reject_new: bool) {
        // ### 修改记录 (2026-03-15)
        // - 原因: draining 变更需要打断 accept 阻塞
        // - 目的: 触发 run 循环关闭/重建监听
        self.draining.store(reject_new, Ordering::SeqCst);
        self.drain_notify.notify_waiters();
    }

    // ### 修改记录 (2026-03-15)
    // - 原因: DrainSignal 需要观测 inflight 计数
    // - 目的: 判断当前是否仍有连接未排空
    pub fn inflight_requests(&self) -> u64 {
        self.inflight_requests.load(Ordering::SeqCst)
    }

    // ### 修改记录 (2026-03-15)
    // - 原因: inflight 计数需要与连接生命周期绑定
    // - 目的: 通过 guard 确保计数增减成对
    fn begin_inflight_guard(&self) -> InflightGuard {
        InflightGuard::new(self.inflight_requests.clone())
    }

    pub async fn run(self: Arc<Self>) -> Result<()> {
        // ### 修改记录 (2026-03-15)
        // - 原因: 排空期间需要停止监听以保证 connect 失败
        // - 目的: 通过可关闭/重绑的 listener 控制排空行为
        let mut listener = Some(
            TcpListener::bind(&self.addr)
                .await
                .context("Failed to bind Edge TCP port")?,
        );
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
                    // ### 修改记录 (2026-03-14)
                    // - 原因: async 环境中 std::sync::Mutex 会阻塞线程
                    // - 目的: 使用 tokio::sync::Mutex 的异步锁
                    {
                        let mut sessions = sessions.lock().await;
                        let count = sessions.cleanup_expired(now_ms);
                        if count > 0 {
                            println!("GC: cleaned up {} expired sessions", count);
                        }
                    }
                    // ### 修改记录 (2026-03-14)
                    // - 原因: 黑名单清理也需要非阻塞锁
                    // - 目的: 与会话清理保持一致
                    {
                        let mut bl = blacklist.lock().await;
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
            // ### 修改记录 (2026-03-03)
            // - 原因: 日志协程不应延长网关生命周期
            // - 目的: 避免测试中持久化锁无法释放
            let gateway = Arc::downgrade(&self);
            // ### 修改记录 (2026-03-01)
            // - 原因: 需要按秒观察指标变化
            // - 目的: 与压测 1 秒粒度的发送速率对齐
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            tokio::spawn(async move {
                // ### 修改记录 (2026-03-01)
                // - 原因: 需要输出区间增量
                // - 目的: 避免累计值难以定位波动
                let mut last_timing = timing_stats.snapshot();
                // ### 修改记录 (2026-03-03)
                // - 原因: 弱引用可能已释放
                // - 目的: 在网关退出后终止日志循环
                let mut last_batcher = match gateway.upgrade() {
                    Some(gateway) => gateway.batcher_stats(),
                    None => return,
                };

                loop {
                    interval.tick().await;
                    let current_timing = timing_stats.snapshot();
                    let delta_timing = current_timing.delta(&last_timing);
                    // ### 修改记录 (2026-03-03)
                    // - 原因: 网关可能已被释放
                    // - 目的: 避免后台任务持有强引用
                    let Some(gateway) = gateway.upgrade() else {
                        break;
                    };
                    let current_batcher = gateway.batcher_stats();
                    let delta_batcher = current_batcher.delta(&last_batcher);

                    // ### 修改记录 (2026-03-01)
                    // - 原因: 需要输出派生指标
                    // - 目的: 快速判断批次规模与平均等待时间
                    let derived = compute_batcher_derived_metrics(&delta_batcher, &delta_timing);
                    println!(
                        "Timing delta: upload_total={} upload_errors={} ingress_ms=[<1:{} <5:{} <10:{} <50:{} <100:{} <500:{} <1000:{} >=1000:{}] batch_wait_ms=[<1:{} <5:{} <10:{} <50:{} <100:{} <500:{} <1000:{} >=1000:{}] write_ms=[<1:{} <5:{} <10:{} <50:{} <100:{} <500:{} <1000:{} >=1000:{}] response_send_ms=[<1:{} <5:{} <10:{} <50:{} <100:{} <500:{} <1000:{} >=1000:{}] | Batcher delta: queue_full={} wait_timeout={} response_dropped={} enqueue_ok={} batch_write_ok={} batch_write_err={} enqueued_total={} batched_sqls_total={} batch_wait_ok={} batch_wait_err={} | Derived: batches={} avg_batch_size={:.2} avg_ingress_ms={:.2} avg_batch_wait_ms={:.2} avg_write_ms={:.2} avg_persist_ms={:.2}",
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
                        delta_timing.write.lt_1ms,
                        delta_timing.write.lt_5ms,
                        delta_timing.write.lt_10ms,
                        delta_timing.write.lt_50ms,
                        delta_timing.write.lt_100ms,
                        delta_timing.write.lt_500ms,
                        delta_timing.write.lt_1000ms,
                        delta_timing.write.ge_1000ms,
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
                        derived.avg_ingress_ms,
                        derived.avg_batch_wait_ms,
                        derived.avg_write_ms,
                        derived.avg_persist_ms
                    );

                    last_timing = current_timing;
                    last_batcher = current_batcher;
                }
            });
        }

        loop {
            // ### 修改记录 (2026-03-15)
            // - 原因: draining=true 时不能继续监听
            // - 目的: 关闭 listener 以保证 connect 直接失败
            if self.draining.load(Ordering::SeqCst) {
                listener.take();
                // ### 修改记录 (2026-03-15)
                // - 原因: Notify 可能错过，需要定期检查状态
                // - 目的: 防止 draining 结束后主循环卡死
                while self.draining.load(Ordering::SeqCst) {
                    let _ = tokio::time::timeout(
                        Duration::from_millis(200),
                        self.drain_notify.notified(),
                    )
                    .await;
                }
                continue;
            }

            if listener.is_none() {
                // ### 修改记录 (2026-03-15)
                // - 原因: 排空结束后必须恢复监听
                // - 目的: 允许客户端重新 connect
                loop {
                    if self.draining.load(Ordering::SeqCst) {
                        break;
                    }
                    match TcpListener::bind(&self.addr).await {
                        Ok(bound) => {
                            listener = Some(bound);
                            println!("EdgeGateway listening on {}", self.addr);
                            break;
                        }
                        Err(err) => {
                            eprintln!("EdgeGateway rebind failed: {:?}", err);
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
                if listener.is_none() {
                    continue;
                }
            }

            // ### 修改记录 (2026-03-15)
            // - 原因: 需要在 accept 阻塞时响应 draining 切换
            // - 目的: 及时中断 accept 进入排空流程
            let notify = self.drain_notify.notified();
            let (socket, _) = tokio::select! {
                _ = notify => {
                    continue;
                }
                accepted = listener.as_ref().unwrap().accept() => {
                    accepted?
                }
            };

            // ### 修改记录 (2026-03-01)
            // - 原因: 需要限制并发连接数
            // - 目的: 防止资源耗尽 (DoS)
            let permit = match self.concurrency_limit.clone().acquire_owned().await {
                Ok(p) => p,
                Err(_) => break Ok(()), // Semaphore closed
            };

            let gateway = self.clone();
            // ### 修改记录 (2026-03-15)
            // - 原因: 需要在连接开始时计入 inflight
            // - 目的: 与 DrainSignal 的排空统计保持一致
            let inflight_guard = gateway.begin_inflight_guard();
            tokio::spawn(async move {
                // ### 修改记录 (2026-03-15)
                // - 原因: 需要保证连接生命周期内持有并发许可
                // - 目的: 在任务结束时自动释放 Semaphore
                let _permit = permit;
                // ### 修改记录 (2026-03-15)
                // - 原因: inflight 需要与连接生命周期绑定
                // - 目的: guard drop 时自动减计数
                let _inflight = inflight_guard;
                // ### 修改记录 (2026-03-15)
                // - 原因: 排空期间不再处理新连接
                // - 目的: 避免 draining 状态下继续处理请求
                if gateway.draining.load(Ordering::SeqCst) {
                    return;
                }
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
                    // ### 修改记录 (2026-03-02)
                    // - 原因: 需要顺序域按分片路由调度器
                    // - 目的: 保持调度分片与批处理分片一致
                    let shard_index = self.shard_index_for_group(&rule.order_group);
                    self.ordering_schedulers[shard_index]
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
                    if let Some(pre_aggregator) = &self.pre_aggregator {
                        match pre_aggregator
                            .enqueue(index, sqls.clone(), record_count)
                            .await
                        {
                            Ok(success_count) => Ok(success_count),
                            Err(err) if err.to_string().contains("PreAgg queue full") => {
                                self.batchers[index].enqueue(sqls, record_count).await
                            }
                            Err(err) => Err(err),
                        }
                    } else {
                        self.batchers[index].enqueue(sqls, record_count).await
                    }
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

                let commands = if success_count > 0 {
                    build_ai_downlink_commands(device_id, &records)
                } else {
                    Vec::new()
                };
                let command_ack_received = count_command_ack_records(&records);
                let response = serde_json::json!({
                    "success": success_count,
                    "failures": failures,
                    "commands_issued": commands.len(),
                    "commands": commands,
                    "command_ack_received": command_ack_received,
                });

                Ok(serde_json::to_vec(&response)?)
            }
            EdgeRequest::AuthHello { .. } => Err(anyhow!("Invalid request type inside session")),
        }
    }

    async fn handle_connection(&self, socket: TcpStream) -> Result<()> {
        let peer_addr = socket.peer_addr().ok();

        if let Some(addr) = peer_addr {
            // ### 修改记录 (2026-03-14)
            // - 原因: 连接入口检查需要异步锁以避免阻塞
            // - 目的: 使用 tokio::sync::Mutex 读取黑名单
            let mut bl = self.blacklist.lock().await;
            if bl.is_banned(addr.ip()) {
                // Silently drop connection
                return Ok(());
            }
        }

        let mut framed = Framed::new(socket, EdgeFrameCodec);

        loop {
            let next_frame = tokio::time::timeout(
                Duration::from_millis(self.ttl_ms.max(1)),
                framed.next(),
            )
            .await;
            let Some(result) = (match next_frame {
                Ok(value) => value,
                Err(_) => break,
            }) else {
                break;
            };
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
                        // ### 修改记录 (2026-03-14)
                        // - 原因: 会话与 nonce 缓存需要异步锁
                        // - 目的: 避免阻塞 tokio 运行时线程
                        let mut sessions = self.sessions.lock().await;
                        let mut nonce_cache = self.nonce_cache.lock().await;
                        handle_auth_hello(
                            &payload,
                            self.ttl_ms,
                            &mut sessions,
                            &mut nonce_cache,
                            lookup_key,
                            now_ms,
                        )
                    };

                    // ### 修改记录 (2026-03-01)
                    // - 原因: clippy 提示可合并条件判断
                    // - 目的: 保持黑名单逻辑不变并提升可读性
                    // ### 修改记录 (2026-03-14)
                    // - 原因: 黑名单记录需要异步锁，且不应持有会话锁时等待
                    // - 目的: 避免锁顺序风险并缩短锁持有时间
                    if let Err(e) = &auth_result
                        && let Some(addr) = peer_addr
                    {
                        let err_msg = e.to_string();
                        if err_msg.contains("signature mismatch")
                            || err_msg.contains("Invalid AuthHello")
                        {
                            let mut bl = self.blacklist.lock().await;
                            bl.record_failure(addr.ip());
                        }
                    }

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
                        // ### 修改记录 (2026-03-14)
                        // - 原因: Session / Nonce 访问需要异步锁
                        // - 目的: 避免在 tokio 运行时内阻塞
                        let mut sessions = self.sessions.lock().await;
                        let mut nonce_cache = self.nonce_cache.lock().await;
                        let res = handle_session_request(
                            &payload,
                            &mut sessions,
                            &mut nonce_cache,
                            now_ms,
                        );

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

                    // ### 修改记录 (2026-03-01)
                    // - 原因: clippy 提示可合并条件判断
                    // - 目的: 保持黑名单逻辑不变并提升可读性
                    // ### 修改记录 (2026-03-14)
                    // - 原因: 黑名单记录不应与会话锁交叉持有
                    // - 目的: 降低锁等待风险
                    if let Err(e) = &session_result
                        && let Some(addr) = peer_addr
                    {
                        let err_msg = e.to_string();
                        if err_msg.contains("signature mismatch")
                            || err_msg.contains("Invalid SessionRequest")
                        {
                            let mut bl = self.blacklist.lock().await;
                            bl.record_failure(addr.ip());
                        }
                    }

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
                    )?;

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
                    self.timing_stats.record_response_send_ms(duration_ms(
                        response_send_start,
                        response_send_end,
                    ));
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
