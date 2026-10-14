//! Halt-admit reporting, the PAPER TIER's engine, and the report for an account that does not run.
//!
//! An account runs at the tier its active row states or not at all (decision 0156): a `paper` row
//! gets [`paper_engine`] — the paper simulator, built without reading a credential — and every
//! account that does not run gets NO engine, only [`report_not_running`]'s line naming why. No
//! venue that cannot go live degrades to a paper client any more; the name of this file is older
//! than that rule.

use vike_model::accounts::account_keys::AccountLabel;

use crate::policy::DEFAULT_PAPER_SLIPPAGE;
use crate::{
    AdapterPolicy, EngineAndRecon, account_max_exposure, arm_universal_defaults, margin_mode_grid,
    merge_operator_budget, multiplier_grid, resolve_fee_schedule, symbol_grid,
};

/// **Whether any `account` row of `(venue, label)` holds a credential** — the question
/// [`report_table_decision`] asks. A credential belongs to its account row (decision 0156 item
/// 6), so the answer is the rows' own credentials, whatever their tier and whether or not active.
fn holds_credentials(policy: Option<&AdapterPolicy>, venue: &str, label: &AccountLabel) -> bool {
    let directory = crate::arming::directory_of(policy);
    let (Some(Ok(accounts)), Some(Ok(book))) = (directory.rows(), directory.credentials()) else {
        return false;
    };
    accounts.known().is_some_and(|rows| {
        rows.iter().any(|r| {
            r.venue == venue
                && r.label.as_deref() == label.text()
                && book.account(r.id).is_some_and(|fields| !fields.is_empty())
        })
    })
}

/// Resolve the halt-admit mode in force at `venue`, and SAY SO when the venue cannot honour it.
///
/// ⚠ **The report is the feature.** An operator who set `policy.halt_admit verify` believes the
/// kill switch verifies positions; on every roster venue but cTrader no position book exists at
/// that boundary, and a SILENT degrade would leave that belief standing until an incident. So the
/// line lands at START, with venue and reason — as `vike_tradehub_core::halt::halt_path_arming_error`
/// reports an unarmable sentinel at start, not at the first order.
///
/// ⚠ **Only this half is answerable up front**: "no position book in this adapter" is a fact about
/// CODE. Whether `verify` ARMED is a fact about the START — [`crate::make_engine_for_account`]
/// calls this before credentials and cTrader's handshake, either of which can leave the account
/// not running — and is [`report_halt_admit_armed`]'s job.
///
/// Silent under the DEFAULT (`HaltAdmit::Admit` degrades nowhere): a box with no
/// `policy.halt_admit` row gains no log line — byte-identical in the trace file too.
pub(crate) fn report_halt_admit(
    venue: &str,
    policy: Option<&AdapterPolicy>,
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

/// Say whether `verify` actually ARMED, from the one place that knows: after the venue client came
/// up (`assemble_engine`, under [`crate::make_engine_for_account`]).
///
/// ⚠ **Announcing it from [`report_halt_admit`] was a claim the start could not keep**: that runs
/// at the TOP of [`crate::make_engine_for_account`], before credentials and cTrader's synchronous
/// `connect_and_auth_exec`, either of which can leave the account NOT RUNNING (decision 0156: no
/// paper client in its place) — so an operator would be told `verify` was armed on an account that
/// runs nothing.
///
/// Called only for a venue client that came up, so it asks `vike_model::halt_admit_arming` the
/// LIVE question; an account that does not run says why through [`report_not_running`] instead, and
/// `HaltAdmit::Admit` logs nothing on any path. The DECISION is `vike_model::halt_admit_arming`,
/// pure and exhaustively tested; this is only its `tracing` shell.
pub(crate) fn report_halt_admit_armed(venue: &str, effective: vike_model::HaltAdmit) {
    match vike_model::halt_admit_arming(effective, true) {
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
             the account does not run (no credentials, or the exec handshake failed) — no client \
             holds a position book for it, so nothing is verified this session. See \
             docs/ops/kill-switches.md"
        ),
    }
}

/// The paper exchange behind the paper engine this crate builds for a `paper` account
/// ([`paper_engine`]) — built ONCE here so it is armed ONCE here.
///
/// ⚠ **The arming is why this exists.** A paper book is still a START: what a `paper` account runs,
/// what `vike-tradehub`'s paper variant runs, and so where an operator REHEARSES
/// `touch <project>/settings/state/HALT` — which did nothing on a paper start before this.
/// `crates/vike-paper/src/client.rs`'s `with_halt_path` says why the sentinel is a parameter and why a
/// BACKTEST must never get one. The path is `crates/vike-tradehub-core/src/halt.rs`'s
/// `halt_path_from_env`, the once-per-process resolution every adapter on the shared `ExecActor`
/// uses, so a mixed live/paper node has exactly ONE sentinel to `touch`.
///
/// `slippage` is a fraction of price: [`paper_book_for`] passes the `config.paper_slippage` row
/// ([`AdapterPolicy::paper_slippage`]) over [`DEFAULT_PAPER_SLIPPAGE`]. It is a SEPARATE cost from
/// `fee_schedule`, which stays the venue's own.
pub(crate) fn paper_client(
    venue: &str,
    symbol: &str,
    slippage: f64,
    fee_schedule: vike_model::FeeSchedule,
) -> vike_paper::PaperExecutionClient {
    vike_paper::PaperExecutionClient::with_fee_schedule(venue, symbol, slippage, fee_schedule)
        .with_halt_path(crate::halt::halt_path_from_env())
}

/// The paper book [`paper_engine`] hands the engine, with its slippage resolved: the
/// `config.paper_slippage` row the root put on the policy, else [`DEFAULT_PAPER_SLIPPAGE`] — the
/// figure this book always had — so no policy, or a policy without the row, builds the same book.
/// A function of its own so the policy-to-book leg is a test on the CONCRETE client (the engine
/// owns it as a trait object, which a test cannot ask for its slippage).
fn paper_book_for(
    venue: &str,
    symbol: &str,
    policy: Option<&AdapterPolicy>,
    fee_schedule: vike_model::FeeSchedule,
) -> vike_paper::PaperExecutionClient {
    let slippage = policy.and_then(|p| p.paper_slippage).unwrap_or(DEFAULT_PAPER_SLIPPAGE);
    paper_client(venue, symbol, slippage, fee_schedule)
}

/// **The whole engine a `paper` account gets** — the paper simulator, assembled WITHOUT reading a
/// credential, fetching an instrument grid or opening a socket. The one paper engine the start
/// builds: an account at `demo`/`live` that does not arm gets NONE (decision 0156). Its
/// `vike_exec::ExecutionEngine::tier` is `new`'s `Paper` seed.
///
/// A separate assembly, not a fall-through: a paper-tier account reaching
/// [`make_engine_for_account`]'s
/// match would already have resolved credentials — the exposure being closed. Each step is the
/// shared tail's call in the same (load-bearing) order, with a paper arm's values: no leg grids, no
/// venue-fetched limits, no reconcile handle, no contract multiplier, `MarginMode::Cross`.
///
/// ⚠ **Takes `account` for the SAME reason the live twin does — a real defect until 2026-09-27.**
/// A paper account got a MORE PERMISSIVE risk engine than its live twin: only the BOX-WIDE
/// `max_account_exposure` was folded, never the per-account figure (now the account rows'
/// `max_exposure`, `crate::arming`'s `account_max_exposure`). The guard test missed it: neither
/// `AdapterPolicy` it builds set an account figure.
pub(crate) fn paper_engine(
    venue: &str,
    symbol: &str,
    account: &AccountLabel,
    declared_legs: &[String],
    risk_profile: Option<&vike_model::ProfileRisk>,
    policy: Option<&crate::AdapterPolicy>,
) -> EngineAndRecon {
    let static_default = vike_venues::fee_schedule_for(vike_catalog::fee_lane(venue, symbol));
    let mut limits = vike_model::RiskLimits::new();
    // No per-leg grid, like every paper arm: a declared leg is judged on the engine symbol's
    // grid, and says so. `NoGrid`: these limits carry no venue grid, so the source is never read.
    symbol_grid::warn_ungridded_legs(
        venue,
        vike_bridge_core::venue_adapter::DeclaredGridSource::NoGrid,
        symbol,
        declared_legs,
        &limits.grid_by_symbol,
        &limits,
    );
    // The PAPER tier's grid source (decision 0156 item 7): no grid was fetched, so a profile's
    // instrument fields are this engine's grid, the same answer the paper daemon's
    // `vike_config::RunProfile::apply_risk` gives a `paper` account.
    limits = merge_operator_budget(venue, limits, risk_profile, vike_model::AccountTier::Paper);
    limits = arm_universal_defaults(limits);
    limits.im_requirement = limits.im_requirement.or(Some(1.0));
    // The ACCOUNT-aggregate ceiling: **a paper start must not be the PERMISSIVE side of the start
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
    // ⚠ `require_live_risk_budget` is NOT called, and must not be: the refusal is scoped to a start
    // intended LIVE, and refusing an account its paper tier just disarmed is the false refusal
    // oanda's
    // `resolve` and fxcm's (`FxcmVenueAdapter` in `crates/bridges/fxcm/src/adapter.rs`) exist to avoid.
    let fee_schedule = resolve_fee_schedule(venue, None, static_default);
    tracing::info!(venue, ?fee_schedule, "effective fee schedule");
    // Spelled out: with one arm, inference would pin `Box<PaperExecutionClient>` and hand back a
    // different `ExecutionEngine<_>` from the one `EngineAndRecon` names.
    let client: Box<dyn vike_exec::ExecutionClient + Send> =
        Box::new(paper_book_for(venue, symbol, policy, static_default));
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

/// Say, once per account the `account` table decides — a `paper` account, which runs the paper
/// simulator, and one the table does NOT RUN (no row, inactive, two active tiers, an unreadable
/// store) — naming the account, the CAUSE (`block`) and the commands that change it, so "why does
/// this account not run?" needs no grep. The line keeps its `ARMING:` prefix:
/// `docs/ops/kill-switches.md` tells operators to read it.
///
/// ⚠ **The LEVEL is the design.** A `warn!` per unconfigured account would fire ~14 times on every
/// start of a never-configured box — the shape `crates/vike-config/src/arming.rs` names as how a
/// refusal list teaches operators to work around it. So: `warn!` when credentials would have armed
/// it (an outcome CHANGED), `debug!` when it has none (absent credentials were already the gate) —
/// and `error!` for `TierConflict` whatever the keys say: the operator activated two tiers of one
/// account, and the start refuses to pick between a demo and a mainnet key set.
///
/// That question is [`holds_credentials`] — PURE (the account rows' own credentials, as the
/// composition root read them; no file, no dial, no signature), worth it so the line can tell an
/// operator whether the account table held anything back.
///
/// An account whose row states `demo`/`live` and that its venue's start did not arm says so through
/// [`report_not_running`] instead, at a level that does not depend on its keys.
///
/// (Named `report_capped_to_paper` until decision 0156, when every one of these accounts was
/// started on paper; none is capped to anything now.)
pub(crate) fn report_table_decision(
    venue: &str,
    account: &AccountLabel,
    block: vike_config::ArmingBlock,
    policy: Option<&AdapterPolicy>,
) {
    let subject = crate::account_route_key(venue, account);
    let id = id_arg(policy, venue, account);
    let remedy = format!(
        "`vike-cli secrets accounts` lists the account rows; `vike-cli secrets account activate \
         --id {id}` and `vike-cli secrets account set-tier --id {id} --tier <paper|demo|live>` run \
         one at the next restart"
    );
    let why = block.as_str();
    // What the table did with it, in words: a `paper` row runs the simulator; everything else does
    // not run (decision 0156 — never a paper engine in its place).
    let outcome = if block == vike_config::ArmingBlock::PaperTier {
        "its tier is `paper`: it runs the paper simulator"
    } else {
        "the account table does NOT RUN it"
    };
    if block == vike_config::ArmingBlock::TierConflict {
        // A venue-scoped tier (decision 0140) conflicts over the rows of EVERY label.
        let rows = if vike_venues::venue_tier_scope(venue) == vike_venues::TierScope::Venue {
            "of this venue, of any label (it rides ONE session per box),"
        } else {
            "of this account"
        };
        tracing::error!(
            venue,
            account = %account,
            block = why,
            "ARMING: {subject}: {outcome} — {why}: two ACTIVE account rows {rows} name different \
             non-paper tiers, and the mount will not pick one. Deactivate one: `vike-cli secrets \
             account deactivate --id {id}`. {remedy}."
        );
    } else if holds_credentials(policy, venue, account) {
        tracing::warn!(
            venue,
            account = %account,
            block = why,
            "ARMING: {subject}: {outcome} — {why}; {remedy}. It has credentials, and no credential \
             was read, no instrument grid fetched and no socket opened for it."
        );
    } else {
        tracing::debug!(
            venue,
            account = %account,
            block = why,
            "ARMING: {subject}: {outcome} — {why}; it has no credentials either, so nothing was \
             refused"
        );
    }
}

/// **Say that an account whose row states `demo`/`live` does NOT RUN** (decision 0156), once, with
/// its reason: the venue's arm did not arm it at that tier (no usable key set, a demo-only arm, the
/// SDK, a box switch), the startup preflight hard-failed its venue, or its start did not come up.
/// No engine is built for it and no paper engine in its place, so an order addressed to it is
/// refused with this same reason (`vike_core::account_ambiguity`'s `unheld_account_refusal`).
///
/// `warn!`: the operator asked for this account to trade, and it does not. `error!` for the two
/// causes only the network or the box could produce — a failed preflight and a start that did not
/// come up — which is where an operator looks first after a start that "did nothing". The full
/// sentence is `vike_config::VenueArming::why`'s, the one `vike-backend venues` renders.
pub(crate) fn report_not_running(not_running: &crate::NotRunning, policy: Option<&AdapterPolicy>) {
    let crate::NotRunning { venue, account, tier, block } = not_running;
    let subject = not_running.route_key();
    let why = block.as_str();
    // The row's own sentence for a roster venue (a `VenueArming` names a roster id); the bare
    // reason word for an id the roster does not carry (a test's planted venue).
    let sentence = vike_model::VENUES.iter().copied().find(|v| *v == venue.as_str()).map_or_else(
        || why.to_string(),
        |roster_id| {
            vike_config::VenueArming {
                venue: roster_id,
                label: account.clone(),
                tier: *tier,
                effective: None,
                block: *block,
                account_ids: crate::arming::account_ids(policy, venue, account),
            }
            .why()
        },
    );
    match block {
        vike_config::ArmingBlock::PreflightFailed | vike_config::ArmingBlock::AdapterFailed => {
            tracing::error!(
                venue = %venue,
                account = %account,
                block = why,
                "ARMING: {subject} does NOT RUN — {why}: {sentence}"
            );
        }
        _ => tracing::warn!(
            venue = %venue,
            account = %account,
            block = why,
            "ARMING: {subject} does NOT RUN — {why}: {sentence}"
        ),
    }
}

/// The `--id` a remedy names: the account's own active row ids, else a placeholder.
fn id_arg(policy: Option<&AdapterPolicy>, venue: &str, account: &AccountLabel) -> String {
    let ids = crate::arming::account_ids(policy, venue, account);
    match ids.as_slice() {
        [] => "<N>".to_string(),
        ids => ids.iter().map(i64::to_string).collect::<Vec<_>>().join("|"),
    }
}

#[path = "paper_fallback_tests.rs"]
#[cfg(test)]
mod paper_fallback_tests;
