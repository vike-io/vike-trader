use super::*;
use crate::transport::{E_TIMEOUT_AMBIGUOUS, ErrorKind};
use vike_exec::event_channel;

/// A taxonomy shaped like a real venue's: an insufficient-balance code, a maintenance code, and
/// a rate-limit code.
const TAX: VenueTaxonomy = VenueTaxonomy {
    venue: "test",
    by_code: |code| match code {
        110_004 => Some(ErrorKind::InsufficientFunds),
        50_001 => Some(ErrorKind::VenueMaintenance),
        _ => None,
    },
    by_msg: |_| None,
};

fn err(code: i64, msg: &str) -> VenueApiError {
    VenueApiError { code, msg: msg.to_string() }
}

/// Build an actor whose command thread returns immediately — this test exercises only
/// `on_submit_error`, which is a pure decision + emit and never touches the command channel.
type Rx = tokio::sync::mpsc::Receiver<vike_exec::Ingest>;

fn actor(policy: ExecErrorPolicy, tax: Option<VenueTaxonomy>) -> (ExecActor, Rx) {
    let (tx, rx) = event_channel(64);
    let a = ExecActor::spawn("test-exec", tx, |_rx| {}).with_error_policy(policy, tax);
    (a, rx)
}

fn drain_rejects(rx: &mut Rx) -> Vec<(String, String)> {
    let mut out = Vec::new();
    while let Ok(vike_exec::Ingest::Event(Event::OrderRejected(r))) = rx.try_recv() {
        out.push((r.client_order_id.clone(), r.reason.to_string()));
    }
    out
}

/// THE non-regression pin, ORDINARY arm: the DEFAULT policy emits exactly what the venues emit
/// today — one terminal reject carrying the venue's RAW message, with no taxonomy decoration.
#[test]
fn legacy_policy_is_byte_identical_to_the_pre_lane_behavior() {
    let (a, mut rx) = actor(ExecErrorPolicy::Legacy, Some(TAX));
    for e in [err(110_004, "Wallet balance is insufficient"), err(50_001, "system maintenance")] {
        let d = a.on_submit_error("c1", 7, &e, SubmitAttempt::first());
        assert!(matches!(d, SubmitDisposition::Reject(_)), "legacy rejects the non-timeout arm");
    }
    let got = drain_rejects(&mut rx);
    assert_eq!(
        got,
        vec![
            ("c1".to_string(), "Wallet balance is insufficient".to_string()),
            ("c1".to_string(), "system maintenance".to_string()),
        ],
        "legacy must carry the RAW venue message, undecorated"
    );
}

/// REGRESSION (adversarial review, major): Legacy must reproduce BOTH of the venues' arms.
/// `binance/spot.rs`, `binance/perp.rs` and the bybit/okx twins match `E_TIMEOUT_AMBIGUOUS`
/// FIRST and route it to `resolve_ambiguous_submit`; only the fall-through arm rejects. The
/// earlier seam collapsed both into a terminal reject — so an adapter doing the natural
/// mechanical adoption ("route the failure through the seam, keep Legacy, flip later") would
/// silently DELETE its audit-T1 re-query and emit a false reject over a possibly-filled order.
#[test]
fn legacy_preserves_the_venues_ambiguous_timeout_requery_arm() {
    let (a, mut rx) = actor(ExecErrorPolicy::Legacy, Some(TAX));
    assert_eq!(
        a.on_submit_error("c1", 7, &err(E_TIMEOUT_AMBIGUOUS, "timed out"), SubmitAttempt::first()),
        SubmitDisposition::Requery,
        "legacy must re-query the ambiguous timeout, exactly like the venue sites do"
    );
    assert!(
        drain_rejects(&mut rx).is_empty(),
        "a phantom position is the cost of rejecting here — legacy must emit NOTHING"
    );
}

/// Legacy's returned kind is the REAL central-baseline classification, not a fabricated
/// `Unknown` — so a caller that branches/meters on the kind (e.g. halting on `Auth`) sees the
/// truth even while that venue is still on Legacy. The EVENT is unchanged (raw venue message).
#[test]
fn legacy_returns_the_real_kind_not_a_fabricated_unknown() {
    let (a, _rx) = actor(ExecErrorPolicy::Legacy, Some(TAX));
    assert_eq!(
        a.on_submit_error("c1", 0, &err(401, "bad key"), SubmitAttempt::first()),
        SubmitDisposition::Reject(ErrorKind::Auth)
    );
}

/// REGRESSION (adversarial review, major): `Retry` emits nothing, so an unbounded retry loop
/// would leave the order with NO terminal event — the "no order may silently vanish" guarantee
/// broken by omission in a caller that does not exist yet. The seam owns the bound: once the
/// budget is spent, the would-be Retry becomes a Reject and the terminal event is emitted HERE.
#[test]
fn classified_retry_is_bounded_and_ends_in_exactly_one_terminal_event() {
    let (a, mut rx) = actor(ExecErrorPolicy::Classified, Some(TAX));
    let e = err(50_001, "system maintenance"); // persistently retryable
    let budget = SubmitRetryBudget::default();
    let mut attempt = SubmitAttempt::first();
    let mut terminal = 0;
    for _ in 0..50 {
        match a.on_submit_error("c9", 0, &e, attempt) {
            SubmitDisposition::Retry(k) => {
                assert_eq!(k, ErrorKind::VenueMaintenance);
                attempt = attempt.next(0);
            }
            SubmitDisposition::Reject(k) => {
                assert_eq!(k, ErrorKind::VenueMaintenance, "the reject names the real cause");
                terminal += 1;
                break;
            }
            SubmitDisposition::Requery => panic!("a maintenance code must not re-query"),
        }
    }
    assert_eq!(terminal, 1, "a persistently-retryable submit MUST reach a terminal event");
    assert_eq!(attempt.attempt, budget.max_attempts, "and only after the budget is spent");
    let rejects = drain_rejects(&mut rx);
    assert_eq!(rejects.len(), 1, "exactly one terminal event, emitted by the seam");
    assert!(
        rejects[0].1.contains("retry budget exhausted") && rejects[0].1.contains("maintenance"),
        "the reason must show BOTH the venue text and that we gave up retrying: {}",
        rejects[0].1
    );
}

/// The elapsed-time arm of the same bound: a slow retry sequence terminates even with attempts
/// to spare (a 30s-stale quote is not the order the operator asked for).
#[test]
fn classified_retry_budget_also_bounds_on_elapsed_time() {
    let (a, mut rx) = actor(ExecErrorPolicy::Classified, Some(TAX));
    let e = err(50_001, "system maintenance");
    assert_eq!(
        a.on_submit_error("c10", 0, &e, SubmitAttempt { attempt: 2, elapsed_ms: 30_000 }),
        SubmitDisposition::Reject(ErrorKind::VenueMaintenance)
    );
    assert_eq!(drain_rejects(&mut rx).len(), 1);
}

/// The ambiguous timeout is NEVER budget-converted: the venue may hold the order, so a
/// synthesized reject would strand a phantom position. It stays `Requery` however long it runs.
#[test]
fn requery_is_never_converted_by_the_retry_budget() {
    let (a, mut rx) = actor(ExecErrorPolicy::Classified, Some(TAX));
    assert_eq!(
        a.on_submit_error(
            "c11",
            0,
            &err(E_TIMEOUT_AMBIGUOUS, "timed out"),
            SubmitAttempt { attempt: 999, elapsed_ms: u64::MAX }
        ),
        SubmitDisposition::Requery
    );
    assert!(drain_rejects(&mut rx).is_empty(), "audit T1 outranks the retry budget");
}

/// A terminal classified failure emits the reject WITH the kind named in the reason.
#[test]
fn classified_terminal_error_rejects_with_the_kind_in_the_reason() {
    let (a, mut rx) = actor(ExecErrorPolicy::Classified, Some(TAX));
    let d = a.on_submit_error(
        "c2",
        9,
        &err(110_004, "Wallet balance is insufficient"),
        SubmitAttempt::first(),
    );
    assert_eq!(d, SubmitDisposition::Reject(ErrorKind::InsufficientFunds));
    assert_eq!(
        drain_rejects(&mut rx),
        vec![(
            "c2".to_string(),
            "venue error [insufficient_funds]: Wallet balance is insufficient".to_string()
        )]
    );
}

/// A retryable failure emits NOTHING — losing an order a backoff would have placed is the exact
/// failure mode the classified path exists to remove.
#[test]
fn classified_transient_error_emits_no_terminal_event() {
    let (a, mut rx) = actor(ExecErrorPolicy::Classified, Some(TAX));
    assert_eq!(
        a.on_submit_error("c3", 0, &err(50_001, "system maintenance"), SubmitAttempt::first()),
        SubmitDisposition::Retry(ErrorKind::VenueMaintenance)
    );
    assert_eq!(
        a.on_submit_error("c3", 0, &err(429, "http error"), SubmitAttempt::first()),
        SubmitDisposition::Retry(ErrorKind::RateLimited)
    );
    assert!(drain_rejects(&mut rx).is_empty(), "a retryable failure must NOT reject the order");
}

/// AUDIT T1 under the new policy: the ambiguous timeout re-queries and emits nothing. Emitting a
/// reject here would strand a phantom position if the venue did accept the order.
#[test]
fn classified_ambiguous_timeout_requeries_and_never_rejects() {
    let (a, mut rx) = actor(ExecErrorPolicy::Classified, Some(TAX));
    assert_eq!(
        a.on_submit_error("c4", 0, &err(E_TIMEOUT_AMBIGUOUS, "timed out"), SubmitAttempt::first()),
        SubmitDisposition::Requery
    );
    assert!(drain_rejects(&mut rx).is_empty(), "the ambiguous path must NEVER synthesize a reject");
}

/// An unknown venue code is terminal — the safe posture — and still discharges the contract.
#[test]
fn classified_unknown_code_rejects() {
    let (a, mut rx) = actor(ExecErrorPolicy::Classified, Some(TAX));
    assert_eq!(
        a.on_submit_error("c5", 0, &err(987_654, "brand new failure"), SubmitAttempt::first()),
        SubmitDisposition::Reject(ErrorKind::Unknown)
    );
    assert_eq!(drain_rejects(&mut rx).len(), 1, "an unknown failure must not vanish");
}

/// Classified WITHOUT a taxonomy still works — it just falls back to the central baseline.
#[test]
fn classified_without_a_taxonomy_uses_the_central_baseline() {
    let (a, _rx) = actor(ExecErrorPolicy::Classified, None);
    // 110004 is unknown to the baseline (only the venue table knows it) → terminal Unknown.
    assert_eq!(
        a.on_submit_error(
            "c6",
            0,
            &err(110_004, "Wallet balance is insufficient"),
            SubmitAttempt::first()
        ),
        SubmitDisposition::Reject(ErrorKind::Unknown)
    );
    // A baseline-known code still classifies.
    assert_eq!(
        a.on_submit_error("c6", 0, &err(500, "http error"), SubmitAttempt::first()),
        SubmitDisposition::Retry(ErrorKind::ServerError)
    );
}
