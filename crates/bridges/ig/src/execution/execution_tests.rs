use super::*;
use crate::transport::IG_HTTP;
use std::assert_matches;

fn req(order_type: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        data_delay: None,
        display_quantity: None,
        release: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        account: None,
        combo_legs: Vec::new(),
        order_client_id: "order-client-id-1".into(),
        venue: VENUE.into(),
        symbol: "CS.D.EURUSD.MINI.IP".into(),
        side,
        qty,
        order_type: vike_model::OrderType::from_stored(order_type),
        price: Some(1.09),
        trigger_price: Some(1.08),
        reduce_only: false,
        time_in_force: vike_model::TimeInForce::Gtc,
        gtd_expiry: None,
        timestamp: 222,
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

/// **The capability row's `reduce_only_kinds` is this client's reduce-only gate, executed.** `submit`'s loop refuses a
/// `reduce_only` order exactly when `closing && !is_market(&req)` (`CLOSE_MARKET_ONLY`, measured on the demo host), so for
/// every kind the row accepts the row lists the kind iff `is_market` holds for it: only a market close is a reduce-only
/// order here, and a bracket's stop-loss and take-profit are refused at the core edge instead.
#[test]
fn the_row_lists_exactly_the_reduce_only_kinds_the_close_route_takes() {
    let capabilities = vike_venues::capabilities_for(VENUE);
    for kind in capabilities.supported_order_kinds {
        assert_eq!(
            is_market(&req(kind, 1, 1.0)),
            capabilities.reduce_only_kinds.contains(kind),
            "ig/{kind}: the row and the close route's gate must agree"
        );
    }
    assert!(CLOSE_MARKET_ONLY.contains("MARKET-only"), "the refusal still says what it refuses");
}

/// **The whole open body of each kind this bridge builds**, so the guard that refuses every other kind
/// cannot move one of them.
#[test]
fn the_open_body_of_every_built_kind_is_pinned_whole() {
    let deal = deal_reference("order-client-id-1");
    for (r, want_path, want) in [
        (
            req("market", 1, 2.0),
            "/positions/otc",
            json!({"epic": "CS.D.EURUSD.MINI.IP", "direction": "BUY", "size": 2.0,
                "orderType": "MARKET", "currencyCode": "USD", "expiry": "-", "forceOpen": true,
                "guaranteedStop": false, "dealReference": deal}),
        ),
        (
            req("limit", -1, 1.0),
            "/workingorders/otc",
            json!({"epic": "CS.D.EURUSD.MINI.IP", "direction": "SELL", "size": 1.0,
                "type": "LIMIT", "level": "1.09000", "timeInForce": "GOOD_TILL_CANCELLED",
                "currencyCode": "USD", "expiry": "-", "guaranteedStop": false, "dealReference": deal}),
        ),
        (
            req("stop", 1, 1.0),
            "/workingorders/otc",
            json!({"epic": "CS.D.EURUSD.MINI.IP", "direction": "BUY", "size": 1.0,
                "type": "STOP", "level": "1.08000", "timeInForce": "GOOD_TILL_CANCELLED",
                "currencyCode": "USD", "expiry": "-", "guaranteedStop": false, "dealReference": deal}),
        ),
    ] {
        assert_eq!(unbuilt_kind(&r), None, "{}", r.order_type);
        assert_eq!(build_request(&r, WIRE_CURRENCY), Ok((want_path.to_string(), "2", want)));
    }
}

/// A kind this bridge builds no order for is refused BY NAME before anything is read or sent (the exec
/// thread asks `unbuilt_kind` first), and EVERY builder refuses it too, so no caller can write its MARKET
/// open or MARKET close: `stop_limit`, `take_profit` and an unrecognised word used to become one. A
/// reduce-only `limit` or `stop` is no MARKET close either.
#[test]
fn a_kind_this_bridge_does_not_build_is_refused_by_name() {
    for kind in ["stop_limit", "take_profit", "market_if_touched"] {
        let named = format!("ig builds no `{kind}` order");
        let r = req(kind, 1, 1.0);
        let why = unbuilt_kind(&r).expect("refused");
        assert!(why.contains(&named), "{why}");
        assert!(!is_market(&r), "{kind} is no market order");
        for built in [
            build_request(&r, WIRE_CURRENCY),
            build_request_expiring(&r, "EUR", "DEC-26"),
            build_close_request(&r),
            build_close_request_expiring(&r, "DEC-26"),
        ] {
            assert!(built.as_ref().is_err_and(|why| why.contains(&named)), "{kind}: {built:?}");
        }
    }
    for kind in ["limit", "stop"] {
        let mut r = req(kind, -1, 1.0);
        r.reduce_only = true;
        assert_eq!(build_close_request(&r), Err(CLOSE_MARKET_ONLY.to_string()), "{kind}");
    }
}

#[test]
fn market_opens_position() {
    let (path, ver, body) = build_request(&req("market", 1, 2.0), WIRE_CURRENCY).unwrap();
    assert_eq!(path, "/positions/otc");
    assert_eq!(ver, "2");
    assert_eq!(body["orderType"], "MARKET");
    assert_eq!(body["direction"], "BUY");
    assert_eq!(body["size"], 2.0);
    assert_eq!(body["epic"], "CS.D.EURUSD.MINI.IP");
}

/// The `size` goes out through `format_to_step` (the pinned wire quantizer) at IG's own size precision, so the
/// float dust a sum or a gate's rounding leaves (`0.1 + 0.2` is `0.30000000000000004`) never reaches the wire, on
/// the open, the working order and the close; it stays a JSON number, and a size on the grid is unchanged.
#[test]
fn the_size_goes_out_without_float_dust_on_every_body() {
    let dusty = 0.1 + 0.2;
    let open = build_request(&req("market", 1, dusty), WIRE_CURRENCY).unwrap().2;
    let working = build_request(&req("limit", 1, dusty), WIRE_CURRENCY).unwrap().2;
    let mut close = req("market", -1, dusty);
    close.reduce_only = true;
    let close = build_close_request(&close).unwrap().2;
    for body in [&open, &working, &close] {
        assert_eq!(body["size"], serde_json::json!(0.3), "{body}");
        assert!(body["size"].is_number(), "{body}");
    }
    let on_grid = build_request(&req("market", 1, 2.5), WIRE_CURRENCY).unwrap().2;
    assert_eq!(on_grid["size"], serde_json::json!(2.5));
}

/// **The any-symbol facts admit only what this wire says**: every order body carries the currency, the
/// expiry and the five-decimal level `market_facts` fences on, from the SAME constants. A change here that
/// is not a change there would admit an epic the wire cannot express.
#[test]
fn the_wire_says_what_the_any_symbol_facts_fence_on() {
    assert_eq!((WIRE_CURRENCY, WIRE_EXPIRY), ("USD", "-"), "the bytes IG has always been sent");
    let market = build_request(&req("market", 1, 1.0), WIRE_CURRENCY).unwrap().2;
    let limit = build_request(&req("limit", 1, 1.0), WIRE_CURRENCY).unwrap().2;
    for body in [&market, &limit, &build_close_request(&req("market", 1, 1.0)).unwrap().2] {
        assert_eq!(body["expiry"], WIRE_EXPIRY, "{body}");
    }
    for body in [&market, &limit] {
        assert_eq!(body["currencyCode"], WIRE_CURRENCY, "{body}");
    }
    assert_eq!(
        fmt_level(crate::instrument::WIRE_PRICE_STEP),
        "0.00001",
        "the finest level the wire can write is the finest pip the facts admit"
    );
}

/// The currency on the wire is the one the epic's table gave, in BOTH open bodies, and a `USD` body is
/// byte-for-byte the one IG has always been sent (only `currencyCode` can differ).
#[test]
fn the_open_bodies_carry_the_currency_they_are_given() {
    for kind in ["market", "limit"] {
        let usd = build_request(&req(kind, 1, 1.0), "USD").unwrap().2;
        let gbp = build_request(&req(kind, 1, 1.0), "GBP").unwrap().2;
        assert_eq!((&usd["currencyCode"], &gbp["currencyCode"]), (&json!("USD"), &json!("GBP")));
        let mut want = usd.clone();
        want["currencyCode"] = json!("GBP");
        assert_eq!(gbp, want, "{kind}: nothing but the currency differs");
    }
}

/// `GET /markets/{epic}` `instrument.currencies[]` as IG states it: a code per row.
fn market_with(currencies: &[&str]) -> serde_json::Value {
    let rows: Vec<_> =
        currencies.iter().map(|c| json!({ "code": c, "isDefault": false })).collect();
    json!({ "instrument": { "epic": "CS.D.EURGBP.MINI.IP", "currencies": rows } })
}

/// Measured on the demo gateway (2026-10-19): EURUSD lists USD, EURGBP GBP alone, USDJPY JPY alone. USD
/// wins whenever listed (the old bytes, whatever the account), then the account's own currency, then the
/// first listed.
#[test]
fn order_currency_prefers_usd_then_the_account_then_the_first_listed() {
    let epic = "CS.D.EURGBP.MINI.IP";
    let pick = |listed: &[&str], account: Option<&str>| {
        order_currency(epic, &market_with(listed), account).unwrap()
    };
    assert_eq!(pick(&["USD"], Some("USD")), "USD");
    assert_eq!(pick(&["GBP"], Some("USD")), "GBP");
    assert_eq!(pick(&["JPY"], None), "JPY");
    assert_eq!(pick(&["GBP", "USD"], Some("GBP")), "USD", "USD listed: the old bytes stay");
    assert_eq!(pick(&["EUR", "GBP"], Some("GBP")), "GBP", "no USD: the account's own");
    assert_eq!(pick(&["EUR", "GBP"], Some("CHF")), "EUR", "account not listed: the first");
    assert_eq!(pick(&["EUR", "GBP"], None), "EUR");
}

/// A table that lists no usable currency leaves nothing to write: a refusal that names the epic.
#[test]
fn order_currency_refuses_in_words_when_nothing_is_listed() {
    let epic = "CS.D.EURGBP.MINI.IP";
    for body in [
        market_with(&[]),
        market_with(&["us", "USDX", ""]),
        json!({ "instrument": {} }),
        json!({}),
        json!({ "instrument": { "currencies": [{ "isDefault": true }] } }),
    ] {
        let why = order_currency(epic, &body, Some("USD")).unwrap_err();
        assert!(why.contains(epic) && why.contains("dealing currency"), "{why}");
    }
}

fn unreadable() -> Result<serde_json::Value, VenueApiHttpError> {
    Err(IG_HTTP.error(429, "error.public-api.exceeded-api-key-allowance"))
}

/// FAIL CLOSED: a table that cannot be read is not a licence to guess `USD` for an epic that lists
/// another currency alone (IG's reference makes that a terminal reject, never observed, and a 429 on the
/// shared allowance is likeliest exactly when the exec lane is busiest). The refusal says why and that the
/// order can be retried, and it keeps nothing, so the next order reads the table again.
#[test]
fn an_unreadable_table_refuses_a_gbp_only_epic_in_words() {
    let (epic, mut kept) = ("CS.D.EURGBP.MINI.IP", HashMap::new());
    let why = epic_wire_with(&mut kept, epic, Some("USD"), None, unreadable).unwrap_err();
    assert!(
        why.contains(epic) && why.contains("dealing currencies") && why.contains("retry"),
        "{why}"
    );
    assert!(kept.is_empty(), "a failed read is not remembered");
    // ...and the next order reads again, and is served once the table answers
    let read_ok = || Ok(market_with(&["GBP"]));
    assert_eq!(
        epic_wire_with(&mut kept, epic, Some("USD"), None, read_ok).unwrap().currency,
        "GBP"
    );
}

/// A good read is the epic's answer for the life of the thread: a table that goes unreadable afterwards
/// does not take a working epic down.
#[test]
fn a_previously_read_epic_is_served_from_the_cache_when_its_table_goes_unreadable() {
    let (epic, mut kept) = ("CS.D.GBPUSD.MINI.IP", HashMap::new());
    let first = epic_wire_with(&mut kept, epic, None, None, || Ok(market_with(&["USD"])));
    assert_eq!(first.unwrap().currency, "USD");
    let again = epic_wire_with(&mut kept, epic, None, None, || -> Result<_, _> {
        panic!("a cached epic must not be read again")
    });
    assert_eq!(again.unwrap().currency, "USD");
}

/// The epic the ACCOUNT states with its wire words (the EUR/USD mini of the account the mount used to be wired to,
/// which lists USD, measured) as its `FallbackEpic`.
fn stated_fallback() -> FallbackEpic {
    FallbackEpic {
        epic: "CS.D.EURUSD.MINI.IP".to_string(),
        wire: EpicWire { currency: "USD".to_string(), expiry: "-".to_string() },
    }
}

/// The epic the account STATES (its `fallback_symbol*` facts: EUR/USD mini, which lists USD, measured) keeps trading
/// on the words it states when its table is unreadable. Any OTHER epic, even one that would list USD, is refused:
/// nothing but a read says what it lists. And an account that states none has no exempt epic at all: the same epic is
/// refused like any other (owner, 2026-11-13: no compiled epic).
#[test]
fn the_stated_epic_keeps_its_words_when_its_table_is_unreadable_and_no_other_does() {
    let fallback = stated_fallback();
    let mut kept = HashMap::new();
    let wire =
        epic_wire_with(&mut kept, &fallback.epic, None, Some(&fallback), unreadable).unwrap();
    assert_eq!((wire.currency.as_str(), wire.expiry.as_str()), ("USD", "-"));
    assert!(kept.is_empty(), "a guess is not remembered");
    let other = epic_wire_with(&mut kept, "CS.D.GBPUSD.MINI.IP", None, Some(&fallback), unreadable);
    assert!(other.unwrap_err().contains("CS.D.GBPUSD.MINI.IP"));
    let unstated = epic_wire_with(&mut kept, &fallback.epic, None, None, unreadable);
    assert!(unstated.unwrap_err().contains("CS.D.EURUSD.MINI.IP"), "no fallback stated: refused");
    // a table that DOES answer is the epic's own word, the stated fallback notwithstanding
    let read =
        epic_wire_with(&mut kept, &fallback.epic, None, Some(&fallback), || Ok(dated("DEC-26")))
            .unwrap();
    assert_eq!(read.expiry, "DEC-26", "the read wins over the fallback");
}

/// The account's row is where the fallback comes from: all three facts, or none is used.
#[test]
fn the_fallback_epic_is_the_rows_three_facts_or_nothing() {
    use vike_secrets::AccountFact;
    let row = |facts: &[(AccountFact, &str)]| vike_secrets::AccountRow {
        facts: facts.iter().map(|(f, v)| (f.as_str().to_string(), v.to_string())).collect(),
        ..vike_secrets::account_row(1, VENUE, vike_model::AccountTier::Demo, None, true)
    };
    let all = [
        (AccountFact::FallbackSymbol, "CS.D.EURUSD.MINI.IP"),
        (AccountFact::FallbackSymbolExpiry, "-"),
        (AccountFact::FallbackSymbolCurrency, "USD"),
    ];
    assert_eq!(FallbackEpic::of(Some(&row(&all))), Some(stated_fallback()));
    for missing in 0..all.len() {
        let partial: Vec<_> =
            all.iter().enumerate().filter(|(i, _)| *i != missing).map(|(_, f)| *f).collect();
        assert_eq!(FallbackEpic::of(Some(&row(&partial))), None, "without {:?}", all[missing].0);
    }
    assert_eq!(FallbackEpic::of(None), None, "no row");
}

/// The close endpoint, its verb override, and — the part a reader will get wrong — that
/// `direction` is the request's OWN side. `OrderIntent::Flatten` already mints the side opposite
/// the position, so negating it here would re-open the very position it was asked to close.
#[test]
fn reduce_only_market_builds_the_close_body_with_the_closing_side() {
    let mut r = req("market", -1, 0.2); // flatten a LONG → a SELL close
    r.reduce_only = true;
    let (path, ver, body) = build_close_request(&r).unwrap();
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
    // confirm; see the function doc and `tests/offline/close_does_not_collide_with_its_open.rs`.
    assert!(body.get("dealId").is_none(), "closed by epic, so IG nets across its own deals");

    // ...and a SHORT flatten is the mirror image.
    let mut long_close = req("market", 1, 0.1);
    long_close.reduce_only = true;
    assert_eq!(build_close_request(&long_close).unwrap().2["direction"], "BUY");
}

/// An OPEN still opens — `forceOpen: true` belongs on that path and only that path.
#[test]
fn a_plain_market_order_still_opens_its_own_deal() {
    let (path, ver, body) = build_request(&req("market", 1, 2.0), WIRE_CURRENCY).unwrap();
    assert_eq!((path.as_str(), ver), ("/positions/otc", "2"));
    assert_eq!(body["forceOpen"], true);
}

#[test]
fn limit_is_working_order_with_level() {
    let (path, _version, body) = build_request(&req("limit", -1, 1.0), WIRE_CURRENCY).unwrap();
    assert_eq!(path, "/workingorders/otc");
    assert_eq!(body["type"], "LIMIT");
    assert_eq!(body["direction"], "SELL");
    assert_eq!(body["level"], "1.09000");
}

#[test]
fn request_time_in_force_is_ignored_working_orders_rest_gtc() {
    // The Ignored ig row of `vike_venues::venue_time_in_force::venue_time_in_force`: the request TIF is NEVER
    // read — a limit asking Ioc still rests GOOD_TILL_CANCELLED, and the MARKET body
    // carries no timeInForce at all. Honoring it is a step-2 wire change behind smokes.
    let mut r = req("limit", 1, 1.0);
    r.time_in_force = vike_model::TimeInForce::Ioc;
    let (_path, _version, body) = build_request(&r, WIRE_CURRENCY).unwrap();
    assert_eq!(body["timeInForce"], "GOOD_TILL_CANCELLED");
    assert_eq!(
        vike_venues::venue_time_in_force::venue_time_in_force(VENUE, vike_model::TimeInForce::Ioc),
        vike_venues::venue_time_in_force::TimeInForceOutcome::Ignored {
            wire: "GOOD_TILL_CANCELLED"
        }
    );
    let (_path, _version, mbody) = build_request(&req("market", 1, 1.0), WIRE_CURRENCY).unwrap();
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
    let events = map_confirm("order-client-id-1", 222, true, &c);
    assert_eq!(events.len(), 3);
    assert_matches!(
        &events[0], Event::OrderAccepted(a) if a.venue_order_id.as_deref() == Some("DIAAA1")
    );
    match &events[1] {
        Event::Fill(fill) => {
            assert_eq!(fill.side, 1);
            assert_eq!(fill.last_qty, 2.0);
            assert_eq!(fill.last_price, 1.093);
            assert_eq!(fill.trade_id, "DIAAA1");
        }
        other => panic!("expected bare Fill second, got {other:?}"),
    }
    match &events[2] {
        Event::OrderFilled(of) => {
            assert_eq!(of.fill.side, 1);
            assert_eq!(of.fill.trade_id, "DIAAA1");
        }
        other => panic!("expected OrderFilled wrap third, got {other:?}"),
    }
}

/// A confirm that neither accepts nor rejects says nothing: on the old code an absent or other `dealStatus` on a
/// market order was Accepted + Fill + OrderFilled at size 0 and price 0, ending the order over a deal nobody saw.
#[test]
fn a_confirm_with_no_deal_status_or_another_one_says_nothing() {
    for market in [true, false] {
        for c in [
            json!({"dealId": "DIAAA1", "epic": "CS.D.EURUSD.MINI.IP", "direction": "BUY"}),
            json!({"dealStatus": "FUND_ACCOUNT", "dealId": "DIAAA1", "size": 2.0, "level": 1.09}),
        ] {
            assert!(map_confirm("order-client-id-1", 222, market, &c).is_empty(), "{c}");
            let ids = Arc::new(Mutex::new(OrderVenueIdRegistry::<String>::default()));
            note_deal_id(&ids, "order-client-id-1", &c);
            assert_eq!(
                ids.lock().unwrap().venue_order_id_of("order-client-id-1"),
                None,
                "no deal is proven: {c}"
            );
        }
    }
}

/// IG accepted a market deal and did not say at what size or level: the accept is true, a fill at 0 is not.
#[test]
fn an_accepted_market_deal_with_no_size_or_level_books_no_fill() {
    for c in [
        json!({"dealStatus": "ACCEPTED", "dealId": "DIAAA1", "size": 2.0}),
        json!({"dealStatus": "ACCEPTED", "dealId": "DIAAA1", "level": 1.09}),
        json!({"dealStatus": "ACCEPTED", "dealId": "DIAAA1", "size": 0.0, "level": 1.09}),
    ] {
        let events = map_confirm("order-client-id-1", 222, true, &c);
        assert_eq!(events.len(), 1, "{c}: {events:?}");
        assert_matches!(&events[0], Event::OrderAccepted(_));
    }
}

#[test]
fn confirm_working_order_accepted_only() {
    let c: serde_json::Value = serde_json::from_str(
            r#"{"dealStatus":"ACCEPTED","dealId":"DIAAA2","epic":"CS.D.EURUSD.MINI.IP","direction":"BUY","size":1.0}"#,
        )
        .unwrap();
    let events = map_confirm("order-client-id-1", 222, false, &c);
    assert_eq!(events.len(), 1); // no fill for a resting working order
    assert_matches!(&events[0], Event::OrderAccepted(_));
}

/// IG answers 404 `error.service.execution.find` for a `dealReference` it has not finished
/// processing, so the FIRST miss is a race rather than an answer — a submit that gave up on it
/// would resolve optimistically for a deal IG was about to confirm normally.
#[test]
fn a_confirm_that_answers_on_a_later_attempt_is_used() {
    let attempts = std::cell::Cell::new(0u32);
    let got = confirm_with_retry_using(
        "REF1",
        |r| {
            assert_eq!(r, "REF1", "the same reference is re-asked, never a mutated one");
            attempts.set(attempts.get() + 1);
            if attempts.get() < CONFIRM_ATTEMPTS {
                Err(IG_HTTP.error(404, "error.service.execution.find"))
            } else {
                Ok(serde_json::json!({"dealStatus": "ACCEPTED", "dealId": "DIAAA1"}))
            }
        },
        || {},
    )
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
    let pauses = std::cell::Cell::new(0u32);
    let err = confirm_with_retry_using(
        "REF1",
        |_| {
            attempts.set(attempts.get() + 1);
            Err(IG_HTTP.error(404, "error.service.execution.find"))
        },
        || pauses.set(pauses.get() + 1),
    )
    .expect_err("a confirm that never answers must not be reported as success");
    assert_eq!(attempts.get(), CONFIRM_ATTEMPTS, "bounded, never a loop");
    assert_eq!(pauses.get(), CONFIRM_ATTEMPTS - 1, "a pause BETWEEN attempts, none after the last");
    assert_eq!(err.status, 404);
    assert_eq!(err.message, "error.service.execution.find", "IG's own code reaches the log");
}

/// Every miss of the confirm ladder is ONE `warn` on `vike_ig::execution`, the last miss included, naming the
/// reference, its attempt of seven, IG's status and code: pinned before the ladder moved to
/// the shared ladder (`vike_utility::retry`).
#[test]
fn every_confirm_miss_is_one_warn_naming_its_attempt_of_seven() {
    let (got, lines) = vike_log::capture::captured(|| {
        confirm_with_retry_using(
            "REF9",
            |_| Err(IG_HTTP.error(404, "error.service.execution.find")),
            || {},
        )
    });
    assert_eq!(got.expect_err("never answered").status, 404);
    let attempts: Vec<String> = lines
        .iter()
        .map(|line| {
            assert_eq!(line.level, tracing::Level::WARN);
            assert_eq!(line.target, "vike_ig::execution");
            assert_eq!(line.message, "IG /confirms did not answer; retrying");
            assert_eq!(
                [
                    line.field("deal_ref"),
                    line.field("of"),
                    line.field("status"),
                    line.field("error")
                ],
                [Some("REF9"), Some("7"), Some("404"), Some("error.service.execution.find")]
            );
            line.field("attempt").unwrap_or_default().to_string()
        })
        .collect();
    assert_eq!(attempts, ["1", "2", "3", "4", "5", "6", "7"]);
}

/// A loopback IG that answers its Nth connection with the Nth of `replies` (`None`: read the request,
/// then hang up without a word, the transport failure a login retries) and hangs up on every later
/// one: its base URL and how many connections it took.
fn ig_answering(
    replies: Vec<Option<&'static str>>,
) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a local port");
    let base = format!("http://{}", listener.local_addr().expect("its address"));
    let taken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = Arc::clone(&taken);
    std::thread::spawn(move || {
        let mut replies = replies.into_iter();
        for sock in listener.incoming() {
            let Ok(sock) = sock else { return };
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut reader = BufReader::new(sock);
            let mut length = 0;
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                    break;
                }
                if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0_u8; length];
            let _ = reader.read_exact(&mut body);
            if let Some(Some(reply)) = replies.next() {
                let _ = reader.get_mut().write_all(reply.as_bytes());
            }
        }
    });
    (base, taken)
}

fn login_config(base: &str) -> IgConfig {
    use vike_model::secret::Secret;
    IgConfig {
        api_key: Secret::new("k".to_string()),
        identifier: Secret::new("i".to_string()),
        password: Secret::new("p".to_string()),
        rest_base: base.to_string(),
        account_pin: None,
    }
}

/// `login_with_retry`'s lines on `vike_ig::execution`, as `(level, message, lane, attempt)`.
fn login_lines(
    lines: &[vike_log::capture::CapturedEvent],
) -> Vec<(tracing::Level, String, String, String)> {
    let field = |line: &vike_log::capture::CapturedEvent, name: &str| {
        line.field(name).unwrap_or_default().to_string()
    };
    lines
        .iter()
        .filter(|line| line.target == "vike_ig::execution")
        .map(|line| (line.level, line.message.clone(), field(line, "lane"), field(line, "attempt")))
        .collect()
}

/// **The login ladder, pinned before it moved to the shared ladder (`vike_utility::retry`)**: five logins that never reach IG,
/// each one `warn` naming its attempt of five, each followed by a 3 s pause (the fifth too: 15 s in all), then
/// one `error` and the lane is down with the last failure's words.
#[test]
fn a_login_that_never_reaches_ig_is_tried_five_times_three_seconds_apart() {
    let (base, taken) = ig_answering(Vec::new());
    let stop = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let (got, lines) =
        vike_log::capture::captured(|| login_with_retry(&login_config(&base), &stop, "exec"));
    let waited = started.elapsed();
    let why = got.err().expect("the lane is down");
    assert!(why.starts_with("5 attempts failed, the last with "), "{why}");
    assert_eq!(taken.load(std::sync::atomic::Ordering::SeqCst), 5, "five logins, no more");
    assert!(
        waited >= std::time::Duration::from_secs(15),
        "five 3 s pauses, the last one too: {waited:?}"
    );
    let retried = |n: &str| {
        (
            tracing::Level::WARN,
            "IG login failed; retrying".to_string(),
            "exec".to_string(),
            n.to_string(),
        )
    };
    let exhausted = (
        tracing::Level::ERROR,
        "IG login exhausted its retry budget. This IG lane is DOWN for the life of the mount"
            .to_string(),
        "exec".to_string(),
        String::new(),
    );
    assert_eq!(
        login_lines(&lines),
        [retried("1"), retried("2"), retried("3"), retried("4"), retried("5"), exhausted]
    );
    let first = lines.iter().find(|line| line.target == "vike_ig::execution").expect("a line");
    assert_eq!(first.field("of"), Some("5"));
    let last = lines.iter().rfind(|line| line.target == "vike_ig::execution").expect("a line");
    assert_eq!(last.field("attempts"), Some("5"));
}

/// The other three ends of the login ladder: IG REFUSING the login stops at once (one `error`, no pause), a
/// stop raised before it asks nothing, and a login that answers on its second attempt says so at `info`.
#[test]
fn a_refused_login_stops_at_once_a_stopped_one_asks_nothing_and_a_late_one_says_so() {
    const REFUSED: &str = "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: 46\r\n\
         connection: close\r\n\r\n{\"errorCode\":\"error.security.invalid-details\"}";
    let (base, taken) = ig_answering(vec![Some(REFUSED)]);
    let stop = Arc::new(AtomicBool::new(false));
    let (got, lines) = vike_log::capture::captured(|| {
        login_with_retry(&login_config(&base), &stop, "trade-stream")
    });
    let why = got.err().expect("refused");
    assert!(why.starts_with("IG refused it ("), "{why}");
    assert_eq!(taken.load(std::sync::atomic::Ordering::SeqCst), 1);
    let refused = login_lines(&lines);
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!((refused[0].0, refused[0].2.as_str()), (tracing::Level::ERROR, "trade-stream"));
    assert!(refused[0].1.starts_with("IG REFUSED this login"), "{refused:?}");

    let (base, taken) = ig_answering(Vec::new());
    let stopped = Arc::new(AtomicBool::new(true));
    let (got, lines) =
        vike_log::capture::captured(|| login_with_retry(&login_config(&base), &stopped, "exec"));
    assert_eq!(got.err().as_deref(), Some("the client is stopping"));
    assert_eq!(taken.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(login_lines(&lines).is_empty());

    const LOGGED_IN: &str = "HTTP/1.1 200 OK\r\nCST: c\r\nX-SECURITY-TOKEN: x\r\ncontent-type: application/json\r\n\
         content-length: 27\r\nconnection: close\r\n\r\n{\"currentAccountId\":\"ACC1\"}";
    // the first login hangs up, the second is answered, its `GET /accounts` hangs up (the session stays put)
    let (base, taken) = ig_answering(vec![None, Some(LOGGED_IN)]);
    let started = Instant::now();
    let (got, lines) =
        vike_log::capture::captured(|| login_with_retry(&login_config(&base), &stop, "exec"));
    assert!(got.is_ok(), "{:?}", got.err());
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(3),
        "one pause before the second login"
    );
    assert_eq!(
        taken.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "two logins and one account list"
    );
    let (warn, info) = (tracing::Level::WARN, tracing::Level::INFO);
    assert_eq!(
        login_lines(&lines),
        [
            (warn, "IG login failed; retrying".to_string(), "exec".to_string(), "1".to_string()),
            (
                info,
                "IG login succeeded after retrying".to_string(),
                "exec".to_string(),
                "2".to_string()
            ),
        ]
    );
}

/// The ladder's WINDOW is the fix, not just its length: the CI box measured IG still answering 404 after
/// the old 3 x 200 ms ladder (~0.4 s), which left the order uncancellable. The waiting must cover
/// 2-3 s — and stay under the point where one unanswerable order wedges the exec thread for good.
#[test]
fn the_confirm_ladder_waits_two_to_three_seconds() {
    let waited = CONFIRM_BACKOFF * (CONFIRM_ATTEMPTS - 1);
    assert!(
        waited >= std::time::Duration::from_secs(2) && waited <= std::time::Duration::from_secs(3),
        "the ladder waits {waited:?} in pauses"
    );
}

/// A confirm that answers teaches the shared map its `dealId` (this is what the confirm WATCHDOG
/// does for an order whose submit-time confirm 404'd); a REJECTION must not, since its deal does
/// not exist and cancel must never address it.
#[test]
fn a_confirm_teaches_the_shared_map_its_deal_id_unless_rejected() {
    let ids = Arc::new(Mutex::new(OrderVenueIdRegistry::<String>::default()));
    note_deal_id(
        &ids,
        "order-client-id-1",
        &serde_json::json!({"dealStatus": "ACCEPTED", "dealId": "DIAAA2"}),
    );
    assert_eq!(
        ids.lock().unwrap().venue_order_id_of("order-client-id-1").map(String::as_str),
        Some("DIAAA2")
    );
    assert_eq!(ids.lock().unwrap().order_client_id_of("DIAAA2"), Some("order-client-id-1"));

    note_deal_id(
        &ids,
        "order-client-id-2",
        &serde_json::json!({"dealStatus": "REJECTED", "dealId": "DIAAA3", "reason": "X"}),
    );
    assert_eq!(ids.lock().unwrap().venue_order_id_of("order-client-id-2"), None);
    // a confirm with no dealId changes nothing
    note_deal_id(&ids, "order-client-id-3", &serde_json::json!({"dealStatus": "ACCEPTED"}));
    assert_eq!(ids.lock().unwrap().venue_order_id_of("order-client-id-3"), None);
}

/// **After a gatewayhub restart** the start's map is empty, so a working order the previous process placed had no
/// `dealId` (its cancel answered "no working order", the listing named no order client id). The re-teaching's venue order id refills
/// it in both directions, the key the cancel and amend address it by and the one the reconcile client names it by; an
/// order taught with no id (or an empty one) teaches nothing.
#[test]
fn the_reteaching_refills_the_deal_ids_of_orders_a_previous_process_placed() {
    let ids = Arc::new(Mutex::new(OrderVenueIdRegistry::<String>::default()));
    teach_deal_ids(
        &ids,
        &[
            vike_execution::RestoredOrder {
                venue_order_id: Some("DIAAA9".into()),
                ..vike_execution::RestoredOrder::new("order-client-id-old", "CS.D.EURUSD.MINI.IP")
            },
            vike_execution::RestoredOrder::new("order-client-id-no-id", "CS.D.EURUSD.MINI.IP"),
            vike_execution::RestoredOrder {
                venue_order_id: Some(String::new()),
                ..vike_execution::RestoredOrder::new("order-client-id-empty", "CS.D.EURUSD.MINI.IP")
            },
        ],
    );
    let ids = ids.lock().unwrap();
    assert_eq!(
        ids.venue_order_id_of("order-client-id-old").map(String::as_str),
        Some("DIAAA9"),
        "the cancel's key"
    );
    assert_eq!(ids.order_client_id_of("DIAAA9"), Some("order-client-id-old"), "the listing's name");
    assert_eq!(ids.venue_order_id_of("order-client-id-no-id"), None);
    assert_eq!(ids.venue_order_id_of("order-client-id-empty"), None);
}

#[test]
fn confirm_rejected() {
    let c: serde_json::Value = serde_json::from_str(
        r#"{"dealStatus":"REJECTED","reason":"INSUFFICIENT_BALANCE","dealId":"DIAAA3"}"#,
    )
    .unwrap();
    let events = map_confirm("order-client-id-1", 222, true, &c);
    assert_eq!(events.len(), 1);
    assert_matches!(&events[0], Event::OrderRejected(r) if r.reason == "INSUFFICIENT_BALANCE");
}

/// A rejection that says the MARKET's state or its DEALING RULES refused the deal keeps IG's code first and then
/// names the field of the market details that answers it; any other code is passed through untouched.
#[test]
fn a_rejection_for_the_markets_state_or_rules_names_the_field_that_answers_it() {
    let reject = |code: &str| {
        let c = serde_json::json!({"dealStatus": "REJECTED", "reason": code, "dealId": "DIAAA3"});
        match map_confirm("order-client-id-1", 222, true, &c).as_slice() {
            [Event::OrderRejected(r)] => r.reason.to_string(),
            other => panic!("one rejection expected, got {other:?}"),
        }
    };
    let closed = reject("MARKET_CLOSED");
    assert!(closed.starts_with("MARKET_CLOSED ") && closed.contains("marketStatus"), "{closed}");
    let size = reject("MINIMUM_ORDER_SIZE_ERROR");
    assert!(
        size.starts_with("MINIMUM_ORDER_SIZE_ERROR ") && size.contains("dealingRules.minDealSize"),
        "{size}"
    );
    let kind = reject("MARKET_ORDERS_NOT_ALLOWED_ON_INSTRUMENT");
    assert!(kind.contains("dealingRules.marketOrderPreference"), "{kind}");
    assert_eq!(
        reject("INSUFFICIENT_BALANCE"),
        "INSUFFICIENT_BALANCE",
        "a code about neither is as IG sent it"
    );
    assert_eq!(reject("SOMETHING_NEW"), "SOMETHING_NEW");
}

/// IG charges commission on a SHARES market and nothing in this client reads it (`commission: 0.0` in every
/// mapper), so an OPENING order on one is refused in words instead of booking a fee of 0 without a word. A
/// currency pair (and a table that states no type) is untouched, and the refusal is not remembered, so the
/// next order reads the table again.
#[test]
fn a_shares_market_is_refused_for_the_commission_nothing_reads() {
    let epic = "UC.D.VOD.CASH.IP";
    let shares = json!({ "instrument": { "epic": epic, "type": "SHARES", "currencies": [{ "code": "GBP" }] } });
    let mut kept = HashMap::new();
    let why =
        epic_wire_with(&mut kept, epic, Some("USD"), None, || Ok(shares.clone())).unwrap_err();
    assert!(why.contains(epic) && why.contains("SHARES") && why.contains("commission"), "{why}");
    assert!(kept.is_empty(), "a refusal is not remembered");

    let fx = json!({ "instrument": { "epic": "CS.D.EURUSD.MINI.IP", "type": "CURRENCIES", "currencies": [{ "code": "USD" }] } });
    assert_eq!(
        epic_wire_with(&mut kept, "CS.D.EURUSD.MINI.IP", None, None, || Ok(fx.clone()))
            .unwrap()
            .currency,
        "USD"
    );
    assert_eq!(
        epic_wire_with(&mut kept, "CS.D.GBPUSD.MINI.IP", None, None, || Ok(market_with(&["USD"])))
            .unwrap()
            .currency,
        "USD",
        "a table that states no type is not a share market"
    );
}

/// `GET /markets/{epic}` as a DATED contract states it (the captured `IX.D.DAX.FBMU1.IP` has `expiry` `DEC-26` and
/// lists EUR alone).
fn dated(expiry: &str) -> serde_json::Value {
    json!({ "instrument": { "epic": "IX.D.DAX.FBMU1.IP", "type": "INDICES", "expiry": expiry,
                            "currencies": [{ "code": "EUR" }] } })
}

/// **A dated contract's own `expiry` is what an order writes**, in the open, the working order and the close; `-`,
/// `DFB`, a table that states none and a word that cannot be an expiry all keep the byte every body always carried.
/// MUTATION PROOF: make `wire_expiry_of` return `WIRE_EXPIRY` always - the `DEC-26` rows come back `-`.
#[test]
fn a_dated_epics_own_expiry_is_written_and_everything_else_keeps_the_dash() {
    for (said, written) in [
        (json!("DEC-26"), "DEC-26"),
        (json!("30-NOV-26"), "30-NOV-26"),
        (json!("-"), "-"),
        (json!("DFB"), "-"),
        (json!(""), "-"),
        (json!("dec-26"), "-"),
        (json!("DEC 26"), "-"),
        (json!("1234567890123"), "-"),
        (json!(null), "-"),
    ] {
        let market = json!({ "instrument": { "expiry": said } });
        assert_eq!(wire_expiry_of(&market), written, "{said}");
    }
    assert_eq!(wire_expiry_of(&json!({})), "-", "a table that states none");

    let mut kept = HashMap::new();
    let wire =
        epic_wire_with(&mut kept, "IX.D.DAX.FBMU1.IP", None, None, || Ok(dated("DEC-26"))).unwrap();
    assert_eq!((wire.currency.as_str(), wire.expiry.as_str()), ("EUR", "DEC-26"));
    assert_eq!(kept.get("IX.D.DAX.FBMU1.IP"), Some(&wire), "kept with its expiry");

    for kind in ["market", "limit"] {
        let open = build_request_expiring(&req(kind, 1, 1.0), "EUR", "DEC-26").unwrap().2;
        assert_eq!(open["expiry"], "DEC-26", "{kind}: {open}");
        let mut undated = open.clone();
        undated["expiry"] = json!("-");
        assert_eq!(
            undated,
            build_request(&req(kind, 1, 1.0), "EUR").unwrap().2,
            "{kind}: nothing but the expiry differs from the plain builder"
        );
    }
    let mut close = req("market", -1, 1.0);
    close.reduce_only = true;
    let dated_close = build_close_request_expiring(&close, "DEC-26").unwrap().2;
    assert_eq!(dated_close["expiry"], "DEC-26");
    assert_eq!(build_close_request_expiring(&close, "-"), build_close_request(&close));
}

/// A CLOSE writes the expiry the open of this process read; for an epic this process never opened it reads the table
/// ONCE for the expiry (and keeps it); the epic the account states reads nothing and closes with its stated expiry;
/// a table that cannot be read closes with `-` as every close always did (never a refusal).
#[test]
fn a_close_finds_the_expiry_of_a_dated_epic_without_ever_being_refused() {
    let epic = "IX.D.DAX.FBMU1.IP";
    let mut wires = HashMap::new();
    let mut closes = HashMap::new();
    wires.insert(epic.to_string(), EpicWire { currency: "EUR".into(), expiry: "DEC-26".into() });
    let unread = || -> Result<serde_json::Value, VenueApiHttpError> {
        panic!("an opened epic reads nothing")
    };
    assert_eq!(close_expiry_with(&wires, &mut closes, epic, None, unread), "DEC-26");

    let other = "MT.D.GC.FBMU3.IP";
    let mut reads = 0;
    let mut read = || {
        reads += 1;
        Ok(dated("DEC-26"))
    };
    assert_eq!(close_expiry_with(&wires, &mut closes, other, None, &mut read), "DEC-26");
    assert_eq!(close_expiry_with(&wires, &mut closes, other, None, &mut read), "DEC-26");
    assert_eq!(reads, 1, "read once, kept");

    let fallback = FallbackEpic {
        wire: EpicWire { currency: "USD".to_string(), expiry: "MAR-27".to_string() },
        ..stated_fallback()
    };
    assert_eq!(
        close_expiry_with(&wires, &mut closes, &fallback.epic, Some(&fallback), unread),
        "MAR-27",
        "the epic the account states reads nothing and closes with the expiry it states"
    );

    let (wires, mut closes) = (HashMap::new(), HashMap::new());
    assert_eq!(
        close_expiry_with(&wires, &mut closes, "CS.D.GBPUSD.MINI.IP", None, unreadable),
        "-",
        "an unreadable table is no reason to refuse a close"
    );
    assert!(closes.is_empty(), "a failed read is not remembered");
}

// --- the reference of a create whose reply is lost ------------------------------------------------

#[test]
fn the_deal_reference_is_deterministic_valid_and_per_order_client_id() {
    let a = deal_reference("order-client-id-1");
    assert_eq!(
        a,
        deal_reference("order-client-id-1"),
        "the same order client id must always yield the same reference"
    );
    assert_ne!(a, deal_reference("order-client-id-2"));
    // IG: [A-Za-z0-9_-]{1,30}, even for an order client id far outside that alphabet or length.
    for order_client_id in
        ["order-client-id-1", "dom-12345678901234567890123456789012345", "a b/ü:é", ""]
    {
        let r = deal_reference(order_client_id);
        assert!(r.len() <= 30 && !r.is_empty(), "{r}");
        assert!(r.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'), "{r}");
    }
    // Pinned: a change would orphan every reference a previous release minted.
    assert_eq!(deal_reference(""), "vkcbf29ce484222325");
}

#[test]
fn an_open_carries_its_own_reference_and_a_close_does_not() {
    let r = req("market", 1, 1.0);
    let want = deal_reference("order-client-id-1");
    assert_eq!(build_request(&r, WIRE_CURRENCY).unwrap().2["dealReference"], want.as_str());
    assert_eq!(
        build_request(&req("limit", 1, 1.0), WIRE_CURRENCY).unwrap().2["dealReference"],
        want.as_str()
    );
    // IG's close body has no such field.
    assert!(build_close_request(&r).unwrap().2.get("dealReference").is_none());
}

#[test]
fn only_a_post_write_failure_or_a_gateway_status_is_an_unknown_outcome() {
    let e = |status| IG_HTTP.error(status, "m");
    assert!(e(VenueApiHttpError::STATUS_OUTCOME_UNKNOWN).outcome_unknown());
    for gateway in [502, 503, 504, 524] {
        assert!(e(gateway).outcome_unknown(), "{gateway}");
    }
    // 0 = provably never sent; the rest are IG's own verdict.
    for definite in [0, 400, 401, 403, 404, 500] {
        assert!(!e(definite).outcome_unknown(), "{definite}");
    }
}

fn not_found() -> Result<serde_json::Value, VenueApiHttpError> {
    Err(IG_HTTP.error(404, "error.service.execution.find"))
}

/// ⚠ An AMBIGUOUS open (the POST never answered) whose `/confirms` ladder exhausts with 404 must
/// emit NOTHING: IG answers 404 for "not processed yet" and "never seen" alike, and an id-less
/// accept would take a possibly never-received order out of the stuck-order watchdog's view.
#[test]
fn an_unacked_open_whose_ladder_exhausts_emits_nothing() {
    assert!(submit_outcome("c1", 1, true, "REF", false, &not_found()).is_empty());
    let net = Err(IG_HTTP.error(0, "network error"));
    assert!(submit_outcome("c1", 1, false, "REF", false, &net).is_empty());
}

/// Where IG DID answer the POST, the exhausted ladder stays the optimistic id-less accept.
#[test]
fn an_acked_open_whose_ladder_exhausts_is_optimistically_accepted() {
    let out = submit_outcome("c1", 1, true, "REF", true, &not_found());
    assert_matches!(out.as_slice(), [Event::OrderAccepted(a)] if a.venue_order_id.is_none());
}

/// Only an echo (the reference we asked about, or a `dealId`) accepts or rejects an unacked open.
#[test]
fn an_unacked_open_resolves_only_on_an_echoing_confirm() {
    let echo = Ok(serde_json::json!({"dealStatus":"ACCEPTED","dealReference":"REF","dealId":"D1"}));
    assert_matches!(
        submit_outcome("c1", 1, false, "REF", false, &echo).as_slice(),
        [Event::OrderAccepted(a)] if a.venue_order_id.is_some()
    );
    let by_id = Ok(serde_json::json!({"dealStatus":"ACCEPTED","dealId":"D1"}));
    assert_eq!(submit_outcome("c1", 1, false, "REF", false, &by_id).len(), 1);
    let rejected =
        Ok(serde_json::json!({"dealStatus":"REJECTED","dealReference":"REF","reason":"X"}));
    assert_matches!(
        submit_outcome("c1", 1, false, "REF", false, &rejected).as_slice(),
        [Event::OrderRejected(_)]
    );
    let other =
        Ok(serde_json::json!({"dealStatus":"ACCEPTED","dealReference":"OTHER","dealId":"D1"}));
    assert!(submit_outcome("c1", 1, false, "REF", false, &other).is_empty(), "not our reference");
    let bare = Ok(serde_json::json!({"dealStatus":"ACCEPTED"}));
    assert!(submit_outcome("c1", 1, false, "REF", false, &bare).is_empty(), "echoes nothing");
}
