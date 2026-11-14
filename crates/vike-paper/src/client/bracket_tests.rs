use super::test_support::{bar, drain};
use super::trigger_kinds_tests::apply_all;
use super::*;
use vike_execution::ExecutionClient;
use vike_model::{
    LadderSpec, LadderTarget, OrderBracket, OrderList, OrderListLeg, OrderRecord, OrderRequest,
    OrderStatus, OrderType, build_bracket, build_ladder, build_order_list,
};

fn filled(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::OrderFilled(f) => Some(f.order_client_id.clone()),
            _ => None,
        })
        .collect()
}
fn canceled(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::OrderCanceled(x) => Some(x.order_client_id.clone()),
            _ => None,
        })
        .collect()
}
/// Every `OrderModified` as `(order client id, new size)`: the OUO resize in place.
fn resized(events: &[Event]) -> Vec<(String, f64)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::OrderModified(m) => Some((m.order_client_id.clone(), m.new_qty?)),
            _ => None,
        })
        .collect()
}
/// Every `OrderCanceled` as `(order client id, reason)`, in emit order.
fn cancels(events: &[Event]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::OrderCanceled(x) => Some((x.order_client_id.clone(), x.reason.to_string())),
            _ => None,
        })
        .collect()
}
fn size(c: &ExecutionClientPaper, order_client_id: &str) -> Option<f64> {
    c.pending.iter().find(|(x, _)| x == order_client_id).map(|(_, o)| o.size)
}
fn pairs(rows: &[(&str, &str)]) -> Vec<(String, String)> {
    rows.iter().map(|&(a, b)| (a.to_string(), b.to_string())).collect()
}

/// A long ladder of `qty` on a market entry, its stop at 90 and one target per `(price, qty)`: its
/// requests in leg order, `entry`, `stop`, `t1`.., as `build_ladder` and `build_order_list` give them.
fn ladder(qty: f64, targets: &[(f64, f64)]) -> Vec<OrderRequest> {
    let spec = LadderSpec {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty,
        entry_price: None,
        stop_loss: 90.0,
        targets: targets.iter().map(|&(price, qty)| LadderTarget { price, qty }).collect(),
        account: None,
    };
    let mut order_client_ids = vec!["entry".to_string(), "stop".to_string()];
    order_client_ids.extend((1..=targets.len()).map(|i| format!("t{i}")));
    build_order_list(&build_ladder(&spec).unwrap(), &order_client_ids).unwrap()
}

/// OUO, a target first: t1's fill of 1 shrinks the stop IN PLACE from 3 to 2 (an `OrderModified`)
/// and cancels nothing; the stop's later fill of the 2 left then takes 2 off the targets, the
/// farthest first, which cancels both (`ouo`), and the book is empty.
#[test]
fn a_ladder_target_fill_shrinks_the_stop_and_the_stop_fill_cancels_the_rest() {
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    for leg in ladder(3.0, &[(110.0, 1.0), (120.0, 1.0), (130.0, 1.0)]) {
        c.submit(&leg);
    }
    c.on_bar(&bar(1_000, 100.0, 100.0, 100.0, 100.0)); // the entry fills 3; the exits arm
    drain(&mut c);
    c.on_bar(&bar(2_000, 100.0, 112.0, 99.0, 105.0)); // t1 at 110
    let events = drain(&mut c);
    assert_eq!(filled(&events), ["t1"]);
    assert_eq!(resized(&events), [("stop".to_string(), 2.0)], "the stop loses what t1 filled");
    assert!(cancels(&events).is_empty(), "an OUO fill cancels no target");
    assert_eq!(
        [size(&c, "stop"), size(&c, "t2"), size(&c, "t3")],
        [Some(2.0), Some(1.0), Some(1.0)]
    );
    c.on_bar(&bar(3_000, 100.0, 101.0, 89.0, 95.0)); // the stop at 90
    let events = drain(&mut c);
    assert_eq!(filled(&events), ["stop"]);
    assert_eq!(cancels(&events), pairs(&[("t3", "ouo"), ("t2", "ouo")]), "the farthest first");
    let got: Vec<_> = c.fills.lock().unwrap().iter().map(|f| (f.side, f.size, f.price)).collect();
    assert_eq!(got, [(1, 3.0, 100.0), (-1, 1.0, 110.0), (-1, 2.0, 90.0)]);
    assert!(c.pending.is_empty(), "nothing rests");
}

/// OUO, the stop first, partly: a manual trim left the ladder's position 1 under its stop of 4, so
/// a bar reaching the stop and t1 gives the stop 1 (the cap) and t1 nothing. The stop's fill of 1
/// takes 1 off the FARTHEST target only (t3 at 130, 2 -> 1), never 1 off every target; t2 keeps
/// its size; t1, left nothing in its bar, ends as before. The stop, filled 1 of its 4 and gone from
/// the book with its bar, ENDS (`capped-to-position`): its FSM never stays partially filled.
#[test]
fn a_ladder_stop_partial_fill_shrinks_the_farthest_target_first() {
    let legs = ladder(4.0, &[(110.0, 1.0), (120.0, 1.0), (130.0, 2.0)]);
    let mut stop_state_machine = OrderRecord::new(legs[1].clone());
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    for leg in &legs {
        c.submit(leg);
    }
    c.on_bar(&bar(1_000, 100.0, 100.0, 100.0, 100.0)); // long 4
    let trim = OrderRequest {
        order_client_id: "trim".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 3.0,
        order_type: OrderType::Market,
        ..Default::default()
    };
    c.submit(&trim);
    c.on_bar(&bar(2_000, 100.0, 100.0, 100.0, 100.0)); // long 1, the stop still 4
    apply_all(&mut stop_state_machine, &drain(&mut c));
    c.on_bar(&bar(3_000, 100.0, 111.0, 89.0, 100.0)); // reaches the stop and t1
    let events = drain(&mut c);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::OrderPartiallyFilled(p) if p.order_client_id == "stop")),
        "the stop fills 1 of its 4: {events:?}"
    );
    assert_eq!(resized(&events), [("t3".to_string(), 1.0)], "only the farthest target shrinks");
    assert_eq!([size(&c, "t2"), size(&c, "t3")], [Some(1.0), Some(1.0)]);
    let want = pairs(&[("stop", "capped-to-position"), ("t1", "oco")]);
    assert_eq!(
        cancels(&events),
        want,
        "the stop's remainder ends; t1, capped to nothing in its bar"
    );
    apply_all(&mut stop_state_machine, &events);
    assert_eq!(
        (stop_state_machine.status, stop_state_machine.filled_qty),
        (OrderStatus::Canceled, 1.0)
    );
    let last = c.fills.lock().unwrap().last().map(|f| (f.side, f.size, f.price));
    assert_eq!(last, Some((-1, 1.0, 90.0)));
}

/// A ladder's whole life: every target fills, each shrinking the stop by its size, and the last
/// takes the stop to nothing, so it is cancelled (`ouo`). Every leg's FSM folds every event this
/// book emitted, the resizes included, and ends terminal; the position is flat.
#[test]
fn a_ladder_whose_targets_all_fill_ends_every_leg() {
    let legs = ladder(3.0, &[(110.0, 1.0), (120.0, 2.0)]);
    let mut fsms: Vec<OrderRecord> = legs.iter().cloned().map(OrderRecord::new).collect();
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    for leg in &legs {
        c.submit(leg);
    }
    c.on_bar(&bar(1_000, 100.0, 100.0, 100.0, 100.0)); // the entry fills 3
    c.on_bar(&bar(2_000, 100.0, 112.0, 100.0, 105.0)); // t1: the stop 3 -> 2
    assert_eq!(size(&c, "stop"), Some(2.0));
    c.on_bar(&bar(3_000, 105.0, 125.0, 104.0, 120.0)); // t2: the stop 2 -> nothing
    let events = drain(&mut c);
    assert_eq!(resized(&events), [("stop".to_string(), 2.0)]);
    assert_eq!(cancels(&events), pairs(&[("stop", "ouo")]));
    let statuses: Vec<_> = fsms
        .iter_mut()
        .map(|fsm| {
            apply_all(fsm, &events);
            (fsm.order_client_id().to_string(), fsm.status)
        })
        .collect();
    let want = [
        ("entry", OrderStatus::Filled),
        ("stop", OrderStatus::Canceled),
        ("t1", OrderStatus::Filled),
        ("t2", OrderStatus::Filled),
    ];
    assert_eq!(statuses, want.map(|(order_client_id, s)| (order_client_id.to_string(), s)));
    assert!(c.pending.is_empty() && c.position.abs() < 1e-12, "flat, nothing rests");
}

/// A sell exit `order_client_id` of `qty` on a long, linked OUO to `linked` with no parent: a plain OUO group,
/// not a ladder.
fn ouo_exit(
    order_client_id: &str,
    order_type: OrderType,
    px: f64,
    qty: f64,
    linked: &[&str],
) -> OrderRequest {
    let stop = order_type == OrderType::Stop;
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty,
        order_type,
        price: (!stop).then_some(px),
        trigger_price: stop.then_some(px),
        contingency_type: Some(vike_model::ContingencyType::Ouo),
        linked_order_ids: linked.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

/// A long of `qty` bought at 100 on the first bar, by a plain market order.
fn long(c: &mut ExecutionClientPaper, qty: f64) {
    let entry = OrderRequest {
        order_client_id: "entry".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty,
        order_type: OrderType::Market,
        ..Default::default()
    };
    c.submit(&entry);
    c.on_bar(&bar(1_000, 100.0, 100.0, 100.0, 100.0));
}

/// Any OUO group that is not a ladder's stop takes a fill of q off EACH resting sibling (every leg
/// covers the same size): `a` filling 1 leaves `b` and `c` 2 each, never 1 off the farthest only.
#[test]
fn a_plain_ouo_fill_reduces_each_sibling_by_its_quantity() {
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    long(&mut c, 3.0);
    c.submit(&ouo_exit("a", OrderType::Limit, 110.0, 1.0, &["b", "c"]));
    c.submit(&ouo_exit("b", OrderType::Limit, 120.0, 3.0, &["a", "c"]));
    c.submit(&ouo_exit("c", OrderType::Limit, 130.0, 3.0, &["a", "b"]));
    drain(&mut c);
    c.on_bar(&bar(2_000, 100.0, 112.0, 100.0, 105.0)); // a at 110
    let events = drain(&mut c);
    assert_eq!(filled(&events), ["a"]);
    assert_eq!(resized(&events), [("b".to_string(), 2.0), ("c".to_string(), 2.0)]);
    assert_eq!([size(&c, "b"), size(&c, "c")], [Some(2.0), Some(2.0)]);
}

/// The same bar, a plain OUO pair: neither leg is reduce-only, so both are order-list ENTRIES and
/// fill in the bar's path order. On a bar 12 each way (an exact tie, low first) the stop fills 2
/// first (the cap leaves the target its 3 on a long of 5), and the target then fills what is left
/// of it, 3 - 2 = 1, said with an `OrderModified`, instead of being cancelled as an OCO sibling; a
/// target left nothing that way is cancelled (`ouo`). On a bar whose high is nearer the target
/// fills first, and the stop it leaves nothing is cancelled (`ouo`).
// The owner's 2026-11-08 ruling (a plain stop is a plain order); paper and simulator never disagree.
#[test]
fn a_plain_ouo_pair_hit_in_one_bar_fills_the_later_leg_s_remainder() {
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    long(&mut c, 5.0);
    let stop = ouo_exit("sl", OrderType::Stop, 90.0, 2.0, &["tp"]);
    let take_profit = ouo_exit("tp", OrderType::Limit, 110.0, 3.0, &["sl"]);
    let mut fsms = [OrderRecord::new(stop.clone()), OrderRecord::new(take_profit.clone())];
    c.submit(&stop);
    c.submit(&take_profit);
    c.on_bar(&bar(2_000, 100.0, 112.0, 88.0, 100.0)); // reaches both
    let events = drain(&mut c);
    let got: Vec<_> = c.fills.lock().unwrap().iter().map(|f| (f.side, f.size, f.price)).collect();
    assert_eq!(got, [(1, 5.0, 100.0), (-1, 2.0, 90.0), (-1, 1.0, 110.0)]);
    assert_eq!(resized(&events), [("tp".to_string(), 1.0)]);
    assert_eq!(filled(&events), ["entry", "sl", "tp"], "each fills whole: tp's size is now 1");
    assert!(cancels(&events).is_empty());
    for fsm in &mut fsms {
        apply_all(fsm, &events);
        assert_eq!(fsm.status, OrderStatus::Filled, "{}", fsm.order_client_id());
    }

    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    long(&mut c, 5.0);
    c.submit(&ouo_exit("sl", OrderType::Stop, 90.0, 3.0, &["tp"]));
    c.submit(&ouo_exit("tp", OrderType::Limit, 110.0, 2.0, &["sl"]));
    c.on_bar(&bar(2_000, 100.0, 112.0, 88.0, 100.0));
    let events = drain(&mut c);
    assert_eq!(filled(&events), ["entry", "sl"]);
    assert_eq!(cancels(&events), pairs(&[("tp", "ouo")]), "3 off a target of 2 leaves nothing");

    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    long(&mut c, 5.0);
    c.submit(&ouo_exit("sl", OrderType::Stop, 90.0, 2.0, &["tp"]));
    c.submit(&ouo_exit("tp", OrderType::Limit, 110.0, 3.0, &["sl"]));
    c.on_bar(&bar(2_000, 100.0, 111.0, 85.0, 100.0)); // the high is nearer: the target first
    let events = drain(&mut c);
    let got: Vec<_> = c.fills.lock().unwrap().iter().map(|f| (f.side, f.size, f.price)).collect();
    assert_eq!(got, [(1, 5.0, 100.0), (-1, 3.0, 110.0)]);
    assert_eq!(cancels(&events), pairs(&[("sl", "ouo")]), "3 off a stop of 2 leaves nothing");
}

/// The simulator's `oco_law` PATHS bars `(o, h, l, c)`: the high nearer, the low nearer, an exact
/// tie (low first).
const PATHS: [[f64; 4]; 3] =
    [[100.0, 106.0, 90.0, 100.0], [100.0, 110.0, 94.0, 100.0], [100.0, 106.0, 94.0, 100.0]];

/// One leg of `qty` of an OCO pair with no parent, at `px` (a stop's trigger, else its limit).
fn oco_leg(
    side: i32,
    order_type: OrderType,
    px: f64,
    qty: f64,
    reduce_only: bool,
    other: usize,
) -> OrderListLeg {
    let stop = order_type == OrderType::Stop;
    OrderListLeg {
        side,
        qty,
        order_type,
        price: (!stop).then_some(px),
        trigger_price: stop.then_some(px),
        trail: None,
        reduce_only,
        time_in_force: Default::default(),
        parent: None,
        contingency: Some(vike_model::ContingencyType::Oco),
        linked: vec![other],
        order_client_id: None,
    }
}

/// Which leg of the OCO pair `legs` (order client ids `a`, placed first, then `b`) fills on each PATHS bar,
/// and how much (it must fill whole), after a long of `long_quantity` (none at 0); the other must be
/// cancelled (`oco`) and nothing rest.
fn first_fills(long_quantity: f64, legs: [OrderListLeg; 2]) -> Vec<(String, f64)> {
    let spec = OrderList {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        legs: legs.to_vec(),
        account: None,
    };
    let reqs = build_order_list(&spec, &["a".to_string(), "b".to_string()]).unwrap();
    PATHS
        .iter()
        .map(|&[o, h, l, cl]| {
            let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
            if long_quantity > 0.0 {
                long(&mut c, long_quantity);
            }
            for leg in &reqs {
                c.submit(leg);
            }
            c.on_bar(&bar(2_000, o, h, l, cl));
            let events = drain(&mut c);
            let leg: Vec<(String, f64)> = events
                .iter()
                .filter_map(|e| match e {
                    Event::OrderFilled(f) if f.order_client_id != "entry" => {
                        Some((f.order_client_id.clone(), f.fill.last_qty))
                    }
                    _ => None,
                })
                .collect();
            let [leg] = leg.as_slice() else { panic!("{o} {h} {l}: one leg fills, got {leg:?}") };
            let other = if leg.0 == "a" { "b" } else { "a" };
            assert_eq!(cancels(&events), pairs(&[(other, "oco")]), "{o} {h} {l}");
            assert!(c.pending.is_empty(), "{o} {h} {l}: the other is cancelled, not resting");
            leg.clone()
        })
        .collect()
}

/// `(order client id, qty)` rows from `order_client_ids`, each filling `qty`.
fn legs_of(order_client_ids: [&str; 3], qty: f64) -> Vec<(String, f64)> {
    order_client_ids.iter().map(|c| (c.to_string(), qty)).collect()
}

/// Two legs of one order list hit in one bar fill as the simulator's `order_group_entries` fills
/// them: a leg with no parent that is not reduce-only is an ENTRY, and entries fill in the bar's
/// PATH order (`path_order`: `a` at 105 on a nearer high, `b` at 95 on a nearer low or a tie),
/// never in the order they were placed, and take the one-bar cap in that order
/// (`resolve_intrabar_fills_path_first`); reduce-only exits keep the adverse-first order and cap
/// (the stop `b`). So a straddle fills alike flat or on an open long, and so does a closing pair
/// NOT flagged reduce-only (a plain stop is a plain order, the owner's 2026-11-08 ruling), even
/// when the leg its path reaches first needs the whole cap (2 of each on a long of 2).
#[test]
fn two_order_list_legs_hit_in_one_bar_fill_in_the_simulator_s_order() {
    let path = ["a", "b", "b"];
    let straddle = || {
        let buy = oco_leg(1, OrderType::Stop, 105.0, 1.0, false, 1);
        [buy, oco_leg(-1, OrderType::Stop, 95.0, 1.0, false, 0)]
    };
    assert_eq!(first_fills(0.0, straddle()), legs_of(path, 1.0), "a straddle, flat");
    assert_eq!(first_fills(1.0, straddle()), legs_of(path, 1.0), "a straddle on an open long");
    let closing = |qty, ro| {
        let target = oco_leg(-1, OrderType::Limit, 105.0, qty, ro, 1);
        [target, oco_leg(-1, OrderType::Stop, 95.0, qty, ro, 0)]
    };
    let stop_first = ["b", "b", "b"];
    assert_eq!(first_fills(2.0, closing(1.0, false)), legs_of(path, 1.0), "not reduce-only");
    assert_eq!(first_fills(2.0, closing(1.0, true)), legs_of(stop_first, 1.0), "reduce-only");
    // the path-first leg needs the whole cap: it takes all 2, never the 0 adverse-first leaves it
    assert_eq!(first_fills(2.0, closing(2.0, false)), legs_of(path, 2.0), "the whole cap");
    assert_eq!(first_fills(2.0, closing(2.0, true)), legs_of(stop_first, 2.0), "reduce-only: 2");
}

#[test]
fn bracket_oto_arms_entry_then_oco_cancels_sibling() {
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    // long bracket: market entry, SL 90, TP 110
    let spec = OrderBracket {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        entry_price: None,
        stop_loss: 90.0,
        take_profit: 110.0,
        account: None,
        order_client_id: None,
    };
    for leg in build_bracket(&spec, "entry", "sl", "tp") {
        c.submit(&leg);
    }
    // bar 1: entry (market) fills at open; the exits are HELD (armed by this fill, not filled).
    c.on_bar(&bar(1, 100.0, 105.0, 95.0, 100.0));
    // bar 2: rally to 112 → TP (limit sell 110) fills; SL must be OCO-canceled.
    c.on_bar(&bar(2, 105.0, 112.0, 104.0, 110.0));

    let events: Vec<Event> = std::iter::from_fn(|| c.poll_events()).collect();
    let f = filled(&events);
    assert!(f.contains(&"entry".to_string()), "entry fills first");
    assert!(f.contains(&"tp".to_string()), "TP fills on the rally");
    assert!(!f.contains(&"sl".to_string()), "SL never fills");
    assert!(canceled(&events).contains(&"sl".to_string()), "SL is OCO-canceled when TP fills");
    assert!(resized(&events).is_empty(), "an OCO bracket resizes nothing");
}

#[test]
fn held_exit_does_not_fill_before_entry() {
    // a limit entry that never fills → the exits stay held even where price would trigger them.
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    let spec = OrderBracket {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(50.0), // far below the bar → entry never fills
        stop_loss: 90.0,
        take_profit: 110.0,
        account: None,
        order_client_id: None,
    };
    for leg in build_bracket(&spec, "e2", "sl2", "tp2") {
        c.submit(&leg);
    }
    // range 95..112 would trigger TP(110) if active, but the entry (limit 50) never fills.
    c.on_bar(&bar(1, 100.0, 112.0, 95.0, 108.0));
    let events: Vec<Event> = std::iter::from_fn(|| c.poll_events()).collect();
    assert!(filled(&events).is_empty(), "no leg fills while the entry is unfilled");
}
