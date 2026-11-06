//! The LIVE cross-exchange mount: the maker folded into the wired-market runtime's core.

#[cfg(doc)]
use vike_core::CoreConfig;
use vike_core::StrategyMount;
#[cfg(doc)]
use vike_mm::XemmMaker;

#[cfg(doc)]
use super::build_paper_xemm_core;
use super::{XemmConfig, XemmConfigError, build_xemm_maker, xemm_mount_legs};
use crate::node::{AssembledRuntime, RuntimeConfig, RuntimeError, assemble_runtime};

/// The spawned LIVE cross-exchange mount: the wired-market [`AssembledRuntime`] with the [`XemmMaker`] folded
/// into its [`CoreConfig::strategy`].
pub struct LiveXemmCore {
    pub runtime: AssembledRuntime,
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

/// The LIVE twin of [`build_paper_xemm_core`], on the REAL [`assemble_runtime`] core. Validated against
/// [`crate::ArmingTable::wired_markets`] over `runtime_cfg.backend`'s table, so an unwired or wrong-symbol taker venue
/// is a STARTUP error. Spawns NO feed (module doc: the caller wires three).
pub fn build_live_xemm_core(
    cfg: &XemmConfig,
    mut runtime_cfg: RuntimeConfig,
) -> Result<LiveXemmCore, XemmCoreError> {
    // The wired markets THIS backend mounts: the table's, not the wiring table's alone, so a taker
    // venue whose bridge the build lacks is a startup error rather than a silent paper hedge.
    let table = runtime_cfg
        .backend
        .arming_table()
        .map_err(|e| XemmCoreError::Runtime(crate::RuntimeError::Backend(e)))?;
    let total_fee =
        cfg.validate(Some(table.wired_markets().as_slice())).map_err(XemmCoreError::Config)?;
    runtime_cfg.core_config.strategy = Some(StrategyMount {
        account: None,
        venue: cfg.maker_venue.clone(),
        symbol: cfg.maker_symbol.clone(),
        interval: cfg.interval.clone(),
        strategy: Box::new(build_xemm_maker(cfg, total_fee)),
        symbols: xemm_mount_legs(cfg),
        underlying_symbol: None,
        controller_id: None,
    });
    Ok(LiveXemmCore { runtime: assemble_runtime(runtime_cfg).map_err(XemmCoreError::Runtime)? })
}
