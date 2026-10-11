//! The operator risk budget a mount folds: the `[risk]` merge, the armed defaults, the live refusal.

use crate::MountError;
#[cfg(doc)]
use crate::would_mount_live;

/// The commented, copy-pasteable live profile shipped in-tree, named by the diagnostic.
/// `crates/vike-mount/tests/risk_budget_diagnostic.rs` reads this path back out of the rendered
/// message and parses the file as a `vike_core::RunProfile`, so it cannot dangle.
pub(crate) const EXAMPLE_PROFILE_PATH: &str = "docs/ops/run-profile-live.toml";

/// Example value + one-line meaning per account-dependent cap, so the diagnostic's `[risk]` table
/// is copy-pasteable. Keyed by the names [`require_live_risk_budget`] pushes into `missing`;
/// `every_missing_key_has_an_example` fails until a new cap gains a row.
/// ⚠ `max_total_exposure` is PER SYMBOL despite its name: `vike_exec::RiskGate::check_inner`'s
/// `over-max-exposure` lane prices
/// `(ctx.position_size + side*qty).abs() * ctx.mark_price * ctx.multiplier` for ONE symbol in ONE
/// engine. Text saying "across venues" makes an operator size it for the whole book and get a cap
/// N times looser than written.
/// ⚠ The cross-symbol cap is `policy.max_account_exposure`
/// (`vike_model::RiskLimits::max_account_exposure`, a pre-folded scalar on the `Copy`
/// `vike_exec::RiskContext`, computed on the cold path so nothing joined the measured
/// `p99 < 10µs` hop). It is deliberately NOT part of the live refusal, so this table stays the two
/// caps a live mount demands.
pub(crate) const BUDGET_EXAMPLES: &[(&str, &str, &str)] = &[
    ("max_notional_per_order", "5000.0", "cap on ONE order's notional"),
    ("max_total_exposure", "25000.0", "cap on ONE symbol's projected open notional"),
];

/// The merge [`make_engine`] applies to fold an operator's `[risk]` budget onto a venue's resolved
/// `limits`, a function of its own so it is unit-testable without the REAL venue fetch a
/// `make_engine` call needs (network-free CI cannot reach that path).
///
/// `GridSource::VenueFetched` is hardcoded: every `limits` here came from a venue fetch (or its
/// venue-owned fallback), so the instrument-grid fields never come from the profile
/// (`ProfileRisk::apply_to`'s doc). `profile: None` leaves `limits` untouched (byte-identical).
/// `profile: Some` merges via [`vike_model::ProfileRisk::apply_to`]; if the profile illegally sets
/// a venue-owned field, it falls back to [`vike_model::ProfileRisk::apply_operator_budget_only`],
/// which still arms every operator-owned field and drops ONLY the venue fields — never the whole
/// budget behind one log line (a silent-degrade class a review caught). Callers holding a full
/// `RunProfile` should still fail loud first via
/// `vike_core::RunProfile::risk_for_live_venue_mount`; this is defense-in-depth. That path stays a
/// plain code span, never a `[…]` link: this crate sits BELOW `vike-core`, so rustdoc cannot
/// resolve it.
pub(crate) fn merge_operator_budget(
    venue: &str,
    limits: vike_model::RiskLimits,
    profile: Option<&vike_model::ProfileRisk>,
) -> vike_model::RiskLimits {
    let Some(profile) = profile else { return limits };
    match profile.apply_to(limits.clone(), vike_model::GridSource::VenueFetched) {
        Ok(merged) => merged,
        Err(e) => {
            tracing::error!(
                venue,
                error = %e,
                "risk profile rejected at merge (a venue-owned instrument field, or an \
                 out-of-range value — see `error`) — dropping ONLY the offending field(s); the \
                 operator's risk budget (max_notional_per_order/max_total_exposure/\
                 max_orders_per_window/window_ms/max_leverage + its derived im_requirement/\
                 required_free_bp_pct) still arms on this venue"
            );
            profile.apply_operator_budget_only(limits)
        }
    }
}

// ---- the operator budget splits in two
// (`docs/superpowers/plans/2026-07-28-runprofile-wiring.md`, Task 6): universally defaultable ->
// `arm_universal_defaults` (armed at mount, always); account-dependent ->
// `require_live_risk_budget` (a live mount refuses to start). `RiskGate::check` guards each cap
// with `if let Some(cap)`, so `None` means the check NEVER RUNS: without this split a live run
// with no profile enforced the venue's lot grid and nothing else. ----

/// The always-armed order throttle: the reference risk-engine default
/// (100 orders per second), reconciled against every per-venue
/// [`vike_bridge_core::ratelimit::RateGate`] so the two throttles never fight. `window_ms` stays
/// `RiskLimits::new()`'s 1000, so this is 100 orders/s — ABOVE every real venue budget: tightest is
/// Deribit Tier4 at **5/s** ([`vike_model::venues::venue_rate_limits::DERIBIT`]`.orders`, wired by
/// `crates/bridges/deribit/src/ratelimit.rs`); OKX 50/2s ≈ 25/s, Bybit 18/s, Binance spot 90/10s ≈
/// 9/s and perp 270/10s = 27/s, Aster spot 90/min = 1.5/s and perp 1080/min = 18/s.
///
/// That direction is load-bearing: `RiskGate`'s throttle DENIES outright (drops the order) while a
/// venue `RateGate` only BLOCKS until a slot frees. A cap below or near a venue budget would drop
/// sustained flow the transport would have queued; above all of them it is inert in normal
/// operation and trips only a runaway loop.
pub(crate) const ARMED_MAX_ORDERS_PER_WINDOW: usize = 100;

/// Arm the operator-budget fields with a UNIVERSALLY-safe default (today: the order throttle).
/// Called from [`crate::make_engine`] immediately before the `im_requirement` rescue and AFTER
/// [`merge_operator_budget`], never before: the merge takes `max_orders_per_window`/`window_ms`
/// UNCONDITIONALLY from any profile, so rescuing first would let a profile's serde-default `None`
/// silently win the moment ANY profile is merged.
///
/// Not rescued here: `max_leverage` (it converts to `im_requirement` at the config edge, so that
/// rescue is the single place "no leverage unless asked", i.e. 1×, is armed — issue #822 removed
/// an inert duplicate), and `required_free_bp_pct` (a plain `f64` already `0.0` on every path, the
/// inert no-haircut value; [`merge_operator_budget`] threads a profile's own value through).
pub(crate) fn arm_universal_defaults(mut limits: vike_model::RiskLimits) -> vike_model::RiskLimits {
    limits.max_orders_per_window =
        limits.max_orders_per_window.or(Some(ARMED_MAX_ORDERS_PER_WINDOW));
    limits
}

/// The live refusal: an ACCOUNT-DEPENDENT cap has no universal safe value (too large never trips,
/// too small rejects every real order), so a LIVE mount REFUSES TO START unless BOTH
/// `max_notional_per_order` and `max_total_exposure` reached `limits`. `Err` names EVERY missing
/// key, so one fix cycle closes the gate.
///
/// Checked ONLY for a venue the operator intends live, from two sites in [`crate::make_engine`]'s
/// mount: PRE-CONNECT (the contract fold, when the bridge's `resolve` — the probe behind
/// [`would_mount_live`] — answers armed at the account's tier; the primary, before any venue
/// session exists) and post-merge (gated on `live_venues` holding the mount's route key; the
/// backstop for a live arm without a probe row). Paper and backtest mounts reach neither and may
/// run unbounded.
///
/// `profile_supplied` (`MountEnv::risk_profile.is_some()`) changes only the DIAGNOSTIC of
/// [`MountError::MissingRiskBudget`], never the verdict: `limits` cannot tell "no profile" from "a
/// profile omitting these caps". Public for the roster tests in
/// `crates/vike-tradehub/tests/mount_roster.rs` (docs/decisions/0096).
pub fn require_live_risk_budget(
    venue: &str,
    limits: &vike_model::RiskLimits,
    profile_supplied: bool,
) -> Result<(), MountError> {
    let mut missing = Vec::new();
    if limits.max_notional_per_order.is_none() {
        missing.push("max_notional_per_order");
    }
    if limits.max_total_exposure.is_none() {
        missing.push("max_total_exposure");
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(MountError::MissingRiskBudget { venue: venue.to_string(), missing, profile_supplied })
    }
}

#[path = "budget_tests.rs"]
#[cfg(test)]
mod budget_tests;
