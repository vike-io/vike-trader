//! A reduce-only close the VENUE refused with "nothing to reduce" (`RejectCause::NothingToReduce`) is HELD for
//! the operator as an `OrphanLocalPosition` (`CoreThread::hold_nothing_to_reduce`), and writes nothing.
//!
//! The case is a venue the reconcile leg has not compared yet (dukascopy, `Wiring::SidecarExecRecon`, before its
//! first pass): the venue closed a position the node was never told about (a stop-loss that fired after a sidecar
//! restart, a close made in the platform), the node's book still holds +1000 USDJPY, the flatten is refused by the
//! venue's own order list, and until a pass compares the books nothing said so. These drive a synchronous `CoreThread` (white-box, like `recon_held_tests`, for the same
//! reason: the store, the sweep and `confirm_recon` are private to `runtime`) through the REAL fold
//! (`dispatch(Ingest::Event(..))`) and the REAL timer arm (`drive_due_timers`).
//!
//! What each test pins:
//!
//! * the fold alone raises nothing (the typed rejection is parked; the hold is the cold sweep's), and the
//!   sweep raises one `OrphanLocalPosition` row for the account and symbol with the confirm payload;
//! * NOTHING WRITES to the book: the position size and the realized PnL are exactly what they were
//!   (the rollout rule: no `PositionDrift` without an operator), under the pass policy `quarantine` as well;
//! * the row is visible where the operator looks (the published snapshot's `recon` block, the ring);
//! * the operator's `recon confirm` is the one write: a zero-PnL drop at the position's own price;
//! * a claim for a position the book no longer holds raises nothing, and an untyped refusal never does;
//! * a venue that DOES reconcile converges: a later pass that also finds the row refreshes the SAME row, and a
//!   pass that covers positions and does not raise it clears it;
//! * the sweep is a timer arm of the stuck-order clock.

use super::*;
use std::time::Duration;
use vike_exec::testing::RecordingClient;
use vike_model::events::{
    FillEvent, LiquiditySide, OrderRejected, OrderSubmitted, PositionSide, RejectCause,
};
use vike_model::{ManagedOrder, OrderRequest, OrderType, PositionStatusReport};

use crate::runtime::test_support::{core_of, engine_on};

type Core = CoreThread<RecordingClient>;

const VENUE: &str = "dukascopy";
const SYMBOL: &str = "USDJPY";
const ENTRY_PX: f64 = 156.937;

fn core_with(config: CoreConfig) -> Core {
    core_of(engine_on(VENUE, SYMBOL, RecordingClient::default()), Vec::new(), config)
}

/// The bare fill `trade_id` of `qty` (signed) of USDJPY at the entry price: the lane the account folds.
fn fill(trade_id: &'static str, qty: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: trade_id.into(),
        client_order_id: format!("c-{trade_id}"),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: if qty > 0.0 { 1 } else { -1 },
        last_qty: qty.abs(),
        last_px: ENTRY_PX,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: LiquiditySide::Taker,
        ts: 1,
        mark_price: None,
        position_side: PositionSide::Both,
    })
}

/// A core whose book holds `qty` (signed) of USDJPY, folded through the real lane.
fn core_holding(qty: f64) -> Core {
    let mut c = core_with(CoreConfig::default());
    c.dispatch(Ingest::Event(fill("entry-1", qty)));
    assert_eq!(size(&c), qty, "the premise: the book holds the position");
    c
}

fn size(c: &Core) -> f64 {
    c.eng(0)
        .account
        .positions
        .get(&(ustr::Ustr::from(VENUE), ustr::Ustr::from(SYMBOL), PositionSide::Both))
        .map_or(0.0, |p| p.size)
}

/// A market order for the registry, `Submitted` the way the core leaves one it sent.
fn sent(c: &mut Core, coid: &str, side: i32, reduce_only: bool) {
    let request = OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side,
        qty: 1000.0,
        order_type: OrderType::Market,
        reduce_only,
        ts: 1,
        ..Default::default()
    };
    c.eng_mut(0).registry.insert(coid.into(), ManagedOrder::new(request));
    c.dispatch(Ingest::Event(Event::OrderSubmitted(OrderSubmitted {
        client_order_id: coid.into(),
        ts: 2,
    })));
}

/// The venue's refusal of `coid`, typed or not, through the real fold.
fn refuse(c: &mut Core, coid: &str, cause: Option<RejectCause>) {
    c.dispatch(Ingest::Event(Event::OrderRejected(OrderRejected {
        client_order_id: coid.into(),
        reason: "reduce-only: no open long order to reduce ... this order was NOT sent".into(),
        ts: 3,
        cause,
    })));
}

/// The flatten of a long: a reduce-only sell, refused as "nothing to reduce".
fn flatten_refused(c: &mut Core) {
    sent(c, "flatten", -1, true);
    refuse(c, "flatten", Some(RejectCause::NothingToReduce));
}

/// The one held row, panicking when there is not exactly one.
fn the_row(c: &Core) -> (u64, &HeldReconAlert) {
    let details: Vec<&String> = c.recon_alerts.values().map(|h| &h.detail).collect();
    assert_eq!(c.recon_alerts.len(), 1, "{details:?}");
    let (id, row) = c.recon_alerts.first().expect("one row");
    (*id, row)
}

fn ring(c: &Core, needle: &str) -> usize {
    c.recent.iter().filter(|l| l.contains(needle)).count()
}

fn pass(positions: Vec<PositionStatusReport>) -> ReconcileReports {
    ReconcileReports {
        venue: VENUE.into(),
        since: 0,
        orders: Vec::new(),
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
        route_key: None,
        admitted_symbols: None,
        lists_trigger_orders: false,
    }
}

fn venue_row(symbol: &str, qty: f64) -> PositionStatusReport {
    PositionStatusReport {
        venue: VENUE.into(),
        symbol: symbol.into(),
        position_side: PositionSide::Both,
        qty,
        avg_px: ENTRY_PX,
        ts: 1_000,
        margin_mode: Default::default(),
        isolated_margin: None,
        delta: None,
    }
}

// -------------------------------------------------------------------------------------------------

/// THE CASE. The venue refuses the flatten as "nothing to reduce"; the fold raises nothing by itself (it only
/// parks the claim, on its cold rejection arm); the sweep then holds ONE `OrphanLocalPosition` for the account
/// and symbol, carrying the confirm payload; and the book has not moved by a single unit.
#[test]
fn a_typed_refusal_is_held_as_a_phantom_by_the_sweep_and_the_book_does_not_move() {
    let mut c = core_holding(1000.0);
    let realized = c.eng(0).account.realized_pnl;
    flatten_refused(&mut c);

    // The fold alone held nothing and said nothing: the hold is the cold sweep's.
    assert!(c.recon_alerts.is_empty(), "the fold must not raise the hold");
    assert_eq!(ring(&c, "RECON alert"), 0);

    c.hold_nothing_to_reduce();

    let (_, row) = the_row(&c);
    assert_eq!(row.kind, DivergenceKind::OrphanLocalPosition);
    assert_eq!(row.venue, VENUE);
    assert_eq!(row.route_key, VENUE, "the account whose engine holds the position");
    let o = row.orphan_position.as_ref().expect("the confirm payload");
    assert_eq!(
        (o.symbol.as_str(), o.position_side, o.local_qty),
        (SYMBOL, PositionSide::Both, 1000.0)
    );
    assert!(row.proposed_events.is_empty(), "a hold folds nothing, whatever the policy");
    assert!(!row.confirm_refused);
    assert!(row.detail.contains("VENUE refused a reduce-only close"), "{}", row.detail);
    assert!(row.detail.contains("flatten"), "the refused coid is named: {}", row.detail);
    assert_eq!(ring(&c, "RECON alert"), 1, "announced on the ring, once");

    // NOTHING WRITES: not the size, not the realized PnL (the rollout rule, kept to the letter).
    assert_eq!(size(&c), 1000.0);
    assert_eq!(c.eng(0).account.realized_pnl, realized);

    // Taking the claim emptied the box: a second sweep adds nothing and re-announces nothing.
    c.hold_nothing_to_reduce();
    assert_eq!(c.recon_alerts.len(), 1);
    assert_eq!(ring(&c, "RECON alert"), 1);
}

/// The row is where the operator looks: the published snapshot's `recon` block (what `trade recon ls`,
/// `trade doctor` and the GUI read), under the kind the operator already knows.
#[test]
fn the_held_row_is_published_where_trade_recon_and_trade_doctor_read() {
    let mut c = core_holding(1000.0);
    flatten_refused(&mut c);
    c.hold_nothing_to_reduce();
    c.publish_guarded();
    let snap = c.snapshot.load();
    assert_eq!(snap.recon.alerts.len(), 1);
    let view = &snap.recon.alerts[0];
    assert_eq!(view.kind, "OrphanLocalPosition");
    assert_eq!(view.account, VENUE);
    assert_eq!(view.proposed_event_count, 0);
}

/// The operator's `recon confirm` is the ONE write, and it is the phantom's own cure: a closing fill at the
/// position's OWN average price, so cash and realized PnL do not move.
#[test]
fn the_operators_confirm_drops_the_phantom_at_its_own_price_with_zero_pnl() {
    let mut c = core_holding(1000.0);
    let (cash, realized) = (c.eng(0).account.balance, c.eng(0).account.realized_pnl);
    flatten_refused(&mut c);
    c.hold_nothing_to_reduce();
    let (id, _) = the_row(&c);

    c.confirm_recon(id);

    assert_eq!(size(&c), 0.0, "the phantom is gone from the book");
    assert!(c.recon_alerts.is_empty(), "the confirmed row is removed");
    assert_eq!(c.eng(0).account.balance, cash, "zero PnL: cash untouched");
    assert_eq!(c.eng(0).account.realized_pnl, realized, "zero PnL: realized untouched");
}

/// A claim whose position the book no longer holds is moot: nothing is held, and the ring says why.
#[test]
fn a_claim_for_a_position_the_book_no_longer_holds_raises_nothing() {
    let mut c = core_holding(1000.0);
    flatten_refused(&mut c);
    // The strategy (or the operator) closed it meanwhile: the book is flat before the sweep runs.
    c.dispatch(Ingest::Event(fill("exit-1", -1000.0)));
    assert_eq!(size(&c), 0.0);
    c.hold_nothing_to_reduce();
    assert!(c.recon_alerts.is_empty());
    assert_eq!(ring(&c, "the book holds no position that close could reduce"), 1);
}

/// A claim names the side it could not reduce: a refused SELL says nothing about a SHORT book.
#[test]
fn a_refused_sell_is_no_claim_against_a_short_book() {
    let mut c = core_holding(-1000.0);
    flatten_refused(&mut c);
    c.hold_nothing_to_reduce();
    assert!(
        c.recon_alerts.is_empty(),
        "a sell would have ADDED to the short; the refusal proves nothing"
    );
    assert_eq!(size(&c), -1000.0);
}

/// Words alone are never a claim, and neither is a cause on an order that opens.
#[test]
fn an_untyped_refusal_and_a_cause_on_an_opening_order_hold_nothing() {
    let mut c = core_holding(1000.0);
    sent(&mut c, "untyped", -1, true);
    refuse(&mut c, "untyped", None);
    sent(&mut c, "opening", 1, false);
    refuse(&mut c, "opening", Some(RejectCause::NothingToReduce));
    c.hold_nothing_to_reduce();
    assert!(c.recon_alerts.is_empty());
    assert_eq!(size(&c), 1000.0);
}

/// A venue that DOES reconcile converges. A later pass that also finds the phantom (the venue's report omits the
/// row) REFRESHES the same row, same confirm id, and never adds a second.
#[test]
fn a_later_pass_that_also_finds_it_refreshes_the_same_row() {
    let mut c = core_holding(1000.0);
    flatten_refused(&mut c);
    c.hold_nothing_to_reduce();
    let (id, _) = the_row(&c);

    // The venue reports a position elsewhere and none in USDJPY: `diff` raises its own OrphanLocalPosition.
    c.reconcile_reports(pass(vec![venue_row("AUDUSD", 5.0)]));

    let orphans: Vec<_> = c
        .recon_alerts
        .iter()
        .filter(|(_, h)| h.kind == DivergenceKind::OrphanLocalPosition)
        .collect();
    assert_eq!(orphans.len(), 1, "one divergence, one row");
    assert_eq!(*orphans[0].0, id, "the pass refreshed the claim's row, same confirm id");
    assert_eq!(size(&c), 1000.0, "quarantine: the pass folds nothing either");
}

/// ...and a pass that COVERED positions and reports the venue holding what the book holds clears the row: the
/// venue's own report contradicts the claim, so it is not confirmable.
#[test]
fn a_pass_that_contradicts_the_claim_clears_the_row() {
    let mut c = core_holding(1000.0);
    flatten_refused(&mut c);
    c.hold_nothing_to_reduce();
    assert_eq!(c.recon_alerts.len(), 1);

    c.reconcile_reports(pass(vec![venue_row(SYMBOL, 1000.0)]));

    assert!(c.recon_alerts.is_empty(), "the pass covered positions and no longer raises it");
    assert_eq!(ring(&c, "RECON cleared"), 1);
    assert_eq!(size(&c), 1000.0);
}

/// The sweep rides the stuck-order clock: armed by `submit_ack_timeout`, it holds the claim when that timer arm
/// fires, and not before.
#[test]
fn the_stuck_order_clock_holds_the_claim() {
    let mut c = core_with(CoreConfig {
        submit_ack_timeout: Some(Duration::from_secs(30)),
        ..CoreConfig::default()
    });
    c.dispatch(Ingest::Event(fill("entry-1", 1000.0)));
    c.arm_boundary_timers(0);
    flatten_refused(&mut c);

    c.drive_due_timers(1_000);
    assert!(c.recon_alerts.is_empty(), "the tick (15 s) has not come");

    c.drive_due_timers(20_000);
    assert_eq!(the_row(&c).1.kind, DivergenceKind::OrphanLocalPosition);
    assert_eq!(size(&c), 1000.0);
}
