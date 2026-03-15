// ### 修改记录 (2026-03-15)
// - 原因: 验证 RaftService proto 是否已生成
// - 目的: 在实现 gRPC 之前确保 pb 类型可用
#[test]
fn raft_service_proto_is_generated() {
    // ### 修改记录 (2026-03-15)
    // - 原因: 只需确认类型存在即可
    // - 目的: 让测试在未生成时直接失败
    let _ = std::any::type_name::<check_program::pb::raft_service_client::RaftServiceClient<tonic::transport::Channel>>();
    let _ = std::any::type_name::<check_program::pb::raft_service_server::RaftServiceServer<()>>();
}
