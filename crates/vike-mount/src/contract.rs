//! **The generic fold over a contract row** — what `make_engine_for_account` and the arming
//! projection do with a `VenueRow::Mount`, venue by venue identical to what the legacy arms did,
//! with no venue named here. `docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use vike_bridge_core::venue_mount::{
    DeclaredGridSource, ExecOutcome, HeldBelowLive, LiveExec, MountInputs, MountOutcome,
    MountRequest, PaperCause, ProcessFacts, Resolution, Tier, VenueMount,
};
use vike_config::{ArmingBlock, VenueMode};
use vike_model::account_keys::AccountLabel;
use vike_secrets::venue_setting::VenueSettings;

use crate::{MountError, MountParts, MountPolicy, VenueRow};

/// The view a venue with no `venue_setting` rows gets.
static EMPTY_SETTINGS: LazyLock<VenueSettings> = LazyLock::new(VenueSettings::default);

/// This venue's settings out of the snapshot the composition root read.
pub(crate) fn settings_of<'a>(policy: Option<&'a MountPolicy>, venue: &str) -> &'a VenueSettings {
    policy.and_then(|p| p.venue_settings.get(venue)).unwrap_or(&*EMPTY_SETTINGS)
}

/// The state and bin directories, from the boot's declaration — the values the legacy dukascopy arm
/// and ctrader's bridge read for themselves until their ports. Both are handed to a bridge as
/// `MountInputs::process` (`crates/bridges/dukascopy/src/mount.rs`'s `DukascopyVenueMount`,
/// `crates/bridges/ctrader/src/mount.rs`'s `CtraderVenueMount`).
///
/// **This is the PROBE's view, and it leaves `halt_path` EMPTY on purpose.** The arming probe, the
/// clock canary and the startup preflight start no client, so they have no sentinel to hand anyone,
/// and resolving one here would run the process-wide resolver — memoizing it, and logging a
/// `HALT sentinel …` line — from a root (`vike-cli`) that never declared a project and was only
/// asking a question. [`mount_process_facts`] adds the sentinel for the one caller that builds a
/// client.
pub(crate) fn process_facts() -> ProcessFacts {
    let state_dir = vike_bridge_core::halt::declared_project_state_dir();
    let bin_dir = state_dir.as_deref().and_then(|state| {
        // `<project>/settings/state` -> `<project>/bin`
        state
            .parent()
            .and_then(Path::parent)
            .map(|project| project.join(vike_model::state_path::PROJECT_BIN_DIR))
    });
    ProcessFacts { state_dir, bin_dir, halt_path: PathBuf::new() }
}

/// [`process_facts`] for a MOUNT: the same directories, plus the HALT sentinel's path.
///
/// **The sentinel's path is resolved HERE, once, by the process-wide resolver**
/// (`vike_bridge_core::halt::halt_path_from_env`), and every live bridge is handed the answer rather
/// than calling the resolver from its own exec client (decision 0099). This is the composition
/// layer's call to make — the paper books and the daemon's startup advisory ask the same resolver —
/// so the live venues, the paper books and the file the operator is told to `touch` cannot name
/// three files. It is also what puts the arming report (`HALT sentinel path resolved`, or `NOT
/// ARMABLE`) in the log at the moment the first venue mounts, as `ExecActor::spawn` used to.
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

fn held_block(held: HeldBelowLive) -> ArmingBlock {
    match held {
        HeldBelowLive::DemoOnlyArm => ArmingBlock::DemoOnlyArm,
        HeldBelowLive::LiveCredentialsAbsent => ArmingBlock::LiveCredentialsAbsent,
    }
}

/// A resolution as an arming row — the legacy probe's `demo_under` fold, generic: a held-below
/// reason is reported only under a `live` ceiling, where something is actually being refused.
pub(crate) fn resolution_to_arming(resolution: Resolution, live: bool) -> (VenueMode, ArmingBlock) {
    match resolution {
        Resolution::Paper(cause) => (VenueMode::Paper, paper_block(cause)),
        Resolution::Armed { tier: Tier::Live, .. } => (VenueMode::Live, ArmingBlock::None),
        Resolution::Armed { tier: Tier::Demo, held_below_live } => (
            VenueMode::Demo,
            match (live, held_below_live) {
                (true, Some(held)) => held_block(held),
                _ => ArmingBlock::None,
            },
        ),
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

/// The arming probe for a contract row: the two generic preconditions, then the bridge's `resolve`.
pub(crate) fn contract_arming(
    row: &dyn VenueMount,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    ceiling: VenueMode,
    policy: Option<&MountPolicy>,
) -> (VenueMode, ArmingBlock) {
    // FIRST, before the ceiling — the legacy probe's order: a labelled account of a venue that
    // cannot address one is refused whatever its line says.
    if !label.is_default() && !row.declaration().addresses_accounts {
        return (VenueMode::Paper, ArmingBlock::NoAccountSupport);
    }
    if ceiling == VenueMode::Paper {
        return (VenueMode::Paper, ArmingBlock::Disarmed);
    }
    let live = crate::arming::ceiling_permits_live(ceiling);
    let process = process_facts();
    let inputs = inputs_for(row, label, vars, live, policy, &process);
    resolution_to_arming(row.resolve(&inputs), live)
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
    pub(crate) risk_profile: Option<&'a vike_exec::ProfileRisk>,
    pub(crate) policy: Option<&'a MountPolicy>,
    pub(crate) mode: VenueMode,
    pub(crate) live_permitted: bool,
    pub(crate) halt_admit: vike_model::HaltAdmit,
    pub(crate) static_default: vike_model::FeeSchedule,
}

/// The error both halves of the ceiling interlock log: the venue and the account, and no value.
fn refuse_beyond_the_ceiling(venue: &str, account: &AccountLabel, what: &str) {
    tracing::error!(
        venue,
        account = %account,
        "{venue}: the bridge went past this account's arming ceiling — {what} while the ceiling is \
         below `live`. Refused: the account mounts PAPER. A bridge may reach its live tier only \
         when `MountInputs::live_permitted` is true, so this is a defect in the bridge."
    );
}

/// **THE CEILING INTERLOCK, mount half.** An outcome that reaches the LIVE tier while the ceiling is
/// below `live` becomes PAPER: its client is shut down, and it leaves no live entry, no identity
/// record and no reconcile handle. `resolve` can answer correctly while `mount` still binds the
/// live tier, and `mount` is the half that signs orders. A correct bridge never produces such an
/// outcome — `MountInputs::live_permitted` was `false`, so it could not have chosen its live tier.
///
/// "Reaches the live tier" is two things, not one. A `Live` exec outcome BOUND to the LIVE tier is
/// the first. The second is an [`IdentityReport`](vike_bridge_core::venue_mount::IdentityReport)
/// whose `tier` is `Live` — which a paper outcome can carry too (hyperliquid's confirmed `userRole`
/// answer survives a failed `meta`), and which [`parts_from_outcome`] would otherwise record as a
/// LIVE-tier account for an account the ceiling holds below `live`. The identity's tier is the
/// network the bridge's own handshake ran on, so a LIVE one means an authenticated read already
/// reached mainnet; the interlock cannot undo that, and refuses to record it.
fn within_the_ceiling(
    venue: &str,
    account: &AccountLabel,
    live_permitted: bool,
    outcome: MountOutcome,
) -> MountOutcome {
    if live_permitted {
        return outcome;
    }
    let what = if matches!(&outcome.exec, ExecOutcome::Live(live) if live.bound_tier == Tier::Live)
    {
        "its `mount` bound the LIVE tier"
    } else if matches!(&outcome.identity, Some(identity) if identity.tier == Tier::Live) {
        "its `mount` reported an account identity at the LIVE tier"
    } else {
        return outcome;
    };
    refuse_beyond_the_ceiling(venue, account, what);
    if let ExecOutcome::Live(mut live) = outcome.exec {
        live.client.begin_detach();
        live.client.detach();
    }
    MountOutcome::paper()
}

/// A contract row's mount: the ceiling interlock, the pre-connect budget refusal, the
/// process-exclusive claim when declared, the bridge's `mount`, and the outcome as [`MountParts`].
pub(crate) fn contract_parts(
    row: &'static dyn VenueMount,
    c: ContractCall<'_>,
) -> Result<MountParts, MountError> {
    // THE ONE PROBE: the pre-connect refusal and the exclusive decision both read it, exactly as
    // the legacy prefix and the legacy dukascopy arm did — so no second `resolve` runs here.
    let (armed, block) = crate::arming::account_arming_under(
        c.registry, c.venue, c.account, c.vars, c.mode, c.policy,
    );
    // THE CEILING INTERLOCK, resolve half. A bridge whose `resolve` answered LIVE under a ceiling
    // below `live` is wrong, and the mount does not act on the answer: no budget check, no claim,
    // no session. The arming PROJECTION is deliberately not clamped, so the arming screen and the
    // roster-wide `effective <= ceiling` test still see the bridge's wrong answer.
    if armed == VenueMode::Live && !c.live_permitted {
        refuse_beyond_the_ceiling(c.venue, c.account, "its `resolve` answered LIVE");
        return Ok(parts_from_outcome(
            c.venue,
            c.symbol,
            c.declared_legs,
            MountOutcome::paper(),
            row.declaration().grid_source,
            c.static_default,
        ));
    }
    if armed != VenueMode::Paper {
        crate::arming::require_live_risk_budget(
            c.venue,
            &c.risk_profile.map(vike_exec::ProfileRisk::to_risk_limits).unwrap_or_default(),
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
    let mount =
        |request| within_the_ceiling(c.venue, c.account, c.live_permitted, row.mount(request));
    let outcome = match declaration.process_exclusive {
        None => mount(request),
        Some(exclusive) => {
            let label = c.account.to_string();
            if block == ArmingBlock::SidecarHeldElsewhere {
                // Would have armed on its own merits, and another account holds the resource.
                // The refusal is RENDERED before it is logged — the legacy arm's order — so the
                // bridge's words are produced whether or not a subscriber is listening.
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
                        // Kept ONLY for a running resource; every other exit drops the guard,
                        // which releases it (Review Focus 3) — an outcome the interlock refused
                        // included.
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

/// A `FeatureAbsent` row's mount: the paper client, exactly what the legacy `_` arm built.
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
            limits: grid.map_or_else(vike_exec::RiskLimits::new, |g| {
                vike_exec::RiskLimits::from_properties(&g)
            }),
            contract_size: contract_size.unwrap_or(0.0),
            default_margin_mode: margin_mode.unwrap_or(vike_model::MarginMode::Cross),
            symbol_grids: crate::symbol_grid::declared_symbol_grids(symbol, declared_legs, |leg| {
                leg_grids.iter().find(|(l, _)| l == leg).map(|(_, p)| *p)
            }),
            grid_source,
            // THE BOUND TIER — what the credentials authenticate, never a conjunct. This is the
            // line that fixes the spec's Finding 1 for every venue that reaches it.
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
            limits: vike_exec::RiskLimits::new(),
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
