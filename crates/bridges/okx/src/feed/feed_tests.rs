use super::DataClientSpecOkx;
use super::depth::okx_depth_decode;
use super::mark::{mark_subscribe_frame, route_mark_frame};
use crate::feed::bar::{route_frame, subscribe_frame};
use crate::feed::tick::KEEPALIVE;
use crate::feed::tick::trades::{parse_trades, trades_subscribe_frame};
use std::assert_matches;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use vike_bridge_core::data_client::context::DataClientContext;
use vike_bridge_core::data_client::event::{BarKline, DataClientMark};
use vike_bridge_core::data_client::ws::{
    DataClientLane, DataClientLanePlan, DataClientSpec, DataClientWebsocket, run_plan_on,
};
use vike_bridge_core::depth::WebsocketSessionDepthOperation;
use vike_bridge_core::feed_frame::FeedFrame;
use vike_bridge_core::market_feed_pins;
use vike_bridge_core::scripted::{LaneStatus, one_session};
use vike_bridge_core::websocket_session::market::{
    WebsocketSessionFrameOutcome, WebsocketSessionMarketOptions,
};
use vike_model::feed_status::{DataClientConnectionState, DataClientStatus};
use vike_venues::venue_urls::VenueUrls;

// The shared capturing sink (testing-arch Phase 4d) — replaces the inline formatted-string
// RecordingSink copy that used to live here (same canonical `calls()` line forms).
use vike_data::RecordingSink;

/// okx's spec on the release's seed rows, with the tests' kept list held (a lane is planned by USER name, decision 0290).
fn spec() -> DataClientSpecOkx {
    crate::instrument::kept_list_for_tests();
    DataClientSpecOkx::new(&VenueUrls::seed("okx")).expect("the seed holds okx's live data rows")
}

/// The engine over okx's spec (built, never dialled).
fn fresh() -> DataClientWebsocket<DataClientSpecOkx> {
    DataClientWebsocket::new(spec(), Arc::new(RecordingSink::default()), || {})
}

/// The pairing PREDICATE (mark-slot semantics, W2-T4): only a `*-SWAP` pairs a mark stream (the
/// engine adds the venue's `mark_streams` row). Spot has no mark price; dated futures are deliberately
/// out of scope (see [`DataClientSpecOkx::pairs_mark`]).
#[test]
fn only_swaps_pair_a_mark_stream() {
    let spec = spec();
    assert!(spec.pairs_mark("BTCUSDT.P"));
    assert!(spec.pairs_mark("btcusd.p"), "a coin-margined swap is a SWAP too, in any case");
    assert!(!spec.pairs_mark("BTCUSDT"), "spot has no mark price");
    assert!(!spec.pairs_mark("BTCUSD_261030"), "dated futures are out of scope");
    assert!(!spec.pairs_mark("NOSUCHUSDT.P"), "a name with no kept row pairs nothing");
}

/// The default this feed runs and the one `config show` prints are ONE fact: the charter constant
/// is `venue.okx.mark_streams`'s declared default.
#[test]
fn the_charter_default_is_the_declared_fields() {
    market_feed_pins::charter_default_is_declared(
        "okx",
        DataClientSpecOkx::MARK_STREAMS_DEFAULT_ON,
    );
}

/// `history-candles` holds CLOSED candles only, so the warm-up has no forming tail to pop.
#[test]
fn the_warm_up_seeds_closed_candles_only() {
    const { assert!(!DataClientSpecOkx::SEED_LAST_BAR_FORMING) };
}

#[test]
fn quotes_and_book_are_unsupported() {
    market_feed_pins::refuses_quotes_and_book(fresh(), "BTC-USDT");
    // subscribe_trades is deliberately NOT asserted here — a real subscribe touches the network; its
    // pure parser (`parse_trades`) and subscribe-frame builder are covered below instead.
}

/// An interval okx has no bar code for is the candle lane's refusal: the engine writes it as the lane's
/// status and ends the lane (no reconnect could fix it).
#[test]
fn an_interval_without_a_bar_code_is_the_candle_lanes_refusal() {
    assert!(spec().plan(DataClientLane::Bars, "BTCUSDT", "7m").is_err());
}

fn candle_frame(row: serde_json::Value) -> String {
    serde_json::json!({
        "arg": {"channel": "candle1m", "instId": "BTC-USDT"},
        "data": [row]
    })
    .to_string()
}

/// A `confirm="1"` candle → a CLOSED bar with exact o/h/l/c/v bit patterns + ts.
#[test]
fn confirmed_candle_classifies_closed_with_exact_bits() {
    // [ts, o, h, l, c, vol, volCcy, volCcyQuote, confirm]
    let frame = candle_frame(serde_json::json!([
        "1700000060000",
        "27010.25",
        "27100.00",
        "27000.00",
        "27080.10",
        "8.10000000",
        "219000.00",
        "5913000.00",
        "1"
    ]));
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

/// A SWAP candle's live bar carries the BASE volume (`volCcy`), as its REST twin does, never `vol`, which
/// counts contracts: the first row of `tests/fixtures/okx_klines_btc_usdt_swap_1m.json` (989.82 contracts
/// of 0.01 BTC), pushed on the candle channel.
#[test]
fn a_swap_candle_carries_the_base_volume_not_the_contract_count() {
    let frame = serde_json::json!({
        "arg": {"channel": "candle1m", "instId": "BTC-USDT-SWAP"},
        "data": [["1794147540000", "75899.8", "75913.9", "75894.8", "75913.9", "989.82", "9.8982",
                  "751269.57155", "1"]]
    })
    .to_string();
    match route_frame(&frame) {
        FeedFrame::Data(BarKline { bar, closed: true }) => {
            assert_eq!(bar.volume.to_bits(), 9.8982_f64.to_bits(), "{}", bar.volume);
        }
        other => panic!("expected Closed, got {other:?}"),
    }
}

/// A `confirm="0"` candle is a still-forming bar (the `forming_bar` sink lane), not a close.
#[test]
fn unconfirmed_candle_classifies_forming() {
    let frame = candle_frame(serde_json::json!([
        "1700000120000",
        "27080.10",
        "27090.00",
        "27080.00",
        "27085.00",
        "1.5",
        "40000.0",
        "40000.0",
        "0"
    ]));
    match route_frame(&frame) {
        FeedFrame::Data(BarKline { bar: b, closed: false }) => {
            assert_eq!(b.timestamp, 1_700_000_120_000);
            assert_eq!(b.close.to_bits(), 27085.00_f64.to_bits());
            assert_eq!(b.open.to_bits(), 27080.10_f64.to_bits());
        }
        other => panic!("expected Forming, got {other:?}"),
    }
}

/// Net-hardening br7: the OKX subscribe ACK (`event:"subscribe"`) now CONFIRMS the handshake
/// (`FeedFrame::Ack`) instead of being silently dropped — it is what disarms `run_live`'s
/// subscribe-ack watchdog. A bare-text `pong` and an unrelated `event` stay Ignored.
#[test]
fn subscribe_ack_is_classified_as_ack_pong_and_junk_ignored() {
    // OKX subscribe ack: has arg.channel but NO data → an Ack (confirms the subscribe)
    assert_eq!(
        route_frame(r#"{"event":"subscribe","arg":{"channel":"candle1m","instId":"BTC-USDT"}}"#),
        FeedFrame::Ack
    );
    // an unrelated event envelope (e.g. unsubscribe) is neither an error nor a confirm
    assert_eq!(
        route_frame(r#"{"event":"unsubscribe","arg":{"channel":"candle1m"}}"#),
        FeedFrame::Ignored
    );
    // bare-text keepalive is not JSON
    assert_eq!(route_frame("pong"), FeedFrame::Ignored);
}

/// Net-hardening br7 anchor (was `subscribe_ack_pong_and_junk_are_ignored` asserting Ignored):
/// an OKX `event:"error"` subscribe reject is now an ATTRIBUTABLE `FeedFrame::Error` carrying
/// the venue's `code`+`msg`, NOT silently dropped. `run_live` returns this as the session error.
#[test]
fn error_frame_is_attributable_not_ignored() {
    match route_frame(r#"{"event":"error","code":"60012","msg":"Invalid request: channel"}"#) {
        FeedFrame::Error(m) => {
            assert!(m.contains("60012"), "carries the OKX error code: {m}");
            assert!(m.contains("Invalid request: channel"), "carries the OKX message: {m}");
        }
        other => panic!("expected an attributable Error, got {other:?}"),
    }
}

/// Net-hardening br7 (scripted-clock, deterministic): a subscribe that never acks and never
/// delivers data trips the `WebsocketSessionSubscribeAck` watchdog once this venue's declared ack window (the
/// `vike_bridge_core::websocket_session::spec::row` knobs every lane's opts consume) elapses — the attributable "no ack/data" error
/// the shared driver returns. An ack (or first data) within the window disarms it. Same
/// clock-free harness `WebsocketSessionSubscribeAck`'s own unit tests use, but pinned to the venue's real row.
#[test]
fn a_missing_ack_within_the_window_becomes_an_attributable_error() {
    // the ACK path (route_frame → Ack → confirm) is what disarms it
    assert_eq!(
        route_frame(r#"{"event":"subscribe","arg":{"channel":"candle1m"}}"#),
        FeedFrame::Ack
    );
    market_feed_pins::ack_watchdog(super::VENUE);
}

#[test]
fn subscribe_frame_targets_the_candle_channel() {
    let f = subscribe_frame("1H", "BTC-USDT");
    assert!(f.contains("\"channel\":\"candle1H\""), "channel wrong: {f}");
    assert!(f.contains("\"instId\":\"BTC-USDT\""));
    assert!(f.contains("\"op\":\"subscribe\""));
}

/// The three lanes subscribe with ONE frame shape (`channel_subscribe_frame`), each naming its own
/// channel, and nothing else rides in it; the mark lane names its `funding-rate` channel beside its
/// `mark-price` one (the venue's public funding rate rides the same socket).
#[test]
fn every_lane_subscribes_with_the_one_channel_frame() {
    let parsed = |f: String| serde_json::from_str::<serde_json::Value>(&f).expect("JSON");
    let want = |channel: &str| serde_json::json!({"op": "subscribe", "args": [{"channel": channel, "instId": "X-SWAP"}]});
    assert_eq!(parsed(subscribe_frame("1m", "X-SWAP")), want("candle1m"));
    assert_eq!(
        parsed(mark_subscribe_frame("X-SWAP")),
        serde_json::json!({"op": "subscribe", "args": [
            {"channel": "mark-price", "instId": "X-SWAP"},
            {"channel": "funding-rate", "instId": "X-SWAP"}
        ]})
    );
    assert_eq!(parsed(trades_subscribe_frame("X-SWAP")), want("trades"));
}

/// **The `funding-rate` channel, from a frame RECORDED on 2026-11-08** (the public socket, no key):
/// `fundingRate` is the current period's rate for the settlement at `fundingTime`, the interval is the
/// gap to `nextFundingTime`, and an empty `nextFundingRate` is no prediction. Through the mark lane's seam
/// the row lands on the sink's funding verb and the mark frame on `mark_tick`, off ONE socket.
#[test]
fn the_funding_rate_channel_is_read_off_the_mark_lane() {
    let recorded = r#"{"arg":{"channel":"funding-rate","instId":"BTC-USDT-SWAP"},"data":[{"formulaType":"withRate","fundingRate":"0.0000127303089891","fundingTime":"1794153600000","impactValue":"20000.0000000000000000","instId":"BTC-USDT-SWAP","instType":"SWAP","interestRate":"0.0001000000000000","maxFundingRate":"0.00375","method":"current_period","minFundingRate":"-0.00375","nextFundingRate":"","nextFundingTime":"1794182400000","premium":"-0.0005087740517345","prevFundingTime":"1794124800000","settFundingRate":"0.0000335788804851","settState":"settled","ts":"1794153374629"}]}"#;
    let v: serde_json::Value = serde_json::from_str(recorded).expect("json");
    // the row is booked under the lane's USER name (decision 0290), never the instId it was read off
    let f = super::mark::route_funding_value(&v, "BTCUSDT.P").expect("a funding row");
    assert_eq!(
        f,
        vike_marketdata::FundingRate {
            timestamp: 1_794_153_374_629,
            symbol: "BTCUSDT.P".to_string(),
            rate: 0.0000127303089891,
            interval_ms: Some(28_800_000),
            next_timestamp: Some(1_794_153_600_000),
            predicted: None,
            price: None,
        }
    );
    let mark_frame: serde_json::Value = serde_json::from_str(
        r#"{"arg":{"channel":"mark-price","instId":"BTC-USDT-SWAP"},"data":[{"instId":"BTC-USDT-SWAP","markPx":"64000.5","ts":"1700000000000"}]}"#,
    )
    .expect("json");
    assert_eq!(super::mark::route_funding_value(&mark_frame, "BTC-USDT-SWAP"), None);

    let sink = Arc::new(RecordingSink::default());
    let (ctx, _seen) = scripted_context(Arc::clone(&sink));
    let sent = Rc::default();
    let ack = r#"{"event":"subscribe","arg":{"channel":"mark-price","instId":"BTC-USDT-SWAP"}}"#;
    let mark = r#"{"arg":{"channel":"mark-price","instId":"BTC-USDT-SWAP"},"data":[{"instId":"BTC-USDT-SWAP","markPx":"64000.5","ts":"1700000000000"}]}"#;
    run_lane(DataClientLane::Mark, "BTCUSDT.P", &ctx, &[ack, recorded, mark], &sent);
    assert_eq!(sink.fundings(), [("okx".to_string(), "BTCUSDT.P".to_string(), f)]);
    assert!(
        sink.calls().contains(&"mark_tick(okx,BTCUSDT.P,64000.5,1700000000000)".to_string()),
        "the mark still lands: {:?}",
        sink.calls()
    );
}

/// **A funding frame never confirms the mark channel**: a session that delivers the `funding-rate` channel's
/// ack and its rows but nothing of `mark-price` lands the funding and never reads healthy, so a stalled mark
/// feed cannot pass for live behind the funding rows.
#[test]
fn a_funding_frame_never_confirms_the_mark_channel() {
    let sink = Arc::new(RecordingSink::default());
    let (ctx, seen) = scripted_context(Arc::clone(&sink));
    let sent = Rc::default();
    let ack = r#"{"event":"subscribe","arg":{"channel":"funding-rate","instId":"BTC-USDT-SWAP"}}"#;
    let row = r#"{"arg":{"channel":"funding-rate","instId":"BTC-USDT-SWAP"},"data":[{"fundingRate":"0.0001","fundingTime":"1794153600000","nextFundingTime":"1794182400000","nextFundingRate":"","instId":"BTC-USDT-SWAP","ts":"1794153374629"}]}"#;
    run_lane(DataClientLane::Mark, "BTCUSDT.P", &ctx, &[ack, row, row], &sent);
    assert_eq!(sink.fundings().len(), 2, "the rows still land");
    assert_eq!(
        *seen.lock().unwrap(),
        [DataClientStatus::new(
            DataClientConnectionState::Error,
            "BTCUSDT.P mark ws error (reconnecting): script exhausted"
        )],
        "no healthy status: nothing of the mark channel arrived"
    );
    assert_eq!(route_mark_frame(ack), FeedFrame::Ignored, "the funding ack is not the mark's");
}

// ---- OKX `trades` channel: parse_trades (Task okxtrades) ----

#[test]
fn trades_subscribe_frame_targets_the_trades_channel() {
    let f = trades_subscribe_frame("BTC-USDT");
    assert!(f.contains("\"channel\":\"trades\""), "channel wrong: {f}");
    assert!(f.contains("\"instId\":\"BTC-USDT\""));
    assert!(f.contains("\"op\":\"subscribe\""));
}

/// A `trades` push with a buy row and a sell row -> 2 `TickTrade`s with exact price/size/ts
/// bit patterns, and `is_buyer_maker` following Binance's `m` convention (true exactly when
/// the buyer was the MAKER, i.e. the taker/aggressor SOLD): `side=="buy"` -> false,
/// `side=="sell"` -> true.
#[test]
fn parse_trades_maps_buy_and_sell_rows_with_exact_bits() {
    let payload = serde_json::json!({
        "arg": {"channel": "trades", "instId": "BTC-USDT"},
        "data": [
            {
                "instId": "BTC-USDT",
                "tradeId": "130639474",
                "px": "64500.1",
                "sz": "0.012",
                "side": "buy",
                "ts": "1710000000000"
            },
            {
                "instId": "BTC-USDT",
                "tradeId": "130639475",
                "px": "64499.9",
                "sz": "0.5",
                "side": "sell",
                "ts": "1710000000123"
            }
        ]
    });
    let ticks = parse_trades(&payload);
    assert_eq!(ticks.len(), 2);

    assert_eq!(ticks[0].timestamp, 1_710_000_000_000);
    assert_eq!(ticks[0].price.to_bits(), 64500.1_f64.to_bits());
    assert_eq!(ticks[0].size.to_bits(), 0.012_f64.to_bits());
    assert!(!ticks[0].is_buyer_maker, "buy taker -> buyer is NOT the maker");
    assert_eq!(ticks[0].symbol, "BTC-USDT");
    assert_eq!(ticks[0].local_timestamp, 0, "the pure mapper leaves local_ts unstamped");

    assert_eq!(ticks[1].timestamp, 1_710_000_000_123);
    assert_eq!(ticks[1].price.to_bits(), 64499.9_f64.to_bits());
    assert_eq!(ticks[1].size.to_bits(), 0.5_f64.to_bits());
    assert!(ticks[1].is_buyer_maker, "sell taker -> buyer WAS the maker");
}

/// Non-`trades` pushes (a candle subscribe ack, and an actual candle data push) yield an
/// empty vec — `parse_trades` must not misclassify another channel's frame.
#[test]
fn parse_trades_ignores_non_trades_pushes() {
    let candle_ack = serde_json::json!({
        "event": "subscribe",
        "arg": {"channel": "candle1m", "instId": "BTC-USDT"}
    });
    assert!(parse_trades(&candle_ack).is_empty());

    let candle_push = serde_json::json!({
        "arg": {"channel": "candle1m", "instId": "BTC-USDT"},
        "data": [["1700000060000", "1", "1", "1", "1", "1", "1", "1", "1"]]
    });
    assert!(parse_trades(&candle_push).is_empty());

    assert!(parse_trades(&serde_json::json!({})).is_empty());
}

/// A malformed/unparseable row (bad price, zero size, unrecognized side, missing field) is
/// skipped in place — never panics, never produces a bogus tick.
#[test]
fn parse_trades_skips_malformed_rows_without_panicking() {
    let payload = serde_json::json!({
        "arg": {"channel": "trades", "instId": "BTC-USDT"},
        "data": [
            {"instId": "BTC-USDT", "px": "not-a-number", "sz": "0.012", "side": "buy", "ts": "1710000000000"},
            {"instId": "BTC-USDT", "px": "1.0", "sz": "0", "side": "buy", "ts": "1710000000000"},
            {"instId": "BTC-USDT", "px": "1.0", "sz": "1.0", "side": "unknown", "ts": "1710000000000"},
            {"instId": "BTC-USDT", "px": "1.0", "sz": "1.0", "side": "buy"},
            {"px": "1.0", "sz": "1.0", "side": "buy", "ts": "1710000000000"},
            "not even an object"
        ]
    });
    assert!(parse_trades(&payload).is_empty());
}

/// One bad row alongside a good one: the batch is NOT failed wholesale (mirrors
/// `vike_binance::feed::tick::trades::rest_agg_trades`'s per-element tolerance).
#[test]
fn parse_trades_keeps_good_rows_alongside_a_bad_one() {
    let payload = serde_json::json!({
        "arg": {"channel": "trades", "instId": "BTC-USDT"},
        "data": [
            {"instId": "BTC-USDT", "px": "bad", "sz": "1.0", "side": "buy", "ts": "1"},
            {"instId": "BTC-USDT", "px": "100.0", "sz": "1.0", "side": "buy", "ts": "2"}
        ]
    });
    let ticks = parse_trades(&payload);
    assert_eq!(ticks.len(), 1);
    assert_eq!(ticks[0].timestamp, 2);
}

// ---- OKX `books` depth fold (seqId/prevSeqId chain), scripted-frame style ----
//
// OKX fixed the `checksum` field to `0` on 2026-06-23 and a public-WS census on 2026-10-10 saw `0` on
// all 869 `books` frames, so `okx_depth_decode` parses each frame ONCE and never reads it.

/// A `[px, sz, "0", "1"]` level array (OKX's `books` shape; only `[0]`/`[1]` are read).
fn depth_levels(ls: &[(&str, &str)]) -> Vec<serde_json::Value> {
    ls.iter().map(|(p, s)| serde_json::json!([p, s, "0", "1"])).collect()
}

/// A scripted OKX `books` frame with an explicit `seqId`/`prevSeqId` and a `checksum` field as the
/// wire carries it (`None` omits the field).
fn books_frame(
    action: &str,
    seq: u64,
    previous_sequence: i64,
    bids: &[(&str, &str)],
    asks: &[(&str, &str)],
    checksum: Option<i64>,
) -> String {
    let mut d = serde_json::json!({
        "bids": depth_levels(bids),
        "asks": depth_levels(asks),
        "ts": "1700000000000",
        "seqId": seq,
        "prevSeqId": previous_sequence,
    });
    if let Some(cs) = checksum {
        d["checksum"] = serde_json::json!(cs);
    }
    serde_json::json!({
        "arg": {"channel": "books", "instId": "BTC-USDT-SWAP"},
        "action": action,
        "data": [d]
    })
    .to_string()
}

/// Happy path: a snapshot then a stream of updates, each carrying the `checksum: 0` the live venue
/// sends, all decode to `Updated` with the frame's `ts`, and the folded book is the merged book.
#[test]
fn a_zero_checksum_snapshot_and_update_stream_folds_the_book() {
    let snap = books_frame(
        "snapshot",
        10,
        -1,
        &[("100.0", "5"), ("99.0", "3")],
        &[("101.0", "4"), ("102.0", "2")],
        Some(0),
    );
    let mut book: Option<std::sync::Arc<vike_marketdata::L2Book>> = None;
    assert_eq!(
        okx_depth_decode(&snap, &mut book),
        WebsocketSessionDepthOperation::Updated(1_700_000_000_000)
    );

    // 50 deltas: the best bid size climbs 6, 7, … 55 and a fresh 98.0 bid appears once.
    let mut previous = 10;
    for i in 0..50_u64 {
        let seq = 11 + i;
        let size = (6 + i).to_string();
        let mut bids = vec![("100.0", size.as_str())];
        if i == 0 {
            bids.push(("98.0", "1"));
        }
        let upd = books_frame("update", seq, previous as i64, &bids, &[], Some(0));
        assert_eq!(
            okx_depth_decode(&upd, &mut book),
            WebsocketSessionDepthOperation::Updated(1_700_000_000_000),
            "i={i}"
        );
        previous = seq;
    }
    let bk = book.expect("book seeded");
    assert_eq!(bk.last_sequence, 60);
    let (bids, asks) = bk.top_n(10);
    assert_eq!(bids.len(), 3, "100.0, 99.0, 98.0");
    assert_eq!(asks.len(), 2);
    assert_eq!(bids[0].qty, 55.0, "the last delta's size sits on the best bid");
}

/// The `checksum` field is never read: a zero, a non-zero that no book could match, and an absent
/// field all fold identically. (Before 2026-10-10 a non-zero mismatch returned `Gap` through a
/// raw-string mirror of the book and a second whole-frame parse; the field is deprecated-to-0 on the
/// live venue, so that work was dead.)
#[test]
fn the_checksum_field_is_never_read() {
    for checksum in [Some(0), Some(424_242), Some(-1), None] {
        let snap = books_frame("snapshot", 10, -1, &[("100.0", "5")], &[("101.0", "4")], checksum);
        let mut book: Option<std::sync::Arc<vike_marketdata::L2Book>> = None;
        assert_matches!(
            okx_depth_decode(&snap, &mut book),
            WebsocketSessionDepthOperation::Updated(_),
            "{checksum:?}"
        );
        let upd = books_frame("update", 11, 10, &[("100.0", "7")], &[], checksum);
        assert_matches!(
            okx_depth_decode(&upd, &mut book),
            WebsocketSessionDepthOperation::Updated(_),
            "{checksum:?}"
        );
    }
}

/// A `prevSeqId` break is a `Gap` on the sequence chain: the one corruption check this fold keeps.
#[test]
fn a_previous_sequence_break_still_gaps() {
    let snap = books_frame("snapshot", 10, -1, &[("100.0", "5")], &[("101.0", "4")], Some(0));
    let mut book: Option<std::sync::Arc<vike_marketdata::L2Book>> = None;
    assert_matches!(okx_depth_decode(&snap, &mut book), WebsocketSessionDepthOperation::Updated(_));
    // prevSeqId 99 != last seqId 10 → seq gap
    let upd = books_frame("update", 11, 99, &[("100.0", "6")], &[], Some(0));
    assert_eq!(okx_depth_decode(&upd, &mut book), WebsocketSessionDepthOperation::Gap);
}

/// An `update` before any `snapshot` is ignored (no book yet), and a replayed `seqId` is stale.
#[test]
fn an_update_before_the_snapshot_or_a_stale_one_is_ignored() {
    let mut book: Option<std::sync::Arc<vike_marketdata::L2Book>> = None;
    let early = books_frame("update", 5, 4, &[("100.0", "1")], &[], Some(0));
    assert_eq!(okx_depth_decode(&early, &mut book), WebsocketSessionDepthOperation::Ignored);

    let snap = books_frame("snapshot", 10, -1, &[("100.0", "5")], &[("101.0", "4")], Some(0));
    assert_matches!(okx_depth_decode(&snap, &mut book), WebsocketSessionDepthOperation::Updated(_));
    let stale = books_frame("update", 10, 0, &[("100.0", "9")], &[], Some(0));
    assert_eq!(okx_depth_decode(&stale, &mut book), WebsocketSessionDepthOperation::Ignored);
}

/// One parse per depth frame: the `books` decoder hands back `data[0].ts` (a decimal STRING on the
/// wire) from the parse it already does. The values are what the second whole-frame parse this
/// replaced (`frame_ts_ms`, now deleted) returned: `0` when the field is absent or not a number.
#[test]
fn the_decoder_carries_the_frame_time_the_re_parse_returned() {
    use crate::feed::tick::{BooksFrame, parse_books_frame};
    let with_ts = |action: &str, ts: serde_json::Value| {
        let mut d = serde_json::json!({
            "bids": [["100.0", "5", "0", "1"]], "asks": [["101.0", "4", "0", "1"]],
            "seqId": 10, "prevSeqId": 9, "checksum": 0,
        });
        if !ts.is_null() {
            d["ts"] = ts;
        }
        serde_json::json!({"arg": {"channel": "books", "instId": "X"}, "action": action, "data": [d]})
            .to_string()
    };
    for (action, ts, want) in [
        ("snapshot", serde_json::json!("1700000000123"), 1_700_000_000_123_i64),
        ("update", serde_json::json!("1700000000456"), 1_700_000_000_456),
        ("update", serde_json::Value::Null, 0),
        ("snapshot", serde_json::json!("not a number"), 0),
    ] {
        let txt = with_ts(action, ts);
        let ts_ms = match parse_books_frame(&txt) {
            BooksFrame::Snapshot { timestamp_ms: ts_ms, .. }
            | BooksFrame::Update { timestamp_ms: ts_ms, .. } => ts_ms,
            BooksFrame::Other => panic!("{action}: not decoded as a books frame"),
        };
        assert_eq!(ts_ms, want, "{action}: {txt}");
    }
}

// ---- OKX `mark-price` channel (mark-slot semantics, W2-T4), scripted-frame style ----

/// A real-shaped `mark-price` data push → `FeedFrame::Data` with the exact `markPx` bits and
/// the row's `ts` (both decimal strings on the wire).
#[test]
fn mark_price_frame_decodes_px_and_ts() {
    let frame = serde_json::json!({
            "arg": {"channel": "mark-price", "instId": "BTC-USDT-SWAP"},
            "data": [{"instType": "SWAP", "instId": "BTC-USDT-SWAP", "markPx": "27123.45", "ts": "1700000000123"}]
        })
        .to_string();
    match route_mark_frame(&frame) {
        FeedFrame::Data(DataClientMark { px, ts }) => {
            assert_eq!(px.to_bits(), 27_123.45_f64.to_bits(), "markPx, not lastPx");
            assert_eq!(ts, 1_700_000_000_123);
        }
        other => panic!("expected Mark, got {other:?}"),
    }
}

/// The subscribe ACK envelope is classified `Ack`; the venue's error envelope is an
/// attributable `Error` carrying code+msg (drives a `Fatal` in the pump).
#[test]
fn mark_ack_and_error_envelopes_classify() {
    let ack = serde_json::json!({
        "event": "subscribe",
        "arg": {"channel": "mark-price", "instId": "BTC-USDT-SWAP"}
    })
    .to_string();
    assert_eq!(route_mark_frame(&ack), FeedFrame::Ack);

    let err = serde_json::json!({
        "event": "error", "code": "60012", "msg": "Invalid request"
    })
    .to_string();
    match route_mark_frame(&err) {
        FeedFrame::Error(m) => assert!(m.contains("60012") && m.contains("Invalid request")),
        other => panic!("expected Error, got {other:?}"),
    }
}

/// A non-`mark-price` push, a dead (0/neg/NaN) mark, a malformed row, and the bare-text
/// keepalive are all `Ignored` — never a bogus mark into the `PriceBoard` mark slot.
#[test]
fn mark_non_channel_dead_and_malformed_frames_are_ignored() {
    let candle = serde_json::json!({
        "arg": {"channel": "candle1m", "instId": "BTC-USDT-SWAP"},
        "data": [["1", "1", "1", "1", "1", "1", "1", "1", "1"]]
    })
    .to_string();
    assert_eq!(route_mark_frame(&candle), FeedFrame::Ignored);

    for dead in ["0", "-1.0", "not-a-number"] {
        let f = serde_json::json!({
            "arg": {"channel": "mark-price", "instId": "BTC-USDT-SWAP"},
            "data": [{"markPx": dead, "ts": "1"}]
        })
        .to_string();
        assert_eq!(route_mark_frame(&f), FeedFrame::Ignored, "dead mark {dead} dropped");
    }

    // empty data array, and the bare-text "pong" keepalive
    let empty = serde_json::json!({
        "arg": {"channel": "mark-price", "instId": "BTC-USDT-SWAP"}, "data": []
    })
    .to_string();
    assert_eq!(route_mark_frame(&empty), FeedFrame::Ignored);
    assert_eq!(route_mark_frame("pong"), FeedFrame::Ignored);
}

/// The subscribe frame targets the public `mark-price` channel on the given inst — the exact
/// `{"op":"subscribe","args":[{"channel":"mark-price","instId":...}]}` wire grammar.
#[test]
fn mark_subscribe_frame_targets_the_mark_price_channel() {
    let v: serde_json::Value =
        serde_json::from_str(&mark_subscribe_frame("BTC-USDT-SWAP")).unwrap();
    assert_eq!(v["op"], "subscribe");
    assert_eq!(v["args"][0]["channel"], "mark-price");
    assert_eq!(v["args"][0]["instId"], "BTC-USDT-SWAP");
}

/// **The candle lane dials the BUSINESS socket.** OKX serves `candle<bar>` on `/ws/v5/business` only: on the
/// public socket the subscribe is answered with a "wrong URL or channel" error and no bar ever follows, which
/// the driver reads as a session error and retries for ever, so an okx mount (runtime or boot) never ran
/// `on_bar` (live proof, 2026-10-31). The candle plan names the business socket (the seed's, byte for byte the
/// URL it always dialled), subscribes `candle<bar>`, pings with [`KEEPALIVE`] and carries the REST warm-up.
#[test]
fn the_candle_lane_dials_the_business_socket_and_never_the_public_one() {
    let spec = spec();
    assert_eq!(spec.business_websocket, "wss://ws.okx.com/ws/v5/business");
    assert_eq!(spec.public_websocket, "wss://ws.okx.com:8443/ws/v5/public");
    assert_eq!(spec.history_rest, "https://www.okx.com");
    let plan = spec.plan(DataClientLane::Bars, "BTCUSDT", "1m").expect("a candle plan");
    let DataClientLanePlan::Session { url, subscribe, keepalive, seed, .. } = plan else {
        panic!("the candle lane is a session")
    };
    assert_eq!(url, spec.business_websocket);
    assert_eq!(subscribe, Some(subscribe_frame("1m", "BTC-USDT")));
    assert_eq!(keepalive, Some(KEEPALIVE), "the candle socket pings: okx cuts one silent for 30 s");
    assert!(seed.is_some(), "the REST warm-up runs once per feed");
}

/// **Every session lane pings, and its pong is no data and no fault.** OKX cuts a socket that pushed nothing for
/// 30 s (a quiet instrument's candle or trades channel), so the candle, mark and trades plans each hand the
/// driver [`KEEPALIVE`] (the pump row's cadence), and the raw-text `pong` OKX answers with decodes as
/// nothing on each lane: it neither confirms a subscribe nor ends the session, and the idle watchdog hears it.
#[test]
fn every_session_lane_pings_and_its_pong_is_neither_data_nor_a_fault() {
    let spec = spec();
    for lane in [DataClientLane::Bars, DataClientLane::Mark, DataClientLane::Trades] {
        let plan = spec.plan(lane, "BTCUSDT.P", "1m").expect("a session plan");
        let DataClientLanePlan::Session { keepalive, mut decode, .. } = plan else {
            panic!("{lane:?} is a session lane")
        };
        assert_eq!(keepalive, Some(KEEPALIVE), "{lane:?} pings");
        let mut emitted = 0;
        let outcome = decode("pong", &mut |_| emitted += 1);
        assert_eq!(outcome, WebsocketSessionFrameOutcome::Ignore, "{lane:?}: the pong");
        assert_eq!(emitted, 0, "{lane:?}: the pong emits nothing");
    }
}

/// A box whose table holds no okx data row builds no feed: the refusal names the row.
#[test]
fn no_data_row_builds_no_feed() {
    let refused = DataClientSpecOkx::new(&VenueUrls::default());
    assert!(refused.expect_err("no row, no feed").to_string().contains("no address row"));
}

// ---- the public lanes (mark, trades) through the engine ---------------------------------------------

/// What a lane that went live and then faulted wrote: the engine's `Connected` healthy text, then
/// `fault` as an `Error`.
fn live_then_error(fault: &str) -> [DataClientStatus; 2] {
    [
        DataClientStatus::new(DataClientConnectionState::Connected, "LIVE · OKX"),
        DataClientStatus::new(DataClientConnectionState::Error, fault),
    ]
}

/// A [`DataClientContext`] over `sink` and a [`LaneStatus`] (it stops the lane at its first fault): (ctx, the
/// statuses the lane wrote).
fn scripted_context(
    sink: Arc<RecordingSink>,
) -> (DataClientContext, Arc<std::sync::Mutex<Vec<DataClientStatus>>>) {
    let LaneStatus { status, stop, wake, seen } = LaneStatus::halting_at("ws error");
    (DataClientContext { venue: "okx", sink, status, wake, stop, own: () }, seen)
}

/// `lane` of `instrument` through the engine over ONE scripted session (`frames`), on the spec's own plan; the
/// dial pins that the lane dials the PUBLIC socket.
fn run_lane(
    lane: DataClientLane,
    instrument: &str,
    ctx: &DataClientContext,
    frames: &[&str],
    sent: &Rc<RefCell<Vec<String>>>,
) {
    let spec = spec();
    let word = lane.word("");
    let plan = spec.plan(lane, instrument, word).expect("the lane's plan");
    let mut session = one_session(frames, sent);
    let public = spec.public_websocket.clone();
    let dial = move |url: &str, opts: &WebsocketSessionMarketOptions<'_>| {
        assert_eq!(url, public, "a public lane dials the public socket");
        session(opts)
    };
    run_plan_on::<DataClientSpecOkx, _>(plan, lane, instrument, word, ctx, dial);
}

/// The mark lane through the engine: it sends the `mark-price` subscribe, the confirmed session writes
/// the ONE healthy text, the mark lands on `mark_tick`, and the fault reads
/// `"{inst} mark ws error (reconnecting): {e}"`.
#[test]
fn the_mark_lane_subscribes_lands_marks_and_reports_through_its_seam() {
    let sink = Arc::new(RecordingSink::default());
    let (ctx, seen) = scripted_context(Arc::clone(&sink));
    let sent = Rc::default();
    let ack = r#"{"event":"subscribe","arg":{"channel":"mark-price","instId":"BTC-USDT-SWAP"}}"#;
    let mark = r#"{"arg":{"channel":"mark-price","instId":"BTC-USDT-SWAP"},
        "data":[{"instId":"BTC-USDT-SWAP","markPx":"64000.5","ts":"1700000000000"}]}"#;
    // the USER name in, the kept row's instId on the wire, the user name out (decision 0290)
    run_lane(DataClientLane::Mark, "BTCUSDT.P", &ctx, &[ack, mark], &sent);
    assert_eq!(*sent.borrow(), [mark_subscribe_frame("BTC-USDT-SWAP")]);
    assert_eq!(sink.calls(), ["mark_tick(okx,BTCUSDT.P,64000.5,1700000000000)"]);
    assert_eq!(
        *seen.lock().unwrap(),
        live_then_error("BTCUSDT.P mark ws error (reconnecting): script exhausted")
    );
}

/// The trades lane through the engine: it sends the `trades` subscribe, the confirmed session writes
/// the ONE healthy text, each print lands on `trade` stamped at receipt, and the fault reads
/// `"{inst} trades ws error (reconnecting): {e}"`.
#[test]
fn the_trades_lane_subscribes_lands_prints_and_reports_through_its_seam() {
    let sink = Arc::new(RecordingSink::default());
    let (ctx, seen) = scripted_context(Arc::clone(&sink));
    let sent = Rc::default();
    let ack = r#"{"event":"subscribe","arg":{"channel":"trades","instId":"BTC-USDT"}}"#;
    let print = r#"{"arg":{"channel":"trades","instId":"BTC-USDT"},"data":[{"instId":"BTC-USDT",
        "tradeId":"130639474","px":"64500.1","sz":"0.012","side":"buy","ts":"1710000000000"}]}"#;
    run_lane(DataClientLane::Trades, "BTCUSDT", &ctx, &[ack, print], &sent);
    assert_eq!(*sent.borrow(), [trades_subscribe_frame("BTC-USDT")]);
    let trades = sink.trades();
    assert_eq!(trades.len(), 1, "{trades:?}");
    let (venue, symbol, tick) = &trades[0];
    assert_eq!(
        (venue.as_str(), symbol.as_str(), tick.price, tick.size),
        ("okx", "BTCUSDT", 64500.1, 0.012)
    );
    assert_eq!(tick.symbol, "BTCUSDT", "the print is booked under the user name");
    assert_eq!(tick.timestamp, 1_710_000_000_000);
    assert!(tick.local_timestamp > 0, "stamped at receipt");
    assert_eq!(
        *seen.lock().unwrap(),
        live_then_error("BTCUSDT trades ws error (reconnecting): script exhausted")
    );
}

/// The mark, trades and depth lanes dial the PUBLIC socket; only the candle lane dials the business one.
#[test]
fn the_public_lanes_dial_the_public_socket() {
    let spec = spec();
    for lane in [DataClientLane::Mark, DataClientLane::Trades, DataClientLane::Depth] {
        let url = match spec.plan(lane, "BTCUSDT.P", lane.word("")).expect("a plan") {
            DataClientLanePlan::Session { url, .. } | DataClientLanePlan::Depth { url, .. } => url,
            DataClientLanePlan::Own(_) | DataClientLanePlan::SessionOn { .. } => {
                panic!("{lane:?} dials for itself")
            }
        };
        assert_eq!(url, spec.public_websocket, "{lane:?}");
    }
}

/// The documented `64008` notice is a market socket's announced close, in the venue's words; an ack, an error of the
/// same code, a notice of another code and a data frame merely carrying the digits are not. Every session lane arms it.
#[test]
fn notice_64008_is_the_only_market_notice_and_every_lane_arms_it() {
    let notice = r#"{"event":"notice","code":"64008","msg":"The connection will soon be closed for a service upgrade. Please reconnect.","connId":"a4d3ae55"}"#;
    let words = super::market_service_upgrade_notice(notice).expect("64008 is the notice");
    assert!(words.contains("64008") && words.contains("service upgrade"), "{words}");
    for other in [
        r#"{"event":"subscribe","arg":{"channel":"trades","instId":"BTC-USDT"},"connId":"a4d3ae55"}"#,
        r#"{"event":"error","code":"64008","msg":"x"}"#,
        r#"{"event":"notice","code":"64009","msg":"x"}"#,
        r#"{"arg":{"channel":"trades","instId":"BTC-USDT"},"data":[{"tradeId":"64008","px":"1","sz":"1","side":"buy","ts":"1"}]}"#,
        "64008",
    ] {
        assert_eq!(super::market_service_upgrade_notice(other), None, "{other}");
    }
    let spec = spec();
    for (lane, interval) in [
        (DataClientLane::Bars, "1m"),
        (DataClientLane::Mark, "mark"),
        (DataClientLane::Trades, "trades"),
    ] {
        match spec.plan(lane, "BTCUSDT.P", interval).expect("a plan") {
            DataClientLanePlan::Session { notice, .. } => {
                assert!(notice.is_some(), "{lane:?} reads no notice");
            }
            _ => panic!("{lane:?} is a session lane"),
        }
    }
}
