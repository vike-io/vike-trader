use super::*;

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

/// OFFLINE, through the REAL `ExecActor` + exec thread: a token whose NegRisk flag cannot be
/// resolved is REJECTED with a named reason — never signed against a guessed EIP-712 domain, and
/// never silently dropped (the venue-adapter contract: exactly one terminal per order).
///
/// No network: the key is valid-shaped so the EOA derives purely, and `secret` is pre-filled so
/// `ensure_l2` short-circuits. The injected resolver always says "don't know", which is the
/// production behavior when `/neg-risk` is unreachable or answers without a usable flag.
#[test]
fn unresolvable_neg_risk_rejects_the_order_offline() {
    let (tx, mut rx) = vike_exec::event_channel(64);
    let mut client = PolymarketExecutionClient::spawn_live(
        PolymarketLiveConfig {
            creds: PolymarketCreds {
                private_key: "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4"
                    .to_string(),
                // non-empty ⇒ `ensure_l2` returns Ok immediately, so this test makes NO call
                secret: "not-a-real-secret".to_string(),
                ..Default::default()
            },
            maker: String::new(),
            signature_type: SignatureType::Eoa,
            neg_risk: NegRiskSource::lookup_with(
                std::collections::HashMap::new(),
                Box::new(|_| None),
            ),
            registry: PolymarketRegistry::new(),
            tracker: None,
            user_channel: None, // no socket, no extra thread
            builder_code: [0u8; 32],
            presubmit_register: false,
            rate_gate: false,
            tick_regime: None, // no transport, no rounding, no re-fetch
        },
        tx,
    );
    client.submit(&OrderRequest {
        client_order_id: "coid-nr".to_string(),
        venue: "polymarket".to_string(),
        symbol: "999".to_string(),
        side: 1,
        qty: 10.0,
        order_type: "limit".to_string(),
        price: Some(0.01),
        ..Default::default()
    });
    client.detach();

    let mut seen = Vec::new();
    while let Ok(vike_exec::Ingest::Event(e)) = rx.try_recv() {
        seen.push(e);
    }
    assert!(
        seen.iter()
            .any(|e| matches!(e, Event::OrderSubmitted(s) if s.client_order_id == "coid-nr")),
        "the synchronous OrderSubmitted still fires: {seen:?}"
    );
    let rejected = seen
        .iter()
        .find_map(|e| match e {
            Event::OrderRejected(r) if r.client_order_id == "coid-nr" => Some(r),
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
fn tif_to_order_type() {
    // Pins the routed polymarket row of `vike_model::venues::venue_tif::venue_tif` byte-for-byte.
    // NOTE the Ioc→FOK fold: the OPPOSITE direction of hyperliquid's Fok→Ioc (step-2 flip).
    assert_eq!(order_type_of(TimeInForce::Gtc), "GTC");
    assert_eq!(order_type_of(TimeInForce::Fok), "FOK");
    assert_eq!(order_type_of(TimeInForce::Ioc), "FOK");
    assert_eq!(order_type_of(TimeInForce::Gtd), "GTD");
    assert_eq!(order_type_of(TimeInForce::Day), "GTC");
}

/// **WIRE-BODY pin.** The tif table's own tests are declaration-vs-declaration — they assert
/// `venue_tif("polymarket", Gtd) == Mapped("GTD")` against a matrix that repeats the same
/// claim — so they stayed GREEN while the builder hardcoded `"expiration": "0"` and never read
/// `gtd_expiry`, shipping `orderType: "GTD"` with a GTC expiry to a live market. This asserts
/// on the JSON that actually goes on the wire, which is the only level that catches it.
#[test]
fn gtd_wire_body_carries_the_expiry_and_never_the_gtc_zero() {
    let order = build_order(
        0.52,
        100.0,
        Side::Buy,
        "71321045679252212594626385532706912750332728571942532289631379312455583992563",
        "0xmaker",
        "0xsigner",
        SignatureType::PolyProxy,
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

/// OFFLINE, through the REAL `ExecActor` + exec thread — the sibling of
/// `unresolvable_neg_risk_rejects_the_order_offline`: a GTD request with no expiry gets exactly
/// one terminal, a LOCAL `OrderRejected`, and never reaches the wire.
///
/// The injected neg-risk resolver is the always-`None` one, so a reason naming `gtd_expiry`
/// rather than NegRisk ALSO proves the expiry gate runs before the signing-domain lookup — i.e.
/// before any signing or network work, which is the placement that makes it a local refusal.
#[test]
fn gtd_without_an_expiry_never_reaches_the_wire() {
    let (tx, mut rx) = vike_exec::event_channel(64);
    let mut client = PolymarketExecutionClient::spawn_live(
        PolymarketLiveConfig {
            creds: PolymarketCreds {
                private_key: "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4"
                    .to_string(),
                // non-empty ⇒ `ensure_l2` returns Ok immediately, so this test makes NO call
                secret: "not-a-real-secret".to_string(),
                ..Default::default()
            },
            maker: String::new(),
            signature_type: SignatureType::Eoa,
            neg_risk: NegRiskSource::lookup_with(
                std::collections::HashMap::new(),
                Box::new(|_| None),
            ),
            registry: PolymarketRegistry::new(),
            tracker: None,
            user_channel: None,
            builder_code: [0u8; 32],
            presubmit_register: false,
            rate_gate: false,
            tick_regime: None,
        },
        tx,
    );
    client.submit(&OrderRequest {
        client_order_id: "coid-gtd".to_string(),
        venue: "polymarket".to_string(),
        symbol: "999".to_string(),
        side: 1,
        qty: 10.0,
        order_type: "limit".to_string(),
        price: Some(0.01),
        time_in_force: TimeInForce::Gtd,
        gtd_expiry: None,
        ..Default::default()
    });
    client.detach();

    let mut seen = Vec::new();
    while let Ok(vike_exec::Ingest::Event(e)) = rx.try_recv() {
        seen.push(e);
    }
    assert!(
        seen.iter()
            .any(|e| matches!(e, Event::OrderSubmitted(s) if s.client_order_id == "coid-gtd")),
        "the synchronous OrderSubmitted still fires first, per the emitter split: {seen:?}"
    );
    let rejected = seen
        .iter()
        .find_map(|e| match e {
            Event::OrderRejected(r) if r.client_order_id == "coid-gtd" => Some(r),
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

/// The safety decision of the whole bulk lane, both halves. `WholeBook` is an ASSERTION that
/// the account-wide `cancel-all` is equivalent to cancelling exactly these ids, so a batch that
/// does not name this mount's whole known book — or that named an id which did not resolve, and
/// is therefore reaching outside it — must stay `Subset`, which `plan_cancels` can never take
/// to the account-wide arm however much budget is free.
#[test]
fn only_a_fully_resolved_whole_book_batch_may_assert_the_account_wide_scope() {
    assert_eq!(batch_scope(true, 3, 3), CancelScope::WholeBook);
    assert_eq!(batch_scope(false, 3, 3), CancelScope::Subset, "not the whole book");
    assert_eq!(batch_scope(true, 2, 3), CancelScope::Subset, "an id did not resolve");
    assert_eq!(batch_scope(false, 2, 3), CancelScope::Subset);
}

/// OFFLINE, through the REAL `ExecActor` + exec thread — the batch door's sibling of
/// `unresolvable_neg_risk_rejects_the_order_offline`. A batch of coids this mount never
/// registered resolves to nothing, so the arm refuses each id and returns before any network
/// work: it proves the batch crossed the seam as ONE command and was still reported PER ID,
/// with the same wording one lone cancel of the same order would carry. Non-terminal, because
/// an order we cannot address is not an order we cancelled.
#[test]
fn a_batch_of_unregistered_coids_is_refused_per_id_offline() {
    let (tx, mut rx) = vike_exec::event_channel(64);
    let mut client = PolymarketExecutionClient::spawn_live(
        PolymarketLiveConfig {
            creds: PolymarketCreds {
                private_key: "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4"
                    .to_string(),
                // non-empty ⇒ `ensure_l2` returns Ok immediately, so this test makes NO call
                secret: "not-a-real-secret".to_string(),
                ..Default::default()
            },
            maker: String::new(),
            signature_type: SignatureType::Eoa,
            neg_risk: NegRiskSource::lookup_with(
                std::collections::HashMap::new(),
                Box::new(|_| None),
            ),
            registry: PolymarketRegistry::new(),
            tracker: None,
            user_channel: None,
            builder_code: [0u8; 32],
            presubmit_register: false,
            rate_gate: false,
            tick_regime: None,
        },
        tx,
    );
    // RiskOff: the reserve may not shed it, so nothing here can be a shed rather than a miss.
    client.cancel_batch_with_intent(
        &["gone-1".to_string(), "gone-2".to_string()],
        CancelIntent::RiskOff,
    );
    client.detach();

    let mut seen = Vec::new();
    while let Ok(vike_exec::Ingest::Event(e)) = rx.try_recv() {
        seen.push(e);
    }
    let refused: Vec<(String, String)> = seen
        .iter()
        .map(|e| match e {
            Event::OrderCancelRejected(r) => (r.client_order_id.clone(), r.reason.to_string()),
            other => panic!("an unaddressable cancel must be NON-terminal: {other:?}"),
        })
        .collect();
    assert_eq!(refused.len(), 2, "one advisory per id, not one per batch: {refused:?}");
    for (coid, reason) in &refused {
        assert!(reason.contains(coid), "the reason must name the order: {reason}");
        assert!(reason.contains("no venue order id"), "single-arm wording: {reason}");
    }
}

/// The DEFAULT-OFF ack-race close (`POLY_PRESUBMIT_REGISTER`), proven at the exact granularity
/// the submit arm injects it — no signing, no network: `build_order` + `derive_order_id` +
/// `on_accept` + `lookup_clob` are the four pure calls the `if presubmit_register { … }` block
/// plus the user-WS pump make. It builds the SAME `Order` the exec thread would for a request,
/// derives its CLOB id pre-submit, registers coid↔id BEFORE any ack, and confirms a fill/cancel
/// that BEATS the ack (a `lookup_clob` on that derived id, the pump's real re-key path) already
/// resolves to our coid + side. It also pins the OFF state (nothing keyed before the register)
/// and the idempotent duplicate ack (the real `OrderAccepted`, same id + side, changes nothing).
#[test]
fn presubmit_register_lets_a_fill_beat_the_ack() {
    let registry = PolymarketRegistry::new();
    let coid = "coid-presub";
    let req_side = 1_i32; // BUY

    // exactly the Order the submit arm builds for this request (same call, same args) …
    let order = build_order(
        0.52,
        100.0,
        if req_side >= 0 { Side::Buy } else { Side::Sell },
        "71321045679252212594626385532706912750332728571942532289631379312455583992563",
        "0xmaker",
        "0xsigner",
        SignatureType::PolyProxy,
        1_700_000_000_000,
        7,
        false,
        [0u8; 32],
    );
    let clob_id = derive_order_id(&order);
    let side_code = if req_side >= 0 { 1 } else { -1 };

    // OFF / pre-injection: with `POLY_PRESUBMIT_REGISTER` unset the block never runs, so the
    // derived id is not keyed yet — byte-identical to before the flag (the ack is sole writer).
    assert_eq!(registry.lookup_clob(&clob_id), None);
    assert_eq!(registry.coid_to_clob(coid), None);

    // ON: exactly what the submit arm's `if presubmit_register { … }` block does —
    // `registry.on_accept(coid, &derive_order_id(&order), side)`. Nothing raced this one, so the
    // park hands nothing back; asserting that (rather than discarding the `#[must_use]`) is what
    // pins "pre-registering an order does not, by itself, replay anything".
    assert!(registry.on_accept(coid, &clob_id, side_code).is_empty(), "nothing was parked");

    // a fill/cancel that BEAT the HTTP ack lands on the user-WS keyed by the CLOB order id,
    // which is exactly this derived id → the pump's `lookup_clob` re-keys it to our coid + side.
    assert_eq!(registry.lookup_clob(&clob_id), Some((coid.to_string(), 1)));
    // and the cancel direction (coid → venue id) is live before any ack, too.
    assert_eq!(registry.coid_to_clob(coid), Some(clob_id.clone()));

    // the real server `OrderAccepted` carries the SAME id (== derived) + side → idempotent no-op,
    // and the park makes that idempotency TOTAL: the second call finds it empty and claims nothing,
    // so the duplicate ack cannot re-emit a fill the first one already replayed.
    assert!(
        registry.on_accept(coid, &clob_id, side_code).is_empty(),
        "the duplicate ack claims nothing"
    );
    assert_eq!(registry.lookup_clob(&clob_id), Some((coid.to_string(), 1)));
    assert_eq!(registry.coid_to_clob(coid), Some(clob_id));
}

// ---- the dynamic tick-size regime, at the exact seam the exec thread consumes it ------------
//
// The submit arm's two tick-regime touches ARE `grid_price` (the price `build_order` is handed)
// and `observe_submit_outcome` (the terminal `Event` fed back to the cache), so driving those two
// with the crate's REST doubles (`tick_regime::stubs`) exercises the wiring itself rather than a
// paraphrase of it. The submit that sits between them is a signed network call, so an end-to-end
// exec-thread test cannot reach it offline — `unresolvable_neg_risk_rejects_the_order_offline`
// above is the exec-thread-level coverage, and it pins the `tick_regime: None` path.

use crate::tick_regime::stubs::{DeadStub, TickStub};

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
        client_order_id: "coid-tick".to_string(),
        reason: reason.to_string().into(),
        ts: 0,
    })
}

fn grid<T: RestTransport>(regime: TickRegime, transport: T) -> TickGrid<T> {
    TickGrid { regime, transport }
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

    for ev in [
        rejected("not enough balance / allowance"),
        rejected("cannot resolve the NegRisk signing domain for token 111"),
        rejected("submit response missing orderID"),
        Event::OrderAccepted(OrderAccepted {
            client_order_id: "coid-tick".to_string(),
            venue_order_id: Some("0xABC".into()),
            ts: 0,
        }),
    ] {
        assert_eq!(observe_submit_outcome(Some(&g), "111", &ev), None, "{ev:?}");
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
    assert_eq!(g.regime.tick_size("111"), Some(0.001), "never clobbered by DEFAULT_TICK_SIZE");
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

/// `tick_regime: None` — every entry point but the live mount — is byte-identical: the price is
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

/// LIVE, real-money (Polygon mainnet): place a TINY BUY at 0.01 — far below any real mid so it
/// rests and CANNOT fill — then cancel. Live acceptance proves the V2 order signature is
/// byte-correct. Order placement is geo-blocked, so run WITH the Dublin proxy configured as
/// `venue.polymarket.*` rows in the settings database (see `crate::egress::egress_tests`'s
/// `declare_from_the_settings_database`):
/// `cargo test -p vike-polymarket --features polymarket \
///   --lib live_place_and_cancel -- --ignored --nocapture`
#[test]
#[ignore = "LIVE real-money: places+cancels a tiny non-filling order on Polymarket mainnet"]
fn live_place_and_cancel() {
    vike_log::test_init();
    crate::egress::egress_tests::declare_from_the_settings_database(
        std::env::var("VIKE_SETTINGS_DIR").ok().as_deref(),
    );
    use crate::ensure_l2;
    use crate::eth_address_from_private_key;
    use crate::{CLOB_BASE, PolymarketCreds};
    use crate::{
        Side, SignatureType, build_order, cancel_order, cancel_order_relayer, order_to_json,
        sign_order, sign_order_1271, submit_order, submit_order_relayer,
    };

    // 1. real creds (python env) → EOA + L2
    let vars = vike_bridge_core::credentials::load_workspace_dotenv();
    let pk = vars.get("POLY_PRIVATE_KEY").expect("POLY_PRIVATE_KEY").clone();
    let relayer_addr = vars.get("POLY_RELAYER_API_KEY_ADDRESS").cloned().unwrap_or_default();
    let relayer_key = vars.get("POLY_RELAYER_API_KEY").cloned().unwrap_or_default();
    let signer = eth_address_from_private_key(&pk).expect("EOA");
    let mut creds =
        PolymarketCreds { private_key: pk.clone(), address: signer.clone(), ..Default::default() };
    let boot = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        as i64;
    ensure_l2(&mut creds, CLOB_BASE, boot).expect("derive L2");
    // The account's funder is a Polymarket DEPOSIT WALLET → signatureType POLY_1271, with
    // maker == signer == the deposit wallet (NOT the EOA); the EOA key is the authorized signer
    // the deposit-wallet contract recognises. Orders route via the relayer (gasless).
    let deposit_wallet = std::env::var("POLY_FUNDER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "0x107C01D04Fd68557ACd52E89dD01972b22803aD5".to_string());
    let sig_type = match std::env::var("POLY_SIGNATURE_TYPE").ok().as_deref() {
        Some("0") => SignatureType::Eoa,
        Some("1") => SignatureType::PolyProxy,
        Some("2") => SignatureType::PolyGnosisSafe,
        _ => SignatureType::Poly1271,
    };
    // for POLY_1271 the order's `signer` field is the deposit wallet itself (verifyingContract).
    let order_signer =
        if sig_type == SignatureType::Poly1271 { deposit_wallet.clone() } else { signer.clone() };
    tracing::info!(target: "vike_polymarket::client", "EOA(auth key)={signer}  maker=signer={deposit_wallet}  sigType={sig_type:?}");

    // 2. a liquid token with a safe mid (reads via the proxy — clob is DNS-blocked here)
    let markets = crate::egress::get_json(CLOB_BASE, "/sampling-markets", "").expect("markets");
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
            let mid = crate::egress::get_json(CLOB_BASE, "/midpoint", &format!("token_id={tid}"))
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
    // (`live_mount_from_vars`): `attribution_code_from` + `decode_builder_bytes32`. Malformed/
    // absent degrades to `[0u8; 32]` (unattributed) rather than failing the smoke.
    let builder_code = vike_bridge_core::credentials::attribution_code_from(&vars, "polymarket")
        .and_then(decode_builder_bytes32)
        .unwrap_or([0u8; 32]);
    let order = build_order(
        price,
        size,
        Side::Buy,
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
    let sig = if sig_type == SignatureType::Poly1271 {
        sign_order_1271(&order, &pk).expect("sign 1271")
    } else {
        sign_order(&order, &pk).expect("sign")
    };
    let order_obj = order_to_json(&order, &sig, 0); // GTC smoke → "no expiry"
    tracing::debug!(target: "vike_polymarket::client", "payload order = {order_obj}");
    // deposit-wallet orders route through the relayer (gasless); else the plain submit.
    let submitted = if sig_type == SignatureType::Poly1271 {
        submit_order_relayer(
            CLOB_BASE,
            &creds,
            order_obj,
            "GTC",
            &relayer_key,
            &relayer_addr,
            false,
        )
    } else {
        submit_order(CLOB_BASE, &creds, order_obj, "GTC", false)
    };
    match submitted {
        Ok(resp) => {
            tracing::info!(target: "vike_polymarket::client", "✅ SUBMIT ACCEPTED — order signature valid: {resp}");
            if let Some(oid) =
                resp.get("orderID").or_else(|| resp.get("orderId")).and_then(|o| o.as_str())
            {
                // LIVE VERIFICATION of the `derive_order_id` premise: the CLOB returns the
                // EIP-712 order hash as `orderID`, so the value computable pre-submit must equal
                // it. Proving this on demo is the gate for wiring `coid`↔`clob_id`
                // pre-registration into the exec submit path (registry.on_accept before the ack).
                let derived = crate::exec_plane::order::derive_order_id(&order);
                assert!(
                    oid.eq_ignore_ascii_case(&derived),
                    "CLOB orderID {oid} must equal the derived EIP-712 order hash {derived}"
                );
                let cancelled = if sig_type == SignatureType::Poly1271 {
                    cancel_order_relayer(CLOB_BASE, &creds, oid, &relayer_key, &relayer_addr)
                } else {
                    cancel_order(CLOB_BASE, &creds, oid)
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
    crate::egress::egress_tests::declare_from_the_settings_database(
        std::env::var("VIKE_SETTINGS_DIR").ok().as_deref(),
    );
    use crate::{
        CLOB_BASE, PolymarketCreds, cancel_order_relayer, ensure_l2, eth_address_from_private_key,
    };
    let oid = std::env::var("POLY_CANCEL_ORDER_ID").expect("set POLY_CANCEL_ORDER_ID");
    let vars = vike_bridge_core::credentials::load_workspace_dotenv();
    let pk = vars.get("POLY_PRIVATE_KEY").expect("POLY_PRIVATE_KEY").clone();
    let relayer_addr = vars.get("POLY_RELAYER_API_KEY_ADDRESS").cloned().unwrap_or_default();
    let relayer_key = vars.get("POLY_RELAYER_API_KEY").cloned().unwrap_or_default();
    let signer = eth_address_from_private_key(&pk).expect("EOA");
    let mut creds = PolymarketCreds { private_key: pk, address: signer, ..Default::default() };
    let boot = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        as i64;
    ensure_l2(&mut creds, CLOB_BASE, boot).expect("derive L2");
    match cancel_order_relayer(CLOB_BASE, &creds, &oid, &relayer_key, &relayer_addr) {
        Ok(c) => tracing::info!(target: "vike_polymarket::client", "✅ CANCELLED {oid}: {c}"),
        Err(e) => panic!("cancel failed: {e}"),
    }
}
