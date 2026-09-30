use super::*;
use vike_tradehub_client::wire::WireOrderRequest;

fn submit(qty: f64, price: Option<f64>) -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: "c1".into(),
        venue: "hyperliquid".into(),
        symbol: "BTC".into(),
        side: 1,
        qty,
        order_type: "limit".into(),
        price,
        trigger_price: None,
        reduce_only: false,
        account: None,
    })
}

fn limits(max_notional: Option<f64>, rate: f64) -> ControlLimits {
    // The exact per-connection construction `handle_connection` performs (audit F13) — a fresh
    // full bucket over a caller-owned config, no env involved.
    ControlLimits::new(ControlLimitsConfig { max_notional, rate_per_sec: rate })
}

#[test]
fn notional_cap_rejects_oversized_submit_allows_small() {
    let mut l = limits(Some(1_000.0), 1e9); // huge rate so ONLY the size cap is under test
    assert!(l.vet(&submit(100.0, Some(50.0))).is_some(), "100*50=5000 > 1000 → refused");
    assert!(l.vet(&submit(10.0, Some(50.0))).is_none(), "10*50=500 ≤ 1000 → allowed");
    assert!(l.vet(&submit(-100.0, Some(50.0))).is_some(), "|qty| used: -100*50=5000 → refused");
}

#[test]
fn market_order_without_a_price_skips_the_size_cap() {
    // No price → the server can't compute notional → not size-capped (the RiskGate still applies).
    let mut l = limits(Some(1.0), 1e9);
    assert!(l.vet(&submit(1_000_000.0, None)).is_none());
}

/// THE MODIFY HOLE: the cap matched `Modify { new_qty: Some, new_price: Some }` — BOTH
/// required — so omitting the price fell to `_ => None` and the ceiling did not apply at all.
/// `/orders` prints coids, so the reachable sequence from either remote surface was
/// `/orders` → `/modify <coid> qty=<huge>` with no price named. Unsizeable must fail CLOSED.
#[test]
fn a_priceless_qty_raising_modify_is_refused_not_waved_through() {
    let mut l = limits(Some(1_000.0), 1e9);
    let reason = l
        .vet(&WireCommand::Modify {
            client_order_id: "c1".into(),
            new_qty: Some(1e9),
            new_price: None,
        })
        .expect("a modify the edge cannot size must be REFUSED, not allowed");
    assert!(
        reason.contains("no price"),
        "the refusal must tell the operator how to make it checkable, got {reason:?}"
    );
    // ...and the same shape is refused on the read-only preview, so a preview can never report
    // a command as fine that `vet` would refuse (they share `notional_reason`).
    assert!(
        l.preview_vet(&WireCommand::Modify {
            client_order_id: "c1".into(),
            new_qty: Some(1e9),
            new_price: None,
        })
        .is_some()
    );
}

/// A priced modify is still sized normally — the refusal above must not become a blanket ban on
/// the verb, and a price-only modify (no qty change) carries no new size to check.
#[test]
fn a_priced_modify_is_capped_and_a_price_only_modify_is_not_refused() {
    let mut l = limits(Some(1_000.0), 1e9);
    let m = |q: Option<f64>, p: Option<f64>| WireCommand::Modify {
        client_order_id: "c1".into(),
        new_qty: q,
        new_price: p,
    };
    assert!(l.vet(&m(Some(100.0), Some(50.0))).is_some(), "100*50=5000 > 1000 → refused");
    assert!(l.vet(&m(Some(10.0), Some(50.0))).is_none(), "10*50=500 ≤ 1000 → allowed");
    // No qty change ⇒ nothing to size at this edge (the core RiskGate judges the projected
    // order either way, and it DOES hold the resting terms).
    assert!(l.vet(&m(None, Some(50.0))).is_none(), "a price-only modify is not size-refused");
    assert!(l.vet(&m(None, None)).is_none(), "an empty modify is not size-refused");
}

/// NOTIONAL IS A MAGNITUDE. The cap abs'd the QTY but not the PRICE, so a negative price made
/// `n` negative, `n > max` could never trip, and the ceiling was bypassed by a sign alone —
/// on BOTH the submit and the modify arm. (`vike_model::order_notional`, which the core
/// `RiskGate` uses, abs's every factor; this edge did not.)
#[test]
fn a_negative_price_cannot_slip_past_the_cap() {
    let mut l = limits(Some(1_000.0), 1e9);
    assert!(
        l.vet(&submit(100.0, Some(-50.0))).is_some(),
        "|100 * -50| = 5000 > 1000 → refused; without the price abs this returned -5000 and passed"
    );
    assert!(
        l.vet(&WireCommand::Modify {
            client_order_id: "c1".into(),
            new_qty: Some(100.0),
            new_price: Some(-50.0),
        })
        .is_some(),
        "the modify arm had the identical sign hole"
    );
}

#[test]
fn risk_reducing_verbs_are_never_size_capped() {
    let mut l = limits(Some(1.0), 1e9);
    assert!(l.vet(&WireCommand::Cancel("c1".into())).is_none());
    assert!(
        l.vet(&WireCommand::Flatten {
            venue: "hyperliquid".into(),
            symbol: "BTC".into(),
            account: None
        })
        .is_none()
    );
    assert!(l.vet(&WireCommand::MarketExit { venue: None, account: None }).is_none());
    assert!(l.vet(&WireCommand::MassCancel { venue: None, symbol: None, account: None }).is_none());
}

/// The B4 vetting decision, pinned: `UpdateParams` is NOT notional-capped (it is not an order —
/// see the `notional_reason` arm for the argument) but it DOES consume a rate token like every
/// command, so a re-tune flood is still rate-refused.
#[test]
fn update_params_skips_the_size_cap_but_still_pays_the_rate_token() {
    let up = || WireCommand::UpdateParams {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        params: serde_json::json!({"SpreadMaker": {"qty": 1e12}}),
    };
    // A tiny notional ceiling that would refuse ANY sized order: the re-tune passes anyway.
    let mut sized = limits(Some(0.000_001), 1e9);
    assert!(sized.vet(&up()).is_none(), "a params update carries no order notional to cap");
    // …but the rate bucket applies: 2/s ⇒ the third immediate re-tune is refused.
    let mut rated = limits(None, 2.0);
    assert!(rated.vet(&up()).is_none(), "token 1");
    assert!(rated.vet(&up()).is_none(), "token 2");
    assert!(rated.vet(&up()).is_some(), "3rd immediate re-tune → rate limited");
}

#[test]
fn rate_limit_refuses_a_burst_past_the_cap() {
    let mut l = limits(None, 2.0); // 2/s, bucket starts full at 2 tokens
    assert!(l.vet(&submit(1.0, Some(1.0))).is_none(), "token 1");
    assert!(l.vet(&submit(1.0, Some(1.0))).is_none(), "token 2");
    assert!(l.vet(&submit(1.0, Some(1.0))).is_some(), "3rd immediate command → rate limited");
}

#[test]
fn no_size_cap_without_a_policy_ceiling() {
    // No `policy.toml` (or no `max_notional_per_order` key) ⇒ `from_policy(None, ..)` ⇒
    // `max_notional: None` ⇒ any notional passes (the RiskGate is the floor); a huge order is
    // NOT refused by the size gate. This is today's default, unchanged by Phase 5.
    let mut l = limits(None, 1e9);
    assert!(l.vet(&submit(1e9, Some(1e9))).is_none());
}

// --- ControlLimitsConfig::from_policy — the pure resolver main.rs feeds the loaded POLICY
// ceiling and the raw rate value to (audit F13: the semantics the retired per-connection
// `from_env` had, now unit-testable and with the ceiling no longer env-settable). ---

#[test]
fn from_policy_with_nothing_set_gives_the_default_config() {
    assert_eq!(ControlLimitsConfig::from_policy(None, None), ControlLimitsConfig::default());
    // …and the default is: no size cap, DEFAULT_CONTROL_RATE commands/sec.
    assert_eq!(
        ControlLimitsConfig::default(),
        ControlLimitsConfig { max_notional: None, rate_per_sec: DEFAULT_CONTROL_RATE }
    );
}

/// The Phase-5 "value flows" property at this end: a `policy.toml` ceiling becomes the
/// server-edge notional cap, while the rate keeps its (still env-settable) string parse.
#[test]
fn from_policy_carries_the_ceiling_through_and_still_trims_the_rate() {
    assert_eq!(
        ControlLimitsConfig::from_policy(Some(250.5), Some(" 40 ")),
        ControlLimitsConfig { max_notional: Some(250.5), rate_per_sec: 40.0 }
    );
}

#[test]
fn from_policy_treats_a_nonsense_ceiling_or_rate_as_unset() {
    // A non-positive/non-finite ceiling degrades to OFF rather than arming a limit that would
    // refuse EVERY order (a silent halt) — `vike_config` already rejects both when loading
    // `policy.toml`, so this only bites a `Policy` built some other way. Garbage/non-positive
    // rate falls back to the default, exactly as before.
    for bad in [Some(0.0), Some(-1.0), Some(f64::NAN), Some(f64::INFINITY)] {
        assert_eq!(ControlLimitsConfig::from_policy(bad, None), ControlLimitsConfig::default());
    }
    assert_eq!(
        ControlLimitsConfig::from_policy(None, Some("nope")),
        ControlLimitsConfig::default()
    );
    assert_eq!(ControlLimitsConfig::from_policy(None, Some("-3")), ControlLimitsConfig::default());
}

#[test]
fn preview_vet_applies_the_notional_cap() {
    // The dry-run verdict mirrors the executing `vet`'s notional decision (oversized → reason,
    // within-cap → None), so a Preview reflects the real gate.
    let l = limits(Some(1_000.0), 1e9);
    assert!(l.preview_vet(&submit(100.0, Some(50.0))).is_some(), "5000 > 1000 → would refuse");
    assert!(l.preview_vet(&submit(10.0, Some(50.0))).is_none(), "500 ≤ 1000 → would pass");
}

#[test]
fn preview_vet_consumes_no_rate_token() {
    // A preview must not drain the command budget nor be rate-limited: even a tiny bucket answers
    // every preview, and the real `vet` afterward still has its full allotment of tokens.
    let mut l = limits(None, 2.0);
    for _ in 0..100 {
        assert!(
            l.preview_vet(&submit(1.0, Some(1.0))).is_none(),
            "previews are never rate-limited"
        );
    }
    // The bucket is untouched: two real commands still pass, the third is rate-limited (as if no
    // preview had ever happened).
    assert!(l.vet(&submit(1.0, Some(1.0))).is_none(), "token 1 intact");
    assert!(l.vet(&submit(1.0, Some(1.0))).is_none(), "token 2 intact");
    assert!(
        l.vet(&submit(1.0, Some(1.0))).is_some(),
        "3rd → rate limited, so previews consumed none"
    );
}
