//! The paper mount: the halt-armed paper book and the single-strategy paper builder.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_core::{CoreConfig, CoreHandle, Mount, spawn_core};
use vike_execution::{Account, BalanceMode, Engine, RiskGate};
use vike_model::{Fill, RiskLimits};
use vike_paper::ExecutionClientPaper;

use super::config::MountPlacement;
#[cfg(doc)]
use super::live::build_live_strategy_core;
#[cfg(doc)]
use super::sink::TickSink;

/// The spawned mount: the [`CoreHandle`] plus a clone of the paper client's `fills` `Arc` (the
/// client itself moved onto the core thread).
pub struct StrategyCorePaper {
    pub handle: CoreHandle,
    pub fills: Arc<Mutex<Vec<Fill>>>,
}

use vike_core::runtime::EquitySampleHook;

/// WHICH operator HALT sentinel a paper book built by this crate watches.
///
/// ⚠ **A MOUNT is armed BY DESIGN; a TEST driving a mount must not inherit the operator's kill
/// switch.** The process-wide path (`vike_boot::halt::halt_path_from_env`:
/// `<project>/settings/state/HALT`, else `<exe_dir>/HALT`) is right for a daemon and makes a test's
/// verdict depend on a file on the box: MEASURED on the CI box, 22 tests across the workspace flipped
/// (5 here, e.g. `tests::taker_fee_of`'s callers as a fee panic that never mentions halt). Full
/// incident: `paper_client_for`'s doc.
///
/// No `Unarmed` variant: a test that wants no halt names a path it owns and never creates (and can
/// engage it deliberately, as `crates/vike-paper/tests/paper_halt.rs` does).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum StrategyCorePaperHalt {
    /// The PROCESS-WIDE operator sentinel: every daemon mount's, and the `Default`, so a caller that
    /// says nothing is armed.
    #[default]
    ProcessWide,
    /// A caller-owned sentinel path. **TEST SEAM**, like
    /// `vike_bridge_core::exec_actor::ExecutionClientActor::with_halt_path` and
    /// `vike_ctrader::execution::ExecutionClientCtrader::with_halt_path`: `halt_path_from_env` memoizes in a
    /// `OnceLock`, and this workspace does not `set_var` under threads.
    Pinned(PathBuf),
}

impl StrategyCorePaperHalt {
    /// The sentinel path a book built under this choice watches.
    pub fn resolve(&self) -> PathBuf {
        match self {
            StrategyCorePaperHalt::ProcessWide => vike_boot::halt::halt_path_from_env(),
            StrategyCorePaperHalt::Pinned(path) => path.clone(),
        }
    }
}

/// Every OPT-IN knob of the paper mount beyond the strategy and its [`MountPlacement`]: the ONE options
/// bag for [`build_paper_strategy_core_with`] (audit F12). [`StrategyCorePaperConfig::default`] is the plain
/// path, so struct-update composes any subset:
/// `StrategyCorePaperConfig { risk_limits, ..Default::default() }`.
///
/// The flag knobs (`oco_cancel_sibling_on_dead_exit`, `cancel_orders_on_shutdown`,
/// `strategy_factory`, `journal`) are here because `vike-tradehub` sets each on its LIVE mount, and
/// a paper rehearsal that ignored one would rehearse the wrong behaviour.
///
/// ⚠ It carries NO mount budget, the residual decision 0249 item 9 declares: neither the loss
/// latch nor the leverage cap (`vike_execution::MountBudget::max_leverage`) judges a mount this
/// builder (or `build_paper_multi_strategy_core_with`) runs, so an opening order a live mount's cap
/// refuses is sent here. The live mount and the backtest (`engine.max_mount_leverage`) judge it.
pub struct StrategyCorePaperConfig {
    /// The [`RiskLimits`] the mounted `RiskGate` is built from: the seam `vike-tradehub` arms the
    /// OPERATOR risk budget through (`max_notional_per_order`/`max_total_exposure`/
    /// `max_orders_per_window`/…, from a `vike_config::ProfileRun` via
    /// `vike_model::ProfileRisk::apply_to`). Default `RiskLimits::new()`.
    pub risk_limits: RiskLimits,
    /// Equity sampler cadence (`vike_core::CoreConfig::equity_sample`); pair with
    /// [`Self::on_equity_sample`]. `None` ⇒ no timer armed.
    pub equity_sample: Option<Duration>,
    /// The equity sampler's sink (row shape: `vike_core::CoreConfig::on_equity_sample`'s doc, one
    /// row per engine plus `"TOTAL"`). ⚠ No production root wires it (vike-app's `RecorderSink`
    /// wiring went with its local core, #1727); the one caller is
    /// `crates/vike-tradehub-core/tests/scripted/mount_scripted.rs`'s
    /// `equity_sample_closure_fires_while_a_position_is_open`.
    pub on_equity_sample: Option<EquitySampleHook>,
    /// Strategy-state sidecar directory (`vike_core::CoreConfig::state_dir`): `Some` makes the
    /// mounted strategy's `save_state` (the spread maker file's breaker trip count + A-S
    /// accumulator) survive a restart; pair with [`Self::state_save`].
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
    /// the cancels hit `ExecutionClientPaper`'s own book, showing which orders a real stop pulls.
    pub cancel_orders_on_shutdown: bool,
    /// Which HALT sentinel the paper book watches ([`StrategyCorePaperHalt`]). Default
    /// [`StrategyCorePaperHalt::ProcessWide`], so a DAEMON that says nothing is armed; a TEST expecting fills
    /// pins a path it owns and never creates.
    pub halt: StrategyCorePaperHalt,
    /// RUNTIME strategy-mount resolver (`vike_core::CoreConfig::strategy_factory`, split-plane B5),
    /// armed live via `RuntimeConfig::core_config`. `None` refuses every runtime
    /// `Command::MountStrategy` with a recent-events note.
    pub strategy_factory: Option<vike_core::StrategyFactory>,
    /// The write-ahead command JOURNAL, already resolved by the caller (`vike-tradehub`, once per
    /// process: `crates/vike-tradehub/src/profile_rows.rs`'s `journal_config_for` over the
    /// `config.journal_dir` and `config.journal_snapshot_every` rows, tuned by the active run row's
    /// `[sinks.journal]`). This is how `config.journal_dir` reaches a paper mount. `None` is
    /// journaling OFF: this builder reads no environment and no profile of its own, so the caller's
    /// answer is the only one.
    pub journal: Option<vike_core::JournalConfig>,
    /// The order-ownership file (`vike_core::CoreConfig::order_owners`, decision 0113): which mount
    /// owns each resting order, and each mount's ledger, across a restart. `vike-tradehub` passes
    /// the one its live arm passes, so a rehearsal's resting orders find their mount after a restart
    /// the way the live node's do. `None` reads and writes nothing.
    pub order_owners: Option<vike_core::order_owners::OrderOwnerLog>,
    /// What the JOURNAL restores into this core (`vike_core::replay::restore_for_boot`, decision
    /// 0127), computed ONCE by the caller (`vike-tradehub`'s `run`, the same value its live arm would
    /// get) and applied with `RestoreReport::apply_to`, so the paper and the live mount fill their
    /// `CoreConfig` through one function. `None` restores nothing from a journal.
    pub journal_restore: Option<vike_core::replay::RestoreReport>,
    /// Charge the paper engines the venue's public funding rate (`vike_core::CoreConfig::charge_paper_funding`;
    /// `vike-tradehub` sets it, as its live arm does). `false` (the default) books no funding from the rate.
    pub charge_paper_funding: bool,
    /// The venue's stored funding settlements a restarted paper engine catches up from
    /// (`vike_core::CoreConfig::funding_history`; `vike-tradehub` hands in the datahub's strategy-lane read,
    /// the one its live arm hands in). `None` keeps the old restart: the first missed settlement only.
    pub funding_history: Option<vike_core::FundingHistory>,
    /// Where a mount's INPUTS are read (`vike_core::CoreConfig::input_opener`, decision 0271; `vike-tradehub`
    /// hands in the datahub's read, the one its live arm hands in, so a paper mount reads every input a live
    /// one does). `None` refuses, by name, a mount whose strategy declares an input.
    pub input_opener: Option<vike_core::InputOpener>,
    /// The KEPT rows a restart's closed trades read a market's class from (`vike_core::CoreConfig::session_rows`;
    /// `vike-tradehub` hands in what its live arm hands in). Empty: every market's class is unknown.
    pub session_rows: Vec<vike_model::Instrument>,
    /// The mounts whose strategy file is still BUILDING, by mount id (`vike_core::CoreConfig::mounts_awaiting_strategy`):
    /// the core parks them and mounts each when its strategy arrives. Empty: every mount runs at the start.
    pub mounts_awaiting_strategy: Vec<String>,
    /// **The ONE starting balance every paper engine starts on, when set** (the owner's ruling of 2026-11-16: the shipped
    /// daemon hands [`crate::PAPER_SEED_CASH`], so a mount's own `seed_cash` gives no paper book another figure). `None`
    /// (the default, a rehearsal's or a test's builder): each mount's `MountPlacement::seed_cash`, summed per venue.
    pub account_seed: Option<f64>,
    /// Journal the forecasts the mounted strategies publish (`vike_core::CoreConfig::journal_forecasts`: the run
    /// profile's `sinks.forecasts`, which `vike-tradehub` hands in as its live arm reads it). `false` (the
    /// default) journals none, and the journal's format never moves for one.
    pub journal_forecasts: bool,
    /// This install's past order client id sessions (`vike_core::CoreConfig::order_client_id_past_sessions`:
    /// `vike-tradehub` hands in its `vike.db` rows, as its live arm does), so a late fill on an id one of them
    /// minted is reported as this install's. Empty (the default): none known.
    pub order_client_id_past_sessions: Vec<(String, u64)>,
    /// Where the session this core mints under is recorded before it mints
    /// (`vike_core::CoreConfig::order_client_id_session_recorder`: `vike-tradehub` hands in its `vike.db` writer, as its
    /// live arm does). `None` (the default): nothing is recorded.
    pub order_client_id_session_recorder: Option<vike_core::OrderClientIdSessionRecorder>,
}

impl Default for StrategyCorePaperConfig {
    /// `RiskLimits::new()`, deliberately NOT `RiskLimits::default()` (`new()` sets
    /// `window_ms: 1000`); every other knob off; HALT at [`StrategyCorePaperHalt::ProcessWide`], the ARMED
    /// default: the alternative silently disarms the kill switch on the shipped paper daemon.
    fn default() -> Self {
        StrategyCorePaperConfig {
            risk_limits: RiskLimits::new(),
            equity_sample: None,
            on_equity_sample: None,
            state_dir: None,
            state_save: None,
            readiness_gate: false,
            oco_cancel_sibling_on_dead_exit: false,
            cancel_orders_on_shutdown: false,
            halt: StrategyCorePaperHalt::ProcessWide,
            strategy_factory: None,
            journal: None,
            order_owners: None,
            journal_restore: None,
            charge_paper_funding: false,
            funding_history: None,
            input_opener: None,
            session_rows: Vec::new(),
            mounts_awaiting_strategy: Vec::new(),
            account_seed: None,
            journal_forecasts: false,
            order_client_id_past_sessions: Vec::new(),
            order_client_id_session_recorder: None,
        }
    }
}

/// The mount's paper book, with the operator HALT kill switch ARMED (a MOUNT, not a simulation).
/// Fee model: the per-venue [`vike_venues::fee_schedule_for`] registry schedule (Polymarket ⇒
/// `Free`) unless the config carries a non-zero `maker_fee`/`taker_fee`, which keeps the flat-rate
/// [`ExecutionClientPaper::new`] path (the constructors seed `0.0` so the registry applies).
///
/// ⚠ **The seam the shipped paper daemon uses.** `vike-tradehub`'s paper variant never touches
/// `crate::make_engine`, so arming the fallback arms there left `touch
/// <project>/settings/state/HALT` doing nothing, silently, on the very node an operator rehearses
/// the switch on. Both seams resolve the SAME `vike_boot::halt::halt_path_from_env`.
/// The BACKTEST path gets none (`vike-backtest`'s r7 gate builds the client directly), so the
/// backtest == paper law cannot depend on a file on disk (`crates/vike-paper/src/client.rs`'s
/// `with_halt_path`).
///
/// ⚠ **WHICH sentinel is a PARAMETER ([`StrategyCorePaperHalt`]) because arming broke tests CI CANNOT SEE.**
/// A HALT file on the box refused `tests::taker_fee_of`'s opening order, so both callers panicked on
/// `assert_eq!(fills.len(), 1, …)`, a fee assertion that never mentions halt (the trigger is the
/// operator's own `/srv/vike-<unit>/settings/state/HALT`). MEASURED on the CI box (via the
/// `VIKE_HALT_FILE` override decision 0099 later retired): 22 tests red over the CI roster, 5 in
/// this crate, against 7147/7147 green without the file. Same defect class as
/// `crates/vike-paper/tests/paper_halt_process_wide.rs` and
/// `crates/vike-ops/tests/architecture/paper_mount_arming_gate.rs`'s rule 3. Daemon callers pass the
/// `Default` [`StrategyCorePaperHalt::ProcessWide`].
///
/// ⚠ It takes the strategy-free [`MountPlacement`]: the arming must not depend on which strategy was
/// named. [`build_paper_strategy_core_with`] is the single caller, so a registry mount
/// (`[strategy] name = "grid"`) and the default maker reach the same armed book.
pub(super) fn paper_client_for(
    spec: &MountPlacement,
    halt: &StrategyCorePaperHalt,
) -> ExecutionClientPaper {
    paper_book(&spec.venue, &spec.symbol, spec, halt)
}

/// The paper book of `symbol` on `venue`, at `spec`'s fee and slippage scalars: the mount's own
/// ([`paper_client_for`]) or one of its declared legs', a cross-venue leg's on the leg's `venue`.
pub(crate) fn paper_book(
    venue: &str,
    symbol: &str,
    spec: &MountPlacement,
    halt: &StrategyCorePaperHalt,
) -> ExecutionClientPaper {
    let book = if spec.maker_fee != 0.0 || spec.taker_fee != 0.0 {
        ExecutionClientPaper::new(venue, symbol, spec.slippage, spec.maker_fee, spec.taker_fee)
    } else {
        ExecutionClientPaper::with_fee_schedule(
            venue,
            symbol,
            spec.slippage,
            // LANE-keyed (`vike_venues::fee_lane`): a `.P` symbol on binance/aster is the PERP
            // lane, priced apart from spot; identity for every other symbol.
            vike_venues::fee_schedule_for(crate::engine::kept_fee_lane(venue, symbol)),
        )
    };
    book.with_halt_path(halt.resolve())
}

/// Build + spawn a paper core: `ExecutionClientPaper` behind the `ExecutionClient` seam, `strategy`
/// via [`Mount`] on `spec`, on [`spawn_core`] (the PRODUCTION runtime), with any subset of the
/// [`StrategyCorePaperConfig`] knobs armed (`StrategyCorePaperConfig { risk_limits, ..Default::default() }` is
/// `vike-tradehub`'s budget seam). The paper twin of [`build_live_strategy_core`], the rehearsal path
/// for whatever a daemon profile named: no venue code, so the daemon and the offline tests differ
/// ONLY in what feeds the handle through a [`TickSink`]. Builds the paper client, `Account`,
/// `RiskGate` and the ONE [`CoreConfig`] literal, and spawns it.
///
/// A mount that declares LEGS ([`MountPlacement::legs`]) is built by
/// [`crate::build_paper_multi_strategy_core_with`] instead, which gives each `(venue, symbol)`
/// its own paper book: a single-symbol book stamps its own symbol onto every fill and would book both
/// legs under one. `fills` is then the mount's OWN book's log, and the mount trades the venue's one
/// paper account, as here (a leg on another venue, that venue's).
pub fn build_paper_strategy_core_with(
    strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
    spec: &MountPlacement,
    opts: StrategyCorePaperConfig,
) -> StrategyCorePaper {
    if !spec.legs.is_empty() {
        let spec = MountPlacement { account: None, ..spec.clone() };
        let own = (spec.venue.clone(), spec.symbol.clone());
        let core = super::multi::build_paper_multi_strategy_core_with(
            vec![super::multi::MountBuild { strategy, spec }],
            opts,
        );
        let fills = core.fills.iter().find(|(book, _)| *book == own).map(|(_, f)| Arc::clone(f));
        let fills = fills.expect("the mount's own symbol has a book");
        return StrategyCorePaper { handle: core.handle, fills };
    }
    let StrategyCorePaperConfig {
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
        order_owners,
        journal_restore,
        charge_paper_funding,
        funding_history,
        input_opener,
        session_rows,
        mounts_awaiting_strategy,
        account_seed,
        journal_forecasts,
        order_client_id_past_sessions,
        order_client_id_session_recorder,
    } = opts;
    // No legs (the branch above took them): `paper_client_for`'s SINGLE-symbol book is the whole book.
    let client = paper_client_for(spec, &halt);
    let fills = Arc::clone(&client.fills);
    let mut engine = Engine::new(
        Account::new(
            1.0,
            &spec.venue,
            crate::paper_contract_value_grid(&spec.venue, [spec.symbol.as_str()]),
            BalanceMode::Delta,
        ),
        RiskGate::new(risk_limits),
        client,
        &spec.venue,
        &spec.symbol,
    );
    // A simulated book, and it says so (the multi-mount builder, `run/multi.rs`, does the same).
    engine.set_tier(vike_model::AccountTier::Paper);
    // A spot pair is `Cash`, as live books it (the spot rule): paper never sells coins it did not buy.
    crate::arming::admit_spot_cash(
        &mut engine.account,
        &spec.venue,
        &spec.symbol,
        vike_model::MarginMode::Cross,
    );
    let mut config = CoreConfig {
        seed_cash: account_seed.unwrap_or(spec.seed_cash),
        strategy: Some(Mount {
            order_client_id_code: spec.order_client_id_code,
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
        // Write-ahead journal: the CALLER's resolved answer and nothing else; enabled, the paper
        // fills (via `pump_client`) land on disk.
        // ⚠ There was a fallback here to a second, environment-driven resolution, and it is what
        // let a paper mount journal from the FILE `VIKE_RUN_PROFILE` named while the daemon's
        // active run row (which named no journal) was the profile in force. One resolution, the
        // caller's, so the paper core cannot journal somewhere the daemon never decided.
        journal,
        // The run profile's `sinks.forecasts`, as the live arm applies it (the opts field).
        journal_forecasts,
        // Both off by default (`StrategyCorePaperConfig`): book left resting at teardown.
        oco_cancel_sibling_on_dead_exit,
        cancel_orders_on_shutdown,
        // `None` ⇒ runtime `Command::MountStrategy` refuses (the opts field).
        strategy_factory,
        // `None` ⇒ no order-ownership file (the opts field, decision 0113).
        order_owners,
        // A paper book lives in this process's memory, so a restart empties it: a restored order
        // on it is reported gone at boot instead of waiting for a pass that never comes (0127).
        restored_paper_orders_gone: true,
        // The paper engine pays funding from the venue's public rate (the opts field)...
        charge_paper_funding,
        // ...and a restart catches up the settlements it missed from the stored series.
        funding_history,
        // A mount's inputs, read where the live arm reads them (the opts field).
        input_opener,
        // ...and the kept rows a restart's closed trades read a market's class from (the opts field).
        session_rows,
        // A mount whose strategy file is still building is parked by the core (the opts field).
        mounts_awaiting_strategy,
        // This install's past order client id sessions, as the live arm hands them in (the opts field).
        order_client_id_past_sessions,
        // ...and where the session it mints under is recorded before it mints (the opts field).
        order_client_id_session_recorder,
        ..CoreConfig::default()
    };
    spec.name_strategy_file(&mut config.mount_strategy_files, &mut config.mount_fills_at_close);
    if let Some(restore) = journal_restore {
        restore.apply_to(&mut config);
    }
    let handle = spawn_core(engine, config);
    StrategyCorePaper { handle, fills }
}
