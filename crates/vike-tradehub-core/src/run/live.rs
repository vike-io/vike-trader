//! The live mount: a strategy folded into the wired-market runtime core, single- and multi-mount,
//! and the fee re-arm a mount's strategy may take once its account's rate has been read.

use vike_core::Mount;
#[cfg(doc)]
use vike_core::{CoreConfig, CoreHandle};

use crate::node::{
    EngineFees, MountFeeRebuild, RuntimeAssembly, RuntimeConfig, RuntimeError, assemble_runtime,
    assemble_runtime_armed, assemble_runtime_with_preflight,
};

use super::config::MountPlacement;
use super::multi::MountBuild;
#[cfg(doc)]
use super::paper::StrategyCorePaper;

/// The spawned LIVE mount: the wired-market [`RuntimeAssembly`] with the mounted strategy in its
/// [`CoreConfig::strategy`]. Unlike [`StrategyCorePaper`], **no `fills` field**: the `ExecutionClient` is a
/// real venue, so fills surface only through the [`CoreHandle`]'s `CoreSnapshot` / the journal.
pub struct StrategyCoreLive {
    /// Destructure for `handle` / `forwarder_stop` / `live_venues` as a [`assemble_runtime`] caller does.
    pub runtime: RuntimeAssembly,
}

/// What rebuilds ONE mount's strategy at its account's own fee schedule ([`EngineFees::of`]: the
/// rate the venue reported at the start, else the static lane), called once after every engine is
/// built and before the core is spawned. `None` keeps the strategy the mount was built with, bit for
/// bit (a paper account, a venue that reports no rate, a rate that moves nothing). The A-S maker's
/// is `vike_market_making::rearm_fee_floor` over its params bag.
pub type FeeRearm = Box<
    dyn FnOnce(
        Option<vike_model::FeeSchedule>,
    ) -> Option<Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>>,
>;

/// The [`MountFeeRebuild`] that hands each mount's [`FeeRearm`] its account's resolved fee schedule:
/// `rearms[i]` is the `i`th mount ([`vike_core::CoreConfig::strategy`], then `extra_mounts`), `None`
/// for a mount nothing re-arms. It replaces a mount's STRATEGY only, never its venue, symbol or
/// account (already judged against the engines).
pub fn fee_rearm_mounts(rearms: Vec<Option<FeeRearm>>) -> MountFeeRebuild<'static> {
    Box::new(move |fees: &EngineFees, core: &mut vike_core::CoreConfig| {
        let mounts = core.strategy.iter_mut().chain(core.extra_mounts.iter_mut());
        for (mount, rearm) in mounts.zip(rearms) {
            let Some(rearm) = rearm else { continue };
            if let Some(strategy) = rearm(fees.of(&mount.venue, mount.account.as_ref())) {
                mount.strategy = strategy;
            }
        }
    })
}

/// A strategy on the REAL wired-market [`assemble_runtime`] core: what a headless daemon calls to
/// mount whatever its profile named. It spawns **no feed**: the caller wires live feeds onto
/// `runtime.handle`'s lanes (via [`crate::CoreLaneSink`]), as `vike-tradehub`'s
/// `wire_venue_feeds` does. **Network-free by itself**: with an empty credentials map no
/// `demo`/`live` account runs. `strategy` is what the daemon's mount resolution returns for the
/// profile's strategy file; `+ Send` because it moves
/// onto the core thread ([`Mount::strategy`]).
///
/// The caller owns `runtime_config`, so its `core_config` carries the LIVE safety knobs
/// (`submit_ack_timeout` / `max_drawdown` / `margin_call`, …). This fn only folds the
/// [`Mount`] in **before** [`assemble_runtime`] consumes it, so the mount lands on
/// `(spec.venue, spec.symbol, spec.interval)` and `spawn_core_multi` wires that engine's
/// applied-fill capture.
///
/// ⚠ It does NOT check that the venue's engine ACCEPTS `spec.symbol`: `crate::make_engine` sets no
/// `extra_symbols` ([`vike_execution::Engine::accepts_symbol`]), so a foreign symbol's orders
/// and fills are SILENTLY dropped. The caller owns that check against [`crate::ArmingTable::wired_markets`] over `runtime_config.backend`'s table.
pub fn build_live_strategy_core(
    strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
    spec: &MountPlacement,
    runtime_config: RuntimeConfig,
) -> Result<StrategyCoreLive, RuntimeError> {
    let mut runtime_config = runtime_config;
    fold_strategy_mount(&mut runtime_config, strategy, spec);
    Ok(StrategyCoreLive { runtime: assemble_runtime(runtime_config)? })
}

/// [`build_live_strategy_core`] over an ALREADY-RUN preflight report (the twin of
/// [`assemble_runtime_with_preflight`]).
///
/// ⚠ The real preflight takes a test's planted LIVE intent away when the venue refuses the probe,
/// so a risk-budget refusal test silently stops testing (measured:
/// `a_live_rhai_mount_without_a_risk_budget_is_refused_pre_connect` asserted nothing, and made a
/// real signed bybit read with a fake key). An EMPTY [`crate::preflight::PreflightReport`] restores
/// the intent and makes such a test NETWORK-FREE.
///
/// ⚠ Production callers must use [`build_live_strategy_core`]: an empty report is "no preflight
/// ran", right only for a caller not mounting a real venue.
pub fn build_live_strategy_core_with_preflight(
    strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
    spec: &MountPlacement,
    runtime_config: RuntimeConfig,
    preflight: &crate::preflight::PreflightReport,
) -> Result<StrategyCoreLive, RuntimeError> {
    let mut runtime_config = runtime_config;
    fold_strategy_mount(&mut runtime_config, strategy, spec);
    Ok(StrategyCoreLive { runtime: assemble_runtime_with_preflight(runtime_config, preflight)? })
}

/// The two builders' shared body: fold the [`Mount`] into the config `assemble_runtime` consumes.
pub(super) fn fold_strategy_mount(
    runtime_config: &mut RuntimeConfig,
    strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
    spec: &MountPlacement,
) {
    let core = &mut runtime_config.core_config;
    spec.name_strategy_file(&mut core.mount_strategy_files, &mut core.mount_fills_at_close);
    runtime_config.core_config.strategy = Some(Mount {
        order_client_id_code: spec.order_client_id_code,
        // `None` = the venue's default account.
        account: spec.account.clone(),
        symbols: spec.legs.clone(),
        controller_id: spec.controller_id.clone(),
        venue: spec.venue.clone(),
        symbol: spec.symbol.clone(),
        interval: spec.interval.clone(),
        strategy,
        // "Option B" cross-symbol routing (`None` ⇒ inert).
        underlying_symbol: spec.underlying_symbol.clone(),
    });
}

/// N strategies on the REAL [`assemble_runtime`] core: the multi-mount twin of
/// [`build_live_strategy_core`] (split-plane I10), what a `[[mounts]]` daemon profile mounts LIVE.
/// The first mount becomes [`CoreConfig::strategy`], the rest [`CoreConfig::extra_mounts`];
/// `spawn_core_multi` routes each mount's orders to its venue's engine ([`crate::wired_markets`]).
///
/// Validates NOTHING about venue/symbol acceptance (the daemon's `validate_for_live` does, per
/// profile row; see [`build_live_strategy_core`]). PANICS on an empty `mounts`.
pub fn build_live_multi_strategy_core(
    mounts: Vec<MountBuild>,
    mut runtime_config: RuntimeConfig,
) -> Result<StrategyCoreLive, RuntimeError> {
    fold_mounts(&mut runtime_config, mounts);
    Ok(StrategyCoreLive { runtime: assemble_runtime(runtime_config)? })
}

/// [`build_live_multi_strategy_core`] whose mounts may be rebuilt at their accounts' own fee rates once
/// every engine has resolved its own (`rearms[i]` for the `i`th of `mounts`, [`fee_rearm_mounts`]):
/// what the tradehub's live node mounts through. PANICS on an empty `mounts`.
pub fn build_live_multi_strategy_core_armed(
    mounts: Vec<MountBuild>,
    rearms: Vec<Option<FeeRearm>>,
    mut runtime_config: RuntimeConfig,
) -> Result<StrategyCoreLive, RuntimeError> {
    fold_mounts(&mut runtime_config, mounts);
    Ok(StrategyCoreLive {
        runtime: assemble_runtime_armed(runtime_config, fee_rearm_mounts(rearms))?,
    })
}

/// The first mount becomes [`vike_core::CoreConfig::strategy`], the rest `extra_mounts`.
fn fold_mounts(runtime_config: &mut RuntimeConfig, mounts: Vec<MountBuild>) {
    assert!(!mounts.is_empty(), "a multi-strategy live mount needs at least one mount");
    for m in &mounts {
        let core = &mut runtime_config.core_config;
        m.spec.name_strategy_file(&mut core.mount_strategy_files, &mut core.mount_fills_at_close);
    }
    let mut iter = mounts.into_iter();
    let first = iter.next().expect("asserted non-empty above");
    runtime_config.core_config.strategy = Some(first.into_mount());
    runtime_config.core_config.extra_mounts = iter.map(MountBuild::into_mount).collect();
}
