use anyhow::{anyhow, Result};
use check_program::hub::edge_gateway::{
    MAGIC, MSG_TYPE_AUTH_HELLO, MSG_TYPE_SESSION_REQUEST, VERSION,
};
use check_program::hub::edge_schema::decode_auth_ack;
use tokio::time::{timeout, Duration};

mod edge_tcp_common;
use edge_tcp_common::{
    build_auth_hello, connect_with_retry, read_frame, send_frame, send_raw_header, spawn_gateway,
    spawn_gateway_with_config,
};

#[tokio::test]
async fn edge_tcp_auth_ack_uses_configured_ttl() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let config_path = dir.path().join("edge_gateway_config.json");
    std::fs::write(
        &config_path,
        r#"{"session_ttl_ms": 120000, "nonce_cache_limit": 1000, "nonce_persist_enabled": false, "nonce_persist_path": "nonce_cache"}"#,
    )?;
    let (addr, handle) = spawn_gateway_with_config(&config_path).await?;
    let mut stream = connect_with_retry(&addr).await?;
    let auth_payload = build_auth_hello(1, 1000, 1);
    send_frame(&mut stream, MSG_TYPE_AUTH_HELLO, 1, &auth_payload).await?;
    let (_header, resp) = read_frame(&mut stream).await?;
    let ack = decode_auth_ack(&resp)?;
    assert_eq!(ack.ttl_ms, 120000);
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn edge_tcp_rejects_replay_after_restart_when_nonce_persisted() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let config_path = dir.path().join("edge_gateway_config_persist.json");
    let persist_path = dir.path().join("nonce_cache");
    let persist_path_str = persist_path.to_str().unwrap().replace("\\", "/");
    
    std::fs::write(
        &config_path,
        format!(
            r#"{{"session_ttl_ms": 60000, "nonce_cache_limit": 10, "nonce_persist_enabled": true, "nonce_persist_path": "{}"}}"#,
            persist_path_str
        ),
    )?;
    
    let (addr, handle) = spawn_gateway_with_config(&config_path).await?;
    let mut stream = connect_with_retry(&addr).await?;
    let auth_payload = build_auth_hello(9, 1000, 7);
    send_frame(&mut stream, MSG_TYPE_AUTH_HELLO, 1, &auth_payload).await?;
    let (_header, _resp) = read_frame(&mut stream).await?;
    drop(stream); // Close connection to release EdgeGateway reference
    handle.abort();

    // Give some time for tasks to finish and sled to release lock
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Restart gateway with same config
    let (addr, handle) = spawn_gateway_with_config(&config_path).await?;
    let mut stream = connect_with_retry(&addr).await?;
    // Replay same nonce
    send_frame(&mut stream, MSG_TYPE_AUTH_HELLO, 2, &auth_payload).await?;
    
    // Expect failure (connection close or error response)
    // Actually, currently it might just close connection on error in handle_auth_hello if unwrap fails?
    // No, handle_auth_hello returns Result, handle_connection logs error.
    // If handle_auth_hello fails, handle_connection loop continues?
    // No, `handle_auth_hello` error propagates up?
    // In `handle_connection`: `handle_auth_hello(...)` result is `response_payload`.
    // If it returns Err, `handle_connection` returns Err, closing connection.
    
    let read_result = timeout(
        Duration::from_millis(200),
        read_frame(&mut stream),
    )
    .await;
    
    // Either timeout (no response) or error (connection closed) is success for rejection
    match read_result {
        Ok(Ok(_)) => return Err(anyhow!("Replay unexpectedly accepted")),
        _ => {} // Connection closed or timeout = Good
    }
    
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn edge_tcp_rejects_oversized_payload() -> Result<()> {
    let (addr, handle) = spawn_gateway().await?;
    let mut stream = connect_with_retry(&addr).await?;

    send_raw_header(
        &mut stream,
        MAGIC,
        VERSION,
        MSG_TYPE_SESSION_REQUEST,
        1,
        (1024 * 1024 + 1) as u32,
    )
    .await?;

    let read_result = timeout(Duration::from_millis(200), read_frame(&mut stream)).await;
    match read_result {
        Ok(Ok(_)) => return Err(anyhow!("oversized payload unexpectedly accepted")),
        Ok(Err(_)) => {}
        Err(_) => {}
    }

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn edge_tcp_drops_invalid_header() -> Result<()> {
    let (addr, handle) = spawn_gateway().await?;
    let mut stream = connect_with_retry(&addr).await?;

    send_raw_header(
        &mut stream,
        *b"BAD!",
        VERSION,
        MSG_TYPE_SESSION_REQUEST,
        2,
        10,
    )
    .await?;

    let read_result = timeout(Duration::from_millis(200), read_frame(&mut stream)).await;
    match read_result {
        Ok(Ok(_)) => return Err(anyhow!("invalid header unexpectedly accepted")),
        Ok(Err(_)) => {}
        Err(_) => {}
    }

    handle.abort();
    Ok(())
}
