#![allow(dead_code)]

use anyhow::{Result, anyhow};
use check_program::config::EdgeGatewayConfig;
use check_program::hub::edge_gateway::EdgeGateway;
use check_program::hub::edge_schema::{
    DataRecord, EdgeRequest, encode_auth_hello, encode_session_request,
};
use check_program::hub::protocol::{
    HEADER_SIZE, Header, MAGIC, MSG_TYPE_RESPONSE, VERSION, calculate_checksum, validate_header,
};
use check_program::management::order_rules_service::OrderRulesStore;
use check_program::raft::router::Router;
use flatbuffers::FlatBufferBuilder;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Duration, sleep};

// ### 修改记录 (2026-03-15)
// - 原因: 修复 ANSI 乱码备注
// - 目的: 确保测试注释可读
// ### 修改记录 (2026-03-15)
// - 原因: 文件编码为 ANSI 导致编译失败
// - 目的: 转换为 UTF-8 保证测试可编译
async fn reserve_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

// ### 修改记录 (2026-03-03)
// - 原因: new_for_test 现在使用真实 SQLite
// - 目的: 测试启动前确保 data 表存在
// - 备注: 避免 UploadData 写入失败
async fn ensure_data_table(router: &Arc<Router>) -> Result<()> {
    router
        .write(
            "CREATE TABLE IF NOT EXISTS data (key TEXT PRIMARY KEY, value BLOB, timestamp INTEGER)"
                .to_string(),
        )
        .await?;
    Ok(())
}

pub async fn spawn_gateway() -> Result<(String, tokio::task::JoinHandle<Result<()>>)> {
    let port = reserve_port().await?;
    let addr = format!("127.0.0.1:{}", port);
    let router = Arc::new(Router::new_for_test(true));
    // ### 修改记录 (2026-03-03)
    // - 原因: UploadData 依赖 data 表
    // - 目的: 统一测试网关启动前的表准备
    ensure_data_table(&router).await?;
    let config = EdgeGatewayConfig::default();
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要在测试环境注入规则存储
    // - 目的: 保证网关顺序规则模块可用
    let order_rules_store = Arc::new(OrderRulesStore::new());
    let gateway = Arc::new(EdgeGateway::new(
        addr.clone(),
        router,
        config,
        order_rules_store,
    )?);
    let handle = tokio::spawn(gateway.run());
    Ok((addr, handle))
}

// ### 修改记录 (2026-03-15)
// - 原因: 排空回归测试需要直接控制网关 draining 状态
// - 目的: 返回 Arc<EdgeGateway> 供测试调用 set_draining/inflight_requests
// - 备注: 保持与 spawn_gateway 相同启动路径，避免行为偏差
pub async fn spawn_gateway_with_handle(
) -> Result<(String, Arc<EdgeGateway>, tokio::task::JoinHandle<Result<()>>)> {
    let port = reserve_port().await?;
    let addr = format!("127.0.0.1:{}", port);
    let router = Arc::new(Router::new_for_test(true));
    // ### 修改记录 (2026-03-15)
    // - 原因: 复用现有测试启动前置条件
    // - 目的: 保证 data 表存在，避免 UploadData 失败
    ensure_data_table(&router).await?;
    let config = EdgeGatewayConfig::default();
    // ### 修改记录 (2026-03-15)
    // - 原因: 测试路径需要与正常网关一致的规则存储
    // - 目的: 避免顺序规则模块缺失导致的非预期失败
    let order_rules_store = Arc::new(OrderRulesStore::new());
    let gateway = Arc::new(EdgeGateway::new(
        addr.clone(),
        router,
        config,
        order_rules_store,
    )?);
    let handle = tokio::spawn(gateway.clone().run());
    Ok((addr, gateway, handle))
}

pub async fn spawn_gateway_with_config(
    config_path: &std::path::Path,
) -> Result<(String, tokio::task::JoinHandle<Result<()>>)> {
    let config = EdgeGatewayConfig::load_from_file(config_path)?;
    spawn_gateway_with_custom_config(config).await
}

pub async fn spawn_gateway_with_custom_config(
    config: EdgeGatewayConfig,
) -> Result<(String, tokio::task::JoinHandle<Result<()>>)> {
    let port = reserve_port().await?;
    let addr = format!("127.0.0.1:{}", port);
    let router = Arc::new(check_program::raft::router::Router::new_for_test(true));
    // ### 修改记录 (2026-03-03)
    // - 原因: UploadData 依赖 data 表
    // - 目的: 确保自定义配置测试可写
    ensure_data_table(&router).await?;
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要在自定义配置测试中注入规则存储
    // - 目的: 保证顺序调度测试可用
    let order_rules_store = Arc::new(OrderRulesStore::new());
    let gateway = Arc::new(EdgeGateway::new(
        addr.clone(),
        router,
        config,
        order_rules_store,
    )?);
    let handle = tokio::spawn(async move { gateway.run().await });
    Ok((addr, handle))
}

pub async fn connect_with_retry(addr: &str) -> Result<TcpStream> {
    for _ in 0..20 {
        if let Ok(stream) = TcpStream::connect(addr).await {
            return Ok(stream);
        }
        sleep(Duration::from_millis(50)).await;
    }
    Err(anyhow!("failed to connect to edge gateway"))
}

pub async fn send_frame(
    stream: &mut TcpStream,
    msg_type: u8,
    request_id: u64,
    payload: &[u8],
) -> Result<()> {
    let mut header = [0u8; HEADER_SIZE];
    header[0..4].copy_from_slice(&MAGIC);
    header[4] = VERSION;
    header[5] = msg_type;
    header[6..14].copy_from_slice(&request_id.to_be_bytes());
    header[14..18].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    let checksum = calculate_checksum(payload);
    header[18..21].copy_from_slice(&checksum);
    stream.write_all(&header).await?;
    stream.write_all(payload).await?;
    Ok(())
}

pub async fn send_raw_header(
    stream: &mut TcpStream,
    magic: [u8; 4],
    version: u8,
    msg_type: u8,
    request_id: u64,
    payload_len: u32,
) -> Result<()> {
    let mut header = [0u8; HEADER_SIZE];
    header[0..4].copy_from_slice(&magic);
    header[4] = version;
    header[5] = msg_type;
    header[6..14].copy_from_slice(&request_id.to_be_bytes());
    header[14..18].copy_from_slice(&payload_len.to_be_bytes());
    // For raw header test, we might send 0 checksum or invalid one, but here we just zero it out
    // as this function is used for negative tests where payload might not match or exist
    header[18..21].copy_from_slice(&[0u8; 3]);
    stream.write_all(&header).await?;
    Ok(())
}

pub async fn read_frame(stream: &mut TcpStream) -> Result<(Header, Vec<u8>)> {
    let mut header_buf = [0u8; HEADER_SIZE];
    stream.read_exact(&mut header_buf).await?;
    let header = validate_header(&header_buf)?;
    let mut payload = vec![0u8; header.payload_len as usize];
    stream.read_exact(&mut payload).await?;
    Ok((header, payload))
}

pub fn build_auth_hello(device_id: u64, timestamp_ms: u64, nonce: u64) -> Vec<u8> {
    build_auth_hello_with_key(device_id, timestamp_ms, nonce, b"device_secret")
}

pub fn build_auth_hello_with_key(
    device_id: u64,
    timestamp_ms: u64,
    nonce: u64,
    key: &[u8],
) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::new();
    let req = EdgeRequest::AuthHello {
        device_id,
        timestamp: timestamp_ms,
        nonce,
    };
    let root = encode_auth_hello(&mut builder, key, &req)
        .expect("build_auth_hello_with_key should encode valid AuthHello request");
    builder.finish(root, None);
    builder.finished_data().to_vec()
}

pub fn build_session_request(
    session_id: u64,
    timestamp_ms: u64,
    nonce: u64,
    session_key: &[u8],
) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::new();
    let records = vec![DataRecord {
        key: "k1".to_string(),
        value: b"v1".to_vec(),
        timestamp: 100,
    }];
    let req = EdgeRequest::UploadData { records };
    let root = encode_session_request(
        &mut builder,
        session_id,
        timestamp_ms,
        nonce,
        session_key,
        &req,
    )
    .expect("build_session_request should encode valid UploadData request");
    builder.finish(root, None);
    builder.finished_data().to_vec()
}

pub fn header_prefix_for_response(request_id: u64) -> [u8; 14] {
    let mut prefix = [0u8; 14];
    prefix[0..4].copy_from_slice(&MAGIC);
    prefix[4] = VERSION;
    prefix[5] = MSG_TYPE_RESPONSE;
    prefix[6..14].copy_from_slice(&request_id.to_be_bytes());
    prefix
}
