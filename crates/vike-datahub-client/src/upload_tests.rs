//! The upload's vocabulary: the request's rules at the door, its frame, and the client's three text
//! helpers (the whole file, a preview's head, the chunks).

use super::*;
use crate::proto::{Request, Response};
use vike_data::csv_bars::CsvBarsFormatWords;

fn target() -> ImportTarget {
    ImportTarget {
        venue: "user".to_string(),
        symbol: "USDT-USD".to_string(),
        interval: "1d".to_string(),
        label: "coinbase".to_string(),
        file_format: CsvBarsFormatWords::default(),
        columns: None,
        sparse: false,
        allow_coarse_grid: false,
    }
}

fn spec() -> CsvUploadSpec {
    CsvUploadSpec { target: target(), bytes: 120, dry_run: false, head: false }
}

#[test]
fn a_spec_is_held_to_the_targets_rules_and_its_own() {
    validate_upload_spec(&spec()).expect("a valid upload");
    validate_upload_spec(&CsvUploadSpec { dry_run: true, head: true, ..spec() })
        .expect("a preview is a dry run of the head");
    let why = validate_upload_spec(&CsvUploadSpec { head: true, ..spec() }).unwrap_err();
    assert!(why.contains("only a dry run"), "{why}");
    let why = validate_upload_spec(&CsvUploadSpec { bytes: 0, ..spec() }).unwrap_err();
    assert!(why.contains("empty"), "{why}");
    let bad = ImportTarget { label: "My File".to_string(), ..target() };
    assert!(validate_upload_spec(&CsvUploadSpec { target: bad, ..spec() }).is_err());
    let bad = ImportTarget { venue: "nowhere".to_string(), ..target() };
    assert!(validate_upload_spec(&CsvUploadSpec { target: bad, ..spec() }).is_err());
}

/// The three frames' shapes, and `dry_run` with no default: a frame that drops it does not decode.
#[test]
fn the_upload_frames_have_their_shape_and_dry_run_cannot_be_dropped() {
    let begin = Request::CsvUpload(spec());
    let v = serde_json::to_value(&begin).unwrap();
    assert!(v["CsvUpload"].get("head").is_none(), "a plain upload leaves `head` off: {v}");
    assert_eq!(v["CsvUpload"]["target"]["venue"], "user", "{v}");
    match serde_json::from_value::<Request>(v.clone()).unwrap() {
        Request::CsvUpload(back) => assert_eq!(back, spec()),
        other => panic!("decoded as {other:?}"),
    }
    let mut dropped = v;
    dropped["CsvUpload"].as_object_mut().unwrap().remove("dry_run");
    assert!(
        serde_json::from_value::<Request>(dropped).is_err(),
        "an omitted dry_run must not decode"
    );
    let chunk = Request::CsvUploadChunk { text: "time,close\n".to_string() };
    assert_eq!(serde_json::to_value(&chunk).unwrap()["CsvUploadChunk"]["text"], "time,close\n");
    assert_eq!(serde_json::to_value(Request::CsvUploadEnd).unwrap(), "CsvUploadEnd");
    let done = CsvUploadDone {
        header: vec!["ts".into(), "close".into()],
        refused: Some("line 3: time: …".into()),
        ..CsvUploadDone::default()
    };
    let frame = serde_json::to_string(&Response::CsvUploaded(Box::new(done.clone()))).unwrap();
    match serde_json::from_str::<Response>(&frame).unwrap() {
        Response::CsvUploaded(back) => assert_eq!(*back, done),
        other => panic!("decoded as {other:?}"),
    }
}

#[test]
fn a_file_is_text_and_a_head_is_cut_on_a_character() {
    assert_eq!(file_text(b"ts,close\n").unwrap(), "ts,close\n");
    let why = file_text(b"ts,close\n\xff\n").unwrap_err();
    assert!(why.contains("not UTF-8") && why.contains("byte 9"), "{why}");
    assert_eq!(file_head(b"ts,close\n").unwrap(), ("ts,close\n", true));
    // A two-byte character split by the cut: the head ends before it.
    let mut big = vec![b'a'; UPLOAD_HEAD_BYTES - 1];
    big.extend_from_slice("é tail".as_bytes());
    let (head, whole) = file_head(&big).unwrap();
    assert!(!whole);
    assert_eq!(head.len(), UPLOAD_HEAD_BYTES - 1);
    let mut bad = vec![b'a'; 10];
    bad.push(0xff);
    bad.extend(vec![b'a'; UPLOAD_HEAD_BYTES]);
    assert!(file_head(&bad).unwrap_err().contains("byte 10"));
}

#[test]
fn the_chunks_tile_the_text_on_character_boundaries() {
    assert_eq!(chunks("").count(), 0);
    assert_eq!(chunks("abc").collect::<Vec<_>>(), ["abc"]);
    let text = format!("{}é{}", "a".repeat(UPLOAD_CHUNK_BYTES - 1), "b".repeat(10));
    let pieces: Vec<&str> = chunks(&text).collect();
    assert_eq!(pieces.len(), 2);
    assert_eq!(pieces[0].len(), UPLOAD_CHUNK_BYTES - 1, "the split character goes whole");
    assert_eq!(pieces.concat(), text);
    assert!(pieces.iter().all(|p| p.len() <= UPLOAD_CHUNK_BYTES));
}
