//! Halt-admit reporting and the paper-fallback path: what happens when a venue can't (or won't)
//! go live — degrade to `PaperExecutionClient`, and report why, louder when the operator's
//! credentials would have armed an account its `account` row holds at paper.

use std::collections::HashMap;

use vike_model::accounts::account_keys::AccountLabel;

use crate::{
    EngineAndRecon, MountPolicy, account_max_exposure, arm_universal_defaults, margin_mode_grid,
    merge_operator_budget, multiplier_grid, resolve_fee_schedule, symbol_grid,
    would_mount_live_under,
};

/// **Whether `venue`'s stored credentials would arm it at EITHER tier** (`demo` or `live`) — the
/// question [`report_capped_to_paper`] asks. NOT `crate::would_mount_live`, which probes only
/// `Live`: a `live` tier with only DEMO keys is PAPER (decision 0095; the no-downgrade rule), so
/// that probe would silently drop demo-only venues from a report whose job is to name every venue
/// with SOME credentials.
fn would_mount_under_some_tier(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
) -> bool {
    would_mount_live_under(registry, venue, vars, vike_config::VenueMode::Live)
        || would_mount_live_under(registry, venue, vars, vike_config::VenueMode::Demo)
}

/// Resolve the halt-admit mode in force at `venue`, and SAY SO when the venue cannot honour it.
///
/// ⚠ **The report is the feature.** An operator who set `policy.halt_admit verify` believes the
/// kill switch verifies positions; on every roster venue but cTrader no position book exists at
/// that boundary, and a SILENT degrade would leave that belief standing until an incident. So the
/// line lands at MOUNT, with venue and reason — as `vike_bridge_core::halt::halt_path_arming_error`
/// reports an unarmable sentinel at mount, not at the first order.
///
/// ⚠ **Only this half is answerable up front**: "no position book in this adapter" is a fact about
/// CODE. Whether `verify` ARMED is a fact about the MOUNT — [`crate::make_engine_for_account`]
/// calls this before credentials and cTrader's handshake, either of which can land on paper — and
/// is [`report_halt_admit_armed`]'s job.
///
/// Silent under the DEFAULT (`HaltAdmit::Admit` degrades nowhere): a box with no
/// `policy.halt_admit` row gains no log line — byte-identical in the trace file too.
pub(crate) fn report_halt_admit(
    venue: &str,
    policy: Option<&MountPolicy>,
) -> vike_model::HaltAdmit {
    let requested = policy.map(|p| p.halt_admit).unwrap_or_default();
    let (effective, degraded) = vike_model::effective_halt_admit(requested, venue);
    if let Some(why) = degraded {
        tracing::warn!(
            venue,
            requested = %requested.as_str(),
            effective = %effective.as_str(),
            reason = %why,
            "`policy.halt_admit` is `verify`, but this venue cannot honour it → DEGRADING \
             to admit (the HALT sentinel still stops opening orders here; it just cannot check a \
             reduce_only claim against a position). See docs/ops/kill-switches.md"
        );
    }
    effective
}

/// Say whether `verify` actually ARMED, from the one place that knows: after the venue resolved to
/// a LIVE client or fell back to paper (`assemble_engine`, under [`crate::make_engine_for_account`]).
///
/// ⚠ **Announcing it from [`report_halt_admit`] was a claim the mount could not keep**: that runs
/// at the TOP of [`crate::make_engine_for_account`], before credentials and cTrader's synchronous
/// `connect_and_auth_exec`, either of which can demote to `vike_paper::PaperExecutionClient` (no
/// position book) — so an operator was told `verify` was armed on a venue that mounted PAPER, the
/// original defect's exact shape.
///
/// So NOT-armed is a `warn!` (missing credentials or a failed handshake — what the operator came to
/// the log for), and `HaltAdmit::Admit` logs nothing on any path. The DECISION is
/// `vike_model::halt_admit_arming`, pure and exhaustively tested; this is only its `tracing` shell.
pub(crate) fn report_halt_admit_armed(venue: &str, effective: vike_model::HaltAdmit, live: bool) {
    match vike_model::halt_admit_arming(effective, live) {
        vike_model::HaltAdmitArming::Silent => {}
        vike_model::HaltAdmitArming::Armed => tracing::info!(
            venue,
            "halt_admit=verify is ARMED on this venue: under an engaged HALT a reduce_only submit \
             is checked against the venue's own position book, and refused only when that book \
             PROVES it opens risk (an unknown book still admits — a halt must never trap you)"
        ),
        vike_model::HaltAdmitArming::MountedPaper => tracing::warn!(
            venue,
            "`policy.halt_admit` is `verify` and this venue's adapter CAN honour it, but \
             it mounted PAPER (no credentials, or the exec handshake failed) — the paper client \
             holds no position book, so nothing is verified this session. See \
             docs/ops/kill-switches.md"
        ),
    }
}

/// The paper exchange behind every paper engine this crate builds — a capped venue's
/// ([`paper_engine`]) and every mount outcome that lands on paper (`crate::contract`) — built ONCE
/// here so it is armed ONCE here.
///
/// ⚠ **The arming is why this exists.** A paper book is still a MOUNT: what a credential-less
/// venue runs, what `vike-tradehub`'s paper variant runs, and so where an operator REHEARSES
/// `touch <project>/settings/state/HALT` — which did nothing on a paper mount before this.
/// `crates/vike-paper/src/lib.rs`'s `with_halt_path` says why the sentinel is a parameter and why a
/// BACKTEST must never get one. The path is `crates/vike-bridge-core/src/halt.rs`'s
/// `halt_path_from_env`, the once-per-process resolution every adapter on the shared `ExecActor`
/// uses, so a mixed live/paper node has exactly ONE sentinel to `touch`.
pub(crate) fn paper_client(
    venue: &str,
    symbol: &str,
    fee_schedule: vike_model::FeeSchedule,
) -> vike_paper::PaperExecutionClient {
    vike_paper::PaperExecutionClient::with_fee_schedule(venue, symbol, 0.0002, fee_schedule)
        .with_halt_path(vike_bridge_core::halt::halt_path_from_env())
}

/// The whole engine an account gets when its TIER is paper — assembled WITHOUT reading a
/// credential, fetching an instrument grid or opening a socket.
///
/// A separate assembly, not a fall-through: a paper-tier account reaching
/// [`make_engine_for_account`]'s
/// match would already have resolved credentials — the exposure being closed. It must still be the
/// SAME engine an uncredentialled venue gets (a paper tier refuses an arming, it invents no mode):
/// `a_capped_venue_is_the_same_engine_an_uncredentialled_one_gets` mounts both through the real
/// function and holds their `vike_exec::state_hash` equal. Each step is the post-match tail's call
/// for a paper venue, in the same (load-bearing) order, with a paper arm's values: no leg grids, no
/// venue-fetched limits, no reconcile handle, no contract multiplier, `MarginMode::Cross`.
///
/// ⚠ **Takes `account` for the SAME reason the live twin does — a real defect until 2026-09-27.**
/// A venue paper by its arming setting got a MORE PERMISSIVE risk engine than one paper for lacking
/// credentials: only the BOX-WIDE `max_account_exposure` was folded, never the per-account figure
/// (now the account rows' `max_exposure`, `crate::arming`'s `account_max_exposure`). The guard test
/// missed it: neither `MountPolicy` it builds set an account figure.
pub(crate) fn paper_engine(
    venue: &str,
    symbol: &str,
    account: &AccountLabel,
    declared_legs: &[String],
    risk_profile: Option<&vike_model::ProfileRisk>,
    policy: Option<&crate::MountPolicy>,
) -> EngineAndRecon {
    let static_default = vike_model::fee_schedule_for(vike_catalog::fee_lane(venue, symbol));
    let mut limits = vike_model::RiskLimits::new();
    // No per-leg grid, like every paper arm: a declared leg is judged on the mounted symbol's
    // grid, and says so. `NoGrid`: these limits carry no venue grid, so the source is never read.
    symbol_grid::warn_ungridded_legs(
        venue,
        vike_bridge_core::venue_mount::DeclaredGridSource::NoGrid,
        symbol,
        declared_legs,
        &limits.grid_by_symbol,
        &limits,
    );
    limits = merge_operator_budget(venue, limits, risk_profile);
    limits = arm_universal_defaults(limits);
    limits.im_requirement = limits.im_requirement.or(Some(1.0));
    // The ACCOUNT-aggregate ceiling: **a paper mount must not be the PERMISSIVE side of the mount
    // it rehearses** — a gate admitting what the live arm refuses teaches an operator an unsafe
    // configuration is safe (`vike_sim::SimBroker::gate_order` states the rule for the backtest,
    // which was measured to be the permissive side once already). `None` is off and costs nothing.
    //
    // ⚠ From `policy`, not `risk_profile`, hence the parameter
    // (`vike_config::Policy::max_account_exposure` argues why the number lives in the policy).
    // ⚠ `narrow_account_exposure`, not an assignment (as at `crate::make_engine_for_account`): the
    // fold is a `min`, so no later writer of the field can RAISE this ceiling.
    limits.narrow_account_exposure(policy.and_then(|p| p.max_account_exposure));
    // …AND THE SAME CEILING FOR THIS ONE ACCOUNT (missing until 2026-09-27; the fn doc). Two folds,
    // as the live twin: `min(box, account)`, either side absent falling through.
    limits.narrow_account_exposure(account_max_exposure(policy, venue, account));
    // The SIZING-EQUITY ceiling, same reason: a rehearsal sizing against a bigger equity than the
    // live arm gives position sizes the operator will not get. `None` is off.
    limits.narrow_sizing_equity(policy.and_then(|p| p.max_sizing_equity));
    // ⚠ `require_live_risk_budget` is NOT called, and must not be: the refusal is scoped to a mount
    // intended LIVE, and refusing an account its paper tier just disarmed is the false refusal
    // oanda's
    // `resolve` and fxcm's (`FxcmVenueMount` in `crates/bridges/fxcm/src/mount.rs`) exist to avoid.
    let fee_schedule = resolve_fee_schedule(venue, None, static_default);
    tracing::info!(venue, ?fee_schedule, "effective fee schedule");
    // Spelled out: with one arm, inference would pin `Box<PaperExecutionClient>` and hand back a
    // different `ExecutionEngine<_>` from the one `EngineAndRecon` names.
    let client: Box<dyn vike_exec::ExecutionClient + Send> =
        Box::new(paper_client(venue, symbol, static_default));
    let mut engine = vike_exec::ExecutionEngine::new(
        vike_exec::Account::new(
            1.0,
            venue,
            multiplier_grid(symbol, 0.0),
            vike_exec::BalanceMode::Delta,
        )
        .with_default_margin_modes(margin_mode_grid(symbol, vike_model::MarginMode::Cross)),
        vike_exec::RiskGate::new(limits),
        client,
        venue,
        symbol,
    );
    engine.fee_schedule = Some(fee_schedule);
    (engine, None)
}

/// Say, once per paper-tier account, that the `account` table put it on paper — naming the
/// account, the CAUSE (`block`: no row, inactive, `paper` tier, two active tiers, an unreadable
/// store) and the commands that change it, so "why is this account paper?" needs no grep. The line
/// keeps its `ARMING:` prefix: `docs/ops/kill-switches.md` tells operators to read it.
///
/// ⚠ **The LEVEL is the design.** A `warn!` per paper account would fire ~14 times on every start
/// of a never-configured box — the shape `crates/vike-config/src/arming.rs` names as how a refusal
/// list teaches operators to work around it. So: `warn!` when credentials would have armed it (an
/// outcome CHANGED), `debug!` when it has none (absent credentials were already the gate) — and
/// `error!` for `TierConflict` whatever the keys say: the operator activated two tiers of one
/// account, and the mount refuses to pick between a demo and a mainnet key set.
///
/// That question is [`would_mount_under_some_tier`] — PURE (reads `vars` through each arm's config
/// loader; no file, no dial, no signature), the one credential-MATERIAL touch on the paper path,
/// worth it so the line can tell an operator whether the account table held anything back.
///
/// ⚠ **Not `crate::would_mount_live`** — `Live`-only, so a DEMO-only venue would be misreported as
/// having no credentials, the wrong half of this split.
pub(crate) fn report_capped_to_paper(
    registry: &'static [crate::VenueRow],
    venue: &str,
    account: &AccountLabel,
    block: vike_config::ArmingBlock,
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) {
    let subject = crate::account_route_key(venue, account);
    let ids = crate::arming::account_ids(policy, venue, account);
    let id = match ids.as_slice() {
        [] => "<N>".to_string(),
        ids => ids.iter().map(i64::to_string).collect::<Vec<_>>().join("|"),
    };
    let remedy = format!(
        "`vike-cli secrets accounts` lists the account rows; `vike-cli secrets account activate \
         --id {id}` and `vike-cli secrets account set-tier --id {id} --tier <demo|live>` arm one \
         at the next restart"
    );
    let why = block.as_str();
    if block == vike_config::ArmingBlock::TierConflict {
        tracing::error!(
            venue,
            account = %account,
            block = why,
            "ARMING: {subject}: the account table holds it at paper — {why}: two ACTIVE account \
             rows of this account name different non-paper tiers, and the mount will not pick one. \
             Deactivate one: `vike-cli secrets account deactivate --id {id}`. {remedy}."
        );
    } else if would_mount_under_some_tier(registry, venue, vars) {
        tracing::warn!(
            venue,
            account = %account,
            block = why,
            "ARMING: {subject}: the account table holds it at paper — {why}; {remedy}. It has \
             credentials, and no credential was read, no instrument grid fetched and no socket \
             opened for it."
        );
    } else {
        tracing::debug!(
            venue,
            account = %account,
            block = why,
            "ARMING: {subject}: the account table holds it at paper — {why}; it has no credentials \
             either, so nothing was refused"
        );
    }
}

#[path = "paper_fallback_tests.rs"]
#[cfg(test)]
mod paper_fallback_tests;
