use super::mark::{mark_subscribe_frame, route_mark_frame};
use super::{BybitUrlsFeed, Category, DataClientSpecBybit, perp_split, websocket_host};
use crate::feed::bar::{route_frame, subscribe_frame};
use crate::feed::tick::trades::{parse_trades, relabel_series, trades_subscribe_frame};
use std::assert_matches;
use std::sync::Arc;
use vike_bridge_core::data_client::event::{BarKline, DataClientMark};
use vike_bridge_core::data_client::ws::{DataClientSpec, DataClientWebsocket};
use vike_bridge_core::depth::WebsocketSessionDepthOperation;
use vike_bridge_core::feed_frame::FeedFrame;
use vike_bridge_core::market_feed_pins;
use vike_marketdata::L2BookLevel;
use vike_venues::venue_urls::VenueUrls;

/// The public sockets the seed's `live` `data` `ws` row dials, byte for byte (the constants they replaced).
const PUBLIC_WEBSOCKET_SPOT: &str = "wss://stream.bybit.com/v5/public/spot";
const PUBLIC_WEBSOCKET_LINEAR: &str = "wss://stream.bybit.com/v5/public/linear";
const PUBLIC_WEBSOCKET_INVERSE: &str = "wss://stream.bybit.com/v5/public/inverse";

/// The seed's public bases.
fn seeded_hosts() -> BybitUrlsFeed {
    BybitUrlsFeed::from_urls(&VenueUrls::seed("bybit")).expect("the seed holds bybit's public rows")
}

// The shared capturing sink (testing-arch Phase 4d) — replaces the inline formatted-string
// RecordingSink copy that used to live here (same canonical `calls()` line forms).
use vike_data::RecordingSink;

/// bybit's spec on the release's seed rows.
fn spec() -> DataClientSpecBybit {
    DataClientSpecBybit::new(&VenueUrls::seed("bybit")).expect("the seed holds bybit's public rows")
}

/// The engine over bybit's spec (built, never dialled).
fn fresh() -> DataClientWebsocket<DataClientSpecBybit> {
    DataClientWebsocket::new(spec(), Arc::new(RecordingSink::default()), || {})
}

/// The feed's bases are the seed's `live` rows (the hosts the constants held), and a venue with no rows
/// builds no feed: the refusal reaches the caller.
#[test]
fn the_feeds_are_built_on_the_venues_live_rows_or_not_at_all() {
    let hosts = seeded_hosts();
    assert_eq!(
        (hosts.ws.as_str(), hosts.listings.as_str(), hosts.history.as_str()),
        ("wss://stream.bybit.com", "https://api.bybit.com", "https://api.bybit.com")
    );
    let refused = DataClientSpecBybit::new(&VenueUrls::default());
    assert!(refused.is_err_and(|r| r.to_string().contains("no address row")));
}

/// **The `tickers` channel's funding fields, from frames RECORDED on 2026-11-08** (the public linear and
/// inverse streams, no key): the snapshot states the rate, the next settlement and the interval, a delta
/// that moves only the mark states none, and a delta that moves the rate is folded onto the remembered
/// settlement time.
#[test]
fn tickers_frames_state_the_funding_rate_folded_across_snapshot_and_deltas() {
    use super::mark::TickerFunding;
    let parse = |s: &str| serde_json::from_str::<serde_json::Value>(s).expect("json");
    let snapshot = r#"{"topic":"tickers.BTCUSDT","type":"snapshot","data":{"symbol":"BTCUSDT","tickDirection":"ZeroPlusTick","price24hPcnt":"0.00445","lastPrice":"76039.20","prevPrice24h":"75702.30","highPrice24h":"76175.30","lowPrice24h":"75533.40","prevPrice1h":"76010.20","markPrice":"76039.64","indexPrice":"76074.24","openInterest":"71032.783","openInterestValue":"5401307247.52","turnover24h":"852036882.3779","volume24h":"11233.0970","fundingIntervalHour":"8","fundingCap":"0.00333","singleOpenInterest":"35516.392","singleOpenInterestValue":"2700653661.78","nextFundingTime":"1794153600000","fundingRate":"0.00008634","bid1Price":"76039.10","bid1Size":"14.941","ask1Price":"76039.20","ask1Size":"0.477","preOpenPrice":"","preQty":"","curPreListingPhase":""},"cs":841408205822,"ts":1794153423421}"#;
    let mark_only = r#"{"topic":"tickers.BTCUSDT","type":"delta","data":{"symbol":"BTCUSDT","markPrice":"76039.60","openInterest":"71032.826","openInterestValue":"5401307675.91","singleOpenInterest":"35516.413","singleOpenInterestValue":"2700653837.95"},"cs":841408209609,"ts":1794153423821}"#;
    let mut fold = TickerFunding::default();
    let f = fold.fold(&parse(snapshot), "BTCUSDT.P").expect("the snapshot states the funding");
    assert_eq!(
        f,
        vike_marketdata::FundingRate {
            timestamp: 1_794_153_423_421,
            symbol: "BTCUSDT.P".to_string(),
            rate: 0.00008634,
            interval_ms: Some(28_800_000),
            next_timestamp: Some(1_794_153_600_000),
            predicted: None,
            price: None,
        }
    );
    assert_eq!(fold.fold(&parse(mark_only), "BTCUSDT.P"), None, "no funding field moved");
    // A later delta that moves the rate alone keeps the settlement the snapshot stated.
    let rate_moved = r#"{"topic":"tickers.BTCUSDT","type":"delta","data":{"symbol":"BTCUSDT","fundingRate":"0.0001"},"cs":1,"ts":1794153500000}"#;
    let f = fold.fold(&parse(rate_moved), "BTCUSDT.P").expect("the rate moved");
    assert_eq!(
        (f.rate, f.next_timestamp, f.timestamp),
        (0.0001, Some(1_794_153_600_000), 1_794_153_500_000)
    );
    // The inverse stream's snapshot (BTCUSD), recorded the same minute.
    let inverse = r#"{"topic":"tickers.BTCUSD","type":"snapshot","data":{"symbol":"BTCUSD","tickDirection":"ZeroMinusTick","price24hPcnt":"0.004141","lastPrice":"75933.70","prevPrice24h":"75620.50","highPrice24h":"76061.00","lowPrice24h":"75419.00","prevPrice1h":"75902.00","markPrice":"75933.70","indexPrice":"75974.56","openInterest":"770356652","openInterestValue":"10145.12","turnover24h":"349.3601","volume24h":"26450545.0000","fundingIntervalHour":"8","fundingCap":"0.005","singleOpenInterest":"385178326","singleOpenInterestValue":"5072.56","nextFundingTime":"1794153600000","fundingRate":"0.00005768","bid1Price":"75937.20","bid1Size":"17369","ask1Price":"75937.30","ask1Size":"24403","preOpenPrice":"","preQty":"","curPreListingPhase":""},"cs":120832622466,"ts":1794153425203}"#;
    let f = TickerFunding::default().fold(&parse(inverse), "BTCUSD").expect("inverse funding");
    assert_eq!((f.rate, f.next_timestamp), (0.00005768, Some(1_794_153_600_000)));
    // A dated future's ticker states no rate: nothing is emitted, whatever its settlement says.
    let dated = r#"{"topic":"tickers.BTCUSDZ26","type":"snapshot","data":{"symbol":"BTCUSDZ26","fundingRate":"","nextFundingTime":"0"},"ts":1}"#;
    assert_eq!(TickerFunding::default().fold(&parse(dated), "BTCUSDZ26"), None);
}

/// Mark-slot semantics (W2-T4): a linear `tickers` snapshot carries the venue's REAL
/// `markPrice` → `FeedFrame::Data` with the exact decimal bits + the frame's top-level `ts`;
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
        FeedFrame::Data(DataClientMark { px, ts }) => {
            assert_eq!(px.to_bits(), 16596.00_f64.to_bits(), "markPrice, not lastPrice");
            assert_eq!(ts, 1_673_272_861_686);
        }
        other => panic!("expected Mark, got {other:?}"),
    }
    // a field-sparse delta with no mark move is dropped, not zero-priced
    let delta = r#"{"topic":"tickers.BTCUSDT","type":"delta","ts":1,"data":{"symbol":"BTCUSDT","lastPrice":"16597.50"}}"#;
    assert_eq!(route_mark_frame(delta), FeedFrame::Ignored);
    // a dead (zero) mark is dropped
    let dead = r#"{"topic":"tickers.BTCUSDT","type":"delta","ts":1,"data":{"markPrice":"0"}}"#;
    assert_eq!(route_mark_frame(dead), FeedFrame::Ignored);
    // br7 envelopes: ack confirms, reject is attributable
    assert_eq!(
        route_mark_frame(r#"{"success":true,"op":"subscribe","conn_id":"x"}"#),
        FeedFrame::Ack
    );
    match route_mark_frame(
        r#"{"success":false,"op":"subscribe","ret_msg":"Invalid symbol :tickers.NOPE"}"#,
    ) {
        FeedFrame::Error(m) => assert!(m.contains("Invalid symbol")),
        other => panic!("expected Error, got {other:?}"),
    }
    assert_eq!(route_mark_frame("ping"), FeedFrame::Ignored);
    assert_eq!(route_mark_frame(r#"{"topic":"kline.1.BTCUSDT","data":[]}"#), FeedFrame::Ignored);
}

/// The three lanes subscribe with ONE frame shape (`topic_subscribe_frame`), each naming its own
/// topic, and nothing else rides in it.
#[test]
fn every_lane_subscribes_with_the_one_topic_frame() {
    let parsed = |f: String| serde_json::from_str::<serde_json::Value>(&f).expect("JSON");
    let want = |topic: &str| serde_json::json!({"op": "subscribe", "args": [topic]});
    assert_eq!(parsed(subscribe_frame("1", "BTCUSDT")), want("kline.1.BTCUSDT"));
    assert_eq!(parsed(mark_subscribe_frame("BTCUSDT")), want("tickers.BTCUSDT"));
    assert_eq!(parsed(trades_subscribe_frame("BTCUSDT")), want("publicTrade.BTCUSDT"));
}

#[test]
fn mark_subscribe_frame_targets_the_tickers_topic() {
    let f = mark_subscribe_frame("BTCUSDT");
    let v: serde_json::Value = serde_json::from_str(&f).unwrap();
    assert_eq!(v["op"], "subscribe");
    assert_eq!(v["args"][0], "tickers.BTCUSDT");
}

/// The pairing PREDICATE (mark-slot semantics, W2-T4): only a `.P` perp pairs a mark stream (the
/// engine adds the venue's `mark_streams` row). Spot has no venue mark price at all.
#[test]
fn only_perps_pair_a_mark_stream() {
    assert!(super::should_pair_mark("BTCUSDT.P"));
    assert!(!super::should_pair_mark("BTCUSDT"), "spot has no mark price");
    assert!(spec().pairs_mark("BTCUSDT.P"), "the spec asks the one predicate");
}

/// **A coin-margined DATED contract pairs its mark by its kept row** (it did as an engine name before decision
/// 0290), a linear dated one and a pair do not, and every perpetual does by its `.P`.
#[test]
fn a_coin_margined_bars_subscription_pairs_its_mark_stream() {
    crate::history::kept_list_for_tests();
    assert!(super::should_pair_mark("BTCUSD.P"));
    assert!(super::should_pair_mark("BTCUSD_261225"));
    assert!(!super::should_pair_mark("ETHUSDT_261106"), "a linear dated contract, as before");
    assert!(!super::should_pair_mark("BTCUSD"), "the spot pair has no mark price");
    assert!(spec().pairs_mark("BTCUSD.P") && !spec().pairs_mark("BTCUSD"));
}

/// The listings of 2026-09-16 for `BTCUSD`: a Trading spot pair AND a Trading inverse perpetual.
fn btcusd_listed() -> crate::history::PerpListingAnswer {
    crate::history::PerpListingAnswer {
        bare_is_ambiguous: true,
        perp_book: crate::instrument::listing::PerpListing::Inverse,
    }
}

/// **Every lane routes a USER name by its KEPT row in a process that keeps the venue's list** (decision 0290, the
/// gatewayhub): a strategy mounted on `BTCUSDT.P` hears the LINEAR candles (BYB-2: its bars were the spot pair's),
/// one on `BTCUSD.P` the inverse ones, the bare `BTCUSDT` and `BTCUSD` are the spot pairs, and the venue's own names
/// of a USDC perpetual and a dated contract are the rows' — all with no listing read (the seeded listings host is
/// never dialled: a network read here would hang the test, not pass it).
#[test]
fn a_user_name_is_routed_by_its_kept_row_with_no_listing_read() {
    crate::history::kept_list_for_tests();
    let hosts = seeded_hosts();
    for (symbol, wire, book, socket) in [
        ("BTCUSDT.P", "BTCUSDT", Category::Linear, PUBLIC_WEBSOCKET_LINEAR),
        ("BTCUSDT", "BTCUSDT", Category::Spot, PUBLIC_WEBSOCKET_SPOT),
        ("BTCUSD.P", "BTCUSD", Category::Inverse, PUBLIC_WEBSOCKET_INVERSE),
        ("BTCUSD", "BTCUSD", Category::Spot, PUBLIC_WEBSOCKET_SPOT),
        ("BTCUSDC.P", "BTCPERP", Category::Linear, PUBLIC_WEBSOCKET_LINEAR),
        ("ETHUSDT_261106", "ETHUSDT-06NOV26", Category::Linear, PUBLIC_WEBSOCKET_LINEAR),
        ("BTCUSD_261225", "BTCUSDZ26", Category::Inverse, PUBLIC_WEBSOCKET_INVERSE),
    ] {
        assert_eq!(super::feed_route(&hosts, symbol), Ok((wire.to_string(), book)), "{symbol}");
        assert_eq!(websocket_host(&hosts.ws, book), socket, "{symbol}");
    }
    for unplaced in ["BTCUSDT.S", "BTCUSDZ26", "NOPEUSDT.P"] {
        let why = super::feed_route(&hosts, unplaced).expect_err("no kept row, no lane");
        assert!(why.contains(unplaced), "{why}");
    }
}

/// **Every session lane pings, and its pong is no data and no fault.** The kline, mark and trades plans each hand
/// the driver [`KEEPALIVE`] (the pump row's cadence; bybit asks for a ping every 20 s and cuts a connection with
/// no ping-pong and no data for 10 minutes), and the pong bybit's public sockets answer with (its v5 "Connect"
/// page: the spot shape and the linear/inverse one, both `op:"ping"`) decodes as nothing on each lane: it neither
/// confirms a subscribe nor ends the session, and the idle watchdog hears it.
#[test]
fn every_session_lane_pings_and_its_pong_is_neither_data_nor_a_fault() {
    use crate::feed::tick::KEEPALIVE;
    use vike_bridge_core::data_client::ws::{DataClientLane, DataClientLanePlan};
    use vike_bridge_core::websocket_session::market::WebsocketSessionFrameOutcome;
    crate::history::kept_list_for_tests();
    let spec = spec();
    let pongs = [
        r#"{"success":true,"ret_msg":"pong","conn_id":"0970e817-426e-429a-a679-ff7f55e0b16a","op":"ping"}"#,
        r#"{"success":true,"ret_msg":"pong","conn_id":"465772b1-7630-4fdc-a492-e003e6f0f260","req_id":"","op":"ping"}"#,
    ];
    for lane in [DataClientLane::Bars, DataClientLane::Mark, DataClientLane::Trades] {
        let plan = spec.plan(lane, "BTCUSDT.P", "1m").expect("a session plan");
        let DataClientLanePlan::Session { keepalive, mut decode, .. } = plan else {
            panic!("{lane:?} is a session lane")
        };
        assert_eq!(keepalive, Some(KEEPALIVE), "{lane:?} pings");
        for pong in pongs {
            let mut emitted = 0;
            let outcome = decode(pong, &mut |_| emitted += 1);
            assert_eq!(outcome, WebsocketSessionFrameOutcome::Ignore, "{lane:?}: {pong}");
            assert_eq!(emitted, 0, "{lane:?}: the pong emits nothing");
        }
    }
}

/// **A process that keeps no list still refuses a bare `BTCUSD`** (decision 0061, the datahub's backfill): the
/// listings route, pure here, refuses the name the venue lists as a spot pair AND an inverse perpetual.
#[test]
fn a_bare_ambiguous_name_with_no_kept_list_is_still_refused() {
    let err = crate::history::live_route_listed_for_test("BTCUSD", btcusd_listed()).unwrap_err();
    assert_eq!(
        err,
        crate::refusals::BybitHistoryRefusal::AmbiguousBareSymbol { wire: "BTCUSD".into() }
    );
}

/// **The pairing half of the daemon's CEX reconcile-health row condition**: the tradehub's rule (a row
/// only while at most one lane wrote the status line) is fed by the engine's `status_writer_lanes`, which
/// counts a paired mark lane as a second writer (`crates/vike-bridge-core/src/data_client/websocket_tests.rs`'s
/// `subscribe_bars_pairs_the_mark_of_a_perp_only`). What is bybit's is WHICH symbols pair one: the
/// DEPLOYED shape — spot, one interval — is a single writer, which is why the CI box's latch was total and
/// permanent rather than thrashing; a `.P` symbol pairs a mark lane onto the SAME status line, and the
/// daemon must see that and withhold the row.
///
/// ⚠ Without it the two halves could drift silently, on the one venue the 42-hour suppression actually
/// happened to.
#[test]
fn a_perp_bars_subscription_pairs_a_second_status_writer() {
    let spec = spec();
    assert!(!spec.pairs_mark("BTCUSDT"), "spot: one writer");
    assert!(spec.pairs_mark("ETHUSDT.P"), "a perp pairs a mark lane: two writers");
}

/// The default this feed runs and the one `config show` prints are ONE fact: the charter constant
/// is `venue.bybit.mark_streams`'s declared default.
#[test]
fn the_charter_default_is_the_declared_fields() {
    market_feed_pins::charter_default_is_declared(
        "bybit",
        DataClientSpecBybit::MARK_STREAMS_DEFAULT_ON,
    );
}

#[test]
fn quotes_and_book_are_unsupported() {
    market_feed_pins::refuses_quotes_and_book(fresh(), "BTCUSDT");
    // subscribe_trades is deliberately NOT asserted here — a real subscribe touches the network; its
    // pure parser (`parse_trades`) and subscribe-frame builder are covered below instead.
}

/// An interval bybit has no interval code for is the kline lane's refusal, decided before the route (so
/// before any listing read): the engine writes it as the lane's status and ends the lane.
#[test]
fn an_interval_without_a_code_is_the_kline_lanes_refusal() {
    use vike_bridge_core::data_client::ws::DataClientLane;
    let refused = spec().plan(DataClientLane::Bars, "BTCUSDT", "7m");
    assert!(refused.is_err(), "no interval code for 7m");
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
        FeedFrame::Data(BarKline { bar: b, closed: true }) => {
            assert_eq!(b.timestamp, 1_700_000_060_000);
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
        FeedFrame::Data(BarKline { bar: b, closed: false }) => {
            assert_eq!(b.timestamp, 1_700_000_120_000);
            assert_eq!(b.close.to_bits(), 27085.00_f64.to_bits());
            assert_eq!(b.open.to_bits(), 27080.10_f64.to_bits());
        }
        other => panic!("expected Forming, got {other:?}"),
    }
}

/// Net-hardening br7: a Bybit subscribe ACK (`{op:"subscribe", success:true}`) now CONFIRMS the
/// handshake (`FeedFrame::Ack`) instead of being silently dropped — via `WebsocketSessionFrameOutcome::Confirm`
/// it disarms the shared driver's subscribe-ack watchdog. A pong-op reply, bare-text `ping`, and
/// a non-kline topic stay Ignored.
#[test]
fn subscribe_ack_is_classified_as_ack_pong_and_junk_ignored() {
    // subscribe success ack → Ack (confirms the subscribe)
    assert_eq!(route_frame(r#"{"success":true,"op":"subscribe","conn_id":"x"}"#), FeedFrame::Ack);
    // a keepalive pong reply (op != subscribe) is ignored
    assert_eq!(
        route_frame(r#"{"success":true,"op":"pong","conn_id":"x","ret_msg":"pong"}"#),
        FeedFrame::Ignored
    );
    // not JSON at all
    assert_eq!(route_frame("ping"), FeedFrame::Ignored);
    // a non-kline topic (well-formed, empty data) is ignored on the topic prefix
    assert_eq!(route_frame(r#"{"topic":"tickers.BTCUSDT","data":[]}"#), FeedFrame::Ignored);
}

/// Net-hardening br7: a Bybit subscribe REJECT (`{op:"subscribe", success:false}`) is now an
/// ATTRIBUTABLE `FeedFrame::Error` carrying the venue's `ret_msg`, NOT silently dropped.
/// `decode_kline` maps it to `WebsocketSessionFrameOutcome::Fatal`, the session error the driver discloses.
#[test]
fn error_frame_is_attributable_not_ignored() {
    match route_frame(
        r#"{"success":false,"op":"subscribe","ret_msg":"Invalid symbol :kline.1.NOPE","conn_id":"x"}"#,
    ) {
        FeedFrame::Error(m) => {
            assert!(m.contains("Invalid symbol"), "carries the Bybit ret_msg: {m}");
        }
        other => panic!("expected an attributable Error, got {other:?}"),
    }
}

/// Net-hardening br7 (scripted-clock, deterministic): a subscribe that never acks and never
/// delivers data trips the `WebsocketSessionSubscribeAck` watchdog once this venue's declared ack window (the
/// `vike_bridge_core::websocket_session::spec::row` knobs every lane's opts consume) elapses — the attributable "no ack/data" error
/// the shared driver returns. An ack within the window disarms it.
#[test]
fn a_missing_ack_within_the_window_becomes_an_attributable_error() {
    // the ACK path (route_frame → Ack → confirm) is what disarms it
    assert_eq!(route_frame(r#"{"success":true,"op":"subscribe","conn_id":"x"}"#), FeedFrame::Ack);
    market_feed_pins::ack_watchdog(super::VENUE);
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

/// A `publicTrade` push with a buy row and a sell row → 2 `TickTrade`s with exact price/size/ts
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

    assert_eq!(ticks[0].timestamp, 1_710_000_000_000);
    assert_eq!(ticks[0].price.to_bits(), 64500.1_f64.to_bits());
    assert_eq!(ticks[0].size.to_bits(), 0.012_f64.to_bits());
    assert!(!ticks[0].is_buyer_maker, "Buy taker → buyer is NOT the maker");
    assert_eq!(ticks[0].symbol, "BTCUSDT");
    assert_eq!(ticks[0].local_timestamp, 0, "the pure mapper leaves local_ts unstamped");

    assert_eq!(ticks[1].timestamp, 1_710_000_000_123);
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
    assert_eq!(ticks[0].timestamp, 2);
}

// ---- DOM depth decode: fold_depth_frame → BookOp (the shared-driver gap wiring) ----

/// The depth decode over the shared driver: the first `snapshot` seeds the slot (`Updated`),
/// an in-sequence delta folds (`Updated`), and a GAPPED delta (dropped frame — `u` jumps past
/// `last_sequence + 1`) maps `DataClientEvent::Resync` → `WebsocketSessionDepthOperation::Gap`, on which the driver reconnects and
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
        WebsocketSessionDepthOperation::Ignored
    );
    assert!(slot.is_none());
    // the first snapshot seeds the book
    assert_matches!(
        super::fold_depth_frame(&frame("snapshot", 10, "5"), "BTCUSDT", &mut slot),
        WebsocketSessionDepthOperation::Updated(_)
    );
    // an in-sequence delta (u = last + 1) folds
    assert_matches!(
        super::fold_depth_frame(&frame("delta", 11, "8"), "BTCUSDT", &mut slot),
        WebsocketSessionDepthOperation::Updated(_)
    );
    // u jumps 11 → 13: a dropped frame → Gap (and the delta did NOT fold)
    assert_eq!(
        super::fold_depth_frame(&frame("delta", 13, "99"), "BTCUSDT", &mut slot),
        WebsocketSessionDepthOperation::Gap
    );
    let book = slot.as_ref().expect("book still present for the driver to discard");
    // best-bid price rides the INFERRED tick grid (float-subtraction tick ⇒ not bit-exact);
    // the qty is exact and is what proves the gapped delta (qty "99") did not fold.
    let L2BookLevel { price: px, qty } = book.best_bid().expect("seeded book has a bid");
    assert!((px - 60000.0).abs() < 1e-3, "best bid price off-grid: {px}");
    assert_eq!(qty, 8.0, "the gapped delta must not fold");
    // a venue-restart regression (u drops far below last) is a Gap too
    assert_eq!(
        super::fold_depth_frame(&frame("delta", 2, "77"), "BTCUSDT", &mut slot),
        WebsocketSessionDepthOperation::Gap
    );
}

/// One parse per depth frame: the decoder hands back the venue `ts` from the parse it already
/// does, and it is exactly what the second whole-frame parse (`frame_ts_ms`) used to return — the
/// top-level `ts`, `0` when the frame carries none.
#[test]
fn the_decoder_carries_the_frame_time_the_re_parse_returned() {
    use crate::feed::tick::route_frame as decode;
    use vike_bridge_core::data_client::event::DataClientEvent;
    let frame = |typ: &str, u: u64, ts: Option<i64>| {
        let mut v = serde_json::json!({
            "topic": "orderbook.200.BTCUSDT", "type": typ,
            "data": {"s":"BTCUSDT","b":[["60000.0","5"]],"a":[["60000.1","4"]],"u":u,"seq":u}
        });
        if let Some(ts) = ts {
            v["ts"] = ts.into();
        }
        v.to_string()
    };
    let mut book = vike_marketdata::L2Book::new(0.1);
    for (typ, u, ts) in [
        ("snapshot", 10, Some(1_700_000_000_123_i64)),
        ("delta", 11, Some(1_700_000_000_456)),
        ("delta", 12, None),
    ] {
        let txt = frame(typ, u, ts);
        match decode(&txt, "BTCUSDT", &mut book) {
            DataClientEvent::BookUpdated { timestamp_ms: ts_ms } => {
                assert_eq!(ts_ms, super::frame_ts_ms(&txt), "{typ} u={u}");
                assert_eq!(ts_ms, ts.unwrap_or(0), "{typ} u={u}");
            }
            other => panic!("{typ} u={u}: expected BookUpdated, got {other:?}"),
        }
    }
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
/// (depth). See [`websocket_host`]'s table for every probe.
///
/// ⚠ This is a mutation target: re-pinning the `Inverse` category's socket to [`PUBLIC_WEBSOCKET_LINEAR`]
/// reddens here, and so does collapsing any two of the three.
#[test]
fn every_category_has_its_own_public_socket() {
    let ws = seeded_hosts().ws;
    assert_eq!(websocket_host(&ws, Category::Spot), PUBLIC_WEBSOCKET_SPOT);
    assert_eq!(websocket_host(&ws, Category::Linear), PUBLIC_WEBSOCKET_LINEAR);
    assert_eq!(websocket_host(&ws, Category::Inverse), PUBLIC_WEBSOCKET_INVERSE);

    let hosts = [PUBLIC_WEBSOCKET_SPOT, PUBLIC_WEBSOCKET_LINEAR, PUBLIC_WEBSOCKET_INVERSE];
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
        let host = websocket_host(&seeded_hosts().ws, book);
        assert!(host.ends_with(host_tail), "{book:?}: {host}");
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

/// (b) For a PERP, the emitted `TickTrade.symbol` is the `.P` SERIES label even though the linear
/// wire `s` is the plain `BTCUSDT` — the `relabel_series` overwrite that keeps the tape's
/// `TickStore` key distinct from the spot twin (the store keys on `tick.symbol`, not the sink arg).
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
/// of the same string — the emitted `TickTrade.symbol` still equals the wire `s`.
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
    assert_eq!(relabeled[0].timestamp, before.timestamp);
}
