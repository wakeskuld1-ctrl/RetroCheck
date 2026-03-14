pub use crate::hub::edge_session_schema::{
    AuthAck, SessionMeta, SessionRequestMeta, SignedResponse, decode_auth_ack, decode_auth_hello,
    decode_session_request, decode_signed_response, encode_auth_ack, encode_auth_hello,
    encode_session_request, encode_signed_response,
};

use flatbuffers::{FlatBufferBuilder, Table, WIPOffset};

use crate::hub::edge_fbs::edge as fbs;

// ### 修改记录 (2026-03-14)
// - 原因: 需要统一 FlatBuffers 校验入口
// - 目的: 使用 Verifier + root_as_* 替换手工结构检查
fn verify_edge_request(buf: &[u8]) -> anyhow::Result<fbs::EdgeRequest<'_>> {
    fbs::root_as_edge_request(buf).map_err(|e| anyhow::anyhow!("Invalid FlatBuffer: {e}"))
}

#[derive(Debug, PartialEq, Clone)]
pub enum EdgeRequest {
    Heartbeat {
        node_id: u64,
        timestamp: u64,
    },
    UploadData {
        records: Vec<DataRecord>,
    },
    AuthHello {
        device_id: u64,
        timestamp: u64,
        nonce: u64,
    },
}

#[derive(Debug, PartialEq, Clone)]
pub struct DataRecord {
    pub key: String,
    pub value: Vec<u8>,
    pub timestamp: u64,
}

pub fn encode_request<'b>(
    builder: &mut FlatBufferBuilder<'b>,
    req: &EdgeRequest,
) -> WIPOffset<Table<'b>> {
    match req {
        EdgeRequest::Heartbeat { node_id, timestamp } => {
            let start = builder.start_table();
            builder.push_slot::<u64>(4, *node_id, 0);
            builder.push_slot::<u64>(6, *timestamp, 0);
            let heartbeat = builder.end_table(start);

            let start = builder.start_table();
            builder.push_slot::<u8>(4, 1, 0);
            builder.push_slot_always(6, heartbeat);
            let offset = builder.end_table(start);
            unsafe {
                std::mem::transmute::<
                    flatbuffers::WIPOffset<flatbuffers::TableFinishedWIPOffset>,
                    WIPOffset<Table<'b>>,
                >(offset)
            }
        }
        EdgeRequest::UploadData { records } => {
            let mut record_offsets = Vec::new();
            for r in records {
                let key_off = builder.create_string(&r.key);
                let val_off = builder.create_vector(&r.value);

                let start = builder.start_table();
                builder.push_slot_always(4, key_off);
                builder.push_slot_always(6, val_off);
                builder.push_slot::<u64>(8, r.timestamp, 0);
                record_offsets.push(builder.end_table(start));
            }

            let records_vec = builder.create_vector(&record_offsets);

            let start = builder.start_table();
            builder.push_slot_always(4, records_vec);
            let upload = builder.end_table(start);

            let start = builder.start_table();
            builder.push_slot::<u8>(4, 2, 0);
            builder.push_slot_always(6, upload);
            let offset = builder.end_table(start);
            unsafe {
                std::mem::transmute::<
                    flatbuffers::WIPOffset<flatbuffers::TableFinishedWIPOffset>,
                    WIPOffset<Table<'b>>,
                >(offset)
            }
        }
        EdgeRequest::AuthHello {
            device_id,
            timestamp,
            nonce,
        } => {
            let start = builder.start_table();
            builder.push_slot::<u64>(4, *device_id, 0);
            builder.push_slot::<u64>(6, *timestamp, 0);
            builder.push_slot::<u64>(8, *nonce, 0);
            let hello = builder.end_table(start);

            let start = builder.start_table();
            builder.push_slot::<u8>(4, 3, 0);
            builder.push_slot_always(6, hello);
            let offset = builder.end_table(start);
            unsafe {
                std::mem::transmute::<
                    flatbuffers::WIPOffset<flatbuffers::TableFinishedWIPOffset>,
                    WIPOffset<Table<'b>>,
                >(offset)
            }
        }
    }
}

pub fn decode_request(buf: &[u8]) -> anyhow::Result<EdgeRequest> {
    // ### 修改记录 (2026-03-14)
    // - 原因: 需要在解码前校验 FlatBuffers 结构
    // - 目的: 避免非法 payload 触发 panic
    let root = verify_edge_request(buf)?;

    match root.req_type() {
        fbs::EdgeRequestPayload::Heartbeat => {
            let heartbeat = root
                .req_as_heartbeat()
                .ok_or_else(|| anyhow::anyhow!("Missing heartbeat"))?;
            Ok(EdgeRequest::Heartbeat {
                node_id: heartbeat.node_id(),
                timestamp: heartbeat.timestamp(),
            })
        }
        fbs::EdgeRequestPayload::UploadData => {
            let upload = root
                .req_as_upload_data()
                .ok_or_else(|| anyhow::anyhow!("Missing upload data"))?;
            let records_vec = upload
                .records()
                .ok_or_else(|| anyhow::anyhow!("Missing records"))?;

            if records_vec.len() > 10000 {
                return Err(anyhow::anyhow!("Too many records: {}", records_vec.len()));
            }

            let mut records = Vec::with_capacity(records_vec.len());
            for i in 0..records_vec.len() {
                let r = records_vec.get(i);
                let key = r.key().unwrap_or("").to_string();
                let value = r
                    .value()
                    .map(|v| v.bytes().to_vec())
                    .unwrap_or_default();
                let timestamp = r.timestamp();
                records.push(DataRecord {
                    key,
                    value,
                    timestamp,
                });
            }
            Ok(EdgeRequest::UploadData { records })
        }
        fbs::EdgeRequestPayload::AuthHello => {
            let hello = root
                .req_as_auth_hello()
                .ok_or_else(|| anyhow::anyhow!("Missing auth hello"))?;
            Ok(EdgeRequest::AuthHello {
                device_id: hello.device_id(),
                timestamp: hello.timestamp(),
                nonce: hello.nonce(),
            })
        }
        _ => Err(anyhow::anyhow!("Unknown request type: {:?}", root.req_type())),
    }
}
