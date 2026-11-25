use std::net::TcpListener;

use vike_hub_protocol::frame::read_frame;

use super::*;

/// **The one wait both daemons' servers use** (`wait_readable`, the gatewayhub's and the datahub's copies
/// until it lived here): nothing within the wait is a `Timeout`, a frame that began is `Data` and is NOT
/// consumed (the whole frame reads back after it), and a closed peer is `Eof`; a zero wait still waits.
#[test]
fn waiting_for_a_frame_reports_timeout_data_and_eof_and_consumes_nothing() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let mut client = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
    let (mut server, _) = listener.accept().expect("accept");

    // A zero wait is raised to 1 ms: a zero read timeout would be an `Err` from the OS.
    assert_eq!(wait_readable(&server, Duration::ZERO).expect("wait"), StreamFrameReady::Timeout);
    assert_eq!(
        wait_readable(&server, Duration::from_millis(20)).expect("wait"),
        StreamFrameReady::Timeout
    );

    write_capped(&mut client, &"hello", 1024).expect("write");
    assert_eq!(
        wait_readable(&server, Duration::from_secs(5)).expect("wait"),
        StreamFrameReady::Data
    );
    assert_eq!(
        wait_readable(&server, Duration::from_secs(5)).expect("wait"),
        StreamFrameReady::Data,
        "peeked"
    );
    assert_eq!(server.read_timeout().expect("timeout"), Some(Duration::from_secs(5)));
    let back: String = read_capped(&mut server, 1024).expect("the whole frame is still there");
    assert_eq!(back, "hello");

    drop(client);
    assert_eq!(
        wait_readable(&server, Duration::from_secs(5)).expect("wait"),
        StreamFrameReady::Eof
    );
}

#[test]
fn a_frame_under_the_ceiling_round_trips_through_the_shared_codec() {
    let mut buffer = Vec::new();
    write_capped(&mut buffer, &"hello", 1024).expect("write");
    let back: String = read_frame(&mut buffer.as_slice()).expect("read");
    assert_eq!(back, "hello");
}

#[test]
fn a_frame_over_the_ceiling_is_refused_before_a_byte_is_written() {
    let mut buffer = Vec::new();
    let big = "x".repeat(500);
    let err = write_capped(&mut buffer, &big, 100).expect_err("over the ceiling");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(buffer.is_empty(), "a refused frame wrote {} bytes", buffer.len());
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
    let mut buffer = Vec::new();
    let err = write_up(&mut buffer, &"x".repeat(500), 100).expect_err("over the ceiling");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(buffer.is_empty(), "a refused frame wrote {} bytes", buffer.len());
    write_up(&mut buffer, &"ok", 100).expect("under the ceiling");
    let back: String = read_capped(&mut buffer.as_slice(), 100).expect("read back");
    assert_eq!(back, "ok");
}

#[test]
fn encoding_into_a_buffer_appends_the_same_bytes_and_a_refusal_adds_none() {
    let mut buffer = Vec::new();
    encode_capped_into(&mut buffer, &"one", 1024).expect("first");
    encode_capped_into(&mut buffer, &"two", 1024).expect("second");
    let mut alone = encode_capped(&"one", 1024).expect("one");
    alone.extend(encode_capped(&"two", 1024).expect("two"));
    assert_eq!(buffer, alone);
    let before = buffer.clone();
    let err = encode_capped_into(&mut buffer, &"x".repeat(500), 100).expect_err("over the ceiling");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert_eq!(buffer, before, "a refused frame left bytes behind");
}
