use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    BodyCodec, MAX_FRAME_LEN, MSGPACK_BODY_MAGIC, MSGPACK_LZ4_BODY_MAGIC, decode_body,
    encode_frame, encode_reply, json_body_len, read_frame_raw, write_frame, write_frame_as,
};

/// A message of every shape the codec carries: a tag, a NaN, a negative zero, an `i64` extreme, an absent optional and
/// a new optional field an older reader skips.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum Message {
    Ping,
    Run { name: String, equity: Vec<f64>, at: i64, note: Option<String> },
}

fn sample() -> Message {
    let equity = (0..2_000).map(|i| 1_000.0 + f64::from(i % 37) * 0.25).collect();
    Message::Run { name: "joat".to_string(), equity, at: i64::MIN, note: None }
}

/// Every codec round-trips a message to the very value, and a JSON frame is the bytes `write_frame` always
/// wrote. MUTATION PROOF: drop the magic, or write msgpack without field names, and the decode or the
/// older-reader leg fails.
#[test]
fn every_codec_round_trips_and_json_keeps_its_bytes() {
    for codec in [BodyCodec::Json, BodyCodec::Msgpack, BodyCodec::MsgpackLz4] {
        let mut wire = Vec::new();
        write_frame_as(&mut wire, codec, &sample()).expect("written");
        let body = read_frame_raw(&mut std::io::Cursor::new(&wire)).expect("one frame");
        assert_eq!(BodyCodec::of(&body), codec, "the body says its codec");
        assert_eq!(decode_body::<Message>(&body).expect("decoded"), sample(), "{codec:?}");
    }
    let mut json = Vec::new();
    write_frame(&mut json, &sample()).expect("json");
    let mut again = Vec::new();
    write_frame_as(&mut again, BodyCodec::Json, &sample()).expect("json");
    assert_eq!(again, json, "a JSON frame is today's bytes");
    // Bits, not values: a NaN and a negative zero cross msgpack as themselves (JSON writes `null` for the one).
    let odd = vec![f64::NAN, -0.0, f64::INFINITY];
    let body = &encode_frame(BodyCodec::Msgpack, &odd).expect("msgpack")[4..];
    let back: Vec<f64> = decode_body(body).expect("decoded");
    assert_eq!(
        back.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        odd.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
    );
    // An older reader of the same message without `note`'s neighbours: field names make it skip what it lacks.
    #[derive(Deserialize)]
    enum Older {
        Run { name: String },
    }
    let body = &encode_frame(BodyCodec::Msgpack, &sample()).expect("msgpack")[4..];
    let Older::Run { name } =
        decode_body::<Older>(body).expect("an older reader skips the new fields");
    assert_eq!(name, "joat");
}

/// LZ4: a request in `MsgpackLz4` is always the LZ4 form (it carries the ask), a reply only when LZ4 shrinks it,
/// and either decodes to the plain msgpack's value. MUTATION PROOF: answer LZ4 to the tiny `Ping`, or plain to
/// the long curve, and the reply legs fail.
#[test]
fn lz4_is_identity_and_a_reply_compresses_only_when_it_shrinks() {
    let request = encode_frame(BodyCodec::MsgpackLz4, &Message::Ping).expect("request");
    assert!(request[4..].starts_with(&MSGPACK_LZ4_BODY_MAGIC), "a request asks by its form");
    assert_eq!(decode_body::<Message>(&request[4..]).expect("decoded"), Message::Ping);
    let tiny = encode_reply(BodyCodec::MsgpackLz4, &Message::Ping).expect("reply");
    assert!(tiny[4..].starts_with(&MSGPACK_BODY_MAGIC), "LZ4 cannot shrink a Ping: answered plain");
    let long = encode_reply(BodyCodec::MsgpackLz4, &sample()).expect("reply");
    let plain = encode_reply(BodyCodec::Msgpack, &sample()).expect("plain");
    assert!(long[4..].starts_with(&MSGPACK_LZ4_BODY_MAGIC), "a compressible reply is LZ4");
    assert!(long.len() < plain.len(), "{} >= {}", long.len(), plain.len());
    assert_eq!(decode_body::<Message>(&long[4..]).expect("decoded"), sample(), "LZ4 is identity");
}

/// A garbled LZ4 body is refused, never read short, and a stated length over the frame cap is refused before
/// any buffer is made.
#[test]
fn a_garbled_lz4_body_is_refused() {
    let long = encode_reply(BodyCodec::MsgpackLz4, &sample()).expect("reply");
    let body = &long[4..];
    assert!(decode_body::<Message>(&body[..body.len() - 3]).is_err(), "a truncated block");
    assert!(decode_body::<Message>(&body[..6]).is_err(), "ends inside its length");
    let mut huge = MSGPACK_LZ4_BODY_MAGIC.to_vec();
    huge.extend_from_slice(&(MAX_FRAME_LEN + 1).to_le_bytes());
    let err = decode_body::<Message>(&huge).expect_err("over the cap");
    assert!(err.to_string().contains("MAX_FRAME_LEN"), "{err}");
    let mut short = body.to_vec();
    short[4..8].copy_from_slice(&1u32.to_le_bytes());
    assert!(decode_body::<Message>(&short).is_err(), "a block longer than its stated length");
}

/// The datahub's frame budget and the gatewayhub client's pager both size rows with it: the count is
/// the byte length `serde_json::to_vec` would produce, and a value serde refuses (a map keyed by
/// something that is not a string) is `None`, never a partial count. MUTATION PROOF: count anything
/// but `buffer.len()` in `ByteCount::write`, or answer `Some` on an error, and this fails.
#[test]
fn json_body_len_is_the_serialized_length_and_none_for_an_unwritable_value() {
    let row = serde_json::json!({"ts": 1, "px": 1.5, "sym": "BTCUSDT", "levels": [[1, 2], [3, 4]]});
    assert_eq!(json_body_len(&row), Some(serde_json::to_vec(&row).expect("json").len()));
    assert_eq!(json_body_len(&Vec::<u8>::new()), Some(2), "`[]`");
    let unwritable: BTreeMap<Vec<u8>, u8> = BTreeMap::from([(vec![1], 1)]);
    assert_eq!(json_body_len(&unwritable), None, "a non-string map key is refused by serde_json");
}

/// A reader of many frames reads them into ONE buffer: each body is the one `read_frame_raw` reads, a frame that
/// fits reuses the allocation, an empty buffer gets the exact allocation `read_frame_raw` has always returned, and an
/// over-cap length is refused before the buffer is touched. MUTATION PROOF: replace the clear-and-resize with a fresh
/// `vec!` and the reuse assertion fails; drop the cap check and the refusal does.
#[test]
fn read_frame_raw_into_reads_every_frame_into_one_buffer() {
    let mut wire = Vec::new();
    let bodies: [&[u8]; 4] =
        [b"a longer first body", b"short", b"", b"the longest body of the four"];
    for body in bodies {
        super::write_frame_raw(&mut wire, body).expect("frame");
    }
    let mut expected = std::io::Cursor::new(wire.clone());
    let mut cursor = std::io::Cursor::new(wire);
    let mut body = Vec::new();
    super::read_frame_raw_into(&mut cursor, &mut body).expect("first");
    assert_eq!(body, super::read_frame_raw(&mut expected).expect("first"));
    assert_eq!(body.capacity(), body.len(), "an empty buffer gets read_frame_raw's allocation");
    let (first, capacity) = (body.as_ptr(), body.capacity());
    for _ in 0..2 {
        super::read_frame_raw_into(&mut cursor, &mut body).expect("a shorter frame");
        assert_eq!(body, super::read_frame_raw(&mut expected).expect("the same frame"));
        assert_eq!((body.as_ptr(), body.capacity()), (first, capacity), "reused, not reallocated");
    }
    super::read_frame_raw_into(&mut cursor, &mut body).expect("a longer frame");
    assert_eq!(body, super::read_frame_raw(&mut expected).expect("the same frame"));

    let mut over = std::io::Cursor::new((super::MAX_FRAME_LEN + 1).to_be_bytes().to_vec());
    let kept = body.clone();
    let refused = super::read_frame_raw_into(&mut over, &mut body).expect_err("over the cap");
    assert_eq!(refused.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(body, kept, "refused before the buffer is touched");
}

/// A reply body made from msgpack held already is the body `encode_reply` makes of the value, in each msgpack form
/// and either size (LZ4 taken and not), and JSON has none. MUTATION PROOF: drop the magic, or compress the `Ping`,
/// and a leg fails.
#[test]
fn a_reply_from_held_msgpack_is_the_values_reply() {
    for message in [Message::Ping, sample()] {
        let msgpack = rmp_serde::to_vec_named(&message).expect("msgpack");
        for codec in [BodyCodec::Msgpack, BodyCodec::MsgpackLz4] {
            let held = super::reply_body_of_msgpack(codec, &msgpack).expect("a msgpack form");
            assert_eq!(held, encode_reply(codec, &message).expect("reply")[4..], "{codec:?}");
        }
        assert!(super::reply_body_of_msgpack(BodyCodec::Json, &msgpack).is_none());
    }
}
