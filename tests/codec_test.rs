use bytes::BytesMut;
use check_program::hub::protocol::{
    EdgeFrameCodec, Header, MSG_TYPE_AUTH_HELLO, VERSION, calculate_checksum,
};
use tokio_util::codec::{Decoder, Encoder};

#[test]
fn test_codec_encode_decode() {
    let mut codec = EdgeFrameCodec;
    let payload = vec![1, 2, 3, 4, 5];
    let checksum = calculate_checksum(&payload);

    let header = Header {
        version: VERSION,
        msg_type: MSG_TYPE_AUTH_HELLO,
        request_id: 123456789,
        payload_len: 5,
        checksum,
    };

    let mut buf = BytesMut::new();
    codec
        .encode((header.clone(), payload.clone()), &mut buf)
        .unwrap();

    let (decoded_header, decoded_payload) = codec.decode(&mut buf).unwrap().unwrap();

    assert_eq!(header, decoded_header);
    assert_eq!(payload, decoded_payload);
}
