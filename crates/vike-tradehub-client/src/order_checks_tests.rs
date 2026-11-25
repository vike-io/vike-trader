use super::*;
use std::assert_matches;

/// The request a ticket of `order_type` builds: the fields the desktop's ticket sets, the rest default.
fn order(
    venue: &str,
    symbol: &str,
    order_type: OrderType,
    qty: f64,
    price: Option<f64>,
) -> OrderRequest {
    OrderRequest {
        order_client_id: "c".into(),
        venue: venue.into(),
        symbol: symbol.into(),
        side: 1,
        qty,
        order_type,
        price,
        ..Default::default()
    }
}

fn limit(venue: &str, symbol: &str, qty: f64, price: f64) -> OrderRequest {
    order(venue, symbol, OrderType::Limit, qty, Some(price))
}

fn market(qty: f64) -> OrderRequest {
    order("binance", "BTCUSDT", OrderType::Market, qty, None)
}

/// A binance BTCUSDT limit BUY of `qty` at 30 000.
fn btc_limit_at_30k(qty: f64) -> OrderRequest {
    limit("binance", "BTCUSDT", qty, 30_000.0)
}

#[test]
fn validate_accepts_good_limit() {
    assert_eq!(validate(&btc_limit_at_30k(0.01), &OrderLimits::default()), Ok(()));
}

#[test]
fn validate_accepts_good_market() {
    // Market has no price → notional not checked, no MissingLimitPrice.
    assert_eq!(validate(&market(0.01), &OrderLimits::default()), Ok(()));
}

#[test]
fn validate_rejects_below_min() {
    let limits = OrderLimits { min_qty: 0.1, ..OrderLimits::default() };
    assert_matches!(
        validate(&btc_limit_at_30k(0.05), &limits),
        Err(OrderLimitsError::QuantityBelowMin { .. })
    );
}

#[test]
fn validate_rejects_non_positive_quantity_even_with_permissive_min() {
    assert_matches!(
        validate(&market(0.0), &OrderLimits::default()),
        Err(OrderLimitsError::QuantityBelowMin { .. })
    );
    assert_matches!(
        validate(&market(-1.0), &OrderLimits::default()),
        Err(OrderLimitsError::QuantityBelowMin { .. })
    );
}

#[test]
fn validate_rejects_above_max() {
    let limits = OrderLimits { max_qty: 1.0, ..OrderLimits::default() };
    assert_matches!(
        validate(&market(5.0), &limits),
        Err(OrderLimitsError::QuantityAboveMax { .. })
    );
}

#[test]
fn validate_rejects_non_finite() {
    assert_eq!(
        validate(&market(f64::NAN), &OrderLimits::default()),
        Err(OrderLimitsError::NonFiniteQuantityOrPrice)
    );
    assert_eq!(
        validate(&limit("binance", "BTCUSDT", 1.0, f64::INFINITY), &OrderLimits::default()),
        Err(OrderLimitsError::NonFiniteQuantityOrPrice)
    );
}

#[test]
fn validate_rejects_missing_limit_price() {
    let req = order("binance", "BTCUSDT", OrderType::Limit, 1.0, None);
    assert_eq!(validate(&req, &OrderLimits::default()), Err(OrderLimitsError::MissingLimitPrice));
}

#[test]
fn validate_rejects_over_notional() {
    let limits = OrderLimits { max_notional: 100.0, ..OrderLimits::default() };
    // 0.01 * 30_000 = 300 > 100.
    assert_matches!(
        validate(&btc_limit_at_30k(0.01), &limits),
        Err(OrderLimitsError::NotionalAboveMax { .. })
    );
}

/// COMPAT PIN: `multiplier = 1.0` — the default, i.e. every spot/linear instrument —
/// reproduces the pre-fix `qty * price` verdict exactly, on both sides of the cap.
/// `x * 1.0` is an IEEE-754 no-op, so this is bit-identical, not merely close.
#[test]
fn multiplier_one_is_byte_identical_to_the_bare_quantity_times_price() {
    for (qty, px, cap) in [
        (0.01, 30_000.0, 100.0), // 300 > 100 → reject
        (0.01, 30_000.0, 500.0), // 300 < 500 → accept
        (0.01, 30_000.0, 300.0), // exactly at the cap → accept (strict >)
        (3.0, 0.045, 1.0),       // the options-ticket shape
        (1.0, f64::MAX, 1.0),    // saturating
    ] {
        let limits = OrderLimits { max_notional: cap, ..OrderLimits::default() };
        let req = limit("binance", "S", qty, px);
        let bare = qty * px;
        let expected = if bare > cap {
            Err(OrderLimitsError::NotionalAboveMax { notional: bare, max: cap })
        } else {
            Ok(())
        };
        assert_eq!(validate(&req, &limits), expected, "qty={qty} px={px} cap={cap}");
        // the explicit-1.0 entry point agrees with the defaulting one
        assert_eq!(validate_with_value(&req, &limits, ContractValue::default()), expected);
    }
}

/// THE BUG: an option with a 100x contract multiplier. `qty × price` measures 300 and slips
/// under a 1,000 cap, but the order's real notional — what `RiskGate` and `OrderBrokerSimulator` measure —
/// is 30,000. The cap must now see it.
#[test]
fn multiplier_gt_one_is_measured_and_blocked() {
    let limits = OrderLimits { max_notional: 1_000.0, ..OrderLimits::default() };
    let req = limit("deribit", "BTC-28MAR25-100000-C", 2.0, 150.0);
    // Pre-fix behaviour, pinned as the thing that was WRONG: 2 × 150 = 300 < 1,000 → passed.
    assert_eq!(validate(&req, &limits), Ok(()));
    // With the real 100x multiplier: 2 × 150 × 100 = 30,000 > 1,000 → blocked.
    match validate_with_value(&req, &limits, ContractValue::linear(100.0)) {
        Err(OrderLimitsError::NotionalAboveMax { notional, max }) => {
            assert_eq!(notional, 30_000.0);
            assert_eq!(max, 1_000.0);
        }
        other => panic!("multiplier-inclusive cap must block the option: {other:?}"),
    }
    // and it still ACCEPTS when the multiplier-inclusive notional genuinely fits.
    let roomy = OrderLimits { max_notional: 50_000.0, ..OrderLimits::default() };
    assert_eq!(validate_with_value(&req, &roomy, ContractValue::linear(100.0)), Ok(()));
}

/// A multiplier BELOW 1 (a fractional contract size) shrinks the notional, so an order the
/// bare `qty × price` would have rejected is correctly admitted.
#[test]
fn multiplier_lt_one_shrinks_the_notional() {
    let limits = OrderLimits { max_notional: 100.0, ..OrderLimits::default() };
    let req = limit("okx", "X", 1.0, 500.0);
    // bare: 1 × 500 = 500 > 100 → rejected
    assert_matches!(validate(&req, &limits), Err(OrderLimitsError::NotionalAboveMax { .. }));
    // with a 0.1 contract size: 1 × 500 × 0.1 = 50 < 100 → admitted
    assert_eq!(validate_with_value(&req, &limits, ContractValue::linear(0.1)), Ok(()));
}

/// Notional is a MAGNITUDE: a negative multiplier enters absolute, so it can never make the
/// cap un-trippable (the sign-flip hole `RiskGate`'s `.abs()` closes for the same reason).
#[test]
fn negative_multiplier_enters_absolute() {
    let limits = OrderLimits { max_notional: 1_000.0, ..OrderLimits::default() };
    let req = limit("deribit", "OPT", 2.0, 150.0);
    let neg = validate_with_value(&req, &limits, ContractValue::linear(-100.0));
    assert_eq!(neg, validate_with_value(&req, &limits, ContractValue::linear(100.0)));
    assert_matches!(neg, Err(OrderLimitsError::NotionalAboveMax { .. }));
}

/// A NaN/infinite multiplier must REJECT, not propagate: NaN makes every `>` comparison false,
/// which would silently disable the cap entirely.
#[test]
fn non_finite_multiplier_is_rejected_not_propagated() {
    let limits = OrderLimits { max_notional: 100.0, ..OrderLimits::default() };
    let req = limit("binance", "S", 1.0, 50.0);
    for m in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            validate_with_value(&req, &limits, ContractValue::linear(m)),
            Err(OrderLimitsError::NonFiniteQuantityOrPrice),
            "multiplier {m} must reject"
        );
    }
}

/// The POLICY ceiling reaching the preview caps — the "value flows" half of Phase 5's
/// contract, at this end of the wire. A PURE function of the caller-supplied value, so this
/// test never touches process-global env (`set_var` is unsound from parallel test threads,
/// and the version of this test that read `VIKE_MAX_ORDER_NOTIONAL` serialized itself by hand
/// to work around exactly that).
#[test]
fn the_policy_ceiling_becomes_the_preview_notional_cap() {
    assert_eq!(OrderLimits::with_max_notional(Some(1234.5)).max_notional, 1234.5);
}

/// No `policy` rows (or no `policy.max_notional_per_order` row) ⇒ TODAY'S DEFAULT: permissive.
/// A nonsense ceiling is ignored the same way — `0.0` would deny every order, a silent halt.
#[test]
fn an_absent_or_nonsense_ceiling_leaves_the_permissive_default() {
    for v in [None, Some(0.0), Some(-1.0), Some(f64::INFINITY), Some(f64::NAN)] {
        assert_eq!(
            OrderLimits::with_max_notional(v).max_notional,
            f64::INFINITY,
            "ceiling {v:?} must not tighten the cap"
        );
    }
}

#[test]
fn a_ceiling_moves_nothing_else_about_the_permissive_default() {
    let l = OrderLimits::with_max_notional(Some(10.0));
    assert_eq!(l.min_qty, 0.0);
    assert_eq!(l.max_qty, f64::INFINITY);
    assert!(l.require_price_for_limit);
}

/// Decision 0170: an INVERSE contract (deribit's `BTC-PERPETUAL`, quantity already in dollars) is
/// measured by its quantity alone — 900 USD is 900 whatever the price, where the linear product
/// would have said 59 million.
#[test]
fn an_inverse_value_is_measured_by_its_quantity_and_not_its_price() {
    let limits = OrderLimits { max_notional: 1_000.0, ..OrderLimits::default() };
    let order = |qty: f64| limit("deribit", "BTC-PERPETUAL", qty, 65_700.0);
    assert_matches!(
        validate(&order(900.0), &limits),
        Err(OrderLimitsError::NotionalAboveMax { .. })
    );
    assert_eq!(validate_with_value(&order(900.0), &limits, ContractValue::inverse(1.0)), Ok(()));
    match validate_with_value(&order(1_100.0), &limits, ContractValue::inverse(1.0)) {
        Err(OrderLimitsError::NotionalAboveMax { notional, max }) => {
            assert_eq!((notional, max), (1_100.0, 1_000.0));
        }
        other => panic!("1100 USD is over a 1000 cap: {other:?}"),
    }
}
