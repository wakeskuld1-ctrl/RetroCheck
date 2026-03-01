pub use crate::hub::edge_session_schema::{
    decode_auth_ack, decode_auth_hello, decode_signed_response, decode_session_request,
    encode_auth_ack, encode_auth_hello, encode_signed_response, encode_session_request, AuthAck,
    SessionMeta, SessionRequestMeta, SignedResponse,
};

use flatbuffers::{FlatBufferBuilder, ForwardsUOffset, Table, WIPOffset};

#[derive(Debug, PartialEq, Clone)]
pub enum EdgeRequest {
    Heartbeat { node_id: u64, timestamp: u64 },
    UploadData { records: Vec<DataRecord> },
    AuthHello { device_id: u64, timestamp: u64, nonce: u64 },
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
    let parsed = std::panic::catch_unwind(|| {
        let req_table = unsafe { flatbuffers::root_unchecked::<Table>(buf) };
        let req_type = unsafe { req_table.get::<u8>(4, Some(0)).unwrap_or(0) };
        let req_table = unsafe { req_table.get::<ForwardsUOffset<Table>>(6, None) }
            .ok_or_else(|| anyhow::anyhow!("Missing request table"))?;

        match req_type {
            1 => {
                let node_id = unsafe { req_table.get::<u64>(4, Some(0)).unwrap_or(0) };
                let timestamp = unsafe { req_table.get::<u64>(6, Some(0)).unwrap_or(0) };
                Ok(EdgeRequest::Heartbeat { node_id, timestamp })
            }
            2 => {
                let records_vec = unsafe {
                    req_table.get::<ForwardsUOffset<flatbuffers::Vector<ForwardsUOffset<Table>>>>(
                        4,
                        None,
                    )
                }
                .ok_or_else(|| anyhow::anyhow!("Missing records"))?;

                let mut records = Vec::new();
                for i in 0..records_vec.len() {
                    let r = records_vec.get(i);
                    let key = unsafe { r.get::<ForwardsUOffset<&str>>(4, None).unwrap_or("") };
                    let val_vec = unsafe {
                        r.get::<ForwardsUOffset<flatbuffers::Vector<u8>>>(6, None)
                    }
                    .ok_or_else(|| anyhow::anyhow!("Missing value"))?;
                    let timestamp = unsafe { r.get::<u64>(8, Some(0)).unwrap_or(0) };

                    records.push(DataRecord {
                        key: key.to_string(),
                        value: val_vec.bytes().to_vec(),
                        timestamp,
                    });
                }
                Ok(EdgeRequest::UploadData { records })
            }
            3 => {
                let device_id = unsafe { req_table.get::<u64>(4, Some(0)).unwrap_or(0) };
                let timestamp = unsafe { req_table.get::<u64>(6, Some(0)).unwrap_or(0) };
                let nonce = unsafe { req_table.get::<u64>(8, Some(0)).unwrap_or(0) };
                Ok(EdgeRequest::AuthHello {
                    device_id,
                    timestamp,
                    nonce,
                })
            }
            _ => Err(anyhow::anyhow!("Unknown request type: {}", req_type)),
        }
    });

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
