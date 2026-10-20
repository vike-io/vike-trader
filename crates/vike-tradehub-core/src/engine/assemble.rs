//! The shared tail: the one shape every start's parts take, and the fold that makes them an engine.

use std::collections::HashSet;

use vike_model::accounts::account_keys::AccountLabel;

use crate::symbol_grid::GridSource;
use crate::{
    AdapterBind, AdapterPolicy, ConfirmedBook, EngineAndRecon, EngineError, IdentityRecord,
    LeverageVerdict, NotRunning, VenueBackend, account_leverage, account_max_exposure,
    account_start_leverage, arm_universal_defaults, contract_value_grid, judge_account_leverage,
    live_twin_must_hold_its_leverage, live_twin_not_started, margin_mode_grid,
    merge_operator_budget, report_halt_admit_armed, require_live_risk_budget, resolve_fee_schedule,
    symbol_grid,
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
    /// The venue's confirmation of the account's stated leverage (`AdapterBind::leverage_cap`).
    pub(crate) leverage_cap: Option<Result<f64, String>>,
    /// The currency the venue says the account's money is in (`AdapterBind::account_ccy`).
    pub(crate) account_ccy: Option<String>,
    /// Whether that currency is an ASSUMPTION (`AdapterBind::account_ccy_assumed`).
    pub(crate) account_ccy_assumed: bool,
}

/// **What the shared tail tells the engine about the account's currency, and how the currency is known**
/// (decision 0183, item 1, and the owner's FXCM ruling of 2026-10-19: "check the currency by API; if you can't,
/// USD"). One pure function so the four cases that matter are one test: the venue's word is used as stated; an
/// ASSUMED currency is used the same way but travels flagged and is said at `warn`; an account that does not
/// convert (a live or paper tier, no account-wide ceiling, a venue with no rate source) is told NOTHING
/// whichever way the currency is known; and no currency is no anchor.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LedgerAnchor<'a> {
    /// The venue stated it.
    Stated(&'a str),
    /// The venue could not, and the lane filled in the ruling's currency.
    Assumed(&'a str),
    /// The engine keeps the label it has, and why.
    NotTold(NotTold),
}

/// Why the engine is not told a currency.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NotTold {
    /// The account may not hold a symbol quoted in another currency (its tier, its ceiling or its venue
    /// forbids it), so a currency would only relabel an engine that has run under `"USDT"`.
    AccountDoesNotConvert,
    /// The backend stated none.
    NoCurrency,
}

/// See [`LedgerAnchor`].
pub(crate) fn ledger_anchor(
    converts_quotes: bool,
    ccy: Option<&str>,
    assumed: bool,
) -> LedgerAnchor<'_> {
    if !converts_quotes {
        return LedgerAnchor::NotTold(NotTold::AccountDoesNotConvert);
    }
    match ccy.map(str::trim).filter(|c| !c.is_empty()) {
        None => LedgerAnchor::NotTold(NotTold::NoCurrency),
        Some(c) if assumed => LedgerAnchor::Assumed(c),
        Some(c) => LedgerAnchor::Stated(c),
    }
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
        leverage_cap,
        account_ccy,
        account_ccy_assumed,
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
        leverage_cap,
        account_ccy,
        account_ccy_assumed,
    }
}

/// **A start that came up and was then REFUSED by the shared tail gives its lease back.** The bound client is
/// told to `begin_detach` then `detach` (the two phases every other refusal path of a started lease takes,
/// `RemoteBackend::start_adapter`'s project/tier/contract-value refusals among them), which ends the lease
/// at the gatewayhub (its mainnet bridge, its user-data stream, the account's live lock), and the backend is
/// told to forget what it kept for the start ([`VenueBackend::release_refused_start`]: its lease table, the
/// door's lease facts, the worker that would re-start the adapter after a gatewayhub restart). Dropping the
/// client alone released none of it: the backend holds more clones of the session than the client does.
fn release_refused(
    backend: &dyn VenueBackend,
    venue: &str,
    account: &AccountLabel,
    route_key: &str,
    client: &mut (dyn vike_exec::ExecutionClient + Send),
) {
    client.begin_detach();
    client.detach();
    backend.release_refused_start(venue, account, route_key);
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
        mut client,
        recon,
        mut limits,
        contract_value,
        default_margin_mode,
        symbol_grids,
        grid_source,
        bound_tier,
        identity,
        static_default,
        leverage_cap,
        account_ccy,
        account_ccy_assumed,
    } = parts;
    // ---- THIS ACCOUNT'S OWN LEVERAGE, judged FIRST (decision 0181) ----
    // The account row's `leverage`, and on a venue that POSTS a leverage (decision 0182) the whole number
    // the start asked the venue to hold (`account_start_leverage`, the same inputs the start query used):
    // the verdict is applied to the margin rate below, but a MAINNET LANE TWIN that the venue did not
    // confirm is refused HERE, before its route key joins the live record (`live_venues`) or any engine
    // exists. ⚠ The demo rule ("not applied, the engine margins at 1x and trades") would leave the real
    // account on a leverage nobody chose.
    let stated = account_leverage(policy, venue, account);
    let posted = account_start_leverage(policy, venue, account, bound_tier, risk_profile);
    let leverage_verdict =
        judge_account_leverage(venue, bound_tier, stated, posted, leverage_cap.as_ref());
    if let LeverageVerdict::Refused(why) = &leverage_verdict
        && live_twin_must_hold_its_leverage(policy, venue, account, bound_tier)
    {
        tracing::error!(
            venue,
            account = %account,
            "{}",
            live_twin_not_started(
                venue,
                &route_key,
                &format!("the venue did not confirm the account's stated leverage: {why}")
            )
        );
        release_refused(backend, venue, account, &route_key, &mut *client);
        return Err(EngineError::NotRunning(NotRunning {
            venue: venue.to_string(),
            account: account.clone(),
            tier: Some(bound_tier),
            block: vike_config::ArmingBlock::AdapterFailed,
        }));
    }
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
    let profile_im = limits.im_requirement;
    limits.im_requirement = limits.im_requirement.or(Some(1.0));
    // ---- THIS ACCOUNT'S OWN LEVERAGE (decision 0181) ----
    // The account row's `leverage`, applied to THIS engine's margin rate only, and only when the VENUE
    // confirmed it (`judge_account_leverage`, asked at the top): a refusal leaves the 1x above, said at
    // error level (a MAINNET lane twin was already refused there instead).
    // It can never loosen what was already armed: the box-wide `policy.max_leverage` ceiling and a
    // run profile's own `[risk] max_leverage` (`profile_im`) both cap it, so the figure is the lowest
    // of the three. It never touches an exposure ceiling (the two `narrow_account_exposure` folds below
    // still bound gross notional) and no other account's engine: `limits` is this account's alone.
    match leverage_verdict {
        LeverageVerdict::Unstated => {}
        LeverageVerdict::Refused(why) => {
            tracing::error!(
                venue,
                account = %account,
                "{venue}: the account's stated leverage is NOT applied and the engine margins at \
                 1x: {why}"
            );
        }
        LeverageVerdict::Applied { leverage } => {
            let ceiling = policy.and_then(|p| p.max_leverage).unwrap_or(f64::INFINITY);
            let profile = profile_im.filter(|im| *im > 0.0).map_or(f64::INFINITY, |im| 1.0 / im);
            let applied = leverage.min(ceiling).min(profile).max(1.0);
            limits.im_requirement = Some(1.0 / applied);
            tracing::info!(
                venue,
                account = %account,
                requested = leverage,
                applied,
                "{venue}: the account's leverage is applied to its engine's margin rate (the \
                 venue confirmed it)"
            );
        }
    }
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
    // ⚠ A LANE TWIN (`vike_secrets::lane_twin`) is a SECOND engine over the parent's row and copies its
    // `max_exposure`; the cap is armed per ENGINE, so the spot engine and the twin each carry the figure and
    // the account's combined bound is TWICE it. Said once at the twin's start so the 2x is visible; the
    // numbers are not changed.
    //
    // Judged by `AccountDirectory::is_derived_lane_twin`, never the label (an operator's own account that
    // spelled `PERP` is no twin), once: it also decides the mark floor's arming below.
    let derived_lane_twin = account
        .text()
        .is_some_and(|label| policy.is_some_and(|p| p.accounts.is_derived_lane_twin(venue, label)));
    if derived_lane_twin {
        let cap = limits.max_account_exposure.map_or_else(|| "none".to_string(), |c| c.to_string());
        tracing::warn!(
            venue,
            account = %account,
            "per-engine caps: the {venue} spot engine and its perp lane twin `{venue}#{account}` each \
             have the account exposure cap {cap}; the account's combined bound is 2x that, because \
             the cap is armed per engine and the twin copies the parent account's"
        );
    }
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
    // FAIL CLOSED on symbols, for the venues whose adapter signs by the order's symbol: an order on a
    // symbol this engine was never told about (no `AdmitSymbol`, or a refused one) is DENIED, not
    // rounded onto the mounted symbol's grid and signed (decision 0173 item 7, made per venue).
    limits.deny_unadmitted_symbols =
        policy.is_some_and(|p| p.fail_closed_symbol_venues.iter().any(|v| v == venue));
    // `max_notional_per_order`/`max_total_exposure` have no universal default, so a LIVE start
    // REFUSES TO START without both (`require_live_risk_budget`'s doc). The pre-connect check
    // (`would_go_live`) refuses first; this post-merge site is the BACKSTOP for a future live
    // arm with no probe row. A `paper` account never enters (its engine is `paper_engine`'s).
    if let Err(refusal) = require_live_risk_budget(venue, &limits, risk_profile.is_some()) {
        // The same release as the leverage refusal above: this start came up, then was refused.
        release_refused(backend, venue, account, &route_key, &mut *client);
        return Err(refusal);
    }
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
    // Whether this engine may hold a symbol quoted in another currency than the account's (decision 0183,
    // phase 1): the root named the venue (`quote_converting_venues`), and the tier the client BOUND and the
    // ceiling the folds above left say the account may.
    let converts_quotes = policy
        .is_some_and(|p| p.quote_converting_venues.iter().any(|v| v == venue))
        && crate::quote_conversion_tier_refusal(
            Some(bound_tier),
            limits.max_account_exposure.is_some_and(|cap| cap > 0.0),
        )
        .is_none();
    let mut engine = vike_exec::ExecutionEngine::new(
        vike_exec::Account::new(1.0, venue, contract_values, vike_exec::BalanceMode::Delta)
            .with_default_margin_modes(margin_modes),
        vike_exec::RiskGate::new(limits),
        client,
        venue,
        symbol,
    );
    engine.fee_schedule = Some(fee_schedule);
    // THE ACCOUNT'S CURRENCY (decision 0183, item 1): `quote_asset` is `"USDT"` in the constructor and
    // nothing else sets it, so the ledger's currency was a label no venue ever agreed to. For an account
    // that converts a quote currency it is anchored to the one the VENUE states, because every rate the
    // engine is later handed is expressed in it. ⚠ ONLY for such an account, on purpose: the journal
    // restore compares a default account's journaled `quote_asset` with the engine's
    // (`vike-core`'s `apply_restored_engines`), so relabelling an engine that has run under `"USDT"` skips
    // the restore of its journaled state once (it is recorded in the next snapshot). No account is
    // relabelled unless it already opted into the conversion by its tier and its ceiling. The engine is
    // TOLD through `set_account_ccy` (not by writing `quote_asset`): that records the statement, and
    // without it a foreign-quoted admission is refused as `account-ccy-unset`.
    // ⚠ A currency the venue could not state and the lane ASSUMED (FXCM: USD, the owner's ruling) is anchored
    // exactly like a stated one, for the same accounts, and SAID at `warn`: every rate and every PnL of a
    // foreign-quoted symbol is expressed in it, so the operator must be told it is a guess to verify.
    //
    // ⚠ NOT when the engine's own MOUNTED symbol is quoted in another money than the one the venue states
    // (`unanchorable_mount`): the engine books its mounted symbol at par, so a ledger labelled with the
    // stated currency would count that symbol's money (USD for EURUSD) as the stated one (EUR) while
    // converting every other symbol into it, one book in two currencies. The engine is left untold, which
    // keeps the label the account has always had and refuses every foreign-quoted admission
    // (`account-ccy-unset`). The fence judges a stated and an assumed currency alike.
    let told = match ledger_anchor(converts_quotes, account_ccy.as_deref(), account_ccy_assumed) {
        LedgerAnchor::Stated(ccy) => Some((ccy, false)),
        LedgerAnchor::Assumed(ccy) => Some((ccy, true)),
        LedgerAnchor::NotTold(NotTold::AccountDoesNotConvert) if account_ccy_assumed => {
            tracing::info!(
                venue,
                account = %account,
                "{venue}: the account's currency is only ASSUMED and this account does not convert a \
                 quote currency (a `demo` account with policy.max_account_exposure set does), so the \
                 ledger keeps the label it has and a symbol quoted in another currency stays refused"
            );
            None
        }
        LedgerAnchor::NotTold(_) => None,
    }
    .filter(|(ccy, _)| {
        unanchorable_mount(venue, symbol, ccy)
            .inspect(|why| {
                tracing::warn!(
                    venue,
                    account = %account,
                    currency = *ccy,
                    mounted = symbol,
                    "{venue}: the ledger is NOT anchored to the account's currency ({ccy}): {why}. A symbol \
                     quoted in another currency stays refused"
                );
            })
            .is_none()
    });
    if let Some((ccy, assumed)) = told {
        match engine.set_account_ccy(ccy) {
            // The ONE line of an assumption, whoever assumed it: fxcm's lane, or the node's USD default for a
            // converting venue whose API stated none (owner ruling 2026-10-19: the API's word, else USD).
            Ok(()) if assumed => tracing::warn!(
                venue,
                account = %account,
                currency = %engine.quote_asset,
                "{venue}: account currency not stated by {venue}: assuming {} (account {account}). The \
                 ledger is anchored to it, an ASSUMED account currency (the venue did not state one): \
                 verify it against the account's own statement before trusting any PnL of a symbol \
                 quoted in another currency",
                engine.quote_asset
            ),
            Ok(()) => tracing::info!(
                venue,
                account = %account,
                currency = %engine.quote_asset,
                "{venue}: the ledger is anchored to the account's own currency ({}), which converts a \
                 symbol quoted in another one",
                engine.quote_asset
            ),
            Err(why) => tracing::warn!(
                venue,
                account = %account,
                currency = ccy,
                "{venue}: the ledger could not be anchored to the account's currency ({why}); a \
                 symbol quoted in another currency stays refused"
            ),
        }
    }
    // THE ROUTING IDENTITY: `new` seeds `route_key = venue` (what `account_route_key` renders for
    // `AccountLabel::Default`), so this is the ONLY place the two ever differ. `venue` stays bare: it
    // keys every capability table; decorating it is the `"binance#2"` trap
    // `crates/vike-exec/tests/engine/route_key.rs` gates.
    engine.route_key = route_key;
    // THE ACCOUNT'S TIER: what an order to this engine goes to, since an account runs at its own
    // tier or not at all (decision 0156). The client BOUND it, and the start's tier interlock
    // (`crate::contract`'s `within_the_tier`) refused every outcome bound to any other, so the
    // bound tier IS the account's. `new` seeds `Paper`; a `paper` account's engine is built by
    // `paper_engine`, which TELLS it so. (`engine_mode`, which derived a second answer from a
    // `live` bool and the bound tier while a paper client could stand in, is gone with it.)
    // `set_tier`, not an assignment: the journal's snapshot carries only a tier that was told (or is
    // not `Paper`), and a boot restore refuses a snapshot of another tier's book.
    engine.set_tier(bound_tier);
    // THE MARK FLOOR (`vike_exec::RiskGate::require_mark`): an opening order on this engine's MOUNTED
    // symbol is denied `no-mark-for-mounted-symbol` while the engine holds no mark for it. Armed for the
    // DERIVED LANE TWIN engine only (`derived_lane_twin` above: `binance#PERP`), because its mounted
    // symbol gets no tick from the venue's quote route and the node's mark lane is the only price it has.
    // ⚠ NOT armed for the venue's other engines, the DEFAULT spot engine included (at every tier,
    // mainnet `live` too): that route prices them, and a floor on them would hold every opening order,
    // a limit one included, while a price lane is down or a market is quiet. Every engine reaching this
    // tail is `demo` or `live` (a `paper` one is built by `paper_engine`).
    engine.gate.require_mark = derived_lane_twin;
    Ok((engine, recon))
}

/// **The money a mounted symbol's NAME says it is quoted in**, or `None` when the name says none: exactly six
/// ASCII letters are a currency pair, quoted in the last three (`EURUSD` -> `USD`), and OANDA's own spelling
/// `BASE_QUOTE` is quoted in the part after the underscore (`EUR_USD`, `SPX500_USD` -> `USD`); and an EPIC (a
/// dotted name of three or more tokens, `CS.D.USDJPY.MINI.IP`) is quoted in the quote of the pair token inside it
/// (`JPY`), read positively by `vike_catalog::fiat_quote_of` (a token that is two fiat codes), so `IX.D.DAX.DAILY.IP`
/// and `CS.D.BITCOIN.CFD.IP` name none, and an equity's `AAPL.SMART.USD` has no such token. Anything else (a slash, a
/// digit-only quote, an equity) is not derived: a guess here would label a ledger.
fn mount_money(symbol: &str) -> Option<String> {
    let name = symbol.trim().to_ascii_uppercase();
    if name.split('.').count() >= 3 {
        return name
            .split('.')
            .filter(|token| token.len() == 6 && token.bytes().all(|b| b.is_ascii_alphabetic()))
            .find_map(vike_catalog::fiat_quote_of)
            .map(str::to_string);
    }
    let quote = match name.split_once('_') {
        Some((base, quote))
            if !base.is_empty() && base.bytes().all(|b| b.is_ascii_alphanumeric()) =>
        {
            quote
        }
        None if name.len() == 6 && name.bytes().all(|b| b.is_ascii_alphabetic()) => &name[3..],
        _ => return None,
    };
    (quote.len() == 3 && quote.bytes().all(|b| b.is_ascii_alphabetic())).then(|| quote.to_string())
}

/// **Why the ledger of the engine mounted on `symbol` of `venue` must not be anchored to `ccy`** (the currency
/// the venue states for the account), or `None` to anchor it (decision 0183, item 1).
///
/// The engine books its MOUNTED symbol at par (it knows no rate for the symbol it was started on), which is the
/// account's money only when the symbol is quoted in it. So for a venue whose mounts are named by their money
/// (its wired symbol says one, `mount_money`: oanda, ctrader, dukascopy, fxcm, and ig by the pair inside
/// its epic) the anchor is allowed only when the mount's quote currency IS `ccy`; a mount whose name says none
/// is refused too, because unknown is never the account's currency. ig's own fence (`ig_account_refusal`) only
/// reads the ACCOUNT's currency, never the money of the epic it is mounted on, so this rule is its only judge
/// of the mount. A venue whose wired symbol names no money (ibkr: `AAPL.SMART.USD`) is not judged here: its own
/// fence stands in the root's door (`unanchorable_mount` there), and this name rule would refuse every one of
/// its mounts.
///
/// PUBLIC because the root's door asks the SAME question before it anchors a lease (one decision, two callers:
/// the engine is never told a currency the door has anchored, and the door never admits on one the engine
/// refused).
#[must_use]
pub fn unanchorable_mount(venue: &str, symbol: &str, ccy: &str) -> Option<String> {
    vike_venues::venue_wiring::wired_symbol(venue).and_then(mount_money)?;
    match mount_money(symbol) {
        Some(money) if money.eq_ignore_ascii_case(ccy) => None,
        Some(money) => Some(format!(
            "the engine is mounted on `{symbol}`, quoted in {money}, which its ledger books at par, and the \
             account's money is {ccy}: the ledger is not {ccy}"
        )),
        None => Some(format!(
            "the engine is mounted on `{symbol}`, whose name does not say what it is quoted in, so the \
             money its ledger books at par is not known to be {ccy}"
        )),
    }
}

#[path = "assemble_tests.rs"]
#[cfg(test)]
mod assemble_tests;

#[path = "leverage_tests.rs"]
#[cfg(test)]
mod leverage_tests;
