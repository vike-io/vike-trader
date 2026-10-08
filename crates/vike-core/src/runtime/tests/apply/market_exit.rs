//! Confirm, flatten and market-exit: expansion, halted/reducing, per-engine routing, windowing.

use super::*;

#[test]
fn confirm_routes_to_client_confirm() {
    let mut c = test_core();
    c.apply_intent(
        OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: "c1".into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(100.0),
            ..Default::default()
        })),
        0,
    );
    c.apply_intent(OrderIntent::Confirm("c1".into()), 0);
    assert_eq!(c.engine.client.confirms, vec!["c1".to_string()]);
}

#[test]
fn flatten_submits_reduce_only_opposite_of_position() {
    let mut c = test_core();
    // seed a +2 long directly (no instant-fill needed); key = (venue, symbol, position_side)
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    let coids = c.apply_intent(
        OrderIntent::Flatten { venue: "sim".into(), symbol: "BTCUSDT".into(), account: None },
        0,
    );
    assert_eq!(coids.len(), 1);
    let o = &c.engine.client.submissions[0];
    assert_eq!(o.side, -1, "long +2 ⇒ sell to flatten");
    assert_eq!(o.qty, 2.0);
    assert!(o.reduce_only);
    assert_eq!(o.order_type, "market");

    // flat position ⇒ no order
    let mut c2 = test_core();
    let none = c2.apply_intent(
        OrderIntent::Flatten { venue: "sim".into(), symbol: "BTCUSDT".into(), account: None },
        0,
    );
    assert!(none.is_empty());
}

#[test]
fn market_exit_expands_to_mass_cancel_then_flatten_per_open_position() {
    let mut c = test_core();
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    c.engine.account.positions.insert(
        ("sim".into(), "ETHUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: -3.0, avg_px: 50.0, ..Default::default() },
    );
    // a FLAT leftover row (a closed position never leaves the map) must NOT expand
    c.engine.account.positions.insert(
        ("sim".into(), "SOLUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 0.0, avg_px: 10.0, ..Default::default() },
    );
    let intents = c.expand_market_exit(None);
    assert_eq!(intents.len(), 3, "mass-cancel + 2 flattens (the flat row is skipped)");
    assert!(matches!(
        &intents[0],
        OrderIntent::MassCancel { venue: None, symbol: None, account: None }
    ));
    assert!(
        matches!(&intents[1], OrderIntent::Flatten { symbol, .. } if symbol == "BTCUSDT"),
        "Account insertion order is the expansion order"
    );
    assert!(matches!(&intents[2], OrderIntent::Flatten { symbol, .. } if symbol == "ETHUSDT"));
}

#[test]
fn market_exit_submits_reduce_only_closing_orders() {
    let mut c = test_core();
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    c.engine.account.positions.insert(
        ("sim".into(), "ETHUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: -3.0, avg_px: 50.0, ..Default::default() },
    );
    let coids = c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);
    assert_eq!(coids.len(), 2, "one closing order per open position");
    let subs = &c.engine.client.submissions;
    assert_eq!(subs.len(), 2);
    assert_eq!((subs[0].symbol.as_str(), subs[0].side, subs[0].qty), ("BTCUSDT", -1, 2.0));
    assert_eq!((subs[1].symbol.as_str(), subs[1].side, subs[1].qty), ("ETHUSDT", 1, 3.0));
    assert!(subs.iter().all(|o| o.reduce_only && o.order_type == "market"));
}

#[test]
fn market_exit_is_a_noop_beyond_mass_cancel_when_flat() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);
    assert!(coids.is_empty());
    assert!(c.engine.client.submissions.is_empty());
}

#[test]
fn market_exit_scoped_to_a_foreign_venue_flattens_nothing_here() {
    let mut c = test_core();
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    let intents = c.expand_market_exit(Some("binance"));
    assert_eq!(intents.len(), 1, "only the venue-scoped mass-cancel; sim is not the target");
    assert!(
        matches!(&intents[0], OrderIntent::MassCancel { venue: Some(v), symbol: None, account: None } if v == "binance")
    );
}

#[test]
fn market_exit_expansion_is_replay_deterministic() {
    // The property the compound verb relies on to need NO journal record kind of its own: TWO
    // cores that folded the SAME record prefix (here: the same submits + the same venue fills,
    // applied in the same order) expand `MarketExit` into the same intent list — the "replay"
    // core is a second, independently-built core, not the same one asked twice.
    let build = || {
        let mut c = test_core_with(QueuedEventClient::default(), Vec::new());
        // the engine folds venue events only for symbols it accepts
        c.engine.extra_symbols = vec!["ETHUSDT".into(), "SOLUSDT".into()];
        for (coid, sym, side, qty) in
            [("a", "BTCUSDT", 1, 1.0), ("b", "ETHUSDT", -1, 2.0), ("c", "SOLUSDT", 1, 3.5)]
        {
            c.apply_intent(
                OrderIntent::Submit(Box::new(OrderRequest {
                    client_order_id: coid.into(),
                    venue: "sim".into(),
                    symbol: sym.into(),
                    side,
                    qty,
                    order_type: "limit".into(),
                    price: Some(10.0),
                    ..Default::default()
                })),
                0,
            );
            c.engine.client.pending.extend(fill_events(coid, "sim", sym, side, qty, 10.0));
            c.pump_client();
        }
        c
    };
    let live = build();
    let replayed = build();
    assert_eq!(
        format!("{:?}", live.expand_market_exit(None)),
        format!("{:?}", replayed.expand_market_exit(None))
    );
    // and it is not vacuously equal — the fills really did open three positions
    assert_eq!(live.market_exit_flatten_legs(EngineRoute::Payload, None).len(), 3);
}

/// REGRESSION (adversarial review, major #1). The flatten legs MUST be derived AFTER the
/// mass-cancel, because `MassCancel`'s own arm ends in `pump_client()` — which can fold a FILL
/// that opens a position on a symbol that was FLAT when the operator hit the panic button. A
/// plan snapshotted up front carries no leg for it and the "get me out" verb hands back an
/// open position.
#[test]
fn market_exit_flattens_a_position_opened_by_the_mass_cancels_own_pump() {
    let mut c = test_core_with(QueuedEventClient::default(), Vec::new());
    c.engine.extra_symbols = vec!["ETHUSDT".into()];
    // a resting BUY on ETHUSDT; ETH is FLAT at this point (no fill folded yet)
    c.apply_intent(
        OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: "e1".into(),
            venue: "sim".into(),
            symbol: "ETHUSDT".into(),
            side: 1,
            qty: 4.0,
            order_type: "limit".into(),
            price: Some(50.0),
            ..Default::default()
        })),
        0,
    );
    assert!(
        c.market_exit_flatten_legs(EngineRoute::Payload, None).is_empty(),
        "precondition: flat at button-press"
    );
    // the venue had already filled it — the events are queued and will surface on the NEXT
    // poll, i.e. inside the mass-cancel's pump
    c.engine.client.pending.extend(fill_events("e1", "sim", "ETHUSDT", 1, 4.0, 50.0));

    c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);

    assert_eq!(
        c.engine.position_size_of("ETHUSDT", "BOTH"),
        4.0,
        "precondition: the mass-cancel pump really did open the position"
    );
    let close = c
        .engine
        .client
        .submissions
        .iter()
        .find(|o| o.symbol == "ETHUSDT" && o.reduce_only)
        .expect("a flatten leg for the position the mass-cancel pump opened");
    assert_eq!(
        (close.side, close.qty, close.order_type.as_str()),
        (-1, 4.0, "market"),
        "the exit must SEND the closing order; without the post-pump re-derivation the              operator is left long 4 ETH with nothing on the wire"
    );
}

/// THE PANIC BUTTON WORKS FROM A HALTED CORE — the guarantee this test now pins, and the exact
/// REVERSAL of what it pinned before.
///
/// It used to be `market_exit_under_halted_denies_every_flatten_leg_and_says_so`, asserting that
/// `RiskGate`'s kill switch denied EVERY order under `Halted`, `reduce_only` included, so the
/// exit was disarmed in precisely the safe-state / dead-man situations an operator reaches for
/// it. That was pinned as a "deliberate non-bypass"; it was really a trap — halted WITH the
/// position open and no way to close it, the escape being to un-halt the whole core (strategy
/// included) and re-issue. The gate now admits a POSITION-COVERED reduce under `Halted`
/// (`vike_model::is_covered_reduce`), which is exactly the shape `Flatten` mints.
///
/// A kill switch must stop OPENING risk, never trap you in it.
#[test]
fn market_exit_under_halted_still_flattens_because_a_halt_must_not_trap_you() {
    let mut c = test_core();
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    c.engine.trading_state = TradingState::Halted;
    c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);
    assert_eq!(
        c.engine.client.submissions.len(),
        1,
        "the flatten leg MUST reach the client while Halted — the operator has to be able to \
             get out: {:?}",
        c.engine.client.submissions
    );
    let leg = &c.engine.client.submissions[0];
    assert_eq!(
        (leg.side, leg.qty, leg.order_type.as_str(), leg.reduce_only),
        (-1, 2.0, "market", true),
        "and it is the closing leg: reduce_only market for the whole position"
    );
    assert!(
        c.recent.iter().any(|m| m.contains("HALTED")),
        "the operator is still TOLD the exit ran under a halt — silence would be worse now \
             that it works, not better: {:?}",
        c.recent
    );
}

/// THE OTHER HALF, and the mutation sentinel for the test above: admitting the flatten must not
/// have turned `Halted` into a state that admits ORDERS generally. An ordinary opening order on
/// the same halted core still dies at the gate and never reaches the venue.
#[test]
fn market_exit_flattening_under_halt_did_not_open_the_gate_to_opening_orders() {
    let mut c = test_core();
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    c.engine.trading_state = TradingState::Halted;
    // an opening BUY on the same symbol — risk-INCREASING, and the thing a halt exists to stop
    c.apply_intent(OrderIntent::Submit(market_req("open-me")), 0);
    assert!(
        c.engine.client.submissions.is_empty(),
        "a halt must still refuse an opening order: {:?}",
        c.engine.client.submissions
    );
    // ...and the exit still works on the very same core, in the same state.
    c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);
    assert_eq!(c.engine.client.submissions.len(), 1, "the exit is still admitted");
    assert!(c.engine.client.submissions[0].reduce_only);
}

/// A HALTED exit with NOTHING open says nothing about flattening. The "HALTED — flattening anyway"
/// note is a positive claim that legs ARE going out (the test above pins it when they are); with
/// no position there is no leg, and the note's `!legs.is_empty()` conjunct keeps it from
/// announcing a flatten of "0 leg(s)" that sends nothing. Pinned because the halted-with-a-leg
/// test cannot see that conjunct: it passes with or without it.
#[test]
fn market_exit_under_halted_with_nothing_open_claims_no_flatten() {
    let mut c = test_core();
    c.engine.trading_state = TradingState::Halted;
    assert!(
        c.market_exit_flatten_legs(EngineRoute::Payload, None).is_empty(),
        "precondition: no open position, so the exit has no flatten leg"
    );
    let coids = c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);
    assert!(coids.is_empty(), "nothing was submitted");
    assert!(c.engine.client.submissions.is_empty());
    assert!(
        !c.recent.iter().any(|m| m.contains("HALTED")),
        "no flatten is announced when there is nothing to flatten: {:?}",
        c.recent
    );
}

/// ⚠ **THE EXIT'S CANCEL LEG REACHES THE VENUE AS `RiskOff`, WHATEVER THE CALLER SAID.**
/// `crates/vike-core/src/runtime/apply/exit.rs`'s `lower_market_exit` re-enters the dispatcher for
/// its mass-cancel leg with `CancelIntent::RiskOff` spelled at the call, not with the
/// `cancel_intent` it was handed: "get me out" is the one verb whose cancels a venue must never
/// hold back under its own rate budget. The engine never reads the value — it only carries it to
/// the client — so the one place it is observable is the venue client, which is what
/// `RecordingClient`'s `cancel_intents` records. No vike-core test read it before; the engine-level
/// plumbing is `crates/vike-exec/tests/engine/cancel_intent.rs`'s
/// `a_classified_cancel_reaches_the_client_unchanged`.
///
/// Two callers: the external command path's `Unspecified` (what `apply_intent` passes) and
/// `Routine`, the one classification a venue MAY shed. Both come out `RiskOff`.
#[test]
fn market_exit_sends_its_mass_cancel_leg_as_risk_off_whatever_the_caller_said() {
    for caller in [CancelIntent::Unspecified, CancelIntent::Routine] {
        let mut c = test_core();
        // A resting order for the cancel leg to reach. `RecordingClient` emits nothing, so it stays
        // live; there is no position, so the exit has no flatten leg and the cancel is all it does.
        c.apply_intent(OrderIntent::Submit(market_req("rest-1")), 0);
        assert!(c.engine.client.cancel_intents.is_empty(), "precondition: nothing cancelled yet");

        c.apply_intent_with_cancel_intent(
            OrderIntent::MarketExit { venue: None, account: None },
            0,
            caller,
        );

        assert_eq!(
            c.engine.client.cancel_intents,
            vec![("rest-1".to_string(), CancelIntent::RiskOff)],
            "the exit's cancel leg must reach the venue as RiskOff when the caller said {caller:?}"
        );
    }
}

/// MUTATION SENTINEL for the test above: the `RiskOff` is the EXIT's, not the mass-cancel arm's. A
/// bare `MassCancel` hands its caller's classification to the venue unchanged, so a change that
/// hard-coded `RiskOff` inside `lower_mass_cancel` would leave the test above green and turn this
/// one red.
#[test]
fn a_bare_mass_cancel_sends_its_callers_classification_unchanged() {
    for caller in [CancelIntent::Unspecified, CancelIntent::Routine] {
        let mut c = test_core();
        c.apply_intent(OrderIntent::Submit(market_req("rest-1")), 0);

        c.apply_intent_with_cancel_intent(
            OrderIntent::MassCancel { venue: None, symbol: None, account: None },
            0,
            caller,
        );

        assert_eq!(
            c.engine.client.cancel_intents,
            vec![("rest-1".to_string(), caller)],
            "a bare mass-cancel carries the caller's classification: {caller:?}"
        );
    }
}

/// The documented counterpart: under `Reducing` the flatten legs ARE permitted (they are
/// `reduce_only`). `lanes.rs` claims this; nothing pinned it before.
#[test]
fn market_exit_under_reducing_still_flattens() {
    let mut c = test_core();
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    c.engine.trading_state = TradingState::Reducing;
    c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);
    assert_eq!(c.engine.client.submissions.len(), 1, "reduce_only passes the Reducing gate");
    assert!(c.engine.client.submissions[0].reduce_only);
    assert!(c.recent.iter().all(|m| !m.contains("HALTED")));
}

/// The cross-engine walk, the `*pv == eng_venue` filter and `Flatten`'s venue routing were
/// entirely unexercised (review test-gap #3).
///
/// ⚠ **READ WHAT THIS MOUNTS BEFORE TRUSTING ITS NAME.** The two engines are `"sim"` and
/// `"bin"` — TWO EXCHANGES — so each leg's own venue STRING already names its engine and the
/// payload route resolves it correctly with no index at all. This proves the cross-VENUE walk
/// and could never have failed on the cross-ACCOUNT one, which is the case where both legs
/// carry the same venue string and the routing has nothing but the index to go on. That case
/// is `crates/vike-core/src/runtime/tests/mount_account/core_minted_orders.rs`'s
/// `a_venue_scoped_market_exit_reaches_each_account_of_the_exchange`, and this test's name
/// read as if it already covered it for a whole round of investigation.
#[test]
fn market_exit_walks_every_engine_and_routes_each_flatten_to_its_own() {
    let mut c =
        test_core_with(QueuedEventClient::default(), vec![(1.0, extra_engine("bin", "BTCUSDT"))]);
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    c.extra_engines[0].1.account.positions.insert(
        ("bin".into(), "ETHUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: -3.0, avg_px: 50.0, ..Default::default() },
    );
    let legs = c.market_exit_flatten_legs(EngineRoute::Payload, None);
    assert_eq!(legs.len(), 2, "primary engine first, then extras in registration order");
    assert!(
        matches!(&legs[0], (0, OrderIntent::Flatten { venue, symbol, .. }) if venue == "sim" && symbol == "BTCUSDT")
    );
    assert!(
        matches!(&legs[1], (1, OrderIntent::Flatten { venue, symbol, .. }) if venue == "bin" && symbol == "ETHUSDT")
    );

    c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);
    assert_eq!(c.engine.client.submissions.len(), 1, "sim leg goes to the sim client");
    assert_eq!(c.engine.client.submissions[0].symbol, "BTCUSDT");
    assert_eq!(c.extra_engines[0].1.client.submissions.len(), 1, "bin leg goes to the bin one");
    assert_eq!(c.extra_engines[0].1.client.submissions[0].symbol, "ETHUSDT");

    // and a venue-scoped exit touches only that engine
    let scoped = c.market_exit_flatten_legs(EngineRoute::Payload, Some("bin"));
    assert!(
        scoped
            .iter()
            .all(|l| matches!(l, (1, OrderIntent::Flatten { venue, .. }) if venue == "bin"))
    );
}

/// Pins the documented hedge-mode scope note (review test-gap #6): non-`BOTH` position rows are
/// SKIPPED, so a future change that starts expanding LONG/SHORT legs cannot land silently.
#[test]
fn market_exit_skips_hedge_mode_long_short_rows() {
    let mut c = test_core();
    for side in ["LONG", "SHORT"] {
        c.engine.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), side.into()),
            vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
    }
    assert!(
        c.market_exit_flatten_legs(EngineRoute::Payload, None).is_empty(),
        "hedge-mode legs are out of scope until the primitives carry per-leg closes"
    );
    let coids = c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);
    assert!(coids.is_empty());
    assert!(c.engine.client.submissions.is_empty());
}

/// CHARACTERIZATION (this REVEALS the current verdict; it argues for no cure): a multi-position
/// `MarketExit` METERS ITS OWN LEGS against one order-rate window and starves the tail of them.
///
/// Three facts compose into it, each true on its own:
///
///   1. `Self::market_exit_flatten_legs` mints ONE `OrderIntent::Flatten` per non-flat
///      position, and each lowers to an ordinary `OrderIntent::Submit` — so each crosses
///      `vike_exec::RiskGate::check` with `consume_throttle: true`, like any opening order;
///   2. `RiskGate`'s sliding window is per-GATE, and `vike_mount::make_engine` builds ONE
///      engine — so ONE gate, so ONE window — per venue, shared by every symbol routed
///      through it (the field's own doc says so);
///   3. `Self::apply_intent`'s `MarketExit` arm applies every leg under the SAME `now`, so the
///      window cannot slide between them. `admit_throttle` evicts on `now_ms - window_ms`, and
///      all N stamps are identical.
///
/// `RiskGate::check_inner`'s throttle lane carries no `covered_reduce` term (unlike the min
/// floors, the price collar, the buying-power charge and the impact veto, which all bypass for
/// a covered reduce, and unlike the `Halted` kill switch, which admits one). Neither does
/// `max_notional_per_order`, which `vike_mount::require_live_risk_budget` makes MANDATORY on a
/// live mount and is therefore the more reachable denial there.
/// `crates/vike-exec/tests/risk/risk_lane_coverage.rs`'s
/// `a_position_covered_reduce_is_metered_by_the_shared_order_rate_window` and
/// `the_mandatory_live_caps_have_no_covered_reduce_bypass_either` pin those two lanes directly.
///
/// Why nothing caught it: `test_core`/`test_core_with` build the gate from `RiskLimits::new()`,
/// whose `max_orders_per_window` is `None`, so EVERY other `MarketExit` test here — the
/// halt-does-not-trap-you one included — runs with the throttle DISARMED.
#[test]
fn a_multi_position_market_exit_meters_its_own_legs_against_one_window() {
    let mut c = test_core();
    // Arm the window at 2 orders (`RiskLimits::new()` supplies `window_ms = 1000`) on the ONE
    // gate this engine owns, then open THREE positions for it to flatten.
    c.engine.gate.limits.max_orders_per_window = Some(2);
    for symbol in ["BTCUSDT", "ETHUSDT", "SOLUSDT"] {
        c.engine.account.positions.insert(
            ("sim".into(), symbol.into(), "BOTH".into()),
            vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
    }
    assert_eq!(
        c.market_exit_flatten_legs(EngineRoute::Payload, None).len(),
        3,
        "precondition: three legs to mint"
    );

    let denied_before = c.engine.dropped_unknown_coid;
    let coids = c.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 0);

    // The verb still MINTS a leg per position — a denied submit returns its coid like any
    // other, so the coid list alone reports success.
    assert_eq!(coids.len(), 3, "one leg minted per open position");
    // ...but only the window's worth of them reaches the venue.
    let sent: Vec<&str> = c.engine.client.submissions.iter().map(|o| o.symbol.as_str()).collect();
    assert_eq!(
        sent,
        vec!["BTCUSDT", "ETHUSDT"],
        "the exit spends its own rate window on its first legs and the last one is refused"
    );
    // The refusal is PUBLISHED as an ordinary `OrderDenied`, observed the way
    // `combo_denied_by_the_gate_emits_order_denied_and_submits_nothing` observes it: a denied
    // order was never registered, so its coid is counted as unknown on the way out.
    //
    // ⚠ Deliberately NOT asserted through `c.recent`. An earlier draft did, and it was wrong
    // about the harness rather than the behaviour: `apply_intent` publishes the event but folds
    // nothing, so `recent` — which the combo paths above populate by pushing to it directly —
    // stays EMPTY here even though the leg really was refused. The first two assertions in this
    // test already prove the refusal happened (one leg short on the wire); this proves the
    // engine said so rather than dropping it silently.
    assert!(
        c.engine.dropped_unknown_coid > denied_before,
        "the starved leg's OrderDenied was published"
    );
    // and the starved position is still OPEN — the operator pressed the panic button and is
    // still short of flat by one symbol.
    assert_eq!(c.engine.position_size_of("SOLUSDT", "BOTH"), 2.0);

    // MUTATION SENTINEL: it was the WINDOW that refused it — not the symbol, and not some
    // later lane. The identical leg, on the same core, one window later (`admit_throttle`
    // evicts stamps at or before `now_ms - window_ms`) reaches the venue.
    c.apply_intent(
        OrderIntent::Flatten { venue: "sim".into(), symbol: "SOLUSDT".into(), account: None },
        1_002,
    );
    let sent: Vec<&str> = c.engine.client.submissions.iter().map(|o| o.symbol.as_str()).collect();
    assert_eq!(
        sent,
        vec!["BTCUSDT", "ETHUSDT", "SOLUSDT"],
        "the starved leg must go out once the window slid — otherwise this pins the wrong lane"
    );
}
