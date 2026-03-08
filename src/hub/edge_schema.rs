pub use crate::hub::edge_session_schema::{
    AuthAck, SessionMeta, SessionRequestMeta, SignedResponse, decode_auth_ack, decode_auth_hello,
    decode_session_request, decode_signed_response, encode_auth_ack, encode_auth_hello,
    encode_session_request, encode_signed_response,
};

use flatbuffers::{FlatBufferBuilder, ForwardsUOffset, Table, WIPOffset};

pub(crate) fn verify_buffer_structure(buf: &[u8]) -> anyhow::Result<()> {
    if buf.len() < 4 {
        return Err(anyhow::anyhow!("Buffer too small"));
    }
    // Read root offset (uoffset_t, u32)
    let root_offset = unsafe { flatbuffers::read_scalar::<u32>(&buf[0..4]) } as usize;

    if root_offset >= buf.len() {
        return Err(anyhow::anyhow!(
            "Root table offset {} out of bounds (len {})",
            root_offset,
            buf.len()
        ));
    }

    // A table starts with an sob (soffset_t, i32) pointing backwards to vtable.
    if root_offset + 4 > buf.len() {
        return Err(anyhow::anyhow!("Root table start out of bounds"));
    }

    let soffset = unsafe { flatbuffers::read_scalar::<i32>(&buf[root_offset..root_offset + 4]) };
    let vtable_offset = (root_offset as i32 - soffset) as usize;

    if vtable_offset >= buf.len() {
        return Err(anyhow::anyhow!(
            "Vtable offset {} out of bounds",
            vtable_offset
        ));
    }

    if vtable_offset + 4 > buf.len() {
        // Vtable needs at least 4 bytes (vbytes, fbytes)
        return Err(anyhow::anyhow!("Vtable header out of bounds"));
    }

    // vbytes: size of vtable (u16)
    let vbytes =
        unsafe { flatbuffers::read_scalar::<u16>(&buf[vtable_offset..vtable_offset + 2]) } as usize;

    if vtable_offset + vbytes > buf.len() {
        return Err(anyhow::anyhow!("Vtable size {} out of bounds", vbytes));
    }

    Ok(())
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
    verify_buffer_structure(buf).map_err(|e| anyhow::anyhow!("Invalid FlatBuffer: {}", e))?;

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
                        4, None,
                    )
                }
                .ok_or_else(|| anyhow::anyhow!("Missing records"))?;

                if records_vec.len() > 10000 {
                    return Err(anyhow::anyhow!("Too many records: {}", records_vec.len()));
                }

                let mut records = Vec::new();
                for i in 0..records_vec.len() {
                    let r = records_vec.get(i);
                    let key = unsafe { r.get::<ForwardsUOffset<&str>>(4, None).unwrap_or("") };
                    let val_vec =
                        unsafe { r.get::<ForwardsUOffset<flatbuffers::Vector<u8>>>(6, None) }
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
