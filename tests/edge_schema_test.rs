use check_program::hub::edge_schema::{decode_request, encode_request, DataRecord, EdgeRequest};
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
