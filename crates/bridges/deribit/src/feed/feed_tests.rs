use super::{
    BarFolder, BarRoll, DEPTH_LEVELS, DataClientSpecDeribit, KEEPALIVE_PING, bars_frame,
    decode_depth_frame, fold_depth_frame, frame_ts_ms, subscribe_refusal,
};
use crate::feed::decode::{FundingIntegral, parse_chart_bar, parse_ticker_funding};
use std::assert_matches;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vike_bridge_core::data_client::context::DataClientContext;
use vike_bridge_core::data_client::event::DataClientEvent;
use vike_bridge_core::data_client::ws::{
    DataClientLane, DataClientLanePlan, DataClientSpec, DataClientWebsocket, run_plan_on,
};
use vike_bridge_core::depth::{FAULT_LOG_EVERY, WebsocketSessionDepthOperation};
use vike_bridge_core::klines::kline_to_bar;
use vike_bridge_core::scripted::{LaneStatus, one_session};
use vike_bridge_core::websocket_session::market::WebsocketSessionMarketOptions;
use vike_data::RecordingSink;
use vike_marketdata::{Bar, L2Book, L2BookLevel};

/// deribit's spec on the release's seed rows.
fn spec() -> DataClientSpecDeribit {
    DataClientSpecDeribit::new(&vike_venues::venue_urls::VenueUrls::seed("deribit"))
        .expect("the seed holds deribit's live rows")
}

/// The engine over deribit's spec (built, never dialled).
fn fresh() -> DataClientWebsocket<DataClientSpecDeribit> {
    DataClientWebsocket::new(spec(), Arc::new(RecordingSink::default()), || {})
}

/// **The bar lane refuses an interval its resolution table has no code for, before anything spawns** (owner
/// ruling B-22: the gatewayhub's `BarIntervalGate` moved here, as the spec's `refuse_bars`), in the
/// tradehub's old `deribit_plan` words. The refusal is the table, in both directions.
#[test]
fn an_interval_without_a_resolution_code_is_refused_before_a_thread_spawns() {
    use vike_data::{DataClient, DataClientError};
    let mut feeds = fresh();
    let Err(DataClientError::Subscribe(text)) = feeds.subscribe_bars("BTC-PERPETUAL", "4h") else {
        panic!("4h must be refused")
    };
    assert!(
        text.starts_with(
            "deribit streams `chart.trades` by RESOLUTION code and has none for interval \"4h\""
        ) && text.contains("there is no 4h")
            && text.ends_with("Name an interval this venue serves"),
        "the tradehub's wording: {text}"
    );
    assert_eq!(feeds.status_writer_lanes(), 0, "nothing spawned");
    for interval in ["1m", "3m", "5m", "10m", "15m", "30m", "1h", "2h", "3h", "6h", "12h", "1d"] {
        assert_eq!(
            super::refuse_bars_interval(interval).is_ok(),
            crate::history::resolution_code(interval).is_ok(),
            "{interval}"
        );
    }
    for interval in ["4h", "1w", "1M", "7m", ""] {
        assert!(super::refuse_bars_interval(interval).is_err(), "{interval:?} has no code");
    }
}

/// Deribit pairs no mark stream with a bars subscription (its marks stream chain-wide through the options
/// feed), so no `mark_streams` row can open one.
#[test]
fn no_bars_subscription_pairs_a_mark_stream() {
    let spec = spec();
    assert!(!spec.pairs_mark("BTC-PERPETUAL") && !spec.pairs_mark("BTC_USDC"));
}

/// The depth lane is DECLARED, so the gate every consumer reads — `vike_data::require_live_verb`,
/// which the datahub's `StreamLabel::Depth` and the desktop's `MarketDataSession::want` both call — admits it.
/// ⚠ `subscribe_depth` itself is not called here: it spawns the real thread, which would dial the
/// public mainnet host from a test. The decode it runs is driven below, over captured frames.
#[test]
fn depth_is_served_through_the_declared_row() {
    assert!(vike_data::require_live_verb("deribit", vike_venues::DataClientVerb::Depth).is_ok());
    assert!(vike_venues::capabilities_for("deribit").live_data.depth);
}

/// The keepalive payload is a well-formed `public/test` JSON-RPC request — the frame whose
/// reply resets the idle watchdog on a quiet channel (the row declares the cadence).
#[test]
fn keepalive_is_a_public_test_rpc() {
    let v: serde_json::Value = serde_json::from_str(KEEPALIVE_PING).unwrap();
    assert_eq!(v["method"], "public/test");
}

/// Every session lane hands the engine this venue's `public/test` keepalive text, and the pump row
/// declares its cadence: the engine builds each session's knobs from the row (`WebsocketSessionKnobs::opts`, row
/// ownership), the payload being the venue's.
#[test]
fn every_session_lane_sends_the_public_test_keepalive() {
    let spec = spec();
    for lane in
        [DataClientLane::Bars, DataClientLane::Quotes, DataClientLane::Trades, DataClientLane::Book]
    {
        let plan = spec.plan(lane, "BTC-PERPETUAL", lane.word("1m")).expect("a plan");
        let DataClientLanePlan::Session { keepalive, .. } = plan else {
            panic!("{lane:?} is an engine session")
        };
        assert_eq!(keepalive, Some(KEEPALIVE_PING), "{lane:?}");
    }
    let knobs = vike_bridge_core::websocket_session::spec::row("deribit").knobs();
    assert!(knobs.keepalive_every.is_some(), "the row declares the cadence");
}

// ── BarFolder: the closed-bar inference ─────────────────────────────────────────────────────

fn bar(ts: i64, close: f64) -> vike_marketdata::Bar {
    kline_to_bar(ts, close, close, close, close, 1.0)
}

#[test]
fn the_first_push_forms_and_closes_nothing() {
    let mut f = BarFolder::new();
    let out = f.fold(bar(60_000, 100.0));
    assert_eq!(out, Some(BarRoll { closed: None, forming: bar(60_000, 100.0) }));
}

#[test]
fn a_same_bucket_push_updates_the_forming_picture() {
    let mut f = BarFolder::new();
    f.fold(bar(60_000, 100.0));
    let out = f.fold(bar(60_000, 101.0));
    assert_eq!(out, Some(BarRoll { closed: None, forming: bar(60_000, 101.0) }));
}

#[test]
fn a_newer_bucket_closes_the_held_one_at_its_last_state() {
    let mut f = BarFolder::new();
    f.fold(bar(60_000, 100.0));
    f.fold(bar(60_000, 101.0));
    let out = f.fold(bar(120_000, 102.0));
    assert_eq!(
        out,
        Some(BarRoll { closed: Some(bar(60_000, 101.0)), forming: bar(120_000, 102.0) }),
        "the close is the bucket's LAST observed state, not its first"
    );
}

/// A quiet instrument can skip buckets entirely (chart pushes are trade-driven): the held
/// bucket still closes; the skipped empties are NOT fabricated (module-doc honesty rule).
#[test]
fn a_bucket_jump_closes_the_held_bar_without_fabricating_empties() {
    let mut f = BarFolder::new();
    f.fold(bar(60_000, 100.0));
    let out = f.fold(bar(300_000, 105.0));
    assert_eq!(
        out,
        Some(BarRoll { closed: Some(bar(60_000, 100.0)), forming: bar(300_000, 105.0) })
    );
}

#[test]
fn an_older_bucket_is_stale_and_changes_nothing() {
    let mut f = BarFolder::new();
    f.fold(bar(120_000, 102.0));
    assert_eq!(f.fold(bar(60_000, 999.0)), None);
    // …and the forming picture is untouched.
    let out = f.fold(bar(120_000, 103.0));
    assert_eq!(out, Some(BarRoll { closed: None, forming: bar(120_000, 103.0) }));
}

// ── depth: the DOM's conflating lane ────────────────────────────────────────────────────────

// REAL captures — 2026-10-04, keyless public mainnet (`wss://www.deribit.com/ws/api/v2`), one
// `public/subscribe` per instrument, nothing authenticated. Every key order, number spelling
// (`2.0e3`, `6.563e-5`) and the `prev_change_id`-LAST layout is as the venue sent it; only each
// side's level list was cut to its first four entries so the files stay readable. Line 0 is the
// subscribe ack, line 1 the snapshot, the rest the changes in wire order.
const BTC_PERPETUAL: &str = include_str!("../../tests/fixtures/book_depth/btc_perpetual.jsonl");
const BTC_USDC_PERPETUAL: &str =
    include_str!("../../tests/fixtures/book_depth/btc_usdc_perpetual.jsonl");
const BTC_USDC_SPOT: &str = include_str!("../../tests/fixtures/book_depth/btc_usdc_spot.jsonl");
/// The venue's real answer to `book.NOT-AN-INSTRUMENT.100ms`: an empty `result`, then silence.
const MATCHED_NO_CHANNEL: &str =
    include_str!("../../tests/fixtures/book_depth/subscribe_matched_no_channel.jsonl");

/// The three instrument SHAPES the datahub will be asked to subscribe: an inverse perpetual
/// (USD-notional contracts), a linear USDC perpetual and a spot pair (both base-coin amounts).
const INSTRUMENTS: [(&str, &str); 3] = [
    ("BTC-PERPETUAL", BTC_PERPETUAL),
    ("BTC_USDC-PERPETUAL", BTC_USDC_PERPETUAL),
    ("BTC_USDC", BTC_USDC_SPOT),
];

fn lines(fixture: &'static str) -> Vec<&'static str> {
    fixture.lines().collect()
}

/// Fold every line of `fixture` through the depth decode, returning the book slot and each verdict.
fn fold_all(
    fixture: &'static str,
    symbol: &str,
) -> (Option<Arc<L2Book>>, Vec<WebsocketSessionDepthOperation>) {
    let mut book = None;
    let ops = lines(fixture).into_iter().map(|l| fold_depth_frame(l, symbol, &mut book)).collect();
    (book, ops)
}

/// Instrument-name routing, against what the venue ACTUALLY named. For each of the three shapes the
/// channel this crate builds is the one the venue's own ack and its notifications carry, and the
/// notification's `instrument_name` is the symbol subscribed — so the underscore in a USDC name
/// (`BTC_USDC-PERPETUAL`, `BTC_USDC`) is carried verbatim and nothing in the path rewrites it.
#[test]
fn every_instrument_shape_is_asked_for_by_the_channel_the_venue_named() {
    for (instrument, fixture) in INSTRUMENTS {
        let ls = lines(fixture);
        let want = crate::feed::decode::book_channel(instrument);
        let ack: serde_json::Value = serde_json::from_str(ls[0]).unwrap();
        assert_eq!(ack["result"][0], want, "{instrument}: the ack names the channel we ask for");
        let sub: serde_json::Value = serde_json::from_str(
            &crate::feed::decode::public_subscribe_frame(std::slice::from_ref(&want)),
        )
        .unwrap();
        assert_eq!(sub["params"]["channels"][0], want, "{instrument}: and we ask for it verbatim");
        let snap: serde_json::Value = serde_json::from_str(ls[1]).unwrap();
        assert_eq!(snap["params"]["channel"], want, "{instrument}");
        assert_eq!(snap["params"]["data"]["instrument_name"], instrument);
    }
}

/// An inverse perpetual: the captured chain (snapshot + four changes, with `new`/`change`/`delete`
/// all present) replays to the venue's book.
#[test]
fn an_inverse_perpetual_replays_its_captured_chain() {
    let (book, ops) = fold_all(BTC_PERPETUAL, "BTC-PERPETUAL");
    assert_eq!(
        ops[0],
        WebsocketSessionDepthOperation::Ignored,
        "the subscribe ack is not a book frame"
    );
    assert_eq!(
        ops[1],
        WebsocketSessionDepthOperation::Updated(1_791_120_409_417),
        "the snapshot carries the venue time"
    );
    assert_eq!(ops[5], WebsocketSessionDepthOperation::Updated(1_791_120_410_684));
    assert!(
        ops[2..].iter().all(|op| matches!(op, WebsocketSessionDepthOperation::Updated(_))),
        "{ops:?}"
    );
    let book = book.expect("the snapshot anchored the book");
    assert_eq!(book.tick_size, 0.5, "the grid inferred from the snapshot's own levels");
    assert_eq!(book.last_sequence, 177_146_680_351, "anchored on the LAST change's id");
    assert_eq!(book.best_bid(), Some(L2BookLevel::new(85_217.5, 2_000.0)), "`2.0e3` parses");
    assert_eq!(book.best_ask(), Some(L2BookLevel::new(85_218.0, 174_870.0)));
    assert_eq!(book.bid_quantity_at(85_209.0), 10_780.0, "a `change` replaced the snapshot's 1940");
    assert_eq!(book.bid_quantity_at(84_301.5), 380.0, "a `new` bid joined");
    assert_eq!(book.ask_quantity_at(85_236.0), 300_000.0, "`3.0e5` parses");
    // 85239.0 was ADDED by the second change and DELETED by the third: gone, not a zero level.
    assert_eq!(book.ask_quantity_at(85_239.0), 0.0);
    // 85240.0 was added by the first change and re-`new`ed by the fourth: the later value stands.
    assert_eq!(book.ask_quantity_at(85_240.0), 7_790.0);
    assert_eq!(book.ask_quantity_at(85_240.5), 19_600.0, "`1.96e4` parses");
}

/// A linear USDC perpetual: fractional base-coin amounts, a 0.1 grid, and `bids: []` on a change
/// (a side the venue did not touch) — which is still a chained frame and still folds.
#[test]
fn a_linear_usdc_perpetual_folds_on_its_own_grid_and_an_untouched_side_is_not_a_fault() {
    let (book, ops) = fold_all(BTC_USDC_PERPETUAL, "BTC_USDC-PERPETUAL");
    assert_eq!(ops[0], WebsocketSessionDepthOperation::Ignored);
    assert!(
        ops[1..].iter().all(|op| matches!(op, WebsocketSessionDepthOperation::Updated(_))),
        "{ops:?}"
    );
    let book = book.expect("anchored");
    // The book quantizes on an INFERRED tick, the smallest gap the snapshot shows: the f64
    // subtraction of two decimals, about 1e-11 off `0.1` at 85,000. A level read back on that raw
    // gap was ~7.4e-6 off (85217.4 came back as 85217.39999256). The shared
    // `vike_bridge_core::depth::infer_tick_size` reads the gap as its decimal, so both are exact.
    assert_eq!(book.tick_size, 0.1, "the inferred tick is the decimal grid");
    assert_eq!(book.last_sequence, 200_328_940_309);
    let best = book.best_bid().expect("a best bid");
    assert_eq!(best.price, 85_217.4, "{best:?}");
    assert_eq!(best.qty, 1.2191);
    assert_eq!(book.ask_quantity_at(85_217.5), 1.719, "a `change` on the best ask");
    // 127826.3 was ADDED by the second change and DELETED by the third.
    assert_eq!(book.ask_quantity_at(127_826.3), 0.0);
}

/// A spot pair: a 0.01 grid, amounts down to a satoshi (`6.563e-5`), and `delete`s that land on
/// levels the snapshot actually held.
#[test]
fn a_spot_pair_folds_on_its_own_grid_and_deletes_remove_what_the_snapshot_held() {
    let (book, ops) = fold_all(BTC_USDC_SPOT, "BTC_USDC");
    assert_eq!(ops[0], WebsocketSessionDepthOperation::Ignored);
    assert!(
        ops[1..].iter().all(|op| matches!(op, WebsocketSessionDepthOperation::Updated(_))),
        "{ops:?}"
    );
    let book = book.expect("anchored");
    assert_eq!(book.tick_size, 0.01, "the inferred tick is the decimal grid");
    assert_eq!(book.bid_quantity_at(85_197.0), 6.563e-5, "the exponent spelling parses exactly");
    assert_eq!(
        book.bid_quantity_at(85_195.5),
        0.0,
        "the first change deleted a level the snapshot held"
    );
    assert_eq!(
        book.bid_quantity_at(85_193.75),
        0.0,
        "added by the second change, deleted by the third"
    );
    assert_eq!(book.bid_quantity_at(85_197.01), 0.16209971);
    assert_eq!(book.ask_quantity_at(85_197.02), 0.04876869, "the LAST change's value stands");
}

/// A frame dropped between two changes is a [`WebsocketSessionDepthOperation::Gap`] — never a fold — so the driver
/// reconnects and re-seeds; the book it leaves behind is exactly the last good one.
#[test]
fn a_dropped_frame_is_a_gap_and_the_book_is_untouched() {
    let ls = lines(BTC_PERPETUAL);
    let mut book = None;
    assert_matches!(
        fold_depth_frame(ls[1], "BTC-PERPETUAL", &mut book),
        WebsocketSessionDepthOperation::Updated(_)
    );
    // ls[2] (the first change) never arrives; ls[3] chains to it, not to the snapshot.
    assert_eq!(
        fold_depth_frame(ls[3], "BTC-PERPETUAL", &mut book),
        WebsocketSessionDepthOperation::Gap
    );
    let book = book.unwrap();
    assert_eq!(book.last_sequence, 177_146_678_834, "the anchor did not move across the gap");
    assert_eq!(
        book.bid_quantity_at(85_202.0),
        0.0,
        "…and nothing from the unchained frame was folded"
    );
}

/// A frame the book already reflects is ignored — NOT a gap, even though its `prev_change_id` no
/// longer matches the advanced anchor (the check-order rule in `market_data`'s module doc).
#[test]
fn a_replayed_frame_is_ignored_not_a_gap() {
    let ls = lines(BTC_PERPETUAL);
    let mut book = None;
    fold_depth_frame(ls[1], "BTC-PERPETUAL", &mut book);
    assert_matches!(
        fold_depth_frame(ls[2], "BTC-PERPETUAL", &mut book),
        WebsocketSessionDepthOperation::Updated(_)
    );
    assert_eq!(
        fold_depth_frame(ls[2], "BTC-PERPETUAL", &mut book),
        WebsocketSessionDepthOperation::Ignored
    );
    assert_eq!(book.unwrap().last_sequence, 177_146_679_077);
}

/// A change that outruns its anchor — the session's first frame is not a snapshot — builds nothing.
#[test]
fn a_change_before_its_snapshot_anchors_nothing() {
    let ls = lines(BTC_PERPETUAL);
    let mut book = None;
    assert_eq!(
        fold_depth_frame(ls[2], "BTC-PERPETUAL", &mut book),
        WebsocketSessionDepthOperation::Ignored
    );
    assert!(book.is_none(), "an unanchored change must not conjure a book");
}

/// A second snapshot mid-session re-anchors: the book is REPLACED, not merged.
#[test]
fn a_second_snapshot_replaces_the_book() {
    let ls = lines(BTC_PERPETUAL);
    let mut book = None;
    fold_depth_frame(ls[1], "BTC-PERPETUAL", &mut book);
    fold_depth_frame(ls[2], "BTC-PERPETUAL", &mut book);
    assert_eq!(book.as_ref().unwrap().ask_quantity_at(85_240.0), 13_000.0, "the change added it");
    assert_matches!(
        fold_depth_frame(ls[1], "BTC-PERPETUAL", &mut book),
        WebsocketSessionDepthOperation::Updated(_)
    );
    let book = book.unwrap();
    assert_eq!(book.last_sequence, 177_146_678_834);
    assert_eq!(book.ask_quantity_at(85_240.0), 0.0, "re-anchored: the earlier change is gone");
}

/// An EMPTY side is a real book, not a fault: a one-sided snapshot anchors and publishes the side it
/// has, and a snapshot of NOTHING anchors on the fallback grid. (SYNTHETIC — the captured books were
/// all two-sided; these are the captured shape with a side emptied.)
#[test]
fn an_empty_side_is_a_book_not_a_fault() {
    const ONE_SIDED: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"book.BTC-PERPETUAL.100ms","data":{"timestamp":1791120409417,"type":"snapshot","change_id":10,"instrument_name":"BTC-PERPETUAL","bids":[["new",85217.5,2.0e3],["new",85217.0,10.0]],"asks":[]}}}"#;
    const EMPTY: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"book.BTC-PERPETUAL.100ms","data":{"timestamp":1791120409417,"type":"snapshot","change_id":11,"instrument_name":"BTC-PERPETUAL","bids":[],"asks":[]}}}"#;
    let mut book = None;
    assert_eq!(
        fold_depth_frame(ONE_SIDED, "BTC-PERPETUAL", &mut book),
        WebsocketSessionDepthOperation::Updated(1_791_120_409_417)
    );
    let b = book.as_ref().unwrap();
    assert_eq!(b.best_ask(), None);
    assert_eq!(b.ask_levels(), 0);
    assert_eq!(b.bid_levels(), 2);
    let (bids, asks) = b.top_n(DEPTH_LEVELS);
    assert_eq!((bids.len(), asks.len()), (2, 0));

    let mut empty = None;
    assert_matches!(
        fold_depth_frame(EMPTY, "BTC-PERPETUAL", &mut empty),
        WebsocketSessionDepthOperation::Updated(_)
    );
    let e = empty.unwrap();
    assert_eq!((e.bid_levels(), e.ask_levels()), (0, 0));
    assert_eq!(e.tick_size, 0.01, "no level to infer a grid from: the shared fallback");
}

/// The venue event-time the freshness watchdog clocks off: `params.data.timestamp`, `0` when absent
/// (the driver's receive-time fallback), never a panic on junk.
#[test]
fn the_frame_time_is_the_venues_own_stamp() {
    let ls = lines(BTC_PERPETUAL);
    assert_eq!(frame_ts_ms(ls[1]), 1_791_120_409_417);
    assert_eq!(frame_ts_ms(ls[2]), 1_791_120_409_710);
    assert_eq!(frame_ts_ms(ls[0]), 0, "an ack carries no data timestamp");
    assert_eq!(frame_ts_ms("not json"), 0);
}

/// One parse per depth frame: on every captured fixture the decoder's `ts_ms` is exactly what the
/// second whole-frame parse (`frame_ts_ms`) returns, and the three values pinned above ride the
/// decoder too (the ack is not a book frame, so it carries no `ts_ms` at all).
#[test]
fn the_decoder_carries_the_frame_time_the_re_parse_returned() {
    use crate::feed::decode::route_frame;
    for (instrument, fixture) in INSTRUMENTS {
        let tick = fold_all(fixture, instrument).0.expect("anchored").tick_size;
        let mut book = L2Book::new(tick);
        let mut seen = 0;
        for l in lines(fixture) {
            if let DataClientEvent::BookUpdated { timestamp_ms: ts_ms } =
                route_frame(l, instrument, &mut book)
            {
                assert_eq!(ts_ms, frame_ts_ms(l), "{instrument}: {l}");
                seen += 1;
            }
        }
        assert!(seen >= 2, "{instrument}: the snapshot and at least one change were decoded");
    }
    let ls = lines(BTC_PERPETUAL);
    let mut book = L2Book::new(0.5);
    let ts_of = |event: DataClientEvent| match event {
        DataClientEvent::BookUpdated { timestamp_ms: ts_ms } => ts_ms,
        other => panic!("expected BookUpdated, got {other:?}"),
    };
    assert_eq!(ts_of(route_frame(ls[1], "BTC-PERPETUAL", &mut book)), 1_791_120_409_417);
    assert_eq!(ts_of(route_frame(ls[2], "BTC-PERPETUAL", &mut book)), 1_791_120_409_710);
    assert_eq!(route_frame(ls[0], "BTC-PERPETUAL", &mut book), DataClientEvent::Ignored, "the ack");
}

/// A refused subscribe is recognised — the captured empty-`result` reply and a JSON-RPC `error`
/// envelope (SYNTHETIC: no error reply was provoked on the live host) — and nothing else is.
#[test]
fn only_a_refusal_is_a_refusal() {
    let why = subscribe_refusal(MATCHED_NO_CHANNEL.trim()).expect("the captured refusal");
    assert!(why.contains("matched no channels"), "{why}");
    let envelope = r#"{"jsonrpc":"2.0","id":1,"error":{"code":10009,"message":"invalid_params"}}"#;
    let err = subscribe_refusal(envelope).expect("an error envelope");
    assert!(err.contains("10009") && err.contains("invalid_params"), "{err}");
    for (name, fixture) in INSTRUMENTS {
        let ls = lines(fixture);
        assert_eq!(subscribe_refusal(ls[0]), None, "{name}: a real ack is not a refusal");
        assert_eq!(subscribe_refusal(ls[1]), None, "{name}: a snapshot is not a refusal");
    }
    // The keepalive's reply (the documented `public/test` answer: an object `result`).
    let pong = r#"{"jsonrpc":"2.0","id":9929,"result":{"version":"1.2.26"}}"#;
    assert_eq!(subscribe_refusal(pong), None);
}

/// The decode gaps a refused session and speaks ONCE per [`FAULT_LOG_EVERY`] — the throttle is the
/// stamp the caller keeps — and only while the book is unanchored.
#[test]
fn a_refused_subscribe_gaps_the_session_and_speaks_once_per_window() {
    let window = FAULT_LOG_EVERY.as_millis() as i64;
    let refusal = MATCHED_NO_CHANNEL.trim();
    let symbol = "NOT-AN-INSTRUMENT";
    let (mut book, mut spoke) = (None, None);
    let t0 = 1_000_000;
    assert_eq!(
        decode_depth_frame(refusal, symbol, &mut book, &mut spoke, t0),
        WebsocketSessionDepthOperation::Gap
    );
    assert_eq!(spoke, Some(t0), "the first refusal speaks");
    let soon = t0 + window - 1;
    assert_eq!(
        decode_depth_frame(refusal, symbol, &mut book, &mut spoke, soon),
        WebsocketSessionDepthOperation::Gap
    );
    assert_eq!(spoke, Some(t0), "…inside the window the verdict is the same but nothing is said");
    let later = t0 + window;
    assert_eq!(
        decode_depth_frame(refusal, symbol, &mut book, &mut spoke, later),
        WebsocketSessionDepthOperation::Gap
    );
    assert_eq!(spoke, Some(later), "…and a window later it speaks again");
    assert!(book.is_none());
}

/// The refusal check is for the unanchored window only: an ack passes, and once the book is
/// anchored a stray reply is just an ignored frame — never a gap that throws a good book away.
#[test]
fn an_ack_passes_and_an_anchored_book_is_never_gapped_by_a_reply() {
    let ls = lines(BTC_PERPETUAL);
    let symbol = "BTC-PERPETUAL";
    let (mut book, mut spoke) = (None, None);
    assert_eq!(
        decode_depth_frame(ls[0], symbol, &mut book, &mut spoke, 5),
        WebsocketSessionDepthOperation::Ignored
    );
    assert_matches!(
        decode_depth_frame(ls[1], symbol, &mut book, &mut spoke, 6),
        WebsocketSessionDepthOperation::Updated(_)
    );
    let refusal = MATCHED_NO_CHANNEL.trim();
    assert_eq!(
        decode_depth_frame(refusal, symbol, &mut book, &mut spoke, 7),
        WebsocketSessionDepthOperation::Ignored
    );
    assert_eq!(spoke, None, "nothing was said");
    assert!(book.is_some(), "the anchored book survived");
}

/// The DOM lane's plan: the `book.{inst}.100ms` subscribe on the mainnet socket, the `public/test`
/// keepalive, the unsubscribe/subscribe re-seed, no REST seed, the top [`DEPTH_LEVELS`] (200) a side
/// published (the engine's `publish_top`: best first, on the book's own tick, stamped at receipt), and
/// this venue's depth timings row.
#[test]
fn the_depth_lane_is_the_book_channel_on_the_depth_driver() {
    let spec = spec();
    let plan = spec.plan(DataClientLane::Depth, "BTC-PERPETUAL", "depth").expect("a plan");
    let DataClientLanePlan::Depth { url, subscribe, keepalive, resubscribe, seed, levels, .. } =
        plan
    else {
        panic!("the DOM lane is a depth plan")
    };
    let channel = ["book.BTC-PERPETUAL.100ms".to_string()];
    let sub = super::public_subscribe_frame(&channel);
    assert_eq!(url, spec.net.ws);
    assert_eq!(subscribe.as_deref(), Some(sub.as_str()));
    assert_eq!(keepalive, Some(KEEPALIVE_PING));
    assert_eq!(resubscribe, [super::public_unsubscribe_frame(&channel), sub]);
    assert!(seed().is_none(), "WS-seeded: no REST book");
    assert_eq!(levels, DEPTH_LEVELS);
    assert_eq!(DEPTH_LEVELS, 200);
}

/// The EFFECTIVE depth-driver timings, each a literal this file held (or read off the pump row)
/// before the venue timings table existed (decision 0160): evidence beside the table's own matrix
/// pin, so a row edit that retunes deribit's DOM watchdog fails here too.
#[test]
fn the_depth_driver_runs_on_the_timings_it_always_did() {
    let plan = spec().plan(DataClientLane::Depth, "BTC-PERPETUAL", "depth").expect("a plan");
    let DataClientLanePlan::Depth { timings, .. } = plan else { panic!("a depth plan") };
    assert_eq!(timings.idle_threshold(), Duration::from_secs(60));
    assert_eq!(timings.freshness_threshold(), Duration::from_secs(120));
    assert_eq!(timings.reseed_interval(), Duration::from_secs(300));
    assert_eq!(timings.backoff(), Duration::from_secs(3));
}

/// The depth lane's watchdog is the timings row's, and the pump row (which the other four lanes
/// read, and which still owns the stop-poll cadence this lane takes) states the same 60 s. The lane
/// does not read the pump row for it, so this is what keeps the two statements one fact.
#[test]
fn the_depth_watchdog_is_the_pump_rows_too() {
    let knobs = vike_bridge_core::websocket_session::spec::row("deribit").knobs();
    let timings = vike_venues::venue_timings::DERIBIT.depth();
    assert_eq!(knobs.idle_threshold, Some(timings.idle_threshold()));
}

// ── the live lanes (quotes, trades) through the engine ──────────────────────────────────────────

/// A [`DataClientContext`] over `sink` and a [`LaneStatus`] (it stops the lane at its first fault): (ctx, the
/// statuses the lane wrote).
fn scripted_context(sink: Arc<RecordingSink>) -> (DataClientContext, Arc<Mutex<Vec<String>>>) {
    let LaneStatus { status, stop, wake, seen } = LaneStatus::halting_at("ws error");
    (DataClientContext { venue: "deribit", sink, status, wake, stop, own: () }, seen)
}

/// `lane` of `symbol` through the engine over ONE scripted session (`frames`), on the spec's own plan;
/// the dial pins that the lane dials the MAINNET socket.
fn run_lane(
    lane: DataClientLane,
    symbol: &str,
    ctx: &DataClientContext,
    frames: &[&str],
    sent: &Rc<RefCell<Vec<String>>>,
) {
    let spec = spec();
    let word = lane.word("");
    let plan = spec.plan(lane, symbol, word).expect("the lane's plan");
    let mut session = one_session(frames, sent);
    let mainnet = spec.net.ws.clone();
    let dial = move |url: &str, opts: &WebsocketSessionMarketOptions<'_>| {
        assert_eq!(url, mainnet, "a live lane dials the mainnet socket");
        session(opts)
    };
    run_plan_on::<DataClientSpecDeribit, _>(plan, lane, symbol, word, ctx, dial);
}

/// **A ticker frame never confirms the bars channel**: on a perpetual's bars socket the `ticker.*.agg2` frame
/// lands its funding on the sink and answers `Ignore`, so a stalled chart cannot pass for live behind the
/// ticker; a chart push still folds and confirms.
#[test]
fn a_ticker_frame_lands_funding_and_never_confirms_the_bars_channel() {
    use vike_bridge_core::websocket_session::market::WebsocketSessionFrameOutcome;
    let mut folder = BarFolder::new();
    let mut funding = crate::feed::decode::FundingIntegral::new();
    let mut events = Vec::new();
    let ticker = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"ticker.BTC-PERPETUAL.agg2","data":{"timestamp":1794153424816,"instrument_name":"BTC-PERPETUAL","mark_price":75980.96,"index_price":75973.68,"current_funding":0.0001,"funding_8h":3.3e-6}}}"#;
    let chart = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"chart.trades.BTC-PERPETUAL.1","data":{"volume":0.05219351,"tick":1573645080000,"open":8869.79,"low":8788.25,"high":8870.31,"cost":463.0,"close":8791.25}}}"#;
    let outcome =
        bars_frame(ticker, "BTC-PERPETUAL", &mut folder, &mut funding, &mut |e| events.push(e));
    assert_eq!(outcome, WebsocketSessionFrameOutcome::Ignore, "the ticker is not the chart");
    assert!(matches!(&events[..], [DataClientEvent::Funding(_)]), "the funding lands: {events:?}");
    let outcome =
        bars_frame(chart, "BTC-PERPETUAL", &mut folder, &mut funding, &mut |e| events.push(e));
    assert_eq!(outcome, WebsocketSessionFrameOutcome::Confirm, "a chart push confirms");
}

// ── the A/B: each engine lane against the `Own` loop it replaced ────────────────────────────────

/// The sink calls the retired bars `Own` loop made, statement for statement: its seed block, then its
/// frame body as it wrote the sink itself (side A of the bars A/B).
fn own_loop_bars(seed: Vec<Bar>, frames: &[&str], symbol: &str, ctx: &DataClientContext) {
    let (venue, interval) = (super::VENUE, "1m");
    let mut bars = seed;
    let forming = if bars.len() > 1 { bars.pop() } else { None };
    ctx.sink.seed_bars(venue, symbol, interval, bars);
    if let Some(f) = forming {
        ctx.sink.bar_close_tick(venue, symbol, f.close, f.timestamp);
        ctx.sink.forming_bar(venue, symbol, interval, f);
    }
    let (mut folder, mut funding_integral) = (BarFolder::new(), FundingIntegral::new());
    for txt in frames {
        let Ok(v) = super::settle_reply(txt) else { continue };
        if let Some(funding) = parse_ticker_funding(&v, symbol, &mut funding_integral) {
            ctx.sink.funding(venue, symbol, funding);
            continue;
        }
        let Some(roll) = parse_chart_bar(&v).and_then(|bar| folder.fold(bar)) else { continue };
        if let Some(c) = roll.closed {
            ctx.sink.close_bar(venue, symbol, interval, c);
        }
        ctx.sink.bar_close_tick(venue, symbol, roll.forming.close, roll.forming.timestamp);
        ctx.sink.forming_bar(venue, symbol, interval, roll.forming);
    }
}

/// The bars lane of `symbol@1m` through the engine over ONE scripted session of `frames`, its REST seed
/// replaced by `seed` (the real one reads mainnet): the sink, the statuses written, the frames sent.
fn engine_bars(
    seed: Vec<Bar>,
    frames: &[&str],
    symbol: &str,
) -> (Arc<RecordingSink>, Vec<String>, Vec<String>) {
    let DataClientLanePlan::Session { url, subscribe, keepalive, seed: rest_seed, decode } =
        spec().plan(DataClientLane::Bars, symbol, "1m").expect("a plan")
    else {
        panic!("the bars lane is an engine session")
    };
    assert!(rest_seed.is_some(), "the lane seeds from REST");
    let seed: Box<dyn FnOnce() -> Result<Vec<Bar>, String>> = Box::new(move || Ok(seed));
    let plan = DataClientLanePlan::Session { url, subscribe, keepalive, seed: Some(seed), decode };
    let sink = Arc::new(RecordingSink::default());
    let (ctx, seen) = scripted_context(Arc::clone(&sink));
    let sent = Rc::default();
    let mut session = one_session(frames, &sent);
    let lane = DataClientLane::Bars;
    run_plan_on::<DataClientSpecDeribit, _>(plan, lane, symbol, "1m", &ctx, |_url, opts| {
        session(opts)
    });
    let seen = seen.lock().unwrap().clone();
    (sink, seen, sent.take())
}

/// A `chart.trades.BTC-PERPETUAL.1` push of the bucket opening at `tick`, closing at `close`.
fn chart(tick: i64, close: f64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","method":"subscription","params":{{"channel":"chart.trades.BTC-PERPETUAL.1","data":{{"volume":0.5,"tick":{tick},"open":1.5,"low":1.0,"high":9.0,"cost":4.0,"close":{close}}}}}}}"#
    )
}

/// **A/B: the bars lane lands on the sink, call for call and byte for byte, what its `Own` loop landed**
/// (K1): the seed (the closed bars seeded, the newest ticked and forming), a funding ticker, a
/// same-bucket update, a roll (the inferred close ALONE, then the new bucket ticked and forming), the
/// keepalive's reply, a stale push, a skipped bucket. The subscribe and the statuses are the loop's.
#[test]
fn the_bars_lane_lands_what_its_own_loop_landed() {
    let symbol = "BTC-PERPETUAL";
    let seed = vec![bar(0, 1.0), bar(60_000, 2.0), bar(120_000, 3.0)];
    let ticker = |ts: i64, rate: f64| {
        format!(
            r#"{{"jsonrpc":"2.0","method":"subscription","params":{{"channel":"ticker.BTC-PERPETUAL.agg2","data":{{"timestamp":{ts},"instrument_name":"BTC-PERPETUAL","mark_price":75980.96,"current_funding":{rate}}}}}}}"#
        )
    };
    let frames = [
        r#"{"jsonrpc":"2.0","id":1,"result":["chart.trades.BTC-PERPETUAL.1","ticker.BTC-PERPETUAL.agg2"]}"#.to_string(),
        chart(120_000, 3.5),
        ticker(1_794_153_424_816, 0.0001),
        chart(120_000, 3.25),
        chart(180_000, 4.0),
        r#"{"jsonrpc":"2.0","id":9929,"result":{"version":"1.2.26"}}"#.to_string(),
        chart(120_000, 9.0),
        ticker(1_794_153_484_816, 0.0002),
        chart(420_000, 5.0),
        chart(420_000, 5.5),
    ];
    let frames: Vec<&str> = frames.iter().map(String::as_str).collect();
    let own = Arc::new(RecordingSink::default());
    let (ctx, _) = scripted_context(Arc::clone(&own));
    own_loop_bars(seed.clone(), &frames, symbol, &ctx);
    let (engine, seen, sent) = engine_bars(seed, &frames, symbol);
    assert_eq!(format!("{:?}", engine.recorded()), format!("{:?}", own.recorded()));
    assert_eq!(own.recorded().len(), 17, "every arm was reached: {:?}", own.calls());
    assert_eq!(own.fundings().len(), 2);
    assert_eq!(own.closed_bars().len(), 2);
    let channels = [super::chart_channel(symbol, "1"), super::ticker_channel(symbol)];
    assert_eq!(sent, [super::public_subscribe_frame(&channels)]);
    assert_eq!(
        seen,
        ["LIVE · Deribit", "BTC-PERPETUAL@1m ws error (reconnecting): script exhausted"]
    );
}

/// The quotes lane through the engine: it sends the `quote.{inst}` public subscribe, the confirmed
/// session writes the ONE healthy text, the quote lands on `quote` stamped at receipt, and the fault
/// reads `"{symbol} quotes ws error (reconnecting): {e}"`.
#[test]
fn the_quotes_lane_subscribes_lands_quotes_and_reports_through_its_seam() {
    let sink = Arc::new(RecordingSink::default());
    let (ctx, seen) = scripted_context(Arc::clone(&sink));
    let sent = Rc::default();
    let ack = r#"{"jsonrpc":"2.0","id":1,"result":["quote.BTC-PERPETUAL"]}"#;
    let quote = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"quote.BTC-PERPETUAL",
        "data":{"timestamp":1550658624149,"instrument_name":"BTC-PERPETUAL","best_bid_price":3914.97,
        "best_bid_amount":40.0,"best_ask_price":3915.5,"best_ask_amount":50.0}}}"#;
    run_lane(DataClientLane::Quotes, "BTC-PERPETUAL", &ctx, &[ack, quote], &sent);
    let sub = crate::feed::decode::public_subscribe_frame(&["quote.BTC-PERPETUAL".to_string()]);
    assert_eq!(*sent.borrow(), [sub]);
    let quotes = sink.quotes();
    assert_eq!(quotes.len(), 1, "{quotes:?}");
    let (venue, symbol, q) = &quotes[0];
    assert_eq!(
        (venue.as_str(), symbol.as_str(), q.bid, q.ask),
        ("deribit", "BTC-PERPETUAL", 3914.97, 3915.5)
    );
    assert!(q.local_timestamp > 0, "stamped at receipt");
    assert_eq!(
        *seen.lock().unwrap(),
        ["LIVE · Deribit", "BTC-PERPETUAL quotes ws error (reconnecting): script exhausted"]
    );
}

/// **A quote whose frame states no venue time takes its receipt time** (owner ruling B-21): the price board
/// reads `ts`, and a `0` there is a price from 1970 that no age-bounded read trusts. A stated time is kept.
#[test]
fn a_quote_with_no_venue_time_takes_its_receipt_time() {
    let sink = Arc::new(RecordingSink::default());
    let (ctx, _seen) = scripted_context(Arc::clone(&sink));
    let sent = Rc::default();
    let quote = |stamp: &str| {
        format!(
            r#"{{"jsonrpc":"2.0","method":"subscription","params":{{"channel":"quote.BTC-PERPETUAL",
            "data":{{{stamp}"instrument_name":"BTC-PERPETUAL","best_bid_price":3914.97,
            "best_bid_amount":40.0,"best_ask_price":3915.5,"best_ask_amount":50.0}}}}}}"#
        )
    };
    let (bare, stamped) = (quote(""), quote(r#""timestamp":1550658624149,"#));
    run_lane(
        DataClientLane::Quotes,
        "BTC-PERPETUAL",
        &ctx,
        &[bare.as_str(), stamped.as_str()],
        &sent,
    );
    let quotes = sink.quotes();
    assert_eq!(quotes.len(), 2, "{quotes:?}");
    let (_, _, q) = &quotes[0];
    assert!(
        q.local_timestamp > 0 && q.timestamp == q.local_timestamp,
        "no venue time: the receipt time, {q:?}"
    );
    assert_eq!(quotes[1].2.timestamp, 1550658624149, "a venue time is kept");
}

/// The trades lane through the engine: it sends the `trades.{inst}.100ms` public subscribe, the
/// confirmed session writes the ONE healthy text, every print lands on `trade` stamped at receipt, and
/// the fault reads `"{symbol} trades ws error (reconnecting): {e}"`.
#[test]
fn the_trades_lane_subscribes_lands_prints_and_reports_through_its_seam() {
    let sink = Arc::new(RecordingSink::default());
    let (ctx, seen) = scripted_context(Arc::clone(&sink));
    let sent = Rc::default();
    let ack = r#"{"jsonrpc":"2.0","id":1,"result":["trades.BTC-PERPETUAL.100ms"]}"#;
    let prints = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"trades.BTC-PERPETUAL.100ms",
        "data":[{"trade_seq":30289432,"trade_id":"48079254","timestamp":1590484512188,"tick_direction":2,
        "price":8950.0,"mark_price":8948.9,"instrument_name":"BTC-PERPETUAL","index_price":8955.88,
        "direction":"sell","amount":10.0}]}}"#;
    run_lane(DataClientLane::Trades, "BTC-PERPETUAL", &ctx, &[ack, prints], &sent);
    let sub =
        crate::feed::decode::public_subscribe_frame(&["trades.BTC-PERPETUAL.100ms".to_string()]);
    assert_eq!(*sent.borrow(), [sub]);
    let trades = sink.trades();
    assert_eq!(trades.len(), 1, "{trades:?}");
    let (venue, symbol, t) = &trades[0];
    assert_eq!(
        (venue.as_str(), symbol.as_str(), t.price, t.size),
        ("deribit", "BTC-PERPETUAL", 8950.0, 10.0)
    );
    assert!(t.local_timestamp > 0, "stamped at receipt");
    assert_eq!(
        *seen.lock().unwrap(),
        ["LIVE · Deribit", "BTC-PERPETUAL trades ws error (reconnecting): script exhausted"]
    );
}

/// Every lane dials the MAINNET socket: the bars, quotes, trades and book sessions and the depth lane,
/// none the venue's own loop; and no lane pairs a mark.
#[test]
fn the_live_lanes_dial_the_mainnet_socket() {
    let spec = spec();
    for lane in [
        DataClientLane::Bars,
        DataClientLane::Quotes,
        DataClientLane::Trades,
        DataClientLane::Book,
        DataClientLane::Depth,
    ] {
        let url = match spec.plan(lane, "BTC-PERPETUAL", lane.word("1m")).expect("a plan") {
            DataClientLanePlan::Session { url, .. } | DataClientLanePlan::Depth { url, .. } => url,
            DataClientLanePlan::Own(_) | DataClientLanePlan::SessionOn { .. } => {
                panic!("{lane:?} dials for itself")
            }
        };
        assert_eq!(url, spec.net.ws, "{lane:?}");
    }
    assert!(spec.plan(DataClientLane::Mark, "BTC-PERPETUAL", "mark").is_err());
}

/// The sink calls the retired book `Own` loop made per frame, statement for statement (side A of the
/// book A/B). A chain gap was its session's fault, which ends side A where it ended that session.
fn own_loop_book(frames: &[&str], symbol: &str, ctx: &DataClientContext) {
    let mut book = Arc::new(L2Book::new(0.0));
    for txt in frames {
        let Ok(v) = super::settle_reply(txt) else { continue };
        if let Some((_sequence, bids, asks)) = crate::feed::decode::parse_book_snapshot(&v) {
            book = Arc::new(L2Book::new(vike_bridge_core::depth::infer_tick_size(&bids, &asks)));
        }
        match crate::feed::decode::route_frame(txt, symbol, Arc::make_mut(&mut book)) {
            DataClientEvent::BookUpdated { .. } => {
                ctx.sink.book(super::VENUE, symbol, Arc::clone(&book));
            }
            DataClientEvent::Resync => return,
            _ => {}
        }
    }
}

/// **A/B: the book lane lands on the sink, call for call and byte for byte (every level of every
/// published book), what its `Own` loop landed** (K3): each captured chain whole, and on the inverse
/// perpetual a replayed change and then a dropped one, whose broken chain ends the session in the
/// loop's words.
#[test]
fn the_book_lane_lands_what_its_own_loop_landed() {
    let ls = lines(BTC_PERPETUAL);
    let gapped = vec![ls[0], ls[1], ls[2], ls[2], ls[4]];
    let runs = [
        (
            "BTC-PERPETUAL",
            gapped,
            "BTC-PERPETUAL book change_id chain broke (frames dropped) — resyncing via a fresh subscribe",
        ),
        ("BTC_USDC-PERPETUAL", lines(BTC_USDC_PERPETUAL), "script exhausted"),
        ("BTC_USDC", lines(BTC_USDC_SPOT), "script exhausted"),
    ];
    for (symbol, frames, fault) in runs {
        let own = Arc::new(RecordingSink::default());
        let (ctx, _) = scripted_context(Arc::clone(&own));
        own_loop_book(&frames, symbol, &ctx);
        let engine = Arc::new(RecordingSink::default());
        let (ctx, seen) = scripted_context(Arc::clone(&engine));
        let sent = Rc::default();
        run_lane(DataClientLane::Book, symbol, &ctx, &frames, &sent);
        assert_eq!(format!("{:?}", engine.recorded()), format!("{:?}", own.recorded()), "{symbol}");
        assert!(own.books().len() >= 2, "{symbol}: the snapshot and a change landed");
        let channel = [crate::feed::decode::book_channel(symbol)];
        assert_eq!(*sent.borrow(), [crate::feed::decode::public_subscribe_frame(&channel)]);
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "LIVE · Deribit".to_string(),
                format!("{symbol} book ws error (reconnecting): {fault}")
            ]
        );
    }
}

/// On the release's seed the five lanes dial the very socket and seed host they dialled before the
/// addresses moved into rows, byte for byte; with no row the feed is refused, naming it.
#[test]
fn the_feed_dials_the_seeds_mainnet_rows_and_refuses_without_them() {
    let spec = spec();
    assert_eq!(spec.net.ws, "wss://www.deribit.com/ws/api/v2");
    assert_eq!(spec.net.rest, "https://www.deribit.com");
    let none = vike_venues::venue_urls::VenueUrls::from_rows("deribit", std::iter::empty());
    let why = DataClientSpecDeribit::new(&none).expect_err("refused");
    assert!(why.to_string().contains("live data ws"), "{why}");
}
