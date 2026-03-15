use anyhow::{Result, anyhow};
use serde::{Serialize, de::DeserializeOwned};

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要统一 Raft gRPC 载荷序列化
/// - 目的: 让请求/响应通过 bytes 传输
pub fn encode_payload<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    bincode::serialize(value).map_err(|e| anyhow!(e))
}

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要统一 Raft gRPC 载荷反序列化
/// - 目的: 将 bytes 还原为 OpenRaft 结构
pub fn decode_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T> {
    bincode::deserialize(payload).map_err(|e| anyhow!(e))
}
