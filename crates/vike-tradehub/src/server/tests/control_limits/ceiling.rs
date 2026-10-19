//! The notional cap, the rate token, `ControlLimitsConfig::from_policy` and `preview_vet`.
use super::*;

#[test]
fn notional_cap_rejects_oversized_submit_allows_small() {
    let mut l = limits(Some(1_000.0), 1e9); // huge rate so ONLY the size cap is under test
    assert!(l.vet(&submit(100.0, Some(50.0)), &[], &[]).is_some(), "100*50=5000 > 1000 → refused");
    assert!(l.vet(&submit(10.0, Some(50.0)), &[], &[]).is_none(), "10*50=500 ≤ 1000 → allowed");
    assert!(
        l.vet(&submit(-100.0, Some(50.0)), &[], &[]).is_some(),
        "|qty| used: -100*50=5000 → refused"
    );
}

/// **The door's per-order ceiling (`policy.max_notional_per_order`) binds an order addressed to a LIVE lane
/// twin exactly as it binds any other**: the address is the twin's (`binance#PERP`, published at `live`), the
/// verdict is the same number on the same arithmetic, and the read-only preview agrees with the send. A
/// tier-blind ceiling is the one the twin must never get around (the owner opened its `live` tier,
/// 2026-10-19).
#[test]
fn the_notional_cap_binds_an_order_addressed_to_a_live_lane_twin() {
    let live = |route_key: &str, symbol: &str| vike_exec::VenueBlock {
        venue: "binance".into(),
        route_key: route_key.into(),
        symbol: symbol.into(),
        tier: Some(vike_model::AccountTier::Live),
        ..Default::default()
    };
    let blocks = [live("binance", "BTCUSDT"), live("binance#PERP", "BTCUSDT.P")];
    let to = |account: Option<&str>, symbol: &str, qty: f64| {
        WireCommand::Submit(WireOrderRequest {
            client_order_id: "c1".into(),
            venue: "binance".into(),
            symbol: symbol.into(),
            side: 1,
            qty,
            order_type: "limit".into(),
            price: Some(50.0),
            trigger_price: None,
            reduce_only: false,
            account: account.map(str::to_string),
        })
    };
    let mut l = limits(Some(1_000.0), 1e9);
    for (account, symbol) in [(Some("PERP"), "BTCUSDT.P"), (None, "BTCUSDT")] {
        let big = to(account, symbol, 100.0);
        assert!(l.vet(&big, &blocks, &[]).is_some(), "{account:?}: 100*50=5000 > 1000 → refused");
        assert!(l.preview_vet(&big, &blocks, &[]).is_some(), "{account:?}: the preview agrees");
        let small = to(account, symbol, 10.0);
        assert!(l.vet(&small, &blocks, &[]).is_none(), "{account:?}: 10*50=500 ≤ 1000 → allowed");
        assert!(l.preview_vet(&small, &blocks, &[]).is_none(), "{account:?}: the preview agrees");
    }
}

#[test]
fn market_order_without_a_price_skips_the_size_cap() {
    // No price → the server can't compute notional → not size-capped (the RiskGate still applies).
    let mut l = limits(Some(1.0), 1e9);
    assert!(l.vet(&submit(1_000_000.0, None), &[], &[]).is_none());
}

/// THE MODIFY HOLE: the cap matched `Modify { new_qty: Some, new_price: Some }` — BOTH
/// required — so omitting the price fell to `_ => None` and the ceiling did not apply at all.
/// `/orders` prints coids, so the reachable sequence from either remote surface was
/// `/orders` → `/modify <coid> qty=<huge>` with no price named. Unsizeable must fail CLOSED.
#[test]
fn a_priceless_qty_raising_modify_is_refused_not_waved_through() {
    let mut l = limits(Some(1_000.0), 1e9);
    let reason = l
        .vet(
            &WireCommand::Modify {
                client_order_id: "c1".into(),
                new_qty: Some(1e9),
                new_price: None,
            },
            &[],
            &[],
        )
        .expect("a modify the edge cannot size must be REFUSED, not allowed");
    assert!(
        reason.contains("no price"),
        "the refusal must tell the operator how to make it checkable, got {reason:?}"
    );
    // ...and the same shape is refused on the read-only preview, so a preview can never report
    // a command as fine that `vet` would refuse (they share `notional_reason`).
    assert!(
        l.preview_vet(
            &WireCommand::Modify {
                client_order_id: "c1".into(),
                new_qty: Some(1e9),
                new_price: None,
            },
            &[],
            &[],
        )
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
    assert!(l.vet(&m(Some(100.0), Some(50.0)), &[], &[]).is_some(), "100*50=5000 > 1000 → refused");
    assert!(l.vet(&m(Some(10.0), Some(50.0)), &[], &[]).is_none(), "10*50=500 ≤ 1000 → allowed");
    // No qty change ⇒ nothing to size at this edge (the core RiskGate judges the projected
    // order either way, and it DOES hold the resting terms).
    assert!(
        l.vet(&m(None, Some(50.0)), &[], &[]).is_none(),
        "a price-only modify is not size-refused"
    );
    assert!(l.vet(&m(None, None), &[], &[]).is_none(), "an empty modify is not size-refused");
}

/// NOTIONAL IS A MAGNITUDE. The cap abs'd the QTY but not the PRICE, so a negative price made
/// `n` negative, `n > max` could never trip, and the ceiling was bypassed by a sign alone —
/// on BOTH the submit and the modify arm. (`vike_model::order_notional`, which the core
/// `RiskGate` uses, abs's every factor; this edge did not.)
#[test]
fn a_negative_price_cannot_slip_past_the_cap() {
    let mut l = limits(Some(1_000.0), 1e9);
    assert!(
        l.vet(&submit(100.0, Some(-50.0)), &[], &[]).is_some(),
        "|100 * -50| = 5000 > 1000 → refused; without the price abs this returned -5000 and passed"
    );
    assert!(
        l.vet(
            &WireCommand::Modify {
                client_order_id: "c1".into(),
                new_qty: Some(100.0),
                new_price: Some(-50.0),
            },
            &[],
            &[],
        )
        .is_some(),
        "the modify arm had the identical sign hole"
    );
}

#[test]
fn risk_reducing_verbs_are_never_size_capped() {
    let mut l = limits(Some(1.0), 1e9);
    assert!(l.vet(&WireCommand::Cancel("c1".into()), &[], &[]).is_none());
    assert!(
        l.vet(
            &WireCommand::Flatten {
                venue: "hyperliquid".into(),
                symbol: "BTC".into(),
                account: None
            },
            &[],
            &[],
        )
        .is_none()
    );
    assert!(l.vet(&WireCommand::MarketExit { venue: None, account: None }, &[], &[]).is_none());
    assert!(
        l.vet(&WireCommand::MassCancel { venue: None, symbol: None, account: None }, &[], &[])
            .is_none()
    );
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
        mount_id: None,
        params: serde_json::json!({"SpreadMaker": {"qty": 1e12}}),
    };
    // A tiny notional ceiling that would refuse ANY sized order: the re-tune passes anyway.
    let mut sized = limits(Some(0.000_001), 1e9);
    assert!(sized.vet(&up(), &[], &[]).is_none(), "a params update carries no order notional");
    // …but the rate bucket applies: 2/s ⇒ the third immediate re-tune is refused.
    let mut rated = limits(None, 2.0);
    assert!(rated.vet(&up(), &[], &[]).is_none(), "token 1");
    assert!(rated.vet(&up(), &[], &[]).is_none(), "token 2");
    assert!(rated.vet(&up(), &[], &[]).is_some(), "3rd immediate re-tune → rate limited");
}

#[test]
fn rate_limit_refuses_a_burst_past_the_cap() {
    let mut l = limits(None, 2.0); // 2/s, bucket starts full at 2 tokens
    assert!(l.vet(&submit(1.0, Some(1.0)), &[], &[]).is_none(), "token 1");
    assert!(l.vet(&submit(1.0, Some(1.0)), &[], &[]).is_none(), "token 2");
    assert!(
        l.vet(&submit(1.0, Some(1.0)), &[], &[]).is_some(),
        "3rd immediate command → rate limited"
    );
}

#[test]
fn no_size_cap_without_a_policy_ceiling() {
    // No `policy.max_notional_per_order` row ⇒ `from_policy(None, ..)` ⇒
    // `max_notional: None` ⇒ any notional passes (the RiskGate is the floor); a huge order is
    // NOT refused by the size gate. This is today's default, unchanged by Phase 5.
    let mut l = limits(None, 1e9);
    assert!(l.vet(&submit(1e9, Some(1e9)), &[], &[]).is_none());
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

/// The Phase-5 "value flows" property at this end: a `policy.max_notional_per_order` ceiling
/// becomes the server-edge notional cap, while the rate keeps its (still env-settable) string
/// parse.
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
    // the `policy` rows, so this only bites a `Policy` built some other way. Garbage/non-positive
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
    assert!(
        l.preview_vet(&submit(100.0, Some(50.0)), &[], &[]).is_some(),
        "5000 > 1000 → would refuse"
    );
    assert!(
        l.preview_vet(&submit(10.0, Some(50.0)), &[], &[]).is_none(),
        "500 ≤ 1000 → would pass"
    );
}

#[test]
fn preview_vet_consumes_no_rate_token() {
    // A preview must not drain the command budget nor be rate-limited: even a tiny bucket answers
    // every preview, and the real `vet` afterward still has its full allotment of tokens.
    let mut l = limits(None, 2.0);
    for _ in 0..100 {
        assert!(
            l.preview_vet(&submit(1.0, Some(1.0)), &[], &[]).is_none(),
            "previews are never rate-limited"
        );
    }
    // The bucket is untouched: two real commands still pass, the third is rate-limited (as if no
    // preview had ever happened).
    assert!(l.vet(&submit(1.0, Some(1.0)), &[], &[]).is_none(), "token 1 intact");
    assert!(l.vet(&submit(1.0, Some(1.0)), &[], &[]).is_none(), "token 2 intact");
    assert!(
        l.vet(&submit(1.0, Some(1.0)), &[], &[]).is_some(),
        "3rd → rate limited, so previews consumed none"
    );
}

/// A qty-only modify of an order the node KNOWS is sized at that order's standing price (the venue
/// keeps it): within the ceiling it passes, over it it is refused for its NOTIONAL, not for naming
/// no price. An order with no standing price (or none known) stays the refusal above.
#[test]
fn a_qty_only_modify_is_sized_at_the_standing_price() {
    let mut l = limits(Some(1_000.0), 1e9);
    let open =
        [published_order("c1", "binance", None, "BTCUSDT", vike_model::OrderStatus::Accepted)];
    let m = |q: f64| WireCommand::Modify {
        client_order_id: "c1".into(),
        new_qty: Some(q),
        new_price: None,
    };
    assert!(l.vet(&m(500.0), &[], &open).is_none(), "500 * standing 1.0 <= 1000 -> allowed");
    let reason = l.vet(&m(2_000.0), &[], &open).expect("2000 * 1.0 > 1000 -> refused");
    assert!(
        !reason.contains("no price"),
        "refused for its size, not for the missing price: {reason}"
    );
}

fn qty_only(q: f64) -> WireCommand {
    WireCommand::Modify { client_order_id: "c1".into(), new_qty: Some(q), new_price: None }
}

/// A standing price of 0 (or a negative / non-finite one) is NO price: notional 0 would let any size
/// pass the ceiling. A market order (no price) and a stop-market order are the same refusal.
#[test]
fn a_qty_only_modify_of_a_zero_or_unpriced_order_is_refused() {
    let mut l = limits(Some(1_000.0), 1e9);
    for px in [Some(0.0), Some(-1.0), Some(f64::NAN), Some(f64::INFINITY), None] {
        let mut o =
            published_order("c1", "binance", None, "BTCUSDT", vike_model::OrderStatus::Accepted);
        o.price = px;
        let reason = l
            .vet(&qty_only(1e9), &[], &[o])
            .unwrap_or_else(|| panic!("standing price {px:?} must be refused, not sized at 0"));
        assert!(reason.contains("no price"), "{reason}");
    }
    let mut stop =
        published_order("c1", "binance", None, "BTCUSDT", vike_model::OrderStatus::Accepted);
    stop.order_type = vike_model::OrderType::Stop;
    stop.price = None;
    stop.trigger_price = Some(50.0);
    assert!(l.vet(&qty_only(1e9), &[], &[stop]).is_some(), "a stop with no limit price: refused");
}

/// THE STALE SNAPSHOT: the price was raised a moment ago through this surface, the coalesced snapshot
/// still shows the old (low) one. The qty-only modify that follows is sized at the HIGHER price.
#[test]
fn a_qty_only_modify_is_sized_at_the_higher_of_the_snapshot_and_a_recent_modify_price() {
    let mut l = limits(Some(1_000.0), 1e9);
    let stale =
        [published_order("c1", "binance", None, "BTCUSDT", vike_model::OrderStatus::Accepted)];
    // snapshot price 1.0: 500 * 1.0 <= 1000 passes before any modify
    assert!(l.vet(&qty_only(500.0), &[], &stale).is_none());
    // price modified to 10.0 (in the cap: 10 * 10 = 100); the snapshot still says 1.0
    let reprice =
        WireCommand::Modify { client_order_id: "c1".into(), new_qty: None, new_price: Some(10.0) };
    assert!(l.vet(&reprice, &[], &stale).is_none());
    assert!(
        l.vet(&qty_only(500.0), &[], &stale).is_some(),
        "500 * 10.0 > 1000 on the real price, though the stale snapshot says 1.0 -> refused"
    );
    assert!(
        l.preview_vet(&qty_only(500.0), &[], &stale).is_some(),
        "the read-only preview agrees with vet"
    );
    assert!(l.vet(&qty_only(50.0), &[], &stale).is_none(), "50 * 10.0 <= 1000 still passes");
}
