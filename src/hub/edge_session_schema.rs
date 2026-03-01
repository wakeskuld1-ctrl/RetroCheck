use anyhow::Context;
use flatbuffers::{FlatBufferBuilder, ForwardsUOffset, Table, WIPOffset};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::hub::edge_schema::{decode_request, encode_request, EdgeRequest};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq)]
pub struct SessionMeta {
    pub device_id: u64,
    pub timestamp_ms: u64,
    pub nonce: u64,
    pub mac: [u8; 16],
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRequestMeta {
    pub session_id: u64,
    pub timestamp_ms: u64,
    pub nonce: u64,
    pub mac: [u8; 16],
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuthAck {
    pub session_id: u64,
    pub session_key: Vec<u8>,
    pub ttl_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SignedResponse {
    pub session_id: u64,
    pub timestamp_ms: u64,
    pub nonce: u64,
    pub payload: Vec<u8>,
    pub mac: [u8; 16],
}

fn push_u64_le(buf: &mut Vec<u8>, value: u64) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn hmac_truncate_16(secret: &[u8], data: &[u8]) -> anyhow::Result<[u8; 16]> {
    let mut mac = HmacSha256::new_from_slice(secret).context("Invalid HMAC key")?;
    mac.update(data);
    let result = mac.finalize().into_bytes();
    let mut output = [0u8; 16];
    output.copy_from_slice(&result[..16]);
    Ok(output)
}

fn auth_hello_signing_input(device_id: u64, timestamp_ms: u64, nonce: u64) -> Vec<u8> {
    let mut buf = Vec::with_capacity(24);
    push_u64_le(&mut buf, device_id);
    push_u64_le(&mut buf, timestamp_ms);
    push_u64_le(&mut buf, nonce);
    buf
}

fn session_signing_input(
    session_id: u64,
    timestamp_ms: u64,
    nonce: u64,
    payload: &[u8],
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(24 + payload.len());
    push_u64_le(&mut buf, session_id);
    push_u64_le(&mut buf, timestamp_ms);
    push_u64_le(&mut buf, nonce);
    buf.extend_from_slice(payload);
    buf
}

fn signed_response_signing_input(
    header_prefix: &[u8],
    session_id: u64,
    timestamp_ms: u64,
    nonce: u64,
    payload: &[u8],
) -> anyhow::Result<Vec<u8>> {
    if header_prefix.len() != 14 {
        return Err(anyhow::anyhow!("header_prefix must be 14 bytes"));
    }
    let mut buf = Vec::with_capacity(38 + payload.len());
    buf.extend_from_slice(header_prefix);
    push_u64_le(&mut buf, session_id);
    push_u64_le(&mut buf, timestamp_ms);
    push_u64_le(&mut buf, nonce);
    buf.extend_from_slice(payload);
    Ok(buf)
}

pub fn encode_auth_hello<'b>(
    builder: &mut FlatBufferBuilder<'b>,
    secret: &[u8],
    req: &EdgeRequest,
) -> WIPOffset<Table<'b>> {
    let (device_id, timestamp_ms, nonce) = match req {
        EdgeRequest::AuthHello {
            device_id,
            timestamp,
            nonce,
        } => (*device_id, *timestamp, *nonce),
        _ => panic!("encode_auth_hello expects EdgeRequest::AuthHello"),
    };

    let signing = auth_hello_signing_input(device_id, timestamp_ms, nonce);
    let mac = hmac_truncate_16(secret, &signing).expect("HMAC should accept any length key");
    let mac_vec = builder.create_vector(&mac);
    let start = builder.start_table();
    builder.push_slot::<u64>(4, device_id, 0);
    builder.push_slot::<u64>(6, timestamp_ms, 0);
    builder.push_slot::<u64>(8, nonce, 0);
    builder.push_slot_always(10, mac_vec);
    let offset = builder.end_table(start);
    unsafe { std::mem::transmute(offset) }
}

pub fn decode_auth_hello<F>(buf: &[u8], lookup_key: F) -> anyhow::Result<(SessionMeta, EdgeRequest)>
where
    F: Fn(u64) -> Option<Vec<u8>>,
{
    let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let table = unsafe { flatbuffers::root_unchecked::<Table>(buf) };
        let device_id = unsafe { table.get::<u64>(4, Some(0)).unwrap_or(0) };
        let timestamp_ms = unsafe { table.get::<u64>(6, Some(0)).unwrap_or(0) };
        let nonce = unsafe { table.get::<u64>(8, Some(0)).unwrap_or(0) };
        let mac_vec =
            unsafe { table.get::<ForwardsUOffset<flatbuffers::Vector<u8>>>(10, None) }
                .ok_or_else(|| anyhow::anyhow!("Missing mac"))?;

        let secret = lookup_key(device_id).ok_or_else(|| anyhow::anyhow!("Unknown device"))?;
        let signing = auth_hello_signing_input(device_id, timestamp_ms, nonce);
        let expected = hmac_truncate_16(&secret, &signing)?;
        let mac_bytes = mac_vec.bytes();

        if mac_bytes.len() != expected.len() || mac_bytes != expected {
            return Err(anyhow::anyhow!("AuthHello signature mismatch"));
        }

        let meta = SessionMeta {
            device_id,
            timestamp_ms,
            nonce,
            mac: expected,
        };
        let req = EdgeRequest::AuthHello {
            device_id,
            timestamp: timestamp_ms,
            nonce,
        };

        Ok((meta, req))
    }));

    match parsed {
        Ok(result) => result,
        Err(payload) => {
            let panic_msg = if let Some(msg) = payload.downcast_ref::<&str>() {
                (*msg).to_string()
            } else if let Some(msg) = payload.downcast_ref::<String>() {
                msg.clone()
            } else {
                "unknown panic".to_string()
            };
            Err(anyhow::anyhow!("FlatBuffers decode panic: {panic_msg}"))
        }
    }
}

pub fn encode_auth_ack<'b>(
    builder: &mut FlatBufferBuilder<'b>,
    ack: &AuthAck,
) -> WIPOffset<Table<'b>> {
    let session_key_vec = builder.create_vector(&ack.session_key);
    let start = builder.start_table();
    builder.push_slot::<u64>(4, ack.session_id, 0);
    builder.push_slot_always(6, session_key_vec);
    builder.push_slot::<u64>(8, ack.ttl_ms, 0);
    let offset = builder.end_table(start);
    unsafe { std::mem::transmute(offset) }
}

pub fn decode_auth_ack(buf: &[u8]) -> anyhow::Result<AuthAck> {
    let parsed = std::panic::catch_unwind(|| {
        let table = unsafe { flatbuffers::root_unchecked::<Table>(buf) };
        let session_id = unsafe { table.get::<u64>(4, Some(0)).unwrap_or(0) };
        let session_key_vec =
            unsafe { table.get::<ForwardsUOffset<flatbuffers::Vector<u8>>>(6, None) }
                .ok_or_else(|| anyhow::anyhow!("Missing session_key"))?;
        let ttl_ms = unsafe { table.get::<u64>(8, Some(0)).unwrap_or(0) };

        Ok(AuthAck {
            session_id,
            session_key: session_key_vec.bytes().to_vec(),
            ttl_ms,
        })
    });

    match parsed {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!("FlatBuffers decode panic")),
    }
}

pub fn encode_session_request<'b>(
    builder: &mut FlatBufferBuilder<'b>,
    session_id: u64,
    timestamp_ms: u64,
    nonce: u64,
    session_key: &[u8],
    req: &EdgeRequest,
) -> WIPOffset<Table<'b>> {
    let mut payload_builder = FlatBufferBuilder::new();
    let payload_root = encode_request(&mut payload_builder, req);
    payload_builder.finish(payload_root, None);
    let payload_bytes = payload_builder.finished_data();

    let signing = session_signing_input(session_id, timestamp_ms, nonce, payload_bytes);
    let mac = hmac_truncate_16(session_key, &signing).expect("HMAC should accept any length key");

    let payload_vec = builder.create_vector(payload_bytes);
    let mac_vec = builder.create_vector(&mac);

    let start = builder.start_table();
    builder.push_slot::<u64>(4, session_id, 0);
    builder.push_slot::<u64>(6, timestamp_ms, 0);
    builder.push_slot::<u64>(8, nonce, 0);
    builder.push_slot_always(10, mac_vec);
    builder.push_slot_always(12, payload_vec);
    let offset = builder.end_table(start);
    unsafe { std::mem::transmute(offset) }
}

pub fn decode_session_request<F>(
    buf: &[u8],
    lookup_key: F,
) -> anyhow::Result<(SessionRequestMeta, EdgeRequest)>
where
    F: Fn(u64) -> Option<Vec<u8>>,
{
    let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let table = unsafe { flatbuffers::root_unchecked::<Table>(buf) };
        let session_id = unsafe { table.get::<u64>(4, Some(0)).unwrap_or(0) };
        let timestamp_ms = unsafe { table.get::<u64>(6, Some(0)).unwrap_or(0) };
        let nonce = unsafe { table.get::<u64>(8, Some(0)).unwrap_or(0) };
        let mac_vec =
            unsafe { table.get::<ForwardsUOffset<flatbuffers::Vector<u8>>>(10, None) }
                .ok_or_else(|| anyhow::anyhow!("Missing mac"))?;
        let payload_vec =
            unsafe { table.get::<ForwardsUOffset<flatbuffers::Vector<u8>>>(12, None) }
                .ok_or_else(|| anyhow::anyhow!("Missing payload"))?;

        let secret = lookup_key(session_id).ok_or_else(|| anyhow::anyhow!("Unknown session"))?;
        let payload_bytes = payload_vec.bytes();
        let signing = session_signing_input(session_id, timestamp_ms, nonce, payload_bytes);
        let expected = hmac_truncate_16(&secret, &signing)?;
        let mac_bytes = mac_vec.bytes();

        if mac_bytes.len() != expected.len() || mac_bytes != expected {
            return Err(anyhow::anyhow!("Session signature mismatch"));
        }

        let req = decode_request(payload_bytes)?;
        let meta = SessionRequestMeta {
            session_id,
            timestamp_ms,
            nonce,
            mac: expected,
        };

        Ok((meta, req))
    }));

    match parsed {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!("FlatBuffers decode panic")),
    }
}

pub fn encode_signed_response<'b>(
    builder: &mut FlatBufferBuilder<'b>,
    session_id: u64,
    timestamp_ms: u64,
    nonce: u64,
    header_prefix: &[u8],
    payload: &[u8],
    session_key: &[u8],
) -> WIPOffset<Table<'b>> {
    let signing = signed_response_signing_input(
        header_prefix,
        session_id,
        timestamp_ms,
        nonce,
        payload,
    )
    .expect("header_prefix must be 14 bytes");
    let mac = hmac_truncate_16(session_key, &signing).expect("HMAC should accept any length key");
    let payload_vec = builder.create_vector(payload);
    let mac_vec = builder.create_vector(&mac);
    let start = builder.start_table();
    builder.push_slot::<u64>(4, session_id, 0);
    builder.push_slot::<u64>(6, timestamp_ms, 0);
    builder.push_slot::<u64>(8, nonce, 0);
    builder.push_slot_always(10, mac_vec);
    builder.push_slot_always(12, payload_vec);
    let offset = builder.end_table(start);
    unsafe { std::mem::transmute(offset) }
}

pub fn decode_signed_response<F>(
    buf: &[u8],
    header_prefix: &[u8],
    lookup_key: F,
) -> anyhow::Result<SignedResponse>
where
    F: Fn(u64) -> Option<Vec<u8>>,
{
    let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let table = unsafe { flatbuffers::root_unchecked::<Table>(buf) };
        let session_id = unsafe { table.get::<u64>(4, Some(0)).unwrap_or(0) };
        let timestamp_ms = unsafe { table.get::<u64>(6, Some(0)).unwrap_or(0) };
        let nonce = unsafe { table.get::<u64>(8, Some(0)).unwrap_or(0) };
        let mac_vec =
            unsafe { table.get::<ForwardsUOffset<flatbuffers::Vector<u8>>>(10, None) }
                .ok_or_else(|| anyhow::anyhow!("Missing mac"))?;
        let payload_vec =
            unsafe { table.get::<ForwardsUOffset<flatbuffers::Vector<u8>>>(12, None) }
                .ok_or_else(|| anyhow::anyhow!("Missing payload"))?;

        let secret = lookup_key(session_id).ok_or_else(|| anyhow::anyhow!("Unknown session"))?;
        let payload_bytes = payload_vec.bytes();
        let signing = signed_response_signing_input(
            header_prefix,
            session_id,
            timestamp_ms,
            nonce,
            payload_bytes,
        )?;
        let expected = hmac_truncate_16(&secret, &signing)?;
        let mac_bytes = mac_vec.bytes();

        if mac_bytes.len() != expected.len() || mac_bytes != expected {
            return Err(anyhow::anyhow!("SignedResponse signature mismatch"));
        }

        Ok(SignedResponse {
            session_id,
            timestamp_ms,
            nonce,
            payload: payload_bytes.to_vec(),
            mac: expected,
        })
    }));

    match parsed {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!("FlatBuffers decode panic")),
    }
}
