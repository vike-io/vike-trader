use std::assert_matches;
use tracing_test::traced_test;

use super::*;
use crate::format::HEADER;
use crate::testutil::*;
use crate::{Journal, JournalFileConfig};

/// Frame `payload` exactly as `Journal::write_framed` does:
/// `[len u32 LE][fnv1a32(payload) u32 LE][payload]` — a VALID frame, so a fixture can then
/// break exactly ONE of the three things `walk_frames` checks and nothing else.
fn framed(payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + payload.len());
    v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    v.extend_from_slice(&fnv1a32(payload).to_le_bytes());
    v.extend_from_slice(payload);
    v
}

/// One valid `Command` frame, as this build writes it (msgpack) — the "good record before the tear" every
/// fixture below leads with, so each proves the walk actually REACHED its planted frame rather than tripping
/// at offset 0.
fn good_frame() -> Vec<u8> {
    let rec = JournalRecord::Command { sequence: 0, now_ms: 1_000, message: ingest(0) };
    let mut payload = Vec::new();
    encode_into(&mut payload, &rec);
    framed(&payload)
}

/// The same record as a format 31..=33 build wrote it: JSON.
fn json_frame() -> Vec<u8> {
    let rec = JournalRecord::Command { sequence: 0, now_ms: 1_000, message: ingest(0) };
    framed(&serde_json::to_vec(&rec).expect("JournalRecord serializes"))
}

/// **The payload is msgpack, and a JSON one still reads**: the frame this build writes and the one a format 31..=33
/// build wrote for the same record decode to the same record, so a journal moved aside by the binary step reads
/// (and streams) unchanged; the msgpack is the smaller of the two.
#[test]
fn a_json_frame_and_a_msgpack_frame_of_one_record_read_alike() {
    let (binary, json) = (good_frame(), json_frame());
    assert_eq!(binary[8], 0x81, "a msgpack record is a one-entry map");
    assert_eq!(json[8], b'{', "a JSON record is an object");
    assert!(binary.len() < json.len(), "msgpack {} < JSON {} bytes", binary.len(), json.len());
    let read = |buffer: &[u8]| {
        let mut out = Vec::new();
        let (at, end) = walk_frames(buffer, 0, |rec| out.push(rec));
        assert_eq!(at, buffer.len());
        assert_matches!(end, JournalWalkEnd::Clean);
        out
    };
    let (from_binary, from_json) = (read(&binary), read(&json));
    assert_eq!(from_binary.len(), 1);
    assert_eq!(
        serde_json::to_value(&from_binary[0]).unwrap(),
        serde_json::to_value(&from_json[0]).unwrap(),
        "one record, whichever format wrote it"
    );
}

/// **The bytes, pinned** (format 34's golden): three records hand-derived from the msgpack spec. Each struct is a
/// map of its field names in declaration order (a `skip_serializing_if` field that skips writes no key), each
/// enum variant a one-entry map keyed by its name, each integer the smallest marker, each `f64` nine bytes. A
/// change to the codec's configuration or to these records' shapes moves them: that is a format step.
#[test]
fn the_binary_payload_is_pinned_byte_for_byte() {
    let encode = |rec: &crate::record::JournalRecordRef<'_>| {
        let mut payload = Vec::new();
        encode_into(&mut payload, rec);
        payload
    };
    let disarmed = encode(&crate::record::JournalRecordRef::OrderEmulationDisarmed {
        sequence: 7,
        now_ms: 1_000,
        arm_id: "a1",
    });
    let want: Vec<u8> = [
        &[0x81, 0xb6][..],
        b"OrderEmulationDisarmed",
        &[0x83, 0xa8],
        b"sequence",
        &[0x07, 0xa6],
        b"now_ms",
        &[0xcd, 0x03, 0xe8, 0xa6],
        b"arm_id",
        &[0xa2],
        b"a1",
    ]
    .concat();
    assert_eq!(disarmed, want, "OrderEmulationDisarmed");

    let expire = encode(&crate::record::JournalRecordRef::GtdExpire {
        sequence: 300,
        now_ms: -5,
        order_client_id: "o",
        engine: 2,
    });
    let want: Vec<u8> = [
        &[0x81, 0xa9][..],
        b"GtdExpire",
        &[0x84, 0xa8],
        b"sequence",
        &[0xcd, 0x01, 0x2c, 0xa6],
        b"now_ms",
        &[0xfb, 0xaf],
        b"order_client_id",
        &[0xa1],
        b"o",
        &[0xa6],
        b"engine",
        &[0x02],
    ]
    .concat();
    assert_eq!(expire, want, "GtdExpire: a u16 sequence, a negative fixint clock");

    // The maker's record (~70% of the CI box's journal before the lean step): a strategy's Modify.
    let intent = vike_execution::OrderIntent::Modify {
        order_client_id: "b1".into(),
        new_qty: None,
        new_price: Some(100.5),
        new_trigger_price: None,
    };
    let modify = encode(&crate::record::JournalRecordRef::StrategySubmit {
        sequence: 9,
        now_ms: 2_000,
        mount_id: "m",
        intent: &intent,
        route_key: None,
    });
    let want: Vec<u8> = [
        &[0x81, 0xae][..],
        b"StrategySubmit",
        &[0x84, 0xa8],
        b"sequence",
        &[0x09, 0xa6],
        b"now_ms",
        &[0xcd, 0x07, 0xd0, 0xa8],
        b"mount_id",
        &[0xa1],
        b"m",
        &[0xa6],
        b"intent",
        &[0x81, 0xa6],
        b"Modify",
        &[0x83, 0xaf],
        b"order_client_id",
        &[0xa2],
        b"b1",
        &[0xa7],
        b"new_qty",
        &[0xc0, 0xa9],
        b"new_price",
        &[0xcb, 0x40, 0x59, 0x20, 0, 0, 0, 0, 0],
    ]
    .concat();
    assert_eq!(
        modify, want,
        "StrategySubmit(Modify): an absent route key and trigger write no key"
    );
    let Some(JournalRecord::StrategySubmit {
        sequence: 9,
        now_ms: 2_000,
        mount_id,
        intent: vike_execution::OrderIntent::Modify { order_client_id, new_price, .. },
        route_key: None,
    }) = decode(&modify)
    else {
        panic!("the pinned bytes decode back");
    };
    assert_eq!((mount_id.as_str(), order_client_id.as_str(), new_price), ("m", "b1", Some(100.5)));
}

/// **The position rows, pinned** (a boot's `PositionSnapshot`; a `PortfolioSnapshot` sample's rows are the same type),
/// hand-derived like the golden above: each row is a map of `venue`, `symbol`, `position_side` (its WORD, a string),
/// `size` and `average_price`, and an account whose positions are known writes no `unknown` key. The JSON a format
/// 31..=33 build wrote for the same record reads back the same.
#[test]
fn the_position_rows_are_pinned_byte_for_byte() {
    let accounts = [crate::JournalPositionSnapshotAccount {
        venue: "b".into(),
        account: "A".into(),
        positions: vec![vike_model::AccountVenuePosition {
            venue: "b".into(),
            symbol: "X".into(),
            position_side: vike_model::events::PositionSide::Long,
            size: -0.5,
            average_price: 2.0,
        }],
        unknown: None,
    }];
    let mut payload = Vec::new();
    encode_into(
        &mut payload,
        &crate::record::JournalRecordRef::PositionSnapshot {
            sequence: 1,
            now_ms: 2,
            accounts: &accounts,
        },
    );
    let want: Vec<u8> = [
        &[0x81, 0xb0][..],
        b"PositionSnapshot",
        &[0x83, 0xa8],
        b"sequence",
        &[0x01, 0xa6],
        b"now_ms",
        &[0x02, 0xa8],
        b"accounts",
        &[0x91, 0x83, 0xa5],
        b"venue",
        &[0xa1],
        b"b",
        &[0xa7],
        b"account",
        &[0xa1],
        b"A",
        &[0xa9],
        b"positions",
        &[0x91, 0x85, 0xa5],
        b"venue",
        &[0xa1],
        b"b",
        &[0xa6],
        b"symbol",
        &[0xa1],
        b"X",
        &[0xad],
        b"position_side",
        &[0xa4],
        b"LONG",
        &[0xa4],
        b"size",
        &[0xcb, 0xbf, 0xe0, 0, 0, 0, 0, 0, 0, 0xad],
        b"average_price",
        &[0xcb, 0x40, 0, 0, 0, 0, 0, 0, 0],
    ]
    .concat();
    assert_eq!(payload, want, "PositionSnapshot: one account, one row, no `unknown` key");
    let json = br#"{"PositionSnapshot":{"sequence":1,"now_ms":2,"accounts":[{"venue":"b","account":"A","positions":[{"venue":"b","symbol":"X","position_side":"LONG","size":-0.5,"average_price":2.0}]}]}}"#;
    for (door, bytes) in [("msgpack", &payload[..]), ("JSON", &json[..])] {
        let Some(JournalRecord::PositionSnapshot { sequence: 1, now_ms: 2, accounts: read }) =
            decode(bytes)
        else {
            panic!("the {door} bytes decode back");
        };
        assert_eq!(read, accounts, "{door}");
    }
}

/// msgpack keeps every `f64` bit, a NaN and an infinity too, where JSON wrote `null`: an `f64` round-trips exactly.
#[test]
fn a_non_finite_price_survives_the_binary_payload() {
    for price in [f64::NAN, f64::INFINITY, -0.0, 0.1 + 0.2, f64::MIN_POSITIVE] {
        let intent = vike_execution::OrderIntent::Modify {
            order_client_id: "b1".into(),
            new_qty: Some(price),
            new_price: Some(price),
            new_trigger_price: None,
        };
        let mut payload = Vec::new();
        encode_into(
            &mut payload,
            &crate::record::JournalRecordRef::StrategySubmit {
                sequence: 1,
                now_ms: 1,
                mount_id: "m",
                intent: &intent,
                route_key: None,
            },
        );
        let Some(JournalRecord::StrategySubmit {
            intent: vike_execution::OrderIntent::Modify { new_qty: Some(q), new_price: Some(p), .. },
            ..
        }) = decode(&payload)
        else {
            panic!("{price}: decodes");
        };
        assert_eq!((q.to_bits(), p.to_bits()), (price.to_bits(), price.to_bits()), "{price}");
    }
}

/// A msgpack payload that checks and is no record of this build (an unknown variant) is a parse failure, as a
/// JSON one is.
#[test]
fn a_msgpack_payload_that_is_no_record_is_parse_failed() {
    let payload = payload_of_value(&serde_json::json!({"NotAVariantOfThisBuild": {"sequence": 0}}));
    assert_eq!(payload[0], 0x81);
    let mut buffer = good_frame();
    let tear = buffer.len();
    buffer.extend_from_slice(&framed(&payload));
    let (at, end) = walk_frames(&buffer, 0, |_| {});
    assert_eq!(at, tear);
    assert_matches!(end, JournalWalkEnd::Corrupt(JournalCorruptCause::ParseFailed));
}

/// A payload that CHECKSUMS CORRECTLY and is not a `JournalRecord` for this build — an unknown
/// externally-tagged variant. This is what a segment written by a DIFFERENT build looks like:
/// nothing tore, the bytes are exactly what was written, and they still do not parse.
fn schema_mismatch_payload() -> Vec<u8> {
    br#"{"NotAVariantOfThisBuild":{"sequence":0}}"#.to_vec()
}

/// FIXTURE 1 of 3 — an out-of-bounds length prefix is `JournalCorruptCause::OobLen`, and NOT either of
/// the other two. Asserting the SPECIFIC variant is the point: a three-way enum whose arms no
/// test tells apart is decoration. (Mutation-checked: pointing this arm at `CheckMismatch` or
/// `ParseFailed` in `walk_frames` reddens this test and only this test.)
#[test]
fn oob_len_is_reported_as_oob_len() {
    let mut buffer = good_frame();
    // a length prefix claiming far more bytes than the buffer holds, with a plausible check
    buffer.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    buffer.extend_from_slice(&0u32.to_le_bytes());
    let tear = good_frame().len();

    let mut seen = 0usize;
    let (current, end) = walk_frames(&buffer, 0, |_| seen += 1);
    assert_eq!(seen, 1, "the walk delivered the one valid record BEFORE the tear");
    assert_eq!(current, tear, "the walk halted AT the torn frame, never past it");
    match end {
        JournalWalkEnd::Corrupt(cause) => assert_eq!(
            cause,
            JournalCorruptCause::OobLen,
            "an out-of-bounds len is a torn LENGTH write, not a check or parse failure"
        ),
        JournalWalkEnd::Clean => panic!("an out-of-bounds len must not read as a clean end"),
    }
}

/// FIXTURE 2 of 3 — a flipped payload byte under an untouched check is
/// `JournalCorruptCause::CheckMismatch`, and the walk never reaches the parse.
#[test]
fn check_mismatch_is_reported_as_check_mismatch() {
    let good = good_frame();
    let mut buffer = good.clone();
    let tear = buffer.len();
    let mut torn = good.clone();
    let last = torn.len() - 1;
    torn[last] ^= 0xFF; // payload changed, stored check NOT recomputed
    buffer.extend_from_slice(&torn);

    let mut seen = 0usize;
    let (current, end) = walk_frames(&buffer, 0, |_| seen += 1);
    assert_eq!(seen, 1, "only the intact record is delivered");
    assert_eq!(current, tear);
    match end {
        JournalWalkEnd::Corrupt(cause) => assert_eq!(
            cause,
            JournalCorruptCause::CheckMismatch,
            "the payload does not match its own check — the parse is never attempted"
        ),
        JournalWalkEnd::Clean => panic!("a check mismatch must not read as a clean end"),
    }
}

/// FIXTURE 3 of 3 — the arm this file had NEVER covered: a payload framed with a **VALID** FNV
/// check that is not a `JournalRecord`. It must be `JournalCorruptCause::ParseFailed`, because it is the
/// one condition that is a SCHEMA mismatch (a different build wrote it) rather than a torn tail,
/// and the two want opposite operator readings.
#[test]
fn valid_check_over_a_non_record_payload_is_reported_as_parse_failed() {
    let payload = schema_mismatch_payload();
    // the fixture is only honest if the check REALLY passes — otherwise it would trip one arm
    // earlier and "prove" nothing about the parse arm
    let planted = framed(&payload);
    assert_eq!(
        fnv1a32(&payload),
        u32::from_le_bytes(planted[4..8].try_into().unwrap()),
        "the planted frame's check must be VALID — that is the whole premise of this fixture"
    );

    let mut buffer = good_frame();
    let tear = buffer.len();
    buffer.extend_from_slice(&planted);

    let mut seen = 0usize;
    let (current, end) = walk_frames(&buffer, 0, |_| seen += 1);
    assert_eq!(seen, 1, "the non-record frame is NOT delivered as a record");
    assert_eq!(current, tear, "the walk halted at the unparseable frame, not past it");
    match end {
        JournalWalkEnd::Corrupt(cause) => assert_eq!(
            cause,
            JournalCorruptCause::ParseFailed,
            "checksum-valid bytes that are not a JournalRecord are a SCHEMA mismatch"
        ),
        JournalWalkEnd::Clean => panic!("an unparseable payload must not read as a clean end"),
    }
}

/// The three causes are pairwise DISTINCT and each carries its own operator reading — the
/// property the three fixtures above rely on, asserted directly so collapsing two arms into one
/// variant cannot pass by making both fixtures agree.
#[test]
fn the_three_causes_are_distinct_and_each_reads_differently() {
    let all = [
        JournalCorruptCause::OobLen,
        JournalCorruptCause::CheckMismatch,
        JournalCorruptCause::ParseFailed,
    ];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(a, b);
            assert_ne!(a.as_str(), b.as_str(), "log tokens must not collide");
            assert_ne!(a.reading(), b.reading(), "operator readings must not collide");
        }
    }
    // the log token is the field a query keys on — pin the spellings
    assert_eq!(JournalCorruptCause::OobLen.as_str(), "oob_len");
    assert_eq!(JournalCorruptCause::CheckMismatch.as_str(), "check_mismatch");
    assert_eq!(JournalCorruptCause::ParseFailed.as_str(), "parse_failed");
    // and only the schema-mismatch reading says SCHEMA — the one that must be read as loud
    assert!(JournalCorruptCause::ParseFailed.reading().contains("SCHEMA"));
    assert!(!JournalCorruptCause::OobLen.reading().contains("SCHEMA"));
    assert!(!JournalCorruptCause::CheckMismatch.reading().contains("SCHEMA"));
}

/// The `ParseFailed` arm through the REAL on-disk reader: plant a checksum-valid non-record
/// frame after 10 good records in a real segment and confirm `read_all` keeps the valid prefix
/// and drops it — behaviour UNCHANGED (this change is diagnostic only), reached by the parse arm
/// rather than by a check mismatch.
#[test]
fn read_all_drops_a_checksum_valid_non_record_frame() {
    let dir = tmp_dir("parse_failed");
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..10 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);

    // Walk to the write cursor (past the 10 records) the way this file's other on-disk fixtures
    // do — the cursor is not exposed. `segment_files`, not the first `read_dir` entry: the dir also
    // holds the `LOCK` sentinel.
    let seg = segment_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    let mut current = HEADER;
    for _ in 0..10 {
        let len = u32::from_le_bytes(bytes[current..current + 4].try_into().unwrap()) as usize;
        assert_ne!(len, 0, "expected 10 valid records while walking to the write cursor");
        current += 8 + len;
    }
    let planted = framed(&schema_mismatch_payload());
    bytes[current..current + planted.len()].copy_from_slice(&planted);
    std::fs::write(&seg, bytes).unwrap();

    // The planted frame is reached through the PARSE arm, not the check arm.
    let bytes = std::fs::read(&seg).unwrap();
    let (halt, end) = walk_frames(&bytes, HEADER, |_| {});
    assert_eq!(halt, current, "the halt offset is the planted frame's own offset");
    assert_matches!(end, JournalWalkEnd::Corrupt(JournalCorruptCause::ParseFailed));

    let back = Journal::read_all(&dir).unwrap();
    assert_eq!(back.len(), 10, "the 10 valid records survive; the non-record frame is dropped");
}

/// The seam that ACTS on the halt logs it, once, naming the segment, the halt offset and the
/// cause. Before this the halt decision was taken in silence — a reader of a live node's logs
/// could not tell that replay had stopped early, let alone why.
#[traced_test]
#[test]
fn read_all_warns_once_naming_the_segment_offset_and_cause() {
    let dir = tmp_dir("halt_warn");
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..4 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);

    let seg = segment_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    let mut current = HEADER;
    for _ in 0..3 {
        let len = u32::from_le_bytes(bytes[current..current + 4].try_into().unwrap()) as usize;
        current += 8 + len;
    }
    // the 4th record's length prefix, torn to an out-of-bounds value
    bytes[current..current + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();

    assert_eq!(Journal::read_all(&dir).unwrap().len(), 3);
    assert!(logs_contain("journal read halted at a corrupt frame"));
    assert!(logs_contain("oob_len"), "the CAUSE is named, not just the fact of a halt");
    assert!(logs_contain(&format!("offset={current}")), "the halt OFFSET is named");
    assert!(
        logs_contain(&seg.file_name().unwrap().to_string_lossy()),
        "the SEGMENT is named — a journal has many"
    );
    assert!(
        !logs_contain("parse_failed") && !logs_contain("check_mismatch"),
        "one cause per halt: a torn length must not also report the other two"
    );
}

#[test]
fn torn_tail_is_detected_and_stops_cleanly() {
    let dir = tmp_dir("torn");
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..10 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    // corrupt the last record's payload byte (simulates a torn page after power cut).
    // Select the SEGMENT explicitly (`segment_files`) rather than the first `read_dir` entry: the
    // dir also holds the interlock sentinel (`LOCK`), whose position in the directory listing
    // is filesystem-dependent.
    let seg = segment_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    let n = bytes.len();
    // walk to the last record start is overkill — flip a byte near the end of written data
    let last_nonzero = bytes.iter().rposition(|&b| b != 0).unwrap();
    bytes[last_nonzero] ^= 0xFF;
    std::fs::write(&seg, bytes).unwrap();
    let back = Journal::read_all(&dir).unwrap();
    assert_eq!(back.len(), 9, "replay stops at the last VALID record, no panic (had {n} bytes)");
}

#[test]
fn torn_tail_out_of_bounds_len() {
    let dir = tmp_dir("oob_len");
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..10 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    // Corrupt the LAST written record's 4 len-prefix bytes to an out-of-bounds huge length
    // (simulates a torn length-prefix write after a power cut — distinct from the CRC-flip
    // torn-tail case above). Records are appended contiguously from HEADER as
    // `[len][crc][payload]` frames, and `next_sequence`/the write cursor aren't exposed, so walk
    // the frames: skip the first 9 records to land on the 10th (last) record's start offset.
    // `segment_files` (not the first `read_dir` entry) — the dir also holds the `LOCK` sentinel.
    let seg = segment_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    let mut current = HEADER;
    for _ in 0..9 {
        let len = u32::from_le_bytes(bytes[current..current + 4].try_into().unwrap()) as usize;
        assert_ne!(len, 0, "expected a valid record while walking frames to the 10th record");
        current += 8 + len;
    }
    let last_record_start = current; // offset of the 10th (last) record's [len] prefix
    bytes[last_record_start..last_record_start + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();
    let back = Journal::read_all(&dir).unwrap();
    assert_eq!(back.len(), 9, "replay stops before the out-of-bounds-len record, no panic");
}

/// Cross-segment guard for the unified `walk_frames` halt policy: a corruption in an EARLY
/// segment must stop replay across the WHOLE journal — records in LATER segments (past the
/// corruption gap) are NEVER returned. Every other corruption test here is single-segment, so
/// this is the case that would silently regress if `read_all`'s journal-wide halt were ever
/// turned into a per-file break (which would still pass all the single-segment tests).
#[test]
fn read_all_halts_journal_wide_at_a_corrupt_early_segment() {
    let dir = tmp_dir("cross-seg-halt");
    // small segments so the journal rolls into several files
    let config = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..200 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);

    // The scenario is only meaningful if there is a LATER segment whose records could be
    // wrongly replayed past the corruption gap.
    let mut segments = segment_files(&dir);
    segments.sort();
    assert!(segments.len() >= 2, "scenario must span multiple segments (got {})", segments.len());

    // Corrupt the 6th record (index 5) of the FIRST segment: overwrite its len prefix with an
    // out-of-bounds huge length (a torn length-prefix write). Walk the frames to find it.
    let segment0 = &segments[0];
    let mut bytes = std::fs::read(segment0).unwrap();
    let mut current = HEADER;
    for _ in 0..5 {
        let len = u32::from_le_bytes(bytes[current..current + 4].try_into().unwrap()) as usize;
        assert_ne!(len, 0, "expected valid records while walking to the 6th record");
        current += 8 + len;
    }
    bytes[current..current + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    std::fs::write(segment0, bytes).unwrap();

    let back = Journal::read_all(&dir).unwrap();
    assert_eq!(
        back.len(),
        5,
        "replay halts journal-wide at the corrupt early segment; later segments are NOT replayed"
    );
    for (i, r) in back.iter().enumerate() {
        assert_matches!(
            r, JournalRecord::Command { sequence: seq, .. } if *seq == i as u64,
            "the 5 records before the corruption survive, in order"
        );
    }
}

/// **What main wrote before the interned venue and symbol changed type** (2026-11-20: `ustr::Ustr` became
/// `vike_marketdata::Symbol`, an in-memory change only). Four `Command` records that set every kind of interned
/// field a record carries: a gateway fill and its ring key, a labelled account's funding with its currency on a
/// 77-character Polymarket token id, a fill whose fee asset is empty (so it writes no key), and a trade-link change.
/// The pins are the msgpack payloads and the JSON a build of main `e32ef713dc` wrote for them: each payload reads
/// back and is written again as the same bytes, and so is the JSON a format 31..=33 journal held.
#[test]
fn records_with_interned_names_main_wrote_read_and_write_back_byte_for_byte() {
    use vike_execution::{GatewayEvent, Ingest, TradeLinkUpdate};
    use vike_model::EventRingCursor;
    use vike_model::events::{
        Event, FillEvent, FundingEvent, LiquiditySide, PositionSide, TradeId,
    };

    let fill = |trade_id: &str, commission_asset: &str| FillEvent {
        trade_id: TradeId::new(trade_id).unwrap(),
        order_client_id: "c1".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 0.25,
        last_price: 64_000.5,
        commission: 0.016,
        commission_asset: commission_asset.into(),
        liquidity_side: LiquiditySide::Taker,
        timestamp: 1_700_000_000_005,
        mark_price: None,
        position_side: PositionSide::Both,
    };
    let command = |sequence: u64, message: Ingest| JournalRecord::Command {
        sequence,
        now_ms: 1_700_000_000_100 + sequence as i64,
        message,
    };
    let records = [
        command(
            1,
            Ingest::GatewayEvent(Box::new(GatewayEvent {
                event: Event::Fill(fill("t-77", "USDT")),
                route_key: "binance:ALT".into(),
                cursor: EventRingCursor {
                    gateway_epoch: 9_007_199_254_740_993,
                    sequence: 44,
                    timestamp_ms: 1_700_000_000_006,
                },
            })),
        ),
        command(
            2,
            Ingest::Event(Event::Funding(FundingEvent {
                venue: "polymarket".into(),
                symbol:
                    "93005850938352995663334573245996733794924636935158112548608169054144721737755"
                        .into(),
                position_side: PositionSide::Short,
                funding_rate: -0.0001,
                amount: 1.25,
                mark_price: Some(0.5),
                timestamp: 1_700_000_000_007,
                route_key: Some("polymarket:B".into()),
                currency: Some("EUR".into()),
            })),
        ),
        command(3, Ingest::Event(Event::Fill(fill("t-78", "")))),
        command(
            4,
            Ingest::TradeLink(Box::new(TradeLinkUpdate {
                route_key: "bybit:X".into(),
                venue: "bybit".into(),
                up: false,
                at_timestamp_ms: 1_700_000_000_008,
            })),
        ),
    ];
    // The bytes and text main wrote for `records`, in order.
    const MAIN_MSGPACK_HEX: [&str; 4] = [
        "81a7436f6d6d616e6483a873657175656e636501a66e6f775f6d73cf0000018bcfe56865a76d657373616765\
         81ac476174657761794576656e7483a56576656e748ea474797065a946696c6c4576656e74a874726164655f\
         6964a4742d3737af6f726465725f636c69656e745f6964a26331a576656e7565a762696e616e6365a673796d\
         626f6ca742544355534454a47369646501a86c6173745f717479cb3fd0000000000000aa6c6173745f707269\
         6365cb40ef401000000000aa636f6d6d697373696f6ecb3f90624dd2f1a9fcb0636f6d6d697373696f6e5f61\
         73736574a455534454ae6c69717569646974795f73696465a574616b6572a974696d657374616d70cf000001\
         8bcfe56805aa6d61726b5f7072696365c0ad706f736974696f6e5f73696465a4424f5448a9726f7574655f6b\
         6579ab62696e616e63653a414c54a6637572736f7283ad676174657761795f65706f6368cf00200000000000\
         01a873657175656e63652cac74696d657374616d705f6d73cf0000018bcfe56806",
        "81a7436f6d6d616e6483a873657175656e636502a66e6f775f6d73cf0000018bcfe56866a76d657373616765\
         81a54576656e748aa474797065ac46756e64696e674576656e74a576656e7565aa706f6c796d61726b6574a6\
         73796d626f6cd94d393330303538353039333833353239393536363333333435373332343539393637333337\
         3934393234363336393335313538313132353438363038313639303534313434373231373337373535ad706f\
         736974696f6e5f73696465a553484f5254ac66756e64696e675f72617465cbbf1a36e2eb1c432da6616d6f75\
         6e74cb3ff4000000000000aa6d61726b5f7072696365cb3fe0000000000000a974696d657374616d70cf0000\
         018bcfe56807a9726f7574655f6b6579ac706f6c796d61726b65743a42a863757272656e6379a3455552",
        "81a7436f6d6d616e6483a873657175656e636503a66e6f775f6d73cf0000018bcfe56867a76d657373616765\
         81a54576656e748da474797065a946696c6c4576656e74a874726164655f6964a4742d3738af6f726465725f\
         636c69656e745f6964a26331a576656e7565a762696e616e6365a673796d626f6ca742544355534454a47369\
         646501a86c6173745f717479cb3fd0000000000000aa6c6173745f7072696365cb40ef401000000000aa636f\
         6d6d697373696f6ecb3f90624dd2f1a9fcae6c69717569646974795f73696465a574616b6572a974696d6573\
         74616d70cf0000018bcfe56805aa6d61726b5f7072696365c0ad706f736974696f6e5f73696465a4424f5448",
        "81a7436f6d6d616e6483a873657175656e636504a66e6f775f6d73cf0000018bcfe56868a76d657373616765\
         81a954726164654c696e6b84a9726f7574655f6b6579a762796269743a58a576656e7565a56279626974a275\
         70c2af61745f74696d657374616d705f6d73cf0000018bcfe56808",
    ];
    const MAIN_JSON: [&str; 4] = [
        r#"{"Command":{"sequence":1,"now_ms":1700000000101,"message":{"GatewayEvent":{"event":{"type":"FillEvent","trade_id":"t-77","order_client_id":"c1","venue":"binance","symbol":"BTCUSDT","side":1,"last_qty":0.25,"last_price":64000.5,"commission":0.016,"commission_asset":"USDT","liquidity_side":"taker","timestamp":1700000000005,"mark_price":null,"position_side":"BOTH"},"route_key":"binance:ALT","cursor":{"gateway_epoch":9007199254740993,"sequence":44,"timestamp_ms":1700000000006}}}}}"#,
        r#"{"Command":{"sequence":2,"now_ms":1700000000102,"message":{"Event":{"type":"FundingEvent","venue":"polymarket","symbol":"93005850938352995663334573245996733794924636935158112548608169054144721737755","position_side":"SHORT","funding_rate":-0.0001,"amount":1.25,"mark_price":0.5,"timestamp":1700000000007,"route_key":"polymarket:B","currency":"EUR"}}}}"#,
        r#"{"Command":{"sequence":3,"now_ms":1700000000103,"message":{"Event":{"type":"FillEvent","trade_id":"t-78","order_client_id":"c1","venue":"binance","symbol":"BTCUSDT","side":1,"last_qty":0.25,"last_price":64000.5,"commission":0.016,"liquidity_side":"taker","timestamp":1700000000005,"mark_price":null,"position_side":"BOTH"}}}}"#,
        r#"{"Command":{"sequence":4,"now_ms":1700000000104,"message":{"TradeLink":{"route_key":"bybit:X","venue":"bybit","up":false,"at_timestamp_ms":1700000000008}}}}"#,
    ];

    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let unhex = |text: &str| {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect::<Vec<u8>>()
    };
    let encoded = |record: &JournalRecord| {
        let mut written = Vec::new();
        encode_into(&mut written, record);
        (written, serde_json::to_string(record).unwrap())
    };
    // What this build writes, every record first (`--nocapture`), so a re-pin reads them all from one run.
    for (i, record) in records.iter().enumerate() {
        let (written, json) = encoded(record);
        println!("PIN msgpack[{i}] {}", hex(&written));
        println!("PIN json[{i}] {json}");
    }
    for (i, record) in records.iter().enumerate() {
        let (written, json) = encoded(record);
        assert_eq!(
            hex(&written),
            MAIN_MSGPACK_HEX[i],
            "record {i}: this build writes main's msgpack"
        );
        assert_eq!(json, MAIN_JSON[i], "record {i}: this build writes main's JSON");

        let main_payload = unhex(MAIN_MSGPACK_HEX[i]);
        let read = decode(&main_payload).expect("main's msgpack payload reads");
        let mut again = Vec::new();
        encode_into(&mut again, &read);
        assert_eq!(
            again, main_payload,
            "record {i}: main's msgpack reads and writes back byte for byte"
        );
        let read = decode(MAIN_JSON[i].as_bytes()).expect("main's JSON payload reads");
        assert_eq!(
            serde_json::to_string(&read).unwrap(),
            MAIN_JSON[i],
            "record {i}: main's JSON, byte for byte"
        );
    }
}

/// **The boot's `Snapshot` search reads a payload's first bytes alone** (`is_snapshot_payload`): a real `Snapshot` frame
/// starts with the tag, and no other record of the same journal does (a `PositionSnapshot` or a `PortfolioSnapshot`
/// carries a longer name, so its fixstr length byte differs).
#[test]
fn a_snapshot_payload_starts_with_its_tag_and_no_other_record_does() {
    let dir = tmp_dir("snap-tag");
    let config = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 64 };
    let mut j = Journal::open(&dir, config).unwrap();
    j.append_command(1_000, &ingest(0)).unwrap();
    j.append_snap(2_000, &snap_engines(), "sess", 7, 0, crate::JournalSnapshotParts::default(), 1)
        .unwrap();
    j.append_command(3_000, &ingest(1)).unwrap();
    drop(j);
    let bytes = std::fs::read(crate::segment::segment_path(&dir, 0)).unwrap();
    let mut seen: Vec<(bool, bool)> = Vec::new();
    let (_, end) = walk_frame_payloads(&bytes, HEADER, |_, payload| {
        let record = decode(payload).expect("a record");
        seen.push((is_snapshot_payload(payload), matches!(record, JournalRecord::Snapshot { .. })));
    });
    assert!(matches!(end, JournalWalkEnd::Clean));
    assert_eq!(seen, vec![(false, false), (true, true), (false, false)]);
}
