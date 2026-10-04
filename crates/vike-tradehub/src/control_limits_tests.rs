use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;

use super::control::{ControlLimits, ControlLimitsConfig, DEFAULT_CONTROL_RATE};
use vike_tradehub_client::auth::{self, NodeKeys};
use vike_tradehub_client::proto::{
    NODE_PROTO_VERSION, Request, Response, Scope, read_frame, write_frame,
};
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest};

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

// Every `vet`/`preview_vet` below that is NOT about the contract multiplier passes `&[], &[]` —
// "this surface knows no engine roster and no order" — which sizes at multiplier 1.0, the
// arithmetic every test in this file used before the ceiling counted one. The multiplier tests
// further down are the ones that publish a roster (and, for a `Modify`, the order it names).

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

fn bracket(side: i32, qty: f64, entry: Option<f64>, sl: f64, tp: f64) -> WireCommand {
    WireCommand::Bracket(vike_tradehub_client::wire::WireBracketSpec {
        venue: "hyperliquid".into(),
        symbol: "BTC".into(),
        side,
        qty,
        entry_price: entry,
        stop_loss: sl,
        take_profit: tp,
    })
}

/// ⚠ **A bracket is three orders of one size, and every priced leg is capped as a `Submit` at that
/// price would be**, so the node is never weaker for a bracket than for its legs sent one by one.
/// A MARKET entry is unpriced, like a market `Submit`, but its exits are not.
#[test]
fn every_priced_leg_of_a_bracket_is_capped_like_a_submit() {
    let mut l = limits(Some(1_000.0), 1e9);
    assert!(
        l.vet(&bracket(1, 10.0, Some(50.0), 45.0, 60.0), &[], &[]).is_none(),
        "every leg <= 600"
    );
    assert!(
        l.vet(&bracket(1, 10.0, Some(150.0), 145.0, 160.0), &[], &[]).is_some(),
        "every leg breaches"
    );
    assert!(
        l.vet(&bracket(1, 10.0, Some(150.0), 45.0, 60.0), &[], &[]).is_some(),
        "the limit entry alone"
    );
    assert!(
        l.vet(&bracket(1, 10.0, None, 45.0, 120.0), &[], &[]).is_some(),
        "market entry, TP leg: 1200"
    );
    assert!(
        l.vet(&bracket(-1, 10.0, Some(50.0), 120.0, 40.0), &[], &[]).is_some(),
        "a short's SL: 1200"
    );
    assert!(
        l.vet(&bracket(1, -10.0, Some(150.0), 145.0, 160.0), &[], &[]).is_some(),
        "|qty| is used"
    );
    assert!(
        l.vet(&bracket(1, 10.0, Some(-150.0), 45.0, 60.0), &[], &[]).is_some(),
        "|price| is used"
    );
}

/// The refusal NAMES the leg that breached (the largest, when several do), so an operator knows
/// which price to move — and a NaN leg, which `n > max` never selects, cannot hide a finite leg
/// that breaches.
#[test]
fn a_bracket_cap_refusal_names_the_breaching_leg() {
    let mut l = limits(Some(1_000.0), 1e9);
    for (cmd, leg) in [
        (bracket(1, 10.0, Some(150.0), 45.0, 60.0), "entry"),
        (bracket(1, 10.0, None, 45.0, 120.0), "take-profit"),
        (bracket(-1, 10.0, Some(50.0), 120.0, 40.0), "stop-loss"),
        (bracket(1, 10.0, Some(150.0), 145.0, 160.0), "take-profit"),
        (bracket(1, 10.0, Some(f64::NAN), 45.0, 120.0), "take-profit"),
    ] {
        let label = format!("{cmd:?}");
        let msg = l.vet(&cmd, &[], &[]).unwrap_or_else(|| panic!("{label} must be refused"));
        assert!(msg.contains(&format!("the bracket's {leg} leg")), "{label}: {msg}");
        assert!(msg.contains("max_notional_per_order"), "{label}: names the ceiling: {msg}");
    }
}

/// The dry-run answers the same cap and spends no token: two previews on a ONE-token bucket, and
/// the real `vet` after them still finds its token.
#[test]
fn a_bracket_preview_applies_the_same_cap_without_a_token() {
    let mut l = limits(Some(1_000.0), 1.0);
    assert!(l.preview_vet(&bracket(1, 10.0, Some(150.0), 145.0, 160.0), &[], &[]).is_some());
    assert!(l.preview_vet(&bracket(1, 10.0, Some(50.0), 45.0, 60.0), &[], &[]).is_none());
    assert!(
        l.vet(&bracket(1, 10.0, Some(50.0), 45.0, 60.0), &[], &[]).is_none(),
        "the bucket's one token is still there, so neither preview spent it"
    );
}

/// A bracket pays the rate token, like every command: a flood of brackets is still a flood.
#[test]
fn a_bracket_pays_the_rate_token() {
    let mut l = limits(None, 1.0);
    assert!(
        l.vet(&bracket(1, 1.0, None, 1.0, 2.0), &[], &[]).is_none(),
        "the first takes the one token"
    );
    assert!(
        l.vet(&bracket(1, 1.0, None, 1.0, 2.0), &[], &[]).is_some(),
        "the second is rate limited"
    );
}

// ---------------------------------------------------------------------------------------------
// THE CONTRACT MULTIPLIER — the ceiling sized `|qty| · |price|` and nothing else
// ---------------------------------------------------------------------------------------------
//
// The desktop's order preview and the core's `RiskGate` size with `vike_model::order_notional`,
// which multiplies by the instrument's contract multiplier; this edge — the only enforcing site of
// `policy.max_notional_per_order` — did not, so for an instrument whose multiplier is not 1 the
// ceiling was weaker here than in the GUI that sent the order. The rows below publish a roster
// (the node's engine blocks) and watch the SAME ceiling move with it.

/// One published engine block: its routing key, the symbol it was mounted on and — when the
/// multiplier is not 1.0 — a grid row for that symbol, the SPARSE shape `vike_mount`'s
/// `multiplier_grid` builds (a 1.0 multiplier collapses to an empty grid, not to a no-op row).
fn block(route_key: &str, symbol: &str, multiplier: f64) -> vike_core::VenueBlock {
    let mut grid = indexmap::IndexMap::new();
    if multiplier != 1.0 {
        grid.insert(symbol.to_string(), multiplier);
    }
    vike_core::VenueBlock {
        venue: route_key.split('#').next().unwrap_or(route_key).to_string(),
        route_key: route_key.to_string(),
        symbol: symbol.to_string(),
        multipliers: Arc::new(grid),
        ..Default::default()
    }
}

/// [`submit`]'s limit order naming an ACCOUNT of the venue — the three wire states
/// (`None`, `DEFAULT`, a label) are what picks WHICH engine's multiplier applies.
fn submit_on(account: Option<&str>, qty: f64, price: f64) -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: "c1".into(),
        venue: "hyperliquid".into(),
        symbol: "BTC".into(),
        side: 1,
        qty,
        order_type: "limit".into(),
        price: Some(price),
        trigger_price: None,
        reduce_only: false,
        account: account.map(str::to_string),
    })
}

/// **THE TABLE.** One ceiling (1000), many rosters: a `Submit` is refused exactly when
/// `|qty| · |price| · multiplier` is above it, the multiplier being the one of the engine the
/// command ADDRESSES for the symbol it NAMES — and the read-only dry-run says the same words the
/// executing gate does, row for row.
///
/// The `at the cap` rows sit on products that are exact in binary floating point (`10 · 100`,
/// `1 · 10 · 100`), because the cap is `>` and an equality row is only a witness where the
/// arithmetic does not round.
///
/// ⚠ **THE FLOOR.** A multiplier below 1, and one that is zero, negative or not a finite number,
/// is counted as 1.0 — the node's ceiling is never weaker than `|qty| · |price|` and a bad value
/// in venue data cannot loosen it. The rows that used to pin "a multiplier below 1 sizes the order
/// SMALLER" (the `0.01` ones) now pin the opposite, and the `0`, negative, NaN and infinite rows
/// each sit where the UNfloored arithmetic gives the other verdict (`0` sizes everything at
/// nothing; `|-0.5|` halves; NaN is never above the cap; infinity is always above it).
///
/// ⚠ KILL PROOF 1: remove the multiplier from `notional_reason` (size `(qty, price, 1.0)` whatever
/// the roster says) and every `multiplier 100` row that expects a refusal, plus the `ALT` row,
/// flips to accepted. KILL PROOF 2: remove the floor (`floored_multiplier` returns `m`) and the
/// `0.01`, `0`, `-0.5` and NaN rows that expect a refusal flip to accepted, the infinite row that
/// expects an accept flips to refused.
#[test]
fn the_ceiling_counts_the_contract_multiplier_of_the_addressed_engine() {
    // (label, published roster, the account the wire names, qty, price, refused?)
    type Row = (&'static str, Vec<vike_core::VenueBlock>, Option<&'static str>, f64, f64, bool);
    let hl = |m: f64| vec![block("hyperliquid", "BTC", m)];
    let two_books =
        || vec![block("hyperliquid", "BTC", 1.0), block("hyperliquid#ALT", "BTC", 100.0)];
    // A block whose GRID lists nothing and whose engine-wide DEFAULT is `m` — the other place a
    // published multiplier comes from (`VenueBlock::multiplier_of`'s fallback).
    let default_only = |m: f64| {
        vec![vike_core::VenueBlock {
            venue: "hyperliquid".into(),
            route_key: "hyperliquid".into(),
            symbol: "BTC".into(),
            multiplier_default: m,
            ..Default::default()
        }]
    };
    let rows: Vec<Row> = vec![
        // No roster, or a multiplier of 1: today's arithmetic, bit for bit.
        ("no roster: exactly at the cap", vec![], None, 10.0, 100.0, false),
        ("no roster: just over", vec![], None, 10.0, 100.01, true),
        ("multiplier 1: exactly at the cap", hl(1.0), None, 10.0, 100.0, false),
        ("multiplier 1: just over", hl(1.0), None, 10.0, 100.01, true),
        // A multiplier above 1 makes the node refuse SOONER.
        ("multiplier 100: exactly at the cap (1 x 10 x 100)", hl(100.0), None, 1.0, 10.0, false),
        ("multiplier 100: just over (1 x 10.01 x 100)", hl(100.0), None, 1.0, 10.01, true),
        (
            "multiplier 100: 1 x 20 is far under the cap unmultiplied; 2000 is over",
            hl(100.0),
            None,
            1.0,
            20.0,
            true,
        ),
        ("multiplier 100: a negative qty is a magnitude", hl(100.0), None, -1.0, 20.0, true),
        ("multiplier 100: a negative price is a magnitude", hl(100.0), None, 1.0, -20.0, true),
        // A multiplier below 1 is FLOORED to 1: the node is stricter than the desktop and the core,
        // which size a sub-1 contract smaller, and never weaker than the unmultiplied arithmetic.
        (
            "multiplier 0.01 floors to 1: 1000 x 99 = 99000 is over (0.01 would say 990, under)",
            hl(0.01),
            None,
            1000.0,
            99.0,
            true,
        ),
        (
            "multiplier 0.01 floors to 1: 1000 x 50 = 50000 is over (0.01 would say 500, under)",
            hl(0.01),
            None,
            1000.0,
            50.0,
            true,
        ),
        (
            "multiplier 0.01 floors to 1: exactly at the cap (10 x 100)",
            hl(0.01),
            None,
            10.0,
            100.0,
            false,
        ),
        (
            "multiplier 0.01 floors to 1: just over (10 x 100.01)",
            hl(0.01),
            None,
            10.0,
            100.01,
            true,
        ),
        (
            "a sub-1 engine-wide DEFAULT floors too (no grid row at all)",
            default_only(0.5),
            None,
            10.0,
            100.01,
            true,
        ),
        // A multiplier that is not a finite positive number is bad venue data: read as 1.
        (
            "multiplier 0 floors to 1: 10 x 100.01 is over (0 would size it at nothing)",
            hl(0.0),
            None,
            10.0,
            100.01,
            true,
        ),
        ("multiplier 0 floors to 1: exactly at the cap", hl(0.0), None, 10.0, 100.0, false),
        (
            "multiplier -0.5 floors to 1: 10 x 100.01 is over (|-0.5| would say 500, under)",
            hl(-0.5),
            None,
            10.0,
            100.01,
            true,
        ),
        (
            "multiplier NaN floors to 1: 10 x 100.01 is over (a NaN notional is never above the cap)",
            hl(f64::NAN),
            None,
            10.0,
            100.01,
            true,
        ),
        ("multiplier NaN floors to 1: exactly at the cap", hl(f64::NAN), None, 10.0, 100.0, false),
        (
            "multiplier +inf floors to 1: exactly at the cap (inf would always be over)",
            hl(f64::INFINITY),
            None,
            10.0,
            100.0,
            false,
        ),
        (
            "multiplier -inf floors to 1: exactly at the cap",
            hl(f64::NEG_INFINITY),
            None,
            10.0,
            100.0,
            false,
        ),
        // WHICH engine's multiplier: the one the command addresses, never a neighbour's.
        (
            "account ALT named: ALT's engine, multiplier 100",
            two_books(),
            Some("ALT"),
            1.0,
            20.0,
            true,
        ),
        ("no account named: the bare-venue engine, not ALT's", two_books(), None, 1.0, 20.0, false),
        (
            "DEFAULT named: the bare-venue engine, not ALT's",
            two_books(),
            Some("DEFAULT"),
            1.0,
            20.0,
            false,
        ),
        ("an account no engine carries sizes at 1", hl(100.0), Some("NOSUCH"), 1.0, 20.0, false),
        (
            "another venue's engine lends no multiplier",
            vec![block("deribit", "BTC", 100.0)],
            None,
            1.0,
            20.0,
            false,
        ),
        (
            "a symbol the engine's grid does not list sizes at 1",
            vec![block("hyperliquid", "ETH", 100.0)],
            None,
            1.0,
            20.0,
            false,
        ),
    ];
    for (label, roster, account, qty, price, refused) in rows {
        let cmd = submit_on(account, qty, price);
        let mut l = limits(Some(1_000.0), 1e9);
        let vetted = l.vet(&cmd, &roster, &[]);
        assert_eq!(vetted.is_some(), refused, "{label}: `vet` answered {vetted:?}");
        let previewed = l.preview_vet(&cmd, &roster, &[]);
        assert_eq!(
            previewed, vetted,
            "{label}: the dry-run must give the executing gate's verdict, word for word"
        );
    }
}

/// The refusal says WHY the notional is not `qty x price` only when there is a multiplier — so every
/// sentence that existed before the node counted one reads byte-identically (tests, runbooks and
/// the Telegram channel key off those words). A multiplier the floor raised to 1 is no multiplier
/// at all as far as the sentence goes.
#[test]
fn the_refusal_names_the_multiplier_only_when_there_is_one() {
    let l = limits(Some(1_000.0), 1e9);
    let plain =
        "order notional 2000.00 exceeds the node's policy ceiling max_notional_per_order 1000.00";
    assert_eq!(l.preview_vet(&submit(5.0, Some(400.0)), &[], &[]).as_deref(), Some(plain));
    assert_eq!(
        l.preview_vet(&submit(5.0, Some(400.0)), &[block("hyperliquid", "BTC", 1.0)], &[])
            .as_deref(),
        Some(plain),
        "a multiplier of exactly 1 adds nothing to the sentence"
    );
    assert_eq!(
        l.preview_vet(&submit(5.0, Some(400.0)), &[block("hyperliquid", "BTC", 0.01)], &[])
            .as_deref(),
        Some(plain),
        "a multiplier the floor raised to 1 adds nothing either, and is not named as 0.01"
    );
    assert_eq!(
        l.preview_vet(&submit(1.0, Some(20.0)), &[block("hyperliquid", "BTC", 100.0)], &[])
            .as_deref(),
        Some(
            "order notional 2000.00 exceeds the node's policy ceiling max_notional_per_order \
             1000.00 (the notional counts this instrument's contract multiplier 100)"
        )
    );
}

/// A bracket is three orders of one size on ONE engine, so every priced leg is sized with that
/// engine's multiplier — and the refusal still names the leg and now says why the number is what
/// it is. A sub-1 engine is floored to 1 on every leg, like a `Submit`.
///
/// ⚠ KILL PROOF: remove the floor and the sub-1 bracket below is sized at 0.01 on every leg
/// (1200 becomes 12) and passes.
#[test]
fn a_bracket_is_sized_with_its_engines_multiplier_on_every_priced_leg() {
    let l = limits(Some(1_000.0), 1e9);
    let roster = [block("hyperliquid", "BTC", 100.0)];
    // Legs 1 x {5, 4, 6} x 100 = 500, 400, 600 — all under.
    assert!(l.preview_vet(&bracket(1, 1.0, Some(5.0), 4.0, 6.0), &roster, &[]).is_none());
    // The take-profit alone is over: 1 x 12 x 100 = 1200.
    let msg = l
        .preview_vet(&bracket(1, 1.0, Some(5.0), 4.0, 12.0), &roster, &[])
        .expect("a take-profit leg of 1200 is over a 1000 ceiling");
    assert!(
        msg.contains("the bracket's take-profit leg")
            && msg.contains("1200.00")
            && msg.contains("contract multiplier 100"),
        "{msg}"
    );
    // Unmultiplied — no roster — the very same bracket passes: the multiplier is the difference.
    assert!(l.preview_vet(&bracket(1, 1.0, Some(5.0), 4.0, 12.0), &[], &[]).is_none());
    // A sub-1 engine is FLOORED to 1, not sized smaller: 100 x {5, 4, 12} x 1 = 500, 400, 1200 —
    // the take-profit is over (0.01 would say 12), and the sentence names no multiplier, because
    // the one counted is 1.
    let sub_one = [block("hyperliquid", "BTC", 0.01)];
    let msg = l
        .preview_vet(&bracket(1, 100.0, Some(5.0), 4.0, 12.0), &sub_one, &[])
        .expect("a sub-1 engine is floored to 1, so the 1200 take-profit leg is over the ceiling");
    assert!(
        msg.contains("the bracket's take-profit leg")
            && msg.contains("1200.00")
            && !msg.contains("contract multiplier"),
        "{msg}"
    );
}

// ---------------------------------------------------------------------------------------------
// A `Modify` IS SIZED WITH THE MULTIPLIER OF THE ORDER IT NAMES
// ---------------------------------------------------------------------------------------------
//
// A `Modify` names a client order id and nothing else, so the edge reads which instrument that id
// rests on off the published OPEN order. This used to be a pinned residual — "a modify is sized
// unmultiplied because it names no instrument" — which let a `Submit` that passed small be
// modified UP on a multiplier engine and checked at 1.0.

/// One published order, as `CoreSnapshot::build` publishes it: the holding engine's venue, its
/// account (`None` is the default account) and the symbol it rests on, with the status that says
/// whether it is still open. Terms are beside the point — only the instrument is read.
fn published_order(
    coid: &str,
    venue: &str,
    account: Option<&str>,
    symbol: &str,
    status: vike_exec::OrderStatus,
) -> vike_core::OrderView {
    vike_core::OrderView {
        client_order_id: coid.to_string(),
        venue: venue.to_string(),
        account: account.map(|a| {
            vike_model::accounts::account_keys::AccountLabel::parse(a).expect("a valid label")
        }),
        symbol: symbol.to_string(),
        side: 1,
        qty: 1.0,
        order_type: "limit".to_string(),
        price: Some(1.0),
        trigger_price: None,
        status,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
    }
}

fn modify_to(coid: &str, qty: f64, price: f64) -> WireCommand {
    WireCommand::Modify { client_order_id: coid.into(), new_qty: Some(qty), new_price: Some(price) }
}

/// **THE TABLE for a `Modify`.** One ceiling (1000), many snapshots: a priced modify is refused
/// exactly when `|new qty| · |new price| · multiplier` is above it, the multiplier being the one of
/// the engine that holds the OPEN order the coid names, for that order's symbol — and an order the
/// published snapshot does not hold sizes at 1.0, which is what every row here did before the edge
/// could resolve one. The read-only dry-run says the same words the executing gate does.
///
/// ⚠ KILL PROOF 1: put the `1.0` back in the `Modify` arm of `notional_reason` (size
/// `(qty, price, 1.0)` whatever the snapshot says) and every row that expects a refusal flips to
/// accepted — `resolved, multiplier 100`, `the ALT order`, `PARTIALLY_FILLED`, `SUBMITTED`. KILL
/// PROOF 2: match the coid on terminal orders too (drop the `is_terminal` test in
/// `order_multiplier`) and the `FILLED` row flips to refused. KILL PROOF 3: look the block up by
/// bare venue instead of by the order's route key and the `ALT` rows swap their verdicts.
#[test]
fn a_modify_is_sized_with_the_multiplier_of_the_order_it_names() {
    use vike_exec::OrderStatus;
    // (label, published roster, published orders, the coid the modify names, qty, price, refused?)
    type Row = (
        &'static str,
        Vec<vike_core::VenueBlock>,
        Vec<vike_core::OrderView>,
        &'static str,
        f64,
        f64,
        bool,
    );
    let hl = |m: f64| vec![block("hyperliquid", "BTC", m)];
    let two_books =
        || vec![block("hyperliquid", "BTC", 1.0), block("hyperliquid#ALT", "BTC", 100.0)];
    let on_hl = |coid: &str, status: OrderStatus| {
        vec![published_order(coid, "hyperliquid", None, "BTC", status)]
    };
    let rows: Vec<Row> = vec![
        // The case this change exists for: 1 x 20 is 20 unmultiplied and 2000 on the engine the
        // order rests on.
        (
            "resolved, multiplier 100: 1 x 20 x 100 = 2000 is over (1.0 would say 20, under)",
            hl(100.0),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            1.0,
            20.0,
            true,
        ),
        (
            "resolved, multiplier 100: exactly at the cap (1 x 10 x 100)",
            hl(100.0),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            1.0,
            10.0,
            false,
        ),
        (
            "resolved, multiplier 100: a negative qty is a magnitude",
            hl(100.0),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            -1.0,
            20.0,
            true,
        ),
        // A multiplier of 1 is today's arithmetic, bit for bit.
        (
            "resolved, multiplier 1: exactly at the cap (10 x 100)",
            hl(1.0),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            10.0,
            100.0,
            false,
        ),
        (
            "resolved, multiplier 1: just over (10 x 100.01)",
            hl(1.0),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            10.0,
            100.01,
            true,
        ),
        // Every OPEN status resolves — a modify arriving before the order is accepted is sized with
        // the instrument it will rest on.
        (
            "resolved while PARTIALLY_FILLED",
            hl(100.0),
            on_hl("c1", OrderStatus::PartiallyFilled),
            "c1",
            1.0,
            20.0,
            true,
        ),
        (
            "resolved while SUBMITTED, before the venue accepted it",
            hl(100.0),
            on_hl("c1", OrderStatus::Submitted),
            "c1",
            1.0,
            20.0,
            true,
        ),
        // What the snapshot does not hold sizes at 1.0 — exactly today's behaviour.
        (
            "no published order: 1.0, the coid may be too new for the last publish",
            hl(100.0),
            vec![],
            "c1",
            1.0,
            20.0,
            false,
        ),
        (
            "a different coid is published: this one is unknown, 1.0",
            hl(100.0),
            on_hl("other", OrderStatus::Accepted),
            "c1",
            1.0,
            20.0,
            false,
        ),
        (
            "a TERMINAL order (FILLED) of that coid lends nothing: 1.0",
            hl(100.0),
            on_hl("c1", OrderStatus::Filled),
            "c1",
            1.0,
            20.0,
            false,
        ),
        (
            "a TERMINAL order (CANCELED) of that coid lends nothing: 1.0",
            hl(100.0),
            on_hl("c1", OrderStatus::Canceled),
            "c1",
            1.0,
            20.0,
            false,
        ),
        (
            "a TERMINAL order (REJECTED) lends nothing: 10 x 100 stays exactly at the cap",
            hl(100.0),
            on_hl("c1", OrderStatus::Rejected),
            "c1",
            10.0,
            100.0,
            false,
        ),
        // WHICH engine's multiplier: the one that holds the order, never a neighbour's.
        (
            "another venue's block lends nothing: the order rests on hyperliquid, deribit is 100",
            vec![block("deribit", "BTC", 100.0), block("hyperliquid", "BTC", 1.0)],
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            1.0,
            20.0,
            false,
        ),
        (
            "the order rests on deribit: deribit's multiplier 100 applies, hyperliquid's 1 does not",
            vec![block("hyperliquid", "BTC", 1.0), block("deribit", "BTC", 100.0)],
            vec![published_order("c1", "deribit", None, "BTC", OrderStatus::Accepted)],
            "c1",
            1.0,
            20.0,
            true,
        ),
        (
            "the order rests on a venue with NO block: 1.0",
            hl(100.0),
            vec![published_order("c1", "deribit", None, "BTC", OrderStatus::Accepted)],
            "c1",
            1.0,
            20.0,
            false,
        ),
        (
            "the ALT order: ALT's engine, multiplier 100",
            two_books(),
            vec![published_order("c1", "hyperliquid", Some("ALT"), "BTC", OrderStatus::Accepted)],
            "c1",
            1.0,
            20.0,
            true,
        ),
        (
            "the DEFAULT-account order: the bare-venue engine, not ALT's",
            two_books(),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            1.0,
            20.0,
            false,
        ),
        (
            "an account no block carries sizes at 1",
            hl(100.0),
            vec![published_order(
                "c1",
                "hyperliquid",
                Some("NOSUCH"),
                "BTC",
                OrderStatus::Accepted,
            )],
            "c1",
            1.0,
            20.0,
            false,
        ),
        (
            "a symbol the engine's grid does not list sizes at 1",
            vec![block("hyperliquid", "ETH", 100.0)],
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            1.0,
            20.0,
            false,
        ),
        // The floor applies to a resolved Modify exactly as to a Submit.
        (
            "resolved, multiplier 0.01 floors to 1: 10 x 100.01 is over (0.01 would say 10, under)",
            hl(0.01),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            10.0,
            100.01,
            true,
        ),
        (
            "resolved, multiplier 0.01 floors to 1: exactly at the cap",
            hl(0.01),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            10.0,
            100.0,
            false,
        ),
        (
            "resolved, multiplier NaN floors to 1: 10 x 100.01 is over",
            hl(f64::NAN),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            10.0,
            100.01,
            true,
        ),
        (
            "resolved, multiplier 0 floors to 1: 10 x 100.01 is over",
            hl(0.0),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            10.0,
            100.01,
            true,
        ),
    ];
    for (label, roster, orders, coid, qty, price, refused) in rows {
        let cmd = modify_to(coid, qty, price);
        let mut l = limits(Some(1_000.0), 1e9);
        let vetted = l.vet(&cmd, &roster, &orders);
        assert_eq!(vetted.is_some(), refused, "{label}: `vet` answered {vetted:?}");
        let previewed = l.preview_vet(&cmd, &roster, &orders);
        assert_eq!(
            previewed, vetted,
            "{label}: the dry-run must give the executing gate's verdict, word for word"
        );
    }
}

/// A resolved `Modify`'s refusal names the multiplier in the same words a `Submit`'s does, and a
/// `Modify` that resolves to 1 reads byte-identically to the sentence before the edge could resolve
/// one.
#[test]
fn a_modify_refusal_names_the_multiplier_only_when_there_is_one() {
    use vike_exec::OrderStatus;
    let l = limits(Some(1_000.0), 1e9);
    let open =
        |coid: &str| vec![published_order(coid, "hyperliquid", None, "BTC", OrderStatus::Accepted)];
    assert_eq!(
        l.preview_vet(
            &modify_to("c1", 1.0, 20.0),
            &[block("hyperliquid", "BTC", 100.0)],
            &open("c1")
        )
        .as_deref(),
        Some(
            "order notional 2000.00 exceeds the node's policy ceiling max_notional_per_order \
             1000.00 (the notional counts this instrument's contract multiplier 100)"
        )
    );
    let plain =
        "order notional 2000.00 exceeds the node's policy ceiling max_notional_per_order 1000.00";
    assert_eq!(
        l.preview_vet(
            &modify_to("c1", 5.0, 400.0),
            &[block("hyperliquid", "BTC", 1.0)],
            &open("c1")
        )
        .as_deref(),
        Some(plain),
        "a multiplier of exactly 1 adds nothing to the sentence"
    );
    assert_eq!(
        l.preview_vet(&modify_to("c1", 5.0, 400.0), &[], &[]).as_deref(),
        Some(plain),
        "an unknown order reads exactly as a Modify always did"
    );
}

/// **Resolving the order can only make the node stricter or equal.** Over a grid of sizes and every
/// multiplier shape the roster can carry (above 1, exactly 1, below 1, zero, negative, NaN,
/// infinite), a `Modify` the node refuses with NO order known is still refused once the order IS
/// known — the floor is what makes that true for the values that are not above 1.
///
/// ⚠ KILL PROOF: remove the floor and the sub-1, zero, negative and NaN multipliers refuse LESS
/// than the unknown-order baseline, which is the failure this loop exists to catch.
#[test]
fn resolving_the_order_never_makes_a_modify_weaker_than_leaving_it_unknown() {
    use vike_exec::OrderStatus;
    let l = limits(Some(1_000.0), 1e9);
    let orders = [published_order("c1", "hyperliquid", None, "BTC", OrderStatus::Accepted)];
    let multipliers =
        [100.0, 2.0, 1.0, 0.5, 0.01, 0.0, -0.5, -100.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY];
    let sizes = [(1.0, 20.0), (10.0, 100.0), (10.0, 100.01), (1000.0, 99.0), (-5.0, 400.0)];
    for m in multipliers {
        let roster = [block("hyperliquid", "BTC", m)];
        for (qty, price) in sizes {
            let cmd = modify_to("c1", qty, price);
            let baseline = l.preview_vet(&cmd, &roster, &[]);
            let resolved = l.preview_vet(&cmd, &roster, &orders);
            // `baseline` sizes at 1.0 (nothing known); `resolved` may only refuse MORE.
            assert!(
                baseline.is_none() || resolved.is_some(),
                "multiplier {m}, {qty} x {price}: refused while the order was unknown ({baseline:?}) \
                 but passes once it is known"
            );
        }
    }
}

/// **A non-finite size or price is REFUSED, not compared.** `NaN > max` is false, so a NaN notional
/// used to read as "within the ceiling"; `inf · 0` is NaN too, so the check is on the FACTORS. The
/// wire cannot carry one in (JSON has no NaN) and the Telegram parser refuses one, so this is the
/// guard for a caller that builds a `WireCommand` in process.
///
/// ⚠ KILL PROOF: delete the `is_finite` check in `notional_reason` and the NaN rows flip to
/// accepted (`NaN > max` is false) and the infinite ones to the old `order notional inf exceeds`
/// sentence, which this test's wording assertion also fails.
#[test]
fn a_non_finite_size_or_price_is_refused_not_compared() {
    let l = limits(Some(1_000.0), 1e9);
    let bad = [
        (f64::NAN, 50.0),
        (10.0, f64::NAN),
        (f64::NAN, f64::NAN),
        (f64::INFINITY, 0.0),
        (f64::INFINITY, 50.0),
        (10.0, f64::INFINITY),
        (f64::NEG_INFINITY, 50.0),
    ];
    for (qty, price) in bad {
        let modify = WireCommand::Modify {
            client_order_id: "c1".into(),
            new_qty: Some(qty),
            new_price: Some(price),
        };
        for cmd in [submit(qty, Some(price)), modify] {
            let label = format!("{cmd:?}");
            let msg = l
                .preview_vet(&cmd, &[], &[])
                .unwrap_or_else(|| panic!("{label} has no finite notional and must be refused"));
            assert!(
                msg.contains("no finite notional") && msg.contains("max_notional_per_order"),
                "{label}: the refusal must say the ceiling could not be evaluated: {msg}"
            );
        }
    }
    // Scoped to a ceiling that exists: with none there is nothing to evaluate and the core
    // `RiskGate` is the floor, exactly as `no_size_cap_without_a_policy_ceiling` says.
    let uncapped = limits(None, 1e9);
    assert!(uncapped.preview_vet(&submit(f64::NAN, Some(50.0)), &[], &[]).is_none());
    // …and a finite order is untouched.
    assert!(l.preview_vet(&submit(10.0, Some(50.0)), &[], &[]).is_none());
}

/// **The roster reaches the ceiling THROUGH `accept_command`**, which is the production call the
/// unit rows above bypass: the gate is handed the `blocks` the surface published, not an empty
/// slice. The refusal happens at step 1, before anything is lowered, so the paper core exists only
/// to supply a real `CommandSink`.
///
/// ⚠ KILL PROOF: have `accept_command` call `limits.vet(&cmd, &[], orders)` and this fails — the
/// order is accepted at multiplier 1.0 and the `expect_err` never sees a refusal.
#[test]
fn accept_command_hands_the_published_roster_to_the_ceiling() {
    let mount = vike_mount::build_paper_maker_core(&vike_mount::MakerMountConfig::outcome_token(
        "polymarket",
        "NOTIONAL_MULTIPLIER_TOKEN",
        Some(3_000_000_000),
    ));
    let sink = mount.handle.command_sink();
    let mut l = limits(Some(1_000.0), 1e9);
    let roster = [block("hyperliquid", "BTC", 100.0)];
    let cmd = submit(1.0, Some(20.0));

    let err = super::control::accept_command(
        cmd.clone(),
        None,
        &mut l,
        &sink,
        None,
        &[],
        &[],
        &roster,
        &[],
        None,
        None,
    )
    .expect_err("1 x 20 x 100 = 2000 is over a 1000 ceiling");
    let msg = err.message();
    assert!(
        msg.contains("2000.00") && msg.contains("contract multiplier 100"),
        "the refusal must come from the roster's multiplier: {msg}"
    );
    assert!(!err.is_fatal(), "a ceiling refusal never closes the surface");
    // The same command with no roster is NOT refused at the edge — the roster is the difference.
    assert!(l.preview_vet(&cmd, &[], &[]).is_none());
}

/// **The order a `Modify` names reaches the ceiling THROUGH `accept_command`** — the production call
/// the table above bypasses. `submit small, modify up` on a multiplier-100 engine: the modify's
/// 1 x 20 is 20 unmultiplied and 2000 on the engine the published order rests on. The refusal
/// happens at step 1, before anything is lowered, so the paper core exists only to supply a real
/// `CommandSink` — and a refused modify must not reach it.
///
/// ⚠ KILL PROOF: have `accept_command` hand `limits.vet` an empty `orders` (or have the `Modify`
/// arm size at 1.0) and the modify is accepted at multiplier 1.0 and the `expect_err` never sees a
/// refusal. The twin assertions pin the unknown-order case: the SAME frame with no published order
/// is not refused at the edge.
#[test]
fn accept_command_hands_the_order_a_modify_names_to_the_ceiling() {
    let mount = vike_mount::build_paper_maker_core(&vike_mount::MakerMountConfig::outcome_token(
        "polymarket",
        "NOTIONAL_MODIFY_MULTIPLIER_TOKEN",
        Some(3_000_000_000),
    ));
    let sink = mount.handle.command_sink();
    let mut l = limits(Some(1_000.0), 1e9);
    let roster = [block("hyperliquid", "BTC", 100.0)];
    let orders =
        [published_order("c1", "hyperliquid", None, "BTC", vike_exec::OrderStatus::Accepted)];
    let cmd = modify_to("c1", 1.0, 20.0);

    let err = super::control::accept_command(
        cmd.clone(),
        None,
        &mut l,
        &sink,
        None,
        &[],
        &[],
        &roster,
        &orders,
        None,
        None,
    )
    .expect_err("1 x 20 x 100 = 2000 is over a 1000 ceiling");
    let msg = err.message();
    assert!(
        msg.contains("2000.00") && msg.contains("contract multiplier 100"),
        "the refusal must come from the named order's multiplier: {msg}"
    );
    assert!(!err.is_fatal(), "a ceiling refusal never closes the surface");
    // The same frame against a snapshot that does not hold the order is sized as it always was.
    assert!(l.preview_vet(&cmd, &roster, &[]).is_none(), "unknown order: 1.0");
    assert!(l.preview_vet(&cmd, &roster, &orders).is_some(), "known order: 100");
}

/// Complete the real handshake as the WRITE scope over a real socket and return the authed stream.
/// The shape of `server_link_liveness_tests.rs`'s `authed_stream`, narrowed to the one scope this
/// test needs.
fn control_stream(addr: SocketAddr, key: &[u8]) -> TcpStream {
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce,
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(key, &nonce, NODE_PROTO_VERSION, Scope::Write);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Write, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("authok") {
        Response::AuthOk { scope: Scope::Write } => {}
        other => panic!("expected AuthOk(Write), got {other:?}"),
    }
    stream
}

/// **The TCP dry-run sizes with the multiplier the node PUBLISHED.** The real server, a real
/// handshake and a test-owned snapshot cell whose one engine block carries a multiplier-100 grid:
/// a `Preview` of an order that is under the ceiling unmultiplied (1 x 20) comes back refused with
/// the multiplier named, and one that is under it either way (1 x 5 x 100 = 500) comes back
/// accepted — so the verdict moved because of the roster and not because the preview refuses
/// everything. No core and no `CommandSink`: a preview lowers nothing.
///
/// ⚠ KILL PROOF: have the `Request::Preview` arm call `limits.preview_vet(&wire_cmd, &[], &orders)`
/// and the first reply flips to accepted.
#[test]
fn a_tcp_preview_sizes_a_submit_with_the_engine_multiplier_the_node_published() {
    const CONTROL_KEY: &[u8] = b"control-key-for-the-notional-multiplier-preview";
    let mut snap = vike_core::CoreSnapshot::empty("hyperliquid", "BTC");
    snap.portfolio.venues.push(block("hyperliquid", "BTC", 100.0));
    let cell = Arc::new(arc_swap::ArcSwap::from_pointee(snap));
    let publisher = crate::publish::spawn(cell, None);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    std::thread::spawn(move || {
        let _ = super::serve(
            listener,
            publisher,
            NodeKeys::new(
                b"observe-key-for-the-notional-multiplier-preview".to_vec(),
                CONTROL_KEY.to_vec(),
            ),
            None,
            ControlLimitsConfig { max_notional: Some(1_000.0), rate_per_sec: 1e9 },
            None,
            None,
            None,
        );
    });
    let mut ctl = control_stream(addr, CONTROL_KEY);
    let mut preview = |cmd: WireCommand| -> (bool, Option<String>) {
        write_frame(&mut ctl, &Request::Preview(cmd)).expect("preview");
        match read_frame::<_, Response>(&mut ctl).expect("verdict") {
            Response::Preview { accepted, reason } => (accepted, reason),
            other => panic!("expected a Preview verdict, got {other:?}"),
        }
    };

    let (accepted, reason) = preview(submit(1.0, Some(20.0)));
    assert!(!accepted, "1 x 20 x 100 = 2000 is over the 1000 ceiling");
    let reason = reason.expect("a refusal carries its reason");
    assert!(
        reason.contains("2000.00") && reason.contains("contract multiplier 100"),
        "the preview must size with the published multiplier: {reason}"
    );

    let (accepted, reason) = preview(submit(1.0, Some(5.0)));
    assert!(accepted, "1 x 5 x 100 = 500 is under the ceiling: {reason:?}");
}

/// **The TCP dry-run sizes a `Modify` with the multiplier of the order the node PUBLISHED.** The
/// real server, a real handshake and a test-owned snapshot cell holding a multiplier-100 engine
/// block, one OPEN order `c1` resting on it and one terminal order `done` — the path
/// `Request::Preview` takes through `PublisherHandle::open_orders_named_by`, which the unit table
/// bypasses. `submit small, modify up`: the modify's 1 x 20 is 20 unmultiplied and 2000 on the
/// engine `c1` rests on, so the preview comes back refused with the multiplier named; a modify that
/// is under the ceiling either way (1 x 5 x 100 = 500) comes back accepted; and a coid the snapshot
/// does not hold, or holds only as a terminal order, is sized at 1.0 as it always was.
///
/// Its body calls no signature this change alters — it drives only the TCP wire and the published
/// cell — so the same test dropped into `main`'s tree compiles and fails for the stated reason: the
/// first verdict is `accepted` there, because that edge sizes every `Modify` at 1.0.
///
/// ⚠ KILL PROOF: have the `Request::Preview` arm hand `preview_vet` an empty `orders` and the first
/// verdict flips to accepted. (The `done` verdict is held by TWO filters, the accessor's and the
/// limiter's own, so it flips only when both `is_terminal` tests go; the accessor's is pinned alone
/// by `open_orders_named_by_answers_the_open_order_a_modify_names_and_nothing_else`.)
#[test]
fn a_tcp_preview_sizes_a_modify_with_the_multiplier_of_the_order_the_node_published() {
    const CONTROL_KEY: &[u8] = b"control-key-for-the-notional-modify-preview";
    let mut snap = vike_core::CoreSnapshot::empty("hyperliquid", "BTC");
    snap.portfolio.venues.push(block("hyperliquid", "BTC", 100.0));
    snap.orders.push(published_order(
        "c1",
        "hyperliquid",
        None,
        "BTC",
        vike_exec::OrderStatus::Accepted,
    ));
    snap.orders.push(published_order(
        "done",
        "hyperliquid",
        None,
        "BTC",
        vike_exec::OrderStatus::Filled,
    ));
    let cell = Arc::new(arc_swap::ArcSwap::from_pointee(snap));
    let publisher = crate::publish::spawn(cell, None);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    std::thread::spawn(move || {
        let _ = super::serve(
            listener,
            publisher,
            NodeKeys::new(
                b"observe-key-for-the-notional-modify-preview".to_vec(),
                CONTROL_KEY.to_vec(),
            ),
            None,
            ControlLimitsConfig { max_notional: Some(1_000.0), rate_per_sec: 1e9 },
            None,
            None,
            None,
        );
    });
    let mut ctl = control_stream(addr, CONTROL_KEY);
    let mut preview = |cmd: WireCommand| -> (bool, Option<String>) {
        write_frame(&mut ctl, &Request::Preview(cmd)).expect("preview");
        match read_frame::<_, Response>(&mut ctl).expect("verdict") {
            Response::Preview { accepted, reason } => (accepted, reason),
            other => panic!("expected a Preview verdict, got {other:?}"),
        }
    };

    let (accepted, reason) = preview(modify_to("c1", 1.0, 20.0));
    assert!(!accepted, "1 x 20 x 100 = 2000 is over the 1000 ceiling for the order c1 rests on");
    let reason = reason.expect("a refusal carries its reason");
    assert!(
        reason.contains("2000.00") && reason.contains("contract multiplier 100"),
        "the preview must size the modify with the published order's multiplier: {reason}"
    );

    let (accepted, reason) = preview(modify_to("c1", 1.0, 5.0));
    assert!(accepted, "1 x 5 x 100 = 500 is under the ceiling: {reason:?}");

    let (accepted, reason) = preview(modify_to("never-published", 1.0, 20.0));
    assert!(accepted, "an order the snapshot does not hold is sized at 1.0: {reason:?}");

    let (accepted, reason) = preview(modify_to("done", 1.0, 20.0));
    assert!(accepted, "a TERMINAL order lends no multiplier: {reason:?}");
}
