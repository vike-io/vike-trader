use super::*;
use std::assert_matches;

fn req(order_type: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        data_delay: None,
        account: None,
        combo_legs: Vec::new(),
        client_order_id: "coid-1".into(),
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

/// **The capability row's `reduce_only_kinds` is this client's reduce-only gate, executed.** `submit`'s loop refuses a
/// `reduce_only` order exactly when `closing && !is_market(&req)` (`CLOSE_MARKET_ONLY`, measured on the demo host), so for
/// every kind the row accepts the row lists the kind iff `is_market` holds for it: only a market close is a reduce-only
/// order here, and a bracket's stop-loss and take-profit are refused at the core edge instead.
#[test]
fn the_row_lists_exactly_the_reduce_only_kinds_the_close_route_takes() {
    let caps = vike_venues::caps_for(VENUE);
    for kind in caps.supported_order_kinds {
        assert_eq!(
            is_market(&req(kind, 1, 1.0)),
            caps.reduce_only_kinds.contains(kind),
            "ig/{kind}: the row and the close route's gate must agree"
        );
    }
    assert!(CLOSE_MARKET_ONLY.contains("MARKET-only"), "the refusal still says what it refuses");
}

#[test]
fn market_opens_position() {
    let (path, ver, body) = build_request(&req("market", 1, 2.0), WIRE_CURRENCY);
    assert_eq!(path, "/positions/otc");
    assert_eq!(ver, "2");
    assert_eq!(body["orderType"], "MARKET");
    assert_eq!(body["direction"], "BUY");
    assert_eq!(body["size"], 2.0);
    assert_eq!(body["epic"], "CS.D.EURUSD.MINI.IP");
}

/// **The any-symbol facts admit only what this wire says**: every order body carries the currency, the
/// expiry and the five-decimal level `market_facts` fences on, from the SAME constants. A change here that
/// is not a change there would admit an epic the wire cannot express.
#[test]
fn the_wire_says_what_the_any_symbol_facts_fence_on() {
    assert_eq!((WIRE_CURRENCY, WIRE_EXPIRY), ("USD", "-"), "the bytes IG has always been sent");
    let market = build_request(&req("market", 1, 1.0), WIRE_CURRENCY).2;
    let limit = build_request(&req("limit", 1, 1.0), WIRE_CURRENCY).2;
    for body in [&market, &limit, &build_close_request(&req("market", 1, 1.0)).2] {
        assert_eq!(body["expiry"], WIRE_EXPIRY, "{body}");
    }
    for body in [&market, &limit] {
        assert_eq!(body["currencyCode"], WIRE_CURRENCY, "{body}");
    }
    assert_eq!(
        fmt_level(crate::market_facts::WIRE_PRICE_STEP),
        "0.00001",
        "the finest level the wire can write is the finest pip the facts admit"
    );
}

/// The currency on the wire is the one the epic's table gave, in BOTH open bodies, and a `USD` body is
/// byte-for-byte the one IG has always been sent (only `currencyCode` can differ).
#[test]
fn the_open_bodies_carry_the_currency_they_are_given() {
    for kind in ["market", "limit"] {
        let usd = build_request(&req(kind, 1, 1.0), "USD").2;
        let gbp = build_request(&req(kind, 1, 1.0), "GBP").2;
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

fn unreadable() -> Result<serde_json::Value, IgApiError> {
    Err(IgApiError { status: 429, message: "error.public-api.exceeded-api-key-allowance".into() })
}

/// FAIL CLOSED: a table that cannot be read is not a licence to guess `USD` for an epic that lists
/// another currency alone (IG's reference makes that a terminal reject, never observed, and a 429 on the
/// shared allowance is likeliest exactly when the exec lane is busiest). The refusal says why and that the
/// order can be retried, and it keeps nothing, so the next order reads the table again.
#[test]
fn an_unreadable_table_refuses_a_gbp_only_epic_in_words() {
    let (epic, mut kept) = ("CS.D.EURGBP.MINI.IP", HashMap::new());
    let why = epic_currency_with(&mut kept, epic, Some("USD"), unreadable).unwrap_err();
    assert!(
        why.contains(epic) && why.contains("dealing currencies") && why.contains("retry"),
        "{why}"
    );
    assert!(kept.is_empty(), "a failed read is not remembered");
    // ...and the next order reads again, and is served once the table answers
    let read_ok = || Ok(market_with(&["GBP"]));
    assert_eq!(epic_currency_with(&mut kept, epic, Some("USD"), read_ok).unwrap(), "GBP");
}

/// A good read is the epic's answer for the life of the thread: a table that goes unreadable afterwards
/// does not take a working epic down.
#[test]
fn a_previously_read_epic_is_served_from_the_cache_when_its_table_goes_unreadable() {
    let (epic, mut kept) = ("CS.D.GBPUSD.MINI.IP", HashMap::new());
    let first = epic_currency_with(&mut kept, epic, None, || Ok(market_with(&["USD"])));
    assert_eq!(first.unwrap(), "USD");
    let again = epic_currency_with(&mut kept, epic, None, || -> Result<_, _> {
        panic!("a cached epic must not be read again")
    });
    assert_eq!(again.unwrap(), "USD");
}

/// The one epic the mount is wired to (`vike_venues::venue_wiring`'s ig row: EUR/USD mini, which lists
/// USD, measured) keeps trading when its table is unreadable: it has always been written `USD`.
/// Any OTHER epic, even one that would list USD, is refused: nothing but a read says what it lists.
#[test]
fn the_wiring_pinned_epic_keeps_usd_when_its_table_is_unreadable_and_no_other_does() {
    let pinned = vike_venues::venue_wiring::wired_symbol(VENUE).expect("ig is wired");
    assert_eq!(pinned, "CS.D.EURUSD.MINI.IP");
    let mut kept = HashMap::new();
    assert_eq!(epic_currency_with(&mut kept, pinned, None, unreadable).unwrap(), "USD");
    assert!(kept.is_empty(), "a guess is not remembered");
    let other = epic_currency_with(&mut kept, "CS.D.GBPUSD.MINI.IP", None, unreadable);
    assert!(other.unwrap_err().contains("CS.D.GBPUSD.MINI.IP"));
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
    // confirm; see the function doc and `tests/offline/close_does_not_collide_with_its_open.rs`.
    assert!(body.get("dealId").is_none(), "closed by epic, so IG nets across its own deals");

    // ...and a SHORT flatten is the mirror image.
    let mut long_close = req("market", 1, 0.1);
    long_close.reduce_only = true;
    assert_eq!(build_close_request(&long_close).2["direction"], "BUY");
}

/// An OPEN still opens — `forceOpen: true` belongs on that path and only that path.
#[test]
fn a_plain_market_order_still_opens_its_own_deal() {
    let (path, ver, body) = build_request(&req("market", 1, 2.0), WIRE_CURRENCY);
    assert_eq!((path.as_str(), ver), ("/positions/otc", "2"));
    assert_eq!(body["forceOpen"], true);
}

#[test]
fn limit_is_working_order_with_level() {
    let (path, _ver, body) = build_request(&req("limit", -1, 1.0), WIRE_CURRENCY);
    assert_eq!(path, "/workingorders/otc");
    assert_eq!(body["type"], "LIMIT");
    assert_eq!(body["direction"], "SELL");
    assert_eq!(body["level"], "1.09000");
}

#[test]
fn request_tif_is_ignored_working_orders_rest_gtc() {
    // The Ignored ig row of `vike_venues::venue_tif::venue_tif`: the request TIF is NEVER
    // read — a limit asking Ioc still rests GOOD_TILL_CANCELLED, and the MARKET body
    // carries no timeInForce at all. Honoring it is a step-2 wire change behind smokes.
    let mut r = req("limit", 1, 1.0);
    r.time_in_force = vike_model::TimeInForce::Ioc;
    let (_path, _ver, body) = build_request(&r, WIRE_CURRENCY);
    assert_eq!(body["timeInForce"], "GOOD_TILL_CANCELLED");
    assert_eq!(
        vike_venues::venue_tif::venue_tif(VENUE, vike_model::TimeInForce::Ioc),
        vike_venues::venue_tif::TifOutcome::Ignored { wire: "GOOD_TILL_CANCELLED" }
    );
    let (_path, _ver, mbody) = build_request(&req("market", 1, 1.0), WIRE_CURRENCY);
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
    let got = confirm_with_retry_using(
        "REF1",
        |r| {
            assert_eq!(r, "REF1", "the same reference is re-asked, never a mutated one");
            attempts.set(attempts.get() + 1);
            if attempts.get() < CONFIRM_ATTEMPTS {
                Err(IgApiError { status: 404, message: "error.service.execution.find".into() })
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
            Err(IgApiError { status: 404, message: "error.service.execution.find".into() })
        },
        || pauses.set(pauses.get() + 1),
    )
    .expect_err("a confirm that never answers must not be reported as success");
    assert_eq!(attempts.get(), CONFIRM_ATTEMPTS, "bounded, never a loop");
    assert_eq!(pauses.get(), CONFIRM_ATTEMPTS - 1, "a pause BETWEEN attempts, none after the last");
    assert_eq!(err.status, 404);
    assert_eq!(err.message, "error.service.execution.find", "IG's own code reaches the log");
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
    let ids = DealIds::shared();
    note_deal_id(
        &ids,
        "coid-1",
        &serde_json::json!({"dealStatus": "ACCEPTED", "dealId": "DIAAA2"}),
    );
    assert_eq!(ids.lock().unwrap().deal_id_for("coid-1").as_deref(), Some("DIAAA2"));
    assert_eq!(ids.lock().unwrap().coid_for("DIAAA2").as_deref(), Some("coid-1"));

    note_deal_id(
        &ids,
        "coid-2",
        &serde_json::json!({"dealStatus": "REJECTED", "dealId": "DIAAA3", "reason": "X"}),
    );
    assert_eq!(ids.lock().unwrap().deal_id_for("coid-2"), None);
    // a confirm with no dealId changes nothing
    note_deal_id(&ids, "coid-3", &serde_json::json!({"dealStatus": "ACCEPTED"}));
    assert_eq!(ids.lock().unwrap().deal_id_for("coid-3"), None);
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

// --- the reference of a create whose reply is lost ------------------------------------------------

#[test]
fn the_deal_reference_is_deterministic_valid_and_per_coid() {
    let a = deal_reference("coid-1");
    assert_eq!(a, deal_reference("coid-1"), "the same coid must always yield the same reference");
    assert_ne!(a, deal_reference("coid-2"));
    // IG: [A-Za-z0-9_-]{1,30}, even for a coid far outside that alphabet or length.
    for coid in ["coid-1", "dom-12345678901234567890123456789012345", "a b/ü:é", ""] {
        let r = deal_reference(coid);
        assert!(r.len() <= 30 && !r.is_empty(), "{r}");
        assert!(r.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'), "{r}");
    }
    // Pinned: a change would orphan every reference a previous release minted.
    assert_eq!(deal_reference(""), "vkcbf29ce484222325");
}

#[test]
fn an_open_carries_its_own_reference_and_a_close_does_not() {
    let r = req("market", 1, 1.0);
    let want = deal_reference("coid-1");
    assert_eq!(build_request(&r, WIRE_CURRENCY).2["dealReference"], want.as_str());
    assert_eq!(
        build_request(&req("limit", 1, 1.0), WIRE_CURRENCY).2["dealReference"],
        want.as_str()
    );
    // IG's close body has no such field.
    assert!(build_close_request(&r).2.get("dealReference").is_none());
}

#[test]
fn only_a_post_write_failure_or_a_gateway_status_is_an_unknown_outcome() {
    use crate::rest::STATUS_OUTCOME_UNKNOWN;
    let e = |status| IgApiError { status, message: "m".into() };
    assert!(e(STATUS_OUTCOME_UNKNOWN).outcome_unknown());
    for gateway in [502, 503, 504, 524] {
        assert!(e(gateway).outcome_unknown(), "{gateway}");
    }
    // 0 = provably never sent; the rest are IG's own verdict.
    for definite in [0, 400, 401, 403, 404, 500] {
        assert!(!e(definite).outcome_unknown(), "{definite}");
    }
}

fn not_found() -> Result<serde_json::Value, IgApiError> {
    Err(IgApiError { status: 404, message: "error.service.execution.find".into() })
}

/// ⚠ An AMBIGUOUS open (the POST never answered) whose `/confirms` ladder exhausts with 404 must
/// emit NOTHING: IG answers 404 for "not processed yet" and "never seen" alike, and an id-less
/// accept would take a possibly never-received order out of the stuck-order watchdog's view.
#[test]
fn an_unacked_open_whose_ladder_exhausts_emits_nothing() {
    assert!(submit_outcome("c1", 1, true, "REF", false, &not_found()).is_empty());
    let net = Err(IgApiError { status: 0, message: "network error".into() });
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
