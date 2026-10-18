//! The contract value (decision 0170): the ceiling is sized with the addressed engine's published
//! value — kind and per-unit factor — by the SAME function the core's `RiskGate` calls.
use super::*;
use vike_model::{ContractValue, NotionalKind};

// ---------------------------------------------------------------------------------------------
// THE CONTRACT VALUE — the ceiling is sized by the core's own number
// ---------------------------------------------------------------------------------------------
//
// The desktop's order preview and the core's `RiskGate` size with `ContractValue::order_notional`;
// this edge — the only enforcing site of `policy.max_notional_per_order` — must be the same number.
// It was not: it counted `|qty| · |price|` first, then a scalar multiplier FLOORED at 1.0, so a
// linear contract with a sub-1 unit was judged larger at the door than at the gate, and an inverse
// contract (quantity already in dollars) was judged `qty · price · lot`. The rows below publish a
// roster (the node's engine blocks) and watch the SAME ceiling move with it.

/// [`submit`]'s limit order naming an ACCOUNT of the venue — the three wire states
/// (`None`, `DEFAULT`, a label) are what picks WHICH engine's value applies.
fn submit_on(account: Option<&str>, qty: f64, price: f64) -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: "c1".into(),
        venue: "hyperliquid".into(),
        symbol: "BTC".into(),
        side: 1,
        qty,
        order_type: "limit".into(),
        price: Some(price),
        trigger_price: None,
        reduce_only: false,
        account: account.map(str::to_string),
    })
}

/// **THE TABLE.** One ceiling (1000), many rosters: a `Submit` is refused exactly when the value's
/// `order_notional` is above it, the value being the one of the engine the command ADDRESSES for
/// the symbol it NAMES — and the read-only dry-run says the same words the executing gate does,
/// row for row.
///
/// The `at the cap` rows sit on products that are exact in binary floating point (`10 · 100`,
/// `1 · 10 · 100`), because the cap is `>` and an equality row is only a witness where the
/// arithmetic does not round.
///
/// There is NO FLOOR: a linear unit below 1 sizes the order SMALLER here, as it does at the core
/// and in the desktop (the rows that used to pin "floored to 1" now pin the true arithmetic). A
/// unit that is not finite is refused outright, because `NaN > cap` is false and an infinite one
/// would make `inf · 0` NaN.
///
/// ⚠ KILL PROOF 1: remove the value from `notional_reason` (size `(qty, price)` with the default
/// value whatever the roster says) and every `unit 100` and `inverse` row that expects a refusal,
/// plus the `ALT` row, flips to accepted. KILL PROOF 2: make the inverse arm of
/// `ContractValue::order_notional` multiply the price in and the inverse rows that expect an accept
/// flip to refused.
#[test]
fn the_ceiling_counts_the_contract_value_of_the_addressed_engine() {
    // (label, published roster, the account the wire names, qty, price, refused?)
    type Row = (&'static str, Vec<vike_exec::VenueBlock>, Option<&'static str>, f64, f64, bool);
    let hl = |m: f64| vec![block("hyperliquid", "BTC", m)];
    let inverse = |u: f64| vec![inverse_block("hyperliquid", "BTC", u)];
    let two_books =
        || vec![block("hyperliquid", "BTC", 1.0), block("hyperliquid#ALT", "BTC", 100.0)];
    // A block whose GRID lists nothing and whose engine-wide DEFAULT is `m` — the other place a
    // published value comes from (`VenueBlock::contract_value_of`'s fallback).
    let default_only = |m: f64| {
        vec![vike_exec::VenueBlock {
            venue: "hyperliquid".into(),
            route_key: "hyperliquid".into(),
            symbol: "BTC".into(),
            contract_value_default: ContractValue::linear(m),
            ..Default::default()
        }]
    };
    let rows: Vec<Row> = vec![
        // No roster, or the default value: today's arithmetic, bit for bit.
        ("no roster: exactly at the cap", vec![], None, 10.0, 100.0, false),
        ("no roster: just over", vec![], None, 10.0, 100.01, true),
        ("unit 1: exactly at the cap", hl(1.0), None, 10.0, 100.0, false),
        ("unit 1: just over", hl(1.0), None, 10.0, 100.01, true),
        // A linear unit above 1 makes the node refuse SOONER.
        ("unit 100: exactly at the cap (1 x 10 x 100)", hl(100.0), None, 1.0, 10.0, false),
        ("unit 100: just over (1 x 10.01 x 100)", hl(100.0), None, 1.0, 10.01, true),
        (
            "unit 100: 1 x 20 is far under the cap unmultiplied; 2000 is over",
            hl(100.0),
            None,
            1.0,
            20.0,
            true,
        ),
        ("unit 100: a negative qty is a magnitude", hl(100.0), None, -1.0, 20.0, true),
        ("unit 100: a negative price is a magnitude", hl(100.0), None, 1.0, -20.0, true),
        // A linear unit BELOW 1 sizes the order smaller, exactly as the core's gate does: the
        // floor that made the door stricter than the gate is deleted.
        (
            "unit 0.01: 1000 x 99 x 0.01 = 990 is under (the floor said 99000)",
            hl(0.01),
            None,
            1000.0,
            99.0,
            false,
        ),
        ("unit 0.01: 1000 x 101 x 0.01 = 1010 is over", hl(0.01), None, 1000.0, 101.0, true),
        (
            "a sub-1 engine-wide DEFAULT sizes smaller too (no grid row at all)",
            default_only(0.5),
            None,
            10.0,
            100.01,
            false,
        ),
        (
            "a sub-1 engine-wide DEFAULT: 10 x 250 x 0.5 = 1250 is over",
            default_only(0.5),
            None,
            10.0,
            250.0,
            true,
        ),
        // A unit that is not finite is bad venue data: refused, never compared.
        ("unit NaN: refused, not compared", hl(f64::NAN), None, 10.0, 100.0, true),
        // A zero or negative unit would size every order at nothing (or flip a sign): refused,
        // whatever the size, never passed.
        ("unit 0: refused, not passed at nothing", hl(0.0), None, 1.0, 1.0, true),
        ("unit -0.5: refused, not halved", hl(-0.5), None, 10.0, 100.0, true),
        ("unit -100: refused", hl(-100.0), None, 1.0, 1.0, true),
        ("inverse unit 0: refused", inverse(0.0), None, 1.0, 1.0, true),
        ("unit +inf: refused, not compared", hl(f64::INFINITY), None, 10.0, 100.0, true),
        ("unit -inf: refused, not compared", hl(f64::NEG_INFINITY), None, 10.0, 100.0, true),
        // THE INVERSE CONTRACT: the quantity is dollars, the price never enters.
        (
            "inverse: 900 USD at a 65,700 price is 900, under the cap (linear would say 59 million)",
            inverse(1.0),
            None,
            900.0,
            65_700.0,
            false,
        ),
        ("inverse: exactly at the cap (1000 USD)", inverse(1.0), None, 1000.0, 65_700.0, false),
        ("inverse: just over (1000.5 USD)", inverse(1.0), None, 1000.5, 65_700.0, true),
        (
            "inverse: the price does not enter (the same order at a 100x price is the same verdict)",
            inverse(1.0),
            None,
            900.0,
            6_570_000.0,
            false,
        ),
        ("inverse: a unit of 2 doubles the quote value", inverse(2.0), None, 600.0, 1.0, true),
        // WHICH engine's value: the one the command addresses, never a neighbour's.
        ("account ALT named: ALT's engine, unit 100", two_books(), Some("ALT"), 1.0, 20.0, true),
        ("no account named: the bare-venue engine, not ALT's", two_books(), None, 1.0, 20.0, false),
        (
            "DEFAULT named: the bare-venue engine, not ALT's",
            two_books(),
            Some("DEFAULT"),
            1.0,
            20.0,
            false,
        ),
        (
            "an account no engine carries sizes at the default",
            hl(100.0),
            Some("NOSUCH"),
            1.0,
            20.0,
            false,
        ),
        (
            "another venue's engine lends no value",
            vec![block("deribit", "BTC", 100.0)],
            None,
            1.0,
            20.0,
            false,
        ),
        (
            "a symbol the engine's grid does not list sizes at the default",
            vec![block("hyperliquid", "ETH", 100.0)],
            None,
            1.0,
            20.0,
            false,
        ),
    ];
    for (label, roster, account, qty, price, refused) in rows {
        let cmd = submit_on(account, qty, price);
        let mut l = limits(Some(1_000.0), 1e9);
        let vetted = l.vet(&cmd, &roster, &[]);
        assert_eq!(vetted.is_some(), refused, "{label}: `vet` answered {vetted:?}");
        let previewed = l.preview_vet(&cmd, &roster, &[]);
        assert_eq!(
            previewed, vetted,
            "{label}: the dry-run must give the executing gate's verdict, word for word"
        );
    }
}

/// An INVERSE contract needs no price: a market `Submit` on one is sized by its quantity, where a
/// priceless order on a linear contract stays the core's to judge.
#[test]
fn a_priceless_submit_is_sized_only_where_the_price_does_not_enter() {
    let l = limits(Some(1_000.0), 1e9);
    let inverse = [inverse_block("hyperliquid", "BTC", 1.0)];
    let linear = [block("hyperliquid", "BTC", 1.0)];
    assert!(l.preview_vet(&submit(900.0, None), &inverse, &[]).is_none());
    let msg = l
        .preview_vet(&submit(1100.0, None), &inverse, &[])
        .expect("1100 USD of an inverse contract is over a 1000 ceiling with no price at all");
    assert!(msg.contains("1100.00") && msg.contains("an inverse contract"), "{msg}");
    assert!(
        l.preview_vet(&submit(1e9, None), &linear, &[]).is_none(),
        "a priceless linear order has nothing to size at this edge"
    );
}

/// The refusal says WHY the notional is not `qty x price` only when the value is not the default —
/// so every sentence that existed before the node counted one reads byte-identically (tests,
/// runbooks and the Telegram channel key off those words).
#[test]
fn the_refusal_names_the_value_only_when_it_is_not_the_default() {
    let l = limits(Some(1_000.0), 1e9);
    let plain =
        "order notional 2000.00 exceeds the node's policy ceiling max_notional_per_order 1000.00";
    assert_eq!(l.preview_vet(&submit(5.0, Some(400.0)), &[], &[]).as_deref(), Some(plain));
    assert_eq!(
        l.preview_vet(&submit(5.0, Some(400.0)), &[block("hyperliquid", "BTC", 1.0)], &[])
            .as_deref(),
        Some(plain),
        "a linear unit of exactly 1 adds nothing to the sentence"
    );
    assert_eq!(
        l.preview_vet(&submit(1.0, Some(20.0)), &[block("hyperliquid", "BTC", 100.0)], &[])
            .as_deref(),
        Some(
            "order notional 2000.00 exceeds the node's policy ceiling max_notional_per_order \
             1000.00 (the notional counts this instrument's contract multiplier 100)"
        )
    );
    assert_eq!(
        l.preview_vet(&submit(2000.0, Some(1.0)), &[inverse_block("hyperliquid", "BTC", 1.0)], &[])
            .as_deref(),
        Some(
            "order notional 2000.00 exceeds the node's policy ceiling max_notional_per_order \
             1000.00 (an inverse contract: the notional is the quantity times 1, the price does \
             not enter)"
        )
    );
}

/// A bracket is three orders of one size on ONE engine, so every priced leg is sized with that
/// engine's value — and the refusal still names the leg and now says why the number is what it is.
/// A sub-1 engine sizes every leg smaller, like a `Submit`.
///
/// ⚠ KILL PROOF: put a floor back and the sub-1 bracket below is sized at 1 on every leg (1200 is
/// over) and is refused.
#[test]
fn a_bracket_is_sized_with_its_engines_value_on_every_priced_leg() {
    let l = limits(Some(1_000.0), 1e9);
    let roster = [block("hyperliquid", "BTC", 100.0)];
    // Legs 1 x {5, 4, 6} x 100 = 500, 400, 600 — all under.
    assert!(l.preview_vet(&bracket(1, 1.0, Some(5.0), 4.0, 6.0), &roster, &[]).is_none());
    // The take-profit alone is over: 1 x 12 x 100 = 1200.
    let msg = l
        .preview_vet(&bracket(1, 1.0, Some(5.0), 4.0, 12.0), &roster, &[])
        .expect("a take-profit leg of 1200 is over a 1000 ceiling");
    assert!(
        msg.contains("the bracket's take-profit leg")
            && msg.contains("1200.00")
            && msg.contains("contract multiplier 100"),
        "{msg}"
    );
    // Unmultiplied — no roster — the very same bracket passes: the value is the difference.
    assert!(l.preview_vet(&bracket(1, 1.0, Some(5.0), 4.0, 12.0), &[], &[]).is_none());
    // A sub-1 engine sizes smaller: 100 x {5, 4, 12} x 0.01 = 5, 4, 12 — all far under.
    let sub_one = [block("hyperliquid", "BTC", 0.01)];
    assert!(l.preview_vet(&bracket(1, 100.0, Some(5.0), 4.0, 12.0), &sub_one, &[]).is_none());
    // An inverse engine sizes every leg by the quantity: 1200 USD is over whatever the prices.
    let inverse = [inverse_block("hyperliquid", "BTC", 1.0)];
    let msg = l
        .preview_vet(&bracket(1, 1200.0, Some(5.0), 4.0, 6.0), &inverse, &[])
        .expect("1200 USD is over a 1000 ceiling on every leg");
    assert!(msg.contains("1200.00") && msg.contains("an inverse contract"), "{msg}");
}

/// **ONE DOOR, ONE NUMBER (the regression of the 10,000x disagreement).** The SAME rows go through
/// `ControlLimits::vet` and the core's `RiskGate` over the SAME value, and the two give the same
/// verdict on the per-order ceiling. A row where they differ is a second number for one order.
#[test]
fn the_control_door_and_the_risk_gate_give_one_verdict() {
    use vike_exec::{RiskContext, RiskGate};
    use vike_model::{OrderRequest, OrderType, RiskLimits};
    // (label, value, qty, price)
    let rows: Vec<(&str, ContractValue, f64, f64)> = vec![
        ("linear unit 1", ContractValue::linear(1.0), 10.0, 100.0),
        ("linear unit 1, over", ContractValue::linear(1.0), 10.0, 100.5),
        ("linear unit 100", ContractValue::linear(100.0), 1.0, 20.0),
        // THE SUB-1 LINEAR UNIT: the lot as a multiplier. The door used to floor it to 1 and the
        // gate did not, so one order was over at the door and under at the gate.
        ("linear unit 0.0001", ContractValue::linear(0.0001), 5_000.0, 2.0),
        ("linear unit 0.0001, larger", ContractValue::linear(0.0001), 5_000.0, 300_000.0),
        ("linear unit 0.01", ContractValue::linear(0.01), 1000.0, 99.0),
        // THE INVERSE CONTRACT.
        ("inverse unit 1, under", ContractValue::inverse(1.0), 900.0, 65_700.0),
        ("inverse unit 1, over", ContractValue::inverse(1.0), 1100.0, 65_700.0),
        ("inverse unit 1, at the cap", ContractValue::inverse(1.0), 1000.0, 65_700.0),
        ("inverse unit 2", ContractValue::inverse(2.0), 600.0, 1.0),
    ];
    for (label, value, qty, price) in rows {
        assert_eq!(value.kind == NotionalKind::Inverse, label.starts_with("inverse"), "{label}");
        let roster = [value_block("hyperliquid", "BTC", value)];
        let door =
            limits(Some(1_000.0), 1e9).vet(&submit(qty, Some(price)), &roster, &[]).is_some();
        let mut gate = RiskGate::new(RiskLimits {
            max_notional_per_order: Some(1_000.0),
            ..RiskLimits::new()
        });
        let order = OrderRequest {
            client_order_id: "c1".into(),
            venue: "hyperliquid".into(),
            symbol: "BTC".into(),
            side: 1,
            qty,
            order_type: OrderType::Limit,
            price: Some(price),
            ..Default::default()
        };
        let ctx =
            RiskContext { mark_price: price, contract_value: value, ..RiskContext::default() };
        let verdict = gate.check(&order, &ctx);
        let gate_refused = !verdict.ok && verdict.reason == "over-max-notional";
        assert_eq!(door, gate_refused, "{label}: the door says {door}, the gate says {verdict:?}");
    }
}
