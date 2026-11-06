//! The checks every captured-wire replay suite runs over its COMMITTED sanitized fixtures
//! (`capture` + `test-support`; binance's and bybit's `tests/offline/captured_wire_replay.rs`).
//!
//! The venue keeps what is its own: where its fixtures live (`env!("CARGO_MANIFEST_DIR")` must be
//! expanded in the venue crate), which kinds it requires, and the decode assertions over its real
//! mapper. What is written here once is venue-neutral: loading a committed fixture (a missing one is
//! a FAILURE, so a deleted gate cannot pass silently), the provenance stamp, the scan that keeps
//! every committed file sanitized against `capture::REDACT_KEYS`, and the accept-frame check, whose
//! assertion reads only the canonical event (each venue hands in its own decode).

use std::path::Path;

use serde_json::Value;
use vike_model::events::Event;

use crate::capture::{CapturedFixture, REDACT_KEYS, SANITIZER_VERSION, load_captured};

/// Every frame of the committed `ws_accepted` fixture under `dir` decodes, through the venue's
/// `decode`, to at least one event, each an `OrderAccepted` with a coid and a venue order id.
pub fn assert_accept_frames_decode_to_order_accepted(
    dir: &Path,
    decode: impl Fn(&Value) -> Vec<Event>,
) {
    for (i, frame) in fixture(dir, "ws_accepted").frames.iter().enumerate() {
        let events = decode(frame);
        assert!(!events.is_empty(), "accept frame {i} decoded to nothing: {frame}");
        for ev in &events {
            let Event::OrderAccepted(a) = ev else {
                panic!("accept frame {i} decoded a non-accept event: {ev:?}")
            };
            assert!(!a.client_order_id.is_empty(), "accept frame {i}: coid");
            assert!(
                a.venue_order_id.as_deref().is_some_and(|v| !v.is_empty()),
                "accept frame {i}: venue order id"
            );
        }
    }
}

/// The committed `captured/<kind>.json` under `dir`; a missing one panics.
pub fn fixture(dir: &Path, kind: &str) -> CapturedFixture {
    load_captured(dir, kind)
        .unwrap_or_else(|| panic!("committed captured fixture missing: captured/{kind}.json"))
}

/// Every one of `kinds` is present under `dir`, stamped with honest provenance (`venue`, the demo
/// source, this sanitizer's id, the UTC capture stamp, a frame count that matches), and non-empty.
pub fn assert_present_with_provenance(dir: &Path, venue: &str, kinds: &[&str]) {
    for &kind in kinds {
        let fx = fixture(dir, kind);
        let p = &fx.provenance;
        assert_eq!(p["venue"], venue, "{kind}: provenance venue");
        assert_eq!(p["kind"], kind, "{kind}: provenance kind");
        assert_eq!(p["sanitizer"], SANITIZER_VERSION, "{kind}: sanitizer id");
        let at = p["captured_at_utc"].as_str().unwrap_or("");
        assert!(at.ends_with('Z') && at.len() == 20, "{kind}: captured_at_utc stamp, got {at:?}");
        let source = p["source"].as_str().unwrap_or("");
        assert!(source.contains("DEMO"), "{kind}: source must name the demo venue: {source:?}");
        assert!(!fx.frames.is_empty(), "{kind}: frames[] must be non-empty");
        assert_eq!(p["frame_count"], fx.frames.len() as u64, "{kind}: frame_count matches");
    }
}

/// EVERY committed capture file under `dir` — optional kinds included — stays sanitized: a leaf under
/// a known-sensitive key holds the stable placeholder (string `[redacted:*]`, number 0, a bool or
/// null), and at least the three required kinds were scanned.
pub fn assert_no_sensitive_leaves(dir: &Path) {
    fn walk(v: &Value, fail: &mut Vec<String>) {
        match v {
            Value::Object(map) => {
                for (k, child) in map {
                    let lc = k.to_ascii_lowercase();
                    if REDACT_KEYS.contains(&lc.as_str()) {
                        let ok = match child {
                            Value::String(s) => s.starts_with("[redacted:"),
                            Value::Number(n) => n.as_f64() == Some(0.0),
                            Value::Bool(_) | Value::Null => true,
                            _ => false,
                        };
                        if !ok {
                            fail.push(format!("{k} = {child}"));
                        }
                    } else {
                        walk(child, fail);
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|c| walk(c, fail)),
            _ => {}
        }
    }
    let mut scanned = 0usize;
    for entry in std::fs::read_dir(dir).expect("captured dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let kind = path.file_stem().unwrap().to_string_lossy().to_string();
        let mut fail = Vec::new();
        for frame in &fixture(dir, &kind).frames {
            walk(frame, &mut fail);
        }
        assert!(fail.is_empty(), "{kind}: unsanitized sensitive leaves: {fail:?}");
        scanned += 1;
    }
    assert!(scanned >= 3, "expected at least the three required kinds, scanned {scanned}");
}
