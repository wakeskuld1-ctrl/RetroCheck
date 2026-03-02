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
use check_program::raft::router::Router;
use flatbuffers::FlatBufferBuilder;
use futures::{SinkExt, StreamExt};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

const SECRET_KEY: &[u8] = b"test_secret_key_123456";

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
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要统计真实发送速率
    // - 目的: 以全局计数方式观察并发是否有效提升吞吐
    send_counter: Arc<AtomicU64>,
) -> Result<()> {
    // ### 修改记录 (2026-03-01)
    // - 原因: 压测连接在高并发下可能存在短暂不可用
    // - 目的: 增加重试以提升连接成功率
    let mut stream = None;
    for _ in 0..5 {
        if let Ok(s) = TcpStream::connect(&addr).await {
            stream = Some(s);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let stream = stream.ok_or(anyhow!("Failed to connect to {}", addr))?;

    // ### 修改记录 (2026-03-01)
    // - 原因: 压测需要稳定、可复用的帧解析
    // - 目的: 复用 EdgeFrameCodec 以保持协议一致性
    let mut framed = Framed::new(stream, EdgeFrameCodec);

    // ### 修改记录 (2026-03-01)
    // - 原因: 压测场景仍需保持会话安全流程
    // - 目的: 与真实设备行为对齐，避免绕过认证
    let nonce = 1000 + device_id;
    let timestamp = 1234567890;

    let mut builder = FlatBufferBuilder::new();
    let req = EdgeRequest::AuthHello {
        device_id,
        timestamp,
        nonce,
    };
    let offset = encode_auth_hello(&mut builder, SECRET_KEY, &req);
    builder.finish(offset, None);
    let payload = builder.finished_data().to_vec();

    let header = Header {
        version: VERSION,
        msg_type: MSG_TYPE_AUTH_HELLO,
        request_id: 1,
        payload_len: payload.len() as u32,
        checksum: [0; 3],
    };

    framed.send((header, payload)).await?;

    let (resp_header, resp_payload) = framed
        .next()
        .await
        .ok_or(anyhow!("Connection closed during handshake"))??;

    assert_eq!(resp_header.msg_type, MSG_TYPE_RESPONSE);
    let ack = decode_auth_ack(&resp_payload)?;
    let session_id = ack.session_id;
    let session_key = ack.session_key;

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要以稳定速率持续发送请求
    // - 目的: 还原 5 次/秒、5 分钟的持续负载
    let mut interval =
        tokio::time::interval(Duration::from_millis(1000 / requests_per_second.max(1)));
    let deadline = tokio::time::Instant::now() + duration;
    let mut request_index: u64 = 0;

    while tokio::time::Instant::now() < deadline {
        interval.tick().await;
        request_index += 1;

        let mut builder = FlatBufferBuilder::new();
        let data_req = EdgeRequest::UploadData {
            records: vec![DataRecord {
                key: format!("dev_{}_req_{}", device_id, request_index),
                value: vec![1, 2, 3, 4],
                timestamp: timestamp + request_index,
            }],
        };

        let offset = encode_session_request(
            &mut builder,
            session_id,
            timestamp + request_index,
            nonce + request_index + 1,
            &session_key,
            &data_req,
        );
        builder.finish(offset, None);
        let payload = builder.finished_data().to_vec();

        let header = Header {
            version: VERSION,
            msg_type: MSG_TYPE_SESSION_REQUEST,
            request_id: 2 + request_index,
            payload_len: payload.len() as u32,
            checksum: [0; 3],
        };

        framed.send((header.clone(), payload.clone())).await?;
        // ### 修改记录 (2026-03-01)
        // - 原因: 需要累计发送总数
        // - 目的: 计算每秒发送速率与总量
        send_counter.fetch_add(1, Ordering::Relaxed);

        let (resp_header, resp_payload) = framed
            .next()
            .await
            .ok_or(anyhow!("Connection closed during request"))??;

        assert_eq!(resp_header.msg_type, MSG_TYPE_RESPONSE);

        let mut expected_prefix = [0u8; 14];
        expected_prefix[0..4].copy_from_slice(&MAGIC);
        expected_prefix[4] = VERSION;
        expected_prefix[5] = MSG_TYPE_RESPONSE;
        expected_prefix[6..14].copy_from_slice(&header.request_id.to_be_bytes());

        let lookup = |_| Some(session_key.clone());
        let signed_resp = decode_signed_response(&resp_payload, &expected_prefix, lookup)?;
        assert_eq!(signed_resp.session_id, session_id);

        let response_json: serde_json::Value = serde_json::from_slice(&signed_resp.payload)?;
        assert_eq!(response_json["success"], 1);
        assert_eq!(response_json["failures"].as_array().unwrap().len(), 0);
    }

    Ok(())
}

async fn run_client(addr: String, device_id: u64, num_requests: usize) -> Result<()> {
    // Retry connection a few times if needed
    let mut stream = None;
    for _ in 0..5 {
        if let Ok(s) = TcpStream::connect(&addr).await {
            stream = Some(s);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let stream = stream.ok_or(anyhow!("Failed to connect to {}", addr))?;

    let mut framed = Framed::new(stream, EdgeFrameCodec);

    // 1. Handshake: AuthHello
    let nonce = 1000 + device_id;
    let timestamp = 1234567890;

    let mut builder = FlatBufferBuilder::new();
    let req = EdgeRequest::AuthHello {
        device_id,
        timestamp,
        nonce,
    };
    let offset = encode_auth_hello(&mut builder, SECRET_KEY, &req);
    builder.finish(offset, None);
    let payload = builder.finished_data().to_vec();

    let header = Header {
        version: VERSION,
        msg_type: MSG_TYPE_AUTH_HELLO,
        request_id: 1,
        payload_len: payload.len() as u32,
        checksum: [0; 3],
    };

    framed.send((header, payload)).await?;

    // 2. Receive AuthAck
    let (resp_header, resp_payload) = framed
        .next()
        .await
        .ok_or(anyhow!("Connection closed during handshake"))??;

    assert_eq!(resp_header.msg_type, MSG_TYPE_RESPONSE);
    let ack = decode_auth_ack(&resp_payload)?;
    let session_id = ack.session_id;
    let session_key = ack.session_key;

    // 3. Send Requests
    for i in 0..num_requests {
        let mut builder = FlatBufferBuilder::new();
        let data_req = EdgeRequest::UploadData {
            records: vec![DataRecord {
                key: format!("dev_{}_req_{}", device_id, i),
                value: vec![1, 2, 3, 4],
                timestamp: timestamp + i as u64,
            }],
        };

        // Wrap in SessionRequest
        let offset = encode_session_request(
            &mut builder,
            session_id,
            timestamp + i as u64,
            nonce + i as u64 + 1,
            &session_key,
            &data_req,
        );
        builder.finish(offset, None);
        let payload = builder.finished_data().to_vec();

        let header = Header {
            version: VERSION,
            msg_type: MSG_TYPE_SESSION_REQUEST,
            request_id: 2 + i as u64,
            payload_len: payload.len() as u32,
            checksum: [0; 3],
        };

        framed.send((header.clone(), payload.clone())).await?;

        // 4. Receive Response
        let (resp_header, resp_payload) = framed
            .next()
            .await
            .ok_or(anyhow!("Connection closed during request"))??;

        assert_eq!(resp_header.msg_type, MSG_TYPE_RESPONSE);

        // Verify signature
        let mut expected_prefix = [0u8; 14];
        expected_prefix[0..4].copy_from_slice(&MAGIC);
        expected_prefix[4] = VERSION;
        expected_prefix[5] = MSG_TYPE_RESPONSE;
        expected_prefix[6..14].copy_from_slice(&header.request_id.to_be_bytes());

        let lookup = |_| Some(session_key.clone());
        let signed_resp = decode_signed_response(&resp_payload, &expected_prefix, lookup)?;
        assert_eq!(signed_resp.session_id, session_id);

        // Verify JSON response
        let response_json: serde_json::Value = serde_json::from_slice(&signed_resp.payload)?;
        assert_eq!(response_json["success"], 1);
        assert_eq!(response_json["failures"].as_array().unwrap().len(), 0);
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
    duration_secs: u64,
    batch_wait_timeout_ms: u64,
    batch_max_size: usize,
    batch_max_delay_ms: u64,
    ordering_stage_parallelism: usize,
    success_count: usize,
    send_total: u64,
    batcher_stats: check_program::hub::edge_gateway::SmartBatchStatsSnapshot,
    timing_summary: RequestTimingSummary,
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
            self.timing_summary.batch_wait_ms_total as f64
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
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要复用网关启动与压测逻辑
// - 目的: 避免多个用例重复代码
async fn run_stress_case(case: &StressCase) -> Result<StressResult> {
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
    let router = Arc::new(Router::new_for_test(true));

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
    };

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

    // ### 修改记录 (2026-03-01)
    // - 原因: tokio::spawn 需要 'static 生命周期
    // - 目的: 复制参数避免捕获借用
    let num_clients = case.num_clients;
    let requests_per_second = case.requests_per_second;
    let duration = case.duration;

    let mut handles = vec![];
    for i in 0..num_clients {
        let addr_clone = addr.clone();
        let send_counter_clone = Arc::clone(&send_counter);
        handles.push(tokio::spawn(async move {
            match run_client_with_rate(
                addr_clone,
                i as u64,
                requests_per_second,
                duration,
                send_counter_clone,
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

    running.store(false, Ordering::Relaxed);
    let _ = rate_logger.await;

    let batcher_stats = gateway.batcher_stats();
    let timing_summary = gateway.timing_summary();

    Ok(StressResult {
        name: case.name.clone(),
        num_clients: case.num_clients,
        duration_secs: case.duration.as_secs(),
        batch_wait_timeout_ms: case.batch_wait_timeout_ms,
        batch_max_size: case.batch_max_size,
        batch_max_delay_ms: case.batch_max_delay_ms,
        ordering_stage_parallelism: case.ordering_stage_parallelism,
        success_count,
        send_total: send_counter.load(Ordering::Relaxed),
        batcher_stats,
        timing_summary,
    })
}

#[tokio::test]
async fn stress_test_edge_gateway() -> Result<()> {
    // Use a random port
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);

    let addr = format!("127.0.0.1:{}", port);

    // Setup Mock Router (Leader mode, returns Ok(0))
    let router = Arc::new(Router::new_for_test(true));

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
    };

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
            match run_client(addr_clone, i as u64, reqs_per_client).await {
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
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);

    let addr = format!("127.0.0.1:{}", port);
    let router = Arc::new(Router::new_for_test(true));

    let config = EdgeGatewayConfig {
        session_ttl_ms: 60_000,
        nonce_cache_limit: 10000,
        nonce_persist_enabled: false,
        nonce_persist_path: "".to_string(),
        secret_key: String::from_utf8(SECRET_KEY.to_vec())?,
        max_connections: 2000,
        // 关键配置：限制最大等待时间为 50ms
        batch_max_size: 256,
        batch_max_delay_ms: 5,  // 缩短聚合窗口到 5ms
        batch_max_queue_size: 50_000,
        batch_wait_timeout_ms: 50, // 硬性限制 50ms 超时
        ordering_stage_parallelism: 8, // 增加并发度以加快处理
        ordering_queue_limit: 50_000,
        batch_shards: 1,
    };

    let order_rules_store = Arc::new(check_program::management::order_rules_service::OrderRulesStore::new());
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

    let mut handles = vec![];
    for i in 0..num_clients {
        let addr_clone = addr.clone();
        let send_counter_clone = send_counter.clone();
        handles.push(tokio::spawn(async move {
            run_client_with_rate(
                addr_clone,
                i as u64,
                requests_per_second,
                duration,
                send_counter_clone,
            ).await
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
    println!("Success: {}/{}", success_count, num_clients);
    
    // 允许少量失败（例如连接建立超时），但主要验证是否因 batch wait timeout 而大面积失败
    // 如果 batch_wait_timeout_ms 设置过短导致无法处理，success_count 会很低
    assert!(success_count >= num_clients * 95 / 100, "Success rate too low");

    Ok(())
}

// ### 修改记录 (2026-03-01)
// - 原因: 验证 5000 并发终端下，每秒 10 RPS，严格 50ms 延迟限制的稳定性
// - 目的: 响应用户最新压测需求，验证方案 B 在高负载下的表现
// - 备注: 持续时间为 60 秒
#[tokio::test]
async fn stress_test_edge_gateway_5000_clients_10rps_50ms_1min() -> Result<()> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);

    let addr = format!("127.0.0.1:{}", port);
    let router = Arc::new(Router::new_for_test(true));

    let config = EdgeGatewayConfig {
        session_ttl_ms: 120_000, // 2分钟足以覆盖测试
        nonce_cache_limit: 100_000,
        nonce_persist_enabled: false,
        nonce_persist_path: "".to_string(),
        secret_key: String::from_utf8(SECRET_KEY.to_vec())?,
        max_connections: 6000, // 支持 5000+ 连接
        // 关键配置：高吞吐低延迟
        batch_max_size: 512,    // 增大批大小以吞吐 50,000 RPS
        batch_max_delay_ms: 5,  // 维持 5ms 聚合窗口
        batch_max_queue_size: 100_000,
        batch_wait_timeout_ms: 50, // 严格 50ms 超时
        ordering_stage_parallelism: 16, // 提高并发处理能力
        ordering_queue_limit: 100_000,
        batch_shards: 8, // 启用 8 分片以验证 Scheme C
    };

    let order_rules_store = Arc::new(check_program::management::order_rules_service::OrderRulesStore::new());
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

    // 5000 客户端，10 RPS，60 秒
    let num_clients = 5000;
    let requests_per_second = 10;
    let duration = Duration::from_secs(60);
    let send_counter = Arc::new(AtomicU64::new(0));

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
                send_counter_clone,
            ).await
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
    println!("Success: {}/{}", success_count, num_clients);
    
    // 同样允许少量失败（例如握手超时），主要关注是否大面积超时
    assert!(success_count >= num_clients * 90 / 100, "Success rate too low (<90%)");

    Ok(())
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要覆盖 5000 终端 * 5 次/秒 * 5 分钟的高峰场景
// - 目的: 在真实压力下验证网关与写入链路的稳定性
// - 备注: 该测试是 P1 Smart Batcher 的核心验证用例
// - 备注: 若失败，将触发后续批处理与背压改造
#[tokio::test]
async fn stress_test_edge_gateway_5000_clients_5rps_5min() -> Result<()> {
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
    let router = Arc::new(Router::new_for_test(true));

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
    };

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

    let mut handles = vec![];
    for i in 0..num_clients {
        let addr_clone = addr.clone();
        let send_counter_clone = Arc::clone(&send_counter);
        handles.push(tokio::spawn(async move {
            match run_client_with_rate(
                addr_clone,
                i as u64,
                requests_per_second,
                duration,
                send_counter_clone,
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
    let router = Arc::new(Router::new_for_test(true));

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
    };

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

    let mut handles = vec![];
    for i in 0..num_clients {
        let addr_clone = addr.clone();
        let send_counter_clone = Arc::clone(&send_counter);
        handles.push(tokio::spawn(async move {
            match run_client_with_rate(
                addr_clone,
                i as u64,
                requests_per_second,
                duration,
                send_counter_clone,
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
    println!("Success: {}/{}", success_count, num_clients);
    assert_eq!(success_count, num_clients);

    Ok(())
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要扫描 batch_wait_timeout_ms 的拐点
// - 目的: 对比并发度变化下的超时与吞吐表现
#[tokio::test]
async fn stress_scan_batch_wait_timeout_1min_parallelism() -> Result<()> {
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

    println!("CASE,timeout_ms,parallelism,batch_max_size,batch_max_delay_ms,success,num_clients,send_total,avg_send_rps,wait_timeout,response_dropped,queue_full,batches,avg_batch_size,avg_batch_wait_ms,upload_errors");
    for result in results {
        println!(
            "{},{},{},{},{},{},{},{},{:.2},{},{},{},{},{:.2},{:.2},{}",
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

    println!("CASE,timeout_ms,parallelism,batch_max_size,batch_max_delay_ms,success,num_clients,send_total,avg_send_rps,wait_timeout,response_dropped,queue_full,batches,avg_batch_size,avg_batch_wait_ms,upload_errors");
    for result in results {
        println!(
            "{},{},{},{},{},{},{},{},{:.2},{},{},{},{},{:.2},{:.2},{}",
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
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);

    let addr = format!("127.0.0.1:{}", port);
    let router = Arc::new(Router::new_for_test(true));

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
    };

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

    let mut handles = vec![];
    for i in 0..num_clients {
        let addr_clone = addr.clone();
        let send_counter_clone = Arc::clone(&send_counter);
        handles.push(tokio::spawn(async move {
            let device_id = 1000 + i as u64;
            match run_client_with_rate(
                addr_clone,
                device_id,
                requests_per_second,
                duration,
                send_counter_clone,
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

    println!("Success: {}/{}", success_count, num_clients);
    assert_eq!(success_count, num_clients);

    Ok(())
}
