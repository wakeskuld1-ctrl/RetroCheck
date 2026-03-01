#![allow(dead_code)]

use anyhow::{anyhow, Result};
use check_program::hub::edge_gateway::{
    validate_header, EdgeGateway, Header, HEADER_SIZE, MAGIC, MSG_TYPE_RESPONSE, VERSION,
};
use check_program::hub::edge_schema::{
    encode_auth_hello, encode_session_request, DataRecord, EdgeRequest,
};
use check_program::config::EdgeGatewayConfig;
use check_program::raft::router::Router;
use flatbuffers::FlatBufferBuilder;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, Duration};

async fn reserve_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

pub async fn spawn_gateway() -> Result<(String, tokio::task::JoinHandle<Result<()>>)> {
    let port = reserve_port().await?;
    let addr = format!("127.0.0.1:{}", port);
    let router = Arc::new(Router::new_for_test(true));
    let config = EdgeGatewayConfig::default();
    let gateway = Arc::new(EdgeGateway::new(addr.clone(), router, config)?);
    let handle = tokio::spawn(gateway.run());
    Ok((addr, handle))
}

pub async fn spawn_gateway_with_config(config_path: &std::path::Path) -> Result<(String, tokio::task::JoinHandle<Result<()>>)> {
    let port = reserve_port().await?;
    let addr = format!("127.0.0.1:{}", port);
    let router = Arc::new(Router::new_for_test(true));
    let config = EdgeGatewayConfig::load_from_file(config_path)?;
    let gateway = Arc::new(EdgeGateway::new(addr.clone(), router, config)?);
    let handle = tokio::spawn(gateway.run());
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
    header[18..21].copy_from_slice(&[0u8; 3]);
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
    let mut builder = FlatBufferBuilder::new();
    let req = EdgeRequest::AuthHello {
        device_id,
        timestamp: timestamp_ms,
        nonce,
    };
    let root = encode_auth_hello(&mut builder, b"device_secret", &req);
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
    let root = encode_session_request(&mut builder, session_id, timestamp_ms, nonce, session_key, &req);
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
