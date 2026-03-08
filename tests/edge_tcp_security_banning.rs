use anyhow::Result;
use check_program::hub::protocol::{MSG_TYPE_AUTH_HELLO, MSG_TYPE_ERROR};

mod edge_tcp_common;
use edge_tcp_common::{
    build_auth_hello_with_key, connect_with_retry, read_frame, send_frame, spawn_gateway,
};

#[tokio::test]
async fn edge_tcp_bans_ip_after_failures() -> Result<()> {
    let (addr, handle) = spawn_gateway().await?;

    // Default blacklist is 5 failures
    for i in 0..5 {
        let mut stream = connect_with_retry(&addr).await?;

        // Send bad signature
        let auth_payload = build_auth_hello_with_key(1, 1000, i as u64, b"wrong_key");
        send_frame(&mut stream, MSG_TYPE_AUTH_HELLO, i as u64, &auth_payload).await?;

        // Server should close connection
        let result = read_frame(&mut stream).await;
        // ### 修改记录 (2026-03-01)
        // - 原因: clippy 提示可合并条件判断
        // - 目的: 保持异常连接处理逻辑不变
        if let Ok((header, _)) = result
            && header.msg_type != MSG_TYPE_ERROR
        {
            panic!(
                "Expected error frame or connection close, got msg_type {}",
                header.msg_type
            );
        }
    }

    // Now try to connect with valid key
    let mut stream = connect_with_retry(&addr).await?;
    let auth_payload = build_auth_hello_with_key(2, 2000, 100, b"device_secret");
    send_frame(&mut stream, MSG_TYPE_AUTH_HELLO, 100, &auth_payload).await?;

    // Server should close connection immediately (banned)
    // read_frame should return error (EOF)
    let result = read_frame(&mut stream).await;
    assert!(
        result.is_err(),
        "Should be banned and connection closed: {:?}",
        result
    );

    // Wait for ban to expire (not testing that here as it takes too long)

    handle.abort();
    Ok(())
}
