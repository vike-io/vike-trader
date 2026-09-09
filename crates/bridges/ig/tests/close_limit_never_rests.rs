//! IG's close endpoint CANNOT rest — the measured evidence behind `CLOSE_MARKET_ONLY`.
//!
//! PR #1431 refused a `reduce_only` LIMIT/STOP terminally because whether
//! `POST /positions/otc` + `_method: DELETE` with `orderType: LIMIT` actually RESTS was
//! unverified. It is now verified, live, and the answer is NO — the endpoint's LIMIT arm is
//! execute-at-level-or-better-NOW, and it has no STOP arm at all. Measured against
//! `demo-api.ig.com` on 2026-08-22 by `tests/ig_limit_close_probe.rs` (the re-runnable probe),
//! against an open 0.5 LONG in `IX.D.SUNNAS.BMU.IP` (bid 29320.1 / offer 29360.1):
//!
//! | probe | request | IG's answer |
//! |---|---|---|
//! | LIMIT close, NON-marketable level (29448.2, above the offer — a resting close if one exists) | `{direction: SELL, epic, expiry: "-", level: "29448.2", orderType: LIMIT, size: 0.5}` | 200 + dealReference, then confirm `dealStatus: REJECTED`, `reason: LIMIT_ORDER_WRONG_SIDE_OF_MARKET`; position untouched, `/workingorders` empty |
//! | LIMIT close, MARKETABLE level (29232.1, below the bid) | same shape | executed IMMEDIATELY at the bid 29320.1 (`dealStatus: ACCEPTED`, `status: CLOSED`) — level-or-better, never resting |
//! | STOP close (level 29232.1) | same shape, `orderType: STOP` | HTTP **400** `invalid.request.orderType` — the close endpoint has no STOP arm |
//!
//! So the `CLOSE_MARKET_ONLY` refusal in `crates/bridges/ig/src/exec.rs` STAYS: there is no
//! resting reduce-only close to wire, and the only honest treatments of a `reduce_only`
//! LIMIT/STOP are the loud terminal refusal (today's behaviour) or a silent coercion into an
//! immediate close — the defect class this workspace refuses on every venue.
//!
//! `fixtures/confirm_close_limit_rejected.json` is the first row's confirm, verbatim; the tests
//! below drive BOTH real mappers over it, because if that body were ever sent the answer must be
//! exactly one terminal on whichever lane delivers it first — never a silent vanish, never an
//! accept for a deal IG refused.

use serde_json::Value;
use vike_model::events::Event;

fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.json"));
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing captured fixture {}: {e}", path.display()));
    serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("malformed captured fixture {}: {e}", path.display()))
}

/// The sync lane: a refused close-limit confirm maps to EXACTLY one event, the terminal
/// `OrderRejected`, carrying IG's own reason — no accept for a deal IG refused, and no fill.
#[test]
fn a_rejected_close_limit_confirm_is_one_terminal_reject_on_the_sync_lane() {
    let confirm = fixture("confirm_close_limit_rejected");
    assert_eq!(
        confirm["reason"].as_str(),
        Some("LIMIT_ORDER_WRONG_SIDE_OF_MARKET"),
        "the fixture really is the venue's cannot-rest answer"
    );
    let evs = vike_ig::map_confirm("coid-close-limit", 7, true, &confirm);
    assert_eq!(evs.len(), 1, "exactly one event, the terminal: {evs:?}");
    match &evs[0] {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, "coid-close-limit");
            assert_eq!(
                r.reason, "LIMIT_ORDER_WRONG_SIDE_OF_MARKET",
                "IG's own reason reaches the strategy verbatim"
            );
        }
        other => panic!("expected the terminal OrderRejected, got {other:?}"),
    }
}

/// The streamed twin: the same confirm arriving on the Lightstreamer `CONFIRMS` lane decodes to
/// the same single terminal. The two lanes agreeing is what makes the second copy a no-op in the
/// FSM instead of a divergent verdict.
#[test]
fn the_streamed_lane_agrees_one_terminal_reject() {
    let confirm = fixture("confirm_close_limit_rejected");
    let evs = vike_ig::decode_trade_confirm(&confirm, "coid-close-limit", 7);
    assert_eq!(evs.len(), 1, "exactly one event on the stream too: {evs:?}");
    assert!(
        matches!(&evs[0], Event::OrderRejected(r) if r.reason == "LIMIT_ORDER_WRONG_SIDE_OF_MARKET"),
        "the streamed decode must reach the same terminal: {evs:?}"
    );
}
