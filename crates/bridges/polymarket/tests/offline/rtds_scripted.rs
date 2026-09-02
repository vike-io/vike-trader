//! Scripted (network-free) coverage of `rtds::run_rtds_session` — the RTDS underlying
//! reference-price session the opt-in `RtdsFeed` runs under the shared reconnect driver. A canned
//! frame queue drives it directly: no socket, no thread spawn, deterministic ordering.
//!
//! The frames below are the SHAPES the RTDS protocol carries (host/topics/keepalive live-VERIFIED
//! 2026-07-22, see the `rtds` module doc) — a subscribe of
//! `{"action":"subscribe","subscriptions":[{"topic":"crypto_prices","type":"update"}]}`, then a
//! snapshot (`data` array of `{timestamp, value}`) followed by live updates. These tests pin the
//! PARSING and the session wiring; the live probe pins the endpoint.

use std::sync::atomic::AtomicBool;

use vike_polymarket::{run_rtds_session, RtdsConfig};

// The shared scripted MarketStream double (testing-arch Phase 4c, vike-bridge-core's
// `test-support` dev-feature) — the same seam `market_feed_scripted.rs` drives the CLOB pump with.
// Exhausting the script surfaces as a stream-closed error, so `run_rtds_session` returning `Err`
// at the end of a script is expected, not a failure.
use vike_bridge_core::scripted::ScriptedStream;

// The shared capturing sink (testing-arch Phase 4d).
use vike_data::RecordingSink;

const BTC: &str = "btc";

fn cfg() -> RtdsConfig {
    RtdsConfig::crypto_prices(BTC)
}

/// The server's first message: a snapshot carrying a `data` array of `{timestamp, value}` entries.
/// `timestamp` is epoch-SECONDS here (the shape the decoder normalizes to ms).
fn snapshot_frame() -> serde_json::Value {
    serde_json::json!({
        "topic": "crypto_prices",
        "type": "update",
        "data": [
            {"symbol": "btc", "timestamp": 1_700_000_000_i64, "value": 64123.5},
            {"symbol": "eth", "timestamp": 1_700_000_000_i64, "value": 3200.25}
        ]
    })
}

/// A live update: one entry, epoch-MILLISECONDS, under `payload`.
fn update_frame(symbol: &str, value: f64, ts_ms: i64) -> serde_json::Value {
    serde_json::json!({
        "topic": "crypto_prices",
        "type": "update",
        "payload": {"symbol": symbol, "timestamp": ts_ms, "value": value}
    })
}

#[test]
fn a_snapshot_then_updates_publish_the_exact_mark_tick_sequence() {
    let mut stream = ScriptedStream::from_json([
        snapshot_frame(),
        update_frame("btc", 64200.0, 1_700_000_001_000),
        update_frame("eth", 3201.5, 1_700_000_002_000),
    ]);
    let sink = RecordingSink::default();
    let stop = AtomicBool::new(false);
    let cfg = cfg();

    let result = run_rtds_session(&mut stream, &sink, &cfg, &stop);
    assert!(result.is_err(), "the script runs out of frames — that surfaces as a stream close");

    assert_eq!(
        stream.sent(),
        vec![cfg.subscribe_frame()],
        "the subscribe frame goes out exactly once, before the first read; the verified `PING` \
         keepalive is on a 5 s wall-clock cadence, so a script this short sends none"
    );
    assert_eq!(
        sink.calls(),
        vec![
            // snapshot: one mark_tick per entry, epoch-seconds normalized to ms
            "mark_tick(polymarket,btc,64123.5,1700000000000)".to_string(),
            "mark_tick(polymarket,eth,3200.25,1700000000000)".to_string(),
            // live updates, in order
            "mark_tick(polymarket,btc,64200,1700000001000)".to_string(),
            "mark_tick(polymarket,eth,3201.5,1700000002000)".to_string(),
        ],
        "RTDS lands on the reference/mark lane — never on quote/trade/book"
    );
}

#[test]
fn junk_and_control_frames_never_end_the_session_or_emit_anything() {
    let mut stream = ScriptedStream::from_json([
        serde_json::json!({"action": "subscribe", "status": "ok"}), // an ack-shaped control frame
        serde_json::json!({"topic": "crypto_prices", "data": []}),  // an empty payload
    ]);
    stream.push_text("not json at all"); // malformed — skipped as data, session continues
    stream.push_text(&update_frame("btc", 1.5, 1_700_000_003_000).to_string());

    let sink = RecordingSink::default();
    let stop = AtomicBool::new(false);

    let result = run_rtds_session(&mut stream, &sink, &cfg(), &stop);
    assert!(result.is_err(), "the session ends on the exhausted script, not on the junk frame");
    assert_eq!(
        sink.calls(),
        vec!["mark_tick(polymarket,btc,1.5,1700000003000)".to_string()],
        "only the real update emitted; every non-data frame was tolerated"
    );
}

/// The live-verified FILTERED session: the empty text frame RTDS sends right after connect, then
/// the `type:"subscribe"` SNAPSHOT (its `data` array nested inside `payload`), then live updates
/// whose STRING `full_accuracy_value` is preferred over the lossy f64 `value`.
#[test]
fn a_filtered_session_folds_the_empty_frame_the_snapshot_and_the_live_updates() {
    let cfg = cfg().with_symbol_filter("btcusdt");
    // the ONLY working filter form — the CSV form the docs show returns nothing, forever
    assert!(
        cfg.subscribe_frame().contains(r#"{\"symbol\":\"btcusdt\"}"#),
        "the subscribe carries the JSON-object filter string: {}",
        cfg.subscribe_frame()
    );

    let mut stream = ScriptedStream::from_steps(Vec::new());
    stream.push_text(""); // the empty frame that arrives immediately after connect
    stream.push_text(
        &serde_json::json!({
            "connection_id": "abc-123",
            "topic": "crypto_prices",
            "type": "subscribe",
            "payload": {
                "symbol": "btcusdt",
                "data": [{"timestamp": 1_784_735_163_000_i64, "value": 66037.0}]
            }
        })
        .to_string(),
    );
    stream.push_text(
        &serde_json::json!({
            "connection_id": "abc-123",
            "topic": "crypto_prices",
            "type": "update",
            "payload": {
                "full_accuracy_value": "66038.47123456",
                "symbol": "btcusdt",
                "timestamp": 1_784_735_164_000_i64,
                "value": 66038.47
            },
            "timestamp": 1_784_735_164_208_i64
        })
        .to_string(),
    );

    let sink = RecordingSink::default();
    let stop = AtomicBool::new(false);
    let result = run_rtds_session(&mut stream, &sink, &cfg, &stop);
    assert!(result.is_err(), "the script runs out of frames — that surfaces as a stream close");
    assert_eq!(
        sink.calls(),
        vec![
            "mark_tick(polymarket,btcusdt,66037,1784735163000)".to_string(),
            "mark_tick(polymarket,btcusdt,66038.47123456,1784735164000)".to_string(),
        ],
        "the empty frame emitted nothing; both payload arms landed on the mark lane"
    );
}

#[test]
fn a_frame_naming_no_symbol_falls_back_to_the_configured_default() {
    let mut stream = ScriptedStream::from_json([serde_json::json!({
        "topic": "crypto_prices",
        "data": [{"timestamp": 1_700_000_004_000_i64, "value": 42.0}]
    })]);
    let sink = RecordingSink::default();
    let stop = AtomicBool::new(false);

    let _ = run_rtds_session(&mut stream, &sink, &cfg(), &stop);
    assert_eq!(sink.calls(), vec!["mark_tick(polymarket,btc,42,1700000004000)".to_string()]);
}

#[test]
fn a_raised_stop_ends_the_session_cleanly_without_reading_the_rest() {
    let stop = AtomicBool::new(true); // already stopped before the first read
    let mut stream = ScriptedStream::from_json([snapshot_frame()]);
    let sink = RecordingSink::default();

    let cfg = cfg();
    let result = run_rtds_session(&mut stream, &sink, &cfg, &stop);
    assert_eq!(result, Ok(()), "a requested stop is a clean end, not an error");
    assert!(sink.calls().is_empty(), "no frame was consumed after the stop");
    assert_eq!(
        stream.sent(),
        vec![cfg.subscribe_frame()],
        "the subscribe still goes out first — the stop is polled at the top of the read loop"
    );
}
