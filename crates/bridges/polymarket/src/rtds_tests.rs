use super::*;
use std::sync::atomic::Ordering;

use vike_data::RecordingSink;

const BTC: &str = "btc";

fn cfg() -> RtdsConfig {
    RtdsConfig::crypto_prices(BTC)
}

/// The decoder under the default topic — the shape every test below reads through.
fn decode(frame: &serde_json::Value) -> Vec<RefPrice> {
    decode_ref_prices(frame, TOPIC_CRYPTO_PRICES, BTC)
}

#[test]
fn the_subscribe_frame_is_the_documented_shape() {
    let s = rtds_subscribe_message(&[(TOPIC_CRYPTO_PRICES, RTDS_TYPE_UPDATE)]);
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["action"], "subscribe");
    assert_eq!(v["subscriptions"][0]["topic"], "crypto_prices");
    assert_eq!(v["subscriptions"][0]["type"], "update");
    assert!(
        v["subscriptions"][0].get("filters").is_none(),
        "no filters key at all = the verified all-symbols subscribe"
    );
    assert_eq!(v["subscriptions"].as_array().unwrap().len(), 1);
    // the config builds exactly that frame
    assert_eq!(cfg().subscribe_frame(), s);
}

#[test]
fn multiple_subscriptions_ride_one_frame() {
    let s = rtds_subscribe_message(&[("crypto_prices", "update"), ("equity_prices", "update")]);
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["subscriptions"].as_array().unwrap().len(), 2);
    assert_eq!(v["subscriptions"][1]["topic"], "equity_prices");
}

/// The ONLY working filter form is the JSON-OBJECT string. The documented comma-separated
/// form silently returns nothing forever, so this crate must never emit it.
#[test]
fn a_symbol_filter_is_the_json_object_string_never_csv() {
    assert_eq!(rtds_symbol_filter("btcusdt"), r#"{"symbol":"btcusdt"}"#);

    let frame = cfg().with_symbol_filter("btcusdt").subscribe_frame();
    let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
    let filters = v["subscriptions"][0]["filters"].as_str().expect("filters is a STRING");
    assert_eq!(filters, r#"{"symbol":"btcusdt"}"#);
    assert!(!filters.contains(','), "the CSV form is the silent-failure trap — never emit it");
    // and the filter string is itself parseable JSON carrying the symbol
    let inner: serde_json::Value = serde_json::from_str(filters).unwrap();
    assert_eq!(inner["symbol"], "btcusdt");
}

#[test]
fn epoch_seconds_are_scaled_and_epoch_ms_pass_through() {
    assert_eq!(normalize_ts_ms(1_700_000_000), 1_700_000_000_000); // seconds → ms
    assert_eq!(normalize_ts_ms(1_700_000_000_000), 1_700_000_000_000); // already ms
    assert_eq!(normalize_ts_ms(0), 0); // the absent sentinel
    assert_eq!(normalize_ts_ms(-5), -5); // nonsense passes through untouched
}

#[test]
fn the_snapshot_data_array_decodes_every_entry() {
    let frame = serde_json::json!({
        "topic": "crypto_prices",
        "type": "update",
        "data": [
            {"symbol": "btc", "timestamp": 1_700_000_000_i64, "value": 64123.5},
            {"symbol": "eth", "timestamp": "1700000001000", "value": "3200.25"}
        ]
    });
    assert_eq!(
        decode(&frame),
        vec![
            RefPrice { symbol: "btc".into(), value: 64123.5, ts: 1_700_000_000_000 },
            RefPrice { symbol: "eth".into(), value: 3200.25, ts: 1_700_000_001_000 },
        ],
        "string AND number wire forms both decode; epoch-seconds are normalized"
    );
}

/// The LIVE update envelope, verbatim from the 2026-07-22 probe: unknown keys are ignored and
/// the STRING `full_accuracy_value` wins over the lossy f64 `value`.
#[test]
fn the_live_update_envelope_decodes_and_prefers_full_accuracy_value() {
    let frame = serde_json::json!({
        "connection_id": "abc-123",
        "payload": {
            "full_accuracy_value": "66038.47123456",
            "symbol": "btcusdt",
            "timestamp": 1_784_735_164_000_i64,
            "value": 66038.47
        },
        "timestamp": 1_784_735_164_208_i64,
        "topic": "crypto_prices",
        "type": "update"
    });
    assert_eq!(
        decode(&frame),
        vec![RefPrice { symbol: "btcusdt".into(), value: 66038.47123456, ts: 1_784_735_164_000 }],
        "the entry timestamp (not the envelope's) and the full-accuracy string are used"
    );
}

/// The SNAPSHOT arm of a filtered subscribe: `type:"subscribe"`, and the `data` array sits
/// INSIDE `payload`, one level deeper than the live update's entry object.
#[test]
fn the_filtered_subscribe_snapshot_arm_decodes_its_nested_data_array() {
    let frame = serde_json::json!({
        "connection_id": "abc-123",
        "topic": "crypto_prices",
        "type": "subscribe",
        "payload": {
            "symbol": "btcusdt",
            "data": [
                {"timestamp": 1_784_735_163_000_i64, "value": 66037.0},
                {"timestamp": 1_784_735_164_000_i64, "value": 66038.47}
            ]
        }
    });
    assert_eq!(
        decode(&frame),
        vec![
            RefPrice { symbol: "btcusdt".into(), value: 66037.0, ts: 1_784_735_163_000 },
            RefPrice { symbol: "btcusdt".into(), value: 66038.47, ts: 1_784_735_164_000 },
        ],
        "the payload's own symbol defaults every entry inside its data array"
    );
    assert_eq!(RTDS_TYPE_SUBSCRIBE, "subscribe");
}

#[test]
fn a_single_object_update_decodes_under_data_payload_or_bare() {
    let want = vec![RefPrice { symbol: "btc".into(), value: 64200.0, ts: 1_700_000_002_000 }];
    let entry = serde_json::json!({"timestamp": 1_700_000_002_000_i64, "value": 64200.0});
    for frame in [
        serde_json::json!({"topic": "crypto_prices", "data": entry.clone()}),
        serde_json::json!({"topic": "crypto_prices", "payload": entry.clone()}),
        // bare: allowed because the envelope's own topic matches the subscription
        serde_json::json!({
            "topic": "crypto_prices",
            "timestamp": 1_700_000_002_000_i64,
            "value": 64200.0
        }),
    ] {
        assert_eq!(decode(&frame), want, "frame: {frame}");
    }
}

/// The bare-envelope gate: a control/ack/error frame carrying a top-level `value` must NOT
/// publish a tick unless it is attributable to this stream (matching topic, or its own symbol).
#[test]
fn a_bare_envelope_only_decodes_when_it_is_attributable_to_this_stream() {
    // neither a matching topic nor a symbol → dropped
    let foreign = serde_json::json!({"topic": "equity_prices", "value": 1.0});
    assert!(decode(&foreign).is_empty(), "another topic's frame is not our data");
    let control = serde_json::json!({"type": "error", "message": "nope", "value": 42.0});
    assert!(decode(&control).is_empty(), "a control frame's stray `value` is never a tick");
    // its own symbol IS attribution enough (the top-level-array shape below relies on it)
    let owned = serde_json::json!({"symbol": "btc", "value": 1.0});
    assert_eq!(decode(&owned).len(), 1);
}

#[test]
fn the_symbol_falls_back_entry_then_envelope_then_config() {
    // entry-level wins
    let f = serde_json::json!({"symbol": "eth", "data": [{"symbol": "sol", "value": 1.0}]});
    assert_eq!(decode(&f)[0].symbol, "sol");
    // envelope-level when the entry names none
    let f = serde_json::json!({"symbol": "eth", "data": [{"value": 1.0}]});
    assert_eq!(decode(&f)[0].symbol, "eth");
    // the config default when neither does
    let f = serde_json::json!({"data": [{"value": 1.0}]});
    assert_eq!(decode(&f)[0].symbol, BTC);
}

#[test]
fn unusable_entries_are_dropped_without_dropping_the_frame() {
    let frame = serde_json::json!({"data": [
        {"symbol": "btc", "value": "not-a-number"},
        {"symbol": "eth"},
        {"symbol": "sol", "value": 1.5}
    ]});
    assert_eq!(
        decode(&frame),
        vec![RefPrice { symbol: "sol".into(), value: 1.5, ts: 0 }],
        "only the usable entry survives; ts 0 = the frame carried no stamp"
    );
}

#[test]
fn control_frames_decode_to_nothing() {
    for frame in [
        serde_json::json!({"action": "subscribe", "status": "ok"}),
        serde_json::json!({}),
        serde_json::json!([]),
    ] {
        assert!(decode(&frame).is_empty(), "frame: {frame}");
    }
}

#[test]
fn a_top_level_array_of_envelopes_decodes_each() {
    let frame = serde_json::json!([
        {"symbol": "btc", "value": 1.0, "timestamp": 1_700_000_000_000_i64},
        {"symbol": "eth", "value": 2.0, "timestamp": 1_700_000_000_000_i64}
    ]);
    assert_eq!(decode(&frame).len(), 2);
}

#[test]
fn on_frame_publishes_mark_ticks_and_ignores_junk() {
    let sink = RecordingSink::default();
    let cfg = cfg();
    assert_eq!(on_frame("not json at all", &cfg, &sink), FrameOutcome::Ignore);
    assert_eq!(on_frame(r#"{"action":"subscribe"}"#, &cfg, &sink), FrameOutcome::Ignore);
    // the empty text frame RTDS sends right after connect
    assert_eq!(on_frame("", &cfg, &sink), FrameOutcome::Ignore);
    assert_eq!(on_frame("   ", &cfg, &sink), FrameOutcome::Ignore);
    assert!(sink.calls().is_empty(), "neither junk nor a control frame emits anything");

    let frame = serde_json::json!({"data": [
        {"symbol": "btc", "timestamp": 1_700_000_000_000_i64, "value": 64123.5}
    ]})
    .to_string();
    assert_eq!(on_frame(&frame, &cfg, &sink), FrameOutcome::Confirm);
    assert_eq!(sink.calls(), vec!["mark_tick(polymarket,btc,64123.5,1700000000000)".to_string()]);
}

/// The VERIFIED liveness contract: the literal `PING` text frame every 5 s, plus a
/// conservative silent-stall watchdog so a half-open socket is redialed.
#[test]
fn the_session_arms_the_verified_ping_keepalive_and_an_idle_watchdog() {
    let cfg = cfg();
    assert_eq!(cfg.keepalive_payload, "PING");
    assert_eq!(cfg.keepalive_interval, Some(Duration::from_secs(5)));
    assert_eq!(cfg.idle_threshold, Some(Duration::from_secs(120)));

    let sub = cfg.subscribe_frame();
    let opts = cfg.opts(&sub);
    let ka = opts.keepalive.expect("RTDS requires an app-level keepalive");
    assert_eq!(ka.payload, RTDS_PING);
    assert_eq!(ka.every, RTDS_KEEPALIVE_INTERVAL);
    assert_eq!(opts.idle_threshold, Some(RTDS_IDLE_THRESHOLD));
    assert_eq!(opts.ack_timeout, None, "no ack frame exists to wait for");
}

#[test]
fn the_verified_topics_are_pinned() {
    assert_eq!(TOPIC_CRYPTO_PRICES, "crypto_prices");
    assert_eq!(TOPIC_CRYPTO_PRICES_CHAINLINK, "crypto_prices_chainlink");
    assert_eq!(TOPIC_EQUITY_PRICES, "equity_prices");
    let eq = RtdsConfig::for_topic(TOPIC_EQUITY_PRICES, "aapl");
    assert_eq!(eq.topic, "equity_prices");
    assert_eq!(eq.default_symbol, "aapl");
    // the observed roster is documentation only — six stream live, the docs name four
    assert_eq!(RTDS_CRYPTO_SYMBOLS_OBSERVED.len(), 6);
    assert!(RTDS_CRYPTO_SYMBOLS_OBSERVED.contains(&"dogeusdt"));
}

/// The OFF/default path: constructing the handle dials nothing, spawns nothing, and emits
/// nothing — RTDS only exists once a caller explicitly calls `start`.
#[test]
fn constructing_the_feed_starts_nothing() {
    let sink = Arc::new(RecordingSink::default());
    let feed = RtdsFeed::new(cfg(), Arc::clone(&sink) as Arc<dyn LiveDataSink>);
    assert!(feed.registry.is_empty(), "no thread until start()");
    assert!(sink.calls().is_empty(), "no sink verb fires from an unstarted feed");
}

/// A network-free stand-in for [`rtds_main`]: polls its own stop flag, never touching a socket.
fn fake_body(_cfg: RtdsConfig, _sink: Arc<dyn LiveDataSink>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn start_stop_and_shutdown_are_a_deterministic_lifecycle() {
    let sink = Arc::new(RecordingSink::default()) as Arc<dyn LiveDataSink>;
    let mut feed = RtdsFeed::new(cfg(), sink);
    let a = feed.spawn_with(fake_body).expect("spawn ok");
    let b = feed.spawn_with(fake_body).expect("spawn ok");
    assert_ne!(a, b, "distinct ids per start");
    feed.stop(a);
    assert_eq!(feed.registry.len(), 1, "stopping one leaves the other running");
    assert!(feed.registry.contains(b));
    feed.shutdown();
    assert!(feed.registry.is_empty());
}

// ---- activity/trades lane (Wave 5c) --------------------------------------------------------

/// An `ActivityTradeSink` test double: records every delivered trade for assertion.
#[derive(Default)]
struct RecordingActivitySink {
    trades: std::sync::Mutex<Vec<ActivityTrade>>,
}

impl ActivityTradeSink for RecordingActivitySink {
    fn on_activity_trade(&self, trade: &ActivityTrade) {
        self.trades.lock().unwrap().push(trade.clone());
    }
}

#[test]
fn the_activity_subscribe_frame_is_the_documented_shape() {
    let cfg = RtdsConfig::activity_trades();
    let s = cfg.subscribe_frame();
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["action"], "subscribe");
    assert_eq!(v["subscriptions"][0]["topic"], "activity");
    assert_eq!(v["subscriptions"][0]["type"], "trades");
    assert!(
        v["subscriptions"][0].get("filters").is_none(),
        "no filters key = the platform-wide activity subscribe"
    );
    assert_eq!(v["subscriptions"].as_array().unwrap().len(), 1);
    assert_eq!(TOPIC_ACTIVITY, "activity");
    assert_eq!(RTDS_TYPE_TRADES, "trades");
    // the builder-produced frame is exactly the hand-built one
    assert_eq!(
        s,
        rtds_subscribe_message(&[(TOPIC_ACTIVITY, RTDS_TYPE_TRADES)]),
        "activity_trades() builds the {{\"topic\":\"activity\",\"type\":\"trades\"}} frame"
    );
}

#[test]
fn trade_side_parses_case_insensitively_else_none() {
    assert_eq!(TradeSide::from_wire("BUY"), Some(TradeSide::Buy));
    assert_eq!(TradeSide::from_wire("sell"), Some(TradeSide::Sell));
    assert_eq!(TradeSide::from_wire("Buy"), Some(TradeSide::Buy));
    assert_eq!(TradeSide::from_wire("HODL"), None);
    assert_eq!(TradeSide::from_wire(""), None);
}

/// The LIVE activity/trades envelope shape: a single trade object under `payload`, unknown keys
/// (`outcomeIndex`, `connection_id`) ignored, the epoch-ms stamp passed through.
#[test]
fn a_single_activity_trade_decodes_from_the_payload() {
    let frame = serde_json::json!({
        "connection_id": "abc-123",
        "topic": "activity",
        "type": "trades",
        "timestamp": 1_700_000_000_208_i64,
        "payload": {
            "proxyWallet": "0xWALLET",
            "side": "BUY",
            "size": 100.0,
            "price": 0.62,
            "conditionId": "0xCOND",
            "asset": "123456789",
            "outcome": "Yes",
            "outcomeIndex": 0,
            "transactionHash": "0xDEAD"
        }
    });
    assert_eq!(
        decode_activity_trades(&frame),
        vec![ActivityTrade {
            proxy_wallet: "0xWALLET".into(),
            side: TradeSide::Buy,
            size: 100.0,
            price: 0.62,
            condition_id: "0xCOND".into(),
            asset: "123456789".into(),
            outcome: "Yes".into(),
            tx_hash: "0xDEAD".into(),
            ts: 1_700_000_000_208,
        }]
    );
}

/// A `payload` ARRAY of trades, with one malformed row (non-numeric `size`) tolerantly skipped
/// and the epoch-SECONDS stamps normalized to ms. The batch survives the bad row.
#[test]
fn an_activity_trade_array_skips_a_malformed_row() {
    let frame = serde_json::json!({
        "topic": "activity",
        "type": "trades",
        "payload": [
            {"proxyWallet": "0xA", "side": "BUY", "size": 10.0, "price": 0.5,
             "asset": "111", "timestamp": 1_700_000_000_i64},
            // malformed: size is not a number → this row is dropped, not the batch
            {"proxyWallet": "0xB", "side": "SELL", "size": "not-a-number", "price": 0.4,
             "asset": "222", "timestamp": 1_700_000_000_i64},
            // malformed: missing required proxyWallet → dropped
            {"side": "SELL", "size": 7.0, "price": 0.3, "asset": "444"},
            {"proxyWallet": "0xC", "side": "SELL", "size": 5.0, "price": 0.9,
             "asset": "333", "timestamp": 1_700_000_001_i64}
        ]
    });
    let got = decode_activity_trades(&frame);
    assert_eq!(got.len(), 2, "the two malformed rows are skipped; the two good rows survive");
    assert_eq!(got[0].proxy_wallet, "0xA");
    assert_eq!(got[0].side, TradeSide::Buy);
    assert_eq!(got[0].ts, 1_700_000_000_000, "epoch-seconds normalized to ms");
    assert_eq!(got[1].proxy_wallet, "0xC");
    assert_eq!(got[1].side, TradeSide::Sell);
    assert_eq!(got[1].size, 5.0);
}

#[test]
fn a_control_or_empty_activity_frame_decodes_to_nothing() {
    for frame in [
        serde_json::json!({"type": "error", "message": "nope"}),
        serde_json::json!({"action": "subscribe", "status": "ok"}),
        serde_json::json!({}),
        serde_json::json!([]),
    ] {
        assert!(decode_activity_trades(&frame).is_empty(), "frame: {frame}");
    }
}

#[test]
fn on_activity_frame_delivers_trades_and_ignores_junk() {
    let sink = RecordingActivitySink::default();
    assert_eq!(on_activity_frame("not json", &sink), FrameOutcome::Ignore);
    assert_eq!(on_activity_frame("", &sink), FrameOutcome::Ignore);
    assert_eq!(on_activity_frame("   ", &sink), FrameOutcome::Ignore);
    assert_eq!(on_activity_frame(r#"{"type":"error"}"#, &sink), FrameOutcome::Ignore);
    assert!(sink.trades.lock().unwrap().is_empty(), "junk emits nothing");

    let frame = serde_json::json!({
        "topic": "activity",
        "type": "trades",
        "payload": {"proxyWallet": "0xW", "side": "SELL", "size": 3.0, "price": 0.71,
                    "asset": "999", "timestamp": 1_700_000_000_000_i64}
    })
    .to_string();
    assert_eq!(on_activity_frame(&frame, &sink), FrameOutcome::Confirm);
    let recorded = sink.trades.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].proxy_wallet, "0xW");
    assert_eq!(recorded[0].side, TradeSide::Sell);
    assert_eq!(recorded[0].asset, "999");
}

/// A network-free stand-in for [`activity_main`]: polls its own stop flag, never a socket.
fn fake_activity_body(_cfg: RtdsConfig, _sink: Arc<dyn ActivityTradeSink>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn constructing_the_activity_feed_starts_nothing() {
    let sink = Arc::new(RecordingActivitySink::default());
    let feed = RtdsActivityFeed::new(
        RtdsConfig::activity_trades(),
        Arc::clone(&sink) as Arc<dyn ActivityTradeSink>,
    );
    assert!(feed.registry.is_empty(), "no thread until start()");
    assert!(sink.trades.lock().unwrap().is_empty(), "no trade fires from an unstarted feed");
}

#[test]
fn activity_feed_start_stop_shutdown_is_a_deterministic_lifecycle() {
    let sink = Arc::new(RecordingActivitySink::default()) as Arc<dyn ActivityTradeSink>;
    let mut feed = RtdsActivityFeed::new(RtdsConfig::activity_trades(), sink);
    let a = feed.spawn_with(fake_activity_body).expect("spawn ok");
    let b = feed.spawn_with(fake_activity_body).expect("spawn ok");
    assert_ne!(a, b, "distinct ids per start");
    feed.stop(a);
    assert_eq!(feed.registry.len(), 1, "stopping one leaves the other running");
    assert!(feed.registry.contains(b));
    feed.shutdown();
    assert!(feed.registry.is_empty());
}
