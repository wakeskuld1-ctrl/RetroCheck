use anyhow::{Result, anyhow};
use check_program::hub::edge_schema::{decode_auth_ack, decode_signed_response};
use check_program::hub::protocol::{
    MSG_TYPE_AUTH_HELLO, MSG_TYPE_RESPONSE, MSG_TYPE_SESSION_REQUEST,
};
use serde_json::Value;

mod edge_tcp_common;
use edge_tcp_common::{
    build_auth_hello, build_session_request, connect_with_retry, header_prefix_for_response,
    read_frame, send_frame, spawn_gateway,
};

#[tokio::test]
async fn edge_tcp_allows_parallel_sessions() -> Result<()> {
    let (addr, handle) = spawn_gateway().await?;
    let mut tasks = Vec::new();

    for i in 0..8u64 {
        let addr = addr.clone();
        tasks.push(tokio::spawn(async move {
            let mut stream = connect_with_retry(&addr).await?;
            let auth_payload = build_auth_hello(100 + i, 1000 + i, 1 + i);
            send_frame(&mut stream, MSG_TYPE_AUTH_HELLO, 1000 + i, &auth_payload).await?;
            let (auth_header, auth_resp) = read_frame(&mut stream).await?;
            if auth_header.msg_type != MSG_TYPE_RESPONSE {
                return Err(anyhow!("unexpected auth response type"));
            }
            let ack = decode_auth_ack(&auth_resp)?;

            let session_payload =
                build_session_request(ack.session_id, 2000 + i, 2 + i, &ack.session_key);
            send_frame(
                &mut stream,
                MSG_TYPE_SESSION_REQUEST,
                2000 + i,
                &session_payload,
            )
            .await?;
            let (resp_header, resp_payload) = read_frame(&mut stream).await?;
            if resp_header.msg_type != MSG_TYPE_RESPONSE {
                return Err(anyhow!("unexpected session response type"));
            }
            let header_prefix = header_prefix_for_response(resp_header.request_id);
            let signed = decode_signed_response(&resp_payload, &header_prefix, |id| {
                if id == ack.session_id {
                    Some(ack.session_key.clone())
                } else {
                    None
                }
            })?;

            let response: Value = serde_json::from_slice(&signed.payload)?;
            if response["success"].as_u64().unwrap_or(0) != 1 {
                return Err(anyhow!("unexpected payload: {:?}", response));
            }
            Ok::<(), anyhow::Error>(())
        }));
    }

    for task in tasks {
        task.await??;
    }

    handle.abort();
    Ok(())
}
