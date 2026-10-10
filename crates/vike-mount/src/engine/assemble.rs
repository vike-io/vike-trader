//! The shared tail: the one shape every mount's parts take, and the fold that makes them an engine.

use std::collections::HashSet;

use vike_model::accounts::account_keys::AccountLabel;

use crate::{
    EngineAndRecon, MountError, MountPolicy, arm_universal_defaults, arming, book_identity,
    margin_mode_grid, merge_operator_budget, multiplier_grid, report_halt_admit_armed,
    require_live_risk_budget, resolve_fee_schedule, symbol_grid,
};

/// What a venue's mount produced, before the shared tail folds it: the ONE shape the contract path
/// and the paper client both produce, so the tail cannot drift between them.
pub(crate) struct MountParts {
    pub(crate) client: Box<dyn vike_exec::ExecutionClient + Send>,
    pub(crate) recon: Option<Box<dyn vike_exec::recon::ReconClient>>,
    pub(crate) limits: vike_model::RiskLimits,
    pub(crate) contract_size: f64,
    pub(crate) default_margin_mode: vike_model::MarginMode,
    pub(crate) symbol_grids: indexmap::IndexMap<String, vike_model::SymbolGrid>,
    pub(crate) grid_source: vike_bridge_core::venue_mount::DeclaredGridSource,
    /// The tier `record_authenticated_account` addresses; `None` asks nothing.
    pub(crate) record_tier: Option<vike_config::VenueMode>,
    /// `(book, evidence, tier)` the venue itself named (hyperliquid's `userRole`).
    pub(crate) identity: Option<(String, &'static str, vike_config::VenueMode)>,
    pub(crate) live: bool,
    pub(crate) static_default: vike_model::FeeSchedule,
}

/// The [`vike_exec::EngineMode`] a mount outcome publishes. `live`: a venue client was built and
/// armed; `tier`: the tier it authenticated (`record_authenticated_account`'s). A live client with
/// no recorded tier reads as real money (chip LIVE, one-click off): the cheaper wrong answer.
pub(crate) fn engine_mode(
    live: bool,
    tier: Option<vike_config::VenueMode>,
) -> vike_exec::EngineMode {
    use vike_config::VenueMode;
    use vike_exec::EngineMode;
    match (live, tier) {
        (false, _) => EngineMode::Paper,
        (true, Some(VenueMode::Demo)) => EngineMode::Demo,
        (true, _) => EngineMode::Live,
    }
}

/// **THE SHARED TAIL** every venue's mount parts become an engine through (docs/decisions/0096),
/// in order: `live_venues` gains the route key on a LIVE mount, the halt-admit ARMED report, the
/// declared-leg grid, the operator budget, the universal defaults, the exposure and equity
/// ceilings, the live-budget backstop, the fee schedule, the identity record, the engine. Called
/// from [`make_engine_for_account`].
pub(super) fn assemble_engine(
    venue: &str,
    symbol: &str,
    account: &AccountLabel,
    declared_legs: &[String],
    parts: MountParts,
    live_venues: &mut HashSet<String>,
    route_key: String,
    halt_admit: vike_model::HaltAdmit,
    risk_profile: Option<&vike_model::ProfileRisk>,
    policy: Option<&MountPolicy>,
) -> Result<EngineAndRecon, MountError> {
    let MountParts {
        client,
        recon,
        mut limits,
        contract_size,
        default_margin_mode,
        symbol_grids,
        grid_source,
        record_tier,
        identity,
        live,
        static_default,
    } = parts;
    if live {
        live_venues.insert(route_key.clone());
    }
    // ARMED/NOT-ARMED half of the halt-admit report: HERE is the first point the answer is known
    // (`report_halt_admit` runs before credentials and handshakes, which can land a venue on PAPER).
    // Silent but on cTrader (`vike_model::effective_halt_admit` degrades `verify` to `admit`).
    report_halt_admit_armed(venue, halt_admit, live);
    // The DECLARED-LEG grid, at ONE site: after every arm's wholesale `RiskLimits::from_properties`
    // (which would discard it). Before the operator merge only for readability — both merge paths
    // (`ProfileRisk::apply_to`, `apply_operator_budget_only`) carry `grid_by_symbol` verbatim.
    // Empty (no leg, or not `InHand` per `declared_grid_source`) is the identity:
    // `RiskLimits::grid_for` returns the scalars and `skip_serializing_if` keeps
    // `vike_exec::state_hash` and journals unchanged.
    limits.grid_by_symbol = symbol_grids;
    // …and SAY so when a declared leg got none: it is judged on the MOUNTED symbol's tick, lot and
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
    limits = merge_operator_budget(venue, limits, risk_profile);
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
    // `vike_config::venue_accounts`' shared-BOOK rule (one ledger, two labels; REPORTED, both mount)
    // the ledger can hold a MULTIPLE of the operator's number; `make_engine_accounts`' warning names
    // the ceiling at startup (`docs/decisions/0042-the-account-exposure-ceiling-is-a-policy-key.md`).
    //
    // ⚠ A NARROWING FOLD (a `min`), never an assignment: no-raise is structural, as
    // `vike_config::VenueMode::cap`.
    limits.narrow_account_exposure(policy.and_then(|p| p.max_account_exposure));
    // ---- …and the same ceiling for this one account ----
    // `policy.account_exposure.<venue>.<LABEL>` through the SAME `min`: the pair composes to
    // `min(box, account)`, either side absent falling through, so an account line can only TIGHTEN.
    //
    // ⚠ WHY: the box-wide figure is ONE number applied once per `(venue, AccountLabel)`; ten real
    // books could not get ten limits (the ordinary case, not the shared-BOOK hazard above).
    //
    // ⚠ The DEFAULT account can be named here, unlike the `[accounts]` mode table
    // (`vike_config::VenuePolicy::account_exposure`'s doc), so single-account boxes reach it too.
    limits.narrow_account_exposure(policy.and_then(|p| p.venues.account_exposure(venue, account)));
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
    // `max_notional_per_order`/`max_total_exposure` have no universal default, so a LIVE mount
    // REFUSES TO START without both (`require_live_risk_budget`'s doc). The pre-connect check
    // (`would_mount_live`) refuses first; this post-merge site is the BACKSTOP for a future live
    // arm with no probe row. Paper never enters (`live_venues` gains only genuinely live arms).
    if live_venues.contains(&route_key) {
        require_live_risk_budget(venue, &limits, risk_profile.is_some())?;
    }
    // Effective fee schedule: the account-actual rate the `ReconClient` fetches
    // (binance/bybit/okx/deribit) over `static_default`, fail-soft; tagged on the engine so the
    // snapshot's `fee_schedule` shows the real cost. Paper has `recon == None`, so it resolves to
    // the schedule its `PaperExecutionClient` fills with (paper-parity by construction).
    let fee_schedule = resolve_fee_schedule(venue, recon.as_deref(), static_default);
    tracing::info!(venue, ?fee_schedule, "effective fee schedule");
    // ---- which account did this mount just authenticate as? ----
    // ONE site: the seam defaults to nothing (no `fetch_account_identity` = no request; paper has no
    // `ReconClient`). `book_identity::record_authenticated_account` carries the per-venue cost, why
    // it ignores `recon_enabled`, and why a failure only warns.
    //
    // ⚠ **LOAD-BEARING ORDERING: AFTER `resolve_fee_schedule`, NEVER BEFORE.** On binance SPOT the
    // account id and commission rates ride ONE body (`/api/v3/account`), and
    // `crates/bridges/binance/src/family/recon.rs`'s `FamilyReconClient` keeps the id from whichever
    // call fetches first: asking first costs two signed reads instead of zero extra. okx and deribit
    // pay one new read wherever this sits.
    //
    // ⚠ The CEX half of the hyperliquid confirmation below: HL computes its book offline from the
    // key and asks `userRole` in its own mount; a CEX store names no account, so this authenticated
    // call is the only one there is (each CEX bridge's declaration, docs/decisions/0096).
    if let Some(tier) = record_tier {
        book_identity::record_authenticated_account(
            venue,
            account,
            tier,
            recon.as_deref(),
            arming::directory_of(policy),
        );
    }
    // Hyperliquid's identity confirmation (decision 0088's B3): its `userRole` probe ran in
    // `crates/bridges/hyperliquid/src/mount.rs`'s `HyperliquidVenueMount` (see its `MasterOutcome`)
    // and arrives as the contract's `IdentityReport` via `contract`'s `parts_from_outcome`. Only a
    // CONFIRMED answer is recorded, as `record_confirmation`'s `Disagrees` arm writes nothing.
    if let Some((book, evidence, tier)) = identity {
        book_identity::record_confirmation(
            venue,
            evidence,
            account,
            tier,
            &book,
            arming::directory_of(policy),
        );
    }
    // Contract-multiplier grid; `None` when the venue reported nothing (every non-deribit venue).
    let multipliers = multiplier_grid(symbol, contract_size);
    // Ruling-margin-mode grid; `None` for `Cross` (all but isolated-only hyperliquid assets).
    let margin_modes = margin_mode_grid(symbol, default_margin_mode);
    let mut engine = vike_exec::ExecutionEngine::new(
        vike_exec::Account::new(1.0, venue, multipliers, vike_exec::BalanceMode::Delta)
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
    // What stands behind the orders. The ceiling's early return (`paper_engine`) keeps `new`'s
    // PAPER seed; a paper OUTCOME (no venue client built) arrives `live == false` → PAPER.
    engine.mode = engine_mode(live, record_tier);
    Ok((engine, recon))
}
