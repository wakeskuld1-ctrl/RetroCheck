#[test]
fn raft_type_config_is_constructible() {
    let _req = check_program::raft::types::Request::Write { sql: "SELECT 1".to_string() };
    let _resp = check_program::raft::types::Response { value: Some("ok".to_string()) };
}

#[test]
fn verify_raft_type_config_trait() {
    // 验证 TypeConfig 实现了 RaftTypeConfig
    fn assert_impl<T: openraft::RaftTypeConfig>() {}
    assert_impl::<check_program::raft::types::TypeConfig>();
}
