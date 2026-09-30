//! **The generic fold over a contract row** — what `make_engine_for_account` and the arming
//! projection do with a `VenueRow::Mount`, venue by venue identical to what the legacy arms did,
//! with no venue named here. `docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use vike_bridge_core::account_directory::AccountDirectory;
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

/// The UNREAD account table — what a question asked with no policy gets.
pub(crate) fn unread_directory() -> &'static AccountDirectory {
    AccountDirectory::unread_ref()
}

/// The state and bin directories, from the boot's declaration — the values the dukascopy and
/// ctrader arms read for themselves today.
pub(crate) fn process_facts() -> ProcessFacts {
    let state_dir = vike_bridge_core::halt::declared_project_state_dir();
    let bin_dir = state_dir.as_deref().and_then(|state| {
        // `<project>/settings/state` -> `<project>/bin`
        state
            .parent()
            .and_then(Path::parent)
            .map(|project| project.join(vike_model::state_path::PROJECT_BIN_DIR))
    });
    ProcessFacts { state_dir, bin_dir }
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

/// Whether `venue` holds a process-exclusive resource. ⚠ The `Legacy` half names dukascopy until
/// its port lands; it is the one venue-shaped line in this file and leaves with that port.
pub(crate) fn is_process_exclusive(row: Option<&VenueRow>, venue: &str) -> bool {
    match row {
        Some(VenueRow::Mount(m)) => m.declaration().process_exclusive.is_some(),
        Some(VenueRow::Legacy(_)) | None => venue == vike_dukascopy::recon_client::VENUE,
        Some(VenueRow::FeatureAbsent { .. }) => false,
    }
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

/// A contract row's mount: the pre-connect budget refusal, the process-exclusive claim when
/// declared, the bridge's `mount`, and the outcome as [`MountParts`].
pub(crate) fn contract_parts(
    row: &'static dyn VenueMount,
    c: ContractCall<'_>,
) -> Result<MountParts, MountError> {
    // THE ONE PROBE: the pre-connect refusal and the exclusive decision both read it, exactly as
    // the legacy prefix and the legacy dukascopy arm did — so no second `resolve` runs here.
    let (armed, block) = crate::arming::account_arming_under(
        c.registry, c.venue, c.account, c.vars, c.mode, c.policy,
    );
    if armed != VenueMode::Paper {
        crate::arming::require_live_risk_budget(
            c.venue,
            &c.risk_profile.map(vike_exec::ProfileRisk::to_risk_limits).unwrap_or_default(),
            c.risk_profile.is_some(),
        )?;
    }
    let declaration = row.declaration();
    let process = process_facts();
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
    let outcome = match declaration.process_exclusive {
        None => row.mount(request),
        Some(exclusive) => {
            let label = c.account.to_string();
            if block == ArmingBlock::SidecarHeldElsewhere {
                // Would have armed on its own merits, and another account holds the resource.
                let holder =
                    crate::exclusive::holder(c.registry, c.venue, c.vars, c.policy).to_string();
                tracing::error!(
                    venue = c.venue,
                    "{}",
                    (exclusive.held_by_another)(&label, &holder)
                );
                MountOutcome::paper()
            } else if armed == VenueMode::Paper {
                // Nothing will be started: the bridge logs its own refusal, if any.
                row.mount(request)
            } else {
                match crate::exclusive::claim(c.venue) {
                    None => {
                        tracing::error!(venue = c.venue, "{}", (exclusive.already_claimed)(&label));
                        MountOutcome::paper()
                    }
                    Some(claim) => {
                        let outcome = row.mount(request);
                        // Kept ONLY for a running resource; every other exit drops the guard,
                        // which releases it (Review Focus 3).
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
