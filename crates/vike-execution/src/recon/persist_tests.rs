//! [`persist`]: two passes in a row, judged per divergence (the module doc).

use super::*;
use vike_model::events::{LiquiditySide, PositionSide};
use vike_model::{FillReport, PositionStatusReport};

fn orphan(order_client_id: &str) -> ReconcileDivergence {
    ReconcileDivergence::OrphanLocalOrder { order_client_id: order_client_id.to_string() }
}

/// A `BTCUSDT` drift: the venue holds `venue_qty`, the book `local_quantity`.
fn drift(venue_qty: f64, local_quantity: f64) -> ReconcileDivergence {
    ReconcileDivergence::PositionDrift {
        report: PositionStatusReport {
            venue: "bybit".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            qty: venue_qty,
            average_price: 100.0,
            timestamp: 1,
            margin_mode: Default::default(),
            isolated_margin: None,
            delta: None,
        },
        local_quantity,
    }
}

fn missing_fill(trade_id: &'static str) -> ReconcileDivergence {
    ReconcileDivergence::MissingFill(FillReport {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        trade_id: trade_id.into(),
        venue_order_id: "v1".into(),
        order_client_id: Some("c1".into()),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        timestamp: 1,
    })
}

/// One pass that covers every kind and proves nothing.
fn pass(
    previous: &ReconcileStreak,
    raised: Vec<ReconcileDivergence>,
) -> (Vec<ReconcileDivergence>, ReconcileStreak) {
    persist(previous, raised, |_| true, |_| false)
}

/// The race: raised once, gone by the next pass. Nothing is confirmed and nothing is remembered.
#[test]
fn a_divergence_the_next_pass_no_longer_raises_is_never_confirmed() {
    let raised = vec![orphan("o-1"), drift(1.0, 0.0), missing_fill("t-1")];
    let (confirmed, streak) = pass(&ReconcileStreak::default(), raised);
    assert!(confirmed.is_empty(), "one pass is no proof: {confirmed:?}");
    assert!(!streak.is_empty(), "the pass is remembered");

    let (confirmed, streak) = pass(&streak, Vec::new());
    assert!(confirmed.is_empty());
    assert!(streak.is_empty(), "a covered pass that raised nothing drops every streak");
}

/// `pending` answers for what is raised and not confirmed yet, never for a standing divergence.
#[test]
fn pending_names_only_what_is_not_confirmed_yet() {
    let is_drift = |kind: ReconcileDivergenceKind| kind == ReconcileDivergenceKind::PositionDrift;
    let (_, streak) = pass(&ReconcileStreak::default(), vec![drift(1.0, 0.0), orphan("o-1")]);
    assert!(streak.pending(is_drift));
    let (_, streak) = pass(&streak, vec![drift(1.0, 0.0)]);
    assert!(!streak.pending(is_drift), "confirmed now");
    assert!(!streak.pending(|_| true), "and the orphan was dropped");
}

/// Raised by two passes in a row: confirmed on the second, and on every pass after it that still raises it.
#[test]
fn a_divergence_two_passes_in_a_row_raise_is_confirmed() {
    let raised = || vec![missing_fill("t-1"), drift(1.0, 0.0)];
    let (first, streak) = pass(&ReconcileStreak::default(), raised());
    assert!(first.is_empty());
    let (second, streak) = pass(&streak, raised());
    assert_eq!(second, raised(), "both, in the pass's order");
    let (third, _) = pass(&streak, raised());
    assert_eq!(third, raised(), "a standing divergence stays confirmed");
}

/// The orphan orders `resolve` folds into ONE row are judged one order at a time: two passes that each catch a
/// DIFFERENT order in flight confirm nothing, and an order missing on two passes in a row is confirmed alone.
#[test]
fn orphan_local_orders_are_judged_per_order() {
    let (_, streak) = pass(&ReconcileStreak::default(), vec![orphan("o-1")]);
    let (confirmed, streak) = pass(&streak, vec![orphan("o-2")]);
    assert!(confirmed.is_empty(), "another order each pass: {confirmed:?}");
    let (confirmed, _) = pass(&streak, vec![orphan("o-2"), orphan("o-3")]);
    assert_eq!(confirmed, vec![orphan("o-2")]);
}

/// A drift is the same divergence while its gap stands: the book and the venue may both move (the account keeps
/// trading beside a missed fill). A gap that flips is a new one (another fill in flight).
#[test]
fn a_position_drift_is_the_same_while_its_gap_stands() {
    let (_, streak) = pass(&ReconcileStreak::default(), vec![drift(0.003, 0.002)]);
    let (confirmed, _) = pass(&streak, vec![drift(0.005, 0.004)]);
    assert_eq!(confirmed.len(), 1, "the same one-lot gap after more trading");

    let (_, streak) = pass(&ReconcileStreak::default(), vec![drift(1.0, 0.0)]);
    let (confirmed, _) = pass(&streak, vec![drift(-1.0, 0.0)]);
    assert!(confirmed.is_empty(), "the gap flipped: another race");
}

/// A pass that cannot judge a kind keeps its streaks as they stood; the next pass that can, and raises it, confirms.
#[test]
fn a_pass_that_cannot_judge_a_kind_neither_breaks_nor_extends_its_streak() {
    let (_, streak) = pass(&ReconcileStreak::default(), vec![drift(1.0, 0.0)]);
    let no_positions =
        |kind: ReconcileDivergenceKind| kind != ReconcileDivergenceKind::PositionDrift;
    let (confirmed, streak) = persist(&streak, Vec::new(), no_positions, |_| false);
    assert!(confirmed.is_empty());
    assert!(!streak.is_empty(), "kept: the pass carried no position row");
    let (confirmed, _) = pass(&streak, vec![drift(1.0, 0.0)]);
    assert_eq!(confirmed.len(), 1);
}

/// A divergence other evidence proves is confirmed on its first pass, and the next pass finds it seen.
#[test]
fn a_proven_divergence_is_confirmed_at_once() {
    let proven = |d: &ReconcileDivergence| matches!(d, ReconcileDivergence::MissingFill(_));
    let raised = || vec![missing_fill("t-1"), orphan("o-1")];
    let (confirmed, streak) = persist(&ReconcileStreak::default(), raised(), |_| true, proven);
    assert_eq!(confirmed, vec![missing_fill("t-1")], "the orphan waits for its second pass");
    let (confirmed, _) = pass(&streak, raised());
    assert_eq!(confirmed, raised());
}
