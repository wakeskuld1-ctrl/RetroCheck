use anyhow::{Result, anyhow};
use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

pub const HEADER_SIZE: usize = 21;
pub const MAGIC: [u8; 4] = *b"EDGE";
pub const VERSION: u8 = 1;
pub const MSG_TYPE_AUTH_HELLO: u8 = 1;
pub const MSG_TYPE_RESPONSE: u8 = 2;
pub const MSG_TYPE_SESSION_REQUEST: u8 = 3;
pub const MSG_TYPE_ERROR: u8 = 0xFF;

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Header {
    pub version: u8,
    pub msg_type: u8,
    pub request_id: u64,
    pub payload_len: u32,
    pub checksum: [u8; 3],
}

pub fn validate_header(buf: &[u8]) -> Result<Header> {
    if buf.len() != HEADER_SIZE {
        return Err(anyhow!("invalid header length"));
    }
    if buf[0..4] != MAGIC {
        return Err(anyhow!("invalid magic"));
    }
    let version = buf[4];
    if version != VERSION {
        return Err(anyhow!("invalid version"));
    }
    let msg_type = buf[5];
    let request_id = u64::from_be_bytes([
        buf[6], buf[7], buf[8], buf[9], buf[10], buf[11], buf[12], buf[13],
    ]);
    let payload_len = u32::from_be_bytes([buf[14], buf[15], buf[16], buf[17]]);
    let checksum = [buf[18], buf[19], buf[20]];
    Ok(Header {
        version,
        msg_type,
        request_id,
        payload_len,
        checksum,
    })
}

pub fn calculate_checksum(payload: &[u8]) -> [u8; 3] {
    let crc = crc32fast::hash(payload);
    let bytes = crc.to_le_bytes();
    [bytes[0], bytes[1], bytes[2]]
}

pub struct EdgeFrameCodec;

impl Decoder for EdgeFrameCodec {
    type Item = (Header, Vec<u8>);
    type Error = anyhow::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>> {
        if src.len() < HEADER_SIZE {
            return Ok(None);
        }

        // Peek header without advancing
        let header_slice = &src[..HEADER_SIZE];
        // ### 修改记录 (2026-03-01)
        // - 原因: clippy 建议使用 ? 简化错误传播
        // - 目的: 保持错误语义一致且减少样板代码
        let header = validate_header(header_slice)?;

        let payload_len = header.payload_len as usize;
        // Limit max payload size to prevent DoS
        if payload_len > 10 * 1024 * 1024 {
            // 10MB limit
            return Err(anyhow!("Payload too large: {} bytes", payload_len));
        }

        let total_len = HEADER_SIZE + payload_len;

        if src.len() < total_len {
            // Reserve space for payload
            src.reserve(total_len - src.len());
            return Ok(None);
        }

        // We have a full frame
        src.advance(HEADER_SIZE);
        let payload = src.split_to(payload_len).to_vec();

        // Verify checksum
        let expected_checksum = calculate_checksum(&payload);
        if header.checksum != [0, 0, 0] && header.checksum != expected_checksum {
            return Err(anyhow!(
                "Checksum mismatch: expected {:?}, got {:?}",
                expected_checksum,
                header.checksum
            ));
        }

        Ok(Some((header, payload)))
    }
}

impl Encoder<(Header, Vec<u8>)> for EdgeFrameCodec {
    type Error = anyhow::Error;

    fn encode(&mut self, item: (Header, Vec<u8>), dst: &mut BytesMut) -> Result<()> {
        let (mut header, payload) = item;

        // Ensure payload length matches header
        if payload.len() as u32 != header.payload_len {
            return Err(anyhow!(
                "Payload length mismatch: header says {}, actual is {}",
                header.payload_len,
                payload.len()
            ));
        }

        // Calculate checksum
        header.checksum = calculate_checksum(&payload);

        // Encode header
        dst.reserve(HEADER_SIZE + payload.len());
        dst.extend_from_slice(&MAGIC);
        dst.put_u8(header.version);
        dst.put_u8(header.msg_type);
        dst.put_u64(header.request_id);
        dst.put_u32(header.payload_len);
        dst.extend_from_slice(&header.checksum);

        // Encode payload
        dst.extend_from_slice(&payload);

        Ok(())
    }
}
