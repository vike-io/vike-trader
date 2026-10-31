//! `Event::FeeAdjustment` through the engine's fold: a LATER correction of the fee of a fill the
//! account already folded (decision 0238). A bare `Fill` with another fee is a duplicate and moves
//! nothing (`Account::apply_fill` is idempotent per `trade_id`), so this event is the only way a fee
//! reaches the ledger after its fill: it books into `balance`, `fees_paid` and `fees_by_asset` once,
//! by the difference, and the per-mount ledger is handed the change as a fee-only `AppliedFill`.

use vike_exec::testing::RecordingClient;
use vike_exec::{EventHandler, ExecutionEngine, Fold, Outbox};
use vike_model::events::{Event, FeeAdjustmentEvent, FillEvent};

use crate::support::EngineBuilder;

const VENUE: &str = "ibkr";
const SYMBOL: &str = "AAPL.SMART.USD";

fn engine() -> ExecutionEngine<RecordingClient> {
    let mut e =
        EngineBuilder { venue: VENUE.into(), symbol: SYMBOL.into(), ..Default::default() }.build();
    e.collect_applied_fills = true; // what the runtime sets iff a strategy is mounted
    e
}

/// A buy fill folded with the fee UNKNOWN: `0.0`, no fee currency (what IBKR's cancel release books).
fn unknown_fee_fill(trade_id: &'static str) -> FillEvent {
    FillEvent {
        trade_id: trade_id.into(),
        client_order_id: "c1".to_string(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: 10.0,
        last_px: 190.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    }
}

fn adjustment(trade_id: &'static str, extra: f64) -> Event {
    Event::FeeAdjustment(FeeAdjustmentEvent {
        trade_id: trade_id.into(),
        client_order_id: "c1".to_string(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        last_px: 190.0,
        extra_commission: extra,
        commission_asset: "USD".into(),
        ts: 2,
    })
}

fn fees(e: &ExecutionEngine<RecordingClient>) -> (f64, f64, Option<f64>) {
    (
        e.account.balance,
        e.account.fees_paid,
        e.account.fees_by_asset.get(&ustr::ustr("USD")).copied(),
    )
}

/// THE FEATURE: the fill booked its fee at 0.0; the real commission arrives later and is booked
/// into the balance, the fee total and the per-asset tally, like a fill's fee would have been.
#[test]
fn a_late_fee_is_booked_into_the_ledger_like_a_fills() {
    let mut e = engine();
    let mut ob = Outbox::default();
    assert_eq!(e.on_event(&Event::Fill(unknown_fee_fill("e1")), &mut ob), Fold::Applied);
    assert_eq!(fees(&e), (0.0, 0.0, None), "the fill carried no fee");

    assert_eq!(e.on_event(&adjustment("e1", 1.25), &mut ob), Fold::Applied);
    assert_eq!(fees(&e), (-1.25, 1.25, Some(1.25)), "the real fee lands once");
}

/// Booked ONCE: the same adjustment again (a re-delivery, IB repeating a commission) books nothing
/// and is dropped, whatever the number of times.
#[test]
fn the_same_adjustment_again_books_nothing() {
    let mut e = engine();
    let mut ob = Outbox::default();
    e.on_event(&Event::Fill(unknown_fee_fill("e1")), &mut ob);
    e.on_event(&adjustment("e1", 1.25), &mut ob);
    let after_one = fees(&e);
    for _ in 0..3 {
        assert_eq!(e.on_event(&adjustment("e1", 1.25), &mut ob), Fold::Dropped);
    }
    assert_eq!(fees(&e), after_one);
}

/// A CHANGED total books the DIFFERENCE, not the new total on top of the old: IB corrects
/// 1.25 to 1.40, the ledger moves by 0.15, and a second correction back down takes it off.
#[test]
fn a_changed_total_books_the_difference() {
    let mut e = engine();
    let mut ob = Outbox::default();
    e.on_event(&Event::Fill(unknown_fee_fill("e1")), &mut ob);
    e.on_event(&adjustment("e1", 1.25), &mut ob);
    assert_eq!(e.on_event(&adjustment("e1", 1.40), &mut ob), Fold::Applied);
    let (balance, paid, usd) = fees(&e);
    assert!((balance + 1.40).abs() < 1e-12 && (paid - 1.40).abs() < 1e-12, "{balance} {paid}");
    assert!((usd.unwrap() - 1.40).abs() < 1e-12);

    assert_eq!(e.on_event(&adjustment("e1", 1.10), &mut ob), Fold::Applied);
    let (balance, paid, usd) = fees(&e);
    assert!((balance + 1.10).abs() < 1e-12 && (paid - 1.10).abs() < 1e-12, "{balance} {paid}");
    assert!((usd.unwrap() - 1.10).abs() < 1e-12);
}

/// A correction whose predecessor was LOST still lands the right total: the event states the
/// total, the ledger books the difference to what it holds.
#[test]
fn a_total_after_a_lost_predecessor_still_lands_the_right_total() {
    let mut e = engine();
    let mut ob = Outbox::default();
    e.on_event(&Event::Fill(unknown_fee_fill("e1")), &mut ob);
    assert_eq!(e.on_event(&adjustment("e1", 1.40), &mut ob), Fold::Applied);
    assert_eq!(fees(&e), (-1.40, 1.40, Some(1.40)));
}

/// A trade id no fill was folded under books nothing: the fee of a fill this ledger never held.
#[test]
fn an_adjustment_of_a_fill_never_folded_books_nothing() {
    let mut e = engine();
    let mut ob = Outbox::default();
    assert_eq!(e.on_event(&adjustment("never-folded", 1.25), &mut ob), Fold::Dropped);
    assert_eq!(fees(&e), (0.0, 0.0, None));
}

/// The hostile-venue guard: a NaN amount does not reach the balance, and the refusal is counted
/// like every other non-finite money event.
#[test]
fn a_non_finite_adjustment_is_refused_and_counted() {
    let mut e = engine();
    let mut ob = Outbox::default();
    e.on_event(&Event::Fill(unknown_fee_fill("e1")), &mut ob);
    assert_eq!(e.on_event(&adjustment("e1", f64::NAN), &mut ob), Fold::Dropped);
    assert_eq!(fees(&e), (0.0, 0.0, None));
    assert_eq!(e.dropped_nonfinite, 1);
    // ...and the trade id was not burned: the real figure still lands.
    assert_eq!(e.on_event(&adjustment("e1", 1.25), &mut ob), Fold::Applied);
}

/// The per-mount ledger is handed the CHANGE as a fee-only entry (zero quantity, no side, the
/// charge in `fx`), and only a booked change: the first correction carries the whole fee, a repeat
/// carries nothing.
#[test]
fn the_mount_ledger_is_handed_each_change_once_as_a_fee_only_entry() {
    let mut e = engine();
    let mut ob = Outbox::default();
    e.on_event(&Event::Fill(unknown_fee_fill("e1")), &mut ob);
    assert_eq!(e.applied_fills.len(), 1);
    assert!(!e.applied_fills[0].is_fee_only(), "a real fill is not a fee-only entry");

    e.on_event(&adjustment("e1", 1.25), &mut ob);
    e.on_event(&adjustment("e1", 1.25), &mut ob);
    e.on_event(&adjustment("e1", 1.40), &mut ob);
    assert_eq!(e.applied_fills.len(), 3, "the repeat produced no entry");
    let charges: Vec<f64> =
        e.applied_fills[1..].iter().map(|a| a.fx.expect("charge").charge).collect();
    assert!(e.applied_fills[1..].iter().all(|a| a.is_fee_only()));
    assert!((charges[0] - 1.25).abs() < 1e-12 && (charges[1] - 0.15).abs() < 1e-12, "{charges:?}");
    assert_eq!(e.applied_fills[1].fill.client_order_id, "c1", "it names the order of the fill");
}

/// A restart: the snapshot carries the correction ledger, so the restored engine books the SAME
/// adjustment once (nothing) and a changed one by the difference, not on top of the total.
#[test]
fn the_correction_ledger_survives_a_snapshot_restore() {
    let mut e = engine();
    let mut ob = Outbox::default();
    e.on_event(&Event::Fill(unknown_fee_fill("e1")), &mut ob);
    e.on_event(&adjustment("e1", 1.25), &mut ob);
    let snap = e.snapshot_state();
    assert_eq!(snap.account.fee_adjusted.len(), 1);

    let mut restored = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
    assert_eq!(fees(&restored), (-1.25, 1.25, Some(1.25)));
    assert_eq!(restored.on_event(&adjustment("e1", 1.25), &mut ob), Fold::Dropped);
    assert_eq!(restored.on_event(&adjustment("e1", 1.50), &mut ob), Fold::Applied);
    let (balance, paid, usd) = fees(&restored);
    assert!((balance + 1.50).abs() < 1e-12 && (paid - 1.50).abs() < 1e-12, "{balance} {paid}");
    assert!((usd.unwrap() - 1.50).abs() < 1e-12);
}

/// Sparse: an engine that never had a fee corrected writes no `fee_adjusted` key, so its canonical
/// JSON and `state_hash` are what they always were.
#[test]
fn an_engine_without_a_correction_writes_no_fee_adjusted_key() {
    let mut e = engine();
    let mut ob = Outbox::default();
    e.on_event(&Event::Fill(unknown_fee_fill("e1")), &mut ob);
    let json = serde_json::to_string(&e.snapshot_state()).expect("serializes");
    assert!(!json.contains("fee_adjusted"), "{json}");
    e.on_event(&adjustment("e1", 1.25), &mut ob);
    let json = serde_json::to_string(&e.snapshot_state()).expect("serializes");
    assert!(json.contains("fee_adjusted"), "{json}");
}
