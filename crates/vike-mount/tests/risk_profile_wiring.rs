//! RunProfile wiring — closing the LIVE gap (last of the backtest → paper → live chain).
//!
//! Proves `vike_mount::make_engine`'s new `risk_profile` parameter actually arms the operator's
//! `[risk]` budget on the mounted `RiskGate` — not just that the struct field gets populated (a
//! prior session's regression class), but that the gate's `check` verdict actually changes. Every
//! scenario below runs over an EMPTY credentials map (`vars: HashMap::new()`), so every venue
//! mounts its PAPER fallback — no network, no creds, CI-safe — while still exercising the REAL
//! `make_engine` merge site (the one directly BEFORE the `im_requirement` rescue — that ordering
//! is load-bearing, see `make_engine`'s own comment for why merging after would silently disarm
//! the buying-power gate).
//!
//! Property 4 below pins the SEMANTICS `ProfileRisk::apply_to` enforces (a venue-owned field is
//! rejected under `GridSource::VenueFetched`), but it does NOT call `make_engine` and therefore
//! does NOT pin the enum `make_engine`'s own merge site hardcodes — that pin lives at
//! `crates/vike-mount/src/lib.rs`'s internal `merge_operator_budget` tests instead (an
//! integration test in this file cannot reach that `pub(crate)` function at all), exercised over
//! a `RiskLimits::from_properties`-shaped populated base so flipping the hardcoded
//! `GridSource::VenueFetched` there to `NoGridFetched` fails first.
//!
//! Four properties, matching the CLAUDE.md-cited wiring plan:
//! 1. No profile ⇒ `limits` matches the pre-wiring mount PLUS the Task-6 armed defaults (see
//!    below — this property's expectation changed when Task 6 landed; it is no longer
//!    byte-identical to pre-Task-6, and that is the intended, reviewed behavior change).
//! 2. A profile arms the operator fields AND the gate actually denies a violating order.
//! 3. The same armed gate still passes a normal-sized order (over-arming would be the real risk).
//! 4. A profile illegally setting a venue-owned instrument field under `GridSource::VenueFetched`
//!    is rejected by the exact merge `make_engine` performs (pinned directly against
//!    `ProfileRisk::apply_to`, since a `VenueFetched` base needs a REAL venue fetch to occur inside
//!    `make_engine` itself, which is not reachable from a network-free CI test).
//!
//! ## Task 6 addendum (armed-risk-defaults, 2026-07-28)
//!
//! `make_engine` now ALSO arms `max_orders_per_window`/`window_ms` to a conservative default at
//! every mount (live or paper) — it armed `max_leverage` too until issue #822 removed that inert
//! duplicate; leverage is enforced solely through the `im_requirement` rescue, which is also what
//! an operator's `[risk] max_leverage` converts into — and returns `Result<EngineAndRecon,
//! MountError>` — a LIVE mount (never a paper one, like every scenario in this file) refuses to
//! start without an operator-supplied `max_notional_per_order`/`max_total_exposure`. The
//! properties that need a genuinely LIVE venue (the headline "does the armed default actually
//! deny" proof, and the refuse-to-start proof) are pinned directly against the pure
//! `arm_universal_defaults`/`require_live_risk_budget` functions in `vike-mount/src/lib.rs`'s own
//! `#[cfg(test)]` module instead — the same network-free-CI constraint that already applies to
//! `merge_operator_budget`'s `GridSource::VenueFetched` pin (see property 4's own doc below). This
//! file adds the two properties THAT ARE reachable end-to-end over a paper mount: the armed
//! defaults surviving a real `make_engine` round-trip, and a profile overriding them (tuning, not
//! fighting).

use std::collections::{HashMap, HashSet};

use vike_exec::{GridSource, ProfileRisk, RiskContext, RiskLimits};
use vike_model::OrderRequest;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";

fn empty_vars() -> HashMap<String, String> {
    HashMap::new()
}

/// A machine policy whose ARMING CEILING permits [`VENUE`].
///
/// ⚠ Load-bearing rather than boilerplate, and for the same reason in every scenario here: a
/// `paper` ceiling (the default, and what `None` means) returns from a SEPARATE paper assembly at
/// the top of `make_engine_with_legs`, so a test that left it at the default would be pinning that
/// assembly rather than the merge site this file exists for. The credentials map stays EMPTY — the
/// mount is still paper, by the older absent-credentials gate.
fn armed() -> vike_mount::MountPolicy {
    vike_mount::MountPolicy {
        venues: vike_config::VenuePolicy::default().declare(VENUE, vike_config::VenueMode::Demo),
        ..vike_mount::MountPolicy::default()
    }
}

fn market_order(side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: "t".into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side,
        qty,
        order_type: "market".into(),
        ..Default::default()
    }
}

/// Property 1 — MERGE-SAFETY, updated for Task 6: with no `risk_profile` threaded in (`None`, the
/// value every call site passed before this parameter existed), `make_engine`'s `limits` carry
/// `im_requirement` (the pre-existing `Some(1.0)` rescue) PLUS the Task-6 armed defaults
/// (`max_orders_per_window`/`window_ms`) — every OTHER operator-budget field
/// (`max_notional_per_order`/`max_total_exposure`) stays `None`, i.e. those `if let Some(cap) = …`
/// checks never run; a live venue with both `None` would refuse to start, but `binance` here has
/// no creds so it stays paper (unaffected by that rule — see
/// `paper_mount_starts_with_no_account_dependent_budget_at_all` in `lib.rs` for the direct pin).
/// Absent creds keep `binance` on the paper fallback, so the venue grid itself is also untouched
/// (permissive `None`s), proving `risk_profile: None` doesn't SNEAK the account-dependent budget
/// in through some other path either.
#[test]
fn no_profile_arms_the_universal_defaults_and_leaves_the_account_dependent_caps_unset() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let (engine, _recon) = vike_mount::make_engine(
        VENUE,
        SYMBOL,
        &empty_vars(),
        &tx,
        &mut live,
        false, // reconcile off — these tests are about the [risk] profile wiring, not recon
        None,
        None,
        None, // no operator RunProfile threaded in
        // …and a policy that ONLY arms the ceiling for this venue: every other field is its
        // no-file default, so the limits below are the pre-Phase-6c mount, byte-for-byte.
        Some(&armed()),
    )
    .expect("a paper mount must never refuse to start");
    // `max_leverage` stays None (issue #822 removed #817's inert arming) — the 1× floor is armed
    // ONCE, by the `im_requirement` rescue, which is the field the gate actually reads.
    let expected = RiskLimits {
        im_requirement: Some(1.0),
        max_orders_per_window: Some(100),
        ..RiskLimits::new()
    };
    assert_eq!(
        engine.gate.limits, expected,
        "no risk_profile must leave `limits` exactly as the (Task-6-updated) mount builds it"
    );
    assert_eq!(
        engine.gate.limits.max_notional_per_order, None,
        "account-dependent cap stays unset"
    );
    assert_eq!(engine.gate.limits.max_total_exposure, None, "account-dependent cap stays unset");
}

/// Task-6 addendum — the compose-not-fight property, end to end through a REAL `make_engine`
/// call: a profile that sets `max_orders_per_window`/`max_leverage` OVERRIDES the armed defaults
/// (tuning, not fighting) rather than the two silently coexisting or conflicting.
#[test]
fn profile_overrides_the_armed_universal_defaults() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let profile = ProfileRisk {
        max_orders_per_window: Some(3),
        window_ms: 250,
        max_leverage: Some(5.0),
        ..ProfileRisk::default()
    };
    let (engine, _recon) = vike_mount::make_engine(
        VENUE,
        SYMBOL,
        &empty_vars(),
        &tx,
        &mut live,
        false, // reconcile off — these tests are about the [risk] profile wiring, not recon
        None,
        None,
        Some(&profile),
        // A policy that arms the ARMING CEILING for this venue and sets nothing else, so the
        // `[risk]` profile is still the only budget authority here — see `armed`.
        Some(&armed()),
    )
    .expect("a paper mount must never refuse to start");
    assert_eq!(
        engine.gate.limits.max_orders_per_window,
        Some(3),
        "the profile's explicit value must win over the armed default (100)"
    );
    assert_eq!(engine.gate.limits.window_ms, 250);
    assert_eq!(engine.gate.limits.max_leverage, Some(5.0), "the declared cap is recorded");
    // …and, issue #822, it is now ENFORCED: 5x converts to a 20% initial-margin requirement,
    // overriding the mount's conservative 1× (`im 1.0`) rescue. Before this change the profile's
    // `max_leverage = 5.0` populated a field nothing read and the mount silently stayed at 1×.
    assert_eq!(
        engine.gate.limits.im_requirement,
        Some(0.2),
        "`max_leverage = 5.0` must arm the buying-power check at 1/5, not leave the 1x rescue"
    );
}

/// …and the OTHER half of "override, don't fight": a profile that sets NEITHER
/// `max_orders_per_window` nor `max_leverage` must still leave the armed defaults standing (the
/// exact `im_requirement`/`window_ms` divergent-default hazard this task's own brief warns about
/// — a profile present for an unrelated reason must not silently null a field it never mentions).
#[test]
fn profile_that_omits_the_defaultable_fields_does_not_null_the_armed_defaults() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let profile = ProfileRisk {
        max_notional_per_order: Some(1_000.0), // sets something else entirely
        ..ProfileRisk::default()
    };
    let (engine, _recon) = vike_mount::make_engine(
        VENUE,
        SYMBOL,
        &empty_vars(),
        &tx,
        &mut live,
        false, // reconcile off — these tests are about the [risk] profile wiring, not recon
        None,
        None,
        Some(&profile),
        // A policy that arms the ARMING CEILING for this venue and sets nothing else, so the
        // `[risk]` profile is still the only budget authority here — see `armed`.
        Some(&armed()),
    )
    .expect("a paper mount must never refuse to start");
    assert_eq!(
        engine.gate.limits.max_orders_per_window,
        Some(100),
        "a profile silent on this field must not null the armed default"
    );
    // A profile silent on leverage leaves `max_leverage` unset and the mount's own 1× rescue
    // standing — the exact divergent-default hazard this test exists for, now on the field that
    // actually enforces (issue #822).
    assert_eq!(engine.gate.limits.max_leverage, None);
    assert_eq!(engine.gate.limits.im_requirement, Some(1.0));
}

/// Property 2 — THE POINT OF THE WHOLE WIRING: a profile setting `max_notional_per_order` must not
/// just populate the field, it must make the mounted `RiskGate` actually DENY a violating order.
/// Asserting on the gate's verdict (not just the struct) is deliberate — the failure class this
/// wiring exists to close is "the field is populated but the check never ran" (see CLAUDE.md's
/// `order-intent-write-contract`/RunProfile-wiring notes on that exact class).
#[test]
fn profile_arms_the_limits_and_the_gate_denies_a_violating_order() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let profile = ProfileRisk {
        max_notional_per_order: Some(100.0),
        max_total_exposure: Some(500.0),
        ..ProfileRisk::default()
    };
    let (mut engine, _recon) = vike_mount::make_engine(
        VENUE,
        SYMBOL,
        &empty_vars(),
        &tx,
        &mut live,
        false, // reconcile off — these tests are about the [risk] profile wiring, not recon
        None,
        None,
        Some(&profile),
        // A policy that arms the ARMING CEILING for this venue and sets nothing else, so the
        // `[risk]` profile is still the only budget authority here — see `armed`.
        Some(&armed()),
    )
    .expect("a paper mount must never refuse to start");
    assert_eq!(engine.gate.limits.max_notional_per_order, Some(100.0));
    assert_eq!(engine.gate.limits.max_total_exposure, Some(500.0));
    // im_requirement's pre-existing rescue must still hold — this wiring must not clobber it.
    assert_eq!(engine.gate.limits.im_requirement, Some(1.0));

    // A flat account, market order for 2 units at a $100 mark ⇒ notional $200 > the $100 cap.
    // `equity: 1_000.0` so this denial is unambiguously the notional cap, not a margin shortfall.
    let ctx = RiskContext { mark_price: 100.0, equity: 1_000.0, ..RiskContext::default() };
    let verdict = engine.gate.check(&market_order(1, 2.0), &ctx);
    assert!(!verdict.ok, "an order over the profile's max_notional_per_order must be DENIED");
    assert_eq!(verdict.reason, "over-max-notional");
}

/// Property 3 — the real risk of this change is OVER-arming, not under-arming: the SAME gate from
/// property 2, presented with a NORMAL-sized order well inside both caps, must still pass.
#[test]
fn profile_armed_gate_still_passes_a_normal_sized_order() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let profile = ProfileRisk {
        max_notional_per_order: Some(100.0),
        max_total_exposure: Some(500.0),
        ..ProfileRisk::default()
    };
    let (mut engine, _recon) = vike_mount::make_engine(
        VENUE,
        SYMBOL,
        &empty_vars(),
        &tx,
        &mut live,
        false, // reconcile off — these tests are about the [risk] profile wiring, not recon
        None,
        None,
        Some(&profile),
        // A policy that arms the ARMING CEILING for this venue and sets nothing else, so the
        // `[risk]` profile is still the only budget authority here — see `armed`.
        Some(&armed()),
    )
    .expect("a paper mount must never refuse to start");

    // 0.5 units at a $100 mark ⇒ notional $50, well under both the $100 per-order cap and the
    // $500 exposure cap. `equity: 1_000.0` covers the `im_requirement` buying-power check the
    // im_requirement rescue (Some(1.0), i.e. 1x/no-leverage) now also arms — a flat-zero-equity
    // context would deny even a tiny order on "insufficient-margin" and mask what this test is
    // actually pinning (the notional/exposure caps).
    let ctx = RiskContext { mark_price: 100.0, equity: 1_000.0, ..RiskContext::default() };
    let verdict = engine.gate.check(&market_order(1, 0.5), &ctx);
    assert!(verdict.ok, "a normal-sized order inside the armed caps must PASS: {verdict:?}");
}

/// Property 4 — `ProfileRisk::apply_to` itself rejects a profile that ALSO sets a venue-owned
/// field (`tick_size`) under `GridSource::VenueFetched`. This exercises `apply_to` directly, NOT
/// `make_engine`'s merge site — a network-free integration test in this file has no way to build
/// a REAL `GridSource::VenueFetched` base (that needs an actual venue fetch), and no way to reach
/// `merge_operator_budget` at all (it is `pub(crate)` to `vike-mount`, not exported). So this test
/// pins `apply_to`'s own contract only — it does NOT pin the enum `make_engine` hardcodes at its
/// merge site; a future edit that flipped that call site to `GridSource::NoGridFetched` would
/// leave this test green. The test that actually pins THAT (`merge_operator_budget`'s own enum
/// choice, over a `RiskLimits::from_properties`-shaped populated base) lives next to the function
/// in `crates/vike-mount/src/lib.rs`'s test module, where `pub(crate)` visibility allows it.
#[test]
fn profile_setting_a_venue_owned_field_is_rejected_under_venue_fetched() {
    let venue_grid = RiskLimits { tick_size: Some(0.01), lot_size: Some(1.0), ..RiskLimits::new() };
    let profile = ProfileRisk { tick_size: Some(999.0), ..ProfileRisk::default() };
    let err = profile
        .apply_to(venue_grid, GridSource::VenueFetched)
        .expect_err("a profile setting a venue-owned field under VenueFetched must be rejected");
    let msg = format!("{err}");
    assert!(msg.contains("risk.tick_size"), "the error must name the offending key: {msg}");
}

/// The #817 "refusal happens POST-connect" residual, CLOSED: the missing-budget refusal now
/// fires PRE-CONNECT, on live INTENT (this venue's credential shape present in the vars map)
/// rather than on an established session. This test runs entirely OFFLINE with FAKE binance demo
/// creds and no `[risk]` profile: the refusal must return before the binance arm's blocking
/// properties pre-fetch or exec-actor spawn ever executes. If the check ever regresses to
/// post-connect, this test starts performing real network I/O with garbage keys — failing (or
/// hanging) in a network-free environment instead of failing an assertion.
#[test]
fn live_intent_without_budget_refuses_before_any_connect() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let mut vars = HashMap::new();
    vars.insert("BINANCE_DEMO_API_KEY".to_string(), "fake-test-key".to_string());
    vars.insert("BINANCE_DEMO_API_SECRET".to_string(), "fake-test-secret".to_string());
    // (a `match`, not `expect_err` — the Ok side's engine is deliberately not `Debug`)
    let err = match vike_mount::make_engine(
        "binance",
        SYMBOL,
        &vars,
        &tx,
        &mut live,
        false,
        None,
        None,
        None, // no risk profile at all — both account-dependent caps missing
        // ⚠ The ARMING CEILING must permit binance, or the mount returns the paper engine ABOVE
        // this refusal and the test passes against an `Ok` — the ceiling is now the FIRST gate, and
        // an unarmed venue has no live intent to refuse over. It supplies no budget of its own.
        Some(&armed()),
    ) {
        Err(e) => e,
        Ok(_) => panic!("live intent with no risk budget must refuse to start, pre-connect"),
    };
    let msg = err.to_string();
    assert!(msg.contains("binance"), "the refusal must name the venue: {msg}");
    assert!(msg.contains("max_notional_per_order"), "must name the 1st missing cap: {msg}");
    assert!(msg.contains("max_total_exposure"), "must name the 2nd missing cap: {msg}");
    assert!(
        live.is_empty(),
        "the refusal must precede the venue arm — nothing may be recorded live"
    );
}

/// …and the SAME live intent WITH a full budget passes the pre-connect gate (the refusal is
/// budget-shaped, not a blanket veto on live intent). Pinned against the same preview the
/// `make_engine` site builds (`to_risk_limits`), NOT via a full live mount — a real credentialed
/// binance mount would perform network I/O this suite must not.
#[test]
fn live_intent_with_full_budget_passes_the_preconnect_preview() {
    let profile = ProfileRisk {
        max_notional_per_order: Some(100.0),
        max_total_exposure: Some(500.0),
        ..ProfileRisk::default()
    };
    let preview = profile.to_risk_limits();
    assert_eq!(preview.max_notional_per_order, Some(100.0));
    assert_eq!(preview.max_total_exposure, Some(500.0));
}

// ---- the ACCOUNT-AGGREGATE ceiling's wiring: `policy.toml` -> `RiskLimits` ------------------
//
// A `Policy` field is worth nothing until something READS it — the defect
// `crates/vike-config/tests/policy_is_consumed.rs` exists for, and the one
// `Policy::max_total_exposure` shipped in. That gate proves a TEXT is present in a file; these two
// tests prove the operator's NUMBER arrives on the mounted gate, over both assemblies
// `make_engine_for_account` can return from.

/// A machine policy that arms [`VENUE`] AND writes an account ceiling.
fn armed_with_account_ceiling(cap: f64) -> vike_mount::MountPolicy {
    vike_mount::MountPolicy { max_account_exposure: Some(cap), ..armed() }
}

/// **The operator's number reaches the gate, and the gate acts on it** — through the ordinary
/// (non-paper-ceiling) assembly, the one every live venue shares.
///
/// Two arms, because either alone would be weak evidence:
///
///   1. the field ARRIVES on `engine.gate.limits` — which a `MountPolicy` projection that dropped it
///      would fail, and which is the whole of what a `policy_is_consumed` needle can see;
///   2. the gate DENIES on it, under the account reason, with an order the per-symbol lane is not
///      even armed for (`max_total_exposure` is `None` here) — so nothing but this ceiling can be
///      what refused, and "carrying a value" cannot be mistaken for "enforcing it".
///
/// ⚠ The credentials map is EMPTY, so binance mounts its paper client by the older
/// absent-credentials gate while still travelling the FULL assembly — the same network-free
/// arrangement every scenario in this file uses.
#[test]
fn a_policy_account_ceiling_arrives_on_the_mounted_gate_and_denies() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let (mut engine, _recon) = vike_mount::make_engine(
        VENUE,
        SYMBOL,
        &empty_vars(),
        &tx,
        &mut live,
        false,
        None,
        None,
        None, // no run profile: this ceiling does not come from one, and must not need one
        Some(&armed_with_account_ceiling(1_000.0)),
    )
    .expect("a paper mount must never refuse to start");

    assert_eq!(
        engine.gate.limits.max_account_exposure,
        Some(1_000.0),
        "the policy file's account ceiling must reach the mounted RiskGate — a projection that \
         dropped it would leave the operator with a validated key and no cap"
    );
    assert_eq!(
        engine.gate.limits.max_total_exposure, None,
        "…and the per-symbol lane is NOT armed here, which is what makes the denial below \
         attributable to the account ceiling alone"
    );

    // 20 @ 100 = 2 000 of projected notional on a flat book, against a 1 000 ceiling.
    //
    // ⚠ `equity` is load-bearing rather than scenery, and is the same value every other scenario in
    // this file supplies: `make_engine` arms `im_requirement` at 1× unconditionally (the rescue
    // beside the merge), so a zero-equity context refuses the ADMIT arm below with
    // `insufficient-margin` — a different lane, and one that would have made the "a small order
    // still passes" claim untestable. 1 000 of equity funds the 100-notional order at 1×.
    let ctx = RiskContext { mark_price: 100.0, equity: 1_000.0, ..RiskContext::default() };
    let v = engine.gate.check(&market_order(1, 20.0), &ctx);
    assert!(!v.ok, "the armed account ceiling must refuse: {v:?}");
    assert!(
        v.reason.starts_with("over-account-exposure"),
        "…under its OWN reason, naming the ceiling the operator has to edit: {}",
        v.reason
    );

    // …and a small order still passes, so this is a ceiling rather than a kill switch.
    let ok = engine.gate.check(&market_order(1, 1.0), &ctx);
    assert!(ok.ok, "an order inside the ceiling must still be admitted: {ok:?}");
}

/// **The PAPER assembly carries it too** — the `paper` arming ceiling returns from a separate
/// assembly at the top of `make_engine_for_account`, and a rehearsal whose gate is more permissive
/// than the live mount it rehearses teaches the operator that a configuration is safe when it is
/// not. (The same standing rule `vike_backtest::SimBroker`'s `gate_order` states for the backtest,
/// where the permissive-side defect was actually measured.)
#[test]
fn the_paper_assembly_carries_the_account_ceiling_too() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let policy = vike_mount::MountPolicy {
        max_account_exposure: Some(1_000.0),
        ..vike_mount::MountPolicy::default() // every venue at `paper` — the no-file default
    };
    let (engine, _recon) = vike_mount::make_engine(
        VENUE,
        SYMBOL,
        &empty_vars(),
        &tx,
        &mut live,
        false,
        None,
        None,
        None,
        Some(&policy),
    )
    .expect("a paper mount must never refuse to start");
    assert_eq!(
        engine.gate.limits.max_account_exposure,
        Some(1_000.0),
        "the paper-ceiling assembly must not be the permissive side of the mount it rehearses"
    );
}
