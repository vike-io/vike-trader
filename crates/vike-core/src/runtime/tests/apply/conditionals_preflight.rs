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
            order_type: vike_model::OrderType::Limit,
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
    let coids = c.apply_intent(
        OrderIntent::Submit(venue_req("polymarket", "stop", vike_model::TimeInForce::Gtc)),
        7,
    );
    assert_eq!(coids.len(), 1, "the refusal still names the order");
    assert!(c.engine.client.submissions.is_empty(), "refused order must not reach the client");
    let mo = c.engine.registry.get(&coids[0]).expect("registered for the FSM to advance");
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
        venue: "polymarket".into(), // no native trigger orders → the SL "stop" child is refused
        symbol: "X".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
        account: None,
    };
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(spec)), 0);
    assert_eq!(coids.len(), 3);
    assert!(c.engine.client.submissions.is_empty(), "no bracket leg may reach the client");
    for coid in &coids {
        let mo = c.engine.registry.get(coid).expect("every leg registered");
        assert_eq!(mo.status, vike_model::OrderStatus::Rejected, "{coid} terminal");
    }
    assert!(c.recent.iter().any(|n| n.contains("TRIGGER_UNSUPPORTED: kind=stop venue=polymarket")));
    assert!(c.recent.iter().any(|n| n.contains("BRACKET_ATOMIC_REFUSED: culprit=")));
}

fn roster_bracket(venue: &str) -> vike_model::BracketSpec {
    vike_model::BracketSpec {
        venue: venue.into(),
        symbol: "X".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
        account: None,
    }
}

/// **A bracket whose EXIT the venue's adapter would refuse by name is refused WHOLE, in words, before
/// the entry is sent.** The exits are held until the entry fills and are never preflighted again, so
/// before the row said anything about a reduce-only kind (`VenueCaps::reduce_only_kinds`) the entry
/// filled and the adapter then refused the exit: IG places a reduce-only MARKET only (its close
/// endpoint cannot rest a stop or a limit) and Dukascopy refuses a reduce-only LIMIT (a take-profit),
/// leaving the position naked or without its profit target.
///
/// The culprit is the first refused leg: IG's stop-loss, Dukascopy's take-profit (its stop is the
/// venue's stop-loss). Every leg is registered and Rejected, nothing reaches the client, nothing is
/// held, and the reason on EVERY leg names the leg and the venue.
#[test]
fn a_bracket_whose_exit_the_venue_cannot_rest_is_refused_whole_in_words() {
    for (venue, leg, culprit_reason) in [
        ("dukascopy", "take_profit", "REDUCE_ONLY_UNSUPPORTED: kind=limit venue=dukascopy"),
        ("ig", "stop_loss", "REDUCE_ONLY_UNSUPPORTED: kind=stop venue=ig"),
    ] {
        let mut c = core_of(
            engine_on(venue, "X", RecordingClient::default()),
            Vec::new(),
            CoreConfig::default(),
        );
        let coids = c.apply_intent(OrderIntent::Bracket(Box::new(roster_bracket(venue))), 0);
        assert_eq!(coids.len(), 3, "{venue}: every leg is named");
        assert!(c.engine.client.submissions.is_empty(), "{venue}: the ENTRY must not be sent");
        assert!(c.held_orders.is_empty(), "{venue}: no exit may be held for an entry never sent");
        for coid in &coids {
            let mo = c.engine.registry.get(coid).expect("every leg registered");
            assert_eq!(mo.status, vike_model::OrderStatus::Rejected, "{venue}: {coid} terminal");
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
    }
}

/// The control of the test above, and the PRESERVE half: a venue whose adapter places both exits
/// (binance's perp lane: `reduceOnly` on the stop and on the limit) still takes the bracket, the
/// entry going out and both exits held.
#[test]
fn a_bracket_the_venue_can_rest_is_still_accepted_and_holds_its_exits() {
    for venue in ["binance", "fxcm", "ctrader", "deribit"] {
        let mut c = core_of(
            engine_on(venue, "X", RecordingClient::default()),
            Vec::new(),
            CoreConfig::default(),
        );
        let coids = c.apply_intent(OrderIntent::Bracket(Box::new(roster_bracket(venue))), 0);
        assert_eq!(coids.len(), 3, "{venue}");
        assert_eq!(c.engine.client.submissions.len(), 1, "{venue}: only the entry is sent");
        assert_eq!(c.engine.client.submissions[0].client_order_id, coids[0], "{venue}");
        assert_eq!(c.held_orders.len(), 2, "{venue}: stop-loss and take-profit held");
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
/// same `caps_venue(routed, …)` preflight that refused the leg here.
#[test]
fn submit_batch_refuses_only_the_unsupported_leg() {
    let mut c = test_core();
    let good = *market_req("b-good");
    let bad = *venue_req("polymarket", "stop", vike_model::TimeInForce::Gtc);
    c.apply_intent(OrderIntent::SubmitBatch(vec![good, bad]), 0);
    assert_eq!(c.engine.client.submissions.len(), 1, "only the healthy leg reaches the client");
    assert_eq!(c.engine.client.submissions[0].client_order_id, "b-good");
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

    let good = *venue_req("polymarket", "limit", vike_model::TimeInForce::Gtc);
    let bad = *venue_req("polymarket", "stop", vike_model::TimeInForce::Gtc);
    let coids = c.apply_intent(OrderIntent::SubmitBatch(vec![good, bad]), 0);

    assert_eq!(
        coids,
        vec!["pf-polymarket-limit".to_string(), "pf-polymarket-stop".to_string()],
        "both legs are named, the refused one included"
    );
    let sent: Vec<&str> =
        c.engine.client.submissions.iter().map(|r| r.client_order_id.as_str()).collect();
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
            vike_exec::PositionEntry { size: position, avg_px: 100.0, ..Default::default() },
        );
    }
    c
}

/// The operator's `submit --type stop --trigger <px> [--reduce-only]`, as the core receives it.
fn stop_submit(venue: &str, side: i32, qty: f64, trigger: f64, reduce_only: bool) -> OrderIntent {
    OrderIntent::Submit(Box::new(stop_req(venue, side, qty, trigger, reduce_only)))
}

fn stop_req(venue: &str, side: i32, qty: f64, trigger: f64, reduce_only: bool) -> OrderRequest {
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
    let seq_before = c.coid_gen.state().1;

    let coids = c.apply_intent(stop_submit("polymarket", -1, 2.0, 95.0, true), 7);

    assert!(coids.is_empty(), "nothing was submitted: the arm id is in the note, not a coid");
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    assert!(c.engine.registry.is_empty(), "no order was registered, so none was rejected either");
    assert_eq!(c.coid_gen.state().1, seq_before, "validate-then-mint: no coid was burned");
    assert_eq!(armed_count(&c), 1);
    let armed = c.cond_engine.values().next().expect("the arm's side record");
    assert!(armed.reduce_only && armed.engine == 0 && armed.mount.is_none(), "{armed:?}");
    assert!(
        c.tick_conditionals,
        "a stop checked per closed bar is no protection: the tick lane is on"
    );
    let note = last_note(&c).expect("the operator is told");
    assert!(note.contains("CORE-HELD STOP") && note.contains("NOT through an outage"), "{note}");
    assert!(note.contains(&format!("{}a0", c.coid_gen.state().0)), "it names its id: {note}");
}

/// **A stop that is not THIS verb keeps the refusal it always had**: not reduce-only (a breakout
/// ENTRY nobody asked to hold unattended), and reduce-only with no trigger price.
#[test]
fn a_stop_that_is_not_a_reduce_only_stop_with_a_trigger_stays_refused() {
    let mut c = held_core("polymarket", 2.0, false);
    let entry = c.apply_intent(stop_submit("polymarket", -1, 2.0, 95.0, false), 0);
    let no_trigger = OrderRequest {
        client_order_id: "no-trigger".into(),
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
    for coid in entry.iter().chain(&bare) {
        let mo = c.engine.registry.get(coid).expect("registered for the FSM to advance");
        assert_eq!(mo.status, vike_model::OrderStatus::Rejected, "{coid} terminal");
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
    let long = |size| vike_exec::PositionEntry { size, avg_px: 100.0, ..Default::default() };
    let key = || ("polymarket".into(), "X".into(), vike_model::events::PositionSide::Both);
    c.engine.account.positions.insert(key(), long(3.0));
    c.extra_engines[0].1.account.positions.insert(key(), long(1.0));

    c.apply_intent(stop_submit("polymarket", -1, 5.0, 95.0, true), 0);
    assert_eq!(armed_count(&c), 0, "no account named on a two-account venue: refused as ambiguous");

    let mut req = stop_req("polymarket", -1, 5.0, 95.0, true);
    req.account =
        Some(vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("label"));
    c.apply_intent(OrderIntent::Submit(Box::new(req)), 1);
    assert_eq!(
        c.cond_engine.values().map(|a| (a.engine, a.reduce_only)).collect::<Vec<_>>(),
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

    let coids = c.apply_intent(stop_submit("polymarket", -1, 2.0, 95.0, true), 0);

    assert_eq!(coids.len(), 1, "the refusal names its order");
    assert_eq!(armed_count(&c), 0, "nothing armed onto a stopped account");
    assert!(c.recent.iter().any(|n| n.contains("ACCOUNT_NOT_RUNNING")), "{:?}", c.recent);
}

/// **THE CAPABILITY MAP ALONE DECIDES, per venue.** Every roster venue takes a reduce-only stop; the
/// ones whose row carries `stop` hand it to their native path (nothing armed), the ones that do not
/// hold it in the core (nothing registered). No venue is named here, so a venue that gains a native
/// stop leaves this path with no edit, and one that has none takes it with no edit.
#[test]
fn the_capability_map_alone_decides_which_venues_take_the_core_held_path() {
    let mut held = Vec::new();
    for &venue in vike_model::VENUES {
        let native = vike_venues::caps_for(venue).supported_order_kinds.contains(&"stop");
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

/// **THE FIRE IS CAPPED TO THE LIVE POSITION and goes out `reduce_only`**: a stop armed for more than
/// the book holds sends the book, a stop armed for less sends what it was armed for, and a BUY stop
/// protects a short the same way. The tick lane the stop kept on goes back off once it has fired.
#[test]
fn the_fire_is_capped_to_the_live_position_and_goes_out_reduce_only() {
    for (position, side, armed_qty, want) in
        [(2.0, -1, 5.0, 2.0), (5.0, -1, 2.0, 2.0), (-3.0, 1, 10.0, 3.0)]
    {
        let mut c = held_core("polymarket", position, false);
        let (trigger, px) = if side < 0 { (95.0, 94.0) } else { (105.0, 106.0) };
        c.apply_intent(stop_submit("polymarket", side, armed_qty, trigger, true), 0);

        c.fire_conditionals_at_price("polymarket", "X", px, 1);

        assert_eq!(c.engine.client.submissions.len(), 1, "position {position}: one market");
        let o = &c.engine.client.submissions[0];
        assert_eq!(
            (o.side, o.qty, o.reduce_only, &o.order_type),
            (side, want, true, &vike_model::OrderType::Market),
            "position {position}, armed {armed_qty}"
        );
        assert!(
            c.cond_engine.is_empty() && !c.tick_conditionals,
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
        assert!(c.cond_engine.is_empty());
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
    let arm_id = c.cond_engine.keys().next().expect("armed").clone();
    let ring = c.recent.len();
    c.report_unheld_cancel(&OrderIntent::Cancel(arm_id.clone()));
    assert_eq!(c.recent.len(), ring, "an armed stop is not an unheld row: nothing is said");

    c.apply_intent(OrderIntent::Cancel(arm_id.clone()), 1);

    assert!(c.engine.client.cancels.is_empty(), "no cancel went to the venue");
    assert_eq!(armed_count(&c), 0);
    assert!(c.cond_engine.is_empty() && !c.tick_conditionals);
    assert!(last_note(&c).expect("said").contains("DISARMED"), "{:?}", c.recent);
    c.fire_conditionals_at_price("polymarket", "X", 94.0, 2);
    assert!(c.engine.client.submissions.is_empty(), "a disarmed stop does not fire");

    c.apply_intent(stop_submit("polymarket", -1, 2.0, 95.0, true), 3);
    let second = c.cond_engine.keys().next().expect("re-armed").clone();
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
        !c.conditional_books.contains_key(&("polymarket".to_string(), "OTHER".to_string())),
        "a tick on an unarmed pair opens no book"
    );
    assert!(c.engine.client.submissions.is_empty());

    c.apply_intent(wipe(), 2);
    assert!(c.cond_engine.is_empty() && !c.tick_conditionals, "the lane went with the arm");

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

/// **WHICH VERBS CLEAR A CORE-HELD STOP** (what `docs/ops/reconcile-on-restart.md` says, pinned): an
/// operator's `flatten` sends the close and leaves the stop resting (it is NOT cleared: the doc once said
/// it was), `market-exit` (the mass-cancel leg first) and `mass-cancel` clear it.
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
    assert!(c.cond_engine.is_empty() && !c.tick_conditionals);
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
/// at the fire, and the order's own refusal line names only its coid, so without this line an operator
/// cannot tell that a `REFUSED` order was the close of the stop they armed.
#[test]
fn a_fire_names_its_arm_and_order_and_a_refused_close_says_unprotected() {
    let mut c = held_core("polymarket", 2.0, false);
    c.apply_intent(stop_submit("polymarket", -1, 2.0, 95.0, true), 0);
    let arm = c.cond_engine.keys().next().expect("armed").clone();

    c.fire_conditionals_at_price("polymarket", "X", 94.0, 1);

    let coid = c.engine.client.submissions[0].client_order_id.clone();
    let note = c.recent.iter().find(|n| n.contains("FIRED")).expect("the fire is said");
    assert!(
        note.contains(&arm) && note.contains(&coid) && note.contains("SELL 2"),
        "names the arm, the order and the size: {note}"
    );
    assert!(!note.contains("REFUSED at once"), "an order that rests is not said refused: {note}");

    // The account is stopped between the arm and the fire: the close is refused at once.
    let mut c = held_core("polymarket", 2.0, false);
    c.apply_intent(stop_submit("polymarket", -1, 2.0, 95.0, true), 0);
    let arm = c.cond_engine.keys().next().expect("armed").clone();
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
/// held, and never fired. Through the real `Ingest::Quote` arm: the quote's mid 94 crosses the stop at
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
        vike_exec::PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() },
    );
    let mut req = stop_req("polymarket", -1, 5.0, 95.0, true);
    req.account =
        Some(vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("label"));
    c.apply_intent(OrderIntent::Submit(Box::new(req)), 0);
    assert_eq!(armed_count(&c), 1, "precondition: armed onto ALT");
    assert!(!c.engine.accepts_symbol("X"), "precondition: the default engine does not hold X");

    c.dispatch(vike_exec::Ingest::Quote(Box::new(vike_exec::QuoteUpdate {
        venue: "polymarket".into(),
        symbol: "X".into(),
        quote: vike_marketdata::QuoteTick {
            ts: 5,
            local_ts: 0,
            bid: 93.0,
            ask: 95.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    })));

    let sent = &c.extra_engines[0].1.client.submissions;
    assert_eq!((sent.len(), sent[0].qty, sent[0].reduce_only), (1, 1.0, true), "ALT's 1");
    assert!(c.engine.client.submissions.is_empty(), "nothing reached the default account");
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
