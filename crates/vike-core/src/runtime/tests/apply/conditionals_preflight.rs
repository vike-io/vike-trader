//! Tagged submit, conditional fire/disarm, mass cancel, capability preflight, batch routing.

use super::*;

#[test]
fn tagged_submit_registers_minted_order_client_id_for_modify() {
    let mut c = test_core();
    // simulate what drain_broker does for a tagged limit: apply Submit(empty order client id), then register
    let order_client_ids = c.apply_intent(
        OrderIntent::Submit(Box::new(OrderRequest {
            order_client_id: String::new(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: vike_model::OrderType::Limit,
            price: Some(100.0),
            timestamp: 0,
            ..Default::default()
        })),
        0,
    );
    c.strategy_tags.insert("0|sim|BTCUSDT|q1".to_string(), order_client_ids[0].clone());
    // resolve + modify by tag (the drain's path)
    let order_client_id = c.strategy_tags.get("0|sim|BTCUSDT|q1").cloned().unwrap();
    c.apply_intent(
        OrderIntent::Modify {
            order_client_id: order_client_id.clone(),
            new_qty: Some(2.0),
            new_price: None,
            new_trigger_price: None,
        },
        0,
    );
    // the recording client saw exactly one submit; the modify targets the accepted order (no-op
    // pre-accept is fine — this asserts the tag→order client id wiring, not the venue modify)
    assert_eq!(c.engine.client.submissions.len(), 1);
    assert_eq!(c.engine.client.submissions[0].order_client_id, order_client_id);
}

#[test]
fn conditional_fire_is_gated() {
    let mut c = test_core();
    c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
    // arm a stop that a downward bar crosses
    c.apply_intent(
        OrderIntent::ArmConditional(ConditionalIntent {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(95.0),
            trail: None,
            trigger_by: None,
            kind_terms: None,
            account: None,
        }),
        0,
    );
    c.engine.trading_state = TradingState::Halted;
    // fire against a crossing bar via submit_fired's public entry (fire_conditionals_bar)
    let bar = mk_bar(1, 90.0, 92.0);
    c.fire_conditionals_bar("sim", "BTCUSDT", &bar);
    assert!(c.engine.client.submissions.is_empty(), "a fired conditional still crosses RiskGate");
}

#[test]
fn global_mass_cancel_clears_conditional_books() {
    let mut c = test_core();
    c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
    c.apply_intent(
        OrderIntent::ArmConditional(ConditionalIntent {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(95.0),
            trail: None,
            trigger_by: None,
            kind_terms: None,
            account: None,
        }),
        0,
    );
    c.apply_intent(OrderIntent::MassCancel { venue: None, symbol: None, account: None }, 0);
    // a bar that WOULD have crossed the stop now fires nothing (book cleared)
    let bar = mk_bar(1, 90.0, 92.0);
    c.fire_conditionals_bar("sim", "BTCUSDT", &bar);
    assert!(
        c.engine.client.submissions.is_empty(),
        "global mass-cancel must clear armed conditionals"
    );
}

/// **What the position covers of a plain stop or trailing arm fires as a close**: unflagged, an exec thread stalled past
/// 20 s refused it for its age (`vike_bridge_core::exec_actor`'s `closes_at_once`), the stop was spent and the position
/// stayed open. Larger than the position, the part past it is a plain order of its own, with the size the stop asked; on
/// its side or on a flat book the arm can only open and stays plain.
#[test]
fn a_plain_arm_fires_what_the_position_covers_flagged_and_the_rest_plain() {
    for (position, side, qty, expect) in [
        (2.0, -1, 1.0, vec![(1.0, true)]),
        (2.0, -1, 2.0, vec![(2.0, true)]),
        (-2.0, 1, 2.0, vec![(2.0, true)]),
        (2.0, -1, 5.0, vec![(2.0, true), (3.0, false)]),
        (-2.0, 1, 3.0, vec![(2.0, true), (1.0, false)]),
        // A float's dust past the position is no second order.
        (3.0, -1, 3.000_000_000_000_000_4, vec![(3.0, true)]),
        (2.0, 1, 1.0, vec![(1.0, false)]),
        (0.0, -1, 1.0, vec![(1.0, false)]),
    ] {
        for trail in [None, Some(5.0)] {
            let mut c = test_core();
            c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
            c.engine.account.positions.insert(
                ("sim".into(), "BTCUSDT".into(), vike_model::events::PositionSide::Both),
                vike_execution::PositionEntry {
                    size: position,
                    average_price: 100.0,
                    ..Default::default()
                },
            );
            let price = trail.is_none().then_some(if side < 0 { 95.0 } else { 105.0 });
            c.apply_intent(
                OrderIntent::ArmConditional(ConditionalIntent {
                    venue: "sim".into(),
                    symbol: "BTCUSDT".into(),
                    side,
                    qty,
                    price,
                    trail,
                    trigger_by: None,
                    kind_terms: None,
                    account: None,
                }),
                0,
            );
            let through = if side < 0 { 90.0 } else { 110.0 };
            c.fire_conditionals_at_price("sim", "BTCUSDT", through, 1);
            let sent: Vec<(f64, bool)> =
                c.engine.client.submissions.iter().map(|r| (r.qty, r.reduce_only)).collect();
            assert_eq!(sent, expect, "{position} {side} {qty} {trail:?}: {:?}", c.recent);
            assert!(
                c.emulated.cond_engine.is_empty()
                    && c.emulated.order_emulation_arms.values().all(|b| b.is_empty())
            );
        }
    }
}

/// **A close is never refused by a check for opens.** A stop larger than the position, on an account that may only reduce,
/// used to be denied whole under a halt: the stop was spent with the position open. Now its close goes, and the part that
/// would open is left out and said, where the gate, still reading the whole position, would have let it through.
#[test]
fn a_stop_larger_than_the_position_closes_on_a_halted_account_and_opens_nothing() {
    for state in [TradingState::Halted, TradingState::Reducing] {
        let mut c = test_core();
        c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
        c.engine.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), vike_model::events::PositionSide::Both),
            vike_execution::PositionEntry { size: 2.0, average_price: 100.0, ..Default::default() },
        );
        c.apply_intent(
            OrderIntent::ArmConditional(ConditionalIntent {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 3.0,
                price: Some(95.0),
                trail: None,
                trigger_by: None,
                kind_terms: None,
                account: None,
            }),
            0,
        );
        c.engine.trading_state = state;
        c.fire_conditionals_at_price("sim", "BTCUSDT", 90.0, 1);
        let sent: Vec<(f64, bool)> =
            c.engine.client.submissions.iter().map(|r| (r.qty, r.reduce_only)).collect();
        assert_eq!(sent, vec![(2.0, true)], "{state:?}: {:?}", c.recent);
        assert!(c.recent.iter().any(|n| n.contains("NOT sent")), "{state:?}: {:?}", c.recent);
    }
}

/// The disarm verb (emulator PR-2): removing an arm by its minted id means the crossing bar
/// that would have fired it releases nothing, and the operator got a confirmation note.
#[test]
fn disarm_conditional_removes_the_arm_so_it_no_longer_fires() {
    let mut c = test_core();
    c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
    c.apply_intent(
        OrderIntent::ArmConditional(ConditionalIntent {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(95.0),
            trail: None,
            trigger_by: None,
            kind_terms: None,
            account: None,
        }),
        0,
    );
    // the runtime minted `{order_client_id_session}a0` for the first arm of this session
    let arm_id = format!("{}a0", c.order_client_id_gen.state().0);
    c.apply_intent(OrderIntent::DisarmConditional { arm_id: arm_id.clone() }, 1);
    assert!(
        c.recent.back().unwrap().contains("DISARMED"),
        "the disarm is confirmed on the recent-events surface: {:?}",
        c.recent
    );
    let bar = mk_bar(2, 90.0, 92.0); // would have crossed the 95 stop
    c.fire_conditionals_bar("sim", "BTCUSDT", &bar);
    assert!(c.engine.client.submissions.is_empty(), "a disarmed conditional must not fire");
}

/// An unknown/stale arm id is a LOUD no-op — surfaced to recent-events, nothing disturbed,
/// never a panic (the stale-click tolerance the book's own `disarm` documents).
#[test]
fn disarm_unknown_arm_id_is_a_loud_noop_that_disturbs_nothing() {
    let mut c = test_core();
    c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
    c.apply_intent(
        OrderIntent::ArmConditional(ConditionalIntent {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(95.0),
            trail: None,
            trigger_by: None,
            kind_terms: None,
            account: None,
        }),
        0,
    );
    c.apply_intent(OrderIntent::DisarmConditional { arm_id: "nope".into() }, 1);
    assert!(
        c.recent.back().unwrap().contains("unknown arm id"),
        "the refusal must be loud: {:?}",
        c.recent
    );
    // and the REAL arm is untouched — the crossing bar still fires it
    c.fire_conditionals_bar("sim", "BTCUSDT", &mk_bar(2, 90.0, 92.0));
    assert_eq!(c.engine.client.submissions.len(), 1, "the resting arm still fires");
}

/// The disarm routes across (venue, symbol) books by PROBING for the id (the intent carries
/// only `arm_id`), and targets exactly ONE arm — siblings on the same and other symbols keep
/// firing.
#[test]
fn disarm_targets_exactly_one_arm_across_books() {
    let mut c = test_core();
    for (symbol_name, px) in [("BTCUSDT", 95.0), ("BTCUSDT", 93.0), ("ETHUSDT", 95.0)] {
        c.apply_intent(
            OrderIntent::ArmConditional(ConditionalIntent {
                venue: "sim".into(),
                symbol: symbol_name.into(),
                side: -1,
                qty: 1.0,
                price: Some(px),
                trail: None,
                trigger_by: None,
                kind_terms: None,
                account: None,
            }),
            0,
        );
    }
    // arms minted a0 (BTC@95), a1 (BTC@93), a2 (ETH@95); disarm the FIRST BTC one
    let session = c.order_client_id_gen.state().0;
    c.apply_intent(OrderIntent::DisarmConditional { arm_id: format!("{session}a0") }, 1);
    // a bar crossing BOTH BTC stops fires only the surviving a1
    c.fire_conditionals_bar("sim", "BTCUSDT", &mk_bar(2, 90.0, 92.0));
    assert_eq!(c.engine.client.submissions.len(), 1, "only the surviving BTC arm fires");
    // and the ETH book was never touched
    c.fire_conditionals_bar("sim", "ETHUSDT", &mk_bar(3, 90.0, 92.0));
    assert_eq!(c.engine.client.submissions.len(), 2, "the ETH arm still fires");
}

// ── the arm's refusals and its duplicate-id backstop ──────────────────────────────────────

/// The newest line on the recent-events ring — the note an operator reads after the command.
fn last_note(c: &CoreThread<RecordingClient>) -> Option<&str> {
    c.recent.back().map(|m| &**m)
}

/// One conditional on (sim, BTCUSDT): a SELL of 1, with exactly the terms the test names.
fn arm_on_btc(
    price: Option<f64>,
    trail: Option<f64>,
    trigger_by: Option<vike_model::TriggerBy>,
) -> OrderIntent {
    OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price,
        trail,
        trigger_by,
        kind_terms: None,
        account: None,
    })
}

/// How many arms rest across every book.
fn armed_count(c: &CoreThread<RecordingClient>) -> usize {
    c.emulated.order_emulation_arms.values().map(|b| b.len()).sum()
}

/// A TRAILING arm on a symbol with NO mark is REFUSED: a trailing stop ratchets from an extreme
/// that is seeded from the mark, and there is none to seed it with. Every other trailing test seeds
/// a mark first, so this branch of `crates/vike-core/src/runtime/apply/conditional.rs`'s
/// `arm_conditional` was reached by none of them.
///
/// The refusal happens BEFORE the arm id is minted (that function's comment: a refused arm that
/// burned an id would leave a gap in the sequence). The sentinel at the end proves both halves:
/// the identical arm, once the symbol has a mark, IS armed — and takes the FIRST id.
#[test]
fn a_trailing_arm_with_no_mark_is_refused_before_an_id_is_minted() {
    let mut c = test_core();
    assert_eq!(
        c.engine.account.mark_of("sim", "BTCUSDT"),
        None,
        "precondition: the armed symbol has no mark"
    );

    c.apply_intent(arm_on_btc(None, Some(5.0), None), 0);

    assert_eq!(
        last_note(&c),
        Some("trailing-stop REFUSED: no mark to seed the extreme"),
        "the refusal is surfaced: {:?}",
        c.recent
    );
    assert_eq!(armed_count(&c), 0, "nothing was armed");
    assert!(
        c.emulated.cond_engine.is_empty(),
        "no account recorded for an arm that does not exist"
    );
    assert_eq!(c.arm_sequence, 0, "refused before the mint: no arm id was spent");

    // SENTINEL: the same arm with a mark to seed from is armed, under the session's FIRST id.
    c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
    c.apply_intent(arm_on_btc(None, Some(5.0), None), 1);
    let first_id = format!("{}a0", c.order_client_id_gen.state().0);
    let book = c
        .emulated
        .order_emulation_arms
        .get(&("sim".to_string(), "BTCUSDT".to_string()))
        .expect("armed once the symbol has a mark");
    assert_eq!(book.len(), 1);
    assert!(book.contains(&first_id), "the armed trailing stop took the first id, {first_id}");
}

/// An arm naming NEITHER a stop price NOR a trail distance is not a conditional at all: it is
/// ignored, and says so. Nothing is minted, nothing is armed.
#[test]
fn an_arm_with_neither_price_nor_trail_is_ignored_and_arms_nothing() {
    let mut c = test_core();

    c.apply_intent(arm_on_btc(None, None, None), 0);

    assert_eq!(
        last_note(&c),
        Some("ArmConditional: neither price nor trail set — ignored"),
        "the no-op is surfaced: {:?}",
        c.recent
    );
    assert_eq!(armed_count(&c), 0, "nothing was armed");
    assert!(c.emulated.order_emulation_arms.is_empty(), "no book was even opened");
    assert!(c.emulated.cond_engine.is_empty());
    assert_eq!(c.arm_sequence, 0, "no arm id was spent");
}

/// An arm requesting the INDEX lane is refused (the core has no index lane, so it could never
/// fire), on the recent-events ring and before an id is minted.
/// `crates/vike-core/tests/journal/conditional_journal/widened_fence.rs`'s
/// `an_index_arm_is_refused_and_journals_no_arm` proves no arm is JOURNALED; this pins the
/// operator's side of the same refusal, and the id it does not spend.
#[test]
fn an_index_triggered_arm_is_refused_before_an_id_is_minted() {
    let mut c = test_core();
    c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);

    c.apply_intent(arm_on_btc(Some(95.0), None, Some(vike_model::TriggerBy::Index)), 0);

    assert_eq!(
        last_note(&c),
        Some("ArmConditional REFUSED: trigger_by Index — the core has no index lane"),
        "the refusal is surfaced: {:?}",
        c.recent
    );
    assert_eq!(armed_count(&c), 0, "nothing was armed");
    assert!(c.emulated.cond_engine.is_empty());
    assert_eq!(c.arm_sequence, 0, "refused before the mint: no arm id was spent");
}

/// The DUPLICATE-ID backstop, STOP branch. A live id cannot collide — `mint_arm_id` is a monotone
/// counter resumed from the `Snapshot` — so the only way here is a book that already holds the id the
/// core is about to mint, i.e. that resume contract broken upstream. This plants exactly that,
/// white-box, and pins what the arm does about it: the newcomer is refused BY NAME, the resting arm
/// keeps its terms (it is never replaced), and no account is recorded for an arm that never entered
/// a book.
///
/// ⚠ CHARACTERIZATION, not an endorsement: unlike the three refusals above, this branch has ALREADY
/// spent the id when the book says no (`arm_sequence` moved). On a journaled core it has also already
/// written that arm's `OrderEmulationArmed` record — this core has no journal, so that half is read
/// from the code, not asserted here.
#[test]
fn a_stop_arm_whose_minted_id_already_rests_is_refused_and_the_resting_arm_is_kept() {
    let mut c = test_core();
    let next_id = format!("{}a{}", c.order_client_id_gen.state().0, c.arm_sequence);
    let key = ("sim".to_string(), "BTCUSDT".to_string());
    assert!(
        c.emulated.order_emulation_arms.entry(key.clone()).or_default().add_stop(
            next_id.clone(),
            1,
            2.0,
            50.0,
            None
        ),
        "plant: a BUY stop at 50 under the id the core mints next"
    );

    c.apply_intent(arm_on_btc(Some(95.0), None, None), 0);

    let refused = format!("ArmConditional: duplicate arm id {next_id} — REFUSED");
    assert_eq!(last_note(&c), Some(refused.as_str()), "refused by name: {:?}", c.recent);
    let book = &c.emulated.order_emulation_arms[&key];
    assert_eq!(book.len(), 1, "the newcomer did not join the book");
    let (id, resting) = book.iter().next().expect("the planted arm");
    assert_eq!(
        (id, resting.order.side, resting.order.price),
        (next_id.as_str(), 1, Some(50.0)),
        "the resting arm keeps ITS terms — never replaced by the SELL at 95"
    );
    assert!(
        c.emulated.cond_engine.is_empty(),
        "no account recorded for an arm that never entered a book"
    );
    assert_eq!(c.arm_sequence, 1, "the id was spent before the book refused it");
}

/// The DUPLICATE-ID backstop, TRAILING branch — the twin of the test above, through the other
/// `add_*` call (`arm_conditional` spells the refusal once per branch). A mark is seeded so the
/// arm gets past the no-mark refusal and reaches the book.
#[test]
fn a_trailing_arm_whose_minted_id_already_rests_is_refused_and_the_resting_arm_is_kept() {
    let mut c = test_core();
    c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
    let next_id = format!("{}a{}", c.order_client_id_gen.state().0, c.arm_sequence);
    let key = ("sim".to_string(), "BTCUSDT".to_string());
    assert!(
        c.emulated.order_emulation_arms.entry(key.clone()).or_default().add_stop(
            next_id.clone(),
            1,
            2.0,
            50.0,
            None
        ),
        "plant: a BUY stop at 50 under the id the core mints next"
    );

    c.apply_intent(arm_on_btc(None, Some(5.0), None), 0);

    let refused = format!("ArmConditional: duplicate arm id {next_id} — REFUSED");
    assert_eq!(last_note(&c), Some(refused.as_str()), "refused by name: {:?}", c.recent);
    let book = &c.emulated.order_emulation_arms[&key];
    assert_eq!(book.len(), 1, "the newcomer did not join the book");
    let (id, resting) = book.iter().next().expect("the planted arm");
    assert_eq!(
        (id, resting.order.price, resting.order.trail),
        (next_id.as_str(), Some(50.0), None),
        "the resting arm is still the planted STOP — never replaced by the trailing newcomer"
    );
    assert!(
        c.emulated.cond_engine.is_empty(),
        "no account recorded for an arm that never entered a book"
    );
    assert_eq!(c.arm_sequence, 1, "the id was spent before the book refused it");
}

// ── mass cancel ───────────────────────────────────────────────────────────────────────────

/// ⚠ **A SYMBOL WITH NO VENUE CANCELS NOTHING.** `MassCancel { venue: None, symbol: Some(_) }` is
/// the one scope `crates/vike-core/src/runtime/apply/exit.rs`'s `lower_mass_cancel` does not guess
/// at: a symbol names no exchange, so it is ignored, and the ring says so. The replay fold's twin
/// of the rule is pinned by `crates/vike-core/src/replay/fold_tests.rs`'s
/// `mass_cancel_scoping_matches_the_live_arm`; the LIVE arm had no test.
///
/// Every store a mass-cancel reaches is populated first, on the very symbol the cancel names — a
/// resting venue order, a HELD OTO exit and an armed conditional — and the sentinel at the end
/// shows the same symbol WITH its venue reaching all three, so "untouched" is not vacuous.
#[test]
fn a_symbol_scoped_mass_cancel_without_a_venue_is_ignored_and_touches_nothing() {
    let mut c = test_core();
    c.apply_intent(OrderIntent::Submit(market_request("rest-1")), 0);
    c.apply_intent(OrderIntent::Submit(Box::new(held_child_request("held-1"))), 0);
    c.apply_intent(arm_on_btc(Some(95.0), None, None), 0);
    assert!(
        c.engine.registry.get("rest-1").is_some_and(|mo| mo.status.is_live()),
        "precondition: rest-1 is live at the venue"
    );
    assert!(c.held_orders.contains_key("held-1"), "precondition: held-1 is held off the venue");
    assert_eq!(armed_count(&c), 1, "precondition: one conditional armed");

    c.apply_intent(
        OrderIntent::MassCancel { venue: None, symbol: Some("BTCUSDT".into()), account: None },
        0,
    );

    assert!(
        c.engine.client.cancels.is_empty(),
        "no venue order was cancelled: {:?}",
        c.engine.client.cancels
    );
    assert!(
        c.held_orders.contains_key("held-1") && c.contingency.is_held("held-1"),
        "the held exit is still held"
    );
    assert_eq!(armed_count(&c), 1, "the armed conditional still rests");
    assert_eq!(
        last_note(&c),
        Some("MassCancel: symbol without venue is ignored"),
        "the operator is told the cancel did nothing: {:?}",
        c.recent
    );

    // SENTINEL: the same symbol WITH its venue reaches all three.
    c.apply_intent(
        OrderIntent::MassCancel {
            venue: Some("sim".into()),
            symbol: Some("BTCUSDT".into()),
            account: None,
        },
        0,
    );
    assert_eq!(
        c.engine.client.cancels,
        vec!["rest-1".to_string()],
        "the resting order is cancelled"
    );
    assert!(!c.held_orders.contains_key("held-1"), "the held exit is dropped");
    assert_eq!(armed_count(&c), 0, "the arm is cleared");
}

#[test]
fn scoped_mass_cancel_clears_only_its_book() {
    let mut c = test_core();
    c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
    c.apply_intent(
        OrderIntent::ArmConditional(ConditionalIntent {
            venue: "sim".into(),
            symbol: "ETHUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(95.0),
            trail: None,
            trigger_by: None,
            kind_terms: None,
            account: None,
        }),
        0,
    );
    c.engine.account.set_mark_from("sim", "ETHUSDT", 100.0, MarkSource::VenueMark, 0);
    c.apply_intent(
        OrderIntent::MassCancel {
            venue: Some("sim".into()),
            symbol: Some("BTCUSDT".into()),
            account: None,
        },
        0,
    );
    let bar = mk_bar(1, 90.0, 92.0);
    c.fire_conditionals_bar("sim", "ETHUSDT", &bar);
    assert_eq!(c.engine.client.submissions.len(), 1, "the untouched (sim,ETH) book still fires");
}

// ── capability preflight (w2-task-5) ─────────────────────────────────────────────────────

fn venue_request(venue: &str, order_type: &str, tif: vike_model::TimeInForce) -> Box<OrderRequest> {
    Box::new(OrderRequest {
        order_client_id: format!("pf-{venue}-{order_type}"),
        venue: venue.into(),
        symbol: "X".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::from_stored(order_type),
        price: Some(1.0),
        time_in_force: tif,
        ..Default::default()
    })
}

/// A refused submit follows the Combo arm's shape: NOTHING reaches the client, and the order
/// terminalizes locally as `OrderSubmitted` → `OrderRejected` (status `Rejected`) with the
/// machine-readable reason surfaced on the recent-events strip.
#[test]
fn preflight_refusal_synthesizes_terminal_reject_and_never_reaches_client() {
    let mut c = test_core();
    // polymarket wires no trigger orders (a CLOB of limit orders on outcome tokens): a "stop" is refused
    let order_client_ids = c.apply_intent(
        OrderIntent::Submit(venue_request("polymarket", "stop", vike_model::TimeInForce::Gtc)),
        7,
    );
    assert_eq!(order_client_ids.len(), 1, "the refusal still names the order");
    assert!(c.engine.client.submissions.is_empty(), "refused order must not reach the client");
    let mo =
        c.engine.registry.get(&order_client_ids[0]).expect("registered for the FSM to advance");
    assert_eq!(mo.status, vike_model::OrderStatus::Rejected, "terminal reject");
    assert!(
        c.recent.iter().any(|n| n.contains("TRIGGER_UNSUPPORTED: kind=stop venue=polymarket")),
        "machine-readable reason surfaced: {:?}",
        c.recent
    );
}

/// The aster/ig flip this task ships: a non-GTC limit that would silently rest GTC is now a
/// loud deny — while a GTC limit (what actually rests) still submits.
#[test]
fn preflight_flips_silent_time_in_force_ignores_to_loud_denies() {
    let mut c = test_core();
    c.apply_intent(
        OrderIntent::Submit(venue_request("aster", "limit", vike_model::TimeInForce::Ioc)),
        0,
    );
    assert!(c.engine.client.submissions.is_empty(), "aster Ioc limit is refused");
    assert!(c.recent.iter().any(|n| n.contains("TIF_UNSUPPORTED: tif=Ioc venue=aster")));
    // A refused order registers `Rejected` under its order client id, and that id is spent (the held-order-client-id
    // refusal): the corrected order carries its own, as the door tells an operator to do.
    let mut gtc = venue_request("aster", "limit", vike_model::TimeInForce::Gtc);
    gtc.order_client_id = "pf-aster-limit-gtc".into();
    c.apply_intent(OrderIntent::Submit(gtc), 0);
    assert_eq!(c.engine.client.submissions.len(), 1, "aster GTC limit still submits");
}

/// A lifetime the venue's adapter would RUN AS ANOTHER ONE is refused by name at the core edge now that
/// an operator or a strategy can name one (polymarket's `Ioc` would be a `Fok`): a terminal rejection that
/// says so, never an order that rests as something else. The honest lifetime and the default still submit.
#[test]
fn a_coerced_lifetime_is_refused_by_name_at_the_core_edge() {
    let mut c = test_core();
    c.apply_intent(
        OrderIntent::Submit(venue_request("polymarket", "limit", vike_model::TimeInForce::Ioc)),
        0,
    );
    assert!(c.engine.client.submissions.is_empty(), "the coerced lifetime is refused");
    assert!(
        c.recent
            .iter()
            .any(|n| n.contains("TIF_COERCED: tif=Ioc would run as Fok venue=polymarket")),
        "{:?}",
        c.recent
    );
}

/// THE COMPAT LAW at the core edge: everything venues accept-and-honor today still submits —
/// binance's perp-lane GTD (lane union), the honest lifetimes of the coercion venues, and every
/// non-roster (sim/paper) venue id. (The coerced lifetimes themselves are refused by name:
/// `a_coerced_lifetime_is_refused_by_name_at_the_core_edge`.)
#[test]
fn preflight_passes_accepted_requests_through() {
    let mut c = test_core();
    for (venue, ot, tif) in [
        ("binance", "limit", vike_model::TimeInForce::Gtd), // perp lane wires native GTD
        ("polymarket", "limit", vike_model::TimeInForce::Fok), // honest on the CLOB
        ("hyperliquid", "take_profit", vike_model::TimeInForce::Gtc), // native tpsl trigger
        ("sim", "limit", vike_model::TimeInForce::Ioc),     // non-roster venue: no row
    ] {
        let before = c.engine.client.submissions.len();
        c.apply_intent(OrderIntent::Submit(venue_request(venue, ot, tif)), 0);
        assert_eq!(
            c.engine.client.submissions.len(),
            before + 1,
            "{venue}/{ot}/{tif:?} must reach the client"
        );
    }
}

/// A bracket with a preflight-refused child is refused ATOMICALLY: no leg reaches the
/// client, the culprit carries its own reason, the siblings carry the culprit's order client id.
#[test]
fn bracket_with_unsupported_child_is_refused_whole() {
    let mut c = test_core();
    let spec = vike_model::OrderBracket {
        venue: "polymarket".into(), // no native trigger orders → the SL "stop" child is refused
        symbol: "X".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
        account: None,
        order_client_id: None,
    };
    let order_client_ids = c.apply_intent(OrderIntent::Bracket(Box::new(spec)), 0);
    assert_eq!(order_client_ids.len(), 3);
    assert!(c.engine.client.submissions.is_empty(), "no bracket leg may reach the client");
    for order_client_id in &order_client_ids {
        let mo = c.engine.registry.get(order_client_id).expect("every leg registered");
        assert_eq!(mo.status, vike_model::OrderStatus::Rejected, "{order_client_id} terminal");
    }
    assert!(c.recent.iter().any(|n| n.contains("TRIGGER_UNSUPPORTED: kind=stop venue=polymarket")));
    assert!(c.recent.iter().any(|n| n.contains("BRACKET_ATOMIC_REFUSED: culprit=")));
}

fn roster_bracket(venue: &str) -> vike_model::OrderBracket {
    vike_model::OrderBracket {
        venue: venue.into(),
        symbol: "X".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
        account: None,
        order_client_id: None,
    }
}

/// A roster engine of `venue` that was TOLD `tier` (`Engine::set_tier`, what the mount does), or
/// left at `new`'s seed when `tier` is `None` (an engine nobody told: not proof of a simulator).
fn engine_told(venue: &str, tier: Option<vike_model::AccountTier>) -> Engine<RecordingClient> {
    let mut e = engine_on(venue, "X", RecordingClient::default());
    if let Some(tier) = tier {
        e.set_tier(tier);
    }
    e
}

/// **A bracket whose EXIT the venue's adapter would refuse by name is refused WHOLE, in words, before
/// the entry is sent.** The exits are held until the entry fills and are never preflighted again, so
/// before the row said anything about a reduce-only kind (`VenueCapabilities::reduce_only_kinds`) the entry
/// filled and the adapter then refused the exit: IG places a reduce-only MARKET only (its close
/// endpoint cannot rest a stop or a limit), leaving the position naked or without its profit target.
/// (Dukascopy was the second venue until its sidecar took a reduce-only LIMIT as the native
/// take-profit: its bracket now passes, `a_bracket_the_venue_can_rest_is_still_accepted_and_holds_its_exits`.)
///
/// The culprit is the first refused leg: IG's stop-loss. Every leg is registered and Rejected,
/// nothing reaches the client, nothing is held, and the reason on EVERY leg names the leg and the venue.
///
/// **On every tier but a stated `paper` one**: a demo engine, a live engine and an engine nobody told
/// (the seed, which is not proof of a simulator) are refused exactly as before (decision 0201 relaxes a
/// PAPER book only, `a_paper_book_takes_the_bracket_the_live_adapter_would_refuse_and_says_so_once`).
#[test]
fn a_bracket_whose_exit_the_venue_cannot_rest_is_refused_whole_in_words() {
    use vike_model::AccountTier::{Demo, Live};
    for (venue, tier, leg, culprit_reason) in [
        ("ig", None, "stop_loss", "REDUCE_ONLY_UNSUPPORTED: kind=stop venue=ig"),
        ("ig", Some(Demo), "stop_loss", "REDUCE_ONLY_UNSUPPORTED: kind=stop venue=ig"),
        ("ig", Some(Live), "stop_loss", "REDUCE_ONLY_UNSUPPORTED: kind=stop venue=ig"),
    ] {
        let mut c = core_of(engine_told(venue, tier), Vec::new(), CoreConfig::default());
        let order_client_ids =
            c.apply_intent(OrderIntent::Bracket(Box::new(roster_bracket(venue))), 0);
        assert_eq!(order_client_ids.len(), 3, "{venue}: every leg is named");
        assert!(c.engine.client.submissions.is_empty(), "{venue}: the ENTRY must not be sent");
        assert!(c.held_orders.is_empty(), "{venue}: no exit may be held for an entry never sent");
        // IG's operator stop is now held by the core (`arm_core_held_stop`), but a bracket's stop-loss
        // is NOT: the bracket stays refused whole and arms nothing, so it is never half protected.
        assert_eq!(armed_count(&c), 0, "{venue}: a refused bracket arms no core-held stop");
        assert!(
            c.emulated.cond_engine.is_empty() && !c.tick_conditionals,
            "{venue}: nor the tick lane"
        );
        for order_client_id in &order_client_ids {
            let mo = c.engine.registry.get(order_client_id).expect("every leg registered");
            assert_eq!(
                mo.status,
                vike_model::OrderStatus::Rejected,
                "{venue}: {order_client_id} terminal"
            );
        }
        assert!(
            c.recent.iter().any(|n| n.contains(culprit_reason)),
            "{venue}: the culprit leg says why: {:?}",
            c.recent
        );
        let tail = format!("leg={leg} venue={venue}");
        assert!(
            c.recent
                .iter()
                .any(|n| n.contains("BRACKET_ATOMIC_REFUSED: culprit=") && n.contains(&tail)),
            "{venue}: a sibling names the refused leg and the venue: {:?}",
            c.recent
        );
        assert!(
            c.recent.iter().any(|n| n.contains(&format!("bracket for {venue} REFUSED WHOLE"))),
            "{venue}: one line says it in words: {:?}",
            c.recent
        );
        assert!(
            !c.recent.iter().any(|n| n.starts_with("paper:")),
            "{venue} {tier:?}: nothing was let through, so there is nothing to confess: {:?}",
            c.recent
        );
    }
}

/// The control of the test above, and the PRESERVE half: a venue whose adapter places both exits
/// (binance's perp lane: `reduceOnly` on the stop and on the limit) still takes the bracket, the
/// entry going out and both exits held.
#[test]
fn a_bracket_the_venue_can_rest_is_still_accepted_and_holds_its_exits() {
    for venue in ["binance", "fxcm", "ctrader", "deribit", "dukascopy"] {
        let mut c = core_of(
            engine_on(venue, "X", RecordingClient::default()),
            Vec::new(),
            CoreConfig::default(),
        );
        let order_client_ids =
            c.apply_intent(OrderIntent::Bracket(Box::new(roster_bracket(venue))), 0);
        assert_eq!(order_client_ids.len(), 3, "{venue}");
        assert_eq!(c.engine.client.submissions.len(), 1, "{venue}: only the entry is sent");
        assert_eq!(c.engine.client.submissions[0].order_client_id, order_client_ids[0], "{venue}");
        assert_eq!(c.held_orders.len(), 2, "{venue}: stop-loss and take-profit held");
    }
}

/// The `paper:` confessions on the recent-events ring (`CoreThread::note_paper_admit`), oldest first.
fn paper_notes(c: &CoreThread<RecordingClient>) -> Vec<String> {
    c.recent.iter().filter(|n| n.starts_with("paper:")).map(|n| n.to_string()).collect()
}

/// **A PAPER book takes the bracket the live adapter would refuse, and says so once** (decision 0201).
/// The exits of a paper bracket go to the simulator, never to the IG adapter whose refusals
/// `VenueCapabilities::reduce_only_kinds` lists, so the engine the mount TOLD `paper` is not judged by that axis:
/// the entry is sent, both exits are held, and the ring says, per `(venue, kind)` and once for the run,
/// that the live adapter would refuse the leg (IG: the stop-loss and the take-profit). A second bracket
/// is silent.
#[test]
fn a_paper_book_takes_the_bracket_the_live_adapter_would_refuse_and_says_so_once() {
    use vike_model::AccountTier::Paper;
    {
        let (venue, kinds) = ("ig", &["stop", "limit"][..]);
        let mut c = core_of(engine_told(venue, Some(Paper)), Vec::new(), CoreConfig::default());
        for round in 1..=2usize {
            let order_client_ids =
                c.apply_intent(OrderIntent::Bracket(Box::new(roster_bracket(venue))), round as i64);
            assert_eq!(order_client_ids.len(), 3, "{venue}");
            assert_eq!(c.engine.client.submissions.len(), round, "{venue}: one entry per bracket");
            assert_eq!(c.engine.client.submissions[round - 1].order_client_id, order_client_ids[0]);
            assert_eq!(c.held_orders.len(), 2 * round, "{venue}: both exits of each bracket held");
            // The entry is on the engine; the exits wait in the core until it fills (nothing registered yet).
            let entry =
                c.engine.registry.get(&order_client_ids[0]).expect("the entry is registered");
            assert_ne!(
                entry.status,
                vike_model::OrderStatus::Rejected,
                "{venue}: the entry is live"
            );
            for exit in &order_client_ids[1..] {
                assert!(c.held_orders.contains_key(exit), "{venue}: {exit} is held for the entry");
            }
            assert!(
                !c.recent.iter().any(|n| n.contains("REFUSED WHOLE")),
                "{venue}: nothing was refused: {:?}",
                c.recent
            );
            let notes = paper_notes(&c);
            assert_eq!(
                notes.len(),
                kinds.len(),
                "{venue} round {round}: one line per refused kind, ever: {notes:?}"
            );
            for kind in kinds {
                let words = format!("the live {venue} adapter would refuse this ({kind})");
                assert!(
                    notes.iter().filter(|n| n.contains(&words)).count() == 1,
                    "{venue}: exactly one line says `{words}`: {notes:?}"
                );
            }
        }
    }
}

/// A plain reduce-only LIMIT or STOP on a paper book of IG reaches the simulator and is noted
/// once per `(venue, kind)`; the same order on a demo, a live and an untold engine is refused as it was.
/// The reduce-only MARKET (a flatten) is in every row and is neither refused nor noted.
#[test]
fn a_paper_books_reduce_only_limit_and_stop_are_sent_and_other_tiers_refuse_them() {
    use vike_model::AccountTier::{Demo, Live, Paper};
    use vike_model::TimeInForce::Gtc;
    let reduce = |venue: &str, kind: &str, n: u32| {
        let mut r = *venue_request(venue, kind, Gtc);
        r.reduce_only = true;
        r.side = -1;
        r.order_client_id = format!("ro-{venue}-{kind}-{n}");
        r
    };
    for (venue, kind) in [("ig", "limit"), ("ig", "stop")] {
        for tier in [Some(Paper), Some(Demo), Some(Live), None] {
            let mut c = core_of(engine_told(venue, tier), Vec::new(), CoreConfig::default());
            for n in 0..2 {
                c.apply_intent(OrderIntent::Submit(Box::new(reduce(venue, kind, n))), 0);
            }
            let case = format!("{venue}/{kind} on {tier:?}");
            if tier == Some(Paper) {
                assert_eq!(c.engine.client.submissions.len(), 2, "{case}: the simulator gets both");
                let notes = paper_notes(&c);
                assert_eq!(notes.len(), 1, "{case}: said once, not per order: {notes:?}");
                assert!(
                    notes[0]
                        .contains(&format!("the live {venue} adapter would refuse this ({kind})")),
                    "{case}: {notes:?}"
                );
            } else {
                assert!(c.engine.client.submissions.is_empty(), "{case}: refused, as before");
                let want = format!("REDUCE_ONLY_UNSUPPORTED: kind={kind} venue={venue}");
                assert!(c.recent.iter().any(|n| n.contains(&want)), "{case}: {:?}", c.recent);
                assert!(paper_notes(&c).is_empty(), "{case}: nothing was let through");
            }
        }
        // …and the close every row keeps is no confession on any tier.
        for tier in [Some(Paper), Some(Live), None] {
            let mut c = core_of(engine_told(venue, tier), Vec::new(), CoreConfig::default());
            c.apply_intent(OrderIntent::Submit(Box::new(reduce(venue, "market", 0))), 0);
            assert_eq!(c.engine.client.submissions.len(), 1, "{venue}/market on {tier:?}");
            assert!(
                paper_notes(&c).is_empty(),
                "{venue}/market on {tier:?}: a flatten is in the row"
            );
        }
    }
}

/// **THE MATRIX, through the core: every roster venue x every order kind x every tier**, a reduce-only
/// order each. An order reaches the client exactly when the venue's row lets it (the whole row for a
/// demo, a live and an untold engine), or, for an engine told `paper`, when it would pass WITHOUT the
/// flag (every other axis still bites), and the ring confesses exactly the orders a paper book was let
/// through where the row would have refused. The oracle is `vike-venues`' own function called with no
/// tier, which `vike-venues`' matrix test pins to the three measured cases.
#[test]
fn every_venue_row_kind_and_tier_is_judged_through_the_core_as_the_row_says() {
    use vike_model::AccountTier::{Demo, Live, Paper};
    let mut confessed = Vec::new();
    for &venue in vike_model::VENUES {
        for kind in vike_model::OrderType::KNOWN.iter().map(vike_model::OrderType::as_str) {
            let mut req = *venue_request(venue, kind, vike_model::TimeInForce::Gtc);
            req.reduce_only = true;
            req.side = -1;
            let flagless = OrderRequest { reduce_only: false, ..req.clone() };
            let live_row = vike_venues::preflight_order_at(&req, venue, None);
            let no_flag = vike_venues::preflight_order_at(&flagless, venue, None);
            for tier in [Some(Paper), Some(Demo), Some(Live), None] {
                let mut c = core_of(engine_told(venue, tier), Vec::new(), CoreConfig::default());
                c.apply_intent(OrderIntent::Submit(Box::new(req.clone())), 0);
                let sent = c.engine.client.submissions.len();
                let case = format!("{venue}/{kind} reduce-only on {tier:?}");
                if tier == Some(Paper) {
                    assert_eq!(
                        sent,
                        usize::from(no_flag.is_ok()),
                        "{case}: judged without the flag"
                    );
                    let admitted_past_the_row = sent == 1 && live_row.is_err();
                    assert_eq!(paper_notes(&c).len(), usize::from(admitted_past_the_row), "{case}");
                    if admitted_past_the_row {
                        confessed.push((venue, kind));
                    }
                } else {
                    assert_eq!(sent, usize::from(live_row.is_ok()), "{case}: the whole row");
                    assert!(paper_notes(&c).is_empty(), "{case}: only a paper book confesses");
                }
            }
        }
    }
    confessed.sort();
    // bybit's stop-limit and take-profit: placed and amended on the demo host, never placed reduce-only, so its
    // row leaves them out of the reduce-only kinds (the `BYBIT` row's doc); dukascopy's stop-limit and if-touched
    // ENTRY orders, which the venue never holds reduce-only (`vike-venues`' twin pin counts them too).
    assert_eq!(
        confessed,
        [
            ("bybit", "stop_limit"),
            ("bybit", "take_profit"),
            ("dukascopy", "stop_limit"),
            ("dukascopy", "take_profit"),
            ("ig", "limit"),
            ("ig", "stop")
        ],
        "the (venue, kind) pairs a paper book is let through where the row refuses"
    );
}

/// The batch arm's own preflight (`lower_submit_batch`, when every leg resolves engine 0) carries the tier
/// too: on a paper book of IG a reduce-only limit leg rides along with the market leg and is noted; on a
/// demo book the same leg is refused and only the market leg is sent.
#[test]
fn an_all_primary_batch_on_a_paper_book_sends_the_reduce_only_leg_and_notes_it() {
    use vike_model::AccountTier::{Demo, Paper};
    use vike_model::TimeInForce::Gtc;
    for (tier, sent) in [(Paper, 2usize), (Demo, 1)] {
        let mut c = core_of(engine_told("ig", Some(tier)), Vec::new(), CoreConfig::default());
        assert_eq!(c.route_of(EngineRoute::Payload, "ig"), Some(0), "precondition: engine 0");
        let market = *venue_request("ig", "market", Gtc);
        let mut leg = *venue_request("ig", "limit", Gtc);
        leg.reduce_only = true;
        c.apply_intent(OrderIntent::SubmitBatch(vec![market, leg]), 0);
        assert_eq!(c.engine.client.submissions.len(), sent, "{tier:?}");
        assert_eq!(paper_notes(&c).len(), usize::from(tier == Paper), "{tier:?}: {:?}", c.recent);
    }
}

/// SubmitBatch on a SINGLE-ENGINE core: a refused leg terminalizes locally while the healthy
/// legs proceed.
///
/// ⚠ **This one is also a byte-identity witness for the `all_primary` narrowing**, and it is
/// worth saying because the doc line used to read "single-engine PATH" and that is no longer
/// which path it takes. The `polymarket` leg names a venue this core runs no engine for, so
/// `route_of` answers `None`: under the old `unwrap_or(0) == 0` the batch was `all_primary` and
/// went to engine 0's `submit_order_batch`; under `== Some(0)` it is not, and both legs
/// re-enter the single-submit arm. Every assertion below is unchanged, because the arm they
/// land in resolves the same engine (`unwrap_or(0)`, §4.2's decided `N = 0` cell) and runs the
/// same `capabilities_venue(routed, …)` preflight that refused the leg here.
#[test]
fn submit_batch_refuses_only_the_unsupported_leg() {
    let mut c = test_core();
    let good = *market_request("b-good");
    let bad = *venue_request("polymarket", "stop", vike_model::TimeInForce::Gtc);
    c.apply_intent(OrderIntent::SubmitBatch(vec![good, bad]), 0);
    assert_eq!(c.engine.client.submissions.len(), 1, "only the healthy leg reaches the client");
    assert_eq!(c.engine.client.submissions[0].order_client_id, "b-good");
    let mo = c.engine.registry.get("pf-polymarket-stop").expect("refused leg registered");
    assert_eq!(mo.status, vike_model::OrderStatus::Rejected);
}

/// SubmitBatch's OWN capability preflight — the per-leg filter inside the batch arm of
/// `crates/vike-core/src/runtime/apply/submit.rs`'s `lower_submit_batch`, which runs only when
/// every leg resolves engine 0. The test above no longer reaches it (its refused leg names a venue
/// this core runs no engine for, so every leg re-enters the single-submit arm and is preflighted
/// THERE), and with `test_core`'s primary on the non-roster `sim` id no batch could. So the primary
/// engine here IS a roster venue: both legs name it, both resolve engine 0 unambiguously, and the
/// refusal has to come from the batch arm's own filter.
#[test]
fn an_all_primary_batch_refuses_its_unsupported_leg_inside_the_batch_arm() {
    let polymarket = engine_on("polymarket", "X", RecordingClient::default());
    let mut c = core_of(polymarket, Vec::new(), CoreConfig::default());
    assert_eq!(
        c.route_of(EngineRoute::Payload, "polymarket"),
        Some(0),
        "precondition: every leg resolves engine 0"
    );
    assert!(
        c.ambiguous_accounts(EngineRoute::Payload, "polymarket").is_none(),
        "precondition: …unambiguously, so the batch arm takes the whole batch"
    );

    let good = *venue_request("polymarket", "limit", vike_model::TimeInForce::Gtc);
    let bad = *venue_request("polymarket", "stop", vike_model::TimeInForce::Gtc);
    let order_client_ids = c.apply_intent(OrderIntent::SubmitBatch(vec![good, bad]), 0);

    assert_eq!(
        order_client_ids,
        vec!["pf-polymarket-limit".to_string(), "pf-polymarket-stop".to_string()],
        "both legs are named, the refused one included"
    );
    let sent: Vec<&str> =
        c.engine.client.submissions.iter().map(|r| r.order_client_id.as_str()).collect();
    assert_eq!(sent, vec!["pf-polymarket-limit"], "only the supported leg reaches the client");
    let mo = c.engine.registry.get("pf-polymarket-stop").expect("the refused leg is registered");
    assert_eq!(mo.status, vike_model::OrderStatus::Rejected, "and terminal");
    assert!(
        c.recent.iter().any(|n| n.contains("TRIGGER_UNSUPPORTED: kind=stop venue=polymarket")),
        "machine-readable reason surfaced: {:?}",
        c.recent
    );
}

/// One leg on this core's own venue, carrying an OTO parent that has not filled, so the
/// single-submit arm must HOLD it rather than send it.
fn held_child_request(order_client_id: &str) -> OrderRequest {
    let mut r = *market_request(order_client_id);
    r.parent_order_id = Some("p-unfilled".into());
    r
}

/// An UNROUTABLE leg — a venue no engine of this core claims, so `route_of` answers `None`. A
/// non-roster id, so `capabilities_for` resolves the permissive default and the capability preflight
/// has nothing to say about it; the only thing under test here is the ROUTING.
fn unroutable_request(order_client_id: &str) -> OrderRequest {
    let mut r = *market_request(order_client_id);
    r.venue = "nowhere".into();
    r
}

/// ⚠ **A BATCH CARRYING ONE UNROUTABLE LEG ROUTES EVERY LEG BACK THROUGH THE PER-LEG ARM**,
/// instead of taking the primary engine's single batch submit.
///
/// The twin, one row down §4.2's table, of `mount_account_tests`'
/// `an_ambiguous_batch_leg_is_refused_while_its_unambiguous_sibling_is_submitted`.
/// `all_primary` asks whether every leg resolves engine 0, and `unwrap_or(0) == 0` answered
/// `true` for a leg that resolved
/// NOTHING — so a batch containing one could skip the single-submit arm entirely, which is
/// where the contingency book, the refusals and the per-leg lowering live. `== Some(0)` is the
/// fix, and this is what pins it.
///
/// **The probe is the CONTINGENCY HOLD, deliberately, because the destination is not a
/// discriminator.** §4.2's `N = 0` cell is a decision rather than an oversight
/// (`CoreThread::ambiguous_accounts`' own doc), so the per-leg arm still lands an unroutable leg
/// on engine 0 — exactly where the batch arm would have put it. What the batch arm does NOT do
/// is enter `has_contingency_links`: it would have sent a HELD OTO child straight to the venue.
/// So a linked sibling riding along is the one leg whose treatment says which arm ran, and its
/// hold is the evidence that every leg took the per-leg path.
#[test]
fn a_batch_with_an_unroutable_leg_routes_every_leg_through_the_per_leg_arm() {
    let mut c = test_core();
    let order_client_ids = c.apply_intent(
        OrderIntent::SubmitBatch(vec![
            held_child_request("b-child"),
            unroutable_request("b-nowhere"),
        ]),
        0,
    );

    assert_eq!(
        order_client_ids,
        vec!["b-child".to_string(), "b-nowhere".to_string()],
        "both legs named"
    );
    assert!(
        c.held_orders.contains_key("b-child"),
        "the OTO child is HELD — only the per-leg arm holds one, so the batch arm was skipped"
    );
    assert!(c.contingency.is_held("b-child"), "…and the book agrees it is not armed");
    assert_eq!(
        c.engine.client.submissions.len(),
        1,
        "so exactly ONE leg reached the client: {:?}",
        c.engine.client.submissions
    );
    assert_eq!(
        c.engine.client.submissions[0].order_client_id, "b-nowhere",
        "and it is the unroutable one, NOT the held child"
    );
}

/// ⚠ **…AND THE UNROUTABLE LEG STILL LANDS ON ENGINE 0** — the path moved, the destination did
/// not.
///
/// §4.2's `N = 0` cell keeps `unwrap_or(0)` and does NOT refuse; that is written down on
/// `CoreThread::ambiguous_accounts` and this narrowing may not change it. The core here is a
/// CROSS-VENUE single-account-per-venue one (`multi_account` is `false`), which is the shape
/// the `all_primary` predicate exists for in the first place: every leg still lands on the
/// primary book, in order, and the second venue's engine is never touched.
///
/// ⚠ What DID move is the CALL SHAPE, and it is the accepted cost rather than a claim of
/// byte-identity: the two legs now reach that book through two `ExecutionClient::submit` calls
/// instead of one `submit_batch`. That is exactly what the mixed-venue path has always cost,
/// and this double records neither shape (its `submit_batch` is the trait default, which fans
/// out to `submit`), so this test pins the DESTINATION — it passes against the old predicate
/// too, deliberately. Its sibling above,
/// `a_batch_with_an_unroutable_leg_routes_every_leg_through_the_per_leg_arm`, is the one that
/// does not.
#[test]
fn an_unroutable_batch_leg_still_lands_on_engine_zero() {
    let bybit = engine_on("bybit", "BTCUSDT", RecordingClient::default());
    let mut c = test_core_with(RecordingClient::default(), vec![(1.0, bybit)]);
    assert!(!c.multi_account, "one account per venue — the single-account shape");

    c.apply_intent(
        OrderIntent::SubmitBatch(vec![*market_request("b-own"), unroutable_request("b-nowhere")]),
        0,
    );

    let sent: Vec<&str> =
        c.engine.client.submissions.iter().map(|r| r.order_client_id.as_str()).collect();
    assert_eq!(sent, vec!["b-own", "b-nowhere"], "both legs on the primary book, in order");
    assert!(
        c.extra_engines[0].1.client.submissions.is_empty(),
        "and the other venue's engine was never touched"
    );
}

// ── the operator's CORE-HELD reduce-only STOP, on a venue with no native stop ───────────────

/// A core whose primary engine is `venue` (a ROSTER venue, so its capability row is enforced) on the
/// symbol `X`, holding `position` of it. `ticks` is the run profile's `guards.conditionals_on_ticks`.
fn held_core(venue: &str, position: f64, ticks: bool) -> CoreThread<RecordingClient> {
    let mut c = core_of(
        engine_on(venue, "X", RecordingClient::default()),
        Vec::new(),
        CoreConfig { conditionals_on_ticks: ticks, ..CoreConfig::default() },
    );
    if position != 0.0 {
        c.engine.account.positions.insert(
            (venue.into(), "X".into(), vike_model::events::PositionSide::Both),
            vike_execution::PositionEntry {
                size: position,
                average_price: 100.0,
                ..Default::default()
            },
        );
    }
    c
}

/// The operator's `submit --type stop --trigger <px> [--reduce-only]`, as the core receives it.
fn stop_submit(venue: &str, side: i32, qty: f64, trigger: f64, reduce_only: bool) -> OrderIntent {
    OrderIntent::Submit(Box::new(stop_request(venue, side, qty, trigger, reduce_only)))
}

fn stop_request(venue: &str, side: i32, qty: f64, trigger: f64, reduce_only: bool) -> OrderRequest {
    OrderRequest {
        venue: venue.into(),
        symbol: "X".into(),
        side,
        qty,
        order_type: vike_model::OrderType::Stop,
        trigger_price: Some(trigger),
        reduce_only,
        ..Default::default()
    }
}

/// **THE ARM.** A `--type stop --reduce-only` submit on polymarket (whose row declares no stop, and never
/// will: a CLOB of limit orders; deribit's gained one with its native stops) used to
/// terminate as `TRIGGER_UNSUPPORTED`; it is now ARMED in the core's book, with nothing minted,
/// registered or sent, the flag and the engine on the arm's side record, and the tick lane on for it
/// although the profile's `guards.conditionals_on_ticks` is OFF. The note says, in the operator's
/// words, that the node holds it and that an outage is no protection.
#[test]
fn a_reduce_only_stop_on_a_venue_with_no_native_stop_is_held_by_the_core_not_refused() {
    let mut c = held_core("polymarket", 2.0, false);
    assert!(!c.tick_conditionals, "precondition: the knob is off and nothing rests");
    let sequence_before = c.order_client_id_gen.state().1;

    let order_client_ids = c.apply_intent(stop_submit("polymarket", -1, 2.0, 95.0, true), 7);

    assert!(
        order_client_ids.is_empty(),
        "nothing was submitted: the arm id is in the note, not an order client id"
    );
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    assert!(c.engine.registry.is_empty(), "no order was registered, so none was rejected either");
    assert_eq!(
        c.order_client_id_gen.state().1,
        sequence_before,
        "validate-then-mint: no order client id was burned"
    );
    assert_eq!(armed_count(&c), 1);
    let armed = c.emulated.cond_engine.values().next().expect("the arm's side record");
    assert!(armed.reduce_only && armed.engine == 0 && armed.mount.is_none(), "{armed:?}");
    assert!(
        c.tick_conditionals,
        "a stop checked per closed bar is no protection: the tick lane is on"
    );
    let note = last_note(&c).expect("the operator is told");
    assert!(note.contains("CORE-HELD STOP") && note.contains("NOT through an outage"), "{note}");
    assert!(
        note.contains(&format!("{}a0", c.order_client_id_gen.state().0)),
        "it names its id: {note}"
    );
}

/// **A stop that is not THIS verb keeps the refusal it always had**: not reduce-only (a breakout
/// ENTRY nobody asked to hold unattended), and reduce-only with no trigger price.
#[test]
fn a_stop_that_is_not_a_reduce_only_stop_with_a_trigger_stays_refused() {
    let mut c = held_core("polymarket", 2.0, false);
    let entry = c.apply_intent(stop_submit("polymarket", -1, 2.0, 95.0, false), 0);
    let no_trigger = OrderRequest {
        order_client_id: "no-trigger".into(),
        venue: "polymarket".into(),
        symbol: "X".into(),
        side: -1,
        qty: 2.0,
        order_type: vike_model::OrderType::Stop,
        reduce_only: true,
        ..Default::default()
    };
    let bare = c.apply_intent(OrderIntent::Submit(Box::new(no_trigger)), 0);

    assert_eq!(entry.len() + bare.len(), 2, "both refusals still NAME their order");
    assert_eq!(armed_count(&c), 0, "neither was armed");
    assert!(c.engine.client.submissions.is_empty());
    for order_client_id in entry.iter().chain(&bare) {
        let mo = c.engine.registry.get(order_client_id).expect("registered for the FSM to advance");
        assert_eq!(mo.status, vike_model::OrderStatus::Rejected, "{order_client_id} terminal");
    }
    assert!(c.recent.iter().any(|n| n.contains("TRIGGER_UNSUPPORTED: kind=stop venue=polymarket")));
}

/// **THE CAP IS THE ARM'S OWN ACCOUNT'S POSITION, never the venue's default account's.** Two accounts
/// of polymarket: the default holds +3, ALT holds +1. A stop naming no account is refused as ambiguous
/// (the rule every risk-increasing verb has), one naming ALT is armed onto ALT's engine, and its fire
/// is a market for ALT's 1 on ALT's client, with nothing sent to the default account.
#[test]
fn a_labelled_accounts_core_held_stop_is_capped_against_that_accounts_position() {
    let mut alt = engine_on("polymarket", "X", RecordingClient::default());
    alt.route_key = "polymarket#ALT".to_string();
    let mut c = core_of(
        engine_on("polymarket", "X", RecordingClient::default()),
        vec![(1_000.0, alt)],
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
    );
    let long =
        |size| vike_execution::PositionEntry { size, average_price: 100.0, ..Default::default() };
    let key = || ("polymarket".into(), "X".into(), vike_model::events::PositionSide::Both);
    c.engine.account.positions.insert(key(), long(3.0));
    c.extra_engines[0].1.account.positions.insert(key(), long(1.0));

    c.apply_intent(stop_submit("polymarket", -1, 5.0, 95.0, true), 0);
    assert_eq!(armed_count(&c), 0, "no account named on a two-account venue: refused as ambiguous");

    let mut req = stop_request("polymarket", -1, 5.0, 95.0, true);
    req.account =
        Some(vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("label"));
    c.apply_intent(OrderIntent::Submit(Box::new(req)), 1);
    assert_eq!(
        c.emulated.cond_engine.values().map(|a| (a.engine, a.reduce_only)).collect::<Vec<_>>(),
        [(1, true)]
    );

    c.fire_conditionals_at_price("polymarket", "X", 94.0, 2);

    let sent = &c.extra_engines[0].1.client.submissions;
    assert_eq!(
        (sent.len(), sent[0].qty, sent[0].reduce_only),
        (1, 1.0, true),
        "ALT's 1, not 3 or 5"
    );
    assert!(c.engine.client.submissions.is_empty(), "nothing reached the default account");
}

/// **A STOPPED ACCOUNT IS NOT ARMED ONTO** (decision 0169): the submit tail refuses everything sent to
/// it as `ACCOUNT_NOT_RUNNING`, and a stop that was held instead would wait there for a fire the tail
/// would then refuse. The stop falls through to that refusal.
#[test]
fn a_stopped_accounts_reduce_only_stop_is_refused_not_armed() {
    let mut c = held_core("polymarket", 2.0, false);
    c.retired_engines.push(0);

    let order_client_ids = c.apply_intent(stop_submit("polymarket", -1, 2.0, 95.0, true), 0);

    assert_eq!(order_client_ids.len(), 1, "the refusal names its order");
    assert_eq!(armed_count(&c), 0, "nothing armed onto a stopped account");
    assert!(c.recent.iter().any(|n| n.contains("ACCOUNT_NOT_RUNNING")), "{:?}", c.recent);
}

/// **THE CAPABILITY MAP ALONE DECIDES, per venue.** Every roster venue takes a reduce-only stop; the
/// ones whose row takes it (the preflight passes it: `stop` is a kind AND a reduce-only kind) hand it
/// to their native path (nothing armed), the ones whose row refuses it (no `stop` kind, or a `stop`
/// that is an entry kind only: IG) hold it in the core (nothing registered). No venue is named here,
/// so a venue that gains a native stop leaves this path with no edit, and one that has none takes it
/// with no edit.
#[test]
fn the_capability_map_alone_decides_which_venues_take_the_core_held_path() {
    let mut held = Vec::new();
    for &venue in vike_model::VENUES {
        let native =
            vike_venues::preflight_order(&stop_request(venue, -1, 2.0, 95.0, true)).is_ok();
        let mut c = held_core(venue, 2.0, false);
        c.apply_intent(stop_submit(venue, -1, 2.0, 95.0, true), 0);
        if native {
            assert_eq!(armed_count(&c), 0, "{venue} has a native stop: it must never be core-held");
            assert!(
                !c.engine.registry.is_empty() || !c.engine.client.submissions.is_empty(),
                "{venue}: the order went down its own path"
            );
        } else {
            assert_eq!(armed_count(&c), 1, "{venue} has no native stop: the core holds it");
            assert!(c.engine.registry.is_empty(), "{venue}: nothing was minted or rejected");
            held.push(venue);
        }
    }
    assert!(!held.is_empty(), "vacuous unless some roster venue declares no stop");
}

/// **EVERY ROSTER VENUE x EVERY ORDER KIND x REDUCE-ONLY OR NOT, against the one decision**: the core
/// holds an order exactly when it is a reduce-only trigger kind (a stop, a stop-limit, a take-profit, a
/// trailing stop, a trailing-limit) its venue's row refuses and whose fire the row takes, and every other
/// order is judged as it always was (refused in the preflight's own words, or sent down the venue's path).
/// The set that is held today is PINNED by name, on purpose: a row change that adds or retires a venue
/// or a kind here must be read, not slip through (the production code names none). `ig` is the row that
/// takes a stop as an ENTRY kind and not as a reduce-only one (`reduce_only_kinds: ["market"]`), so of its
/// held kinds only the two whose fire is a market are held; `polymarket` has no trigger kind at all.
#[test]
fn only_a_reduce_only_stop_the_row_refuses_is_held_and_every_other_order_is_judged_as_before() {
    let mut held = Vec::new();
    let kinds =
        ["market", "limit", "stop", "stop_limit", "take_profit", "trailing", "trailing_limit"];
    for &venue in vike_model::VENUES {
        for kind in kinds {
            for reduce_only in [false, true] {
                let mut req = *venue_request(venue, kind, vike_model::TimeInForce::Gtc);
                req.reduce_only = reduce_only;
                req.trigger_price =
                    matches!(kind, "stop" | "stop_limit" | "take_profit").then_some(95.0);
                if req.order_type.is_trailing() {
                    // a trailing kind states no price; its trail, and a trailing-limit's offset
                    req.price = None;
                    req.trail = Some(5.0);
                    req.limit_offset = (kind == "trailing_limit").then_some(1.0);
                }
                let verdict = vike_venues::preflight_order(&req);
                let order_client_id = req.order_client_id.clone();
                let what = format!("{venue} {kind} reduce_only={reduce_only}");

                // a mark, so a trailing arm can seed its extreme
                let mut c = held_core(venue, 0.0, false);
                c.engine.account.set_mark_from(venue, "X", 100.0, MarkSource::VenueMark, 0);
                c.apply_intent(OrderIntent::Submit(Box::new(req)), 0);

                let was_held = armed_count(&c) == 1;
                assert!(armed_count(&c) <= 1, "{what}");
                assert!(
                    !was_held || (reduce_only && verdict.is_err()),
                    "{what}: refused reduce-only"
                );
                if was_held {
                    assert!(c.engine.registry.is_empty(), "{what}: held, nothing registered");
                    held.push((venue, kind));
                } else if let Err(deny) = verdict {
                    let mo = c
                        .engine
                        .registry
                        .get(&order_client_id)
                        .expect("refused in words, registered");
                    assert_eq!(mo.status, vike_model::OrderStatus::Rejected, "{what}");
                    assert!(
                        c.recent.iter().any(|n| n.contains(&deny.to_string())),
                        "{what}: the preflight's own words: {:?}",
                        c.recent
                    );
                } else {
                    assert!(
                        !c.engine.registry.is_empty() || !c.engine.client.submissions.is_empty(),
                        "{what}: it went down its own path"
                    );
                }
            }
        }
    }
    held.sort_unstable();
    // Every reduce-only trigger kind a row refuses is held, BUT on ig a kind whose fire is a reduce-only
    // LIMIT (a stop-limit, a limit-if-touched take-profit, a trailing-limit): ig's row places no
    // reduce-only limit, so those keep their refusal at accept (decision 0219's shape).
    assert_eq!(
        held,
        [
            ("alpaca", "take_profit"),
            ("alpaca", "trailing"),
            ("alpaca", "trailing_limit"),
            ("aster", "stop_limit"),
            ("aster", "take_profit"),
            ("aster", "trailing"),
            ("aster", "trailing_limit"),
            ("binance", "trailing"),
            ("binance", "trailing_limit"),
            ("bybit", "stop_limit"),
            ("bybit", "take_profit"),
            ("bybit", "trailing"),
            ("bybit", "trailing_limit"),
            ("ctrader", "take_profit"),
            ("ctrader", "trailing"),
            ("ctrader", "trailing_limit"),
            ("deribit", "trailing_limit"),
            ("dukascopy", "stop_limit"),
            ("dukascopy", "take_profit"),
            ("dukascopy", "trailing"),
            ("dukascopy", "trailing_limit"),
            ("fxcm", "stop_limit"),
            ("fxcm", "take_profit"),
            ("fxcm", "trailing"),
            ("fxcm", "trailing_limit"),
            ("hyperliquid", "trailing"),
            ("hyperliquid", "trailing_limit"),
            ("ibkr", "take_profit"),
            ("ibkr", "trailing"),
            ("ibkr", "trailing_limit"),
            ("ig", "stop"),
            ("ig", "trailing"),
            ("oanda", "stop_limit"),
            ("oanda", "trailing"),
            ("oanda", "trailing_limit"),
            ("okx", "trailing_limit"),
            ("polymarket", "stop"),
            ("polymarket", "stop_limit"),
            ("polymarket", "take_profit"),
            ("polymarket", "trailing"),
            ("polymarket", "trailing_limit"),
        ]
    );
}

/// **THE ATTRIBUTES WAVE'S FORMS x EVERY ROSTER VENUE x REDUCE-ONLY OR NOT** (decision 0267, question 3): a
/// trailing take-profit (with and without its offset), a percent trailing stop and trailing-limit, and an
/// adjustable stop and stop-limit. The core holds one exactly when it is reduce-only, its row refuses its
/// kind, its percent (`percent_trail_kinds`) or `adjust`, and the row takes its fire; every other one is
/// refused in the preflight's own words or sent down the venue's path. Pinned by name: the forms a row
/// takes (`opened`, A6's flips behind their smokes) go down the venue's path, reduce-only or not; every
/// other reduce-only form is held, BUT on ig the three whose fire is a reduce-only LIMIT (an offset
/// take-profit, a trailing-limit, an adjustment of a stop-limit). A row that opens one of these moves it
/// from the held set to `opened` and must be read here.
#[test]
fn the_attribute_waves_forms_are_held_exactly_where_the_row_refuses_them() {
    use vike_model::{OrderStopAdjust, OrderStopAdjustTo, OrderType};
    let forms: [(&str, OrderType); 6] = [
        ("ttp", OrderType::TrailingTakeProfit),
        ("ttp_offset", OrderType::TrailingTakeProfit),
        ("trailing_pct", OrderType::Trailing),
        ("trailing_limit_pct", OrderType::TrailingLimit),
        ("stop_adjust", OrderType::Stop),
        ("stop_limit_adjust", OrderType::StopLimit),
    ];
    // the forms a row takes natively, each behind its own smoke, sorted
    let opened: &[(&str, &str)] = &[
        ("binance", "trailing_pct"),
        ("ibkr", "stop_adjust"),
        ("ibkr", "stop_limit_adjust"),
        ("ibkr", "trailing_pct"),
        ("ibkr", "ttp"),
        // the row cannot tell `TRAIL LIT` (an offset) from `TRAIL MIT`: the bridge's to refuse
        ("ibkr", "ttp_offset"),
        ("okx", "trailing_pct"),
    ];
    let mut held = Vec::new();
    let mut sent = Vec::new();
    for &venue in vike_model::VENUES {
        for (form, kind) in &forms {
            for reduce_only in [false, true] {
                let mut req = *venue_request(venue, kind.as_str(), vike_model::TimeInForce::Gtc);
                req.reduce_only = reduce_only;
                (req.price, req.trigger_price) = (None, None);
                match *form {
                    "ttp" => req.trail = Some(5.0),
                    "ttp_offset" => (req.trail, req.limit_offset) = (Some(5.0), Some(1.0)),
                    "trailing_pct" => req.trail_percent = Some(5.0),
                    "trailing_limit_pct" => {
                        (req.trail_percent, req.limit_offset) = (Some(5.0), Some(1.0));
                    }
                    "stop_adjust" => {
                        req.trigger_price = Some(95.0);
                        req.adjust = Some(OrderStopAdjust {
                            at: 90.0,
                            to: OrderStopAdjustTo::Stop { stop: 93.0 },
                        });
                    }
                    _ => {
                        (req.trigger_price, req.price) = (Some(95.0), Some(96.0));
                        let to = OrderStopAdjustTo::StopLimit { stop: 93.0, limit: 94.0 };
                        req.adjust = Some(OrderStopAdjust { at: 90.0, to });
                    }
                }
                let verdict = vike_venues::preflight_order(&req);
                let order_client_id = req.order_client_id.clone();
                let what = format!("{venue} {form} reduce_only={reduce_only}");
                let mut c = held_core(venue, 0.0, false);
                c.engine.account.set_mark_from(venue, "X", 100.0, MarkSource::VenueMark, 0);
                c.apply_intent(OrderIntent::Submit(Box::new(req)), 0);

                let was_held = armed_count(&c) == 1;
                assert!(
                    !was_held || (reduce_only && verdict.is_err()),
                    "{what}: refused reduce-only"
                );
                if was_held {
                    assert!(c.engine.registry.is_empty(), "{what}: held, nothing registered");
                    held.push((venue, *form));
                } else if let Err(deny) = verdict {
                    let mo = c
                        .engine
                        .registry
                        .get(&order_client_id)
                        .expect("refused in words, registered");
                    assert_eq!(mo.status, vike_model::OrderStatus::Rejected, "{what}");
                    assert!(
                        c.recent.iter().any(|n| n.contains(&deny.to_string())),
                        "{what}: the preflight's own words: {:?}",
                        c.recent
                    );
                } else {
                    assert!(
                        !c.engine.registry.is_empty() || !c.engine.client.submissions.is_empty(),
                        "{what}: it went down its own path"
                    );
                    sent.push((venue, *form));
                }
            }
        }
    }
    sent.sort_unstable();
    sent.dedup();
    assert_eq!(sent, opened, "the forms a row takes, both reduce-only and not");
    let ig_limit_fires =
        [("ig", "stop_limit_adjust"), ("ig", "trailing_limit_pct"), ("ig", "ttp_offset")];
    let mut want: Vec<(&str, &str)> = vike_model::VENUES
        .iter()
        .flat_map(|&v| forms.iter().map(move |(f, _)| (v, *f)))
        .filter(|p| !ig_limit_fires.contains(p) && !opened.contains(p))
        .collect();
    want.sort_unstable();
    held.sort_unstable();
    assert_eq!(held, want);
}

/// **IG: AN OPERATOR'S REDUCE-ONLY STOP IS HELD BY THE CORE AND FIRES AS A CAPPED REDUCE-ONLY MARKET.**
/// IG's row takes `stop` as an ENTRY kind but lists `reduce_only_kinds: ["market"]` (its close endpoint
/// rests nothing), so the stop used to end `REDUCE_ONLY_UNSUPPORTED: kind=stop venue=ig` with no stop-loss
/// at all. Now it is armed (nothing minted, registered or sent, the tick lane on although the profile's
/// knob is off) with a note that says the node holds it and an outage is no protection; a price on the
/// safe side fires nothing; the crossing one sends the position (2), not the armed size (5), as a
/// reduce-only MARKET: the one reduce-only kind IG's close route places.
#[test]
fn an_igs_reduce_only_stop_is_held_by_the_core_and_fires_as_a_capped_reduce_only_market() {
    let mut c = held_core("ig", 2.0, false);
    assert!(!c.tick_conditionals, "precondition: the knob is off and nothing rests");
    let sequence_before = c.order_client_id_gen.state().1;

    let order_client_ids = c.apply_intent(stop_submit("ig", -1, 5.0, 95.0, true), 7);

    assert!(
        order_client_ids.is_empty(),
        "nothing was submitted: the arm id is in the note, not an order client id"
    );
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    assert!(c.engine.registry.is_empty(), "no order was registered, so none was rejected either");
    assert_eq!(
        c.order_client_id_gen.state().1,
        sequence_before,
        "validate-then-mint: no order client id was burned"
    );
    assert_eq!(armed_count(&c), 1);
    assert!(c.tick_conditionals, "a stop checked per closed bar is no protection: the lane is on");
    let note = last_note(&c).expect("the operator is told");
    assert!(note.contains("CORE-HELD STOP") && note.contains("NOT through an outage"), "{note}");
    assert!(note.contains("ig has NO native reduce-only stop"), "{note}");
    assert!(!c.recent.iter().any(|n| n.contains("REDUCE_ONLY_UNSUPPORTED")), "{:?}", c.recent);

    c.fire_conditionals_at_price("ig", "X", 96.0, 8);
    assert!(c.engine.client.submissions.is_empty(), "the safe side of the stop fires nothing");

    c.fire_conditionals_at_price("ig", "X", 94.0, 9);

    let sent = &c.engine.client.submissions;
    assert_eq!(sent.len(), 1, "one close: {sent:?}");
    assert_eq!(
        (sent[0].side, sent[0].qty, sent[0].reduce_only, &sent[0].order_type),
        (-1, 2.0, true, &vike_model::OrderType::Market),
        "the position, not the armed 5; a reduce-only MARKET, the one kind IG's close route takes"
    );
    assert!(
        c.emulated.cond_engine.is_empty() && !c.tick_conditionals,
        "spent, and the lane is off again"
    );
}

/// **ON IG ONLY THE REDUCE-ONLY STOP WITH A TRIGGER IS HELD**: an ENTRY stop (not reduce-only) is a working
/// order and goes down the venue's path as it always did; a reduce-only LIMIT, and a reduce-only stop with
/// no trigger price, keep the preflight's refusal in words. Nothing is armed for any of the three.
#[test]
fn on_ig_an_entry_stop_a_reduce_only_limit_and_a_triggerless_stop_keep_their_way() {
    let mut c = held_core("ig", 2.0, false);

    c.apply_intent(stop_submit("ig", 1, 2.0, 105.0, false), 0);
    assert_eq!(c.engine.client.submissions.len(), 1, "the entry stop is a working order, sent");
    assert_eq!(c.engine.client.submissions[0].order_type, vike_model::OrderType::Stop);
    assert!(!c.engine.client.submissions[0].reduce_only);

    let limit = OrderRequest {
        order_client_id: "ig-ro-limit".into(),
        venue: "ig".into(),
        symbol: "X".into(),
        side: -1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(99.0),
        reduce_only: true,
        ..Default::default()
    };
    let triggerless = OrderRequest {
        order_client_id: "ig-ro-stop".into(),
        order_type: vike_model::OrderType::Stop,
        price: None,
        ..limit.clone()
    };
    for (req, words) in [
        (limit, "REDUCE_ONLY_UNSUPPORTED: kind=limit venue=ig"),
        (triggerless, "REDUCE_ONLY_UNSUPPORTED: kind=stop venue=ig"),
    ] {
        let order_client_id = req.order_client_id.clone();
        c.apply_intent(OrderIntent::Submit(Box::new(req)), 1);
        let mo = c.engine.registry.get(&order_client_id).expect("refused in words, registered");
        assert_eq!(mo.status, vike_model::OrderStatus::Rejected, "{order_client_id}");
        assert!(c.recent.iter().any(|n| n.contains(words)), "{order_client_id}: {:?}", c.recent);
    }
    assert_eq!(armed_count(&c), 0, "none of the three was armed");
    assert_eq!(c.engine.client.submissions.len(), 1, "only the entry stop reached the venue");
}

/// **A REDUCE-ONLY STOP-LIMIT, LIMIT-IF-TOUCHED OR TRAILING-LIMIT THE ROW REFUSES IS HELD AND FIRES A
/// LIMIT**, capped to the position and `reduce_only`, never a market (wave 3, decision 0199's shape). On
/// polymarket, whose row has none of the three and takes a reduce-only limit: the arm is held (nothing
/// minted or sent, the note names the limit it sends), a price on the safe side fires nothing, and the
/// crossing one sends the position (2), not the armed 5, as a reduce-only LIMIT at the arm's limit.
#[test]
fn a_held_stop_limit_take_profit_or_trailing_limit_fires_a_capped_reduce_only_limit() {
    use vike_model::OrderType;
    // (kind, price, trigger, trail, offset, the safe tick, the crossing tick, the limit sent)
    let cases = [
        (OrderType::StopLimit, Some(94.0), Some(95.0), None, None, 96.0, 94.5, 94.0),
        (OrderType::TakeProfit, Some(104.5), Some(105.0), None, None, 104.0, 105.5, 104.5),
        (OrderType::TrailingLimit, None, None, Some(5.0), Some(1.0), 99.0, 94.0, 94.0),
    ];
    for (kind, price, trigger, trail, offset, safe, cross, limit) in cases {
        let mut c = held_core("polymarket", 2.0, false);
        c.engine.account.set_mark_from("polymarket", "X", 100.0, MarkSource::VenueMark, 0);
        let mut req = stop_request("polymarket", -1, 5.0, 0.0, true);
        (req.order_type, req.price, req.trigger_price) = (kind.clone(), price, trigger);
        (req.trail, req.limit_offset) = (trail, offset);
        let sequence_before = c.order_client_id_gen.state().1;

        assert!(c.apply_intent(OrderIntent::Submit(Box::new(req)), 7).is_empty(), "{kind}");
        assert_eq!(armed_count(&c), 1, "{kind}: held");
        assert!(c.engine.registry.is_empty() && c.engine.client.submissions.is_empty(), "{kind}");
        assert_eq!(
            c.order_client_id_gen.state().1,
            sequence_before,
            "{kind}: no order client id was burned"
        );
        let note = last_note(&c).expect("the operator is told");
        assert!(
            note.contains("polymarket has NO native reduce-only") && note.contains("a limit"),
            "{note}"
        );

        c.fire_conditionals_at_price("polymarket", "X", safe, 8);
        assert!(c.engine.client.submissions.is_empty(), "{kind}: the safe side fires nothing");
        c.fire_conditionals_at_price("polymarket", "X", cross, 9);

        let sent = &c.engine.client.submissions;
        assert_eq!(sent.len(), 1, "{kind}: one close: {sent:?}");
        assert_eq!(
            (sent[0].side, sent[0].qty, sent[0].reduce_only, &sent[0].order_type, sent[0].price),
            (-1, 2.0, true, &OrderType::Limit, Some(limit)),
            "{kind}: the position, as a reduce-only LIMIT"
        );
        assert!(c.emulated.cond_engine.is_empty() && !c.tick_conditionals, "{kind}: spent");
    }
}

/// **ON IG A REDUCE-ONLY KIND WHOSE FIRE IS A LIMIT KEEPS ITS REFUSAL** (ig's row places no reduce-only
/// limit, so the core could not send what the arm would fire): refused at accept in the row's words,
/// nothing armed. A reduce-only TRAILING stop, whose fire is a market, is held.
#[test]
fn on_ig_a_reduce_only_kind_that_fires_a_limit_is_refused_and_a_trailing_stop_is_held() {
    use vike_model::OrderType;
    let mut c = held_core("ig", 2.0, false);
    c.engine.account.set_mark_from("ig", "X", 100.0, MarkSource::VenueMark, 0);
    let mut stop_limit = stop_request("ig", -1, 1.0, 95.0, true);
    (stop_limit.order_client_id, stop_limit.order_type, stop_limit.price) =
        ("ig-ro-sl".into(), OrderType::StopLimit, Some(94.0));
    c.apply_intent(OrderIntent::Submit(Box::new(stop_limit)), 1);
    let mo = c.engine.registry.get("ig-ro-sl").expect("refused in words, registered");
    assert_eq!(mo.status, vike_model::OrderStatus::Rejected);
    assert!(c.recent.iter().any(|n| n.contains("TRIGGER_UNSUPPORTED: kind=stop_limit venue=ig")));
    assert_eq!(armed_count(&c), 0);

    let mut tr = stop_request("ig", -1, 1.0, 0.0, true);
    (tr.order_type, tr.trigger_price, tr.trail) = (OrderType::Trailing, None, Some(5.0));
    c.apply_intent(OrderIntent::Submit(Box::new(tr)), 2);
    assert_eq!(armed_count(&c), 1, "the trailing stop is held: {:?}", c.recent);
    c.fire_conditionals_at_price("ig", "X", 94.0, 3);
    let sent = &c.engine.client.submissions;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(
        (sent[0].qty, sent[0].reduce_only, &sent[0].order_type),
        (1.0, true, &OrderType::Market)
    );
}

/// **THE FIRE IS CAPPED TO THE LIVE POSITION and goes out `reduce_only`**: a stop armed for more than
/// the book holds sends the book, a stop armed for less sends what it was armed for, and a BUY stop
/// protects a short the same way. The tick lane the stop kept on goes back off once it has fired.
#[test]
fn the_fire_is_capped_to_the_live_position_and_goes_out_reduce_only() {
    for (position, side, armed_quantity, want) in
        [(2.0, -1, 5.0, 2.0), (5.0, -1, 2.0, 2.0), (-3.0, 1, 10.0, 3.0)]
    {
        let mut c = held_core("polymarket", position, false);
        let (trigger, px) = if side < 0 { (95.0, 94.0) } else { (105.0, 106.0) };
        c.apply_intent(stop_submit("polymarket", side, armed_quantity, trigger, true), 0);

        c.fire_conditionals_at_price("polymarket", "X", px, 1);

        assert_eq!(c.engine.client.submissions.len(), 1, "position {position}: one market");
        let o = &c.engine.client.submissions[0];
        assert_eq!(
            (o.side, o.qty, o.reduce_only, &o.order_type),
            (side, want, true, &vike_model::OrderType::Market),
            "position {position}, armed {armed_quantity}"
        );
        assert!(
            c.emulated.cond_engine.is_empty() && !c.tick_conditionals,
            "spent, and the lane is off again"
        );
    }
}

/// **THE FIRE IS DROPPED WHEN THERE IS NOTHING TO REDUCE**: a flat book, or a position on the side the
/// stop would ADD to. Nothing is sent or registered, the arm is gone, and the ring says so by name.
#[test]
fn the_fire_is_dropped_when_the_position_is_gone_or_on_the_wrong_side() {
    for position in [0.0, -2.0] {
        let mut c = held_core("polymarket", position, false);
        c.apply_intent(stop_submit("polymarket", -1, 2.0, 95.0, true), 0);

        c.fire_conditionals_at_price("polymarket", "X", 94.0, 1);

        assert!(c.engine.client.submissions.is_empty(), "position {position}: nothing sent");
        assert!(c.engine.registry.is_empty(), "…nor registered");
        assert_eq!(armed_count(&c), 0, "…and the arm is gone: it does not wait to fire again");
        assert!(c.emulated.cond_engine.is_empty());
        let note = last_note(&c).expect("said");
        assert!(
            note.contains("REDUCE-ONLY STOP DROPPED") && note.contains("NOTHING was sent"),
            "{note}"
        );
    }
}

/// **UNDER THE DRAWDOWN LATCH THE CAPPED FIRE PASSES ON MERIT.** `RiskGate` trusts a bare
/// `reduce_only` while `Reducing`, and a flag-ignoring venue would then OPEN; with the quantity cut to
/// the book the order is a covered reduce, admitted under `Reducing` AND `Halted` by the same law that
/// admits a flatten.
#[test]
fn under_the_drawdown_latch_the_capped_fire_passes_on_merit() {
    for state in [TradingState::Reducing, TradingState::Halted] {
        let mut c = held_core("polymarket", 2.0, false);
        c.apply_intent(stop_submit("polymarket", -1, 5.0, 95.0, true), 0);
        c.engine.trading_state = state;

        c.fire_conditionals_at_price("polymarket", "X", 94.0, 1);

        assert_eq!(c.engine.client.submissions.len(), 1, "{state:?}: the exit is admitted");
        assert_eq!(c.engine.client.submissions[0].qty, 2.0, "{state:?}: capped to the book");
    }
}

/// **A CANCEL OF THE ARM ID DISARMS IT**, the way a held bracket exit is cancelled: nothing reaches
/// the venue, the arm and its tick lane go, the operator is NOT told the id is "unheld", and the
/// crossing price fires nothing. A batch carrying the id takes it out too.
#[test]
fn a_cancel_of_the_arm_id_disarms_the_core_held_stop() {
    let mut c = held_core("polymarket", 2.0, false);
    c.apply_intent(stop_submit("polymarket", -1, 2.0, 95.0, true), 0);
    let arm_id = c.emulated.cond_engine.keys().next().expect("armed").clone();
    let ring = c.recent.len();
    c.report_unheld_cancel(&OrderIntent::Cancel(arm_id.clone()));
    assert_eq!(c.recent.len(), ring, "an armed stop is not an unheld row: nothing is said");

    c.apply_intent(OrderIntent::Cancel(arm_id.clone()), 1);

    assert!(c.engine.client.cancels.is_empty(), "no cancel went to the venue");
    assert_eq!(armed_count(&c), 0);
    assert!(c.emulated.cond_engine.is_empty() && !c.tick_conditionals);
    assert!(last_note(&c).expect("said").contains("DISARMED"), "{:?}", c.recent);
    c.fire_conditionals_at_price("polymarket", "X", 94.0, 2);
    assert!(c.engine.client.submissions.is_empty(), "a disarmed stop does not fire");

    c.apply_intent(stop_submit("polymarket", -1, 2.0, 95.0, true), 3);
    let second = c.emulated.cond_engine.keys().next().expect("re-armed").clone();
    c.apply_intent(OrderIntent::CancelBatch(vec![second]), 4);
    assert_eq!(armed_count(&c), 0, "the batch took it out as well");
    assert!(c.engine.client.cancels.is_empty());
}

/// **THE TICK LANE FOLLOWS THE STOP, and a pair with no armed book costs and arms nothing.** The
/// lane is the profile's knob OR a core-held stop resting; a mass-cancel takes the arm and so the
/// lane; with the knob ON the lane stays on whatever rests. A tick on a pair nothing is armed on
/// opens no book (the guard now finds the book by comparing names, with no key built per tick).
#[test]
fn the_tick_lane_follows_the_core_held_stop_and_an_idle_pair_arms_nothing() {
    let wipe = || OrderIntent::MassCancel { venue: None, symbol: None, account: None };
    let mut c = held_core("polymarket", 2.0, false);
    c.apply_intent(stop_submit("polymarket", -1, 2.0, 95.0, true), 0);
    assert!(c.tick_conditionals);

    c.fire_conditionals_at_price("polymarket", "OTHER", 1.0, 1);
    assert!(
        !c.emulated
            .order_emulation_arms
            .contains_key(&("polymarket".to_string(), "OTHER".to_string())),
        "a tick on an unarmed pair opens no book"
    );
    assert!(c.engine.client.submissions.is_empty());

    c.apply_intent(wipe(), 2);
    assert!(
        c.emulated.cond_engine.is_empty() && !c.tick_conditionals,
        "the lane went with the arm"
    );

    let mut on = held_core("polymarket", 2.0, true);
    on.apply_intent(stop_submit("polymarket", -1, 2.0, 95.0, true), 0);
    on.apply_intent(wipe(), 1);
    assert!(on.tick_conditionals, "the profile's knob keeps the lane on whatever rests");
}

/// A reduce-only MARKET close of `qty` on `X`, as a flatten, a margin call's liquidation or a first fired
/// stop sends it (an async client has not folded its fill: the book still holds the whole position).
fn reduce_market(venue: &str, side: i32, qty: f64) -> OrderIntent {
    OrderIntent::Submit(Box::new(OrderRequest {
        venue: venue.into(),
        symbol: "X".into(),
        side,
        qty,
        order_type: vike_model::OrderType::Market,
        reduce_only: true,
        ..Default::default()
    }))
}

/// **A FIRE NETS THE CLOSES ALREADY IN FLIGHT** (the margin call and the budget latch's flatten do the
/// same, `closes_in_flight`). Long 2, a reduce-only market SELL 1 already out and unfilled: the stop
/// sends the 1 that is left, not the 2 the folded book still shows. Without the netting the two closes
/// add up to 3 against a position of 2, and on a venue that sends no reduce-only flag (dukascopy, fxcm
/// netting, polymarket) the position flips short.
#[test]
fn a_fire_beside_a_close_already_in_flight_sends_only_what_is_left() {
    let mut c = held_core("polymarket", 2.0, false);
    c.apply_intent(stop_submit("polymarket", -1, 5.0, 95.0, true), 0);
    c.apply_intent(reduce_market("polymarket", -1, 1.0), 1);
    assert_eq!(c.engine.client.submissions.len(), 1, "precondition: the first close is out");

    c.fire_conditionals_at_price("polymarket", "X", 94.0, 2);

    let sent = &c.engine.client.submissions;
    assert_eq!(sent.len(), 2, "the stop still fired");
    assert_eq!((sent[1].qty, sent[1].reduce_only, sent[1].side), (1.0, true, -1), "{sent:?}");
}

/// …and when the closes in flight already cover the whole position the fire sends NOTHING, names why on
/// the ring (the stop is spent, so the note says what to do if that close fails), and two stops crossed
/// by ONE tick close the position once between them, not once each.
#[test]
fn a_fire_covered_by_closes_in_flight_is_dropped_and_two_crossed_stops_close_once() {
    let mut c = held_core("polymarket", 2.0, false);
    c.apply_intent(stop_submit("polymarket", -1, 5.0, 95.0, true), 0);
    c.apply_intent(reduce_market("polymarket", -1, 2.0), 1);

    c.fire_conditionals_at_price("polymarket", "X", 94.0, 2);

    assert_eq!(c.engine.client.submissions.len(), 1, "no second close beside the one in flight");
    assert_eq!(armed_count(&c), 0, "the arm is spent");
    let note = last_note(&c).expect("said");
    assert!(
        note.contains("REDUCE-ONLY STOP DROPPED") && note.contains("already in flight"),
        "{note}"
    );

    let mut two = held_core("polymarket", 2.0, false);
    two.apply_intent(stop_submit("polymarket", -1, 2.0, 96.0, true), 0);
    two.apply_intent(stop_submit("polymarket", -1, 2.0, 95.0, true), 1);
    two.fire_conditionals_at_price("polymarket", "X", 94.0, 2);
    let sent: Vec<f64> = two.engine.client.submissions.iter().map(|o| o.qty).collect();
    assert_eq!(sent, [2.0], "both stops crossed, the position is closed ONCE");
}

/// **A LIMIT A FIRE RELEASED IS A CLOSE IN FLIGHT, crossing or not** (polymarket sends no reduce-only flag, so
/// two closes of a long 2 would flip it). Two core-held stop-limits (1.5 and 2, limits 94) crossed by a gap to
/// 93 in ONE tick: the first releases a reduce-only limit of 1.5 that rests above the market (it does not
/// cross), and the second is CAPPED to the 0.5 left; a third, crossed later, is DROPPED in words because the
/// released limits cover the whole position. The closes never add up to more than the position.
#[test]
fn a_limit_a_fire_released_capabilities_the_next_core_held_fire_on_a_venue_with_no_reduce_only_flag()
 {
    use vike_model::OrderType;
    let mut c = held_core("polymarket", 2.0, false);
    for (qty, at) in [(1.5, 0), (2.0, 1), (2.0, 2)] {
        let mut req = stop_request("polymarket", -1, qty, if at == 2 { 92.0 } else { 95.0 }, true);
        (req.order_type, req.price) = (OrderType::StopLimit, Some(94.0));
        c.apply_intent(OrderIntent::Submit(Box::new(req)), at);
    }
    assert_eq!(armed_count(&c), 3);

    c.fire_conditionals_at_price("polymarket", "X", 93.0, 3);
    let sent: Vec<_> =
        c.engine.client.submissions.iter().map(|o| (o.qty, o.price, o.reduce_only)).collect();
    assert_eq!(sent, [(1.5, Some(94.0), true), (0.5, Some(94.0), true)], "the second is capped");

    c.fire_conditionals_at_price("polymarket", "X", 91.0, 4);
    assert_eq!(c.engine.client.submissions.len(), 2, "no third close beside the two resting");
    assert_eq!(armed_count(&c), 0, "every arm is spent");
    let note = last_note(&c).expect("said");
    assert!(
        note.contains("REDUCE-ONLY STOP DROPPED") && note.contains("the limits a fire released"),
        "{note}"
    );
}

/// `n` PLAIN sell stops at 95 of the given sizes on `venue`'s `X`, over a long of `position`, crossed by ONE tick.
fn plain_stops_crossed(venue: &str, position: f64, sizes: &[f64]) -> CoreThread<RecordingClient> {
    let mut c = held_core(venue, position, false);
    for (at, &qty) in sizes.iter().enumerate() {
        c.apply_intent(
            OrderIntent::ArmConditional(ConditionalIntent {
                venue: venue.into(),
                symbol: "X".into(),
                side: -1,
                qty,
                price: Some(95.0),
                trail: None,
                trigger_by: None,
                kind_terms: None,
                account: None,
            }),
            at as i64,
        );
    }
    c.fire_conditionals_at_price(venue, "X", 94.0, sizes.len() as i64);
    c
}

/// Fill every order `c` sent, at 94, so the book ends where the venue would.
fn fill_all(c: &mut CoreThread<RecordingClient>, venue: &str) {
    let sent: Vec<_> =
        c.engine.client.submissions.iter().map(|r| (r.order_client_id.clone(), r.qty)).collect();
    for (order_client_id, qty) in sent {
        for event in fill_events(&order_client_id, venue, "X", -1, qty, 94.0) {
            c.dispatch(Ingest::Event(event));
        }
    }
}

/// **ONLY ONE PLAIN STOP CLOSES; THE OTHER OPENS THE OPPOSITE POSITION** (owner ruling of 2026-11-08: "only 1 can close,
/// other will open new opposite position"; perps, futures, shortable stocks, CFDs and forex alike), on a venue that puts a
/// reduce-only flag on its wire (binance) and on one that does not (dukascopy) alike. Two plain stops of the long's size (2):
/// the first closes it (the close flagged, as always: on ig and a hedging fxcm or oanda account the flag is what CLOSES), the
/// second finds that close in flight, closes nothing and opens a short of 2 (plain). Stops of 1.5 and 2: the first closes
/// 1.5, the second closes the 0.5 left and opens 1.5. Filled, the books end short 2 and short 1.5.
#[test]
fn only_one_plain_stop_closes_and_the_other_opens_the_opposite_position_on_every_venue() {
    for venue in ["binance", "dukascopy"] {
        for (sizes, want, end) in [
            (vec![2.0, 2.0], vec![(2.0, true), (2.0, false)], -2.0),
            (vec![1.5, 2.0], vec![(1.5, true), (0.5, true), (1.5, false)], -1.5),
        ] {
            let mut c = plain_stops_crossed(venue, 2.0, &sizes);
            let sent: Vec<(f64, bool)> =
                c.engine.client.submissions.iter().map(|r| (r.qty, r.reduce_only)).collect();
            assert_eq!(sent, want, "{venue} {sizes:?}: {:?}", c.recent);
            assert!(
                c.emulated.cond_engine.is_empty() && armed_count(&c) == 0,
                "{venue} {sizes:?}: spent"
            );
            fill_all(&mut c, venue);
            assert_eq!(c.engine.gate_position_size("X"), end, "{venue} {sizes:?}");
        }
    }
}

/// **The opening order of the second stop is judged as an opening order**: on a HALTED account the first stop still closes
/// the long, and the second's short is refused in words ("NOT sent"); the arm is spent and the book ends flat.
#[test]
fn on_a_halted_account_the_second_plain_stop_closes_nothing_and_opens_nothing() {
    let mut c = held_core("binance", 2.0, false);
    c.engine.trading_state = TradingState::Halted;
    for at in [0, 1] {
        c.apply_intent(
            OrderIntent::ArmConditional(ConditionalIntent {
                venue: "binance".into(),
                symbol: "X".into(),
                side: -1,
                qty: 2.0,
                price: Some(95.0),
                trail: None,
                trigger_by: None,
                kind_terms: None,
                account: None,
            }),
            at,
        );
    }
    c.fire_conditionals_at_price("binance", "X", 94.0, 2);
    let sent: Vec<(f64, bool)> =
        c.engine.client.submissions.iter().map(|r| (r.qty, r.reduce_only)).collect();
    assert_eq!(sent, [(2.0, true)], "{:?}", c.recent);
    assert!(c.recent.iter().any(|n| n.contains("already closing") && n.contains("NOT sent")));
    assert!(c.emulated.cond_engine.is_empty() && armed_count(&c) == 0);
    fill_all(&mut c, "binance");
    assert_eq!(c.engine.gate_position_size("X"), 0.0);
}

/// **On a SPOT (cash) account the second plain stop opens nothing**: crypto spot cannot short (the owner, 2026-11-08).
/// The first stop closes the long of 2; the second's sell of 2 finds nothing free, and the engine refuses it before the
/// gate in `vike_execution`'s `cash_oversell_denial` words, a `Denied` the strategy would hear; filled, the book is flat,
/// never short. The same two stops on the perpetual open the short
/// (`only_one_plain_stop_closes_and_the_other_opens_the_opposite_position_on_every_venue`).
#[test]
fn on_a_spot_account_the_second_plain_stop_is_refused_in_words_and_opens_no_short() {
    let mut c = held_core("binance", 2.0, false);
    c.engine.account.admit_margin_mode("X", Some(vike_model::MarginMode::Cash));
    c.engine.collect_applied_fills = true;
    for at in [0, 1] {
        c.apply_intent(
            OrderIntent::ArmConditional(ConditionalIntent {
                venue: "binance".into(),
                symbol: "X".into(),
                side: -1,
                qty: 2.0,
                price: Some(95.0),
                trail: None,
                trigger_by: None,
                kind_terms: None,
                account: None,
            }),
            at,
        );
    }
    c.fire_conditionals_at_price("binance", "X", 94.0, 2);
    let sent: Vec<(f64, bool)> =
        c.engine.client.submissions.iter().map(|r| (r.qty, r.reduce_only)).collect();
    assert_eq!(
        sent,
        [(2.0, true)],
        "only the first stop's close reached the venue: {:?}",
        c.recent
    );
    let denied: Vec<String> = c
        .engine
        .order_events
        .iter()
        .filter_map(|e| match &e.event.kind {
            vike_model::OrderEventKind::Denied { reason } => Some(reason.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        denied,
        ["a spot account cannot sell more than it holds: it has 0, this sells 2; nothing was sent"]
    );
    assert!(c.emulated.cond_engine.is_empty() && armed_count(&c) == 0, "both arms are spent");
    fill_all(&mut c, "binance");
    assert_eq!(c.engine.gate_position_size("X"), 0.0, "flat, never short");
}

/// **WHICH VERBS CLEAR A CORE-HELD STOP** (what `docs/ops/reconcile-on-restart.md` says, pinned): an
/// operator's `flatten` sends the close and leaves the stop resting AT THAT MOMENT (the stuck-order sweep
/// clears it once the position is flat: `core_held_stops.rs`), `market-exit` (the mass-cancel leg first) and
/// `mass-cancel` clear it at once.
#[test]
fn a_flatten_leaves_a_core_held_stop_resting_and_market_exit_clears_it() {
    let mut c = held_core("polymarket", 2.0, false);
    c.apply_intent(stop_submit("polymarket", -1, 2.0, 95.0, true), 0);

    c.apply_intent(
        OrderIntent::Flatten { venue: "polymarket".into(), symbol: "X".into(), account: None },
        1,
    );

    assert_eq!(c.engine.client.submissions.len(), 1, "the flatten's close went out");
    assert_eq!(armed_count(&c), 1, "…and the stop still rests");
    assert!(c.tick_conditionals);

    c.apply_intent(OrderIntent::MarketExit { venue: Some("polymarket".into()), account: None }, 2);

    assert_eq!(armed_count(&c), 0, "market-exit cleared it");
    assert!(c.emulated.cond_engine.is_empty() && !c.tick_conditionals);
}

/// **THE SAFE SIDE OF THE STOP FIRES NOTHING, for a long and for a short**, the stop stays armed (and its
/// tick lane on), and the first tick through it fires once. The SELL stop of a long fires below its
/// trigger; the BUY stop of a short above.
#[test]
fn a_tick_on_the_safe_side_of_a_core_held_stop_fires_nothing_for_a_long_and_a_short() {
    for (position, side, trigger, safe, through) in
        [(2.0, -1, 95.0, [100.0, 95.01], 94.99), (-2.0, 1, 105.0, [100.0, 104.99], 105.01)]
    {
        let mut c = held_core("polymarket", position, false);
        c.apply_intent(stop_submit("polymarket", side, 2.0, trigger, true), 0);
        for px in safe {
            c.fire_conditionals_at_price("polymarket", "X", px, 1);
        }
        assert!(c.engine.client.submissions.is_empty(), "position {position}: nothing fired");
        assert!(armed_count(&c) == 1 && c.tick_conditionals, "position {position}: still armed");

        c.fire_conditionals_at_price("polymarket", "X", through, 2);

        assert_eq!(
            c.engine.client.submissions.len(),
            1,
            "position {position}: fired once at {through}"
        );
        assert_eq!(c.engine.client.submissions[0].side, side);
    }
}

/// **A FIRE SAYS WHICH ORDER CARRIES IT, and a refused one says the position is bare.** The arm is spent
/// at the fire, and the order's own refusal line names only its order client id, so without this line an operator
/// cannot tell that a `REFUSED` order was the close of the stop they armed.
#[test]
fn a_fire_names_its_arm_and_order_and_a_refused_close_says_unprotected() {
    let mut c = held_core("polymarket", 2.0, false);
    c.apply_intent(stop_submit("polymarket", -1, 2.0, 95.0, true), 0);
    let arm = c.emulated.cond_engine.keys().next().expect("armed").clone();

    c.fire_conditionals_at_price("polymarket", "X", 94.0, 1);

    let order_client_id = c.engine.client.submissions[0].order_client_id.clone();
    let note = c.recent.iter().find(|n| n.contains("FIRED")).expect("the fire is said");
    assert!(
        note.contains(&arm) && note.contains(&order_client_id) && note.contains("SELL 2"),
        "names the arm, the order and the size: {note}"
    );
    assert!(!note.contains("REFUSED at once"), "an order that rests is not said refused: {note}");

    // The account is stopped between the arm and the fire: the close is refused at once.
    let mut c = held_core("polymarket", 2.0, false);
    c.apply_intent(stop_submit("polymarket", -1, 2.0, 95.0, true), 0);
    let arm = c.emulated.cond_engine.keys().next().expect("armed").clone();
    c.retired_engines.push(0);

    c.fire_conditionals_at_price("polymarket", "X", 94.0, 1);

    assert!(c.engine.client.submissions.is_empty(), "nothing reached the venue");
    let note = c.recent.iter().find(|n| n.contains("FIRED")).expect("the fire is said");
    assert!(
        note.contains(&arm) && note.contains("REFUSED at once") && note.contains("UNPROTECTED"),
        "{note}"
    );
}

/// **THE TICK REACHES A STOP ARMED ONTO A LABELLED ACCOUNT even when the venue's DEFAULT engine does not
/// hold the symbol** (an admitted symbol, decision 0174: only ALT trades `X`). The conditional check sat
/// inside the block that hands the price to the default engine, so such a stop was armed, said it was
/// held, and never fired. Through the real `Ingest::Quote` arm: the quote's bid 93 crosses the sell stop at
/// 95 and ALT's own position (1, not the default's) is closed on ALT's client.
#[test]
fn a_labelled_accounts_stop_fires_off_the_venues_tick_though_the_default_engine_lacks_the_symbol() {
    let mut alt = engine_on("polymarket", "X", RecordingClient::default());
    alt.route_key = "polymarket#ALT".to_string();
    let mut c = core_of(
        engine_on("polymarket", "Y", RecordingClient::default()),
        vec![(1_000.0, alt)],
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
    );
    c.extra_engines[0].1.account.positions.insert(
        ("polymarket".into(), "X".into(), vike_model::events::PositionSide::Both),
        vike_execution::PositionEntry { size: 1.0, average_price: 100.0, ..Default::default() },
    );
    let mut req = stop_request("polymarket", -1, 5.0, 95.0, true);
    req.account =
        Some(vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("label"));
    c.apply_intent(OrderIntent::Submit(Box::new(req)), 0);
    assert_eq!(armed_count(&c), 1, "precondition: armed onto ALT");
    assert!(!c.engine.accepts_symbol("X"), "precondition: the default engine does not hold X");

    c.dispatch(vike_execution::Ingest::Quote(Box::new(vike_execution::QuoteUpdate {
        venue: "polymarket".into(),
        symbol: "X".into(),
        quote: vike_marketdata::TickQuote {
            timestamp: 5,
            local_timestamp: 0,
            bid: 93.0,
            ask: 95.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: Default::default(),
        },
    })));

    let sent = &c.extra_engines[0].1.client.submissions;
    assert_eq!((sent.len(), sent[0].qty, sent[0].reduce_only), (1, 1.0, true), "ALT's 1");
    assert!(c.engine.client.submissions.is_empty(), "nothing reached the default account");
}

/// One two-sided quote of `X` through the real `Ingest::Quote` arm.
fn quote_x(venue: &str, bid: f64, ask: f64, timestamp: i64) -> vike_execution::Ingest {
    vike_execution::Ingest::Quote(Box::new(vike_execution::QuoteUpdate {
        venue: venue.into(),
        symbol: "X".into(),
        quote: vike_marketdata::TickQuote {
            timestamp,
            local_timestamp: 0,
            bid,
            ask,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: Default::default(),
        },
    }))
}

/// **A stop triggers on ITS side of the quote, never the mid** (owner, 2026-11-15). The owner's example
/// through the real `Ingest::Quote` arm: long, a SELL stop at 99 950. Bid 99 951 / ask 99 959 does not
/// trigger it; bid 99 950 / ask 99 960 FIRES it although the mid, 99 955, is still above the stop. And a BUY
/// stop is the ask's: ask 100 049 leaves it, ask 100 050 fires it while the mid (100 045) is short of it.
#[test]
fn a_sell_stop_fires_when_the_bid_reaches_it_and_a_buy_stop_when_the_ask_does() {
    let mut c = held_core("polymarket", 1.0, false);
    c.apply_intent(stop_submit("polymarket", -1, 1.0, 99_950.0, true), 0);
    assert_eq!(armed_count(&c), 1, "precondition: the core holds the sell stop");
    c.dispatch(quote_x("polymarket", 99_951.0, 99_959.0, 1));
    assert!(c.engine.client.submissions.is_empty(), "the bid is above the stop");
    c.dispatch(quote_x("polymarket", 99_950.0, 99_960.0, 2));
    let sent = &c.engine.client.submissions;
    assert_eq!((sent.len(), sent[0].side, sent[0].qty), (1, -1, 1.0), "the BID reached 99 950");

    let mut c = held_core("polymarket", -1.0, false);
    c.apply_intent(stop_submit("polymarket", 1, 1.0, 100_050.0, true), 0);
    assert_eq!(armed_count(&c), 1, "precondition: the core holds the buy stop");
    c.dispatch(quote_x("polymarket", 100_040.0, 100_049.0, 1));
    assert!(c.engine.client.submissions.is_empty(), "the ask is below the stop");
    c.dispatch(quote_x("polymarket", 100_040.0, 100_050.0, 2));
    let sent = &c.engine.client.submissions;
    assert_eq!(
        (sent.len(), sent[0].side),
        (1, 1),
        "the ASK reached 100 050, the mid 100 045 did not"
    );
}

/// **A core-held SELL stop is judged at a bar close on the bar's BID path, through the real `Ingest::BarClose` arm**
/// (REVIEW-FIX finding 7). The cTrader example: a sell stop at 1.08480; the window's bid touched 1.08480 while the
/// mid's low was 1.08485. With the bar's sides the stop fires at this close; the same bar with no sides (a candle no
/// quote came beside) is judged on its own OHLC and waits, today's law.
#[test]
fn a_bar_close_fires_a_core_held_sell_stop_on_the_bars_bid_low() {
    let close = |with_sides: bool| {
        let mut c = held_core("polymarket", 1.0, false);
        c.apply_intent(stop_submit("polymarket", -1, 1.0, 1.08480, true), 0);
        assert_eq!(armed_count(&c), 1, "precondition: the core holds the sell stop");
        let mut bar = vike_marketdata::test_support::shape_bar(1.08500, 1.08515, 1.08485, 1.08500);
        bar.timestamp = 60_000;
        let sides = vike_marketdata::BarSides {
            bid: vike_marketdata::test_support::shape_bar(1.08495, 1.08510, 1.08480, 1.08495),
            ask: vike_marketdata::test_support::shape_bar(1.08505, 1.08520, 1.08490, 1.08505),
        };
        c.dispatch(vike_execution::Ingest::BarClose(Box::new(vike_execution::BarUpdate {
            venue: "polymarket".into(),
            symbol: "X".into(),
            interval: "1m".into(),
            bar,
            sides: with_sides.then(|| Box::new(sides)),
        })));
        c.engine.client.submissions.len()
    };
    assert_eq!(close(true), 1, "the bid's low reached the stop: fired at this close");
    assert_eq!(close(false), 0, "no sides: the mid's low 1.08485 is judged, and waits");
}

// ── CancelBatch ───────────────────────────────────────────────────────────────────────────

/// `CancelBatch` on a MULTI-ENGINE core splits the batch by the engine each order client id was routed to
/// (`order_client_id_venue`) and hands each engine its own batch. That half of the arm in
/// `crates/vike-core/src/runtime/apply/routed.rs`'s `apply_intent_routed` was reached by no test:
/// `crates/vike-core/tests/runtime_smoke/dispatch_lanes.rs`'s
/// `command_batch_submit_then_cancel_batch` runs one engine.
#[test]
fn a_cancel_batch_on_a_multi_engine_core_cancels_each_order_on_its_own_engine() {
    let bin = engine_on("bin", "BTCUSDT", RecordingClient::default());
    let mut c = test_core_with(RecordingClient::default(), vec![(1.0, bin)]);
    c.apply_intent(OrderIntent::Submit(market_request("on-sim")), 0);
    let mut on_bin = *market_request("on-bin");
    on_bin.venue = "bin".into();
    c.apply_intent(OrderIntent::Submit(Box::new(on_bin)), 0);
    assert_eq!(
        c.order_client_id_venue.get("on-bin"),
        Some(&1),
        "precondition: on-bin was routed to engine 1"
    );
    assert_eq!(c.engine.client.submissions.len(), 1, "precondition: on-sim rests on engine 0");
    assert_eq!(
        c.extra_engines[0].1.client.submissions.len(),
        1,
        "precondition: on-bin rests on engine 1"
    );

    c.apply_intent(OrderIntent::CancelBatch(vec!["on-sim".into(), "on-bin".into()]), 0);

    assert_eq!(
        c.engine.client.cancels,
        vec!["on-sim".to_string()],
        "engine 0 cancels its own order and only that"
    );
    assert_eq!(
        c.extra_engines[0].1.client.cancels,
        vec!["on-bin".to_string()],
        "engine 1 cancels the order routed to it — a batch sent whole to engine 0 would skip it there"
    );
}

/// `CancelBatch` takes HELD exits out of the batch before the venue sees it: a held OTO child is
/// not at the venue and has no registered order, so the engine's batch cancel would skip it in
/// silence and leave it held. It is dropped off the book instead (`cancel_held`), while the live
/// order in the same batch goes to the venue. The single-order-client-id twin is
/// `crates/vike-core/src/runtime/tests/apply/bracket_contingency.rs`'s
/// `cancel_a_held_bracket_exit_drops_it_and_never_hits_the_venue`; no test sent a batch.
#[test]
fn a_cancel_batch_drops_a_held_exit_off_the_book_and_sends_only_the_live_order() {
    let mut c = test_core();
    c.apply_intent(OrderIntent::Submit(Box::new(held_child_request("held-1"))), 0);
    c.apply_intent(OrderIntent::Submit(market_request("live-1")), 0);
    assert!(c.held_orders.contains_key("held-1"), "precondition: held-1 is held off the venue");
    assert!(!c.engine.registry.contains_key("held-1"), "precondition: …with no registered order");

    c.apply_intent(OrderIntent::CancelBatch(vec!["held-1".into(), "live-1".into()]), 0);

    assert!(
        !c.held_orders.contains_key("held-1") && !c.contingency.contains("held-1"),
        "the held exit left the held map and the book"
    );
    assert_eq!(
        c.engine.client.cancels,
        vec!["live-1".to_string()],
        "only the live order reached the venue"
    );
}
