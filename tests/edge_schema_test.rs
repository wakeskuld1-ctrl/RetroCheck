use check_program::hub::edge_schema::{
    DataRecord, EdgeRequest, decode_request, encode_auth_hello, encode_request,
    encode_signed_response,
};
use flatbuffers::{FlatBufferBuilder, ForwardsUOffset, Table};

#[test]
fn test_heartbeat_encoding() {
    let mut builder = FlatBufferBuilder::new();
    let req = EdgeRequest::Heartbeat {
        node_id: 12345,
        timestamp: 99999,
    };

    let root = encode_request(&mut builder, &req);
    builder.finish(root, None);

    let buf = builder.finished_data();
    let decoded = decode_request(buf).unwrap();

    assert_eq!(decoded, req);
}

#[test]
fn test_upload_data_encoding() {
    let mut builder = FlatBufferBuilder::new();
    let records = vec![
        DataRecord {
            key: "k1".to_string(),
            value: b"v1".to_vec(),
            timestamp: 100,
        },
        DataRecord {
            key: "k2".to_string(),
            value: b"v2".to_vec(),
            timestamp: 200,
        },
    ];
    let req = EdgeRequest::UploadData { records };

    let root = encode_request(&mut builder, &req);
    builder.finish(root, None);

    let buf = builder.finished_data();
    let decoded = decode_request(buf).unwrap();

    assert_eq!(decoded, req);
}

#[test]
fn test_encode_auth_hello_rejects_non_authhello_request() {
    let mut builder = FlatBufferBuilder::new();
    let req = EdgeRequest::Heartbeat {
        node_id: 7,
        timestamp: 11,
    };
    let res = encode_auth_hello(&mut builder, b"secret", &req);
    assert!(res.is_err());
}

#[test]
fn test_encode_signed_response_rejects_invalid_header_prefix() {
    let mut builder = FlatBufferBuilder::new();
    let res = encode_signed_response(
        &mut builder,
        1,
        2,
        3,
        &[1, 2, 3],
        b"payload",
        b"session_key",
    );
    assert!(res.is_err());
}

// ### 修改记录 (2026-03-14)
// - 原因: 需要验证损坏的 records 偏移能被识别为非法 FlatBuffer
// - 目的: 为后续 Verifier 改造提供失败用例
#[test]
fn decode_request_rejects_corrupted_upload_vector_offset() {
    let mut builder = FlatBufferBuilder::new();
    // ### 修改记录 (2026-03-14)
    // - 原因: 需要构造最小 UploadData 请求
    // - 目的: 便于定位 records vector 的偏移
    let records = vec![DataRecord {
        key: "k1".to_string(),
        value: b"v1".to_vec(),
        timestamp: 100,
    }];
    let req = EdgeRequest::UploadData { records };

    // ### 修改记录 (2026-03-14)
    // - 原因: 需要得到完整 buffer 后再篡改偏移
    // - 目的: 确保篡改的是已序列化的数据
    let root = encode_request(&mut builder, &req);
    builder.finish(root, None);

    // ### 修改记录 (2026-03-14)
    // - 原因: 需要定位 records vector 在 buffer 中的位置
    // - 目的: 人为篡改第一个元素的偏移量以触发非法解析路径
    let mut buf = builder.finished_data().to_vec();
    let req_table = unsafe { flatbuffers::root_unchecked::<Table>(&buf) };
    let upload_table = unsafe {
        req_table
            .get::<ForwardsUOffset<Table>>(6, None)
            .expect("missing upload table")
    };
    let records_vec = unsafe {
        upload_table
            .get::<ForwardsUOffset<flatbuffers::Vector<ForwardsUOffset<Table>>>>(4, None)
            .expect("missing records")
    };
    let records_bytes = records_vec.bytes();
    let buf_ptr = buf.as_ptr() as usize;
    let bytes_ptr = records_bytes.as_ptr() as usize;
    let first_elem_offset = bytes_ptr - buf_ptr;
    // ### 修改记录 (2026-03-14)
    // - 原因: 需要把第一个记录的 uoffset 设为超界
    // - 目的: 触发当前解析路径的 panic 行为
    buf[first_elem_offset..first_elem_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());

    // ### 修改记录 (2026-03-14)
    // - 原因: 需要把当前错误信息固化为回归点
    // - 目的: Verifier 改造后应返回 Invalid FlatBuffer
    let err = decode_request(&buf).unwrap_err();
    assert!(err.to_string().contains("Invalid FlatBuffer"));
}
