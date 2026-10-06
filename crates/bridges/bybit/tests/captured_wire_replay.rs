//! CI'd replay of the COMMITTED sanitized real-capture fixtures (testing-arch plan §5, PR 5b):
//! decodes `tests/fixtures/captured/<kind>.json` — real bybit V5 linear-perp private-WS frames,
//! captured from the demo venue by `bybit_capture_smoke.rs`'s opt-in capture arm and sanitized by
//! `vike_bridge_core::capture` — through the REAL `map_bybit_perp`, asserting the expected events.
//! Unlike the hand-authored r6/conformance frames (which pin mapper MATH over an assumed shape),
//! these pin WIRE-FORMAT fidelity: a venue field rename/retype/re-wrap surfaces here as a
//! re-capture diff + a failing decode. What sets them apart is that the bytes are the VENUE's own
//! rather than a frame anybody wrote down — this clause used to say "with no Python oracle
//! involved", which no longer distinguishes anything: nothing in the tree consults a live Python
//! implementation (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`), and the
//! r6 frames it was contrasting with are frozen bytes too.
//!
//! NORMAL tests — no `#[ignore]`, no network, no creds. A missing committed fixture is a FAILURE
//! (a deleted gate must not pass silently); refresh via the capture smoke (see its module doc).

use std::path::PathBuf;

use vike_bridge_core::capture::{CapturedFixture, REDACT_KEYS, SANITIZER_VERSION, load_captured};
use vike_bybit::event_mapper::map_bybit_perp;
use vike_model::events::Event;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/captured")
}

fn fixture(kind: &str) -> CapturedFixture {
    load_captured(&fixtures_dir(), kind)
        .unwrap_or_else(|| panic!("committed captured fixture missing: captured/{kind}.json"))
}

fn decode(frame: &serde_json::Value) -> Vec<Event> {
    map_bybit_perp(frame, "bybit", "BTCUSDT")
}

/// Every required kind is present, stamped with honest provenance (demo venue, sanitizer id),
/// and non-empty.
#[test]
fn captured_fixtures_present_with_provenance() {
    for kind in ["ws_accepted", "ws_fill", "ws_canceled"] {
        let fx = fixture(kind);
        let p = &fx.provenance;
        assert_eq!(p["venue"], "bybit", "{kind}: provenance venue");
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

/// Real accept frames (order topic `orderStatus=New`) decode to `OrderAccepted` with a coid
/// (`orderLinkId`) and a venue order id (the V5 UUID).
#[test]
fn captured_accept_frames_decode_to_order_accepted() {
    for (i, frame) in fixture("ws_accepted").frames.iter().enumerate() {
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

/// Real fill frames (execution topic `execType=Trade`, cum==orderQty) dual-publish per row —
/// pairs of `[Fill, OrderFilled]` with live economics and the `execId` reconnect-dedup key.
#[test]
fn captured_fill_frames_dual_publish_with_real_economics() {
    for (i, frame) in fixture("ws_fill").frames.iter().enumerate() {
        let events = decode(frame);
        assert!(!events.is_empty(), "fill frame {i} decoded to nothing: {frame}");
        assert_eq!(events.len() % 2, 0, "fill frame {i}: dual-publish pairs, got {events:?}");
        for pair in events.chunks(2) {
            let Event::Fill(f) = &pair[0] else { panic!("fill frame {i} even slot: {pair:?}") };
            let Event::OrderFilled(w) = &pair[1] else {
                panic!("fill frame {i} odd slot must be the terminal wrap: {pair:?}")
            };
            assert!(f.last_qty > 0.0, "fill frame {i}: qty");
            assert!(f.last_px > 0.0, "fill frame {i}: px");
            assert!(!f.trade_id.as_str().is_empty(), "fill frame {i}: execId trade id");
            assert!(!f.client_order_id.is_empty(), "fill frame {i}: coid");
            assert_eq!(f.symbol.as_str(), "BTCUSDT", "fill frame {i}: symbol from the row");
            assert_eq!(w.fill.trade_id, f.trade_id, "fill frame {i}: wrap carries the same fill");
            assert_eq!(w.client_order_id, f.client_order_id, "fill frame {i}: wrap coid");
        }
    }
}

/// Real cancel frames (order topic `orderStatus=Cancelled`) decode to `OrderCanceled` with the
/// venue's `cancelType` carried as the reason.
#[test]
fn captured_cancel_frames_decode_to_order_canceled() {
    for (i, frame) in fixture("ws_canceled").frames.iter().enumerate() {
        let events = decode(frame);
        assert!(!events.is_empty(), "cancel frame {i} decoded to nothing: {frame}");
        for ev in &events {
            let Event::OrderCanceled(c) = ev else {
                panic!("cancel frame {i} decoded a non-cancel event: {ev:?}")
            };
            assert!(!c.client_order_id.is_empty(), "cancel frame {i}: coid");
            assert!(!c.reason.as_str().is_empty(), "cancel frame {i}: cancelType reason");
        }
    }
}

/// The committed fixtures stay SANITIZED: any leaf under a known-sensitive key must hold the
/// stable placeholder (string `[redacted:*]` or number 0) — a hand-edit that re-introduces a
/// secret, or a sanitizer regression at capture time, trips here.
#[test]
fn committed_fixtures_carry_no_sensitive_leaves() {
    fn walk(v: &serde_json::Value, fail: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, child) in map {
                    let lc = k.to_ascii_lowercase();
                    if REDACT_KEYS.contains(&lc.as_str()) {
                        let ok = match child {
                            serde_json::Value::String(s) => s.starts_with("[redacted:"),
                            serde_json::Value::Number(n) => n.as_f64() == Some(0.0),
                            serde_json::Value::Bool(_) | serde_json::Value::Null => true,
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
            serde_json::Value::Array(items) => items.iter().for_each(|c| walk(c, fail)),
            _ => {}
        }
    }
    // EVERY committed capture file — including optional kinds (ws_account_state, ws_fill_partial)
    // beyond the three required ones — must be sanitized.
    let mut scanned = 0usize;
    for entry in std::fs::read_dir(fixtures_dir()).expect("captured dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let kind = path.file_stem().unwrap().to_string_lossy().to_string();
        let mut fail = Vec::new();
        for frame in &fixture(&kind).frames {
            walk(frame, &mut fail);
        }
        assert!(fail.is_empty(), "{kind}: unsanitized sensitive leaves: {fail:?}");
        scanned += 1;
    }
    assert!(scanned >= 3, "expected at least the three required kinds, scanned {scanned}");
}
