//! OFFLINE end-to-end CROSS-EXCHANGE mount test: scripted ticks on TWO venues → the PRODUCTION live
//! core (two engines, `spawn_core_multi`) → [`vike_mm::XemmMaker`] → two paper exchanges.
//!
//! No network, no credentials, no venue feature. It proves the one thing neither the pure-core tests
//! in `vike-mm` nor the lane tests in `vike-core` can prove on their own — that the whole assembly
//! agrees:
//!
//! 1. the TAKER venue's touch reaches the maker as a REFERENCE quote (a mount on hyperliquid
//!    receiving okx's book), and the maker rests a two-sided quote on the MAKER venue priced off it;
//! 2. a reference-only move RE-PRICES those quotes, with the maker venue silent — the proof that
//!    the reference lane is a real emission clock and not merely an observation;
//! 3. a paper FILL on the maker venue produces a hedge order on the TAKER venue's engine. That is
//!    the #997 regression surface end to end: before cross-venue routing the hedge collapsed back
//!    onto the maker venue, netting the position instead of hedging it, silently.
//!
//! Deterministic: every clock here is EVENT time off the scripted timestamps, and paper fills happen
//! only on the closed bars this test sends.

mod common;

use std::time::{Duration, Instant};

use vike_exec::{BarUpdate, QuoteUpdate};
use vike_model::{Bar, QuoteTick};
use vike_run::{PaperHalt, XemmMountConfig, build_paper_xemm_core_with};

/// This binary's OWN operator-HALT sentinel: a path it NAMES and never CREATES.
///
/// ⚠ **Both xemm paper books are HALT-armed by design**, so `build_paper_xemm_core` would have this
/// test inherit the kill switch off the box that runs it: while
/// `VIKE_HALT_FILE`/`<project>/settings/state/HALT` exists, both books refuse every opening submit,
/// the maker never rests a quote and this test fails on a message that mentions no halt at all.
/// MEASURED on the CI box — with `VIKE_HALT_FILE` pointing at a real file this test went red; without it,
/// green. See `vike_run::PaperHalt` for the workspace-wide measurement.
///
/// ⚠ The sentinel is a child of a `tempfile::TempDir` returned ALONGSIDE it, and the caller must
/// BIND that guard for the test's duration. The name used to be
/// `env::temp_dir().join(format!("vike-run-xemm-scripted-owns-this-halt-{pid}"))`, and while
/// nothing ever created it — so it leaked nothing — "this path does not exist" was a claim about
/// everything else on a shared `/tmp`, not about this run. A freshly-minted random root makes it a
/// property of THIS run: no stale entry can inhabit it, so the books below cannot inherit a kill
/// switch from a leftover, and the guard removes the root it created.
fn no_halt() -> (tempfile::TempDir, PaperHalt) {
    let root = tempfile::Builder::new()
        .prefix("vike-run-xemm-scripted-owns-this-halt-")
        .tempdir()
        .expect("temp sentinel root");
    let sentinel = root.path().join("HALT");
    (root, PaperHalt::Pinned(sentinel))
}

const MAKER_VENUE: &str = "hyperliquid";
const MAKER_SYMBOL: &str = "BTC";
const TAKER_VENUE: &str = "okx";
const HEDGE_SYMBOL: &str = "BTC-USDT-SWAP";
const INTERVAL: &str = "1m";
const QTY: f64 = 0.01;
const TICK: f64 = 0.5;

/// The v1 pair with the freshness bounds opened wide: this test drives event timestamps by hand and
/// a 2 s default bound would make it a clock race rather than a routing proof. Everything else keeps
/// its armed default.
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

fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
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

/// The maker venue's resting quotes, as `(side, price)` — read off the live snapshot, i.e. what an
/// operator would actually see.
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

    // `_halt_root` is the sentinel's owning temp directory — held for the whole test (see
    // `no_halt`), removed when it drops.
    let (_halt_root, halt) = no_halt();
    let mount = build_paper_xemm_core_with(&config(), &halt).expect("the v1 pair validates");
    let h = &mount.handle;

    // ── Phase 1: warm BOTH venues. The reference touch is the pricing input; the maker venue's own
    // touch is only the passive clamp's anchor. ──
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
    // Priced off the REFERENCE touch (100_000 / 100_010) backed off by edge + fee (11.5 bp), then
    // clamped inside the maker venue's own touch and snapped onto its 0.5 grid.
    // Below the reference bid by at least the required edge — the HEDGE identity. (That bound is
    // already tighter than the maker venue's own bid here, so the passive clamp is not what is
    // binding on this side; `vike-mm`'s `passive_clamp` tests pin the clamp itself.)
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

    // ── Phase 2: the REFERENCE moves and the maker venue stays silent. The quotes must follow — if
    // the reference lane were observe-only, a stale quote would sit here waiting to be picked off. ──
    send(h, TAKER_VENUE, HEDGE_SYMBOL, quote(2_000, 99_500.0, 99_510.0, HEDGE_SYMBOL));
    assert!(
        wait_until(10, || maker_quotes(h).iter().all(|&(_, px)| px != bid_px && px != ask_px)),
        "a reference-only move must re-price BOTH quotes; still at {resting:?}"
    );

    // ── Phase 3: a closed bar on the MAKER venue crosses the resting bid ⇒ a paper fill ⇒ a hedge
    // must appear on the TAKER venue's engine. ──
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

/// This binary's verdict must not change when an operator HALT sentinel is engaged —
/// `common::assert_indifferent_to_an_engaged_halt_sentinel` carries the argument and the mechanism.
#[test]
fn the_xemm_scripted_suite_is_indifferent_to_an_engaged_halt_sentinel() {
    common::assert_indifferent_to_an_engaged_halt_sentinel();
}
