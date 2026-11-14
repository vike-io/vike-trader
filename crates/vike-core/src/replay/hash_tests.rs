//! White-box tests of [`conditionals_hash`] — the book half of the widened determinism fence
//! (emulator PR-6). One test proves the ONE documented exclusion (a trailing arm's ratcheted
//! `extreme`) genuinely cannot move the hash; the rest prove every OTHER part of a book does,
//! so the exclusion is a scalpel rather than a hole.

use super::*;
use vike_journal::OrderEmulationRecord;

fn fixed(arm_id: &str) -> JournalSnapshotOrderEmulationArm {
    JournalSnapshotOrderEmulationArm {
        arm_id: arm_id.into(),
        terms: OrderEmulationRecord {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(95.0),
            trail: None,
            extreme: None,
            trigger_by: None,
            reduce_only: false,
            kind_terms: None,
        },
        route_key: None,
    }
}

fn trailing(arm_id: &str, extreme: f64) -> JournalSnapshotOrderEmulationArm {
    JournalSnapshotOrderEmulationArm {
        arm_id: arm_id.into(),
        terms: OrderEmulationRecord {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: None,
            trail: Some(5.0),
            extreme: Some(extreme),
            trigger_by: None,
            reduce_only: false,
            kind_terms: None,
        },
        route_key: None,
    }
}

/// THE documented exclusion, proven: a trailing arm's `extreme` ratchets on non-journaled
/// market data (the books' analogue of `EngineSnapshotAccount::marks`), so restore reconstructs the
/// last-snapshot value rather than the crash-instant one. Hashing it would fail a valid
/// journal; clearing it must make the two indistinguishable — including the `Some`/`None`
/// shape difference a mark-less reconstruction produces.
#[test]
fn the_ratcheted_extreme_is_excluded_from_the_hash() {
    let live = vec![trailing("a0", 110.0)];
    let restored_stale = vec![trailing("a0", 100.0)];
    let restored_markless = {
        let mut v = vec![trailing("a0", 100.0)];
        v[0].terms.extreme = None;
        v
    };
    assert_eq!(conditionals_hash(&live), conditionals_hash(&restored_stale));
    assert_eq!(conditionals_hash(&live), conditionals_hash(&restored_markless));
}

/// An adjustable SELL stop at 95 that becomes `to` once the price reaches 105, as a `Snapshot` records it,
/// before (`adjusted` false) or after the live book applied the adjustment (market data, never journaled).
fn adjustable(
    to: vike_model::OrderStopAdjustTo,
    adjusted: bool,
) -> JournalSnapshotOrderEmulationArm {
    let mut o = vike_model::OrderResting::new(vike_model::OrderKind::Stop, -1, 1.0);
    o.price = Some(95.0);
    o.adjust = Some(Box::new(vike_model::OrderStopAdjust { at: 105.0, to }));
    if adjusted {
        vike_model::apply_stop_adjust(&mut o, 106.0, 104.0);
        assert!(o.adjust.is_none(), "precondition: the live book adjusted it");
    }
    let terms = OrderEmulationRecord::of_arm("sim".into(), "BTCUSDT".into(), &o, None, true);
    JournalSnapshotOrderEmulationArm { arm_id: "a0".into(), terms, route_key: None }
}

/// **A stop that ADJUSTED between two `Snapshot`s hashes as the one the replay reproduces** (decision 0267):
/// the adjustment is market data, like a trailing extreme, so the live book holds the target while the
/// refold still holds the adjustable stop. For each target the two agree; a target the live book never
/// had (another stop, another limit, another trail) still moves the hash.
#[test]
fn an_adjustment_that_fired_between_snaps_is_excluded_and_its_target_is_fenced() {
    use vike_model::OrderStopAdjustTo;
    let targets = [
        OrderStopAdjustTo::Stop { stop: 101.0 },
        OrderStopAdjustTo::StopLimit { stop: 101.0, limit: 100.5 },
        OrderStopAdjustTo::Trailing { stop: 101.0, trail: Some(4.0), trail_percent: None },
        OrderStopAdjustTo::Trailing { stop: 101.0, trail: None, trail_percent: Some(4.0) },
    ];
    for to in targets {
        let replayed = conditionals_hash(&[adjustable(to, false)]);
        assert_eq!(replayed, conditionals_hash(&[adjustable(to, true)]), "{to:?}");
        let mut tampered = adjustable(to, true);
        tampered.terms.qty = 2.0;
        assert_ne!(replayed, conditionals_hash(&[tampered]), "{to:?}: the qty is fenced");
    }
    let one = conditionals_hash(&[adjustable(OrderStopAdjustTo::Stop { stop: 101.0 }, false)]);
    let other = conditionals_hash(&[adjustable(OrderStopAdjustTo::Stop { stop: 100.0 }, true)]);
    assert_ne!(one, other, "the target's stop is fenced");
    let trail =
        |t| OrderStopAdjustTo::Trailing { stop: 101.0, trail: Some(t), trail_percent: None };
    let one = conditionals_hash(&[adjustable(trail(4.0), false)]);
    assert_ne!(one, conditionals_hash(&[adjustable(trail(5.0), true)]), "the target's trail");
}

/// The scalpel's other edge: everything the journal CAN reproduce is fenced. Each tamper is
/// one field, so a future `conditionals_hash` that over-clears is caught here.
#[test]
fn every_other_field_moves_the_hash() {
    let base = vec![trailing("a0", 110.0)];
    let h = conditionals_hash(&base);

    let mut t = base.clone();
    t[0].arm_id = "a1".into(); // the DISARM key — the most load-bearing field of all
    assert_ne!(h, conditionals_hash(&t), "arm_id (the disarm key) is fenced");

    let mut t = base.clone();
    t[0].terms.trail = Some(6.0);
    assert_ne!(h, conditionals_hash(&t), "the trail DISTANCE is fenced (only the extreme is not)");

    let mut t = base.clone();
    t[0].terms.qty = 2.0;
    assert_ne!(h, conditionals_hash(&t), "qty is fenced");

    let mut t = base.clone();
    t[0].terms.side = 1;
    assert_ne!(h, conditionals_hash(&t), "side is fenced");

    let mut t = base.clone();
    t[0].terms.venue = "other".into();
    assert_ne!(h, conditionals_hash(&t), "venue is fenced");

    let mut t = base.clone();
    t[0].terms.symbol = "ETHUSDT".into();
    assert_ne!(h, conditionals_hash(&t), "symbol is fenced");

    let mut t = vec![fixed("a0")];
    let hf = conditionals_hash(&t);
    t[0].terms.price = Some(96.0);
    assert_ne!(hf, conditionals_hash(&t), "a fixed stop's trigger price is fenced");
}

/// MEMBERSHIP and ORDER are the two properties the fold can get wrong (a resurrected arm, a
/// dropped one, a mis-ordered rebuild) — and books' insertion order IS fire order, so it is
/// state, not presentation.
#[test]
fn membership_and_order_move_the_hash() {
    let two = vec![fixed("a0"), fixed("a1")];
    assert_ne!(
        conditionals_hash(&two),
        conditionals_hash(&[fixed("a0")]),
        "a resurrected/dropped arm moves the hash"
    );
    assert_ne!(
        conditionals_hash(&two),
        conditionals_hash(&[fixed("a1"), fixed("a0")]),
        "fire order is state, so order moves the hash"
    );
    assert_eq!(conditionals_hash(&[]), conditionals_hash(&[]), "empty books agree");
}

/// [`fence_floor`]'s asymmetry, pinned: an overshoot is legal (deliberately produced by a
/// mark-refused tail trailing arm and by a pre-v7 base), an undercount is the id-reuse bug.
#[test]
fn the_counter_fence_is_a_floor_not_an_equality() {
    assert!(fence_floor("arm_sequence", 7, 7).is_ok(), "equality passes");
    assert!(fence_floor("arm_sequence", 9, 7).is_ok(), "an overshoot only wastes ids");
    match fence_floor("arm_sequence", 6, 7) {
        Err(JournalReplayError::RestoreMismatch { field, expected, got }) => {
            assert_eq!((field, expected, got), ("arm_sequence", 7, 6));
        }
        other => panic!("an undercount must be rejected, got {other:?}"),
    }
}
