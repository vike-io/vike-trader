//! The LIVE cross-exchange mount: the maker folded into the wired-market runtime's core.

#[cfg(doc)]
use vike_core::CoreConfig;
use vike_core::LiveBroker;
use vike_model::{Strategy, XemmParams};

#[cfg(doc)]
use super::build_paper_xemm_core;
use super::{XemmConfigError, validate_xemm_mount};
use crate::node::{
    EngineFees, MountFeeRebuild, RuntimeAssembly, RuntimeConfig, RuntimeError,
    assemble_runtime_armed,
};
use crate::run::{MountBuild, MountPlacement};

/// The spawned LIVE cross-exchange mount: the wired-market [`RuntimeAssembly`] with the caller's
/// cross-exchange maker folded into its [`CoreConfig::strategy`].
pub struct XemmCoreLive {
    pub runtime: RuntimeAssembly,
}

/// Why a live start failed: a configuration refusal, or the runtime's own fault.
#[derive(Debug)]
pub enum XemmCoreError {
    Config(XemmConfigError),
    Runtime(RuntimeError),
}

impl std::fmt::Display for XemmCoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            XemmCoreError::Config(e) => write!(f, "{e}"),
            XemmCoreError::Runtime(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for XemmCoreError {}

/// The LIVE twin of [`build_paper_xemm_core`], on the REAL `assemble_runtime` core. Validated against
/// [`crate::ArmingTable::wired_markets`] over `runtime_config.backend`'s table, so an unwired or wrong-symbol taker venue
/// is a STARTUP error. Spawns NO feed (module doc: the caller wires three).
///
/// The round-trip fee the maker backs its quotes off by is the two ACCOUNTS' resolved rates
/// ([`xemm_fee_arm`]): what each venue reported at the start, else its static lane, which is what
/// [`validate_xemm_mount`] priced the mount at.
///
/// The maker is the CALLER's (no library builds a strategy): `maker` is handed the mount, `params`
/// and a round trip, once at the static fee once the mount validates and again from the fee arm
/// when the accounts' rates move it.
pub fn build_live_xemm_core(
    spec: &MountPlacement,
    params: &XemmParams,
    mut runtime_config: RuntimeConfig,
    maker: impl Fn(&MountPlacement, &XemmParams, f64) -> Box<dyn Strategy<LiveBroker> + Send> + 'static,
) -> Result<XemmCoreLive, XemmCoreError> {
    // The wired markets THIS backend mounts: the table's, not the wiring table's alone, so a taker
    // venue whose bridge the build lacks is a startup error rather than a silent paper hedge.
    let table = runtime_config
        .backend
        .arming_table()
        .map_err(|e| XemmCoreError::Runtime(crate::RuntimeError::Backend(e)))?;
    let wired = table.wired_markets();
    let total_fee = validate_xemm_mount(
        spec,
        params,
        Some((wired.as_slice(), &runtime_config.policy.accounts)),
    )
    .map_err(XemmCoreError::Config)?;
    let strategy = maker(spec, params, total_fee);
    spec.name_strategy_file(&mut runtime_config.core_config.mount_strategy_files);
    runtime_config.core_config.strategy =
        Some(MountBuild { strategy, spec: spec.clone() }.into_mount());
    let arm = xemm_fee_arm(spec.clone(), *params, total_fee, maker);
    Ok(XemmCoreLive {
        runtime: assemble_runtime_armed(runtime_config, arm).map_err(XemmCoreError::Runtime)?,
    })
}

/// The [`MountFeeRebuild`] that rebuilds the xEMM maker (through `maker`) at the round trip of its two
/// accounts' resolved schedules (maker venue's maker leg, taker venue's taker leg, each its default
/// account). The maker
/// keeps the `static_fee` it was built at, with a `warn`, when either engine is absent or the
/// resolved shapes have no flat fraction; it is rebuilt only when the fee moves.
fn xemm_fee_arm(
    spec: MountPlacement,
    params: XemmParams,
    static_fee: f64,
    maker: impl Fn(&MountPlacement, &XemmParams, f64) -> Box<dyn Strategy<LiveBroker> + Send> + 'static,
) -> MountFeeRebuild<'static> {
    Box::new(move |fees: &EngineFees, core: &mut vike_core::CoreConfig| {
        let taker_venue = spec.legs[0].venue.as_deref().unwrap_or(&spec.venue);
        let resolved = fees.of(&spec.venue, None).zip(fees.of(taker_venue, None));
        let fee = resolved.and_then(|(m, t)| vike_model::xemm_round_trip_fee(m, t, false));
        let Some(fee) = fee else {
            tracing::warn!(
                maker = %spec.venue,
                taker = %taker_venue,
                ?resolved,
                "xEMM round-trip fee kept at the static fee lanes: the accounts' fee rates are absent \
                 or have no flat fraction of price"
            );
            return;
        };
        if fee == static_fee {
            return;
        }
        tracing::info!(
            maker = %spec.venue,
            taker = %taker_venue,
            round_trip_fee_bps = fee * 1e4,
            "xEMM round-trip fee ARMED from the accounts' own fee rates"
        );
        if let Some(mount) = core.strategy.as_mut() {
            mount.strategy = maker(&spec, &params, fee);
        }
    })
}
