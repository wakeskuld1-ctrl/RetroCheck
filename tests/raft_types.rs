//! ### 修改记录 (2026-02-17)
//! - 原因: 需要验证请求/响应的序列化稳定性
//! - 目的: 保障 Raft 日志在网络与磁盘传输中的一致性

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要验证 Request/Response 的序列化可用性
/// - 目的: 在引入 Raft 之前锁定最小契约
#[test]
fn request_response_roundtrip() {
    // ### 修改记录 (2026-02-17)
    // - 原因: 写入请求是最关键日志内容
    // - 目的: 保障 Write 请求能被稳定序列化/反序列化
    let req = check_program::raft::types::Request::Write {
        sql: "CREATE TABLE t(x INT)".to_string(),
    };
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要验证二进制序列化
    // - 目的: 确保 bincode 对类型可用
    let bytes = bincode::serialize(&req).unwrap();
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要反序列化验证一致性
    // - 目的: 保证传输后语义不变
    let decoded: check_program::raft::types::Request = bincode::deserialize(&bytes).unwrap();
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要比较结构一致性
    // - 目的: 防止字段丢失或变更
    assert_eq!(format!("{:?}", req), format!("{:?}", decoded));

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要验证 Response 的序列化
    // - 目的: 确保返回值格式稳定
    let resp = check_program::raft::types::Response {
        value: Some("ok".to_string()),
    };
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要验证 Response 的序列化流程
    // - 目的: 覆盖 Response 的序列化路径
    let bytes = bincode::serialize(&resp).unwrap();
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要验证 Response 的反序列化流程
    // - 目的: 覆盖 Response 的反序列化路径
    let decoded: check_program::raft::types::Response = bincode::deserialize(&bytes).unwrap();
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要确认输出一致性
    // - 目的: 保证对外接口稳定
    assert_eq!(format!("{:?}", resp), format!("{:?}", decoded));
}
