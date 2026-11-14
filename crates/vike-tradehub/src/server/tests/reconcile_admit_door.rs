//! **A reconcile CONFIRM admits the symbol its held row books, when the account's engine does not trade it**
//! (owner, 2026-11-10: four polymarket wallet tokens held as `PositionOnlyExternal` on a LIVE account, whose
//! confirms folded nothing). Driven through the ONE acceptance path into a REAL core (`accept_command` over
//! `admits_any_symbol`'s `Node`), with a faked gatewayhub answer for the facts:
//!
//! * an unknown token is admitted and the confirm adopts the venue's position into the book;
//! * a refused admission refuses the confirm in words and sends nothing, so the row stays held;
//! * a second confirm of the same id books nothing twice;
//! * nothing is committed to the gatewayhub (`SymbolAdmitter::admitted` is never called) and no price lane is opened.

use std::sync::Mutex;
use std::time::Duration;
use vike_gatewayhub_client::trade::SymbolPropertiesRequestError;

use vike_execution::recon::{ReconcileBalanceTolerance, ReconcileMode, ReconcilePolicy};
use vike_execution::{Command, ReconcileReports, SymbolAdmission};
use vike_model::events::PositionSide;
use vike_model::{AccountTier, SymbolGrid};
use vike_tradehub_client::wire::CommandWire;

use super::admits_any_symbol::{Node, wait_until};
use crate::server::control::accept_command;
use crate::server::symbol_admission::{AccountReadiness, SymbolAdmitter};

const VENUE: &str = "polymarket";
/// The token the engine was started on.
const MOUNTED: &str =
    "21742633143463906290569050155826241533067272736897614950488156847949938836455";
/// A wallet token the engine never traded (one of the CI box's four rows).
const TOKEN: &str = "1484359444963781501765067677755406951259550728919736807361564486189514136251";

/// The gatewayhub's facts, faked: a grid for [`TOKEN`], or the refusal it answers. Records what it is asked.
struct Facts {
    refuse: Option<&'static str>,
    calls: Mutex<Vec<String>>,
}

impl Facts {
    fn answering() -> Self {
        Facts { refuse: None, calls: Mutex::new(Vec::new()) }
    }
    fn refusing(why: &'static str) -> Self {
        Facts { refuse: Some(why), calls: Mutex::new(Vec::new()) }
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl SymbolAdmitter for Facts {
    fn admission(
        &self,
        route_key: &str,
        _venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, SymbolPropertiesRequestError> {
        self.calls.lock().unwrap().push(format!("admission:{symbol}"));
        if let Some(why) = self.refuse {
            return Err(SymbolPropertiesRequestError::Refused(why.to_string()));
        }
        Ok(SymbolAdmission {
            route_key: route_key.into(),
            symbol: symbol.into(),
            grid: SymbolGrid {
                tick_size: Some(0.001),
                lot_size: Some(0.01),
                min_qty: Some(5.0),
                ..SymbolGrid::default()
            },
            contract_value: None,
            initial_mark: None,
            initial_mark_ms: None,
            quote_currency: None,
            quote_rate: None,
            rate_as_of_ms: None,
            initial_margin_requirement: None,
            release: false,
            margin_mode: None,
        })
    }
    fn price(
        &self,
        _: &str,
        _: &str,
        symbol: &str,
        subscribe: bool,
    ) -> Result<Option<f64>, String> {
        self.calls.lock().unwrap().push(format!("price:{symbol}:{subscribe}"));
        Ok(None)
    }
    fn admitted(&self, _: &str, _: &str, symbol: &str) {
        self.calls.lock().unwrap().push(format!("admitted:{symbol}"));
    }
}

impl AccountReadiness for Facts {
    fn generation(&self, _: &str) -> u64 {
        0
    }
}

/// One pass under `quarantine` reporting the wallet's [`TOKEN`] holding: held as `PositionOnlyExternal`.
fn wallet_pass() -> Command {
    Command::ReconcileReports(Box::new(ReconcileReports {
        venue: VENUE.into(),
        since: 0,
        orders: Vec::new(),
        fills: Vec::new(),
        positions: vec![vike_model::PositionStatusReport {
            venue: VENUE.into(),
            symbol: TOKEN.into(),
            position_side: PositionSide::Both,
            qty: 11.11,
            average_price: 0.2099,
            timestamp: 1_000,
            margin_mode: Default::default(),
            isolated_margin: None,
            delta: None,
        }],
        policy: ReconcilePolicy { default: ReconcileMode::Quarantine, ..Default::default() },
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: ReconcileBalanceTolerance::default(),
        route_key: None,
        admitted_symbols: None,
        lists_trigger_orders: false,
    }))
}

/// A LIVE polymarket node holding the wallet row; answers the node and the held row.
fn held_node() -> (Node, vike_execution::ReconcileAlertView) {
    let node = Node::start(VENUE, MOUNTED, AccountTier::Live);
    node.core.send_command(wallet_pass());
    let cell = node.core.snapshot_cell();
    wait_until("the wallet row to be held", || !cell.load().reconcile.alerts.is_empty());
    let row = cell.load().reconcile.alerts[0].clone();
    assert_eq!(row.kind, "PositionOnlyExternal");
    assert_eq!(
        row.fill_symbol.map(|s| s.to_string()).as_deref(),
        Some(TOKEN),
        "the row names its token"
    );
    assert!((row.fill_qty - 11.11).abs() < 1e-9, "and the size it would book: {}", row.fill_qty);
    (node, row)
}

/// The confirm through the door, with the published blocks and the held row it names.
fn confirm(
    node: &mut Node,
    id: u64,
    rows: &[vike_execution::ReconcileAlertView],
    facts: &Facts,
) -> Result<(), String> {
    let blocks = node.blocks();
    let sink = node.core.command_sink();
    accept_command(
        CommandWire::ReconcileConfirm { id },
        None,
        &mut node.limits,
        &sink,
        None,
        &[],
        &[],
        &[],
        &blocks,
        &[],
        rows,
        &[],
        Some(facts as &dyn SymbolAdmitter),
        None,
        None,
    )
    .map(|_| ())
    .map_err(|e| e.message())
}

fn held_size(node: &Node, symbol: &str) -> f64 {
    let snap = node.core.snapshot_cell().load();
    snap.positions.iter().filter(|p| p.symbol == symbol).map(|p| p.size).sum()
}

#[test]
fn a_confirm_admits_an_unknown_token_and_the_position_lands_once() {
    let (mut node, row) = held_node();
    let facts = Facts::answering();

    confirm(&mut node, row.id, std::slice::from_ref(&row), &facts)
        .expect("the confirm is accepted");
    wait_until("the venue's position to land in the book", || {
        (held_size(&node, TOKEN) - 11.11).abs() < 1e-9
    });
    let cell = node.core.snapshot_cell();
    wait_until("the row to clear", || cell.load().reconcile.alerts.is_empty());
    assert!(node.blocks().iter().any(|b| b.trades(TOKEN)), "the engine trades the token now");
    let calls = facts.calls();
    assert!(calls.contains(&format!("admission:{TOKEN}")), "the facts were looked up: {calls:?}");
    assert!(
        !calls.iter().any(|c| c.starts_with("admitted:")),
        "nothing told the gatewayhub: {calls:?}"
    );
    assert!(!calls.iter().any(|c| c.ends_with(":true")), "no price lane was opened: {calls:?}");

    // The same id again, with the row the operator saw (now stale): the engine trades the token, so nothing
    // is admitted, and the core's unknown-id note answers. The book is not booked twice.
    confirm(&mut node, row.id, std::slice::from_ref(&row), &facts)
        .expect("a retried confirm is accepted");
    wait_until("the core's unknown-id note", || {
        cell.load()
            .recent_events
            .iter()
            .any(|l| l.contains(&format!("unknown alert id {}", row.id)))
    });
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        (held_size(&node, TOKEN) - 11.11).abs() < 1e-9,
        "booked once: {}",
        held_size(&node, TOKEN)
    );
    node.core.shutdown_and_join();
}

#[test]
fn a_refused_admission_refuses_the_confirm_in_words_and_the_row_stays_held() {
    let (mut node, row) = held_node();
    let facts = Facts::refusing("the market of this token is closed and takes no orders");

    let why = confirm(&mut node, row.id, std::slice::from_ref(&row), &facts)
        .expect_err("an admission the facts refuse refuses the confirm");
    assert!(why.contains("cannot be admitted"), "{why}");
    assert!(why.contains("the market of this token is closed"), "the facts' own words: {why}");
    assert!(why.contains("the row stays held"), "{why}");

    std::thread::sleep(Duration::from_millis(200));
    let snap = node.core.snapshot_cell().load();
    assert!(
        snap.reconcile.alerts.iter().any(|a| a.id == row.id),
        "still held: {:?}",
        snap.reconcile.alerts
    );
    assert_eq!(held_size(&node, TOKEN), 0.0, "nothing booked");
    assert!(!node.blocks().iter().any(|b| b.trades(TOKEN)), "nothing admitted");
    node.core.shutdown_and_join();
}
