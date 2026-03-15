// ### 修改记录 (2026-03-15)
// - 原因: 需要验证 Raft gRPC 载荷的编解码
// - 目的: 确保 bincode 序列化可往返
use check_program::raft::grpc_codec::{decode_payload, encode_payload};
use check_program::raft::types::TypeConfig;
use openraft::raft::{AppendEntriesRequest, AppendEntriesResponse};
use openraft::Vote;

#[test]
fn encode_decode_append_entries_roundtrip() {
    // ### 修改记录 (2026-03-15)
    // - 原因: 需要构造最小 AppendEntries 请求
    // - 目的: 覆盖 vote 与空 entries 的编码路径
    let req = AppendEntriesRequest::<TypeConfig> {
        vote: Vote::new(1, 1),
        prev_log_id: None,
        entries: vec![],
        leader_commit: None,
    };
    let payload = encode_payload(&req).expect("encode req");
    let decoded: AppendEntriesRequest<TypeConfig> = decode_payload(&payload).expect("decode req");
    assert_eq!(decoded.vote, req.vote);

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要验证响应的编码路径
    // - 目的: 确保 Result<AppendEntriesResponse> 往返一致
    let resp: Result<AppendEntriesResponse<u64>, openraft::error::RaftError<u64>> =
        Ok(AppendEntriesResponse::Success);
    let payload = encode_payload(&resp).expect("encode resp");
    let decoded: Result<AppendEntriesResponse<u64>, openraft::error::RaftError<u64>> =
        decode_payload(&payload).expect("decode resp");
    assert!(decoded.is_ok());
}
