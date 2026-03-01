use anyhow::{anyhow, Result};
use check_program::hub::edge_gateway::{
    MSG_TYPE_AUTH_HELLO, MSG_TYPE_RESPONSE, MSG_TYPE_SESSION_REQUEST,
};
use check_program::hub::edge_schema::{decode_auth_ack, decode_signed_response};
use tokio::time::{timeout, Duration};

mod edge_tcp_common;
use edge_tcp_common::{
    build_auth_hello, build_session_request, connect_with_retry, header_prefix_for_response,
    read_frame, send_frame, spawn_gateway,
};

#[tokio::test]
async fn edge_tcp_handshake_and_upload_roundtrip() -> Result<()> {
    let (addr, handle) = spawn_gateway().await?;
    let mut stream = connect_with_retry(&addr).await?;

    let auth_payload = build_auth_hello(42, 1000, 1);
    send_frame(&mut stream, MSG_TYPE_AUTH_HELLO, 1, &auth_payload).await?;
    let (auth_header, auth_resp) = read_frame(&mut stream).await?;
    assert_eq!(auth_header.msg_type, MSG_TYPE_RESPONSE);
    let ack = decode_auth_ack(&auth_resp)?;
    assert!(ack.session_id > 0);
    assert!(!ack.session_key.is_empty());
    assert_eq!(ack.ttl_ms, 60_000);

    let session_payload = build_session_request(ack.session_id, 2000, 2, &ack.session_key);
    send_frame(&mut stream, MSG_TYPE_SESSION_REQUEST, 2, &session_payload).await?;
    let (resp_header, resp_payload) = read_frame(&mut stream).await?;
    assert_eq!(resp_header.msg_type, MSG_TYPE_RESPONSE);
    let header_prefix = header_prefix_for_response(resp_header.request_id);
    let signed = decode_signed_response(&resp_payload, &header_prefix, |id| {
        if id == ack.session_id {
            Some(ack.session_key.clone())
        } else {
            None
        }
    })?;
    assert_eq!(signed.session_id, ack.session_id);
    assert_eq!(signed.nonce, 2);
    assert_eq!(signed.payload, b"OK");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn edge_tcp_tampered_payload_is_rejected() -> Result<()> {
    let (addr, handle) = spawn_gateway().await?;
    let mut stream = connect_with_retry(&addr).await?;

    let auth_payload = build_auth_hello(7, 3000, 3);
    send_frame(&mut stream, MSG_TYPE_AUTH_HELLO, 10, &auth_payload).await?;
    let (_auth_header, auth_resp) = read_frame(&mut stream).await?;
    let ack = decode_auth_ack(&auth_resp)?;

    let mut session_payload = build_session_request(ack.session_id, 4000, 4, &ack.session_key);
    if let Some(last) = session_payload.last_mut() {
        *last = last.wrapping_add(1);
    }
    send_frame(&mut stream, MSG_TYPE_SESSION_REQUEST, 11, &session_payload).await?;

    let read_result = timeout(Duration::from_millis(200), read_frame(&mut stream)).await;
    match read_result {
        Ok(Ok(_)) => return Err(anyhow!("tampered payload unexpectedly accepted")),
        Ok(Err(_)) => {}
        Err(_) => {}
    }

    handle.abort();
    Ok(())
}
