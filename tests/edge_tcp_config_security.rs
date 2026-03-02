use anyhow::Result;
use check_program::config::EdgeGatewayConfig;
use check_program::hub::protocol::{MSG_TYPE_AUTH_HELLO, MSG_TYPE_ERROR};

mod edge_tcp_common;
use edge_tcp_common::{
    build_auth_hello_with_key, connect_with_retry, read_frame, send_frame,
    spawn_gateway_with_custom_config,
};

#[tokio::test]
async fn edge_tcp_handshake_fails_with_wrong_key() -> Result<()> {
    // ### 修改记录 (2026-03-01)
    // - 原因: clippy 提示 Default 后字段再赋值
    // - 目的: 使用结构体更新语法保持一致性
    let config = EdgeGatewayConfig {
        secret_key: "server_secret".to_string(),
        ..EdgeGatewayConfig::default()
    };

    let (addr, handle) = spawn_gateway_with_custom_config(config).await?;
    let mut stream = connect_with_retry(&addr).await?;

    // Client uses "wrong_secret"
    let auth_payload = build_auth_hello_with_key(1, 1000, 1, b"wrong_secret");
    send_frame(&mut stream, MSG_TYPE_AUTH_HELLO, 1, &auth_payload).await?;

    // Server should close connection (or return error frame)
    let result = read_frame(&mut stream).await;
    if let Ok((header, _)) = result {
        if header.msg_type == MSG_TYPE_ERROR {
            // This is also a valid failure scenario
            handle.abort();
            return Ok(());
        }
        return Err(anyhow::anyhow!(
            "Received unexpected successful response: {:?}",
            header
        ));
    }
    // If result is Err, it means connection closed, which is also fine.

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn edge_tcp_handshake_succeeds_with_correct_key() -> Result<()> {
    // ### 修改记录 (2026-03-01)
    // - 原因: clippy 提示 Default 后字段再赋值
    // - 目的: 使用结构体更新语法保持一致性
    let config = EdgeGatewayConfig {
        secret_key: "my_secure_key".to_string(),
        ..EdgeGatewayConfig::default()
    };

    let (addr, handle) = spawn_gateway_with_custom_config(config).await?;
    let mut stream = connect_with_retry(&addr).await?;

    // Client uses "my_secure_key"
    let auth_payload = build_auth_hello_with_key(2, 2000, 2, b"my_secure_key");
    send_frame(&mut stream, MSG_TYPE_AUTH_HELLO, 2, &auth_payload).await?;

    let result = read_frame(&mut stream).await;
    assert!(result.is_ok(), "Should succeed with correct key");

    handle.abort();
    Ok(())
}
