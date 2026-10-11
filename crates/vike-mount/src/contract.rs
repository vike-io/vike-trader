//! **The generic fold over a contract row** — what `make_engine_for_account` and the arming
//! projection do with a `VenueRow::Mount`, with no venue named here.
//! `docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use vike_bridge_core::venue_mount::{
    DeclaredGridSource, ExecOutcome, LiveExec, MountInputs, MountOutcome, MountRequest, PaperCause,
    ProcessFacts, Resolution, Tier, VenueMount,
};
use vike_config::{ArmingBlock, VenueMode};
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::venue_setting::VenueSettings;

use crate::{MountError, MountParts, MountPolicy, VenueRow};

/// The view a venue with no `venue_setting` rows gets.
static EMPTY_SETTINGS: LazyLock<VenueSettings> = LazyLock::new(VenueSettings::default);

/// This venue's settings out of the snapshot the composition root read.
pub(crate) fn settings_of<'a>(policy: Option<&'a MountPolicy>, venue: &str) -> &'a VenueSettings {
    policy.and_then(|p| p.venue_settings.get(venue)).unwrap_or(&*EMPTY_SETTINGS)
}

/// The state and bin directories from the boot's declaration, handed to a bridge as
/// `MountInputs::process` (e.g. `crates/bridges/dukascopy/src/mount.rs`'s `DukascopyVenueMount`,
/// `crates/bridges/ctrader/src/mount.rs`'s `CtraderVenueMount`).
///
/// **The PROBE's view: `halt_path` is EMPTY on purpose.** The arming probe, clock canary and
/// preflight start no client; resolving the sentinel here would run (memoize, and log `HALT
/// sentinel …` from) the process-wide resolver in a root like `vike-cli` that declared no project.
/// [`mount_process_facts`] adds it for the one caller that builds a client.
pub(crate) fn process_facts() -> ProcessFacts {
    let state_dir = vike_bridge_core::halt::declared_project_state_dir();
    let bin_dir = state_dir.as_deref().and_then(|state| {
        // `<project>/settings/state` -> `<project>/bin`
        state
            .parent()
            .and_then(Path::parent)
            .map(|project| project.join(vike_model::paths::state_path::PROJECT_BIN_DIR))
    });
    ProcessFacts { state_dir, bin_dir, halt_path: PathBuf::new() }
}

/// [`process_facts`] for a MOUNT: the same directories, plus the HALT sentinel's path.
///
/// **The sentinel's path is resolved HERE, once, by the process-wide resolver**
/// (`vike_bridge_core::halt::halt_path_from_env`) and handed to every live bridge (decision 0099):
/// the paper books and the daemon's advisory ask the same resolver, so live venues, paper books and
/// the file the operator is told to `touch` cannot name three files. It also logs the arming report
/// (`HALT sentinel path resolved`, or `NOT ARMABLE`) when the first venue mounts.
pub(crate) fn mount_process_facts() -> ProcessFacts {
    ProcessFacts { halt_path: vike_bridge_core::halt::halt_path_from_env(), ..process_facts() }
}

pub(crate) fn tier_mode(tier: Tier) -> VenueMode {
    match tier {
        Tier::Demo => VenueMode::Demo,
        Tier::Live => VenueMode::Live,
    }
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

/// A resolution as an arming row, under an account whose tier is `live` (`live == true`) or `demo`.
///
/// # ⚠ THE NO-DOWNGRADE RULE: a `live` account never trades demo
///
/// The tier a bridge binds must EQUAL the account's tier. Under a `live` tier a bridge may still
/// answer a DEMO arming — aster falls back to its testnet keys
/// (`HeldBelowLive::LiveCredentialsAbsent`), a demo-only arm (ibkr, deribit, dukascopy, …) answers
/// `HeldBelowLive::DemoOnlyArm` — and that answer is PAPER here,
/// `ArmingBlock::LiveCredentialsAbsent`: a live account whose usable live key set is absent,
/// whatever the bridge's reason for reaching only demo. The operator said `live`; a demo session
/// would trade a different account under the live account's name. To trade demo, the account's
/// tier says `demo`.
///
/// The other direction — a bridge answering LIVE under a `demo` tier — is NOT folded here: the
/// projection shows the bridge's wrong answer, and [`contract_parts`]' interlock refuses it.
pub(crate) fn resolution_to_arming(resolution: Resolution, live: bool) -> (VenueMode, ArmingBlock) {
    match resolution {
        Resolution::Paper(cause) => (VenueMode::Paper, paper_block(cause)),
        Resolution::Armed { tier: Tier::Live, .. } => (VenueMode::Live, ArmingBlock::None),
        Resolution::Armed { tier: Tier::Demo, .. } if live => {
            (VenueMode::Paper, ArmingBlock::LiveCredentialsAbsent)
        }
        Resolution::Armed { tier: Tier::Demo, .. } => (VenueMode::Demo, ArmingBlock::None),
    }
}

/// The inputs a pre-mount question is asked with — the arming probe, the credential probe, the
/// mount. `process` is borrowed from the caller so the struct outlives no temporary.
pub(crate) fn inputs_for<'a>(
    row: &dyn VenueMount,
    label: &'a AccountLabel,
    vars: &'a HashMap<String, String>,
    live_permitted: bool,
    policy: Option<&'a MountPolicy>,
    process: &'a ProcessFacts,
) -> MountInputs<'a> {
    MountInputs {
        account: label,
        secrets: vars,
        settings: settings_of(policy, row.venue()),
        live_permitted,
        accounts: crate::arming::directory_of(policy),
        process,
    }
}

/// The arming probe for a contract row at the account's `tier`: the two generic preconditions,
/// then the bridge's `resolve`, folded by [`resolution_to_arming`] (the no-downgrade rule).
pub(crate) fn contract_arming(
    row: &dyn VenueMount,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    tier: VenueMode,
    policy: Option<&MountPolicy>,
) -> (VenueMode, ArmingBlock) {
    // FIRST, before the tier: a labelled account of a venue that cannot address one is refused
    // whatever its row says.
    if !label.is_default() && !row.declaration().addresses_accounts {
        return (VenueMode::Paper, ArmingBlock::NoAccountSupport);
    }
    if tier == VenueMode::Paper {
        return (VenueMode::Paper, ArmingBlock::PaperTier);
    }
    let live = crate::arming::tier_permits_live(tier);
    let process = process_facts();
    let inputs = inputs_for(row, label, vars, live, policy, &process);
    resolution_to_arming(row.resolve(&inputs), live)
}

/// **Ask a bridge to speak for each LABELLED account of its venue that the fan-out will never
/// mount**, once per start — [`VenueMount::report_unmounted_account`], for exactly those accounts.
///
/// A labelled account that resolved paper never reaches its bridge's `mount`
/// ([`crate::accounts_to_mount`]), so what `mount` would say about its stored credentials ("a
/// live-tier key set is stored and unused", "… incomplete, these keys are missing") went unsaid.
///
/// Which accounts: not the default, not in the mount set, and blocked by a missing or unusable key
/// set (or the box one needs): `LiveTierNotWired`, `NoCredentials`, `SdkAbsent` (fxcm without its
/// shim, which would otherwise hide a STORE finding). Every other block is someone else's to say:
/// `PaperTier`/`NoAccountRow`/`AccountInactive`/`TierConflict` (the account table; silent by
/// design, or loud in the mount for a conflict), `NoAccountSupport` (the venue),
/// `AccountNotInStore`/`SidecarHeldElsewhere` (venue state). The bridge's default says nothing.
///
/// **Returns nothing, called AFTER the arming rows exist**: it cannot change which account mounts
/// or at which tier — `accounts_to_mount` alone decides, and the unit test beside this module holds
/// the mount set unchanged.
pub(crate) fn report_unmounted_accounts(
    registry: &'static [VenueRow],
    venue: &str,
    rows: &[vike_config::VenueArming],
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) {
    let Some(VenueRow::Mount(row)) = crate::row_of(registry, venue) else { return };
    let mounted = crate::accounts_to_mount(rows);
    let process = process_facts();
    for r in rows {
        let speaks = matches!(
            r.block,
            ArmingBlock::LiveTierNotWired | ArmingBlock::NoCredentials | ArmingBlock::SdkAbsent
        );
        if r.is_default_account() || mounted.contains(&r.label) || !speaks {
            continue;
        }
        let live_permitted = crate::arming::tier_permits_live(r.tier);
        row.report_unmounted_account(&inputs_for(
            *row,
            &r.label,
            vars,
            live_permitted,
            policy,
            &process,
        ));
    }
}

/// Whether this row holds a process-exclusive resource — its declaration's
/// `process_exclusive`. A `FeatureAbsent` row and an id the registry does not carry hold none.
pub(crate) fn is_process_exclusive(row: Option<&VenueRow>) -> bool {
    matches!(row, Some(VenueRow::Mount(m)) if m.declaration().process_exclusive.is_some())
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
    pub(crate) policy: Option<&'a MountPolicy>,
    /// The account's tier (`crate::arming`'s `account_tier`), never `Paper` here.
    pub(crate) tier: VenueMode,
    pub(crate) live_permitted: bool,
    pub(crate) halt_admit: vike_model::HaltAdmit,
    pub(crate) static_default: vike_model::FeeSchedule,
}

/// The error both halves of the tier interlock log: the venue, the account and its tier, and no
/// value.
fn refuse_beyond_the_tier(venue: &str, account: &AccountLabel, tier: VenueMode, what: &str) {
    tracing::error!(
        venue,
        account = %account,
        tier = tier.as_str(),
        "{venue}: the bridge went past this account's tier — {what} while the account's tier is \
         `{tier}`. Refused: the account mounts PAPER. A bridge binds exactly the account's \
         tier: its live tier only when `MountInputs::live_permitted` is true, and never demo for \
         a `live` account. This is a defect in the bridge."
    );
}

/// The error the no-downgrade rule logs when it holds a `live` account to PAPER before its bridge
/// is asked to mount.
fn refuse_a_downgrade(venue: &str, account: &AccountLabel) {
    tracing::error!(
        venue,
        account = %account,
        "{venue}: account `{account}` has tier `live`, and no usable LIVE key set is stored for it \
         — its bridge could reach only a demo/testnet tier, and a live account never trades demo. \
         It mounts PAPER. Store the account's LIVE keys, or say what you mean: \
         `vike-cli secrets accounts` to find its row, then \
         `vike-cli secrets account set-tier --id <N> --tier demo`."
    );
}

/// **THE TIER INTERLOCK, mount half.** An outcome BOUND to a tier other than the account's
/// becomes PAPER: client shut down, no live entry, identity record or reconcile handle. `resolve`
/// can answer correctly while `mount` (the half that signs orders) still binds another tier; a
/// correct bridge never does.
///
/// Both directions are refused:
/// * **above** — the LIVE tier while the account's tier is below `live` (`live_permitted` was
///   `false`): real money the operator did not arm.
/// * **below** — the DEMO tier for a `live` account: the no-downgrade rule
///   ([`resolution_to_arming`]), whose resolve half normally stops it before `mount` is called.
///
/// "Binds a tier" is two things: a `Live` exec outcome's `bound_tier`, or an
/// [`IdentityReport`](vike_bridge_core::venue_mount::IdentityReport)'s `tier` — which a paper
/// outcome can carry too (hyperliquid's confirmed `userRole` survives a failed `meta`), and which
/// [`parts_from_outcome`] would record at that tier. Such an identity means an authenticated read
/// already reached that network; the interlock cannot undo that, and refuses to record it.
fn within_the_tier(
    venue: &str,
    account: &AccountLabel,
    tier: VenueMode,
    outcome: MountOutcome,
) -> MountOutcome {
    let bound = match &outcome.exec {
        ExecOutcome::Live(live) => Some(tier_mode(live.bound_tier)),
        ExecOutcome::Paper => None,
    };
    let identity = outcome.identity.as_ref().map(|identity| tier_mode(identity.tier));
    let what = if let Some(bound) = bound.filter(|b| *b != tier) {
        format!("its `mount` bound the {} tier", bound.as_str().to_uppercase())
    } else if let Some(at) = identity.filter(|t| *t != tier) {
        format!(
            "its `mount` reported an account identity at the {} tier",
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
    MountOutcome::paper()
}

/// A contract row's mount: the tier interlock, the pre-connect budget refusal, the
/// process-exclusive claim when declared, the bridge's `mount`, and the outcome as [`MountParts`].
pub(crate) fn contract_parts(
    row: &'static dyn VenueMount,
    c: ContractCall<'_>,
) -> Result<MountParts, MountError> {
    // THE ONE PROBE: the pre-connect refusal and the exclusive decision both read it, so no second
    // `resolve` runs here.
    let (armed, block) = crate::arming::account_arming_under(
        c.registry, c.venue, c.account, c.vars, c.tier, c.policy,
    );
    let paper = || {
        parts_from_outcome(
            c.venue,
            c.symbol,
            c.declared_legs,
            MountOutcome::paper(),
            row.declaration().grid_source,
            c.static_default,
        )
    };
    // THE TIER INTERLOCK, resolve half. A bridge whose `resolve` answered LIVE under a tier below
    // `live` is wrong, and the mount does not act on the answer: no budget check, no claim, no
    // session. The arming PROJECTION is deliberately not clamped, so `vike-backend venues` and the
    // roster-wide `effective <= tier` test still see the bridge's wrong answer.
    if armed == VenueMode::Live && !c.live_permitted {
        refuse_beyond_the_tier(c.venue, c.account, c.tier, "its `resolve` answered LIVE");
        return Ok(paper());
    }
    // …AND THE NO-DOWNGRADE RULE, resolve half: a `live` account the probe answers PAPER for want
    // of a live key set never reaches its bridge's `mount`, which (aster's testnet fallback, a
    // demo-only arm) would open a DEMO session under the live account's name. Where the bridge
    // itself answered `LiveCredentialsAbsent` (binance, bybit, okx, hyperliquid: no fallback), its
    // `mount` would have come back paper anyway; this line speaks for both.
    if c.tier == VenueMode::Live
        && armed == VenueMode::Paper
        && block == ArmingBlock::LiveCredentialsAbsent
    {
        refuse_a_downgrade(c.venue, c.account);
        return Ok(paper());
    }
    if armed != VenueMode::Paper {
        crate::arming::require_live_risk_budget(
            c.venue,
            &c.risk_profile.map(vike_model::ProfileRisk::to_risk_limits).unwrap_or_default(),
            c.risk_profile.is_some(),
        )?;
    }
    let declaration = row.declaration();
    let process = mount_process_facts();
    let request = MountRequest {
        inputs: inputs_for(row, c.account, c.vars, c.live_permitted, c.policy, &process),
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
    // Every call of the bridge's `mount` goes through the interlock's mount half.
    let mount = |request| within_the_tier(c.venue, c.account, c.tier, row.mount(request));
    let outcome = match declaration.process_exclusive {
        None => mount(request),
        Some(exclusive) => {
            let label = c.account.to_string();
            if block == ArmingBlock::SidecarHeldElsewhere {
                // Would have armed on its own merits; another account holds the resource. The
                // refusal is RENDERED before it is logged, so the bridge's words are produced
                // whether or not a subscriber is listening.
                let holder =
                    crate::exclusive::holder(c.registry, c.venue, c.vars, c.policy).to_string();
                let refusal = (exclusive.held_by_another)(&label, &holder);
                tracing::error!(venue = c.venue, "{refusal}");
                MountOutcome::paper()
            } else if armed == VenueMode::Paper {
                // Nothing will be started: the bridge logs its own refusal, if any.
                mount(request)
            } else {
                match crate::exclusive::claim(exclusive.resource) {
                    None => {
                        let refusal = (exclusive.already_claimed)(&label);
                        tracing::error!(venue = c.venue, "{refusal}");
                        MountOutcome::paper()
                    }
                    Some(claim) => {
                        let outcome = mount(request);
                        // Kept ONLY for a running resource; every other exit (an outcome the
                        // interlock refused included) drops the guard, which releases it.
                        if matches!(outcome.exec, ExecOutcome::Live(_)) {
                            claim.keep();
                        }
                        outcome
                    }
                }
            }
        }
    };
    Ok(parts_from_outcome(
        c.venue,
        c.symbol,
        c.declared_legs,
        outcome,
        declaration.grid_source,
        c.static_default,
    ))
}

/// A `FeatureAbsent` row's mount: the paper client.
pub(crate) fn absent_parts(
    venue: &str,
    symbol: &str,
    static_default: vike_model::FeeSchedule,
) -> MountParts {
    parts_from_outcome(
        venue,
        symbol,
        &[],
        MountOutcome::paper(),
        DeclaredGridSource::NoGrid,
        static_default,
    )
}

/// A mount outcome as the parts the shared tail assembles.
pub(crate) fn parts_from_outcome(
    venue: &str,
    symbol: &str,
    declared_legs: &[String],
    outcome: MountOutcome,
    grid_source: DeclaredGridSource,
    static_default: vike_model::FeeSchedule,
) -> MountParts {
    let MountOutcome { exec, recon, identity } = outcome;
    let identity = identity.map(|i| (i.book, i.evidence, tier_mode(i.tier)));
    match exec {
        ExecOutcome::Live(LiveExec {
            client,
            bound_tier,
            grid,
            contract_size,
            margin_mode,
            leg_grids,
        }) => MountParts {
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
            // Finding 1).
            record_tier: Some(tier_mode(bound_tier)),
            identity,
            live: true,
            static_default,
        },
        // A paper outcome records no identity from its recon client: that client (polymarket's
        // recon-only lane) authenticates an account this engine does not trade.
        ExecOutcome::Paper => MountParts {
            client: Box::new(crate::paper_fallback::paper_client(venue, symbol, static_default)),
            recon,
            limits: vike_model::RiskLimits::new(),
            contract_size: 0.0,
            default_margin_mode: vike_model::MarginMode::Cross,
            symbol_grids: indexmap::IndexMap::new(),
            grid_source,
            record_tier: None,
            identity,
            live: false,
            static_default,
        },
    }
}
