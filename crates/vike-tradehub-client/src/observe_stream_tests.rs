use super::*;
use crate::wire::CoreSnapshotWire;
use vike_hub_protocol::frame::{encode_reply, read_frame_raw, write_frame};

fn frame() -> Response {
    let mut wire = CoreSnapshotWire::empty();
    wire.sequence = 42;
    wire.recent_events = (0..200).map(|i| format!("FillEvent c-{i} 0.1@100")).collect();
    Response::SnapshotFrame(Box::new(wire))
}

/// A compressed body decodes to the very response its plain body does, and is smaller.
#[test]
fn a_compressed_body_decodes_to_the_same_response() {
    let json = serde_json::to_vec(&frame()).expect("serializes");
    let compressed = compress_body(&json);
    assert_eq!(compressed[0], COMPRESSED_BODY_MARKER);
    assert!(compressed.len() * 3 < json.len(), "{} of {}", compressed.len(), json.len());
    assert_eq!(decode_response(&compressed, BodyCodec::Json).expect("inflates"), frame());
    assert_eq!(decode_response(&json, BodyCodec::Json).expect("plain"), frame());
}

/// A msgpack connection reads its deflated body (smaller) and every form of the hub codec, LZ4 included; a deflated JSON
/// body, which a node of this build never sends it, is no form there: refused, never misread.
#[test]
fn a_msgpack_connection_reads_its_deflated_body_and_the_codecs_forms() {
    let body = |form| encode_reply(form, &frame()).expect("encodes")[4..].to_vec();
    let plain = body(BodyCodec::Msgpack);
    let deflated = deflate_msgpack_body(&plain);
    assert!(deflated.starts_with(&MSGPACK_DEFLATE_BODY_MAGIC));
    assert!(deflated.len() * 2 < plain.len(), "{} of {}", deflated.len(), plain.len());
    for each in [plain, deflated, body(BodyCodec::MsgpackLz4)] {
        assert_eq!(decode_response(&each, BodyCodec::Msgpack).expect("decodes"), frame());
    }
    let json = serde_json::to_vec(&frame()).expect("serializes");
    assert!(decode_response(&compress_body(&json), BodyCodec::Msgpack).is_err());
}

/// A framed compressed body reads back through the codec's raw read, and the plain frame `write_frame` writes is
/// decoded unchanged: the reader takes either from a node that decides per its row.
#[test]
fn either_body_reads_off_the_codec() {
    let json = serde_json::to_vec(&frame()).expect("serializes");
    let framed = frame_body(&compress_body(&json)).expect("fits");
    let body = read_frame_raw(&mut &framed[..]).expect("a whole frame");
    assert_eq!(decode_response(&body, BodyCodec::Json).expect("inflates"), frame());
    let mut plain = Vec::new();
    write_frame(&mut plain, &frame()).expect("frames");
    let body = read_frame_raw(&mut &plain[..]).expect("a whole frame");
    assert_eq!(decode_response(&body, BodyCodec::Json).expect("plain"), frame());
}

/// A JSON body never opens with the marker, so no plain frame is ever read as compressed.
#[test]
fn no_plain_body_opens_with_the_marker() {
    for response in [frame(), Response::Pong, Response::Error("x".into())] {
        let json = serde_json::to_vec(&response).expect("serializes");
        assert_ne!(json[0], COMPRESSED_BODY_MARKER);
    }
}

/// The receiver rebuilds a state from a whole frame and its patches, says ONE resync at a gap, waits out the patches
/// until a whole frame, and then rebuilds again.
#[test]
fn the_receiver_rebuilds_and_asks_once_at_a_gap() {
    use crate::proto::SnapshotPatchWire;
    let state = |sequence: u64| {
        let mut wire = CoreSnapshotWire::empty();
        wire.sequence = sequence;
        wire
    };
    let value = |sequence: u64| serde_json::to_value(state(sequence)).expect("a value");
    let patched = |frame: u64, base: Option<u64>, patch: serde_json::Value| {
        Response::SnapshotPatch(Box::new(SnapshotPatchWire { frame, base, patch }))
    };
    let next =
        |from: u64, to: u64| crate::observe_patch::diff(&value(from), &value(to)).expect("changed");
    let mut receiver = ObserveReceiver::default();
    assert_eq!(
        receiver.receive(patched(1, None, value(10))),
        Received::Snapshot(Box::new(state(10)))
    );
    assert_eq!(
        receiver.receive(patched(2, Some(1), next(10, 11))),
        Received::Snapshot(Box::new(state(11)))
    );
    // Frame 3 never arrives.
    assert_eq!(receiver.receive(patched(4, Some(3), next(12, 13))), Received::Resync);
    assert_eq!(
        receiver.receive(patched(5, Some(4), next(13, 14))),
        Received::Nothing,
        "asked once"
    );
    assert_eq!(
        receiver.receive(patched(6, None, value(15))),
        Received::Snapshot(Box::new(state(15)))
    );
    assert_eq!(
        receiver.receive(patched(7, Some(6), next(15, 16))),
        Received::Snapshot(Box::new(state(16)))
    );
    assert_eq!(receiver.receive(Response::Pong), Received::Nothing);
    assert_eq!(
        receiver.receive(Response::SnapshotFrame(Box::new(state(3)))),
        Received::Snapshot(Box::new(state(3))),
        "a whole plain frame is the state as it is"
    );
}

/// A body that inflates past the ceiling is refused, never held.
#[test]
fn an_inflation_past_the_ceiling_is_refused() {
    let zeros = vec![b' '; MAX_FRAME_LEN as usize + 1];
    let compressed = compress_body(&zeros);
    assert!(compressed.len() < 1_000_000, "the bomb is small: {}", compressed.len());
    let err = plain_body(&compressed).expect_err("refused");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}
