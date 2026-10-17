//! The shared tail: the one shape every start's parts take, and the fold that makes them an engine.

use std::collections::HashSet;

use vike_model::accounts::account_keys::AccountLabel;

use crate::symbol_grid::GridSource;
use crate::{
    AdapterBind, AdapterPolicy, ConfirmedBook, EngineAndRecon, EngineError, IdentityRecord,
    VenueBackend, account_max_exposure, arm_universal_defaults, contract_value_grid,
    margin_mode_grid, merge_operator_budget, report_halt_admit_armed, require_live_risk_budget,
    resolve_fee_schedule, symbol_grid,
};

/// What a venue's start produced, before the shared tail folds it: the ONE shape a successful
/// backend start ([`AdapterBind`]) takes ([`parts_from_bind`]). A declined start produces none —
/// the account does not run (decision 0156) — and a `paper` account's engine is
/// `crate::paper_fallback`'s `paper_engine`, which reads no credential.
///
/// Formerly `MountParts`, renamed in the shape-and-names change (decision 0166).
pub(crate) struct AdapterParts {
    pub(crate) client: Box<dyn vike_exec::ExecutionClient + Send>,
    pub(crate) recon: Option<Box<dyn vike_exec::recon::ReconClient>>,
    pub(crate) limits: vike_model::RiskLimits,
    /// What one unit of the engine symbol's quantity is worth (decision 0170); the default, `Linear`
    /// 1.0, when the venue reported none.
    pub(crate) contract_value: vike_model::ContractValue,
    pub(crate) default_margin_mode: vike_model::MarginMode,
    pub(crate) symbol_grids: indexmap::IndexMap<String, vike_model::SymbolGrid>,
    pub(crate) grid_source: GridSource,
    /// The tier the venue client BOUND — what the identity record addresses. The start's
    /// tier interlock has already held it equal to the account's tier.
    pub(crate) bound_tier: vike_model::AccountTier,
    /// The book the venue itself named (hyperliquid's `userRole`).
    pub(crate) identity: Option<ConfirmedBook>,
    pub(crate) static_default: vike_model::FeeSchedule,
}

/// A successful backend start as the parts the shared tail assembles — the only outcome that
/// becomes an engine on the contract path (decision 0156: a declined start is NOT RUNNING, never a
/// paper client). `grid_source` is the venue's declared source, from the arming table's facts.
pub(crate) fn parts_from_bind(
    symbol: &str,
    declared_legs: &[String],
    bind: AdapterBind,
    grid_source: GridSource,
    static_default: vike_model::FeeSchedule,
) -> AdapterParts {
    let AdapterBind {
        client,
        recon,
        bound_tier,
        grid,
        contract_value,
        margin_mode,
        leg_grids,
        confirmed_book,
    } = bind;
    AdapterParts {
        client,
        recon,
        limits: grid.map_or_else(vike_model::RiskLimits::new, |g| {
            vike_model::RiskLimits::from_properties(&g)
        }),
        // A value from outside this process (the gatewayhub's reply) is folded here: a zero or
        // negative unit would size every order at nothing (decision 0170, review).
        contract_value: contract_value.unwrap_or_default().folded(),
        default_margin_mode: margin_mode.unwrap_or(vike_model::MarginMode::Cross),
        symbol_grids: symbol_grid::declared_symbol_grids(symbol, declared_legs, |leg| {
            leg_grids.iter().find(|(l, _)| l == leg).map(|(_, p)| *p)
        }),
        grid_source,
        // THE BOUND TIER — what the credentials authenticate, never a conjunct (the spec's
        // Finding 1). The tier interlock has already held it equal to the account's.
        bound_tier,
        identity: confirmed_book,
        static_default,
    }
}

/// **THE SHARED TAIL** every venue's mount parts become an engine through (docs/decisions/0096),
/// in order: `live_venues` gains the route key, the halt-admit ARMED report, the declared-leg grid,
/// the operator budget, the universal defaults, the exposure and equity ceilings, the live-budget
/// backstop, the fee schedule, the identity record, the engine at the account's `tier`. Called from
/// [`make_engine_for_account`] for a `demo`/`live` account whose venue client came up.
// The venue port is the eleventh argument: the shared tail records the identity through it, after
// the fee schedule (R38). Bundling it with the others would hide which are the start's own facts.
#[expect(clippy::too_many_arguments)]
pub(super) fn assemble_engine(
    backend: &dyn VenueBackend,
    venue: &str,
    symbol: &str,
    account: &AccountLabel,
    declared_legs: &[String],
    parts: AdapterParts,
    live_venues: &mut HashSet<String>,
    route_key: String,
    halt_admit: vike_model::HaltAdmit,
    risk_profile: Option<&vike_model::ProfileRisk>,
    policy: Option<&AdapterPolicy>,
) -> Result<EngineAndRecon, EngineError> {
    let AdapterParts {
        client,
        recon,
        mut limits,
        contract_value,
        default_margin_mode,
        symbol_grids,
        grid_source,
        bound_tier,
        identity,
        static_default,
    } = parts;
    // Every engine this tail builds stands on a venue client: the account's route key joins the
    // live record (the DOM's `● LIVE` badge set, and the set `refuse_unarmed_live_venues` compares
    // against the locked one).
    live_venues.insert(route_key.clone());
    // ARMED half of the halt-admit report: HERE is the first point the answer is known
    // (`report_halt_admit` runs before credentials and handshakes, which can leave the account not
    // running). Silent but on cTrader (`vike_model::effective_halt_admit` degrades `verify` to
    // `admit`).
    report_halt_admit_armed(venue, halt_admit);
    // The DECLARED-LEG grid, at ONE site: after every arm's wholesale `RiskLimits::from_properties`
    // (which would discard it). Before the operator merge only for readability — both merge paths
    // (`ProfileRisk::apply_to`, `apply_operator_budget_only`) carry `grid_by_symbol` verbatim.
    // Empty (no leg, or not `InHand` per `declared_grid_source`) is the identity:
    // `RiskLimits::grid_for` returns the scalars and `skip_serializing_if` keeps
    // `vike_exec::state_hash` and journals unchanged.
    limits.grid_by_symbol = symbol_grids;
    // …and SAY so when a declared leg got none: it is judged on the engine symbol's tick, lot and
    // floors (the pre-existing behaviour, disclosed).
    symbol_grid::warn_ungridded_legs(
        venue,
        grid_source,
        symbol,
        declared_legs,
        &limits.grid_by_symbol,
        &limits,
    );
    // The operator's `[risk]` budget, merged at this ONE site for every venue
    // (`merge_operator_budget`; `risk_profile_wiring.rs`'s regression test calls it).
    //
    // LOAD-BEARING ORDERING: merge BEFORE the `im_requirement` rescue, never after.
    // `ProfileRisk::apply_to` takes EVERY operator-owned field unconditionally, so a profile silent
    // on `im_requirement` carries `None`; merging after the rescue let it CLOBBER `Some(1.0)` and
    // disarm the buying-power gate for every venue
    // (`profile_arms_the_limits_and_the_gate_denies_a_violating_order` failed on that order).
    // Merged first, the rescue fires when neither venue nor profile set it.
    //
    // The merge judges the profile's instrument fields under the ACCOUNT's tier (decision 0156
    // item 7). Every account reaching this tail is `demo` or `live` — a `paper` one returned through
    // `paper_engine` in `make_engine_for_account`, which reads no credential — and the tier a
    // client BOUND is the account's (the start's tier interlock, `crate::contract`'s
    // `within_the_tier`, refuses any other). The grid source is the venue's (a profile that set the
    // venue's grid never gets here: the start was refused before any credential,
    // `refuse_venue_owned_fields`).
    limits = merge_operator_budget(venue, limits, risk_profile, bound_tier);
    // The UNIVERSALLY-defaultable budget fields (`arm_universal_defaults`'s doc justifies each, and
    // why `required_free_bp_pct` needs no code). SAME ordering rule, merge FIRST: a profile silent on
    // `max_orders_per_window`/`window_ms` carries `None`, which would otherwise win and disarm them.
    limits = arm_universal_defaults(limits);
    // The buying-power gate's conservative 1× default (im 1.0), LAST (see above): `from_properties`
    // leaves it `None`. 1× = no leverage; the leverage pill raises it live (`Command::SetMargin`).
    // The SINGLE site arming "no leverage unless asked" (#822); `[risk] max_leverage` becomes this
    // field via `ProfileRisk`, so the `.or` is then a no-op.
    limits.im_requirement = limits.im_requirement.or(Some(1.0));
    // ---- the account-aggregate exposure ceiling ----
    // `policy.max_account_exposure`, armed only here, evaluated by
    // `vike_exec::RiskGate::check_inner`'s `over-account-exposure` lane on THIS account's projected
    // book. Why a policy key: `vike_config::Policy::max_account_exposure`; meaning:
    // `vike_model::RiskLimits::max_account_exposure`. `None` = lane off. It can only REFUSE.
    //
    // ⚠ ORDERING: after `merge_operator_budget` as belt-and-braces, not load-bearing — both merge
    // paths carry this policy-owned field from `base`; it sits with the other post-merge arming.
    //
    // ⚠ PER ENGINE: one cap per `(venue, AccountLabel)`, measured over its OWN book. Under
    // `vike_config::venue_accounts`' shared-BOOK rule (one ledger, two labels; REPORTED, both start)
    // the ledger can hold a MULTIPLE of the operator's number; `make_engine_accounts`' warning names
    // the ceiling at startup (`docs/decisions/0042-the-account-exposure-ceiling-is-a-policy-key.md`).
    //
    // ⚠ A NARROWING FOLD (a `min`), never an assignment: no-raise is structural, as
    // `vike_model::AccountTier::cap`.
    limits.narrow_account_exposure(policy.and_then(|p| p.max_account_exposure));
    // ---- …and the same ceiling for this one account ----
    // The account rows' own `max_exposure` (the tightest among the ACTIVE rows of
    // `(venue, label)`), through the SAME `min`: the pair composes to `min(box, account)`, either
    // side absent falling through, so an account's figure can only TIGHTEN.
    //
    // ⚠ WHY: the box-wide figure is ONE number applied once per `(venue, AccountLabel)`; ten real
    // books could not get ten limits (the ordinary case, not the shared-BOOK hazard above). The
    // DEFAULT account's rows carry one too, so single-account boxes reach it.
    limits.narrow_account_exposure(account_max_exposure(policy, venue, account));
    // ---- the sizing-equity ceiling ----
    // `policy.max_sizing_equity` caps the equity FIGURE sizing and admission see, applied only by
    // `vike_exec::ExecutionEngine::sizing_equity`, armed only here.
    //
    // ⚠ WHY: under `vike_exec::BalanceMode::Authoritative` equity is `venue wallet + unrealized` for
    // the WHOLE account, adopted every reconcile pass, so a third party funding or draining a shared
    // account moves what this daemon sizes against. Capped, not disputed:
    // `docs/decisions/0048-the-equity-a-strategy-sizes-against-is-capped-not-disputed.md`.
    //
    // ⚠ THE ASYMMETRY: a LOWER figure is conservative for sizing and DESTRUCTIVE for the
    // margin-call sweep, which liquidates on it. So only `ExecutionEngine::sizing_equity` reads it;
    // `resolved_equity` (`vike_core`'s `sweep_margin_call_engine`, every report) is untouched.
    // `None` = `sizing_equity` bit-identical to `resolved_equity`.
    //
    // ⚠ A NARROWING FOLD, never an assignment, as above.
    limits.narrow_sizing_equity(policy.and_then(|p| p.max_sizing_equity));
    // `max_notional_per_order`/`max_total_exposure` have no universal default, so a LIVE start
    // REFUSES TO START without both (`require_live_risk_budget`'s doc). The pre-connect check
    // (`would_go_live`) refuses first; this post-merge site is the BACKSTOP for a future live
    // arm with no probe row. A `paper` account never enters (its engine is `paper_engine`'s).
    require_live_risk_budget(venue, &limits, risk_profile.is_some())?;
    // Effective fee schedule: the account-actual rate the `ReconClient` fetches
    // (binance/bybit/okx/deribit) over `static_default`, fail-soft; tagged on the engine so the
    // snapshot's `fee_schedule` shows the real cost. Paper has `recon == None`, so it resolves to
    // the schedule its `PaperExecutionClient` fills with (paper-parity by construction).
    let fee_schedule = resolve_fee_schedule(venue, recon.as_deref(), static_default);
    tracing::info!(venue, ?fee_schedule, "effective fee schedule");
    // ---- which account did this start just authenticate as? ----
    // ONE site: the seam defaults to nothing (no `fetch_account_identity` = no request; paper has no
    // `ReconClient`). The venue half's `record_authenticated_account` carries the per-venue cost, why
    // it ignores `recon_enabled`, and why a failure only warns.
    //
    // ⚠ **LOAD-BEARING ORDERING: AFTER `resolve_fee_schedule`, NEVER BEFORE.** On binance SPOT the
    // account id and commission rates ride ONE body (`/api/v3/account`), and
    // `crates/bridges/binance/src/family/recon.rs`'s `FamilyReconClient` keeps the id from whichever
    // call fetches first: asking first costs two signed reads instead of zero extra. okx and deribit
    // pay one new read wherever this sits.
    //
    // ⚠ The CEX half of the hyperliquid confirmation below: HL computes its book offline from the
    // key and asks `userRole` in its own start; a CEX store names no account, so this authenticated
    // call is the only one there is (each CEX bridge's declaration, docs/decisions/0096).
    // ONE verb for both halves, in the order the body always had: the authenticated read first, then
    // the venue-CONFIRMED book. Hyperliquid's identity confirmation (decision 0088's B3): its
    // `userRole` probe ran in `crates/bridges/hyperliquid/src/adapter.rs`'s
    // `HyperliquidVenueAdapter` (see its `MasterOutcome`) and arrives with the bind. Only a CONFIRMED
    // answer is recorded, as `record_confirmation`'s `Disagrees` arm writes nothing. A failure of the
    // record is a warning and never fails the start: the session authenticated.
    if let Err(why) = backend.record_identity(
        &IdentityRecord { venue, account, bound_tier, confirmed: identity.as_ref() },
        recon.as_deref(),
    ) {
        tracing::warn!(
            venue,
            account = %account,
            error = %why,
            "{venue}: could not record which account this session authenticated as — the start is \
             unaffected"
        );
    }
    // Contract-value grid; `None` while the value is the default (every venue but deribit and ibkr).
    let contract_values = contract_value_grid(symbol, contract_value);
    // Ruling-margin-mode grid; `None` for `Cross` (all but isolated-only hyperliquid assets).
    let margin_modes = margin_mode_grid(symbol, default_margin_mode);
    let mut engine = vike_exec::ExecutionEngine::new(
        vike_exec::Account::new(1.0, venue, contract_values, vike_exec::BalanceMode::Delta)
            .with_default_margin_modes(margin_modes),
        vike_exec::RiskGate::new(limits),
        client,
        venue,
        symbol,
    );
    engine.fee_schedule = Some(fee_schedule);
    // THE ROUTING IDENTITY: `new` seeds `route_key = venue` (what `account_route_key` renders for
    // `AccountLabel::Default`), so this is the ONLY place the two ever differ. `venue` stays bare: it
    // keys every capability table; decorating it is the `"binance#2"` trap
    // `crates/vike-exec/tests/engine/route_key.rs` gates.
    engine.route_key = route_key;
    // THE ACCOUNT'S TIER: what an order to this engine goes to, since an account runs at its own
    // tier or not at all (decision 0156). The client BOUND it, and the start's tier interlock
    // (`crate::contract`'s `within_the_tier`) refused every outcome bound to any other, so the
    // bound tier IS the account's. `new` seeds `Paper`; a `paper` account's engine is built by
    // `paper_engine` and keeps that seed. (`engine_mode`, which derived a second answer from a
    // `live` bool and the bound tier while a paper client could stand in, is gone with it.)
    engine.tier = bound_tier;
    Ok((engine, recon))
}

#[path = "assemble_tests.rs"]
#[cfg(test)]
mod assemble_tests;
