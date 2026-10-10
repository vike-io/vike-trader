//! Combo orders: the capability refusal, the risk gate across legs, margin, exposure, leg fills.

use super::*;
use std::assert_matches;

/// The two-leg call spread every combo test below is built from. `venue`/`symbol`s match the
/// `test_core` engine ("sim") so the per-leg mark/position lookups actually resolve.
fn combo_spec(qty: f64) -> vike_model::ComboSpec {
    vike_model::ComboSpec {
        venue: "sim".into(),
        side: 1,
        qty,
        legs: vec![
            vike_model::ComboLeg { symbol: "BTC-27MAR26-100000-C".into(), ratio: 1 },
            vike_model::ComboLeg { symbol: "BTC-27MAR26-120000-C".into(), ratio: -1 },
        ],
        net_limit: Some(-0.0125), // a CREDIT combo, the awkward case
        time_in_force: vike_model::TimeInForce::Gtc,
    }
}

/// Give both legs of [`combo_spec`] a real mark on the primary engine. `check_combo` DENIES a
/// leg with no mark (`leg <sym>: no-mark`), so every admitted-path test needs this. The combo
/// gate's per-leg reference is resolver-priced, so feed BOTH the `Account.marks` scalar and the
/// price board — the same pair every live mark write-site writes; a fresh board price equal to
/// the scalar keeps every admitted-path verdict byte-identical.
/// A combo the RiskGate DENIED names the order it refused (`lower_submit`'s shape), so the mount that
/// built it is attributed the `OrderDenied`; the order was never registered and reached no venue.
fn assert_denied_combo(c: &CoreThread<RecordingClient>, coids: &[String], why: &str) {
    assert_eq!(coids.len(), 1, "{why}: a denied combo names the order it refused");
    assert!(
        !c.engine.registry.contains_key(&coids[0]),
        "{why}: a denied combo is never registered"
    );
}

fn mark_combo_legs<C: ExecutionClient>(c: &mut CoreThread<C>, px: f64) {
    for leg in combo_spec(1.0).legs {
        c.engine.account.set_mark_from("sim", &leg.symbol, px, MarkSource::VenueMark, 0);
        c.engine.price_board.set_mark("sim", &leg.symbol, px, 0);
    }
}

#[test]
fn combo_on_unsupported_venue_gets_a_terminal_reject_never_a_silent_drop() {
    // The emitter-split contract: no order may vanish. A venue that cannot take a combo must
    // still produce a full terminal lifecycle locally, because no venue client will.
    let mut c = test_core();
    mark_combo_legs(&mut c, 100.0);
    let before_seq = c.coid_gen.state().1;

    let coids = c.lower_combo(combo_spec(2.0), 7, false, EngineRoute::Payload);

    // the reject NAMES the order (as `lower_submit`'s capability reject does), so its builder is
    // attributed the event; it is registered TERMINAL, never live
    assert_eq!(coids.len(), 1, "the capability reject names the order it rejected");
    assert!(c.engine.registry.contains_key(&coids[0]));
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    assert_eq!(c.coid_gen.state().1, before_seq + 1, "ONE coid is minted for the combo");
    // the order exists and is TERMINAL — not dropped
    assert_eq!(c.engine.registry.len(), 1);
    let mo = c.engine.registry.values().next().unwrap();
    assert_eq!(mo.status, vike_exec::OrderStatus::Rejected, "must reach a TERMINAL state");
    assert!(c.recent.back().unwrap().contains("no combo support"));
}

#[test]
fn combo_passing_the_gate_mints_exactly_one_coid_and_submits() {
    // ONE coid for the WHOLE combo — not one per leg — carrying both legs on one request.
    let mut c = test_core();
    mark_combo_legs(&mut c, 100.0);
    let before_seq = c.coid_gen.state().1;

    let coids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);

    assert_eq!(coids.len(), 1, "ONE coid for the whole combo, never one per leg");
    assert_eq!(c.coid_gen.state().1, before_seq + 1, "exactly one mint");
    assert_eq!(c.engine.client.submissions.len(), 1, "ONE order reaches the venue");
    let sent = &c.engine.client.submissions[0];
    assert_eq!(sent.client_order_id, coids[0]);
    assert_eq!(sent.combo_legs.len(), 2, "both legs ride the one request");
    // the SIGNED net limit rides through verbatim — never absolute-valued, never clamped
    assert_eq!(sent.price, Some(-0.0125));
    assert!(c.engine.registry.contains_key(&coids[0]), "registered as ONE ManagedOrder");
}

#[test]
fn combo_denied_by_the_gate_emits_order_denied_and_submits_nothing() {
    // A combo veto must surface exactly as a single-order veto does: an `OrderDenied` event,
    // nothing on the wire, and no registry entry.
    let mut c = test_core();
    mark_combo_legs(&mut c, 100.0);
    c.engine.trading_state = TradingState::Halted; // the gate's kill switch precedes all else
    let denied_before = c.engine.dropped_unknown_coid;

    let coids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);

    assert_denied_combo(&c, &coids, "a denied combo");
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    assert!(c.engine.registry.is_empty(), "a denied combo never enters the registry");
    assert!(c.recent.back().unwrap().contains("combo DENIED"));
    // the OrderDenied really was published (an unregistered coid is counted as it routes)
    assert!(c.engine.dropped_unknown_coid > denied_before, "OrderDenied was published");
    // the denied path stamps the engine clock BEFORE the gate, like the single-order path
    // (adversarial review, minor #3)
    assert_eq!(c.engine.now_ms, 7, "a denied combo must not leave now_ms stale");
}

#[test]
fn combo_legs_accumulate_so_individually_affordable_legs_are_collectively_denied() {
    // THE point of the whole PR: #453 built leg-by-leg ACCUMULATION (each admitted leg's
    // initial margin is threaded onto the next leg's `margin_used`) precisely so N legs cannot
    // each fit inside the same unchanged free buying power. That behavior only means something
    // once a production caller supplies real per-symbol facts — this test proves it does.
    //
    // Sized so ONE leg fits the account and TWO do not: equity 1000, 100% IM, mark 100,
    // multiplier 1, qty 6 ⇒ each leg needs 6 × 100 × 1 × 1.0 = 600. Leg 1 is admitted
    // (600 <= 1000 free); leg 2 then sees margin_used 600, i.e. only 400 free against another
    // 600 ⇒ the COMBO is denied. (`Account::new`'s first argument is the contract MULTIPLIER,
    // not cash — the account's equity comes from `equity_seed` below.)
    let mut limits = RiskLimits::new();
    limits.im_requirement = Some(1.0);
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
    let mut c = assemble_core(
        engine,
        Vec::new(),
        CoreConfig::default(),
        market,
        snapshot,
        Arc::new(AtomicU64::new(0)),
    );
    c.engine.equity_seed = 1000.0;
    mark_combo_legs(&mut c, 100.0);

    let coids = c.lower_combo(combo_spec(6.0), 7, true, EngineRoute::Payload);

    assert_denied_combo(&c, &coids, "legs that individually fit must COLLECTIVELY be denied");
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    let reason = c.recent.back().unwrap();
    assert!(reason.contains("combo DENIED"), "unexpected refusal: {reason}");
    // the SECOND leg is the one that breaks the budget — proving leg 1 was admitted first and
    // its commitment was carried forward, which is the accumulation contract itself.
    assert!(
        reason.contains("BTC-27MAR26-120000-C"),
        "the SECOND leg must be the one denied (accumulation), got: {reason}"
    );

    // And the same combo at a size BOTH legs fit (qty 2 ⇒ 200 each, 400 total <= 1000) passes,
    // so the denial above is a budget verdict rather than a blanket combo refusal.
    let ok = c.lower_combo(combo_spec(2.0), 8, true, EngineRoute::Payload);
    assert_eq!(ok.len(), 1, "a combo that fits in aggregate is admitted");
}

/// **THE ACCOUNT-AGGREGATE CEILING ACCUMULATES ACROSS COMBO LEGS TOO** — the exposure twin of
/// the margin accumulation directly above, and the only test that drives
/// `RiskGate::check_combo`'s `committed_notional`.
///
/// `leg_ctx` reports every leg the account as it stood BEFORE the combo — it is a snapshot, and
/// it has to be, because the legs have not been sent. So without a running total each leg is
/// judged against the same unchanged ceiling and an N-leg combo consumes N times what one leg
/// was allowed: the buying-power hole #453 closed, wearing the exposure axis. This is the
/// production caller that makes the accumulation mean something.
///
/// Sized so ONE leg fits and TWO do not: ceiling 900, mark 100, qty 6 ⇒ each leg projects 600.
/// Leg 1 is admitted (600 ≤ 900); leg 2 then sees 600 already committed against its own 600 and
/// the COMBO is denied — under the account reason, not the per-symbol one, which is never armed
/// here at all.
#[test]
fn combo_legs_accumulate_against_the_account_ceiling_too() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits { max_account_exposure: Some(900.0), ..RiskLimits::new() }),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
    let mut c = assemble_core(
        engine,
        Vec::new(),
        CoreConfig::default(),
        market,
        snapshot,
        Arc::new(AtomicU64::new(0)),
    );
    mark_combo_legs(&mut c, 100.0);

    let coids = c.lower_combo(combo_spec(6.0), 7, true, EngineRoute::Payload);

    assert_denied_combo(&c, &coids, "two legs that each fit must COLLECTIVELY be denied");
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    let reason = c.recent.back().expect("a denial is noted").clone();
    assert!(
        reason.contains("over-account-exposure"),
        "the ACCOUNT ceiling must be what refused it, under its own reason: {reason}"
    );
    assert!(
        reason.contains("BTC-27MAR26-120000-C"),
        "…and the SECOND leg must be the one denied, which is the accumulation itself: \
             {reason}"
    );

    // And the same combo at a size both legs fit in AGGREGATE (2 × 200 = 400 ≤ 900) is
    // admitted, so the denial above is a budget verdict rather than a blanket combo refusal —
    // and the ceiling is genuinely armed on this path in both directions.
    let ok = c.lower_combo(combo_spec(2.0), 8, true, EngineRoute::Payload);
    assert_eq!(ok.len(), 1, "a combo that fits in aggregate is admitted");
}

/// REGRESSION (adversarial review, MAJOR #1). The `margin_used` baseline must price a MARKED
/// open position with NO per-symbol IM override exactly as `gate_and_register` does — falling
/// back to the priced symbol's own `im_req` — never silently skipping it. The divergence arms
/// exactly when the global `im_requirement` is None and margin was armed per-symbol, which is
/// precisely what `Command::SetMargin` produces (it only writes `im_by_symbol`): the old skip
/// saw the whole equity as free and ADMITTED a combo whose naked legs would be DENIED —
/// violating the gate's own invariant ("a combo must never pass a gate its naked legs would
/// fail").
#[test]
fn combo_margin_baseline_counts_no_override_positions_like_the_single_path() {
    let mut c = test_core();
    c.engine.equity_seed = 1000.0;
    // margin armed PER-SYMBOL only (Command::SetMargin's exact shape): global im stays None
    for leg in combo_spec(1.0).legs {
        c.engine.gate.limits.im_by_symbol.insert(leg.symbol, 1.0);
    }
    mark_combo_legs(&mut c, 100.0);
    // a MARKED open position on a FOREIGN symbol with NO per-symbol IM override:
    // 9 × 100 × 1, priced at the order/leg symbol's fallback rate 1.0 ⇒ 900 of the 1000
    // equity is already spoken for (mark == avg_px, so equity stays exactly 1000)
    c.engine.account.positions.insert(
        ("sim".into(), "FOREIGN".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 9.0, avg_px: 100.0, ..Default::default() },
    );
    c.engine.account.set_mark_from("sim", "FOREIGN", 100.0, MarkSource::VenueMark, 0);
    // the margin fold is resolver-priced now — feed the board the same price the live
    // write-sites would store alongside `account.set_mark`
    c.engine.price_board.set_mark("sim", "FOREIGN", 100.0, 0);

    // the NAKED leg is denied by the single-order path: free = 1000 − 900 = 100 while the
    // leg needs 2 × 100 × 1.0 = 200 (explicit limit price: the single-symbol engine's
    // `mark()` prices off the MOUNTED symbol, which this test never marks)
    let leg1 = combo_spec(1.0).legs[0].symbol.clone();
    c.apply_intent(
        OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: "naked".into(),
            venue: "sim".into(),
            symbol: leg1.clone(),
            side: 1,
            qty: 2.0,
            order_type: "limit".into(),
            price: Some(100.0),
            ..Default::default()
        })),
        0,
    );
    assert!(
        c.engine.client.submissions.is_empty(),
        "precondition: the naked leg is DENIED by the single-order path"
    );

    // CONSISTENCY, asserted directly: the combo carrying that same leg must be denied too.
    // (The old skip priced FOREIGN at 0, saw 1000 free, and admitted BOTH legs.)
    let coids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
    assert_denied_combo(&c, &coids, "the combo must fail exactly where its naked leg fails");
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    let reason = c.recent.back().unwrap();
    assert!(
        reason.contains(&leg1) && reason.contains("insufficient-margin"),
        "the leg must fail the margin check against the foreign position, got: {reason}"
    );
}

/// MAJOR-3 (the liquidation law's partition in the COMBO leg baseline): an ISOLATED open
/// position is backed by its own walled-off wallet, not the shared equity the combo is
/// admitted against, so it must no longer inflate the per-leg `margin_used` baseline. The
/// scenario is the test above with FOREIGN flipped Isolated: counted (the old fold) it
/// spoke for 900 of the 1000 equity and DENIED the combo; excluded, the combo fits with
/// room to spare and is ADMITTED. Cross books are untouched (the test above still denies).
#[test]
fn combo_margin_baseline_excludes_isolated_positions() {
    let mut c = test_core();
    c.engine.equity_seed = 1000.0;
    for leg in combo_spec(1.0).legs {
        c.engine.gate.limits.im_by_symbol.insert(leg.symbol, 1.0);
    }
    mark_combo_legs(&mut c, 100.0);
    // the SAME foreign position as the consistency test above — 9 × 100 × 1.0 = 900 if
    // counted — but ISOLATED with its own wallet: it never consumes the shared equity.
    c.engine.account.positions.insert(
        ("sim".into(), "FOREIGN".into(), "BOTH".into()),
        vike_exec::PositionEntry {
            size: 9.0,
            avg_px: 100.0,
            margin_mode: vike_model::MarginMode::Isolated,
            isolated_margin: Some(900.0),
        },
    );
    c.engine.account.set_mark_from("sim", "FOREIGN", 100.0, MarkSource::VenueMark, 0);

    // both legs need 2 × 100 × 1.0 = 200 each; baseline 0 + accumulation 200 → 400 of the
    // 1000 equity → ADMITTED. (Counted at 900, leg 1 alone would already fail: 100 free.)
    let coids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
    assert_eq!(
        coids.len(),
        1,
        "an isolated position must not consume the combo's shared margin baseline: {:?}",
        c.recent.back()
    );
    assert_eq!(c.engine.client.submissions.len(), 1, "the admitted combo reaches the venue");
}

/// The combo twin of #550's `gate_exposure_reads_the_resolver_not_the_stale_mark`: a combo
/// leg's projected-exposure REFERENCE used to be the raw `Account.marks` scalar (`mark_of`),
/// while the SAME crossing's per-leg equity/margin were already resolver-priced. That split let
/// a stale-LOW scalar UNDER-measure a leg's projected exposure and admit a combo the fresh
/// board denies — the risk-unsafe direction, and exactly what the single-order gate closed. The
/// reference now shares the resolver, so the whole crossing speaks ONE price. Long 10 on the
/// FIRST leg, board fresh at 200, stale `Account.marks` at 100, `max_total_exposure` 2500: a
/// combo buy 3 projects (10+3)·200 = 2600 > 2500 on that leg → DENY. Under the split basis it
/// was (10+3)·100 = 1300 → admitted.
#[test]
fn combo_exposure_reads_the_resolver_not_the_stale_mark() {
    let mut c = test_core();
    c.engine.gate.limits.max_total_exposure = Some(2500.0);
    let legs = combo_spec(1.0).legs;
    // pre-existing long on the FIRST leg — the one the exposure cap will trip
    c.engine.account.positions.insert(
        ("sim".into(), legs[0].symbol.as_str().into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 10.0, avg_px: 100.0, ..Default::default() },
    );
    // stale-LOW scalar vs fresh-HIGH board, on BOTH legs (the second leg must also price, or it
    // would `no-mark`-deny under the OLD basis and mask the real verdict)
    for leg in &legs {
        c.engine.account.set_mark_from("sim", &leg.symbol, 100.0, MarkSource::VenueMark, 0);
        c.engine.price_board.set_mark("sim", &leg.symbol, 200.0, 1);
    }

    let coids = c.lower_combo(combo_spec(3.0), 7, true, EngineRoute::Payload);

    assert_denied_combo(&c, &coids, "a fresh board must not be under-measured by a stale mark");
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    let reason = c.recent.back().unwrap();
    assert!(
        reason.contains(&legs[0].symbol) && reason.contains("over-max-exposure"),
        "the first leg must trip the resolver-priced exposure cap, got: {reason}"
    );
}

/// Stated as the invariant: the combo exposure verdict is a function of the RESOLVED price
/// alone — three wildly different `Account.marks` scalars over ONE fresh board all reach the
/// identical DENY, so the raw scalar is no longer an input (the combo twin of
/// `the_notional_lane_verdict_is_independent_of_the_mark_scalar`). Under the split basis, stale
/// 0 and 100 ADMITTED while stale 5_000 denied — the verdict tracked the scalar, not the board.
#[test]
fn the_combo_exposure_verdict_is_independent_of_the_mark_scalar() {
    for stale in [0.0, 100.0, 5_000.0] {
        let mut c = test_core();
        c.engine.gate.limits.max_total_exposure = Some(2500.0);
        let legs = combo_spec(1.0).legs;
        c.engine.account.positions.insert(
            ("sim".into(), legs[0].symbol.as_str().into(), "BOTH".into()),
            vike_exec::PositionEntry { size: 10.0, avg_px: 100.0, ..Default::default() },
        );
        for leg in &legs {
            c.engine.account.set_mark_from("sim", &leg.symbol, stale, MarkSource::VenueMark, 0);
            c.engine.price_board.set_mark("sim", &leg.symbol, 200.0, 1);
        }

        let coids = c.lower_combo(combo_spec(3.0), 7, true, EngineRoute::Payload);

        assert_denied_combo(&c, &coids, &format!("stale scalar {stale} changed the verdict"));
        assert!(
            c.engine.client.submissions.is_empty(),
            "stale scalar {stale} changed a resolver-priced combo exposure verdict"
        );
        let reason = c.recent.back().unwrap();
        assert!(
            reason.contains("over-max-exposure"),
            "stale {stale}: expected the board-priced exposure DENY, got: {reason}"
        );
    }
}

/// REGRESSION (sibling #457 review, MAJOR — the fix belongs in the lowering). A combo's leg
/// fills arrive as bare `Event::Fill`s carrying LEG symbols — neither the engine's mounted
/// symbol nor (before this fix) in `extra_symbols` — so `on_event`'s account-wide-WS symbol
/// filter DROPPED them: the FSM reached Filled (the wraps route by coid) while the Account
/// stayed flat and `Strategy::on_fill` never fired. Registration must admit every leg symbol
/// into the engine's scope.
#[test]
fn combo_leg_fills_fold_into_the_account_for_both_legs() {
    use vike_model::events::{Event, FillEvent, OrderAccepted, OrderFilled};
    let mut c = test_core_with(QueuedEventClient::default(), Vec::new());
    mark_combo_legs(&mut c, 100.0);
    c.engine.collect_applied_fills = true; // a strategy is mounted: on_fill delivery matters

    let coids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
    assert_eq!(coids.len(), 1, "precondition: the combo was gated + registered + submitted");
    let coid = coids[0].clone();

    // the venue's leg-fill stream: ONE coid, one bare Fill PER LEG carrying the LEG symbol
    // (distinct trade_ids — same-id fills are reconnect-deduped), then the terminal wrap
    let legs = combo_spec(2.0).legs;
    // `&'static str`: both call sites pass a source literal, so `TradeId: From<&'static str>`
    let mk_fill = |tid: &'static str, sym: &str, side: i32| FillEvent {
        trade_id: tid.into(),
        client_order_id: coid.clone(),
        venue: "sim".into(),
        symbol: sym.into(),
        side,
        last_qty: 2.0,
        last_px: 10.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 8,
        mark_price: Some(10.0),
        position_side: "BOTH".into(),
    };
    let f1 = mk_fill("t-leg1", &legs[0].symbol, 1); // ratio +1, combo bought ⇒ buy
    let f2 = mk_fill("t-leg2", &legs[1].symbol, -1); // ratio −1 ⇒ sell
    c.engine.client.pending.push_back(Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.clone(),
        venue_order_id: Some("v-combo".to_string().into()),
        ts: 8,
    }));
    c.engine.client.pending.push_back(Event::Fill(f1));
    c.engine.client.pending.push_back(Event::Fill(f2.clone()));
    c.engine.client.pending.push_back(Event::OrderFilled(OrderFilled {
        client_order_id: coid,
        fill: f2,
        ts: 8,
    }));
    c.pump_client();

    // the Account gained BOTH leg positions — the whole point of the symbol admission
    assert_eq!(c.engine.position_size_of(&legs[0].symbol, "BOTH"), 2.0);
    assert_eq!(c.engine.position_size_of(&legs[1].symbol, "BOTH"), -2.0);
    // and the strategy actually HEARS its fills
    assert_eq!(c.engine.applied_fills.len(), 2, "on_fill delivery for both leg fills");

    // idempotent: re-registering the same legs must not grow extra_symbols again
    let n = c.engine.extra_symbols.len();
    let again = c.lower_combo(combo_spec(2.0), 9, true, EngineRoute::Payload);
    assert_eq!(again.len(), 1);
    assert_eq!(c.engine.extra_symbols.len(), n, "leg symbols are admitted exactly once");
}

/// A hand-built INVALID spec must refuse BEFORE the mint (adversarial review, minor #1): a
/// burned coid would leave a sequence gap, and a gap is only diagnostic while ids that name
/// no order stay impossible — the same mint-after-validation rule the `ArmConditional` arm
/// documents.
#[test]
fn combo_invalid_spec_refuses_without_burning_a_coid() {
    let mut c = test_core();
    let before_seq = c.coid_gen.state().1;
    let mut spec = combo_spec(1.0);
    spec.legs.truncate(1); // < 2 legs: the constructor/Deserialize would refuse this
    let coids = c.lower_combo(spec, 7, true, EngineRoute::Payload);
    assert!(coids.is_empty());
    assert_eq!(c.coid_gen.state().1, before_seq, "a refused spec must not burn a coid");
    assert!(c.engine.client.submissions.is_empty());
    assert!(c.recent.back().unwrap().contains("combo REFUSED"));
}

/// A combo veto must reach `Strategy::on_order_event` exactly as a single-order veto does
/// (`gate_and_register` pushes a Denied `OrderEventOut` at its veto site) — routed by the
/// FIRST leg's symbol, because the combo request's own symbol is EMPTY by design
/// (adversarial review, minor #2).
#[test]
fn combo_denied_captures_an_order_event_for_the_strategy() {
    let mut c = test_core();
    mark_combo_legs(&mut c, 100.0);
    c.engine.collect_applied_fills = true; // a strategy is mounted
    c.engine.trading_state = TradingState::Halted;
    let coids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
    assert_denied_combo(&c, &coids, "a halted gate");
    assert_eq!(c.engine.order_events.len(), 1, "the veto must be captured for the strategy");
    let ev = &c.engine.order_events[0];
    assert_eq!(ev.venue, "sim");
    assert_eq!(ev.symbol, combo_spec(1.0).legs[0].symbol, "routed by the FIRST leg's symbol");
    assert_matches!(
        &ev.event.kind,
        vike_model::OrderEventKind::Denied { reason } if reason == "halted"
    );
}
