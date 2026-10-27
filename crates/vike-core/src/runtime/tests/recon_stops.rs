//! POSITION WITHOUT STOP (`super::recon_stops`), driven through the real pass: a synchronous
//! `CoreThread` is handed `Command::ReconcileReports`' payload through `reconcile_reports`, and the
//! assertions read the recent-events ring (what the operator sees) and, where the level matters, the
//! log.
//!
//! Every test here pairs a fire with the thing that must NOT fire (or the clear that must follow),
//! so a mutation of the production rule reddens the one test that owns it:
//! - cover sources: the report row, the registry (whole and part filled), an armed conditional
//!   (stop and trailing), a held bracket exit, and what is NOT cover (a take-profit, another
//!   account's or another venue's stop);
//! - the size rule: partial cover, the tolerance (absolute and relative), dust, how a size prints;
//! - the edges: named on the second bare pass, one aggregated line, cleared (at WARN), restated on
//!   the summary clock, never repeated;
//! - the exclusions: a venue that cannot rest a stop (said once, in its own words), a stopped
//!   account's tombstone, a flat row, an empty report that does not repeat.
//!
//! The venue under test is `binance` (reduce-only and a stop kind in `vike_venues::caps_for`); the
//! `sim` venue of the other white-box suites has no caps row and would read as one that cannot rest a
//! stop. A position is NAMED on its second consecutive bare pass, so most tests run `named`.

use super::*;
use tracing_test::traced_test;
use vike_exec::testing::RecordingClient;
use vike_model::events::PositionSide;
use vike_model::{
    ManagedOrder, OrderRequest, OrderStatus, OrderStatusReport, OrderType, PositionStatusReport,
};

use crate::runtime::recon_held::HELD_SUMMARY_INTERVAL_MS;
use crate::runtime::recon_stops::STOP_COVER_QTY_TOL;
use crate::runtime::test_support::{core_of, engine_on};

type Core = CoreThread<RecordingClient>;

const SYMBOL: &str = "BTCUSDT";
const VENUE: &str = "binance";
const ALT_KEY: &str = "binance#ALT";
const RAISED: &str = "POSITION WITHOUT STOP:";
const STILL: &str = "POSITION WITHOUT STOP still:";
const CLEARED: &str = "POSITION WITHOUT STOP cleared:";
/// The venue-independent sentence every raising line carries.
const REPORT_MAY_MISS: &str = "The order report of this venue may not list resting stops \
     (known: bybit); check the venue before placing another stop.";

fn core_on(venue: &str) -> Core {
    core_of(engine_on(venue, SYMBOL, RecordingClient::default()), Vec::new(), CoreConfig::default())
}

/// Two accounts of the test venue: the default (engine 0) and `ALT_KEY` (engine 1).
fn two_accounts() -> Core {
    let mut alt = engine_on(VENUE, SYMBOL, RecordingClient::default());
    alt.route_key = ALT_KEY.to_string();
    core_of(
        engine_on(VENUE, SYMBOL, RecordingClient::default()),
        vec![(500.0, alt)],
        CoreConfig::default(),
    )
}

fn position(venue: &str, symbol: &str, side: PositionSide, qty: f64) -> PositionStatusReport {
    PositionStatusReport {
        venue: venue.into(),
        symbol: symbol.into(),
        position_side: side,
        qty,
        avg_px: 100.0,
        ts: 1,
        margin_mode: Default::default(),
        isolated_margin: None,
        delta: None,
    }
}

/// Give the engine at `idx` a LOCAL position in `symbol` (what its own book holds, apart from what the
/// venue reports). A held bracket exit counts as cover only against one it would close.
fn set_local(c: &mut Core, idx: usize, symbol: &str, side: PositionSide, size: f64) {
    let venue = c.eng(idx).venue.clone();
    c.eng_mut(idx).account.positions.insert(
        (ustr::Ustr::from(venue.as_str()), ustr::Ustr::from(symbol), side),
        vike_exec::PositionEntry { size, avg_px: 100.0, ..Default::default() },
    );
}

/// A one-way position of `qty` (signed) on the test venue and symbol.
fn held(qty: f64) -> PositionStatusReport {
    position(VENUE, SYMBOL, PositionSide::Both, qty)
}

/// A live, resting order row of `order_type` on the venue's report, for any symbol.
fn row_on(
    symbol: &str,
    coid: &str,
    side: i32,
    qty: f64,
    order_type: OrderType,
) -> OrderStatusReport {
    OrderStatusReport {
        venue: VENUE.into(),
        symbol: symbol.into(),
        venue_order_id: format!("v-{coid}").into(),
        client_order_id: Some(coid.to_string()),
        side,
        order_type,
        qty,
        filled_qty: 0.0,
        avg_px: 0.0,
        status: OrderStatus::Accepted,
        ts: 1,
    }
}

fn row(coid: &str, side: i32, qty: f64, order_type: OrderType) -> OrderStatusReport {
    row_on(SYMBOL, coid, side, qty, order_type)
}

/// The protective SELL stop of a long position.
fn sell_stop(coid: &str, qty: f64) -> OrderStatusReport {
    row(coid, -1, qty, OrderType::Stop)
}

/// A request for the registry or the held map: a stop (the callers change the kind they test).
fn request(coid: &str, venue: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: venue.into(),
        symbol: SYMBOL.into(),
        side,
        qty,
        order_type: OrderType::Stop,
        trigger_price: Some(90.0),
        ..Default::default()
    }
}

/// One pass for `venue`'s account `route_key` (`None`: its sole account), a minute after the last,
/// under `quarantine` so nothing folds and the engine's own books stay as the test set them.
fn run_for(
    c: &mut Core,
    venue: &str,
    route_key: Option<&str>,
    orders: Vec<OrderStatusReport>,
    positions: Vec<PositionStatusReport>,
) {
    c.engine.now_ms += 60_000;
    c.reconcile_reports(ReconcileReports {
        venue: venue.into(),
        since: 0,
        orders,
        fills: Vec::new(),
        positions,
        policy: vike_exec::recon::ReconPolicy {
            default: vike_exec::recon::ReconMode::Quarantine,
            ..Default::default()
        },
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: vike_exec::recon::BalanceTol::default(),
        route_key: route_key.map(str::to_string),
        admitted_symbols: None,
        lists_trigger_orders: false,
    });
}

fn run(c: &mut Core, orders: Vec<OrderStatusReport>, positions: Vec<PositionStatusReport>) {
    run_for(c, VENUE, None, orders, positions);
}

/// The same reports on two consecutive passes: the second is the one that NAMES a bare position.
fn named(c: &mut Core, orders: Vec<OrderStatusReport>, positions: Vec<PositionStatusReport>) {
    run(c, orders.clone(), positions.clone());
    run(c, orders, positions);
}

/// The recent-events lines that contain `needle`.
fn lines(c: &Core, needle: &str) -> Vec<String> {
    c.recent.iter().filter(|l| l.contains(needle)).map(|l| l.to_string()).collect()
}

fn raised(c: &Core) -> usize {
    lines(c, RAISED).len()
}

fn still(c: &Core) -> usize {
    lines(c, STILL).len()
}

fn cleared(c: &Core) -> usize {
    lines(c, CLEARED).len()
}

/// FIRE, in the operator's words, ONCE, and never again while it stays true: the first bare pass says
/// nothing (it may be an excursion), the second names the account, venue, symbol, direction and size,
/// carries the venue-independent warning about the order report, and puts the account and symbol in
/// the suggested command; the same line reaches the log at WARN. Two more passes add nothing.
#[traced_test]
#[test]
fn a_position_with_no_stop_is_named_once_and_never_repeated() {
    let mut c = core_on(VENUE);
    run(&mut c, Vec::new(), vec![held(0.5)]);
    assert_eq!(raised(&c), 0, "one bare pass is not yet a position: {:?}", c.recent);
    for _ in 0..3 {
        run(&mut c, Vec::new(), vec![held(0.5)]);
    }

    let said = lines(&c, RAISED);
    assert_eq!(said.len(), 1, "{said:?}");
    for want in [
        "binance binance: 1 position with no full stop: BTCUSDT long 0.5",
        REPORT_MAY_MISS,
        "vike-cli trade position flatten binance BTCUSDT --node <host:port>",
    ] {
        assert!(said[0].contains(want), "wanted {want:?} in {}", said[0]);
    }
    assert!(!said[0].contains("cannot rest"), "{}", said[0]);
    assert!(!said[0].to_lowercase().contains("okx"), "okx lists its algo stops: {}", said[0]);
    assert_eq!(cleared(&c), 0);
    logs_assert(|log: &[&str]| match log.iter().filter(|l| l.contains(RAISED)).count() {
        1 => Ok(()),
        n => Err(format!("one WARN for the position over four passes, got {n}")),
    });
}

/// EXCURSION: a position that opens and is flat again inside one interval is never named: a bare
/// position must persist for two consecutive passes (vike-mm's makers place no stops).
#[test]
fn an_excursion_that_does_not_persist_two_passes_is_never_named() {
    let mut c = core_on(VENUE);
    for _ in 0..3 {
        run(&mut c, Vec::new(), vec![held(0.5)]);
        run(&mut c, Vec::new(), vec![held(0.0)]);
    }
    assert_eq!((raised(&c), cleared(&c)), (0, 0), "{:?}", c.recent);

    named(&mut c, Vec::new(), vec![held(0.5)]);
    assert_eq!(raised(&c), 1, "two in a row is a position: {:?}", c.recent);
}

/// CLEAR, then the edge re-arms: a stop the report lists ends it with one line, a pass that keeps
/// the stop says nothing, and losing the stop later is a new fire (after its own two passes).
#[test]
fn a_listed_stop_clears_it_and_losing_the_stop_names_it_again() {
    let mut c = core_on(VENUE);
    named(&mut c, Vec::new(), vec![held(0.5)]);
    assert_eq!((raised(&c), cleared(&c)), (1, 0));

    run(&mut c, vec![sell_stop("s1", 0.5)], vec![held(0.5)]);
    assert_eq!((raised(&c), cleared(&c)), (1, 1), "the stop appeared: one clear");

    run(&mut c, vec![sell_stop("s1", 0.5)], vec![held(0.5)]);
    assert_eq!((raised(&c), cleared(&c)), (1, 1), "still covered: silent");

    named(&mut c, Vec::new(), vec![held(0.5)]);
    assert_eq!((raised(&c), cleared(&c)), (2, 1), "the stop is gone: a new edge");
}

/// The clear is a WARN too: the shipped units log at `warn`, and a clear at INFO is invisible there.
#[traced_test]
#[test]
fn the_clear_is_logged_at_warn() {
    let mut c = core_on(VENUE);
    named(&mut c, Vec::new(), vec![held(0.5)]);
    run(&mut c, vec![sell_stop("s1", 0.5)], vec![held(0.5)]);
    assert_eq!(cleared(&c), 1);
    logs_assert(|log: &[&str]| {
        match log.iter().any(|l| l.contains(CLEARED) && l.contains("WARN")) {
            true => Ok(()),
            false => Err(format!("no WARN line carries the clear: {log:?}")),
        }
    });
}

/// STILL: a position that stays bare is restated once an hour on `recon_held`'s summary clock, not
/// per pass, and not before the hour is up.
#[test]
fn a_position_that_stays_bare_is_restated_once_an_hour() {
    let mut c = core_on(VENUE);
    named(&mut c, Vec::new(), vec![held(0.5)]);
    run(&mut c, Vec::new(), vec![held(0.5)]);
    assert_eq!(still(&c), 0, "not due yet");

    c.engine.now_ms += HELD_SUMMARY_INTERVAL_MS;
    run(&mut c, Vec::new(), vec![held(0.5)]);
    let said = lines(&c, STILL);
    assert_eq!(said.len(), 1, "{:?}", c.recent);
    assert!(
        said[0].contains("BTCUSDT long 0.5") && said[0].contains(REPORT_MAY_MISS),
        "{}",
        said[0]
    );

    run(&mut c, Vec::new(), vec![held(0.5)]);
    assert_eq!(still(&c), 1, "the clock restarted at the restatement");
    c.engine.now_ms += HELD_SUMMARY_INTERVAL_MS;
    run(&mut c, Vec::new(), vec![held(0.5)]);
    assert_eq!((still(&c), raised(&c)), (2, 1), "{:?}", c.recent);
}

/// ONE LINE per account per pass: five bare positions are one raise line naming the first three and
/// counting the rest, and one clear line when they are covered together.
#[test]
fn one_line_per_account_names_the_first_three_and_counts_the_rest() {
    let symbols = ["AAAUSDT", "BBBUSDT", "CCCUSDT", "DDDUSDT", "EEEUSDT"];
    let bare: Vec<_> =
        symbols.iter().map(|s| position(VENUE, s, PositionSide::Both, 1.0)).collect();
    let mut c = core_on(VENUE);
    named(&mut c, Vec::new(), bare.clone());

    let said = lines(&c, RAISED);
    assert_eq!(said.len(), 1, "{said:?}");
    let want =
        "5 positions with no full stop: AAAUSDT long 1; BBBUSDT long 1; CCCUSDT long 1 and 2 more.";
    assert!(said[0].contains(want), "wanted {want:?} in {}", said[0]);
    assert!(!said[0].contains("DDDUSDT"), "only three are named: {}", said[0]);

    let stops = symbols.iter().map(|s| row_on(s, &format!("s-{s}"), -1, 1.0, OrderType::Stop));
    run(&mut c, stops.collect(), bare);
    let gone = lines(&c, CLEARED);
    assert_eq!(gone.len(), 1, "{gone:?}");
    let want =
        "5 positions now covered by a stop or no longer open: AAAUSDT, BBBUSDT, CCCUSDT and 2 more";
    assert!(gone[0].contains(want), "wanted {want:?} in {}", gone[0]);
}

/// An EMPTY position report is also what a venue sends for a closed position, but one alone must not
/// clear what was said (a transient empty report would flap): the second in a row does, and a report
/// with rows in between starts the count again.
#[test]
fn an_empty_report_clears_only_when_it_repeats() {
    let mut quiet = core_on(VENUE);
    run(&mut quiet, Vec::new(), Vec::new());
    assert_eq!((raised(&quiet), cleared(&quiet)), (0, 0));

    let mut c = core_on(VENUE);
    named(&mut c, Vec::new(), vec![held(0.5)]);
    run(&mut c, Vec::new(), Vec::new());
    assert_eq!(cleared(&c), 0, "one empty report judges nothing: {:?}", c.recent);
    run(&mut c, Vec::new(), vec![held(0.5)]);
    run(&mut c, Vec::new(), Vec::new());
    assert_eq!((raised(&c), cleared(&c)), (1, 0), "the rows in between reset the count");
    run(&mut c, Vec::new(), Vec::new());
    assert_eq!((raised(&c), cleared(&c)), (1, 1), "two empty reports in a row read as flat");
}

/// ...and a bare position that has been seen once survives ONE empty report between two bare passes:
/// the third pass is its second consecutive bare one (the empty report judged nothing), so it is named.
#[test]
fn a_bare_position_survives_one_empty_report_between_two_bare_passes() {
    let mut c = core_on(VENUE);
    run(&mut c, Vec::new(), vec![held(0.5)]);
    run(&mut c, Vec::new(), Vec::new());
    assert_eq!(raised(&c), 0, "{:?}", c.recent);
    run(&mut c, Vec::new(), vec![held(0.5)]);
    assert_eq!(raised(&c), 1, "named on its second bare pass: {:?}", c.recent);
}

/// PARTIAL cover: a stop smaller than the position is named, with what is left; a second stop that
/// completes the sum clears it. A size prints plainly: `0.3 - 0.2` is `0.1`, not `0.09999999999999998`.
#[test]
fn a_smaller_stop_is_partial_cover_and_names_what_is_left() {
    let mut c = core_on(VENUE);
    named(&mut c, vec![sell_stop("s1", 0.2)], vec![held(0.3)]);

    let said = lines(&c, RAISED);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("BTCUSDT long 0.3 (a stop covers 0.2, 0.1 has none)"), "{}", said[0]);
    assert!(!said[0].contains("0.0999"), "{}", said[0]);

    run(&mut c, vec![sell_stop("s1", 0.2), sell_stop("s2", 0.1)], vec![held(0.3)]);
    assert_eq!(cleared(&c), 1, "the two stops sum to the position");
}

/// A TAKE-PROFIT is not cover: a stop of 0.5 beside a take-profit of 1.0 on a long of 1.0 is partial
/// cover, with 0.5 left.
#[test]
fn a_take_profit_beside_a_stop_is_partial_cover() {
    let mut c = core_on(VENUE);
    let tp = row("t1", -1, 1.0, OrderType::TakeProfit);
    named(&mut c, vec![sell_stop("s1", 0.5), tp], vec![held(1.0)]);

    let said = lines(&c, RAISED);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("(a stop covers 0.5, 0.5 has none)"), "{}", said[0]);
}

/// TOLERANCE: sizes are compared with the tolerance `ExecutionEngine::local_view` hands `diff`
/// (absolute) plus a relative part, never with float equality. A stop short of the position by float
/// noise covers it, at 0.3 and at a hundred million; one short by a real amount does not; and the
/// absolute constant is pinned to the engine's own.
#[test]
fn sizes_are_compared_with_the_local_views_tolerance_not_float_equality() {
    let mut c = core_on(VENUE);
    assert_eq!(STOP_COVER_QTY_TOL, c.eng(0).local_view().qty_tol);

    // 0.1 + 0.2 is 0.30000000000000004: one ulp above the 0.3 of the stop.
    let (size, stop) = (0.1 + 0.2, 0.3);
    assert!(size > stop, "PREMISE: exact equality would read this stop as short");
    named(&mut c, vec![sell_stop("s1", stop)], vec![held(size)]);
    assert_eq!(raised(&c), 0, "{:?}", c.recent);

    // seven ulps at this size: beyond the absolute part, inside the relative one
    let big = 100_000_000.0;
    let big_stop = row_on("BIGUSDT", "s2", -1, big - 1e-7, OrderType::Stop);
    named(&mut c, vec![big_stop], vec![position(VENUE, "BIGUSDT", PositionSide::Both, big)]);
    assert_eq!(raised(&c), 0, "{:?}", c.recent);

    named(&mut c, vec![sell_stop("s3", 1.0 - 1e-6)], vec![held(1.0)]);
    assert_eq!(raised(&c), 1, "a stop short by a millionth is partial cover: {:?}", c.recent);
}

/// A VENUE THAT CANNOT REST A STOP (no reduce-only flag, or no trigger kind in its caps) is named
/// once, in words that say so, never at every pass and never restated on the summary clock (its
/// position cannot carry a resting stop, so an hourly line would say nothing new); it carries the
/// venue-independent warning too. (Deribit was the roster's one venue with the reduce-only flag and NO
/// trigger kind until its native stops landed, so the "no trigger kind" half of the rule has no roster
/// exemplar now; the rows below declare no reduce-only flag, and the `can_rest` test is the same
/// expression either way.)
#[test]
fn a_venue_that_cannot_rest_a_stop_is_named_once_in_its_own_words() {
    for venue in ["oanda", "ig", "polymarket"] {
        let mut c = core_on(venue);
        let positions = vec![position(venue, SYMBOL, PositionSide::Both, 2.0)];
        for _ in 0..4 {
            run_for(&mut c, venue, None, Vec::new(), positions.clone());
        }
        c.engine.now_ms += HELD_SUMMARY_INTERVAL_MS;
        run_for(&mut c, venue, None, Vec::new(), positions.clone());
        assert_eq!(still(&c), 0, "{venue}: never restated: {:?}", c.recent);
        let said = lines(&c, RAISED);
        assert_eq!(said.len(), 1, "{venue}: {said:?}");
        assert!(
            said[0].contains("cannot rest a protective stop for this position"),
            "{venue}: {}",
            said[0]
        );
        assert!(said[0].contains(REPORT_MAY_MISS), "{venue}: {}", said[0]);
        assert!(!said[0].contains("place a reduce-only stop"), "{venue}: {}", said[0]);
    }
}

/// ...and what the core itself holds still covers a position there: an armed stop is cover on a venue
/// that cannot rest one, so the position is not named.
#[test]
fn an_armed_stop_covers_a_position_on_a_venue_that_cannot_rest_one() {
    let mut c = core_on("oanda");
    assert!(
        c.conditional_books
            .entry(("oanda".to_string(), SYMBOL.to_string()))
            .or_default()
            .add_stop("a1", -1, 2.0, 90.0, None)
    );
    let positions = vec![position("oanda", SYMBOL, PositionSide::Both, 2.0)];
    for _ in 0..2 {
        run_for(&mut c, "oanda", None, Vec::new(), positions.clone());
    }
    assert_eq!(raised(&c), 0, "{:?}", c.recent);
}

/// DUST: a position below the instrument's lot step could not be closed, so it is not named; one
/// above it is. The lot read is the symbol's OWN: a position on another symbol is not judged on the
/// mount's lot.
#[test]
fn dust_below_the_lot_step_is_never_named() {
    let mut c = core_on(VENUE);
    c.eng_mut(0).gate.limits.lot_size = Some(0.001);

    named(&mut c, Vec::new(), vec![held(0.0004)]);
    assert_eq!(raised(&c), 0, "dust: {:?}", c.recent);

    named(&mut c, Vec::new(), vec![held(0.002)]);
    assert_eq!(raised(&c), 1, "two lots are a position: {:?}", c.recent);

    let eth = position(VENUE, "ETHUSDT", PositionSide::Both, 0.0004);
    named(&mut c, Vec::new(), vec![held(0.002), eth]);
    assert_eq!(raised(&c), 2, "the mount's lot is not lent to ETHUSDT: {:?}", c.recent);
}

/// A flat row (zero, noise under the tolerance, or NaN) is nothing to protect.
#[test]
fn a_flat_row_is_never_named() {
    let mut c = core_on(VENUE);
    for qty in [0.0, 1e-12, f64::NAN] {
        named(&mut c, Vec::new(), vec![held(qty)]);
    }
    assert_eq!(raised(&c), 0, "{:?}", c.recent);
}

/// TOMBSTONE: a stopped account's engine is never named, though a pass reaches it; the same pass on
/// a running engine is. (The account stop also forgets what was said of it, below.)
#[test]
fn a_stopped_accounts_tombstone_is_never_named() {
    let mut live = core_on(VENUE);
    named(&mut live, Vec::new(), vec![held(0.5)]);
    assert_eq!(raised(&live), 1, "PREMISE: the same passes name a running engine");

    let mut tombstone = core_on(VENUE);
    tombstone.retired_engines.push(0);
    named(&mut tombstone, Vec::new(), vec![held(0.5)]);
    assert_eq!(raised(&tombstone), 0, "{:?}", tombstone.recent);
}

/// What the edge remembers is per ACCOUNT, an account stop forgets its own and no other's, and an
/// account that is not the default is told its own book in the suggested command.
#[test]
fn the_edge_is_per_account_and_an_account_stop_forgets_its_own() {
    let mut c = two_accounts();
    for _ in 0..3 {
        run_for(&mut c, VENUE, Some(VENUE), Vec::new(), vec![held(0.5)]);
        run_for(&mut c, VENUE, Some(ALT_KEY), Vec::new(), vec![held(0.5)]);
    }
    assert_eq!(raised(&c), 2, "each account is told once: {:?}", c.recent);
    let alt = lines(&c, &format!("{RAISED} {ALT_KEY} {VENUE}:"));
    assert_eq!(alt.len(), 1, "{:?}", c.recent);
    assert!(
        alt[0].contains("vike-cli trade position flatten binance/ALT BTCUSDT --node <host:port>"),
        "{}",
        alt[0]
    );

    c.apply_account_change(
        vike_exec::AccountChange::StopEngine {
            not_running: vike_exec::NotRunningAccount {
                venue: VENUE.to_string(),
                account: Some(
                    vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("label"),
                ),
                route_key: ALT_KEY.to_string(),
                tier: None,
                reason: "account inactive".to_string(),
            },
        },
        10,
    );
    assert!(!c.recon_announce.naked.holds(ALT_KEY), "the stopped account is forgotten");
    assert!(c.recon_announce.naked.holds(VENUE), "the other account is not");
}

/// ANOTHER ACCOUNT'S stops do not cover this one: a held exit and an armed stop that release or fire
/// onto the ALT engine leave the default account's position bare, and cover ALT's own.
#[test]
fn an_other_accounts_held_and_armed_stops_do_not_cover() {
    let mut c = two_accounts();
    // both books hold the long, so the held exit's account (not a flat local book) is what is judged
    set_local(&mut c, 0, SYMBOL, PositionSide::Both, 0.5);
    set_local(&mut c, 1, SYMBOL, PositionSide::Both, 0.5);
    c.held_orders.insert("sl-alt".to_string(), request("sl-alt", VENUE, -1, 0.25));
    c.coid_venue.insert("sl-alt".to_string(), 1);
    assert!(
        c.conditional_books
            .entry((VENUE.to_string(), SYMBOL.to_string()))
            .or_default()
            .add_stop("a-alt", -1, 0.25, 90.0, None)
    );
    c.cond_engine
        .insert("a-alt".to_string(), ArmedBy { engine: 1, mount: None, reduce_only: false });

    for _ in 0..2 {
        run_for(&mut c, VENUE, Some(VENUE), Vec::new(), vec![held(0.5)]);
    }
    assert_eq!(raised(&c), 1, "ALT's stops are not the default account's cover: {:?}", c.recent);
    assert!(lines(&c, RAISED)[0].contains("BTCUSDT long 0.5."), "{:?}", c.recent);

    for _ in 0..2 {
        run_for(&mut c, VENUE, Some(ALT_KEY), Vec::new(), vec![held(0.5)]);
    }
    assert_eq!(raised(&c), 1, "the two together cover ALT's own position: {:?}", c.recent);
}

/// A HELD exit of another VENUE with the same symbol is no cover for this one.
#[test]
fn a_held_exit_of_another_venue_does_not_cover() {
    let mut c = core_on(VENUE);
    set_local(&mut c, 0, SYMBOL, PositionSide::Both, 0.5); // so only the venue is what is judged
    c.held_orders.insert("sl-other".to_string(), request("sl-other", "bybit", -1, 0.5));
    named(&mut c, Vec::new(), vec![held(0.5)]);
    assert_eq!(raised(&c), 1, "{:?}", c.recent);
}

/// WHICH report rows cover: only a LIVE, OPPOSITE-side, same-symbol row that is a STOP kind with
/// quantity left. Each case is a fresh long of 0.5; `true` means the row alone covers it.
#[test]
fn only_a_live_opposite_side_stop_row_of_the_same_symbol_covers() {
    let with = |f: fn(&mut OrderStatusReport)| {
        let mut r = sell_stop("s1", 0.5);
        f(&mut r);
        r
    };
    let cases: Vec<(&str, OrderStatusReport, bool)> = vec![
        ("an opposite stop", sell_stop("s1", 0.5), true),
        ("a stop-limit", with(|r| r.order_type = OrderType::StopLimit), true),
        ("a take-profit trigger", with(|r| r.order_type = OrderType::TakeProfit), false),
        (
            "an unrecognized stop word",
            with(|r| r.order_type = OrderType::Unrecognized("trailing_stop_market".into())),
            true,
        ),
        (
            "an unrecognized take-profit word",
            with(|r| r.order_type = OrderType::Unrecognized("take_profit_limit".into())),
            false,
        ),
        (
            "an unrecognized take-profit trigger word",
            with(|r| r.order_type = OrderType::Unrecognized("take_profit_trigger".into())),
            false,
        ),
        (
            "an unrecognized word naming both (ctrader)",
            with(|r| r.order_type = OrderType::Unrecognized("stop_loss_take_profit".into())),
            true,
        ),
        (
            "an unrecognized word naming no trigger",
            with(|r| r.order_type = OrderType::Unrecognized("market_if_touched".into())),
            false,
        ),
        ("a same-side stop", with(|r| r.side = 1), false),
        ("another symbol's stop", with(|r| r.symbol = "ETHUSDT".into()), false),
        ("a plain limit", with(|r| r.order_type = OrderType::Limit), false),
        ("a canceled stop", with(|r| r.status = OrderStatus::Canceled), false),
        ("a stop with nothing left", with(|r| r.filled_qty = 0.5), false),
    ];
    for (what, report, covers) in cases {
        let mut c = core_on(VENUE);
        named(&mut c, vec![report], vec![held(0.5)]);
        assert_eq!(raised(&c) == 0, covers, "{what}: {:?}", lines(&c, "POSITION"));
    }
}

/// DIRECTION: a short needs a BUY stop; a hedge-mode short leg likewise, and a long leg a SELL one.
#[test]
fn a_short_needs_a_buy_stop_and_a_long_a_sell_stop() {
    let short_one_way = held(-0.5);
    let short_leg = position(VENUE, SYMBOL, PositionSide::Short, -0.5);
    let long_leg = position(VENUE, SYMBOL, PositionSide::Long, 0.5);
    for (what, pos) in [("one-way short", short_one_way), ("hedge short leg", short_leg)] {
        let mut c = core_on(VENUE);
        named(&mut c, vec![sell_stop("s1", 0.5)], vec![pos.clone()]);
        assert_eq!(raised(&c), 1, "{what}: a SELL stop does not cover a short");
        run(&mut c, vec![row("b1", 1, 0.5, OrderType::Stop)], vec![pos]);
        assert_eq!(cleared(&c), 1, "{what}: a BUY stop does");
    }
    let mut c = core_on(VENUE);
    named(&mut c, vec![row("b1", 1, 0.5, OrderType::Stop)], vec![long_leg.clone()]);
    assert_eq!(raised(&c), 1, "a BUY stop does not cover a long leg");
    run(&mut c, vec![sell_stop("s1", 0.5)], vec![long_leg]);
    assert_eq!(cleared(&c), 1, "a SELL stop does");
}

/// The core's own books are cover. A live stop in the registry that the report does not list (a
/// conditional order the venue's parser does not type as a stop, placed this session) covers the
/// position.
#[test]
fn a_registry_stop_the_report_does_not_list_covers() {
    let mut c = core_on(VENUE);
    let mut mo = ManagedOrder::new(request("s-reg", VENUE, -1, 0.5));
    mo.status = OrderStatus::Accepted;
    c.engine.registry.insert("s-reg".to_string(), mo);
    named(&mut c, Vec::new(), vec![held(0.5)]);
    assert_eq!(raised(&c), 0, "{:?}", c.recent);
}

/// A TAKE-PROFIT in the registry is not cover either (the registry is its own source, apart from the
/// report's).
#[test]
fn a_registry_take_profit_does_not_cover() {
    let mut c = core_on(VENUE);
    let mut tp = request("t-reg", VENUE, -1, 0.5);
    tp.order_type = OrderType::TakeProfit;
    let mut mo = ManagedOrder::new(tp);
    mo.status = OrderStatus::Accepted;
    c.engine.registry.insert("t-reg".to_string(), mo);
    named(&mut c, Vec::new(), vec![held(0.5)]);
    assert_eq!(raised(&c), 1, "{:?}", c.recent);
}

/// ...by what is LEFT of it: a registry stop of 0.5 that is 0.2 filled covers 0.3.
#[test]
fn a_part_filled_registry_stop_covers_only_what_is_left() {
    let mut c = core_on(VENUE);
    let mut mo = ManagedOrder::new(request("s-reg", VENUE, -1, 0.5));
    mo.status = OrderStatus::PartiallyFilled;
    mo.filled_qty = 0.2;
    c.engine.registry.insert("s-reg".to_string(), mo);
    named(&mut c, Vec::new(), vec![held(0.5)]);

    let said = lines(&c, RAISED);
    assert_eq!(said.len(), 1, "{:?}", c.recent);
    assert!(said[0].contains("(a stop covers 0.3, 0.2 has none)"), "{}", said[0]);
}

/// ...but one order is one order: a registry stop the report ALSO lists, by coid or by venue order id,
/// is counted once, so a stop of half the position is partial cover, not full.
#[test]
fn a_stop_in_both_the_registry_and_the_report_counts_once() {
    for by_venue_id in [false, true] {
        let mut c = core_on(VENUE);
        let mut mo = ManagedOrder::new(request("s1", VENUE, -1, 0.5));
        mo.status = OrderStatus::Accepted;
        mo.venue_order_id = Some("v-s1".to_string());
        c.engine.registry.insert("s1".to_string(), mo);
        let mut listed = sell_stop("s1", 0.5);
        if by_venue_id {
            listed.client_order_id = None;
        }
        named(&mut c, vec![listed], vec![held(1.0)]);
        let said = lines(&c, RAISED);
        assert_eq!(said.len(), 1, "by_venue_id={by_venue_id}: counted twice would cover it");
        assert!(said[0].contains("(a stop covers 0.5, 0.5 has none)"), "{}", said[0]);
    }
}

/// An armed conditional (an emulated stop) fires onto this engine: cover; one on the wrong side
/// protects nothing.
#[test]
fn an_armed_stop_of_the_same_symbol_covers() {
    let mut c = core_on(VENUE);
    assert!(
        c.conditional_books
            .entry((VENUE.to_string(), SYMBOL.to_string()))
            .or_default()
            .add_stop("a1", -1, 0.5, 90.0, None)
    );
    named(&mut c, Vec::new(), vec![held(0.5)]);
    assert_eq!(raised(&c), 0, "{:?}", c.recent);

    let mut wrong = core_on(VENUE);
    assert!(
        wrong
            .conditional_books
            .entry((VENUE.to_string(), SYMBOL.to_string()))
            .or_default()
            .add_stop("a1", 1, 0.5, 110.0, None)
    );
    named(&mut wrong, Vec::new(), vec![held(0.5)]);
    assert_eq!(raised(&wrong), 1, "{:?}", wrong.recent);
}

/// A TRAILING arm is a stop too.
#[test]
fn a_trailing_arm_covers() {
    let mut c = core_on(VENUE);
    assert!(
        c.conditional_books
            .entry((VENUE.to_string(), SYMBOL.to_string()))
            .or_default()
            .add_trailing("t1", -1, 0.5, 5.0, 100.0, None)
    );
    named(&mut c, Vec::new(), vec![held(0.5)]);
    assert_eq!(raised(&c), 0, "{:?}", c.recent);
}

/// A held bracket exit (its entry has partly filled, so the engine's book holds the position) is cover
/// when it is the STOP leg, and not when it is the TAKE-PROFIT leg: the same book, the same size, only
/// the leg's kind differs.
#[test]
fn a_held_stop_leg_covers_and_a_held_take_profit_leg_does_not() {
    let mut c = core_on(VENUE);
    set_local(&mut c, 0, SYMBOL, PositionSide::Both, 0.5);
    c.held_orders.insert("sl-held".to_string(), request("sl-held", VENUE, -1, 0.5));
    named(&mut c, Vec::new(), vec![held(0.5)]);
    assert_eq!(raised(&c), 0, "{:?}", c.recent);

    let mut tp = core_on(VENUE);
    set_local(&mut tp, 0, SYMBOL, PositionSide::Both, 0.5);
    let mut leg = request("tp-held", VENUE, -1, 0.5);
    leg.order_type = OrderType::TakeProfit;
    tp.held_orders.insert("tp-held".to_string(), leg);
    named(&mut tp, Vec::new(), vec![held(0.5)]);
    assert_eq!(raised(&tp), 1, "{:?}", tp.recent);
}

/// THE G1 CASE the warning exists to catch: a bracket's entry filled while the node was down. The
/// journal restored the held stop leg, but the reconcile path never calls `drive_contingency_on_fill`,
/// so the exit is never released, the engine's own book is FLAT and the venue shows an open position.
/// A held exit with a flat local position is the unreleased-bracket case: not cover, so it is named.
#[test]
fn a_held_exit_with_a_flat_local_position_is_not_cover() {
    let mut c = core_on(VENUE);
    c.held_orders.insert("sl-held".to_string(), request("sl-held", VENUE, -1, 0.5));
    assert_eq!(c.eng(0).position_size_of(SYMBOL, PositionSide::Both), 0.0, "PREMISE: flat book");
    named(&mut c, Vec::new(), vec![held(0.5)]);

    let said = lines(&c, RAISED);
    assert_eq!(said.len(), 1, "{:?}", c.recent);
    assert!(said[0].contains("BTCUSDT long 0.5"), "{}", said[0]);
}

/// ...and WRONG SIDE is not cover either: a held SELL exit closes a long, so against a local SHORT it
/// closes nothing (the venue still shows a long).
#[test]
fn a_held_exit_against_the_wrong_side_of_the_local_position_is_not_cover() {
    for (what, side, size) in [
        ("one-way short", PositionSide::Both, -0.5),
        ("hedge short leg", PositionSide::Short, -0.5),
    ] {
        let mut c = core_on(VENUE);
        set_local(&mut c, 0, SYMBOL, side, size);
        c.held_orders.insert("sl-held".to_string(), request("sl-held", VENUE, -1, 0.5));
        named(&mut c, Vec::new(), vec![held(0.5)]);
        assert_eq!(raised(&c), 1, "{what}: {:?}", c.recent);
    }
}

/// A held exit IS cover against a local position it would close, on either side and in either mode: a
/// SELL exit against a one-way or hedge-mode long, a BUY exit against a one-way or hedge-mode short.
#[test]
fn a_held_exit_against_the_side_it_closes_is_cover() {
    let cases = [
        ("sell exit, one-way long", -1, PositionSide::Both, 0.5, held(0.5)),
        (
            "sell exit, hedge long leg",
            -1,
            PositionSide::Long,
            0.5,
            position(VENUE, SYMBOL, PositionSide::Long, 0.5),
        ),
        ("buy exit, one-way short", 1, PositionSide::Both, -0.5, held(-0.5)),
        (
            "buy exit, hedge short leg",
            1,
            PositionSide::Short,
            -0.5,
            position(VENUE, SYMBOL, PositionSide::Short, -0.5),
        ),
    ];
    for (what, exit_side, local_side, local_size, venue_row) in cases {
        let mut c = core_on(VENUE);
        set_local(&mut c, 0, SYMBOL, local_side, local_size);
        c.held_orders.insert("sl-held".to_string(), request("sl-held", VENUE, exit_side, 0.5));
        named(&mut c, Vec::new(), vec![venue_row]);
        assert_eq!(raised(&c), 0, "{what}: {:?}", c.recent);
    }
}
