//! OFFLINE end-to-end CROSS-EXCHANGE mount test: scripted ticks on TWO venues → the PRODUCTION live
//! core (two engines, `spawn_core_multi`) → [`vike_mm::XemmMaker`] → two paper exchanges.
//!
//! No network, credentials or venue feature. Proves what neither `vike-mm`'s pure-core tests nor
//! `vike-core`'s lane tests can alone — that the whole assembly agrees:
//!
//! 1. the TAKER venue's touch reaches the maker as a REFERENCE quote, and the maker rests a
//!    two-sided quote on the MAKER venue priced off it;
//! 2. a reference-only move RE-PRICES those quotes with the maker venue silent: the reference lane
//!    is an emission clock, not merely an observation;
//! 3. a paper FILL on the maker venue produces a hedge on the TAKER venue's engine — the #997
//!    surface: without cross-venue routing the hedge silently NETS on the maker venue.
//!
//! Deterministic: every clock is EVENT time, and paper fills happen only on bars this test closes.

mod common;

use common::wait_until;
use vike_exec::{BarUpdate, QuoteUpdate};
use vike_model::{Bar, QuoteTick};
use vike_mount::{PaperHalt, XemmMountConfig, build_paper_xemm_core_with};

/// This binary's OWN operator-HALT sentinel: a path it NAMES and never CREATES.
///
/// ⚠ **Both xemm paper books are HALT-armed by design**, so `build_paper_xemm_core` inherits the
/// box's kill switch: while `<project>/settings/state/HALT` exists no quote rests and the test
/// fails on a message naming no halt. MEASURED on the CI box: red with a real file there, green without
/// (`vike_mount::PaperHalt` has the workspace-wide measurement).
///
/// ⚠ The caller must BIND the returned `tempfile::TempDir` for the test's duration
/// (`common::no_halt` carries why).
fn no_halt() -> (tempfile::TempDir, PaperHalt) {
    common::no_halt("vike-mount-xemm-scripted-owns-this-halt-")
}

const MAKER_VENUE: &str = "hyperliquid";
const MAKER_SYMBOL: &str = "BTC";
const TAKER_VENUE: &str = "okx";
const HEDGE_SYMBOL: &str = "BTC-USDT-SWAP";
const INTERVAL: &str = "1m";
const QTY: f64 = 0.01;
const TICK: f64 = 0.5;

/// The v1 pair with freshness bounds opened wide (a 2 s bound over hand-driven timestamps would
/// make this a clock race, not a routing proof); everything else keeps its armed default.
fn config() -> XemmMountConfig {
    let mut cfg = XemmMountConfig::crypto(
        MAKER_VENUE,
        MAKER_SYMBOL,
        TAKER_VENUE,
        HEDGE_SYMBOL,
        QTY,
        0.0005,
        TICK,
    );
    cfg.freshness = Some((600_000, 600_000, 600_000));
    cfg
}

fn quote(ts: i64, bid: f64, ask: f64, symbol: &str) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: 0,
        bid,
        ask,
        bid_size: 5.0,
        ask_size: 5.0,
        symbol: symbol.to_string(),
    }
}

fn send(handle: &vike_core::CoreHandle, venue: &str, symbol: &str, q: QuoteTick) {
    handle
        .tick_sender()
        .quote(QuoteUpdate { venue: venue.into(), symbol: symbol.into(), quote: q })
        .unwrap();
}

/// The maker venue's resting quotes as `(side, price)`, off the live snapshot an operator sees.
fn maker_quotes(handle: &vike_core::CoreHandle) -> Vec<(i32, f64)> {
    let mut v: Vec<(i32, f64)> = handle
        .snapshot()
        .orders
        .iter()
        .filter(|o| o.venue == MAKER_VENUE && o.order_type == "limit" && o.status.is_live())
        .map(|o| (o.side, o.price.unwrap_or(0.0)))
        .collect();
    v.sort_by_key(|&(side, _)| side);
    v
}

/// Everything that reached the TAKER venue's engine, as `(symbol, side, qty)`.
fn taker_orders(handle: &vike_core::CoreHandle) -> Vec<(String, i32, f64)> {
    handle
        .snapshot()
        .orders
        .iter()
        .filter(|o| o.venue == TAKER_VENUE)
        .map(|o| (o.symbol.clone(), o.side, o.qty))
        .collect()
}

#[test]
fn a_cross_venue_maker_quotes_off_the_reference_and_hedges_on_the_taker_venue() {
    vike_log::test_init();

    // The sentinel's owning temp directory, held for the whole test (see `no_halt`).
    let (_halt_root, halt) = no_halt();
    let mount = build_paper_xemm_core_with(&config(), &halt).expect("the v1 pair validates");
    let h = &mount.handle;

    // ---- Phase 1: warm BOTH venues (the reference prices; the maker anchors the clamp) ----
    send(h, TAKER_VENUE, HEDGE_SYMBOL, quote(1_000, 100_000.0, 100_010.0, HEDGE_SYMBOL));
    send(h, MAKER_VENUE, MAKER_SYMBOL, quote(1_000, 99_990.0, 100_000.0, MAKER_SYMBOL));

    assert!(
        wait_until(10, || maker_quotes(h).len() == 2),
        "the maker never rested a two-sided quote on {MAKER_VENUE}: {:?}",
        maker_quotes(h)
    );
    let resting = maker_quotes(h);
    let (ask_px, bid_px) = (resting[0].1, resting[1].1);
    assert_eq!(resting[0].0, -1, "sorted ask first");
    assert_eq!(resting[1].0, 1, "then bid");
    // Priced off the REFERENCE touch (100_000 / 100_010) backed off by edge + fee (11.5 bp),
    // clamped inside the maker venue's touch, snapped to its 0.5 grid. The edge bound (the HEDGE
    // identity) binds here, not the clamp; `vike-mm`'s `passive_clamp` tests pin the clamp.
    assert!(
        bid_px < 100_000.0 * (1.0 - 0.0005),
        "the bid must sit below the reference bid by at least the required edge: {bid_px}"
    );
    assert!(
        bid_px < 100_000.0 - TICK,
        "and strictly inside the maker venue's own ask — never marketable: {bid_px}"
    );
    assert!(
        ask_px > 100_010.0 * (1.0 + 0.0005),
        "the ask must sit above the reference ask by at least the required edge: {ask_px}"
    );
    assert!(
        taker_orders(h).is_empty(),
        "nothing belongs on the taker venue before a fill: {:?}",
        taker_orders(h)
    );

    // ---- Phase 2: only the REFERENCE moves; the quotes follow (a stale one is picked off) ----
    send(h, TAKER_VENUE, HEDGE_SYMBOL, quote(2_000, 99_500.0, 99_510.0, HEDGE_SYMBOL));
    assert!(
        wait_until(10, || maker_quotes(h).iter().all(|&(_, px)| px != bid_px && px != ask_px)),
        "a reference-only move must re-price BOTH quotes; still at {resting:?}"
    );

    // ---- Phase 3: a MAKER bar crosses the bid ⇒ paper fill ⇒ a hedge on the TAKER engine ----
    let bid_now = maker_quotes(h).iter().find(|&&(s, _)| s == 1).expect("a resting bid").1;
    h.bar_sender()
        .close(BarUpdate {
            venue: MAKER_VENUE.into(),
            symbol: MAKER_SYMBOL.into(),
            interval: INTERVAL.into(),
            bar: Bar {
                ts: 60_000,
                open: 99_600.0,
                high: 99_650.0,
                // straddle the resting bid so the maker's BUY fills
                low: bid_now - 10.0,
                close: 99_600.0,
                volume: 1.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            },
        })
        .unwrap();

    assert!(
        wait_until(10, || !mount.maker_fills.lock().unwrap().is_empty()),
        "the resting bid never filled on the maker venue's paper book"
    );
    assert!(
        wait_until(10, || !taker_orders(h).is_empty()),
        "THE #997 REGRESSION SURFACE: a maker fill produced no order on {TAKER_VENUE}. Before \
         cross-venue routing the hedge collapsed back onto the maker venue and NETTED the position \
         instead of hedging it, with no error anywhere."
    );

    let hedges = taker_orders(h);
    assert_eq!(hedges.len(), 1, "exactly one hedge, got {hedges:?}");
    let (symbol, side, qty) = &hedges[0];
    assert_eq!(symbol, HEDGE_SYMBOL, "the hedge names the TAKER venue's instrument");
    assert_eq!(*side, -1, "a maker BUY is hedged by a taker SELL");
    assert_eq!(qty.to_bits(), QTY.to_bits(), "fully hedged at ratio 1.0, bit-for-bit");
    // ...and nothing extra landed on the maker venue: the hedge did NOT double back.
    assert!(
        !mount.maker_fills.lock().unwrap().iter().any(|f| f.side < 0),
        "a SELL on the maker venue means the hedge routed to the wrong engine"
    );

    mount.handle.shutdown_and_join();
}

/// The verdict must not change under an engaged HALT sentinel
/// (`common::assert_indifferent_to_an_engaged_halt_sentinel`).
#[test]
fn the_xemm_scripted_suite_is_indifferent_to_an_engaged_halt_sentinel() {
    common::assert_indifferent_to_an_engaged_halt_sentinel();
}
