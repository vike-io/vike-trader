//! The mount leverage cap's one judge, [`judge_mount_leverage`] (rule v3, round 4): each verb judged ALONE on
//! the mount's own rows, netted on its row in QUANTITY, a covered reduce passing, an order list judged as one
//! (linked legs as alternatives, children where their parents filled), each outcome valued at the worse of
//! the mark and the prices that reach it, a combo as one unit, an unpriced row valued at its entry and an
//! order of unknown value refused.

use super::*;

const UNIT: vike_model::ContractValue = vike_model::ContractValue::linear(1.0);

/// The row `key` of a mount's book holding `size`, entered at 100 and marked at `price`.
fn row(key: &'static str, size: f64, price: Option<f64>) -> MountLeverageRow<&'static str> {
    MountLeverageRow { key, size, average_price: 100.0, price, value: UNIT }
}

/// A leg of `side` · `quantity` at its own `price` (`None`: a market order), armed by leg `parent`'s fill.
fn leg(side: i32, quantity: f64, price: Option<f64>, parent: Option<usize>) -> MountLeverageLeg {
    MountLeverageLeg { side, quantity, price, parent, linked: Vec::new() }
}

/// The same leg linked (OCO or OUO) to the legs `linked`.
fn linked(leg: MountLeverageLeg, linked: &[usize]) -> MountLeverageLeg {
    MountLeverageLeg { linked: linked.to_vec(), ..leg }
}

/// Cap 2 on an equity of 10 000 (the mount may hold 20 000): whether `legs` on `key` pass over `rows`.
fn passes(
    rows: &[MountLeverageRow<&'static str>],
    key: &'static str,
    legs: &[MountLeverageLeg],
) -> bool {
    judge_mount_leverage(Some(2.0), 10_000.0, rows, &[(&key, UNIT, legs)]).is_none()
}

/// NETTED on the order's row: long 100, sell 200 is 100 short after it (10 000), not 300 (30 000); a
/// reversal whose new side is past the cap is refused, in the one word; the other rows count beside it.
#[test]
fn an_order_is_netted_on_its_own_row() {
    let long = [row("BTC", 100.0, Some(100.0))];
    assert!(passes(&long, "BTC", &[leg(-1, 200.0, None, None)]));
    let sell = [leg(-1, 400.0, None, None)];
    let why = judge_mount_leverage(Some(2.0), 10_000.0, &long, &[(&"BTC", UNIT, &sell[..])]);
    assert!(why.is_some_and(|w| w.starts_with(MOUNT_LEVERAGE_REFUSAL)), "300 short is 30 000");
    let beside = [row("BTC", 100.0, Some(100.0)), row("ETH", -110.0, Some(100.0))];
    assert!(!passes(&beside, "BTC", &[leg(-1, 200.0, None, None)]), "10 000 + 11 000");
}

/// The mount's OWN row decides what opens: a mount closes what it holds, and a flat mount's sell OPENS,
/// reduce-only or not; a first trade on an instrument it holds no row of counts beside its other rows.
#[test]
fn the_mounts_own_row_decides_what_opens() {
    assert!(passes(&[row("BTC", 150.0, Some(100.0))], "BTC", &[leg(-1, 150.0, None, None)]));
    assert!(!passes(&[row("BTC", 0.0, Some(100.0))], "BTC", &[leg(-1, 250.0, None, None)]));
    assert!(!passes(&[row("ETH", 150.0, Some(100.0))], "BTC", &[leg(1, 60.0, Some(100.0), None)]));
}

/// Each order is judged ALONE: the same book judges two orders the same whichever comes first, and two
/// that each fit pass though their sum would not (the engine's `RiskGate` bounds the account).
#[test]
fn each_order_is_judged_alone() {
    let rows = [row("BTC", 100.0, Some(100.0))];
    let (a, b) = ([leg(1, 90.0, None, None)], [leg(1, 80.0, None, None)]);
    assert!(passes(&rows, "BTC", &a) && passes(&rows, "BTC", &b), "19 000 and 18 000, each alone");
}

/// OCO siblings are ALTERNATIVES (review #2): an OCO exit pair over the mount's long is a covered reduce,
/// and an OCO pair of two buys of 150 counts one (15 750 at the worse price, 105), never their sum.
#[test]
fn linked_legs_are_alternatives_never_a_sum() {
    let long = [row("BTC", 190.0, Some(100.0))];
    let exits = [
        linked(leg(-1, 190.0, Some(110.0), None), &[1]),
        linked(leg(-1, 190.0, Some(90.0), None), &[0]),
    ];
    assert!(passes(&long, "BTC", &exits), "an exit pair over the long");
    let entries = [
        linked(leg(1, 150.0, Some(95.0), None), &[1]),
        linked(leg(1, 150.0, Some(105.0), None), &[0]),
    ];
    assert!(passes(&[], "BTC", &entries), "one of the two fills");
}

/// An order list's legs count at their OWN prices (review #4), a child where its parent filled: a market
/// buy of 1 arming a breakout buy of 150 at 140 reaches 151 at the worse of the mark and 140 (21 140),
/// refused, where valuing the child at the first leg's price (15 100) would pass; a depth-2 chain adds up.
#[test]
fn a_lists_legs_are_valued_at_their_own_prices() {
    let flat = [row("BTC", 0.0, Some(100.0))];
    let oto = [leg(1, 1.0, None, None), leg(1, 150.0, Some(140.0), Some(0))];
    assert!(!passes(&flat, "BTC", &oto));
    let chain =
        [leg(1, 50.0, None, None), leg(1, 50.0, None, Some(0)), leg(1, 110.0, None, Some(1))];
    assert!(!passes(&flat, "BTC", &chain), "5 000 + 5 000 + 11 000");
    assert!(passes(&flat, "BTC", &chain[..2]), "10 000");
}

/// A BRACKET is its entry: its reduce-only exits, OCO under the entry, net the row in QUANTITY back toward
/// flat, so an entry that fits passes whatever its exits' prices (review round 4 #4: netting in notional
/// counted a take-profit at 300 as 57 000 of profit against the entry's 19 000, and refused it).
#[test]
fn a_bracket_is_its_entry() {
    let flat = [row("BTC", 0.0, Some(100.0))];
    let bracket = |take_profit: f64| {
        [
            leg(1, 190.0, None, None),
            linked(leg(-1, 190.0, Some(90.0), Some(0)), &[2]),
            linked(leg(-1, 190.0, Some(take_profit), Some(0)), &[1]),
        ]
    };
    assert!(passes(&flat, "BTC", &bracket(110.0)), "19 000");
    assert!(passes(&flat, "BTC", &bracket(300.0)), "a far take-profit is no exposure");
    let ladder = [
        leg(1, 190.0, None, None),
        linked(leg(-1, 190.0, Some(90.0), Some(0)), &[2, 3]),
        linked(leg(-1, 95.0, Some(110.0), Some(0)), &[1, 3]),
        linked(leg(-1, 95.0, Some(120.0), Some(0)), &[1, 2]),
    ];
    assert!(passes(&flat, "BTC", &ladder), "a ladder is its entry too");
}

/// An order whose value cannot be known is REFUSED, fail closed (review #5): a market order on an
/// instrument nothing marks (a never-quoted leg, a flat stale row); a priced order there passes, and a
/// covered reduce passes whatever.
#[test]
fn an_order_of_unknown_value_is_refused() {
    let stale = [row("ETH", 0.0, None)];
    assert!(!passes(&stale, "ETH", &[leg(1, 1.0, None, None)]), "no price, no mark");
    assert!(!passes(&[], "ETH", &[leg(1, 1.0, None, None)]), "no row at all");
    assert!(passes(&stale, "ETH", &[leg(1, 1.0, Some(50.0), None)]), "its own price");
    let held = [row("ETH", 5.0, None)];
    assert!(passes(&held, "ETH", &[leg(-1, 5.0, None, None)]), "a reduce is not held up");
    assert!(!passes(&[], "ETH", &[leg(1, f64::NAN, Some(50.0), None)]), "a NaN size");
}

/// A held row nothing prices is valued at its own average entry (round 2 #3 stays): it cannot block the
/// mount for ever, and still counts.
#[test]
fn an_unpriced_row_is_valued_at_its_entry() {
    let small = [row("ETH", 5.0, None)];
    assert!(passes(&small, "BTC", &[leg(1, 1.0, Some(100.0), None)]), "500 + 100");
    let large = [row("ETH", 150.0, Some(f64::NAN))];
    assert!(!passes(&large, "BTC", &[leg(1, 60.0, Some(100.0), None)]), "15 000 + 6 000");
    let inverse = MountLeverageRow {
        value: vike_model::ContractValue::inverse(10.0),
        ..row("ETH", 5.0, None)
    };
    assert!(passes(&[inverse], "BTC", &[leg(1, 1.0, Some(100.0), None)]), "50 + 100");
    let refused = [leg(1, 60.0, Some(100.0), None)];
    let off = judge_mount_leverage(None, 10_000.0, &large, &[(&"BTC", UNIT, &refused[..])]);
    assert_eq!(off, None, "no cap");
}

/// An OPENING order is valued at the WORSE of its own price and the mark (review round 4 #1): a sell limit
/// of 300 at 1 fills near the mark 100 (30 000, refused; valued at its own price it was 300), a buy stop of
/// 250 at 50 triggers at once (25 000, refused; it was 12 500), while a resting buy of 150 at 120, above the
/// mark, counts its own price (18 000) and passes. An inverse contract's notional is its contract value,
/// whatever the price.
#[test]
fn an_opening_order_is_valued_at_the_worse_of_its_price_and_the_mark() {
    let flat = [row("BTC", 0.0, Some(100.0))];
    assert!(!passes(&flat, "BTC", &[leg(-1, 300.0, Some(1.0), None)]), "a sell through the market");
    assert!(!passes(&flat, "BTC", &[leg(1, 250.0, Some(50.0), None)]), "a stop that fires at once");
    assert!(passes(&flat, "BTC", &[leg(1, 150.0, Some(120.0), None)]), "18 000");
    assert!(!passes(&flat, "BTC", &[leg(1, 170.0, Some(120.0), None)]), "20 400");
    let inverse = vike_model::ContractValue::inverse(10.0);
    let book = [MountLeverageRow { value: inverse, ..row("BTC", 0.0, Some(100.0)) }];
    for price in [1.0, 1_000.0] {
        let legs = [leg(1, 1_999.0, Some(price), None)];
        let order = [(&"BTC", inverse, &legs[..])];
        let why = judge_mount_leverage(Some(2.0), 10_000.0, &book, &order);
        assert_eq!(why, None, "19 990 at {price}");
    }
}

/// Cap 2 on 10 000: why a COMBO trading `a` on `A` and `b` on `B` is refused over `rows`.
fn combo(
    rows: &[MountLeverageRow<&'static str>],
    (a, b): (MountLeverageLeg, MountLeverageLeg),
) -> Option<String> {
    let (a, b) = ([a], [b]);
    judge_mount_leverage(Some(2.0), 10_000.0, rows, &[(&"A", UNIT, &a[..]), (&"B", UNIT, &b[..])])
}

/// A COMBO is ONE unit (review round 4 #3), all its legs or none: a roll of the mount's long 150 on `A` into
/// 150 on `B` holds 15 000 before and after (judged leg by leg, `B` alone counted 30 000 beside `A`, refused),
/// and buys of 110 on two flat rows reach 22 000 together (each alone was 11 000), refused.
#[test]
fn a_combo_is_judged_as_one_unit() {
    let rows = [row("A", 150.0, Some(100.0)), row("B", 0.0, Some(100.0))];
    let roll = combo(&rows, (leg(-1, 150.0, None, None), leg(1, 150.0, None, None)));
    assert_eq!(roll, None, "a roll moves its exposure, it adds none");
    let flat = [row("A", 0.0, Some(100.0)), row("B", 0.0, Some(100.0))];
    let both = combo(&flat, (leg(1, 110.0, None, None), leg(1, 110.0, None, None)));
    assert!(both.is_some_and(|w| w.starts_with(MOUNT_LEVERAGE_REFUSAL)), "22 000");
}

/// A limit order of `side`, `left` to fill, at `limit`, as an amend names it.
fn amended(side: i32, left: f64, limit: f64) -> MountLeverageAmended {
    MountLeverageAmended {
        side,
        left,
        kind: Some(vike_model::OrderType::Limit),
        limit: Some(limit),
        trigger: None,
        discretion: None,
        reduce_only: false,
    }
}

/// The amend rule ([`mount_leverage_amend`]): a grow of a live order on its growth, a size-only decrease
/// passing, and a size-only grow of a HELD child on its WHOLE new size after its ancestors (review round 5 #1):
/// a buy of 100 at 99 arming a sell of 100 at 110 grown to 350 reaches 250 short at 110 (27 500, refused),
/// where judging its growth alone counted 150 short (16 500) and let it in.
#[test]
fn an_amend_of_a_held_child_is_judged_on_its_whole_new_size() {
    let flat = [row("BTC", 0.0, Some(100.0))];
    let parent = leg(1, 100.0, Some(99.0), None);
    let child = amended(-1, 100.0, 110.0);
    let legs = mount_leverage_amend(&child, (Some(350.0), None, None), 0.0, vec![parent.clone()]);
    let legs = legs.expect("a grow is judged");
    assert_eq!(legs.last().map(|l| (l.quantity, l.parent)), Some((350.0, Some(0))));
    assert!(!passes(&flat, "BTC", &legs), "27 500");
    let decrease = mount_leverage_amend(&child, (Some(50.0), None, None), 0.0, vec![parent]);
    assert!(decrease.is_none(), "a size-only decrease passes");
    let live = amended(1, 40.0, 100.0);
    let growth = mount_leverage_amend(&live, (Some(190.0), None, None), 60.0, Vec::new());
    assert_eq!(growth.map(|l| l[0].quantity), Some(150.0), "a live order on its growth");
}

/// A PRICE-ONLY amend of a reduce-only exit its own list covers passes (review round 5 #2): long 20 with a
/// held stop-loss of 190 under an entry of 190 (the entry puts 210 on, the exit takes 190 off) adds no
/// exposure, while a repriced opening order is judged on all it rests at its new price.
#[test]
fn a_repriced_covered_exit_passes() {
    let exit = MountLeverageAmended {
        kind: Some(vike_model::OrderType::Stop),
        limit: None,
        trigger: Some(90.0),
        reduce_only: true,
        ..amended(-1, 190.0, 0.0)
    };
    let entry = vec![leg(1, 190.0, None, None)];
    assert!(mount_leverage_amend(&exit, (None, Some(85.0), None), 20.0, entry.clone()).is_none());
    let past = MountLeverageAmended { left: 250.0, ..exit.clone() };
    let uncovered = mount_leverage_amend(&past, (None, Some(85.0), None), 20.0, entry);
    assert!(uncovered.is_some(), "250 is past the 210 its list puts on");
    let opening = amended(1, 150.0, 50.0);
    let repriced = mount_leverage_amend(&opening, (None, Some(140.0), None), 0.0, Vec::new());
    assert_eq!(repriced.map(|l| (l[0].quantity, l[0].price)), Some((150.0, Some(140.0))));
}

/// A STOP-LIMIT keeps its LIMIT as its price when an amend names only a trigger (review round 5 #6); a plain
/// stop's one price is its trigger.
#[test]
fn a_stop_limits_amend_keeps_its_limit_as_its_price() {
    let stop_limit = MountLeverageAmended {
        kind: Some(vike_model::OrderType::StopLimit),
        trigger: Some(120.0),
        ..amended(1, 140.0, 135.0)
    };
    let legs = mount_leverage_amend(&stop_limit, (Some(160.0), None, Some(101.0)), 0.0, Vec::new());
    assert_eq!(legs.map(|l| l[0].price), Some(Some(135.0)));
    let stop =
        MountLeverageAmended { limit: None, kind: Some(vike_model::OrderType::Stop), ..stop_limit };
    let legs = mount_leverage_amend(&stop, (None, Some(101.0), None), 0.0, Vec::new());
    assert_eq!(legs.map(|l| l[0].price), Some(Some(101.0)));
}
