use super::{
    BarFolder, BarRoll, DEPTH_LEVELS, FeedCtx, Feeds, KEEPALIVE_PING, decode_depth_frame,
    fold_depth_frame, frame_ts_ms, publish_book, subscribe_refusal,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vike_bridge_core::depth::{BookOp, FAULT_LOG_EVERY};
use vike_bridge_core::klines::kline_to_bar;
use vike_data::{DataClient, RecordingSink};
use vike_model::{BookLevel, L2Book};

/// A network-free stand-in for a lane main: polls its own stop flag at the real feeds' WS
/// cadence without ever touching a socket — the shared lifecycle-test idiom.
fn fake_feed_body(_symbol: String, _interval: String, ctx: FeedCtx) {
    while !ctx.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn subscribe_returns_distinct_ids() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let id1 = feeds.spawn_with("BTC-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
    let id2 = feeds.spawn_with("ETH-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
    assert_ne!(id1, id2, "distinct ids per subscribe");
    feeds.shutdown();
}

#[test]
fn unsubscribe_stops_only_that_one_feed_others_keep_running() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let id1 = feeds.spawn_with("BTC-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
    let id2 = feeds.spawn_with("ETH-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");

    feeds.unsubscribe(id1);
    assert_eq!(feeds.registry.len(), 1, "only the unsubscribed stream is removed");
    assert!(feeds.registry.contains(id2), "the other subscription keeps running");

    feeds.shutdown();
    assert!(feeds.registry.is_empty());
}

#[test]
fn unsubscribe_of_an_unknown_id_is_a_no_op() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let id = feeds.spawn_with("BTC-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
    feeds.unsubscribe(vike_data::SubscriptionId(id.0 + 100)); // never issued
    assert_eq!(feeds.registry.len(), 1, "unknown id must not disturb the real subscription");
    feeds.shutdown();
}

#[test]
fn shutdown_joins_every_feed() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    feeds.spawn_with("BTC-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
    feeds.spawn_with("ETH-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
    feeds.spawn_with("SOL_USDC-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
    feeds.shutdown(); // must return only once every thread has actually joined
    assert!(feeds.registry.is_empty());
}

/// The depth lane is DECLARED, so the gate every consumer reads — `vike_data::require_live_verb`,
/// which the datahub's `MdLane::Depth` and the desktop's `MdSession::want` both call — admits it.
/// ⚠ `subscribe_depth` itself is not called here: it spawns the real thread, which would dial the
/// public mainnet host from a test (this file's other lane tests stop at `spawn_with` for the same
/// reason). The decode and the emission it runs are driven below, over captured frames.
#[test]
fn depth_is_served_through_the_declared_row() {
    assert!(vike_data::require_live_verb("deribit", vike_model::LiveVerb::Depth).is_ok());
    assert!(vike_model::caps_for("deribit").live_data.depth);
}

/// The keepalive payload is a well-formed `public/test` JSON-RPC request — the frame whose
/// reply resets the idle watchdog on a quiet channel (the row declares the cadence).
#[test]
fn keepalive_is_a_public_test_rpc() {
    let v: serde_json::Value = serde_json::from_str(KEEPALIVE_PING).unwrap();
    assert_eq!(v["method"], "public/test");
}

/// `pump_opts` CONSUMES the venue's pump_spec row (row ownership): the subscribe payload and
/// the keepalive text are the venue's; every timing knob is the row's.
#[test]
fn pump_opts_consume_the_declared_row() {
    let sub = super::public_subscribe_frame(&["book.BTC-PERPETUAL.100ms".to_string()]);
    let opts = super::pump_opts(&sub);
    assert_eq!(opts.subscribe, Some(sub.as_str()));
    let knobs = vike_bridge_core::pump_spec::market_pump_spec("deribit").knobs();
    let ka = opts.keepalive.expect("the row declares a keepalive cadence");
    assert_eq!(ka.payload, KEEPALIVE_PING);
    assert_eq!(Some(ka.every), knobs.keepalive_every);
    assert_eq!(opts.read_timeout, knobs.read_timeout);
    assert_eq!(opts.idle_threshold, knobs.idle_threshold);
    assert_eq!(opts.connect_timeout, knobs.connect_timeout);
}

// ── BarFolder: the closed-bar inference ─────────────────────────────────────────────────────

fn bar(ts: i64, close: f64) -> vike_model::Bar {
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
const BTC_PERPETUAL: &str = include_str!("../tests/fixtures/book_depth/btc_perpetual.jsonl");
const BTC_USDC_PERPETUAL: &str =
    include_str!("../tests/fixtures/book_depth/btc_usdc_perpetual.jsonl");
const BTC_USDC_SPOT: &str = include_str!("../tests/fixtures/book_depth/btc_usdc_spot.jsonl");
/// The venue's real answer to `book.NOT-AN-INSTRUMENT.100ms`: an empty `result`, then silence.
const MATCHED_NO_CHANNEL: &str =
    include_str!("../tests/fixtures/book_depth/subscribe_matched_no_channel.jsonl");

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
fn fold_all(fixture: &'static str, sym: &str) -> (Option<L2Book>, Vec<BookOp>) {
    let mut book = None;
    let ops = lines(fixture).into_iter().map(|l| fold_depth_frame(l, sym, &mut book)).collect();
    (book, ops)
}

/// Two values within `tol`. The book quantizes on an INFERRED tick — the smallest gap the snapshot
/// shows, as the f64 SUBTRACTION of two decimals, so on a `0.1` grid at 85,000 it is
/// `0.09999999999127`, not `0.1`. The tick itself is then right to ~1e-10, but a published price is
/// `grid index × tick`, and the index is ~850,000: MEASURED on the lane, `85217.4` came back as
/// `85217.39999256` (7.4e-6 off). Every sibling's depth lane has the same property (the shared
/// `vike_bridge_core::depth::infer_tick_size`); a consumer re-quantizes on the tick it is handed.
fn near(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() < tol
}

/// Instrument-name routing, against what the venue ACTUALLY named. For each of the three shapes the
/// channel this crate builds is the one the venue's own ack and its notifications carry, and the
/// notification's `instrument_name` is the symbol subscribed — so the underscore in a USDC name
/// (`BTC_USDC-PERPETUAL`, `BTC_USDC`) is carried verbatim and nothing in the path rewrites it.
#[test]
fn every_instrument_shape_is_asked_for_by_the_channel_the_venue_named() {
    for (instrument, fixture) in INSTRUMENTS {
        let ls = lines(fixture);
        let want = crate::market_data::book_channel(instrument);
        let ack: serde_json::Value = serde_json::from_str(ls[0]).unwrap();
        assert_eq!(ack["result"][0], want, "{instrument}: the ack names the channel we ask for");
        let sub: serde_json::Value = serde_json::from_str(
            &crate::market_data::public_subscribe_frame(std::slice::from_ref(&want)),
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
    assert_eq!(ops[0], BookOp::Ignored, "the subscribe ack is not a book frame");
    assert_eq!(ops[1], BookOp::Updated(1_791_120_409_417), "the snapshot carries the venue time");
    assert_eq!(ops[5], BookOp::Updated(1_791_120_410_684));
    assert!(ops[2..].iter().all(|op| matches!(op, BookOp::Updated(_))), "{ops:?}");
    let book = book.expect("the snapshot anchored the book");
    assert_eq!(book.tick_size, 0.5, "the grid inferred from the snapshot's own levels");
    assert_eq!(book.last_seq, 177_146_680_351, "anchored on the LAST change's id");
    assert_eq!(book.best_bid(), Some(BookLevel::new(85_217.5, 2_000.0)), "`2.0e3` parses");
    assert_eq!(book.best_ask(), Some(BookLevel::new(85_218.0, 174_870.0)));
    assert_eq!(book.bid_qty_at(85_209.0), 10_780.0, "a `change` replaced the snapshot's 1940");
    assert_eq!(book.bid_qty_at(84_301.5), 380.0, "a `new` bid joined");
    assert_eq!(book.ask_qty_at(85_236.0), 300_000.0, "`3.0e5` parses");
    // 85239.0 was ADDED by the second change and DELETED by the third: gone, not a zero level.
    assert_eq!(book.ask_qty_at(85_239.0), 0.0);
    // 85240.0 was added by the first change and re-`new`ed by the fourth: the later value stands.
    assert_eq!(book.ask_qty_at(85_240.0), 7_790.0);
    assert_eq!(book.ask_qty_at(85_240.5), 19_600.0, "`1.96e4` parses");
}

/// A linear USDC perpetual: fractional base-coin amounts, a 0.1 grid, and `bids: []` on a change
/// (a side the venue did not touch) — which is still a chained frame and still folds.
#[test]
fn a_linear_usdc_perpetual_folds_on_its_own_grid_and_an_untouched_side_is_not_a_fault() {
    let (book, ops) = fold_all(BTC_USDC_PERPETUAL, "BTC_USDC-PERPETUAL");
    assert_eq!(ops[0], BookOp::Ignored);
    assert!(ops[1..].iter().all(|op| matches!(op, BookOp::Updated(_))), "{ops:?}");
    let book = book.expect("anchored");
    assert!(near(book.tick_size, 0.1, 1e-6), "inferred tick {}", book.tick_size);
    assert_eq!(book.last_seq, 200_328_940_309);
    let best = book.best_bid().expect("a best bid");
    assert!(near(best.price, 85_217.4, 1e-4), "{best:?}");
    assert_eq!(best.qty, 1.2191);
    assert_eq!(book.ask_qty_at(85_217.5), 1.719, "a `change` on the best ask");
    // 127826.3 was ADDED by the second change and DELETED by the third.
    assert_eq!(book.ask_qty_at(127_826.3), 0.0);
}

/// A spot pair: a 0.01 grid, amounts down to a satoshi (`6.563e-5`), and `delete`s that land on
/// levels the snapshot actually held.
#[test]
fn a_spot_pair_folds_on_its_own_grid_and_deletes_remove_what_the_snapshot_held() {
    let (book, ops) = fold_all(BTC_USDC_SPOT, "BTC_USDC");
    assert_eq!(ops[0], BookOp::Ignored);
    assert!(ops[1..].iter().all(|op| matches!(op, BookOp::Updated(_))), "{ops:?}");
    let book = book.expect("anchored");
    assert!(near(book.tick_size, 0.01, 1e-6), "inferred tick {}", book.tick_size);
    assert_eq!(book.bid_qty_at(85_197.0), 6.563e-5, "the exponent spelling parses exactly");
    assert_eq!(
        book.bid_qty_at(85_195.5),
        0.0,
        "the first change deleted a level the snapshot held"
    );
    assert_eq!(book.bid_qty_at(85_193.75), 0.0, "added by the second change, deleted by the third");
    assert_eq!(book.bid_qty_at(85_197.01), 0.16209971);
    assert_eq!(book.ask_qty_at(85_197.02), 0.04876869, "the LAST change's value stands");
}

/// A frame dropped between two changes is a [`BookOp::Gap`] — never a fold — so the driver
/// reconnects and re-seeds; the book it leaves behind is exactly the last good one.
#[test]
fn a_dropped_frame_is_a_gap_and_the_book_is_untouched() {
    let ls = lines(BTC_PERPETUAL);
    let mut book = None;
    assert!(matches!(fold_depth_frame(ls[1], "BTC-PERPETUAL", &mut book), BookOp::Updated(_)));
    // ls[2] (the first change) never arrives; ls[3] chains to it, not to the snapshot.
    assert_eq!(fold_depth_frame(ls[3], "BTC-PERPETUAL", &mut book), BookOp::Gap);
    let book = book.unwrap();
    assert_eq!(book.last_seq, 177_146_678_834, "the anchor did not move across the gap");
    assert_eq!(book.bid_qty_at(85_202.0), 0.0, "…and nothing from the unchained frame was folded");
}

/// A frame the book already reflects is ignored — NOT a gap, even though its `prev_change_id` no
/// longer matches the advanced anchor (the check-order rule in `market_data`'s module doc).
#[test]
fn a_replayed_frame_is_ignored_not_a_gap() {
    let ls = lines(BTC_PERPETUAL);
    let mut book = None;
    fold_depth_frame(ls[1], "BTC-PERPETUAL", &mut book);
    assert!(matches!(fold_depth_frame(ls[2], "BTC-PERPETUAL", &mut book), BookOp::Updated(_)));
    assert_eq!(fold_depth_frame(ls[2], "BTC-PERPETUAL", &mut book), BookOp::Ignored);
    assert_eq!(book.unwrap().last_seq, 177_146_679_077);
}

/// A change that outruns its anchor — the session's first frame is not a snapshot — builds nothing.
#[test]
fn a_change_before_its_snapshot_anchors_nothing() {
    let ls = lines(BTC_PERPETUAL);
    let mut book = None;
    assert_eq!(fold_depth_frame(ls[2], "BTC-PERPETUAL", &mut book), BookOp::Ignored);
    assert!(book.is_none(), "an unanchored change must not conjure a book");
}

/// A second snapshot mid-session re-anchors: the book is REPLACED, not merged.
#[test]
fn a_second_snapshot_replaces_the_book() {
    let ls = lines(BTC_PERPETUAL);
    let mut book = None;
    fold_depth_frame(ls[1], "BTC-PERPETUAL", &mut book);
    fold_depth_frame(ls[2], "BTC-PERPETUAL", &mut book);
    assert_eq!(book.as_ref().unwrap().ask_qty_at(85_240.0), 13_000.0, "the change added it");
    assert!(matches!(fold_depth_frame(ls[1], "BTC-PERPETUAL", &mut book), BookOp::Updated(_)));
    let book = book.unwrap();
    assert_eq!(book.last_seq, 177_146_678_834);
    assert_eq!(book.ask_qty_at(85_240.0), 0.0, "re-anchored: the earlier change is gone");
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
        BookOp::Updated(1_791_120_409_417)
    );
    let b = book.as_ref().unwrap();
    assert_eq!(b.best_ask(), None);
    assert_eq!(b.ask_levels(), 0);
    assert_eq!(b.bid_levels(), 2);
    let (bids, asks) = b.top_n(DEPTH_LEVELS);
    assert_eq!((bids.len(), asks.len()), (2, 0));

    let mut empty = None;
    assert!(matches!(fold_depth_frame(EMPTY, "BTC-PERPETUAL", &mut empty), BookOp::Updated(_)));
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
    let sym = "NOT-AN-INSTRUMENT";
    let (mut book, mut spoke) = (None, None);
    let t0 = 1_000_000;
    assert_eq!(decode_depth_frame(refusal, sym, &mut book, &mut spoke, t0), BookOp::Gap);
    assert_eq!(spoke, Some(t0), "the first refusal speaks");
    let soon = t0 + window - 1;
    assert_eq!(decode_depth_frame(refusal, sym, &mut book, &mut spoke, soon), BookOp::Gap);
    assert_eq!(spoke, Some(t0), "…inside the window the verdict is the same but nothing is said");
    let later = t0 + window;
    assert_eq!(decode_depth_frame(refusal, sym, &mut book, &mut spoke, later), BookOp::Gap);
    assert_eq!(spoke, Some(later), "…and a window later it speaks again");
    assert!(book.is_none());
}

/// The refusal check is for the unanchored window only: an ack passes, and once the book is
/// anchored a stray reply is just an ignored frame — never a gap that throws a good book away.
#[test]
fn an_ack_passes_and_an_anchored_book_is_never_gapped_by_a_reply() {
    let ls = lines(BTC_PERPETUAL);
    let sym = "BTC-PERPETUAL";
    let (mut book, mut spoke) = (None, None);
    assert_eq!(decode_depth_frame(ls[0], sym, &mut book, &mut spoke, 5), BookOp::Ignored);
    assert!(matches!(decode_depth_frame(ls[1], sym, &mut book, &mut spoke, 6), BookOp::Updated(_)));
    let refusal = MATCHED_NO_CHANNEL.trim();
    assert_eq!(decode_depth_frame(refusal, sym, &mut book, &mut spoke, 7), BookOp::Ignored);
    assert_eq!(spoke, None, "nothing was said");
    assert!(book.is_some(), "the anchored book survived");
}

/// The emission: the top [`DEPTH_LEVELS`] per side, best first, on the book's own tick, stamped at
/// receipt, with one repaint nudge. 250 levels a side in, 200 out.
#[test]
fn publish_emits_the_top_levels_best_first_on_the_books_tick() {
    let rec = Arc::new(RecordingSink::default());
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = Arc::clone(&wakes);
    let ctx = FeedCtx {
        sink: rec.clone(),
        status: Arc::new(Mutex::new(String::new())),
        wake: Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }),
        stop: Arc::new(AtomicBool::new(false)),
    };
    let bids: Vec<BookLevel> = (0..250_i32)
        .map(|i| BookLevel::new(60_000.0 - 0.5 * f64::from(i), 1.0 + f64::from(i)))
        .collect();
    let asks: Vec<BookLevel> = (0..250_i32)
        .map(|i| BookLevel::new(60_000.5 + 0.5 * f64::from(i), 1.0 + f64::from(i)))
        .collect();
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(1, &bids, &asks);

    publish_book(&book, "BTC-PERPETUAL", &ctx);

    let snaps = rec.l2_snapshots();
    assert_eq!(snaps.len(), 1);
    let (venue, symbol, tick, b, a, ts) = &snaps[0];
    assert_eq!((venue.as_str(), symbol.as_str(), *tick), ("deribit", "BTC-PERPETUAL", 0.5));
    assert_eq!((b.len(), a.len()), (DEPTH_LEVELS, DEPTH_LEVELS), "capped at DEPTH_LEVELS a side");
    assert_eq!(b[0], BookLevel::new(60_000.0, 1.0), "bids descend from the best");
    assert_eq!(b[1], BookLevel::new(59_999.5, 2.0));
    assert_eq!(a[0], BookLevel::new(60_000.5, 1.0), "asks ascend from the best");
    assert_eq!(a[1], BookLevel::new(60_001.0, 2.0));
    assert!(*ts > 0, "stamped at receipt");
    assert_eq!(wakes.load(Ordering::SeqCst), 1, "one repaint nudge");
}

/// The idle watchdog the depth lane consumes is the pump row's. ⚠ The lane reads `None` as "no
/// watchdog" (`Duration::MAX`), so a row that stopped declaring one would silently disarm it here.
#[test]
fn the_pump_row_declares_the_idle_watchdog_the_depth_lane_consumes() {
    let knobs = vike_bridge_core::pump_spec::market_pump_spec("deribit").knobs();
    assert!(knobs.idle_threshold.is_some(), "deribit's row must declare an idle watchdog");
}
