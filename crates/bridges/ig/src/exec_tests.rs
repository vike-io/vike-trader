use super::*;
use std::assert_matches;

fn req(order_type: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        account: None,
        combo_legs: Vec::new(),
        client_order_id: "coid-1".into(),
        venue: VENUE.into(),
        symbol: "CS.D.EURUSD.MINI.IP".into(),
        side,
        qty,
        order_type: order_type.into(),
        price: Some(1.09),
        trigger_price: Some(1.08),
        reduce_only: false,
        time_in_force: vike_model::TimeInForce::Gtc,
        gtd_expiry: None,
        ts: 222,
        parent_order_id: None,
        linked_order_ids: vec![],
        order_list_id: None,
        contingency_type: None,
        weight: 0.0,
        stop: None,
        trail: None,
        extreme: None,
        on_close: false,
        margin_mode: None,
        trigger_by: None,
    }
}

#[test]
fn market_opens_position() {
    let (path, ver, body) = build_request(&req("market", 1, 2.0));
    assert_eq!(path, "/positions/otc");
    assert_eq!(ver, "2");
    assert_eq!(body["orderType"], "MARKET");
    assert_eq!(body["direction"], "BUY");
    assert_eq!(body["size"], 2.0);
    assert_eq!(body["epic"], "CS.D.EURUSD.MINI.IP");
}

/// The close endpoint, its verb override, and — the part a reader will get wrong — that
/// `direction` is the request's OWN side. `OrderIntent::Flatten` already mints the side opposite
/// the position, so negating it here would re-open the very position it was asked to close.
#[test]
fn reduce_only_market_builds_the_close_body_with_the_closing_side() {
    let mut r = req("market", -1, 0.2); // flatten a LONG → a SELL close
    r.reduce_only = true;
    let (path, ver, body) = build_close_request(&r);
    assert_eq!(path, "/positions/otc");
    assert_eq!(ver, "1", "the close endpoint is Version 1, not the open's Version 2");
    assert_eq!(body["direction"], "SELL");
    assert_eq!(body["size"], 0.2);
    assert_eq!(body["orderType"], "MARKET");
    assert_eq!(body["epic"], "CS.D.EURUSD.MINI.IP");
    assert_eq!(body["expiry"], "-");
    // ⚠ A close body carries NO `forceOpen` — the field only exists on the open endpoint, and
    // sending it here is what a copy-paste from `build_request` would do.
    assert!(body.get("forceOpen").is_none(), "a close cannot carry forceOpen: {body}");
    // Closing by epic+expiry rather than dealId is what keeps a multi-deal flatten to ONE
    // confirm; see the function doc and `tests/close_does_not_collide_with_its_open.rs`.
    assert!(body.get("dealId").is_none(), "closed by epic, so IG nets across its own deals");

    // ...and a SHORT flatten is the mirror image.
    let mut long_close = req("market", 1, 0.1);
    long_close.reduce_only = true;
    assert_eq!(build_close_request(&long_close).2["direction"], "BUY");
}

/// An OPEN still opens — `forceOpen: true` belongs on that path and only that path.
#[test]
fn a_plain_market_order_still_opens_its_own_deal() {
    let (path, ver, body) = build_request(&req("market", 1, 2.0));
    assert_eq!((path.as_str(), ver), ("/positions/otc", "2"));
    assert_eq!(body["forceOpen"], true);
}

#[test]
fn limit_is_working_order_with_level() {
    let (path, _ver, body) = build_request(&req("limit", -1, 1.0));
    assert_eq!(path, "/workingorders/otc");
    assert_eq!(body["type"], "LIMIT");
    assert_eq!(body["direction"], "SELL");
    assert_eq!(body["level"], "1.09000");
}

#[test]
fn request_tif_is_ignored_working_orders_rest_gtc() {
    // The Ignored ig row of `vike_model::venues::venue_tif::venue_tif`: the request TIF is NEVER
    // read — a limit asking Ioc still rests GOOD_TILL_CANCELLED, and the MARKET body
    // carries no timeInForce at all. Honoring it is a step-2 wire change behind smokes.
    let mut r = req("limit", 1, 1.0);
    r.time_in_force = vike_model::TimeInForce::Ioc;
    let (_path, _ver, body) = build_request(&r);
    assert_eq!(body["timeInForce"], "GOOD_TILL_CANCELLED");
    assert_eq!(
        vike_model::venues::venue_tif::venue_tif(VENUE, vike_model::TimeInForce::Ioc),
        vike_model::venues::venue_tif::TifOutcome::Ignored { wire: "GOOD_TILL_CANCELLED" }
    );
    let (_path, _ver, mbody) = build_request(&req("market", 1, 1.0));
    assert!(mbody.get("timeInForce").is_none());
}

#[test]
fn confirm_market_accepted_then_filled() {
    let c: serde_json::Value = serde_json::from_str(
            r#"{"dealStatus":"ACCEPTED","reason":"SUCCESS","dealId":"DIAAA1","epic":"CS.D.EURUSD.MINI.IP",
                "direction":"BUY","size":2.0,"level":1.09300}"#,
        )
        .unwrap();
    // Dual-publish: Accepted, then bare Fill (Account folds position/PnL), then the
    // OrderFilled wrap (FSM) — both fills carrying the same dealId trade_id.
    let evs = map_confirm("coid-1", 222, true, &c);
    assert_eq!(evs.len(), 3);
    assert_matches!(
        &evs[0], Event::OrderAccepted(a) if a.venue_order_id.as_deref() == Some("DIAAA1")
    );
    match &evs[1] {
        Event::Fill(fill) => {
            assert_eq!(fill.side, 1);
            assert_eq!(fill.last_qty, 2.0);
            assert_eq!(fill.last_px, 1.093);
            assert_eq!(fill.trade_id, "DIAAA1");
        }
        other => panic!("expected bare Fill second, got {other:?}"),
    }
    match &evs[2] {
        Event::OrderFilled(of) => {
            assert_eq!(of.fill.side, 1);
            assert_eq!(of.fill.trade_id, "DIAAA1");
        }
        other => panic!("expected OrderFilled wrap third, got {other:?}"),
    }
}

#[test]
fn confirm_working_order_accepted_only() {
    let c: serde_json::Value = serde_json::from_str(
            r#"{"dealStatus":"ACCEPTED","dealId":"DIAAA2","epic":"CS.D.EURUSD.MINI.IP","direction":"BUY","size":1.0}"#,
        )
        .unwrap();
    let evs = map_confirm("coid-1", 222, false, &c);
    assert_eq!(evs.len(), 1); // no fill for a resting working order
    assert_matches!(&evs[0], Event::OrderAccepted(_));
}

/// IG answers 404 `error.service.execution.find` for a `dealReference` it has not finished
/// processing, so the FIRST miss is a race rather than an answer — a submit that gave up on it
/// would resolve optimistically for a deal IG was about to confirm normally.
#[test]
fn a_confirm_that_answers_on_a_later_attempt_is_used() {
    let attempts = std::cell::Cell::new(0u32);
    let got = confirm_with_retry_using("REF1", |r| {
        assert_eq!(r, "REF1", "the same reference is re-asked, never a mutated one");
        attempts.set(attempts.get() + 1);
        if attempts.get() < CONFIRM_ATTEMPTS {
            Err(IgApiError { status: 404, message: "error.service.execution.find".into() })
        } else {
            Ok(serde_json::json!({"dealStatus": "ACCEPTED", "dealId": "DIAAA1"}))
        }
    })
    .expect("the late answer is taken");
    assert_eq!(got["dealId"], "DIAAA1");
    assert_eq!(attempts.get(), CONFIRM_ATTEMPTS);
}

/// ⚠ ...and it STOPS. This runs on the exec command thread, in front of every later command, so
/// an unbounded confirm ladder would wedge the whole venue on one order IG will never confirm.
/// The failure is handed back for [`unresolved_confirm`] to resolve — never swallowed.
#[test]
fn the_confirm_retry_is_bounded_and_surfaces_the_last_failure() {
    let attempts = std::cell::Cell::new(0u32);
    let err = confirm_with_retry_using("REF1", |_| {
        attempts.set(attempts.get() + 1);
        Err(IgApiError { status: 404, message: "error.service.execution.find".into() })
    })
    .expect_err("a confirm that never answers must not be reported as success");
    assert_eq!(attempts.get(), CONFIRM_ATTEMPTS, "bounded, never a loop");
    assert_eq!(err.status, 404);
    assert_eq!(err.message, "error.service.execution.find", "IG's own code reaches the log");
}

#[test]
fn confirm_rejected() {
    let c: serde_json::Value = serde_json::from_str(
        r#"{"dealStatus":"REJECTED","reason":"INSUFFICIENT_BALANCE","dealId":"DIAAA3"}"#,
    )
    .unwrap();
    let evs = map_confirm("coid-1", 222, true, &c);
    assert_eq!(evs.len(), 1);
    assert_matches!(&evs[0], Event::OrderRejected(r) if r.reason == "INSUFFICIENT_BALANCE");
}
