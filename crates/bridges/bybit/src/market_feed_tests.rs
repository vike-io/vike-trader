use super::{
    Category, FeedCtx, Feeds, KlineEvent, MarkEvent, PUBLIC_WS_INVERSE, PUBLIC_WS_LINEAR,
    PUBLIC_WS_SPOT, mark_subscribe_frame, parse_trades, perp_split, relabel_series, route_frame,
    route_mark_frame, subscribe_frame, trades_subscribe_frame, ws_host,
};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use vike_bridge_core::SubscribeAck;
use vike_bridge_core::depth::BookOp;
use vike_data::DataClient;
use vike_model::BookLevel;

// The shared capturing sink (testing-arch Phase 4d) — replaces the inline formatted-string
// RecordingSink copy that used to live here (same canonical `calls()` line forms).
use vike_data::RecordingSink;

/// A network-free stand-in for [`super::feed_main`]: just polls its own stop flag, same
/// cadence as the real feed's WS loop, without ever touching a socket.
fn fake_feed_body(_symbol: String, _interval: String, ctx: FeedCtx) {
    while !ctx.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn subscribe_returns_distinct_ids() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let id1 = feeds.spawn_with("BTCUSDT", "1", fake_feed_body).expect("spawn ok");
    let id2 = feeds.spawn_with("ETHUSDT", "1", fake_feed_body).expect("spawn ok");
    assert_ne!(id1, id2, "distinct ids per subscribe");
    feeds.shutdown();
}

#[test]
fn unsubscribe_stops_only_that_one_feed_others_keep_running() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let id1 = feeds.spawn_with("BTCUSDT", "1", fake_feed_body).expect("spawn ok");
    let id2 = feeds.spawn_with("ETHUSDT", "1", fake_feed_body).expect("spawn ok");

    feeds.unsubscribe(id1);
    assert_eq!(feeds.registry.len(), 1, "only the unsubscribed stream is removed");
    assert!(feeds.registry.contains(id2), "the other subscription keeps running");

    feeds.shutdown();
    assert!(feeds.registry.is_empty());
}

#[test]
fn shutdown_joins_every_feed() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    feeds.spawn_with("BTCUSDT", "1", fake_feed_body).expect("spawn ok");
    feeds.spawn_with("ETHUSDT", "1", fake_feed_body).expect("spawn ok");
    feeds.shutdown();
    assert!(feeds.registry.is_empty());
}

/// Mark-slot semantics (W2-T4): a linear `tickers` snapshot carries the venue's REAL
/// `markPrice` → `MarkEvent::Mark` with the exact decimal bits + the frame's top-level `ts`;
/// a delta that didn't move the mark (no `markPrice` field), a dead mark, the subscribe
/// ack/reject envelopes, and junk are classified like the kline router's br7 shape.
#[test]
fn tickers_mark_frame_classification() {
    let snap = serde_json::json!({
        "topic": "tickers.BTCUSDT",
        "type": "snapshot",
        "cs": 24_987_956_059_i64,
        "ts": 1_673_272_861_686_i64,
        "data": {
            "symbol": "BTCUSDT",
            "markPrice": "16596.00",
            "indexPrice": "16598.54",
            "lastPrice": "16597.00"
        }
    })
    .to_string();
    match route_mark_frame(&snap) {
        MarkEvent::Mark { px, ts } => {
            assert_eq!(px.to_bits(), 16596.00_f64.to_bits(), "markPrice, not lastPrice");
            assert_eq!(ts, 1_673_272_861_686);
        }
        other => panic!("expected Mark, got {other:?}"),
    }
    // a field-sparse delta with no mark move is dropped, not zero-priced
    let delta = r#"{"topic":"tickers.BTCUSDT","type":"delta","ts":1,"data":{"symbol":"BTCUSDT","lastPrice":"16597.50"}}"#;
    assert_eq!(route_mark_frame(delta), MarkEvent::Ignored);
    // a dead (zero) mark is dropped
    let dead = r#"{"topic":"tickers.BTCUSDT","type":"delta","ts":1,"data":{"markPrice":"0"}}"#;
    assert_eq!(route_mark_frame(dead), MarkEvent::Ignored);
    // br7 envelopes: ack confirms, reject is attributable
    assert_eq!(
        route_mark_frame(r#"{"success":true,"op":"subscribe","conn_id":"x"}"#),
        MarkEvent::Ack
    );
    match route_mark_frame(
        r#"{"success":false,"op":"subscribe","ret_msg":"Invalid symbol :tickers.NOPE"}"#,
    ) {
        MarkEvent::Error(m) => assert!(m.contains("Invalid symbol")),
        other => panic!("expected Error, got {other:?}"),
    }
    assert_eq!(route_mark_frame("ping"), MarkEvent::Ignored);
    assert_eq!(route_mark_frame(r#"{"topic":"kline.1.BTCUSDT","data":[]}"#), MarkEvent::Ignored);
}

#[test]
fn mark_subscribe_frame_targets_the_tickers_topic() {
    let f = mark_subscribe_frame("BTCUSDT");
    let v: serde_json::Value = serde_json::from_str(&f).unwrap();
    assert_eq!(v["op"], "subscribe");
    assert_eq!(v["args"][0], "tickers.BTCUSDT");
}

/// The pairing PREDICATE (mark-slot semantics, W2-T4): only a `.P` perp pairs a mark stream,
/// and only while the knob is on. Spot has no venue mark price at all.
#[test]
fn only_perps_pair_a_mark_stream_and_only_while_the_knob_is_on() {
    assert!(super::should_pair_mark("BTCUSDT.P", true));
    assert!(!super::should_pair_mark("BTCUSDT", true), "spot has no mark price");
    assert!(!super::should_pair_mark("BTCUSDT.P", false), "a `mark_streams = 0` row suppresses");
}

/// SPAWN side, network-free: `pair_mark_stream` is the production path `try_spawn` calls (only
/// `body` differs here). A perp spawns a companion `tickers` mark stream; unsubscribing the
/// bars id stops BOTH, while unrelated subscriptions keep running.
#[test]
fn a_perp_bars_subscription_spawns_and_then_stops_its_mark_stream() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let bars_id = feeds.spawn_with("BTCUSDT.P", "1", fake_feed_body).expect("spawn ok");
    let other_id = feeds.spawn_with("ETHUSDT", "1", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_id, "BTCUSDT.P", fake_feed_body);
    assert_eq!(feeds.registry.len(), 3, "bars + mark + the unrelated feed");

    feeds.unsubscribe(bars_id);
    assert_eq!(feeds.registry.len(), 1, "bars AND mark stopped together");
    assert!(feeds.registry.contains(other_id), "unrelated subscriptions keep running");
    assert!(feeds.mark_pairings.is_empty(), "the pairing is consumed");
    feeds.shutdown();
}

/// **The COUNT half of the daemon's CEX reconcile-health row condition** —
/// `crates/vike-tradehub/src/feeds.rs`'s `cex_status_row_is_unambiguous` holds the RULE, and
/// this holds the measurement it is fed. Network-free, over the same `fake_feed_body` the
/// pairing tests above use.
///
/// ⚠ Without it the two halves could drift silently: the daemon would keep asking "is this
/// handle single-writer" and a bridge change could start answering wrong, on the one venue the
/// 42-hour suppression actually happened to.
#[test]
fn a_perp_bars_subscription_reports_two_status_writer_lanes() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    // Set explicitly rather than inherited from the ambient knob, the mirror of
    // `the_mark_streams_knob_off_suppresses_the_perp_spawn`'s idiom: the daemon's question is
    // "did this mount spawn a second writer", and the answer must not depend on the test
    // runner's environment.
    feeds.mark_streams = true;
    assert_eq!(feeds.status_writer_lanes(), 0, "nothing spawned yet");

    let spot_id = feeds.spawn_with("BTCUSDT", "1", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(spot_id, "BTCUSDT", fake_feed_body);
    assert_eq!(
        feeds.status_writer_lanes(),
        1,
        "the DEPLOYED shape — spot, one interval — is a single writer, which is why the CI box's \
             latch was total and permanent rather than thrashing"
    );

    let perp_id = feeds.spawn_with("ETHUSDT.P", "1", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(perp_id, "ETHUSDT.P", fake_feed_body);
    assert_eq!(
        feeds.status_writer_lanes(),
        3,
        "a `.P` symbol pairs a mark lane onto the SAME status mutex — the daemon must see \
             that and withhold the row"
    );
    feeds.shutdown();
}

#[test]
fn a_spot_bars_subscription_spawns_no_mark_stream() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let bars_id = feeds.spawn_with("BTCUSDT", "1", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_id, "BTCUSDT", fake_feed_body);
    assert_eq!(feeds.registry.len(), 1, "no companion stream for spot");
    feeds.shutdown();
}

/// A `mark_streams = 0` row suppresses the spawn even for a perp — the knob's whole job, pinned.
#[test]
fn the_mark_streams_knob_off_suppresses_the_perp_spawn() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    feeds.mark_streams = false;
    let bars_id = feeds.spawn_with("BTCUSDT.P", "1", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_id, "BTCUSDT.P", fake_feed_body);
    assert_eq!(feeds.registry.len(), 1, "knob off -> no mark stream even for a perp");
    feeds.shutdown();
}

/// The default this feed runs and the one `config show` prints are ONE fact: the charter constant
/// is `venue.bybit.mark_streams`'s declared default.
#[test]
fn the_charter_default_is_the_declared_fields() {
    let declared = vike_model::venues::venue_fields::venue_field("bybit", "mark_streams")
        .expect("declared")
        .default;
    assert_eq!(super::MARK_STREAM_DEFAULT_ON, declared == "1", "declared {declared:?}");
}

/// Decision 0095: the `mark_streams` row decides, and no row keeps the charter default (ON here).
#[test]
fn the_mark_streams_row_decides_and_no_row_keeps_the_charter_default() {
    let fresh = || Feeds::new(Arc::new(RecordingSink::default()), || {});
    assert!(fresh().mark_streams, "charter default: ON");
    assert!(!fresh().with_mark_streams(Some("0")).mark_streams);
    assert!(fresh().with_mark_streams(Some("1")).mark_streams);
    assert!(fresh().with_mark_streams(None).mark_streams);
}

/// Per-symbol dedupe: a 1-min AND a 5-min chart on the same perp share ONE mark socket.
#[test]
fn two_intervals_on_one_perp_share_a_single_mark_stream() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let bars_1 = feeds.spawn_with("BTCUSDT.P", "1", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_1, "BTCUSDT.P", fake_feed_body);
    let bars_5 = feeds.spawn_with("BTCUSDT.P", "5", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_5, "BTCUSDT.P", fake_feed_body);
    assert_eq!(feeds.registry.len(), 3, "two bars feeds but only ONE mark stream");

    feeds.unsubscribe(bars_1);
    assert_eq!(feeds.registry.len(), 2, "the mark stream the 5-min chart still needs stays up");
    feeds.unsubscribe(bars_5);
    assert!(feeds.registry.is_empty(), "the last release stops the mark stream too");
    feeds.shutdown();
}

#[test]
fn quotes_and_book_are_unsupported() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    assert!(matches!(
        feeds.subscribe_quotes("BTCUSDT"),
        Err(vike_data::LiveDataError::Unsupported(_))
    ));
    assert!(matches!(
        feeds.subscribe_book("BTCUSDT"),
        Err(vike_data::LiveDataError::Unsupported(_))
    ));
    // subscribe_trades is deliberately NOT asserted here — it is now wired to a real live feed
    // (mirrors subscribe_bars/subscribe_depth, which also aren't exercised via a real subscribe
    // in unit tests since that would touch the network); its pure parser (`parse_trades`) and
    // subscribe-frame builder are covered below instead.
}

/// A confirmed Bybit kline frame → a CLOSED bar with the exact o/h/l/c/v bit patterns + ts.
#[test]
fn confirmed_kline_classifies_closed_with_exact_bits() {
    let frame = serde_json::json!({
        "topic": "kline.1.BTCUSDT",
        "type": "snapshot",
        "ts": 1_700_000_060_100_i64,
        "data": [{
            "start": 1_700_000_060_000_i64,
            "end": 1_700_000_119_999_i64,
            "interval": "1",
            "open": "27010.25",
            "close": "27080.10",
            "high": "27100.00",
            "low": "27000.00",
            "volume": "8.10000000",
            "turnover": "219000.00",
            "confirm": true,
            "timestamp": 1_700_000_060_100_i64
        }]
    })
    .to_string();
    match route_frame(&frame) {
        KlineEvent::Closed(b) => {
            assert_eq!(b.ts, 1_700_000_060_000);
            assert_eq!(b.open.to_bits(), 27010.25_f64.to_bits());
            assert_eq!(b.high.to_bits(), 27100.00_f64.to_bits());
            assert_eq!(b.low.to_bits(), 27000.00_f64.to_bits());
            assert_eq!(b.close.to_bits(), 27080.10_f64.to_bits());
            assert_eq!(b.volume.to_bits(), 8.1_f64.to_bits());
            assert!(b.symbol.is_none() && b.funding.is_none() && b.bid.is_none());
        }
        other => panic!("expected Closed, got {other:?}"),
    }
}

/// A `confirm=false` frame is a still-forming bar (the `forming_bar` sink lane), not a close.
#[test]
fn unconfirmed_kline_classifies_forming() {
    let frame = serde_json::json!({
        "topic": "kline.1.BTCUSDT",
        "data": [{
            "start": 1_700_000_120_000_i64,
            "open": "27080.10", "close": "27085.00", "high": "27090.00",
            "low": "27080.00", "volume": "1.5", "confirm": false
        }]
    })
    .to_string();
    match route_frame(&frame) {
        KlineEvent::Forming(b) => {
            assert_eq!(b.ts, 1_700_000_120_000);
            assert_eq!(b.close.to_bits(), 27085.00_f64.to_bits());
            assert_eq!(b.open.to_bits(), 27080.10_f64.to_bits());
        }
        other => panic!("expected Forming, got {other:?}"),
    }
}

/// Net-hardening br7: a Bybit subscribe ACK (`{op:"subscribe", success:true}`) now CONFIRMS the
/// handshake (`KlineEvent::Ack`) instead of being silently dropped — via `FrameOutcome::Confirm`
/// it disarms the shared driver's subscribe-ack watchdog. A pong-op reply, bare-text `ping`, and
/// a non-kline topic stay Ignored.
#[test]
fn subscribe_ack_is_classified_as_ack_pong_and_junk_ignored() {
    // subscribe success ack → Ack (confirms the subscribe)
    assert_eq!(route_frame(r#"{"success":true,"op":"subscribe","conn_id":"x"}"#), KlineEvent::Ack);
    // a keepalive pong reply (op != subscribe) is ignored
    assert_eq!(
        route_frame(r#"{"success":true,"op":"pong","conn_id":"x","ret_msg":"pong"}"#),
        KlineEvent::Ignored
    );
    // not JSON at all
    assert_eq!(route_frame("ping"), KlineEvent::Ignored);
    // a non-kline topic (well-formed, empty data) is ignored on the topic prefix
    assert_eq!(route_frame(r#"{"topic":"tickers.BTCUSDT","data":[]}"#), KlineEvent::Ignored);
}

/// Net-hardening br7: a Bybit subscribe REJECT (`{op:"subscribe", success:false}`) is now an
/// ATTRIBUTABLE `KlineEvent::Error` carrying the venue's `ret_msg`, NOT silently dropped.
/// `feed_main` maps it to `FrameOutcome::Fatal`, the session error the driver discloses.
#[test]
fn error_frame_is_attributable_not_ignored() {
    match route_frame(
        r#"{"success":false,"op":"subscribe","ret_msg":"Invalid symbol :kline.1.NOPE","conn_id":"x"}"#,
    ) {
        KlineEvent::Error(m) => {
            assert!(m.contains("Invalid symbol"), "carries the Bybit ret_msg: {m}");
        }
        other => panic!("expected an attributable Error, got {other:?}"),
    }
}

/// Net-hardening br7 (scripted-clock, deterministic): a subscribe that never acks and never
/// delivers data trips the `SubscribeAck` watchdog once this venue's declared ack window (the
/// `MarketPumpSpec` row `subscribe_only_pump_opts` consumes) elapses — the attributable "no ack/data" error
/// the shared driver returns. An ack within the window disarms it.
#[test]
fn a_missing_ack_within_the_window_becomes_an_attributable_error() {
    let sub_ack_timeout = vike_bridge_core::pump_spec::market_pump_spec(super::VENUE)
        .knobs()
        .ack_timeout
        .expect("bybit declares the br7 ack watchdog");
    let timeout_ms = sub_ack_timeout.as_millis() as i64;
    let armed_at = 1_000; // scripted "subscribe sent" wall-clock ms
    let ack = SubscribeAck::new(armed_at, timeout_ms);
    assert!(!ack.overdue(armed_at + timeout_ms), "at the deadline, not yet overdue (strict >)");
    assert!(
        ack.overdue(armed_at + timeout_ms + 1),
        "one ms past the {}s window with no ack/data → attributable error",
        sub_ack_timeout.as_secs()
    );
    // the ACK path (route_frame → Ack → confirm) disarms it: no trip however far the clock runs
    let mut acked = SubscribeAck::new(armed_at, timeout_ms);
    assert_eq!(route_frame(r#"{"success":true,"op":"subscribe","conn_id":"x"}"#), KlineEvent::Ack);
    acked.confirm();
    assert!(!acked.overdue(armed_at + timeout_ms * 100), "a confirmed subscribe never trips");
}

#[test]
fn subscribe_frame_targets_the_kline_topic() {
    let f = subscribe_frame("60", "BTCUSDT");
    assert!(f.contains("\"kline.60.BTCUSDT\""), "topic wrong: {f}");
    assert!(f.contains("\"op\":\"subscribe\""));
}

// ---- Bybit `publicTrade` channel: parse_trades ----

#[test]
fn trades_subscribe_frame_targets_the_public_trade_topic() {
    let f = trades_subscribe_frame("BTCUSDT");
    assert!(f.contains("\"publicTrade.BTCUSDT\""), "topic wrong: {f}");
    assert!(f.contains("\"op\":\"subscribe\""));
}

/// A `publicTrade` push with a buy row and a sell row → 2 `TradeTick`s with exact price/size/ts
/// bit patterns, and `is_buyer_maker` following Binance's `m` convention (true exactly when the
/// buyer was the MAKER, i.e. the taker/aggressor SOLD): `S=="Buy"` → false, `S=="Sell"` → true.
/// `symbol` is read off the wire `s` field.
#[test]
fn parse_trades_maps_buy_and_sell_rows_with_exact_bits() {
    let payload = serde_json::json!({
        "topic": "publicTrade.BTCUSDT",
        "type": "snapshot",
        "ts": 1_710_000_000_050_i64,
        "data": [
            {
                "T": 1_710_000_000_000_i64,
                "s": "BTCUSDT",
                "S": "Buy",
                "v": "0.012",
                "p": "64500.1",
                "L": "PlusTick",
                "i": "130639474",
                "BT": false
            },
            {
                "T": 1_710_000_000_123_i64,
                "s": "BTCUSDT",
                "S": "Sell",
                "v": "0.5",
                "p": "64499.9",
                "L": "MinusTick",
                "i": "130639475",
                "BT": false
            }
        ]
    });
    let ticks = parse_trades(&payload);
    assert_eq!(ticks.len(), 2);

    assert_eq!(ticks[0].ts, 1_710_000_000_000);
    assert_eq!(ticks[0].price.to_bits(), 64500.1_f64.to_bits());
    assert_eq!(ticks[0].size.to_bits(), 0.012_f64.to_bits());
    assert!(!ticks[0].is_buyer_maker, "Buy taker → buyer is NOT the maker");
    assert_eq!(ticks[0].symbol, "BTCUSDT");
    assert_eq!(ticks[0].local_ts, 0, "the pure mapper leaves local_ts unstamped");

    assert_eq!(ticks[1].ts, 1_710_000_000_123);
    assert_eq!(ticks[1].price.to_bits(), 64499.9_f64.to_bits());
    assert_eq!(ticks[1].size.to_bits(), 0.5_f64.to_bits());
    assert!(ticks[1].is_buyer_maker, "Sell taker → buyer WAS the maker");
}

/// Non-`publicTrade` pushes (a kline data push, and a subscribe ack) yield an empty vec —
/// `parse_trades` must not misclassify another topic's frame.
#[test]
fn parse_trades_ignores_non_trades_pushes() {
    let kline_push = serde_json::json!({
        "topic": "kline.1.BTCUSDT",
        "data": [{"start": 1_700_000_060_000_i64, "open": "1", "close": "1", "high": "1",
                  "low": "1", "volume": "1", "confirm": true}]
    });
    assert!(parse_trades(&kline_push).is_empty());

    let ack = serde_json::json!({"success": true, "op": "subscribe", "conn_id": "x"});
    assert!(parse_trades(&ack).is_empty());

    assert!(parse_trades(&serde_json::json!({})).is_empty());
}

/// A malformed/unparseable row (bad price, zero size, unrecognized side, missing field) is
/// skipped in place — never panics, never produces a bogus tick.
#[test]
fn parse_trades_skips_malformed_rows_without_panicking() {
    let payload = serde_json::json!({
        "topic": "publicTrade.BTCUSDT",
        "data": [
            {"T": 1_i64, "s": "BTCUSDT", "S": "Buy", "v": "0.012", "p": "not-a-number"},
            {"T": 1_i64, "s": "BTCUSDT", "S": "Buy", "v": "0", "p": "1.0"},
            {"T": 1_i64, "s": "BTCUSDT", "S": "unknown", "v": "1.0", "p": "1.0"},
            {"s": "BTCUSDT", "S": "Buy", "v": "1.0", "p": "1.0"},
            {"T": 1_i64, "S": "Buy", "v": "1.0", "p": "1.0"},
            "not even an object"
        ]
    });
    assert!(parse_trades(&payload).is_empty());
}

/// One bad row alongside a good one: the batch is NOT failed wholesale (per-element tolerance,
/// mirrors okx's `parse_trades`).
#[test]
fn parse_trades_keeps_good_rows_alongside_a_bad_one() {
    let payload = serde_json::json!({
        "topic": "publicTrade.BTCUSDT",
        "data": [
            {"T": 1_i64, "s": "BTCUSDT", "S": "Buy", "v": "1.0", "p": "bad"},
            {"T": 2_i64, "s": "BTCUSDT", "S": "Buy", "v": "1.0", "p": "100.0"}
        ]
    });
    let ticks = parse_trades(&payload);
    assert_eq!(ticks.len(), 1);
    assert_eq!(ticks[0].ts, 2);
}

// ---- DOM depth decode: fold_depth_frame → BookOp (the shared-driver gap wiring) ----

/// The depth decode over the shared driver: the first `snapshot` seeds the slot (`Updated`),
/// an in-sequence delta folds (`Updated`), and a GAPPED delta (dropped frame — `u` jumps past
/// `last_seq + 1`) maps `MdEvent::Resync` → `BookOp::Gap`, on which the driver reconnects and
/// re-seeds immediately (previously `_ => Ignored` swallowed the corruption until the timed
/// reseed). The gapped delta must not have folded.
#[test]
fn depth_decode_maps_a_seq_gap_to_book_op_gap() {
    let frame = |typ: &str, u: u64, bid_qty: &str| {
        serde_json::json!({
                "topic": "orderbook.200.BTCUSDT", "type": typ, "ts": 1_700_000_000_000_i64,
                "data": {"s":"BTCUSDT","b":[["60000.0",bid_qty],["59999.9","2"]],"a":[["60000.1","4"]],"u":u,"seq":u}
            })
            .to_string()
    };
    let mut slot = None;
    // a delta before the first snapshot is ignored (nothing to fold into)
    assert_eq!(
        super::fold_depth_frame(&frame("delta", 11, "1"), "BTCUSDT", &mut slot),
        BookOp::Ignored
    );
    assert!(slot.is_none());
    // the first snapshot seeds the book
    assert!(matches!(
        super::fold_depth_frame(&frame("snapshot", 10, "5"), "BTCUSDT", &mut slot),
        BookOp::Updated(_)
    ));
    // an in-sequence delta (u = last + 1) folds
    assert!(matches!(
        super::fold_depth_frame(&frame("delta", 11, "8"), "BTCUSDT", &mut slot),
        BookOp::Updated(_)
    ));
    // u jumps 11 → 13: a dropped frame → Gap (and the delta did NOT fold)
    assert_eq!(
        super::fold_depth_frame(&frame("delta", 13, "99"), "BTCUSDT", &mut slot),
        BookOp::Gap
    );
    let book = slot.as_ref().expect("book still present for the driver to discard");
    // best-bid price rides the INFERRED tick grid (float-subtraction tick ⇒ not bit-exact);
    // the qty is exact and is what proves the gapped delta (qty "99") did not fold.
    let BookLevel { price: px, qty } = book.best_bid().expect("seeded book has a bid");
    assert!((px - 60000.0).abs() < 1e-3, "best bid price off-grid: {px}");
    assert_eq!(qty, 8.0, "the gapped delta must not fold");
    // a venue-restart regression (u drops far below last) is a Gap too
    assert_eq!(
        super::fold_depth_frame(&frame("delta", 2, "77"), "BTCUSDT", &mut slot),
        BookOp::Gap
    );
}

// ---- Bybit `publicTrade` spot-vs-linear perp split (mirrors the kline feed) ----

/// The `.P` split: a perp symbol strips to the plain exchange symbol (`is_perp = true`); a spot
/// symbol passes through unchanged (`is_perp = false`) — the predicate [`should_pair_mark`] still
/// wants, and the one this file used to mistake for a HOST.
#[test]
fn perp_split_strips_dot_p_for_the_exchange_symbol() {
    assert_eq!(perp_split("BTCUSDT.P"), ("BTCUSDT".to_string(), true));
    assert_eq!(perp_split("BTCUSDT"), ("BTCUSDT".to_string(), false));
    // only a trailing `.P` triggers it (a bare `P` in the name does not)
    assert_eq!(perp_split("1000PEPEUSDT"), ("1000PEPEUSDT".to_string(), false));
}

/// **The third socket, and the three hosts pinned distinct.** MEASURED 2026-09-16: bybit's
/// public WS is STRICT per host in both directions — an inverse topic on the linear socket
/// answers `error:handler not found`, and a linear topic on the inverse socket does the same —
/// so a wrong answer here is a reconnect loop (kline/trades/mark) or a book that never seeds
/// (depth). See [`PUBLIC_WS_INVERSE`]'s table for every probe.
///
/// ⚠ This is a mutation target: re-pinning [`ws_host`]'s `Inverse` arm to [`PUBLIC_WS_LINEAR`]
/// reddens here, and so does collapsing any two of the three.
#[test]
fn every_category_has_its_own_public_socket() {
    assert_eq!(ws_host(Category::Spot), PUBLIC_WS_SPOT);
    assert_eq!(ws_host(Category::Linear), PUBLIC_WS_LINEAR);
    assert_eq!(ws_host(Category::Inverse), PUBLIC_WS_INVERSE);
    assert_eq!(PUBLIC_WS_INVERSE, "wss://stream.bybit.com/v5/public/inverse");

    let hosts = [PUBLIC_WS_SPOT, PUBLIC_WS_LINEAR, PUBLIC_WS_INVERSE];
    for (i, a) in hosts.iter().enumerate() {
        for b in &hosts[i + 1..] {
            assert_ne!(a, b, "two categories must never share a socket");
        }
    }
}

/// The host decision and the REST category decision are ONE value, which is what stops a lane's
/// warmup seed and its stream reaching different books. Asserted as the pairing rather than as
/// two independent facts, because two independent facts is exactly what this crate had.
#[test]
fn the_host_and_the_rest_category_come_from_the_same_value() {
    for (book, host_tail, category) in [
        (Category::Spot, "/spot", "spot"),
        (Category::Linear, "/linear", "linear"),
        (Category::Inverse, "/inverse", "inverse"),
    ] {
        assert!(ws_host(book).ends_with(host_tail), "{book:?}: {}", ws_host(book));
        assert_eq!(book.wire(), category, "{book:?}");
    }
}

/// (a) The PERP subscribe topic uses the `.P`-STRIPPED exchange symbol: input `BTCUSDT.P` →
/// `publicTrade.BTCUSDT` (never `publicTrade.BTCUSDT.P`), because the topic is built from the
/// `api_symbol` half of `perp_split`.
#[test]
fn perp_subscribe_topic_strips_dot_p() {
    let (api_symbol, is_perp) = perp_split("BTCUSDT.P");
    assert!(is_perp);
    let f = trades_subscribe_frame(&api_symbol);
    assert!(f.contains("\"publicTrade.BTCUSDT\""), "perp topic must strip .P: {f}");
    assert!(!f.contains(".P"), "the .P suffix must never reach the exchange topic: {f}");
}

/// (b) For a PERP, the emitted `TradeTick.symbol` is the `.P` SERIES label even though the linear
/// wire `s` is the plain `BTCUSDT` — the `relabel_series` overwrite that keeps the tape's
/// `TradeStore` key distinct from the spot twin (the store keys on `tick.symbol`, not the sink arg).
#[test]
fn perp_emitted_tick_symbol_is_the_dot_p_series_label() {
    // A linear-perp wire push: `s` is the plain exchange symbol, no `.P`.
    let payload = serde_json::json!({
        "topic": "publicTrade.BTCUSDT",
        "type": "snapshot",
        "data": [
            {"T": 1_i64, "s": "BTCUSDT", "S": "Buy", "v": "0.5", "p": "64500.0"},
            {"T": 2_i64, "s": "BTCUSDT", "S": "Sell", "v": "0.25", "p": "64499.0"}
        ]
    });
    let parsed = parse_trades(&payload);
    assert_eq!(parsed.len(), 2);
    assert!(parsed.iter().all(|t| t.symbol == "BTCUSDT"), "wire s is the plain symbol");

    let relabeled = relabel_series(parsed, "BTCUSDT.P");
    assert!(
        relabeled.iter().all(|t| t.symbol == "BTCUSDT.P"),
        "every emitted perp tick carries the .P series key"
    );
    // the re-label touches ONLY `symbol` — price/size/ts/side are untouched
    assert_eq!(relabeled[0].price.to_bits(), 64500.0_f64.to_bits());
    assert!(!relabeled[0].is_buyer_maker && relabeled[1].is_buyer_maker);
}

/// (c) SPOT is byte-identical: `series_symbol == wire s`, so `relabel_series` is a no-op overwrite
/// of the same string — the emitted `TradeTick.symbol` still equals the wire `s`.
#[test]
fn spot_relabel_is_a_no_op_byte_identical() {
    let payload = serde_json::json!({
        "topic": "publicTrade.BTCUSDT",
        "data": [{"T": 1_i64, "s": "BTCUSDT", "S": "Buy", "v": "1.0", "p": "100.0"}]
    });
    let parsed = parse_trades(&payload);
    let before = parsed[0].clone();
    let relabeled = relabel_series(parsed, "BTCUSDT"); // spot: series == wire s
    assert_eq!(relabeled[0].symbol, "BTCUSDT");
    // no field changed at all
    assert_eq!(relabeled[0].symbol, before.symbol);
    assert_eq!(relabeled[0].price.to_bits(), before.price.to_bits());
    assert_eq!(relabeled[0].size.to_bits(), before.size.to_bits());
    assert_eq!(relabeled[0].ts, before.ts);
}
