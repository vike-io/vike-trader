use vike_hub_proto::frame::read_frame;

use super::*;

#[test]
fn a_frame_under_the_ceiling_round_trips_through_the_shared_codec() {
    let mut buf = Vec::new();
    write_capped(&mut buf, &"hello", 1024).expect("write");
    let back: String = read_frame(&mut buf.as_slice()).expect("read");
    assert_eq!(back, "hello");
}

#[test]
fn a_frame_over_the_ceiling_is_refused_before_a_byte_is_written() {
    let mut buf = Vec::new();
    let big = "x".repeat(500);
    let err = write_capped(&mut buf, &big, 100).expect_err("over the ceiling");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(buf.is_empty(), "a refused frame wrote {} bytes", buf.len());
    assert!(err.to_string().contains("100-byte ceiling"), "{err}");
}

#[test]
fn the_prefix_is_the_big_endian_body_length() {
    let frame = encode_capped(&"pong", 1024).expect("encode");
    let len = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    assert_eq!(len, frame.len() - 4);
}

#[test]
fn a_client_frame_over_its_ceiling_is_refused_before_a_byte_is_written() {
    let mut buf = Vec::new();
    let err = write_up(&mut buf, &"x".repeat(500), 100).expect_err("over the ceiling");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(buf.is_empty(), "a refused frame wrote {} bytes", buf.len());
    write_up(&mut buf, &"ok", 100).expect("under the ceiling");
    let back: String = read_capped(&mut buf.as_slice(), 100).expect("read back");
    assert_eq!(back, "ok");
}
