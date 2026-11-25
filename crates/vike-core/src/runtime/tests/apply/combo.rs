//! Combo orders: the capability refusal, the risk gate across legs, margin, exposure, leg fills, the
//! combo's account, the leg limits (paper only), and the option exercise's per-venue refusal.

use super::*;
use std::assert_matches;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::PositionSide;

/// The two-leg call spread every combo test below is built from. `venue`/`symbol`s match the
/// `test_core` engine ("sim") so the per-leg mark/position lookups actually resolve.
fn combo_spec(qty: f64) -> vike_model::OrderCombo {
    vike_model::OrderCombo {
        venue: "sim".into(),
        side: 1,
        qty,
        legs: vec![
            vike_model::OrderComboLeg {
                symbol: "BTC-27MAR26-100000-C".into(),
                ratio: 1,
                limit: None,
            },
            vike_model::OrderComboLeg {
                symbol: "BTC-27MAR26-120000-C".into(),
                ratio: -1,
                limit: None,
            },
        ],
        net_limit: Some(-0.0125), // a CREDIT combo, the awkward case
        time_in_force: vike_model::TimeInForce::Gtc,
        account: None,
        order_client_id: None,
    }
}

/// Give both legs of [`combo_spec`] a real mark on the primary engine. `check_combo` DENIES a
/// leg with no mark (`leg <sym>: no-mark`), so every admitted-path test needs this. The combo
/// gate's per-leg reference is resolver-priced, so feed BOTH the `Account.marks` scalar and the
/// price board — the same pair every live mark write-site writes; a fresh board price equal to
/// the scalar keeps every admitted-path verdict byte-identical.
/// A combo the RiskGate DENIED names the order it refused (`lower_submit`'s shape), so the mount that
/// built it is attributed the `OrderDenied`; the order was never registered and reached no venue.
fn assert_denied_combo(c: &CoreThread<RecordingClient>, order_client_ids: &[String], why: &str) {
    assert_eq!(order_client_ids.len(), 1, "{why}: a denied combo names the order it refused");
    assert!(
        !c.engine.registry.contains_key(&order_client_ids[0]),
        "{why}: a denied combo is never registered"
    );
}

fn mark_combo_legs<C: ExecutionClient>(c: &mut CoreThread<C>, px: f64) {
    for i in 0..=c.extra_engines.len() {
        let engine = c.engine_mut(i);
        for leg in combo_spec(1.0).legs {
            engine.account.set_mark_from("sim", &leg.symbol, px, MarkSource::VenueMark, 0);
            engine.price_board.set_mark("sim", &leg.symbol, px, 0);
        }
    }
}

#[test]
fn combo_on_unsupported_venue_gets_a_terminal_reject_never_a_silent_drop() {
    // The emitter-split contract: no order may vanish. A venue that cannot take a combo must
    // still produce a full terminal lifecycle locally, because no venue client will.
    let mut c = test_core();
    mark_combo_legs(&mut c, 100.0);
    let before_sequence = c.order_client_id_gen.state().1;

    let order_client_ids = c.lower_combo(combo_spec(2.0), 7, false, EngineRoute::Payload);

    // the reject NAMES the order (as `lower_submit`'s capability reject does), so its builder is
    // attributed the event; it is registered TERMINAL, never live
    assert_eq!(order_client_ids.len(), 1, "the capability reject names the order it rejected");
    assert!(c.engine.registry.contains_key(&order_client_ids[0]));
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    assert_eq!(
        c.order_client_id_gen.state().1,
        before_sequence + 1,
        "ONE order client id is minted for the combo"
    );
    // the order exists and is TERMINAL — not dropped
    assert_eq!(c.engine.registry.len(), 1);
    let mo = c.engine.registry.values().next().unwrap();
    assert_eq!(mo.status, vike_model::OrderStatus::Rejected, "must reach a TERMINAL state");
    assert!(c.recent.back().unwrap().contains("no combo support"));
}

#[test]
fn combo_passing_the_gate_mints_exactly_one_order_client_id_and_submits() {
    // ONE order client id for the WHOLE combo — not one per leg — carrying both legs on one request.
    let mut c = test_core();
    mark_combo_legs(&mut c, 100.0);
    let before_sequence = c.order_client_id_gen.state().1;

    let order_client_ids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);

    assert_eq!(
        order_client_ids.len(),
        1,
        "ONE order client id for the whole combo, never one per leg"
    );
    assert_eq!(c.order_client_id_gen.state().1, before_sequence + 1, "exactly one mint");
    assert_eq!(c.engine.client.submissions.len(), 1, "ONE order reaches the venue");
    let sent = &c.engine.client.submissions[0];
    assert_eq!(sent.order_client_id, order_client_ids[0]);
    assert_eq!(sent.combo_legs.len(), 2, "both legs ride the one request");
    // the SIGNED net limit rides through verbatim — never absolute-valued, never clamped
    assert_eq!(sent.price, Some(-0.0125));
    assert!(c.engine.registry.contains_key(&order_client_ids[0]), "registered as ONE OrderRecord");
}

#[test]
fn combo_denied_by_the_gate_emits_order_denied_and_submits_nothing() {
    // A combo veto must surface exactly as a single-order veto does: an `OrderDenied` event,
    // nothing on the wire, and no registry entry.
    let mut c = test_core();
    mark_combo_legs(&mut c, 100.0);
    c.engine.trading_state = TradingState::Halted; // the gate's kill switch precedes all else
    let denied_before = c.engine.events_dropped_order_unknown;

    let order_client_ids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);

    assert_denied_combo(&c, &order_client_ids, "a denied combo");
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    assert!(c.engine.registry.is_empty(), "a denied combo never enters the registry");
    assert!(c.recent.back().unwrap().contains("combo DENIED"));
    // the OrderDenied really was published (an unregistered order client id is counted as it routes)
    assert!(c.engine.events_dropped_order_unknown > denied_before, "OrderDenied was published");
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
    // Sized so ONE leg fits the account and TWO do not: equity 1000, 100% initial margin, mark 100,
    // multiplier 1, qty 6 ⇒ each leg needs 6 × 100 × 1 × 1.0 = 600. Leg 1 is admitted
    // (600 <= 1000 free); leg 2 then sees margin_used 600, i.e. only 400 free against another
    // 600 ⇒ the COMBO is denied. (`Account::new`'s first argument is the contract MULTIPLIER,
    // not cash — the account's equity comes from `equity_seed` below.)
    let mut limits = RiskLimits::new();
    limits.initial_margin_requirement = Some(1.0);
    let engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let market = Arc::new(MarketConflation {
        state: Mutex::new(MarketConflationState::default()),
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

    let order_client_ids = c.lower_combo(combo_spec(6.0), 7, true, EngineRoute::Payload);

    assert_denied_combo(
        &c,
        &order_client_ids,
        "legs that individually fit must COLLECTIVELY be denied",
    );
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
/// `leg_context` reports every leg the account as it stood BEFORE the combo — it is a snapshot, and
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
    let engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits { max_account_exposure: Some(900.0), ..RiskLimits::new() }),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let market = Arc::new(MarketConflation {
        state: Mutex::new(MarketConflationState::default()),
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

    let order_client_ids = c.lower_combo(combo_spec(6.0), 7, true, EngineRoute::Payload);

    assert_denied_combo(
        &c,
        &order_client_ids,
        "two legs that each fit must COLLECTIVELY be denied",
    );
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
/// open position with NO per-symbol initial margin override exactly as `gate_and_register` does — falling
/// back to the priced symbol's own `initial_margin_requirement` — never silently skipping it. The divergence arms
/// exactly when the global `initial_margin_requirement` is None and margin was armed per-symbol, which is
/// precisely what `Command::SetMargin` produces (it only writes `initial_margin_by_symbol`): the old skip
/// saw the whole equity as free and ADMITTED a combo whose naked legs would be DENIED —
/// violating the gate's own invariant ("a combo must never pass a gate its naked legs would
/// fail").
#[test]
fn combo_margin_baseline_counts_no_override_positions_like_the_single_path() {
    let mut c = test_core();
    c.engine.equity_seed = 1000.0;
    // margin armed PER-SYMBOL only (Command::SetMargin's exact shape): global im stays None
    for leg in combo_spec(1.0).legs {
        c.engine.gate.limits.initial_margin_by_symbol.insert(leg.symbol, 1.0);
    }
    mark_combo_legs(&mut c, 100.0);
    // a MARKED open position on a FOREIGN symbol with NO per-symbol initial margin override:
    // 9 × 100 × 1, priced at the order/leg symbol's fallback rate 1.0 ⇒ 900 of the 1000
    // equity is already spoken for (mark == average_price, so equity stays exactly 1000)
    c.engine.account.positions.insert(
        ("sim".into(), "FOREIGN".into(), PositionSide::Both),
        vike_execution::PositionEntry { size: 9.0, average_price: 100.0, ..Default::default() },
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
            order_client_id: "naked".into(),
            venue: "sim".into(),
            symbol: leg1.clone(),
            side: 1,
            qty: 2.0,
            order_type: vike_model::OrderType::Limit,
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
    let order_client_ids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
    assert_denied_combo(
        &c,
        &order_client_ids,
        "the combo must fail exactly where its naked leg fails",
    );
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    let reason = c.recent.back().unwrap();
    assert!(
        reason.contains(&leg1) && reason.contains("insufficient-margin"),
        "the leg must fail the margin check against the foreign position, got: {reason}"
    );
}

/// **What a resting order committed, a combo cannot spend again.** Equity 1000, 100% initial margin, marks 100. A
/// resting limit BUY 7 at 100 on a third symbol commits 700, so 300 is free, and the combo needs 200 a
/// leg. Sent as plain orders the first leg fits (200 of 300) and the second is refused (100 free). The
/// combo saw 1000 free and went out whole, the opposite of `check_combo`'s own rule ("a combo must never
/// pass a gate its naked legs would fail").
#[test]
fn a_resting_order_commits_the_margin_a_combo_is_admitted_against() {
    let legs = combo_spec(1.0).legs;
    let core = |resting: bool| {
        let mut c = test_core();
        c.engine.equity_seed = 1000.0;
        for leg in &legs {
            c.engine.gate.limits.initial_margin_by_symbol.insert(leg.symbol.clone(), 1.0);
        }
        mark_combo_legs(&mut c, 100.0);
        c.engine.price_board.set_mark("sim", "FOREIGN", 100.0, 0);
        if resting {
            c.apply_intent(OrderIntent::Submit(Box::new(limit_buy("resting", "FOREIGN", 7.0))), 0);
            assert_eq!(c.engine.client.submissions.len(), 1, "precondition: the limit rests");
        }
        c
    };

    let mut naked = core(true);
    for (i, leg) in legs.iter().enumerate() {
        let order_client_id = format!("naked-{i}");
        naked.apply_intent(
            OrderIntent::Submit(Box::new(limit_buy(&order_client_id, &leg.symbol, 2.0))),
            0,
        );
    }
    let sent: Vec<&str> =
        naked.engine.client.submissions.iter().map(|s| s.order_client_id.as_str()).collect();
    assert_eq!(sent, ["resting", "naked-0"], "precondition: the second naked leg is refused");

    let mut combo = core(true);
    let order_client_ids = combo.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
    assert_eq!(
        combo.engine.client.submissions.len(),
        1,
        "300 is free and the two legs need 400: the combo must stay off the wire: {:?}",
        combo.recent.back()
    );
    assert_denied_combo(&combo, &order_client_ids, "the combo fails where its naked legs fail");
    let reason = combo.recent.back().unwrap();
    assert!(
        reason.contains(&legs[1].symbol) && reason.contains("insufficient-margin"),
        "the second leg breaks the budget, got: {reason}"
    );

    // Nothing resting: 400 of 1000, admitted, so the denial above is the resting order's doing.
    let mut quiet = core(false);
    assert_eq!(quiet.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload).len(), 1);
    assert_eq!(quiet.engine.client.submissions.len(), 1, "{:?}", quiet.recent.back());
}

/// **A leg sees a hedge bucket as the plain order does** (`Engine::gate_position_size`). Long 5 in
/// the LONG bucket of the first leg, mark 100, `max_total_exposure` 600: a plain BUY 2 projects 7 x 100 = 700
/// and is refused. The leg read the BOTH bucket, saw flat, projected 2 x 100 = 200 and went out.
#[test]
fn a_combo_leg_is_judged_on_the_hedge_bucket_a_plain_order_is() {
    let legs = combo_spec(1.0).legs;
    let core = || {
        let mut c = test_core();
        c.engine.gate.limits.max_total_exposure = Some(600.0);
        mark_combo_legs(&mut c, 100.0);
        c.engine.account.positions.insert(
            ("sim".into(), legs[0].symbol.as_str().into(), PositionSide::Long),
            vike_execution::PositionEntry { size: 5.0, average_price: 100.0, ..Default::default() },
        );
        c
    };

    let mut naked = core();
    naked.apply_intent(OrderIntent::Submit(Box::new(limit_buy("naked", &legs[0].symbol, 2.0))), 0);
    assert!(naked.engine.client.submissions.is_empty(), "precondition: the naked leg is refused");

    let mut combo = core();
    let order_client_ids = combo.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
    assert!(
        combo.engine.client.submissions.is_empty(),
        "long 5 plus 2 projects 700 over a ceiling of 600: {:?}",
        combo.recent.back()
    );
    assert_denied_combo(&combo, &order_client_ids, "the combo fails where its naked leg fails");
    let reason = combo.recent.back().unwrap();
    assert!(
        reason.contains(&legs[0].symbol) && reason.contains("over-max-exposure"),
        "the first leg trips the exposure cap, got: {reason}"
    );
}

/// **A RESTING COMBO COMMITS ITS LEGS** to both live-order walks (`Engine::live_order_margin`, and
/// `live_order_notional` under `resolved_account_exposure_excluding`). The registry's copy of a combo has an
/// EMPTY symbol (`vike_model::build_combo`), so both walks priced "": the margin walk skipped it and it committed
/// nothing, and the exposure walk fell back to the combo's own price, the SIGNED net, negative for this credit
/// spread, so the sum was infinite and `max_account_exposure` refused every order but a covered reduce
/// (`account-exposure-unpriced`). The spread bought 2 with both legs at 100: 200 a leg, 400 of margin at 100 % initial margin
/// and 400 of exposure.
#[test]
fn a_resting_combo_commits_its_legs_margin_and_exposure() {
    let resting = |arm: fn(&mut RiskLimits)| {
        let mut c = test_core();
        arm(&mut c.engine.gate.limits);
        c.engine.equity_seed = 1000.0;
        mark_combo_legs(&mut c, 100.0);
        c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
        c.engine.price_board.set_mark("sim", "BTCUSDT", 100.0, 0);
        assert_eq!(c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload).len(), 1);
        assert_eq!(c.engine.client.submissions.len(), 1, "the combo rests: {:?}", c.recent.back());
        c
    };
    // A plain limit BUY at 100 through the engine's own gate; the denial reason, if any.
    let plain = |c: &mut CoreThread<RecordingClient>, order_client_id: &str, qty: f64| {
        let mut outbox = vike_execution::EventBusOutbox::default();
        c.engine.submit_order(&limit_buy(order_client_id, "BTCUSDT", qty), 8, &mut outbox);
        vike_execution::testing::denied_reason(&outbox)
    };

    // MARGIN: equity 1000, 100 % initial margin. The legs commit 400, so 600 is free.
    let mut c = resting(|l| l.initial_margin_requirement = Some(1.0));
    let config = c.engine.price_config;
    assert_eq!(c.engine.live_order_margin(&config, 1.0, ""), 400.0, "each leg at its own price");
    assert_eq!(plain(&mut c, "p7", 7.0).as_deref(), Some("insufficient-margin"), "700 > 600");
    assert_eq!(plain(&mut c, "p5", 5.0), None, "500 <= 600");

    // EXPOSURE: ceiling 1000. The legs count 400, not infinity.
    let mut c = resting(|l| l.max_account_exposure = Some(1000.0));
    let config = c.engine.price_config;
    assert_eq!(c.engine.resolved_account_exposure_excluding("BTCUSDT", "", &config), 400.0);
    assert_eq!(plain(&mut c, "p5", 5.0), None, "400 + 500 <= 1000");
    let reason = plain(&mut c, "p2", 2.0).expect("400 + 500 + 200 > 1000");
    assert!(reason.contains("projected 1100.00"), "{reason}");
}

/// A plain limit BUY at 100 on the `test_core` venue.
fn limit_buy(order_client_id: &str, symbol: &str, qty: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "sim".into(),
        symbol: symbol.into(),
        side: 1,
        qty,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        ..Default::default()
    }
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
        c.engine.gate.limits.initial_margin_by_symbol.insert(leg.symbol, 1.0);
    }
    mark_combo_legs(&mut c, 100.0);
    // the SAME foreign position as the consistency test above — 9 × 100 × 1.0 = 900 if
    // counted — but ISOLATED with its own wallet: it never consumes the shared equity.
    c.engine.account.positions.insert(
        ("sim".into(), "FOREIGN".into(), PositionSide::Both),
        vike_execution::PositionEntry {
            size: 9.0,
            average_price: 100.0,
            margin_mode: vike_model::MarginMode::Isolated,
            isolated_margin: Some(900.0),
        },
    );
    c.engine.account.set_mark_from("sim", "FOREIGN", 100.0, MarkSource::VenueMark, 0);

    // both legs need 2 × 100 × 1.0 = 200 each; baseline 0 + accumulation 200 → 400 of the
    // 1000 equity → ADMITTED. (Counted at 900, leg 1 alone would already fail: 100 free.)
    let order_client_ids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
    assert_eq!(
        order_client_ids.len(),
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
        ("sim".into(), legs[0].symbol.as_str().into(), PositionSide::Both),
        vike_execution::PositionEntry { size: 10.0, average_price: 100.0, ..Default::default() },
    );
    // stale-LOW scalar vs fresh-HIGH board, on BOTH legs (the second leg must also price, or it
    // would `no-mark`-deny under the OLD basis and mask the real verdict)
    for leg in &legs {
        c.engine.account.set_mark_from("sim", &leg.symbol, 100.0, MarkSource::VenueMark, 0);
        c.engine.price_board.set_mark("sim", &leg.symbol, 200.0, 1);
    }

    let order_client_ids = c.lower_combo(combo_spec(3.0), 7, true, EngineRoute::Payload);

    assert_denied_combo(
        &c,
        &order_client_ids,
        "a fresh board must not be under-measured by a stale mark",
    );
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
            ("sim".into(), legs[0].symbol.as_str().into(), PositionSide::Both),
            vike_execution::PositionEntry {
                size: 10.0,
                average_price: 100.0,
                ..Default::default()
            },
        );
        for leg in &legs {
            c.engine.account.set_mark_from("sim", &leg.symbol, stale, MarkSource::VenueMark, 0);
            c.engine.price_board.set_mark("sim", &leg.symbol, 200.0, 1);
        }

        let order_client_ids = c.lower_combo(combo_spec(3.0), 7, true, EngineRoute::Payload);

        assert_denied_combo(
            &c,
            &order_client_ids,
            &format!("stale scalar {stale} changed the verdict"),
        );
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
/// filter DROPPED them: the FSM reached Filled (the wraps route by order client id) while the Account
/// stayed flat and `Strategy::on_fill` never fired. Registration must admit every leg symbol
/// into the engine's scope.
#[test]
fn combo_leg_fills_fold_into_the_account_for_both_legs() {
    use vike_model::events::{Event, FillEvent, OrderAccepted, OrderFilled};
    let mut c = test_core_with(QueuedEventClient::default(), Vec::new());
    mark_combo_legs(&mut c, 100.0);
    c.engine.collect_applied_fills = true; // a strategy is mounted: on_fill delivery matters

    let order_client_ids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
    assert_eq!(
        order_client_ids.len(),
        1,
        "precondition: the combo was gated + registered + submitted"
    );
    let order_client_id = order_client_ids[0].clone();

    // the venue's leg-fill stream: ONE order client id, one bare Fill PER LEG carrying the LEG symbol
    // (distinct trade_ids — same-id fills are reconnect-deduped), then the terminal wrap
    let legs = combo_spec(2.0).legs;
    // `&'static str`: both call sites pass a source literal, so `TradeId: From<&'static str>`
    let mk_fill = |tid: &'static str, symbol_name: &str, side: i32| FillEvent {
        trade_id: tid.into(),
        order_client_id: order_client_id.clone(),
        venue: "sim".into(),
        symbol: symbol_name.into(),
        side,
        last_qty: 2.0,
        last_price: 10.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        timestamp: 8,
        mark_price: Some(10.0),
        position_side: PositionSide::Both,
    };
    let f1 = mk_fill("t-leg1", &legs[0].symbol, 1); // ratio +1, combo bought ⇒ buy
    let f2 = mk_fill("t-leg2", &legs[1].symbol, -1); // ratio −1 ⇒ sell
    c.engine.client.pending.push_back(Event::OrderAccepted(OrderAccepted {
        order_client_id: order_client_id.clone(),
        venue_order_id: Some("v-combo".to_string().into()),
        timestamp: 8,
    }));
    c.engine.client.pending.push_back(Event::Fill(f1));
    c.engine.client.pending.push_back(Event::Fill(f2.clone()));
    c.engine.client.pending.push_back(Event::OrderFilled(OrderFilled {
        order_client_id,
        fill: f2,
        timestamp: 8,
    }));
    c.pump_client();

    // the Account gained BOTH leg positions — the whole point of the symbol admission
    assert_eq!(c.engine.position_size_of(&legs[0].symbol, PositionSide::Both), 2.0);
    assert_eq!(c.engine.position_size_of(&legs[1].symbol, PositionSide::Both), -2.0);
    // and the strategy actually HEARS its fills
    assert_eq!(c.engine.applied_fills.len(), 2, "on_fill delivery for both leg fills");

    // idempotent: re-registering the same legs must not grow extra_symbols again
    let n = c.engine.extra_symbols.len();
    let again = c.lower_combo(combo_spec(2.0), 9, true, EngineRoute::Payload);
    assert_eq!(again.len(), 1);
    assert_eq!(c.engine.extra_symbols.len(), n, "leg symbols are admitted exactly once");
}

/// A hand-built INVALID spec must refuse BEFORE the mint (adversarial review, minor #1): a
/// burned order client id would leave a sequence gap, and a gap is only diagnostic while ids that name
/// no order stay impossible — the same mint-after-validation rule the `ArmConditional` arm
/// documents.
#[test]
fn combo_invalid_spec_refuses_without_burning_an_order_client_id() {
    let mut c = test_core();
    let before_sequence = c.order_client_id_gen.state().1;
    let mut spec = combo_spec(1.0);
    spec.legs.truncate(1); // < 2 legs: the constructor/Deserialize would refuse this
    let order_client_ids = c.lower_combo(spec, 7, true, EngineRoute::Payload);
    assert!(order_client_ids.is_empty());
    assert_eq!(
        c.order_client_id_gen.state().1,
        before_sequence,
        "a refused spec must not burn an order client id"
    );
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
    let order_client_ids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
    assert_denied_combo(&c, &order_client_ids, "a halted gate");
    assert_eq!(c.engine.order_events.len(), 1, "the veto must be captured for the strategy");
    let event = &c.engine.order_events[0];
    assert_eq!(event.venue, "sim");
    assert_eq!(event.symbol, combo_spec(1.0).legs[0].symbol, "routed by the FIRST leg's symbol");
    assert_matches!(
        &event.event.kind,
        vike_model::OrderEventKind::Denied { reason } if reason == "halted"
    );
}

/// A combo over legs quoted in ANOTHER currency than the account's: each leg's risk context carries
/// its rate and whether it has gone stale at the combo's own `now`, so the atomic crossing denies the
/// whole combo for a stale rate (`leg <symbol>: quote-rate-stale`, an opening structure) and admits
/// the identical combo while the rate is fresh. Legs admitted at rate 1/128 observed at 0.
#[test]
fn a_combo_over_foreign_quoted_legs_is_denied_for_a_stale_rate_and_passes_while_fresh() {
    let setup = || {
        let mut c = test_core();
        c.engine.quote_asset = "USD".into();
        for leg in combo_spec(1.0).legs {
            let outcome = c.engine.admit_symbol(&vike_execution::SymbolAdmission {
                route_key: "sim".into(),
                symbol: leg.symbol,
                grid: vike_model::SymbolGrid {
                    tick_size: Some(0.25),
                    lot_size: Some(1.0),
                    ..vike_model::SymbolGrid::default()
                },
                contract_value: None,
                initial_mark: None,
                initial_mark_ms: None,
                quote_currency: Some("JPY".into()),
                quote_rate: Some(1.0 / 128.0),
                rate_as_of_ms: Some(0),
                initial_margin_requirement: None,
                release: false,
                margin_mode: None,
            });
            assert_eq!(outcome, vike_execution::AdmitOutcome::Admitted);
        }
        mark_combo_legs(&mut c, 100.0);
        c
    };

    // Fresh: the combo reaches the venue as ONE order.
    let mut fresh = setup();
    let order_client_ids = fresh.lower_combo(combo_spec(2.0), 1_000, true, EngineRoute::Payload);
    assert_eq!(order_client_ids.len(), 1);
    assert_eq!(fresh.engine.client.submissions.len(), 1, "a fresh rate: the combo is admitted");

    // Stale: one millisecond past the window, the same combo is denied, by name, and never registered.
    let mut stale = setup();
    let now = vike_execution::DEFAULT_QUOTE_RATE_TTL_MS + 1;
    let order_client_ids = stale.lower_combo(combo_spec(2.0), now, true, EngineRoute::Payload);
    assert_denied_combo(&stale, &order_client_ids, "a combo over a stale rate");
    assert!(stale.engine.client.submissions.is_empty(), "nothing reaches the venue");
    let note = stale.recent.back().unwrap();
    assert!(
        note.contains("combo DENIED") && note.contains("quote-rate-stale"),
        "the denial names the rate: {note}"
    );
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// Two accounts of the `sim` venue: the default one at engine 0 and `sim#ALT` at engine 1.
fn two_account_core() -> CoreThread<RecordingClient> {
    let mut alt = sim_engine_with(RecordingClient::default());
    alt.route_key = "sim#ALT".into();
    core_of(sim_engine_with(RecordingClient::default()), vec![(1.0, alt)], CoreConfig::default())
}

/// Every order that reached a venue client, on every engine of `c`.
fn sent_anywhere(c: &CoreThread<RecordingClient>) -> usize {
    (0..=c.extra_engines.len()).map(|i| c.engine(i).client.submissions.len()).sum()
}

/// [`combo_spec`] priced by its LEGS (`OrderComboLeg::limit`), no net limit.
fn leg_priced_spec() -> vike_model::OrderCombo {
    let mut spec = combo_spec(1.0);
    spec.net_limit = None;
    spec.legs[0].limit = Some(0.03);
    spec.legs[1].limit = Some(0.01);
    spec
}

/// Wave 6: a combo NAMING its account (decision 0254) is resolved BEFORE the ambiguity gate, so on a
/// venue of two accounts it reaches the named engine and only it, recorded against that engine; the
/// same combo naming none is still refused as ambiguous on that very core.
#[test]
fn a_combo_naming_an_account_reaches_that_engine_and_only_it() {
    let mut c = two_account_core();
    mark_combo_legs(&mut c, 100.0);
    let mut spec = combo_spec(1.0);
    spec.account = Some(alt());

    let order_client_ids = c.lower_combo(spec, 7, true, EngineRoute::Payload);

    assert_eq!(
        order_client_ids.len(),
        1,
        "a named account is not ambiguous: {:?}",
        c.recent.back()
    );
    assert_eq!(c.engine(1).client.submissions.len(), 1, "the combo reached `sim#ALT`");
    assert_eq!(c.engine(1).client.submissions[0].account, Some(alt()), "the order carries it");
    assert_eq!(sent_anywhere(&c), 1, "exactly one book, never a fan-out");
    assert_eq!(
        c.order_client_id_venue.get(&order_client_ids[0]),
        Some(&1),
        "recorded against ALT's engine"
    );
    assert!(c.engine(1).registry.contains_key(&order_client_ids[0]));

    let bare = c.lower_combo(combo_spec(1.0), 8, true, EngineRoute::Payload);
    assert!(bare.is_empty(), "the account-less combo is still refused before the mint");
    assert!(c.recent.back().unwrap().contains("sim#ALT"), "{:?}", c.recent.back());
    assert_eq!(sent_anywhere(&c), 1);
}

/// Wave 6: a combo naming an account this node does not run is refused BY NAME before the mint, on a
/// two-account core and on a single-account one, where the venue's sole engine would otherwise answer.
#[test]
fn a_combo_naming_an_unheld_account_is_refused_in_words_and_mints_nothing() {
    let nosuch = AccountLabel::parse("NOSUCH").expect("a legal label");
    for (what, mut c) in [("two-account", two_account_core()), ("single-account", test_core())] {
        mark_combo_legs(&mut c, 100.0);
        let before_sequence = c.order_client_id_gen.state().1;
        let mut spec = combo_spec(1.0);
        spec.account = Some(nosuch.clone());

        let order_client_ids = c.lower_combo(spec, 7, true, EngineRoute::Payload);

        assert!(order_client_ids.is_empty(), "{what}: refused before the mint");
        assert_eq!(c.order_client_id_gen.state().1, before_sequence, "{what}: nothing is minted");
        assert_eq!(sent_anywhere(&c), 0, "{what}: nothing is sent");
        let note = c.recent.back().unwrap();
        assert!(note.starts_with("combo REFUSED") && note.contains("NOSUCH"), "{what}: {note}");
        assert!(note.contains("Nothing was sent"), "{what}: {note}");
    }
}

/// Wave 6: a combo whose SENDER named its id is placed under it (nothing minted, the journal's
/// `MintedSubmit` carries it), and a RETRY under the same id places nothing new: refused whole before
/// anything is journaled or sent, in a single submit's words for a reused id and the combo's own line.
#[test]
fn a_combo_retried_under_its_own_id_places_nothing_new() {
    let dir = crate::scratch::Scratch::reserved("combo-given-id-retry");
    let journal = Some(crate::runtime::JournalConfig::at(dir.to_path_buf()));
    let mut c = core_with(CoreConfig { journal, ..CoreConfig::default() });
    mark_combo_legs(&mut c, 100.0);
    let mut spec = combo_spec(1.0);
    spec.order_client_id = Some("my-combo-1".into());
    let before_sequence = c.order_client_id_gen.state().1;

    assert_eq!(c.lower_combo(spec.clone(), 7, true, EngineRoute::Payload), ["my-combo-1"]);
    assert_eq!(
        c.order_client_id_gen.state().1,
        before_sequence,
        "the sender's id: nothing is minted"
    );
    assert_eq!(c.engine.client.submissions.len(), 1);
    assert_eq!(c.engine.client.submissions[0].order_client_id, "my-combo-1");

    assert!(c.lower_combo(spec, 8, true, EngineRoute::Payload).is_empty(), "the retry");
    assert_eq!(c.engine.client.submissions.len(), 1, "nothing new is sent");
    assert_eq!(c.engine.registry.len(), 1, "nothing new is registered");
    assert_eq!(c.order_client_id_gen.state().1, before_sequence, "nor minted");
    let note = c.recent.back().unwrap();
    assert!(
        note.starts_with("combo on sim REFUSED WHOLE") && note.contains("my-combo-1"),
        "{note}"
    );
    assert!(c.recent.iter().any(|l| l.starts_with("order my-combo-1 REFUSED")), "{:?}", c.recent);

    c.journal.as_mut().expect("journal").flush().expect("flush");
    let minted: Vec<String> = vike_journal::Journal::read_all(&dir)
        .expect("readable")
        .into_iter()
        .filter_map(|r| match r {
            vike_journal::JournalRecord::MintedSubmit { request: req, .. } => {
                Some(req.order_client_id)
            }
            _ => None,
        })
        .collect();
    assert_eq!(minted, ["my-combo-1"], "journaled once, under the sender's id");
}

/// Wave 6: the wire owes the grid multiple. A combo whose leg size (`|ratio| × qty`) is off the lot is
/// DENIED whole, naming the leg, and nothing reaches the venue or the paper book; the same combo on
/// the grid goes out as it was built.
#[test]
fn a_combo_with_an_off_lot_leg_is_denied_and_an_on_grid_one_goes_out_unchanged() {
    let core = || {
        let mut c = test_core();
        c.engine.gate.limits.lot_size = Some(1.0);
        mark_combo_legs(&mut c, 100.0);
        c
    };
    let mut c = core();
    let order_client_ids = c.lower_combo(combo_spec(2.5), 7, true, EngineRoute::Payload);
    assert_denied_combo(&c, &order_client_ids, "an off-lot leg");
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    let note = c.recent.back().unwrap();
    assert!(note.starts_with("combo DENIED: leg BTC-27MAR26-100000-C: off-grid-qty"), "{note}");

    let mut c = core();
    assert_eq!(c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload).len(), 1);
    assert_eq!(c.engine.client.submissions.len(), 1, "{:?}", c.recent.back());
    assert_eq!(c.engine.client.submissions[0].qty, 2.0, "the combo's own size");
}

/// The spot rule meets every leg a combo SELLS (`Engine::combo_cash_denial`, ahead of the gate): the
/// sold leg a `Cash` symbol and nothing held, the combo is DENIED whole naming that leg and nothing is sent (on
/// margin the same combo goes out: `combo_passing_the_gate_mints_exactly_one_order_client_id_and_submits`).
#[test]
fn a_combo_selling_a_spot_leg_it_does_not_hold_is_denied_naming_the_leg() {
    let mut c = test_core();
    mark_combo_legs(&mut c, 100.0);
    c.engine.account.admit_margin_mode("BTC-27MAR26-120000-C", Some(vike_model::MarginMode::Cash));
    let order_client_ids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
    assert_denied_combo(&c, &order_client_ids, "a spot leg sold past the holding");
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    let note = c.recent.back().unwrap();
    assert!(
        note.starts_with(
            "combo DENIED: leg BTC-27MAR26-120000-C: a spot account cannot sell more than it holds"
        ),
        "{note}"
    );
}

/// Wave 6: a combo priced by its legs goes out on a STATED paper engine only (its book fills it by
/// `vike_model::combo_legs_cross`): a `Limit` with no net price, every leg's limit on the request. On
/// an engine nobody told its tier (the seed is no proof), a demo and a live one it is minted and
/// rejected TERMINAL in words (the no-combo-venue shape), and nothing is sent.
#[test]
fn a_leg_priced_combo_goes_out_on_a_paper_engine_only_and_is_rejected_in_words_elsewhere() {
    let mut paper = test_core();
    paper.engine.set_tier(vike_model::AccountTier::Paper);
    mark_combo_legs(&mut paper, 100.0);
    let order_client_ids = paper.lower_combo(leg_priced_spec(), 7, true, EngineRoute::Payload);
    assert_eq!(order_client_ids.len(), 1);
    assert_eq!(paper.engine.client.submissions.len(), 1, "{:?}", paper.recent.back());
    let sent = &paper.engine.client.submissions[0];
    assert_eq!(sent.order_type, vike_model::OrderType::Limit);
    assert_eq!(sent.price, None, "a combo priced by its legs states no net");
    let limits: Vec<Option<f64>> = sent.combo_legs.iter().map(|l| l.limit).collect();
    assert_eq!(limits, [Some(0.03), Some(0.01)]);

    for tier in [None, Some(vike_model::AccountTier::Demo), Some(vike_model::AccountTier::Live)] {
        let mut c = test_core();
        if let Some(t) = tier {
            c.engine.set_tier(t);
        }
        mark_combo_legs(&mut c, 100.0);
        let before_sequence = c.order_client_id_gen.state().1;

        let order_client_ids = c.lower_combo(leg_priced_spec(), 7, true, EngineRoute::Payload);

        assert_eq!(order_client_ids.len(), 1, "{tier:?}: the reject names the order it minted");
        assert_eq!(
            c.order_client_id_gen.state().1,
            before_sequence + 1,
            "{tier:?}: one order client id"
        );
        assert!(c.engine.client.submissions.is_empty(), "{tier:?}: nothing reaches the venue");
        assert_eq!(
            c.engine.registry[&order_client_ids[0]].status,
            vike_model::OrderStatus::Rejected
        );
        let note = c.recent.back().unwrap();
        assert!(
            note.contains("takes no limit per combo leg") && note.contains("no venue row"),
            "{tier:?}: {note}"
        );
    }

    // A venue with no combo at all keeps its own words, leg limits or not.
    let mut c = test_core();
    mark_combo_legs(&mut c, 100.0);
    assert_eq!(c.lower_combo(leg_priced_spec(), 7, false, EngineRoute::Payload).len(), 1);
    assert!(c.recent.back().unwrap().contains("no combo support"), "{:?}", c.recent.back());
}

/// Wave 6: an option exercise is refused BY NAME on every venue, nothing minted or sent: deribit's
/// options are European, ibkr's stay closed (decision 0234), alpaca is deferred, any other venue
/// trades no option (`vike_venues::venue_caps::exercise_refusal`), an id off the roster is said by the
/// core. Its account resolves first (an unheld one is refused by name, a bare venue of two accounts as
/// ambiguous), and a spec whose terms are no exercise says so in its own words.
#[test]
fn an_option_exercise_is_refused_by_name_on_every_venue_and_sends_nothing() {
    let exercise = |venue: &str, qty: f64, account: Option<AccountLabel>| {
        OrderIntent::Exercise(Box::new(vike_model::ExerciseSpec {
            venue: venue.into(),
            symbol: "BTC-27MAR26-100000-C".into(),
            qty,
            account,
        }))
    };
    let refused = |c: &mut CoreThread<RecordingClient>, intent: OrderIntent| {
        let before_sequence = c.order_client_id_gen.state().1;
        assert!(c.apply_intent(intent, 7).is_empty(), "nothing is named");
        assert_eq!(c.order_client_id_gen.state().1, before_sequence, "nothing is minted");
        assert_eq!(sent_anywhere(c), 0, "nothing is sent");
        c.recent.back().unwrap().to_string()
    };

    for (venue, words) in [
        ("deribit", "European"),
        ("ibkr", "decision 0234"),
        ("alpaca", "deferred"),
        ("binance", "trades no option"),
    ] {
        let mut c = core_of(
            engine_on(venue, "X", RecordingClient::default()),
            vec![],
            CoreConfig::default(),
        );
        let note = refused(&mut c, exercise(venue, 1.0, None));
        let head = format!("exercise of 1 BTC-27MAR26-100000-C on {venue} REFUSED");
        assert!(note.starts_with(&head) && note.contains(words), "{venue}: {note}");
    }

    let mut c = test_core();
    let note = refused(&mut c, exercise("sim", 0.0, None));
    assert!(note.contains("not a finite number above zero"), "the spec's own words: {note}");
    let nosuch = AccountLabel::parse("NOSUCH").expect("a legal label");
    let note = refused(&mut c, exercise("sim", 1.0, Some(nosuch)));
    assert!(note.starts_with("exercise REFUSED") && note.contains("NOSUCH"), "{note}");

    let mut c = two_account_core();
    let note = refused(&mut c, exercise("sim", 1.0, None));
    assert!(note.contains("sim#ALT"), "a bare venue of two accounts is ambiguous: {note}");
    let note = refused(&mut c, exercise("sim", 1.0, Some(alt())));
    assert!(note.contains("on sim REFUSED") && note.contains("no roster venue"), "{note}");
}

/// **The HOST hears an accepted combo's legs** (`CoreConfig::on_combo_legs`), once per combo and never for a
/// refused one. The core writes the legs into its engine with no `AdmitSymbol`, so this is the only way the
/// venue's reconcile client comes to read them (`vike-tradehub` commits each to the gatewayhub); before it, a
/// held leg was an `OrphanLocalPosition` on every pass once the bridge forgot the leg's fills. A leg on the
/// engine's OWN symbol is not named: the client reads that one already. The combo's id rides along, so the host can
/// tell a leg a resting combo still needs.
#[test]
fn an_accepted_combo_tells_the_host_its_legs_and_a_refused_one_tells_nothing() {
    type Heard = std::sync::Arc<std::sync::Mutex<Vec<(String, String, Vec<String>)>>>;
    let heard: Heard = Heard::default();
    let sink = std::sync::Arc::clone(&heard);
    let mut c = core_with(CoreConfig {
        on_combo_legs: Some(Box::new(
            move |route_key: &str, order_client_id: &str, legs: &[String]| {
                sink.lock().unwrap().push((
                    route_key.into(),
                    order_client_id.into(),
                    legs.to_vec(),
                ));
            },
        )),
        ..CoreConfig::default()
    });
    mark_combo_legs(&mut c, 100.0);
    let legs: Vec<String> = combo_spec(1.0).legs.into_iter().map(|l| l.symbol).collect();

    c.lower_combo(combo_spec(2.0), 7, false, EngineRoute::Payload);
    c.engine.trading_state = TradingState::Halted;
    c.lower_combo(combo_spec(2.0), 8, true, EngineRoute::Payload);
    c.engine.trading_state = TradingState::Active;
    assert!(heard.lock().unwrap().is_empty(), "a rejected and a denied combo name no leg");
    assert!(legs.iter().all(|leg| !c.engine.accepts_symbol(leg)), "and admitted none");

    let order_client_ids = c.lower_combo(combo_spec(2.0), 9, true, EngineRoute::Payload);
    assert_eq!(order_client_ids.len(), 1);
    assert!(legs.iter().all(|leg| c.engine.accepts_symbol(leg)), "the engine took the legs");
    let route_key = c.engine.route_key.clone();
    assert_eq!(
        *heard.lock().unwrap(),
        [(route_key.clone(), order_client_ids[0].clone(), legs.clone())]
    );

    // A second combo on the same legs is heard again: the host de-duplicates (the lease knows what it told).
    c.lower_combo(combo_spec(1.0), 10, true, EngineRoute::Payload);
    assert_eq!(heard.lock().unwrap().len(), 2);

    // A leg on the engine's own symbol is left out of what the host hears.
    for i in 0..=c.extra_engines.len() {
        let engine = c.engine_mut(i);
        engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
        engine.price_board.set_mark("sim", "BTCUSDT", 100.0, 0);
    }
    let mut own = combo_spec(1.0);
    own.legs[1].symbol = "BTCUSDT".into();
    let own_order_client_id = c.lower_combo(own, 11, true, EngineRoute::Payload);
    assert_eq!(own_order_client_id.len(), 1);
    assert_eq!(
        heard.lock().unwrap()[2],
        (route_key, own_order_client_id[0].clone(), vec![legs[0].clone()])
    );
}
