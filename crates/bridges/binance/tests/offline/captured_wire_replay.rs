//! CI'd replay of the COMMITTED sanitized real-capture fixtures (testing-arch plan §5, PR 5b):
//! decodes `tests/fixtures/captured/<kind>.json` — real binance spot WS-API user-data frames,
//! captured from the demo venue by `binance_capture_smoke.rs`'s opt-in capture arm and sanitized
//! by `vike_bridge_core::capture` — through the REAL `map_binance_private`, asserting the expected
//! events. Unlike the hand-authored r6/conformance frames (which pin mapper MATH over an assumed
//! shape), these pin WIRE-FORMAT fidelity: a venue field rename/retype/re-wrap surfaces here as a
//! re-capture diff + a failing decode. What sets them apart is that the bytes are the VENUE's own
//! rather than a frame anybody wrote down — this clause used to say "with no Python oracle
//! involved", which no longer distinguishes anything: nothing in the tree consults a live Python
//! implementation (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`), and the
//! r6 frames it was contrasting with are frozen bytes too.
//!
//! NORMAL tests — no `#[ignore]`, no network, no creds. A missing committed fixture is a FAILURE
//! (a deleted gate must not pass silently); refresh via the capture smoke (see its module doc).

use std::path::PathBuf;

use vike_binance::event_mapper::map_binance_private;
use vike_bridge_core::capture_replay::{
    assert_accept_frames_decode_to_order_accepted, assert_no_sensitive_leaves,
    assert_present_with_provenance, fixture,
};
use vike_model::events::Event;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/captured")
}

fn decode(frame: &serde_json::Value) -> Vec<Event> {
    map_binance_private(frame, "binance", "BTCUSDT")
}

/// Every required kind is present, stamped with honest provenance (demo venue, sanitizer id),
/// and non-empty.
#[test]
fn captured_fixtures_present_with_provenance() {
    assert_present_with_provenance(
        &fixtures_dir(),
        "binance",
        &["ws_accepted", "ws_fill", "ws_canceled"],
    );
}

/// Real accept frames (`x=NEW`) decode to `OrderAccepted` with a coid and a venue order id.
#[test]
fn captured_accept_frames_decode_to_order_accepted() {
    assert_accept_frames_decode_to_order_accepted(&fixtures_dir(), decode);
}

/// Real fill frames (`x=TRADE, X=FILLED`) dual-publish exactly `[Fill, OrderFilled]` with live
/// economics — qty/px positive, a venue trade id (the reconnect-dedup key), matching wrap.
#[test]
fn captured_fill_frames_dual_publish_with_real_economics() {
    for (i, frame) in fixture(&fixtures_dir(), "ws_fill").frames.iter().enumerate() {
        let events = decode(frame);
        assert_eq!(events.len(), 2, "fill frame {i} must dual-publish, got {events:?}");
        let Event::Fill(f) = &events[0] else { panic!("fill frame {i} slot 0: {events:?}") };
        let Event::OrderFilled(w) = &events[1] else { panic!("fill frame {i} slot 1: {events:?}") };
        assert!(f.last_qty > 0.0, "fill frame {i}: qty");
        assert!(f.last_px > 0.0, "fill frame {i}: px");
        assert!(!f.trade_id.as_str().is_empty(), "fill frame {i}: venue trade id");
        assert!(!f.client_order_id.is_empty(), "fill frame {i}: coid");
        assert_eq!(f.symbol.as_str(), "BTCUSDT", "fill frame {i}: symbol from the frame's own s");
        assert_eq!(w.fill.trade_id, f.trade_id, "fill frame {i}: wrap carries the same fill");
        assert_eq!(w.client_order_id, f.client_order_id, "fill frame {i}: wrap coid");
        // FEES-AUDIT 2026-10-26 §4.12: this replay never read the fee. The real frame's fee is `n` in
        // asset `N` (a BUY pays the BASE coin, a SELL the quote). The ledger must hold a CHARGE in QUOTE
        // units, and of the table row's magnitude: within [½ x maker, 1.5 x taker] of the notional,
        // `fee_schedule_for("binance")` being the spot lane (10/10 bps).
        let event = &frame["event"];
        let wire_fee: f64 = event["n"].as_str().unwrap().parse().unwrap();
        let wire_asset = event["N"].as_str().unwrap();
        assert!(wire_fee > 0.0, "fill frame {i}: the captured frame states a fee");
        assert_eq!(f.commission_asset.as_str(), "USDT", "fill frame {i}: booked in the quote");
        let expect = if wire_asset == "BTC" { wire_fee * f.last_px } else { wire_fee };
        assert!(
            (f.commission - expect).abs() < 1e-12,
            "fill frame {i}: {} vs {expect}",
            f.commission
        );
        let (maker, taker) = vike_venues::fee_schedule_for("binance").maker_taker_rates();
        let notional = f.last_qty * f.last_px;
        assert!(
            f.commission >= 0.5 * maker * notional && f.commission <= 1.5 * taker * notional,
            "fill frame {i}: fee {} is not 10 bps of {notional} (table row)",
            f.commission
        );
    }
}

/// Real cancel frames (`x=CANCELED`) decode to `OrderCanceled` carrying the RESTING order's coid.
///
/// Live-capture regression (#604): on a CANCELED executionReport Binance puts the resting order's
/// coid in `C` (origClientOrderId) and the cancel *request's* own auto-generated id in `c`. The
/// OMS correlates the cancel back to the resting order by THIS coid, so decoding it from `c` would
/// surface an id nothing local can match. Assert the decoded coid is the frame's `C`, NOT its `c`.
#[test]
fn captured_cancel_frames_decode_to_order_canceled() {
    for (i, frame) in fixture(&fixtures_dir(), "ws_canceled").frames.iter().enumerate() {
        let events = decode(frame);
        assert!(!events.is_empty(), "cancel frame {i} decoded to nothing: {frame}");
        // The resting coid the OMS must see (origClientOrderId), and the throwaway cancel-request
        // id it must NOT — both present and distinct in the real captured frame.
        let orig = frame["event"]["C"].as_str().unwrap_or_default();
        let cancel_req = frame["event"]["c"].as_str().unwrap_or_default();
        assert!(!orig.is_empty(), "cancel frame {i}: fixture must carry `C` (origClientOrderId)");
        assert_ne!(orig, cancel_req, "cancel frame {i}: `C`/`c` must differ to prove the fix");
        for ev in &events {
            let Event::OrderCanceled(c) = ev else {
                panic!("cancel frame {i} decoded a non-cancel event: {ev:?}")
            };
            assert_eq!(
                c.client_order_id, orig,
                "cancel frame {i}: coid must be the RESTING order's `C`, not the cancel-request `c`"
            );
            assert_ne!(
                c.client_order_id, cancel_req,
                "cancel frame {i}: coid must NOT be the auto-generated cancel-request id `c`"
            );
        }
    }
}

/// The committed fixtures stay SANITIZED: any leaf under a known-sensitive key must hold the
/// stable placeholder (string `[redacted:*]` or number 0) — a hand-edit that re-introduces a
/// secret, or a sanitizer regression at capture time, trips here.
#[test]
fn committed_fixtures_carry_no_sensitive_leaves() {
    assert_no_sensitive_leaves(&fixtures_dir());
}
