//! **The generic fold over a contract row** — what `make_engine_for_account` and the arming
//! projection do with a `VenueRow::Adapter`, with no venue named here.
//! `docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, DeclaredGridSource, ExecOutcome, HeldBelowLive,
    LiveExec, PaperCause, ProcessFacts, Resolution, VenueAdapter,
};
use vike_config::ArmingBlock;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::venue_setting::VenueSettings;

use crate::{AdapterParts, AdapterPolicy, EngineError, NotRunning, VenueRow};

/// The view a venue with no `venue_setting` rows gets.
static EMPTY_SETTINGS: LazyLock<VenueSettings> = LazyLock::new(VenueSettings::default);

/// This venue's settings out of the snapshot the composition root read.
pub(crate) fn settings_of<'a>(policy: Option<&'a AdapterPolicy>, venue: &str) -> &'a VenueSettings {
    policy.and_then(|p| p.venue_settings.get(venue)).unwrap_or(&*EMPTY_SETTINGS)
}

/// The state and bin directories from the boot's declaration, handed to a bridge as
/// `AdapterInputs::process` (e.g. `crates/bridges/dukascopy/src/adapter.rs`'s `DukascopyVenueAdapter`,
/// `crates/bridges/ctrader/src/adapter.rs`'s `CtraderVenueAdapter`).
///
/// **The PROBE's view: `halt_path` is EMPTY on purpose.** The arming probe, clock canary and
/// preflight start no client; resolving the sentinel here would run (memoize, and log `HALT
/// sentinel …` from) the process-wide resolver in a root like `vike-cli` that declared no project.
/// [`adapter_process_facts`] adds it for the one caller that builds a client.
pub(crate) fn process_facts() -> ProcessFacts {
    let state_dir = crate::halt::declared_project_state_dir();
    let bin_dir = state_dir.as_deref().and_then(|state| {
        // `<project>/settings/state` -> `<project>/bin`
        state
            .parent()
            .and_then(Path::parent)
            .map(|project| project.join(vike_paths::state_path::PROJECT_BIN_DIR))
    });
    ProcessFacts { state_dir, bin_dir, halt_path: PathBuf::new() }
}

/// [`process_facts`] for a START: the same directories, plus the HALT sentinel's path.
///
/// **The sentinel's path is resolved HERE, once, by the process-wide resolver**
/// (`vike_tradehub_core::halt::halt_path_from_env`) and handed to every live bridge (decision 0099):
/// the paper books and the daemon's advisory ask the same resolver, so live venues, paper books and
/// the file the operator is told to `touch` cannot name three files. It also logs the arming report
/// (`HALT sentinel path resolved`, or `NOT ARMABLE`) when the first venue starts.
///
/// Formerly `mount_process_facts`, renamed in the shape-and-names change (decision 0166).
pub(crate) fn adapter_process_facts() -> ProcessFacts {
    ProcessFacts { halt_path: crate::halt::halt_path_from_env(), ..process_facts() }
}

fn paper_block(cause: PaperCause) -> ArmingBlock {
    match cause {
        PaperCause::NoCredentials => ArmingBlock::NoCredentials,
        PaperCause::LiveCredentialsAbsent => ArmingBlock::LiveCredentialsAbsent,
        PaperCause::LiveTierNotWired => ArmingBlock::LiveTierNotWired,
        PaperCause::ExecFlagUnset => ArmingBlock::ExecFlagUnset,
        PaperCause::LiveOnlyArm => ArmingBlock::LiveOnlyArm,
        PaperCause::SdkAbsent => ArmingBlock::SdkAbsent,
        PaperCause::AccountNotInStore => ArmingBlock::AccountNotInStore,
        PaperCause::NoLiveArm => ArmingBlock::NoLiveArm,
    }
}

/// A resolution as an arming row, under an account whose `tier` is `live` or `demo`: `(Some(t),
/// block)` when the bridge would arm it, `(None, why)` when the account does NOT RUN.
///
/// # ⚠ THE NO-DOWNGRADE RULE: a `live` account never trades demo
///
/// The tier a bridge binds must EQUAL the account's tier. Under a `live` tier a bridge may still
/// answer a DEMO arming — aster falls back to its testnet keys
/// (`HeldBelowLive::LiveCredentialsAbsent`), a demo-only arm (ibkr, deribit, dukascopy, …) answers
/// `HeldBelowLive::DemoOnlyArm` — and that answer is NOT RUNNING here,
/// `ArmingBlock::LiveCredentialsAbsent`: a live account whose usable live key set is absent,
/// whatever the bridge's reason for reaching only demo. The operator said `live`; a demo session
/// would trade a different account under the live account's name. To trade demo, the account's
/// tier says `demo`.
///
/// The other direction — a bridge answering LIVE under a `demo` tier — is NOT folded here: the
/// projection shows the bridge's wrong answer, and [`contract_parts`]' interlock refuses it.
///
/// # A held arm: bound, and sending nothing
///
/// `held_below_live: Some(HeldBelowLive::OrdersHeld)` is honoured at the tier the bridge binds:
/// the row is that tier with [`ArmingBlock::OrdersHeld`], so the account starts (a live arm binds
/// live, under the live risk budget like any other) and [`contract_parts`] wraps its client in
/// [`crate::orders_held::OrdersHeldClient`], which refuses every order before the bridge sees it
/// (`docs/decisions/0140-ibkr-rides-one-session-one-product-one-tier.md`, stage A). It does not
/// lift the no-downgrade rule: a held DEMO arming under a `live` tier still does not run. The two
/// demo-arm reasons on a LIVE arming are a bridge's incoherent answer and fold as they always did,
/// to a clear live row.
pub(crate) fn resolution_to_arming(
    resolution: Resolution,
    tier: AccountTier,
) -> (Option<AccountTier>, ArmingBlock) {
    let held = Some(HeldBelowLive::OrdersHeld);
    match resolution {
        Resolution::Paper(cause) => (None, paper_block(cause)),
        Resolution::Armed { tier: AccountTier::Live, held_below_live }
            if held_below_live == held =>
        {
            (Some(AccountTier::Live), ArmingBlock::OrdersHeld)
        }
        Resolution::Armed { tier: AccountTier::Live, .. } => {
            (Some(AccountTier::Live), ArmingBlock::None)
        }
        Resolution::Armed { tier: AccountTier::Demo, .. } if tier == AccountTier::Live => {
            (None, ArmingBlock::LiveCredentialsAbsent)
        }
        Resolution::Armed { tier: AccountTier::Demo, held_below_live }
            if held_below_live == held =>
        {
            (Some(AccountTier::Demo), ArmingBlock::OrdersHeld)
        }
        Resolution::Armed { tier: AccountTier::Demo, .. } => {
            (Some(AccountTier::Demo), ArmingBlock::None)
        }
        // A bridge that declines answers `Resolution::Paper` with its cause; "armed at paper" is
        // no arming at all, and nothing is built for it.
        Resolution::Armed { tier: AccountTier::Paper, .. } => (None, ArmingBlock::NoLiveArm),
    }
}

/// The inputs a pre-assembly question is asked with — the arming probe, the credential probe, the
/// start. `process` is borrowed from the caller so the struct outlives no temporary.
pub(crate) fn inputs_for<'a>(
    row: &dyn VenueAdapter,
    label: &'a AccountLabel,
    vars: &'a HashMap<String, String>,
    tier: AccountTier,
    policy: Option<&'a AdapterPolicy>,
    process: &'a ProcessFacts,
) -> AdapterInputs<'a> {
    AdapterInputs {
        account: label,
        // The ONE active `account` row of `(venue, label)` at `tier`, and its credentials by field
        // (decision 0156 item 6): the bridge asks for a field and never composes a name.
        credentials: crate::arming::directory_of(policy).credentials_for(row.venue(), label, tier),
        secrets: vars,
        settings: settings_of(policy, row.venue()),
        tier,
        accounts: crate::arming::directory_of(policy),
        process,
    }
}

/// The arming probe for a contract row at the account's `tier`: the two generic preconditions,
/// then the bridge's `resolve`, folded by [`resolution_to_arming`] (the no-downgrade rule).
/// `credentials` is the account's key set when the caller SUPPLIES it (a policy-less probe), or
/// `None` to resolve it from `policy`'s account directory, as the start does.
pub(crate) fn contract_arming(
    row: &dyn VenueAdapter,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    credentials: Option<vike_secrets::account_credentials::AccountCredentials<'_>>,
    tier: AccountTier,
    policy: Option<&AdapterPolicy>,
) -> (Option<AccountTier>, ArmingBlock) {
    // FIRST, before the tier: a labelled account of a venue that cannot address one is refused
    // whatever its row says.
    if !label.is_default() && !row.declaration().addresses_accounts {
        return (None, ArmingBlock::NoAccountSupport);
    }
    if tier == AccountTier::Paper {
        return crate::arming::PAPER_RUNS;
    }
    let process = process_facts();
    let mut inputs = inputs_for(row, label, vars, tier, policy, &process);
    if let Some(credentials) = credentials {
        inputs.credentials = credentials;
    }
    resolution_to_arming(row.resolve(&inputs), tier)
}

/// **Ask a bridge to speak for each account of its venue that the fan-out will never start**, once
/// per start — [`VenueAdapter::report_not_running_account`], for exactly those accounts.
///
/// An account that does not run never reaches its bridge's `start` ([`crate::accounts_to_run`]),
/// so what `start` would say about its stored credentials ("a live-tier key set is stored and
/// unused", "… incomplete, these keys are missing") went unsaid. Since decision 0156 that is the
/// DEFAULT account too: it is no longer started on paper to hear its bridge out.
///
/// Which accounts: not in the start set, and blocked by a missing or unusable key set (or the box
/// one needs): `LiveTierNotWired`, `NoCredentials`, `SdkAbsent` (fxcm without its shim, which would
/// otherwise hide a STORE finding). Every other block is someone else's to say:
/// `NoAccountRow`/`AccountInactive`/`TierConflict` (the account table; said by
/// `crate::paper_fallback`'s `report_not_running`), `NoAccountSupport` (the venue),
/// `AccountNotInStore`/`SidecarHeldElsewhere` (venue state). The bridge's default says nothing.
///
/// **Returns nothing, called AFTER the arming rows exist**: it cannot change which account starts
/// or at which tier — `accounts_to_run` alone decides, and the unit test beside this module holds
/// the start set unchanged.
pub(crate) fn report_not_running_accounts(
    registry: &'static [VenueRow],
    venue: &str,
    rows: &[vike_config::VenueArming],
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) {
    let Some(VenueRow::Adapter(row)) = crate::row_of(registry, venue) else { return };
    let mounted = crate::accounts_to_run(rows);
    let process = process_facts();
    for r in rows {
        let speaks = matches!(
            r.block,
            ArmingBlock::LiveTierNotWired | ArmingBlock::NoCredentials | ArmingBlock::SdkAbsent
        );
        // Only an account the table states a tier for reaches a key question at all.
        let Some(tier) = r.tier else { continue };
        if mounted.contains(&r.label) || !speaks {
            continue;
        }
        row.report_not_running_account(&inputs_for(*row, &r.label, vars, tier, policy, &process));
    }
}

/// Whether this row holds a process-exclusive resource — its declaration's
/// `process_exclusive`. A `FeatureAbsent` row and an id the registry does not carry hold none.
pub(crate) fn is_process_exclusive(row: Option<&VenueRow>) -> bool {
    matches!(row, Some(VenueRow::Adapter(m)) if m.declaration().process_exclusive.is_some())
}

/// Everything `make_engine_for_account` hands the contract path.
pub(crate) struct ContractCall<'a> {
    pub(crate) registry: &'static [VenueRow],
    pub(crate) venue: &'a str,
    pub(crate) symbol: &'a str,
    pub(crate) account: &'a AccountLabel,
    pub(crate) declared_legs: &'a [String],
    pub(crate) vars: &'a HashMap<String, String>,
    pub(crate) live_events: &'a vike_exec::EventSender,
    pub(crate) recon_enabled: bool,
    pub(crate) recon_trigger: Option<std::sync::mpsc::Sender<()>>,
    pub(crate) properties_rec: Option<std::sync::Arc<vike_data::PropertiesRecorder>>,
    pub(crate) risk_profile: Option<&'a vike_model::ProfileRisk>,
    pub(crate) policy: Option<&'a AdapterPolicy>,
    /// The account's tier (`crate::arming`'s `account_tier`), never `Paper` here.
    pub(crate) tier: AccountTier,
    pub(crate) halt_admit: vike_model::HaltAdmit,
    pub(crate) static_default: vike_model::FeeSchedule,
}

/// The error both halves of the tier interlock log: the venue, the account and its tier, and no
/// value.
fn refuse_beyond_the_tier(venue: &str, account: &AccountLabel, tier: AccountTier, what: &str) {
    tracing::error!(
        venue,
        account = %account,
        tier = tier.as_str(),
        "{venue}: the bridge went past this account's tier — {what} while the account's tier is \
         `{tier}`. Refused: the account does NOT RUN. A bridge binds exactly the account's \
         tier: its live tier only when `AdapterInputs::tier` is `live`, and never demo for \
         a `live` account. This is a defect in the bridge."
    );
}

/// The error the no-downgrade rule logs when a `live` account does not run because its bridge
/// could bind only demo — before that bridge is asked to start.
fn refuse_a_downgrade(venue: &str, account: &AccountLabel) {
    tracing::error!(
        venue,
        account = %account,
        "{venue}: account `{account}` has tier `live`, and no usable LIVE key set is stored for it \
         — its bridge could reach only a demo/testnet tier, and a live account never trades demo. \
         It does NOT RUN. Store the account's LIVE keys, or say what you mean: \
         `vike-cli secrets accounts` to find its row, then \
         `vike-cli secrets account set-tier --id <N> --tier demo`."
    );
}

/// **THE TIER INTERLOCK, start half.** An outcome BOUND to a tier other than the account's
/// becomes no outcome at all: client shut down, no live entry, identity record or reconcile
/// handle, and the account does not run. `resolve` can answer correctly while `start` (the half
/// that signs orders) still binds another tier; a correct bridge never does.
///
/// Both directions are refused:
/// * **above** — the LIVE tier while the account's tier is below `live`: real money the operator
///   did not arm.
/// * **below** — the DEMO tier for a `live` account: the no-downgrade rule
///   ([`resolution_to_arming`]), whose resolve half normally stops it before `start` is called.
///
/// "Binds a tier" is two things: a `Live` exec outcome's `bound_tier`, or an
/// [`IdentityReport`](vike_bridge_core::venue_adapter::IdentityReport)'s `tier` — which a declined
/// outcome can carry too (hyperliquid's confirmed `userRole` survives a failed `meta`), and which
/// [`contract_parts`] would record at that tier. Such an identity means an authenticated read
/// already reached that network; the interlock cannot undo that, and refuses to record it.
fn within_the_tier(
    venue: &str,
    account: &AccountLabel,
    tier: AccountTier,
    outcome: AdapterOutcome,
) -> AdapterOutcome {
    let bound = match &outcome.exec {
        ExecOutcome::Live(live) => Some(live.bound_tier),
        ExecOutcome::Paper => None,
    };
    let identity = outcome.identity.as_ref().map(|identity| identity.tier);
    let what = if let Some(bound) = bound.filter(|b| *b != tier) {
        format!("its `start` bound the {} tier", bound.as_str().to_uppercase())
    } else if let Some(at) = identity.filter(|t| *t != tier) {
        format!(
            "its `start` reported an account identity at the {} tier",
            at.as_str().to_uppercase()
        )
    } else {
        return outcome;
    };
    refuse_beyond_the_tier(venue, account, tier, &what);
    if let ExecOutcome::Live(mut live) = outcome.exec {
        live.client.begin_detach();
        live.client.detach();
    }
    AdapterOutcome::paper()
}

/// **A contract row's start** — the tier interlock, the pre-connect budget refusal, the
/// process-exclusive claim when declared, the bridge's `start`, and the outcome as [`AdapterParts`].
///
/// ⚠ **`Err(EngineError::NotRunning)` is the answer for every account that does not run at its
/// tier** (decision 0156): a probe that does not arm it, an interlock refusal, a resource another
/// account holds, and a `start` that came back without a client (a failed connect, a declined key).
/// No paper client is built in its place, and the bridge's `start` is not asked for an account the
/// probe does not arm. `Err(EngineError::MissingRiskBudget)` stays the one refusal of the NODE.
pub(crate) fn contract_parts(
    row: &'static dyn VenueAdapter,
    c: ContractCall<'_>,
) -> Result<AdapterParts, EngineError> {
    // THE ONE PROBE: the pre-connect refusal and the exclusive decision both read it, so no second
    // `resolve` runs here.
    let (armed, block) = crate::arming::account_arming_under(
        c.registry, c.venue, c.account, c.vars, c.tier, c.policy,
    );
    let not_running = |block: ArmingBlock| {
        Err(EngineError::NotRunning(NotRunning {
            venue: c.venue.to_string(),
            account: c.account.clone(),
            tier: Some(c.tier),
            block,
        }))
    };
    // THE TIER INTERLOCK, resolve half. A bridge whose `resolve` answered LIVE under a tier below
    // `live` is wrong, and the start does not act on the answer: no budget check, no claim, no
    // session. The arming PROJECTION is deliberately not clamped, so `vike-backend venues` and the
    // roster-wide `effective <= tier` test still see the bridge's wrong answer.
    if armed == Some(AccountTier::Live) && c.tier != AccountTier::Live {
        refuse_beyond_the_tier(c.venue, c.account, c.tier, "its `resolve` answered LIVE");
        return not_running(ArmingBlock::AdapterFailed);
    }
    // …AND THE NO-DOWNGRADE RULE, resolve half: a `live` account the probe does not arm for want of
    // a live key set never reaches its bridge's `start`, which (aster's testnet fallback, a
    // demo-only arm) would open a DEMO session under the live account's name. Where the bridge
    // itself answered `LiveCredentialsAbsent` (binance, bybit, okx, hyperliquid: no fallback), its
    // `start` would have come back empty anyway; this line speaks for both.
    if c.tier == AccountTier::Live && armed.is_none() && block == ArmingBlock::LiveCredentialsAbsent
    {
        refuse_a_downgrade(c.venue, c.account);
        return not_running(block);
    }
    let declaration = row.declaration();
    // THE ONE-SESSION REFUSAL, said in the bridge's own words: the account would have armed on its
    // own merits and another account holds the process-exclusive resource. The refusal is RENDERED
    // before it is logged, so the bridge's words are produced whether or not a subscriber listens.
    if block == ArmingBlock::SidecarHeldElsewhere
        && let Some(exclusive) = declaration.process_exclusive
    {
        let holder = crate::exclusive::holder(c.registry, c.venue, c.vars, c.policy).to_string();
        let refusal = (exclusive.held_by_another)(&c.account.to_string(), &holder);
        tracing::error!(venue = c.venue, "{refusal}");
        return not_running(block);
    }
    // Every other answer the probe does not arm: the account does not run, and its bridge is not
    // asked to start it (nothing it built would be used).
    if armed.is_none() {
        return not_running(block);
    }
    crate::arming::require_live_risk_budget(
        c.venue,
        &c.risk_profile.map(vike_model::ProfileRisk::to_risk_limits).unwrap_or_default(),
        c.risk_profile.is_some(),
    )?;
    let process = adapter_process_facts();
    let request = AdapterRequest {
        inputs: inputs_for(row, c.account, c.vars, c.tier, c.policy, &process),
        symbol: c.symbol,
        declared_legs: c.declared_legs,
        events: c.live_events,
        recon_enabled: c.recon_enabled,
        recon_trigger: if declaration.takes_recon_trigger { c.recon_trigger } else { None },
        properties_rec: c.properties_rec,
        risk_profile: c.risk_profile,
        market_slippage: c.policy.and_then(|p| p.market_slippage),
        halt_admit: c.halt_admit,
    };
    // Every call of the bridge's `start` goes through the interlock's start half.
    let start = |request| within_the_tier(c.venue, c.account, c.tier, row.start(request));
    let outcome = match declaration.process_exclusive {
        None => start(request),
        Some(exclusive) => match crate::exclusive::claim(exclusive.resource) {
            None => {
                let refusal = (exclusive.already_claimed)(&c.account.to_string());
                tracing::error!(venue = c.venue, "{refusal}");
                return not_running(ArmingBlock::AdapterFailed);
            }
            Some(claim) => {
                let outcome = start(request);
                // Kept ONLY for a running resource; every other exit (an outcome the interlock
                // refused included) drops the guard, which releases it.
                if matches!(outcome.exec, ExecOutcome::Live(_)) {
                    claim.keep();
                }
                outcome
            }
        },
    };
    // THE ORDER HOLD, start half: the probe said this arm sends no order, so its bound client is
    // wrapped and no order reaches it, whatever the bridge's own client would do with one.
    let outcome = if block == ArmingBlock::OrdersHeld {
        crate::orders_held::hold_orders(c.venue, c.account, c.live_events, outcome)
    } else {
        outcome
    };
    let AdapterOutcome { exec, recon, identity } = outcome;
    let identity = identity.map(|i| (i.book, i.evidence, i.tier));
    match exec {
        ExecOutcome::Live(live) => Ok(parts_from_live(
            c.symbol,
            c.declared_legs,
            live,
            recon,
            identity,
            declaration.grid_source,
            c.static_default,
        )),
        // THE START DID NOT COME UP at the tier the probe armed: a failed connect (ctrader, ibkr),
        // a key the venue declined (hyperliquid, polymarket), or an outcome the interlock refused.
        // The account does NOT RUN — no paper client in its place. Its reconcile handle (polymarket's
        // recon-only lane) authenticates an account no engine here trades, so it is dropped. A
        // venue-CONFIRMED identity is still a fact an authenticated read established about the
        // book, so it is recorded as it always was.
        ExecOutcome::Paper => {
            drop(recon);
            if let Some((book, evidence, tier)) = identity {
                crate::book_identity::record_confirmation(
                    c.venue,
                    evidence,
                    c.account,
                    tier,
                    &book,
                    crate::arming::directory_of(c.policy),
                );
            }
            not_running(ArmingBlock::AdapterFailed)
        }
    }
}

/// A `Live` start outcome as the parts the shared tail assembles — the only outcome that becomes
/// an engine on the contract path (decision 0156: a declined start is NOT RUNNING, never a paper
/// client). Its paper twins are gone: the parts a `Paper` outcome used to become, and
/// `absent_parts`, the paper client a feature missing from the binary produced — that account is
/// NOT RUNNING with `FeatureAbsent` now (`crate::make_engine_for_account`).
pub(crate) fn parts_from_live(
    symbol: &str,
    declared_legs: &[String],
    live: LiveExec,
    recon: Option<Box<dyn vike_exec::recon::ReconClient>>,
    identity: Option<(String, &'static str, AccountTier)>,
    grid_source: DeclaredGridSource,
    static_default: vike_model::FeeSchedule,
) -> AdapterParts {
    let LiveExec { client, bound_tier, grid, contract_size, margin_mode, leg_grids } = live;
    AdapterParts {
        client,
        recon,
        limits: grid.map_or_else(vike_model::RiskLimits::new, |g| {
            vike_model::RiskLimits::from_properties(&g)
        }),
        contract_size: contract_size.unwrap_or(0.0),
        default_margin_mode: margin_mode.unwrap_or(vike_model::MarginMode::Cross),
        symbol_grids: crate::symbol_grid::declared_symbol_grids(symbol, declared_legs, |leg| {
            leg_grids.iter().find(|(l, _)| l == leg).map(|(_, p)| *p)
        }),
        grid_source,
        // THE BOUND TIER — what the credentials authenticate, never a conjunct (the spec's
        // Finding 1). The tier interlock has already held it equal to the account's.
        bound_tier,
        identity,
        static_default,
    }
}
