use super::*;

#[test]
fn build_limit_buy_matches_literal() {
    let ticket = OrderTicket::limit("binance", "BTCUSDT", 1, 0.5, 30_000.0);
    let req = build_order_request(&ticket, "dom-7".to_string());
    // Exactly what the inline `OrderRequest { .. }` literal produced at the DOM Place site.
    let expected = OrderRequest {
        client_order_id: "dom-7".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 0.5,
        order_type: "limit".to_string(),
        price: Some(30_000.0),
        trigger_price: None,
        reduce_only: false,
        ..Default::default()
    };
    assert_eq!(req, expected);
}

#[test]
fn build_stop_matches_dom_place_shape() {
    // DOM Place with stop == true: order_type "stop", price None, trigger_price Some(px).
    let ticket = OrderTicket::stop("okx", "BTC-USDT", -1, 2.0, 25_000.0);
    let req = build_order_request(&ticket, "dom-9".to_string());
    assert_eq!(req.order_type, "stop");
    assert_eq!(req.price, None);
    assert_eq!(req.trigger_price, Some(25_000.0));
    assert_eq!(req.side, -1);
    assert_eq!(req.venue, "okx");
}

#[test]
fn build_reduce_only_flag_carried() {
    let ticket = OrderTicket {
        reduce_only: true,
        ..OrderTicket::limit("bybit", "ETHUSDT", -1, 1.0, 2000.0)
    };
    let req = build_order_request(&ticket, "dom-1".to_string());
    assert!(req.reduce_only);
}

#[test]
fn build_options_ticket_shape() {
    // Mirrors the options confirm-ticket site: venue "deribit", limit, price Some, no trigger.
    let ticket = OrderTicket::limit("deribit", "BTC-28MAR25-100000-C", 1, 3.0, 0.045);
    let req = build_order_request(&ticket, "opt-4".to_string());
    let expected = OrderRequest {
        client_order_id: "opt-4".to_string(),
        venue: "deribit".to_string(),
        symbol: "BTC-28MAR25-100000-C".to_string(),
        side: 1,
        qty: 3.0,
        order_type: "limit".to_string(),
        price: Some(0.045),
        ..Default::default()
    };
    assert_eq!(req, expected);
}

#[test]
fn coid_format() {
    assert_eq!(next_client_order_id("dom", 42), "dom-42");
    assert_eq!(next_client_order_id("opt", 0), "opt-0");
}

#[test]
fn validate_accepts_good_limit() {
    let req = build_order_request(
        &OrderTicket::limit("binance", "BTCUSDT", 1, 0.01, 30_000.0),
        "c-1".into(),
    );
    assert_eq!(validate(&req, &OrderLimits::default()), Ok(()));
}

#[test]
fn validate_accepts_good_market() {
    // Market has no price → notional not checked, no MissingLimitPrice.
    let req =
        build_order_request(&OrderTicket::market("binance", "BTCUSDT", -1, 0.01), "c-2".into());
    assert_eq!(validate(&req, &OrderLimits::default()), Ok(()));
}

#[test]
fn validate_rejects_below_min() {
    let limits = OrderLimits { min_qty: 0.1, ..OrderLimits::default() };
    let req = build_order_request(
        &OrderTicket::limit("binance", "BTCUSDT", 1, 0.05, 30_000.0),
        "c".into(),
    );
    assert!(matches!(validate(&req, &limits), Err(OrderReject::QtyBelowMin { .. })));
}

#[test]
fn validate_rejects_non_positive_qty_even_with_permissive_min() {
    let req = build_order_request(&OrderTicket::market("binance", "BTCUSDT", 1, 0.0), "c".into());
    assert!(matches!(
        validate(&req, &OrderLimits::default()),
        Err(OrderReject::QtyBelowMin { .. })
    ));
    let req_neg =
        build_order_request(&OrderTicket::market("binance", "BTCUSDT", 1, -1.0), "c".into());
    assert!(matches!(
        validate(&req_neg, &OrderLimits::default()),
        Err(OrderReject::QtyBelowMin { .. })
    ));
}

#[test]
fn validate_rejects_above_max() {
    let limits = OrderLimits { max_qty: 1.0, ..OrderLimits::default() };
    let req = build_order_request(&OrderTicket::market("binance", "BTCUSDT", 1, 5.0), "c".into());
    assert!(matches!(validate(&req, &limits), Err(OrderReject::QtyAboveMax { .. })));
}

#[test]
fn validate_rejects_non_finite() {
    let req =
        build_order_request(&OrderTicket::market("binance", "BTCUSDT", 1, f64::NAN), "c".into());
    assert_eq!(validate(&req, &OrderLimits::default()), Err(OrderReject::NonFiniteQtyOrPrice));
    let req_inf_px = build_order_request(
        &OrderTicket::limit("binance", "BTCUSDT", 1, 1.0, f64::INFINITY),
        "c".into(),
    );
    assert_eq!(
        validate(&req_inf_px, &OrderLimits::default()),
        Err(OrderReject::NonFiniteQtyOrPrice)
    );
}

#[test]
fn validate_rejects_missing_limit_price() {
    // A limit with no price. Build the request directly since the limit ctor always sets a price.
    let req = OrderRequest {
        client_order_id: "c".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: None,
        ..Default::default()
    };
    assert_eq!(validate(&req, &OrderLimits::default()), Err(OrderReject::MissingLimitPrice));
}

#[test]
fn validate_rejects_over_notional() {
    let limits = OrderLimits { max_notional: 100.0, ..OrderLimits::default() };
    // 0.01 * 30_000 = 300 > 100.
    let req = build_order_request(
        &OrderTicket::limit("binance", "BTCUSDT", 1, 0.01, 30_000.0),
        "c".into(),
    );
    assert!(matches!(validate(&req, &limits), Err(OrderReject::NotionalAboveMax { .. })));
}

/// COMPAT PIN: `multiplier = 1.0` — the default, i.e. every spot/linear instrument —
/// reproduces the pre-fix `qty * price` verdict exactly, on both sides of the cap.
/// `x * 1.0` is an IEEE-754 no-op, so this is bit-identical, not merely close.
#[test]
fn multiplier_one_is_byte_identical_to_the_bare_qty_times_price() {
    for (qty, px, cap) in [
        (0.01, 30_000.0, 100.0), // 300 > 100 → reject
        (0.01, 30_000.0, 500.0), // 300 < 500 → accept
        (0.01, 30_000.0, 300.0), // exactly at the cap → accept (strict >)
        (3.0, 0.045, 1.0),       // the options-ticket shape
        (1.0, f64::MAX, 1.0),    // saturating
    ] {
        let limits = OrderLimits { max_notional: cap, ..OrderLimits::default() };
        let req = build_order_request(&OrderTicket::limit("binance", "S", 1, qty, px), "c".into());
        let bare = qty * px;
        let expected = if bare > cap {
            Err(OrderReject::NotionalAboveMax { notional: bare, max: cap })
        } else {
            Ok(())
        };
        assert_eq!(validate(&req, &limits), expected, "qty={qty} px={px} cap={cap}");
        // the explicit-1.0 entry point agrees with the defaulting one
        assert_eq!(validate_with_multiplier(&req, &limits, 1.0), expected);
    }
}

/// THE BUG: an option with a 100x contract multiplier. `qty × price` measures 300 and slips
/// under a 1,000 cap, but the order's real notional — what `RiskGate` and `SimBroker` measure —
/// is 30,000. The cap must now see it.
#[test]
fn multiplier_gt_one_is_measured_and_blocked() {
    let limits = OrderLimits { max_notional: 1_000.0, ..OrderLimits::default() };
    let req = build_order_request(
        &OrderTicket::limit("deribit", "BTC-28MAR25-100000-C", 1, 2.0, 150.0),
        "opt-1".into(),
    );
    // Pre-fix behaviour, pinned as the thing that was WRONG: 2 × 150 = 300 < 1,000 → passed.
    assert_eq!(validate(&req, &limits), Ok(()));
    // With the real 100x multiplier: 2 × 150 × 100 = 30,000 > 1,000 → blocked.
    match validate_with_multiplier(&req, &limits, 100.0) {
        Err(OrderReject::NotionalAboveMax { notional, max }) => {
            assert_eq!(notional, 30_000.0);
            assert_eq!(max, 1_000.0);
        }
        other => panic!("multiplier-inclusive cap must block the option: {other:?}"),
    }
    // and it still ACCEPTS when the multiplier-inclusive notional genuinely fits.
    let roomy = OrderLimits { max_notional: 50_000.0, ..OrderLimits::default() };
    assert_eq!(validate_with_multiplier(&req, &roomy, 100.0), Ok(()));
}

/// A multiplier BELOW 1 (a fractional contract size) shrinks the notional, so an order the
/// bare `qty × price` would have rejected is correctly admitted.
#[test]
fn multiplier_lt_one_shrinks_the_notional() {
    let limits = OrderLimits { max_notional: 100.0, ..OrderLimits::default() };
    let req = build_order_request(&OrderTicket::limit("okx", "X", 1, 1.0, 500.0), "c".into());
    // bare: 1 × 500 = 500 > 100 → rejected
    assert!(matches!(validate(&req, &limits), Err(OrderReject::NotionalAboveMax { .. })));
    // with a 0.1 contract size: 1 × 500 × 0.1 = 50 < 100 → admitted
    assert_eq!(validate_with_multiplier(&req, &limits, 0.1), Ok(()));
}

/// Notional is a MAGNITUDE: a negative multiplier enters absolute, so it can never make the
/// cap un-trippable (the sign-flip hole `RiskGate`'s `.abs()` closes for the same reason).
#[test]
fn negative_multiplier_enters_absolute() {
    let limits = OrderLimits { max_notional: 1_000.0, ..OrderLimits::default() };
    let req = build_order_request(&OrderTicket::limit("deribit", "OPT", 1, 2.0, 150.0), "c".into());
    let neg = validate_with_multiplier(&req, &limits, -100.0);
    assert_eq!(neg, validate_with_multiplier(&req, &limits, 100.0));
    assert!(matches!(neg, Err(OrderReject::NotionalAboveMax { .. })));
}

/// A NaN/infinite multiplier must REJECT, not propagate: NaN makes every `>` comparison false,
/// which would silently disable the cap entirely.
#[test]
fn non_finite_multiplier_is_rejected_not_propagated() {
    let limits = OrderLimits { max_notional: 100.0, ..OrderLimits::default() };
    let req = build_order_request(&OrderTicket::limit("binance", "S", 1, 1.0, 50.0), "c".into());
    for m in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            validate_with_multiplier(&req, &limits, m),
            Err(OrderReject::NonFiniteQtyOrPrice),
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

/// No `policy.toml` (or no `max_notional_per_order` key) ⇒ TODAY'S DEFAULT: permissive.
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
