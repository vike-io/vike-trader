//! ADVERSARIAL tests: what the money-lane fold does with a HOSTILE venue payload.
//!
//! ## The threat model, stated once (this file is the pattern — copy it)
//!
//! TLS is verified end-to-end, so this is NOT a man-in-the-middle. The actor here is **the venue
//! itself**: the endpoint we authenticated to, connected to, and trust by construction, returning
//! well-formed JSON with hostile CONTENTS. That is the one participant the design has no defence
//! against, because every other input is either ours or checked.
//!
//! Until this file existed there was not ONE adversarial test in `vike-exec`, `vike-core` or
//! `crates/bridges` — no huge qty, no wrong symbol, no negative price, no NaN. Every `f64::NAN` in
//! the tree is a test SENTINEL meaning "this field was absent", never an INPUT. So the tests all
//! answered "does the fold compute the right number from right numbers", and none answered "what
//! does the fold do with a number no venue should send".
//!
//! ## Conventions for adding to this pattern
//!
//! - Name what the venue DID and what must not happen: `a_nan_<field>_is_refused_and_<state>_stays_finite`.
//! - Assert on STATE, not just on the counter — a counter can move while the poison still lands.
//! - Cover the whole non-finite family (`NAN`, `INFINITY`, `NEG_INFINITY`), not just NaN: several
//!   real guards in this tree (`px > 0.0`, `qty != 0.0`) screen two of the three and pass `+inf`.
//! - Pin the ADMISSION side too — the guard must not become a reason to drop legitimate data.
//!
//! ## Why `"NaN"` is reachable at all
//!
//! Venue numbers arrive as decimal STRINGS and are decoded in one place,
//! `vike_bridge_core::json::json_num`, whose `Value::String(s) => s.parse::<f64>().ok()` arm calls
//! `f64::from_str` — which ACCEPTS `"NaN"`, `"inf"` and `"-inf"`. No parse error, no type error;
//! the value simply arrives in a typed field. `vike_model::finite`'s module doc is the authority.

use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventBus, ExecutionEngine, Fold, Outbox, RiskGate, RiskLimits,
};
use vike_model::OrderRequest;
use vike_model::events::{
    AccountState, Event, FillEvent, FundingEvent, OrderAccepted, OrderFilled, PositionLiquidated,
    PositionSide,
};

const NON_FINITE: [f64; 3] = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY];

fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    )
}

fn fill(trade_id: &'static str, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        trade_id: trade_id.into(),
        client_order_id: "c1".to_string(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: Default::default(),
        ts: 0,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

/// Position size + avg entry + realized PnL + balance + equity — everything a poisoned fill reaches.
fn ledger(eng: &ExecutionEngine<RecordingClient>) -> Vec<f64> {
    let key = (ustr::Ustr::from("binance"), ustr::Ustr::from("BTCUSDT"), PositionSide::Both);
    let p = eng.account.positions.get(&key).copied().unwrap_or_default();
    vec![
        p.size,
        p.avg_px,
        eng.account.realized_pnl,
        eng.account.balance,
        eng.account.equity_all(0.0),
    ]
}

fn assert_all_finite(vals: &[f64], what: &str) {
    for (i, v) in vals.iter().enumerate() {
        assert!(v.is_finite(), "{what}: ledger slot {i} is {v} — the poison LANDED");
    }
}

// ---------------------------------------------------------------------------------------------
// Bare Event::Fill — the money lane
// ---------------------------------------------------------------------------------------------

/// THE headline case. One `"L": "NaN"` off the wire must not reach `compute_fill`, because a NaN
/// `avg_px` is STORED on the position and every later fill recomputes from it: the poisoning is
/// permanent for the session and no reconcile can see it (every comparison against NaN is false).
#[test]
fn a_non_finite_fill_price_is_refused_and_the_ledger_stays_finite() {
    for bad in NON_FINITE {
        let mut eng = engine();
        let mut bus = EventBus::new();
        // A good fill first, so there is REAL state the poison could destroy.
        bus.publish(Event::Fill(fill("t-good", 2.0, 100.0)), &mut eng);
        let before = ledger(&eng);

        let verdict = bus.publish(Event::Fill(fill("t-evil", 1.0, bad)), &mut eng);

        assert_eq!(verdict, Fold::Dropped, "a {bad} price must be refused");
        assert_eq!(eng.dropped_nonfinite, 1, "and counted ({bad})");
        assert_eq!(ledger(&eng), before, "state must be UNCHANGED by the refused fill ({bad})");
        assert_all_finite(&ledger(&eng), &format!("price {bad}"));
    }
}

#[test]
fn a_non_finite_fill_qty_is_refused_and_the_ledger_stays_finite() {
    for bad in NON_FINITE {
        let mut eng = engine();
        let mut bus = EventBus::new();
        bus.publish(Event::Fill(fill("t-good", 2.0, 100.0)), &mut eng);
        let before = ledger(&eng);

        assert_eq!(
            bus.publish(Event::Fill(fill("t-evil", bad, 100.0)), &mut eng),
            Fold::Dropped,
            "a {bad} qty must be refused"
        );
        assert_eq!(ledger(&eng), before, "state unchanged ({bad})");
        assert_all_finite(&ledger(&eng), &format!("qty {bad}"));
    }
}

/// `commission` is netted straight into `balance` — a separate poisoning path from the position
/// fold, and one that survives even a flat position.
#[test]
fn a_non_finite_commission_is_refused_and_balance_stays_finite() {
    for bad in NON_FINITE {
        let mut eng = engine();
        let mut bus = EventBus::new();
        let mut f = fill("t-evil", 1.0, 100.0);
        f.commission = bad;

        assert_eq!(bus.publish(Event::Fill(f), &mut eng), Fold::Dropped, "{bad}");
        assert!(eng.account.balance.is_finite(), "balance poisoned by commission {bad}");
        assert!(eng.account.fees_paid.is_finite(), "fees_paid poisoned by commission {bad}");
        assert_all_finite(&ledger(&eng), &format!("commission {bad}"));
    }
}

/// The refusal must not consume the fill's `trade_id`: if it did, the venue's own well-formed
/// RETRANSMISSION of that same fill would be swallowed by the replay dedup and the fill lost for
/// good. Rejecting must cost exactly the malformed frame — nothing more.
#[test]
fn a_refused_fill_does_not_burn_its_trade_id_for_the_good_retransmission() {
    let mut eng = engine();
    let mut bus = EventBus::new();

    assert_eq!(bus.publish(Event::Fill(fill("t7", 1.0, f64::NAN)), &mut eng), Fold::Dropped);
    // Same trade_id, now well-formed — the venue re-sent it correctly.
    assert_eq!(bus.publish(Event::Fill(fill("t7", 1.0, 100.0)), &mut eng), Fold::Applied);

    assert_eq!(ledger(&eng)[0], 1.0, "the good retransmission MUST fold");
    assert_eq!(eng.dropped_nonfinite, 1, "exactly one refusal");
}

/// The guard must not become a new way to lose money: ordinary fills, including the awkward-but-
/// legal ones, keep folding exactly as before.
#[test]
fn ordinary_fills_are_untouched_by_the_guard() {
    let mut eng = engine();
    let mut bus = EventBus::new();
    assert_eq!(bus.publish(Event::Fill(fill("t1", 2.0, 100.0)), &mut eng), Fold::Applied);
    // A zero-price fill and a maker REBATE (negative commission) are both finite and both legal.
    let mut rebate = fill("t2", 1.0, 0.0);
    rebate.commission = -0.5;
    assert_eq!(bus.publish(Event::Fill(rebate), &mut eng), Fold::Applied);

    assert_eq!(eng.dropped_nonfinite, 0, "nothing legitimate may be refused");
    assert_eq!(ledger(&eng)[0], 3.0, "both fills folded");
}

// ---------------------------------------------------------------------------------------------
// mark_price — guarded at the single writer, NOT by discarding the fill
// ---------------------------------------------------------------------------------------------

/// A malformed DECORATIVE field must not discard a MONEY event: the fill folds, and only the mark
/// write is refused. ⚠ `+inf` is the interesting one — the pre-existing `mp > 0.0` screen passes it.
#[test]
fn a_non_finite_mark_price_is_dropped_but_the_fill_still_folds() {
    for bad in NON_FINITE {
        let mut eng = engine();
        let mut bus = EventBus::new();
        let mut f = fill("t-mark", 1.0, 100.0);
        f.mark_price = Some(bad);

        assert_eq!(
            bus.publish(Event::Fill(f), &mut eng),
            Fold::Applied,
            "the FILL is money and must fold despite a bad mark ({bad})"
        );
        assert_eq!(ledger(&eng)[0], 1.0, "position folded ({bad})");
        assert_eq!(eng.account.mark_of("binance", "BTCUSDT"), None, "no mark written ({bad})");
        assert_all_finite(&ledger(&eng), &format!("mark {bad}"));
    }
}

/// `set_mark_from` is THE only writer of the mark slot, so guarding it covers every producer at
/// once. Pinned directly, because equity — not the position — is what a poisoned mark destroys.
#[test]
fn set_mark_from_refuses_every_non_finite_price_and_keeps_equity_finite() {
    for bad in NON_FINITE {
        let mut eng = engine();
        let mut bus = EventBus::new();
        bus.publish(Event::Fill(fill("t1", 1.0, 100.0)), &mut eng);
        // a good mark first, so we can prove the bad one does not even OVERWRITE it
        assert!(eng.account.set_mark_from(
            "binance",
            "BTCUSDT",
            101.0,
            vike_exec::MarkSource::VenueMark,
            1
        ));

        let landed = eng.account.set_mark_from(
            "binance",
            "BTCUSDT",
            bad,
            vike_exec::MarkSource::VenueMark,
            2,
        );

        assert!(!landed, "a {bad} mark must not land");
        assert_eq!(
            eng.account.mark_of("binance", "BTCUSDT"),
            Some(101.0),
            "good mark survives {bad}"
        );
        assert!(eng.account.equity_all(0.0).is_finite(), "equity poisoned by mark {bad}");
        assert!(
            eng.account.unrealized_pnl("binance", "BTCUSDT", "BOTH").is_finite(),
            "uPnL poisoned {bad}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The other three money-lane events
// ---------------------------------------------------------------------------------------------

#[test]
fn a_non_finite_funding_amount_is_refused_and_balance_stays_finite() {
    for bad in NON_FINITE {
        let mut eng = engine();
        let mut bus = EventBus::new();
        let ev = Event::Funding(FundingEvent {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            funding_rate: 0.0001,
            amount: bad,
            mark_price: None,
            ts: 0,
            route_key: None,
        });

        assert_eq!(bus.publish(ev, &mut eng), Fold::Dropped, "{bad}");
        assert!(eng.account.balance.is_finite(), "balance poisoned by funding {bad}");
        assert!(eng.account.funding_paid.is_finite(), "funding_paid poisoned {bad}");
    }
}

/// The most damaging of the four: an AccountState assignment OVERWRITES `balance` outright and
/// flips the account to `Authoritative`, so a poisoned frame does not merely add — it replaces.
#[test]
fn a_non_finite_account_state_balance_is_refused_and_does_not_overwrite() {
    for bad in NON_FINITE {
        let mut eng = engine();
        let mut bus = EventBus::new();
        let good = Event::AccountState(AccountState {
            venue: "binance".into(),
            balances: vec![("USDT".to_string(), 5_000.0)],
            ts: 0,
            route_key: None,
        });
        assert_eq!(bus.publish(good, &mut eng), Fold::Applied);

        let evil = Event::AccountState(AccountState {
            venue: "binance".into(),
            // the SECOND entry — a "check the first balance" shortcut would miss it, and the
            // `py_sum` selection path folds every one of them
            balances: vec![("USDT".to_string(), 5_000.0), ("BTC".to_string(), bad)],
            ts: 1,
            route_key: None,
        });
        assert_eq!(bus.publish(evil, &mut eng), Fold::Dropped, "{bad}");

        assert_eq!(eng.account.balance, 5_000.0, "the authoritative balance must survive {bad}");
        assert!(eng.account.equity_all(0.0).is_finite(), "equity poisoned {bad}");
    }
}

#[test]
fn a_non_finite_liquidation_price_is_refused_and_the_position_survives() {
    for bad in NON_FINITE {
        let mut eng = engine();
        let mut bus = EventBus::new();
        bus.publish(Event::Fill(fill("t1", 5.0, 100.0)), &mut eng);
        let before = ledger(&eng);

        let ev = Event::PositionLiquidated(PositionLiquidated {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            qty: 5.0,
            liq_price: bad,
            fee: 0.0,
            ts: 0,
            trade_id: "liq1".into(),
            route_key: None,
        });
        assert_eq!(bus.publish(ev, &mut eng), Fold::Dropped, "{bad}");

        assert_eq!(ledger(&eng), before, "state unchanged by the refused liquidation ({bad})");
        assert_all_finite(&ledger(&eng), &format!("liq_price {bad}"));
    }
}

// ---------------------------------------------------------------------------------------------
// The fill WRAP — the order FSM's own copy of the numbers
// ---------------------------------------------------------------------------------------------

/// A wrap carries its OWN embedded `FillEvent`, folded into `filled_qty`/`avg_fill_px` by
/// `accumulate_fill`. That is a second, independent poisoning path: a NaN `avg_fill_px` makes every
/// later `remaining_qty` comparison false, so the order can never complete.
#[test]
fn a_non_finite_wrap_fill_is_refused_and_the_order_stays_sane() {
    for bad in NON_FINITE {
        let mut eng = engine();
        let mut bus = EventBus::new();
        let mut outbox = Outbox::default();
        let req = OrderRequest {
            client_order_id: "c1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 5.0,
            order_type: "limit".into(),
            price: Some(100.0),
            ts: 1,
            ..Default::default()
        };
        eng.submit_order(&req, 1, &mut outbox);
        bus.publish(
            Event::OrderAccepted(OrderAccepted {
                client_order_id: "c1".into(),
                venue_order_id: None,
                ts: 1,
            }),
            &mut eng,
        );

        let wrap = Event::OrderFilled(OrderFilled {
            client_order_id: "c1".into(),
            fill: fill("w1", 5.0, bad),
            ts: 2,
        });
        assert_eq!(bus.publish(wrap, &mut eng), Fold::Dropped, "{bad}");

        // ⚠ ASSERT ON THE COUNTER, not only on `Fold::Dropped`. The FSM ALSO refuses this wrap for
        // its own arithmetic reasons, so a `Dropped` verdict alone passes with the guard reverted —
        // it proves the event did not land, not that THIS guard is what stopped it.
        // `dropped_nonfinite` moves nowhere else, so it is the only attributable signal here.
        assert_eq!(eng.dropped_nonfinite, 1, "the finiteness guard is what refused it ({bad})");
        let mo = eng.registry.get("c1").expect("order still tracked");
        assert!(mo.avg_fill_px.is_finite(), "avg_fill_px poisoned by the wrap ({bad})");
        assert!(mo.filled_qty.is_finite(), "filled_qty poisoned by the wrap ({bad})");
    }
}
