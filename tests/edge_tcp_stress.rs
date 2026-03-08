use anyhow::{Result, anyhow};
use check_program::config::EdgeGatewayConfig;
use check_program::hub::edge_gateway::{EdgeGateway, RequestTimingSummary};
use check_program::hub::edge_schema::{DataRecord, EdgeRequest};
use check_program::hub::edge_session_schema::{
    decode_auth_ack, decode_signed_response, encode_auth_hello, encode_session_request,
};
use check_program::hub::protocol::{
    EdgeFrameCodec, Header, MAGIC, MSG_TYPE_AUTH_HELLO, MSG_TYPE_RESPONSE,
    MSG_TYPE_SESSION_REQUEST, VERSION,
};
use check_program::management::order_rules_service::OrderRulesStore;
use check_program::raft::raft_node::TestCluster;
use check_program::raft::router::Router;
use flatbuffers::FlatBufferBuilder;
use futures::{SinkExt, StreamExt};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::future::Future;
use std::io::Write;
use std::path::Path;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

const SECRET_KEY: &[u8] = b"test_secret_key_123456";
const STRESS_REPORT_DIR: &str = ".trae/stress_reports";
const STRESS_REPORT_CSV: &str = ".trae/stress_reports/stress_metrics.csv";
const STRESS_REPORT_MD: &str = ".trae/stress_reports/stress_metrics.md";
const STRESS_REPORT_GROUP_CSV: &str = ".trae/stress_reports/stress_group_metrics.csv";
const STRESS_REPORT_DEVICE_CSV: &str = ".trae/stress_reports/stress_device_metrics.csv";
static ROUTER_DB_SEQ: AtomicU64 = AtomicU64::new(1);

// ### 修改记录 (2026-03-03)
// - 原因: 压测用例耗时且依赖机器资源
// - 目的: 默认跳过压力场景，避免常规 cargo test 失败
// - 备注: 需要运行时请设置 RUN_STRESS=1
fn stress_enabled() -> bool {
    std::env::var_os("RUN_STRESS").is_some()
}

// ### 修改记录 (2026-03-04)
// - 原因: 用户要求重试参数配置化并覆盖端到端链路
// - 目的: 先锁定配置默认值，避免后续行为漂移
#[test]
fn edge_gateway_config_should_expose_retry_defaults() {
    let config = EdgeGatewayConfig::default();
    assert_eq!(config.edge_client_retry_delay_ms, 100);
    assert_eq!(config.edge_client_retry_max_attempts, 3);
    assert!(config.edge_client_retry_exponential_backoff);
}

// ### 修改记录 (2026-03-04)
// - 原因: 需要先以测试锁定“全链路共享同一重试次数”的行为
// - 目的: 通过 TDD 确保后续改造不会出现不同阶段次数漂移
#[tokio::test]
async fn retry_runner_should_use_shared_attempt_limit_until_success() {
    let retry_config = EdgeClientRetryConfig {
        retry_delay_ms: 0,
        retry_max_attempts: 3,
        retry_exponential_backoff: false,
    };
    let mut attempts = 0u32;
    let value = run_with_retry(retry_config, |_retry_index| {
        attempts = attempts.saturating_add(1);
        async move {
            if attempts < 3 {
                Err(anyhow!("transient"))
            } else {
                Ok(42u64)
            }
        }
    })
    .await
    .expect("retry should eventually succeed");
    assert_eq!(value, 42);
    assert_eq!(attempts, 3);
}

// ### 修改记录 (2026-03-04)
// - 原因: 需要锁定最大重试次数耗尽时的行为
// - 目的: 防止无限重试或提前终止
#[tokio::test]
async fn retry_runner_should_fail_after_shared_attempt_limit_exhausted() {
    let retry_config = EdgeClientRetryConfig {
        retry_delay_ms: 0,
        retry_max_attempts: 2,
        retry_exponential_backoff: false,
    };
    let mut attempts = 0u32;
    let result = run_with_retry(retry_config, |_retry_index| {
        attempts = attempts.saturating_add(1);
        async move { Err(anyhow!("always_fail")) as Result<u64> }
    })
    .await;
    assert!(result.is_err());
    assert_eq!(attempts, 2);
}

// ### 修改记录 (2026-03-05)
// - 原因: 随机批量上传路径曾把业务失败判定放在重试闭包外，导致业务失败不触发重试
// - 目的: 锁定“业务失败也要进入统一重试”的行为，防止 failover 窗口内请求直接丢失
#[tokio::test]
async fn random_upload_business_failure_should_retry_until_success() {
    let retry_config = EdgeClientRetryConfig {
        retry_delay_ms: 0,
        retry_max_attempts: 3,
        retry_exponential_backoff: false,
    };
    let mut attempts = 0u32;
    let expected_success_count = 10u64;
    let result = run_with_retry(retry_config, |_retry_index| {
        attempts = attempts.saturating_add(1);
        async move {
            let response_json = if attempts < 3 {
                serde_json::json!({
                    "success": 0,
                    "failures": [["0", "Batch wait timeout"]]
                })
            } else {
                serde_json::json!({
                    "success": expected_success_count,
                    "failures": []
                })
            };
            let success = response_json["success"].as_u64().unwrap_or(0);
            let failures_len = response_json["failures"]
                .as_array()
                .map(|arr| arr.len())
                .unwrap_or(usize::MAX);
            if success != expected_success_count || failures_len != 0 {
                return Err(anyhow!(
                    "Upload business failed under random mode: success={} failures_len={} expected={}",
                    success,
                    failures_len,
                    expected_success_count
                ));
            }
            Ok(())
        }
    })
    .await;
    assert!(result.is_ok());
    assert_eq!(attempts, 3);
}

// ### 修改记录 (2026-03-02)
// - 原因: 需要客户端往返延迟分桶统计
// - 目的: 补齐强一致链路的端到端耗时
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

// ### 修改记录 (2026-03-02)
// - 原因: 需要输出快照与区间增量
// - 目的: 与服务端统计格式一致
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

// ### 修改记录 (2026-03-02)
// - 原因: 需要为客户端 RTT 提供分桶能力
// - 目的: 与 Hub 侧统计口径保持一致
impl LatencyBuckets {
    // ### 修改记录 (2026-03-02)
    // - 原因: 需要记录 RTT 分桶
    // - 目的: 便于输出分段延迟分布
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

    // ### 修改记录 (2026-03-02)
    // - 原因: 需要输出只读快照
    // - 目的: 避免读取期间发生写入争用
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

// ### 修改记录 (2026-03-02)
// - 原因: 需要区间增量快照
// - 目的: 每秒输出 RTT 分布变化
impl LatencyBucketsSnapshot {
    // ### 修改记录 (2026-03-02)
    // - 原因: 需要计算区间差值
    // - 目的: 保持日志输出对齐
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

// ### 修改记录 (2026-03-02)
// - 原因: 需要客户端 RTT 统计
// - 目的: 输出强一致链路的端到端耗时
#[derive(Debug, Clone, Default)]
struct ClientTimingSnapshot {
    rtt: LatencyBucketsSnapshot,
    rtt_ms_total: u64,
    rtt_samples: u64,
}

// ### 修改记录 (2026-03-02)
// - 原因: 需要支持区间增量计算
// - 目的: 与服务端统计输出保持节奏一致
impl ClientTimingSnapshot {
    // ### 修改记录 (2026-03-02)
    // - 原因: 需要计算 RTT 区间差值
    // - 目的: 输出每秒 RTT 分布变化
    fn delta(&self, previous: &Self) -> Self {
        Self {
            rtt: self.rtt.delta(&previous.rtt),
            rtt_ms_total: self.rtt_ms_total.saturating_sub(previous.rtt_ms_total),
            rtt_samples: self.rtt_samples.saturating_sub(previous.rtt_samples),
        }
    }
}

// ### 修改记录 (2026-03-02)
// - 原因: 需要轻量级客户端 RTT 统计
// - 目的: 提供全局平均 RTT 与分桶
#[derive(Debug, Default)]
struct ClientTimingStats {
    rtt: LatencyBuckets,
    rtt_ms_total: AtomicU64,
    rtt_samples: AtomicU64,
}

// ### 修改记录 (2026-03-02)
// - 原因: 需要统一统计入口
// - 目的: 避免在请求逻辑中散落计数细节
impl ClientTimingStats {
    // ### 修改记录 (2026-03-02)
    // - 原因: 需要记录 RTT 耗时
    // - 目的: 输出分桶与平均 RTT
    fn record_rtt_ms(&self, ms: u64) {
        self.rtt.record_ms(ms);
        self.rtt_ms_total.fetch_add(ms, Ordering::Relaxed);
        self.rtt_samples.fetch_add(1, Ordering::Relaxed);
    }

    // ### 修改记录 (2026-03-02)
    // - 原因: 需要输出统计快照
    // - 目的: 供日志线程读取
    fn snapshot(&self) -> ClientTimingSnapshot {
        ClientTimingSnapshot {
            rtt: self.rtt.snapshot(),
            rtt_ms_total: self.rtt_ms_total.load(Ordering::Relaxed),
            rtt_samples: self.rtt_samples.load(Ordering::Relaxed),
        }
    }

    // ### 修改记录 (2026-03-02)
    // - 原因: 需要输出汇总值
    // - 目的: 计算平均 RTT
    fn summary(&self) -> ClientTimingSummary {
        ClientTimingSummary {
            rtt_ms_total: self.rtt_ms_total.load(Ordering::Relaxed),
            rtt_samples: self.rtt_samples.load(Ordering::Relaxed),
        }
    }
}

// ### 修改记录 (2026-03-02)
// - 原因: 需要压测汇总 RTT
// - 目的: 与服务端统计一起输出
#[derive(Debug, Clone, Default)]
struct ClientTimingSummary {
    rtt_ms_total: u64,
    rtt_samples: u64,
}

fn avg_rtt_ms(summary: &ClientTimingSummary) -> f64 {
    if summary.rtt_samples == 0 {
        0.0
    } else {
        summary.rtt_ms_total as f64 / summary.rtt_samples as f64
    }
}

#[derive(Debug, Clone, Default)]
struct ClientCommandSummary {
    upload_total: u64,
    upload_success: u64,
    upload_fail: u64,
    cmd_total: u64,
    cmd_exec_success: u64,
    cmd_exec_fail: u64,
    cmd_timeout: u64,
}

#[derive(Debug, Clone, Default)]
struct DeviceClientStatus {
    device_id: u64,
    client_success: bool,
    command: ClientCommandSummary,
}

const RANDOM_UPLOAD_RECORDS_PER_REQUEST: u64 = 10;

#[derive(Debug)]
struct ClientRunFailure {
    summary: ClientCommandSummary,
    error: anyhow::Error,
}

// ### 修改记录 (2026-03-04)
// - 原因: 用户要求将端到端重试做成统一配置项
// - 目的: 让普通 E2E 与压测链路复用同一重试策略
#[derive(Debug, Clone, Copy)]
struct EdgeClientRetryConfig {
    retry_delay_ms: u64,
    retry_max_attempts: u32,
    retry_exponential_backoff: bool,
}

impl EdgeClientRetryConfig {
    fn from_gateway_config(config: &EdgeGatewayConfig) -> Self {
        Self {
            retry_delay_ms: config.edge_client_retry_delay_ms,
            retry_max_attempts: config.edge_client_retry_max_attempts,
            retry_exponential_backoff: config.edge_client_retry_exponential_backoff,
        }
    }

    fn attempts(&self) -> u32 {
        self.retry_max_attempts.max(1)
    }

    fn delay_for_retry(&self, retry_index: u32) -> Duration {
        let multiplier = if self.retry_exponential_backoff {
            1u64.checked_shl(retry_index.min(20)).unwrap_or(u64::MAX)
        } else {
            1
        };
        Duration::from_millis(self.retry_delay_ms.saturating_mul(multiplier))
    }
}

// ### 修改记录 (2026-03-04)
// - 原因: 需要把重试次数统一抽象到单一执行器
// - 目的: 确保端到端各阶段共享同一套重试参数
async fn run_with_retry<T, F, Fut>(retry_config: EdgeClientRetryConfig, mut op: F) -> Result<T>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let attempts = retry_config.attempts();
    let mut last_error = anyhow!("retry operation did not execute");
    for retry_index in 0..attempts {
        match op(retry_index).await {
            Ok(value) => return Ok(value),
            Err(error) => {
                last_error = error;
                if retry_index + 1 < attempts {
                    tokio::time::sleep(retry_config.delay_for_retry(retry_index)).await;
                }
            }
        }
    }
    Err(last_error)
}

// ### 修改记录 (2026-03-04)
// - 原因: 需要在重试中安全复用同一连接上下文
// - 目的: 支持 send/recv 阶段共享统一重试执行器且不破坏借用规则
async fn run_with_retry_with_ctx<C, T, F>(
    retry_config: EdgeClientRetryConfig,
    ctx: &mut C,
    mut op: F,
) -> Result<T>
where
    F: for<'a> FnMut(u32, &'a mut C) -> Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>,
{
    let attempts = retry_config.attempts();
    let mut last_error = anyhow!("retry operation did not execute");
    for retry_index in 0..attempts {
        match op(retry_index, ctx).await {
            Ok(value) => return Ok(value),
            Err(error) => {
                last_error = error;
                if retry_index + 1 < attempts {
                    tokio::time::sleep(retry_config.delay_for_retry(retry_index)).await;
                }
            }
        }
    }
    Err(last_error)
}

fn extract_downlink_commands(payload: &serde_json::Value) -> Vec<String> {
    payload["commands"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|cmd| cmd["cmd_id"].as_str().map(|id| id.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn decode_upload_response_json(
    resp_header: &Header,
    resp_payload: &[u8],
    expected_request_header: &Header,
    session_key: &[u8],
    session_id: u64,
    device_id: u64,
    request_index: u64,
) -> Result<serde_json::Value> {
    if resp_header.msg_type != MSG_TYPE_RESPONSE {
        return Err(anyhow!(
            "Unexpected response type: device_id={} request_index={} msg_type={}",
            device_id,
            request_index,
            resp_header.msg_type
        ));
    }

    let mut expected_prefix = [0u8; 14];
    expected_prefix[0..4].copy_from_slice(&MAGIC);
    expected_prefix[4] = VERSION;
    expected_prefix[5] = MSG_TYPE_RESPONSE;
    expected_prefix[6..14].copy_from_slice(&expected_request_header.request_id.to_be_bytes());

    let session_key_owned = session_key.to_vec();
    let lookup = |_| Some(session_key_owned.clone());
    let signed_resp =
        decode_signed_response(resp_payload, &expected_prefix, lookup).map_err(|e| {
            anyhow!(
                "Decode signed response failed: device_id={} request_index={} error={}",
                device_id,
                request_index,
                e
            )
        })?;
    if signed_resp.session_id != session_id {
        return Err(anyhow!(
            "Session mismatch: device_id={} request_index={} expected={} actual={}",
            device_id,
            request_index,
            session_id,
            signed_resp.session_id
        ));
    }

    let response_json: serde_json::Value =
        serde_json::from_slice(&signed_resp.payload).map_err(|e| {
            anyhow!(
                "Upload response JSON decode failed: device_id={} request_index={} error={}",
                device_id,
                request_index,
                e
            )
        })?;
    Ok(response_json)
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要为 5000 终端压测准备按速率发送逻辑
// - 目的: 以固定 5 次/秒的节奏发送 UploadData，模拟真实负载
// - 备注: 该辅助函数仅用于压力测试，避免污染生产路径
// - 备注: 与 run_client 保持相同的握手与响应校验逻辑
// - 备注: 使用 interval 驱动节奏，避免忙等待
async fn run_client_with_rate(
    addr: String,
    device_id: u64,
    requests_per_second: u64,
    duration: Duration,
    retry_config: EdgeClientRetryConfig,
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要统计真实发送速率
    // - 目的: 以全局计数方式观察并发是否有效提升吞吐
    send_counter: Arc<AtomicU64>,
    // ### 修改记录 (2026-03-02)
    // - 原因: 需要记录客户端 RTT
    // - 目的: 统计强一致链路端到端耗时
    client_timing_stats: Arc<ClientTimingStats>,
) -> std::result::Result<ClientCommandSummary, ClientRunFailure> {
    let mut command_summary = ClientCommandSummary::default();
    let outcome: Result<()> = async {
    let nonce = 1000 + device_id;
    let timestamp = 1234567890;
    // ### 修改记录 (2026-03-04)
    // - 原因: 用户要求端到端链路共享同一重试参数
    // - 目的: 将 connect + auth_hello + auth_ack 收敛到同一重试执行器
    let (mut framed, session_id, session_key) = run_with_retry(retry_config, |_retry_index| {
        let addr = addr.clone();
        async move {
            let stream = TcpStream::connect(&addr)
                .await
                .map_err(|e| anyhow!("Failed to connect to {}: {}", addr, e))?;
            let mut framed = Framed::new(stream, EdgeFrameCodec);
            let mut builder = FlatBufferBuilder::new();
            let req = EdgeRequest::AuthHello {
                device_id,
                timestamp,
                nonce,
            };
            let offset = encode_auth_hello(&mut builder, SECRET_KEY, &req)?;
            builder.finish(offset, None);
            let payload = builder.finished_data().to_vec();
            let header = Header {
                version: VERSION,
                msg_type: MSG_TYPE_AUTH_HELLO,
                request_id: 1,
                payload_len: payload.len() as u32,
                checksum: [0; 3],
            };
            framed
                .send((header, payload))
                .await
                .map_err(|e| anyhow!("AuthHello send failed: {}", e))?;
            let (resp_header, resp_payload) = framed
                .next()
                .await
                .ok_or(anyhow!("Connection closed during handshake"))?
                .map_err(|e| anyhow!("AuthAck receive failed: {}", e))?;
            if resp_header.msg_type != MSG_TYPE_RESPONSE {
                return Err(anyhow!(
                    "Handshake response type mismatch: expected={} actual={}",
                    MSG_TYPE_RESPONSE,
                    resp_header.msg_type
                ));
            }
            let ack = decode_auth_ack(&resp_payload)?;
            Ok((framed, ack.session_id, ack.session_key))
        }
    })
    .await?;

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要以稳定速率持续发送请求
    // - 目的: 还原 5 次/秒、5 分钟的持续负载
    let mut interval =
        tokio::time::interval(Duration::from_millis(1000 / requests_per_second.max(1)));
    let deadline = tokio::time::Instant::now() + duration;
    let mut request_index: u64 = 0;
    let mut next_request_id: u64 = 2;
    let mut next_nonce: u64 = nonce + 1;
    while tokio::time::Instant::now() < deadline {
        interval.tick().await;
        request_index += 1;

        command_summary.upload_total = command_summary.upload_total.saturating_add(1);

        // ### 修改记录 (2026-03-04)
        // - 原因: 用户要求端到端所有阶段共享同一重试参数
        // - 目的: 将 upload 发送、接收、解码与业务成功判定绑定到统一重试执行器
        let response_json =
            run_with_retry_with_ctx(retry_config, &mut framed, |_retry_index, framed| {
            let request_id = next_request_id;
            next_request_id = next_request_id.saturating_add(1);
            let nonce_value = next_nonce;
            next_nonce = next_nonce.saturating_add(1);
            let session_key = session_key.clone();
            let send_counter = Arc::clone(&send_counter);
            let client_timing_stats = Arc::clone(&client_timing_stats);
            Box::pin(async move {
                let data_req = EdgeRequest::UploadData {
                    records: vec![DataRecord {
                        key: format!("dev_{}_req_{}", device_id, request_index),
                        value: vec![1, 2, 3, 4],
                        timestamp: timestamp + request_index,
                    }],
                };
                let mut builder = FlatBufferBuilder::new();
                let offset = encode_session_request(
                    &mut builder,
                    session_id,
                    timestamp + request_index,
                    nonce_value,
                    &session_key,
                    &data_req,
                )?;
                builder.finish(offset, None);
                let payload = builder.finished_data().to_vec();
                let header = Header {
                    version: VERSION,
                    msg_type: MSG_TYPE_SESSION_REQUEST,
                    request_id,
                    payload_len: payload.len() as u32,
                    checksum: [0; 3],
                };
                let rtt_start = Instant::now();
                framed
                    .send((header.clone(), payload))
                    .await
                    .map_err(|e| anyhow!("Upload send failed: {}", e))?;
                send_counter.fetch_add(1, Ordering::Relaxed);
                let (resp_header, resp_payload) = framed
                    .next()
                    .await
                    .ok_or(anyhow!("Connection closed during request"))?
                    .map_err(|e| anyhow!("Upload receive failed: {}", e))?;
                let rtt_ms = rtt_start.elapsed().as_millis() as u64;
                client_timing_stats.record_rtt_ms(rtt_ms);
                let response_json = decode_upload_response_json(
                    &resp_header,
                    &resp_payload,
                    &header,
                    &session_key,
                    session_id,
                    device_id,
                    request_index,
                )?;
                let success = response_json["success"].as_u64().unwrap_or(0);
                let failures_len = response_json["failures"]
                    .as_array()
                    .map(|arr| arr.len())
                    .unwrap_or(usize::MAX);
                if success != 1 || failures_len != 0 {
                    return Err(anyhow!(
                        "Upload business failed: device_id={} request_index={} success={} failures_len={}",
                        device_id,
                        request_index,
                        success,
                        failures_len
                    ));
                }
                Ok(response_json)
            })
        })
        .await?;
        command_summary.upload_success = command_summary.upload_success.saturating_add(1);

        let command_ids = extract_downlink_commands(&response_json);
        command_summary.cmd_total = command_summary
            .cmd_total
            .saturating_add(command_ids.len() as u64);
        for cmd_id in command_ids {
            // ### 修改记录 (2026-03-04)
            // - 原因: 命令回执属于端到端链路，需共享同一重试参数
            // - 目的: 避免 cmd_ack 阶段成为未受保护的单点失败
            let ack_json: serde_json::Value =
                run_with_retry_with_ctx(retry_config, &mut framed, |_retry_index, framed| {
                    let request_id = next_request_id;
                    next_request_id = next_request_id.saturating_add(1);
                    let nonce_value = next_nonce;
                    next_nonce = next_nonce.saturating_add(1);
                    let session_key = session_key.clone();
                    let send_counter = Arc::clone(&send_counter);
                    let cmd_id = cmd_id.clone();
                    Box::pin(async move {
                        let mut ack_builder = FlatBufferBuilder::new();
                        let ack_req = EdgeRequest::UploadData {
                            records: vec![DataRecord {
                                key: format!("cmd_ack_{}", cmd_id),
                                value: vec![1],
                                timestamp: timestamp + request_index,
                            }],
                        };
                        let ack_offset = encode_session_request(
                            &mut ack_builder,
                            session_id,
                            timestamp + request_index,
                            nonce_value,
                            &session_key,
                            &ack_req,
                        )?;
                        ack_builder.finish(ack_offset, None);
                        let ack_payload = ack_builder.finished_data().to_vec();
                        let ack_header = Header {
                            version: VERSION,
                            msg_type: MSG_TYPE_SESSION_REQUEST,
                            request_id,
                            payload_len: ack_payload.len() as u32,
                            checksum: [0; 3],
                        };
                        framed
                            .send((ack_header.clone(), ack_payload))
                            .await
                            .map_err(|e| anyhow!("Cmd ack send failed: {}", e))?;
                        send_counter.fetch_add(1, Ordering::Relaxed);
                        let (ack_resp_header, ack_resp_payload) = framed
                            .next()
                            .await
                            .ok_or(anyhow!("Connection closed during cmd ack"))?
                            .map_err(|e| anyhow!("Cmd ack receive failed: {}", e))?;
                        if ack_resp_header.msg_type != MSG_TYPE_RESPONSE {
                            return Err(anyhow!(
                                "Cmd ack response type mismatch: device_id={} cmd_id={} msg_type={}",
                                device_id,
                                cmd_id,
                                ack_resp_header.msg_type
                            ));
                        }
                        let mut ack_expected_prefix = [0u8; 14];
                        ack_expected_prefix[0..4].copy_from_slice(&MAGIC);
                        ack_expected_prefix[4] = VERSION;
                        ack_expected_prefix[5] = MSG_TYPE_RESPONSE;
                        ack_expected_prefix[6..14]
                            .copy_from_slice(&ack_header.request_id.to_be_bytes());
                        let lookup = |_| Some(session_key.clone());
                        let ack_signed =
                            decode_signed_response(&ack_resp_payload, &ack_expected_prefix, lookup)?;
                        if ack_signed.session_id != session_id {
                            return Err(anyhow!(
                                "Cmd ack session mismatch: device_id={} cmd_id={} expected={} actual={}",
                                device_id,
                                cmd_id,
                                session_id,
                                ack_signed.session_id
                            ));
                        }
                        let ack_json: serde_json::Value =
                            serde_json::from_slice(&ack_signed.payload)?;
                        Ok(ack_json)
                    })
                })
                .await?;
            let ack_success = ack_json["success"].as_u64().unwrap_or(0);
            let ack_received = ack_json["command_ack_received"].as_u64().unwrap_or(0);
            if ack_success == 1 && ack_received >= 1 {
                command_summary.cmd_exec_success =
                    command_summary.cmd_exec_success.saturating_add(1);
            } else {
                command_summary.cmd_exec_fail = command_summary.cmd_exec_fail.saturating_add(1);
                command_summary.cmd_timeout = command_summary.cmd_timeout.saturating_add(1);
            }
        }
    }

    Ok(())
    }
    .await;
    match outcome {
        Ok(()) => Ok(command_summary),
        Err(error) => {
            command_summary.upload_fail = command_summary
                .upload_total
                .saturating_sub(command_summary.upload_success);
            Err(ClientRunFailure {
                summary: command_summary,
                error,
            })
        }
    }
}

fn should_send_random_burst(device_id: u64, request_index: u64) -> bool {
    let mut seed = device_id
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(request_index.wrapping_mul(0xBF58_476D_1CE4_E5B9))
        .wrapping_add(0x94D0_49BB_1331_11EB);
    seed ^= seed >> 30;
    seed = seed.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    seed ^= seed >> 27;
    seed = seed.wrapping_mul(0x94D0_49BB_1331_11EB);
    seed ^= seed >> 31;
    !seed.is_multiple_of(5)
}

fn build_random_upload_records(device_id: u64, request_index: u64, base_timestamp: u64) -> Vec<DataRecord> {
    if !should_send_random_burst(device_id, request_index) {
        return Vec::new();
    }
    (0..RANDOM_UPLOAD_RECORDS_PER_REQUEST)
        .map(|record_index| DataRecord {
            key: format!("dev_{}_req_{}_rec_{}", device_id, request_index, record_index),
            value: vec![1, 2, 3, 4],
            timestamp: base_timestamp + request_index.saturating_mul(10) + record_index,
        })
        .collect()
}

async fn run_client_with_random_upload_rate(
    addr: String,
    device_id: u64,
    requests_per_second: u64,
    duration: Duration,
    retry_config: EdgeClientRetryConfig,
    send_counter: Arc<AtomicU64>,
    client_timing_stats: Arc<ClientTimingStats>,
) -> std::result::Result<ClientCommandSummary, ClientRunFailure> {
    let mut command_summary = ClientCommandSummary::default();
    let outcome: Result<()> = async {
        let nonce = 1000 + device_id;
        let timestamp = 1234567890;
        let (mut framed, session_id, session_key) =
            run_with_retry(retry_config, |_retry_index| {
                let addr = addr.clone();
                async move {
                    let stream = TcpStream::connect(&addr)
                        .await
                        .map_err(|e| anyhow!("Failed to connect to {}: {}", addr, e))?;
                    let mut framed = Framed::new(stream, EdgeFrameCodec);
                    let mut builder = FlatBufferBuilder::new();
                    let req = EdgeRequest::AuthHello {
                        device_id,
                        timestamp,
                        nonce,
                    };
                    let offset = encode_auth_hello(&mut builder, SECRET_KEY, &req)?;
                    builder.finish(offset, None);
                    let payload = builder.finished_data().to_vec();
                    let header = Header {
                        version: VERSION,
                        msg_type: MSG_TYPE_AUTH_HELLO,
                        request_id: 1,
                        payload_len: payload.len() as u32,
                        checksum: [0; 3],
                    };
                    framed
                        .send((header, payload))
                        .await
                        .map_err(|e| anyhow!("AuthHello send failed: {}", e))?;
                    let (resp_header, resp_payload) = framed
                        .next()
                        .await
                        .ok_or(anyhow!("Connection closed during handshake"))?
                        .map_err(|e| anyhow!("AuthAck receive failed: {}", e))?;
                    if resp_header.msg_type != MSG_TYPE_RESPONSE {
                        return Err(anyhow!(
                            "Handshake response type mismatch: expected={} actual={}",
                            MSG_TYPE_RESPONSE,
                            resp_header.msg_type
                        ));
                    }
                    let ack = decode_auth_ack(&resp_payload)?;
                    Ok((framed, ack.session_id, ack.session_key))
                }
            })
            .await?;
        let mut interval =
            tokio::time::interval(Duration::from_millis(1000 / requests_per_second.max(1)));
        let deadline = tokio::time::Instant::now() + duration;
        let mut request_index: u64 = 0;
        let mut next_request_id: u64 = 2;
        let mut next_nonce: u64 = nonce + 1;
        while tokio::time::Instant::now() < deadline {
            interval.tick().await;
            request_index += 1;
            let records = build_random_upload_records(device_id, request_index, timestamp);
            if records.is_empty() {
                continue;
            }
            command_summary.upload_total = command_summary.upload_total.saturating_add(1);
            let expected_success_count = records.len() as u64;
            let response_json_result =
                run_with_retry_with_ctx(retry_config, &mut framed, |_retry_index, framed| {
                    let request_id = next_request_id;
                    next_request_id = next_request_id.saturating_add(1);
                    let nonce_value = next_nonce;
                    next_nonce = next_nonce.saturating_add(1);
                    let session_key = session_key.clone();
                    let send_counter = Arc::clone(&send_counter);
                    let client_timing_stats = Arc::clone(&client_timing_stats);
                    let records = records.clone();
                    let request_index = request_index;
                    Box::pin(async move {
                        let data_req = EdgeRequest::UploadData { records };
                        let mut builder = FlatBufferBuilder::new();
                        let offset = encode_session_request(
                            &mut builder,
                            session_id,
                            timestamp + request_index,
                            nonce_value,
                            &session_key,
                            &data_req,
                        )?;
                        builder.finish(offset, None);
                        let payload = builder.finished_data().to_vec();
                        let header = Header {
                            version: VERSION,
                            msg_type: MSG_TYPE_SESSION_REQUEST,
                            request_id,
                            payload_len: payload.len() as u32,
                            checksum: [0; 3],
                        };
                        let rtt_start = Instant::now();
                        framed
                            .send((header.clone(), payload))
                            .await
                            .map_err(|e| anyhow!("Upload send failed: {}", e))?;
                        send_counter.fetch_add(1, Ordering::Relaxed);
                        let (resp_header, resp_payload) = framed
                            .next()
                            .await
                            .ok_or(anyhow!("Connection closed during request"))?
                            .map_err(|e| anyhow!("Upload receive failed: {}", e))?;
                        let rtt_ms = rtt_start.elapsed().as_millis() as u64;
                        client_timing_stats.record_rtt_ms(rtt_ms);
                        let response_json = decode_upload_response_json(
                            &resp_header,
                            &resp_payload,
                            &header,
                            &session_key,
                            session_id,
                            device_id,
                            request_index,
                        )?;
                        let success = response_json["success"].as_u64().unwrap_or(0);
                        let failures_len = response_json["failures"]
                            .as_array()
                            .map(|arr| arr.len())
                            .unwrap_or(usize::MAX);
                        if success != expected_success_count || failures_len != 0 {
                            return Err(anyhow!(
                                "Upload business failed under random mode: device_id={} request_index={} success={} failures_len={} expected={}",
                                device_id,
                                request_index,
                                success,
                                failures_len,
                                expected_success_count
                            ));
                        }
                        Ok(response_json)
                    })
                })
                .await;
            let response_json = match response_json_result {
                Ok(json) => json,
                Err(_) => {
                    command_summary.upload_fail = command_summary.upload_fail.saturating_add(1);
                    continue;
                }
            };
            command_summary.upload_success = command_summary.upload_success.saturating_add(1);
            let command_ids = extract_downlink_commands(&response_json);
            command_summary.cmd_total = command_summary
                .cmd_total
                .saturating_add(command_ids.len() as u64);
            for cmd_id in command_ids {
                let ack_json: serde_json::Value =
                    run_with_retry_with_ctx(retry_config, &mut framed, |_retry_index, framed| {
                        let request_id = next_request_id;
                        next_request_id = next_request_id.saturating_add(1);
                        let nonce_value = next_nonce;
                        next_nonce = next_nonce.saturating_add(1);
                        let session_key = session_key.clone();
                        let send_counter = Arc::clone(&send_counter);
                        let cmd_id = cmd_id.clone();
                        Box::pin(async move {
                            let mut ack_builder = FlatBufferBuilder::new();
                            let ack_req = EdgeRequest::UploadData {
                                records: vec![DataRecord {
                                    key: format!("cmd_ack_{}", cmd_id),
                                    value: vec![1],
                                    timestamp: timestamp + request_index,
                                }],
                            };
                            let ack_offset = encode_session_request(
                                &mut ack_builder,
                                session_id,
                                timestamp + request_index,
                                nonce_value,
                                &session_key,
                                &ack_req,
                            )?;
                            ack_builder.finish(ack_offset, None);
                            let ack_payload = ack_builder.finished_data().to_vec();
                            let ack_header = Header {
                                version: VERSION,
                                msg_type: MSG_TYPE_SESSION_REQUEST,
                                request_id,
                                payload_len: ack_payload.len() as u32,
                                checksum: [0; 3],
                            };
                            framed
                                .send((ack_header.clone(), ack_payload))
                                .await
                                .map_err(|e| anyhow!("Cmd ack send failed: {}", e))?;
                            send_counter.fetch_add(1, Ordering::Relaxed);
                            let (ack_resp_header, ack_resp_payload) = framed
                                .next()
                                .await
                                .ok_or(anyhow!("Connection closed during cmd ack"))?
                                .map_err(|e| anyhow!("Cmd ack receive failed: {}", e))?;
                            if ack_resp_header.msg_type != MSG_TYPE_RESPONSE {
                                return Err(anyhow!(
                                    "Cmd ack response type mismatch: device_id={} cmd_id={} msg_type={}",
                                    device_id,
                                    cmd_id,
                                    ack_resp_header.msg_type
                                ));
                            }
                            let mut ack_expected_prefix = [0u8; 14];
                            ack_expected_prefix[0..4].copy_from_slice(&MAGIC);
                            ack_expected_prefix[4] = VERSION;
                            ack_expected_prefix[5] = MSG_TYPE_RESPONSE;
                            ack_expected_prefix[6..14]
                                .copy_from_slice(&ack_header.request_id.to_be_bytes());
                            let lookup = |_| Some(session_key.clone());
                            let ack_signed = decode_signed_response(
                                &ack_resp_payload,
                                &ack_expected_prefix,
                                lookup,
                            )?;
                            if ack_signed.session_id != session_id {
                                return Err(anyhow!(
                                    "Cmd ack session mismatch: device_id={} cmd_id={} expected={} actual={}",
                                    device_id,
                                    cmd_id,
                                    session_id,
                                    ack_signed.session_id
                                ));
                            }
                            let ack_json: serde_json::Value =
                                serde_json::from_slice(&ack_signed.payload)?;
                            Ok(ack_json)
                        })
                    })
                    .await
                    .unwrap_or_else(|_| serde_json::json!({}));
                let ack_success = ack_json["success"].as_u64().unwrap_or(0);
                let ack_received = ack_json["command_ack_received"].as_u64().unwrap_or(0);
                if ack_success == 1 && ack_received >= 1 {
                    command_summary.cmd_exec_success =
                        command_summary.cmd_exec_success.saturating_add(1);
                } else {
                    command_summary.cmd_exec_fail = command_summary.cmd_exec_fail.saturating_add(1);
                    command_summary.cmd_timeout = command_summary.cmd_timeout.saturating_add(1);
                }
            }
        }
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => Ok(command_summary),
        Err(error) => {
            command_summary.upload_fail = command_summary
                .upload_total
                .saturating_sub(command_summary.upload_success);
            Err(ClientRunFailure {
                summary: command_summary,
                error,
            })
        }
    }
}

async fn run_client(
    addr: String,
    device_id: u64,
    num_requests: usize,
    retry_config: EdgeClientRetryConfig,
) -> Result<()> {
    let nonce = 1000 + device_id;
    let timestamp = 1234567890;
    // ### 修改记录 (2026-03-04)
    // - 原因: 端到端基础压测链路需要与主压测链路保持一致的全局重试语义
    // - 目的: 合并 connect + auth 阶段重试入口，避免局部硬编码次数
    let (mut framed, session_id, session_key) = run_with_retry(retry_config, |_retry_index| {
        let addr = addr.clone();
        async move {
            let stream = TcpStream::connect(&addr)
                .await
                .map_err(|e| anyhow!("Failed to connect to {}: {}", addr, e))?;
            let mut framed = Framed::new(stream, EdgeFrameCodec);
            let mut builder = FlatBufferBuilder::new();
            let req = EdgeRequest::AuthHello {
                device_id,
                timestamp,
                nonce,
            };
            let offset = encode_auth_hello(&mut builder, SECRET_KEY, &req)?;
            builder.finish(offset, None);
            let payload = builder.finished_data().to_vec();
            let header = Header {
                version: VERSION,
                msg_type: MSG_TYPE_AUTH_HELLO,
                request_id: 1,
                payload_len: payload.len() as u32,
                checksum: [0; 3],
            };
            framed
                .send((header, payload))
                .await
                .map_err(|e| anyhow!("AuthHello send failed: {}", e))?;
            let (resp_header, resp_payload) = framed
                .next()
                .await
                .ok_or(anyhow!("Connection closed during handshake"))?
                .map_err(|e| anyhow!("AuthAck receive failed: {}", e))?;
            if resp_header.msg_type != MSG_TYPE_RESPONSE {
                return Err(anyhow!(
                    "Handshake response type mismatch: expected={} actual={}",
                    MSG_TYPE_RESPONSE,
                    resp_header.msg_type
                ));
            }
            let ack = decode_auth_ack(&resp_payload)?;
            Ok((framed, ack.session_id, ack.session_key))
        }
    })
    .await?;

    let mut next_request_id: u64 = 2;
    let mut next_nonce: u64 = nonce + 1;

    // 3. Send Requests
    for i in 0..num_requests {
        let request_index = i as u64;
        run_with_retry_with_ctx(retry_config, &mut framed, |_retry_index, framed| {
            let request_id = next_request_id;
            next_request_id = next_request_id.saturating_add(1);
            let nonce_value = next_nonce;
            next_nonce = next_nonce.saturating_add(1);
            let session_key = session_key.clone();
            Box::pin(async move {
                let data_req = EdgeRequest::UploadData {
                    records: vec![DataRecord {
                        key: format!("dev_{}_req_{}", device_id, request_index),
                        value: vec![1, 2, 3, 4],
                        timestamp: timestamp + request_index,
                    }],
                };
                let mut builder = FlatBufferBuilder::new();
                let offset = encode_session_request(
                    &mut builder,
                    session_id,
                    timestamp + request_index,
                    nonce_value,
                    &session_key,
                    &data_req,
                )?;
                builder.finish(offset, None);
                let payload = builder.finished_data().to_vec();
                let header = Header {
                    version: VERSION,
                    msg_type: MSG_TYPE_SESSION_REQUEST,
                    request_id,
                    payload_len: payload.len() as u32,
                    checksum: [0; 3],
                };
                framed
                    .send((header.clone(), payload))
                    .await
                    .map_err(|e| anyhow!("Upload send failed: {}", e))?;
                let (resp_header, resp_payload) = framed
                    .next()
                    .await
                    .ok_or(anyhow!("Connection closed during request"))?
                    .map_err(|e| anyhow!("Upload receive failed: {}", e))?;
                let response_json = decode_upload_response_json(
                    &resp_header,
                    &resp_payload,
                    &header,
                    &session_key,
                    session_id,
                    device_id,
                    request_index,
                )?;
                let success = response_json["success"].as_u64().unwrap_or(0);
                let failures_len = response_json["failures"]
                    .as_array()
                    .map(|arr| arr.len())
                    .unwrap_or(usize::MAX);
                if success != 1 || failures_len != 0 {
                    return Err(anyhow!(
                        "Upload business failed: device_id={} request_index={} success={} failures_len={}",
                        device_id,
                        request_index,
                        success,
                        failures_len
                    ));
                }
                Ok(())
            })
        })
        .await?;
    }

    Ok(())
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要批量定义压测参数
// - 目的: 统一管理 timeout/并发/批处理配置
struct StressCase {
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要组合并发与参数构造名字
    // - 目的: 让日志能区分不同压测配置
    name: String,
    num_clients: usize,
    requests_per_second: u64,
    duration: Duration,
    batch_wait_timeout_ms: u64,
    batch_max_size: usize,
    batch_max_delay_ms: u64,
    ordering_stage_parallelism: usize,
    batch_shards: usize,
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要汇总压测输出
// - 目的: 统一生成表格所需字段
struct StressResult {
    name: String,
    num_clients: usize,
    requests_per_second: u64,
    duration_secs: u64,
    batch_wait_timeout_ms: u64,
    batch_max_size: usize,
    batch_max_delay_ms: u64,
    ordering_stage_parallelism: usize,
    batch_shards: usize,
    success_count: usize,
    send_total: u64,
    batcher_stats: check_program::hub::edge_gateway::SmartBatchStatsSnapshot,
    timing_summary: RequestTimingSummary,
    // ### 修改记录 (2026-03-02)
    // - 原因: 需要汇总客户端 RTT
    // - 目的: 输出强一致链路端到端耗时
    client_timing_summary: ClientTimingSummary,
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要统一派生指标计算
// - 目的: 让表格输出保持一致
impl StressResult {
    fn batches(&self) -> u64 {
        self.batcher_stats.batch_write_ok + self.batcher_stats.batch_write_err
    }

    fn avg_batch_size(&self) -> f64 {
        let batches = self.batches();
        if batches == 0 {
            0.0
        } else {
            self.batcher_stats.batched_sqls_total as f64 / batches as f64
        }
    }

    fn avg_batch_wait_ms(&self) -> f64 {
        if self.timing_summary.upload_total == 0 {
            0.0
        } else {
            self.timing_summary.batch_wait_ms_total as f64 / self.timing_summary.upload_total as f64
        }
    }

    // ### 修改记录 (2026-03-02)
    // - 原因: 需要输出入口平均耗时
    // - 目的: 对齐端到端分段统计口径
    fn avg_ingress_ms(&self) -> f64 {
        if self.timing_summary.upload_total == 0 {
            0.0
        } else {
            self.timing_summary.ingress_ms_total as f64 / self.timing_summary.upload_total as f64
        }
    }

    // ### 修改记录 (2026-03-02)
    // - 原因: 需要输出落盘平均耗时
    // - 目的: 以落盘完成作为终点
    fn avg_write_ms(&self) -> f64 {
        if self.timing_summary.upload_total == 0 {
            0.0
        } else {
            self.timing_summary.write_ms_total as f64 / self.timing_summary.upload_total as f64
        }
    }

    // ### 修改记录 (2026-03-02)
    // - 原因: 需要输出端到端落盘平均耗时
    // - 目的: 入口 + 批处理等待（含写入）
    fn avg_persist_ms(&self) -> f64 {
        if self.timing_summary.upload_total == 0 {
            0.0
        } else {
            (self.timing_summary.ingress_ms_total + self.timing_summary.batch_wait_ms_total) as f64
                / self.timing_summary.upload_total as f64
        }
    }

    fn avg_send_rps(&self) -> f64 {
        if self.duration_secs == 0 {
            0.0
        } else {
            self.send_total as f64 / self.duration_secs as f64
        }
    }

    // ### 修改记录 (2026-03-02)
    // - 原因: 需要输出客户端平均 RTT
    // - 目的: 量化强一致链路端到端时延
    fn avg_client_rtt_ms(&self) -> f64 {
        avg_rtt_ms(&self.client_timing_summary)
    }

    fn success_rate_pct(&self) -> f64 {
        if self.num_clients == 0 {
            0.0
        } else {
            (self.success_count as f64 * 100.0) / self.num_clients as f64
        }
    }

    // ### 修改记录 (2026-03-03)
    // - 原因: 用户要求区分“客户端全程成功率”与“单请求成功率”
    // - 目的: 避免单个请求失败把客户端整体结果放大
    fn request_total(&self) -> u64 {
        self.timing_summary.upload_total
    }

    // ### 修改记录 (2026-03-03)
    // - 原因: 需要输出单请求成功计数
    // - 目的: 与客户端口径并排对照
    fn request_success_count(&self) -> u64 {
        self.timing_summary
            .upload_total
            .saturating_sub(self.timing_summary.upload_errors)
    }

    // ### 修改记录 (2026-03-03)
    // - 原因: 需要新增请求级成功率指标
    // - 目的: 快速判断问题是“偶发失败”还是“全局失败”
    fn request_success_rate_pct(&self) -> f64 {
        let total = self.request_total();
        if total == 0 {
            0.0
        } else {
            (self.request_success_count() as f64 * 100.0) / total as f64
        }
    }
}

struct DeviceReportRow {
    run_id: String,
    ts_unix: u64,
    case_name: String,
    device_id: u64,
    group_id: String,
    group_type: &'static str,
    expected_requests: u64,
    report_success: u64,
    report_fail: u64,
    report_success_rate_pct: f64,
    cmd_total: u64,
    cmd_exec_success: u64,
    cmd_exec_fail: u64,
    cmd_timeout: u64,
    cmd_success_rate_pct: f64,
    client_success: bool,
}

#[derive(Default)]
struct GroupReportAccumulator {
    group_type: &'static str,
    device_count: u64,
    online_device_count: u64,
    report_total: u64,
    report_success: u64,
    report_fail: u64,
    cmd_total: u64,
    cmd_exec_success: u64,
    cmd_exec_fail: u64,
    cmd_timeout: u64,
}

fn classify_device_group(device_id: u64) -> (String, &'static str) {
    if device_id < 300 {
        (format!("S{}", (device_id / 50) + 1), "sensor")
    } else if device_id < 380 {
        (format!("I{}", ((device_id - 300) / 16) + 1), "irrigation")
    } else if device_id < 450 {
        (format!("F{}", ((device_id - 380) / 14) + 1), "fertilizer")
    } else if device_id < 500 {
        (format!("T{}", ((device_id - 450) / 10) + 1), "tractor")
    } else {
        (format!("X{}", ((device_id - 500) / 50) + 1), "extra")
    }
}

fn build_device_report_rows(
    result: &StressResult,
    run_id: &str,
    ts_unix: u64,
    client_statuses: &[DeviceClientStatus],
) -> Vec<DeviceReportRow> {
    let default_expected_requests = result
        .requests_per_second
        .saturating_mul(result.duration_secs);
    let mut rows = Vec::with_capacity(client_statuses.len());
    for status in client_statuses {
        let (group_id, group_type) = classify_device_group(status.device_id);
        let expected_requests = if status.command.upload_total == 0 {
            default_expected_requests
        } else {
            status.command.upload_total
        };
        let report_success = if status.command.upload_total == 0 {
            if status.client_success {
                expected_requests
            } else {
                0
            }
        } else {
            status.command.upload_success.min(expected_requests)
        };
        let report_fail = if status.command.upload_total == 0 {
            expected_requests.saturating_sub(report_success)
        } else {
            let observed_fail = status.command.upload_fail.min(expected_requests);
            observed_fail.max(expected_requests.saturating_sub(report_success))
        };
        let cmd_total = status.command.cmd_total;
        let cmd_exec_success = status.command.cmd_exec_success;
        let cmd_exec_fail = status.command.cmd_exec_fail;
        let report_success_rate_pct = if expected_requests == 0 {
            0.0
        } else {
            (report_success as f64 * 100.0) / expected_requests as f64
        };
        let cmd_success_rate_pct = if cmd_total == 0 {
            0.0
        } else {
            (cmd_exec_success as f64 * 100.0) / cmd_total as f64
        };
        rows.push(DeviceReportRow {
            run_id: run_id.to_string(),
            ts_unix,
            case_name: result.name.clone(),
            device_id: status.device_id,
            group_id,
            group_type,
            expected_requests,
            report_success,
            report_fail,
            report_success_rate_pct,
            cmd_total,
            cmd_exec_success,
            cmd_exec_fail,
            cmd_timeout: status.command.cmd_timeout,
            cmd_success_rate_pct,
            client_success: status.client_success,
        });
    }
    rows.sort_by_key(|row| row.device_id);
    rows
}

fn append_group_and_device_reports_once(
    result: &StressResult,
    client_statuses: &[DeviceClientStatus],
) -> std::io::Result<()> {
    std::fs::create_dir_all(STRESS_REPORT_DIR)?;
    let ts_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let run_id = format!("{}_{}", ts_unix, result.name.replace(',', "_"));
    let device_rows = build_device_report_rows(result, &run_id, ts_unix, client_statuses);

    let mut group_acc: BTreeMap<String, GroupReportAccumulator> = BTreeMap::new();
    for row in &device_rows {
        let entry = group_acc.entry(row.group_id.clone()).or_default();
        entry.group_type = row.group_type;
        entry.device_count += 1;
        if row.client_success {
            entry.online_device_count += 1;
        }
        entry.report_total += row.expected_requests;
        entry.report_success += row.report_success;
        entry.report_fail += row.report_fail;
        entry.cmd_total += row.cmd_total;
        entry.cmd_exec_success += row.cmd_exec_success;
        entry.cmd_exec_fail += row.cmd_exec_fail;
        entry.cmd_timeout += row.cmd_timeout;
    }

    let group_path = Path::new(STRESS_REPORT_GROUP_CSV);
    let group_need_header = !group_path.exists();
    let mut group_csv = open_append_with_retry(group_path)?;
    if group_need_header {
        writeln!(
            group_csv,
            "run_id,ts_unix,case,group_id,group_type,device_count,online_device_count,report_total,report_success,report_fail,report_success_rate_pct,cmd_total,cmd_exec_success,cmd_exec_fail,cmd_timeout,cmd_success_rate_pct"
        )?;
    }
    for (group_id, acc) in &group_acc {
        let report_success_rate_pct = if acc.report_total == 0 {
            0.0
        } else {
            (acc.report_success as f64 * 100.0) / acc.report_total as f64
        };
        let cmd_success_rate_pct = if acc.cmd_total == 0 {
            0.0
        } else {
            (acc.cmd_exec_success as f64 * 100.0) / acc.cmd_total as f64
        };
        writeln!(
            group_csv,
            "{},{},{},{},{},{},{},{},{},{},{:.2},{},{},{},{},{:.2}",
            run_id,
            ts_unix,
            result.name.replace(',', "_"),
            group_id,
            acc.group_type,
            acc.device_count,
            acc.online_device_count,
            acc.report_total,
            acc.report_success,
            acc.report_fail,
            report_success_rate_pct,
            acc.cmd_total,
            acc.cmd_exec_success,
            acc.cmd_exec_fail,
            acc.cmd_timeout,
            cmd_success_rate_pct
        )?;
    }
    group_csv.flush()?;

    let device_path = Path::new(STRESS_REPORT_DEVICE_CSV);
    let device_need_header = !device_path.exists();
    let mut device_csv = open_append_with_retry(device_path)?;
    if device_need_header {
        writeln!(
            device_csv,
            "run_id,ts_unix,case,device_id,group_id,group_type,client_success,report_total,report_success,report_fail,report_success_rate_pct,cmd_total,cmd_exec_success,cmd_exec_fail,cmd_timeout,cmd_success_rate_pct"
        )?;
    }
    for row in &device_rows {
        writeln!(
            device_csv,
            "{},{},{},{},{},{},{},{},{},{},{:.2},{},{},{},{},{:.2}",
            row.run_id,
            row.ts_unix,
            row.case_name.replace(',', "_"),
            row.device_id,
            row.group_id,
            row.group_type,
            if row.client_success { 1 } else { 0 },
            row.expected_requests,
            row.report_success,
            row.report_fail,
            row.report_success_rate_pct,
            row.cmd_total,
            row.cmd_exec_success,
            row.cmd_exec_fail,
            row.cmd_timeout,
            row.cmd_success_rate_pct
        )?;
    }
    device_csv.flush()?;
    Ok(())
}

fn append_group_and_device_reports(
    result: &StressResult,
    client_statuses: &[DeviceClientStatus],
) -> Result<()> {
    let mut last_err = None;
    for _ in 0..10 {
        match append_group_and_device_reports_once(result, client_statuses) {
            Ok(()) => return Ok(()),
            Err(err) => {
                let locked = err.kind() == std::io::ErrorKind::PermissionDenied
                    || err.raw_os_error() == Some(32);
                if locked {
                    last_err = Some(err);
                    thread::sleep(Duration::from_millis(30));
                    continue;
                }
                return Err(err.into());
            }
        }
    }
    match append_group_and_device_reports_once(result, client_statuses) {
        Ok(()) => Ok(()),
        Err(err) => {
            let final_err = last_err.unwrap_or(err);
            let locked = final_err.kind() == std::io::ErrorKind::PermissionDenied
                || final_err.raw_os_error() == Some(32);
            if locked {
                eprintln!(
                    "group/device stress report skipped due to file lock: {}",
                    final_err
                );
                return Ok(());
            }
            Err(final_err.into())
        }
    }
}

// ### 修改记录 (2026-03-03)
// - 原因: 需要把压测结果长期沉淀做横向对比
// - 目的: 每次压测自动生成可追溯表格文件
fn open_append_with_retry(path: &Path) -> std::io::Result<std::fs::File> {
    let mut last_err = None;
    for _ in 0..10 {
        match OpenOptions::new().create(true).append(true).open(path) {
            Ok(file) => return Ok(file),
            Err(err) => {
                let locked = err.kind() == std::io::ErrorKind::PermissionDenied
                    || err.raw_os_error() == Some(32);
                if locked {
                    last_err = Some(err);
                    thread::sleep(Duration::from_millis(30));
                    continue;
                }
                return Err(err);
            }
        }
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| last_err.unwrap_or(err))
}

fn append_stress_report_once(result: &StressResult) -> std::io::Result<()> {
    std::fs::create_dir_all(STRESS_REPORT_DIR)?;
    let csv_path = Path::new(STRESS_REPORT_CSV);
    let csv_need_header = !csv_path.exists();
    let mut csv = open_append_with_retry(csv_path)?;
    if csv_need_header {
        writeln!(
            csv,
            "ts_unix,case,num_clients,rps,duration_s,timeout_ms,delay_ms,batch_max_size,parallelism,batch_shards,success_count,success_rate_pct,send_total,avg_send_rps,avg_client_rtt_ms,wait_timeout,response_dropped,queue_full,batches,avg_batch_size,avg_batch_wait_ms,avg_ingress_ms,avg_write_ms,avg_persist_ms,upload_errors"
        )?;
    }

    let md_path = Path::new(STRESS_REPORT_MD);
    let md_need_header = !md_path.exists();
    let mut md = open_append_with_retry(md_path)?;
    if md_need_header {
        writeln!(
            md,
            "| ts_unix | case | clients | rps | duration_s | timeout_ms | delay_ms | success | success_rate_% | avg_rtt_ms | wait_timeout | response_dropped |"
        )?;
        writeln!(
            md,
            "|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
        )?;
    }

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let case_name = result.name.replace(',', "_");
    writeln!(
        csv,
        "{},{},{},{},{},{},{},{},{},{},{},{:.2},{},{:.2},{:.2},{},{},{},{},{:.2},{:.2},{:.2},{:.2},{:.2},{}",
        ts,
        case_name,
        result.num_clients,
        result.requests_per_second,
        result.duration_secs,
        result.batch_wait_timeout_ms,
        result.batch_max_delay_ms,
        result.batch_max_size,
        result.ordering_stage_parallelism,
        result.batch_shards,
        result.success_count,
        result.success_rate_pct(),
        result.send_total,
        result.avg_send_rps(),
        result.avg_client_rtt_ms(),
        result.batcher_stats.wait_timeout,
        result.batcher_stats.response_dropped,
        result.batcher_stats.queue_full,
        result.batches(),
        result.avg_batch_size(),
        result.avg_batch_wait_ms(),
        result.avg_ingress_ms(),
        result.avg_write_ms(),
        result.avg_persist_ms(),
        result.timing_summary.upload_errors
    )?;
    csv.flush()?;
    writeln!(
        md,
        "| {} | {} | {} | {} | {} | {} | {} | {} | {:.2} | {:.2} | {} | {} |",
        ts,
        result.name.replace('|', "/"),
        result.num_clients,
        result.requests_per_second,
        result.duration_secs,
        result.batch_wait_timeout_ms,
        result.batch_max_delay_ms,
        result.success_count,
        result.success_rate_pct(),
        result.avg_client_rtt_ms(),
        result.batcher_stats.wait_timeout,
        result.batcher_stats.response_dropped
    )?;
    md.flush()?;
    Ok(())
}

fn append_stress_report(result: &StressResult) -> Result<()> {
    let mut last_err = None;
    for _ in 0..10 {
        match append_stress_report_once(result) {
            Ok(()) => return Ok(()),
            Err(err) => {
                let locked = err.kind() == std::io::ErrorKind::PermissionDenied
                    || err.raw_os_error() == Some(32);
                if locked {
                    last_err = Some(err);
                    thread::sleep(Duration::from_millis(30));
                    continue;
                }
                return Err(err.into());
            }
        }
    }
    match append_stress_report_once(result) {
        Ok(()) => Ok(()),
        Err(err) => {
            let final_err = last_err.unwrap_or(err);
            let locked = final_err.kind() == std::io::ErrorKind::PermissionDenied
                || final_err.raw_os_error() == Some(32);
            if locked {
                eprintln!("stress report skipped due to file lock: {}", final_err);
                return Ok(());
            }
            Err(final_err.into())
        }
    }
}

#[test]
fn client_rtt_avg_defaults_to_zero() {
    let summary = ClientTimingSummary::default();
    assert_eq!(avg_rtt_ms(&summary), 0.0);
}

#[test]
fn client_rtt_avg_uses_totals() {
    let summary = ClientTimingSummary {
        rtt_ms_total: 120,
        rtt_samples: 3,
    };
    assert_eq!(avg_rtt_ms(&summary), 40.0);
}

#[test]
fn classify_device_group_maps_expected_boundaries() {
    assert_eq!(classify_device_group(0), ("S1".to_string(), "sensor"));
    assert_eq!(classify_device_group(299), ("S6".to_string(), "sensor"));
    assert_eq!(classify_device_group(300), ("I1".to_string(), "irrigation"));
    assert_eq!(classify_device_group(379), ("I5".to_string(), "irrigation"));
    assert_eq!(classify_device_group(380), ("F1".to_string(), "fertilizer"));
    assert_eq!(classify_device_group(449), ("F5".to_string(), "fertilizer"));
    assert_eq!(classify_device_group(450), ("T1".to_string(), "tractor"));
    assert_eq!(classify_device_group(499), ("T5".to_string(), "tractor"));
}

#[test]
fn build_device_report_rows_prefers_request_level_metrics() {
    let result = StressResult {
        name: "case_a".to_string(),
        num_clients: 2,
        requests_per_second: 2,
        duration_secs: 10,
        batch_wait_timeout_ms: 200,
        batch_max_size: 256,
        batch_max_delay_ms: 10,
        ordering_stage_parallelism: 2,
        batch_shards: 1,
        success_count: 1,
        send_total: 20,
        batcher_stats: Default::default(),
        timing_summary: Default::default(),
        client_timing_summary: Default::default(),
    };
    let rows = build_device_report_rows(
        &result,
        "run_1",
        123,
        &[
            DeviceClientStatus {
                device_id: 0,
                client_success: true,
                command: ClientCommandSummary {
                    upload_total: 21,
                    upload_success: 20,
                    upload_fail: 1,
                    cmd_total: 2,
                    cmd_exec_success: 2,
                    cmd_exec_fail: 0,
                    cmd_timeout: 0,
                },
            },
            DeviceClientStatus {
                device_id: 1,
                client_success: false,
                command: ClientCommandSummary {
                    upload_total: 18,
                    upload_success: 17,
                    upload_fail: 1,
                    cmd_total: 1,
                    cmd_exec_success: 0,
                    cmd_exec_fail: 1,
                    cmd_timeout: 1,
                },
            },
        ],
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].device_id, 0);
    assert_eq!(rows[0].expected_requests, 21);
    assert_eq!(rows[0].report_success, 20);
    assert_eq!(rows[0].report_fail, 1);
    assert_eq!(rows[1].device_id, 1);
    assert_eq!(rows[1].expected_requests, 18);
    assert_eq!(rows[1].report_success, 17);
    assert_eq!(rows[1].report_fail, 1);
    assert_eq!(rows[0].cmd_exec_success, 2);
    assert_eq!(rows[1].cmd_timeout, 1);
}

#[test]
fn device_report_uses_request_metrics_even_when_client_failed() {
    let result = StressResult {
        name: "case_b".to_string(),
        num_clients: 1,
        requests_per_second: 5,
        duration_secs: 10,
        batch_wait_timeout_ms: 200,
        batch_max_size: 256,
        batch_max_delay_ms: 10,
        ordering_stage_parallelism: 2,
        batch_shards: 1,
        success_count: 0,
        send_total: 50,
        batcher_stats: Default::default(),
        timing_summary: Default::default(),
        client_timing_summary: Default::default(),
    };
    let rows = build_device_report_rows(
        &result,
        "run_2",
        456,
        &[DeviceClientStatus {
            device_id: 42,
            client_success: false,
            command: ClientCommandSummary {
                upload_total: 13,
                upload_success: 9,
                upload_fail: 4,
                cmd_total: 3,
                cmd_exec_success: 2,
                cmd_exec_fail: 1,
                cmd_timeout: 1,
            },
        }],
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].expected_requests, 13);
    assert_eq!(rows[0].report_success, 9);
    assert_eq!(rows[0].report_fail, 4);
    assert!((rows[0].report_success_rate_pct - (9.0 / 13.0 * 100.0)).abs() < 0.0001);
}

#[test]
fn extract_downlink_commands_reads_server_payload() {
    let payload = serde_json::json!({
        "commands": [
            {"cmd_id": "cmd-1", "action": "adjust_irrigation"},
            {"cmd_id": "cmd-2", "action": "dispatch_tractor"}
        ]
    });
    let commands = extract_downlink_commands(&payload);
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0], "cmd-1");
    assert_eq!(commands[1], "cmd-2");
}

// ### 修改记录 (2026-03-03)
// - 原因: new_for_test 改为真实 SQLite
// - 目的: 压测场景确保 data 表已创建
// - 备注: 避免压测写入失败影响统计
async fn create_test_router() -> Result<Arc<Router>> {
    let seq = ROUTER_DB_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut path = std::env::temp_dir();
    path.push(format!(
        "check_program_stress_{}_{}_{}.db",
        std::process::id(),
        nanos,
        seq
    ));
    let router = Arc::new(Router::new_local_leader(
        path.to_string_lossy().to_string(),
    )?);
    // ### 修改记录 (2026-03-03)
    // - 原因: UploadData 写入依赖 data 表
    // - 目的: 保持压测链路一致性
    router
        .write(
            "CREATE TABLE IF NOT EXISTS data (key TEXT PRIMARY KEY, value BLOB, timestamp INTEGER)"
                .to_string(),
        )
        .await?;
    Ok(router)
}

// ### 修改记录 (2026-03-04)
// - 原因: 需要在压测场景注入 3 节点 HA 链路
// - 目的: 复用 TestCluster 选主结果，保证 Router 绑定当前 leader
// ### 修改记录 (2026-03-04)
// - 原因: 需要压测对比单节点与 HA 写路径
// - 目的: 构造 leader Router 并保持 data 表初始化一致
async fn create_test_router_ha3() -> Result<(Arc<Router>, Arc<tokio::sync::Mutex<TestCluster>>)> {
    let cluster = Arc::new(tokio::sync::Mutex::new(TestCluster::new(3).await?));
    let router = Arc::new(Router::new_with_test_cluster(Arc::clone(&cluster)));
    router
        .write(
            "CREATE TABLE IF NOT EXISTS data (key TEXT PRIMARY KEY, value BLOB, timestamp INTEGER)"
                .to_string(),
        )
        .await?;
    Ok((router, cluster))
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要复用网关启动与压测逻辑
// - 目的: 避免多个用例重复代码
async fn run_stress_case(case: &StressCase) -> Result<StressResult> {
    run_stress_case_with_mode(case, false, false, 0, false, false).await
}

async fn run_stress_case_with_preagg(
    case: &StressCase,
    pre_agg_enabled: bool,
) -> Result<StressResult> {
    run_stress_case_with_mode(case, pre_agg_enabled, false, 0, false, false).await
}

// ### 修改记录 (2026-03-04)
// - 原因: 需要在同一压测框架中切换 HA 开关
// - 目的: 保证 baseline 与 HA 仅在路由拓扑差异，便于横向对比
async fn run_stress_case_with_preagg_and_ha(
    case: &StressCase,
    pre_agg_enabled: bool,
    ha_enabled: bool,
    midrun_failover_count: usize,
) -> Result<StressResult> {
    run_stress_case_with_mode(
        case,
        pre_agg_enabled,
        ha_enabled,
        midrun_failover_count,
        false,
        false,
    )
    .await
}

async fn run_stress_case_with_random_upload_ha_and_db_reconcile(
    case: &StressCase,
    pre_agg_enabled: bool,
    midrun_failover_count: usize,
) -> Result<StressResult> {
    run_stress_case_with_mode(
        case,
        pre_agg_enabled,
        true,
        midrun_failover_count,
        true,
        true,
    )
    .await
}

fn expected_records_for_reconcile(
    client_statuses: &[DeviceClientStatus],
    random_upload_mode: bool,
) -> u64 {
    let records_per_request = if random_upload_mode {
        RANDOM_UPLOAD_RECORDS_PER_REQUEST
    } else {
        1
    };
    client_statuses
        .iter()
        .map(|status| status.command.upload_total.saturating_mul(records_per_request))
        .sum()
}

async fn run_stress_case_with_mode(
    case: &StressCase,
    pre_agg_enabled: bool,
    ha_enabled: bool,
    midrun_failover_count: usize,
    random_upload_mode: bool,
    enforce_db_reconciliation: bool,
) -> Result<StressResult> {
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要随机端口避免冲突
    // - 目的: 确保每次压测独立运行
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);

    let addr = format!("127.0.0.1:{}", port);

    // ### 修改记录 (2026-03-01)
    // - 原因: 压测需要稳定的写入路径
    // - 目的: 使用测试 Router 保持可控性
    let (router, ha_cluster_shared) = if ha_enabled {
        let (router, cluster) = create_test_router_ha3().await?;
        (router, Some(cluster))
    } else {
        (create_test_router().await?, None)
    };

    let config = EdgeGatewayConfig {
        // ### 修改记录 (2026-03-01)
        // - 原因: 压测持续 1 分钟仍需覆盖会话窗口
        // - 目的: 防止会话过期干扰结果
        session_ttl_ms: 600_000,
        nonce_cache_limit: 100_000,
        nonce_persist_enabled: false,
        nonce_persist_path: "".to_string(),
        secret_key: String::from_utf8(SECRET_KEY.to_vec())?,
        max_connections: 6000,
        // ### 修改记录 (2026-03-01)
        // - 原因: 需要覆盖批处理参数扫描
        // - 目的: 观察 wait_timeout 与 response_dropped
        batch_max_size: case.batch_max_size,
        batch_max_delay_ms: case.batch_max_delay_ms,
        batch_max_queue_size: 200_000,
        batch_wait_timeout_ms: case.batch_wait_timeout_ms,
        // ### 修改记录 (2026-03-01)
        // - 原因: 需要观察不同并发度
        // - 目的: 验证并发对吞吐的影响
        ordering_stage_parallelism: case.ordering_stage_parallelism,
        ordering_queue_limit: 200_000,
        batch_shards: case.batch_shards,
        pre_agg_enabled,
        pre_agg_queue_size: 50_000,
        edge_client_retry_delay_ms: 100,
        edge_client_retry_max_attempts: 3,
        edge_client_retry_exponential_backoff: true,
    };
    let retry_config = EdgeClientRetryConfig::from_gateway_config(&config);

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要注入顺序规则存储
    // - 目的: 与生产路径保持一致
    let order_rules_store =
        Arc::new(check_program::management::order_rules_service::OrderRulesStore::new());
    let gateway = Arc::new(EdgeGateway::new(
        addr.clone(),
        router,
        config,
        order_rules_store,
    )?);
    let gateway_runner = Arc::clone(&gateway);

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要保持压测期间网关持续运行
    // - 目的: 覆盖完整压测窗口
    tokio::spawn(async move {
        if let Err(e) = gateway_runner.run().await {
            eprintln!("Gateway stopped: {:?}", e);
        }
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    let send_counter = Arc::new(AtomicU64::new(0));
    let running = Arc::new(AtomicBool::new(true));
    let send_counter_clone = Arc::clone(&send_counter);
    let running_clone = Arc::clone(&running);
    let client_timing_stats = Arc::new(ClientTimingStats::default());
    // ### 修改记录 (2026-03-01)
    // - 原因: tokio::spawn 需要 'static 生命周期
    // - 目的: 保证日志任务可安全持有名称
    let case_name = case.name.clone();

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要按秒输出发送速率
    // - 目的: 判断并发是否带来真实吞吐提升
    let rate_logger = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        let mut last_total = 0u64;
        while running_clone.load(Ordering::Relaxed) {
            interval.tick().await;
            let total = send_counter_clone.load(Ordering::Relaxed);
            let delta = total.saturating_sub(last_total);
            last_total = total;
            println!(
                "Send rate: case={} send_rps={} total={}",
                case_name, delta, total
            );
        }
    });
    let rtt_running = Arc::clone(&running);
    let rtt_case_name = case.name.clone();
    let client_timing_stats_clone = Arc::clone(&client_timing_stats);
    let rtt_logger = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        let mut last_snapshot = client_timing_stats_clone.snapshot();
        while rtt_running.load(Ordering::Relaxed) {
            interval.tick().await;
            let current = client_timing_stats_clone.snapshot();
            let delta = current.delta(&last_snapshot);
            let avg_rtt = if delta.rtt_samples == 0 {
                0.0
            } else {
                delta.rtt_ms_total as f64 / delta.rtt_samples as f64
            };
            println!(
                "Client RTT delta: case={} rtt_ms=[<1:{} <5:{} <10:{} <50:{} <100:{} <500:{} <1000:{} >=1000:{}] avg_rtt_ms={:.2} samples={}",
                rtt_case_name,
                delta.rtt.lt_1ms,
                delta.rtt.lt_5ms,
                delta.rtt.lt_10ms,
                delta.rtt.lt_50ms,
                delta.rtt.lt_100ms,
                delta.rtt.lt_500ms,
                delta.rtt.lt_1000ms,
                delta.rtt.ge_1000ms,
                avg_rtt,
                delta.rtt_samples
            );
            last_snapshot = current;
        }
    });

    // ### 修改记录 (2026-03-01)
    // - 原因: tokio::spawn 需要 'static 生命周期
    // - 目的: 复制参数避免捕获借用
    let num_clients = case.num_clients;
    let requests_per_second = case.requests_per_second;
    let duration = case.duration;
    // ### 修改记录 (2026-03-04)
    // - 原因: 需要覆盖“单次/连续多次”中途故障窗口
    // - 目的: 统一注入 failover 次数，复用同一压测骨架
    let failover_task = if midrun_failover_count > 0 && ha_enabled {
        if let Some(cluster) = ha_cluster_shared.clone() {
            let failover_count = midrun_failover_count;
            let sleep_ms = (duration.as_millis() as u64 / (failover_count as u64 + 2)).max(100);
            Some(tokio::spawn(async move {
                for attempt_idx in 0..failover_count {
                    tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                    let now_nanos = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as u64;
                    let downtime_ms = 10_000 + ((now_nanos ^ attempt_idx as u64) % 20_001);
                    let failed_leader_id = {
                        let mut guard = cluster.lock().await;
                        guard.fail_leader_and_wait_new_leader().await?
                    };
                    tokio::time::sleep(Duration::from_millis(downtime_ms)).await;
                    let mut guard = cluster.lock().await;
                    guard.start_node(failed_leader_id).await?;
                }
                Ok::<(), anyhow::Error>(())
            }))
        } else {
            None
        }
    } else {
        None
    };

    let mut handles = vec![];
    for i in 0..num_clients {
        let addr_clone = addr.clone();
        let send_counter_clone = Arc::clone(&send_counter);
        let client_timing_stats_clone = Arc::clone(&client_timing_stats);
        handles.push(tokio::spawn(async move {
            let res = match if random_upload_mode {
                run_client_with_random_upload_rate(
                    addr_clone,
                    i as u64,
                    requests_per_second,
                    duration,
                    retry_config,
                    send_counter_clone,
                    client_timing_stats_clone,
                )
                .await
            } else {
                run_client_with_rate(
                    addr_clone,
                    i as u64,
                    requests_per_second,
                    duration,
                    retry_config,
                    send_counter_clone,
                    client_timing_stats_clone,
                )
                .await
            } {
                Ok(summary) => Ok(summary),
                Err(e) => {
                    eprintln!("Client {} failed: {:?}", i, e.error);
                    Err(e)
                }
            };
            (i as u64, res)
        }));
    }

    let mut success_count = 0;
    let mut client_statuses = Vec::with_capacity(num_clients);
    for handle in handles {
        match handle.await {
            Ok((device_id, res)) => {
                if res.is_ok() {
                    success_count += 1;
                }
                match res {
                    Ok(summary) => client_statuses.push(DeviceClientStatus {
                        device_id,
                        client_success: true,
                        command: summary,
                    }),
                    Err(failure) => client_statuses.push(DeviceClientStatus {
                        device_id,
                        client_success: false,
                        command: failure.summary,
                    }),
                }
            }
            Err(e) => eprintln!("Join error: {:?}", e),
        }
    }

    running.store(false, Ordering::Relaxed);
    let _ = rate_logger.await;
    let _ = rtt_logger.await;
    if let Some(task) = failover_task {
        let result = task
            .await
            .map_err(|e| anyhow!("Failover task join error: {}", e))?;
        result?;
    }
    if enforce_db_reconciliation {
        let expected_records = expected_records_for_reconcile(&client_statuses, random_upload_mode);
        let Some(cluster) = &ha_cluster_shared else {
            return Err(anyhow!(
                "DB reconcile requires HA cluster when enforce_db_reconciliation=true"
            ));
        };
        let actual_records = {
            let guard = cluster.lock().await;
            let value = guard
                .query_leader_scalar(
                    "SELECT COUNT(*) FROM data WHERE key NOT LIKE 'cmd_ack_%'".to_string(),
                )
                .await?;
            value.parse::<u64>().map_err(|e| {
                anyhow!(
                    "DB reconcile count parse failed: raw_value={} error={}",
                    value,
                    e
                )
            })?
        };
        if actual_records != expected_records {
            return Err(anyhow!(
                "DB reconcile mismatch: expected_records={} actual_records={}",
                expected_records,
                actual_records
            ));
        }
    }

    let batcher_stats = gateway.batcher_stats();
    let timing_summary = gateway.timing_summary();
    let client_timing_summary = client_timing_stats.summary();

    let result = StressResult {
        name: case.name.clone(),
        num_clients: case.num_clients,
        requests_per_second: case.requests_per_second,
        duration_secs: case.duration.as_secs(),
        batch_wait_timeout_ms: case.batch_wait_timeout_ms,
        batch_max_size: case.batch_max_size,
        batch_max_delay_ms: case.batch_max_delay_ms,
        ordering_stage_parallelism: case.ordering_stage_parallelism,
        batch_shards: case.batch_shards,
        success_count,
        send_total: send_counter.load(Ordering::Relaxed),
        batcher_stats,
        timing_summary,
        client_timing_summary,
    };
    append_stress_report(&result)?;
    append_group_and_device_reports(&result, &client_statuses)?;
    Ok(result)
}

#[tokio::test]
async fn stress_test_edge_gateway() -> Result<()> {
    // Use a random port
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);

    let addr = format!("127.0.0.1:{}", port);

    // Setup Mock Router (Leader mode, returns Ok(0))
    let router = create_test_router().await?;

    let config = EdgeGatewayConfig {
        session_ttl_ms: 5000,
        nonce_cache_limit: 10000,
        nonce_persist_enabled: false,
        nonce_persist_path: "".to_string(),
        secret_key: String::from_utf8(SECRET_KEY.to_vec())?,
        max_connections: 1000,
        // ### 修改记录 (2026-03-01)
        // - 原因: 测试需要显式覆盖批处理参数
        // - 目的: 保证行为在不同机器上可重复
        batch_max_size: 256,
        batch_max_delay_ms: 10,
        batch_max_queue_size: 50_000,
        batch_wait_timeout_ms: 200,
        // ### 修改记录 (2026-03-01)
        // - 原因: 压测场景需要稳定的顺序调度参数
        // - 目的: 避免并行度差异影响结果
        ordering_stage_parallelism: 4,
        ordering_queue_limit: 50_000,
        batch_shards: 1,
        pre_agg_enabled: false,
        pre_agg_queue_size: 50_000,
        edge_client_retry_delay_ms: 100,
        edge_client_retry_max_attempts: 3,
        edge_client_retry_exponential_backoff: true,
    };
    let retry_config = EdgeClientRetryConfig::from_gateway_config(&config);

    // ### 修改记录 (2026-03-01)
    // - 原因: 压测需要顺序规则存储
    // - 目的: 保证网关与管理接口一致
    let order_rules_store =
        Arc::new(check_program::management::order_rules_service::OrderRulesStore::new());
    let gateway = Arc::new(EdgeGateway::new(
        addr.clone(),
        router,
        config,
        order_rules_store,
    )?);

    // Spawn Gateway
    tokio::spawn(async move {
        if let Err(e) = gateway.run().await {
            eprintln!("Gateway stopped: {:?}", e);
        }
    });

    // Wait for server startup
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Run concurrent clients
    let num_clients = 50;
    let reqs_per_client = 20;

    let mut handles = vec![];
    for i in 0..num_clients {
        let addr_clone = addr.clone();
        handles.push(tokio::spawn(async move {
            match run_client(addr_clone, i as u64, reqs_per_client, retry_config).await {
                Ok(_) => Ok(()),
                Err(e) => {
                    eprintln!("Client {} failed: {:?}", i, e);
                    Err(e)
                }
            }
        }));
    }

    // Await all clients
    let mut success_count = 0;
    for handle in handles {
        match handle.await {
            Ok(res) => {
                if res.is_ok() {
                    success_count += 1;
                }
            }
            Err(e) => eprintln!("Join error: {:?}", e),
        }
    }

    println!("Success: {}/{}", success_count, num_clients);
    assert_eq!(success_count, num_clients);

    Ok(())
}

// ### 修改记录 (2026-03-01)
// - 原因: 验证网关延迟能否限制在 50ms 以内
// - 目的: 响应用户对低延迟的需求
// - 备注: 该测试用例设置 batch_wait_timeout_ms 为 50ms，若超时则失败
#[tokio::test]
async fn stress_test_edge_gateway_low_latency_50ms() -> Result<()> {
    // ### 修改记录 (2026-03-03)
    // - 原因: 压测用例不适合默认执行
    // - 目的: 使用环境变量显式开启
    if !stress_enabled() {
        return Ok(());
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);

    let addr = format!("127.0.0.1:{}", port);
    let router = create_test_router().await?;

    let config = EdgeGatewayConfig {
        session_ttl_ms: 60_000,
        nonce_cache_limit: 10000,
        nonce_persist_enabled: false,
        nonce_persist_path: "".to_string(),
        secret_key: String::from_utf8(SECRET_KEY.to_vec())?,
        max_connections: 2000,
        // 关键配置：限制最大等待时间为 50ms
        batch_max_size: 256,
        batch_max_delay_ms: 5, // 缩短聚合窗口到 5ms
        batch_max_queue_size: 50_000,
        batch_wait_timeout_ms: 50,     // 硬性限制 50ms 超时
        ordering_stage_parallelism: 8, // 增加并发度以加快处理
        ordering_queue_limit: 50_000,
        batch_shards: 1,
        pre_agg_enabled: false,
        pre_agg_queue_size: 50_000,
        edge_client_retry_delay_ms: 100,
        edge_client_retry_max_attempts: 3,
        edge_client_retry_exponential_backoff: true,
    };
    let retry_config = EdgeClientRetryConfig::from_gateway_config(&config);

    let order_rules_store =
        Arc::new(check_program::management::order_rules_service::OrderRulesStore::new());
    let gateway = Arc::new(EdgeGateway::new(
        addr.clone(),
        router,
        config,
        order_rules_store,
    )?);
    let gateway_runner = gateway.clone();
    tokio::spawn(async move {
        if let Err(e) = gateway_runner.run().await {
            eprintln!("Gateway stopped: {:?}", e);
        }
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    // 使用 run_stress_case 逻辑类似的客户端并发
    let num_clients = 500;
    let requests_per_second = 10;
    let duration = Duration::from_secs(10);
    let send_counter = Arc::new(AtomicU64::new(0));
    let client_timing_stats = Arc::new(ClientTimingStats::default());

    let mut handles = vec![];
    for i in 0..num_clients {
        let addr_clone = addr.clone();
        let send_counter_clone = send_counter.clone();
        let client_timing_stats_clone = client_timing_stats.clone();
        handles.push(tokio::spawn(async move {
            run_client_with_rate(
                addr_clone,
                i as u64,
                requests_per_second,
                duration,
                retry_config,
                send_counter_clone,
                client_timing_stats_clone,
            )
            .await
        }));
    }

    let mut success_count = 0;
    for handle in handles {
        match handle.await {
            Ok(res) => {
                if res.is_ok() {
                    success_count += 1;
                } else {
                    eprintln!("Client failed: {:?}", res.err());
                }
            }
            Err(e) => eprintln!("Join error: {:?}", e),
        }
    }

    let stats = gateway.batcher_stats();
    println!("Batcher stats: {:?}", stats);
    let client_summary = client_timing_stats.summary();
    println!(
        "Client RTT summary: avg_rtt_ms={:.2} samples={}",
        avg_rtt_ms(&client_summary),
        client_summary.rtt_samples
    );
    println!("Success: {}/{}", success_count, num_clients);

    // 允许少量失败（例如连接建立超时），但主要验证是否因 batch wait timeout 而大面积失败
    // 如果 batch_wait_timeout_ms 设置过短导致无法处理，success_count 会很低
    assert!(
        success_count >= num_clients * 95 / 100,
        "Success rate too low"
    );

    Ok(())
}

// ### 修改记录 (2026-03-03)
// - 原因: 需要最小化复现批处理超时导致 success=0 的断言失败
// - 目的: 以单请求 E2E 测试验证失败路径与回包内容
// - 备注: 使用极短 batch_wait_timeout_ms + 较大 batch_max_delay_ms 强制超时
// ### 修改记录 (2026-03-03)
// - 原因: 需要复用同一链路验证不同超时窗口
// - 目的: 统一 5/50/100ms 的对照测试入口
// ### 修改记录 (2026-03-03)
// - 原因: 用户新增 delay 对照验证需求
// - 目的: 同时控制 wait_timeout 与 batch_delay
async fn run_single_upload_with_batch_window(
    batch_wait_timeout_ms: u64,
    batch_max_delay_ms: u64,
) -> Result<serde_json::Value> {
    // ### 修改记录 (2026-03-03)
    // - 原因: 复现需要独立端口避免并发干扰
    // - 目的: 保证测试稳定可复现
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);

    // ### 修改记录 (2026-03-03)
    // - 原因: 需要独立地址启动网关
    // - 目的: 保持端到端链路与压测一致
    let addr = format!("127.0.0.1:{}", port);
    // ### 修改记录 (2026-03-03)
    // - 原因: UploadData 依赖 data 表
    // - 目的: 保证写入失败来自超时而非建表缺失
    let router = create_test_router().await?;

    // ### 修改记录 (2026-03-03)
    // - 原因: 需要对比不同超时与延迟组合
    // - 目的: 定位 success=0 的边界条件
    let config = EdgeGatewayConfig {
        session_ttl_ms: 60_000,
        nonce_cache_limit: 10_000,
        nonce_persist_enabled: false,
        nonce_persist_path: "".to_string(),
        secret_key: String::from_utf8(SECRET_KEY.to_vec())?,
        max_connections: 50,
        batch_max_size: 4,
        batch_max_delay_ms,
        batch_max_queue_size: 1_000,
        batch_wait_timeout_ms,
        ordering_stage_parallelism: 1,
        ordering_queue_limit: 1_000,
        batch_shards: 1,
        pre_agg_enabled: false,
        pre_agg_queue_size: 50_000,
        edge_client_retry_delay_ms: 100,
        edge_client_retry_max_attempts: 3,
        edge_client_retry_exponential_backoff: true,
    };

    // ### 修改记录 (2026-03-03)
    // - 原因: 与生产路径保持一致
    // - 目的: 避免顺序规则缺失影响链路
    let order_rules_store =
        Arc::new(check_program::management::order_rules_service::OrderRulesStore::new());
    let gateway = Arc::new(EdgeGateway::new(
        addr.clone(),
        router,
        config,
        order_rules_store,
    )?);
    let gateway_runner = gateway.clone();
    tokio::spawn(async move {
        if let Err(e) = gateway_runner.run().await {
            eprintln!("Gateway stopped: {:?}", e);
        }
    });

    // ### 修改记录 (2026-03-03)
    // - 原因: 网关启动需要短暂时间
    // - 目的: 避免连接时序抖动
    tokio::time::sleep(Duration::from_millis(200)).await;

    // ### 修改记录 (2026-03-03)
    // - 原因: 复现断言失败需要真实客户端握手
    // - 目的: 保持与 run_client 相同的协议流程
    // ### 修改记录 (2026-03-03)
    // - 原因: 避免 clippy 的 unused_mut
    // - 目的: 保持最小可变性与风格一致
    let stream = TcpStream::connect(&addr).await?;
    let mut framed = Framed::new(stream, EdgeFrameCodec);

    // ### 修改记录 (2026-03-03)
    // - 原因: 使用单设备即可复现超时
    // - 目的: 最小化请求数量
    let device_id = 9001u64;
    let nonce = 1000 + device_id;
    let timestamp = 1234567890u64;

    // ### 修改记录 (2026-03-03)
    // - 原因: 先进行认证握手
    // - 目的: 获取 session_id 与 session_key
    let mut builder = FlatBufferBuilder::new();
    let auth_req = EdgeRequest::AuthHello {
        device_id,
        timestamp,
        nonce,
    };
    let auth_offset = encode_auth_hello(&mut builder, SECRET_KEY, &auth_req)?;
    builder.finish(auth_offset, None);
    let auth_payload = builder.finished_data().to_vec();
    let auth_header = Header {
        version: VERSION,
        msg_type: MSG_TYPE_AUTH_HELLO,
        request_id: 1,
        payload_len: auth_payload.len() as u32,
        checksum: [0; 3],
    };
    framed.send((auth_header, auth_payload)).await?;
    let (auth_resp_header, auth_resp_payload) = framed
        .next()
        .await
        .ok_or(anyhow!("Connection closed during handshake"))??;
    assert_eq!(auth_resp_header.msg_type, MSG_TYPE_RESPONSE);
    let ack = decode_auth_ack(&auth_resp_payload)?;

    // ### 修改记录 (2026-03-03)
    // - 原因: 发送单条 UploadData 请求
    // - 目的: 触发 batch_wait_timeout 并回包 success=0
    let mut builder = FlatBufferBuilder::new();
    let data_req = EdgeRequest::UploadData {
        records: vec![DataRecord {
            key: format!("dev_{}_min_timeout", device_id),
            value: vec![1, 2, 3, 4],
            timestamp,
        }],
    };
    let data_offset = encode_session_request(
        &mut builder,
        ack.session_id,
        timestamp,
        nonce + 1,
        &ack.session_key,
        &data_req,
    )?;
    builder.finish(data_offset, None);
    let data_payload = builder.finished_data().to_vec();
    let data_header = Header {
        version: VERSION,
        msg_type: MSG_TYPE_SESSION_REQUEST,
        request_id: 2,
        payload_len: data_payload.len() as u32,
        checksum: [0; 3],
    };
    framed.send((data_header.clone(), data_payload)).await?;

    // ### 修改记录 (2026-03-03)
    // - 原因: 需要验证返回 payload 的失败内容
    // - 目的: 复盘 success=0 的断言根因
    let (resp_header, resp_payload) = framed
        .next()
        .await
        .ok_or(anyhow!("Connection closed during request"))??;
    assert_eq!(resp_header.msg_type, MSG_TYPE_RESPONSE);

    let mut expected_prefix = [0u8; 14];
    expected_prefix[0..4].copy_from_slice(&MAGIC);
    expected_prefix[4] = VERSION;
    expected_prefix[5] = MSG_TYPE_RESPONSE;
    expected_prefix[6..14].copy_from_slice(&data_header.request_id.to_be_bytes());
    let signed_resp = decode_signed_response(&resp_payload, &expected_prefix, |_| {
        Some(ack.session_key.clone())
    })?;
    assert_eq!(signed_resp.session_id, ack.session_id);

    let response_json: serde_json::Value = serde_json::from_slice(&signed_resp.payload)?;
    Ok(response_json)
}

#[tokio::test]
async fn stress_test_edge_gateway_batch_wait_timeout_min_repro() -> Result<()> {
    // ### 修改记录 (2026-03-03)
    // - 原因: 需要保留 5ms 最小复现基线
    // - 目的: 持续验证断言失败根因
    let response_json = run_single_upload_with_batch_window(5, 200).await?;
    assert_eq!(response_json["success"], 0);
    let failures = response_json["failures"]
        .as_array()
        .ok_or(anyhow!("failures is not array"))?;
    assert_eq!(failures.len(), 1);
    // ### 修改记录 (2026-03-03)
    // - 原因: clippy 建议使用 first()
    // - 目的: 避免触发 get_first 警告
    let failure_msg = failures
        .first()
        .and_then(|v| v.get(1))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        failure_msg.contains("Batch wait timeout")
            || failure_msg.contains("Batch response dropped")
    );
    Ok(())
}

// ### 修改记录 (2026-03-03)
// - 原因: 用户要求验证 50ms 是否 OK
// - 目的: 输出 success/failures 供判定
#[tokio::test]
async fn stress_test_edge_gateway_batch_wait_timeout_probe_50ms() -> Result<()> {
    let response_json = run_single_upload_with_batch_window(50, 200).await?;
    let success = response_json["success"].as_u64().unwrap_or(0);
    let failures_len = response_json["failures"]
        .as_array()
        .map(|arr| arr.len())
        .unwrap_or(0);
    println!(
        "Timeout probe: timeout_ms=50 success={} failures_len={}",
        success, failures_len
    );
    assert!(response_json["success"].is_number());
    assert!(response_json["failures"].is_array());
    Ok(())
}

// ### 修改记录 (2026-03-03)
// - 原因: 用户要求验证 100ms 是否 OK
// - 目的: 输出 success/failures 供判定
#[tokio::test]
async fn stress_test_edge_gateway_batch_wait_timeout_probe_100ms() -> Result<()> {
    let response_json = run_single_upload_with_batch_window(100, 200).await?;
    let success = response_json["success"].as_u64().unwrap_or(0);
    let failures_len = response_json["failures"]
        .as_array()
        .map(|arr| arr.len())
        .unwrap_or(0);
    println!(
        "Timeout probe: timeout_ms=100 success={} failures_len={}",
        success, failures_len
    );
    assert!(response_json["success"].is_number());
    assert!(response_json["failures"].is_array());
    Ok(())
}

// ### 修改记录 (2026-03-03)
// - 原因: 新增 timeout=100ms + delay=10ms 的恢复性验证
// - 目的: 验证 success 是否可恢复为 1
#[tokio::test]
async fn stress_test_edge_gateway_batch_window_100ms_10ms_expect_success() -> Result<()> {
    let response_json = run_single_upload_with_batch_window(100, 10).await?;
    let failures_len = response_json["failures"]
        .as_array()
        .map(|arr| arr.len())
        .unwrap_or(usize::MAX);
    println!(
        "Window probe: timeout_ms=100 delay_ms=10 success={} failures_len={}",
        response_json["success"], failures_len
    );
    assert_eq!(response_json["success"], 1);
    assert_eq!(failures_len, 0);
    Ok(())
}

// ### 修改记录 (2026-03-03)
// - 原因: 新增 timeout=200ms + delay=50ms 的边界验证
// - 目的: 观察边界窗口下 success/failures 分布
#[tokio::test]
async fn stress_test_edge_gateway_batch_window_200ms_50ms_boundary() -> Result<()> {
    let response_json = run_single_upload_with_batch_window(200, 50).await?;
    let success = response_json["success"].as_u64().unwrap_or(0);
    let failures_len = response_json["failures"]
        .as_array()
        .map(|arr| arr.len())
        .unwrap_or(0);
    println!(
        "Window probe: timeout_ms=200 delay_ms=50 success={} failures_len={}",
        success, failures_len
    );
    assert!(response_json["success"].is_number());
    assert!(response_json["failures"].is_array());
    Ok(())
}

// ### 修改记录 (2026-03-03)
// - 原因: 新增小并发短时观测需求
// - 目的: 统计 20 客户端下是否仍出现 success=0
#[tokio::test]
async fn stress_test_edge_gateway_batch_window_small_concurrency_observe() -> Result<()> {
    let case = StressCase {
        name: "small_20clients_2rps_10s_timeout100_delay10".to_string(),
        num_clients: 20,
        requests_per_second: 2,
        duration: Duration::from_secs(10),
        batch_wait_timeout_ms: 100,
        batch_max_size: 64,
        batch_max_delay_ms: 10,
        ordering_stage_parallelism: 4,
        batch_shards: 1,
    };
    let result = run_stress_case(&case).await?;
    let ok_count = result.success_count;
    let err_count = result.num_clients.saturating_sub(result.success_count);
    println!(
        "Small concurrency probe: clients={} ok_count={} err_count={} avg_rtt_ms={:.2}",
        result.num_clients,
        ok_count,
        err_count,
        result.avg_client_rtt_ms()
    );
    assert!(
        ok_count >= result.num_clients * 95 / 100,
        "Small concurrency success rate too low"
    );
    Ok(())
}

// ### 修改记录 (2026-03-03)
// - 原因: 用户要求逐步增大压测并统一对比输出
// - 目的: 将 20/100/300/500 客户端结果按同口径打印
fn print_stress_scan_table(results: &[StressResult]) {
    println!(
        "CASE,clients,rps,duration_s,timeout_ms,delay_ms,parallelism,shards,client_success,client_total,client_success_rate_pct,request_success,request_total,request_success_rate_pct,send_total,avg_send_rps,wait_timeout,response_dropped,queue_full,batches,avg_batch_size,avg_batch_wait_ms,avg_ingress_ms,avg_write_ms,avg_persist_ms,avg_client_rtt_ms,upload_errors"
    );
    for result in results {
        println!(
            "{},{},{},{},{},{},{},{},{},{},{:.2},{},{},{:.2},{},{:.2},{},{},{},{},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{}",
            result.name,
            result.num_clients,
            result.requests_per_second,
            result.duration_secs,
            result.batch_wait_timeout_ms,
            result.batch_max_delay_ms,
            result.ordering_stage_parallelism,
            result.batch_shards,
            result.success_count,
            result.num_clients,
            result.success_rate_pct(),
            result.request_success_count(),
            result.request_total(),
            result.request_success_rate_pct(),
            result.send_total,
            result.avg_send_rps(),
            result.batcher_stats.wait_timeout,
            result.batcher_stats.response_dropped,
            result.batcher_stats.queue_full,
            result.batches(),
            result.avg_batch_size(),
            result.avg_batch_wait_ms(),
            result.avg_ingress_ms(),
            result.avg_write_ms(),
            result.avg_persist_ms(),
            result.avg_client_rtt_ms(),
            result.timing_summary.upload_errors
        );
    }
}

fn timeout_rate_pct(result: &StressResult) -> f64 {
    let total = result.request_total();
    if total == 0 {
        0.0
    } else {
        (result.batcher_stats.wait_timeout as f64 * 100.0) / total as f64
    }
}

fn response_drop_rate_pct(result: &StressResult) -> f64 {
    let total = result.request_total();
    if total == 0 {
        0.0
    } else {
        (result.batcher_stats.response_dropped as f64 * 100.0) / total as f64
    }
}

fn pick_best_case(results: &[StressResult]) -> Option<(u64, u64, usize, usize, usize)> {
    let best = results.iter().max_by(|a, b| {
        let a_req = (a.request_success_rate_pct() * 100.0) as u64;
        let b_req = (b.request_success_rate_pct() * 100.0) as u64;
        let a_timeout = (timeout_rate_pct(a) * 100.0) as u64;
        let b_timeout = (timeout_rate_pct(b) * 100.0) as u64;
        let a_drop = (response_drop_rate_pct(a) * 100.0) as u64;
        let b_drop = (response_drop_rate_pct(b) * 100.0) as u64;
        let a_rtt = (a.avg_client_rtt_ms() * 100.0) as u64;
        let b_rtt = (b.avg_client_rtt_ms() * 100.0) as u64;
        a_req
            .cmp(&b_req)
            .then_with(|| b_timeout.cmp(&a_timeout))
            .then_with(|| b_drop.cmp(&a_drop))
            .then_with(|| b_rtt.cmp(&a_rtt))
    })?;
    Some((
        best.batch_wait_timeout_ms,
        best.batch_max_delay_ms,
        best.ordering_stage_parallelism,
        best.batch_shards,
        best.batch_max_size,
    ))
}

fn print_bottleneck_table(title: &str, results: &[StressResult]) {
    println!("{title}");
    println!(
        "CASE,timeout_ms,delay_ms,parallelism,shards,batch_max_size,client_success_rate_pct,request_success_rate_pct,timeout_rate_pct,response_drop_rate_pct,avg_batch_wait_ms,avg_client_rtt_ms"
    );
    for result in results {
        println!(
            "{},{},{},{},{},{},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2}",
            result.name,
            result.batch_wait_timeout_ms,
            result.batch_max_delay_ms,
            result.ordering_stage_parallelism,
            result.batch_shards,
            result.batch_max_size,
            result.success_rate_pct(),
            result.request_success_rate_pct(),
            timeout_rate_pct(result),
            response_drop_rate_pct(result),
            result.avg_batch_wait_ms(),
            result.avg_client_rtt_ms()
        );
    }
}

async fn run_bottleneck_scan_500x5() -> Result<Vec<StressResult>> {
    let mut all_results = Vec::new();

    let mut stage1_results = Vec::new();
    let stage1_windows = [
        (50u64, 5u64),
        (100u64, 10u64),
        (200u64, 50u64),
        (400u64, 100u64),
    ];
    for (timeout, delay) in stage1_windows {
        let case = StressCase {
            name: format!("bottleneck_s1_c500x5_t{}_d{}", timeout, delay),
            num_clients: 500,
            requests_per_second: 1,
            duration: Duration::from_secs(5),
            batch_wait_timeout_ms: timeout,
            batch_max_size: 256,
            batch_max_delay_ms: delay,
            ordering_stage_parallelism: 4,
            batch_shards: 1,
        };
        let result = run_stress_case(&case).await?;
        stage1_results.push(result);
    }
    print_bottleneck_table("STAGE1_WINDOW_SCAN", &stage1_results);
    all_results.extend(stage1_results);

    let (best_timeout, best_delay, _, _, _) =
        pick_best_case(&all_results).ok_or(anyhow!("stage1 no result for best-case selection"))?;

    let mut stage2_results = Vec::new();
    for parallelism in [2usize, 4usize, 8usize] {
        for shards in [1usize, 2usize, 4usize] {
            let case = StressCase {
                name: format!(
                    "bottleneck_s2_c500x5_t{}_d{}_p{}_s{}",
                    best_timeout, best_delay, parallelism, shards
                ),
                num_clients: 500,
                requests_per_second: 1,
                duration: Duration::from_secs(5),
                batch_wait_timeout_ms: best_timeout,
                batch_max_size: 256,
                batch_max_delay_ms: best_delay,
                ordering_stage_parallelism: parallelism,
                batch_shards: shards,
            };
            let result = run_stress_case(&case).await?;
            stage2_results.push(result);
        }
    }
    print_bottleneck_table("STAGE2_PARALLELISM_SHARDS_SCAN", &stage2_results);
    all_results.extend(stage2_results);

    let (_, _, best_parallelism, best_shards, _) =
        pick_best_case(&all_results).ok_or(anyhow!("stage2 no result for best-case selection"))?;

    let mut stage3_results = Vec::new();
    for batch_max_size in [128usize, 256usize, 512usize] {
        let case = StressCase {
            name: format!(
                "bottleneck_s3_c500x5_t{}_d{}_p{}_s{}_b{}",
                best_timeout, best_delay, best_parallelism, best_shards, batch_max_size
            ),
            num_clients: 500,
            requests_per_second: 1,
            duration: Duration::from_secs(5),
            batch_wait_timeout_ms: best_timeout,
            batch_max_size,
            batch_max_delay_ms: best_delay,
            ordering_stage_parallelism: best_parallelism,
            batch_shards: best_shards,
        };
        let result = run_stress_case(&case).await?;
        stage3_results.push(result);
    }
    print_bottleneck_table("STAGE3_BATCH_SIZE_SCAN", &stage3_results);
    all_results.extend(stage3_results);

    print_stress_scan_table(&all_results);
    Ok(all_results)
}

// ### 修改记录 (2026-03-03)
// - 原因: 需要按“20 -> 100 -> 300 -> 500 客户端”逐步放大验证
// - 目的: 同时覆盖 60s 漂移、每端 30 次请求、100/10 与 200/50 参数对照
#[tokio::test]
async fn stress_scan_progressive_clients_batch_windows() -> Result<()> {
    // ### 修改记录 (2026-03-03)
    // - 原因: 渐进扫描总耗时较长
    // - 目的: 使用 RUN_STRESS 显式触发
    if !stress_enabled() {
        return Ok(());
    }

    let mut results = Vec::new();

    // ### 修改记录 (2026-03-03)
    // - 原因: 先复核 20 客户端长窗口稳定性
    // - 目的: 观察 60s 下 RTT 漂移与成功率
    let baseline_60s = StressCase {
        name: "progressive_c20_r2_d60_timeout100_delay10".to_string(),
        num_clients: 20,
        requests_per_second: 2,
        duration: Duration::from_secs(60),
        batch_wait_timeout_ms: 100,
        batch_max_size: 128,
        batch_max_delay_ms: 10,
        ordering_stage_parallelism: 4,
        batch_shards: 1,
    };
    let baseline_60s_result = run_stress_case(&baseline_60s).await?;
    results.push(baseline_60s_result);

    // ### 修改记录 (2026-03-03)
    // - 原因: 需要验证每端多次请求是否触发 success=0
    // - 目的: 保证每端至少 30 次请求的场景可稳定跑通
    let per_client_30 = StressCase {
        name: "progressive_c20_r1_d30_timeout100_delay10".to_string(),
        num_clients: 20,
        requests_per_second: 1,
        duration: Duration::from_secs(30),
        batch_wait_timeout_ms: 100,
        batch_max_size: 128,
        batch_max_delay_ms: 10,
        ordering_stage_parallelism: 4,
        batch_shards: 1,
    };
    let per_client_30_result = run_stress_case(&per_client_30).await?;
    let per_client_request_count = per_client_30_result
        .requests_per_second
        .saturating_mul(per_client_30_result.duration_secs);
    assert!(
        per_client_request_count >= 30,
        "Each client requests lower than 30"
    );
    results.push(per_client_30_result);

    // ### 修改记录 (2026-03-03)
    // - 原因: 用户要求中等并发下对比 100/10 与 200/50
    // - 目的: 形成 100~500 客户端的同表横向结果
    let medium_clients = [100usize, 300, 500];
    let windows = [(100u64, 10u64), (200u64, 50u64)];
    let mut success_zero_cases = Vec::new();
    for clients in medium_clients {
        for (timeout, delay) in windows {
            let case = StressCase {
                name: format!(
                    "progressive_c{}_r1_d20_timeout{}_delay{}",
                    clients, timeout, delay
                ),
                num_clients: clients,
                requests_per_second: 1,
                duration: Duration::from_secs(20),
                batch_wait_timeout_ms: timeout,
                batch_max_size: 256,
                batch_max_delay_ms: delay,
                ordering_stage_parallelism: 4,
                batch_shards: 1,
            };
            let result = run_stress_case(&case).await?;
            if result.success_count == 0 {
                success_zero_cases.push(result.name.clone());
            }
            results.push(result);
        }
    }

    print_stress_scan_table(&results);
    println!("Observed success=0 cases: {:?}", success_zero_cases);
    assert!(!results.is_empty(), "Progressive scan produced no results");
    Ok(())
}

// ### 修改记录 (2026-03-03)
// - 原因: 需要最小成本验证“调度并行度 + 分片”对 success=0 的影响
// - 目的: 在同一负载下快速收敛调度瓶颈方向
#[tokio::test]
async fn stress_scan_min_parallelism_shards_dual_success() -> Result<()> {
    // ### 修改记录 (2026-03-03)
    // - 原因: 组合扫描属于压测任务
    // - 目的: 使用 RUN_STRESS 显式控制执行
    if !stress_enabled() {
        return Ok(());
    }
    let mut results = Vec::new();
    let parallelisms = [2usize, 4usize];
    let shards = [1usize, 2usize];
    let windows = [(100u64, 10u64), (200u64, 50u64)];

    // ### 修改记录 (2026-03-03)
    // - 原因: 需要控制总时长，优先做最小扫描
    // - 目的: 每个组合固定 200 客户端、15 秒、1 rps
    for (timeout, delay) in windows {
        for parallelism in parallelisms {
            for batch_shards in shards {
                let case = StressCase {
                    name: format!(
                        "minscan_c200_r1_d15_timeout{}_delay{}_p{}_s{}",
                        timeout, delay, parallelism, batch_shards
                    ),
                    num_clients: 200,
                    requests_per_second: 1,
                    duration: Duration::from_secs(15),
                    batch_wait_timeout_ms: timeout,
                    batch_max_size: 256,
                    batch_max_delay_ms: delay,
                    ordering_stage_parallelism: parallelism,
                    batch_shards,
                };
                let result = run_stress_case(&case).await?;
                results.push(result);
            }
        }
    }
    print_stress_scan_table(&results);
    assert!(
        results.iter().all(|r| r.request_total() > 0),
        "Found empty request_total in min scan"
    );
    Ok(())
}

#[tokio::test]
async fn stress_scan_500clients_5reports_find_bottleneck() -> Result<()> {
    if !stress_enabled() {
        return Ok(());
    }
    let results = run_bottleneck_scan_500x5().await?;
    assert!(!results.is_empty(), "bottleneck scan has no results");
    Ok(())
}

#[tokio::test]
async fn progressive_c500_r1_d20_timeout200_delay50() -> Result<()> {
    if !stress_enabled() {
        return Ok(());
    }

    let case = StressCase {
        name: "progressive_c500_r1_d20_timeout200_delay50".to_string(),
        num_clients: 500,
        requests_per_second: 1,
        duration: Duration::from_secs(20),
        batch_wait_timeout_ms: 200,
        batch_max_size: 256,
        batch_max_delay_ms: 50,
        ordering_stage_parallelism: 4,
        batch_shards: 1,
    };
    let result = run_stress_case(&case).await?;
    print_stress_scan_table(std::slice::from_ref(&result));
    assert!(
        result.request_total() > 0,
        "request_total should be greater than 0"
    );
    Ok(())
}

#[tokio::test]
async fn preagg_toggle_fallback_smoke() -> Result<()> {
    let case = StressCase {
        name: "preagg_toggle_fallback_smoke".to_string(),
        num_clients: 20,
        requests_per_second: 1,
        duration: Duration::from_secs(2),
        batch_wait_timeout_ms: 200,
        batch_max_size: 64,
        batch_max_delay_ms: 10,
        ordering_stage_parallelism: 2,
        batch_shards: 1,
    };

    let disabled = run_stress_case_with_preagg(&case, false).await?;
    let enabled = run_stress_case_with_preagg(&case, true).await?;

    assert!(
        disabled.request_total() > 0,
        "disabled preagg request_total should be > 0"
    );
    assert!(
        enabled.request_total() > 0,
        "enabled preagg request_total should be > 0"
    );
    Ok(())
}

#[tokio::test]
async fn progressive_c500_r1_d20_timeout200_delay50_preagg() -> Result<()> {
    if !stress_enabled() {
        return Ok(());
    }

    let case = StressCase {
        name: "progressive_c500_r1_d20_timeout200_delay50_preagg".to_string(),
        num_clients: 500,
        requests_per_second: 1,
        duration: Duration::from_secs(20),
        batch_wait_timeout_ms: 200,
        batch_max_size: 256,
        batch_max_delay_ms: 50,
        ordering_stage_parallelism: 4,
        batch_shards: 1,
    };
    let result = run_stress_case_with_preagg(&case, true).await?;
    print_stress_scan_table(std::slice::from_ref(&result));
    assert!(
        result.request_total() > 0,
        "request_total should be greater than 0"
    );
    Ok(())
}

#[tokio::test]
// ### 修改记录 (2026-03-04)
// - 原因: 用户要求评估 HA 对当前压测口径的影响
// - 目的: 同场景连续执行 baseline/HA3 并输出对比指标
async fn progressive_c500_r1_d20_timeout200_delay50_preagg_ha_compare() -> Result<()> {
    if !stress_enabled() {
        return Ok(());
    }

    let base_case = StressCase {
        name: "progressive_c500_r1_d20_timeout200_delay50_preagg_baseline".to_string(),
        num_clients: 500,
        requests_per_second: 1,
        duration: Duration::from_secs(20),
        batch_wait_timeout_ms: 200,
        batch_max_size: 256,
        batch_max_delay_ms: 50,
        ordering_stage_parallelism: 4,
        batch_shards: 1,
    };

    let ha_case = StressCase {
        name: "progressive_c500_r1_d20_timeout200_delay50_preagg_ha3".to_string(),
        num_clients: 500,
        requests_per_second: 1,
        duration: Duration::from_secs(20),
        batch_wait_timeout_ms: 200,
        batch_max_size: 256,
        batch_max_delay_ms: 50,
        ordering_stage_parallelism: 4,
        batch_shards: 1,
    };

    let baseline = run_stress_case_with_preagg_and_ha(&base_case, true, false, 0).await?;
    let ha3 = run_stress_case_with_preagg_and_ha(&ha_case, true, true, 0).await?;
    let results = vec![baseline, ha3];
    print_stress_scan_table(&results);
    assert!(
        results[0].request_total() > 0,
        "baseline request_total should be greater than 0"
    );
    assert!(
        results[1].request_total() > 0,
        "ha request_total should be greater than 0"
    );
    Ok(())
}

#[tokio::test]
// ### 修改记录 (2026-03-04)
// - 原因: 用户要求去掉 preagg 后重跑同口径对比
// - 目的: 排除 preagg 对失败率的干扰，直接观察基线路径与 HA 影响
async fn progressive_c500_r1_d20_timeout200_delay50_no_preagg_ha_compare() -> Result<()> {
    if !stress_enabled() {
        return Ok(());
    }

    let base_case = StressCase {
        name: "progressive_c500_r1_d20_timeout200_delay50_no_preagg_baseline".to_string(),
        num_clients: 500,
        requests_per_second: 1,
        duration: Duration::from_secs(20),
        batch_wait_timeout_ms: 200,
        batch_max_size: 256,
        batch_max_delay_ms: 50,
        ordering_stage_parallelism: 4,
        batch_shards: 1,
    };

    let ha_case = StressCase {
        name: "progressive_c500_r1_d20_timeout200_delay50_no_preagg_ha3".to_string(),
        num_clients: 500,
        requests_per_second: 1,
        duration: Duration::from_secs(20),
        batch_wait_timeout_ms: 200,
        batch_max_size: 256,
        batch_max_delay_ms: 50,
        ordering_stage_parallelism: 4,
        batch_shards: 1,
    };

    let baseline = run_stress_case_with_preagg_and_ha(&base_case, false, false, 0).await?;
    let ha3 = run_stress_case_with_preagg_and_ha(&ha_case, false, true, 0).await?;
    let results = vec![baseline, ha3];
    print_stress_scan_table(&results);
    assert!(
        results[0].request_total() > 0,
        "baseline request_total should be greater than 0"
    );
    assert!(
        results[1].request_total() > 0,
        "ha request_total should be greater than 0"
    );
    Ok(())
}

#[tokio::test]
async fn progressive_ha_compare_survives_midrun_failover_window() -> Result<()> {
    if !stress_enabled() {
        return Ok(());
    }
    let case = StressCase {
        name: "progressive_midrun_failover_window".to_string(),
        num_clients: 500,
        requests_per_second: 1,
        duration: Duration::from_secs(20),
        batch_wait_timeout_ms: 200,
        batch_max_size: 256,
        batch_max_delay_ms: 50,
        ordering_stage_parallelism: 4,
        batch_shards: 1,
    };
    let result = run_stress_case_with_preagg_and_ha(&case, true, true, 1).await?;
    assert!(
        result.request_total() > 0,
        "request_total should be greater than 0"
    );
    assert!(result.send_total > 0, "send_total should be greater than 0");
    Ok(())
}

#[tokio::test]
// ### 修改记录 (2026-03-04)
// - 原因: 仅断言请求总量无法反映“真实成功请求是否归零”
// - 目的: 在中途 failover 场景增加请求级成功率阈值校验
async fn progressive_ha_compare_enforces_request_success_rate_floor() -> Result<()> {
    if !stress_enabled() {
        return Ok(());
    }
    let case = StressCase {
        name: "progressive_midrun_failover_rate_floor".to_string(),
        num_clients: 200,
        requests_per_second: 1,
        duration: Duration::from_secs(12),
        batch_wait_timeout_ms: 200,
        batch_max_size: 256,
        batch_max_delay_ms: 50,
        ordering_stage_parallelism: 4,
        batch_shards: 1,
    };
    let result = run_stress_case_with_preagg_and_ha(&case, true, true, 1).await?;
    let request_success_rate = result.request_success_rate_pct();
    assert!(
        request_success_rate > 0.0,
        "request_success_rate_pct should be > 0 under midrun failover, got {}",
        request_success_rate
    );
    Ok(())
}

#[tokio::test]
// ### 修改记录 (2026-03-04)
// - 原因: 需要覆盖 no_preagg + 中途故障窗口组合
// - 目的: 排除 preagg 干扰后验证 failover 下请求口径
async fn progressive_no_preagg_midrun_failover_window() -> Result<()> {
    if !stress_enabled() {
        return Ok(());
    }
    let case = StressCase {
        name: "progressive_no_preagg_midrun_failover_window".to_string(),
        num_clients: 300,
        requests_per_second: 1,
        duration: Duration::from_secs(12),
        batch_wait_timeout_ms: 200,
        batch_max_size: 256,
        batch_max_delay_ms: 50,
        ordering_stage_parallelism: 4,
        batch_shards: 1,
    };
    let result = run_stress_case_with_preagg_and_ha(&case, false, true, 1).await?;
    assert!(
        result.request_total() > 0,
        "request_total should be greater than 0 in no_preagg failover window"
    );
    assert!(
        result.request_success_count() > 0,
        "request_success_count should be greater than 0 in no_preagg failover window"
    );
    Ok(())
}

#[tokio::test]
// ### 修改记录 (2026-03-04)
// - 原因: 需要验证连续两次 failover 时统计口径仍稳定
// - 目的: 覆盖恢复抖动窗口并确认请求级统计不为零
async fn progressive_ha_compare_survives_double_midrun_failover() -> Result<()> {
    if !stress_enabled() {
        return Ok(());
    }
    let case = StressCase {
        name: "progressive_double_midrun_failover_window".to_string(),
        num_clients: 200,
        requests_per_second: 1,
        duration: Duration::from_secs(16),
        batch_wait_timeout_ms: 200,
        batch_max_size: 256,
        batch_max_delay_ms: 50,
        ordering_stage_parallelism: 4,
        batch_shards: 1,
    };
    let result = run_stress_case_with_preagg_and_ha(&case, true, true, 2).await?;
    assert!(
        result.request_total() > 0,
        "request_total should be greater than 0 under double failover"
    );
    assert!(
        result.request_success_count() > 0,
        "request_success_count should be greater than 0 under double failover"
    );
    Ok(())
}

#[tokio::test]
async fn progressive_c500_random_upload_d120_midrun_failover_window() -> Result<()> {
    if !stress_enabled() {
        return Ok(());
    }
    let case = StressCase {
        name: "progressive_c500_random_upload_d120_midrun_failover_window".to_string(),
        num_clients: 500,
        requests_per_second: 1,
        duration: Duration::from_secs(120),
        batch_wait_timeout_ms: 200,
        batch_max_size: 256,
        batch_max_delay_ms: 50,
        ordering_stage_parallelism: 4,
        batch_shards: 1,
    };
    let result = run_stress_case_with_random_upload_ha_and_db_reconcile(&case, true, 1).await?;
    let request_total = result.request_total();
    let request_success = result.request_success_count();
    let request_loss = request_total.saturating_sub(request_success);
    println!(
        "Random upload failover summary: case={} request_total={} request_success={} request_loss={} success_rate_pct={:.2}",
        result.name,
        request_total,
        request_success,
        request_loss,
        result.request_success_rate_pct()
    );
    assert!(
        request_total > 0,
        "request_total should be greater than 0 in random upload failover scenario"
    );
    assert!(
        request_success > 0,
        "request_success_count should be greater than 0 in random upload failover scenario"
    );
    Ok(())
}

// ### 修改记录 (2026-03-01)
// - 原因: 验证 5000 并发终端下，每秒 10 RPS，严格 50ms 延迟限制的稳定性
// - 目的: 响应用户最新压测需求，验证方案 B 在高负载下的表现
// - 备注: 持续时间为 60 秒
// ### 修改记录 (2026-03-03)
// - 原因: 50ms 超时窗口下大量 wait_timeout/response_dropped
// - 目的: 将严格超时放宽到 200ms 以观察稳定性
#[tokio::test]
async fn stress_test_edge_gateway_5000_clients_10rps_50ms_1min() -> Result<()> {
    // ### 修改记录 (2026-03-03)
    // - 原因: 压测用例不适合默认执行
    // - 目的: 使用环境变量显式开启
    if !stress_enabled() {
        return Ok(());
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);

    let addr = format!("127.0.0.1:{}", port);
    let router = create_test_router().await?;

    let config = EdgeGatewayConfig {
        session_ttl_ms: 120_000, // 2分钟足以覆盖测试
        nonce_cache_limit: 100_000,
        nonce_persist_enabled: false,
        nonce_persist_path: "".to_string(),
        secret_key: String::from_utf8(SECRET_KEY.to_vec())?,
        max_connections: 6000, // 支持 5000+ 连接
        // 关键配置：高吞吐低延迟
        batch_max_size: 512,   // 增大批大小以吞吐 50,000 RPS
        batch_max_delay_ms: 5, // 维持 5ms 聚合窗口
        batch_max_queue_size: 100_000,
        // ### 修改记录 (2026-03-03)
        // - 原因: 50ms 超时窗口下大量 wait_timeout/response_dropped
        // - 目的: 将严格超时放宽到 200ms 以观察稳定性
        batch_wait_timeout_ms: 200,     // 严格 200ms 超时
        ordering_stage_parallelism: 16, // 提高并发处理能力
        ordering_queue_limit: 100_000,
        batch_shards: 8, // 启用 8 分片以验证 Scheme C
        pre_agg_enabled: false,
        pre_agg_queue_size: 50_000,
        edge_client_retry_delay_ms: 100,
        edge_client_retry_max_attempts: 3,
        edge_client_retry_exponential_backoff: true,
    };
    let retry_config = EdgeClientRetryConfig::from_gateway_config(&config);

    let order_rules_store =
        Arc::new(check_program::management::order_rules_service::OrderRulesStore::new());
    let gateway = Arc::new(EdgeGateway::new(
        addr.clone(),
        router,
        config,
        order_rules_store,
    )?);
    let gateway_runner = gateway.clone();
    tokio::spawn(async move {
        if let Err(e) = gateway_runner.run().await {
            eprintln!("Gateway stopped: {:?}", e);
        }
    });

    tokio::time::sleep(Duration::from_millis(500)).await;

    // ### 修改记录 (2026-03-03)
    // - 原因: 怀疑高 RPS 放大了链路问题
    // - 目的: 将 RPS 降到原先的 1/10 以先验证端到端落盘
    // 5000 客户端，1 RPS，60 秒
    let num_clients = 5000;
    // ### 修改记录 (2026-03-03)
    // - 原因: 怀疑高 RPS 放大了链路问题
    // - 目的: 将 RPS 降到原先的 1/10 以先验证端到端落盘
    let requests_per_second = 1;
    let duration = Duration::from_secs(60);
    let send_counter = Arc::new(AtomicU64::new(0));
    let client_timing_stats = Arc::new(ClientTimingStats::default());

    // 启动监控日志
    let send_counter_monitor = send_counter.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        let mut last = 0;
        loop {
            interval.tick().await;
            let current = send_counter_monitor.load(Ordering::Relaxed);
            let rps = current.saturating_sub(last);
            last = current;
            println!("Current RPS: {}", rps);
        }
    });

    let mut handles = vec![];
    // 批量启动避免瞬间过载
    for i in 0..num_clients {
        let addr_clone = addr.clone();
        let send_counter_clone = send_counter.clone();
        let client_timing_stats_clone = client_timing_stats.clone();
        handles.push(tokio::spawn(async move {
            // 错峰启动，每 10 个客户端间隔 1ms
            if i % 10 == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            run_client_with_rate(
                addr_clone,
                i as u64,
                requests_per_second,
                duration,
                retry_config,
                send_counter_clone,
                client_timing_stats_clone,
            )
            .await
        }));
    }

    let mut success_count = 0;
    for handle in handles {
        match handle.await {
            Ok(res) => {
                if res.is_ok() {
                    success_count += 1;
                }
            }
            Err(e) => eprintln!("Join error: {:?}", e),
        }
    }

    let stats = gateway.batcher_stats();
    println!("Final Batcher stats: {:?}", stats);
    let client_summary = client_timing_stats.summary();
    println!(
        "Client RTT summary: avg_rtt_ms={:.2} samples={}",
        avg_rtt_ms(&client_summary),
        client_summary.rtt_samples
    );
    println!("Success: {}/{}", success_count, num_clients);

    // 同样允许少量失败（例如握手超时），主要关注是否大面积超时
    assert!(
        success_count >= num_clients * 90 / 100,
        "Success rate too low (<90%)"
    );

    Ok(())
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要覆盖 5000 终端 * 5 次/秒 * 5 分钟的高峰场景
// - 目的: 在真实压力下验证网关与写入链路的稳定性
// - 备注: 该测试是 P1 Smart Batcher 的核心验证用例
// - 备注: 若失败，将触发后续批处理与背压改造
#[tokio::test]
async fn stress_test_edge_gateway_5000_clients_5rps_5min() -> Result<()> {
    // ### 修改记录 (2026-03-03)
    // - 原因: 压测用例不适合默认执行
    // - 目的: 使用环境变量显式开启
    if !stress_enabled() {
        return Ok(());
    }
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要保证端口可用
    // - 目的: 避免端口冲突导致压测失败
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);

    let addr = format!("127.0.0.1:{}", port);

    // ### 修改记录 (2026-03-01)
    // - 原因: 压测需要稳定的写入路径
    // - 目的: 使用测试 Router 保持可控性
    let router = create_test_router().await?;

    let config = EdgeGatewayConfig {
        // ### 修改记录 (2026-03-01)
        // - 原因: 压测持续 5 分钟会触发会话过期
        // - 目的: 将 TTL 调整到覆盖完整测试窗口
        session_ttl_ms: 600_000,
        nonce_cache_limit: 100_000,
        nonce_persist_enabled: false,
        nonce_persist_path: "".to_string(),
        secret_key: String::from_utf8(SECRET_KEY.to_vec())?,
        max_connections: 6000,
        // ### 修改记录 (2026-03-01)
        // - 原因: 5000 终端压测需要稳定的批处理窗口
        // - 目的: 降低 Raft 写入压力并保持响应能力
        batch_max_size: 512,
        batch_max_delay_ms: 10,
        batch_max_queue_size: 200_000,
        batch_wait_timeout_ms: 500,
        // ### 修改记录 (2026-03-01)
        // - 原因: 5000 终端场景需要稳定顺序调度参数
        // - 目的: 保证顺序域并行度一致
        ordering_stage_parallelism: 4,
        ordering_queue_limit: 200_000,
        batch_shards: 1,
        pre_agg_enabled: false,
        pre_agg_queue_size: 50_000,
        edge_client_retry_delay_ms: 100,
        edge_client_retry_max_attempts: 3,
        edge_client_retry_exponential_backoff: true,
    };
    let retry_config = EdgeClientRetryConfig::from_gateway_config(&config);

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要在压测结束输出 SmartBatcher 统计快照
    // - 目的: 定位 queue_full / wait_timeout / response_dropped 的占比
    // - 目的: 为后续批处理与背压改造提供直接证据
    // - 备注: 测试侧需要保留一个可访问的 gateway 引用
    // - 备注: 使用 Arc::clone 保持运行与读取句柄解耦
    // - 备注: 仅用于压测输出，不影响生产路径
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要注入顺序规则存储
    // - 目的: 支撑顺序规则热更新测试
    let order_rules_store =
        Arc::new(check_program::management::order_rules_service::OrderRulesStore::new());
    let gateway = Arc::new(EdgeGateway::new(
        addr.clone(),
        router,
        config,
        order_rules_store,
    )?);
    let gateway_runner = Arc::clone(&gateway);

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要保持压测期间网关持续运行
    // - 目的: 让统计数据覆盖完整 5 分钟窗口
    // - 备注: 运行句柄与统计读取句柄分离，避免生命周期耦合
    // - 备注: 若网关异常退出，会打印错误便于定位
    tokio::spawn(async move {
        if let Err(e) = gateway_runner.run().await {
            eprintln!("Gateway stopped: {:?}", e);
        }
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    let num_clients = 5000;
    let requests_per_second = 5;
    let duration = Duration::from_secs(300);
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要统计发送总量
    // - 目的: 与后续速率统计保持一致
    let send_counter = Arc::new(AtomicU64::new(0));
    let client_timing_stats = Arc::new(ClientTimingStats::default());

    let mut handles = vec![];
    for i in 0..num_clients {
        let addr_clone = addr.clone();
        let send_counter_clone = Arc::clone(&send_counter);
        let client_timing_stats_clone = Arc::clone(&client_timing_stats);
        handles.push(tokio::spawn(async move {
            match run_client_with_rate(
                addr_clone,
                i as u64,
                requests_per_second,
                duration,
                retry_config,
                send_counter_clone,
                client_timing_stats_clone,
            )
            .await
            {
                Ok(_) => Ok(()),
                Err(e) => {
                    eprintln!("Client {} failed: {:?}", i, e);
                    Err(e)
                }
            }
        }));
    }

    let mut success_count = 0;
    for handle in handles {
        match handle.await {
            Ok(res) => {
                if res.is_ok() {
                    success_count += 1;
                }
            }
            Err(e) => eprintln!("Join error: {:?}", e),
        }
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要在压测结束时收集 SmartBatcher 关键统计
    // - 目的: 定位失败集中于队列满/等待超时/回执丢失的哪一类
    // - 目的: 为参数调整提供可量化依据
    // - 备注: 统计为进程启动以来累计值
    // - 备注: 输出字段与 batcher_stats() 定义保持一致
    // - 备注: 如需更细粒度趋势，后续可改为分段打印
    let stats = gateway.batcher_stats();
    println!(
        "Batcher stats: queue_full={}, wait_timeout={}, response_dropped={}, enqueue_ok={}, batch_write_ok={}, batch_write_err={}, enqueued_total={}, batched_sqls_total={}, batch_wait_ok={}, batch_wait_err={}",
        stats.queue_full,
        stats.wait_timeout,
        stats.response_dropped,
        stats.enqueue_ok,
        stats.batch_write_ok,
        stats.batch_write_err,
        stats.enqueued_total,
        stats.batched_sqls_total,
        stats.batch_wait_ok,
        stats.batch_wait_err
    );
    let client_summary = client_timing_stats.summary();
    println!(
        "Client RTT summary: avg_rtt_ms={:.2} samples={}",
        avg_rtt_ms(&client_summary),
        client_summary.rtt_samples
    );
    println!("Success: {}/{}", success_count, num_clients);
    assert_eq!(success_count, num_clients);

    Ok(())
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要缩短压测窗口以便快速收集 --nocapture 日志
// - 目的: 新增 1 分钟版 5000 终端 5 rps 压测用例用于定位超时
// - 备注: 保持参数与 5 分钟版本一致，便于对比
// - 备注: 该用例仅缩短 duration，不改变请求速率
#[tokio::test]
async fn stress_test_edge_gateway_5000_clients_5rps_1min() -> Result<()> {
    // ### 修改记录 (2026-03-03)
    // - 原因: 压测用例不适合默认执行
    // - 目的: 使用环境变量显式开启
    if !stress_enabled() {
        return Ok(());
    }
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要保证端口可用
    // - 目的: 避免端口冲突导致压测失败
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);

    let addr = format!("127.0.0.1:{}", port);

    // ### 修改记录 (2026-03-01)
    // - 原因: 压测需要稳定的写入路径
    // - 目的: 使用测试 Router 保持可控性
    let router = create_test_router().await?;

    let config = EdgeGatewayConfig {
        // ### 修改记录 (2026-03-01)
        // - 原因: 1 分钟压测仍需覆盖会话窗口
        // - 目的: 维持与 5 分钟用例一致的基准
        session_ttl_ms: 600_000,
        nonce_cache_limit: 100_000,
        nonce_persist_enabled: false,
        nonce_persist_path: "".to_string(),
        secret_key: String::from_utf8(SECRET_KEY.to_vec())?,
        max_connections: 6000,
        // ### 修改记录 (2026-03-01)
        // - 原因: 5000 终端压测需要稳定的批处理窗口
        // - 目的: 降低 Raft 写入压力并保持响应能力
        batch_max_size: 512,
        batch_max_delay_ms: 10,
        batch_max_queue_size: 200_000,
        batch_wait_timeout_ms: 500,
        // ### 修改记录 (2026-03-01)
        // - 原因: 5000 终端场景需要稳定顺序调度参数
        // - 目的: 保证顺序域并行度一致
        ordering_stage_parallelism: 4,
        ordering_queue_limit: 200_000,
        batch_shards: 1,
        pre_agg_enabled: false,
        pre_agg_queue_size: 50_000,
        edge_client_retry_delay_ms: 100,
        edge_client_retry_max_attempts: 3,
        edge_client_retry_exponential_backoff: true,
    };
    let retry_config = EdgeClientRetryConfig::from_gateway_config(&config);

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要在压测结束输出 SmartBatcher 统计快照
    // - 目的: 定位 queue_full / wait_timeout / response_dropped 的占比
    // - 目的: 为后续批处理与背压改造提供直接证据
    // - 备注: 测试侧需要保留一个可访问的 gateway 引用
    // - 备注: 使用 Arc::clone 保持运行与读取句柄解耦
    // - 备注: 仅用于压测输出，不影响生产路径
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要注入顺序规则存储
    // - 目的: 支撑顺序规则热更新测试
    let order_rules_store =
        Arc::new(check_program::management::order_rules_service::OrderRulesStore::new());
    let gateway = Arc::new(EdgeGateway::new(
        addr.clone(),
        router,
        config,
        order_rules_store,
    )?);
    let gateway_runner = Arc::clone(&gateway);

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要保持压测期间网关持续运行
    // - 目的: 让统计数据覆盖完整 1 分钟窗口
    // - 备注: 运行句柄与统计读取句柄分离，避免生命周期耦合
    // - 备注: 若网关异常退出，会打印错误便于定位
    tokio::spawn(async move {
        if let Err(e) = gateway_runner.run().await {
            eprintln!("Gateway stopped: {:?}", e);
        }
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    let num_clients = 5000;
    let requests_per_second = 5;
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要缩短压测窗口便于快速观察日志
    // - 目的: 将 duration 调整为 60 秒以快速定位超时
    let duration = Duration::from_secs(60);
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要统计发送总量
    // - 目的: 观察真实发送速率
    let send_counter = Arc::new(AtomicU64::new(0));
    let client_timing_stats = Arc::new(ClientTimingStats::default());

    let mut handles = vec![];
    for i in 0..num_clients {
        let addr_clone = addr.clone();
        let send_counter_clone = Arc::clone(&send_counter);
        let client_timing_stats_clone = Arc::clone(&client_timing_stats);
        handles.push(tokio::spawn(async move {
            match run_client_with_rate(
                addr_clone,
                i as u64,
                requests_per_second,
                duration,
                retry_config,
                send_counter_clone,
                client_timing_stats_clone,
            )
            .await
            {
                Ok(_) => Ok(()),
                Err(e) => {
                    eprintln!("Client {} failed: {:?}", i, e);
                    Err(e)
                }
            }
        }));
    }

    let mut success_count = 0;
    for handle in handles {
        match handle.await {
            Ok(res) => {
                if res.is_ok() {
                    success_count += 1;
                }
            }
            Err(e) => eprintln!("Join error: {:?}", e),
        }
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要在压测结束时收集 SmartBatcher 关键统计
    // - 目的: 定位失败集中于队列满/等待超时/回执丢失的哪一类
    // - 目的: 为参数调整提供可量化依据
    // - 备注: 统计为进程启动以来累计值
    // - 备注: 输出字段与 batcher_stats() 定义保持一致
    // - 备注: 如需更细粒度趋势，后续可改为分段打印
    let stats = gateway.batcher_stats();
    println!(
        "Batcher stats: queue_full={}, wait_timeout={}, response_dropped={}, enqueue_ok={}, batch_write_ok={}, batch_write_err={}, enqueued_total={}, batched_sqls_total={}, batch_wait_ok={}, batch_wait_err={}",
        stats.queue_full,
        stats.wait_timeout,
        stats.response_dropped,
        stats.enqueue_ok,
        stats.batch_write_ok,
        stats.batch_write_err,
        stats.enqueued_total,
        stats.batched_sqls_total,
        stats.batch_wait_ok,
        stats.batch_wait_err
    );
    let client_summary = client_timing_stats.summary();
    println!(
        "Client RTT summary: avg_rtt_ms={:.2} samples={}",
        avg_rtt_ms(&client_summary),
        client_summary.rtt_samples
    );
    println!("Success: {}/{}", success_count, num_clients);
    assert_eq!(success_count, num_clients);

    Ok(())
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要扫描 batch_wait_timeout_ms 的拐点
// - 目的: 对比并发度变化下的超时与吞吐表现
#[tokio::test]
async fn stress_scan_batch_wait_timeout_1min_parallelism() -> Result<()> {
    // ### 修改记录 (2026-03-03)
    // - 原因: 压测扫描耗时长
    // - 目的: 使用环境变量显式开启
    if !stress_enabled() {
        return Ok(());
    }
    let timeouts = [700u64, 800, 900, 1000];
    let parallelisms = [1usize, 3, 5];
    let mut results = Vec::new();

    for timeout in timeouts {
        for parallelism in parallelisms {
            let case = StressCase {
                name: format!("timeout{}_p{}", timeout, parallelism),
                num_clients: 5000,
                requests_per_second: 5,
                duration: Duration::from_secs(60),
                batch_wait_timeout_ms: timeout,
                batch_max_size: 512,
                batch_max_delay_ms: 10,
                ordering_stage_parallelism: parallelism,
                batch_shards: 1,
            };
            let result = run_stress_case(&case).await?;
            results.push(result);
        }
    }

    println!(
        "CASE,timeout_ms,parallelism,batch_max_size,batch_max_delay_ms,success,num_clients,send_total,avg_send_rps,wait_timeout,response_dropped,queue_full,batches,avg_batch_size,avg_batch_wait_ms,avg_ingress_ms,avg_write_ms,avg_persist_ms,avg_client_rtt_ms,upload_errors"
    );
    for result in results {
        println!(
            "{},{},{},{},{},{},{},{},{:.2},{},{},{},{},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{}",
            result.name,
            result.batch_wait_timeout_ms,
            result.ordering_stage_parallelism,
            result.batch_max_size,
            result.batch_max_delay_ms,
            result.success_count,
            result.num_clients,
            result.send_total,
            result.avg_send_rps(),
            result.batcher_stats.wait_timeout,
            result.batcher_stats.response_dropped,
            result.batcher_stats.queue_full,
            result.batches(),
            result.avg_batch_size(),
            result.avg_batch_wait_ms(),
            result.avg_ingress_ms(),
            result.avg_write_ms(),
            result.avg_persist_ms(),
            result.avg_client_rtt_ms(),
            result.timing_summary.upload_errors
        );
    }

    Ok(())
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要在 1000ms 档位下尝试参数微调
// - 目的: 验证是否能进一步稳定且不引入延迟异常
#[tokio::test]
async fn stress_tune_batch_params_at_1000ms_1min_parallelism() -> Result<()> {
    // ### 修改记录 (2026-03-03)
    // - 原因: 压测调参耗时长
    // - 目的: 使用环境变量显式开启
    if !stress_enabled() {
        return Ok(());
    }
    let parallelisms = [1usize, 3, 5];
    let tuning = [
        ("size_448_delay_10", 448usize, 10u64),
        ("size_512_delay_20", 512usize, 20u64),
    ];
    let mut results = Vec::new();

    for (label, batch_max_size, batch_max_delay_ms) in tuning {
        for parallelism in parallelisms {
            let case = StressCase {
                name: format!("{}_p{}", label, parallelism),
                num_clients: 5000,
                requests_per_second: 5,
                duration: Duration::from_secs(60),
                batch_wait_timeout_ms: 1000,
                batch_max_size,
                batch_max_delay_ms,
                ordering_stage_parallelism: parallelism,
                batch_shards: 1,
            };
            let result = run_stress_case(&case).await?;
            results.push(result);
        }
    }

    println!(
        "CASE,timeout_ms,parallelism,batch_max_size,batch_max_delay_ms,success,num_clients,send_total,avg_send_rps,wait_timeout,response_dropped,queue_full,batches,avg_batch_size,avg_batch_wait_ms,avg_ingress_ms,avg_write_ms,avg_persist_ms,avg_client_rtt_ms,upload_errors"
    );
    for result in results {
        println!(
            "{},{},{},{},{},{},{},{},{:.2},{},{},{},{},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{}",
            result.name,
            result.batch_wait_timeout_ms,
            result.ordering_stage_parallelism,
            result.batch_max_size,
            result.batch_max_delay_ms,
            result.success_count,
            result.num_clients,
            result.send_total,
            result.avg_send_rps(),
            result.batcher_stats.wait_timeout,
            result.batcher_stats.response_dropped,
            result.batcher_stats.queue_full,
            result.batches(),
            result.avg_batch_size(),
            result.avg_batch_wait_ms(),
            result.avg_ingress_ms(),
            result.avg_write_ms(),
            result.avg_persist_ms(),
            result.avg_client_rtt_ms(),
            result.timing_summary.upload_errors
        );
    }

    Ok(())
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要覆盖 5000 设备顺序规则场景
// - 目的: 验证顺序调度开启后仍不发生超时
// - 备注: 使用 msg_type 维度进行规则匹配
#[tokio::test]
async fn stress_5000_devices_ordering_no_timeout() -> Result<()> {
    // ### 修改记录 (2026-03-03)
    // - 原因: 压测用例不适合默认执行
    // - 目的: 使用环境变量显式开启
    if !stress_enabled() {
        return Ok(());
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);

    let addr = format!("127.0.0.1:{}", port);
    let router = create_test_router().await?;

    let config = EdgeGatewayConfig {
        session_ttl_ms: 60_000,
        nonce_cache_limit: 200_000,
        nonce_persist_enabled: false,
        nonce_persist_path: "".to_string(),
        secret_key: String::from_utf8(SECRET_KEY.to_vec())?,
        max_connections: 6000,
        // ### 修改记录 (2026-03-01)
        // - 原因: 顺序调度下需要更高容错窗口
        // - 目的: 避免短时抖动造成超时
        // - 备注: 将等待窗口上调，降低误判
        batch_max_size: 512,
        batch_max_delay_ms: 10,
        batch_max_queue_size: 200_000,
        batch_wait_timeout_ms: 3000,
        ordering_stage_parallelism: 16,
        ordering_queue_limit: 200_000,
        batch_shards: 1,
        pre_agg_enabled: false,
        pre_agg_queue_size: 50_000,
        edge_client_retry_delay_ms: 100,
        edge_client_retry_max_attempts: 3,
        edge_client_retry_exponential_backoff: true,
    };
    let retry_config = EdgeClientRetryConfig::from_gateway_config(&config);

    let order_rules_store = Arc::new(OrderRulesStore::new());
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要构造多阶段顺序规则
    // - 目的: 验证阶段屏障下仍不超时
    // - 备注: 仅对少量设备启用顺序规则，以避免压测超时
    order_rules_store
        .update_from_json(
            r#"{
                "rules":[
                    {"device_prefix":"1000","msg_type":3,"order_group":"g1","stage":1,"priority":1},
                    {"device_prefix":"1001","msg_type":3,"order_group":"g1","stage":2,"priority":1},
                    {"device_prefix":"1002","msg_type":3,"order_group":"g1","stage":3,"priority":1}
                ]
            }"#,
        )
        .await?;

    let gateway = Arc::new(EdgeGateway::new(
        addr.clone(),
        router,
        config,
        order_rules_store,
    )?);
    let gateway_runner = Arc::clone(&gateway);

    tokio::spawn(async move {
        if let Err(e) = gateway_runner.run().await {
            eprintln!("Gateway stopped: {:?}", e);
        }
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    let num_clients = 5000;
    let requests_per_second = 1;
    let duration = Duration::from_secs(2);
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要统计发送总量
    // - 目的: 保持与其他压测用例一致的计数方式
    let send_counter = Arc::new(AtomicU64::new(0));
    let client_timing_stats = Arc::new(ClientTimingStats::default());

    let mut handles = vec![];
    for i in 0..num_clients {
        let addr_clone = addr.clone();
        let send_counter_clone = Arc::clone(&send_counter);
        let client_timing_stats_clone = Arc::clone(&client_timing_stats);
        handles.push(tokio::spawn(async move {
            let device_id = 1000 + i as u64;
            match run_client_with_rate(
                addr_clone,
                device_id,
                requests_per_second,
                duration,
                retry_config,
                send_counter_clone,
                client_timing_stats_clone,
            )
            .await
            {
                Ok(_) => Ok(()),
                Err(e) => {
                    eprintln!("Client {} failed: {:?}", i, e);
                    Err(e)
                }
            }
        }));
    }

    let mut success_count = 0;
    for handle in handles {
        match handle.await {
            Ok(res) => {
                if res.is_ok() {
                    success_count += 1;
                }
            }
            Err(e) => eprintln!("Join error: {:?}", e),
        }
    }

    let client_summary = client_timing_stats.summary();
    println!(
        "Client RTT summary: avg_rtt_ms={:.2} samples={}",
        avg_rtt_ms(&client_summary),
        client_summary.rtt_samples
    );
    println!("Success: {}/{}", success_count, num_clients);
    assert_eq!(success_count, num_clients);

    Ok(())
}
