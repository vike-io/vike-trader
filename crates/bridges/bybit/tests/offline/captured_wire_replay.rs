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

use vike_bridge_core::capture_replay::{
    assert_accept_frames_decode_to_order_accepted, assert_no_sensitive_leaves,
    assert_present_with_provenance, fixture,
};
use vike_bybit::event_mapper::map_bybit_perp;
use vike_model::events::Event;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/captured")
}

fn decode(frame: &serde_json::Value) -> Vec<Event> {
    map_bybit_perp(frame, "bybit", "BTCUSDT")
}

/// Every required kind is present, stamped with honest provenance (demo venue, sanitizer id),
/// and non-empty.
#[test]
fn captured_fixtures_present_with_provenance() {
    assert_present_with_provenance(
        &fixtures_dir(),
        "bybit",
        &["ws_accepted", "ws_fill", "ws_canceled"],
    );
}

/// Real accept frames (order topic `orderStatus=New`) decode to `OrderAccepted` with a coid
/// (`orderLinkId`) and a venue order id (the V5 UUID).
#[test]
fn captured_accept_frames_decode_to_order_accepted() {
    assert_accept_frames_decode_to_order_accepted(&fixtures_dir(), decode);
}

/// Real fill frames (execution topic `execType=Trade`, cum==orderQty) dual-publish per row —
/// pairs of `[Fill, OrderFilled]` with live economics and the `execId` reconnect-dedup key.
#[test]
fn captured_fill_frames_dual_publish_with_real_economics() {
    for (i, frame) in fixture(&fixtures_dir(), "ws_fill").frames.iter().enumerate() {
        let events = decode(frame);
        assert!(!events.is_empty(), "fill frame {i} decoded to nothing: {frame}");
        assert_eq!(events.len() % 2, 0, "fill frame {i}: dual-publish pairs, got {events:?}");
        for (j, pair) in events.chunks(2).enumerate() {
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
            // FEES-AUDIT 2026-10-26 §4.12: this replay never read the fee. The real row states
            // `execFee` (a CHARGE, unsigned, in `feeCurrency`) beside `execValue` and `feeRate`; the
            // ledger must hold exactly the wire figure, and it must be of the table row's magnitude:
            // within [½ x maker, 1.5 x taker] of the notional (`fee_schedule_for("bybit")`: 2/5.5 bps).
            let row = &frame["data"][j];
            let num = |key: &str| -> f64 { row[key].as_str().unwrap().parse().unwrap() };
            assert!(f.commission > 0.0, "fill frame {i}: a charge, not a rebate: {}", f.commission);
            assert_eq!(f.commission, num("execFee"), "fill frame {i}: the wire execFee, unsigned");
            assert_eq!(f.commission_asset.as_str(), row["feeCurrency"].as_str().unwrap());
            assert!(
                (f.commission - num("execValue") * num("feeRate")).abs() < 1e-7,
                "fill frame {i}: execFee = execValue x feeRate to the venue's rounding"
            );
            let (maker, taker) = vike_venues::fee_schedule_for("bybit").maker_taker_rates();
            let notional = num("execValue");
            assert!(
                f.commission >= 0.5 * maker * notional && f.commission <= 1.5 * taker * notional,
                "fill frame {i}: fee {} is outside the table row's band for {notional}",
                f.commission
            );
        }
    }
}

/// Real cancel frames (order topic `orderStatus=Cancelled`) decode to `OrderCanceled` with the
/// venue's `cancelType` carried as the reason.
#[test]
fn captured_cancel_frames_decode_to_order_canceled() {
    for (i, frame) in fixture(&fixtures_dir(), "ws_canceled").frames.iter().enumerate() {
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
    assert_no_sensitive_leaves(&fixtures_dir());
}
