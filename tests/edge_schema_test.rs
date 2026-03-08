use check_program::hub::edge_schema::{
    DataRecord, EdgeRequest, decode_request, encode_auth_hello, encode_request,
    encode_signed_response,
};
use flatbuffers::FlatBufferBuilder;

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
