//! Integration coverage for the OANDA LIVE market-data half: the pricing-stream frame decoders
//! (`vike_oanda::market_data`) and the `DataClient` seam they feed
//! (`vike_oanda::market_feed::Feeds`).
//!
//! Every payload below is hand-authored against the v20 `GET /v3/accounts/{id}/pricing/stream`
//! response shape (`Accept-Datetime-Format: UNIX`, so `time` is an epoch-seconds STRING) — the
//! same authored-not-exported convention `tests/candles.rs` states, since the retired Python app
//! exported no fixture for this venue. The pump's own lifecycle (reconnect/backoff,
//! heartbeat→freshness) is unit-tested inside `market_feed.rs` where the private seams live; what
//! is proven HERE is the surface a consumer actually touches.

use std::sync::Arc;

use vike_data::{DataClient, LiveDataError, NoopSink, RecordingSink};
use vike_oanda::market_data::{decode_pricing_frame, parse_forming_candle, PricingFrame};
use vike_oanda::market_feed::Feeds;

/// A wire-faithful two-sided `PRICE` frame with a DEEPER ladder than top-of-book, so the
/// "best-first, `[0]` is the top" contract is actually exercised rather than assumed.
fn price_frame() -> serde_json::Value {
    serde_json::from_str(
        r#"{
            "type": "PRICE", "time": "1478012400.250000000", "instrument": "EUR_USD",
            "bids": [{"price": "1.09000", "liquidity": 10000000},
                     {"price": "1.08995", "liquidity": 20000000},
                     {"price": "1.08990", "liquidity": 50000000}],
            "asks": [{"price": "1.09010", "liquidity": 10000000},
                     {"price": "1.09015", "liquidity": 20000000}],
            "closeoutBid": "1.08980", "closeoutAsk": "1.09030",
            "status": "tradeable", "tradeable": true
        }"#,
    )
    .unwrap()
}

#[test]
fn price_frame_decodes_to_top_of_book_l1() {
    let PricingFrame::Quote(q) = decode_pricing_frame(&price_frame()) else {
        panic!("a two-sided PRICE frame must decode to a quote");
    };
    assert_eq!(q.bid, 1.09, "bids[0] — OANDA orders each ladder best-first");
    assert_eq!(q.ask, 1.0901);
    assert_eq!(q.bid_size, 10_000_000.0);
    assert_eq!(q.ask_size, 10_000_000.0);
    assert_eq!(q.ts, 1_478_012_400_250, "epoch-SECONDS string → epoch ms");
    assert_eq!(q.symbol, "EUR_USD", "the pure decoder carries the VENUE instrument");
    assert_eq!(q.local_ts, 0, "receive time is the pump's to stamp, not the decoder's");
    assert!((q.mid() - 1.09005).abs() < 1e-12);
}

#[test]
fn heartbeat_is_liveness_not_data() {
    let v = serde_json::json!({"type": "HEARTBEAT", "time": "1478012401.000000000"});
    // Distinct from `Other`: the pump treats a heartbeat as transport-alive (it closes a gap and
    // re-judges freshness) while it advances NO data clock — that split is the whole reason this
    // is its own variant rather than a skipped frame.
    assert_eq!(decode_pricing_frame(&v), PricingFrame::Heartbeat { ts: 1_478_012_401_000 });
}

#[test]
fn a_one_sided_or_unpriced_frame_derives_no_quote() {
    // Half a market is not an L1 quote — the same rule the market-data conformance harness pins
    // for the derived-L1 venues ("a one-sided book derives no quote").
    let mut v = price_frame();
    v["bids"] = serde_json::json!([]);
    assert_eq!(decode_pricing_frame(&v), PricingFrame::Other);

    // A non-tradeable halt frame still carries both ladders in practice; what genuinely cannot
    // decode is a ladder whose level has no parseable `price`.
    let v = serde_json::json!({
        "type": "PRICE", "time": "1.0", "instrument": "EUR_USD",
        "bids": [{"liquidity": 100}], "asks": [{"price": "1.09010", "liquidity": 100}]
    });
    assert_eq!(decode_pricing_frame(&v), PricingFrame::Other);
}

#[test]
fn unknown_frame_types_are_skipped_not_guessed_at() {
    for v in [
        serde_json::json!({"type": "SOMETHING_NEW", "time": "1.0"}),
        serde_json::json!({"time": "1.0"}),
        serde_json::json!({}),
    ] {
        assert_eq!(decode_pricing_frame(&v), PricingFrame::Other, "{v}");
    }
}

#[test]
fn forming_candle_complements_the_complete_only_parser() {
    // `parse_candles` (lossless lane) keeps only `complete` candles; `parse_forming_candle`
    // (conflating lane) takes the incomplete tail and nothing else. Together they partition a
    // candles response with no candle counted twice — the property the bar poller relies on.
    let v: serde_json::Value = serde_json::from_str(
        r#"{"candles": [
            {"complete": true,  "volume": 7, "time": "60.0",
             "mid": {"o": "1.1", "h": "1.2", "l": "1.0", "c": "1.15"}},
            {"complete": false, "volume": 3, "time": "120.0",
             "mid": {"o": "1.15", "h": "1.25", "l": "1.14", "c": "1.24"}}
        ]}"#,
    )
    .unwrap();
    let closed = vike_oanda::parse_candles(&v);
    let forming = parse_forming_candle(&v).expect("the incomplete tail is the forming bar");
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].ts, 60_000);
    assert_eq!(forming.ts, 120_000);
    assert_eq!(forming.close, 1.24);
    assert!(closed.iter().all(|b| b.ts != forming.ts), "no candle on both lanes");
}

#[test]
fn an_uncredentialed_feed_refuses_every_verb_and_tears_down_clean() {
    // Absent credentials ARE the live gate (the workspace rule): the feed constructs fine, spawns
    // nothing, and says so — it does NOT read the environment looking for a token.
    let mut feeds = Feeds::new(Arc::new(NoopSink), || {});
    for err in [
        feeds.subscribe_quotes("eurusd").unwrap_err(),
        feeds.subscribe_bars("eurusd", "1m").unwrap_err(),
    ] {
        let LiveDataError::Subscribe(msg) = err else {
            panic!("a missing credential is a Subscribe refusal, not an Unsupported capability");
        };
        assert!(msg.contains("no credentials"), "{msg}");
    }
    // …while the verbs the venue genuinely does not serve refuse from the DECLARED caps row.
    for err in [
        feeds.subscribe_trades("eurusd").unwrap_err(),
        feeds.subscribe_book("eurusd").unwrap_err(),
        feeds.subscribe_depth("eurusd").unwrap_err(),
    ] {
        assert!(matches!(err, LiveDataError::Unsupported(_)), "{err}");
    }
    feeds.begin_shutdown();
    feeds.shutdown(); // nothing was spawned — teardown is a no-op, never a hang
}

#[test]
fn the_declared_caps_row_matches_what_the_feed_serves() {
    // The cross-check that keeps `require_live_verb`'s refusals honest: the row a consumer reads
    // (`vike_model::caps_for`) is exactly the set of verbs `Feeds` implements.
    let caps = vike_model::caps_for("oanda").live_data;
    assert!(caps.bars && caps.quotes, "the split-plane feed serves bars (REST poll) + quotes");
    assert!(!caps.trades, "OANDA publishes no trade tape");
    assert!(!caps.book && !caps.depth, "the pricing ladder is unsequenced top-of-book, not L2");
}

#[test]
fn a_sink_sees_nothing_until_a_subscription_starts() {
    // Guards the fire-and-forget seam contract from the other side: construction alone must not
    // emit, so a consumer wiring the feed at startup cannot be surprised by a phantom series.
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(Arc::clone(&sink) as Arc<dyn vike_data::LiveDataSink>, || {});
    let _ = feeds.subscribe_quotes("eurusd");
    feeds.shutdown();
    assert!(sink.calls().is_empty(), "no credentials → no thread → no sink traffic");
}
