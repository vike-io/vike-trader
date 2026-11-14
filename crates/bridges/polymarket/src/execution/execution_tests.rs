use super::*;
use vike_execution::ExecutionClient;
use vike_model::OrderCancelIntent;

/// The host an offline exec thread is handed where no request may go (the `.invalid` TLD never
/// resolves).
const NO_HOST: &str = "http://unused.invalid";

#[test]
fn submit_accept_decision() {
    // success:true with an orderID → accept, carrying the id
    let ok = serde_json::json!({ "success": true, "orderID": "0xABC", "status": "live" });
    assert_eq!(accept_outcome(&ok), Ok("0xABC".to_string()));

    // success:false → reject with the errorMsg
    let bad = serde_json::json!({ "success": false, "errorMsg": "not enough balance / allowance" });
    assert_eq!(accept_outcome(&bad), Err("not enough balance / allowance".to_string()));

    // 2xx body with neither success nor orderID → reject (defensive; never blind-accept)
    let empty = serde_json::json!({});
    assert!(accept_outcome(&empty).is_err());
}

/// OFFLINE, through the REAL `ExecutionClientActor` + exec thread: a token whose NegRisk flag cannot be
/// resolved is REJECTED with a named reason — never signed against a guessed EIP-712 domain, and
/// never silently dropped (the venue-adapter contract: exactly one terminal per order).
///
/// No network: the key is valid-shaped so the EOA derives purely, and `secret` is pre-filled so
/// `ensure_l2` short-circuits. The injected resolver always says "don't know", which is the
/// production behavior when `/neg-risk` is unreachable or answers without a usable flag.
#[test]
fn unresolvable_neg_risk_rejects_the_order_offline() {
    let (tx, mut rx) = vike_execution::event_channel(64);
    let mut client = spawn_live(
        PolymarketLiveConfig {
            creds: PolymarketConfig {
                private_key: "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4"
                    .to_string()
                    .into(),
                // non-empty ⇒ `ensure_l2` returns Ok immediately, so this test makes NO call
                secret: "not-a-real-secret".to_string().into(),
                ..Default::default()
            },
            maker: String::new(),
            signature_type: SignatureType::Eoa,
            neg_risk: NegRiskSource::lookup_with(
                std::collections::HashMap::new(),
                Box::new(|_| None),
            ),
            registry: OrderVenueIdTracker::new(),
            tracker: None,
            user_channel: None, // no socket, no extra thread
            builder_code: [0u8; 32],
            presubmit_register: false,
            rate_gate: false,
            tick_regime: None, // no transport, no rounding, no re-fetch
        },
        tx,
        NO_HOST,
        NO_HOST,
        NO_HOST,
    );
    client.submit(&OrderRequest {
        order_client_id: "order-client-id-nr".to_string(),
        venue: "polymarket".to_string(),
        symbol: "999".to_string(),
        side: 1,
        qty: 10.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(0.01),
        ..Default::default()
    });
    client.detach();

    let mut seen = Vec::new();
    while let Ok(vike_execution::Ingest::Event(e)) = rx.try_recv() {
        seen.push(e);
    }
    assert!(
        seen.iter().any(
            |e| matches!(e, Event::OrderSubmitted(s) if s.order_client_id == "order-client-id-nr")
        ),
        "the synchronous OrderSubmitted still fires: {seen:?}"
    );
    let rejected = seen
        .iter()
        .find_map(|e| match e {
            Event::OrderRejected(r) if r.order_client_id == "order-client-id-nr" => Some(r),
            _ => None,
        })
        .expect("exactly one terminal — an OrderRejected naming the reason");
    assert!(
        rejected.reason.contains("NegRisk"),
        "the reason must name the real cause, not a generic failure: {}",
        rejected.reason
    );
    assert!(
        !seen.iter().any(|e| matches!(e, Event::OrderAccepted(_))),
        "an unresolvable domain must never reach the wire"
    );
}

/// `history_rows` accepts BOTH CLOB wire shapes and degrades to an empty replay rather than
/// panicking on the resync thread.
#[test]
fn history_rows_tolerates_both_wire_shapes() {
    let bare = serde_json::json!([{ "id": "1" }]);
    assert_eq!(history_rows(&bare), &bare);
    let paged = serde_json::json!({ "data": [{ "id": "1" }], "next_cursor": "LTE=" });
    assert_eq!(history_rows(&paged), &serde_json::json!([{ "id": "1" }]));
    assert!(history_rows(&serde_json::json!({ "error": "nope" })).is_null());
    assert!(history_rows(&serde_json::Value::Null).is_null());
}

#[test]
fn time_in_force_to_order_type() {
    // Pins the routed polymarket row of `vike_venues::venue_time_in_force::venue_time_in_force` byte-for-byte.
    // NOTE the Ioc→FOK fold: the OPPOSITE direction of hyperliquid's Fok→Ioc (step-2 flip).
    assert_eq!(order_type_of(TimeInForce::Gtc), "GTC");
    assert_eq!(order_type_of(TimeInForce::Fok), "FOK");
    assert_eq!(order_type_of(TimeInForce::Ioc), "FOK");
    assert_eq!(order_type_of(TimeInForce::Gtd), "GTD");
    assert_eq!(order_type_of(TimeInForce::Day), "GTC");
}

/// **WIRE-BODY pin.** The tif table's own tests are declaration-vs-declaration — they assert
/// `venue_time_in_force("polymarket", Gtd) == Mapped("GTD")` against a matrix that repeats the same
/// claim — so they stayed GREEN while the builder hardcoded `"expiration": "0"` and never read
/// `gtd_expiry`, shipping `orderType: "GTD"` with a GTC expiry to a live market. This asserts
/// on the JSON that actually goes on the wire, which is the only level that catches it.
#[test]
fn gtd_wire_body_carries_the_expiry_and_never_the_gtc_zero() {
    let order = build_order(
        0.52,
        100.0,
        OrderSideWire::Buy,
        "71321045679252212594626385532706912750332728571942532289631379312455583992563",
        "0xmaker",
        "0xsigner",
        SignatureType::PolymarketProxy,
        1_700_000_000_000,
        7,
        false,
        [0u8; 32],
    );
    let now_ms = 1_700_000_000_000_i64;

    // GTD with an hour of lead → the REAL deadline reaches the wire, in unix SECONDS.
    let deadline_ms = now_ms + 3_600_000;
    let secs = expiration_secs_of(TimeInForce::Gtd, Some(deadline_ms), now_ms)
        .expect("an hour of lead clears the floor");
    let body = order_to_json(&order, "0xsig", secs);
    assert_eq!(body["expiration"], (deadline_ms / 1_000).to_string());
    assert_ne!(body["expiration"], "0", "THE BUG: a GTD order shipping the GTC expiry");
    assert_eq!(order_type_of(TimeInForce::Gtd), "GTD", "and it rides with orderType GTD");

    // …while every OTHER tif keeps the historical `"0"` byte-for-byte. GTC/FOK/IOC/Day are the
    // only orders this venue has ever actually sent, and this fix must not move any of them.
    for tif in [TimeInForce::Gtc, TimeInForce::Fok, TimeInForce::Ioc, TimeInForce::Day] {
        assert_eq!(expiration_secs_of(tif, None, now_ms), Ok(0), "{tif:?}");
        // a stray gtd_expiry on a non-GTD request is ignored, never smuggled onto the wire
        assert_eq!(expiration_secs_of(tif, Some(deadline_ms), now_ms), Ok(0), "{tif:?}");
        assert_eq!(order_to_json(&order, "0xsig", 0)["expiration"], "0", "{tif:?}");
    }
}

/// The client-side floor, at the boundary: `now + `[`GTD_MIN_LEAD_SECS`] is accepted and one
/// second nearer is not, a GTD with no expiry at all is refused, and a mid-second deadline
/// truncates DOWN so it can never round up past the bound.
///
/// ⚠ 180s is TRANSCRIBED from the `@polymarket/client` SDK constant, not confirmed against a
/// live venue response. It is applied as a client-side floor precisely so that a wrong value
/// costs a local refusal of an order the venue might have taken — never a malformed wire body.
#[test]
fn gtd_expiry_inside_the_venue_floor_is_refused_locally() {
    let now_ms = 1_700_000_000_000_i64;
    let floor_secs = now_ms / 1_000 + GTD_MIN_LEAD_SECS;

    assert_eq!(
        expiration_secs_of(TimeInForce::Gtd, Some(floor_secs * 1_000), now_ms),
        Ok(u64::try_from(floor_secs).unwrap()),
        "exactly at the floor is accepted"
    );
    let err = expiration_secs_of(TimeInForce::Gtd, Some((floor_secs - 1) * 1_000), now_ms)
        .expect_err("one second inside the floor");
    assert!(err.contains("180"), "the reason must name the bound: {err}");

    // the case the old builder silently turned into `expiration: "0"`
    let err = expiration_secs_of(TimeInForce::Gtd, None, now_ms).expect_err("no expiry at all");
    assert!(err.contains("gtd_expiry"), "the reason must name the missing field: {err}");

    assert!(
        expiration_secs_of(TimeInForce::Gtd, Some(floor_secs * 1_000 - 1), now_ms).is_err(),
        "a mid-second deadline truncates down, never up past the floor"
    );
}

/// OFFLINE, through the REAL `ExecutionClientActor` + exec thread — the sibling of
/// `unresolvable_neg_risk_rejects_the_order_offline`: a GTD request with no expiry gets exactly
/// one terminal, a LOCAL `OrderRejected`, and never reaches the wire.
///
/// The injected neg-risk resolver is the always-`None` one, so a reason naming `gtd_expiry`
/// rather than NegRisk ALSO proves the expiry gate runs before the signing-domain lookup — i.e.
/// before any signing or network work, which is the placement that makes it a local refusal.
#[test]
fn gtd_without_an_expiry_never_reaches_the_wire() {
    let (tx, mut rx) = vike_execution::event_channel(64);
    let mut client = spawn_live(
        PolymarketLiveConfig {
            creds: PolymarketConfig {
                private_key: "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4"
                    .to_string()
                    .into(),
                // non-empty ⇒ `ensure_l2` returns Ok immediately, so this test makes NO call
                secret: "not-a-real-secret".to_string().into(),
                ..Default::default()
            },
            maker: String::new(),
            signature_type: SignatureType::Eoa,
            neg_risk: NegRiskSource::lookup_with(
                std::collections::HashMap::new(),
                Box::new(|_| None),
            ),
            registry: OrderVenueIdTracker::new(),
            tracker: None,
            user_channel: None,
            builder_code: [0u8; 32],
            presubmit_register: false,
            rate_gate: false,
            tick_regime: None,
        },
        tx,
        NO_HOST,
        NO_HOST,
        NO_HOST,
    );
    client.submit(&OrderRequest {
        order_client_id: "order-client-id-gtd".to_string(),
        venue: "polymarket".to_string(),
        symbol: "999".to_string(),
        side: 1,
        qty: 10.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(0.01),
        time_in_force: TimeInForce::Gtd,
        gtd_expiry: None,
        ..Default::default()
    });
    client.detach();

    let mut seen = Vec::new();
    while let Ok(vike_execution::Ingest::Event(e)) = rx.try_recv() {
        seen.push(e);
    }
    assert!(
        seen.iter().any(
            |e| matches!(e, Event::OrderSubmitted(s) if s.order_client_id == "order-client-id-gtd")
        ),
        "the synchronous OrderSubmitted still fires first, per the emitter split: {seen:?}"
    );
    let rejected = seen
        .iter()
        .find_map(|e| match e {
            Event::OrderRejected(r) if r.order_client_id == "order-client-id-gtd" => Some(r),
            _ => None,
        })
        .expect("exactly one terminal — an OrderRejected naming the reason");
    assert!(
        rejected.reason.contains("gtd_expiry"),
        "the reason must name the missing expiry (and so prove the gate ran BEFORE the \
             NegRisk lookup this config can never satisfy): {}",
        rejected.reason
    );
    assert!(
        !seen.iter().any(|e| matches!(e, Event::OrderAccepted(_))),
        "a GTD with no expiry must never reach the wire"
    );
}

/// **A MARKET order that names a price is refused here, in words, before anything is signed.** The CLOB has one submit
/// path and every order is a signed limit at its price, so this order would REST or fill at the number it names while
/// the node's ceilings size a market order at the mark, which under-sizes it. The terminal is the one `OrderRejected`
/// the contract owes, after the synchronous `OrderSubmitted`. No call is made: the always-`None` NegRisk resolver
/// answers a market order that names NO price (the shape `Flatten` and `MarketExit` rely on), so its reason proves
/// that shape is not this refusal's.
#[test]
fn a_market_order_that_names_a_price_is_refused_before_anything_is_signed() {
    let (tx, mut rx) = vike_execution::event_channel(64);
    let mut client = spawn_live(
        PolymarketLiveConfig {
            creds: PolymarketConfig {
                private_key: "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4"
                    .to_string()
                    .into(),
                secret: "not-a-real-secret".to_string().into(),
                ..Default::default()
            },
            maker: String::new(),
            signature_type: SignatureType::Eoa,
            neg_risk: NegRiskSource::lookup_with(
                std::collections::HashMap::new(),
                Box::new(|_| None),
            ),
            registry: OrderVenueIdTracker::new(),
            tracker: None,
            user_channel: None,
            builder_code: [0u8; 32],
            presubmit_register: false,
            rate_gate: false,
            tick_regime: None,
        },
        tx,
        NO_HOST,
        NO_HOST,
        NO_HOST,
    );
    let market = |order_client_id: &str, price: Option<f64>| OrderRequest {
        order_client_id: order_client_id.to_string(),
        venue: "polymarket".to_string(),
        symbol: "999".to_string(),
        side: -1,
        qty: 10.0,
        order_type: vike_model::OrderType::Market,
        price,
        ..Default::default()
    };
    client.submit(&market("order-client-id-priced", Some(0.5)));
    client.submit(&market("order-client-id-bare", None));
    client.detach();

    let mut seen = Vec::new();
    while let Ok(vike_execution::Ingest::Event(e)) = rx.try_recv() {
        seen.push(e);
    }
    let reason_of = |order_client_id: &str| {
        seen.iter()
            .find_map(|e| match e {
                Event::OrderRejected(r) if r.order_client_id == order_client_id => {
                    Some(r.reason.to_string())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("exactly one terminal for {order_client_id}: {seen:?}"))
    };
    assert!(
        seen.iter()
            .any(|e| matches!(e, Event::OrderSubmitted(s) if s.order_client_id == "order-client-id-priced")),
        "the synchronous OrderSubmitted still fires first: {seen:?}"
    );
    let priced = reason_of("order-client-id-priced");
    assert!(priced.contains("market order") && priced.contains("limit"), "{priced}");
    assert!(reason_of("order-client-id-bare").contains("NegRisk"), "{seen:?}");
    assert!(!seen.iter().any(|e| matches!(e, Event::OrderAccepted(_))), "{seen:?}");
}

/// The safety decision of the whole bulk lane, both halves. `WholeBook` is an ASSERTION that
/// the account-wide `cancel-all` is equivalent to cancelling exactly these ids, so a batch that
/// does not name this start's whole known book — or that named an id which did not resolve, and
/// is therefore reaching outside it — must stay `Subset`, which `plan_cancels` can never take
/// to the account-wide arm however much budget is free.
#[test]
fn only_a_fully_resolved_whole_book_batch_may_assert_the_account_wide_scope() {
    assert_eq!(batch_scope(true, 3, 3), CancelScope::WholeBook);
    assert_eq!(batch_scope(false, 3, 3), CancelScope::Subset, "not the whole book");
    assert_eq!(batch_scope(true, 2, 3), CancelScope::Subset, "an id did not resolve");
    assert_eq!(batch_scope(false, 2, 3), CancelScope::Subset);
}

/// OFFLINE, through the REAL `ExecutionClientActor` + exec thread — the batch door's sibling of
/// `unresolvable_neg_risk_rejects_the_order_offline`. A batch of order client ids this start never
/// registered resolves to nothing, so the arm refuses each id and returns before any network
/// work: it proves the batch crossed the seam as ONE command and was still reported PER ID,
/// with the same wording one lone cancel of the same order would carry. Non-terminal, because
/// an order we cannot address is not an order we cancelled.
#[test]
fn a_batch_of_unregistered_order_client_ids_is_refused_per_id_offline() {
    let (tx, mut rx) = vike_execution::event_channel(64);
    let mut client = spawn_live(
        PolymarketLiveConfig {
            creds: PolymarketConfig {
                private_key: "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4"
                    .to_string()
                    .into(),
                // non-empty ⇒ `ensure_l2` returns Ok immediately, so this test makes NO call
                secret: "not-a-real-secret".to_string().into(),
                ..Default::default()
            },
            maker: String::new(),
            signature_type: SignatureType::Eoa,
            neg_risk: NegRiskSource::lookup_with(
                std::collections::HashMap::new(),
                Box::new(|_| None),
            ),
            registry: OrderVenueIdTracker::new(),
            tracker: None,
            user_channel: None,
            builder_code: [0u8; 32],
            presubmit_register: false,
            rate_gate: false,
            tick_regime: None,
        },
        tx,
        NO_HOST,
        NO_HOST,
        NO_HOST,
    );
    // RiskOff: the reserve may not shed it, so nothing here can be a shed rather than a miss.
    client.cancel_batch_with_intent(
        &["gone-1".to_string(), "gone-2".to_string()],
        OrderCancelIntent::RiskOff,
    );
    client.detach();

    let mut seen = Vec::new();
    while let Ok(vike_execution::Ingest::Event(e)) = rx.try_recv() {
        seen.push(e);
    }
    let refused: Vec<(String, String)> = seen
        .iter()
        .map(|e| match e {
            Event::OrderCancelRejected(r) => (r.order_client_id.clone(), r.reason.to_string()),
            other => panic!("an unaddressable cancel must be NON-terminal: {other:?}"),
        })
        .collect();
    assert_eq!(refused.len(), 2, "one advisory per id, not one per batch: {refused:?}");
    for (order_client_id, reason) in &refused {
        assert!(reason.contains(order_client_id), "the reason must name the order: {reason}");
        assert!(reason.contains("no venue order id"), "single-arm wording: {reason}");
    }
}

/// The DEFAULT-OFF ack-race close (`POLY_PRESUBMIT_REGISTER`), proven at the exact granularity
/// the submit arm injects it — no signing, no network: `build_order` + `derive_order_id` +
/// `on_accept` + `lookup_clob` are the four pure calls the `if presubmit_register { … }` block
/// plus the user-WS pump make. It builds the SAME `Order` the exec thread would for a request,
/// derives its CLOB id pre-submit, registers order client id↔id BEFORE any ack, and confirms a fill/cancel
/// that BEATS the ack (a `lookup_clob` on that derived id, the pump's real re-key path) already
/// resolves to our order client id + side. It also pins the OFF state (nothing keyed before the register)
/// and the idempotent duplicate ack (the real `OrderAccepted`, same id + side, changes nothing).
#[test]
fn presubmit_register_lets_a_fill_beat_the_ack() {
    let registry = OrderVenueIdTracker::new();
    let order_client_id = "order-client-id-presub";
    let request_side = 1_i32; // BUY

    // exactly the Order the submit arm builds for this request (same call, same args) …
    let order = build_order(
        0.52,
        100.0,
        if request_side >= 0 { OrderSideWire::Buy } else { OrderSideWire::Sell },
        "71321045679252212594626385532706912750332728571942532289631379312455583992563",
        "0xmaker",
        "0xsigner",
        SignatureType::PolymarketProxy,
        1_700_000_000_000,
        7,
        false,
        [0u8; 32],
    );
    let clob_id = derive_order_id(&order);
    let side_code = if request_side >= 0 { 1 } else { -1 };

    // OFF / pre-injection: with `POLY_PRESUBMIT_REGISTER` unset the block never runs, so the
    // derived id is not keyed yet — byte-identical to before the flag (the ack is sole writer).
    assert_eq!(registry.lookup_clob(&clob_id), None);
    assert_eq!(registry.order_client_id_to_clob(order_client_id), None);

    // ON: exactly what the submit arm's `if presubmit_register { … }` block does —
    // `registry.on_accept(order client id, &derive_order_id(&order), side)`. Nothing raced this one, so the
    // park hands nothing back; asserting that (rather than discarding the `#[must_use]`) is what
    // pins "pre-registering an order does not, by itself, replay anything".
    assert!(
        registry.on_accept(order_client_id, &clob_id, side_code).is_empty(),
        "nothing was parked"
    );

    // a fill/cancel that BEAT the HTTP ack lands on the user-WS keyed by the CLOB order id,
    // which is exactly this derived id → the pump's `lookup_clob` re-keys it to our order client id + side.
    assert_eq!(registry.lookup_clob(&clob_id), Some((order_client_id.to_string(), 1)));
    // and the cancel direction (order client id → venue id) is live before any ack, too.
    assert_eq!(registry.order_client_id_to_clob(order_client_id), Some(clob_id.clone()));

    // the real server `OrderAccepted` carries the SAME id (== derived) + side → idempotent no-op,
    // and the park makes that idempotency TOTAL: the second call finds it empty and claims nothing,
    // so the duplicate ack cannot re-emit a fill the first one already replayed.
    assert!(
        registry.on_accept(order_client_id, &clob_id, side_code).is_empty(),
        "the duplicate ack claims nothing"
    );
    assert_eq!(registry.lookup_clob(&clob_id), Some((order_client_id.to_string(), 1)));
    assert_eq!(registry.order_client_id_to_clob(order_client_id), Some(clob_id));
}

// ---- the dynamic tick-size regime, at the exact seam the exec thread consumes it ------------
//
// The submit arm's two tick-regime touches ARE `grid_price` (the price `build_order` is handed)
// and `observe_submit_outcome` (the terminal `Event` fed back to the cache), so driving those two
// with the crate's REST doubles (`instrument::tick_regime::stubs`) exercises the wiring itself rather than a
// paraphrase of it. The submit that sits between them is a signed network call, so an end-to-end
// exec-thread test cannot reach it offline — `unresolvable_neg_risk_rejects_the_order_offline`
// above is the exec-thread-level coverage, and it pins the `tick_regime: None` path.

use crate::instrument::tick_regime::stubs::{DeadStub, TickStub};

/// Rounded prices compare with a tolerance, NOT `==`: `n * tick` is a floating-point product
/// whose last bit depends on the multiplier, and nothing here is a parity fixture — the property
/// under test is "which grid was used", not a bit pattern.
fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-12
}

/// Exactly the `OrderRejected` the submit arm's `reject(...)` closure builds, so the tests feed
/// the wiring the same event shape production does.
fn rejected(reason: &str) -> Event {
    Event::OrderRejected(OrderRejected {
        cause: None,
        order_client_id: "order-client-id-tick".to_string(),
        reason: reason.to_string().into(),
        timestamp: 0,
    })
}

fn grid<T: RestTransport>(regime: TickRegime, transport: T) -> TickGrid<T> {
    TickGrid { regime, transport, clob: NO_HOST.to_string() }
}

/// NO GUESSED TICK (owner, 2026-11-13): with a regime threaded, an order on a token whose tick lookup
/// fails is refused in words that name why, and the next order asks again; once the lookup answers, the
/// tick is cached and the order is signed (no refusal). Without a regime nothing is asked.
#[test]
fn an_order_on_a_token_with_no_tick_is_refused_until_its_lookup_answers() {
    let regime = TickRegime::new();
    let dead = grid(regime.clone(), DeadStub::default());
    let why = tick_unknown(Some(&dead), "111").expect("no tick, no order");
    assert_eq!(
        why,
        "polymarket: 111: properties not loaded (neither its /tick-size lookup nor the /markets walk \
         resolved its tick); the order was NOT sent, and the properties are fetched again"
    );
    let asked = dead.transport.calls();
    assert!(tick_unknown(Some(&dead), "111").is_some(), "still refused");
    assert!(dead.transport.calls() > asked, "the next order asks again");

    let live = grid(regime.clone(), TickStub::new(0.001));
    assert_eq!(tick_unknown(Some(&live), "111"), None, "the lookup answered: the order flows");
    assert_eq!(regime.tick_size("111"), Some(0.001), "cached for the next order");
    assert_eq!(tick_unknown(None::<&TickGrid<DeadStub>>, "111"), None, "no regime, nothing asked");
}

/// THE FIX: a maker quoting a longshot on a stale cent grid is refused, the refusal re-fetches
/// the tick ONCE, and the very next order is priced on the tightened grid — the reject loop the
/// venue never tells us about is broken after exactly one rejected order.
#[test]
fn an_off_grid_reject_refetches_once_and_reprices_the_next_order() {
    let g = grid(TickRegime::new(), TickStub::new(0.001));
    g.regime.set("111", 0.01); // the stale cent grid the maker was quoting on

    let stale = grid_price(Some(&g), "111", 0.9636);
    assert!(approx(stale, 0.96), "the stale cent grid prices 0.9636 at 0.96, got {stale}");

    assert_eq!(
        observe_submit_outcome(Some(&g), "111", &rejected("invalid tick size")),
        Some(0.001),
        "the off-grid refusal resolved the grid the venue is now enforcing"
    );
    assert_eq!(g.transport.calls(), 1, "exactly one direct /tick-size lookup, no /markets walk");

    let fresh = grid_price(Some(&g), "111", 0.9636);
    assert!(approx(fresh, 0.964), "the next order is priced on the new grid, got {fresh}");
    assert_eq!(g.transport.calls(), 1, "and pricing itself never costs a REST call");
}

/// Every non-off-grid outcome is a no-op that costs NO REST round-trip — the balance/allowance
/// refusal is the one that would otherwise put a `/tick-size` GET on the reject path of an
/// under-funded account, and an ACCEPTED order must obviously never trigger one.
#[test]
fn an_unrelated_reject_or_an_acceptance_costs_no_rest_call() {
    let g = grid(TickRegime::new(), TickStub::new(0.001));
    g.regime.set("111", 0.01);

    for event in [
        rejected("not enough balance / allowance"),
        rejected("cannot resolve the NegRisk signing domain for token 111"),
        rejected("submit response missing orderID"),
        Event::OrderAccepted(OrderAccepted {
            order_client_id: "order-client-id-tick".to_string(),
            venue_order_id: Some("0xABC".into()),
            timestamp: 0,
        }),
    ] {
        assert_eq!(observe_submit_outcome(Some(&g), "111", &event), None, "{event:?}");
    }
    assert_eq!(g.transport.calls(), 0, "no REST traffic on any of them");
    assert_eq!(g.regime.tick_size("111"), Some(0.01), "and the cached grid is untouched");
}

/// A re-fetch that resolves NOTHING (the Dublin proxy dropping the request) must leave the
/// known-good grid alone rather than defaulting it to `0.01` — otherwise one transient blip
/// silently re-prices every subsequent order onto a grid the venue is not enforcing.
#[test]
fn a_failed_refetch_preserves_the_grid_the_next_order_is_priced_on() {
    let g = grid(TickRegime::new(), DeadStub::default());
    g.regime.set("111", 0.001); // the known-good tightened grid

    assert_eq!(observe_submit_outcome(Some(&g), "111", &rejected("invalid tick size")), None);
    assert!(g.transport.calls() >= 2, "the direct lookup AND the paged fallback were tried");
    assert_eq!(g.regime.tick_size("111"), Some(0.001), "never clobbered by a guess");
    let after = grid_price(Some(&g), "111", 0.9636);
    assert!(approx(after, 0.964), "so pricing still uses the surviving grid, got {after}");
}

/// An UNKNOWN token is priced VERBATIM: a regime that has never resolved this token must not
/// guess it onto the venue default, which would silently move a price the caller chose. This is
/// also why a freshly-mounted (empty) regime is inert until the venue itself teaches it.
#[test]
fn an_unknown_token_is_priced_verbatim() {
    let g = grid(TickRegime::new(), TickStub::new(0.001));
    // no arithmetic runs on an untouched price, so `==` IS exact here
    assert_eq!(grid_price(Some(&g), "999", 0.96351), 0.96351);
    assert_eq!(g.regime.tick_size("999"), None, "and it stays unknown, not born on a grid");
    assert_eq!(g.transport.calls(), 0, "pricing an unknown token is not a lookup");
}

/// `tick_regime: None` — every entry point but the live start — is byte-identical: the price is
/// passed through untouched and no reject is ever observed (there is no transport to observe it
/// with, which is exactly why `run` builds none).
#[test]
fn without_a_regime_the_submit_path_is_byte_identical() {
    let none: Option<&TickGrid<TickStub>> = None;
    for price in [0.9636, 0.5, 0.0, 1.0] {
        assert_eq!(grid_price(none, "111", price), price);
    }
    assert_eq!(observe_submit_outcome(none, "111", &rejected("invalid tick size")), None);
}

/// The DEFAULT polymarket account's credential fields, out of the smoke store — the account at tier
/// `live`, the only tier this venue has (decision 0156 item 6: a credential belongs to its account,
/// and no name is composed here).
fn live_account(
    dir: &vike_secrets::account_directory::AccountDirectory,
) -> vike_secrets::account_credentials::AccountCredentials<'_> {
    dir.credentials_for(
        crate::VENUE,
        &vike_model::accounts::account_keys::AccountLabel::Default,
        vike_model::AccountTier::Live,
    )
}

/// LIVE, real-money (Polygon mainnet): place a TINY BUY at 0.01 — far below any real mid so it
/// rests and CANNOT fill — then cancel. Live acceptance proves the V2 order signature is
/// byte-correct. Order placement is geo-blocked, so run WITH the Dublin proxy configured as
/// `venue.polymarket.*` rows in the settings database (see `crate::transport::transport_tests`'s
/// `declare_from_the_settings_database`):
/// `cargo test -p vike-polymarket --features polymarket \
///   --lib live_place_and_cancel -- --ignored --nocapture`
#[test]
#[ignore = "LIVE real-money: places+cancels a tiny non-filling order on Polymarket mainnet"]
fn live_place_and_cancel() {
    vike_log::test_init();
    crate::transport::transport_tests::declare_from_the_settings_database(
        std::env::var("VIKE_SETTINGS_DIR").ok().as_deref(),
    );
    use crate::PolymarketConfig;
    use crate::ensure_l2;
    use crate::{
        OrderSideWire, SignatureType, build_order, cancel_order, cancel_order_relayer,
        order_to_json, sign_order, sign_order_1271, submit_order, submit_order_relayer,
    };
    use vike_bridge_core::eip712::eth_address_from_private_key;
    // The release's seed row: a smoke reads no store.
    let urls = vike_venues::venue_urls::VenueUrls::seed(crate::VENUE);
    let clob = crate::config::clob_base(&urls).expect("the seed's CLOB row");

    // 1. real creds (the live polymarket account's fields) → EOA + L2
    let dir = vike_secrets::smoke_directory();
    let account = live_account(&dir);
    let pk = account.get("PRIVATE_KEY").expect("the live account's PRIVATE_KEY").to_string();
    let relayer_address = account.get("RELAYER_API_KEY_ADDRESS").unwrap_or_default().to_string();
    let relayer_key = account.get("RELAYER_API_KEY").unwrap_or_default().to_string();
    let signer = eth_address_from_private_key(&pk).expect("EOA");
    let mut creds = PolymarketConfig {
        private_key: pk.clone().into(),
        address: signer.clone(),
        ..Default::default()
    };
    let boot = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        as i64;
    ensure_l2(&mut creds, clob, boot).expect("derive L2");
    // The account's funder is a Polymarket DEPOSIT WALLET → signatureType POLY_1271, with
    // maker == signer == the deposit wallet (NOT the EOA); the EOA key is the authorized signer
    // the deposit-wallet contract recognises. Orders route via the relayer (gasless).
    let deposit_wallet = account.get("FUNDER").expect("the live account's FUNDER").to_string();
    let sig_type = crate::reconcile::signature_type_for_account(&account);
    // for POLY_1271 the order's `signer` field is the deposit wallet itself (verifyingContract).
    let order_signer = if sig_type == SignatureType::Polymarket1271 {
        deposit_wallet.clone()
    } else {
        signer.clone()
    };
    tracing::info!(target: "vike_polymarket::client", "EOA(auth key)={signer}  maker=signer={deposit_wallet}  sigType={sig_type:?}");

    // 2. a liquid token with a safe mid (reads via the proxy — clob is DNS-blocked here)
    let markets = crate::transport::get_json(clob, "/sampling-markets", "").expect("markets");
    let empty = Vec::new();
    let data = markets.get("data").and_then(|d| d.as_array()).unwrap_or(&empty);
    let mut chosen: Option<(String, bool, f64)> = None;
    'outer: for m in data {
        let neg_risk = m.get("neg_risk").and_then(|n| n.as_bool()).unwrap_or(false);
        for tk in m.get("tokens").and_then(|t| t.as_array()).unwrap_or(&empty) {
            let Some(tid) = tk.get("token_id").and_then(|x| x.as_str()).filter(|s| !s.is_empty())
            else {
                continue;
            };
            let mid = crate::transport::get_json(clob, "/midpoint", &format!("token_id={tid}"))
                .ok()
                .and_then(|v| {
                    v.get("mid").and_then(|m| m.as_str()).and_then(|s| s.parse::<f64>().ok())
                });
            if let Some(mid) = mid
                && (0.15..0.85).contains(&mid)
            {
                chosen = Some((tid.to_string(), neg_risk, mid));
                break 'outer;
            }
        }
    }
    let (token_id, neg_risk, mid) = chosen.expect("a token with a safe midpoint");
    tracing::info!(target: "vike_polymarket::client", "token={token_id}  neg_risk={neg_risk}  mid={mid}");

    // 3. tiny BUY @ 0.01 (mid > 0.15 → CANNOT fill), ~$1.20 notional
    let price = 0.01;
    let size = (1.2_f64 / price).ceil();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    // salt MUST be ≤ 2^53-1 (the wire carries it as a JSON number); a wider salt makes the
    // server rebuild a different EIP-712 hash → "invalid signature". Match the SDK's 53-bit cap.
    let salt = now.as_nanos() & ((1u128 << 53) - 1);
    // builder code for fee attribution, resolved through the SAME path production uses
    // (`live_runtime_for_account`): `attribution_code_from` + `decode_builder_bytes32`. Malformed/
    // absent degrades to `[0u8; 32]` (unattributed) rather than failing the smoke. The code is a
    // DEPLOYMENT name, read out of the smoke's named values.
    let vars = vike_secrets::smoke_vars();
    let builder_code = vike_secrets::credentials::attribution_code_from(&vars, "polymarket")
        .and_then(decode_builder_bytes32)
        .unwrap_or([0u8; 32]);
    let order = build_order(
        price,
        size,
        OrderSideWire::Buy,
        &token_id,
        &deposit_wallet,
        &order_signer,
        sig_type,
        now.as_millis(),
        salt,
        neg_risk,
        builder_code,
    );
    tracing::info!(target: "vike_polymarket::client", "BUY {size} @ {price}  makerAmt={} takerAmt={}", order.maker_amount, order.taker_amount);
    // deposit wallet (POLY_1271) signs via the ERC-1271 wrapper; else a plain V2 signature.
    let sig = if sig_type == SignatureType::Polymarket1271 {
        sign_order_1271(&order, &pk).expect("sign 1271")
    } else {
        sign_order(&order, &pk).expect("sign")
    };
    let order_obj = order_to_json(&order, &sig, 0); // GTC smoke → "no expiry"
    tracing::debug!(target: "vike_polymarket::client", "payload order = {order_obj}");
    // deposit-wallet orders route through the relayer (gasless); else the plain submit.
    let submitted = if sig_type == SignatureType::Polymarket1271 {
        submit_order_relayer(clob, &creds, order_obj, "GTC", &relayer_key, &relayer_address, false)
    } else {
        submit_order(clob, &creds, order_obj, "GTC", false)
    };
    match submitted {
        Ok(response) => {
            tracing::info!(target: "vike_polymarket::client", "✅ SUBMIT ACCEPTED — order signature valid: {response}");
            if let Some(oid) =
                response.get("orderID").or_else(|| response.get("orderId")).and_then(|o| o.as_str())
            {
                // LIVE VERIFICATION of the `derive_order_id` premise: the CLOB returns the
                // EIP-712 order hash as `orderID`, so the value computable pre-submit must equal
                // it. Proving this on demo is the gate for wiring `order_client_id`↔`clob_id`
                // pre-registration into the exec submit path (registry.on_accept before the ack).
                let derived = crate::execution::order::derive_order_id(&order);
                assert!(
                    oid.eq_ignore_ascii_case(&derived),
                    "CLOB orderID {oid} must equal the derived EIP-712 order hash {derived}"
                );
                let cancelled = if sig_type == SignatureType::Polymarket1271 {
                    cancel_order_relayer(clob, &creds, oid, &relayer_key, &relayer_address)
                } else {
                    cancel_order(clob, &creds, oid)
                };
                match cancelled {
                    Ok(c) => {
                        tracing::info!(target: "vike_polymarket::client", "✅ CANCELLED clean: {c}")
                    }
                    Err(e) => {
                        tracing::warn!(target: "vike_polymarket::client", "⚠ CANCEL FAILED — cancel {oid} MANUALLY: {e}")
                    }
                }
            }
        }
        Err(e) => tracing::error!(target: "vike_polymarket::client", "❌ SUBMIT REJECTED: {e}"),
    }
}

/// Cancel one resting order by id (deposit-wallet/relayer flow). Run with the Dublin proxy:
/// `POLY_CANCEL_ORDER_ID=0x… cargo test -p vike-polymarket --features polymarket --lib \
///   live_cancel_order -- --ignored --nocapture`
#[test]
#[ignore = "LIVE: cancels the POLY_CANCEL_ORDER_ID order on Polymarket mainnet"]
fn live_cancel_order() {
    vike_log::test_init();
    crate::transport::transport_tests::declare_from_the_settings_database(
        std::env::var("VIKE_SETTINGS_DIR").ok().as_deref(),
    );
    use crate::{PolymarketConfig, cancel_order_relayer, ensure_l2};
    use vike_bridge_core::eip712::eth_address_from_private_key;
    // The release's seed row: a smoke reads no store.
    let urls = vike_venues::venue_urls::VenueUrls::seed(crate::VENUE);
    let clob = crate::config::clob_base(&urls).expect("the seed's CLOB row");
    let oid = std::env::var("POLY_CANCEL_ORDER_ID").expect("set POLY_CANCEL_ORDER_ID");
    let dir = vike_secrets::smoke_directory();
    let account = live_account(&dir);
    let pk = account.get("PRIVATE_KEY").expect("the live account's PRIVATE_KEY").to_string();
    let relayer_address = account.get("RELAYER_API_KEY_ADDRESS").unwrap_or_default().to_string();
    let relayer_key = account.get("RELAYER_API_KEY").unwrap_or_default().to_string();
    let signer = eth_address_from_private_key(&pk).expect("EOA");
    let mut creds =
        PolymarketConfig { private_key: pk.into(), address: signer, ..Default::default() };
    let boot = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        as i64;
    ensure_l2(&mut creds, clob, boot).expect("derive L2");
    match cancel_order_relayer(clob, &creds, &oid, &relayer_key, &relayer_address) {
        Ok(c) => tracing::info!(target: "vike_polymarket::client", "✅ CANCELLED {oid}: {c}"),
        Err(e) => panic!("cancel failed: {e}"),
    }
}

/// The cancel of a read that must not cancel.
fn never_cancel(_: &str) -> Result<serde_json::Value, String> {
    panic!("a find the core can still take costs no cancel")
}

/// An UNKNOWN submit's row for an order that reached this client `age_ms` ago.
fn unknown_aged(age_ms: i64) -> UnknownSubmits {
    let unknown = UnknownSubmits::default();
    let arrived = vike_model::now_ms() - age_ms;
    unknown.0.lock().unwrap().insert("order-client-id-1".into(), ("0xabc".into(), arrived));
    unknown
}

/// `confirm` re-queries ONLY a submit left UNKNOWN, and turns only a FOUND order into an
/// `OrderAccepted`; not found / an error / an unverified shape report nothing (never a reject).
#[test]
fn confirm_resolves_an_unknown_submit_only_when_the_venue_has_the_order() {
    let none = UnknownSubmits::default();
    let never = |_: &str| -> Result<serde_json::Value, String> { panic!("must not query") };
    assert!(
        confirm_unknown_submit(&none, "order-client-id-1", 7, never, never_cancel).is_empty(),
        "not an unknown one"
    );

    let unknown = unknown_aged(0);
    let absent = |_: &str| Ok(serde_json::json!({"error": "order not found"}));
    assert!(
        confirm_unknown_submit(&unknown, "order-client-id-1", 7, absent, never_cancel).is_empty()
    );
    let down = |_: &str| Err("network: timeout".to_string());
    assert!(
        confirm_unknown_submit(&unknown, "order-client-id-1", 7, down, never_cancel).is_empty()
    );
    assert!(
        unknown.0.lock().unwrap().contains_key("order-client-id-1"),
        "still unknown after a non-answer"
    );

    let found = |id: &str| {
        assert_eq!(id, "0xabc", "the derived id is what is asked for");
        Ok(serde_json::json!({"id": "0xabc", "status": "LIVE"}))
    };
    let evs = confirm_unknown_submit(&unknown, "order-client-id-1", 7, found, never_cancel);
    assert!(matches!(&evs[..], [Event::OrderAccepted(a)]
        if a.order_client_id == "order-client-id-1" && a.venue_order_id.as_deref() == Some("0xabc")));
    assert!(unknown.0.lock().unwrap().is_empty(), "resolved: forgotten");
}

/// A found order is read by its STATUS: a closed one is the accept plus its cancel (the FSM takes
/// a cancel only from an accepted order), an open or unknown one the accept alone.
#[test]
fn confirm_reads_the_status_of_a_found_order() {
    for (status, canceled) in [
        ("LIVE", false),
        ("MATCHED", false),
        ("DELAYED", false),
        ("brand_new_status", false),
        ("", false),
        ("CANCELED", true),
        ("canceled", true),
        ("UNMATCHED", true),
        ("INVALID", true),
        ("CANCELED_MARKET_RESOLVED", true),
    ] {
        let unknown = unknown_aged(0);
        let reply = serde_json::json!({"data": {"id": "0xABC", "status": status}});
        let evs =
            confirm_unknown_submit(&unknown, "order-client-id-1", 7, |_| Ok(reply), never_cancel);
        assert!(
            matches!(evs.first(), Some(Event::OrderAccepted(a))
                if a.venue_order_id.as_deref() == Some("0xABC")),
            "{status}: {evs:?}"
        );
        match (canceled, &evs[1..]) {
            (false, []) => {}
            (true, [Event::OrderCanceled(c)]) => assert_eq!(c.order_client_id, "order-client-id-1"),
            _ => panic!("{status}: {evs:?}"),
        }
        assert!(unknown.0.lock().unwrap().is_empty(), "{status}: resolved, forgotten");
    }
}

/// The venue's non-answers (404 `order not found`, 400 `Invalid orderID`, 429, 5xx) are `Err` from
/// `get_signed`, and even an `Ok` error body names no order: NO event, and the order client id stays unknown.
#[test]
fn confirm_says_nothing_on_a_non_answer() {
    let replies: [Result<serde_json::Value, String>; 6] = [
        Err("polymarket 404: {\"error\":\"order not found\"}".into()),
        Err("polymarket 400: {\"error\":\"Invalid orderID\"}".into()),
        Err("polymarket 429: {\"error\":\"Too Many Requests\"}".into()),
        Err("polymarket 500: {\"error\":\"Internal Server Error\"}".into()),
        Err("polymarket 503: {\"error\":\"Service Unavailable\"}".into()),
        Ok(serde_json::json!({"error": "order not found"})),
    ];
    for reply in replies {
        let unknown = unknown_aged(0);
        let read = |_: &str| reply.clone();
        assert!(
            confirm_unknown_submit(&unknown, "order-client-id-1", 7, read, never_cancel).is_empty()
        );
        assert!(unknown.0.lock().unwrap().contains_key("order-client-id-1"), "{reply:?}");
    }
}

/// **The core takes an accept only from `Submitted`, so a find it may have rejected is not reported
/// as accepted: it is canceled at the venue by its id, and the log says so.** The reject that follows
/// is what an operator reads if the core was still waiting, and a dropped repeat if it was not.
#[test]
#[tracing_test::traced_test]
fn a_late_find_is_canceled_at_the_venue_by_its_id_and_never_reported_as_accepted() {
    let unknown = unknown_aged(LATEST_ACCEPT_AGE_MS);
    let live = |_: &str| Ok(serde_json::json!({"id": "0xABC", "status": "LIVE"}));
    let asked = std::cell::Cell::new(false);
    let cancel = |id: &str| {
        assert_eq!(id, "0xABC", "the venue's own id is what is canceled");
        asked.set(true);
        Ok(serde_json::json!({"canceled": [id]}))
    };
    let evs = confirm_unknown_submit(&unknown, "order-client-id-1", 7, live, cancel);
    assert!(asked.get(), "it was not canceled");
    let [Event::OrderRejected(r)] = &evs[..] else { panic!("one terminal reject: {evs:?}") };
    assert!(r.reason.contains("0xABC") && r.reason.contains("canceled at the venue"), "{r:?}");
    assert!(logs_contain("CANCELED AT THE VENUE"));
    assert!(unknown.0.lock().unwrap().is_empty(), "settled: nothing may report it again");
}

/// A late find that will not cancel claims nothing, and tells the operator what is left to do.
#[test]
#[tracing_test::traced_test]
fn a_late_find_that_cannot_be_canceled_says_so_and_reports_nothing() {
    let unknown = unknown_aged(LATEST_ACCEPT_AGE_MS);
    let live = |_: &str| Ok(serde_json::json!({"id": "0xabc", "status": "LIVE"}));
    let refused = |_: &str| Err("polymarket 500: a body that must not be logged".to_string());
    assert!(confirm_unknown_submit(&unknown, "order-client-id-1", 7, live, refused).is_empty());
    assert!(logs_contain("CANCEL IT BY HAND"));
    assert!(!logs_contain("a body that must not be logged"));
}

/// A late find the venue has already closed has nothing to cancel and nothing the core can take.
#[test]
fn a_late_find_already_closed_costs_no_cancel() {
    for status in ["CANCELED", "UNMATCHED", "INVALID", "CANCELED_MARKET_RESOLVED"] {
        let unknown = unknown_aged(LATEST_ACCEPT_AGE_MS);
        let closed = |_: &str| Ok(serde_json::json!({"id": "0xabc", "status": status}));
        let evs = confirm_unknown_submit(&unknown, "order-client-id-1", 7, closed, never_cancel);
        assert!(evs.is_empty(), "{status}: {evs:?}");
        assert!(unknown.0.lock().unwrap().is_empty(), "{status}: settled");
    }
}

// --- the wallet read, through the REAL exec thread -------------------------------------------

mod wallet_read {
    use super::*;
    use crate::execution::test_support::{FakeServer, Reply, declare_direct_egress};
    use vike_execution::testing::rejection;

    const KEY: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";
    const TOKEN: &str =
        "32338220190071351435772801779725302244575775216413325951443816017994629993401";
    const STD: &str = "0xE111180000d2663C0091e4f400237545B87B996B";
    const NEG: &str = "0xe2222d279d744050d28e00520010520000310F59";
    pub(super) const MAX: &str =
        "115792089237316195423570985008687907853269984665640564039457584007913129639935";

    /// A loopback CLOB: the wallet's balance and allowances are what the test says, and `POST /order`
    /// accepts. Every request is recorded.
    pub(super) fn clob(balance: &'static str, allowance: &'static str) -> FakeServer {
        FakeServer::start(move |req| match (req.method.as_str(), req.path()) {
            ("GET", "/balance-allowance") => Reply::json(
                200,
                serde_json::json!({ "balance": balance, "allowances": { STD: allowance, NEG: allowance } }),
            ),
            ("POST", "/order") => Reply::json(
                200,
                serde_json::json!({ "success": true, "orderID": "0xorder1", "status": "live" }),
            ),
            _ => Reply::json(404, serde_json::json!({ "error": "not found" })),
        })
    }

    /// An account the doubles can serve: L2 already held, every token standard, nothing else started.
    pub(super) fn account(signature_type: SignatureType) -> PolymarketLiveConfig {
        PolymarketLiveConfig {
            creds: PolymarketConfig {
                private_key: KEY.to_string().into(),
                api_key: "key-1".to_string().into(),
                // non-empty ⇒ `ensure_l2` returns at once: the derive costs no request
                secret: "cG9seW1hcmtldC1sMi1zZWNyZXQta2V5LTEyMzQ1Njc4".to_string().into(),
                passphrase: "pass-1".to_string().into(),
                ..Default::default()
            },
            maker: "0x00000000000000000000000000000000000000aa".to_string(),
            signature_type,
            neg_risk: NegRiskSource::lookup_with(
                std::collections::HashMap::new(),
                Box::new(|_| Some(false)),
            ),
            registry: OrderVenueIdTracker::new(),
            tracker: None,
            user_channel: None,
            builder_code: [0u8; 32],
            presubmit_register: false,
            rate_gate: false,
            tick_regime: None,
        }
    }

    /// A limit order of 10 shares at 0.50 on [`TOKEN`], stating no `ts`, as the control door's does.
    pub(super) fn order(order_client_id: &str, side: i32) -> OrderRequest {
        OrderRequest {
            order_client_id: order_client_id.to_string(),
            venue: "polymarket".to_string(),
            symbol: TOKEN.to_string(),
            side,
            qty: 10.0,
            order_type: vike_model::OrderType::Limit,
            price: Some(0.5),
            ..Default::default()
        }
    }

    /// Run ONE order through the real exec thread (an EOA wallet, so no relayer is involved) against
    /// `clob`, and return every event it produced.
    fn place(clob: &FakeServer, side: i32, signature_type: SignatureType) -> Vec<Event> {
        declare_direct_egress();
        let (tx, mut rx) = vike_execution::event_channel(64);
        let mut client = spawn_live(account(signature_type), tx, &clob.base, NO_HOST, NO_HOST);
        client.submit(&order("order-client-id-w", side));
        client.detach();
        let mut seen = Vec::new();
        while let Ok(vike_execution::Ingest::Event(e)) = rx.try_recv() {
            seen.push(e);
        }
        seen
    }

    /// A wallet that cannot pay is refused in words BEFORE anything is signed: the venue sees one
    /// read and no order, and the terminal is the one `OrderRejected` the contract owes.
    #[test]
    fn a_poor_wallet_is_refused_in_words_and_no_order_reaches_the_venue() {
        let clob = clob("0", MAX);
        let events = place(&clob, 1, SignatureType::Eoa);
        let why = rejection(&events).expect("exactly one terminal: an OrderRejected");
        assert!(why.contains("BUY was NOT sent") && why.contains("needs 5 pUSD"), "{why}");
        assert!(why.contains("holds 0"), "{why}");
        assert!(!events.iter().any(|e| matches!(e, Event::OrderAccepted(_))));
        let seen = clob.seen();
        assert!(
            !seen.iter().any(|r| r.method == "POST"),
            "no order was signed and sent to a wallet that cannot pay: {seen:?}"
        );
        assert_eq!(seen.len(), 1, "one wallet read: {seen:?}");
        assert_eq!(seen[0].query(), "asset_type=COLLATERAL&signature_type=0");
    }

    /// The same, for a wallet that holds the money but never approved the exchange: the words name
    /// the exchange, and an EOA is told to approve itself (no relayer is dialled: its base is dead).
    #[test]
    fn an_unapproved_wallet_is_refused_naming_its_exchange() {
        let clob = clob("9000000", "0");
        let why = rejection(&place(&clob, 1, SignatureType::Eoa)).expect("a rejection");
        assert!(why.contains("has not approved the CTF exchange") && why.contains(STD), "{why}");
        assert!(why.contains("not a deposit wallet"), "{why}");
        assert!(!clob.seen().iter().any(|r| r.method == "POST"));
    }

    /// The check must not stand between a healthy wallet and its order: the order is signed and sent
    /// to the SAME host, with its L2 headers, and the accept comes back as the terminal.
    #[test]
    fn a_funded_and_approved_wallet_places_its_order_as_before() {
        let clob = clob("9000000", MAX);
        let events = place(&clob, 1, SignatureType::Eoa);
        assert_eq!(rejection(&events), None, "{events:?}");
        let accepted = events
            .iter()
            .find_map(|e| match e {
                Event::OrderAccepted(a) => Some(format!("{:?}", a.venue_order_id)),
                _ => None,
            })
            .expect("the accept is the terminal");
        assert!(accepted.contains("0xorder1"), "{accepted}");
        let seen = clob.seen();
        let post = seen.iter().find(|r| r.method == "POST").expect("the order was posted");
        assert_eq!(post.path(), "/order");
        assert_eq!(post.header("poly_api_key"), Some("key-1"));
        let body: serde_json::Value = serde_json::from_str(&post.body).unwrap();
        assert_eq!(body["order"]["makerAmount"], "5000000", "10 shares at 0.50 is 5 pUSD");
        assert_eq!(body["order"]["tokenId"], TOKEN);
    }

    /// A modify reaches the exec thread and is ANSWERED, a non-terminal refusal in words (this venue has
    /// no amend), and the venue hears nothing. It used to vanish in the client's inherited no-op.
    #[test]
    fn a_modify_is_answered_with_a_refusal_in_words() {
        let (tx, mut rx) = vike_execution::event_channel(64);
        let mut client = spawn_live(account(SignatureType::Eoa), tx, NO_HOST, NO_HOST, NO_HOST);
        client.modify(&order("order-client-id-m", 1), Some(5.0), None);
        client.detach();
        let mut seen = Vec::new();
        while let Ok(vike_execution::Ingest::Event(e)) = rx.try_recv() {
            seen.push(e);
        }
        assert!(
            matches!(seen.as_slice(), [Event::OrderModifyRejected(r)]
                if r.order_client_id == "order-client-id-m" && format!("{:?}", r.reason).contains("no order amend")),
            "{seen:?}"
        );
    }

    /// A SELL reads the outcome-token balance of THAT token, and a wallet with none is told so.
    #[test]
    fn a_sell_of_shares_the_wallet_does_not_hold_is_refused_before_it_is_signed() {
        let clob = clob("0", MAX);
        let events = place(&clob, -1, SignatureType::Eoa);
        let why = rejection(&events).expect("a rejection");
        assert!(why.contains("SELL was NOT sent") && why.contains("nothing to sell"), "{why}");
        let seen = clob.seen();
        assert_eq!(
            seen[0].query(),
            format!("asset_type=CONDITIONAL&token_id={TOKEN}&signature_type=0")
        );
        assert!(!seen.iter().any(|r| r.method == "POST"));
    }
}

// --- an order too old to sign, through the REAL exec loop -------------------------------------

mod queue_age {
    use super::wallet_read::{MAX, account, clob, order};
    use super::*;
    use crate::execution::test_support::{FakeServer, Reply, declare_direct_egress};
    use vike_execution::testing::rejection;

    /// An order that has already waited `ms` on the queue.
    pub(super) fn waited(order_client_id: &str, ms: i64) -> ExecutionClientActorCommand {
        ExecutionClientActorCommand::Submit(
            Box::new(OrderRequest {
                timestamp: vike_model::now_ms() - ms,
                ..order(order_client_id, 1)
            }),
            std::time::Instant::now(),
        )
    }

    /// `commands`, queued before the loop starts, through the real exec loop against `clob`, and every
    /// event it produced. It runs below `ExecutionClient::submit`, which stamps `ts`: that is how a
    /// test says how long an order waited without waiting that long.
    pub(super) fn drain(
        clob: &FakeServer,
        registry: OrderVenueIdTracker,
        commands: Vec<ExecutionClientActorCommand>,
    ) -> Vec<Event> {
        declare_direct_egress();
        let (events, mut rx) = vike_execution::event_channel(64);
        let (queue, commands_rx) = std::sync::mpsc::channel();
        for command in commands.into_iter().chain([ExecutionClientActorCommand::Shutdown]) {
            queue.send(command).expect("the loop's queue");
        }
        run(
            PolymarketLiveConfig { registry, ..account(SignatureType::Eoa) },
            events,
            commands_rx,
            UnknownSubmits::default(),
            &clob.base,
            NO_HOST,
            &GammaClient { base: NO_HOST.to_string() },
        );
        let mut seen = Vec::new();
        while let Ok(vike_execution::Ingest::Event(e)) = rx.try_recv() {
            seen.push(e);
        }
        seen
    }

    /// **The order the core has given up on is not signed.** A black-holed proxy held the one exec
    /// thread for 34 s per order, so a third queued order was still unsent at the core's reject; it was
    /// then signed and posted when the tunnel returned, and rested at the venue under a rejected id.
    #[test]
    fn an_order_too_old_at_dequeue_is_refused_and_the_venue_hears_nothing() {
        let clob = clob("9000000", MAX);
        let events =
            drain(&clob, OrderVenueIdTracker::new(), vec![waited("order-client-id-old", 31_000)]);
        assert!(matches!(events.first(), Some(Event::OrderSubmitted(_))), "{events:?}");
        let why = rejection(&events).expect("the terminal reject the contract owes");
        assert!(why.contains("was NOT sent") && why.contains("waited 3"), "{why}");
        assert!(!events.iter().any(|e| matches!(e, Event::OrderAccepted(_))), "{events:?}");
        assert!(clob.seen().is_empty(), "not even a read: {:?}", clob.seen());
    }

    /// **A close past the limit is signed and sent**: `Flatten`'s market sale (no price, reduce-only). Left unsent it
    /// leaves the position open with nothing asking again, and a sale spends only what the wallet holds.
    #[test]
    fn a_close_past_the_limit_is_signed_and_sent() {
        let clob = FakeServer::start(|req| match req.method.as_str() {
            "GET" => {
                Reply::json(200, serde_json::json!({ "balance": "90000000", "allowance": MAX }))
            }
            _ => Reply::json(
                200,
                serde_json::json!({ "success": true, "orderID": "0xclose", "status": "matched" }),
            ),
        });
        let close = OrderRequest {
            timestamp: vike_model::now_ms() - 31_000,
            order_type: vike_model::OrderType::Market,
            price: None,
            reduce_only: true,
            ..order("order-client-id-close", -1)
        };
        let command =
            ExecutionClientActorCommand::Submit(Box::new(close), std::time::Instant::now());
        let events = drain(&clob, OrderVenueIdTracker::new(), vec![command]);
        assert_eq!(rejection(&events), None, "{events:?}");
        assert!(clob.seen().iter().any(|r| r.method == "POST"), "{:?}", clob.seen());
    }

    /// The reads in front of the signature spend what the order had left, so the moment of signing
    /// is what is judged: this one is young at dequeue and past the limit when its wallet read ends.
    #[test]
    fn an_order_that_outlives_the_limit_in_its_wallet_read_is_not_signed() {
        let clob = FakeServer::start(|req| match req.method.as_str() {
            "GET" => {
                std::thread::sleep(std::time::Duration::from_millis(3_500));
                Reply::json(200, serde_json::json!({ "balance": "9000000", "allowance": MAX }))
            }
            _ => Reply::json(
                200,
                serde_json::json!({ "success": true, "orderID": "0xlate", "status": "live" }),
            ),
        });
        let events =
            drain(&clob, OrderVenueIdTracker::new(), vec![waited("order-client-id-slow", 27_000)]);
        let why = rejection(&events).expect("the terminal reject the contract owes");
        assert!(why.contains("was NOT sent"), "{why}");
        let seen = clob.seen();
        assert!(!seen.iter().any(|r| r.method == "POST"), "signed and sent late: {seen:?}");
    }

    /// **The clock is not the order's own `ts`.** The control door states none (0, so 1970) and a
    /// strategy may stamp the trade that moved it: read as an age, either refuses a fresh order.
    #[test]
    fn a_fresh_order_is_sent_whatever_ts_it_arrived_with() {
        for (row, ts) in [
            ("the control door states none", 0),
            ("a strategy stamped an old trade", vike_model::now_ms() - 3_600_000),
        ] {
            declare_direct_egress();
            let clob = clob("9000000", MAX);
            let (tx, mut rx) = vike_execution::event_channel(64);
            let mut client =
                spawn_live(account(SignatureType::Eoa), tx, &clob.base, NO_HOST, NO_HOST);
            client.submit(&OrderRequest { timestamp: ts, ..order("order-client-id-fresh", 1) });
            client.detach();
            let mut events = Vec::new();
            while let Ok(vike_execution::Ingest::Event(e)) = rx.try_recv() {
                events.push(e);
            }
            assert_eq!(rejection(&events), None, "{row}: {events:?}");
            assert!(
                events.iter().any(|e| matches!(e, Event::OrderAccepted(_))),
                "{row}: {events:?}"
            );
            assert!(clob.seen().iter().any(|r| r.method == "POST"), "{row}");
        }
    }

    /// A cancel has no age: a late cancel is safe and a dropped one is not. It goes out from behind an
    /// order just refused for waiting too long.
    #[test]
    fn a_cancel_goes_out_however_long_it_waited() {
        let clob = FakeServer::start(|req| match req.method.as_str() {
            "DELETE" => Reply::json(200, serde_json::json!({ "canceled": ["0xresting"] })),
            _ => Reply::json(404, serde_json::json!({ "error": "not found" })),
        });
        let registry = OrderVenueIdTracker::new();
        assert!(registry.on_accept("order-client-id-resting", "0xresting", 1).is_empty());
        let cancel = ExecutionClientActorCommand::Cancel {
            order_client_id: "order-client-id-resting".to_string(),
            intent: OrderCancelIntent::Unspecified,
        };
        let events = drain(&clob, registry, vec![waited("order-client-id-old", 31_000), cancel]);
        assert!(
            events.iter().any(
                |e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == "order-client-id-resting")
            ),
            "{events:?}"
        );
        assert!(clob.seen().iter().any(|r| r.method == "DELETE"), "{:?}", clob.seen());
    }

    /// Exactly the limit is not past it, the words name the age, and a clock stepped back says nothing.
    #[test]
    fn the_limit_is_thirty_seconds_and_the_words_name_the_age() {
        assert_eq!(too_old_to_sign(1_000, 31_000), None);
        let words = too_old_to_sign(1_000, 32_500).expect("past the limit");
        assert!(words.contains("waited 31.5 s") && words.contains("the 30 s"), "{words}");
        assert_eq!(too_old_to_sign(60_000, 1_000), None);
    }

    /// The sum [`MAX_SUBMIT_AGE_MS`] rests on, against the core's own grace: a raise of the limit or of
    /// the POST's, or a cut of the grace, goes red here instead of stranding an order.
    #[test]
    fn the_slowest_submit_is_answered_before_the_core_can_reject_it() {
        // What the live root arms: `crates/vike-tradehub/src/tradehub_cli/live_runtime.rs`'s `submit_ack_timeout`.
        let ack = std::time::Duration::from_secs(30);
        let grace = vike_core::CoreConfig::default().submit_ack_confirm_grace;
        let limit = std::time::Duration::from_millis(MAX_SUBMIT_AGE_MS as u64);
        assert!(limit + crate::transport::REST_TIMEOUT <= ack + grace * 2);
        let latest = std::time::Duration::from_millis(LATEST_ACCEPT_AGE_MS as u64);
        assert!(latest < ack + grace * 2, "an accept this old may land on a rejected order");
    }
}

// --- a POST that timed out, through the REAL exec loop ----------------------------------------

mod timed_out_post {
    use super::queue_age::{drain, waited};
    use super::wallet_read::MAX;
    use super::*;
    use crate::execution::test_support::{FakeServer, Recorded, Reply};
    use vike_execution::testing::rejection;

    /// A CLOB whose `POST /order` answers nothing the bridge can read after `hold`, and whose order
    /// read holds the order as `status` (`None`: it does not know it).
    fn venue(hold: std::time::Duration, status: Option<&'static str>) -> FakeServer {
        FakeServer::start(move |req| match (req.method.as_str(), req.path()) {
            ("GET", "/balance-allowance") => {
                Reply::json(200, serde_json::json!({ "balance": "9000000", "allowance": MAX }))
            }
            ("POST", "/order") => {
                std::thread::sleep(hold);
                Reply::json(504, serde_json::json!({ "error": "gateway timeout" }))
            }
            ("DELETE", "/order") => Reply::json(200, serde_json::json!({ "canceled": [] })),
            ("GET", path) => match (path.strip_prefix("/data/order/"), status) {
                (Some(id), Some(status)) => {
                    Reply::json(200, serde_json::json!({ "id": id, "status": status }))
                }
                _ => Reply::json(404, serde_json::json!({ "error": "order not found" })),
            },
            _ => Reply::json(404, serde_json::json!({ "error": "not found" })),
        })
    }

    /// The id the exec thread asked the venue about.
    fn read_id(seen: &[Recorded]) -> String {
        seen.iter()
            .find_map(|r| r.path().strip_prefix("/data/order/"))
            .expect("the exec thread read the order")
            .to_string()
    }

    /// **The core's one confirm can come while the POST is in flight and find no row, and nothing
    /// asked again**, so the exec thread asks itself. In time, the find is the accept the core is
    /// waiting for, with the venue's id.
    #[test]
    fn a_timed_out_post_found_in_time_is_reported_with_its_venue_id() {
        let clob = venue(std::time::Duration::ZERO, Some("LIVE"));
        let events =
            drain(&clob, OrderVenueIdTracker::new(), vec![waited("order-client-id-t", 1_000)]);
        let seen = clob.seen();
        let id = read_id(&seen);
        assert_eq!(rejection(&events), None, "{events:?}");
        assert!(
            events.iter().any(|e| matches!(e, Event::OrderAccepted(a)
                if a.order_client_id == "order-client-id-t" && a.venue_order_id.as_deref() == Some(&*id))),
            "{events:?}"
        );
        assert!(!seen.iter().any(|r| r.method == "DELETE"), "in time costs no cancel: {seen:?}");
    }

    /// Absent is no evidence: nothing further goes out, and the order stays the core's to settle.
    #[test]
    fn a_timed_out_post_the_venue_does_not_show_says_nothing_further() {
        let clob = venue(std::time::Duration::ZERO, None);
        let events =
            drain(&clob, OrderVenueIdTracker::new(), vec![waited("order-client-id-t", 1_000)]);
        assert!(matches!(&events[..], [Event::OrderSubmitted(_)]), "{events:?}");
        let seen = clob.seen();
        assert_eq!(read_id(&seen).len(), 66, "it did ask, by the derived id: {seen:?}");
        assert!(!seen.iter().any(|r| r.method == "DELETE"), "{seen:?}");
    }

    /// **The venue took it after all: the first fill accepts it**, by the id the exec thread derived, since the table
    /// refuses a wrap on a `Submitted` order and keeps nothing of it.
    #[test]
    fn a_fill_of_a_timed_out_post_the_venue_did_not_show_accepts_it_first() {
        let clob = venue(std::time::Duration::ZERO, None);
        let registry = OrderVenueIdTracker::new();
        let events = drain(&clob, registry.clone(), vec![waited("order-client-id-t", 1_000)]);
        assert!(matches!(&events[..], [Event::OrderSubmitted(_)]), "{events:?}");
        let id = read_id(&clob.seen());
        let trade = serde_json::json!({
            "event_type": "trade", "type": "TRADE", "id": "t1", "status": "MATCHED",
            "asset_id": "111", "side": "BUY", "size": "1", "price": "0.5",
            "taker_order_id": id, "maker_orders": []
        });
        let said = crate::execution::user_data::decode::decode_user(&trade, &registry);
        assert!(
            said.iter().any(|e| matches!(e, Event::OrderAccepted(a)
                if a.order_client_id == "order-client-id-t" && a.venue_order_id.as_deref() == Some(&*id))),
            "{said:?}"
        );
    }

    /// A trade frame of the order with CLOB id `id`, as the user channel pushes it.
    fn trade_of(id: &str) -> serde_json::Value {
        serde_json::json!({
            "event_type": "trade", "type": "TRADE", "id": "t1", "status": "MATCHED",
            "asset_id": "111", "side": "BUY", "size": "1", "price": "0.5",
            "taker_order_id": id, "maker_orders": []
        })
    }

    /// **A live trade frame inside the read's window**, with `on_accept` done and the read not back: it got no accept and
    /// the bracket's exits stayed held. Reddens on `note_silent` going back behind the read.
    #[test]
    fn a_fill_heard_during_the_read_of_a_timed_out_post_accepts_it_first() {
        let registry = OrderVenueIdTracker::new();
        let heard = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let clob = {
            let (registry, heard) = (registry.clone(), heard.clone());
            FakeServer::start(move |req| match (req.method.as_str(), req.path()) {
                ("GET", "/balance-allowance") => {
                    Reply::json(200, serde_json::json!({ "balance": "9000000", "allowance": MAX }))
                }
                ("POST", "/order") => Reply::json(504, serde_json::json!({ "error": "timeout" })),
                ("GET", path) if path.starts_with("/data/order/") => {
                    let id = path.trim_start_matches("/data/order/");
                    let said =
                        crate::execution::user_data::decode::decode_user(&trade_of(id), &registry);
                    heard.lock().unwrap().extend(said);
                    Reply::json(404, serde_json::json!({ "error": "order not found" }))
                }
                _ => Reply::json(404, serde_json::json!({ "error": "not found" })),
            })
        };
        let events = drain(&clob, registry, vec![waited("order-client-id-t", 1_000)]);
        assert!(matches!(&events[..], [Event::OrderSubmitted(_)]), "{events:?}");
        let heard = heard.lock().unwrap();
        assert!(
            heard.iter().any(
                |e| matches!(e, Event::OrderAccepted(a) if a.order_client_id == "order-client-id-t")
            ),
            "{heard:?}"
        );
    }

    /// A read that found the order said so itself, so the first fill behind it implies no second accept.
    #[test]
    fn a_fill_of_a_timed_out_post_found_in_time_implies_no_second_accept() {
        let clob = venue(std::time::Duration::ZERO, Some("LIVE"));
        let registry = OrderVenueIdTracker::new();
        drain(&clob, registry.clone(), vec![waited("order-client-id-t", 1_000)]);
        let said = crate::execution::user_data::decode::decode_user(
            &trade_of(&read_id(&clob.seen())),
            &registry,
        );
        assert!(!said.iter().any(|e| matches!(e, Event::OrderAccepted(_))), "{said:?}");
    }

    /// **The whole shape, on the real clock.** Sent at 25 s, within the limit; the POST runs out at
    /// 55 s; the venue holds the order; the core may have rejected it by now, so the accept cannot
    /// be reported, and the order is canceled at the venue by the id it was read under.
    #[test]
    fn a_post_that_times_out_late_and_is_found_open_is_canceled_at_the_venue() {
        let hold = crate::transport::REST_TIMEOUT + std::time::Duration::from_millis(500);
        let clob = venue(hold, Some("LIVE"));
        let events =
            drain(&clob, OrderVenueIdTracker::new(), vec![waited("order-client-id-late", 25_000)]);
        let seen = clob.seen();
        let id = read_id(&seen);
        assert!(!events.iter().any(|e| matches!(e, Event::OrderAccepted(_))), "{events:?}");
        let why = rejection(&events).expect("the reject that says what happened");
        assert!(why.contains(&id) && why.contains("canceled at the venue"), "{why}");
        let delete = seen.iter().find(|r| r.method == "DELETE").expect("it was canceled");
        let body: serde_json::Value = serde_json::from_str(&delete.body).unwrap();
        assert_eq!(body["orderID"], id, "by the id it was read under: {seen:?}");
    }
}
