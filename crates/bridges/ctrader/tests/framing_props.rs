//! Property harness for the cTrader wire framing (`crates/bridges/ctrader/src/framing.rs`'s
//! `encode` / `FrameReader`): round-trip identity over arbitrary frames, totality over arbitrary
//! byte streams, and the length-prefix DoS edge (a claimed 4 GiB frame must not be allocated up
//! front). Payload-level protobuf decoding is the fuzz tier's job (fuzz/ ctrader_framing target);
//! this file pins the frame layer every inbound cTrader byte crosses first.

use std::io::Cursor;

use proptest::prelude::*;
use vike_ctrader::framing::{FrameReader, encode};

proptest! {
    /// Round-trip: `encode` then `FrameReader::next_frame` yields the same ProtoMessage
    /// envelope, with nothing left over in the stream.
    #[test]
    fn encode_then_read_round_trips(
        payload_type in any::<u32>(),
        body in prop::collection::vec(any::<u8>(), 0..4096),
        client_msg_id in "[ -~]{0,64}",
    ) {
        let bytes = encode(payload_type, &body, &client_msg_id);
        let mut reader = FrameReader::new(Cursor::new(bytes));
        let msg = reader.next_frame().expect("io on a cursor").expect("one full frame");
        prop_assert_eq!(msg.payload_type, payload_type);
        prop_assert_eq!(msg.payload.as_deref(), Some(body.as_slice()));
        prop_assert_eq!(msg.client_msg_id.as_deref(), Some(client_msg_id.as_str()));
        prop_assert!(reader.next_frame().expect("io on a cursor").is_none());
    }

    /// Totality: arbitrary byte streams (<= 64 KiB) through `next_frame` in a loop terminate
    /// without a panic — frames, exhaustion (`Ok(None)`) and `Err(InvalidData)` are all fine.
    #[test]
    fn arbitrary_byte_streams_never_panic(
        bytes in prop::collection::vec(any::<u8>(), 0..65536),
    ) {
        let mut reader = FrameReader::new(Cursor::new(bytes));
        let mut terminated = false;
        // Each iteration consumes a buffered frame, errors, or hits EOF; the cap only turns a
        // hypothetical livelock into a test failure instead of a hang.
        for _ in 0..65_600 {
            match reader.next_frame() {
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => {
                    terminated = true;
                    break;
                }
            }
        }
        prop_assert!(terminated, "next_frame neither drained nor errored in 65600 iterations");
    }
}

/// A stream whose 4-byte BE length prefix claims `u32::MAX` must answer `Ok(None)` (more bytes
/// needed — the caller owns the timeout) or `Err`, WITHOUT attempting the 4 GiB allocation the
/// prefix advertises: `try_take_frame` sizes nothing off the prefix before the bytes actually
/// arrive, so buffered growth is bounded by what a peer really sends.
///
/// If this test ever OOMs or hangs, that is a REAL finding — a malicious peer could kill the
/// connection thread. STOP AND REPORT it; do not fix it silently in this PR.
#[test]
fn a_u32_max_length_prefix_does_not_allocate() {
    let mut bytes = vec![0xFF, 0xFF, 0xFF, 0xFF]; // length prefix: u32::MAX
    bytes.extend_from_slice(&[0x08, 0x33, 0x12, 0x00]); // a few plausible protobuf bytes
    let mut reader = FrameReader::new(Cursor::new(bytes));
    for _ in 0..4 {
        match reader.next_frame() {
            Ok(None) | Err(_) => {} // waiting for bytes that never come / explicit refusal
            Ok(Some(m)) => panic!("a truncated 4 GiB claim decoded to a frame: {m:?}"),
        }
    }
}
