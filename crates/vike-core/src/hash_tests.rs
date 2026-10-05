//! White-box tests of [`conditionals_hash`] — the book half of the widened determinism fence
//! (emulator PR-6). One test proves the ONE documented exclusion (a trailing arm's ratcheted
//! `extreme`) genuinely cannot move the hash; the rest prove every OTHER part of a book does,
//! so the exclusion is a scalpel rather than a hole.

use super::*;
use vike_journal::ConditionalRecord;

fn fixed(arm_id: &str) -> SnapConditional {
    SnapConditional {
        arm_id: arm_id.into(),
        terms: ConditionalRecord {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(95.0),
            trail: None,
            extreme: None,
            trigger_by: None,
        },
    }
}

fn trailing(arm_id: &str, extreme: f64) -> SnapConditional {
    SnapConditional {
        arm_id: arm_id.into(),
        terms: ConditionalRecord {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: None,
            trail: Some(5.0),
            extreme: Some(extreme),
            trigger_by: None,
        },
    }
}

/// THE documented exclusion, proven: a trailing arm's `extreme` ratchets on non-journaled
/// market data (the books' analogue of `AccountSnapshot::marks`), so restore reconstructs the
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
    assert!(fence_floor("arm_seq", 7, 7).is_ok(), "equality passes");
    assert!(fence_floor("arm_seq", 9, 7).is_ok(), "an overshoot only wastes ids");
    match fence_floor("arm_seq", 6, 7) {
        Err(ReplayError::RestoreMismatch { field, expected, got }) => {
            assert_eq!((field, expected, got), ("arm_seq", 7, 6));
        }
        other => panic!("an undercount must be rejected, got {other:?}"),
    }
}
