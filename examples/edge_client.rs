use anyhow::{Result, anyhow};
use check_program::hub::edge_schema::{DataRecord, EdgeRequest};
use check_program::hub::edge_session_schema::{
    decode_auth_ack, decode_signed_response, encode_auth_hello, encode_session_request,
};
use check_program::hub::protocol::{
    EdgeFrameCodec, Header, MAGIC, MSG_TYPE_AUTH_HELLO, MSG_TYPE_RESPONSE,
    MSG_TYPE_SESSION_REQUEST, VERSION,
};
use clap::Parser;
use flatbuffers::FlatBufferBuilder;
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Server address (host:port)
    #[arg(long, default_value = "127.0.0.1:8080")]
    addr: String,

    /// Device ID to simulate
    #[arg(long, default_value_t = 1001)]
    device_id: u64,

    /// Number of requests to send
    #[arg(long, default_value_t = 10)]
    count: usize,

    /// Shared secret key (must match server config)
    #[arg(long, default_value = "test_secret_key_123456")]
    secret: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    println!("Connecting to {} as device {}", args.addr, args.device_id);

    let stream = TcpStream::connect(&args.addr)
        .await
        .map_err(|e| anyhow!("Failed to connect to {}: {}", args.addr, e))?;

    let mut framed = Framed::new(stream, EdgeFrameCodec);
    let secret_bytes = args.secret.as_bytes();

    // 1. Handshake
    let nonce = 1000 + args.device_id;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;

    let mut builder = FlatBufferBuilder::new();
    let req = EdgeRequest::AuthHello {
        device_id: args.device_id,
        timestamp,
        nonce,
    };
    let offset = encode_auth_hello(&mut builder, secret_bytes, &req)?;
    builder.finish(offset, None);
    let payload = builder.finished_data().to_vec();

    let header = Header {
        version: VERSION,
        msg_type: MSG_TYPE_AUTH_HELLO,
        request_id: 1,
        payload_len: payload.len() as u32,
        checksum: [0; 3],
    };

    println!("Sending AuthHello...");
    framed.send((header, payload)).await?;

    // 2. Receive AuthAck
    let (resp_header, resp_payload) = framed
        .next()
        .await
        .ok_or(anyhow!("Connection closed during handshake"))??;

    if resp_header.msg_type != MSG_TYPE_RESPONSE {
        return Err(anyhow!(
            "Unexpected response type: {}",
            resp_header.msg_type
        ));
    }

    let ack = decode_auth_ack(&resp_payload)?;
    println!(
        "Auth success! Session ID: {}, TTL: {}ms",
        ack.session_id, ack.ttl_ms
    );
    let session_id = ack.session_id;
    let session_key = ack.session_key;

    // 3. Send Requests
    for i in 0..args.count {
        let mut builder = FlatBufferBuilder::new();
        let data_req = EdgeRequest::UploadData {
            records: vec![DataRecord {
                key: format!("dev_{}_req_{}", args.device_id, i),
                value: vec![1, 2, 3, 4], // Dummy data
                timestamp: timestamp + i as u64,
            }],
        };

        let req_nonce = nonce + i as u64 + 1;
        // Wrap in SessionRequest
        let offset = encode_session_request(
            &mut builder,
            session_id,
            timestamp + i as u64,
            req_nonce,
            &session_key,
            &data_req,
        )?;
        builder.finish(offset, None);
        let payload = builder.finished_data().to_vec();

        let req_id = 2 + i as u64;
        let header = Header {
            version: VERSION,
            msg_type: MSG_TYPE_SESSION_REQUEST,
            request_id: req_id,
            payload_len: payload.len() as u32,
            checksum: [0; 3],
        };

        framed.send((header, payload)).await?;

        // 4. Receive Response
        let (resp_header, resp_payload) = framed
            .next()
            .await
            .ok_or(anyhow!("Connection closed during request"))??;

        if resp_header.msg_type != MSG_TYPE_RESPONSE {
            return Err(anyhow!(
                "Unexpected response type: {}",
                resp_header.msg_type
            ));
        }

        // Verify signature
        let mut expected_prefix = [0u8; 14];
        expected_prefix[0..4].copy_from_slice(&MAGIC);
        expected_prefix[4] = VERSION;
        expected_prefix[5] = MSG_TYPE_RESPONSE;
        expected_prefix[6..14].copy_from_slice(&req_id.to_be_bytes());

        let lookup = |_| Some(session_key.clone());
        let signed_resp = decode_signed_response(&resp_payload, &expected_prefix, lookup)?;

        if signed_resp.session_id != session_id {
            return Err(anyhow!("Session ID mismatch in response"));
        }

        if (i + 1) % 100 == 0 {
            println!("Sent {} requests...", i + 1);
        }
    }

    println!("Completed {} requests successfully.", args.count);
    Ok(())
}
