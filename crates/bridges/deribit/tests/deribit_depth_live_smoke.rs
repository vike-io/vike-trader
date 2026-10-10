//! Live smoke for the DOM lane (`Feeds::subscribe_depth`) — connects to REAL Deribit mainnet
//! (public, keyless, read-only: no auth, no order, no account). `#[ignore]`d so CI never dials the
//! venue; run it explicitly to prove the production path end to end — the lane thread, the shared
//! depth driver, the `book.{inst}.100ms` fold and the `l2_snapshot` emission — against the real
//! wire, which the captured-frame unit tests in `src/market_feed_tests.rs` cannot:
//!
//! ```sh
//! cargo test -p vike-deribit --test deribit_depth_live_smoke -- --ignored --nocapture
//! ```
//!
//! Three instrument shapes are asked for, because the lane's inputs differ by shape and not by
//! name: an inverse perpetual (USD-notional contracts, a 0.5 grid), a spot pair (a 1.1 MB snapshot,
//! a 0.01 grid) and an instrument that does not exist (the venue answers `{"result":[]}` and then
//! never speaks, so the lane must publish nothing and still stop on request).

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_data::{DataClient, RecordingSink};
use vike_deribit::market_feed::Feeds;

// Polls every 100 ms, unlike the 10 ms twin
// `crates/vike-tradehub/src/server/tests/server_link_liveness.rs`'s `wait_until`.
/// Poll `done` every 100 ms until it holds or `within` passes; the last answer is the verdict.
fn wait_until(within: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    done()
}

/// Subscribe `symbol`'s DOM lane, wait for `want` snapshots, stop it, and hand back the sink — so a
/// test reads what the lane actually published rather than what it was told.
fn stream(symbol: &str, want: usize, within: Duration) -> (Arc<RecordingSink>, bool, Duration) {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink.clone(), || {});
    let id = feeds.subscribe_depth(symbol).expect("subscribe_depth spawns the lane");
    let flowing = wait_until(within, || sink.l2_snapshots().len() >= want);
    // The stop must JOIN: the lane notices the flag within one read-timeout tick (or a backoff
    // slice), so this is the "a refused/quiet lane still stops on request" proof as well.
    let stopping = Instant::now();
    feeds.unsubscribe(id);
    let stopped_in = stopping.elapsed();
    feeds.shutdown();
    (sink, flowing, stopped_in)
}

#[test]
#[ignore = "hits real Deribit mainnet WS; run explicitly with --ignored"]
fn the_dom_lane_streams_a_live_ladder_for_the_inverse_perpetual() {
    let (sink, flowing, stopped_in) = stream("BTC-PERPETUAL", 3, Duration::from_secs(20));
    assert!(flowing, "no three l2 snapshots from BTC-PERPETUAL within 20 s");
    let snaps = sink.l2_snapshots();
    let (venue, symbol, tick, bids, asks, ts) = snaps.last().expect("a snapshot");
    println!(
        "{} snapshots; last: tick {tick}, {} bids / {} asks, best {:?} / {:?}, ts {ts}; stopped in {stopped_in:?}",
        snaps.len(),
        bids.len(),
        asks.len(),
        bids.first(),
        asks.first()
    );
    assert_eq!((venue.as_str(), symbol.as_str()), ("deribit", "BTC-PERPETUAL"));
    assert!((*tick - 0.5).abs() < 1e-9, "BTC-PERPETUAL trades on a 0.5 grid, got {tick}");
    assert!((50..=200).contains(&bids.len()), "bids published: {}", bids.len());
    assert!((50..=200).contains(&asks.len()), "asks published: {}", asks.len());
    assert!(bids[0].price < asks[0].price, "a real resting book is not crossed");
    assert!(bids.windows(2).all(|w| w[0].price > w[1].price), "bids descend from the best");
    assert!(asks.windows(2).all(|w| w[0].price < w[1].price), "asks ascend from the best");
    assert!(bids.iter().chain(asks).all(|l| l.qty > 0.0), "a published level has a size");
    assert!(*ts > 0, "stamped");
    assert!(stopped_in < Duration::from_secs(8), "the stop joined in {stopped_in:?}");
}

#[test]
#[ignore = "hits real Deribit mainnet WS; run explicitly with --ignored"]
fn the_dom_lane_streams_a_live_ladder_for_a_spot_pair() {
    // The spot snapshot is 1.1 MB (23,912 bids / 18,450 asks measured 2026-10-04): give it room.
    let (sink, flowing, stopped_in) = stream("BTC_USDC", 3, Duration::from_secs(30));
    assert!(flowing, "no three l2 snapshots from BTC_USDC within 30 s");
    let snaps = sink.l2_snapshots();
    let (venue, symbol, tick, bids, asks, _) = snaps.last().expect("a snapshot");
    println!(
        "{} snapshots; last: tick {tick}, {} bids / {} asks; stopped in {stopped_in:?}",
        snaps.len(),
        bids.len(),
        asks.len()
    );
    assert_eq!((venue.as_str(), symbol.as_str()), ("deribit", "BTC_USDC"));
    assert!((*tick - 0.01).abs() < 1e-6, "BTC_USDC trades on a 0.01 grid, got {tick}");
    // A deep spot book: the lane publishes its DEPTH_LEVELS cap, not the whole of it.
    assert_eq!((bids.len(), asks.len()), (200, 200), "capped at the lane's top-N");
    assert!(bids[0].price < asks[0].price);
    assert!(stopped_in < Duration::from_secs(8), "the stop joined in {stopped_in:?}");
}

#[test]
#[ignore = "hits real Deribit mainnet WS; run explicitly with --ignored"]
fn a_refused_instrument_publishes_nothing_and_still_stops_on_request() {
    let (sink, flowing, stopped_in) = stream("NOT-AN-INSTRUMENT", 1, Duration::from_secs(8));
    assert!(!flowing, "a book for an instrument that does not exist");
    assert!(sink.l2_snapshots().is_empty());
    assert!(stopped_in < Duration::from_secs(8), "the stop joined in {stopped_in:?}");
}
