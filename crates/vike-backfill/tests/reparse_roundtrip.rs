//! End-to-end proof the raw tap round-trips: capture a scripted Polymarket session's raw frames to
//! real gz files (live pump + TappedStream + RecorderSink → store A), then re-parse those gz files
//! (GzFileStream + LocalTsRewriteSink + RecorderSink → store B), and assert store B reproduces store
//! A's NORMALIZED data bit-identically. `local_ts` is deliberately EXCLUDED from the A==B comparison:
//! the live pump stamps emission wall-clock ms, the re-parse restores the captured receive time
//! (ns→ms converted) — two different clocks by design (the local_ts REWRITE itself is proven in
//! reparse.rs's unit test).

#![cfg(feature = "poly-reparse")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64};

use vike_backfill::reparse::{GzFileStream, LocalTsRewriteSink, RecvNsCell, gz_files_for};
use vike_bridge_core::StreamHealth;
use vike_data::{DataFusionHist, HistStore, LiveDataSink, RecorderConfig, RecorderSink, TsRange};
use vike_polymarket::raw_tap::{RawCaptureConfig, RawTap, TappedStream};
// The shared scripted MarketStream double (testing-arch Phase 4c, vike-bridge-core
// `test-support` feature) — replaces the inline `Canned` copy that used to live here (queued
// JSON frame strings, then Closed).
use vike_bridge_core::scripted::ScriptedStream;
use vike_polymarket::{MarketStream, PumpMode, TokenState, Watchdog, run_session};

const TOKEN: &str = "111";
const TICK: f64 = 1.0; // whole-number prices quantize exactly (mirrors market_feed_scripted.rs)

/// Build the scripted market frames: one book snapshot, two price_change deltas. Wire shape mirrors
/// `crates/bridges/polymarket/tests/offline/market_feed_scripted.rs`'s `book_frame`/`price_change_frame`
/// helpers (asset_id = TOKEN, prices as strings, side "BUY"/"SELL"), PLUS an explicit top-level
/// `"timestamp"` string on EVERY frame. That timestamp is load-bearing: `ws.rs::frame_ts` reads it
/// for the derived tick `ts`, falling back to `now_ms()` when absent — so without it the live
/// capture and the re-parse would each stamp a DIFFERENT wall-clock `ts` and the `a.ts == b.ts`
/// round-trip assertion would be non-deterministic. With it, both sides decode the identical `ts`.
/// Each delta moves the top so it emits a change-gated quote, giving the round-trip real quote AND
/// book_update rows to compare. Only 3 frames — well under `ANCHOR_EVERY_DELTAS` (512), so no
/// synthetic anchor fires (that path is covered by the pump's own scripted tests).
fn scripted_frames() -> std::collections::VecDeque<String> {
    let mut frames = std::collections::VecDeque::new();
    // 1) book snapshot — top (bid 51 / ask 55).
    frames.push_back(
        serde_json::json!({
            "event_type": "book",
            "asset_id": TOKEN,
            "timestamp": "1700000000000",
            "bids": [{"price": "51", "size": "100"}],
            "asks": [{"price": "55", "size": "40"}]
        })
        .to_string(),
    );
    // 2) price_change delta — ask improves to 54 (top moves → book_update + quote).
    frames.push_back(
        serde_json::json!({
            "event_type": "price_change",
            "timestamp": "1700000000001",
            "price_changes": [
                {"asset_id": TOKEN, "price": "54", "size": "90", "side": "SELL"}
            ]
        })
        .to_string(),
    );
    // 3) price_change delta — bid improves to 52 (top moves → book_update + quote).
    frames.push_back(
        serde_json::json!({
            "event_type": "price_change",
            "timestamp": "1700000000002",
            "price_changes": [
                {"asset_id": TOKEN, "price": "52", "size": "80", "side": "BUY"}
            ]
        })
        .to_string(),
    );
    frames
}

/// Minimal `run_session` driver (no reconnect): `PumpMode::Book`, a benign fresh watchdog, stop=false.
/// Generic over the concrete stream (a `TappedStream` on the live side, a `GzFileStream` on replay).
fn drive_pump<S: MarketStream>(stream: &mut S, sink: Arc<dyn LiveDataSink>) {
    let mut state = TokenState::new(TICK);
    let mut health = StreamHealth::new(60_000);
    let stop = AtomicBool::new(false);
    let mut wd = Watchdog::new(&mut health, "book");
    // run_session returns Err(Closed) at end-of-frames — expected clean termination, not a failure.
    let _ = run_session(
        stream,
        sink.as_ref(),
        TOKEN,
        PumpMode::Book,
        &mut state,
        &stop,
        &mut wd,
        || {},
    );
}

#[test]
fn capture_then_reparse_reproduces_normalized_ticks_bit_identically() {
    // ---- LIVE side: pump the scripted frames through a TappedStream → real gz + RecorderSink (A)
    let raw_dir = tempfile::tempdir().unwrap();
    let store_a_dir = tempfile::tempdir().unwrap();
    let store_a = Arc::new(DataFusionHist::open(store_a_dir.path()).unwrap());
    let (rec_a, rec_a_handle) =
        RecorderSink::spawn(store_a.clone(), RecorderConfig::default()).unwrap();
    let (tap, tap_owner) =
        RawTap::spawn(RawCaptureConfig { dir: raw_dir.path().to_path_buf(), channel_cap: 1024 })
            .unwrap();

    let canned = ScriptedStream::from_texts(scripted_frames());
    let mut tapped = TappedStream::new(canned, Some(tap), TOKEN.to_string());
    drive_pump(&mut tapped, rec_a.clone());
    tap_owner.shutdown(); // flush gz
    rec_a_handle.shutdown(); // flush store A

    // ---- RE-PARSE side: GzFileStream → LocalTsRewriteSink → RecorderSink (B)
    let store_b_dir = tempfile::tempdir().unwrap();
    let store_b = Arc::new(DataFusionHist::open(store_b_dir.path()).unwrap());
    let (rec_b, rec_b_handle) =
        RecorderSink::spawn(store_b.clone(), RecorderConfig::default()).unwrap();
    let cell: RecvNsCell = Arc::new(AtomicI64::new(0));
    let rewrite: Arc<dyn LiveDataSink> =
        Arc::new(LocalTsRewriteSink::new(rec_b.clone(), cell.clone()));
    // find the captured files (all dates: 1970-01-01..9999-12-31 spans any real UTC day)
    let files = gz_files_for(raw_dir.path(), TOKEN, "1970-01-01", "9999-12-31");
    assert!(!files.is_empty(), "capture wrote at least one gz file");
    let mut gz = GzFileStream::new(files, cell);
    drive_pump(&mut gz, rewrite);
    rec_b_handle.shutdown();

    // ---- assert A and B match on all NORMALIZED fields except local_ts
    let qa = store_a.scan_quotes("polymarket", TOKEN, TsRange::all()).unwrap();
    let qb = store_b.scan_quotes("polymarket", TOKEN, TsRange::all()).unwrap();
    assert_eq!(qa.len(), qb.len(), "same quote count");
    assert!(!qa.is_empty(), "the scripted session produced quotes to compare");
    for (a, b) in qa.iter().zip(&qb) {
        assert_eq!(a.ts, b.ts);
        assert_eq!(a.bid.to_bits(), b.bid.to_bits());
        assert_eq!(a.ask.to_bits(), b.ask.to_bits());
        assert_eq!(a.bid_size.to_bits(), b.bid_size.to_bits());
        assert_eq!(a.ask_size.to_bits(), b.ask_size.to_bits());
        // local_ts intentionally NOT compared (different clocks by design)
    }
    let ba = store_a.scan_book_updates("polymarket", TOKEN, TsRange::all()).unwrap();
    let bb = store_b.scan_book_updates("polymarket", TOKEN, TsRange::all()).unwrap();
    assert_eq!(ba.len(), bb.len(), "same book_update count");
    assert!(!ba.is_empty(), "the scripted session produced book_updates to compare");
    for (a, b) in ba.iter().zip(&bb) {
        assert_eq!(a.ts, b.ts);
        assert_eq!(a.seq, b.seq);
        assert_eq!(a.kind, b.kind);
        assert_eq!(a.tick_size.to_bits(), b.tick_size.to_bits());
        assert_eq!(a.bids.len(), b.bids.len());
        assert_eq!(a.asks.len(), b.asks.len());
        for (x, y) in a.bids.iter().zip(&b.bids) {
            assert_eq!(x.0.to_bits(), y.0.to_bits());
            assert_eq!(x.1.to_bits(), y.1.to_bits());
        }
        for (x, y) in a.asks.iter().zip(&b.asks) {
            assert_eq!(x.0.to_bits(), y.0.to_bits());
            assert_eq!(x.1.to_bits(), y.1.to_bits());
        }
    }
    // and B's local_ts is the restored receive time (> 0), not a re-parse-time stamp of 0
    assert!(qb.iter().all(|q| q.local_ts > 0), "re-parse restored receive time as local_ts");
    assert!(
        bb.iter().all(|u| u.local_ts > 0),
        "re-parse restored receive time on book_updates too"
    );
    // ms-scale sanity: local_ts is contractually epoch-MILLISECONDS (vike-model bar.rs/orderbook.rs
    // docs). The tap captures epoch-NANOSECONDS; a regression that forgets the ns→ms divide would
    // still pass the `> 0` checks above (today's real ns wall-clock is positive) but would produce a
    // value ~1e6x too large — bound it below year-2100 in ms so that class of regression can't hide.
    const YEAR_2100_MS: i64 = 4_102_444_800_000;
    assert!(
        qb.iter().all(|q| q.local_ts < YEAR_2100_MS),
        "quote local_ts is ms-scale, not ns-scale"
    );
    assert!(
        bb.iter().all(|u| u.local_ts < YEAR_2100_MS),
        "book_update local_ts is ms-scale, not ns-scale"
    );
}
