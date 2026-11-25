//! **A reconcile pass adopts the venue's cash figure into an ANCHORED foreign-quoted ledger, and still
//! withholds it from the two ledgers whose currency it is not** (decision 0183, amended by the owner's
//! ruling of 2026-10-19: "the reconcile balance needs no currency label: it is, by definition, in the
//! account's currency, known by the API, else USD").
//!
//! `ReconcileReports::balance` is a bare `f64`: the venue's cash with no asset beside it. Until this
//! ruling the pass dropped it for every engine that held a symbol quoted in another currency, because a
//! label-less number could not be shown to be in the ledger's currency. That ledger is in the ACCOUNT's
//! currency, the engine can only hold such a symbol once it was TOLD that currency (`account-currency-unset`),
//! and a venue's account balance is in the account's currency, so the figure is adopted: OANDA's equity
//! stops being the seed plus the balance, and a restored `Authoritative` balance is re-adopted by the first
//! pass instead of staying as restored for ever.
//!
//! An INVERSE engine (Deribit) takes it too, as a COIN amount: its books are in the settle coin (decision
//! 0258), and the figure is published at the coin's price. **The core names no venue.** A venue whose figure is not the account currency's withholds it
//! itself, in its own `ReconcileClient` (IBKR's `fetch_balance` answers `None` once the engine admitted a stock in
//! another currency: `vike-ibkr`'s `holds_foreign_stock`), and the pass then carries no balance at all: these
//! tests hold that a pass of ANY venue name is adopted here, and that an ABSENT balance folds nothing.
//!
//! The controls are the same passes on an engine with no foreign-quoted symbol, which takes the figure.

use std::sync::Arc;

use indexmap::IndexMap;
use vike_core::spawn_core;
use vike_execution::recon::{ReconcileMode, ReconcilePolicy};
use vike_execution::testing::{RecordingClient, dyn_engine};
use vike_execution::{
    Account, AdmitOutcome, BalanceMode, Command, CoreSnapshot, Engine, ExecutionClient, MarkSource,
    ReconcileReports, RiskGate, SymbolAdmission,
};
use vike_model::events::{Event, FillEvent, PositionSide, TradeId};
use vike_model::{ContractValue, RiskLimits, SymbolGrid};

use crate::kit::handle::wait_for_snapshot;
use crate::kit::requests::PassConfig;
use vike_core::testkit::test_config;

type EngineBoxed = Engine<Box<dyn ExecutionClient + Send>>;

/// The venue's cash figure, in a currency the report does not name.
const VENUE_CASH: f64 = 10_000.0;

/// A pass of `venue` reporting `cash`, built from a `PassConfig` field by field as the reconcile manager
/// builds it.
fn pass(venue: &str, cash: f64, reconcile_balance: bool) -> Command {
    let config = PassConfig { reconcile_balance, ..PassConfig::default() };
    Command::ReconcileReports(Box::new(ReconcileReports {
        venue: venue.into(),
        since: 0,
        orders: Vec::new(),
        fills: Vec::new(),
        positions: Vec::new(),
        policy: config.policy.clone(),
        balance: Some(cash),
        generate_missing_orders: config.generate_missing_orders,
        reconcile_balance: config.reconcile_balance,
        balance_tol: config.balance_tol,
        route_key: None,
        admitted_symbols: None,
        lists_trigger_orders: false,
    }))
}

/// A USD account of `venue`, told so, holding a symbol quoted in yen when `holding_yen`.
fn usd_engine(venue: &str, holding_yen: bool) -> EngineBoxed {
    let mut engine = dyn_engine(venue, "BTCUSDT", Box::new(RecordingClient::default()));
    engine.set_account_currency("USD").expect("a currency code");
    if holding_yen {
        engine.now_ms = 10;
        let yen = SymbolAdmission {
            route_key: venue.into(),
            symbol: "USDJPY".into(),
            grid: SymbolGrid {
                tick_size: Some(0.001),
                lot_size: Some(1.0),
                ..SymbolGrid::default()
            },
            contract_value: None,
            initial_mark: None,
            initial_mark_ms: None,
            quote_currency: Some("JPY".into()),
            quote_rate: Some(1.0 / 150.0),
            rate_as_of_ms: Some(10),
            initial_margin_requirement: None,
            release: false,
            margin_mode: None,
        };
        assert_eq!(engine.admit_symbol(&yen), AdmitOutcome::Admitted);
    }
    engine
}

/// Deribit's shape: the mounted perpetual is an INVERSE contract, so the ledger is one currency, the coin's.
fn inverse_engine(venue: &str) -> EngineBoxed {
    let mut grid = IndexMap::new();
    grid.insert("BTC-PERPETUAL".to_string(), ContractValue::inverse(1.0));
    Engine::new(
        Account::new(1.0, venue, Some(grid), BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        Box::new(RecordingClient::default()) as Box<dyn ExecutionClient + Send>,
        venue,
        "BTC-PERPETUAL",
    )
}

/// Run two passes of `venue` reporting `cash` and return the snapshot after both. Two, because the first is
/// dispatched at `now_ms == 0` (the test clock hands out the message index), which `recon.last_pass_timestamp`
/// cannot tell from "no pass yet".
fn after_two_passes(
    engine: EngineBoxed,
    venue: &str,
    cash: f64,
    reconcile_balance: bool,
) -> Arc<CoreSnapshot> {
    let handle = spawn_core(engine, test_config(VENUE_CASH));
    let cell = handle.snapshot_cell();
    handle.send_command(pass(venue, cash, reconcile_balance));
    handle.send_command(pass(venue, cash, reconcile_balance));
    wait_for_snapshot(&cell, 5, "both passes ran", |s| s.reconcile.last_pass_timestamp >= 1);
    handle.shutdown_and_join();
    cell.load_full()
}

fn published_balance(snap: &CoreSnapshot) -> f64 {
    snap.portfolio.venues.first().map(|v| v.balance).unwrap_or(f64::NAN)
}

fn published_equity(snap: &CoreSnapshot) -> f64 {
    snap.portfolio.venues.first().map(|v| v.equity).unwrap_or(f64::NAN)
}

fn reconcile_balance_notes(snap: &CoreSnapshot) -> Vec<String> {
    snap.recent_events
        .iter()
        .filter(|l| l.contains("RECON balance"))
        .map(|l| l.to_string())
        .collect()
}

/// The control, in one place: no foreign-quoted symbol, and the pass takes the venue's figure whole.
fn assert_takes_the_figure(snap: &CoreSnapshot, tag: &str) {
    assert_eq!(snap.balance_mode, BalanceMode::Authoritative, "control, {tag}");
    assert!((published_balance(snap) - VENUE_CASH).abs() < 1e-9, "control, {tag}");
    assert!(!reconcile_balance_notes(snap).is_empty(), "control: the adoption is noted, {tag}");
}

/// The withheld shape, in one place: nothing adopted, rolled, diffed or held.
fn assert_withheld(snap: &CoreSnapshot, tag: &str) {
    assert_eq!(snap.balance_mode, BalanceMode::Delta, "the ledger is not Authoritative, {tag}");
    assert!(
        (published_balance(snap) - VENUE_CASH).abs() > 1.0,
        "the venue's figure is not the ledger's, {tag}: {}",
        published_balance(snap)
    );
    assert!(
        reconcile_balance_notes(snap).is_empty(),
        "no adoption, no roll, no drift was noted, {tag}: {:?}",
        reconcile_balance_notes(snap)
    );
    assert!(snap.reconcile.alerts.is_empty(), "and nothing is held for an operator, {tag}");
}

/// **RED until the owner's ruling is applied.** A venue's account balance is in the account's currency, and
/// an engine that holds a foreign-quoted symbol was told that currency, so the pass adopts the figure: both
/// the Feature 2 lane (`reconcile_balance` on: adopt, then roll) and the legacy silent seed (off).
#[test]
fn an_anchored_foreign_quoted_ledger_adopts_the_venues_balance() {
    for reconcile_balance in [true, false] {
        let tag = format!("reconcile_balance={reconcile_balance}");

        let plain =
            after_two_passes(usd_engine("oanda", false), "oanda", VENUE_CASH, reconcile_balance);
        assert_takes_the_figure(&plain, &tag);

        let yen =
            after_two_passes(usd_engine("oanda", true), "oanda", VENUE_CASH, reconcile_balance);
        assert_takes_the_figure(&yen, &format!("the yen ledger, {tag}"));
        assert!(yen.reconcile.alerts.is_empty(), "the first sync and the roll hold nothing, {tag}");
        // Authoritative: the equity is the venue's cash (no unrealized, no seed beside it).
        assert!(
            (published_equity(&yen) - VENUE_CASH).abs() < 1e-9,
            "{tag}: {}",
            published_equity(&yen)
        );
    }
}

/// **Deribit's coin ledger takes the coin figure** (decision 0258): the wallet figure of an inverse engine is a
/// COIN amount (0.1 BTC), which is what its books are kept in, so the pass adopts it; it is published at the
/// coin's price (60,000), 6,000 dollars, and the equity is that wallet.
#[test]
fn an_inverse_coin_ledger_takes_the_coin_balance() {
    for reconcile_balance in [true, false] {
        let tag = format!("reconcile_balance={reconcile_balance}");
        let mut engine = inverse_engine("deribit");
        engine.price_board.set_mark("deribit", "BTC-PERPETUAL", 60_000.0, 0);
        engine.account.set_mark_from(
            "deribit",
            "BTC-PERPETUAL",
            60_000.0,
            MarkSource::VenueMark,
            0,
        );
        let snap = after_two_passes(engine, "deribit", 0.1, reconcile_balance);
        assert_eq!(snap.balance_mode, BalanceMode::Authoritative, "{tag}");
        assert!(
            (published_balance(&snap) - 6_000.0).abs() < 1e-6,
            "{tag}: {}",
            published_balance(&snap)
        );
        assert!(
            (published_equity(&snap) - 6_000.0).abs() < 1e-6,
            "{tag}: {}",
            published_equity(&snap)
        );
        assert!(
            snap.reconcile.alerts.is_empty(),
            "the first sync and the roll hold nothing, {tag}"
        );
    }
}

/// **A coin ledger's drift of 0.01 BTC is HELD, not swallowed** (decision 0258). The tolerance is in the coin: the
/// dollar floor (1) at the engine's last journaled fill price (60,000) is 0.0000167 BTC, where the bare floor read
/// one coin (about 60,000 dollars) and let a 600-dollar divergence pass as noise. The wallet (0.1 BTC) is adopted,
/// a long fills at 60,000, and the venue then reports 0.09: the drift is held for an operator under `quarantine`.
#[test]
fn a_coin_ledgers_drift_of_a_hundredth_of_a_coin_is_held_not_swallowed() {
    let handle = spawn_core(inverse_engine("deribit"), test_config(VENUE_CASH));
    let cell = handle.snapshot_cell();
    handle.send_command(pass("deribit", 0.1, true));
    wait_for_snapshot(&cell, 5, "the first sync", |s| s.balance_mode == BalanceMode::Authoritative);
    let fill = Event::Fill(FillEvent {
        trade_id: TradeId::new("coin-1").expect("non-empty"),
        order_client_id: String::new(),
        venue: "deribit".into(),
        symbol: "BTC-PERPETUAL".into(),
        side: 1,
        last_qty: 1_000.0,
        last_price: 60_000.0,
        commission: 0.0,
        commission_asset: "BTC".into(),
        liquidity_side: "taker".into(),
        timestamp: 1,
        mark_price: Some(60_000.0),
        position_side: PositionSide::Both,
    });
    handle.event_sender().blocking_send(fill).expect("fill");
    wait_for_snapshot(&cell, 5, "the long", |s| s.positions.iter().any(|p| p.size > 0.0));
    let Command::ReconcileReports(mut drifted) = pass("deribit", 0.09, true) else {
        panic!("a pass is a ReconcileReports command")
    };
    drifted.policy =
        ReconcilePolicy { default: ReconcileMode::Quarantine, ..ReconcilePolicy::default() };
    // Twice: the drift is held on its second pass in a row (decision 0043 as amended 2026-11-21).
    handle.send_command(Command::ReconcileReports(drifted.clone()));
    handle.send_command(Command::ReconcileReports(drifted));
    wait_for_snapshot(&cell, 5, "the drift", |s| !s.reconcile.alerts.is_empty());
    let snap = cell.load_full();
    assert_eq!(snap.reconcile.alerts.len(), 1, "{:?}", snap.reconcile.alerts);
    handle.shutdown_and_join();
}

/// **The core names no venue**: the pass of ANY roster venue is adopted into an anchored foreign-quoted ledger,
/// ibkr included (its client withholds itself and sends no balance, so the pass it sends carries none), and a
/// pass that carries NO balance folds none, adopts none and raises no drift on that ledger, whatever the venue.
#[test]
fn the_core_names_no_venue_a_withheld_balance_is_an_absent_one() {
    for venue in ["oanda", "ibkr", "fxcm", "ig", "ctrader"] {
        let snap = after_two_passes(usd_engine(venue, true), venue, VENUE_CASH, true);
        assert_takes_the_figure(&snap, &format!("{venue} holding yen"));
    }
    // a pass whose client answered "not reported": nothing adopted, rolled, diffed or held
    let handle = spawn_core(usd_engine("ibkr", true), test_config(VENUE_CASH));
    let cell = handle.snapshot_cell();
    for _ in 0..2 {
        let Command::ReconcileReports(mut reports) = pass("ibkr", VENUE_CASH, true) else {
            panic!("a pass is a ReconcileReports command")
        };
        reports.balance = None;
        handle.send_command(Command::ReconcileReports(reports));
    }
    wait_for_snapshot(&cell, 5, "both passes ran", |s| s.reconcile.last_pass_timestamp >= 1);
    handle.shutdown_and_join();
    assert_withheld(&cell.load_full(), "an absent balance");
}

/// The wallet a previous run journaled, and the one the venue reports after the restart.
const RESTORED_CASH: f64 = 5_000.0;
const FRESH_CASH: f64 = 7_000.0;

/// What the next boot builds for a foreign-quoted OANDA account: the mount-built engine (`Delta`, told its
/// currency, the yen symbol re-admitted) with the journal's `Authoritative` absolute balance restored onto it
/// (`restore_folded`, the boot restore's account step).
fn restored_engine() -> EngineBoxed {
    let mut before = usd_engine("oanda", true);
    before.account.balance = RESTORED_CASH;
    before.account.balance_mode = BalanceMode::Authoritative;
    let journaled = before.snapshot_state();
    let mut booted = usd_engine("oanda", true);
    booted.restore_runtime_state(&journaled);
    assert_eq!(booted.account.balance_mode, BalanceMode::Authoritative, "precondition: restored");
    assert!((booted.account.balance - RESTORED_CASH).abs() < 1e-9, "precondition: restored");
    booted
}

/// **A restored absolute balance is re-adopted by the first pass, and the seed is not added to it.**
/// `restore_folded` brings the journal's wallet back as `Authoritative` with no sync baseline, so the
/// first pass is a plain `Adopt` that REPLACES the balance (never adds to it) and anchors the baseline
/// beside it; the next pass diffs against that anchor. Without the adoption the restored figure would
/// stay for ever on a ledger that holds a foreign-quoted symbol (the pass withheld the venue's), and
/// every realized PnL since the restart would be missing from `equity_all`.
#[test]
fn a_restored_absolute_balance_is_re_adopted_by_the_first_pass_with_no_seed_double_counted() {
    for reconcile_balance in [true, false] {
        let tag = format!("reconcile_balance={reconcile_balance}");
        let snap = after_two_passes(restored_engine(), "oanda", FRESH_CASH, reconcile_balance);
        assert_eq!(snap.balance_mode, BalanceMode::Authoritative, "{tag}");
        assert!(
            (published_balance(&snap) - FRESH_CASH).abs() < 1e-9,
            "the venue's figure replaced the restored one, {tag}: {}",
            published_balance(&snap)
        );
        // Not the seed on top (7_000 + 10_000), not the restored figure on top (7_000 + 5_000).
        assert!(
            (published_equity(&snap) - FRESH_CASH).abs() < 1e-9,
            "equity is the venue's cash alone, {tag}: {}",
            published_equity(&snap)
        );
        assert!(snap.reconcile.alerts.is_empty(), "an adopt raises no drift, {tag}");
    }
}

/// **A restored `Delta` snapshot is adopted too**: an engine whose journal never saw a venue balance restores
/// into `Delta` (seed + balance + realized + unrealized), and the first pass is the plain first sync, which
/// takes the venue's whole figure and flips the ledger to `Authoritative`: the venue's cash alone is the
/// equity, with neither the seed nor the journal's `Delta` figure on top.
#[test]
fn a_restored_delta_snapshot_is_adopted_by_the_first_pass() {
    for reconcile_balance in [true, false] {
        let tag = format!("reconcile_balance={reconcile_balance}");
        let mut before = usd_engine("oanda", true);
        before.account.balance = -3.0;
        assert_eq!(before.account.balance_mode, BalanceMode::Delta, "precondition: never synced");
        let journaled = before.snapshot_state();
        let mut booted = usd_engine("oanda", true);
        booted.restore_runtime_state(&journaled);
        assert_eq!(
            booted.account.balance_mode,
            BalanceMode::Delta,
            "precondition: restored as Delta"
        );

        let snap = after_two_passes(booted, "oanda", FRESH_CASH, reconcile_balance);
        assert_eq!(snap.balance_mode, BalanceMode::Authoritative, "{tag}");
        assert!((published_balance(&snap) - FRESH_CASH).abs() < 1e-9, "{tag}");
        assert!((published_equity(&snap) - FRESH_CASH).abs() < 1e-9, "{tag}");
        assert!(snap.reconcile.alerts.is_empty(), "{tag}");
    }
}

/// A bare external fill on `USDJPY` (yen-quoted: the converted symbol), `side` 1 buy / -1 sell.
fn yen_fill(tid: &str, side: i32, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::new(tid).expect("test trade ids are non-empty"),
        order_client_id: String::new(),
        venue: "oanda".into(),
        symbol: "USDJPY".into(),
        side,
        last_qty: qty,
        last_price: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        timestamp: 1,
        mark_price: Some(px),
        position_side: PositionSide::Both,
    })
}

/// **A foreign-quoted position with realized PnL, across two passes: the roll and the diff run on the CONVERTED
/// figure.** A round trip on USDJPY (1,000 units, 150.00 to 150.15) realizes 150 yen = 1.00 dollar at the rate the
/// ledger holds (a yen is 1/150 dollar). Pass one adopts the venue's 10,000 and anchors the baseline at the
/// realized 1.00; a second round trip makes it 2.00 and the venue (which booked the same dollars) reports 10,001:
/// pass two rolls the local figure to 10,001 and raises NOTHING. An unexplained 49 on top (10,050) is a held
/// `BalanceDrift`, and the local figure stays at the roll (the declared freeze under `quarantine`).
#[test]
fn a_foreign_quoted_round_trip_rolls_and_diffs_on_the_converted_realized_pnl() {
    let handle = spawn_core(usd_engine("oanda", true), test_config(VENUE_CASH));
    let cell = handle.snapshot_cell();
    let sender = handle.event_sender();
    let round_trip = |tag: &str| {
        sender.blocking_send(yen_fill(&format!("{tag}b"), 1, 1_000.0, 150.00)).expect("fill");
        sender.blocking_send(yen_fill(&format!("{tag}s"), -1, 1_000.0, 150.15)).expect("fill");
    };

    round_trip("a");
    handle.send_command(pass("oanda", VENUE_CASH, true));
    wait_for_snapshot(&cell, 5, "the first sync", |s| s.balance_mode == BalanceMode::Authoritative);
    assert!((published_balance(&cell.load_full()) - VENUE_CASH).abs() < 1e-9, "adopted whole");

    round_trip("b");
    handle.send_command(pass("oanda", VENUE_CASH + 1.0, true));
    wait_for_snapshot(&cell, 5, "the roll", |s| {
        (published_balance(s) - (VENUE_CASH + 1.0)).abs() < 1e-9
    });
    assert!(
        cell.load_full().reconcile.alerts.is_empty(),
        "the venue agrees with the converted PnL"
    );

    // the node's default policy is `quarantine`: the drift is HELD for an operator, not folded
    let Command::ReconcileReports(mut drifted) = pass("oanda", VENUE_CASH + 50.0, true) else {
        panic!("a pass is a ReconcileReports command")
    };
    drifted.policy =
        ReconcilePolicy { default: ReconcileMode::Quarantine, ..ReconcilePolicy::default() };
    // Twice: the drift is held on its second pass in a row (decision 0043 as amended 2026-11-21).
    handle.send_command(Command::ReconcileReports(drifted.clone()));
    handle.send_command(Command::ReconcileReports(drifted));
    wait_for_snapshot(&cell, 5, "the drift", |s| !s.reconcile.alerts.is_empty());
    let snap = cell.load_full();
    assert_eq!(snap.reconcile.alerts.len(), 1, "{:?}", snap.reconcile.alerts);
    assert!(
        (published_balance(&snap) - (VENUE_CASH + 1.0)).abs() < 1e-9,
        "held under quarantine: the local figure stays at the roll"
    );
    handle.shutdown_and_join();
}
