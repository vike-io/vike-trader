//! **A reconcile pass cannot adopt an unlabelled cash figure into a ledger that holds a foreign-quoted
//! symbol** (decision 0183, found by the independent review of the engine half).
//!
//! `ReconcileReports::balance` is a bare `f64`: the venue's cash with no asset beside it. The ledger of
//! an engine that holds a symbol quoted in another currency is in the ACCOUNT's currency, and a venue
//! whose figure is in some other one (IBKR states the mounted contract's currency: 10,000 EUR of cash)
//! would be adopted on the first sync as 10,000 of a ledger that is in dollars, then rolled forward and
//! diffed from there. `Account::apply_account_state` already refuses that for a labelled balance frame
//! (only the balance whose asset IS the account's currency is the ledger); the reconcile lane had the
//! guard for an inverse engine only. The pass is withheld the same way: both the Feature 2 lane
//! (`reconcile_balance` on: adopt, roll, diff) and the legacy silent seed (off).
//!
//! The control is the same pass on an engine with no foreign-quoted symbol, which takes the figure:
//! the withholding is the foreign symbol and nothing else.

use std::sync::Arc;

use vike_core::spawn_core;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    AdmitOutcome, BalanceMode, Command, CoreSnapshot, ExecutionClient, ExecutionEngine,
    ReconcileReports, SymbolAdmission,
};
use vike_model::SymbolGrid;

use crate::kit::engines::{dyn_engine, test_config};
use crate::kit::handle::wait_for_snapshot;
use crate::kit::requests::PassConfig;

type Engine = ExecutionEngine<Box<dyn ExecutionClient + Send>>;

/// The venue's cash figure, in a currency the report does not name.
const VENUE_CASH: f64 = 10_000.0;

/// A pass reporting [`VENUE_CASH`], built from a `PassConfig` field by field as the reconcile manager
/// builds it.
fn pass(reconcile_balance: bool) -> Command {
    let cfg = PassConfig { reconcile_balance, ..PassConfig::default() };
    Command::ReconcileReports(Box::new(ReconcileReports {
        venue: "binance".into(),
        since: 0,
        orders: Vec::new(),
        fills: Vec::new(),
        positions: Vec::new(),
        policy: cfg.policy.clone(),
        balance: Some(VENUE_CASH),
        generate_missing_orders: cfg.generate_missing_orders,
        reconcile_balance: cfg.reconcile_balance,
        balance_tol: cfg.balance_tol,
        route_key: None,
        admitted_symbols: None,
    }))
}

/// A USD account, told so, holding a symbol quoted in yen when `holding_yen`.
fn usd_engine(holding_yen: bool) -> Engine {
    let mut engine = dyn_engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    engine.set_account_ccy("USD").expect("a currency code");
    if holding_yen {
        engine.now_ms = 10;
        let yen = SymbolAdmission {
            route_key: "binance".into(),
            symbol: "USDJPY".into(),
            grid: SymbolGrid {
                tick_size: Some(0.001),
                lot_size: Some(1.0),
                ..SymbolGrid::default()
            },
            contract_value: None,
            initial_mark: None,
            quote_ccy: Some("JPY".into()),
            quote_rate: Some(1.0 / 150.0),
            rate_as_of_ms: Some(10),
            im_requirement: None,
        };
        assert_eq!(engine.admit_symbol(&yen), AdmitOutcome::Admitted);
    }
    engine
}

/// Run two passes and return the snapshot after both. Two, because the first is dispatched at
/// `now_ms == 0` (the test clock hands out the message index), which `recon.last_pass_ts` cannot tell
/// from "no pass yet".
fn after_two_passes(engine: Engine, reconcile_balance: bool) -> Arc<CoreSnapshot> {
    let handle = spawn_core(engine, test_config(10_000.0));
    let cell = handle.snapshot_cell();
    handle.send_command(pass(reconcile_balance));
    handle.send_command(pass(reconcile_balance));
    wait_for_snapshot(&cell, 5, "both passes ran", |s| s.recon.last_pass_ts >= 1);
    handle.shutdown_and_join();
    cell.load_full()
}

fn published_balance(snap: &CoreSnapshot) -> f64 {
    snap.portfolio.venues.first().map(|v| v.balance).unwrap_or(f64::NAN)
}

fn recon_balance_notes(snap: &CoreSnapshot) -> Vec<String> {
    snap.recent_events
        .iter()
        .filter(|l| l.contains("RECON balance"))
        .map(|l| l.to_string())
        .collect()
}

#[test]
fn a_pass_does_not_take_a_cash_figure_it_cannot_label_into_a_ledger_holding_a_foreign_symbol() {
    for reconcile_balance in [true, false] {
        let tag = format!("reconcile_balance={reconcile_balance}");

        // the control: no foreign symbol, and the pass takes the venue's figure whole
        let plain = after_two_passes(usd_engine(false), reconcile_balance);
        assert_eq!(plain.balance_mode, BalanceMode::Authoritative, "control, {tag}");
        assert!((published_balance(&plain) - VENUE_CASH).abs() < 1e-9, "control, {tag}");
        assert!(!recon_balance_notes(&plain).is_empty(), "control: the adoption is noted, {tag}");

        // the subject: the same passes on an engine that holds yen-quoted USDJPY
        let yen = after_two_passes(usd_engine(true), reconcile_balance);
        assert_eq!(yen.balance_mode, BalanceMode::Delta, "the ledger is not Authoritative, {tag}");
        assert!(
            (published_balance(&yen) - VENUE_CASH).abs() > 1.0,
            "the venue's figure is not the ledger's, {tag}: {}",
            published_balance(&yen)
        );
        assert!(
            recon_balance_notes(&yen).is_empty(),
            "no adoption, no roll, no drift was noted, {tag}: {:?}",
            recon_balance_notes(&yen)
        );
        assert!(yen.recon.alerts.is_empty(), "and nothing is held for an operator, {tag}");
    }
}
