//! **The generic fold over a contract row** — what `make_engine_for_account` and the arming
//! projection do with a `VenueAdapterRow::Adapter`, with no venue named here.
//! `docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use vike_bridge_core::venue_adapter::{
    AdapterFlags, AdapterInputs, AdapterOutcome, AdapterRequest, ArmingHeldBelowLive,
    ArmingNotRunningCause, ArmingResolution, BookConfirmed, ProcessPaths, VenueAdapter,
    VenueAdapterExecutionOutcome, VenueAdapterExecutionRunning,
};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, ArmingBlock};
use vike_secrets::venue_setting::VenueSettings;
use vike_venues::venue_connection::venue_wide_exec_for;

use super::registry::{VenueAdapterRow, row_of};
use super::snapshot::TradeLaneSnapshot;
use super::{arming, exclusive, orders_held};
use crate::trade::lane::StartedAdapter;

/// The view a venue the snapshot holds no entry for gets: no `venue_setting` row and the release's
/// addresses (decision 0265), what `vike_secrets::venue_setting::seeded_venue_settings` hands a box
/// with no store. Only a policy-less probe or a test's bare snapshot reaches it: the boot's read
/// (`crates/vike-gatewayhub/src/gatewayhub_cli/boot.rs`'s `venue_settings_of`) holds an entry for every
/// seeded venue.
static SEEDED_SETTINGS: LazyLock<BTreeMap<String, VenueSettings>> =
    LazyLock::new(vike_secrets::venue_setting::seeded_venue_settings);

/// The view a venue with neither a `venue_setting` row nor a seeded address gets.
static EMPTY_SETTINGS: LazyLock<VenueSettings> = LazyLock::new(VenueSettings::default);

/// This venue's settings out of the snapshot the composition root read.
pub(crate) fn settings_of<'a>(
    policy: Option<&'a TradeLaneSnapshot>,
    venue: &str,
) -> &'a VenueSettings {
    policy
        .and_then(|p| p.venue_settings.get(venue))
        .or_else(|| SEEDED_SETTINGS.get(venue))
        .unwrap_or(&*EMPTY_SETTINGS)
}

/// The state and bin directories from the boot's declaration, handed to a bridge as
/// `AdapterInputs::process` (e.g. `crates/bridges/dukascopy/src/adapter.rs`'s `VenueAdapterDukascopy`,
/// `crates/bridges/ctrader/src/adapter.rs`'s `VenueAdapterCtrader`).
///
/// **The PROBE's view: `halt_path` is EMPTY on purpose.** The arming probe, clock canary and
/// preflight start no client; resolving the sentinel here would run (memoize, and log `HALT
/// sentinel …` from) the process-wide resolver in a root like `vike-cli` that declared no project.
/// [`adapter_process_facts`] adds it for the one caller that builds a client.
pub(crate) fn process_facts() -> ProcessPaths {
    let state_dir = vike_boot::halt::declared_project_state_dir();
    let bin_dir = state_dir.as_deref().and_then(|state| {
        // `<project>/settings/state` -> `<project>/bin`
        state
            .parent()
            .and_then(Path::parent)
            .map(|project| project.join(vike_paths::state_path::PROJECT_BIN_DIR))
    });
    ProcessPaths { state_dir, bin_dir, halt_path: PathBuf::new() }
}

/// [`process_facts`] for a START: the same directories, plus the HALT sentinel's path.
///
/// **The sentinel's path is resolved HERE, once, by the process-wide resolver**
/// (`vike_boot::halt::halt_path_from_env`) and handed to every live bridge (decision 0099):
/// the paper books and the daemon's advisory ask the same resolver, so live venues, paper books and
/// the file the operator is told to `touch` cannot name three files. It also logs the arming report
/// (`HALT sentinel path resolved`, or `NOT ARMABLE`) when the first venue starts.
///
/// Formerly `mount_process_facts`, renamed in the shape-and-names change (decision 0166).
pub(crate) fn adapter_process_facts() -> ProcessPaths {
    ProcessPaths { halt_path: vike_boot::halt::halt_path_from_env(), ..process_facts() }
}

fn paper_block(cause: ArmingNotRunningCause) -> ArmingBlock {
    match cause {
        ArmingNotRunningCause::NoCredentials => ArmingBlock::NoCredentials,
        ArmingNotRunningCause::LiveCredentialsAbsent => ArmingBlock::LiveCredentialsAbsent,
        ArmingNotRunningCause::LiveTierNotWired => ArmingBlock::LiveTierNotWired,
        ArmingNotRunningCause::ExecFlagUnset => ArmingBlock::ExecFlagUnset,
        ArmingNotRunningCause::LiveOnlyArm => ArmingBlock::LiveOnlyArm,
        ArmingNotRunningCause::SdkAbsent => ArmingBlock::SdkAbsent,
        ArmingNotRunningCause::AccountNotInStore => ArmingBlock::AccountNotInStore,
        ArmingNotRunningCause::NoLiveArm => ArmingBlock::NoLiveArm,
    }
}

/// A resolution as an arming row, under an account whose `tier` is `live` or `demo`: `(Some(t),
/// block)` when the bridge would arm it, `(None, why)` when the account does NOT RUN.
///
/// # ⚠ THE NO-DOWNGRADE RULE: a `live` account never trades demo
///
/// The tier a bridge binds must EQUAL the account's tier. Under a `live` tier a bridge may still
/// answer a DEMO arming — aster falls back to its testnet keys
/// (`ArmingHeldBelowLive::LiveCredentialsAbsent`), a demo-only arm (ibkr, deribit, dukascopy, …) answers
/// `ArmingHeldBelowLive::DemoOnlyArm` — and that answer is NOT RUNNING here,
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
/// `held_below_live: Some(ArmingHeldBelowLive::OrdersHeld)` is honoured at the tier the bridge binds:
/// the row is that tier with [`ArmingBlock::OrdersHeld`], so the account starts (a live arm binds
/// live, under the live risk budget like any other) and [`contract_parts`] wraps its client in
/// [`vike_bridge_core::execution_client::orders_held::ExecutionClientOrdersHeld`] ([`orders_held::hold_orders`]), which refuses every order before the bridge sees it
/// (`docs/decisions/0140-ibkr-rides-one-session-one-product-one-tier.md`, stage A). It does not
/// lift the no-downgrade rule: a held DEMO arming under a `live` tier still does not run. The two
/// demo-arm reasons on a LIVE arming are a bridge's incoherent answer and fold as they always did,
/// to a clear live row.
pub(crate) fn resolution_to_arming(
    resolution: ArmingResolution,
    tier: AccountTier,
) -> (Option<AccountTier>, ArmingBlock) {
    let held = Some(ArmingHeldBelowLive::OrdersHeld);
    match resolution {
        ArmingResolution::NotRunning(cause) => (None, paper_block(cause)),
        ArmingResolution::Armed { tier: AccountTier::Live, held_below_live }
            if held_below_live == held =>
        {
            (Some(AccountTier::Live), ArmingBlock::OrdersHeld)
        }
        ArmingResolution::Armed { tier: AccountTier::Live, .. } => {
            (Some(AccountTier::Live), ArmingBlock::None)
        }
        ArmingResolution::Armed { tier: AccountTier::Demo, .. } if tier == AccountTier::Live => {
            (None, ArmingBlock::LiveCredentialsAbsent)
        }
        ArmingResolution::Armed { tier: AccountTier::Demo, held_below_live }
            if held_below_live == held =>
        {
            (Some(AccountTier::Demo), ArmingBlock::OrdersHeld)
        }
        ArmingResolution::Armed { tier: AccountTier::Demo, .. } => {
            (Some(AccountTier::Demo), ArmingBlock::None)
        }
        // A bridge that declines answers `ArmingResolution::NotRunning` with its cause; "armed at paper" is
        // no arming at all, and nothing is built for it.
        ArmingResolution::Armed { tier: AccountTier::Paper, .. } => (None, ArmingBlock::NoLiveArm),
    }
}

/// **The venue gates a start reads, as bools** (`AdapterFlags`): the resolved `flags.*` rows this daemon read
/// (`TradeLaneSnapshot::flags`), copied field for field, so no bridge names a settings key or links `vike-config`
/// (decision 0095). The ONE place a bridge's flags are built; the literal is exhaustive, so a gate added to
/// `AdapterFlags` does not compile until it is filled here.
pub(crate) fn adapter_flags(flags: &vike_config::Flags) -> AdapterFlags {
    AdapterFlags {
        poly_exec: flags.poly_exec,
        poly_reconcile: flags.poly_reconcile,
        hyperliquid_hip3: flags.hyperliquid_hip3,
        allow_withdraw_keys: flags.allow_withdraw_keys,
    }
}

/// The inputs a pre-assembly question is asked with — the arming probe, the credential probe, the
/// start. `process` is borrowed from the caller so the struct outlives no temporary. `fx_clock` is the FX
/// login gate's clock (`AdapterInputs::fx_clock`): the trade lane's for a question that may open a venue
/// session, `None` for one that opens none. The flags are `policy`'s, or the guarded defaults with no snapshot.
pub(crate) fn inputs_for<'a>(
    row: &dyn VenueAdapter,
    label: &'a AccountLabel,
    vars: &'a HashMap<String, String>,
    tier: AccountTier,
    policy: Option<&'a TradeLaneSnapshot>,
    process: &'a ProcessPaths,
    fx_clock: Option<&'a Arc<dyn vike_model::Clock + Send + Sync>>,
) -> AdapterInputs<'a> {
    AdapterInputs {
        account: label,
        // The ONE active `account` row of `(venue, label)` at `tier`, and its credentials by field
        // (decision 0156 item 6): the bridge asks for a field and never composes a name.
        credentials: crate::trade::adapters::snapshot::directory_of(policy).credentials_for(
            row.venue(),
            label,
            tier,
        ),
        secrets: vars,
        flags: adapter_flags(&policy.map_or_else(vike_config::Flags::default, |p| p.flags)),
        settings: settings_of(policy, row.venue()),
        tier,
        accounts: crate::trade::adapters::snapshot::directory_of(policy),
        process,
        fx_clock,
    }
}

/// The arming probe for a contract row at the account's `tier`: the two generic preconditions,
/// then the bridge's `resolve`, folded by [`resolution_to_arming`] (the no-downgrade rule).
/// `probe` is the account's key set and the flags when the caller SUPPLIES them (a policy-less probe), or
/// `None` to read both from `policy` (its account directory and its flags), as the start does.
pub(crate) fn contract_arming(
    row: &dyn VenueAdapter,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    probe: Option<(vike_secrets::account_credentials::AccountCredentials<'_>, &vike_config::Flags)>,
    tier: AccountTier,
    policy: Option<&TradeLaneSnapshot>,
) -> (Option<AccountTier>, ArmingBlock) {
    // FIRST, before the tier: a labelled account of a venue that cannot address one is refused
    // whatever its row says.
    if !label.is_default() && !row.declaration().addresses_accounts {
        return (None, ArmingBlock::NoAccountSupport);
    }
    if tier == AccountTier::Paper {
        return vike_config::account_arming::PAPER_RUNS;
    }
    let process = process_facts();
    // `resolve` is pure: it opens no session, so it needs no clock.
    let mut inputs = inputs_for(row, label, vars, tier, policy, &process, None);
    if let Some((credentials, flags)) = probe {
        inputs.credentials = credentials;
        inputs.flags = adapter_flags(flags);
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
    registry: &'static [VenueAdapterRow],
    venue: &str,
    rows: &[vike_config::ArmingAccount],
    vars: &HashMap<String, String>,
    policy: Option<&TradeLaneSnapshot>,
) {
    let Some(VenueAdapterRow::Adapter(row)) = row_of(registry, venue) else { return };
    let mounted: Vec<AccountLabel> =
        rows.iter().filter(|row| row.runs()).map(|row| row.label.clone()).collect();
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
        // A report opens no session: no clock.
        let inputs = inputs_for(*row, &r.label, vars, tier, policy, &process, None);
        row.report_not_running_account(&inputs);
    }
}

/// Whether this row holds a process-exclusive resource — its declaration's
/// `process_exclusive`. A `FeatureAbsent` row and an id the registry does not carry hold none.
pub(crate) fn is_process_exclusive(row: Option<&VenueAdapterRow>) -> bool {
    matches!(row, Some(VenueAdapterRow::Adapter(m)) if m.declaration().process_exclusive.is_some())
}

/// What a start that came up hands back: the bound client with the facts the tradehub builds the
/// engine from, and the book the venue confirmed itself (hyperliquid's `userRole`), recorded by
/// `TradeLaneBridge::record_identity` after the tradehub resolved the fee schedule (the order the fold
/// has always had: on binance spot the account id and the commission rates ride ONE body, and
/// asking first costs a second signed read).
pub(crate) struct Bound {
    pub(crate) started: StartedAdapter,
    pub(crate) confirmed_book: Option<BookConfirmed>,
}

/// A start that did not come up: the account is NOT RUNNING (decision 0156) for this reason, with the
/// refusal sentence the bridge rendered where it rendered one (the process-exclusive resource's own
/// words). No paper client is built in its place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VenueAdapterStartRefusal {
    pub(crate) block: ArmingBlock,
    pub(crate) text: Option<String>,
}

impl VenueAdapterStartRefusal {
    /// A refusal that is its block alone.
    pub(crate) fn of(block: ArmingBlock) -> Self {
        VenueAdapterStartRefusal { block, text: None }
    }
}

/// Everything the tradehub's `make_engine_for_account` hands the contract path, over the wire.
pub(crate) struct ContractCall<'a> {
    pub(crate) registry: &'static [VenueAdapterRow],
    pub(crate) venue: &'a str,
    pub(crate) symbol: &'a str,
    pub(crate) account: &'a AccountLabel,
    pub(crate) declared_legs: &'a [String],
    pub(crate) vars: &'a HashMap<String, String>,
    pub(crate) events: &'a vike_execution::EventSender,
    pub(crate) reconcile_enabled: bool,
    pub(crate) reconcile_trigger: Option<std::sync::mpsc::Sender<()>>,
    pub(crate) risk_profile: Option<&'a vike_model::ProfileRisk>,
    pub(crate) policy: Option<&'a TradeLaneSnapshot>,
    /// The account's tier (`vike_config::account_arming`'s `account_tier`), never `Paper` here.
    pub(crate) tier: AccountTier,
    pub(crate) halt_admit: vike_model::HaltAdmit,
    /// The operator's market-slippage request (`TradeLaneSnapshot::market_slippage`).
    pub(crate) market_slippage: Option<f64>,
    /// The FX login gate's clock the started bridge's sessions log in and redial on (`AdapterInputs::fx_clock`).
    pub(crate) fx_clock: Option<&'a Arc<dyn vike_model::Clock + Send + Sync>>,
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
         `vike-cli account ls` to find its row, then \
         `vike-cli account set-tier --id <N> --tier demo`."
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
/// [`BookConfirmed`](vike_bridge_core::venue_adapter::BookConfirmed)'s `tier` — which a declined
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
        VenueAdapterExecutionOutcome::Running(live) => Some(live.bound_tier),
        VenueAdapterExecutionOutcome::NotRunning | VenueAdapterExecutionOutcome::Unreachable => {
            None
        }
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
    if let VenueAdapterExecutionOutcome::Running(mut live) = outcome.exec {
        live.client.begin_detach();
        live.client.detach();
    }
    AdapterOutcome::paper()
}

/// **A contract row's start** — the tier interlock, the process-exclusive claim when declared, the
/// bridge's `start`, the order hold, and the outcome as an [`AdapterBind`].
///
/// ⚠ **The pre-connect budget refusal is NOT here any more**: it needs the run profile, which is
/// the keep half's, so `crate::make_engine_for_account` makes it AFTER the arming table says the
/// account is armed and BEFORE this is called. That is the order the body had it in — resolve, the
/// resolve-half interlock, the no-downgrade rule, the exclusive refusal, the not-armed refusal, THEN
/// the budget, then the claim and the start — because none of the refusals ahead of the budget can
/// be reached by an account the fold sends here after a clean armed row (`crate::make_engine_for_account`
/// skips the budget for the one armed-at-live-under-a-lower-tier row the interlock refuses here).
///
/// ⚠ **`Err(VenueBackendError::NotRunning)` is the answer for every account that does not run at its
/// tier** (decision 0156): a probe that does not arm it, an interlock refusal, a resource another
/// account holds, and a `start` that came back without a client (a failed connect, a declined key).
/// No paper client is built in its place, and the bridge's `start` is not asked for an account the
/// probe does not arm.
pub(crate) fn contract_parts(
    row: &'static dyn VenueAdapter,
    c: ContractCall<'_>,
) -> Result<Bound, VenueAdapterStartRefusal> {
    // THE ONE PROBE: the tier interlock and the exclusive decision both read it, so no second
    // `resolve` runs here.
    let (armed, block) =
        arming::account_arming_under(c.registry, c.venue, c.account, c.vars, c.tier, c.policy);
    let not_running = |block: ArmingBlock| Err(VenueAdapterStartRefusal::of(block));
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
        let holder = exclusive::holder(c.registry, c.venue, c.vars, c.policy).to_string();
        let refusal = (exclusive.held_by_another)(&c.account.to_string(), &holder);
        tracing::error!(venue = c.venue, "{refusal}");
        return Err(VenueAdapterStartRefusal { block, text: Some(refusal) });
    }
    // Every other answer the probe does not arm: the account does not run, and its bridge is not
    // asked to start it (nothing it built would be used).
    if armed.is_none() {
        return not_running(block);
    }
    let process = adapter_process_facts();
    let request = AdapterRequest {
        inputs: inputs_for(row, c.account, c.vars, c.tier, c.policy, &process, c.fx_clock),
        symbol: c.symbol,
        declared_legs: c.declared_legs,
        events: c.events,
        reconcile_enabled: c.reconcile_enabled,
        // Handed only to a venue whose connection row nudges reconcile after a reopen.
        reconcile_trigger: c
            .reconcile_trigger
            .filter(|_| venue_wide_exec_for(c.venue).nudges_reconcile),
        // Production has passed `None` since #2093, and the wire never carries the recorder.
        properties_rec: None,
        risk_profile: c.risk_profile,
        market_slippage: c.market_slippage,
        halt_admit: c.halt_admit,
    };
    // Every call of the bridge's `start` goes through the interlock's start half.
    let start = |request| within_the_tier(c.venue, c.account, c.tier, row.start(request));
    let outcome = match declaration.process_exclusive {
        None => start(request),
        Some(exclusive) => match exclusive::claim(exclusive.resource) {
            None => {
                let refusal = (exclusive.already_claimed)(&c.account.to_string());
                tracing::error!(venue = c.venue, "{refusal}");
                return Err(VenueAdapterStartRefusal {
                    block: ArmingBlock::AdapterFailed,
                    text: Some(refusal),
                });
            }
            // Held ONLY by a running resource's client, for as long as that client lives (the
            // adapter's life: the gatewayhub outlives every tradehub, so "the life of the process"
            // would refuse the next start); every other exit (an outcome the interlock refused
            // included) drops the guard, which releases it.
            Some(claim) => exclusive::hold_while_running(claim, start(request)),
        },
    };
    // THE ORDER HOLD, start half: the probe said this arm sends no order, so its bound client is
    // wrapped and no order reaches it, whatever the bridge's own client would do with one.
    let outcome = if block == ArmingBlock::OrdersHeld {
        orders_held::hold_orders(c.venue, c.account, c.events, outcome)
    } else {
        outcome
    };
    let AdapterOutcome { exec, reconcile: recon, identity } = outcome;
    match exec {
        // The bound client and what the tradehub builds the engine from (`parts_from_bind` there)
        // from the wire reply (the declared-leg grids, the venue-fetched limits).
        VenueAdapterExecutionOutcome::Running(live) => {
            let VenueAdapterExecutionRunning {
                client,
                bound_tier,
                grid,
                contract_value,
                margin_mode,
                leg_grids,
            } = live;
            Ok(Bound {
                started: StartedAdapter {
                    bound_tier,
                    grid,
                    contract_value,
                    margin_mode,
                    leg_grids,
                    client,
                    reconcile: recon,
                },
                confirmed_book: identity,
            })
        }
        // THE START DID NOT COME UP at the tier the probe armed: a failed connect (ctrader, ibkr),
        // a key the venue declined (hyperliquid, polymarket), or an outcome the interlock refused.
        // The account does NOT RUN — no paper client in its place. Its reconcile handle (polymarket's
        // recon-only lane) authenticates an account no engine here trades, so it is dropped. A
        // venue-CONFIRMED identity is still a fact an authenticated read established about the
        // book, so it is recorded as it always was.
        VenueAdapterExecutionOutcome::NotRunning => {
            drop(recon);
            if let Some(BookConfirmed { book, evidence, tier }) = identity {
                super::book_identity::record_confirmation(
                    c.venue,
                    evidence,
                    c.account,
                    tier,
                    &book,
                    crate::trade::adapters::snapshot::directory_of(c.policy),
                );
            }
            not_running(ArmingBlock::AdapterFailed)
        }
        // THE CONNECT FAILED before the venue said anything (the bridge tells it apart: ibkr's IB Gateway not
        // running): nothing was refused and no read reached the venue, so there is no identity to record. Transient:
        // the tradehub asks again until it connects.
        VenueAdapterExecutionOutcome::Unreachable => {
            drop(recon);
            not_running(ArmingBlock::VenueUnreachable)
        }
    }
}
