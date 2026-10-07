//! Halt-admit reporting and the paper-fallback path: what happens when a venue can't (or won't)
//! go live — degrade to `PaperExecutionClient`, report why, and warn once if the operator's
//! credentials say a venue should be live but the `policy.venues` rows haven't caught up.

use std::collections::HashMap;

use vike_model::accounts::account_keys::AccountLabel;

use crate::{
    EngineAndRecon, MountPolicy, arm_universal_defaults, margin_mode_grid, merge_operator_budget,
    multiplier_grid, resolve_fee_schedule, symbol_grid, would_mount_live_under,
};

/// **Whether `venue`'s stored credentials would arm it under EITHER tier** (`demo` or `live`) —
/// the question both reports below ask. NOT `crate::would_mount_live`, which probes only `Live`:
/// since decision 0095 a `live` ceiling with only DEMO keys is PAPER for
/// binance/bybit/okx/hyperliquid, so that probe would silently drop their demo-only rows from a
/// report whose job is to name every venue with SOME credentials.
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

/// The whole engine a venue gets when the ARMING CEILING refused it — assembled WITHOUT reading a
/// credential, fetching an instrument grid or opening a socket.
///
/// A separate assembly, not a fall-through: a capped venue reaching [`make_engine_for_account`]'s
/// match would already have resolved credentials — the exposure being closed. It must still be the
/// SAME engine an uncredentialled venue gets (the ceiling refuses an arming, it invents no mode):
/// `a_capped_venue_is_the_same_engine_an_uncredentialled_one_gets` mounts both through the real
/// function and holds their `vike_exec::state_hash` equal. Each step is the post-match tail's call
/// for a paper venue, in the same (load-bearing) order, with a paper arm's values: no leg grids, no
/// venue-fetched limits, no reconcile handle, no contract multiplier, `MarginMode::Cross`.
///
/// ⚠ **Takes `account` for the SAME reason the live twin does — a real defect until 2026-09-27.**
/// A venue paper by `policy.venues.<venue>` got a MORE PERMISSIVE risk engine than one paper for
/// lacking credentials: only the BOX-WIDE `max_account_exposure` was folded, never the per-account
/// `policy.account_exposure.<venue>.<LABEL>` (`vike_config::VenuePolicy::account_exposure`). The
/// guard test missed it: neither `MountPolicy` it builds sets an account-exposure row.
pub(crate) fn paper_engine(
    venue: &str,
    symbol: &str,
    account: &AccountLabel,
    declared_legs: &[String],
    risk_profile: Option<&vike_exec::ProfileRisk>,
    policy: Option<&crate::MountPolicy>,
) -> EngineAndRecon {
    let static_default = vike_model::fee_schedule_for(vike_catalog::fee_lane(venue, symbol));
    let mut limits = vike_exec::RiskLimits::new();
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
    limits.narrow_account_exposure(policy.and_then(|p| p.venues.account_exposure(venue, account)));
    // The SIZING-EQUITY ceiling, same reason: a rehearsal sizing against a bigger equity than the
    // live arm gives position sizes the operator will not get. `None` is off.
    limits.narrow_sizing_equity(policy.and_then(|p| p.max_sizing_equity));
    // ⚠ `require_live_risk_budget` is NOT called, and must not be: the refusal is scoped to a mount
    // intended LIVE, and refusing a venue the ceiling just disarmed is the false refusal oanda's
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

/// Say, once per capped venue, that its `policy.venues.<venue>` row put it on paper — naming the
/// venue, the ROW and the command that changes it, so "why is this venue paper?" needs no grep.
///
/// ⚠ **The LEVEL is the design.** A `warn!` per capped venue would fire ~14 times on every start of
/// a never-configured box — the shape `crates/vike-config/src/arming.rs` names as how a refusal
/// list teaches operators to work around it. So: `warn!` when credentials would have armed it (an
/// outcome CHANGED), `debug!` when it has none (absent credentials were already the gate).
///
/// That question is [`would_mount_under_some_tier`] — PURE (reads `vars` through each arm's config
/// loader; no file, no dial, no signature), the one credential-MATERIAL touch on the capped path,
/// worth it so the line can tell an operator whether their ceiling changed anything.
///
/// ⚠ **Not `crate::would_mount_live`** — `Live`-only, so after decision 0095 a DEMO-only
/// binance/bybit/okx/hyperliquid would be misreported as having no credentials, the wrong half of
/// this split.
pub(crate) fn report_capped_to_paper(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) {
    let declared = policy.is_some_and(|p| p.venues.is_declared());
    if would_mount_under_some_tier(registry, venue, vars) {
        tracing::warn!(
            venue,
            declared,
            "ARMING CEILING: {venue} has credentials, and this deployment's ceiling for it is \
             `paper` → staying PAPER. No credential was read, no instrument grid fetched and no \
             socket opened for it. Run `vike-cli config set policy.venues.{venue} <mode>` (one of \
             paper / demo / live) to change that."
        );
    } else {
        tracing::debug!(
            venue,
            declared,
            "arming ceiling for {venue} is `paper` (the policy.venues.{venue} row); it has no \
             credentials either, so nothing was refused"
        );
    }
}

/// **The upgrade warning, fired at most ONCE per process**: a box with credentials and no
/// `policy.venues` row drops EVERY venue to paper on its first start under the ceiling, and
/// must not discover that from a fills report.
///
/// # Why WARN-AND-PAPER rather than refuse
///
/// Per `docs/decisions/0013-degrade-vs-refuse.md` a venue connection is a CAPABILITY (listed there
/// by name), so degrade — and paper strictly REDUCES authority (worst case a missed trade).
/// Concretely this fires on an UPGRADE of a running deployment, where a refusal strands resting
/// orders on nine venues behind a daemon that will not come up and cannot flatten — the record's
/// own reopen condition, answered rather than invoked. `crates/vike-config/src/arming.rs` refuses
/// instead for the credential-file case, correctly: an appended real-money switch row (a live
/// `flags.poly_exec`, or a retired `{VENUE}_MAINNET` one decision 0095 refuses on sight) asks for
/// MORE authority or arms nothing, so refusing costs an honest operator nothing.
///
/// # Self-silencing, and what silences it
///
/// Fires only while BOTH hold: some venue has credentials that WOULD have armed it, and no
/// `policy.venues.<venue>` row exists (`vike_config::VenuePolicy::is_declared`). One row, even at
/// `paper`, silences it (a warning firing on a correct config is the "refusal
/// list that fires on harmless lines" `arming.rs` warns about); `report_capped_to_paper` still
/// covers venues left out. No credentials, nothing refused, no message.
///
/// # ⚠ Venue slugs only
///
/// It names venues and `vike-cli config` commands, never a credential key NAME (nor a value) —
/// as `refuse_credential_file_arming` — because a paste-ready block carrying key names invites the
/// reply that carries values.
///
/// Returns the message (testable with no subscriber); the `Once` latch is at the call site. Public
/// for the roster tests in `crates/vike-tradehub/tests/mount_roster.rs` (docs/decisions/0096).
pub fn venue_arming_migration_message(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) -> Option<String> {
    if policy.is_some_and(|p| p.venues.is_declared()) {
        return None;
    }
    // Decision 0095: NOT `would_mount_live` (`Live`-only; it drops demo-only switched venues) —
    // `would_mount_under_some_tier` asks: credentials for EITHER tier?
    let credentialled: Vec<&'static str> = vike_model::VENUES
        .iter()
        .copied()
        .filter(|v| would_mount_under_some_tier(registry, v, vars))
        .collect();
    if credentialled.is_empty() {
        return None;
    }
    let mut out = String::from(
        "ARMING CEILING NOW IN FORCE: this box's settings carry no policy.venues rows, so EVERY \
         venue on this box is capped to `paper` — including the ones whose credentials would have \
         armed them before this build.\n\n\
         Credential presence used to be the only gate, which is why a run profile naming one venue \
         could hold live authenticated sessions on nine. The ceiling is the setting that says which \
         venues this deployment MEANS to trade, and its default is the safe end.\n\n\
         These venues have credentials and are now PAPER:\n\n",
    );
    for venue in &credentialled {
        out.push_str(&format!("  {venue}\n"));
    }
    out.push_str(
        "\nTo restore them, run one of these — and CHOOSE each value (`live` is real money; `demo` \
         is the venue's own demo/testnet account; `paper` keeps it simulated). For most roster \
         venues a ceiling can only ever REFUSE: it declines to stop the credentials and the mount \
         arm from reaching the tier they resolve to.",
    );
    // ⚠ Venue slugs only: the explainer names ONLY switched venues (decision 0095) that ARE
    // credentialled on THIS box — a bybit-only box must never see "binance";
    // `the_migration_warning_names_the_command_and_only_venue_slugs` pins it.
    let switched: Vec<&'static str> =
        credentialled.iter().copied().filter(|v| network_is_the_ceiling(v)).collect();
    if !switched.is_empty() {
        let list = switched.join("/");
        out.push_str(&format!(
            " For {list} the ceiling now SELECTS the tier outright (decision 0095) — the lines \
             below name `demo` for them because that is what this box actually traded before (a \
             `{{VENUE}}_MAINNET` flag could never have been set on a box that reaches this \
             message, since a SET one refuses this process's own boot); `live` for them would be a \
             DELIBERATE new choice, never a restoration, and this message never pastes it for you."
        ));
    }
    out.push_str("\n\n");
    for venue in &credentialled {
        // Decision 0095: for the switched four the ceiling ALONE picks the network, and a box
        // reaching this message never had `{VENUE}_MAINNET` set (a set one refuses boot), so its
        // PRIOR trading was DEMO whatever LIVE keys sit in the store: the restore line says `demo`
        // and never makes the NEW `live` choice for the operator. Other venues ask the real probe.
        let mode = if network_is_the_ceiling(venue) {
            "demo"
        } else if would_mount_live_under(registry, venue, vars, vike_config::VenueMode::Live) {
            "live"
        } else {
            "demo"
        };
        out.push_str(&format!("vike-cli config set policy.venues.{venue} {mode}\n"));
    }
    out.push_str(
        "\nWriting even one of these silences this message, whatever the values — including \
         `paper`, which is a decision like any other. `vike-cli config show --filter policy` \
         prints what the binaries will read.\n",
    );
    Some(out)
}

/// **The venues whose NETWORK the ceiling chooses** (decision 0095), from the store migration's own
/// list `vike_secrets::live_means_mainnet::SWITCHED_VENUES`: a box reaching
/// [`venue_arming_migration_message`] traded these on DEMO, so its restore line never says `live`.
///
/// ⚠ A membership test on that constant, NOT a venue list of this crate's own: the same four
/// venues, exactly the set whose `{VENUE}_MAINNET` switch the restore argument is about (it
/// replaced `crate::arming`'s `ceiling_selects_mainnet` plus a hyperliquid equality,
/// docs/decisions/0096).
fn network_is_the_ceiling(venue: &str) -> bool {
    vike_secrets::live_means_mainnet::SWITCHED_VENUES.contains(&venue)
}

/// The `Once` latch over [`venue_arming_migration_message`]: [`make_engine_for_account`] runs per
/// mount, this text names every venue at once — `vike_bridge_core::halt::halt_path_from_env`'s
/// idiom (a process-wide fact noticed per venue; fourteen paste-ready blocks would bury it).
pub(crate) fn venue_arming_migration(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if let Some(message) = venue_arming_migration_message(registry, vars, policy) {
            tracing::warn!("{message}");
        }
    });
}

#[path = "paper_fallback_tests.rs"]
#[cfg(test)]
mod paper_fallback_tests;
