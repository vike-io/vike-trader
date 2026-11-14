//! ADVERSARIAL tests: what the money-lane fold does with a HOSTILE venue payload.
//!
//! ## The threat model (this file is the pattern — copy it)
//!
//! TLS is verified end-to-end, so this is NOT a man-in-the-middle. The actor is **the venue
//! itself**: the endpoint we authenticated to and trust by construction, returning well-formed
//! JSON with hostile CONTENTS — the one participant the design has no defence against, because
//! every other input is ours or checked. The question is not "right numbers from right numbers"
//! but "what does the fold do with a number no venue should send".
//!
//! ## Conventions for adding to this pattern
//!
//! - Name what the venue DID and what must not happen:
//!   `a_nan_<field>_is_refused_and_<state>_stays_finite`.
//! - Assert on STATE, not just on the counter — a counter can move while the poison still lands.
//! - Cover the whole non-finite family (`NAN`, `INFINITY`, `NEG_INFINITY`), not just NaN: several
//!   real guards in this tree (`px > 0.0`, `qty != 0.0`) screen two of the three and pass `+inf`.
//! - Pin the ADMISSION side too — the guard must not become a reason to drop legitimate data.
//!
//! ## Why `"NaN"` is reachable at all
//!
//! Venue numbers arrive as decimal STRINGS, decoded in one place,
//! `vike_bridge_core::json::json_number`, whose `Value::String` arm calls `f64::from_str` — which
//! ACCEPTS `"NaN"`, `"inf"` and `"-inf"` with no error. `vike_model::finite`'s module doc is the
//! authority.

use vike_execution::testing::RecordingClient;
use vike_execution::{Engine, EventBus, EventBusFold, EventBusOutbox};
use vike_model::OrderRequest;
use vike_model::events::{
    AccountState, Event, FillEvent, FundingEvent, OrderAccepted, OrderFilled, PositionLiquidated,
    PositionSide,
};

use vike_execution::testing::EngineBuilder;

const NON_FINITE: [f64; 3] = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY];

fn engine() -> Engine<RecordingClient> {
    EngineBuilder { venue: "binance".into(), ..Default::default() }.build()
}

fn fill(trade_id: &'static str, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        trade_id: trade_id.into(),
        order_client_id: "c1".to_string(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_price: px,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: Default::default(),
        timestamp: 0,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

/// Position size + avg entry + realized PnL + balance + equity — everything a poisoned fill reaches.
fn ledger(engine: &Engine<RecordingClient>) -> Vec<f64> {
    let key = (ustr::Ustr::from("binance"), ustr::Ustr::from("BTCUSDT"), PositionSide::Both);
    let p = engine.account.positions.get(&key).copied().unwrap_or_default();
    vec![
        p.size,
        p.average_price,
        engine.account.realized_pnl,
        engine.account.balance,
        engine.account.equity_all(0.0),
    ]
}

fn assert_all_finite(vals: &[f64], what: &str) {
    for (i, v) in vals.iter().enumerate() {
        assert!(v.is_finite(), "{what}: ledger slot {i} is {v} — the poison LANDED");
    }
}

// --- Bare Event::Fill — the money lane ---

/// THE headline case. One `"L": "NaN"` off the wire must not reach `compute_fill`: a NaN `average_price`
/// is STORED on the position and every later fill recomputes from it, so the poisoning is permanent
/// for the session and no reconcile can see it (every comparison against NaN is false).
#[test]
fn a_non_finite_fill_price_is_refused_and_the_ledger_stays_finite() {
    for bad in NON_FINITE {
        let mut engine_under_test = engine();
        let mut bus = EventBus::new();
        // A good fill first, so there is REAL state the poison could destroy.
        bus.publish(Event::Fill(fill("t-good", 2.0, 100.0)), &mut engine_under_test);
        let before = ledger(&engine_under_test);

        let verdict = bus.publish(Event::Fill(fill("t-evil", 1.0, bad)), &mut engine_under_test);

        assert_eq!(verdict, EventBusFold::Dropped, "a {bad} price must be refused");
        assert_eq!(engine_under_test.dropped_nonfinite, 1, "and counted ({bad})");
        assert_eq!(
            ledger(&engine_under_test),
            before,
            "state must be UNCHANGED by the refused fill ({bad})"
        );
        assert_all_finite(&ledger(&engine_under_test), &format!("price {bad}"));
    }
}

#[test]
fn a_non_finite_fill_quantity_is_refused_and_the_ledger_stays_finite() {
    for bad in NON_FINITE {
        let mut engine_under_test = engine();
        let mut bus = EventBus::new();
        bus.publish(Event::Fill(fill("t-good", 2.0, 100.0)), &mut engine_under_test);
        let before = ledger(&engine_under_test);

        assert_eq!(
            bus.publish(Event::Fill(fill("t-evil", bad, 100.0)), &mut engine_under_test),
            EventBusFold::Dropped,
            "a {bad} qty must be refused"
        );
        assert_eq!(ledger(&engine_under_test), before, "state unchanged ({bad})");
        assert_all_finite(&ledger(&engine_under_test), &format!("qty {bad}"));
    }
}

/// `commission` is netted straight into `balance` — a separate poisoning path from the position
/// fold, and one that survives even a flat position.
#[test]
fn a_non_finite_commission_is_refused_and_balance_stays_finite() {
    for bad in NON_FINITE {
        let mut engine_under_test = engine();
        let mut bus = EventBus::new();
        let mut f = fill("t-evil", 1.0, 100.0);
        f.commission = bad;

        assert_eq!(
            bus.publish(Event::Fill(f), &mut engine_under_test),
            EventBusFold::Dropped,
            "{bad}"
        );
        assert!(
            engine_under_test.account.balance.is_finite(),
            "balance poisoned by commission {bad}"
        );
        assert!(
            engine_under_test.account.fees_paid.is_finite(),
            "fees_paid poisoned by commission {bad}"
        );
        assert_all_finite(&ledger(&engine_under_test), &format!("commission {bad}"));
    }
}

/// The refusal must not consume the fill's `trade_id`, or the venue's well-formed RETRANSMISSION of
/// that fill would be swallowed by the replay dedup and lost for good.
#[test]
fn a_refused_fill_does_not_burn_its_trade_id_for_the_good_retransmission() {
    let mut engine_under_test = engine();
    let mut bus = EventBus::new();

    assert_eq!(
        bus.publish(Event::Fill(fill("t7", 1.0, f64::NAN)), &mut engine_under_test),
        EventBusFold::Dropped
    );
    // Same trade_id, now well-formed — the venue re-sent it correctly.
    assert_eq!(
        bus.publish(Event::Fill(fill("t7", 1.0, 100.0)), &mut engine_under_test),
        EventBusFold::Applied
    );

    assert_eq!(ledger(&engine_under_test)[0], 1.0, "the good retransmission MUST fold");
    assert_eq!(engine_under_test.dropped_nonfinite, 1, "exactly one refusal");
}

/// The ADMISSION side: ordinary fills, including awkward-but-legal ones, still fold.
#[test]
fn ordinary_fills_are_untouched_by_the_guard() {
    let mut engine_under_test = engine();
    let mut bus = EventBus::new();
    assert_eq!(
        bus.publish(Event::Fill(fill("t1", 2.0, 100.0)), &mut engine_under_test),
        EventBusFold::Applied
    );
    // A zero-price fill and a maker REBATE (negative commission) are both finite and both legal.
    let mut rebate = fill("t2", 1.0, 0.0);
    rebate.commission = -0.5;
    assert_eq!(bus.publish(Event::Fill(rebate), &mut engine_under_test), EventBusFold::Applied);

    assert_eq!(engine_under_test.dropped_nonfinite, 0, "nothing legitimate may be refused");
    assert_eq!(ledger(&engine_under_test)[0], 3.0, "both fills folded");
}

// --- mark_price — guarded at the single writer, NOT by discarding the fill ---

/// A malformed DECORATIVE field must not discard a MONEY event: the fill folds, and only the mark
/// write is refused. ⚠ `+inf` is the interesting one — the pre-existing `mp > 0.0` screen passes it.
#[test]
fn a_non_finite_mark_price_is_dropped_but_the_fill_still_folds() {
    for bad in NON_FINITE {
        let mut engine_under_test = engine();
        let mut bus = EventBus::new();
        let mut f = fill("t-mark", 1.0, 100.0);
        f.mark_price = Some(bad);

        assert_eq!(
            bus.publish(Event::Fill(f), &mut engine_under_test),
            EventBusFold::Applied,
            "the FILL is money and must fold despite a bad mark ({bad})"
        );
        assert_eq!(ledger(&engine_under_test)[0], 1.0, "position folded ({bad})");
        assert_eq!(
            engine_under_test.account.mark_of("binance", "BTCUSDT"),
            None,
            "no mark written ({bad})"
        );
        assert_all_finite(&ledger(&engine_under_test), &format!("mark {bad}"));
    }
}

/// `set_mark_from` is THE only writer of the mark slot, so guarding it covers every producer at
/// once. Pinned directly, because equity — not the position — is what a poisoned mark destroys.
#[test]
fn set_mark_from_refuses_every_non_finite_price_and_keeps_equity_finite() {
    for bad in NON_FINITE {
        let mut engine_under_test = engine();
        let mut bus = EventBus::new();
        bus.publish(Event::Fill(fill("t1", 1.0, 100.0)), &mut engine_under_test);
        // a good mark first, so we can prove the bad one does not even OVERWRITE it
        assert!(engine_under_test.account.set_mark_from(
            "binance",
            "BTCUSDT",
            101.0,
            vike_execution::MarkSource::VenueMark,
            1
        ));

        let landed = engine_under_test.account.set_mark_from(
            "binance",
            "BTCUSDT",
            bad,
            vike_execution::MarkSource::VenueMark,
            2,
        );

        assert!(!landed, "a {bad} mark must not land");
        assert_eq!(
            engine_under_test.account.mark_of("binance", "BTCUSDT"),
            Some(101.0),
            "good mark survives {bad}"
        );
        assert!(
            engine_under_test.account.equity_all(0.0).is_finite(),
            "equity poisoned by mark {bad}"
        );
        assert!(
            engine_under_test
                .account
                .unrealized_pnl("binance", "BTCUSDT", PositionSide::Both)
                .is_finite(),
            "uPnL poisoned {bad}"
        );
    }
}

// --- The other three money-lane events ---

#[test]
fn a_non_finite_funding_amount_is_refused_and_balance_stays_finite() {
    for bad in NON_FINITE {
        let mut engine_under_test = engine();
        let mut bus = EventBus::new();
        let event = Event::Funding(FundingEvent {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            funding_rate: 0.0001,
            amount: bad,
            mark_price: None,
            timestamp: 0,
            route_key: None,
            currency: None,
        });

        assert_eq!(bus.publish(event, &mut engine_under_test), EventBusFold::Dropped, "{bad}");
        assert!(engine_under_test.account.balance.is_finite(), "balance poisoned by funding {bad}");
        assert!(engine_under_test.account.funding_paid.is_finite(), "funding_paid poisoned {bad}");
    }
}

/// The most damaging of the four: an AccountState assignment OVERWRITES `balance` outright and
/// flips the account to `Authoritative`, so a poisoned frame does not merely add — it replaces.
#[test]
fn a_non_finite_account_state_balance_is_refused_and_does_not_overwrite() {
    for bad in NON_FINITE {
        let mut engine_under_test = engine();
        let mut bus = EventBus::new();
        let good = Event::AccountState(AccountState {
            venue: "binance".into(),
            balances: vec![("USDT".to_string(), 5_000.0)],
            timestamp: 0,
            route_key: None,
        });
        assert_eq!(bus.publish(good, &mut engine_under_test), EventBusFold::Applied);

        let evil = Event::AccountState(AccountState {
            venue: "binance".into(),
            // the SECOND entry — a "check the first balance" shortcut would miss it, and the
            // `py_sum` selection path folds every one of them
            balances: vec![("USDT".to_string(), 5_000.0), ("BTC".to_string(), bad)],
            timestamp: 1,
            route_key: None,
        });
        assert_eq!(bus.publish(evil, &mut engine_under_test), EventBusFold::Dropped, "{bad}");

        assert_eq!(
            engine_under_test.account.balance, 5_000.0,
            "the authoritative balance must survive {bad}"
        );
        assert!(engine_under_test.account.equity_all(0.0).is_finite(), "equity poisoned {bad}");
    }
}

#[test]
fn a_non_finite_liquidation_price_is_refused_and_the_position_survives() {
    for bad in NON_FINITE {
        let mut engine_under_test = engine();
        let mut bus = EventBus::new();
        bus.publish(Event::Fill(fill("t1", 5.0, 100.0)), &mut engine_under_test);
        let before = ledger(&engine_under_test);

        let event = Event::PositionLiquidated(PositionLiquidated {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            qty: 5.0,
            liquidation_price: bad,
            fee: 0.0,
            timestamp: 0,
            trade_id: "liq1".into(),
            route_key: None,
            fee_currency: None,
        });
        assert_eq!(bus.publish(event, &mut engine_under_test), EventBusFold::Dropped, "{bad}");

        assert_eq!(
            ledger(&engine_under_test),
            before,
            "state unchanged by the refused liquidation ({bad})"
        );
        assert_all_finite(&ledger(&engine_under_test), &format!("liquidation_price {bad}"));
    }
}

// --- The fill WRAP — the order FSM's own copy of the numbers ---

/// A wrap carries its OWN embedded `FillEvent`, folded into `filled_qty`/`average_fill_price` by
/// `accumulate_fill`. That is a second, independent poisoning path: a NaN `average_fill_price` makes every
/// later `remaining_qty` comparison false, so the order can never complete.
#[test]
fn a_non_finite_wrap_fill_is_refused_and_the_order_stays_sane() {
    for bad in NON_FINITE {
        let mut engine_under_test = engine();
        let mut bus = EventBus::new();
        let mut outbox = EventBusOutbox::default();
        let req = OrderRequest {
            order_client_id: "c1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 5.0,
            order_type: vike_model::OrderType::Limit,
            price: Some(100.0),
            timestamp: 1,
            ..Default::default()
        };
        engine_under_test.submit_order(&req, 1, &mut outbox);
        bus.publish(
            Event::OrderAccepted(OrderAccepted {
                order_client_id: "c1".into(),
                venue_order_id: None,
                timestamp: 1,
            }),
            &mut engine_under_test,
        );

        let wrap = Event::OrderFilled(OrderFilled {
            order_client_id: "c1".into(),
            fill: fill("w1", 5.0, bad),
            timestamp: 2,
        });
        assert_eq!(bus.publish(wrap, &mut engine_under_test), EventBusFold::Dropped, "{bad}");

        // ⚠ ASSERT ON THE COUNTER, not only on `EventBusFold::Dropped`: the FSM ALSO refuses this wrap for
        // its own arithmetic reasons, so `Dropped` alone passes with the guard reverted.
        // `dropped_nonfinite` moves nowhere else, so it is the only attributable signal here.
        assert_eq!(
            engine_under_test.dropped_nonfinite, 1,
            "the finiteness guard is what refused it ({bad})"
        );
        let mo = engine_under_test.registry.get("c1").expect("order still tracked");
        assert!(
            mo.average_fill_price.is_finite(),
            "average_fill_price poisoned by the wrap ({bad})"
        );
        assert!(mo.filled_qty.is_finite(), "filled_qty poisoned by the wrap ({bad})");
    }
}
