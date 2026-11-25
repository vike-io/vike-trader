//! OFFLINE end-to-end CROSS-EXCHANGE mount test: scripted ticks on TWO venues → the PRODUCTION live
//! core (two engines, `spawn_core_multi`) → the strategy file's `XemmMaker`
//! (`strategies/market_making/cross_exchange_maker.rs`) → two paper exchanges.
//!
//! No network, credentials or venue feature. Proves what neither `vike-market-making`'s pure-core tests nor
//! `vike-core`'s lane tests can alone — that the whole assembly agrees:
//!
//! 1. the TAKER venue's touch reaches the maker as a REFERENCE quote, and the maker rests a
//!    two-sided quote on the MAKER venue priced off it;
//! 2. a reference-only move RE-PRICES those quotes with the maker venue silent: the reference lane
//!    is an emission clock, not merely an observation;
//! 3. a paper FILL on the maker venue produces a hedge on the TAKER venue's engine — the #997
//!    surface: without cross-venue routing the hedge silently NETS on the maker venue.
//!
//! Run over two pairs: the v1 pair of two names (hyperliquid `BTCUSDC.P` × okx `BTC-USDT-SWAP`), and ONE user
//! name on two venues (binance × bybit `BTCUSDT.P`), whose hedge leg is named by its account,
//! `BTCUSDT.P@bybit` (decision 0293): each leg's orders reach its own venue's book, and the hedge fill
//! comes back under the leg's name, so the maker hedges once and never again.
//!
//! Deterministic: every clock is EVENT time, and a paper fill happens only on a quote this test sends
//! (the paper book's clock, decision 0285's amendment of 2026-11-16).

use crate::common;

use std::time::Duration;
use vike_execution::QuoteUpdate;
use vike_log::poll::wait_until;
use vike_marketdata::TickQuote;
use vike_model::XemmParams;
use vike_strategy_lab::makers::cross_exchange_maker::XemmMaker;
use vike_tradehub_core::{MountPlacement, build_paper_xemm_core_with, xemm_mount, xemm_params};

/// The prefix of this binary's OWN operator-HALT sentinel root (`common::no_halt`): a path it NAMES
/// and never CREATES.
///
/// ⚠ **Both xemm paper books are HALT-armed by design**, so `build_paper_xemm_core` inherits the
/// box's kill switch: while `<project>/settings/state/HALT` exists no quote rests and the test
/// fails on a message naming no halt. MEASURED on the CI box: red with a real file there, green without
/// (`vike_tradehub_core::StrategyCorePaperHalt` has the workspace-wide measurement).
///
/// ⚠ A caller must BIND the `tempfile::TempDir` that `common::no_halt` returns for the test's
/// duration (`common::no_halt` carries why).
const HALT_ROOT_PREFIX: &str = "vike-tradehub-core-xemm-scripted-owns-this-halt-";

const QTY: f64 = 0.01;
const TICK: f64 = 0.5;

/// One cross-exchange pair: the maker's venue and symbol, the taker's venue, the hedge leg's NAME (what
/// the maker orders and hears it under) and its symbol on the taker venue.
struct Pair {
    maker_venue: &'static str,
    maker_symbol: &'static str,
    taker_venue: &'static str,
    hedge_name: &'static str,
    hedge_symbol: &'static str,
}

/// The v1 pair: two names.
const V1: Pair = Pair {
    maker_venue: "hyperliquid",
    maker_symbol: "BTCUSDC.P",
    taker_venue: "okx",
    hedge_name: "BTC-USDT-SWAP",
    hedge_symbol: "BTC-USDT-SWAP",
};

/// ONE user name on two venues: the hedge is named by its account (decision 0293).
const SAME_NAME: Pair = Pair {
    maker_venue: "binance",
    maker_symbol: "BTCUSDT.P",
    taker_venue: "bybit",
    hedge_name: "BTCUSDT.P@bybit",
    hedge_symbol: "BTCUSDT.P",
};

/// The pair's mount and its tunables with freshness bounds opened wide (a 2 s bound over
/// hand-driven timestamps would make this a clock race, not a routing proof); everything else keeps
/// its armed default.
fn config(pair: &Pair) -> (MountPlacement, XemmParams) {
    let spec =
        xemm_mount(pair.maker_venue, pair.maker_symbol, pair.taker_venue, pair.hedge_name, TICK);
    let params = XemmParams {
        max_ref_age_ms: 600_000,
        max_own_touch_age_ms: 600_000,
        max_emission_gap_ms: 600_000,
        ..xemm_params(QTY, 0.0005)
    };
    (spec, params)
}

/// The maker the mount is handed (no library builds a strategy): the strategy file's `XemmMaker`
/// of a validated mount at `total_fee`, `params` whole with the two mount facts written over it
/// (the round trip and the mount's `tick_size`), applied once over `XemmMaker::new`. The hedge is
/// handed by its leg's NAME, the one its orders and fills carry.
fn build_xemm_maker(spec: &MountPlacement, params: &XemmParams, total_fee: f64) -> XemmMaker {
    let [hedge] = spec.legs.as_slice() else { panic!("validate_xemm_mount passed this mount") };
    let mut m = XemmMaker::new(
        &spec.symbol,
        &hedge.name,
        params.qty,
        params.min_profitability,
        total_fee,
        spec.tick_size,
    );
    m.apply_params(&XemmParams { total_fee, maker_tick_size: spec.tick_size, ..*params });
    m
}

fn quote(ts: i64, bid: f64, ask: f64, symbol: &str) -> TickQuote {
    TickQuote {
        timestamp: ts,
        local_timestamp: 0,
        bid,
        ask,
        bid_size: 5.0,
        ask_size: 5.0,
        symbol: symbol.into(),
    }
}

fn send(handle: &vike_core::CoreHandle, venue: &str, symbol: &str, q: TickQuote) {
    handle
        .tick_sender()
        .quote(QuoteUpdate { venue: venue.into(), symbol: symbol.into(), quote: q })
        .unwrap();
}

/// The maker venue's resting quotes as `(side, price)`, off the live snapshot an operator sees.
fn maker_quotes(handle: &vike_core::CoreHandle, pair: &Pair) -> Vec<(i32, f64)> {
    let mut v: Vec<(i32, f64)> = handle
        .snapshot()
        .orders
        .iter()
        .filter(|o| {
            o.venue == pair.maker_venue
                && o.order_type == vike_model::OrderType::Limit
                && o.status.is_live()
        })
        .map(|o| (o.side, o.price.unwrap_or(0.0)))
        .collect();
    v.sort_by_key(|&(side, _)| side);
    v
}

/// Everything that reached the TAKER venue's engine, as `(symbol, side, qty)`.
fn taker_orders(handle: &vike_core::CoreHandle, pair: &Pair) -> Vec<(String, i32, f64)> {
    handle
        .snapshot()
        .orders
        .iter()
        .filter(|o| o.venue == pair.taker_venue)
        .map(|o| (o.symbol.clone(), o.side, o.qty))
        .collect()
}

/// The three phases of the module doc over `pair`.
fn a_cross_venue_maker_quotes_off_the_reference_and_hedges_on_the_taker_venue_over(pair: &Pair) {
    vike_log::test_init();

    // The sentinel's owning temp directory, held for the whole test (see `common::no_halt`).
    let (_halt_root, halt) = common::no_halt(HALT_ROOT_PREFIX);
    let (spec, params) = config(pair);
    let mount = build_paper_xemm_core_with(&spec, &params, &halt, |s, p, f| {
        Box::new(build_xemm_maker(s, p, f))
    })
    .expect("the pair validates");
    let h = &mount.handle;
    let (maker_venue, maker_symbol) = (pair.maker_venue, pair.maker_symbol);
    let (taker_venue, hedge_symbol) = (pair.taker_venue, pair.hedge_symbol);

    // ---- Phase 1: warm BOTH venues (the reference prices; the maker anchors the clamp) ----
    send(h, taker_venue, hedge_symbol, quote(1_000, 100_000.0, 100_010.0, hedge_symbol));
    send(h, maker_venue, maker_symbol, quote(1_000, 99_990.0, 100_000.0, maker_symbol));

    assert!(
        wait_until(Duration::from_secs(10), || maker_quotes(h, pair).len() == 2),
        "the maker never rested a two-sided quote on {maker_venue}: {:?}",
        maker_quotes(h, pair)
    );
    let resting = maker_quotes(h, pair);
    let (ask_price, bid_price) = (resting[0].1, resting[1].1);
    assert_eq!(resting[0].0, -1, "sorted ask first");
    assert_eq!(resting[1].0, 1, "then bid");
    // Priced off the REFERENCE touch (100_000 / 100_010) backed off by edge + fee, clamped inside the
    // maker venue's touch, snapped to its 0.5 grid. The edge bound (the HEDGE identity) binds here,
    // not the clamp; `vike-market-making`'s `passive_clamp` tests pin the clamp.
    assert!(
        bid_price < 100_000.0 * (1.0 - 0.0005),
        "the bid must sit below the reference bid by at least the required edge: {bid_price}"
    );
    assert!(
        bid_price < 100_000.0 - TICK,
        "and strictly inside the maker venue's own ask — never marketable: {bid_price}"
    );
    assert!(
        ask_price > 100_010.0 * (1.0 + 0.0005),
        "the ask must sit above the reference ask by at least the required edge: {ask_price}"
    );
    assert!(
        taker_orders(h, pair).is_empty(),
        "nothing belongs on the taker venue before a fill: {:?}",
        taker_orders(h, pair)
    );

    // ---- Phase 2: only the REFERENCE moves; the quotes follow (a stale one is picked off) ----
    send(h, taker_venue, hedge_symbol, quote(2_000, 99_500.0, 99_510.0, hedge_symbol));
    assert!(
        wait_until(Duration::from_secs(10), || maker_quotes(h, pair)
            .iter()
            .all(|&(_, px)| px != bid_price && px != ask_price)),
        "a reference-only move must re-price BOTH quotes; still at {resting:?}"
    );

    // ---- Phase 3: a MAKER quote crosses the bid ⇒ paper fill ⇒ a hedge on the TAKER engine ----
    // (the quote is the paper book's clock, decision 0285's amendment of 2026-11-16: an ask under the
    // resting bid fills the maker's BUY the moment it comes)
    let bid_now = maker_quotes(h, pair).iter().find(|&&(s, _)| s == 1).expect("a resting bid").1;
    send(h, maker_venue, maker_symbol, quote(60_000, bid_now - 20.0, bid_now - 10.0, maker_symbol));

    assert!(
        wait_until(Duration::from_secs(10), || !mount.maker_fills.lock().unwrap().is_empty()),
        "the resting bid never filled on the maker venue's paper book"
    );
    assert!(
        wait_until(Duration::from_secs(10), || !taker_orders(h, pair).is_empty()),
        "THE #997 REGRESSION SURFACE: a maker fill produced no order on {taker_venue}. Before \
         cross-venue routing the hedge collapsed back onto the maker venue and NETTED the position \
         instead of hedging it, with no error anywhere."
    );

    let hedges = taker_orders(h, pair);
    assert_eq!(hedges.len(), 1, "exactly one hedge, got {hedges:?}");
    let (symbol, side, qty) = &hedges[0];
    assert_eq!(symbol, hedge_symbol, "the hedge names the TAKER venue's instrument");
    assert_eq!(*side, -1, "a maker BUY is hedged by a taker SELL");
    assert_eq!(qty.to_bits(), QTY.to_bits(), "fully hedged at ratio 1.0, bit-for-bit");
    // …and nothing extra landed on the maker venue: the hedge did NOT double back.
    assert!(
        !mount.maker_fills.lock().unwrap().iter().any(|f| f.side < 0),
        "a SELL on the maker venue means the hedge routed to the wrong engine"
    );

    // ---- Phase 4: the hedge FILLS on the taker venue; the maker hears it as the HEDGE, by its name ----
    // (an ask at or under the hedge's market fills it; a fill heard as a MAKER fill would hedge again)
    send(h, taker_venue, hedge_symbol, quote(61_000, 99_500.0, 99_510.0, hedge_symbol));
    assert!(
        wait_until(Duration::from_secs(10), || !mount.hedge_fills.lock().unwrap().is_empty()),
        "the hedge never filled on the taker venue's paper book"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        taker_orders(h, pair).len(),
        1,
        "the hedge fill was heard as the hedge (by its leg's name `{}`), so no second hedge went out",
        pair.hedge_name
    );

    mount.handle.shutdown_and_join();
}

#[test]
fn a_cross_venue_maker_quotes_off_the_reference_and_hedges_on_the_taker_venue() {
    a_cross_venue_maker_quotes_off_the_reference_and_hedges_on_the_taker_venue_over(&V1);
}

/// **One user name on two venues** (binance × bybit `BTCUSDT.P`): the hedge leg named by its account
/// routes to bybit's book, the maker's quotes stay on binance's, and the hedge fill is heard as the hedge.
#[test]
fn a_maker_and_its_hedge_on_one_user_name_each_reach_their_own_venue() {
    a_cross_venue_maker_quotes_off_the_reference_and_hedges_on_the_taker_venue_over(&SAME_NAME);
}

/// The verdict must not change under an engaged HALT sentinel
/// (`common::assert_indifferent_to_an_engaged_halt_sentinel`).
#[test]
fn the_xemm_scripted_suite_is_indifferent_to_an_engaged_halt_sentinel() {
    common::assert_indifferent_to_an_engaged_halt_sentinel("xemm_scripted::");
}
