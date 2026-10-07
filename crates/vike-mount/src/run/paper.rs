//! The paper mount: the halt-armed paper book, the A-S maker, and the single-strategy paper builders.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_core::{CoreConfig, CoreHandle, StrategyMount, spawn_core};
use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate, RiskLimits};
// Named by their own crates; the crate-root re-exports carry their reasons.
use vike_mm::{QuoteStyle, SpreadMaker};
#[cfg(doc)]
use vike_model::{RewardParams, ToxicityParams};
use vike_paper::{PaperExecutionClient, PaperFill};

use super::config::{MakerMountConfig, MountSpec};
#[cfg(doc)]
use super::live::build_live_strategy_core;
#[cfg(doc)]
use super::sink::MakerSink;

/// The spawned mount: the [`CoreHandle`] plus a clone of the paper client's `fills` `Arc` (the
/// client itself moved onto the core thread).
pub struct MakerMount {
    pub handle: CoreHandle,
    pub fills: Arc<Mutex<Vec<PaperFill>>>,
}

use vike_core::runtime::EquitySampleHook;

/// WHICH operator HALT sentinel a paper book built by this crate watches.
///
/// ⚠ **A MOUNT is armed BY DESIGN; a TEST driving a mount must not inherit the operator's kill
/// switch.** The process-wide path (`vike_bridge_core::halt::halt_path_from_env`:
/// `<project>/settings/state/HALT`, else `<exe_dir>/HALT`) is right for a daemon and makes a test's
/// verdict depend on a file on the box: MEASURED on the CI box, 22 tests across the workspace flipped
/// (5 here, e.g. `tests::taker_fee_of`'s callers as a fee panic that never mentions halt). Full
/// incident: `paper_client_for`'s doc.
///
/// No `Unarmed` variant: a test that wants no halt names a path it owns and never creates (and can
/// engage it deliberately, as `crates/vike-paper/tests/paper_halt.rs` does).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PaperHalt {
    /// The PROCESS-WIDE operator sentinel: every daemon mount's, and the `Default`, so a caller that
    /// says nothing is armed.
    #[default]
    ProcessWide,
    /// A caller-owned sentinel path. **TEST SEAM**, like
    /// `vike_bridge_core::exec_actor::ExecActor::with_halt_path` and
    /// `vike_ctrader::exec::CtraderExec::with_halt_path`: `halt_path_from_env` memoizes in a
    /// `OnceLock`, and this workspace does not `set_var` under threads.
    Pinned(PathBuf),
}

impl PaperHalt {
    /// The sentinel path a book built under this choice watches.
    pub fn resolve(&self) -> PathBuf {
        match self {
            PaperHalt::ProcessWide => vike_bridge_core::halt::halt_path_from_env(),
            PaperHalt::Pinned(path) => path.clone(),
        }
    }
}

/// Every OPT-IN knob of the paper mount beyond the [`MakerMountConfig`]: the ONE options bag for
/// [`build_paper_maker_core_with`] (audit F12). [`PaperMountOpts::default`] is the plain
/// [`build_paper_maker_core`] path, so struct-update composes any subset:
/// `PaperMountOpts { risk_limits, ..Default::default() }`.
///
/// The flag knobs (`oco_cancel_sibling_on_dead_exit`, `cancel_orders_on_shutdown`,
/// `strategy_factory`, `journal`) are here because `vike-tradehub` sets each on its LIVE mount, and
/// a paper rehearsal that ignored one would rehearse the wrong behaviour.
pub struct PaperMountOpts {
    /// The [`RiskLimits`] the mounted `RiskGate` is built from: the seam `vike-tradehub` arms the
    /// OPERATOR risk budget through (`max_notional_per_order`/`max_total_exposure`/
    /// `max_orders_per_window`/…, from a `vike_core::RunProfile` via
    /// `vike_core::ProfileRisk::apply_to`). Default `RiskLimits::new()`.
    pub risk_limits: RiskLimits,
    /// Equity sampler cadence (`vike_core::CoreConfig::equity_sample`); pair with
    /// [`Self::on_equity_sample`]. `None` ⇒ no timer armed.
    pub equity_sample: Option<Duration>,
    /// The equity sampler's sink (row shape: `vike_core::CoreConfig::on_equity_sample`'s doc, one
    /// row per engine plus `"TOTAL"`). ⚠ No production root wires it (vike-app's `RecorderSink`
    /// wiring went with its local core, #1727); the one caller is
    /// `crates/vike-mount/tests/mount_scripted.rs`'s
    /// `equity_sample_closure_fires_while_a_position_is_open`.
    pub on_equity_sample: Option<EquitySampleHook>,
    /// Strategy-state sidecar directory (`vike_core::CoreConfig::state_dir`): `Some` makes the
    /// [`SpreadMaker`]'s breaker trip count + A-S accumulator (`vike_mm`'s `save_state`/
    /// `load_state`) survive a restart; pair with [`Self::state_save`].
    pub state_dir: Option<PathBuf>,
    /// State-save cadence (`vike_core::CoreConfig::state_save`).
    pub state_save: Option<Duration>,
    /// Readiness gate (`vike_core::CoreConfig::readiness_gate`, whose doc has the trade-off): `true`
    /// keeps the mount `Pending` (buffered intents discarded) until `(venue, token_id)` prices.
    pub readiness_gate: bool,
    /// Cancel the surviving OCO sibling when a released bracket exit dies UNFILLED
    /// (`vike_core::CoreConfig::oco_cancel_sibling_on_dead_exit`; read live from
    /// `vike_config::Flags::oco_cancel_sibling_on_dead_exit`). `false` keeps protection. Inert for
    /// the A-S maker alone (no brackets); live once the order-control channel (TCP, Telegram)
    /// submits intents into the core.
    pub oco_cancel_sibling_on_dead_exit: bool,
    /// Cancel every resting order at shutdown teardown
    /// (`vike_core::CoreConfig::cancel_orders_on_shutdown`; live:
    /// `vike_config::Flags::cancel_orders_on_shutdown`). `false` leaves the book resting. On paper
    /// the cancels hit `PaperExecutionClient`'s own book, showing which orders a real stop pulls.
    pub cancel_orders_on_shutdown: bool,
    /// Which HALT sentinel the paper book watches ([`PaperHalt`]). Default
    /// [`PaperHalt::ProcessWide`], so a DAEMON that says nothing is armed; a TEST expecting fills
    /// pins a path it owns and never creates.
    pub halt: PaperHalt,
    /// RUNTIME strategy-mount resolver (`vike_core::CoreConfig::strategy_factory`, split-plane B5),
    /// armed live via `NodeConfig::core_config`. `None` refuses every runtime
    /// `Command::MountStrategy` with a recent-events note.
    pub strategy_factory: Option<vike_core::StrategyFactory>,
    /// The write-ahead command JOURNAL, already resolved by the caller (`vike-tradehub`, once per
    /// process: `crates/vike-tradehub/src/tradehub_cli/flags.rs`'s `journal_vars`, where the
    /// ENVIRONMENT still beats the file). This is how `config.journal_dir` reaches a paper mount.
    /// `None` falls back to [`vike_core::journal_config_from_env`].
    pub journal: Option<vike_core::JournalConfig>,
}

impl Default for PaperMountOpts {
    /// `RiskLimits::new()`, deliberately NOT `RiskLimits::default()` (`new()` sets
    /// `window_ms: 1000`); every other knob off; HALT at [`PaperHalt::ProcessWide`], the ARMED
    /// default: the alternative silently disarms the kill switch on the shipped paper daemon.
    fn default() -> Self {
        PaperMountOpts {
            risk_limits: RiskLimits::new(),
            equity_sample: None,
            on_equity_sample: None,
            state_dir: None,
            state_save: None,
            readiness_gate: false,
            oco_cancel_sibling_on_dead_exit: false,
            cancel_orders_on_shutdown: false,
            halt: PaperHalt::ProcessWide,
            strategy_factory: None,
            journal: None,
        }
    }
}

/// Build + spawn the paper maker core: `PaperExecutionClient` behind the `ExecutionClient` seam, the
/// A-S [`SpreadMaker`] via [`StrategyMount`], on [`spawn_core`] (the PRODUCTION runtime). No venue
/// code: the daemon and the offline tests differ ONLY in what feeds the handle through a
/// [`MakerSink`]. Exactly `build_paper_maker_core_with(cfg, PaperMountOpts::default())`.
pub fn build_paper_maker_core(cfg: &MakerMountConfig) -> MakerMount {
    build_paper_maker_core_with(cfg, PaperMountOpts::default())
}

/// The mount's paper book, with the operator HALT kill switch ARMED (a MOUNT, not a simulation).
/// Fee model: the per-venue [`vike_model::fee_schedule_for`] registry schedule (Polymarket ⇒
/// `Free`) unless the config carries a non-zero `maker_fee`/`taker_fee`, which keeps the flat-rate
/// [`PaperExecutionClient::new`] path (the constructors seed `0.0` so the registry applies).
///
/// ⚠ **The seam the shipped paper daemon uses.** `vike-tradehub`'s paper variant never touches
/// `crate::make_engine`, so arming the fallback arms there left `touch
/// <project>/settings/state/HALT` doing nothing, silently, on the very node an operator rehearses
/// the switch on. Both seams resolve the SAME `vike_bridge_core::halt::halt_path_from_env`.
/// The BACKTEST path gets none (`vike-backtest`'s r7 gate builds the client directly), so the
/// backtest == paper law cannot depend on a file on disk (`crates/vike-paper/src/lib.rs`'s
/// `with_halt_path`).
///
/// ⚠ **WHICH sentinel is a PARAMETER ([`PaperHalt`]) because arming broke tests CI CANNOT SEE.**
/// A HALT file on the box refused `tests::taker_fee_of`'s opening order, so both callers panicked on
/// `assert_eq!(fills.len(), 1, …)`, a fee assertion that never mentions halt (the trigger is the
/// operator's own `/srv/vike-<unit>/settings/state/HALT`). MEASURED on the CI box (via the
/// `VIKE_HALT_FILE` override decision 0099 later retired): 22 tests red over the CI roster, 5 in
/// this crate, against 7147/7147 green without the file. Same defect class as
/// `crates/vike-paper/tests/paper_halt_process_wide.rs` and
/// `crates/vike-ops/tests/wiring/paper_mount_arming_gate.rs`'s rule 3. Daemon callers pass the
/// `Default` [`PaperHalt::ProcessWide`].
///
/// ⚠ It takes a [`MountSpec`], not a [`MakerMountConfig`]: the arming must not depend on which
/// strategy was named. [`build_paper_strategy_core_with`] is the single caller, so a registry mount
/// (`[strategy] name = "grid"`) and the default maker reach the same armed book.
pub(super) fn paper_client_for(spec: &MountSpec, halt: &PaperHalt) -> PaperExecutionClient {
    let book = if spec.maker_fee != 0.0 || spec.taker_fee != 0.0 {
        PaperExecutionClient::new(
            &spec.venue,
            &spec.symbol,
            spec.slippage,
            spec.maker_fee,
            spec.taker_fee,
        )
    } else {
        PaperExecutionClient::with_fee_schedule(
            &spec.venue,
            &spec.symbol,
            spec.slippage,
            // LANE-keyed (`vike_catalog::fee_lane`): a `.P` symbol on binance/aster is the PERP
            // lane, priced apart from spot; identity for every other symbol.
            vike_model::fee_schedule_for(vike_catalog::fee_lane(&spec.venue, &spec.symbol)),
        )
    };
    book.with_halt_path(halt.resolve())
}

/// The A-S [`SpreadMaker`] `cfg` means: the ONE definition, so `vike-tradehub`'s default mount
/// boxes THIS rather than re-deriving the knob folding (and a test can read the maker's `reward`,
/// unobservable once `spawn_core` moves it). `with_quote_style` sets ONLY the L1 tick grid
/// (`QuoteStyle::Mid`/`depth_levels` are ignored while A-S prices).
///
/// Each opt-in is applied ONLY when its field is `Some`, so a default mount never calls
/// [`SpreadMaker::with_liquidity_rewards`] / [`SpreadMaker::with_flow_toxicity`] /
/// [`SpreadMaker::with_skew`] / [`SpreadMaker::with_fill_breaker`] /
/// [`SpreadMaker::with_refresh_tolerance`]. A `Some` can still be inert: `weight == 0` per
/// [`RewardParams`], both knobs `0.0` per [`ToxicityParams`], and the toxicity guard while nothing
/// feeds `on_flow` ([`MakerMountConfig::toxicity`]).
pub fn build_maker(cfg: &MakerMountConfig) -> SpreadMaker {
    let mut maker = SpreadMaker::new(cfg.qty, cfg.half_spread)
        .with_quote_style(QuoteStyle::Mid, 1, cfg.tick_size)
        .with_avellaneda_stoikov(cfg.as_params);
    if let Some(reward) = cfg.reward {
        maker = maker.with_liquidity_rewards(reward);
    }
    if let Some(toxicity) = cfg.toxicity {
        maker = maker.with_flow_toxicity(toxicity);
    }
    if let Some(s) = cfg.skew {
        maker = maker.with_skew(s.target_inventory, s.max_inventory, s.skew);
    }
    if let Some(b) = cfg.breaker {
        maker =
            maker.with_fill_breaker(b.fill_window_ms, b.net_fill_threshold, b.suppress_cooldown_ms);
    }
    if let Some(t) = cfg.refresh_tolerance {
        maker = maker.with_refresh_tolerance(t.price_bps, t.size_bps);
    }
    maker
}

/// [`build_paper_maker_core`] with any subset of the [`PaperMountOpts`] knobs armed (audit F12;
/// `PaperMountOpts { risk_limits, ..Default::default() }` is `vike-tradehub`'s budget seam).
pub fn build_paper_maker_core_with(cfg: &MakerMountConfig, opts: PaperMountOpts) -> MakerMount {
    build_paper_strategy_core_with(Box::new(build_maker(cfg)), &cfg.mount_spec(), opts)
}

/// [`build_paper_maker_core_with`] with the strategy as a PARAMETER: the paper twin of
/// [`build_live_strategy_core`], the rehearsal path for whatever a daemon profile named. Builds the
/// paper client, `Account`, `RiskGate` and the ONE [`CoreConfig`] literal, and spawns it.
pub fn build_paper_strategy_core_with(
    strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
    spec: &MountSpec,
    opts: PaperMountOpts,
) -> MakerMount {
    let PaperMountOpts {
        risk_limits,
        equity_sample,
        on_equity_sample,
        state_dir,
        state_save,
        readiness_gate,
        oco_cancel_sibling_on_dead_exit,
        cancel_orders_on_shutdown,
        halt,
        strategy_factory,
        journal,
    } = opts;
    // TRIPWIRE. `paper_client_for`'s SINGLE-symbol book stamps its own symbol onto every fill
    // (`emit_fill`), so a multi-symbol paper rehearsal would book both legs under one symbol and
    // look correct while the live core routed them apart. Unreachable today ([`MountSpec::legs`]
    // is empty on every spec built here); whoever adds a two-leg mount surface must switch this
    // builder to `vike_paper::MultiPaperExecutionClient` (routes `submit` by `request.symbol`,
    // rejects an unknown one with a terminal `OrderRejected`).
    // ⚠ It reads the CONFIGURABLE `spec.legs`, so it fires the moment a config sets a leg.
    assert!(
        spec.legs.is_empty(),
        "paper rehearsal of a multi-symbol mount needs MultiPaperExecutionClient: a \
         single-symbol paper book stamps its own symbol onto every fill and would hide the \
         misrouting this rehearsal exists to catch"
    );
    let client = paper_client_for(spec, &halt);
    let fills = Arc::clone(&client.fills);
    let engine = ExecutionEngine::new(
        Account::new(1.0, &spec.venue, None, BalanceMode::Delta),
        RiskGate::new(risk_limits),
        client,
        &spec.venue,
        &spec.symbol,
    );
    let config = CoreConfig {
        seed_cash: spec.seed_cash,
        strategy: Some(StrategyMount {
            account: None,
            symbols: spec.legs.clone(),
            controller_id: spec.controller_id.clone(),
            venue: spec.venue.clone(),
            symbol: spec.symbol.clone(),
            interval: spec.interval.clone(),
            strategy,
            // "Option B": this underlying's marks reach `on_mark`; `None` ⇒ no routing.
            underlying_symbol: spec.underlying_symbol.clone(),
        }),
        equity_sample,
        on_equity_sample,
        state_dir,
        state_save,
        readiness_gate,
        // Write-ahead journal: the env fallback is OFF unless `VIKE_JOURNAL_DIR` /
        // `VIKE_RUN_PROFILE` is set; enabled, the paper fills (via `pump_client`) land on disk.
        // ⚠ The CALLER's resolved answer first (how `config.journal_dir` enables the WAL on a
        // paper mount), else the process env (what every caller passing no `journal` gets).
        journal: journal.or_else(vike_core::journal_config_from_env),
        // Both off by default (`PaperMountOpts`): book left resting at teardown.
        oco_cancel_sibling_on_dead_exit,
        cancel_orders_on_shutdown,
        // `None` ⇒ runtime `Command::MountStrategy` refuses (the opts field).
        strategy_factory,
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    MakerMount { handle, fills }
}
