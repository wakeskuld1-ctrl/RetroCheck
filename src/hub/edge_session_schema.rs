use anyhow::Context;
use flatbuffers::{FlatBufferBuilder, Table, WIPOffset};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::hub::edge_schema::{
    EdgeRequest, decode_request, encode_request,
};
use crate::hub::edge_fbs::edge as fbs;

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
) -> anyhow::Result<WIPOffset<Table<'b>>> {
    let (device_id, timestamp_ms, nonce) = match req {
        EdgeRequest::AuthHello {
            device_id,
            timestamp,
            nonce,
        } => (*device_id, *timestamp, *nonce),
        _ => {
            return Err(anyhow::anyhow!(
                "encode_auth_hello expects EdgeRequest::AuthHello"
            ));
        }
    };

    let signing = auth_hello_signing_input(device_id, timestamp_ms, nonce);
    let mac = hmac_truncate_16(secret, &signing)?;
    let mac_vec = builder.create_vector(&mac);
    let start = builder.start_table();
    builder.push_slot::<u64>(4, device_id, 0);
    builder.push_slot::<u64>(6, timestamp_ms, 0);
    builder.push_slot::<u64>(8, nonce, 0);
    builder.push_slot_always(10, mac_vec);
    let offset = builder.end_table(start);
    Ok(unsafe {
        std::mem::transmute::<
            flatbuffers::WIPOffset<flatbuffers::TableFinishedWIPOffset>,
            WIPOffset<Table<'b>>,
        >(offset)
    })
}

pub fn decode_auth_hello<F>(buf: &[u8], lookup_key: F) -> anyhow::Result<(SessionMeta, EdgeRequest)>
where
    F: Fn(u64) -> Option<Vec<u8>>,
{
    // ### 修改记录 (2026-03-14)
    // - 原因: 需要在解码前校验 AuthHello 的 FlatBuffers 结构
    // - 目的: 避免非法 payload 触发 panic
    let table = flatbuffers::root::<fbs::AuthHello>(buf)
        .map_err(|e| anyhow::anyhow!("Invalid AuthHello: {e}"))?;

    let device_id = table.device_id();
    let timestamp_ms = table.timestamp();
    let nonce = table.nonce();
    let mac_vec = table
        .mac()
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
    // ### 修改记录 (2026-03-14)
    // - 原因: 需要在解码前校验 AuthAck 的 FlatBuffers 结构
    // - 目的: 避免非法 payload 触发 panic
    let table = flatbuffers::root::<fbs::AuthAck>(buf)
        .map_err(|e| anyhow::anyhow!("Invalid AuthAck: {e}"))?;

    let session_id = table.session_id();
    let session_key_vec = table
        .session_key()
        .ok_or_else(|| anyhow::anyhow!("Missing session_key"))?;
    let ttl_ms = table.ttl_ms();

    Ok(AuthAck {
        session_id,
        session_key: session_key_vec.bytes().to_vec(),
        ttl_ms,
    })
}

pub fn encode_session_request<'b>(
    builder: &mut FlatBufferBuilder<'b>,
    session_id: u64,
    timestamp_ms: u64,
    nonce: u64,
    session_key: &[u8],
    req: &EdgeRequest,
) -> anyhow::Result<WIPOffset<Table<'b>>> {
    let mut payload_builder = FlatBufferBuilder::new();
    let payload_root = encode_request(&mut payload_builder, req);
    payload_builder.finish(payload_root, None);
    let payload_bytes = payload_builder.finished_data();

    let signing = session_signing_input(session_id, timestamp_ms, nonce, payload_bytes);
    let mac = hmac_truncate_16(session_key, &signing)?;

    let payload_vec = builder.create_vector(payload_bytes);
    let mac_vec = builder.create_vector(&mac);

    let start = builder.start_table();
    builder.push_slot::<u64>(4, session_id, 0);
    builder.push_slot::<u64>(6, timestamp_ms, 0);
    builder.push_slot::<u64>(8, nonce, 0);
    builder.push_slot_always(10, mac_vec);
    builder.push_slot_always(12, payload_vec);
    let offset = builder.end_table(start);
    Ok(unsafe {
        std::mem::transmute::<
            flatbuffers::WIPOffset<flatbuffers::TableFinishedWIPOffset>,
            WIPOffset<Table<'b>>,
        >(offset)
    })
}

pub fn decode_session_request<F>(
    buf: &[u8],
    lookup_key: F,
) -> anyhow::Result<(SessionRequestMeta, EdgeRequest)>
where
    F: Fn(u64) -> Option<Vec<u8>>,
{
    // ### 修改记录 (2026-03-14)
    // - 原因: 需要在解码前校验 SessionRequest 的 FlatBuffers 结构
    // - 目的: 避免非法 payload 触发 panic
    let table = flatbuffers::root::<fbs::SessionRequest>(buf)
        .map_err(|e| anyhow::anyhow!("Invalid SessionRequest: {e}"))?;

    let session_id = table.session_id();
    let timestamp_ms = table.timestamp_ms();
    let nonce = table.nonce();
    let mac_vec = table.mac().ok_or_else(|| anyhow::anyhow!("Missing mac"))?;
    let payload_vec = table
        .payload()
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
}

pub fn encode_signed_response<'b>(
    builder: &mut FlatBufferBuilder<'b>,
    session_id: u64,
    timestamp_ms: u64,
    nonce: u64,
    header_prefix: &[u8],
    payload: &[u8],
    session_key: &[u8],
) -> anyhow::Result<WIPOffset<Table<'b>>> {
    let signing =
        signed_response_signing_input(header_prefix, session_id, timestamp_ms, nonce, payload)
            ?;
    let mac = hmac_truncate_16(session_key, &signing)?;
    let payload_vec = builder.create_vector(payload);
    let mac_vec = builder.create_vector(&mac);
    let start = builder.start_table();
    builder.push_slot::<u64>(4, session_id, 0);
    builder.push_slot::<u64>(6, timestamp_ms, 0);
    builder.push_slot::<u64>(8, nonce, 0);
    builder.push_slot_always(10, mac_vec);
    builder.push_slot_always(12, payload_vec);
    let offset = builder.end_table(start);
    Ok(unsafe {
        std::mem::transmute::<
            flatbuffers::WIPOffset<flatbuffers::TableFinishedWIPOffset>,
            WIPOffset<Table<'b>>,
        >(offset)
    })
}

pub fn decode_signed_response<F>(
    buf: &[u8],
    header_prefix: &[u8],
    lookup_key: F,
) -> anyhow::Result<SignedResponse>
where
    F: Fn(u64) -> Option<Vec<u8>>,
{
    // ### 修改记录 (2026-03-14)
    // - 原因: 需要在解码前校验 SignedResponse 的 FlatBuffers 结构
    // - 目的: 避免非法 payload 触发 panic
    let table = flatbuffers::root::<fbs::SignedResponse>(buf)
        .map_err(|e| anyhow::anyhow!("Invalid SignedResponse: {e}"))?;

    let session_id = table.session_id();
    let timestamp_ms = table.timestamp_ms();
    let nonce = table.nonce();
    let mac_vec = table.mac().ok_or_else(|| anyhow::anyhow!("Missing mac"))?;
    let payload_vec = table
        .payload()
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
}
