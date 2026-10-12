//! The live mount: a strategy folded into the wired-market node core, single- and multi-mount.

use vike_core::StrategyMount;
#[cfg(doc)]
use vike_core::{CoreConfig, CoreHandle};
#[cfg(doc)]
use vike_mm::SpreadMaker;

use crate::node::{Node, NodeConfig, NodeError, build_node, build_node_with_preflight};

use super::config::{MakerMountConfig, MountSpec};
use super::multi::StrategyMountSpec;
use super::paper::build_maker;
#[cfg(doc)]
use super::paper::{MakerMount, build_paper_maker_core};

/// The spawned LIVE maker mount: the wired-market [`Node`] with the A-S [`SpreadMaker`] in its
/// [`CoreConfig::strategy`]. Unlike [`MakerMount`], **no `fills` field**: the `ExecutionClient` is a
/// real venue, so fills surface only through the [`CoreHandle`]'s `CoreSnapshot` / the journal.
pub struct LiveMakerMount {
    /// Destructure for `handle` / `forwarder_stop` / `live_venues` as a [`build_node`] caller does.
    pub node: Node,
}

/// The live twin of [`build_paper_maker_core`]: the A-S [`SpreadMaker`] on the REAL wired-market
/// [`build_node`] core. It spawns **no feed**: the caller wires live `Feeds` onto `node.handle`'s
/// lanes (via [`vike_core::CoreLaneSink`]), as `vike-tradehub`'s `wire_venue_feeds` does. The rest
/// is [`build_live_strategy_core`]'s contract, with the mount on `(cfg.venue, cfg.token_id,
/// cfg.interval)`.
///
/// **Network-free by itself**: with an empty credentials map every venue mounts paper (proven by
/// `vike-tradehub/tests/daemon/live_gate_paper.rs`). [`NodeError`] is the forwarder-thread spawn.
pub fn build_live_maker_core(
    cfg: &MakerMountConfig,
    node_cfg: NodeConfig,
) -> Result<LiveMakerMount, NodeError> {
    build_live_strategy_core(Box::new(build_maker(cfg)), &cfg.mount_spec(), node_cfg)
}

/// [`build_live_maker_core`] with the strategy as a PARAMETER: what a headless daemon calls to mount
/// whatever its profile named. `strategy` is what
/// `vike_strategy::strategy_by_name::<vike_core::LiveBroker>` returns; `+ Send` because it moves
/// onto the core thread ([`StrategyMount::strategy`]).
///
/// The caller owns `node_cfg`, so its `core_config` carries the LIVE safety knobs
/// (`submit_ack_timeout` / `max_drawdown` / `margin_call`, …). This fn only folds the
/// [`StrategyMount`] in **before** [`build_node`] consumes it, so the mount lands on
/// `(spec.venue, spec.symbol, spec.interval)` and `spawn_core_multi` wires that engine's
/// applied-fill capture.
///
/// ⚠ It does NOT check that the venue's engine ACCEPTS `spec.symbol`: `crate::make_engine` sets no
/// `extra_symbols` ([`vike_exec::ExecutionEngine::accepts_symbol`]), so a foreign symbol's orders
/// and fills are SILENTLY dropped. The caller owns that check against [`NodeConfig::markets`].
pub fn build_live_strategy_core(
    strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
    spec: &MountSpec,
    node_cfg: NodeConfig,
) -> Result<LiveMakerMount, NodeError> {
    let mut node_cfg = node_cfg;
    fold_strategy_mount(&mut node_cfg, strategy, spec);
    Ok(LiveMakerMount { node: build_node(node_cfg)? })
}

/// [`build_live_strategy_core`] over an ALREADY-RUN preflight report (the twin of
/// [`build_node_with_preflight`]).
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
    spec: &MountSpec,
    node_cfg: NodeConfig,
    preflight: &crate::preflight::PreflightReport,
) -> Result<LiveMakerMount, NodeError> {
    let mut node_cfg = node_cfg;
    fold_strategy_mount(&mut node_cfg, strategy, spec);
    Ok(LiveMakerMount { node: build_node_with_preflight(node_cfg, preflight)? })
}

/// The two builders' shared body: fold the [`StrategyMount`] into the config `build_node` consumes.
pub(super) fn fold_strategy_mount(
    node_cfg: &mut NodeConfig,
    strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
    spec: &MountSpec,
) {
    node_cfg.core_config.strategy = Some(StrategyMount {
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

/// N strategies on the REAL [`build_node`] core: the multi-mount twin of
/// [`build_live_strategy_core`] (split-plane I10), what a `[[mounts]]` daemon profile mounts LIVE.
/// The first mount becomes [`CoreConfig::strategy`], the rest [`CoreConfig::extra_mounts`];
/// `spawn_core_multi` routes each mount's orders to its venue's engine ([`NodeConfig::markets`]).
///
/// Validates NOTHING about venue/symbol acceptance (the daemon's `validate_for_live` does, per
/// profile row; see [`build_live_strategy_core`]). PANICS on an empty `mounts`.
pub fn build_live_multi_strategy_core(
    mounts: Vec<StrategyMountSpec>,
    mut node_cfg: NodeConfig,
) -> Result<LiveMakerMount, NodeError> {
    assert!(!mounts.is_empty(), "a multi-strategy live mount needs at least one mount");
    let mut iter = mounts.into_iter();
    let first = iter.next().expect("asserted non-empty above");
    node_cfg.core_config.strategy = Some(first.into_mount());
    node_cfg.core_config.extra_mounts = iter.map(StrategyMountSpec::into_mount).collect();
    Ok(LiveMakerMount { node: build_node(node_cfg)? })
}
