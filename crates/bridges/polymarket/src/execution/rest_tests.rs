use super::*;
use std::assert_matches;

#[test]
fn history_query_shapes() {
    assert_eq!(trades_query(50), "limit=50");
    assert_eq!(orders_query(25), "limit=25");
}

#[test]
fn bodies() {
    let ob = serde_json::json!({"salt":"1","side":"BUY"});
    let s = submit_body(ob, "api-key-1", "GTC");
    assert_eq!(s["owner"], "api-key-1");
    assert_eq!(s["orderType"], "GTC");
    assert_eq!(s["order"]["side"], "BUY");
    assert_eq!(cancel_body("0xdeadbeef")["orderID"], "0xdeadbeef");
}

// --- the client-side rate-budget mirror ---------------------------------------------------
//
// `crate::transport::ratelimit::budget` is a pure deterministic mirror (time is injected as `now_ms`, it calls
// no `Instant::now()` itself), so every decision this module makes from it is driven here with
// an explicit clock and an explicitly-constructed budget — no sleeping, no network, no
// process-env mutation (`set_var` is unsound under threads — the repo-wide rule).
//
// Standard tier throughout: capacity 40 tokens per bucket, reserve 25% = 10.

/// Capability 1, and the CONTRACT of this PR: with `POLY_RATE_GATE` unset the gate can only
/// ever return `Ok`, so an over-budget submit still reaches the wire exactly as it does today.
/// Turning the flag on converts the same verdict into a named refusal — which the exec thread
/// maps to an `OrderRejected`, so no order silently vanishes either way.
#[test]
fn the_submit_gate_is_inert_while_the_flag_is_unset() {
    let mut rb = RateBudget::new(Tier::Standard, 0);
    // A healthy bucket allows in both modes.
    assert_eq!(submit_gate_action(rb.submit_decision(0), false), Ok(()));
    assert_eq!(submit_gate_action(rb.submit_decision(0), true), Ok(()));

    let before = rate_gate_would_block_count();
    for _ in 0..40 {
        rb.on_submit(0); // drain the order bucket
    }
    let dry = rb.submit_decision(0);
    assert_matches!(dry, SubmitDecision::Throttle { .. }, "expected a dry bucket: {dry:?}");

    // OBSERVE-ONLY (the default): logged + counted, but the order still goes out.
    assert_eq!(submit_gate_action(dry, false), Ok(()));
    // ENFORCED (`venue.polymarket.rate_gate` = `1`): a refusal naming the real cause.
    let refused = submit_gate_action(dry, true).expect_err("enforced mode must refuse");
    assert!(refused.contains("rate budget"), "the reason must name the cause: {refused}");
    // Both modes produce the evidence for deciding to enforce.
    assert!(rate_gate_would_block_count() >= before + 2);
}

/// The venue's settings holding one machine-scoped `venue.polymarket.<field>` row — the shape the
/// composition root reads out of the settings database (decision 0095).
fn settings(field: &str, value: &str) -> vike_secrets::venue_setting::VenueSettings {
    vike_secrets::venue_setting::VenueSettings::from_rows(
        "polymarket",
        &[vike_secrets::VenueSettingRow {
            venue: "polymarket".to_string(),
            tier: None,
            field: vike_venues::venue_fields::VenueFieldName::normalised(field).store_spelling(),
            value: value.to_string(),
        }],
    )
}

/// The enforcement flag is OFF by default and accepts the trimmed EXACT `1` only, read typed from the
/// venue's `venue.polymarket.rate_gate` row (decision 0095: the settings database, no environment
/// layer and — since Task 7 — no credential-map fold): an annotated `1 # …`, which `config set`
/// refuses to write, is not on.
#[test]
fn the_rate_gate_flag_is_off_by_default_and_exact() {
    assert!(!rate_gate_enforced(&vike_secrets::venue_setting::VenueSettings::default()));
    assert!(rate_gate_enforced(&settings("rate_gate", "1")));
    assert!(rate_gate_enforced(&settings("rate_gate", " 1 ")));
    for off in ["0", "true", "yes", "on", "", "11", "1x", "1  # enforce once the headers agree"] {
        assert!(!rate_gate_enforced(&settings("rate_gate", off)), "{off:?}");
    }
    // Another field's row is not this one.
    assert!(!rate_gate_enforced(&settings("presubmit_register", "1")));
}

/// Capability 3: `cancel-all` is taken only while its `1 + n` debit provably cannot overshoot.
/// The squeezed case is the exact hazard the deep-dive flagged — a bulk call that would drive
/// the cancel bucket NEGATIVE and lock out every subsequent cancel.
#[test]
fn a_cancel_all_that_would_overshoot_picks_targeted_instead() {
    let mut rb = RateBudget::new(Tier::Standard, 0);
    // Wide open: 1 + 10 tokens is affordable AND leaves the reserve intact → one bulk call.
    let fat = plan_cancels(&rb, 10, CancelScope::WholeBook, OrderCancelIntent::Routine, 0);
    assert_eq!(fat.strategy, CancelStrategy::CancelAll);
    assert_eq!((fat.send, fat.deferred), (10, 0));

    // Snap the cancel bucket to 12 the way a venue header would, then ask for 15:
    // 1 + 15 = 16 > 12, so `cancel-all` would go negative → targeted.
    rb.reconcile(&RateLimitSignal { cancel_remaining: Some(12.0), ..Default::default() }, 0);
    let squeezed = plan_cancels(&rb, 15, CancelScope::WholeBook, OrderCancelIntent::Routine, 0);
    assert_eq!(squeezed.strategy, CancelStrategy::Targeted);
    // …and the reserve floor (10) caps this routine pass at the 2 tokens sitting above it.
    assert_eq!((squeezed.send, squeezed.deferred), (2, 13));
    assert!(squeezed.retry_ms > 0, "a deferred remainder must carry a refill ETA");

    // A risk-off flatten of the same book ignores the reserve and sends every id.
    let flatten = plan_cancels(&rb, 15, CancelScope::WholeBook, OrderCancelIntent::RiskOff, 0);
    assert_eq!(flatten.strategy, CancelStrategy::Targeted);
    assert_eq!((flatten.send, flatten.deferred, flatten.retry_ms), (15, 0, 0));
}

/// The case the `ExecutionClientActorCommand::CancelBatch` seam exists FOR, and the one the earlier tests
/// never reach: the SAME whole-book batch is held back under `Routine` and goes out as ONE
/// account-wide `cancel-all` under `RiskOff`. That is the trade an emergency wants — `1 + n`
/// tokens instead of `n`, but one round trip instead of `n` — and it is only affordable
/// because the reserve refused the routine churn that would otherwise have spent it.
#[test]
fn a_riskoff_batch_takes_the_bulk_arm_the_reserve_holds_back_from_routine() {
    let mut rb = RateBudget::new(Tier::Standard, 0);
    // Snap the cancel bucket to 12 the way a venue header would. `1 + 6 = 7` is affordable
    // outright, so the venue guard clears the bulk arm — the ONLY thing left deciding is which
    // intent may spend down through the reserve floor (10).
    rb.reconcile(&RateLimitSignal { cancel_remaining: Some(12.0), ..Default::default() }, 0);

    let routine = plan_cancels(&rb, 6, CancelScope::WholeBook, OrderCancelIntent::Routine, 0);
    assert_eq!(routine.strategy, CancelStrategy::Targeted, "churn may not spend the reserve");
    assert_eq!((routine.send, routine.deferred), (2, 4), "only the 2 tokens above the floor");
    assert!(routine.retry_ms > 0, "a deferred remainder must carry a refill ETA");

    let flatten = plan_cancels(&rb, 6, CancelScope::WholeBook, OrderCancelIntent::RiskOff, 0);
    assert_eq!(flatten.strategy, CancelStrategy::CancelAll, "ONE round trip, not six");
    assert_eq!((flatten.send, flatten.deferred, flatten.retry_ms), (6, 0, 0));
}

/// Capability 2, the reserve invariant: routine churn stops with the flatten reserve intact,
/// and a flatten issued right after it still fires every id.
#[test]
fn the_reserve_floor_holds_under_routine_churn() {
    let mut rb = RateBudget::new(Tier::Standard, 0);
    let plan = plan_cancels(&rb, 40, CancelScope::WholeBook, OrderCancelIntent::Routine, 0);
    // 1 + 40 = 41 > 40 tokens, so the bulk arm is out on the venue guard alone…
    assert_eq!(plan.strategy, CancelStrategy::Targeted);
    // …and the reserve caps the churn at 30 of the 40 ids.
    assert_eq!((plan.send, plan.deferred), (30, 10));
    assert!(plan.retry_ms > 0);

    // Spending exactly what the plan authorises leaves the reserve untouched.
    for _ in 0..plan.send {
        rb.on_cancel(0);
    }
    let left = rb.cancel_available(0);
    assert!(left >= 10.0 - 1e-9, "routine churn breached the reserve: {left}");

    // Which is the whole point: the flatten that follows is NOT rate-limit-locked.
    let flatten = plan_cancels(&rb, 10, CancelScope::WholeBook, OrderCancelIntent::RiskOff, 0);
    assert_eq!((flatten.send, flatten.deferred), (10, 0));
}

/// Capability 4, driven exactly as [`log_rate_signal`] drives it: the venue's own header pairs
/// → `parse_headers` → `fold_rate_signal`. A drifted bucket snaps to the server's number; a
/// response carrying no rate headers at all leaves the mirror untouched.
#[test]
fn a_header_reconcile_snaps_a_drifted_bucket() {
    let mut rb = RateBudget::new(Tier::Standard, 0);
    for _ in 0..40 {
        rb.on_submit(0);
    }
    assert!(rb.order_available(0) < 1.0, "the bucket must be drained first");

    let sig = crate::transport::ratelimit::budget::parse_headers([
        ("Poly-RateLimit-Warning", "1"),
        ("poly-ratelimit-order-remaining", "25"),
        ("poly-ratelimit-cancel-remaining", "7"),
    ]);
    assert!(fold_rate_signal(&mut rb, &sig, 0), "an actionable signal must be applied");
    assert!((rb.order_available(0) - 25.0).abs() < 1e-9);
    assert!((rb.cancel_available(0) - 7.0).abs() < 1e-9);

    let quiet =
        crate::transport::ratelimit::budget::parse_headers([("content-type", "application/json")]);
    assert!(!fold_rate_signal(&mut rb, &quiet, 0), "a bare response must change nothing");
    assert!((rb.order_available(0) - 25.0).abs() < 1e-9);
}

/// `DELETE /cancel-all` is ACCOUNT-WIDE, so a subset request may never take it however much
/// budget is free — and an empty request is a no-op plan, never a bulk call.
#[test]
fn a_subset_never_takes_the_account_wide_bulk_arm() {
    let rb = RateBudget::new(Tier::Standard, 0);
    let subset = plan_cancels(&rb, 5, CancelScope::Subset, OrderCancelIntent::Routine, 0);
    assert_eq!(subset.strategy, CancelStrategy::Targeted);
    assert_eq!((subset.send, subset.deferred), (5, 0));

    let empty = plan_cancels(&rb, 0, CancelScope::WholeBook, OrderCancelIntent::RiskOff, 0);
    assert_eq!(
        empty,
        CancelPlan { strategy: CancelStrategy::Targeted, send: 0, deferred: 0, retry_ms: 0 }
    );
}

/// A deferred routine pass is a DELAY, not a deadlock: the reported ETA really does unblock
/// churn again, and a fully refilled bucket clears the whole batch in one bulk call.
#[test]
fn a_deferred_routine_pass_recovers_after_the_reported_refill() {
    let mut rb = RateBudget::new(Tier::Standard, 0);
    rb.reconcile(&RateLimitSignal { cancel_remaining: Some(10.0), ..Default::default() }, 0);
    let stalled = plan_cancels(&rb, 4, CancelScope::WholeBook, OrderCancelIntent::Routine, 0);
    assert_eq!((stalled.send, stalled.deferred), (0, 4), "sitting exactly on the floor");
    assert!(stalled.retry_ms > 0);

    // The ETA is "one token above the reserve again", so it unblocks the pass, not the batch.
    let after =
        plan_cancels(&rb, 4, CancelScope::WholeBook, OrderCancelIntent::Routine, stalled.retry_ms);
    assert!(after.send > 0, "the reported ETA must actually unblock churn: {after:?}");

    // A full second of refill (Standard = 40 tokens/s) restores the cheap bulk path.
    let rested = plan_cancels(&rb, 4, CancelScope::WholeBook, OrderCancelIntent::Routine, 1_000);
    assert_eq!(rested.strategy, CancelStrategy::CancelAll);
    assert_eq!((rested.send, rested.deferred), (4, 0));
}

/// A cancel budget sitting EXACTLY on the reserve floor: Standard tier is a 40-token bucket
/// with a 25% reserve, so 10 remaining is the boundary the per-order gate must act on.
fn budget_at_the_reserve_floor() -> RateBudget {
    let mut rb = RateBudget::new(Tier::Standard, 0);
    rb.reconcile(&RateLimitSignal { cancel_remaining: Some(10.0), ..Default::default() }, 0);
    rb
}

/// THE POINT OF THE WHOLE CHANGE: at the reserve floor a ROUTINE cancel is held back, and the
/// refusal says when to retry. Above the floor it fires like any other cancel.
#[test]
fn the_reserve_holds_back_a_routine_cancel() {
    let mut rb = budget_at_the_reserve_floor();
    let shed = gate_cancel(&mut rb, OrderCancelIntent::Routine, 0)
        .expect_err("a routine cancel at the reserve floor must be shed");
    assert!(shed.contains("reserve"), "the refusal must name why: {shed}");
    assert!(shed.contains("retry"), "…and when to retry: {shed}");

    // Standard refills 40 tokens/s, so a quarter second is comfortably back above the floor.
    assert!(
        gate_cancel(&mut rb, OrderCancelIntent::Routine, 250).is_ok(),
        "above the floor, routine churn is ordinary"
    );
}

/// …and the reserve exists so that THIS one still fires. Same budget, same instant, drained
/// far past the floor and even NEGATIVE (the `cancel-all` over-debit model): an emergency
/// cancel is never held back, which is what the held-back tokens were being held FOR.
#[test]
fn the_reserve_never_holds_back_an_emergency_cancel() {
    let mut rb = budget_at_the_reserve_floor();
    assert!(gate_cancel(&mut rb, OrderCancelIntent::RiskOff, 0).is_ok());

    rb.on_cancel_all(60, 0); // drive the bucket negative
    assert!(rb.cancel_available(0) < 0.0, "fixture: the bucket must be negative");
    assert!(
        gate_cancel(&mut rb, OrderCancelIntent::RiskOff, 0).is_ok(),
        "a flatten fires even from a locked-out bucket — a 429 beats not trying"
    );
}

/// The backward-compatibility claim, as a test: a cancel that named no intent behaves EXACTLY
/// as this venue behaved before the intent existed — it fires, and the gate neither reads nor
/// moves the bucket on its way through.
#[test]
fn an_unspecified_cancel_behaves_exactly_as_today() {
    let mut rb = budget_at_the_reserve_floor();
    let before = rb.cancel_available(0);
    assert!(
        gate_cancel(&mut rb, OrderCancelIntent::Unspecified, 0).is_ok(),
        "an unclassified cancel is treated as the flatten, never shed"
    );
    assert!(
        (rb.cancel_available(0) - before).abs() < 1e-9,
        "the gate must not debit — `cancel_order` owns the debit, and only for what it sends"
    );

    rb.on_cancel_all(60, 0); // negative bucket: still not this gate's business
    assert!(gate_cancel(&mut rb, OrderCancelIntent::Unspecified, 0).is_ok());
}

/// The relayer header pair is now built in ONE place ([`relayer_headers`]) for submit, cancel
/// and the bulk cancel — pin its exact wire names, since the deposit-wallet flow is rejected
/// outright if either is misspelled.
#[test]
fn relayer_headers_keep_the_exact_wire_names() {
    assert_eq!(
        relayer_headers("rk", "0xaddr"),
        vec![
            ("RELAYER_API_KEY".to_string(), "rk".to_string()),
            ("RELAYER_API_KEY_ADDRESS".to_string(), "0xaddr".to_string()),
        ]
    );
}

// --- order-lifecycle holes: the cancel reply and the UNKNOWN submit ---------------------------

/// The CLOB answers HTTP 200 to a cancel it did NOT perform: `not_canceled` names the order and
/// its reason. That must read as a refusal (an `OrderCancelRejected` upstream), while a reply that
/// lists the id under `canceled` is the cancel.
#[test]
fn a_200_cancel_reply_that_names_the_id_in_not_canceled_is_a_refusal() {
    let refused = serde_json::json!({
        "canceled": [],
        "not_canceled": {"0xabc": "order can't be found - or already canceled or matched"}
    });
    assert_eq!(
        not_canceled_reason(&refused, "0xabc").as_deref(),
        Some("not canceled: order can't be found - or already canceled or matched")
    );
    assert_eq!(
        check_cancel_reply(Ok(refused.clone()), "0xabc"),
        Err(CancelFailure::Refused(
            "not canceled: order can't be found - or already canceled or matched".to_string()
        ))
    );

    // another id's refusal is not this id's
    assert_eq!(not_canceled_reason(&refused, "0xdef"), None);
    // an empty reason still refuses
    let blank = serde_json::json!({"not_canceled": {"0xabc": ""}});
    assert_eq!(
        not_canceled_reason(&blank, "0xabc").as_deref(),
        Some("not canceled: order not canceled")
    );

    let done = serde_json::json!({"canceled": ["0xabc"], "not_canceled": {}});
    assert_matches!(check_cancel_reply(Ok(done), "0xabc"), Ok(_));
    // the CLOB's refusal keeps its words
    let refusal = VenueApiError { code: 400, msg: "polymarket 400: x".into() };
    assert_eq!(
        check_cancel_reply(Err(refusal), "0xabc"),
        Err(CancelFailure::Refused("polymarket 400: x".to_string()))
    );
}

/// **PM-5: a cancel 200 that names the order in NEITHER list leaves its outcome unknown, never a cancel.** The
/// documented `DELETE /order` replies (docs.polymarket.com `api-reference/trade/cancel-single-order`):
/// `{"canceled": [id], "not_canceled": {}}` is the cancel, `{"canceled": [], "not_canceled": {id: "Order not found or
/// already canceled"}}` the refusal, as they were. Any other 200 says nothing of the order: it may have been cancelled
/// or have just matched, so it is [`CancelFailure::OutcomeUnknown`], which the exec thread reads back by the id.
#[test]
fn a_cancel_reply_naming_the_order_in_neither_list_leaves_its_outcome_unknown() {
    let canceled = serde_json::json!({"canceled": ["0xabc"], "not_canceled": {}});
    assert_eq!(check_cancel_reply(Ok(canceled.clone()), "0xabc"), Ok(canceled));
    // a hex hash, whatever its case
    let upper = serde_json::json!({"canceled": ["0xABC"], "not_canceled": {}});
    assert_eq!(check_cancel_reply(Ok(upper.clone()), "0xabc"), Ok(upper));
    let refused = serde_json::json!({
        "canceled": [],
        "not_canceled": {"0xabc": "Order not found or already canceled"}
    });
    assert_eq!(
        check_cancel_reply(Ok(refused), "0xabc"),
        Err(CancelFailure::Refused(
            "not canceled: Order not found or already canceled".to_string()
        ))
    );
    for silent in [
        serde_json::json!({"canceled": [], "not_canceled": {}}),
        serde_json::json!({
            "canceled": ["0xdef"],
            "not_canceled": {"0x123": "Order not found or already canceled"}
        }),
        serde_json::json!({}),
        serde_json::json!({"canceled": null, "not_canceled": null}),
    ] {
        assert_eq!(
            check_cancel_reply(Ok(silent.clone()), "0xabc"),
            Err(CancelFailure::OutcomeUnknown(CANCEL_SILENT.to_string())),
            "{silent}"
        );
    }
}

/// **A `cancel-all` 200 cancels only what its `canceled` list names.** The request names no order, so an order the
/// reply names neither way was not cancelled by it (it had just matched, or was not resting): a non-terminal refusal,
/// never the terminal cancel that read a filled order CANCELED for good. A `not_canceled` reason keeps its words.
#[test]
fn a_cancel_all_reply_that_does_not_name_an_order_did_not_cancel_it() {
    let reply = serde_json::json!({
        "canceled": ["0xabc"],
        "not_canceled": {"0xdef": "order can't be found - or already canceled or matched"}
    });
    assert_eq!(cancel_all_refusal(&reply, "0xabc"), None);
    assert_eq!(
        cancel_all_refusal(&reply, "0xdef").as_deref(),
        Some("not canceled: order can't be found - or already canceled or matched")
    );
    for silent in [reply.clone(), serde_json::json!({}), serde_json::json!({"canceled": []})] {
        assert_eq!(
            cancel_all_refusal(&silent, "0x123").as_deref(),
            Some(CANCEL_ALL_SILENT),
            "{silent}"
        );
    }
}

/// **Only a failure that may have reached the CLOB is an UNKNOWN submit, and the error's TYPE says so**
/// (`ErrorKind::must_requery`, as on every other order venue): every send failure that can happen AFTER the
/// body was written is unknown; the provably pre-send kinds and a local refusal (the rate gate, the
/// signing: `not_sent`) are definite, worded `network: …` or as they came. Any 5xx is unknown too (the
/// CLOB may have processed it; owner ruling B-1).
#[test]
fn only_a_failure_after_send_is_an_unknown_submit() {
    use std::io::{Error as IoError, ErrorKind as K};
    use vike_transport::send_outcome::server_error_leaves_write_unknown;
    let post =
        |e: ureq::Error| classify_rest_reply(Err(e), true, &POST_REPLY, &[], |_| {}).unwrap_err();
    let timeout = post(ureq::Error::Timeout(ureq::Timeout::Global));
    assert!(timeout.kind().must_requery(), "a send timeout may have landed: {timeout}");
    for kind in [K::UnexpectedEof, K::ConnectionReset, K::BrokenPipe, K::ConnectionAborted] {
        let e = post(ureq::Error::Io(IoError::new(kind, "peer disconnected")));
        assert!(e.kind().must_requery(), "{kind:?} may follow the write: {e}");
    }
    for e in [ureq::Error::RedirectFailed, ureq::Error::LargeResponseHeader(9, 1)] {
        let e = post(e);
        assert!(e.kind().must_requery(), "{e}");
    }
    for e in [
        ureq::Error::HostNotFound,
        ureq::Error::ConnectionFailed,
        ureq::Error::InvalidProxyUrl,
        ureq::Error::ConnectProxyFailed("407".into()),
        ureq::Error::BadUri("x".into()),
        ureq::Error::Io(IoError::new(K::ConnectionRefused, "refused")),
    ] {
        // ureq's words, except a bad URL's, which never quote it (`send_error_text`).
        let words = format!("network: {}", vike_bridge_core::transport::send_error_text(&e));
        let e = post(e);
        assert_eq!((e.kind().must_requery(), e.msg), (false, words), "never left the host");
    }
    let gate = not_sent("submit rate gate: over budget".to_string());
    assert!(!gate.kind().must_requery(), "{gate}");
    for s in [500, 501, 502, 503, 504, 524] {
        assert!(server_error_leaves_write_unknown(s, None), "{s}");
    }
    for s in [200, 400, 401, 403, 429] {
        assert!(!server_error_leaves_write_unknown(s, None), "{s}");
    }
}

/// Every signed call's words on the shared reply engine: `polymarket {status}: {summary}` for a status,
/// `bad json ({status}): …` for a body that is not JSON, `network: …` for a transport failure; the
/// summary is the engine's (`body_summary`), never the body.
#[test]
fn the_clob_words_carry_the_summary() {
    let bad = serde_json::from_str::<serde_json::Value>("<html>").unwrap_err();
    fn words(w: RestReplyWord<'_>) -> String {
        clob_words(&w)
    }
    assert_eq!(
        words(RestReplyWord::Gateway { status: 503, summary: "busy" }),
        "polymarket 503: busy"
    );
    assert_eq!(words(RestReplyWord::Redirect { status: 302, summary: "" }), "polymarket 302: ");
    let unread = words(RestReplyWord::BadJson { status: 200, error: &bad, summary: "6-byte" });
    assert_eq!(unread, format!("bad json (200): {bad}: 6-byte"));
    let refused = words(RestReplyWord::Refused { status: 400, summary: "x", not_json: None });
    assert_eq!(refused, "polymarket 400: x");
    let page = words(RestReplyWord::Refused { status: 404, summary: "s", not_json: Some(&bad) });
    assert_eq!(page, format!("bad json (404): {bad}: s"));
    let timeout = ureq::Error::Timeout(ureq::Timeout::Global);
    assert_eq!(words(RestReplyWord::BodyUnread(&timeout)), format!("network: {timeout}"));
    assert_eq!(words(RestReplyWord::SendFailed(&timeout)), format!("network: {timeout}"));
}

/// **No credential reaches a signed call's error** (`crate::transport::sign::l1`'s
/// `a_non_json_derive_response_body_never_reaches_the_error`, on the order path): a proxy or CDN page in
/// front of the CLOB answers with the request it received, its `POLY_*` headers included, as a page or as
/// JSON, and none of their values (the key, the passphrase, the signature) reaches the error of an order
/// POST, a cancel, a read or a beat: the text a log line, the journal and an `OrderRejected` reason carry.
/// The CLOB's own `{"error": …}` sentence still does, and the order's verdict is the error's type.
#[test]
fn an_echoed_request_never_reaches_a_signed_calls_error() {
    use crate::execution::test_support::{FakeServer, Reply, declare_direct_egress};
    declare_direct_egress();
    let creds = PolymarketConfig {
        secret: "cG9seW1hcmtldC1sMi1zZWNyZXQta2V5LTEyMzQ1Njc4".to_string().into(),
        api_key: "key-1".to_string().into(),
        passphrase: "pass-1".to_string().into(),
        ..Default::default()
    };
    for (status, json) in [(400, false), (400, true), (502, false), (502, true), (200, false)] {
        let clob = FakeServer::start(move |req| {
            let echo = format!("{:?}", req.headers);
            let body = if json {
                serde_json::json!({ "error": "Bad Gateway", "request": echo }).to_string()
            } else {
                format!("<html><pre>{echo}</pre></html>")
            };
            Reply::text(status, &body)
        });
        let post = post_signed(&clob.base, "/order", &creds, &serde_json::json!({"x": 1}));
        let cancel = del_signed(&clob.base, "/order", &creds, &cancel_body("0xabc"), &[]);
        let read = get_signed(&clob.base, "/data/orders", "limit=1", &creds);
        let beat = post_signed_raw(&clob.base, "/v1/heartbeats", &creds, &serde_json::json!({}));
        let seen = clob.seen();
        let mut secrets: Vec<String> =
            seen.iter().filter_map(|r| r.header("POLY_SIGNATURE")).map(str::to_string).collect();
        assert_eq!(secrets.len(), 4, "{status}: every call was signed and sent");
        secrets.extend(["key-1".to_string(), "pass-1".to_string()]);
        let post = post.expect_err("an echo page is no order");
        assert_eq!(post.kind().must_requery(), status != 400, "{status}: {post}");
        if json {
            assert_eq!(post.msg, format!("polymarket {status}: Bad Gateway"));
        }
        // A JSON non-2xx is the beat's answer to read (its 400 resync), not an error.
        let errors = [Some(post.msg), cancel.err().map(|e| e.msg), read.err(), beat.err()];
        for error in errors.iter().flatten() {
            for secret in &secrets {
                // secret-absent: the signatures, key and passphrase the calls carried
                assert!(!error.contains(secret.as_str()), "{status} json {json}: {error}");
            }
        }
    }
}

/// The confirm reads a row only when it NAMES the derived id, whichever way the reply wraps it
/// (the wrapper is unverified live); everything else is "unknown yet".
#[test]
fn found_order_reads_only_a_row_naming_the_id() {
    let id = "0xAbC123";
    let row = serde_json::json!({"id": "0xabc123", "status": "live", "original_size": "5"});
    for (shape, reply) in [
        ("object", row.clone()),
        ("data object", serde_json::json!({"data": row})),
        ("order object", serde_json::json!({"order": row})),
        ("array", serde_json::json!([row])),
        ("data array", serde_json::json!({"data": [row]})),
        ("order array", serde_json::json!({"order": [row]})),
    ] {
        let found = found_order(&reply, id).unwrap_or_else(|| panic!("{shape}: {reply}"));
        assert_eq!((found.id.as_str(), found.status.as_str()), ("0xabc123", "LIVE"), "{shape}");
    }
    let bare = found_order(&serde_json::json!({"id": "0xABC123"}), id).expect("no status");
    assert_eq!(bare.status, "", "a row without a status reads as an empty one");
    for none in [
        serde_json::Value::Null,
        serde_json::json!({}),
        serde_json::json!([]),
        serde_json::json!({"error": "order not found"}),
        serde_json::json!({"error": "Invalid orderID"}),
        serde_json::json!({"id": "0xother"}),
        serde_json::json!({"id": ""}),
        serde_json::json!({"data": null}),
        serde_json::json!({"data": [{"id": "0xother"}, {"id": "0xabc123"}]}),
    ] {
        assert!(found_order(&none, id).is_none(), "{none}");
    }
}

// --- a signed call never follows a redirect -------------------------------------------------

/// Answer every connection on `l` with `reply`, record each request's text, until `stop` is set.
fn serve_loopback(
    l: std::net::TcpListener,
    reply: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<Vec<String>> {
    use std::io::{Read, Write};
    l.set_nonblocking(true).expect("nonblocking listener");
    std::thread::spawn(move || {
        let mut seen = Vec::new();
        while !stop.load(Ordering::Relaxed) {
            let Ok((mut s, _)) = l.accept() else {
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            };
            s.set_nonblocking(false).expect("blocking stream");
            s.set_read_timeout(Some(std::time::Duration::from_secs(2))).expect("read timeout");
            let mut buffer = [0u8; 4096];
            let n = s.read(&mut buffer).unwrap_or(0);
            seen.push(String::from_utf8_lossy(&buffer[..n]).into_owned());
            let _ = s.write_all(reply.as_bytes());
        }
        seen
    })
}

/// **PM-4: the one typed 5xx is the CLOB's NOT NOW** (`POST_REPLY`'s `verdict_on_5xx`): a 503 whose body names `code`
/// `post_only_mode` is a refusal at once, never an unknown submit; the untyped 503 and 500 beside it stay unknown
/// (owner ruling B-1), and so do a body that only SAYS post-only mode and a code the bridge does not declare. The
/// bodies are the documented ones (docs.polymarket.com `trading/matching-engine`, `resources/error-codes`); no live
/// reply was captured.
#[test]
fn the_typed_post_only_mode_503_is_not_now_and_every_untyped_5xx_stays_unknown() {
    use std::sync::atomic::AtomicBool;
    use vike_utility::errors::FailureOutcome::{NotNow, OutcomeUnknown, Refused};
    // Direct, as `a_signed_call_never_follows_a_redirect` declares it (the declaration is process-wide).
    let _ = crate::transport::declare_egress(crate::transport::EgressSettings::from_lookup(|f| {
        (*f == vike_venues::venue_fields::SOCKS_PROXY).then(|| "none".to_string())
    }));
    let creds = PolymarketConfig {
        secret: "cG9seW1hcmtldC1sMi1zZWNyZXQta2V5LTEyMzQ1Njc4".to_string().into(),
        api_key: "key-1".to_string().into(),
        passphrase: "pass-1".to_string().into(),
        ..Default::default()
    };
    let post_only = "post-only mode: only post-only orders and cancels are allowed";
    let typed = serde_json::json!({ "error": post_only, "code": "post_only_mode", "retry_after_seconds": 79 });
    let rows = [
        (503, typed.clone(), NotNow),
        (503, serde_json::json!({ "error": post_only }), OutcomeUnknown),
        (
            503,
            serde_json::json!({ "error": post_only, "code": "cancel_only_mode" }),
            OutcomeUnknown,
        ),
        (503, serde_json::json!({ "error": "Trading is currently disabled" }), OutcomeUnknown),
        (500, serde_json::json!({ "error": "order timed out" }), OutcomeUnknown),
        (400, typed, Refused),
    ];
    for (status, body, outcome) in rows {
        let body = body.to_string();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://127.0.0.1:{}", listener.local_addr().expect("addr").port());
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let reply = format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        );
        let served = serve_loopback(listener, reply, stop.clone());
        let err = post_signed(&base, "/order", &creds, &serde_json::json!({"x": 1}))
            .expect_err("a non-2xx is no success");
        stop.store(true, Ordering::Relaxed);
        assert_eq!(served.join().expect("server thread").len(), 1, "{status}: one POST");
        assert_eq!(err.kind().failure_outcome(), outcome, "{status} {body}: {err:?}");
        if outcome == NotNow {
            assert_eq!(
                err,
                VenueApiError { code: 503, msg: format!("polymarket 503: {post_only}") }
            );
            let id = vike_model::OrderRefusalId::VenueRefused.word();
            assert_eq!(err.refusal_id_word(), id, "the CLOB's own refusal");
        }
    }
}

/// **A 3xx on a signed call surfaces as an error and the `Location` is never dialled.** ureq's
/// default follows ten redirects and replays EVERY header except `Authorization`/`Cookie` to the
/// target, which would put `POLY_API_KEY`, `POLY_SIGNATURE` and `POLY_PASSPHRASE` on a host the
/// venue never named. The second loopback listener stands in for that host.
///
/// MUTATION PROOF: end `crate::transport::agent_through`'s direct arm in `vike_transport::http::agent_public_get`
/// instead of `agent_no_redirect` — the 302 is followed (the second listener sees the replayed request) and the assertions below fail.
#[test]
fn a_signed_call_never_follows_a_redirect() {
    use std::sync::atomic::AtomicBool;
    // No proxy: the process-wide declaration is the only way to a direct agent, and every unit test
    // that declares it declares this one. Its answer is ignored: a rival declaration only ever differs
    // on a box that is not running unit tests.
    let _ = crate::transport::declare_egress(crate::transport::EgressSettings::from_lookup(|f| {
        (*f == vike_venues::venue_fields::SOCKS_PROXY).then(|| "none".to_string())
    }));
    assert_eq!(crate::transport::proxy_url(), None, "the loopback test must dial direct");

    let target = std::net::TcpListener::bind("127.0.0.1:0").expect("bind target");
    let target_port = target.local_addr().expect("target addr").port();
    let origin = std::net::TcpListener::bind("127.0.0.1:0").expect("bind origin");
    let base = format!("http://127.0.0.1:{}", origin.local_addr().expect("origin addr").port());
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let target_server = serve_loopback(
        target,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\
         Connection: close\r\n\r\n{}"
            .to_string(),
        stop.clone(),
    );
    let origin_server = serve_loopback(
        origin,
        format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{target_port}/stolen\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
        ),
        stop.clone(),
    );

    let creds = PolymarketConfig {
        secret: "cG9seW1hcmtldC1sMi1zZWNyZXQta2V5LTEyMzQ1Njc4".to_string().into(),
        address: "0xabc".to_string(),
        api_key: "key-1".to_string().into(),
        passphrase: "pass-1".to_string().into(),
        ..Default::default()
    };
    let body = serde_json::json!({"x": 1});
    let writes = [
        ("POST", post_signed(&base, "/order", &creds, &body)),
        ("DELETE", del_signed(&base, "/order", &creds, &body, &[])),
    ];
    let read = get_signed(&base, "/data/orders", "limit=1", &creds);

    stop.store(true, Ordering::Relaxed);
    let at_origin = origin_server.join().expect("origin thread");
    let at_target = target_server.join().expect("target thread");

    // Owner ruling B-3: a 3xx on a write (an order POST, a cancel's DELETE) is no verdict on it (unknown); a read's
    // is the definite non-2xx it always was.
    for (verb, write) in writes {
        let err = write.expect_err(&format!("{verb}: a 302 must not read as success"));
        assert!(
            err.kind().must_requery(),
            "{verb}: a redirect answer leaves a write unknown: {err}"
        );
        assert!(err.msg.contains("302"), "{verb}: the error must carry the status: {err}");
    }
    let read = read.expect_err("GET: a 302 must not read as success");
    assert!(read.contains("302"), "GET: the error must carry the status: {read}");
    assert_eq!(at_origin.len(), 3, "each call reached the origin exactly once");
    assert!(at_target.is_empty(), "the redirect target was dialled: {at_target:?}");
}

/// **A cancel, the dead-man beat and a read keep the words they always had**, with the body's summary for the body: a
/// non-2xx is `polymarket {status}: {summary}`, or `bad json ({status}): …: {summary}` for a body that is not JSON; the
/// summary is the CLOB's `{"error": …}` sentence, else the body's size; a read's 410 is `PolymarketReadRefusal::Retired`; and the beat
/// READS a non-2xx JSON body (its 400 resync) instead of refusing it. A cancel is a write: its 5xx (owner ruling B-1,
/// `polymarket {status}: {summary}` whatever the body) and a 2xx that does not parse leave it OUTCOME UNKNOWN, and its
/// 4xx is the CLOB's refusal.
#[test]
fn a_cancel_a_beat_and_a_read_keep_their_words() {
    use std::sync::atomic::AtomicBool;
    // Direct, as `a_signed_call_never_follows_a_redirect` declares it (the declaration is process-wide).
    let _ = crate::transport::declare_egress(crate::transport::EgressSettings::from_lookup(|f| {
        (*f == vike_venues::venue_fields::SOCKS_PROXY).then(|| "none".to_string())
    }));
    assert_eq!(crate::transport::proxy_url(), None, "the loopback test must dial direct");
    let creds = PolymarketConfig {
        secret: "cG9seW1hcmtldC1sMi1zZWNyZXQta2V5LTEyMzQ1Njc4".to_string().into(),
        api_key: "key-1".to_string().into(),
        passphrase: "pass-1".to_string().into(),
        ..Default::default()
    };
    let resync = r#"{"error":"Invalid Heartbeat ID","heartbeat_id":"hb-2"}"#;
    for (status, body, summary) in [
        (200, r#"{"ok":true}"#, ""),
        (200, "<html>", "6-byte body not shown"),
        (400, resync, "Invalid Heartbeat ID"),
        (410, "gone", "4-byte body not shown"),
        (502, r#"{"error":"busy"}"#, "busy"),
        (503, "<html>busy</html>", "17-byte body not shown"),
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://127.0.0.1:{}", listener.local_addr().expect("addr").port());
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let reply = format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        );
        let served = serve_loopback(listener, reply, stop.clone());
        let cancel = del_signed(&base, "/order", &creds, &cancel_body("0xabc"), &[]);
        let read = get_signed(&base, "/data/orders", "limit=1", &creds);
        let beat = post_signed_raw(&base, "/v1/heartbeats", &creds, &serde_json::json!({"x": 1}));
        stop.store(true, Ordering::Relaxed);
        assert_eq!(served.join().expect("server thread").len(), 3, "{status}: three calls");
        // (outcome unknown, words)
        let cancel = cancel.map_err(|e| (e.kind().must_requery(), e.msg));
        let unknown = !(400..500).contains(&status);
        match serde_json::from_str::<serde_json::Value>(body) {
            Ok(json) if status == 200 => {
                assert_eq!(cancel, Ok(json.clone()));
                assert_eq!(read, Ok(json.clone()));
                assert_eq!(beat, Ok((status, json)));
            }
            Ok(json) => {
                let words = format!("polymarket {status}: {summary}");
                assert_eq!(cancel, Err((unknown, words.clone())));
                assert_eq!(read, Err(words));
                assert_eq!(beat, Ok((status, json)), "the beat reads a non-2xx JSON body");
            }
            Err(e) => {
                let words = format!("bad json ({status}): {e}: {summary}");
                // A write's 5xx is unknown by its status, whatever the body: the status's words.
                let status_words = format!("polymarket {status}: {summary}");
                let cancel_words = if status >= 500 { status_words } else { words.clone() };
                assert_eq!(cancel, Err((unknown, cancel_words)));
                assert_eq!(beat, Err(words.clone()));
                let gone = crate::refusals::PolymarketReadRefusal::Retired {
                    path: "/data/orders".to_string(),
                }
                .to_string();
                assert_eq!(read, Err(if status == 410 { gone } else { words }));
            }
        }
    }
    // A read that times out after the write is `network: …`.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://127.0.0.1:{}", silent.local_addr().expect("addr").port());
    let timeout = std::time::Duration::from_millis(300);
    let err = get_signed_within(&base, "/data/orders", "", &creds, timeout).expect_err("silent");
    assert!(err.starts_with("network: "), "{err}");
    drop(silent);
}

/// **A cancel whose reply is lost after the write is OUTCOME UNKNOWN, one that never left NOT SENT, by the error's
/// type** (decision 0303): `cancel_order` against a socket that takes the request and closes with no reply, a 5xx and
/// a 3xx is [`CancelFailure::OutcomeUnknown`]; against a port nothing listens on (the dial refused before the write)
/// [`CancelFailure::NotSent`]; the CLOB's 4xx, 429 and `not_canceled` stay its refusal, in the words they always had.
/// A send timeout reads as the lost reply does, a refused connect as the dead port.
#[test]
fn a_lost_cancel_reply_is_unknown_and_a_refused_dial_is_not_sent() {
    use std::sync::atomic::AtomicBool;
    let _ = crate::transport::declare_egress(crate::transport::EgressSettings::from_lookup(|f| {
        (*f == vike_venues::venue_fields::SOCKS_PROXY).then(|| "none".to_string())
    }));
    assert_eq!(crate::transport::proxy_url(), None, "the loopback test must dial direct");
    let creds = PolymarketConfig {
        secret: "cG9seW1hcmtldC1sMi1zZWNyZXQta2V5LTEyMzQ1Njc4".to_string().into(),
        api_key: "key-1".to_string().into(),
        passphrase: "pass-1".to_string().into(),
        ..Default::default()
    };
    // `cancel_order` against a socket that answers the raw `reply` to the one request it takes.
    let answered = |reply: String| {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://127.0.0.1:{}", listener.local_addr().expect("addr").port());
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let served = serve_loopback(listener, reply, stop.clone());
        let cancel = cancel_order(&base, &creds, "0xabc");
        stop.store(true, Ordering::Relaxed);
        assert_eq!(served.join().expect("server thread").len(), 1, "one request: {cancel:?}");
        cancel
    };
    let http = |status: u16, body: &str| {
        format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
    };
    assert_matches!(answered(String::new()), Err(CancelFailure::OutcomeUnknown(_)));
    let unknown = |words: &str| -> Result<serde_json::Value, _> {
        Err(CancelFailure::OutcomeUnknown(words.to_string()))
    };
    assert_eq!(answered(http(500, r#"{"error":"boom"}"#)), unknown("polymarket 500: boom"));
    assert_eq!(answered(http(302, r#"{"error":"moved"}"#)), unknown("polymarket 302: moved"));
    let refused = |words: &str| -> Result<serde_json::Value, _> {
        Err(CancelFailure::Refused(words.to_string()))
    };
    assert_eq!(
        answered(http(400, r#"{"error":"Invalid orderID"}"#)),
        refused("polymarket 400: Invalid orderID")
    );
    assert_eq!(
        answered(http(429, r#"{"error":"Too Many Requests"}"#)),
        refused("polymarket 429: Too Many Requests")
    );
    let gone = r#"{"canceled":[],"not_canceled":{"0xabc":"Order not found or already canceled"}}"#;
    assert_eq!(
        answered(http(200, gone)),
        refused("not canceled: Order not found or already canceled")
    );
    assert_matches!(
        cancel_order("http://127.0.0.1:9", &creds, "0xabc"),
        Err(CancelFailure::NotSent(_)),
        "nothing listens on port 9"
    );

    let sent = |e: ureq::Error| {
        CancelFailure::from(
            classify_rest_reply(Err(e), true, &POST_REPLY, &[], |_| {}).unwrap_err(),
        )
    };
    assert_matches!(
        sent(ureq::Error::Timeout(ureq::Timeout::Global)),
        CancelFailure::OutcomeUnknown(_)
    );
    assert_matches!(sent(ureq::Error::ConnectionFailed), CancelFailure::NotSent(_));
}

/// **A 5xx to an order POST is an UNKNOWN submit, a 4xx the CLOB's refusal** (owner ruling B-1): the CLOB's
/// `{"error": …}` body carries no code of its own, so nothing says a bare 500 came before it took the order.
#[test]
fn a_5xx_to_an_order_post_is_unknown_and_a_4xx_is_a_refusal() {
    for (status, unknown) in [(500, true), (501, true), (503, true), (400, false)] {
        let err = order_post_answered(status, "boom");
        assert_eq!(err.kind().must_requery(), unknown, "{status}: {err:?}");
        assert_eq!(err.msg, format!("polymarket {status}: boom"));
    }
}

/// **PM-2, PM-3: the 400 words that leave an order POST open** (`leaves_the_order_open`): "context canceled", which
/// the CLOB answers as a 400 whatever the original status, inside any message, and "order match delayed due to market
/// conditions", while the documented refusal beside them stays a refusal. The sentences are the documented ones
/// (docs.polymarket.com `resources/error-codes`); no live reply was captured.
#[test]
fn the_words_that_leave_an_order_post_open() {
    for (sentence, unknown) in [
        ("context canceled", true),
        ("could not insert order: context canceled", true),
        ("order match delayed due to market conditions", true),
        ("not enough balance / allowance", false),
        ("invalid post-only order: order crosses book", false),
    ] {
        let err = order_post_answered(400, sentence);
        assert_eq!(err.kind().must_requery(), unknown, "{sentence}: {err:?}");
        assert_eq!(err.msg, format!("polymarket 400: {sentence}"), "the CLOB's words are kept");
    }
}

/// The error of one order POST a loopback CLOB answers `status` `{"error": sentence}`.
fn order_post_answered(status: u16, sentence: &str) -> VenueApiError {
    use std::sync::atomic::AtomicBool;
    // Direct, as `a_signed_call_never_follows_a_redirect` declares it (the declaration is process-wide).
    let _ = crate::transport::declare_egress(crate::transport::EgressSettings::from_lookup(|f| {
        (*f == vike_venues::venue_fields::SOCKS_PROXY).then(|| "none".to_string())
    }));
    assert_eq!(crate::transport::proxy_url(), None, "the loopback test must dial direct");
    let creds = PolymarketConfig {
        secret: "cG9seW1hcmtldC1sMi1zZWNyZXQta2V5LTEyMzQ1Njc4".to_string().into(),
        api_key: "key-1".to_string().into(),
        passphrase: "pass-1".to_string().into(),
        ..Default::default()
    };
    let body = serde_json::json!({ "error": sentence }).to_string();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://127.0.0.1:{}", listener.local_addr().expect("addr").port());
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let reply = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let served = serve_loopback(listener, reply, stop.clone());
    let err = post_signed(&base, "/order", &creds, &serde_json::json!({"x": 1}))
        .expect_err("a non-2xx is no success");
    stop.store(true, Ordering::Relaxed);
    assert_eq!(served.join().expect("server thread").len(), 1, "{status}: one POST");
    err
}
