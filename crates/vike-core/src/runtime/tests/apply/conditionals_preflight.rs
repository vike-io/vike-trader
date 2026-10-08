//! Tagged submit, conditional fire/disarm, mass cancel, capability preflight, batch routing.

use super::*;

#[test]
fn tagged_submit_registers_minted_coid_for_modify() {
    let mut c = test_core();
    // simulate what drain_broker does for a tagged limit: apply Submit(empty coid), then register
    let coids = c.apply_intent(
        OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: String::new(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(100.0),
            ts: 0,
            ..Default::default()
        })),
        0,
    );
    c.strategy_tags.insert("0|sim|BTCUSDT|q1".to_string(), coids[0].clone());
    // resolve + modify by tag (the drain's path)
    let coid = c.strategy_tags.get("0|sim|BTCUSDT|q1").cloned().unwrap();
    c.apply_intent(
        OrderIntent::Modify { client_order_id: coid.clone(), new_qty: Some(2.0), new_price: None },
        0,
    );
    // the recording client saw exactly one submit; the modify targets the accepted order (no-op
    // pre-accept is fine — this asserts the tag→coid wiring, not the venue modify)
    assert_eq!(c.engine.client.submissions.len(), 1);
    assert_eq!(c.engine.client.submissions[0].client_order_id, coid);
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
        }),
        0,
    );
    // the runtime minted `{coid_session}a0` for the first arm of this session
    let arm_id = format!("{}a0", c.coid_gen.state().0);
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
    for (sym, px) in [("BTCUSDT", 95.0), ("BTCUSDT", 93.0), ("ETHUSDT", 95.0)] {
        c.apply_intent(
            OrderIntent::ArmConditional(ConditionalIntent {
                venue: "sim".into(),
                symbol: sym.into(),
                side: -1,
                qty: 1.0,
                price: Some(px),
                trail: None,
                trigger_by: None,
            }),
            0,
        );
    }
    // arms minted a0 (BTC@95), a1 (BTC@93), a2 (ETH@95); disarm the FIRST BTC one
    let session = c.coid_gen.state().0;
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
    })
}

/// How many arms rest across every book.
fn armed_count(c: &CoreThread<RecordingClient>) -> usize {
    c.conditional_books.values().map(|b| b.len()).sum()
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
    assert!(c.cond_engine.is_empty(), "no account recorded for an arm that does not exist");
    assert_eq!(c.arm_seq, 0, "refused before the mint: no arm id was spent");

    // SENTINEL: the same arm with a mark to seed from is armed, under the session's FIRST id.
    c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
    c.apply_intent(arm_on_btc(None, Some(5.0), None), 1);
    let first_id = format!("{}a0", c.coid_gen.state().0);
    let book = c
        .conditional_books
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
    assert!(c.conditional_books.is_empty(), "no book was even opened");
    assert!(c.cond_engine.is_empty());
    assert_eq!(c.arm_seq, 0, "no arm id was spent");
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
    assert!(c.cond_engine.is_empty());
    assert_eq!(c.arm_seq, 0, "refused before the mint: no arm id was spent");
}

/// The DUPLICATE-ID backstop, STOP branch. A live id cannot collide — `mint_arm_id` is a monotone
/// counter resumed from the `Snap` — so the only way here is a book that already holds the id the
/// core is about to mint, i.e. that resume contract broken upstream. This plants exactly that,
/// white-box, and pins what the arm does about it: the newcomer is refused BY NAME, the resting arm
/// keeps its terms (it is never replaced), and no account is recorded for an arm that never entered
/// a book.
///
/// ⚠ CHARACTERIZATION, not an endorsement: unlike the three refusals above, this branch has ALREADY
/// spent the id when the book says no (`arm_seq` moved). On a journaled core it has also already
/// written that arm's `ConditionalArmed` record — this core has no journal, so that half is read
/// from the code, not asserted here.
#[test]
fn a_stop_arm_whose_minted_id_already_rests_is_refused_and_the_resting_arm_is_kept() {
    let mut c = test_core();
    let next_id = format!("{}a{}", c.coid_gen.state().0, c.arm_seq);
    let key = ("sim".to_string(), "BTCUSDT".to_string());
    assert!(
        c.conditional_books.entry(key.clone()).or_default().add_stop(
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
    let book = &c.conditional_books[&key];
    assert_eq!(book.len(), 1, "the newcomer did not join the book");
    let (id, resting) = book.iter().next().expect("the planted arm");
    assert_eq!(
        (id, resting.order.side, resting.order.price),
        (next_id.as_str(), 1, Some(50.0)),
        "the resting arm keeps ITS terms — never replaced by the SELL at 95"
    );
    assert!(c.cond_engine.is_empty(), "no account recorded for an arm that never entered a book");
    assert_eq!(c.arm_seq, 1, "the id was spent before the book refused it");
}

/// The DUPLICATE-ID backstop, TRAILING branch — the twin of the test above, through the other
/// `add_*` call (`arm_conditional` spells the refusal once per branch). A mark is seeded so the
/// arm gets past the no-mark refusal and reaches the book.
#[test]
fn a_trailing_arm_whose_minted_id_already_rests_is_refused_and_the_resting_arm_is_kept() {
    let mut c = test_core();
    c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
    let next_id = format!("{}a{}", c.coid_gen.state().0, c.arm_seq);
    let key = ("sim".to_string(), "BTCUSDT".to_string());
    assert!(
        c.conditional_books.entry(key.clone()).or_default().add_stop(
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
    let book = &c.conditional_books[&key];
    assert_eq!(book.len(), 1, "the newcomer did not join the book");
    let (id, resting) = book.iter().next().expect("the planted arm");
    assert_eq!(
        (id, resting.order.price, resting.order.trail),
        (next_id.as_str(), Some(50.0), None),
        "the resting arm is still the planted STOP — never replaced by the trailing newcomer"
    );
    assert!(c.cond_engine.is_empty(), "no account recorded for an arm that never entered a book");
    assert_eq!(c.arm_seq, 1, "the id was spent before the book refused it");
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
    c.apply_intent(OrderIntent::Submit(market_req("rest-1")), 0);
    c.apply_intent(OrderIntent::Submit(Box::new(held_child_req("held-1"))), 0);
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

fn venue_req(venue: &str, order_type: &str, tif: vike_model::TimeInForce) -> Box<OrderRequest> {
    Box::new(OrderRequest {
        client_order_id: format!("pf-{venue}-{order_type}"),
        venue: venue.into(),
        symbol: "X".into(),
        side: 1,
        qty: 1.0,
        order_type: order_type.into(),
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
    // deribit wires no trigger orders — a "stop" would coerce to an IMMEDIATE market there
    let coids = c.apply_intent(
        OrderIntent::Submit(venue_req("deribit", "stop", vike_model::TimeInForce::Gtc)),
        7,
    );
    assert_eq!(coids.len(), 1, "the refusal still names the order");
    assert!(c.engine.client.submissions.is_empty(), "refused order must not reach the client");
    let mo = c.engine.registry.get(&coids[0]).expect("registered for the FSM to advance");
    assert_eq!(mo.status, vike_exec::OrderStatus::Rejected, "terminal reject");
    assert!(
        c.recent.iter().any(|n| n.contains("TRIGGER_UNSUPPORTED: kind=stop venue=deribit")),
        "machine-readable reason surfaced: {:?}",
        c.recent
    );
}

/// The aster/ig flip this task ships: a non-GTC limit that would silently rest GTC is now a
/// loud deny — while a GTC limit (what actually rests) still submits.
#[test]
fn preflight_flips_silent_tif_ignores_to_loud_denies() {
    let mut c = test_core();
    c.apply_intent(
        OrderIntent::Submit(venue_req("aster", "limit", vike_model::TimeInForce::Ioc)),
        0,
    );
    assert!(c.engine.client.submissions.is_empty(), "aster Ioc limit is refused");
    assert!(c.recent.iter().any(|n| n.contains("TIF_UNSUPPORTED: tif=Ioc venue=aster")));
    c.apply_intent(
        OrderIntent::Submit(venue_req("aster", "limit", vike_model::TimeInForce::Gtc)),
        0,
    );
    assert_eq!(c.engine.client.submissions.len(), 1, "aster GTC limit still submits");
}

/// THE COMPAT LAW at the core edge: everything venues accept-and-honor today still submits —
/// binance's perp-lane GTD (lane union), the coercion venues' coerced TIFs, and every
/// non-roster (sim/paper) venue id.
#[test]
fn preflight_passes_accepted_requests_through() {
    let mut c = test_core();
    for (venue, ot, tif) in [
        ("binance", "limit", vike_model::TimeInForce::Gtd), // perp lane wires native GTD
        ("polymarket", "limit", vike_model::TimeInForce::Ioc), // live Ioc→FOK coercion
        ("hyperliquid", "take_profit", vike_model::TimeInForce::Gtc), // native tpsl trigger
        ("sim", "limit", vike_model::TimeInForce::Ioc),     // non-roster venue: no row
    ] {
        let before = c.engine.client.submissions.len();
        c.apply_intent(OrderIntent::Submit(venue_req(venue, ot, tif)), 0);
        assert_eq!(
            c.engine.client.submissions.len(),
            before + 1,
            "{venue}/{ot}/{tif:?} must reach the client"
        );
    }
}

/// A bracket with a preflight-refused child is refused ATOMICALLY: no leg reaches the
/// client, the culprit carries its own reason, the siblings carry the culprit's coid.
#[test]
fn bracket_with_unsupported_child_is_refused_whole() {
    let mut c = test_core();
    let spec = vike_model::BracketSpec {
        venue: "deribit".into(), // no native trigger orders → the SL "stop" child is refused
        symbol: "X".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
    };
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(spec)), 0);
    assert_eq!(coids.len(), 3);
    assert!(c.engine.client.submissions.is_empty(), "no bracket leg may reach the client");
    for coid in &coids {
        let mo = c.engine.registry.get(coid).expect("every leg registered");
        assert_eq!(mo.status, vike_exec::OrderStatus::Rejected, "{coid} terminal");
    }
    assert!(c.recent.iter().any(|n| n.contains("TRIGGER_UNSUPPORTED: kind=stop venue=deribit")));
    assert!(c.recent.iter().any(|n| n.contains("BRACKET_ATOMIC_REFUSED: culprit=")));
}

/// SubmitBatch on a SINGLE-ENGINE core: a refused leg terminalizes locally while the healthy
/// legs proceed.
///
/// ⚠ **This one is also a byte-identity witness for the `all_primary` narrowing**, and it is
/// worth saying because the doc line used to read "single-engine PATH" and that is no longer
/// which path it takes. The `deribit` leg names a venue this core runs no engine for, so
/// `route_of` answers `None`: under the old `unwrap_or(0) == 0` the batch was `all_primary` and
/// went to engine 0's `submit_order_batch`; under `== Some(0)` it is not, and both legs
/// re-enter the single-submit arm. Every assertion below is unchanged, because the arm they
/// land in resolves the same engine (`unwrap_or(0)`, §4.2's decided `N = 0` cell) and runs the
/// same `caps_venue(routed, …)` preflight that refused the leg here.
#[test]
fn submit_batch_refuses_only_the_unsupported_leg() {
    let mut c = test_core();
    let good = *market_req("b-good");
    let bad = *venue_req("deribit", "stop", vike_model::TimeInForce::Gtc);
    c.apply_intent(OrderIntent::SubmitBatch(vec![good, bad]), 0);
    assert_eq!(c.engine.client.submissions.len(), 1, "only the healthy leg reaches the client");
    assert_eq!(c.engine.client.submissions[0].client_order_id, "b-good");
    let mo = c.engine.registry.get("pf-deribit-stop").expect("refused leg registered");
    assert_eq!(mo.status, vike_exec::OrderStatus::Rejected);
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
    let deribit = engine_on("deribit", "X", RecordingClient::default());
    let mut c = core_of(deribit, Vec::new(), CoreConfig::default());
    assert_eq!(
        c.route_of(EngineRoute::Payload, "deribit"),
        Some(0),
        "precondition: every leg resolves engine 0"
    );
    assert!(
        c.ambiguous_accounts(EngineRoute::Payload, "deribit").is_none(),
        "precondition: …unambiguously, so the batch arm takes the whole batch"
    );

    let good = *venue_req("deribit", "limit", vike_model::TimeInForce::Gtc);
    let bad = *venue_req("deribit", "stop", vike_model::TimeInForce::Gtc);
    let coids = c.apply_intent(OrderIntent::SubmitBatch(vec![good, bad]), 0);

    assert_eq!(
        coids,
        vec!["pf-deribit-limit".to_string(), "pf-deribit-stop".to_string()],
        "both legs are named, the refused one included"
    );
    let sent: Vec<&str> =
        c.engine.client.submissions.iter().map(|r| r.client_order_id.as_str()).collect();
    assert_eq!(sent, vec!["pf-deribit-limit"], "only the supported leg reaches the client");
    let mo = c.engine.registry.get("pf-deribit-stop").expect("the refused leg is registered");
    assert_eq!(mo.status, vike_exec::OrderStatus::Rejected, "and terminal");
    assert!(
        c.recent.iter().any(|n| n.contains("TRIGGER_UNSUPPORTED: kind=stop venue=deribit")),
        "machine-readable reason surfaced: {:?}",
        c.recent
    );
}

/// One leg on this core's own venue, carrying an OTO parent that has not filled, so the
/// single-submit arm must HOLD it rather than send it.
fn held_child_req(coid: &str) -> OrderRequest {
    let mut r = *market_req(coid);
    r.parent_order_id = Some("p-unfilled".into());
    r
}

/// An UNROUTABLE leg — a venue no engine of this core claims, so `route_of` answers `None`. A
/// non-roster id, so `caps_for` resolves the permissive default and the capability preflight
/// has nothing to say about it; the only thing under test here is the ROUTING.
fn unroutable_req(coid: &str) -> OrderRequest {
    let mut r = *market_req(coid);
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
    let coids = c.apply_intent(
        OrderIntent::SubmitBatch(vec![held_child_req("b-child"), unroutable_req("b-nowhere")]),
        0,
    );

    assert_eq!(coids, vec!["b-child".to_string(), "b-nowhere".to_string()], "both legs named");
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
        c.engine.client.submissions[0].client_order_id, "b-nowhere",
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
        OrderIntent::SubmitBatch(vec![*market_req("b-own"), unroutable_req("b-nowhere")]),
        0,
    );

    let sent: Vec<&str> =
        c.engine.client.submissions.iter().map(|r| r.client_order_id.as_str()).collect();
    assert_eq!(sent, vec!["b-own", "b-nowhere"], "both legs on the primary book, in order");
    assert!(
        c.extra_engines[0].1.client.submissions.is_empty(),
        "and the other venue's engine was never touched"
    );
}

// ── CancelBatch ───────────────────────────────────────────────────────────────────────────

/// `CancelBatch` on a MULTI-ENGINE core splits the batch by the engine each coid was routed to
/// (`coid_venue`) and hands each engine its own batch. That half of the arm in
/// `crates/vike-core/src/runtime/apply/routed.rs`'s `apply_intent_routed` was reached by no test:
/// `crates/vike-core/tests/runtime_smoke/dispatch_lanes.rs`'s
/// `command_batch_submit_then_cancel_batch` runs one engine.
#[test]
fn a_cancel_batch_on_a_multi_engine_core_cancels_each_order_on_its_own_engine() {
    let bin = engine_on("bin", "BTCUSDT", RecordingClient::default());
    let mut c = test_core_with(RecordingClient::default(), vec![(1.0, bin)]);
    c.apply_intent(OrderIntent::Submit(market_req("on-sim")), 0);
    let mut on_bin = *market_req("on-bin");
    on_bin.venue = "bin".into();
    c.apply_intent(OrderIntent::Submit(Box::new(on_bin)), 0);
    assert_eq!(c.coid_venue.get("on-bin"), Some(&1), "precondition: on-bin was routed to engine 1");
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
/// order in the same batch goes to the venue. The single-coid twin is
/// `crates/vike-core/src/runtime/tests/apply/bracket_contingency.rs`'s
/// `cancel_a_held_bracket_exit_drops_it_and_never_hits_the_venue`; no test sent a batch.
#[test]
fn a_cancel_batch_drops_a_held_exit_off_the_book_and_sends_only_the_live_order() {
    let mut c = test_core();
    c.apply_intent(OrderIntent::Submit(Box::new(held_child_req("held-1"))), 0);
    c.apply_intent(OrderIntent::Submit(market_req("live-1")), 0);
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
