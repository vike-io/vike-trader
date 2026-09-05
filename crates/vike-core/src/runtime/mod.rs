//! The single-writer live core runtime — plan §1's pinned architecture.
//!
//! One dedicated OS thread ("vt-core") owns the [`ExecutionEngine`] (account, registry, gate,
//! client) and is the ONLY mutator. Everything reaches it through ONE totally-ordered
//! ingest queue, so determinism is by construction, not by locking:
//!
//! - **Exec events** (fills/orders/account/funding/liq): `tokio::mpsc` `send().await` /
//!   `blocking_send` — LOSSLESS, back-pressures the venue task, never the GUI.
//! - **Commands** (GUI → core): `try_send` into the same queue — the GUI thread NEVER
//!   blocks; a full-queue rejection is returned to the caller AND counted in the snapshot
//!   (`rejected_commands`) so it can't be silent.
//! - **Market data**: a latest-wins conflation slot + a marker message. The producer
//!   overwrites the slot (drop counter surfaced); at most ONE marker is ever in flight, so
//!   a slow core sees the freshest price without unbounded queueing.
//! - Core side: `blocking_recv()` (no tokio runtime on the hot thread), then an opportunistic
//!   `try_recv` drain (bounded batch) before the coalesced snapshot publish.
//!
//! **Snapshot cadence:** dirty-flag + coalesced publish (≥ every `snapshot_interval`
//! (~16 ms) while busy, immediately when the queue goes idle) into an `arc_swap` cell the
//! GUI reads on repaint — the lossy-observer seam; the build never runs per-event.
//!
//! **Panic policy** (plan: catch_unwind per handler + supervisor + safe-state): every
//! dispatch runs under `catch_unwind`. On a panic the core does NOT die and does NOT keep
//! trading: it enters SAFE-STATE — `trading_state = HALTED` (the RiskGate now denies every
//! new order), best-effort cancel of all non-terminal working orders, `fault` surfaced in
//! the snapshot. `AssertUnwindSafe` is sound here because a possibly-inconsistent engine is
//! never traded on again — only read for display — until the process restarts.

use arc_swap::ArcSwap;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use vike_model::events::{Event, OrderRejected};
use vike_model::{
    Bar, BracketSpec, Broker, Clock, EquitySample, FeedStatus, Fill, FlowToxicity, HftBroker,
    LiveClock, MarkTick, OrderRequest, Strategy, StrategyParams, amount_to_order, build_bracket,
    units_from_percent, units_from_value,
};

use crate::emulator::ConditionalBook;

use vike_exec::lanes::{Conflated, ConflatedState}; // doc-hidden: vike-core is the lane consumer
use vike_exec::price_board::Resolution;
use vike_exec::{
    BalanceMode, BarSeed, BarSender, BarSeries, BarUpdate, BookUpdate, CancelIntent, Command,
    ConditionalIntent, ContingencyBook, EventBus, EventSender, FlowUpdate, Fold, Ingest,
    MarginUpdate, MarkSource, MarketSender, OrderIntent, Outbox, ParamsUpdate, QuoteUpdate,
    ReconcileReports, ReconcileSnapshot, RouteKey, SeriesKey, StreamStatusUpdate, TickSender,
    TradeUpdate,
};
use vike_exec::{DivergenceKind, ExecutionClient, ExecutionEngine, PriceCfg, TradingState};

use crate::schedule::LiveSchedule;
use crate::snapshot::{CoreSnapshot, MountView};
use crate::timer_wheel::{DeadlineTimerWheel, TimerId};

// LiveBroker (the live Broker/HftBroker) and CoreHandle/ReconcileDriver (the GUI-facing
// read side) are split into sibling modules — NON-hot code only; the CoreThread fold stays
// here untouched. Re-exported so `runtime::{LiveBroker, CoreHandle, ReconcileDriver}` and the
// crate-root `vike_core::*` paths are unchanged.
mod apply;
mod broker;
mod deadman;
mod handle;
mod publish;
mod recon_held;
mod strategy_drive;
mod timers;
mod watchdog;

#[cfg(test)]
mod deadman_tests;

#[cfg(test)]
mod mount_account_tests;

#[cfg(test)]
mod multi_mount_tests;

#[cfg(test)]
mod recon_held_tests;

#[cfg(test)]
mod route_key_tests;
#[cfg(test)]
mod runtime_mount_tests;

pub use broker::LiveBroker;
use deadman::DeadMan;
pub use deadman::{DeadManAction, DeadManConfig};
pub use handle::{CommandSink, CoreHandle, ReconcileDriver};

/// How long a TERMINAL order's `coid -> mount` attribution entry LINGERS before
/// [`CoreThread::prune_terminal_coids`] retires it (5 minutes on the core clock).
///
/// The window exists entirely to protect the LATE FILL. `Event::Fill` and the FSM terminal that
/// accompanies it are SEPARATE events with no ordering contract between them, and `Account` folds a
/// fill regardless of FSM state (the terminal-drop guard is on the FSM lane only) — so a partial
/// fill racing its own `OrderCanceled` ack, or a WS reconnect re-delivering executions after a
/// cancel, arrives AFTER the order is terminal. Erasing the entry on the terminal itself would leave
/// those fills OWNERLESS, and an ownerless fill books into the residual row instead of the mount
/// that traded it — the exact silent-attribution-loss shape the budget-latch flatten had, let back
/// in through the side door. Bounding a map is not worth that.
///
/// 5 minutes is far beyond any plausible venue reordering (sub-second) or reconnect-replay gap
/// (seconds), while still bounding the map by the terminal RATE rather than by session length: a
/// maker terminalizing 10 orders/second holds ~3k entries, flat, forever, instead of one per order
/// ever placed. Not configurable — there is no operational reason to tune it, and a knob would need
/// a `SETTINGS` row for nothing.
const COID_PRUNE_LINGER_MS: i64 = 5 * 60 * 1_000;

/// What a fired [`DeadlineTimerWheel`] entry asks the drain-loop boundary to do. One variant today
/// (the stuck-order watchdog sweep cadence, audit co6); the freshness/dead-man/order-TTL timers the
/// wheel is meant to consolidate land as future variants. `Copy` so the boundary can pop fired
/// kinds out of the reusable buffer without borrowing the wheel while it acts on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimerKind {
    /// run [`CoreThread::sweep_stuck_orders`] and re-arm the next sweep — the audit-C3 watchdog's
    /// cadence, moved off the per-message `Ingest::Watchdog` dispatch onto the coalesced boundary.
    StuckSweep,
    /// run [`CoreThread::sample_equity`] and re-arm while any position is still open — the
    /// portfolio-observer PR-3 equity sampler's cadence. Armed/disarmed at the drain-loop
    /// boundary by [`CoreThread::maintain_equity_timer`], gated behind
    /// [`CoreConfig::equity_sample`]; never armed at all when that config is `None`.
    EquitySample,
    /// run [`CoreThread::save_all_strategy_state`] and re-arm while any mount exists — the
    /// portfolio-observer PR-4 strategy-state periodic save cadence. Armed/disarmed at the
    /// drain-loop boundary by [`CoreThread::maintain_state_save_timer`], gated behind
    /// [`CoreConfig::state_save`] AND [`CoreConfig::state_dir`] both being `Some`; never armed at
    /// all when either is `None`. UNLIKE [`Self::EquitySample`], this does NOT additionally gate
    /// on any position being open — a strategy's durable state (e.g. a breaker or an A-S
    /// accumulator) matters whether the book is flat or not.
    StateSave,
    /// run [`CoreThread::sweep_deadman`] and re-arm the next sweep — the dead-man's-switch
    /// (auto cancel-on-disconnect) cadence. Armed at [`CoreThread::arm_boundary_timers`] ONLY when
    /// [`CoreConfig::deadman`] is `Some`; never armed at all (the default) otherwise, so the wheel
    /// stays empty and the boundary advance is skipped. Self-rescheduling like [`Self::StuckSweep`]
    /// (the switch, once armed, stays armed for the core's life — no disarm condition).
    DeadManSweep,
    /// run [`CoreThread::sweep_gtd_expiry`] and re-arm the next sweep — the MANAGED good-till-date
    /// cadence (core-ergonomics). Armed at [`CoreThread::arm_boundary_timers`] ONLY when
    /// [`CoreConfig::gtd_sweep`] is `Some`; never armed at all (the default) otherwise, so the
    /// wheel stays empty and the boundary advance is skipped. Self-rescheduling like
    /// [`Self::StuckSweep`] (once armed it stays armed for the core's life).
    GtdSweep,
    /// run [`CoreThread::write_portfolio_snap`] and re-arm — the opt-in periodic portfolio
    /// (equity + positions) journal record's cadence, [`CoreConfig::portfolio_snapshot_interval`].
    /// Armed ONLY when that interval AND [`CoreConfig::journal`] are BOTH `Some` (there is nowhere
    /// to write otherwise); never armed at all by default.
    PortfolioSnap,
    /// run [`CoreThread::sweep_inflight_confirms`] and re-arm the next sweep — the FAST in-flight
    /// confirm cadence (recon path-to-superset, F1-A). Armed at [`CoreThread::arm_boundary_timers`]
    /// ONLY when [`CoreConfig::inflight_confirm`] is `Some`; never armed at all (the default)
    /// otherwise, so the wheel stays empty and the boundary advance is skipped. Self-rescheduling
    /// like [`Self::StuckSweep`] (once armed it stays armed for the core's life). This is the
    /// REJECT-FREE early rung of the stuck-order ladder — it only re-queries, never terminalizes.
    InflightConfirm,
}

/// Why a command was not accepted — surfaced to the caller, never swallowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandRejected {
    /// ingest queue full — retry after a repaint; also counted in the snapshot
    Busy,
    /// core thread has exited
    Gone,
}

/// Opt-in write-ahead command journal (spec 2026-07-10-journal-replay-and-data-freshness §A).
/// `None` (the [`CoreConfig`] default) = today's behavior, ZERO overhead — no journal is opened
/// and the fold path is byte-identical. `Some` opens a [`crate::journal::CommandJournal`] at
/// construction and write-ahead-journals every exec-lane message before it folds (see
/// [`CoreThread::dispatch`]).
#[derive(Debug, Clone)]
pub struct JournalConfig {
    /// directory the segment files live in (created if absent).
    pub dir: std::path::PathBuf,
    /// segment sizing + flush cadence of the underlying journal.
    pub file: crate::journal::JournalFileConfig,
    /// write a full-state `Snap` record every N appended records (plus always on Shutdown / the
    /// senders-dropped exit). `0` would snapshot on every record.
    pub snapshot_every: u64,
}

impl JournalConfig {
    /// A journal at `dir` with the standard segment/flush/snapshot cadence — the same defaults the
    /// `[sinks.journal]` profile section uses (64 MiB segments, flush every 256 records, snapshot
    /// every 1024). The one-liner a binary reaches for when enabling the journal from a single
    /// directory knob (e.g. `VIKE_JOURNAL_DIR`) rather than a full [`crate::RunProfile`].
    pub fn at(dir: impl Into<std::path::PathBuf>) -> Self {
        JournalConfig {
            dir: dir.into(),
            file: crate::journal::JournalFileConfig {
                segment_bytes: 64 * 1024 * 1024,
                flush_every: 256,
            },
            snapshot_every: 1024,
        }
    }
}

/// Runtime configuration. Defaults follow the plan; the two hooks are for tests/harnesses.
pub struct CoreConfig {
    pub seed_cash: f64,
    /// coalesced snapshot publish interval while busy (idle always publishes)
    pub snapshot_interval: Duration,
    pub recent_events_cap: usize,
    pub ingest_capacity: usize,
    /// max messages folded per wakeup before a publish check (keeps latency bounded)
    pub batch_max: usize,
    /// injected [`Clock`] for `submit_order` now_ms + persistence timestamps —
    /// `LiveClock` (wall time) by default; tests inject `TestClock` or a bare closure
    /// (any `Fn() -> i64` is a `Clock`)
    pub clock: Box<dyn Clock + Send>,
    /// GUI wake (egui `request_repaint`); called on each coalesced publish
    pub repaint: Option<Box<dyn Fn() + Send>>,
    /// FAULT-INJECTION TEST HOOK: panic when a bare FillEvent with this trade_id arrives
    pub panic_on_trade_id: Option<String>,
    /// LATENCY-HARNESS HOOK: called when a message is dequeued, immediately BEFORE
    /// dispatch — measures the pure send→core hop; dispatch cost is measured separately
    pub on_dequeued: Option<DequeuedHook>,
    /// R7: a strategy mounted on one bar series (paper or live — same path)
    pub strategy: Option<StrategyMount>,
    /// Opt-in stuck-order watchdog (audit C3), stage 1 of the confirm-grace ladder: an order with
    /// no venue ack within this is flagged un-acked — the sweep SOFT-WARNS (it does NOT reject here).
    /// `None` = disabled (default; behavior unchanged — no watchdog thread is spawned, no
    /// `Ingest::Watchdog` is ever enqueued). Set a CONSERVATIVE value (e.g. 30s) for reconcile-less
    /// venues; a too-short value merely starts the ladder early — the hard reject is still gated on
    /// [`Self::submit_ack_confirm_grace`] elapsing afterward, so a slow-but-real venue ack (or the
    /// adapter's own post-submit REST confirm) still wins.
    /// Set this to at LEAST the adapter's own submit + ambiguous-requery budget
    /// (`vike_bridge_core::resolve_ambiguous_submit`): the timeout backstops the adapter's REST
    /// confirm, it does not race it — a too-short value that fires while the adapter's re-query is
    /// still in flight defeats the ladder's whole point (the grace can absorb some of this, but the
    /// timeout itself should not undercut the adapter's known worst-case confirm latency).
    ///
    /// HARD LOWER BOUND (b): `submit_ack_timeout` MUST exceed the venue's worst-case
    /// order-visibility latency — the gap between the venue accepting an order and that order
    /// becoming queryable/streamed. STAGE 1 issues an ACTIVE confirm (a status re-query) the instant
    /// this elapses; if the venue has accepted the order but not yet indexed it, that first confirm
    /// can come back empty and, on a reconcile-less venue, be mistaken for "never landed". The grace +
    /// the in-flight-confirm guard below absorb this, but keeping the timeout above the visibility
    /// latency is the first line of defence against a phantom reject of a just-accepted order.
    pub submit_ack_timeout: Option<Duration>,
    /// Confirm-grace window (audit C3 hardening), stage 2 of the ladder: the ADDITIONAL time after
    /// `submit_ack_timeout` before the watchdog's LAST-RESORT synthesized `OrderRejected`. The order
    /// is terminalized only if it is STILL pre-ack (`Initialized`/`Submitted`) at the reject deadline
    /// — i.e. no authoritative `OrderAccepted`/`OrderFilled` (from a slow WS ack OR the adapter's own
    /// REST re-query, `vike_bridge_core::resolve_ambiguous_submit`) folded meanwhile. This is what
    /// stops the backstop from racing a real venue ack into a phantom reject that would strand a live
    /// position. Ignored when `submit_ack_timeout` is `None`.
    ///
    /// IN-FLIGHT-CONFIRM GUARD (the tuning-independent hardening): stage 1 does not merely WAIT — it
    /// issues an active `confirm` when the order enters the grace. If that confirm is still in flight
    /// when the plain reject deadline (`created + submit_ack_timeout + grace`) passes, the backstop is
    /// DEFERRED one ADDITIONAL grace window, to `created + submit_ack_timeout + 2·grace`. So a confirm
    /// gets up to `2·grace` to land (a real `Accepted`/`Filled` at any point moves the order out of
    /// pre-ack and cancels the reject entirely). This converts "the reject must not fire before the
    /// confirm returns" from a knife-edge into a guarded window — see [`CoreThread::sweep_stuck_orders`].
    ///
    /// HARD LOWER BOUND (a): keep `2·submit_ack_confirm_grace > submit_ack_timeout/2 + N·requery_timeout`
    /// (equivalently `submit_ack_confirm_grace` alone should exceed `submit_ack_timeout/2 +
    /// N·requery_timeout` for margin), where `requery_timeout` is the adapter's per-re-query REST
    /// timeout (`~5s`, `vike_bridge_core::http::blocking_agent_with_timeout`) and `N` is the number of
    /// sequential re-queries the confirm makes: N=2 for Bybit (realtime→history, worst case ~10s), N=1
    /// for OKX. The `submit_ack_timeout/2` term is the watchdog tick jitter (the timer fires every
    /// `submit_ack_timeout/2`, so the active confirm can be issued up to one tick late). Under-sizing
    /// the grace lets the reject deadline fall inside the confirm's own round-trip; the guard's
    /// `2·grace` window is the slack that keeps a slow-but-real confirm from being clobbered.
    ///
    /// A NON-TRIVIAL grace IS the safety margin: `Duration::ZERO` collapses the ladder back to
    /// immediate-reject-at-`submit_ack_timeout` (both the grace AND the guard become no-ops) and
    /// reintroduces exactly the phantom-reject risk this window exists to remove. Keep it comfortably
    /// above the venue's ack-jitter + the adapter's re-query RTT; the default (15s) already clears
    /// Bybit's ~10s two-hop confirm with the `2·grace` guard.
    pub submit_ack_confirm_grace: Duration,
    /// Opt-in margin-call watchdog (accounting-upgrade Phase B; LEAN DefaultMarginCallModel
    /// semantics). Swept once per CLOSED bar of the engine's own series (marks fresh, off
    /// the event fold). Warning → recent-events ring; liquidation submits reduce-only
    /// market orders through the NORMAL gate path. `None` (default) = disabled.
    pub margin_call: Option<vike_exec::MarginCallConfig>,
    /// Opt-in drawdown latch (audit exec#4): the max tolerated fractional drawdown from the
    /// high-water-mark of the DAEMON'S OWN equity curve before the core LATCHES into
    /// liquidate-only (`trading_state = Reducing`, so the existing RiskGate then denies
    /// risk-increasing orders and permits only reduce-only). Folded on the SAME per-CLOSED-bar
    /// cadence and through the SAME resolver as the margin-call watchdog (marks fresh, off the
    /// event fold). E.g. `Some(0.20)` = latch at 20% drawdown. `None` (default) — and any value
    /// `<= 0.0` — DISABLES the latch: no HWM is folded, behavior is byte-identical to today. LATCH
    /// semantics: once tripped it STAYS Reducing even if the curve later recovers (never
    /// auto-un-latched — un-latching is a deliberate manual `Command::SetTradingState`).
    ///
    /// ⚠ **A FRACTION OF WHAT.** Of `Σ seed_of(engine) + Σ resolved_own_pnl(engine)` at its peak —
    /// the operator's CONFIGURED capital plus the daemon's own realized+unrealized P&L, and
    /// deliberately NOT the observed account equity, which on an `Authoritative` block is a
    /// venue wallet for an account this daemon may not own alone. [`CoreThread::sweep_drawdown_latch`]
    /// is the authority and carries the the CI box measurement that forced it; on an all-`Delta` core
    /// the two are the same number. A positive `seed_cash` is therefore load-bearing whenever this
    /// is `Some` — `RunProfile::validate` requires it.
    pub max_drawdown: Option<f64>,
    /// Phase C (opt-in, Rust-native): also check the ConditionalBook on every tick of the
    /// engine's series (quote mid / trade px) — intra-bar stop/trailing triggering the
    /// Python oracle's bar-close book lacks. Default false = oracle-faithful bar-close only.
    pub conditionals_on_ticks: bool,
    /// Phase D (opt-in): ADDITIONAL strategy mounts beyond `strategy` — one engine, N
    /// (venue, symbol, interval) mounts over the one multi-symbol Account. Symbols beyond
    /// the engine's primary must be listed in `ExecutionEngine::extra_symbols` so their
    /// venue events fold. Empty (default) = single-mount behavior, unchanged.
    pub extra_mounts: Vec<StrategyMount>,
    /// Opt-in write-ahead command journal (spec §A). `None` (default) = no journal opened,
    /// zero overhead — the fold path is byte-identical to today.
    pub journal: Option<JournalConfig>,
    /// Resume the [`vike_exec::ClientOrderIdGenerator`] from a persisted `(session, seq)` on
    /// restart (so replayed/new orders keep unique, non-colliding ids across a restart). `None`
    /// (default) = a fresh random session, today's behavior.
    pub coid_session: Option<(String, u64)>,
    /// Resume the emulated-conditional arm-id counter ([`CoreThread::mint_arm_id`]) on restart,
    /// the arm-side twin of [`Self::coid_session`]. `0` (default) = a fresh session's counter.
    ///
    /// This matters BECAUSE `coid_session` is resumed: an arm id is `{coid_session}a{arm_seq}`, so
    /// a restart that restores the session but restarts the counter at 0 would re-emit ids the
    /// pre-restart run already used — and resume APPENDS to the same journal directory, so both
    /// live in ONE log. And since emulator PR-2 the arm id is the DISARM key
    /// ([`vike_exec::OrderIntent::DisarmConditional`]), so a duplicate would let a disarm target
    /// the wrong arm. [`crate::replay::RestoredState::arm_seq`] supplies the resume value (the
    /// latest `Snap`'s stamped counter, prune-safe — see that field for the pre-v7 fallback); a
    /// caller that does not resume keeps today's behavior.
    pub arm_seq: u64,
    /// Re-arm the emulated-conditional books on restart (emulator PR-3, re-arm-on-restore): each
    /// entry is one armed conditional — its minted `arm_id` and the terms the book held at the
    /// restore base's `Snap` (a TRAILING arm's ratcheted extreme included) — seeded into
    /// `conditional_books` ONCE at [`assemble_core`], in order (fire order is insertion order).
    /// [`crate::replay::RestoredState::conditionals`] supplies the value on a crash restart, next
    /// to `coid_session`/`arm_seq`; empty (default) = today's behavior, a core that starts with
    /// empty books (and a pre-v8 journal's Snap carries no books, so it restores empty — never an
    /// error). Zero hot-fold cost: consumed entirely at assembly.
    ///
    /// ⚠ **DECLARED RESIDUAL — a RESTORED arm loses its ACCOUNT.** `SnapConditional` carries the
    /// arm's `(venue, symbol)` terms and no route key, so a seeded arm gets no
    /// [`CoreThread::cond_engine`] entry and its fire takes the historical payload route (the
    /// venue's DEFAULT account). That is exactly today's behaviour for every core, and it is
    /// correct wherever one engine per venue makes the two answers the same — but on a
    /// MULTI-ACCOUNT core it means a labelled mount's protective stop, armed before a restart and
    /// triggered after it, fires onto the default account. Closing it means putting the route key
    /// on `SnapConditional`, i.e. a durable journal-shape change; the live arm is exact today.
    pub conditionals: Vec<crate::journal::SnapConditional>,
    /// Re-seed the live-runtime OTO/OCO contingency book on restart (live-runtime OCO/OTO), the
    /// contingency twin of [`Self::conditionals`]: each entry is one resting linked leg — its
    /// linkage (`parent`/`linked`/`active`) and, for a HELD exit, the resolved order the runtime is
    /// keeping OFF the venue until its parent fills. Seeded into `contingency`/`held_orders` ONCE at
    /// [`assemble_core`], in the captured insertion order (arm/cancel iteration order). Supplied on a
    /// crash restart by [`crate::replay::RestoredState::contingencies`]; empty (default) = today's
    /// behavior (a core that starts with no contingency state; a pre-v11 journal's Snap carries none,
    /// so it restores empty — never an error). Zero hot-fold cost: consumed entirely at assembly.
    pub contingencies: Vec<crate::journal::SnapContingency>,
    /// Re-seed the per-mount fill-ATTRIBUTION ledgers on restart (multi-mount durability, gap D):
    /// one row per mount, resolved onto its slot by `mount_id`
    /// ([`crate::strategy_state::mount_id_with`]) at [`assemble_core`], so a re-ordered mount list
    /// still restores onto the right mounts and a row naming no mounted slot is simply ignored
    /// (inert, never an error). Supplied on a crash/clean restart by
    /// [`crate::replay::RestoredState::mount_attr`] (the latest `Snap`'s captured ledgers).
    ///
    /// Empty (default) = today's behavior: every mount starts with a zeroed ledger. Zero hot-fold
    /// cost — consumed entirely at assembly.
    pub mount_attr: Vec<crate::journal::SnapMountAttr>,
    /// Re-seed the coid -> mount ATTRIBUTION MAP on restart (multi-mount durability, gap D): each
    /// entry is `(client_order_id, mount_id)` for an order some mount minted before the restart, so
    /// a resting order that fills AFTER it still books into its originating mount's ledger (and is
    /// still cancellable scoped-to-that-mount by the budget latch). Resolved onto mount slots by
    /// `mount_id` at [`assemble_core`], exactly like [`Self::mount_attr`]; an entry naming no
    /// mounted slot is ignored.
    ///
    /// Supplied by [`crate::replay::RestoredState::coid_mounts`], which folds the journal's own
    /// `StrategySubmit` provenance records — the origin has always survived to the journal, nothing
    /// read it back. Empty (default) = today's behavior: post-restart fills on pre-restart orders
    /// are unattributed (they land in the residual row).
    pub coid_mounts: Vec<(String, String)>,
    /// Opt-in mmap counter mirror (audit co9): a file the key runtime health counters
    /// (`conflated_market_drops`, `rejected_commands`, `exec_db_*`, `stranded_terminal_drops`, …)
    /// are copied into at the coalesced publish cadence, so an external `vike_stat` process can read
    /// them without attaching to a headless trader. `None` (default) = no file is opened and the
    /// publish path is byte-identical to today; the per-message fold is untouched either way (the
    /// mirror runs only at publish, off the hot fold). A binary may source the path from an env var
    /// (env reads stay in binaries — see [`crate::counters`]).
    pub counters_path: Option<std::path::PathBuf>,
    /// Opt-in strategy-state persistence sidecar (portfolio-observer PR-4 T2): when set, each
    /// mount's deterministic id ([`crate::strategy_state::mount_id_of`]) resolves to
    /// `<state_dir>/<mount_id>.json`. `assemble_core` loads that sidecar (if present) into the
    /// mount's strategy (`Strategy::load_state`) before it ever runs; a clean shutdown saves the
    /// strategy's current durable state back out (`Strategy::save_state`), best-effort. `None`
    /// (default) is zero overhead: no path is ever built, no file is ever touched, and mount
    /// assembly/teardown stay byte-identical to today.
    pub state_dir: Option<std::path::PathBuf>,
    /// Timer-armed PERIODIC strategy-state save (portfolio-observer PR-4 T3), gated behind
    /// `Some` AND [`Self::state_dir`] also being `Some` (see
    /// [`CoreThread::maintain_state_save_timer`]): while at least one mount exists, on this
    /// cadence, every live mount's durable state is re-saved
    /// ([`CoreThread::save_all_strategy_state`]) — the same best-effort
    /// `Strategy::save_state` -> `write_json_atomic` sidecar write [`Self::state_dir`]'s doc
    /// describes for the clean-shutdown save, just on a running cadence instead of (in addition
    /// to) shutdown only. Mirrors [`Self::equity_sample`]'s timer machinery almost exactly; the
    /// one difference is the arm condition does NOT check open positions — a strategy's durable
    /// state (a breaker's trip count, an A-S accumulator, ...) matters whether the book is flat
    /// or not. Entirely COLD-path (drain-loop boundary arm/disarm + a self-rescheduling
    /// [`DeadlineTimerWheel`] entry — never the per-message fold). `None` (default), OR a `Some`
    /// with [`Self::state_dir`] left `None`, is zero overhead: no timer is ever armed and the
    /// fold path is byte-identical to a save-timer-free runtime.
    pub state_save: Option<Duration>,
    /// Timer-armed equity sampler (portfolio-observer PR-3), gated behind `Some`: while ANY
    /// position is open (see [`CoreThread::any_position_open`]), on this wall-clock cadence,
    /// resolve per-venue + cross-venue equity (`ExecutionEngine::resolve_equity`) and hand the
    /// batch to [`Self::on_equity_sample`]. Entirely COLD-path — the drain-loop boundary
    /// arm/disarm plus a self-rescheduling [`DeadlineTimerWheel`] entry — NEVER the per-message
    /// fold. `None` (default) = zero overhead: the boundary check is a single
    /// `Option::is_some()` read, no timer is ever armed, and the fold path is byte-identical to
    /// a sampler-free runtime.
    pub equity_sample: Option<Duration>,
    /// The equity sampler's sink (Option B: vike-core takes NO vike-data dependency — the
    /// CALLER wires persistence/display through this closure). Fired with the full batch — one
    /// row per engine (primary + extras, registration order) plus a `"TOTAL"` cross-venue row
    /// (`py_sum` laws, matching `CoreSnapshot::equity_total`) — each time `equity_sample` fires.
    /// `None` (default) = no sink; the sampler still arms/fires when `equity_sample` is `Some`,
    /// but the finished batch is simply not delivered anywhere (set both together in practice).
    pub on_equity_sample: Option<EquitySampleHook>,
    /// Journal cross-check (#3): builds a [`vike_exec::recon::JournalView`] for a venue from the
    /// materialized Tier-2 exec log (unified-journaling #2). `Some` unlocks the three-way
    /// (local-vs-venue-vs-journal) reconcile so a persisted fill the live `Account` lost surfaces as
    /// a `JournalDivergence` alert. Called on the fold thread at reconcile time only (an OCCASIONAL
    /// command — a bounded lookback query, off the p99 hot path). `None` (default) = two-way
    /// reconcile, byte-identical. The hook lives here because the store read lives in `vike-app-core`
    /// (which deps both vike-core and vike-data); vike-core cannot read the store itself.
    pub journal_view_provider: Option<JournalViewHook>,
    /// Resolver knobs the equity sampler AND `CoreSnapshot::build` both resolve prices through
    /// (threading deferred from PR-2's `ExecutionEngine::resolve_equity`). Permissive default
    /// (`PriceCfg::default()`: mark enabled, no freshness windows) is behavior-preserving for
    /// every caller that never touches this.
    pub price_cfg: PriceCfg,
    /// How long a REAL venue mark keeps ownership of the account mark slot (ms).
    ///
    /// The rule itself is NOT here and NOT in any lane: it lives in `Account::set_mark_from`,
    /// keyed by [`vike_exec::MarkSource`], which is the only door into the (private) `marks` map.
    /// This field is only the WINDOW, pushed onto every engine's `Account` at spawn — a knob, not
    /// a policy. See [`vike_exec::MarkSource`] for the law and its consequences.
    ///
    /// Default 10s = ~10 missed ticks of a 1s venue mark stream. `0` disables ownership entirely
    /// (pure last-write-wins, the pre-law behavior).
    pub mark_staleness_ms: i64,
    /// How long a RECONCILE mark ([`vike_exec::MarkSource::ReconcileMark`]) keeps ownership of the
    /// account mark slot (ms) — the per-source twin of [`Self::mark_staleness_ms`] for the
    /// reconcile cadence, which samples the venue's mark far less often than a 1s stream.
    ///
    /// MUST exceed the deployment's `VIKE_RECONCILE_INTERVAL_MS` (default 60s): sized so a
    /// reconciled stream-less venue's slot stays owned by the reconcile mark CONTINUOUSLY between
    /// passes instead of expiring each minute and alternating with candle closes. Default 150s
    /// (2.5× the default reconcile interval); closes reclaim the slot only if reconcile STOPS for
    /// longer than this. Pushed onto every engine's `Account` at spawn (a knob, not a policy —
    /// the law lives in `Account::set_mark_from`). `0` makes reconcile-mark ownership expire
    /// immediately.
    pub reconcile_mark_staleness_ms: i64,
    /// Opt-in per-mount READINESS GATE (portfolio-observer PR-4 T5): when `true`, a freshly
    /// mounted strategy starts `Pending` (see [`MountState`]) and stays that way until its
    /// symbol actually prices through the [`vike_exec::price_board::PriceBoard`] (checked at
    /// the drain-loop boundary, [`CoreThread::maintain_mount_readiness`], using [`Self::price_cfg`]
    /// — the SAME resolver knobs the equity sampler and `CoreSnapshot::build` already price
    /// through, so "priced" means one consistent thing everywhere). A `Pending` mount still
    /// RECEIVES every hook call (bars/quotes/trades/fills/...) — its estimator/warmup proceeds
    /// exactly as if the gate were off — but any order intents it buffers are DISCARDED at
    /// [`CoreThread::drain_broker`] instead of reaching the engine, so nothing can hit a venue
    /// before its own symbol has a real price. `false` (the default) is INERT: every mount is
    /// seeded `Ready` at [`assemble_core`] and never re-checked, so the boundary probe never
    /// runs and `drain_broker`'s new check is a compare against a constant — BYTE-IDENTICAL to
    /// today's behavior.
    pub readiness_gate: bool,
    /// Opt-in DEAD-MAN'S SWITCH (auto cancel-on-disconnect; trading-hardening) — the AUTOMATIC
    /// counterpart to the manual HALT sentinel. When `Some`, a cold-path timer (a sibling of the
    /// stuck-order watchdog, driven by the same [`DeadlineTimerWheel`] at the drain-loop boundary,
    /// NEVER the fold) watches how long market data / venue events have been silent; if that
    /// exceeds [`DeadManConfig::timeout`] it TRIPS once — cancelling every resting order through the
    /// one order-write path and, for [`DeadManAction::CancelAllAndHalt`], engaging HALT (in-process
    /// `trading_state = Halted` + the cross-process HALT sentinel file). It re-arms on recovery.
    /// `None` (default) = DISABLED: no [`deadman::DeadMan`] is built, no timer/waker is armed for
    /// it, and the per-message fold never touches it — byte-identical to today. See
    /// [`deadman`] for the full contract.
    pub deadman: Option<DeadManConfig>,
    /// Opt-in MANAGED GTD EXPIRY sweep cadence (core-ergonomics): how often the core re-checks
    /// resting orders for a passed `gtd_expiry` and cancels them ([`CoreThread::sweep_gtd_expiry`]).
    /// Runs on the EXISTING [`DeadlineTimerWheel`] at the drain-loop boundary — no new thread, and
    /// nothing at all on the per-message fold. `None` (default) = DISABLED: no timer is armed, the
    /// sweep never runs, and a venue's own native GTD handling (or the order simply resting) is
    /// unchanged — byte-identical to today. The order terms themselves are NOT new
    /// (`OrderRequest::time_in_force` / `gtd_expiry` already exist); this only adds the local
    /// enforcement for venues that do not expire GTD themselves. Pick a cadence coarse relative to
    /// the expiries you set: an order can rest up to one tick past its deadline before the sweep
    /// sees it.
    ///
    /// **REPLAY: SUPPORTED as of emulator PR-5** (it was a loud `Unsupported` refusal before).
    /// The sweep journals every expiry it decides write-ahead as
    /// [`crate::journal::JournalRecord::GtdExpire`], and its only local action — `cancel_order` —
    /// publishes nothing and moves no fenced state; the authoritative `OrderCanceled` comes back
    /// from the venue as its own journaled `Ingest::Event` and replays independently. So a
    /// `gtd_sweep` session now replays and crash-restores like any other, and the record is
    /// replay-NEUTRAL (never re-applied — re-issuing the cancel would double it). Combining this
    /// with [`Self::submit_ack_timeout`] or [`Self::deadman`] still forfeits replay, for THOSE
    /// features' own wall-clock reasons ([`crate::replay::ReplayError::Unsupported`]).
    pub gtd_sweep: Option<Duration>,
    /// Opt-in PERIODIC PORTFOLIO SNAPSHOT cadence (core-ergonomics): on this cadence the core
    /// appends ONE compact [`crate::journal::PortfolioSample`] record (per-venue equity/balance/
    /// realized + every open position) to the write-ahead journal — a low-rate equity/positions
    /// time series for downstream reporting, without the GUI having to poll a running process.
    ///
    /// WHY THE JOURNAL (the "least-invasive existing off-fold path" choice): the SQLite `exec_db`
    /// writer is retired (see [`crate::counters`]'s reserved slots), so the journal + its
    /// materializer is the only surviving durable off-fold sink the core already owns — it is
    /// already opened, already framed/checksummed/segmented, already pruned, and already read by
    /// `vike-app-core`'s materializer. Writing here therefore adds no file handle, no thread and no
    /// new dependency edge. The record is REPLAY-NEUTRAL (`replay.rs`'s tail extraction ignores it,
    /// exactly like `MintedSubmit`): it is an observation, never a command to re-apply.
    ///
    /// Gated on [`Self::journal`] ALSO being `Some` — with no journal there is nowhere to write, so
    /// the timer is simply never armed. `None` (default) = DISABLED, byte-identical.
    ///
    /// REPLAY/RESTORE STAYS INTACT. The cadence needs the boundary waker to reach an idle core, and
    /// the waker's `Ingest::Watchdog` message is what normally forces `replay_from` to refuse a
    /// session. That refusal exists for the WALL-CLOCK sweeps that mutate state, so the waker
    /// record is written only when one of those is enabled (`CoreThread::journal_waker_records`) —
    /// a core running this cadence alone journals no waker records and still replays/crash-restores
    /// exactly as before. Combining it with `submit_ack_timeout`/`deadman` does forfeit replay, but
    /// for those features' own reasons; `gtd_sweep` no longer forfeits it (emulator PR-5).
    ///
    /// DISK GROWTH (it is cheap per record, not free per session): these records deliberately do
    /// NOT bump `journaled_since_snap` — that counter paces the REPLAY-BASE `Snap` cadence, which
    /// stays a function of folded commands. Consequence: an otherwise IDLE journaling core (no
    /// exec-lane traffic at all) never reaches the `snapshot_every` threshold, so no `Snap` is
    /// written, so `prune_before_latest_snap` has no floor to prune to and segments accumulate for
    /// the life of the session (the shutdown `Snap` is the first prunable point). A 1s cadence on a
    /// silent weekend is ~86k unprunable records/day. Size the cadence for the session length —
    /// seconds-to-minutes for a reporting series, not sub-second.
    pub portfolio_snapshot_interval: Option<Duration>,
    /// Opt-in FAST IN-FLIGHT CONFIRM cadence + age threshold (recon path-to-superset, F1-A; grafted
    /// from Nautilus's ~2s `check_inflight_orders`). On this cadence the core issues a REJECT-FREE
    /// venue re-query ([`vike_exec::ExecutionEngine::confirm_order`]) for every order stuck
    /// SUBMITTED-but-unacked (`Initialized`/`Submitted`, `created_ms == Some`) whose age is in the
    /// band `[inflight_confirm, submit_ack_timeout)` — the early window the reject-capable
    /// stuck-order ladder ([`CoreThread::sweep_stuck_orders`], gated on [`Self::submit_ack_timeout`])
    /// deliberately ignores. It catches a wedged adapter in ~2s instead of waiting the conservative
    /// `submit_ack_timeout` (at LEAST 30s) for the ladder's first confirm.
    ///
    /// It NEVER synthesizes a terminal and NEVER fights the reject ladder: it calls only the
    /// `is_live`-guarded, publish-nothing `confirm_order`; it is BAND-LIMITED to
    /// `age < submit_ack_timeout` so once an order crosses that timeout this rung stops touching it
    /// and the late (reject-capable) rung owns it — the two never act on the same order at the same
    /// age, and this rung never touches the ladder's `confirm_issued_ms`; and it keeps its OWN dedup
    /// map (`inflight_confirm_last_ms`) so a still-stuck order is re-confirmed at most once per
    /// `inflight_confirm` window, not every tick. When `submit_ack_timeout` is `None` (watchdog off)
    /// the band is `[inflight_confirm, +inf)` — this becomes the ONLY stuck-order signal, still
    /// reject-free (without a configured reject timeout we must never invent a terminal).
    ///
    /// `None` (default) = DISABLED, byte-identical: the [`TimerKind::InflightConfirm`] timer is
    /// never armed, the waker spawn/tick is unchanged, `sweep_inflight_confirms` is never reachable,
    /// and no dedup state is ever written. Suggested live value ~2s
    /// (`VIKE_RECONCILE_INFLIGHT_MS=2000`). REPLAY STAYS INTACT: the sweep's only action is
    /// `confirm_order`, which publishes nothing and moves no fenced state (like the `gtd_sweep`
    /// cancel), so a core running this cadence alone journals no waker record and replays/crash-
    /// restores exactly as before.
    pub inflight_confirm: Option<Duration>,
    /// Opt-in OCO SIBLING-CANCEL ON A DEAD PROTECTIVE EXIT (live-runtime OCO/OTO). Governs what
    /// [`CoreThread::drive_contingency_on_terminal`] does when a RELEASED bracket exit leg (a stop-
    /// loss / take-profit already armed by its entry's fill) reaches a terminal-UNFILLED state — a
    /// venue reject / cancel / expire:
    ///
    /// - `false` (default) — KEEP the surviving OCO sibling. The dead leg's own stale book entry is
    ///   cleaned and the death is surfaced, but the sibling is left resting: a position that just
    ///   lost one protective leg retains whatever protection it still has (a naked position is the
    ///   worse default). Byte-identical to the feature as first shipped — only a FILL cancels a
    ///   sibling.
    /// - `true` — ALSO cancel the surviving OCO sibling (and clean its book entry), through the SAME
    ///   mechanics the fill-driven OCO-cancel uses: a still-held sibling is dropped, a resting one is
    ///   canceled at the venue. A deployment that prefers a fully flat book over keeping partial
    ///   protection opts into this. Zero effect on any run without brackets (the contingency book
    ///   stays empty, so the terminal drive is a no-op regardless).
    ///
    /// A binary may source this from an env var (env reads stay in binaries) — `vike-app` reads
    /// `VIKE_OCO_CANCEL_SIBLING_ON_DEAD_EXIT=1`.
    pub oco_cancel_sibling_on_dead_exit: bool,
    /// Opt-in per-mount loss/notional BUDGET (steal/core-per-mount-budget), keyed by the MOUNT ID —
    /// `StrategyMount::controller_id` when the mount carries one, else the legacy
    /// `{venue}__{symbol}__{interval}` derivation ([`crate::strategy_state::mount_id_with`], the ONE
    /// mount-identity law). A mount whose key is present (and whose [`MountBudget`] has an active
    /// arm) is watched on the SAME per-closed-bar cadence as [`Self::max_drawdown`]; on breach it
    /// latches liquidate-only (cancel its resting orders + optional flatten) while every other mount
    /// keeps trading. Empty (default) = no per-mount latch, byte-identical to today (the sweep is
    /// never even armed — see `CoreThread::any_mount_budget`).
    ///
    /// KEYED HERE, not as a field on [`StrategyMount`]: the id resolves each budget onto its mount at
    /// [`assemble_core`], and a key naming no mounted id is simply never matched (inert), never an
    /// error.
    ///
    /// **Why the MOUNT ID and not the `(venue, symbol, interval)` triple** (which is what this map
    /// used to be keyed by): the triple is COARSER than mount identity. Since
    /// [`StrategyMount::controller_id`] landed, two mounts may legitimately share one triple — that
    /// is the exact configuration a controller id exists to legitimize — and under the old key they
    /// resolved to the SAME budget entry. Each latched on its OWN attributed loss, but against a cap
    /// neither could be given independently: two mounts, one budget, no way to express two. The
    /// mount id is the unique identity `assemble_core` already asserts on (a duplicate is a loud
    /// mount-time panic), so keying on it makes "one budget per mount" true by construction. A
    /// controller-id-free mount's key is the SAME `{venue}__{symbol}__{interval}` string as before,
    /// only spelled as one segment instead of a tuple.
    ///
    /// ⚠ The key is the SANITIZED id, not the raw controller id: `mount_id_with` replaces every
    /// non-alphanumeric char with `_`, so a mount whose `controller_id` is `"maker-a"` keys as
    /// `"maker_a"`. Build the key with `mount_id_with`/`mount_id_of` rather than by hand.
    pub mount_budgets: std::collections::HashMap<String, MountBudget>,
    /// Opt-in per-mount WALL-CLOCK SCHEDULE (steal/core-live-scheduler), keyed by the MOUNT ID —
    /// the same key [`Self::mount_budgets`] uses, for the same reason: a same-triple mount PAIR
    /// (which [`StrategyMount::controller_id`] permits) used to share ONE schedule and could not be
    /// given two.
    /// Each [`crate::schedule::LiveSchedule`] registers wall-clock [`crate::schedule::TimeRule`]s
    /// (daily-at-HH:MM in a fixed-offset tz, market-open/close offsets) that fire
    /// `Strategy::on_schedule(broker, tag)` at the right instant — the LIVE twin of the backtest
    /// bar-boundary `Schedule`, with identical rule semantics. Checked at EVERY drain-loop boundary
    /// pass (NEVER per message; the `p99 < 10µs` fold is untouched) — no timer-wheel entry, the
    /// readiness-gate pattern: `check_due` is a few latch compares per rule, and the boundary waker
    /// (whose cadence [`Self::schedule_poll`] feeds) guarantees an idle core reaches the boundary on
    /// cadence. Driving off the CLOCK VALUE at the boundary rather than wheel deadlines is what
    /// keeps an injected/deterministic clock (replay, tests) working: a wheel deadline computed
    /// from a frozen clock is unreachable until the clock moves, which starves the poll.
    ///
    /// Empty (default) = byte-identical: `any_mount_schedule` is false (one bool read per boundary),
    /// no waker cadence is added, and `drive_schedule` never runs. A key naming no mounted mount id
    /// is simply never matched (inert), never an error.
    pub mount_schedules: std::collections::HashMap<String, LiveSchedule>,
    /// WAKER cadence contribution for the wall-clock schedule — how often an otherwise-IDLE core is
    /// woken to reach the drain-loop boundary where [`Self::mount_schedules`] is checked. Only in
    /// effect when some mount has a non-empty schedule; `None` (default) ⇒ 1s. A rule fires at the
    /// first boundary pass AT or AFTER its scheduled instant, so on an idle core this bounds the
    /// firing lateness (the waker tick is `max(schedule_poll/2, 50ms)`); a busy core fires at the
    /// next drain boundary regardless — set it finer for sub-second idle precision, coarser to
    /// reduce idle wakeups.
    pub schedule_poll: Option<Duration>,
    /// SHUTDOWN POLICY (opt-in): cancel every resting order during the core's own teardown, before
    /// the client is detached. `false` (the default) is BYTE-IDENTICAL to the behavior that shipped
    /// before this field existed — teardown detaches and exits, and **resting orders stay live at
    /// the venue with nothing left running to manage them**.
    ///
    /// That default is a deliberate operator decision, not an accident: a book left resting through
    /// a deploy is what many operators want, and cancelling on the way out would be a silent
    /// behavior change for every existing deployment. An operator who would rather leave nothing
    /// behind flips one value — `cancel_orders_on_shutdown` in `<project>/settings/flags.toml`
    /// (`vike-tradehub` is the reader).
    ///
    /// ⚠ IT CANCELS; IT DOES NOT FLATTEN. Positions are untouched — this is "leave no resting
    /// orders", never "get me out" ([`vike_exec::OrderIntent::MarketExit`] is that verb, and it is an
    /// operator action, not something a stop may decide on its own). It is BEST-EFFORT like every
    /// other cancel path here: the cancels are queued to each venue's `ExecActor` before the detach
    /// and drained by that actor's own teardown, but no venue `OrderCanceled` is waited for. The
    /// whole teardown is hard-capped by the caller's deadline
    /// (`vike_ops::shutdown::run_with_deadline`), which abandons whatever is still in flight, so a
    /// wedged venue can never stop the process exiting.
    ///
    /// ⚠ IT CANNOT RUN ON A PATH THAT NEVER REACHES TEARDOWN — but as of the graceful-stop change
    /// `systemctl stop` IS such a path. This comment previously said the opposite, in capitals: that
    /// no signal handler existed, so SIGTERM ended the process where it stood and this flag was inert
    /// under a service. That was true until `vike_ops::stop`'s handler landed and
    /// `crates/vike-tradehub/src/tradehub_cli.rs` began waiting on the flag instead of parking.
    ///
    /// ⚠ SO READ THE CHANGE IN BEHAVIOUR BEFORE ENABLING THIS: on a build carrying that change, the
    /// FIRST `systemctl stop` after deploying will sweep the resting book. That is the intended
    /// behaviour and the reason the flag exists — but anyone who set it while the old comment was
    /// true set it believing it could not fire as a service.
    ///
    /// Still effective on the interactive paths (`shutdown`/`quit`/Ctrl-D), and still subject to the
    /// hard cap above: what the deadline abandons is not cancelled. `docs/ops/kill-switches.md` and
    /// `docs/ops/graceful-stop.md` carry the operator-facing versions.
    pub cancel_orders_on_shutdown: bool,
    /// RUNTIME strategy-mount resolver (split-plane B5) — what turns a
    /// [`vike_exec::MountSpec`]'s `name`/`rhai`+`params` vocabulary into a live
    /// `Box<dyn Strategy<LiveBroker>>` when a [`Command::MountStrategy`] arrives. Injected by the
    /// COMPOSITION ROOT (vike-core sits below the strategy crates — vike-strategy/vike-script are
    /// dev-deps here — so the core cannot resolve a name itself; the root closes over its own
    /// registry/profile machinery, e.g. vike-tradehub's `mount_factory`). `None` (the default, and
    /// every config that predates the field) REFUSES every runtime mount with a recent-events note
    /// — byte-identical otherwise: no arm of the per-event fold reads this field.
    ///
    /// The factory runs ON the fold thread, inside the `Command::MountStrategy` arm, under the same
    /// `catch_unwind` discipline as every other user-code call site — acceptable because a mount is
    /// an OCCASIONAL operator verb (the `UpdateParams` precedent: strategy hooks already run
    /// there), never per-message work. A slow resolve (a Rhai compile) briefly delays the fold like
    /// a reconcile pass does; it never touches the p99 event path of a core nobody is mounting
    /// into.
    pub strategy_factory: Option<StrategyFactory>,
}

/// The [`CoreConfig::strategy_factory`] shape: resolve one runtime [`vike_exec::MountSpec`] into
/// the strategy object to mount, or a human-readable refusal (surfaced verbatim in the core's
/// recent-events ring). `FnMut` so a root's factory may keep state (e.g. a compile cache).
pub type StrategyFactory = Box<
    dyn FnMut(&vike_exec::MountSpec) -> Result<Box<dyn Strategy<LiveBroker> + Send>, String> + Send,
>;

/// Dequeue observer (latency harness / diagnostics). Runs under the core's panic guard.
pub type DequeuedHook = Box<dyn FnMut(&Ingest) + Send>;

/// The equity sampler's sink (portfolio-observer PR-3) — see [`CoreConfig::on_equity_sample`].
pub type EquitySampleHook = Box<dyn FnMut(&[EquitySample]) + Send>;

/// Builds a per-venue [`vike_exec::recon::JournalView`] from the materialized exec log — see
/// [`CoreConfig::journal_view_provider`]. Supplied by `vike-app-core` (which can read the store).
pub type JournalViewHook = Box<dyn Fn(&str) -> vike_exec::recon::JournalView + Send>;

/// One buffered order intent from a strategy handler, drained after the handler returns. `tag` is
/// a strategy-chosen stable id (HFT modify surface) — `None` for the portable [`Broker`] verbs.
///
/// `pub` (with the sibling `Buffered*` intents + [`LiveBroker`]'s buffer fields below) so the
/// `vike-mm` maker crate's white-box tests can build a `LiveBroker` and read back the verbs the
/// generic `SpreadMaker` pushed — a TEST-only surface (vike-mm dev-deps vike-core); the live path
/// still populates/drains these internally exactly as before.
pub struct BufferedSubmit {
    /// The symbol the STRATEGY named, when it named one. `None` (every inherent verb —
    /// `order_target`, `set_holdings`, the tagged HFT lane) means "this mount's own symbol".
    ///
    /// It is recorded UNCONDITIONALLY by the `Broker` verbs and resolved at DRAIN time against
    /// the mount's [`StrategyMount::symbols`] declaration: an undeclared mount ignores it exactly
    /// as before. Recording here rather than deciding in the broker is what keeps all nine
    /// `LiveBroker` construction sites untouched.
    pub symbol: Option<String>,
    pub side: i32,
    pub qty: f64,
    pub order_type: String,
    pub price: Option<f64>,
    pub reduce_only: bool,
    pub tag: Option<String>,
}

/// One buffered modify-by-tag intent (RUST-NATIVE HFT surface; resolved to a coid at drain time).
pub struct BufferedModify {
    pub tag: String,
    pub new_qty: Option<f64>,
    pub new_price: Option<f64>,
}

/// One buffered bracket intent (entry + SL + TP). venue/symbol come from the mount at drain time;
/// the three coids are minted then, so id minting stays in the runtime's one place.
pub struct BufferedBracket {
    /// See [`BufferedSubmit::symbol`]. `None` = the mount's own symbol.
    pub symbol: Option<String>,
    pub side: i32,
    pub qty: f64,
    pub entry_price: Option<f64>,
    pub stop_loss: f64,
    pub take_profit: f64,
}

/// One buffered conditional-order intent (stop / trailing) for the core-owned
/// [`crate::emulator::ConditionalBook`] — port of `exec/conditionals.py` registration:
/// ARMING bypasses the gate (only the FIRE goes mint → RiskGate → client). Trailing
/// extremes seed from the current mark at drain time (refused without a mark, like
/// `live_portfolio_engine.py::submit_trailing`).
pub struct BufferedConditional {
    /// See [`BufferedSubmit::symbol`]. `None` = the mount's own symbol.
    pub symbol: Option<String>,
    pub side: i32,
    pub qty: f64,
    /// Some = fixed stop trigger price
    pub price: Option<f64>,
    /// Some = trailing distance
    pub trail: Option<f64>,
}

/// One EXTRA leg a mount may trade beyond its own `(venue, symbol)` — see
/// [`StrategyMount::symbols`].
///
/// `venue: None` (the common case) means the mount's OWN venue: the leg is a second instrument
/// on the same exchange. `Some(v)` routes it to a DIFFERENT venue, which is what a cross-exchange
/// strategy needs — an xEMM maker rests on one venue and hedges on another, so its two legs
/// cannot share a venue by construction.
///
/// The shape mirrors `vike_strategy::ControllerHarness`'s `venue_map` (`symbol -> venue`, empty
/// = single-venue): the same problem was already solved once for the funding-carry controller,
/// and two different answers to "which venue does this symbol trade on" would be a bug waiting
/// to happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountLeg {
    pub symbol: String,
    /// `None` = the mount's own venue.
    pub venue: Option<String>,
}

impl MountLeg {
    /// A leg on the mount's OWN venue.
    pub fn same_venue(symbol: impl Into<String>) -> Self {
        MountLeg { symbol: symbol.into(), venue: None }
    }

    /// A leg on a DIFFERENT venue — the cross-exchange case.
    pub fn at(symbol: impl Into<String>, venue: impl Into<String>) -> Self {
        MountLeg { symbol: symbol.into(), venue: Some(venue.into()) }
    }
}

/// A strategy mounted on one (venue, symbol, interval) bar series.
pub struct StrategyMount {
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    /// **WHICH ACCOUNT of [`Self::venue`] this mount trades on.** `None` — every mount that existed
    /// before this field — is the venue's DEFAULT account, whose route key IS the bare venue id, so
    /// [`assemble_core`] resolves the identical engine index the venue lookup resolved and nothing
    /// about a single-account process changes.
    ///
    /// `Some(label)` is resolved ONCE, at assembly, into [`CoreThread::mount_engine`] — an INDEX —
    /// and every lane that used to ask "which engine does this venue belong to" asks that vector
    /// instead. Two properties follow, and both are the point:
    ///
    /// * the account never touches a venue STRING. Decorating `venue` (the `"binance#ALT"` trap
    ///   `vike_exec::route_key`'s module doc names) would route correctly and then silently break
    ///   `ExecutionEngine::apply_snapshot`'s reap filter, `resolved_position_price`'s position
    ///   lookup and `local_view`'s filter, all of which compare against the engine's CANONICAL
    ///   venue — while fills arrive from a bridge that carries the canonical id and cannot know
    ///   accounts exist;
    /// * the mount's READS move with its writes. `Broker::position`/`equity`/`multiplier`/
    ///   `lot_size` resolve through the same `mount_engine` index, so a mount cannot trade one book
    ///   while sizing against another — which is a worse defect than the one this field fixes and
    ///   one no order-routing test would catch.
    ///
    /// ⚠ It is deliberately NOT part of the derived mount ID (`crate::strategy_state::mount_id_with`):
    /// the id is a state-sidecar FILENAME and the journal attribution key, so folding the account in
    /// would rename every existing sidecar. Two mounts differing only by account therefore collide
    /// on the id and are refused by the existing duplicate-id rule, which correctly tells the
    /// operator to name a `controller_id`.
    pub account: Option<vike_model::account_keys::AccountLabel>,
    pub strategy: Box<dyn Strategy<LiveBroker> + Send>,
    /// Opt-in ADDITIONAL symbols (same `venue`) this mount may trade and read — the declaration
    /// that turns the otherwise-IGNORED `symbol` argument of the [`vike_model::Broker`] order verbs
    /// into an AUTHORITATIVE one for this mount.
    ///
    /// EMPTY — every mount that exists today — keeps the single-symbol contract exactly:
    /// `drain_broker` stamps the mount's own `(venue, symbol)` onto every intent and the verbs'
    /// `symbol` argument is ignored, byte-identically to before this field existed.
    ///
    /// **Why a declaration is required rather than simply honouring the argument.** Shipped
    /// strategies pass a symbol they never expect to be used: live bars carry `Bar::symbol == None`
    /// (`vike_bridge_core::klines::kline_to_bar`), so a strategy doing
    /// `bar.symbol.clone().unwrap_or_default()` passes `""` — `vike-backtest`'s `r7_gate.rs`,
    /// `vike-core`'s `safe_state_tests.rs` and `vike-script`'s live bar lane all say so explicitly.
    /// Honouring the argument unconditionally would send `symbol: ""` to venues. The MOUNT, not the
    /// call site, is therefore where the intent has to be expressed.
    ///
    /// It exists because a TWO-LEG strategy (a pairs trade, an xEMM hedge) cannot be split across
    /// two mounts: a mount OWNS its strategy (`Box<dyn Strategy>`, not `Arc`), so two mounts hold
    /// two different objects with two different states and there is no shared-state seam. ONE mount
    /// emitting orders for more than one symbol is the only available shape.
    ///
    /// WHAT READS IT TODAY (it is no longer inert — an earlier revision of this doc said it was,
    /// and that has been stale since #916/#924/#997):
    /// - `drain_broker` → `resolve_intent_symbol` (a declared symbol on an order verb is honoured;
    ///   an UNDECLARED one is refused, never rewritten) and `resolve_intent_venue` (a
    ///   [`MountLeg::at`] leg's orders route to THAT venue's engine);
    /// - `declared_views` (the per-symbol `Broker::position`/`price` tables — which carry every
    ///   declared instrument plus the mount's OWN symbol, minus whichever series is DISPATCHING,
    ///   because the ctx scalars are that series' own and fresher answer; built on EVERY
    ///   strategy-hook lane, and each row read out of that leg's OWN venue's engine — plus the
    ///   `Broker::bars` table, which carries the dispatching symbol TOO and is therefore what
    ///   `LiveBroker::carries` reads to tell an uncarried symbol from a dispatching one);
    /// - the BAR and TICK dispatch predicates ([`CoreThread::drive_strategy`],
    ///   [`CoreThread::drive_strategy_tick`]) — a declared SAME-VENUE symbol's bars/ticks reach the
    ///   mount, and live bars now carry their series symbol so the legs are distinguishable;
    /// - [`CoreThread::drive_strategy_reference_quote`] — a leg declared on a DIFFERENT venue has
    ///   that venue's L1 touch delivered to [`Strategy::on_reference_quote`] (the xEMM lane).
    ///
    /// ⚠ STILL NOT ROUTED for a leg on a DIFFERENT venue: its BARS (the bar lane additionally
    /// requires that venue's engine to `accepts_symbol` the leg), its FEED STATUS (`FeedStatus`
    /// carries neither venue nor symbol) and its L2 BOOK (`L2Book` carries neither, so it cannot be
    /// attributed — its derived L1 is delivered instead).
    ///
    /// ⚠ ALL FOUR read gaps `docs/superpowers/specs/2026-08-07-multi-symbol-read-half.md` listed are
    /// CLOSED. The empty per-symbol tables on the non-dispatch lanes, the mount-venue resolution of a
    /// cross-venue leg and the drain-keyed tag registry went first
    /// (`crates/vike-core/tests/wiring/multi_symbol_reads.rs` is the regression proof for those
    /// three); `LiveBroker::bars` — which
    /// ignored its `symbol` argument on EVERY lane and returned the dispatching series — went with
    /// `LiveBroker::bar_views`, pinned by `crates/vike-backtest/tests/multi_symbol_read_parity.rs`'s
    /// `live_bars_are_symbol_addressed`. A symbol a DECLARED mount does not carry now reads EMPTY
    /// (`0.0` / `0.0` / `&[]`) instead of the dispatching series' numbers, which is the read-side
    /// mirror of `resolve_intent_symbol` REFUSING that same symbol on the write side.
    pub symbols: Vec<MountLeg>,
    /// Optional CROSS-SYMBOL underlying/reference series this mount WATCHES ("Option B" routing): a
    /// DIFFERENT `symbol` (same `venue`) whose marks the strategy anchors on via [`Strategy::on_mark`]
    /// — e.g. the `btcusdt` RTDS spot a Polymarket BTC up/down maker blends its fair mid toward. When
    /// `Some`, the runtime routes that symbol's drained marks to this mount's `on_mark`, building the
    /// broker ctx on the mount's OWN (venue, symbol). `None` (the default for every existing mount) ⇒
    /// no mark is ever routed here and [`CoreThread::drive_strategy_mark`] early-returns, so a run
    /// with no underlying-anchored mount is byte-identical.
    pub underlying_symbol: Option<String>,
    /// Opt-in explicit CONTROLLER ID naming THIS mount (multi-mount correctness, gap C). When
    /// `Some`, it IS the mount id ([`crate::strategy_state::mount_id_with`], sanitized to a safe
    /// filename segment); `None` — every mount that exists today — keeps the legacy
    /// `{venue}__{symbol}__{interval}` derivation, so its state sidecar, its journal `mount_id`
    /// provenance and its budget/schedule keys are all byte-identical to before this field existed.
    ///
    /// It exists because the derived identity is NOT unique: two strategies mounted on the SAME
    /// `(venue, symbol, interval)` collide onto one state sidecar file and one journal `mount_id`.
    /// [`assemble_core`] asserts mount-id uniqueness and PANICS at mount time on a duplicate rather
    /// than silently sharing state, so a same-triple pair must name its mounts here.
    ///
    /// **The identity lives on the MOUNT, not beside it.** An earlier shape kept these ids in a
    /// POSITIONAL `CoreConfig` vec indexed by assembly order; reordering [`CoreConfig::extra_mounts`]
    /// then silently reassigned every id, so a strategy would cross-load a SIBLING's durable state on
    /// the next start (a mount id is both a sidecar FILENAME and the journal attribution key). A
    /// field on the mount cannot drift that way — see
    /// `multi_mount_tests::reordering_extra_mounts_preserves_each_mount_id`.
    pub controller_id: Option<String>,
}

/// Optional per-mount risk BUDGET (steal/core-per-mount-budget) — the SCOPED counterpart to the
/// account-wide [`CoreConfig::max_drawdown`] latch. When a mount's attributed loss or gross
/// notional exceeds its cap, the runtime latches THAT mount liquidate-only (cancels only its own
/// resting orders + optionally flattens its own attributed net position) while every OTHER mount
/// keeps trading — so a runaway experimental mount can no longer burn the shared account and take
/// the profitable maker's global drawdown latch down with it.
///
/// Supplied via [`CoreConfig::mount_budgets`], keyed by the mount's ID. Both arms `None`/`<= 0.0`
/// (the [`Default`]) is INERT — treated as no budget, so it never latches and never even arms the
/// sweep (the byte-identical path).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MountBudget {
    /// Max tolerated cumulative LOSS (realized net of fees + resolver-marked unrealized), as a
    /// POSITIVE account-currency number, before this mount latches liquidate-only. `None` (or
    /// `<= 0.0`) disables the loss arm.
    pub max_loss: Option<f64>,
    /// Max tolerated GROSS notional exposure (resolver-priced) of this mount's attributed net
    /// position before it latches liquidate-only. `None` (or `<= 0.0`) disables the notional arm.
    pub max_notional: Option<f64>,
    /// When `true`, the latch ALSO submits a reduce-only MARKET to flatten the mount's attributed
    /// net position (journaled write-ahead like a margin-call liquidation), not just cancel its
    /// resting orders. `false` (default) = cancel-only (leave the position for the operator).
    pub flatten_on_breach: bool,
}

impl MountBudget {
    /// Whether either arm is a positive cap. A budget with both arms `None`/`<= 0.0` never latches
    /// — treated as absent (keeps `any_mount_budget` false, i.e. the byte-identical path).
    pub fn is_active(&self) -> bool {
        self.max_loss.is_some_and(|v| v > 0.0) || self.max_notional.is_some_and(|v| v > 0.0)
    }
}

/// Per-mount fill ATTRIBUTION ledger (steal/core-per-mount-budget) — one per mount slot, folded
/// from that mount's OWN attributed fills (coid -> mount via `CoreThread::coid_mount`) at fill
/// cadence in `dispatch_applied_fills`, NEVER the per-message hot fold. Pure numbers (Copy) so the
/// per-closed-bar budget sweep and the publish-time view can snapshot it cheaply; the (venue,
/// symbol) it prices against comes from the parallel `CoreThread::mount_vs`, not from here. The
/// position/avg-px fold mirrors `Account::fold` exactly (both call `vike_model::compute_fill`), so
/// a mount that trades ONE symbol tracks the same weighted-average-cost position the account does.
#[derive(Debug, Clone, Copy, Default)]
struct MountAttribution {
    /// signed net position folded from this mount's attributed fills
    size: f64,
    /// weighted-average entry price of the folded position (0.0 while flat)
    avg_px: f64,
    /// cumulative GROSS realized price PnL on this mount's closed portions
    realized_pnl: f64,
    /// cumulative commission/fees on this mount's attributed fills (signed; a maker rebate is < 0)
    fees_paid: f64,
}

/// Per-mount readiness (portfolio-observer PR-4 T5) — one entry in `CoreThread::mount_states`,
/// parallel to `CoreThread::mounts` (same index). A mount starts `Pending` only when
/// [`CoreConfig::readiness_gate`] is on ([`assemble_core`] seeds every slot `Ready` otherwise, so
/// the gate-off path never allocates a `Pending` state at all). `Pending` still receives every
/// strategy-hook call ([`CoreThread::drive_strategy`] / `drive_strategy_tick` / ... all still
/// call the hook) — only the ORDER OUTPUT is gated, at [`CoreThread::drain_broker`]. The boundary
/// probe ([`CoreThread::maintain_mount_readiness`]) is the ONLY place a mount ever flips
/// `Pending -> Ready`; nothing ever flips it back (a mount that has traded once stays eligible).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MountState {
    /// Not yet priced: the mount's (venue, symbol) has never resolved a bid OR ask/mark/trade/bar
    /// through the `PriceBoard`. Buffered order intents from this mount's hook calls are dropped
    /// at `drain_broker` — the strategy trades on paper-thin air otherwise (no venue-informed
    /// price to size/cross against), so nothing is submitted until there is one.
    Pending,
    /// Priced at least once (or the gate is off, in which case every mount starts here and stays
    /// here): `drain_broker` drains this mount's buffered intents to the engine normally.
    Ready,
}

impl Default for CoreConfig {
    fn default() -> Self {
        CoreConfig {
            seed_cash: 0.0,
            snapshot_interval: Duration::from_millis(16),
            recent_events_cap: 64,
            ingest_capacity: 8192,
            batch_max: 1024,
            clock: Box::new(LiveClock),
            repaint: None,
            panic_on_trade_id: None,
            on_dequeued: None,
            strategy: None,
            submit_ack_timeout: None,
            // Conservative default (bumped 5s → 15s per the confirm-race hardening): a single grace
            // of 15s already exceeds the worst-case adapter confirm (Bybit's realtime→history re-query
            // ≈ 2×5s = 10s), and the in-flight-confirm guard gives a still-pending confirm up to
            // 2×15s = 30s to land before the last-resort reject — so even a much slower venue confirm
            // is never raced into a phantom reject. Only in effect once `submit_ack_timeout` is opted
            // in. See the `submit_ack_confirm_grace` field docs for the hard lower bound.
            submit_ack_confirm_grace: Duration::from_secs(15),
            margin_call: None,
            max_drawdown: None,
            conditionals_on_ticks: false,
            extra_mounts: Vec::new(),
            journal: None,
            coid_session: None,
            arm_seq: 0,
            conditionals: Vec::new(),
            contingencies: Vec::new(),
            mount_attr: Vec::new(),
            coid_mounts: Vec::new(),
            counters_path: None,
            state_dir: None,
            state_save: None,
            equity_sample: None,
            on_equity_sample: None,
            journal_view_provider: None,
            price_cfg: PriceCfg::default(),
            mark_staleness_ms: 10_000,
            reconcile_mark_staleness_ms: 150_000,
            readiness_gate: false,
            deadman: None,
            gtd_sweep: None,
            portfolio_snapshot_interval: None,
            inflight_confirm: None,
            oco_cancel_sibling_on_dead_exit: false,
            mount_budgets: std::collections::HashMap::new(),
            mount_schedules: std::collections::HashMap::new(),
            schedule_poll: None,
            // OFF = today's behavior, byte-identical: teardown detaches and exits, resting orders
            // stay live at the venue. See the field doc for why that is the default.
            cancel_orders_on_shutdown: false,
            // No resolver = every runtime MountStrategy refuses (split-plane B5); byte-identical
            // for every config that predates the field.
            strategy_factory: None,
        }
    }
}

/// Spawn the single-writer core thread owning `engine`. The pinned contract in one place.
pub fn spawn_core<C: ExecutionClient + Send + 'static>(
    engine: ExecutionEngine<C>,
    config: CoreConfig,
) -> CoreHandle {
    spawn_core_multi(engine, Vec::new(), config)
}

/// Cross-venue entry: ONE core thread driving the primary engine plus one engine per
/// extra venue — each `(seed_cash, engine)` keeps its own Account/RiskGate (the
/// CrossVenueDriver firewall, now structural in the runtime). Venue-tagged events and
/// `OrderIntent::Submit` route by venue; order-lifecycle replies route via the coid map.
/// Use `C = Box<dyn ExecutionClient + Send>` for heterogeneous venue adapters.
/// Strategy mounts remain primary-venue in this slice (named next increment).
pub fn spawn_core_multi<C: ExecutionClient + Send + 'static>(
    mut engine: ExecutionEngine<C>,
    mut extra_engines: Vec<(f64, ExecutionEngine<C>)>,
    config: CoreConfig,
) -> CoreHandle {
    // Pre-warm the ustr intern table off the hot path: the first Ustr construction builds the
    // global table (~3.2 ms one-time). Doing it here, synchronously before the core thread spawns
    // and before any venue feed connects, keeps that cost off the first live fill/quote.
    vike_model::events::prewarm_interner();

    // applied-fill capture (Strategy::on_fill delivery) only when a strategy is mounted,
    // so a GUI-only engine never grows the buffer
    engine.collect_applied_fills = config.strategy.is_some() || !config.extra_mounts.is_empty();
    engine.equity_seed = config.seed_cash;
    // The ONE resolver-config source (`CoreConfig::price_cfg`), imposed on every engine's own
    // `price_cfg` so the exec-internal decision sites (the pre-trade gate) read the SAME knobs
    // the snapshot/sampler/watchdog pass explicitly. Like `equity_seed`, config-not-state:
    // re-imposed here on restore too.
    engine.price_cfg = config.price_cfg;
    // Same config-not-state discipline for the account mark-slot ownership windows (streamed AND
    // reconcile): the law lives in `Account::set_mark_from`, so both knobs have to reach every
    // Account the core folds.
    engine.account.set_mark_staleness_ms(config.mark_staleness_ms);
    engine.account.set_reconcile_staleness_ms(config.reconcile_mark_staleness_ms);
    for (seed, e) in extra_engines.iter_mut() {
        e.equity_seed = *seed;
        e.price_cfg = config.price_cfg;
        e.account.set_mark_staleness_ms(config.mark_staleness_ms);
        e.account.set_reconcile_staleness_ms(config.reconcile_mark_staleness_ms);
    }
    let watchdog_timeout = config.submit_ack_timeout; // copy out before `config` moves into CoreThread
    let deadman_timeout = config.deadman.as_ref().map(|c| c.timeout); // ditto — waker cadence below
    // Core-ergonomics: the managed-GTD sweep and the periodic portfolio snapshot ride the SAME
    // boundary waker (no thread of their own) — copied out here for the cadence fold below. The
    // portfolio one is included only when a journal exists, mirroring its arm condition exactly
    // (`arm_boundary_timers`): no journal ⇒ no timer ⇒ nothing to wake for.
    let gtd_interval = config.gtd_sweep;
    let portfolio_snap_interval = config.journal.as_ref().and(config.portfolio_snapshot_interval);
    let inflight_interval = config.inflight_confirm; // recon path-to-superset (F1-A): waker cadence below
    // steal/core-live-scheduler: the wall-clock schedule poll rides the SAME boundary waker (no
    // thread of its own). Its cadence is `schedule_poll` (default 1s), contributed to the waker ONLY
    // when some mount actually has a non-empty schedule (mirroring the journal-gated portfolio snap).
    let schedule_interval = if config.mount_schedules.values().any(|s| !s.is_empty()) {
        Some(config.schedule_poll.unwrap_or(Duration::from_millis(1000)))
    } else {
        None
    };
    let (tx, rx) = mpsc::channel::<Ingest>(config.ingest_capacity);
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
    let rejected = Arc::new(AtomicU64::new(0));

    let core = assemble_core(
        engine,
        extra_engines,
        config,
        Arc::clone(&market),
        Arc::clone(&snapshot),
        Arc::clone(&rejected),
    );
    let join = std::thread::Builder::new()
        .name("vt-core".into())
        .spawn(move || {
            // Opt-in HFT pinning (VIKE_PIN_CORES=core:N): keep the single-writer hot hop on a fixed
            // core. No-op unless the env names the `core` role — the default desktop path.
            vike_exec::affinity::pin_current_thread(vike_exec::affinity::Role::Core, "vt-core");
            core.run(rx)
        })
        .expect("spawn vt-core");

    // audit C3 + dead-man's switch: opt-in boundary WAKER thread. A WEAK sender is the linchpin —
    // it does NOT keep the ingest channel open, so the core's "every sender dropped -> break" clean
    // exit still fires; the timer self-exits when `upgrade()` returns None. A full queue (try_send
    // Err) just means the core is busy — the next tick retries. `Ingest::Watchdog` is a NO-OP
    // dispatch now (a pure waker: the stuck-order sweep AND the dead-man sweep both run at the
    // drain-loop boundary off the `DeadlineTimerWheel`); the waker only guarantees an idle core
    // reaches that boundary on cadence. The tick is the SMALLEST half-timeout among the enabled
    // features (stuck-order watchdog and/or dead-man) so BOTH sweeps fire on time. Never spawned
    // when both are disabled (the default), so zero idle cost.
    let waker_tick = [
        watchdog_timeout,
        deadman_timeout,
        gtd_interval,
        portfolio_snap_interval,
        inflight_interval,
        schedule_interval,
    ]
    .into_iter()
    .flatten()
    .map(|t| (t / 2).max(Duration::from_millis(50)))
    .min();
    if let Some(tick) = waker_tick {
        let weak = tx.downgrade();
        std::thread::Builder::new()
            .name("vt-core-watchdog".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(tick);
                    match weak.upgrade() {
                        Some(tx) => {
                            let _ = tx.try_send(Ingest::Watchdog);
                        }
                        None => break,
                    }
                }
            })
            .expect("spawn vt-core-watchdog");
    }

    CoreHandle { ingest: tx, market, snapshot, rejected, join }
}

/// The SYMBOL a venue-tagged payload names, for the two-accounts-of-one-exchange disambiguation in
/// [`CoreThread::route_event`]. `None` for every other event, and — importantly — for
/// [`Event::AccountState`].
///
/// ⚠ **`AccountState` has no symbol, and it does not need one.** It is an account-wide balance
/// snapshot, so it never had a symbol to be disambiguated by — the residual this used to declare.
/// It is now closed by [`event_route_key`] instead: the payload carries the route key outright,
/// stamped by the MOUNT (`vike_mount::account_event_sender`), which is the layer that knows which
/// account a venue lane belongs to. Its POSITIONS and FILLS were never affected — those are
/// symbol-tagged and route exactly.
fn event_symbol(ev: &Event) -> Option<&str> {
    match ev {
        Event::Fill(f) => Some(&f.symbol),
        Event::Funding(f) => Some(&f.symbol),
        Event::PositionLiquidated(p) => Some(&p.symbol),
        _ => None,
    }
}

/// The route key a venue-tagged payload carries OUTRIGHT — the exact answer to "which account of
/// this exchange", consulted by [`CoreThread::route_event`] before the symbol disambiguator.
///
/// **EVERY venue-tagged payload with no CLIENT-ORDER-ID answers here** — [`Event::AccountState`],
/// [`Event::Funding`] and [`Event::PositionLiquidated`] — and the membership rule is exactly that:
/// a coid or a stamped key, never a symbol.
///
/// ⚠ **The last two are a CORRECTION, and the reasoning that omitted them is kept because it was
/// nearly right.** The rule used to be "only `AccountState`, because every other venue-tagged
/// payload names a symbol and a symbol resolves EXACTLY — the mount's symbol-collision rule makes
/// it unique per account within a venue". When that rule was deleted (two accounts on one
/// instrument is an ordinary spread — `vike_config::venue_accounts`), the justification was
/// rewritten to "what carries the other payloads instead is their client-order-id", and THAT is
/// where it went wrong: it is true of [`Event::Fill`] and of nothing else.
/// [`Event::Funding`] and [`Event::PositionLiquidated`] carry a symbol and no coid, so on the very
/// configuration a labelled mount creates — both engines claiming one symbol, so
/// [`CoreThread::engine_idx_for_venue_symbol`] answers `None` — they fell through to the venue
/// lookup and folded into the venue's DEFAULT engine. A labelled account's funding debit landed on
/// the default account's `balance`; a labelled account's liquidation CLOSED the default account's
/// position at the venue's liq price while the liquidated account went on reporting it open.
/// `vike_model::events::FundingEvent::route_key` carries the same argument from the wire's side.
///
/// [`Event::Fill`] stays OUT deliberately: [`event_coid`] resolves it through the submit-time
/// `coid_venue` map, exactly, for every order this process placed and with nothing on the wire — so
/// stamping it would put a redundant field on the one shape the frozen parity fixtures pin, to
/// answer a question already answered.
///
/// `None` on a default-account box — nothing stamps a key equal to its own venue
/// (`vike_exec::EventSender::routed`), so this returns `None` for every payload such a box has
/// ever seen and the caller takes the branch it always took.
fn event_route_key(ev: &Event) -> Option<&str> {
    match ev {
        Event::AccountState(a) => a.route_key.as_ref().map(|k| k.as_str()),
        Event::Funding(f) => f.route_key.as_ref().map(|k| k.as_str()),
        Event::PositionLiquidated(p) => p.route_key.as_ref().map(|k| k.as_str()),
        _ => None,
    }
}

/// Order-lifecycle reply routing key: the coid every lifecycle event carries.
///
/// ⚠ **[`Event::Fill`] is in this list and is the one VENUE-TAGGED payload that is**, which reads
/// as an inconsistency until you look at where it is consulted. A bare execution report names the
/// order it belongs to, and [`CoreThread::route_event`] uses that as its second-most-exact answer —
/// above the symbol, below the payload's own stamped route key — because a coid resolves through
/// the submit-time `coid_venue` map and is therefore EXACT for every order this process placed,
/// while a symbol is exact only while one engine of the venue claims it. It changes nothing for a
/// payload with no venue: `route_event`'s venue arm returns before the coid fallback below, so the
/// non-venue-tagged lifecycle events reach that fallback exactly as they always did.
fn event_coid(ev: &Event) -> Option<&str> {
    match ev {
        Event::Fill(e) => Some(&e.client_order_id),
        Event::OrderSubmitted(e) => Some(&e.client_order_id),
        Event::OrderAccepted(e) => Some(&e.client_order_id),
        Event::OrderRejected(e) => Some(&e.client_order_id),
        Event::OrderDenied(e) => Some(&e.client_order_id),
        Event::OrderTriggered(e) => Some(&e.client_order_id),
        Event::OrderPartiallyFilled(e) => Some(&e.client_order_id),
        Event::OrderFilled(e) => Some(&e.client_order_id),
        Event::OrderCanceled(e) => Some(&e.client_order_id),
        Event::OrderExpired(e) => Some(&e.client_order_id),
        Event::OrderLiquidated(e) => Some(&e.client_order_id),
        Event::OrderModified(e) => Some(&e.client_order_id),
        Event::OrderCancelRejected(e) => Some(&e.client_order_id),
        Event::OrderModifyRejected(e) => Some(&e.client_order_id),
        _ => None,
    }
}

/// **THE mount → engine resolution.** One function, called from [`assemble_core`] at spawn and from
/// [`CoreThread::mount_strategy_runtime`] for a mount added at runtime, so the two cannot disagree
/// about which engine a mount belongs to.
///
/// Two arms, and the asymmetry between them is requirement-shaped rather than stylistic:
///
/// * **`account: None`** — the venue's DEFAULT account. `RouteKey::sole_account_of(venue)` is
///   exactly the lookup every strategy lane used to make, and `unwrap_or(0)` is exactly the
///   fallback each of them applied, so an account-less mount resolves the index it has always
///   resolved. Byte-identical BY CONSTRUCTION, including the historical tolerance of a mount on a
///   venue this core runs no engine for (a paper/test core), which must keep working.
/// * **`account: Some(label)`** — a DECLARED route key (`venue#LABEL`, rendered by
///   `vike_model::account_keys::route_key_of`, the ONE spelling the mount fan-out stamps and the
///   live lock names its sentinel after), and a miss is a PANIC. `unwrap_or(0)` here would be the
///   catastrophe stated as three characters: a strategy whose operator named `ALT` would trade the
///   DEFAULT account's book, silently, with no error anywhere. The composition root has already
///   refused this case loudly and by name (`vike_run::build_live_strategy_core`'s arming check
///   drops the mount); this is the backstop no future root can reach past, and it matches
///   [`assemble_core`]'s own duplicate-id doctrine — a configuration fault caught before a single
///   order exists.
fn mount_engine_idx<C: ExecutionClient>(
    engine: &ExecutionEngine<C>,
    extra_engines: &[(f64, ExecutionEngine<C>)],
    mount: &StrategyMount,
    mount_id: &str,
) -> usize {
    let idx_of = |key: &str| -> Option<usize> {
        if key == engine.route_key {
            return Some(0);
        }
        extra_engines.iter().position(|(_, e)| e.route_key == key).map(|i| i + 1)
    };
    match mount.account.as_ref().filter(|l| !l.is_default()) {
        None => idx_of(RouteKey::sole_account_of(&mount.venue).as_str()).unwrap_or(0),
        Some(label) => {
            let key = vike_model::account_keys::route_key_of(&mount.venue, label);
            idx_of(RouteKey::declared(&key).as_str()).unwrap_or_else(|| {
                let mounted: Vec<&str> = std::iter::once(engine.route_key.as_str())
                    .chain(extra_engines.iter().map(|(_, e)| e.route_key.as_str()))
                    .collect();
                panic!(
                    "strategy mount `{mount_id}` names account {label} of {}, and this core runs \
                     no engine with route key `{key}` (mounted: {mounted:?}). It must NOT fall \
                     through to that venue's default account — a strategy executing on an account \
                     its author did not choose is silent and unrecoverable. Arm the account \
                     (`policy.accounts.{}.{label}` in <project>/settings/policy.toml, plus its \
                     `__{label}` credential keys), or drop `account` from the mount",
                    mount.venue, mount.venue
                )
            })
        }
    }
}

/// **WHICH ENGINE an order intent is lowered onto** — the parameter that makes an account-scoped
/// mount's writes structural rather than a lookup over a venue string.
///
/// The two variants are the two kinds of caller, and the split exists because they carry different
/// information rather than because one is safer:
///
/// * **[`Self::Payload`]** — every EXTERNAL command path (a DOM click, a tradehub ticket, a CLI
///   verb, a margin-call or drawdown liquidation). None of them can name an account: they carry a
///   canonical venue and reach that venue's DEFAULT account, exactly as they always have.
/// * **[`Self::Mount`]** — a strategy's own buffered intent, lowered through
///   `CoreThread::apply_strategy_intent`, which already holds the mount index. It resolves through
///   `CoreThread::mount_engine` — the account the mount DECLARED, resolved once at assemble — so a
///   labelled mount's orders cannot reach the default account's engine by any spelling of any
///   string.
///
/// ⚠ **A `Mount` route still defers to the payload for a FOREIGN venue.** A declared leg
/// (`MountLeg::at(sym, other_venue)`) is a cross-exchange hedge; the mount's account is a fact about
/// its OWN venue and says nothing about another exchange, so a payload whose venue is not the
/// mount's engine's venue routes by the payload. That is what keeps xEMM working, and it is checked
/// against the ENGINE's canonical venue rather than the mount's string so the two can never drift.
/// * **[`Self::Engine`]** — an engine index ALREADY resolved, by a core-internal producer that
///   knows which book it is acting on but owns no mount index: the per-engine margin-call sweep
///   ([`CoreThread::sweep_margin_call_engine`]) and the conditional FIRE
///   (`CoreThread::submit_fired`, which recovers the arm's engine from `cond_engine`). Both used to
///   lower through the payload, which meant the ORDER that protects one account's book was
///   submitted to the venue's DEFAULT account — see those two sites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EngineRoute {
    /// Route by the payload's own venue string.
    Payload,
    /// Route to the engine the mount at this index declared.
    Mount(usize),
    /// Route to THIS engine index, resolved by the caller.
    Engine(usize),
}

/// The ONE `CoreThread` struct-literal construction, shared by `spawn_core` (the live runtime)
/// and the test-only `test_core` (which drives the core synchronously with no OS thread) so the
/// field literal exists in exactly one place.
fn assemble_core<C: ExecutionClient>(
    engine: ExecutionEngine<C>,
    mut extra_engines: Vec<(f64, ExecutionEngine<C>)>,
    mut config: CoreConfig,
    market: Arc<Conflated>,
    snapshot: Arc<ArcSwap<CoreSnapshot>>,
    rejected: Arc<AtomicU64>,
) -> CoreThread<C> {
    // **Does this process hold TWO ENGINES OF ONE EXCHANGE?** — see [`CoreThread::multi_account`].
    // Computed once, here, from the engines themselves rather than from a flag a caller could get
    // wrong: `vike_mount::make_engine_accounts` is what produces a second one, and it stamps a
    // distinct `route_key` on it while leaving `venue` the canonical id, so a shared `venue` IS the
    // question. `false` on every single-account process, which is every process with no
    // `[accounts]` table.
    let multi_account = {
        let mut venues: Vec<&str> = std::iter::once(engine.venue.as_str())
            .chain(extra_engines.iter().map(|(_, e)| e.venue.as_str()))
            .collect();
        venues.sort_unstable();
        let before = venues.len();
        venues.dedup();
        venues.len() != before
    };
    // Phase D: unify the primary mount + extra_mounts into one slot vector (Option per
    // slot for the take/replace dance around strategy calls)
    let mut mounts: Vec<Option<StrategyMount>> = Vec::new();
    if let Some(m) = config.strategy.take() {
        mounts.push(Some(m));
    }
    mounts.extend(config.extra_mounts.drain(..).map(Some));
    // Extra engines capture applied fills only when a mount TRADES their venue.
    //
    // ⚠ "trades" includes a DECLARED CROSS-VENUE LEG, not just the mount's own series. This test
    // used to be `m.venue == e.venue` alone, and a `MountLeg::at(sym, other_venue)` leg — the shape
    // an xEMM hedge requires by construction — left the hedge engine's capture DISARMED. That
    // engine folded the fill into its account normally, but recorded no `AppliedFill`, so
    // `dispatch_applied_fills` had nothing to deliver and `Strategy::on_fill` NEVER FIRED for the
    // hedge leg. A cross-venue maker was never told its hedge filled: no error, no event, and the
    // account quietly correct while the strategy's own view was not.
    //
    // It stayed invisible because nothing exercised it — no test anywhere in the workspace paired
    // `spawn_core_multi` with a hook assertion on a SECONDARY engine's venue, so every existing
    // multi-engine test either had no mount on the extra venue or never asked whether the hook ran.
    // `a_cross_venue_leg_fill_reaches_on_fill` is that test.
    //
    // Byte-identical for every single-venue configuration: with no declared leg naming another
    // venue the added disjunct is always false, so a mount whose own venue matches arms exactly as
    // before, and one that matches nothing stays disarmed (a GUI-only engine never grows the
    // buffer — the reason this gate exists at all).
    for (_, e) in extra_engines.iter_mut() {
        e.collect_applied_fills = mounts.iter().flatten().any(|m| {
            m.venue == e.venue
                || m.symbols.iter().any(|leg| leg.venue.as_deref() == Some(e.venue.as_str()))
        });
    }
    // Portfolio-observer PR-4 T2: derive each mount's deterministic mount_id here, at the ONE
    // mount choke point, and — when a state_dir is configured — load its durable-state sidecar
    // (if one exists) into the strategy BEFORE it ever runs. `on_start` is not called anywhere
    // today; this is load-only, no lifecycle hook is added. `mount_ids` is stored on `CoreThread`
    // (parallel to `mounts`, same indices) so save-on-stop reuses it without recomputing.
    //
    // Multi-mount correctness (gap C): the id is now `mount_id_with`, i.e. this mount's OWN
    // `StrategyMount::controller_id` when it has one, else the SAME legacy
    // `{venue}__{symbol}__{interval}` derivation as before — so every controller-id-free config
    // (all of them today) computes byte-identical ids. The uniqueness assert below is what makes
    // the id an identity rather than a hint. Reading the id OFF THE MOUNT is load-bearing, not
    // cosmetic: the id is a state-sidecar filename AND the journal attribution key, so an
    // assembly-order-indexed source would silently hand mount B the id (and therefore the durable
    // state) of mount A the moment `extra_mounts` is reordered.
    let mount_ids: Vec<String> = mounts
        .iter_mut()
        .map(|slot| {
            let m = slot.as_mut().expect("just populated above");
            let mid = crate::strategy_state::mount_id_with(
                m.controller_id.as_deref(),
                &m.venue,
                &m.symbol,
                &m.interval,
            );
            if let Some(dir) = config.state_dir.as_ref() {
                let sidecar = crate::strategy_state::sidecar_path(dir, &mid);
                if let Some(v) = crate::strategy_state::read_json(&sidecar) {
                    // `load_state` is arbitrary user strategy code — guard it like every other
                    // strategy-hook call site in this file (see `dispatch()`'s `catch_unwind`).
                    // A corrupt/incompatible sidecar must fail OPEN (start fresh, un-loaded)
                    // rather than crash app startup; `AssertUnwindSafe` is sound for the same
                    // reason the module doc gives for the dispatch guard — a strategy left
                    // mid-panic is only ever read/overwritten again, never relied on to be
                    // consistent.
                    if let Err(payload) =
                        catch_unwind(AssertUnwindSafe(|| m.strategy.load_state(&v)))
                    {
                        tracing::warn!(
                            target: "vike_core::strategy_state",
                            mount_id = %mid,
                            reason = %panic_text(payload),
                            "strategy load_state panicked — starting fresh"
                        );
                    }
                }
            }
            mid
        })
        .collect();
    // Multi-mount correctness (gap C): mount identity must be UNIQUE, and a duplicate is a
    // configuration fault the runtime must not paper over. Two mounts sharing an id share ONE state
    // sidecar file (`<state_dir>/<mount_id>.json` — the later save silently overwrites the earlier
    // strategy's durable state), ONE journal `mount_id` provenance (so the coid->mount rebuild on
    // restart cannot tell them apart), and ONE identity everywhere else the id is a key. Failing
    // LOUDLY at mount time — before a single order can be minted — is the same discipline the
    // journal open a few lines below applies (`expect`: "a live core that thinks it is journaling but
    // is not is worse than one that refuses to start"). Cost is O(mounts²) over a handful of slots,
    // once, at assembly.
    for (i, id) in mount_ids.iter().enumerate() {
        if let Some(j) = mount_ids[..i].iter().position(|prev| prev == id) {
            panic!(
                "duplicate strategy-mount id `{id}` (mounts {j} and {i}): two mounts on one \
                 (venue, symbol, interval) share a state sidecar, a journal identity and a budget \
                 key — give each a distinct `StrategyMount::controller_id`"
            );
        }
    }
    // Portfolio-observer PR-4 T5: seed each mount's readiness, parallel to `mounts`/`mount_ids`
    // (same indices, computed once, never resized after). `readiness_gate: false` (the default)
    // seeds every slot `Ready` immediately — the gate is then permanently inert (no mount is ever
    // `Pending`, so `maintain_mount_readiness` never has anything to probe and `drain_broker`'s
    // check is a compare against a constant `Ready`), which is what makes the whole feature
    // byte-identical to today when off.
    let initial_state = if config.readiness_gate { MountState::Pending } else { MountState::Ready };
    let mount_states: Vec<MountState> = vec![initial_state; mounts.len()];
    // steal/core-per-mount-budget: resolve each mount's optional [`MountBudget`] by its MOUNT ID
    // (the unique identity asserted just above — NOT the coarser `(venue, symbol, interval)` triple
    // two mounts may legitimately share), and capture its `(venue, symbol)` for the resolver — all
    // parallel to `mounts`/`mount_ids`/`mount_states` (same indices, computed once, never resized).
    // `any_mount_budget` is the byte-identical gate: false (no active budget) means the per-closed-
    // bar sweep is never called and `mount_latched` stays all-false forever, so `drain_broker`'s
    // new check is a compare against a constant. `mount_attr`/`mount_latched` seed empty/false.
    let mount_budget: Vec<Option<MountBudget>> =
        mount_ids.iter().map(|id| config.mount_budgets.get(id).copied()).collect();
    let mount_vs: Vec<(String, String)> = mounts
        .iter()
        .map(|slot| slot.as_ref().map(|m| (m.venue.clone(), m.symbol.clone())).unwrap_or_default())
        .collect();
    let any_mount_budget = mount_budget.iter().flatten().any(|b| b.is_active());
    // **WHICH ENGINE each mount trades on**, resolved ONCE, here, parallel to `mounts`/`mount_ids`
    // (same indices, never resized after — only appended to by `mount_strategy_runtime`).
    //
    // This is the whole of "a strategy names its account": every strategy lane used to ask
    // `engine_idx_for_route_key(RouteKey::sole_account_of(<some venue string>))`, which by
    // construction can only ever answer with a venue's DEFAULT account — so a mount tagged with a
    // canonical venue resolved engine 0 whatever account the operator meant. The account is turned
    // into a route key exactly ONCE (right here), and the answer is an INDEX every lane reads
    // through `CoreThread::mount_eng` / `CoreThread::route_of`.
    //
    // ⚠ The two lanes that still resolve by VENUE are the MARKET ones (`drive_strategy`'s mark +
    // price-board write and `drive_strategy_tick`'s), and they are correct: a mark is a fact about
    // the exchange, not about an account. `CoreThread::mirror_venue_price` is what carries it to the
    // venue's other accounts. `crates/vike-core/src/runtime/mount_account_tests.rs`'s
    // `a_labelled_mount_trades_and_reads_its_own_account` is the gate on the mount half.
    let mount_engine: Vec<usize> = mounts
        .iter()
        .enumerate()
        .map(|(i, slot)| {
            let m = slot.as_ref().expect("just populated above");
            mount_engine_idx(&engine, &extra_engines, m, &mount_ids[i])
        })
        .collect();
    // Multi-mount durability (gap D): the ledgers seed ZEROED, then any RESTORED rows
    // (`CoreConfig::mount_attr`, from the latest journal `Snap`) are folded back onto their slots by
    // `mount_id`. A row naming no mounted slot is dropped (inert); an empty config — the default and
    // every non-restoring caller — leaves the all-zero seed, byte-identical to today.
    let mut mount_attr: Vec<MountAttribution> = vec![MountAttribution::default(); mounts.len()];
    for row in config.mount_attr.drain(..) {
        if let Some(i) = mount_ids.iter().position(|id| *id == row.mount_id) {
            mount_attr[i] = MountAttribution {
                size: row.size,
                avg_px: row.avg_px,
                realized_pnl: row.realized_pnl,
                fees_paid: row.fees_paid,
            };
        }
    }
    // ...and the coid -> mount map its fills route through, restored the same way (by `mount_id`)
    // from the journal's own `StrategySubmit` provenance. Empty (default) = an empty map, i.e. the
    // pre-restore behavior where a pre-restart order's fill is unattributed.
    let mut coid_mount: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (coid, mount_id) in config.coid_mounts.drain(..) {
        if let Some(i) = mount_ids.iter().position(|id| *id == mount_id) {
            coid_mount.insert(coid, i);
        }
    }
    let mount_latched: Vec<bool> = vec![false; mounts.len()];
    // Per-mount DECLARED extra symbols (`StrategyMount::symbols`), parallel to `mounts` (same
    // indices) — the opt-in that makes the `Broker` verbs' `symbol` argument authoritative for
    // that mount. `any_mount_multi` is the byte-identical gate (mirrors `any_mount_budget`):
    // false — every mount that exists today — means the drain takes the identical
    // stamp-the-mount's-symbol path and every per-symbol read falls through to its scalar.
    let mount_symbols: Vec<Vec<MountLeg>> =
        mounts.iter().map(|m| m.as_ref().map(|m| m.symbols.clone()).unwrap_or_default()).collect();
    let any_mount_multi = mount_symbols.iter().any(|v| !v.is_empty());
    // The CROSS-VENUE reference-quote gate (the xEMM lane) — the narrower sibling of
    // `any_mount_multi`: `true` only when some mount declared a leg on a venue OTHER than its own,
    // which is the only shape [`CoreThread::drive_strategy_reference_quote`] can ever match. It is
    // computed HERE, once, precisely so the two per-market-message call sites in the `Ingest::Quote`
    // / `Ingest::Book` arms cost ONE BOOL LOAD on every runtime that has no such mount (i.e. every
    // runtime today) — the `p99 < 10µs` fold must not grow a `mounts` scan for a feature nobody
    // mounted. A same-venue-only declared mount (`MountLeg::same_venue`) leaves this `false`.
    let any_mount_ref = mounts.iter().enumerate().any(|(i, m)| {
        m.as_ref().is_some_and(|m| {
            mount_symbols[i].iter().any(|l| l.venue.as_deref().is_some_and(|v| v != m.venue))
        })
    });
    // steal/core-live-scheduler: resolve each mount's optional wall-clock LiveSchedule by its MOUNT
    // ID (the budget map's key, and for the same reason — see `CoreConfig::mount_schedules`) —
    // parallel to `mounts` (same indices), MOVED out of the config map (a LiveSchedule owns a Vec,
    // so unlike a Copy MountBudget it is removed, not copied).
    // `any_mount_schedule` is the byte-identical gate (mirrors `any_mount_budget`): false (no mount
    // has a non-empty schedule) ⇒ the boundary check is one bool read and `drive_schedule` never runs.
    let mount_schedule: Vec<LiveSchedule> =
        mount_ids.iter().map(|id| config.mount_schedules.remove(id).unwrap_or_default()).collect();
    let any_mount_schedule = mount_schedule.iter().any(|s| !s.is_empty());
    // Open the write-ahead journal ONCE, here, so BOTH spawn paths (single- and multi-symbol,
    // via spawn_core/spawn_core_multi) and the synchronous test core get it. A broken journal
    // dir MUST fail loudly (`expect`), never be silently dropped — a live core that thinks it is
    // journaling but is not is worse than one that refuses to start.
    let journal = config.journal.as_ref().map(|jc| {
        // Fire once at startup (here), not per-message: `snapshot_every == 0` makes the after-fold
        // `journaled_since_snap >= snapshot_every` cadence check ALWAYS true, so the core would take
        // a full-state Snap + blocking `flush()` on EVERY exec-lane message — a silent p99 cliff.
        // Turn that perf cliff loud.
        debug_assert!(
            jc.snapshot_every > 0,
            "snapshot_every must be >= 1 (0 would snapshot+flush every message)"
        );
        // TODO(follow-up): the single-writer interlock gave this `expect` a new, ROUTINE
        // operator-facing failure mode — a double-launch now lands here as an `AddrInUse`
        // io::Error and surfaces as a panic + backtrace, burying the actionable message the
        // lock error carefully composed. Follow-up: print the io::Error's Display and exit
        // non-zero instead of panicking. Left as-is in this lane (fail-loud is still correct).
        crate::journal::CommandJournal::open(&jc.dir, jc.file.clone())
            .expect("open write-ahead command journal")
    });
    // Open the opt-in mmap counter mirror ONCE here (audit co9), the same place + spirit as the
    // journal: a broken path the operator explicitly opted into MUST fail loudly at startup (before
    // any trading), never be silently dropped. `None` (default) = no file, byte-identical publish.
    let counters = config
        .counters_path
        .as_ref()
        .map(|p| crate::counters::CountersFile::create(p).expect("open mmap counters file"));
    // Resume the coid generator across a restart when a session was persisted; else a fresh
    // random session (today's behavior).
    let coid_gen = match &config.coid_session {
        Some((session, seq)) => vike_exec::ClientOrderIdGenerator::resume(session.clone(), *seq),
        None => vike_exec::ClientOrderIdGenerator::new(None),
    };
    // Dead-man's switch (trading-hardening): build the pure trip-logic state machine ONLY when the
    // feature is opted in, BEFORE `config` moves into the struct literal below. `None` (default)
    // means the observe hook + boundary sweep are both inert, byte-identical to today.
    let deadman = config.deadman.as_ref().map(DeadMan::new);
    // Read before `config` moves into the struct literal below: the arm-id counter's resume seed
    // (0 = fresh session, today's behavior). See `CoreConfig::arm_seq` for why it is resumed at all.
    let config_arm_seq = config.arm_seq;
    // Emulator PR-3 (re-arm-on-restore): seed the conditional books from the restored Snap
    // capture, ONCE, here — [`CoreConfig::conditionals`]'s doc has the contract. Entry order is
    // fire order (the Snap captured the books' insertion order), so `entry().or_default()` +
    // per-book insert reproduces both the books map's and each book's insertion order exactly.
    // Empty (default) leaves the map empty — byte-identical to a pre-PR-3 runtime. Malformed or
    // duplicate entries fail OPEN and LOUD (skip + warn), never a panic at assembly: a restore
    // must not refuse to start over one bad arm, and losing that arm silently would be worse.
    let mut conditional_books: indexmap::IndexMap<(String, String), ConditionalBook> =
        indexmap::IndexMap::new();
    for c in config.conditionals.drain(..) {
        let crate::journal::SnapConditional { arm_id, terms } = c;
        let crate::journal::ConditionalRecord {
            venue,
            symbol,
            side,
            qty,
            price,
            trail,
            extreme,
            trigger_by,
        } = terms;
        let book = conditional_books.entry((venue, symbol)).or_default();
        let armed = if let (Some(trail), Some(extreme)) = (trail, extreme) {
            book.add_trailing(&arm_id, side, qty, trail, extreme, trigger_by)
        } else if let Some(px) = price {
            book.add_stop(&arm_id, side, qty, px, trigger_by)
        } else {
            tracing::warn!(
                target: "vike_core::core",
                arm_id = %arm_id,
                "restore: conditional with neither price nor trail+extreme — skipped"
            );
            continue;
        };
        if !armed {
            tracing::warn!(
                target: "vike_core::core",
                arm_id = %arm_id,
                "restore: duplicate conditional arm id — skipped (first entry kept)"
            );
        }
    }
    // Live-runtime OTO/OCO re-arm-on-restore (the contingency twin of the conditional re-seed just
    // above): rebuild the shared `ContingencyBook` + the held-order map from the restore base's Snap
    // capture, ONCE, here. `insert_active` restores each leg's EXACT armed/held state (plain
    // `insert` would re-HOLD an already-armed exit); a HELD leg's captured request is put back into
    // `held_orders` so its parent's fill can release it. Order is the captured insertion order (the
    // book's arm/cancel iteration order). Empty (default) leaves both empty — byte-identical to a
    // pre-OCO/OTO runtime (and a pre-v11 journal's Snap carries none, so it restores empty). Zero
    // hot-fold cost: consumed entirely here.
    let mut contingency = ContingencyBook::new();
    let mut held_orders: indexmap::IndexMap<String, OrderRequest> = indexmap::IndexMap::new();
    for c in config.contingencies.drain(..) {
        let crate::journal::SnapContingency { coid, parent, linked, active, held_request } = c;
        contingency.insert_active(coid.clone(), parent, linked, active);
        if let Some(req) = held_request {
            held_orders.insert(coid, req);
        }
    }
    // Read before `config` moves into the struct literal below; see the field's construction
    // comment for why these three (and only these three) arm the waker-record write.
    let config_journal_waker_records =
        config.submit_ack_timeout.is_some() || config.deadman.is_some();
    CoreThread {
        engine,
        bus: EventBus::new(),
        config,
        market,
        snapshot,
        rejected,
        recent: VecDeque::new(),
        fault: None,
        seq: 0,
        dirty: false,
        bars: indexmap::IndexMap::new(),
        coid_gen,
        strategy_tags: indexmap::IndexMap::new(),
        mounts,
        mount_ids,
        mount_states,
        conditional_books,
        contingency,
        held_orders,
        arm_seq: config_arm_seq,
        // Refusal ids start at 0 every session and are deliberately NOT resumed — see the field.
        refusal_seq: 0,
        extra_engines,
        multi_account,
        coid_venue: std::collections::HashMap::new(),
        cond_engine: std::collections::HashMap::new(),
        journal,
        journaled_since_snap: 0,
        // WAKER-RECORD GATE (core-ergonomics review fix; NARROWED by emulator PR-5):
        // `Ingest::Watchdog` is a pure waker whose dispatch arm is `{}` — it can only matter
        // through the boundary sweeps it lets run. Of those, exactly TWO still mutate order/
        // trading state off wall clock in a way replay cannot reproduce: the stuck-order sweep and
        // the dead-man sweep. Journal the waker record only when one of those is enabled — that
        // record is what makes `replay_from` return `Unsupported`, and it MUST keep doing so for
        // those two (their wall-clock effects are not re-derivable).
        //
        // The GTD expiry sweep USED to be the third member and no longer is: it now journals each
        // expiry write-ahead (`JournalRecord::GtdExpire`) and its only local action —
        // `ExecutionEngine::cancel_order` — publishes nothing and mutates no fenced state (the
        // authoritative `OrderCanceled` arrives as its own journaled `Ingest::Event` and replays
        // independently). So a `gtd_sweep`-only core writes NO waker record and replays/crash-
        // restores normally. Enabling it ALONGSIDE the stuck-order watchdog or the dead-man switch
        // still forfeits replay — for those features' own reasons, loudly, exactly as before.
        // When none is on, the boundary can only run
        // replay-NEUTRAL observers (equity sample, strategy-state save, portfolio snapshot,
        // readiness probe), so the waker provably cannot move fenced state and journaling it would
        // only make an otherwise perfectly replayable session unreplayable. Byte-identical for
        // every pre-existing config: before this branch the waker thread was spawned ONLY when the
        // stuck-order watchdog or the dead-man switch was configured, i.e. exactly when this stays
        // `true`.
        journal_waker_records: config_journal_waker_records,
        counters,
        confirm_issued_ms: std::collections::HashMap::new(),
        pnl_curve_peak: None,
        pnl_curve_disarmed_noted: false,
        // 50ms resolution × 1024 slots ⇒ ~51s rotation; covers a typical watchdog half-timeout in
        // the bounded stepping path and falls back to the full-rotation pass for larger ones. Empty
        // until `arm_boundary_timers` arms the sweep cadence (only when the watchdog is enabled).
        timers: DeadlineTimerWheel::new(50, 1024),
        due_timers: Vec::new(),
        watchdog_tick_ms: 0,
        equity_timer: None,
        equity_rows: Vec::new(),
        state_save_timer: None,
        recon_alerts: indexmap::IndexMap::new(),
        recon_announce: recon_held::HeldAnnouncer::default(),
        recon_coin_deltas: indexmap::IndexMap::new(),
        recon_next_alert_id: 1,
        recon_last_pass_ts: 0,
        // Dead-man's switch (trading-hardening): the state machine built just above (only when the
        // feature is opted in). `deadman_tick_ms` is set later by `arm_boundary_timers`.
        deadman,
        deadman_tick_ms: 0,
        // Core-ergonomics: both tick cadences are set by `arm_boundary_timers` (only when their
        // config knob is `Some`); 0 + an empty set means the features are inert.
        gtd_tick_ms: 0,
        gtd_canceled: std::collections::HashSet::new(),
        portfolio_snap_tick_ms: 0,
        // Recon path-to-superset (F1-A): 0 tick + an empty dedup map ⇒ the fast in-flight confirm
        // is inert; `arm_boundary_timers` sets the tick only when `inflight_confirm` is `Some`.
        inflight_confirm_tick_ms: 0,
        inflight_confirm_last_ms: std::collections::HashMap::new(),
        // steal/core-per-mount-budget: attribution ledgers + coid->mount map + per-mount budget
        // latch. Both start EMPTY/zeroed unless a restart seeded them above (gap D — the map is
        // otherwise populated at strategy-submit); `any_mount_budget` gates the whole budget feature
        // to a byte-identical no-op when no mount has an active budget.
        mount_attr,
        coid_mount,
        // A RESTORED `coid_mount` seeds NO prune candidates: whether those pre-restart orders are
        // still live is not knowable from the map alone, and the queue only ever fills from a
        // TERMINAL this core actually observes. A restored entry whose order died before the restart
        // therefore lingers — bounded by the restore, not by session length, and strictly no worse
        // than before this queue existed.
        coid_terminal: VecDeque::new(),
        mount_budget,
        mount_vs,
        mount_engine,
        mount_latched,
        mount_symbols,
        any_mount_multi,
        any_mount_ref,
        any_mount_budget,
        // steal/core-live-scheduler: per-mount wall-clock schedules + their boundary gate.
        // All-empty schedules ⇒ `any_mount_schedule` false ⇒ the boundary check is one bool read
        // and the feature is inert (byte-identical).
        mount_schedule,
        any_mount_schedule,
    }
}

struct CoreThread<C: ExecutionClient> {
    engine: ExecutionEngine<C>,
    bus: EventBus,
    config: CoreConfig,
    market: Arc<Conflated>,
    snapshot: Arc<ArcSwap<CoreSnapshot>>,
    rejected: Arc<AtomicU64>,
    /// The bounded recent-events ring, held UN-RENDERED (perf audit finding #2 - see
    /// [`crate::recent`]). Rendered to `Vec<String>` once per coalesced publish, inside
    /// `CoreSnapshot::build`, instead of `format!`-ing on the per-message fold.
    recent: VecDeque<std::sync::Arc<str>>,
    fault: Option<String>,
    seq: u64,
    dirty: bool,
    /// the core-owned bar cache (plan §1 "Cache write BEFORE dispatch"); snapshots share
    /// the closed series by Arc — R7 strategies read this same cache deterministically
    bars: indexmap::IndexMap<SeriesKey, BarSeries>,
    /// client-order-id generator for the mounted strategy's orders
    coid_gen: vike_exec::ClientOrderIdGenerator,
    /// HFT modify surface: strategy-chosen tag → the current coid it minted for, so a strategy can
    /// `modify(tag, ..)` a resting order it never saw the coid of. A re-submit under a tag overwrites;
    /// stale entries (terminal orders) are harmless (modify_order gates on status).
    ///
    /// KEYED `{mount_idx}|{venue}|{symbol}|{tag}` (multi-mount correctness, bug A). The mount index
    /// leads because tags are strategy-LOCAL names, not global ones: `vike-mm`'s maker emits the
    /// literal tags `"bid"`/`"ask"`, so two makers on ONE (venue, symbol) both wrote
    /// `binance|BTCUSDT|bid` — mount B's submit OVERWROTE mount A's coid, and A's next
    /// `cancel_tagged("bid")`/`modify_tagged("bid")` then operated on B's RESTING ORDER (a real
    /// cross-mount money bug, not a display one). The index is unique per slot by construction and
    /// cheaper than the string mount id. Single-mount runs are unaffected in behavior (every key
    /// simply gains the constant `0|` prefix; the map is runtime-only and never persisted or
    /// published, so nothing on the wire or in the journal changes).
    strategy_tags: indexmap::IndexMap<String, String>,
    /// Phase D: every strategy mount (config.strategy + extra_mounts); Option per slot
    /// for the take/replace dance around strategy calls
    mounts: Vec<Option<StrategyMount>>,
    /// Portfolio-observer PR-4 T2: each mount's deterministic [`crate::strategy_state::mount_id_of`],
    /// same length and index alignment as `mounts`, computed once in `assemble_core`. Read by the
    /// `run()` teardown's save-on-stop (gated on [`CoreConfig::state_dir`]) so it never recomputes
    /// the id from a possibly-taken (`None`) mount slot.
    mount_ids: Vec<String>,
    /// Portfolio-observer PR-4 T5: each mount's [`MountState`], same length and index alignment
    /// as `mounts` (computed once in `assemble_core`, never resized after). All `Ready` forever
    /// when [`CoreConfig::readiness_gate`] is off (the default); seeded `Pending` when it is on,
    /// flipped to `Ready` one-way by [`Self::maintain_mount_readiness`]. Read by
    /// [`Self::drain_broker`] (the discard gate) and [`Self::mount_views`] (the snapshot view).
    mount_states: Vec<MountState>,
    /// Phase C/D: emulated conditional orders per (venue, symbol) — checked per closed
    /// bar BEFORE `on_bar` (oracle firing order), optionally per tick.
    conditional_books: indexmap::IndexMap<(String, String), ConditionalBook>,
    /// Live-runtime OTO/OCO: the ONE shared [`vike_exec::ContingencyBook`] resolver, keyed by coid
    /// (globally unique per core). Records a leg's linkage the instant a linked/bracket order is
    /// submitted; on a leg's FILL it arms held OTO children (released via `held_orders`) and returns
    /// the OCO siblings to cancel. EMPTY for every non-bracket run — plain orders are never inserted,
    /// so the whole feature is a single `is_empty()` check away on the byte-identical path.
    contingency: ContingencyBook,
    /// The resolved requests of OTO children the runtime is HOLDING back from the venue until their
    /// parent fills (live-runtime OCO/OTO), keyed by coid. A held exit lives ONLY here — it is NOT
    /// in any engine registry and NOT at the venue — until its parent's fill arms it, at which point
    /// it is drained here and submitted (`release_held_child`). Also the durable half a `Snap`
    /// captures (`SnapContingency.held_request`). `IndexMap`: release/drop order is the map's own
    /// insertion order, deterministic. Empty on every non-bracket run.
    held_orders: indexmap::IndexMap<String, OrderRequest>,
    /// Monotone counter behind [`Self::mint_arm_id`] — the emulated conditionals' id sequence
    /// (emulator-journal PR-1). Deliberately SEPARATE from the `ClientOrderIdGenerator`: an arm
    /// is not an order, and drawing arm ids from the coid generator would shift every subsequent
    /// order's coid the moment a conditional is armed.
    ///
    /// Seeded from [`CoreConfig::arm_seq`] — `0` for a fresh session, and on a restart that
    /// resumes `coid_session` the value [`crate::replay::RestoredState::arm_seq`] carries, so the
    /// `{coid_session}a{arm_seq}` ids minted after a restart cannot collide with the ones the
    /// pre-restart run already wrote into the SAME (appended-to) journal. Stamped into every
    /// `Snap` next to `coid_seq` (emulator PR-2) so that resume value survives segment pruning.
    /// Since emulator PR-3 the arms themselves survive a restart too: the books ride the same
    /// `Snap` (`Snap.conditionals`) and re-seed via [`CoreConfig::conditionals`], so the resumed
    /// counter and the re-armed ids come from the ONE restore base together.
    arm_seq: u64,
    /// Monotone counter behind [`CoreThread::mint_refusal_id`] — the id sequence for an intent the
    /// drain REFUSED before it could become an order (today: a declared-multi mount naming an
    /// UNDECLARED symbol, see `CoreThread::resolve_intent_symbol`).
    ///
    /// SEPARATE from the [`vike_exec::ClientOrderIdGenerator`] for the same reason [`Self::arm_seq`]
    /// is, only more sharply: a refusal is NOT an order — it journals nothing and submits nothing —
    /// so drawing its id from the coid generator would burn a client-order-id that never appears in
    /// the journal. Every later order's coid would then sit one step ahead of the `coid_seq` the
    /// `Snap` records, and a restart that resumes from that journal would re-mint an id the live
    /// session had already spent: a misconfigured session would become un-restorable.
    ///
    /// Not stamped into a `Snap` and not resumed (unlike `arm_seq`): a refusal id is never a lookup
    /// key for anything durable — no journal record carries it, no disarm targets it — so a restart
    /// restarting the sequence at 0 can collide with nothing. It is purely a handle for the
    /// operator/strategy-visible denial the refusal emits.
    refusal_seq: u64,
    /// Cross-venue: additional (seed_cash, engine) pairs, one per extra venue. Empty
    /// (spawn_core default) = single-venue behavior, byte-identical. Index 0 in routing
    /// terms is the primary `engine`; extra i is routing index i+1.
    extra_engines: Vec<(f64, ExecutionEngine<C>)>,
    /// **Do two engines in this process share one exchange?** — i.e. does this process mount more
    /// than one ACCOUNT of some venue. Computed once at construction (`assemble_core`) from the
    /// engines' own `venue` fields; `false` on every process with one account per venue, which is
    /// every process with no `policy.toml` `[accounts]` table.
    ///
    /// It exists for exactly one branch, in [`Self::route_event`], and the whole reason it is a
    /// precomputed `bool` rather than a scan is that the branch sits on the fold: a `false` costs
    /// one predictable compare per venue-tagged event and reaches the identical `return` the
    /// routing has always taken, so the single-account hot path is byte-identical.
    multi_account: bool,
    /// coid → routing index, written at submit time — order-LIFECYCLE events carry no
    /// venue field (wire schema), so replies route back through this map (miss = primary,
    /// today's behavior).
    coid_venue: std::collections::HashMap<String, usize>,
    /// **arm id → routing index, written when the conditional is ARMED** — [`Self::coid_venue`]'s
    /// twin for the one order the core mints with no request behind it.
    ///
    /// `conditional_books` is keyed by `(venue, symbol)`, which is a fact about the EXCHANGE, so
    /// two accounts of one venue arm into ONE book and a fired arm carries no account. The FIRE
    /// (`CoreThread::submit_fired`) used to lower through `apply_intent`, i.e. by the payload's
    /// venue, which resolves the venue's DEFAULT engine: a labelled mount's `Broker::submit_stop`
    /// armed against its own book and, on trigger, sold into somebody else's — the protective exit
    /// silently opening a naked position on the default account while the position it was armed to
    /// close stayed open. The arm's engine is known at ARM time (`apply_intent_routed`'s
    /// `ArmConditional` arm already resolves it to read the seeding mark), so it is recorded here
    /// and spent at the fire.
    ///
    /// Bounded by the book: an entry is written when an arm enters a `ConditionalBook` and removed
    /// when it leaves one (fired, disarmed, or cleared by a mass-cancel).
    ///
    /// Byte-identical on every single-account core, and NOT because it is skipped there: the
    /// recorded index is `route_of(route, venue)`, which on a core with one engine per venue IS
    /// what the fire's payload lookup resolved.
    cond_engine: std::collections::HashMap<String, usize>,
    /// Opt-in write-ahead journal (spec §A). `None` = journaling off (default) — the whole
    /// hot-path hook is skipped and the fold is byte-identical to today. Opened once at
    /// construction from [`CoreConfig::journal`].
    journal: Option<crate::journal::CommandJournal>,
    /// Records appended since the last `Snap` — drives the [`JournalConfig::snapshot_every`]
    /// cadence. Reset to 0 by [`CoreThread::write_snap`].
    journaled_since_snap: u64,
    /// Whether an `Ingest::Watchdog` waker record is written to the journal — `true` iff a
    /// wall-clock sweep that MUTATES state is enabled (stuck-order watchdog / dead-man's switch /
    /// GTD expiry sweep). See the construction site in `assemble_core` for the full rationale; the
    /// short version is that the record exists to make such a session refuse to replay, and a
    /// core running only replay-neutral boundary observers must not inherit that refusal.
    journal_waker_records: bool,
    /// Opt-in mmap counter mirror (audit co9). `None` (default) = disabled — [`Self::publish`]
    /// skips the mirror and the publish path is byte-identical. `Some` is opened once at
    /// construction from [`CoreConfig::counters_path`]; each publish copies the current counters in.
    /// Written ONLY at publish cadence, never the per-message fold — the `p99 < 10µs` gate is
    /// untouched.
    counters: Option<crate::counters::CountersFile>,
    /// Stuck-order watchdog (audit C3) confirm-grace bookkeeping, hardened into the in-flight-confirm
    /// guard: `coid → the now_ms at which stage 1 ISSUED its active confirm for that order`. Two jobs:
    /// (a) stage 1 warns + confirms each coid exactly ONCE per stuck episode (present ⇒ already
    /// confirmed, skip); (b) stage 2's guard — a reject candidate WITH an entry has a confirm in
    /// flight, so the last-resort reject is DEFERRED to `created + submit_ack_timeout +
    /// 2·submit_ack_confirm_grace` (one extra grace window) to let a slow confirm (Bybit's
    /// realtime→history re-query can exceed a single grace) land before terminalizing. Whenever a coid
    /// is here the order is STILL pre-ack (any authoritative fold moves it out), so an entry means
    /// "confirm issued, nothing authoritative folded since". Core-local, NOT serialized (the watchdog
    /// path is out of replay scope); self-cleaning — pruned each sweep to only still-pre-ack coids, so
    /// it stays bounded. Empty unless the watchdog is enabled.
    confirm_issued_ms: std::collections::HashMap<String, i64>,
    /// Drawdown latch (audit exec#4) high-water-mark, on the DAEMON'S OWN equity curve
    /// (`Σ seed_of(e) + Σ ExecutionEngine::resolved_own_pnl(e)` — never an account balance level;
    /// see [`CoreThread::sweep_drawdown_latch`] for why the wallet had to leave this number).
    /// Folded on the per-CLOSED-bar sweep, only when [`CoreConfig::max_drawdown`] is enabled.
    /// Core-local, NOT serialized (never journaled/snapshotted); `None` until the first sweep (or
    /// forever, when the latch is disabled). RESTART: the LATCH itself
    /// (`trading_state = Reducing`) IS restored via the engine snapshot, and the CURVE resumes
    /// underwater because `Account`'s PnL terms are in `AccountSnapshot` — so this HWM re-seeds to
    /// `max(capital_base, curve)`, which forgives no pre-restart loss below configured capital.
    pnl_curve_peak: Option<f64>,
    /// One-shot guard for the drawdown latch's DISARMED warning — see
    /// [`CoreThread::sweep_drawdown_latch`]. The condition (a non-positive capital base under an
    /// armed `max_drawdown`) holds on every subsequent sweep, so without this the per-bar ring
    /// would carry nothing else. Core-local, NOT serialized, like the HWM above.
    pnl_curve_disarmed_noted: bool,
    /// Deadline timer wheel (audit co6), advanced ONCE per drain-loop boundary — NEVER per message,
    /// so the p99<10µs fold is untouched. Empty by default (a single `is_empty` gate keeps the
    /// wheel-free path byte-identical); armed with a self-rescheduling [`TimerKind::StuckSweep`] only
    /// when the watchdog (`submit_ack_timeout`) is enabled. See [`CoreThread::drive_due_timers`].
    timers: DeadlineTimerWheel<TimerKind>,
    /// Reusable expiry buffer for [`Self::drive_due_timers`] — cleared and refilled each boundary so
    /// the timer path allocates nothing on the steady state.
    due_timers: Vec<TimerKind>,
    /// The stuck-order sweep cadence (ms) the [`TimerKind::StuckSweep`] timer re-arms itself at —
    /// `submit_ack_timeout / 2` clamped to `>= 50ms`, mirroring the OS-thread waker's tick. 0 when
    /// the watchdog is disabled.
    watchdog_tick_ms: i64,
    /// The armed [`TimerKind::EquitySample`] timer (portfolio-observer PR-3), `None` while flat
    /// (or whenever [`CoreConfig::equity_sample`] is disabled — never armed in that case at
    /// all). Set by [`CoreThread::maintain_equity_timer`]'s arm branch and the re-arm in
    /// [`CoreThread::drive_due_timers`]; cleared by its disarm branch and by a fire that finds
    /// the book gone flat.
    equity_timer: Option<TimerId>,
    /// Reusable batch buffer for [`Self::sample_equity`] — cleared and refilled each fire (one
    /// row per engine plus the `"TOTAL"` row) so steady-state sampling allocates no `Vec` of its
    /// own, mirroring [`Self::due_timers`].
    equity_rows: Vec<EquitySample>,
    /// The armed [`TimerKind::StateSave`] timer (portfolio-observer PR-4 T3), `None` whenever
    /// [`CoreConfig::state_save`] or [`CoreConfig::state_dir`] is disabled, or (transiently)
    /// while no mount exists — never armed at all in the first two cases. Set by
    /// [`CoreThread::maintain_state_save_timer`]'s arm branch and the re-arm in
    /// [`CoreThread::drive_due_timers`]; cleared by its disarm branch and by a fire that finds
    /// every mount gone.
    state_save_timer: Option<TimerId>,
    /// Task 17: held Quarantine/Hybrid-quarantined reconcile alerts awaiting an operator
    /// `Command::ConfirmRecon`, keyed by the monotonic id `reconcile_reports` assigns when the
    /// alert is first surfaced. Fold-thread-only state (single-writer invariant — never mutated
    /// off this thread); the GUI-facing projection is `CoreSnapshot.recon.alerts`
    /// (`ReconAlertView`, built by `Self::recon_block`), which carries kind/detail/count but
    /// never the raw `Event`s. NOT journaled: ids reset with the process (see
    /// `vike_exec::Command::ConfirmRecon`'s doc for why a replayed confirm against a stale id is
    /// a harmless no-op, not a correctness gap).
    recon_alerts: indexmap::IndexMap<u64, HeldReconAlert>,
    /// Which held divergences have already been ANNOUNCED to the log, per venue — the state that
    /// turns a per-pass repetition into a per-transition line. Purely a logging concern: it never
    /// decides what folds, what holds or what `recon_alerts` stores, and it is touched ONLY from
    /// `reconcile_reports` (interval cadence) and `confirm_recon` (an operator command), never the
    /// per-message fold. Empty and allocation-free on a box with no held divergences. See
    /// `crate::runtime::recon_held`.
    recon_announce: recon_held::HeldAnnouncer,
    /// Next id `reconcile_reports` assigns to a newly-held alert. Monotonic within this process
    /// run only (see `recon_alerts` doc).
    recon_next_alert_id: u64,
    /// wall-clock ms of the most recently completed reconcile pass (`engine.now_ms` at the time
    /// `reconcile_reports` ran); 0 before the first pass. Surfaced verbatim as
    /// `CoreSnapshot.recon.last_pass_ts`.
    recon_last_pass_ts: i64,
    /// Latest venue-REPORTED per-position coin delta from each reconcile pass, keyed `(venue,
    /// symbol)` (Wave 5d). Upserted from every `PositionStatusReport` that carries a `delta`
    /// (Deribit `get_positions.delta` today); empty and inert for every other venue. Cloned into
    /// `CoreSnapshot::recon_coin_deltas` at the coalesced publish so the greeks tool can fold a
    /// Deribit perp/future leg via `coin_delta × spot` — `PositionView` is fills-derived and has
    /// no venue delta of its own.
    recon_coin_deltas: indexmap::IndexMap<(String, String), f64>,
    /// Dead-man's-switch (auto cancel-on-disconnect) pure trip-logic state machine — `Some` ONLY
    /// when [`CoreConfig::deadman`] is set, else `None` (default) and the whole feature is inert.
    /// [`Self::dispatch`] feeds it the freshest data/event ts ([`DeadMan::observe`], the ONE cheap
    /// per-message store, gated on `is_some`); [`Self::sweep_deadman`] evaluates it at the drain-loop
    /// boundary on the [`TimerKind::DeadManSweep`] cadence — NEVER the per-message fold.
    deadman: Option<DeadMan>,
    /// The dead-man sweep cadence (ms) the [`TimerKind::DeadManSweep`] timer re-arms itself at —
    /// `deadman.timeout / 2` clamped `>= 50ms`, mirroring [`Self::watchdog_tick_ms`]. 0 when the
    /// switch is disabled (never armed).
    deadman_tick_ms: i64,
    /// The MANAGED-GTD sweep cadence (ms) the [`TimerKind::GtdSweep`] timer re-arms itself at —
    /// [`CoreConfig::gtd_sweep`], clamped `>= 1ms`. 0 when the feature is disabled (never armed).
    gtd_tick_ms: i64,
    /// Coids whose GTD expiry already triggered a cancel this episode (core-ergonomics) — the
    /// FIRE-ONCE guard for [`Self::sweep_gtd_expiry`], so a venue slow to report the terminal is
    /// not spammed with duplicate cancels. Core-local, NOT serialized (like `confirm_issued_ms`);
    /// self-cleaning — pruned each sweep to coids still live in some registry, so it stays bounded.
    /// Empty forever unless [`CoreConfig::gtd_sweep`] is enabled.
    gtd_canceled: std::collections::HashSet<String>,
    /// The periodic-portfolio-snapshot cadence (ms) the [`TimerKind::PortfolioSnap`] timer re-arms
    /// itself at — [`CoreConfig::portfolio_snapshot_interval`], clamped `>= 1ms`. 0 when disabled.
    portfolio_snap_tick_ms: i64,
    /// The FAST in-flight confirm cadence (ms) the [`TimerKind::InflightConfirm`] timer re-arms
    /// itself at — [`CoreConfig::inflight_confirm`], clamped `>= 1ms`. 0 when the feature is
    /// disabled (never armed). See [`Self::sweep_inflight_confirms`].
    inflight_confirm_tick_ms: i64,
    /// FAST in-flight confirm dedup (recon path-to-superset, F1-A): `coid -> the now_ms at which
    /// this rung last issued a reject-free `confirm_order` re-query for that order`. Deliberately
    /// SEPARATE from the reject ladder's `confirm_issued_ms` so the early (reject-free) rung and the
    /// late (reject-capable) rung never share bookkeeping — the band guard keeps them on disjoint
    /// age windows, and each owns its own map. A still-stuck order is re-confirmed at most once per
    /// [`CoreConfig::inflight_confirm`] window (present + gap-not-elapsed ⇒ skip). Core-local, NOT
    /// serialized (like `confirm_issued_ms`); self-cleaning — pruned each sweep to only still-pre-ack
    /// coids, so it stays bounded. Empty forever unless the feature is enabled.
    inflight_confirm_last_ms: std::collections::HashMap<String, i64>,
    /// Per-mount fill ATTRIBUTION ledger (steal/core-per-mount-budget), parallel to `mounts` (same
    /// index), folded from attributed fills off the hot fold — see [`MountAttribution`]. All-zero
    /// for a mount that has never filled.
    mount_attr: Vec<MountAttribution>,
    /// coid -> mount index, written at the ONE strategy-order submit site
    /// ([`Self::apply_strategy_intent`]) so a mount's fills AND resting orders are attributable back
    /// to it — cross-venue safe (coids are globally unique per core). Read by the fill-attribution
    /// fold ([`Self::dispatch_applied_fills`]) and the budget latch's scoped cancel
    /// ([`Self::latch_mount`]). NOT serialized (attribution is display + budget-local, never fenced
    /// state). Empty when no strategy is mounted; BOUNDED by [`Self::coid_terminal`], which retires
    /// an entry once its order has been terminal for [`COID_PRUNE_LINGER_MS`].
    coid_mount: std::collections::HashMap<String, usize>,
    /// The PRUNE QUEUE that bounds [`Self::coid_mount`]: `(coid, the ms at which its order was first
    /// observed TERMINAL)` in observation order, drained from the FRONT by
    /// [`Self::prune_terminal_coids`] once an entry has aged past [`COID_PRUNE_LINGER_MS`].
    ///
    /// `coid_mount` is written once per mount-minted order and used to be erased NEVER, so a
    /// long-lived maker session grew it without bound — and a restart made it WORSE, because
    /// [`crate::replay`]'s `fold_coid_mounts` rebuilds order provenance across the WHOLE readable
    /// record set rather than just the tail. It is a pure routing/attribution index, so a long-dead
    /// order's entry is dead weight.
    ///
    /// **Why a LINGER queue and not an immediate remove** — see [`Self::note_terminal_coid`].
    /// Core-local, never serialized, and empty on a core whose orders never terminalize.
    coid_terminal: VecDeque<(String, i64)>,
    /// Per-mount resolved [`MountBudget`] (steal/core-per-mount-budget), parallel to `mounts`, copied
    /// from [`CoreConfig::mount_budgets`] at assemble. `None` for a mount with no budget.
    mount_budget: Vec<Option<MountBudget>>,
    /// Per-mount `(venue, symbol)` (steal/core-per-mount-budget), parallel to `mounts`, captured at
    /// assemble so the budget sweep + the snapshot view price a mount's ledger without depending on a
    /// transiently-taken (`None`) mount slot.
    mount_vs: Vec<(String, String)>,
    /// **Per-mount ENGINE INDEX**, parallel to `mounts` — the mount's declared account resolved to
    /// one of this core's engines, ONCE, at assemble ([`mount_engine_idx`]).
    ///
    /// It is the ONE answer to "which book does this mount trade", and every strategy lane reads it
    /// through [`Self::engine_of_mount_venue`] rather than re-deriving one from a venue string.
    /// That is the structural half of the account seam: a venue-keyed lookup can only ever resolve
    /// a venue's DEFAULT account, so as long as the lanes asked that question a second account was
    /// unaddressable no matter what a mount declared, and no runtime test could have said so.
    ///
    /// Identical to the old `engine_idx_for_route_key(sole_account_of(mount.venue)).unwrap_or(0)`
    /// for every account-less mount, which is every mount that existed before the field.
    mount_engine: Vec<usize>,
    /// Per-mount budget LATCH (steal/core-per-mount-budget), parallel to `mounts`: `true` once that
    /// mount breached its budget and was latched liquidate-only. Fire-once — a re-breach on a
    /// still-open residual never re-cancels/re-flattens; read by [`Self::drain_broker`] (discards a
    /// latched mount's intents, the liquidate-only enforcement) and [`Self::sweep_mount_budgets`].
    /// Core-local, NOT serialized (like `equity_peak`). All `false` when no budget is set — the
    /// byte-identical path.
    mount_latched: Vec<bool>,
    /// Per-mount DECLARED extra symbols (`StrategyMount::symbols`), parallel to `mounts`. Empty
    /// for every single-symbol mount, which is what makes the drain byte-identical.
    mount_symbols: Vec<Vec<MountLeg>>,
    /// Whether ANY mount declared extra symbols. `false` (default) ⇒ the drain's symbol
    /// resolution is one bool read and the per-symbol read tables are never built.
    any_mount_multi: bool,
    /// Whether ANY mount declared a leg on a venue OTHER than its own (the xEMM shape). `false`
    /// (default, and true of every same-venue declared mount too) ⇒ the two per-market-message
    /// reference-quote call sites in the `Ingest::Quote`/`Ingest::Book` arms are ONE bool load and
    /// [`Self::drive_strategy_reference_quote`] never runs — the byte-identical gate, mirroring
    /// `any_mount_multi`/`any_mount_budget`.
    any_mount_ref: bool,
    /// Whether ANY mount has an ACTIVE budget (steal/core-per-mount-budget). `false` (default) makes
    /// [`Self::drive_strategy`] skip the per-closed-bar budget sweep with a single bool read — the
    /// byte-identical gate, mirroring `config.max_drawdown`'s `Option` gate for the drawdown latch.
    any_mount_budget: bool,
    /// Per-mount wall-clock [`crate::schedule::LiveSchedule`] (steal/core-live-scheduler), parallel
    /// to `mounts` (same indices), resolved from [`CoreConfig::mount_schedules`] at assemble. Empty
    /// for a mount with no schedule. Checked at EVERY drain-loop boundary pass
    /// ([`Self::drive_schedule`], the readiness-gate pattern — no timer-wheel entry), NEVER per
    /// message.
    mount_schedule: Vec<LiveSchedule>,
    /// Whether ANY mount has a non-empty schedule (steal/core-live-scheduler). `false` (default) ⇒
    /// the boundary check is a single bool read, no clock read happens, no waker cadence is added,
    /// and the boundary is byte-identical — the gate mirrors `any_mount_budget`.
    any_mount_schedule: bool,
}

/// Fold-thread-only record of one held reconcile alert (Task 17): the full proposed events
/// (folded verbatim on confirm) plus the metadata `Self::recon_block` needs to build its
/// `ReconAlertView` projection, and the `route_key` needed to re-resolve the routing engine index
/// at confirm time (indices are NOT stored directly — cheap to recompute via
/// `engine_idx_for_route_key`, and robust if that ever became less than perfectly stable).
///
/// ⚠ **`venue` and `route_key` are stored SEPARATELY, and this record is why.** An operator confirm
/// arrives minutes to hours after the pass that raised the alert, and `Self::confirm_recon` folds
/// the held `proposed_events` into whichever engine this record resolves — so a single string here
/// would be a ROUTING decision made at pass time, kept, and re-applied later under the name of a
/// canonical venue. Everything else about the row is a per-VENUE fact (the dedup identity below,
/// which pairs with `kind` + `dedup_key`; the `ReconAlertView` label an operator reads), so
/// collapsing the two would force one of them to be wrong for a second account of one exchange:
/// route by `venue` and the approved events fold into the FIRST account's book, or label by
/// `route_key` and the dedup identity + the operator-facing venue become a string
/// `vike_model::VENUES` does not contain.
struct HeldReconAlert {
    /// The CANONICAL exchange id (`vike_model::VENUES`) — the operator-facing label and, with
    /// `kind` + `dedup_key`, this row's dedup identity. NEVER the routing input.
    venue: String,
    /// The ENGINE this alert's `proposed_events` fold into on confirm — carried verbatim from the
    /// pass that raised it (`ReconcileReports::route`), so the confirm lands where the pass would
    /// have. Equal to `venue` for every alert this tree can raise.
    route_key: String,
    kind: DivergenceKind,
    detail: String,
    proposed_events: Vec<Event>,
    /// Order-loss recovery payload (recon `JournalDivergence`): venue orders to RE-REGISTER into
    /// local state on confirm, via `ExecutionEngine::reregister_orders` (insert-only). Empty for
    /// every other alert.
    recover_orders: Vec<vike_model::OrderStatusReport>,
    /// This row's DEDUP IDENTITY — a later pass re-raising the same divergence REFRESHES this row
    /// in place (same confirm id, freshened payload) instead of appending a new one.
    ///
    /// ⚠ This used to be `vike_exec::recon::ReconAlert::dedup_key`, an `Option` that only SOME
    /// kinds set, and an alert without one appended a row EVERY PASS with a fresh id. Nothing caps,
    /// prunes or retains this store — `shift_remove` fires only on an operator confirm — so an
    /// un-keyed divergence that nothing heals grew it without bound: measured on the CI box, alert id
    /// 337 at 00:00 → 815 at 06:59 on 2026-08-25, ~65 rows/hour ≈ 1,560/day, every one of them the
    /// same two divergences, each holding its own `proposed_events` and each projected into every
    /// published `CoreSnapshot` by `Self::recon_block`. `recon_held::HeldId` is the stable name
    /// those alerts never carried; where a `dedup_key` DOES exist it still decides alone, so this
    /// is a strict generalization of the old match rather than a different rule.
    identity: recon_held::HeldId,
}

impl<C: ExecutionClient> CoreThread<C> {
    fn run(mut self, mut rx: mpsc::Receiver<Ingest>) {
        // Arm the boundary timer wheel (audit co6) BEFORE the loop, off the hot path. Gated on the
        // watchdog being enabled so that when it is OFF (default) NO clock read happens here — the
        // wheel stays empty, the boundary advance is skipped by a single `is_empty` check, and the
        // fold path (and, critically, the per-message clock-read COUNT that replay/harness clocks
        // depend on for deterministic `now_ms`) is byte-identical to a wheel-free runtime.
        if self.config.submit_ack_timeout.is_some()
            || self.config.deadman.is_some()
            || self.config.gtd_sweep.is_some()
            || self.config.portfolio_snapshot_interval.is_some()
            || self.config.inflight_confirm.is_some()
        {
            self.arm_boundary_timers(self.config.clock.now_ms());
        }
        let mut last_pub =
            Instant::now().checked_sub(self.config.snapshot_interval).unwrap_or_else(Instant::now); // publish ASAP
        'outer: loop {
            // Never block with dirty state unpublished — this also covers the batch-max
            // boundary case (a burst of exactly batch_max messages then silence would
            // otherwise strand the final state until the next message).
            let first = match rx.try_recv() {
                Ok(msg) => msg,
                Err(mpsc::error::TryRecvError::Empty) => {
                    // leftover on_fill / on_order_event deliveries beyond the per-message round cap
                    // must not strand on a quiet feed — drain (guarded, like handle()) before parking
                    if !self.engine.applied_fills.is_empty()
                        || !self.engine.order_events.is_empty()
                        || self
                            .extra_engines
                            .iter()
                            .any(|(_, e)| !e.applied_fills.is_empty() || !e.order_events.is_empty())
                    {
                        if let Err(payload) =
                            catch_unwind(AssertUnwindSafe(|| self.dispatch_applied_fills()))
                        {
                            self.enter_safe_state(panic_text(payload));
                            self.drain_poisoned(); // audit C4 (safeguard)
                        }
                        self.drain_delivered();
                    }
                    if self.dirty {
                        self.publish_guarded();
                        last_pub = Instant::now();
                    }
                    match rx.blocking_recv() {
                        Some(msg) => msg,
                        None => break, // every sender dropped -> clean exit
                    }
                }
                Err(mpsc::error::TryRecvError::Disconnected) => break,
            };
            if self.handle(first).is_break() {
                break 'outer;
            }
            // opportunistic bounded drain (keeps per-wakeup latency bounded)
            for _ in 1..self.config.batch_max {
                match rx.try_recv() {
                    Ok(msg) => {
                        if self.handle(msg).is_break() {
                            break 'outer;
                        }
                    }
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => break 'outer,
                }
            }
            // EQUITY SAMPLER (portfolio-observer PR-3): arm/disarm the cold equity-sample timer at
            // this SAME drain-loop boundary, before the wheel advances below — so an interval armed
            // this pass (a position that just opened in the batch just folded) is visible to
            // `drive_due_timers` immediately. `equity_sample` gates the whole block to a single
            // `Option::is_some()` read when disabled (the default): nothing is armed, `self.timers`
            // is never touched, and this boundary stays byte-identical to a sampler-free runtime.
            if self.config.equity_sample.is_some() {
                let now = self.config.clock.now_ms();
                self.maintain_equity_timer(now);
            }
            // PERIODIC STATE-SAVE (portfolio-observer PR-4 T3): arm/disarm the cold state-save
            // timer at this SAME drain-loop boundary, mirroring the equity sampler immediately
            // above — a mount that just got added this pass is visible to `drive_due_timers`
            // immediately. `state_save` gates the whole block to a single `Option::is_some()`
            // read when disabled (the default): nothing is armed, `self.timers` is never touched
            // by this feature, and this boundary stays byte-identical to a save-timer-free
            // runtime. (`state_dir` is checked inside `maintain_state_save_timer` itself, not
            // here, so both gates stay independently readable at their own call sites.)
            if self.config.state_save.is_some() {
                let now = self.config.clock.now_ms();
                self.maintain_state_save_timer(now);
            }
            // READINESS GATE (portfolio-observer PR-4 T5): probe still-Pending mounts at this SAME
            // drain-loop boundary — no timer-wheel entry needed (unlike the two blocks above, this
            // has no cadence of its own: it just re-checks the board every boundary pass while
            // anything is still Pending, and `maintain_mount_readiness` itself no-ops in O(1) the
            // moment nothing is). `readiness_gate` gates the whole block to a single `bool` read
            // when off (the default): no clock read, no scan, byte-identical to a gate-free runtime.
            if self.config.readiness_gate {
                let now = self.config.clock.now_ms();
                self.maintain_mount_readiness(now);
            }
            // WALL-CLOCK SCHEDULE (steal/core-live-scheduler): checked at this SAME drain-loop
            // boundary, the readiness-gate pattern — NO timer-wheel entry. Driving off the CLOCK
            // VALUE here (rather than a wheel deadline computed as `now + tick`) is load-bearing:
            // an injected/deterministic clock (replay, tests) may not advance between boundary
            // passes, and a wheel deadline computed from a frozen clock is unreachable until the
            // clock moves — which starves the poll to at most one check per clock movement (and the
            // establish-then-fire latch law consumes the first). The boundary waker (cadence fed by
            // `schedule_poll`) guarantees an idle core reaches this check on cadence; a rule fires
            // at the first boundary pass AT or AFTER its wall-clock instant. `any_mount_schedule`
            // gates the whole block to a single bool read when off (the default): no clock read, no
            // scan, byte-identical to a schedule-free runtime.
            if self.any_mount_schedule {
                let now = self.config.clock.now_ms();
                self.drive_schedule(now);
            }
            // DEADLINE TIMER WHEEL (audit co6): advance ONCE here, at the drain-loop boundary —
            // after the whole batch folded, NEVER per message (the p99<10µs fold above is
            // untouched). The `is_empty` gate makes this a single field read when no timer is armed
            // (the default: watchdog off ⇒ empty wheel), so the wheel-free path stays byte-identical.
            // When armed, this is where the stuck-order sweep (and, since PR-3, the equity
            // sampler, and since PR-4, the periodic strategy-state save) fires — driven by the
            // injected clock at the coalesced boundary, not by the `Ingest::Watchdog` message
            // (which is now only a waker; see its dispatch arm). The OS waker thread guarantees
            // the core wakes at least once per tick, so an idle core still reaches this boundary
            // on cadence.
            if !self.timers.is_empty() {
                let now = self.config.clock.now_ms();
                self.drive_due_timers(now);
            }
            // interval-coalesced publish while busy (idle publish happens above)
            if self.dirty && last_pub.elapsed() >= self.config.snapshot_interval {
                self.publish_guarded();
                last_pub = Instant::now();
            }
        }
        // Symmetric teardown + final snapshot so the GUI sees terminal state. Guarded:
        // a panicking client detach must still leave a published fault, not a dead cell.
        if catch_unwind(AssertUnwindSafe(|| {
            self.drain_market(); // fold any tick stranded behind Shutdown in the queue
            self.dispatch_applied_fills(); // pending on_fill deliveries while the client is attached
            // OPT-IN SHUTDOWN POLICY, and it must run HERE — this is the last instant at which the
            // core still owns live engines AND the client is still attached, so it is the only
            // point a cancel can still reach the venue. Everything after this guard is teardown:
            // phase one below raises every client's stop flag and phase two `shutdown()`s them,
            // and the same sweep fired into a client whose flag is already up would go nowhere.
            // OFF by default ⇒ one bool read, and the teardown stays byte-identical to the
            // version that had no such field.
            if self.config.cancel_orders_on_shutdown {
                self.cancel_resting_on_shutdown();
            }
            // TWO PHASES, and the split is the whole cost model — the exec-plane twin of
            // `crates/vike-recorder/src/runtime.rs`'s `stop_all`, whose doc argues the same finding
            // one plane over.
            //
            // `ExecutionEngine::shutdown` detaches the venue client, and a venue's detach JOINS its
            // user-data pump — a thread that learns it should stop on its next stop-flag poll
            // (`crates/bridges/binance/src/family/listenkey.rs`'s `POLL` is 1s, and the recv loop
            // in `vike_bridge_core::run_user_data_forever_with_idle` re-checks the flag only on that
            // cadence). The one-loop version of this teardown — detach-and-join, one engine at a
            // time — therefore cost ONE wind-down PER ENGINE, serially, on the shutdown path of
            // every shipped binary: a twelve-venue mount paid about twelve of them while a
            // service-managed stop was already counting against its unit's `TimeoutStopSec=`.
            //
            // PHASE 1 — raise every flag, join nothing. This must complete for EVERY engine before
            // the first `shutdown()` below, or the parallelism it exists for is given back one
            // venue at a time. `begin_shutdown` is a default no-op on any client that has not
            // wired it, so this loop is free where it buys nothing.
            self.engine.begin_shutdown();
            for (_, e) in self.extra_engines.iter_mut() {
                e.begin_shutdown();
            }
            // PHASE 2 — the exact detaches that shipped before, now joining threads that have all
            // been winding down since phase one. Nothing else about this teardown moves: no engine
            // state is touched by phase one, the order below is unchanged, and a single-engine
            // mount behaves as it always did.
            //
            // ⚠ It does NOT race the venue events phase one stops sooner. The fold loop has already
            // broken by the time we get here, and nothing in this teardown drains the venue ingest
            // lane — `drain_market` above drains MARKET data and `pump_client` below polls
            // IN-PROCESS clients only. A venue event arriving during the old code's serial join
            // window was folded by nobody either.
            self.engine.shutdown();
            for (_, e) in self.extra_engines.iter_mut() {
                e.shutdown();
            }
            self.pump_client();
            self.dispatch_applied_fills(); // fills the final pump surfaced
            self.drain_delivered();
        }))
        .is_err()
        {
            self.engine.trading_state = TradingState::Halted;
            if self.fault.is_none() {
                self.fault = Some("panic during teardown".to_string());
            }
            self.drain_poisoned(); // audit C4 (safeguard)
        }
        // Final Snap on exit — Shutdown, senders-dropped, and disconnect all funnel here. This
        // always-present tail Snap is what a clean restart restores from (replay is latest-Snap-
        // wins). `engine.shutdown()` above only detaches the client (OMS state is unchanged), so
        // this captures the fully-folded terminal state. No-op when journaling is off, so the
        // is_some guard keeps the teardown byte-identical to today when disabled.
        if self.journal.is_some() {
            let now = self.config.clock.now_ms();
            self.write_snap(now);
        }
        // Portfolio-observer PR-4 T2/T3: best-effort save of each live mount's durable-state
        // sidecar on a clean shutdown — the SAME per-mount loop the periodic
        // [`TimerKind::StateSave`] fire uses (T3), factored into `save_all_strategy_state` so the
        // two call sites can't drift apart (DRY; see that method's doc for the full guard
        // rationale — catch_unwind per mount, indexed so a panicked mount can't shift
        // `mount_ids` alignment, write failures logged and swallowed). `None` `state_dir`
        // (default) skips this entirely — byte-identical teardown to today.
        if self.config.state_dir.is_some() {
            self.save_all_strategy_state();
        }
        self.publish_guarded();
    }

    /// Dispatch one message under the panic policy. Hooks run inside the guard too — no
    /// user-supplied code on this thread can unwind past run() unsurfaced.
    fn handle(&mut self, msg: Ingest) -> std::ops::ControlFlow<()> {
        if let Ingest::Command(Command::Shutdown) = msg {
            return std::ops::ControlFlow::Break(()); // teardown runs in run()
        }
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            if let Some(hook) = self.config.on_dequeued.as_mut() {
                hook(&msg);
            }
            self.dispatch(msg);
            // deliver any fills this message's fold accepted (venue lane, client pumps)
            self.dispatch_applied_fills();
        }));
        match outcome {
            Ok(()) => {}
            Err(payload) => {
                self.enter_safe_state(panic_text(payload));
                self.drain_poisoned(); // audit C4: don't lose an event the panic interrupted mid-fold
            }
        }
        self.drain_delivered();
        std::ops::ControlFlow::Continue(())
    }

    /// Consumes the message — the dominant Event path moves straight into the bus with no
    /// hot-path clone (the same rationale that keeps `Ingest::Event` unboxed).
    fn dispatch(&mut self, msg: Ingest) {
        // one wall-clock stamp per message: submit_order's now_ms AND every persistence
        // updated_ts in this dispatch (Python called its now_ms lambda at each use site;
        // one read per message is the same clock, minus mid-dispatch drift)
        self.engine.now_ms = self.config.clock.now_ms();
        let now_all = self.engine.now_ms;
        for (_, e) in self.extra_engines.iter_mut() {
            e.now_ms = now_all;
        }
        // DEAD-MAN'S SWITCH freshness observe (trading-hardening): the ONE cheap per-message store —
        // record `now_all` as the freshest data/event ingest ts, so the boundary sweep can tell how
        // long the feed has been silent. Gated on the switch being armed (`Option::is_some`-cheap
        // via `if let`), so when disabled (default) this is a single null-check branch, no store, no
        // log, no allocation — the `p99 < 10µs` fold stays byte-identical. Only genuine liveness
        // messages advance it: venue events + market data (quote/trade/book/mark/closed-bar), NEVER
        // control (Command), the periodic waker (Watchdog), a health-status change (StreamStatus), or
        // a historical bar SEED (BarSeed).
        if let Some(dm) = self.deadman.as_mut()
            && matches!(
                msg,
                Ingest::Event(_)
                    | Ingest::Market
                    | Ingest::BarClose(_)
                    | Ingest::Quote(_)
                    | Ingest::Trade(_)
                    | Ingest::Book(_)
            )
        {
            dm.observe(now_all);
        }
        // WRITE-AHEAD JOURNAL (spec §A): journal the exec-lane message HERE — after the wall-clock
        // stamp, BEFORE the `match msg` fold below — so a crash mid-fold still finds the message on
        // disk and replays it. `append_cmd` serializes `&msg` by BORROW (no clone on the p99<10µs
        // hot path; the `match msg` below still owns and consumes it); the returned seq is unused.
        // Only exec-lane verbs are journaled — market/bar/tick lanes are not command state.
        // `Command::Shutdown` never reaches here (intercepted in `handle`); it gets a Snap, not a
        // Cmd. The whole hook is skipped (one `Option::is_some`) when journaling is off.
        // The cadence SNAPSHOT decision is deferred to AFTER the fold (tail of this fn) so a snap
        // reflects THIS message's effect; only the append + counter bump (the durability write-
        // ahead) run here, before the `match msg` fold. `journaled` is captured now (it gates the
        // after-fold snap to the same exec-lane messages) because `match msg` below consumes `msg`.
        let journaled = self.journal.is_some()
            && match msg {
                // The hot Event arm stays FIRST (one discriminant test, exactly as before).
                Ingest::Event(_) => true,
                // Runtime mount/unmount (split-plane B5) is deliberately NOT journaled: it is
                // session TOPOLOGY, not order state. The mount-less replay core has no
                // `strategy_factory` and could never re-resolve a strategy, so a journaled mount
                // would replay as a refusal note — a divergence, not a restore. Restart survival
                // is instead DAEMON-LEVEL STATE (documented on `Command::MountStrategy` too): the
                // mount arm records the spec in the `crate::mount_topology` sidecar (gated on
                // `state_dir`, atomic rewrite, unmount removes it) and the composition root
                // replays that file at startup through this same command path — so the journal's
                // replay determinism fence stays over exactly what re-folds, while the journal's
                // own `StrategySubmit` records still carry the mount id for attribution. The
                // unmount's cancels are replay-neutral exactly like `latch_mount`'s: the venue's
                // authoritative `OrderCanceled` is journaled as its own `Ingest::Event` and
                // replays independently.
                Ingest::Command(Command::MountStrategy(_) | Command::UnmountStrategy { .. }) => {
                    false
                }
                Ingest::Command(_) => true,
                // Pure waker: journaled only when a state-mutating wall-clock sweep can ride it
                // (see `journal_waker_records`). One bool read; nothing else on this path moved.
                Ingest::Watchdog => self.journal_waker_records,
                _ => false,
            };
        if journaled {
            self.journal.as_mut().unwrap().append_cmd(now_all, &msg).expect("journal append");
            self.journaled_since_snap += 1;
        }
        match msg {
            Ingest::Event(ev) => {
                if let (Some(tid), Event::Fill(f)) = (self.config.panic_on_trade_id.as_deref(), &ev)
                    && f.trade_id == tid
                {
                    panic!("injected fault: trade_id {tid}");
                }
                // Live-runtime OTO/OCO: capture this REAL-venue event's effect on the contingency
                // book BEFORE `ev` is consumed by the bus (the shared `contingency_terminal`
                // classifier `pump_client` also uses), then drive it AFTER the engine has folded the
                // event. A terminal `OrderFilled` ARMS this leg's held OTO children + cancels its OCO
                // siblings (matching the paper oracle's `apply_contingency`); a terminal-WITHOUT-fill
                // (`Canceled`/`Rejected`/`Expired`) cascade-drops the children that can now never arm
                // (the oracle's `expire_children_of`). A partial fill leaves the group resting.
                let contingency_ev = self.contingency_terminal(&ev);
                // ⚠ THE FOLD'S VERDICT GATES THE DRIVE (hostile-venue hardening). `contingency_ev`
                // is classified from the EVENT ALONE, so before this the bracket/OCO/OTO machinery
                // reacted to events the engine had REFUSED — "invalid transitions are dropped" did
                // not extend to it. A venue frame naming an unknown coid is dropped and counted into
                // `dropped_unknown_coid`, yet a fabricated `OrderFilled` on one OCO leg still
                // cancelled its SIBLING (name the take-profit ⇒ the stop-loss goes and the position
                // is left NAKED), and a fabricated fill on a bracket entry still armed its held OTO
                // children into real venue orders. Legitimate events are untouched: they fold
                // `Applied`, so an accepted terminal drives exactly what it drove before.
                //
                // ONLY THIS LANE IS GATED. `pump_client` and `publish_and_drive_outbox` carry
                // events synthesized IN-PROCESS, not venue frames — and gating the latter would be
                // an outright REGRESSION: `gate_and_register` publishes `OrderDenied` for an order
                // it deliberately never registered, so that event folds `Dropped` BY DESIGN, and
                // suppressing its drive would orphan a denied bracket entry's held children forever
                // (`contingency_terminal`'s own doc: "a denied bracket entry must cascade-drop its
                // held children, never orphan them"). A venue cannot reach that path: `Event` is
                // never deserialized from a venue payload — every venue frame goes through a
                // per-venue `event_mapper`, and no bridge constructs `OrderDenied`.
                let fold = match self.route_event(&ev) {
                    0 => self.bus.publish(ev, &mut self.engine),
                    i => self.bus.publish(ev, &mut self.extra_engines[i - 1].1),
                };
                self.pump_client();
                match contingency_ev {
                    // The `fold` guards sit on the ARMS rather than wrapping the whole `match`, so
                    // the overwhelmingly common no-bracket path — `contingency_terminal`
                    // early-returns `None` on an empty book — never evaluates the comparison.
                    Some((coid, true)) if fold == Fold::Applied => {
                        self.drive_contingency_on_fill(&coid, now_all);
                        // fold the release submits / sibling cancels the drive just queued
                        self.pump_client();
                    }
                    Some((coid, false)) if fold == Fold::Applied => {
                        self.drive_contingency_on_terminal(&coid)
                    }
                    // A terminal-shaped event the engine REFUSED. Per-order and fault-adjacent
                    // (never per-tick), and it is the one signature a fabricated venue frame leaves.
                    // A terminal-shaped event the engine REFUSED. Per-order and fault-adjacent
                    // (never per-tick), and it is the one signature a fabricated venue frame leaves.
                    Some((coid, _)) => tracing::warn!(
                        target: "vike_exec::oms",
                        coid = %coid,
                        "contingency drive SUPPRESSED: the engine dropped this terminal event \
                         (unknown coid / illegal transition / replay), so it arms and cancels \
                         nothing"
                    ),
                    None => {}
                }
                self.dirty = true;
            }
            Ingest::Command(cmd) => {
                match cmd {
                    Command::Order(intent) => {
                        self.apply_intent(intent, self.engine.now_ms);
                    }
                    // Live-parameter plane (audit co8): route a typed params update to the ONE mount
                    // on (venue, symbol, interval) and run its `on_params_updated` hook — a running
                    // strategy is re-tuned WITHOUT unmounting (which would lose queue position). A
                    // RARE control command (a GUI/operator re-tune), NOT a market message, so it is
                    // off the p99<10µs event fold; it touches NO OMS state (only the mounted
                    // strategy's tunables + any orders that hook buffers, drained the one live path).
                    Command::UpdateParams(u) => {
                        let ParamsUpdate { venue, symbol, interval, params } = *u;
                        self.drive_strategy_params(&venue, &symbol, &interval, &params);
                    }
                    Command::SetTradingState(st) => {
                        self.engine.trading_state = st;
                        for (_, e) in self.extra_engines.iter_mut() {
                            e.trading_state = st;
                        }
                    }
                    // LIVE per-symbol leverage change — write the target venue's engine's
                    // `im_by_symbol` (rare operator/GUI verb; off the hot fold).
                    //
                    // ⚠ Deliberately `venue`, not `route_key`, and deliberately a FAN-OUT rather
                    // than `engine_idx_for_route_key`: this is the one command in this file that
                    // already addresses ENGINES BY VENUE and applies to every match. Identical
                    // today (one engine per venue), and left alone because the operator's verb is
                    // genuinely venue-shaped — `MarginUpdate` has no field that could name one
                    // account of two. Wiring a second account has to decide what "set binance
                    // leverage" means before this site can move; guessing now would silently pick
                    // one of the two accounts, which is worse than the fan-out.
                    Command::SetMargin(u) => {
                        let MarginUpdate { venue, symbol, im_requirement } = *u;
                        if self.engine.venue == venue {
                            self.engine
                                .gate
                                .limits
                                .im_by_symbol
                                .insert(symbol.clone(), im_requirement);
                        }
                        for (_, e) in self.extra_engines.iter_mut() {
                            if e.venue == venue {
                                e.gate.limits.im_by_symbol.insert(symbol.clone(), im_requirement);
                            }
                        }
                    }
                    // ReconcileSnapshot carries no venue — primary only; extra venues
                    // reconcile BEFORE spawn (their engines are built by the caller).
                    // Audit exec#2 DRIFT DETECTION: before venue truth overwrites the
                    // locally-folded Account, diff the two and surface any divergence
                    // (position size / authoritative balance / open-order set) through the
                    // GUI-visible recent-events ring + a tracing::warn — the same soft-signal
                    // channel the margin-call watchdog uses. Venue truth STILL wins the seed
                    // (apply_snapshot below is unchanged); this only ADDS the alert the silent
                    // overwrite was swallowing. Fires on every ReconcileSnapshot the core is
                    // handed — the startup/session reconcile (venue smoke tests drive exactly this
                    // path) AND, now, the opt-in `CoreHandle::spawn_periodic_reconcile` driver
                    // (default off) that re-issues ApplySnapshot on an interval so drift-checking is
                    // CONTINUOUS, not startup-only — both flow through THIS one handler with zero
                    // further change. Diffing at this ONE choke point stays the in-scope cadence: NO
                    // new wire verb or Event variant. A RECONNECT-triggered re-snapshot from the
                    // venue adapter stays the DEFERRED fork (the bridge-core A3 resync + user-data
                    // pump hold an EventSender, not a command lane, so they replay missed lifecycle
                    // Events only, never a re-snapshot). This is a
                    // rare command, NOT the per-event hot fold, so a tracing::warn here is within
                    // the per-order-boundary / fault-transition logging budget.
                    Command::ApplySnapshot(snap) => {
                        for w in self.engine.diff_snapshot(&snap) {
                            tracing::warn!(
                                target: "vike_core::reconcile",
                                venue = %self.engine.venue,
                                "{w}"
                            );
                            self.note(w);
                        }
                        self.engine.apply_snapshot(&snap);
                    }
                    Command::ReconcileReports(reports) => {
                        self.reconcile_reports(*reports);
                    }
                    Command::ConfirmRecon(id) => {
                        self.confirm_recon(id);
                    }
                    // RUNTIME strategy mount/unmount (split-plane B5) — occasional operator verbs,
                    // off the p99 event fold (the `UpdateParams` argument verbatim). Every failure
                    // is a REFUSAL note in recent-events, never a panic: unlike spawn-time
                    // assembly, a running core must keep trading through a bad mount request.
                    Command::MountStrategy(spec) => {
                        self.mount_strategy_runtime(*spec);
                    }
                    Command::UnmountStrategy { controller_id } => {
                        self.unmount_strategy_runtime(&controller_id);
                    }
                    Command::Shutdown => unreachable!("handled in handle()"),
                }
                self.dirty = true;
            }
            Ingest::Market => self.drain_market(),
            Ingest::BarSeed(seed) => {
                let BarSeed { venue, symbol, interval, bars } = *seed;
                let series = self.bars.entry((venue, symbol, interval)).or_default();
                series.closed = Arc::new(bars);
                series.forming = None; // the live stream refreshes it
                self.dirty = true;
            }
            Ingest::BarClose(update) => {
                let BarUpdate { venue, symbol, interval, bar } = *update;
                let key = (venue, symbol, interval);
                // STAMP THE SERIES SYMBOL ONCE, here, so every downstream consumer sees the same
                // bar: the paper client (which already got a stamped COPY below), the closed-bar
                // cache that `Broker::bars` hands back, and the strategy's `on_bar`.
                //
                // Live bars arrive with `symbol: None` — `vike_bridge_core::klines::kline_to_bar`
                // builds them from a venue kline that carries no vike symbol — and the runtime is
                // the first place that KNOWS it (`key.1`). Withholding it made a mount's `on_bar`
                // structurally unable to tell one series from another, which is why a two-leg
                // strategy could not work live even after its ORDERS were routed correctly.
                //
                // Safe for every existing mount BECAUSE of the opt-in routing lane: a mount that
                // declares no extra symbols ignores the `symbol` argument of the `Broker` verbs
                // entirely (`resolve_intent_symbol`), so a strategy that now passes a REAL symbol
                // where it used to pass `""` routes exactly where it did before. Before that lane
                // existed this stamp would have sent a real symbol to venues, which is why it was
                // withheld.
                //
                // A bar that ALREADY carries a symbol is left alone (a backtest/seeded bar names
                // its own instrument, and the replayed value is the source of truth).
                let mut bar = bar;
                if bar.symbol.is_none() {
                    bar.symbol = Some(key.1.clone());
                }
                let last_ts = self.bars.get(&key).and_then(|s| s.closed.last().map(|b| b.ts));
                let append = match last_ts {
                    // reconnect overlap: the same window re-closes — replace idempotently
                    Some(t) if bar.ts == t => {
                        if let Some(series) = self.bars.get_mut(&key)
                            && let Some(last) = Arc::make_mut(&mut series.closed).last_mut()
                        {
                            *last = bar.clone();
                        }
                        false
                    }
                    Some(t) if bar.ts < t => false, // stale replay — drop
                    _ => true,
                };
                if append {
                    // Paper fills for the engine's own (venue, symbol) fold BEFORE the bar
                    // joins the cache: their `on_fill` deliveries see history through bar
                    // i-1 at the standing mark — the backtest fill-phase view (fills happen
                    // at bar i's OPEN; bar i's close is not knowable at that moment).
                    // ⚠ EVERY engine of this exchange, not just its default account. The bar is
                    // the paper exchange's fill clock (`ExecutionClient::on_bar`; a real venue
                    // adapter's default impl ignores it), and it was delivered only to the engine
                    // the venue string resolves — so a SECOND account whose credentials were absent
                    // at mount and fell back to `vike_paper::PaperExecutionClient` held a book
                    // nothing ever filled: its strategy's orders rested forever, never filling and
                    // never terminalizing, with no error anywhere.
                    //
                    // `engines_of_venue` is `[default]` on every single-account core, so the
                    // sequence below — including `pump_client`, which already drains every
                    // engine — is unchanged there.
                    for eidx in self.engines_of_venue(&key.0) {
                        if self.eng(eidx).accepts_symbol(&key.1) {
                            // The series symbol is stamped once at the top of this arm now, so a
                            // multi-book paper client can route and every consumer agrees on the
                            // same bar; single-book clients ignore `bar.symbol` (r7 law intact).
                            self.eng_mut(eidx).client.on_bar(&bar);
                            self.pump_client();
                            self.dispatch_applied_fills();
                        }
                    }
                    let series = self.bars.entry(key.clone()).or_default();
                    Arc::make_mut(&mut series.closed).push(bar.clone());
                }
                // a close supersedes any forming state of that (or an older) window
                if let Some(series) = self.bars.get_mut(&key)
                    && series.forming.as_ref().is_some_and(|f| f.ts <= bar.ts)
                {
                    series.forming = None;
                }
                self.dirty = true;
                if append {
                    self.drive_strategy(&key, &bar);
                }
            }
            Ingest::Quote(qu) => {
                let QuoteUpdate { venue, symbol, quote } = *qu;
                let px = quote.mid();
                let ts = quote.ts;
                if let Some(eidx) = self.engine_idx_for_route_key(RouteKey::sole_account_of(&venue))
                    && self.eng(eidx).accepts_symbol(&symbol)
                {
                    self.eng_mut(eidx)
                        .price_board
                        .set_quote(&venue, &symbol, quote.bid, quote.ask, ts);
                }
                self.drive_strategy_tick(&venue, &symbol, px, ts, |s, ctx| {
                    s.on_quote_tick(ctx, &quote)
                });
                // The cross-venue REFERENCE lane (xEMM): the same touch, delivered to any mount on
                // a DIFFERENT venue that declared this `(venue, symbol)` as a leg. Disjoint from
                // the tick lane above by construction (that one requires `m.venue == venue`, this
                // one `!=`), so no mount is dispatched twice. `any_mount_ref` is false for every
                // runtime without a cross-venue mount, making this one bool load on the fold.
                if self.any_mount_ref {
                    self.drive_strategy_reference_quote(
                        &venue,
                        &symbol,
                        quote.bid,
                        quote.ask,
                        quote.bid_size,
                        quote.ask_size,
                        ts,
                    );
                }
            }
            Ingest::Trade(tu) => {
                let TradeUpdate { venue, symbol, trade } = *tu;
                let px = trade.price;
                let ts = trade.ts;
                if let Some(eidx) = self.engine_idx_for_route_key(RouteKey::sole_account_of(&venue))
                    && self.eng(eidx).accepts_symbol(&symbol)
                {
                    self.eng_mut(eidx).price_board.set_last_trade(&venue, &symbol, px, ts);
                }
                self.drive_strategy_tick(&venue, &symbol, px, ts, |s, ctx| {
                    s.on_trade_tick(ctx, &trade)
                });
            }
            Ingest::Book(bu) => {
                // `book` is an `Arc<L2Book>` (perf audit finding #1): every read below and the
                // `&L2Book` handed to `on_order_book` reach through `Deref`, and the drop at the
                // end of this arm is a refcount DECREMENT — not the full two-`BTreeMap` teardown
                // a by-value book cost this thread, the one the `p99 < 10µs` gate measures.
                // The producing pump keeps folding into the same allocation via `Arc::make_mut`.
                let BookUpdate { venue, symbol, book } = *bu;
                // A two-sided top is needed to mark/price the step; a one-sided/empty book has
                // no mid, so the strategy sees the next (fuller) update (v1 simplification).
                if let Some(px) = book.mid() {
                    let now = self.engine.now_ms;
                    // The derived TOP, read once. Sizes are bound too (they used to be discarded)
                    // because the cross-venue reference lane below delivers a full `QuoteTick`: a
                    // maker sizing against the reference venue's displayed depth needs them, and a
                    // book's derived L1 must be indistinguishable from that venue's native L1.
                    let top = (book.best_bid(), book.best_ask());
                    if let (Some((bb, _)), Some((ba, _))) = top
                        && let Some(eidx) =
                            self.engine_idx_for_route_key(RouteKey::sole_account_of(&venue))
                        && self.eng(eidx).accepts_symbol(&symbol)
                    {
                        self.eng_mut(eidx).price_board.set_quote(&venue, &symbol, bb, ba, now);
                    }
                    self.drive_strategy_tick(&venue, &symbol, px, now, |s, ctx| {
                        s.on_order_book(ctx, &book)
                    });
                    // The cross-venue REFERENCE lane (xEMM) — the book twin of the quote arm's
                    // call above. A foreign venue's L2 is delivered as its DERIVED L1, not as an
                    // `L2Book`: `L2Book` carries neither a venue nor a symbol
                    // (`vike_model::orderbook`), so a strategy handed two venues' books could not
                    // attribute them, and stamping either onto that journaled serde payload would
                    // be a wire change. The touch is what a reference-priced maker consumes anyway.
                    if self.any_mount_ref
                        && let (Some((bb, bq)), Some((ba, aq))) = top
                    {
                        self.drive_strategy_reference_quote(&venue, &symbol, bb, ba, bq, aq, now);
                    }
                }
            }
            // audit C3 + co6: the watchdog SWEEP now runs at the drain-loop boundary, driven by the
            // `DeadlineTimerWheel` (see `run` + `drive_due_timers`), NOT here. This message is kept
            // purely as the periodic WAKER (the OS thread injects it so an idle core reaches the
            // boundary on cadence) — and it is still JOURNALED (the `matches!` above), which is what
            // keeps a watchdog session's journal refused for replay (`replay.rs`), preserving the
            // existing "watchdog sweep is not a deterministic function of the journal" exclusion.
            Ingest::Watchdog => {}
            // Feed-health status CHANGE (net-hardening §B) — an OCCASIONAL control event (the
            // producer fires it only on a StreamStatus transition), so this is NOT the per-tick
            // hot path. Touches NO OMS fold: it only runs the mounted strategy's on_feed_status
            // hook + drains any orders that hook buffered (e.g. a "pull my quotes" mass_cancel)
            // through the one live path.
            Ingest::StreamStatus(su) => {
                let StreamStatusUpdate { venue, symbol, stream: _, status } = *su;
                self.drive_strategy_feed_status(&venue, &symbol, status);
            }
            // Per-side FLOW-TOXICITY update (RTDS wallet-toxicity guard) — an OCCASIONAL control
            // event (the producer fires it at toxicity cadence, not per market message), so this is
            // NOT the per-tick hot path. Touches NO OMS fold: it only runs the mounted strategy's
            // on_flow hook + drains any orders that hook buffered through the one live path. The
            // exact twin of the StreamStatus arm above.
            Ingest::Flow(fu) => {
                let FlowUpdate { venue, symbol, flow } = *fu;
                self.drive_strategy_flow(&venue, &symbol, flow);
            }
        }
        // CADENCE SNAPSHOT (spec §A): taken AFTER the fold above so the snap reflects THIS message's
        // effect. Taken before (as the write-ahead append is), a cadence snap would omit the just-
        // journaled message while that message sits at a LOWER seq than the snap — and replay
        // (latest-Snap-wins, apply seq > snap) would silently drop it: a <=1-message loss on a
        // cadence-snap restore. `journaled` gates this to the same exec-lane messages the counter
        // tracks (never a market/bar/tick) and is false when journaling is off, so the disabled
        // path stays zero-overhead and byte-identical.
        if journaled
            && self.journaled_since_snap >= self.config.journal.as_ref().unwrap().snapshot_every
        {
            self.write_snap(now_all);
        }
    }

    /// RUNTIME strategy MOUNT (split-plane B5) — the [`Command::MountStrategy`] arm's body. Adds
    /// one mount to the live slot vector WITHOUT a restart, resolving the spec through the
    /// composition root's injected [`CoreConfig::strategy_factory`].
    ///
    /// COLD PATH: runs only when a mount command arrives (an occasional operator verb, the
    /// `UpdateParams` argument). Nothing on the per-event fold changes: the fold keeps reading the
    /// same `any_mount_*` booleans, which [`Self::recompute_mount_gates`] re-derives HERE, off the
    /// event path.
    ///
    /// Every failure is a REFUSAL note in recent-events, never a panic — the runtime twin of
    /// [`assemble_core`]'s duplicate-id PANIC, which deliberately stays: at spawn a duplicate is a
    /// configuration fault caught before a single order exists, while a live core must keep
    /// trading through a bad mount request.
    fn mount_strategy_runtime(&mut self, spec: vike_exec::MountSpec) {
        let mid = crate::strategy_state::mount_id_with(
            spec.controller_id.as_deref(),
            &spec.venue,
            &spec.symbol,
            &spec.interval,
        );
        // (1) Duplicate check over LIVE slots only: a tombstoned (unmounted) slot keeps its id as
        // the label of its residual attribution ledger, but is no longer an identity holder — a
        // re-mount of the same id is legitimate and lands on a FRESH slot.
        if (0..self.mounts.len()).any(|i| self.mounts[i].is_some() && self.mount_ids[i] == mid) {
            self.note(format!(
                "MOUNT REFUSED: duplicate strategy-mount id `{mid}` — a mount with this identity \
                 is already live (give the new mount a distinct controller_id)"
            ));
            return;
        }
        // (2) The venue — AND THE ACCOUNT — must name an engine this core already runs: a mount
        // cannot conjure one, and orders from an engine-less venue would fall through
        // `engine_idx_for_route_key`'s primary fallback and be booked against the wrong venue's
        // account.
        //
        // ⚠ The account half is a REFUSAL and not a fallback, which is the one place this file
        // departs from degrade-not-refuse: a mount whose operator named `ALT` running on the
        // DEFAULT account is the catastrophe itself — silent, and indistinguishable from working.
        // `mount_engine_idx` panics on the same miss at spawn; this arm reaches it first for a
        // RUNTIME mount, where a refused command is the right shape (the daemon stays up and the
        // operator gets a note naming what to fix).
        let account_key = vike_model::account_keys::route_key_of(
            &spec.venue,
            spec.account.as_ref().unwrap_or(&vike_model::account_keys::AccountLabel::Default),
        );
        if self.engine_idx_for_route_key(RouteKey::declared(&account_key)).is_none() {
            let mounted: Vec<&str> = std::iter::once(self.engine.route_key.as_str())
                .chain(self.extra_engines.iter().map(|(_, e)| e.route_key.as_str()))
                .collect();
            self.note(match spec.account.as_ref().filter(|l| !l.is_default()) {
                None => format!(
                    "MOUNT REFUSED: `{mid}` names venue `{}`, but this core runs no engine for it",
                    spec.venue
                ),
                Some(label) => format!(
                    "MOUNT REFUSED: `{mid}` names account `{label}` of venue `{}`, and this core \
                     runs no engine with route key `{account_key}` (mounted: {mounted:?}). It will \
                     NOT be mounted on that venue's default account — arm the account \
                     (`policy.accounts.{}.{label}`, plus its `__{label}` credential keys)",
                    spec.venue, spec.venue
                ),
            });
            return;
        }
        // (3) Resolve through the injected factory — composition-root/user code (a registry
        // build, a Rhai compile), guarded like every other strategy-code call site in this file.
        let Some(factory) = self.config.strategy_factory.as_mut() else {
            self.note(format!(
                "MOUNT REFUSED: `{mid}` — no strategy factory is configured on this core \
                 (runtime mounts are disabled on this binary)"
            ));
            return;
        };
        let strategy = match catch_unwind(AssertUnwindSafe(|| factory(&spec))) {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                self.note(format!("MOUNT REFUSED: `{mid}` — {e}"));
                return;
            }
            Err(payload) => {
                self.note(format!(
                    "MOUNT REFUSED: `{mid}` — strategy factory panicked: {}",
                    panic_text(payload)
                ));
                return;
            }
        };
        let mut mount = StrategyMount {
            account: spec.account.clone(),
            venue: spec.venue.clone(),
            symbol: spec.symbol.clone(),
            interval: spec.interval.clone(),
            strategy,
            symbols: Vec::new(),
            underlying_symbol: None,
            controller_id: spec.controller_id.clone(),
        };
        // (4) Durable-state load — the same sidecar read + panic guard `assemble_core` applies at
        // spawn, so a strategy unmounted earlier (or in a prior session) resumes its state.
        if let Some(dir) = self.config.state_dir.as_ref() {
            let sidecar = crate::strategy_state::sidecar_path(dir, &mid);
            if let Some(v) = crate::strategy_state::read_json(&sidecar)
                && let Err(payload) =
                    catch_unwind(AssertUnwindSafe(|| mount.strategy.load_state(&v)))
            {
                tracing::warn!(
                    target: "vike_core::strategy_state",
                    mount_id = %mid,
                    reason = %panic_text(payload),
                    "strategy load_state panicked — starting fresh"
                );
            }
        }
        // (5) Arm applied-fill capture the way spawn would have: the primary engine collects
        // whenever ANY mount exists (`spawn_core_multi`'s rule), an extra engine when a mount
        // trades its venue (`assemble_core`'s rule; a runtime mount declares no cross-venue legs,
        // so venue equality is the whole test). NEVER disarmed on unmount — a sibling mount may
        // depend on it, and an in-flight order's fill must still be capturable.
        self.engine.collect_applied_fills = true;
        for (_, e) in self.extra_engines.iter_mut() {
            if e.venue == mount.venue {
                e.collect_applied_fills = true;
            }
        }
        // (6) Grow EVERY parallel per-mount vector together (same indices — `assemble_core`'s
        // layout law). APPEND-ONLY: an existing slot's index is an attribution key (`coid_mount`
        // values, `strategy_tags`' `{idx}|` prefix), so nothing may ever shift.
        let state =
            if self.config.readiness_gate { MountState::Pending } else { MountState::Ready };
        self.mount_ids.push(mid.clone());
        self.mount_states.push(state);
        self.mount_budget.push(self.config.mount_budgets.get(&mid).copied());
        self.mount_vs.push((mount.venue.clone(), mount.symbol.clone()));
        // …and the ENGINE this mount trades on, resolved through the SAME function `assemble_core`
        // uses, so a runtime mount and a spawn mount cannot disagree about which book they are on.
        // Step (2) above has already refused the miss, so the panic arm is unreachable from here.
        self.mount_engine.push(mount_engine_idx(&self.engine, &self.extra_engines, &mount, &mid));
        self.mount_latched.push(false);
        self.mount_attr.push(MountAttribution::default());
        self.mount_symbols.push(Vec::new());
        self.mount_schedule.push(self.config.mount_schedules.remove(&mid).unwrap_or_default());
        self.mounts.push(Some(mount));
        self.recompute_mount_gates();
        // (7) Record the mount in the TOPOLOGY sidecar (B5 residual closed) — daemon-level STATE,
        // deliberately NOT a journal write: the `journaled` match in `dispatch` still excludes
        // this command, because the mount-less replay core (no `strategy_factory`) could never
        // re-fold it and the replay determinism fence must stay over exactly what re-folds. The
        // sidecar rides the same `state_dir` gate and the same atomic-rename shape as the
        // strategy-state sidecar this arm loads in step (4); the composition root replays it at
        // startup through this very command path — [`crate::mount_topology`]'s module doc is the
        // seam's authority. SUCCESS-ONLY and best-effort: a refusal above records nothing, and a
        // write failure warns without unwinding the mount that already landed.
        if let Some(dir) = self.config.state_dir.as_ref()
            && let Err(e) = crate::mount_topology::upsert(
                dir,
                crate::mount_topology::MountRecord::stamped(
                    spec.clone(),
                    self.config.clock.now_ms(),
                ),
            )
        {
            tracing::warn!(
                target: "vike_core::mount_topology",
                mount_id = %mid,
                error = %e,
                "failed to record runtime mount in the topology sidecar (best-effort) — the \
                 mount is live but will NOT resurrect on restart"
            );
        }
        self.note(format!(
            "MOUNTED strategy `{mid}` ({}/{} @ {})",
            spec.venue, spec.symbol, spec.interval
        ));
    }

    /// RUNTIME strategy UNMOUNT (split-plane B5) — the [`Command::UnmountStrategy`] arm's body.
    ///
    /// ⚠ **THE RESTING-ORDER DECISION** (the spec's named trading-safety question): unmount
    /// CANCELS the mount's live orders BEFORE removal — the safe default. After removal nothing
    /// routes `on_fill`/`on_order_event` to this strategy any more, so an order left resting would
    /// be an UNMANAGED book: a maker's stale quote sitting at the venue until an operator notices,
    /// which is exactly the silent-vanish class the emitter-split contract exists to prevent.
    /// Scope: EXACTLY the mount's attributed coids (`coid_mount` — the same attribution
    /// [`Self::latch_mount`] cancels by), so a sibling mount on the same symbol is untouched;
    /// terminal/unknown coids are no-ops in the engine's cancel path. It CANCELS; it does NOT
    /// flatten — the standing stop-policy law ([`CoreConfig::cancel_orders_on_shutdown`]'s doc):
    /// the mount's attributed position stays open and visible in the residual attribution row, and
    /// closing it is an operator decision (`Flatten`), never a side effect of unmounting. The
    /// cancels are replay-neutral exactly like `latch_mount`'s (the venue's authoritative
    /// `OrderCanceled` journals as its own `Ingest::Event`).
    fn unmount_strategy_runtime(&mut self, controller_id: &str) {
        // The identity law verbatim: ids are STORED sanitized (`mount_id_with`), so the requested
        // id is sanitized the same way — `maker-a` finds the mount keyed `maker_a`. An
        // empty/whitespace id can name nothing.
        if controller_id.trim().is_empty() {
            self.note("UNMOUNT REFUSED: empty mount id".to_string());
            return;
        }
        let want = crate::strategy_state::mount_id_with(Some(controller_id), "", "", "");
        let Some(i) =
            (0..self.mounts.len()).find(|&i| self.mounts[i].is_some() && self.mount_ids[i] == want)
        else {
            self.note(format!("UNMOUNT REFUSED: no live strategy mount with id `{want}`"));
            return;
        };
        let now = self.engine.now_ms;
        // (1) Cancel this mount's attributed live orders (the decision documented above).
        let coids: Vec<String> =
            self.coid_mount.iter().filter(|&(_, &m)| m == i).map(|(c, _)| c.clone()).collect();
        let canceled = coids.len();
        if !coids.is_empty() {
            // Unclassified on purpose (never `Routine`): once the mount is removed nothing re-runs
            // this, so a cancel a venue held back would leave the UNMANAGED book this decision
            // exists to prevent. Not labeled `RiskOff` either — an unmount is not an emergency; it
            // takes the flatten-safe default, which is what "must not be shed" already means.
            self.apply_intent(OrderIntent::CancelBatch(coids), now);
        }
        // (2) Durable-state save — the single-slot body of [`Self::save_all_strategy_state`]
        // (same panic guard, same atomic write), so the strategy's state survives to a later
        // re-mount or restart.
        if let Some(dir) = self.config.state_dir.as_ref()
            && let Some(mount) = self.mounts[i].as_ref()
        {
            match catch_unwind(AssertUnwindSafe(|| mount.strategy.save_state())) {
                Ok(Some(v)) => {
                    let sidecar = crate::strategy_state::sidecar_path(dir, &want);
                    if let Err(e) = crate::strategy_state::write_json_atomic(&sidecar, &v) {
                        tracing::warn!(
                            target: "vike_core::strategy_state",
                            mount_id = %want,
                            error = %e,
                            "failed to save strategy state sidecar on unmount (best-effort)"
                        );
                    }
                }
                Ok(None) => {}
                Err(payload) => {
                    tracing::warn!(
                        target: "vike_core::strategy_state",
                        mount_id = %want,
                        reason = %panic_text(payload),
                        "strategy save_state panicked on unmount (best-effort)"
                    );
                }
            }
        }
        // (3) Retire the slot's tag-registry entries (strategy-local names keyed `{i}|…`): the
        // orders they point at were just canceled, and nothing else ever prunes the map.
        let prefix = format!("{i}|");
        self.strategy_tags.retain(|k, _| !k.starts_with(&prefix));
        // (4) Tombstone: the slot goes `None` PERMANENTLY and its index is never reused —
        // `coid_mount` values and the journal's `StrategySubmit` provenance already point at this
        // index/id, and reusing it would hand a FUTURE mount the dead strategy's in-flight fills.
        // Every runtime iterator already skips a `None` slot (the take/replace-dance defensive
        // filters); `mount_ids[i]`/`mount_attr[i]` deliberately STAY, so a straggler fill of a
        // just-canceled order still folds into the dead mount's ledger (the published view's
        // residual row absorbs it — `mount_views` skips the tombstone). Per-slot FEATURE state is
        // cleared so [`Self::recompute_mount_gates`] sees exactly what `assemble_core`'s formulas
        // would; `Ready` keeps `maintain_mount_readiness`'s Pending short-circuit true.
        self.mounts[i] = None;
        self.mount_states[i] = MountState::Ready;
        self.mount_budget[i] = None;
        self.mount_symbols[i].clear();
        self.mount_schedule[i] = LiveSchedule::default();
        self.mount_latched[i] = false;
        self.recompute_mount_gates();
        // (5) Forget the mount in the TOPOLOGY sidecar (B5 residual closed) — the atomic-rewrite
        // twin of the mount arm's step (7), and the ONE place a runtime mount is forgotten: a
        // clean shutdown keeps records deliberately (a runtime mount survives a restart the way
        // it survives a crash — "stop the daemon" is not "unmount"), so only this explicit verb
        // removes one. Removing an id that was never recorded (a spawn-time profile mount, or a
        // mount from before `state_dir` was armed) is a quiet no-op inside `remove`. Same
        // not-a-journal-write seam as the mount arm: [`crate::mount_topology`]'s module doc.
        if let Some(dir) = self.config.state_dir.as_ref()
            && let Err(e) = crate::mount_topology::remove(dir, &want)
        {
            tracing::warn!(
                target: "vike_core::mount_topology",
                mount_id = %want,
                error = %e,
                "failed to remove the unmounted strategy from the topology sidecar \
                 (best-effort) — a restart may resurrect a mount the operator removed"
            );
        }
        self.note(format!("UNMOUNTED strategy `{want}` — canceled {canceled} attributed order(s)"));
    }

    /// Re-derive the four `any_mount_*` inert-when-empty gates after a runtime mount/unmount —
    /// the SAME formulas [`assemble_core`] evaluates at spawn, kept verbatim so the two sites can
    /// never disagree about when a feature is armed. COLD PATH: called only from the two command
    /// arms above; the per-event fold keeps reading one bool per gate, exactly as before.
    fn recompute_mount_gates(&mut self) {
        self.any_mount_budget = self.mount_budget.iter().flatten().any(|b| b.is_active());
        self.any_mount_multi = self.mount_symbols.iter().any(|v| !v.is_empty());
        self.any_mount_ref = self.mounts.iter().enumerate().any(|(i, m)| {
            m.as_ref().is_some_and(|m| {
                self.mount_symbols[i]
                    .iter()
                    .any(|l| l.venue.as_deref().is_some_and(|v| v != m.venue))
            })
        });
        self.any_mount_schedule = self.mount_schedule.iter().any(|s| !s.is_empty());
    }

    /// Write a full-state `Snap` record (spec §A "snapshot-as-command"): every engine (primary +
    /// extras) via [`ExecutionEngine::snapshot_state`], the coid `(session, seq)` for restart
    /// continuity, and `state_hash` as the determinism fence the T5 replay checks against. Asks the
    /// journal's syncer thread to carry the checkpoint to disk (a Snap is the durability checkpoint
    /// the next replay resumes from) and resets the cadence counter. No-op when journaling is off,
    /// so callers may invoke it unconditionally.
    ///
    /// # ⚠ This runs on the FOLD THREAD — the one the `p99 < 10µs` gate protects
    ///
    /// Both cadence call sites (`dispatch`'s tail and `pump_client`'s [`Self::maybe_cadence_snap`])
    /// fire from inside the single-writer fold, once per `snapshot_every` journaled records — 1024
    /// at the production default. So **nothing in here may block**, and until #932 something did:
    /// this function ended with `CommandJournal::flush()`, the blocking whole-mapping `msync` whose
    /// own doc says it is "not safe to call from the vike-core fold". #929 had moved the journal's
    /// other two blocking `msync`s off this thread and left this one, invisibly — `runtime_latency`
    /// pinned `snapshot_every = u64::MAX`, so no snapshot ever fired inside a measured hop.
    ///
    /// MEASURED on the latency box (cores 28-31, `SCHED_FIFO 50`, 100 000 hops, 64 MiB segment, ~287 B/record
    /// ⇒ ~287 KB dirty per snap), production config vs the same run with `snapshot_every = u64::MAX`:
    ///
    /// ```text
    ///   variant       p99      p99.9     max        hops>100µs
    ///   journal       5.2 µs   38.6 µs    77.7 µs      0        (no snap fires)
    ///   journal-snap  6.1 µs  153.9 µs    14.1 ms    102        (97 snaps + jitter)
    /// ```
    ///
    /// One hop over 100 µs per snapshot, near-exactly (`snaps_expected=97`), and a worst hop of
    /// 10.7–16.7 ms across four reps — ~1 400x the budget. One rep also breached the plan gate
    /// itself (`p99 = 10 059 ns`). `tests/runtime_latency.rs`'s `journal-snap` variant is that
    /// measurement, kept as a gate so the call cannot come back unnoticed.
    ///
    /// The cure is the mechanism #929 already built: post the watermark to the journal's syncer
    /// thread and return (`queue_sync`). The snapshot BUILD below stays here — it is pure CPU,
    /// O(engines + arms + legs + mounts), and runs at snapshot cadence, not per message.
    fn write_snap(&mut self, now_ms: i64) {
        if self.journal.is_none() {
            return;
        }
        let mut engines: Vec<vike_exec::EngineSnapshot> =
            Vec::with_capacity(1 + self.extra_engines.len());
        engines.push(self.engine.snapshot_state());
        for (_, e) in &self.extra_engines {
            engines.push(e.snapshot_state());
        }
        let (coid_session, coid_seq) = self.coid_gen.state();
        let hash = vike_exec::state_hash(&engines);
        // `arm_seq` rides the Snap next to `coid_seq`, mirroring it exactly: both are the id
        // counters a restart must resume, and the Snap is the ONE restore base — a value derived
        // from the surviving record set instead (the pre-v7 shape) is prune-UNSAFE, because
        // `prune_before_latest_snap` deletes whole early segments out from under the count.
        let arm_seq = self.arm_seq;
        // Emulator PR-3: the resting conditional BOOKS ride the Snap too — the only durable home
        // for a trailing arm's CURRENT ratcheted extreme (it moves on non-journaled market data,
        // so neither the write-ahead ARM `Cmd` nor the `ConditionalArmed` seed record can carry
        // it). Captured in fire order (books map insertion order, then each book's). O(arms) at
        // snapshot cadence, zero cost on the per-message fold. Deliberately NOT folded into
        // `hash` (which stays `state_hash(&engines)`): book contents move fenced state only
        // through a FIRE, and every fire has its own write-ahead record — see `crate::replay`.
        let mut conditionals: Vec<crate::journal::SnapConditional> = Vec::new();
        for ((venue, symbol), book) in &self.conditional_books {
            for (arm_id, o) in book.iter() {
                conditionals.push(crate::journal::SnapConditional {
                    arm_id: arm_id.to_string(),
                    terms: crate::journal::ConditionalRecord {
                        venue: venue.clone(),
                        symbol: symbol.clone(),
                        side: o.order.side,
                        qty: o.order.size,
                        price: o.order.price,
                        trail: o.order.trail,
                        extreme: o.order.extreme,
                        trigger_by: o.trigger_by,
                    },
                });
            }
        }
        // Live-runtime OTO/OCO: the resting contingency book rides the Snap too — the ONLY durable
        // home for a HELD exit (a leg the runtime is keeping off the venue until its parent fills;
        // an ACTIVE leg is already in `engines` via the registry). Captured in the book's own
        // insertion order (arm/cancel iteration order); each held leg carries its resolved request
        // so a restart can re-submit it. O(legs) at snapshot cadence, nothing on the per-message
        // fold. NOT folded into `hash` (same fence argument as the conditionals above): the book
        // moves fenced state only through a fill that releases/cancels, and that fill is itself a
        // journaled `Cmd` event replay re-folds through the re-seeded book — see `crate::replay`.
        let contingencies: Vec<crate::journal::SnapContingency> = self
            .contingency
            .snapshot()
            .into_iter()
            .map(|(coid, parent, linked, active)| crate::journal::SnapContingency {
                held_request: self.held_orders.get(&coid).cloned(),
                coid,
                parent,
                linked,
                active,
            })
            .collect();
        // Multi-mount durability (gap D): the per-mount ATTRIBUTION ledgers ride the Snap too — the
        // only durable home they have. Attribution deliberately lives one layer up from `Account`
        // (which is parity-gated and wire-pinned), so nothing in `engines` carries it, and without
        // this a restart resumed every mount ledger at zero while `Account.realized_pnl` kept the
        // same PnL — mount sums silently stopped equalling the account total. One row per mount slot,
        // keyed by `mount_id` so a re-ordered mount list restores onto the right mounts. O(mounts) at
        // snapshot cadence, nothing on the per-message fold; NOT folded into `hash` (the ledger is a
        // read-only view, exactly like the two books above).
        // LIVE slots only (split-plane B5): a runtime-UNMOUNTED slot is a tombstone whose id may
        // be re-mounted onto a fresh slot later in the same session — capturing both would write
        // two rows under one `mount_id`, and the restore's first-match fold would seed the NEW
        // mount with the DEAD strategy's ledger. The tombstone's residual PnL is deliberately not
        // restored anywhere (its mount no longer exists after a restart), exactly like a
        // pre-restart manual ticket's.
        let mount_attr: Vec<crate::journal::SnapMountAttr> = self
            .mount_ids
            .iter()
            .zip(self.mount_attr.iter())
            .zip(self.mounts.iter())
            .filter(|(_, slot)| slot.is_some())
            .map(|((mount_id, a), _)| crate::journal::SnapMountAttr {
                mount_id: mount_id.clone(),
                size: a.size,
                avg_px: a.avg_px,
                realized_pnl: a.realized_pnl,
                fees_paid: a.fees_paid,
            })
            .collect();
        let j = self.journal.as_mut().unwrap();
        j.append_snap(
            now_ms,
            &engines,
            &coid_session,
            coid_seq,
            arm_seq,
            &conditionals,
            &contingencies,
            &mount_attr,
            hash,
        )
        .expect("journal snap");
        // NOT `j.flush()` — see this function's doc. One atomic store, one atomic swap and at most
        // one channel send; the `msync` itself happens on the journal's syncer thread, which this
        // thread never joins and never waits on. The watermark is monotonic, so a syncer that is
        // still busy with an earlier request coalesces this one instead of queueing behind it —
        // the backlog cannot grow and this call can never block, whatever the disk is doing.
        //
        // Durability is unchanged in every case that matters. Per the spec's §A1.1 contract an
        // un-synced tail survives a process crash / OOM-kill / clean reboot (the pages are the OS
        // page cache), a hard power cut is repaired by the post-replay venue reconcile (§A4), and a
        // CLEAN stop still forces the tail: the exit `Snap` this same function writes during
        // teardown is followed by `CoreThread::run` dropping `self` — journal included — inside the
        // vt-core thread, and `CommandJournal::drop` JOINS the syncer after its final whole-segment
        // `msync`. So `CoreHandle::shutdown_and_join` still returns only once the checkpoint is on
        // disk.
        j.queue_sync();
        self.journaled_since_snap = 0;
    }

    /// Drain every conflated slot (per-symbol freshest ticks + freshest forming bars)
    /// and clear the marker so the next publish re-arms it. One marker can carry many
    /// symbols' updates.
    /// Routing index for a ROUTE KEY: 0 = primary, i+1 = extra i, None = unknown.
    ///
    /// Matches on `ExecutionEngine::route_key` — which engine — and NEVER on
    /// `ExecutionEngine::venue`, which answers the different question of what the exchange
    /// supports. `ExecutionEngine::new` seeds the two equal, so while every engine in this process
    /// is its venue's only account this resolves exactly what the old `venue` comparison did, for
    /// every input, bit-identically.
    ///
    /// ⚠ **Almost every caller passes a CANONICAL VENUE STRING** — `FillEvent::venue`,
    /// `OrderRequest::venue`, `MountSpec::venue`, an operator's `--venue` argument. That is correct
    /// precisely while `route_key == venue` for every engine, and it is the seam a second account
    /// has to come through: the payload has to start carrying the route key, because a
    /// `FillEvent` labelled `"binance"` cannot say WHICH binance account it belongs to. Nothing
    /// resolves that here — this function only stops the ROUTING question and the CAPABILITY
    /// question from sharing one field, which is what made the two-account shape unexpressible.
    ///
    /// That "almost every" is why the parameter is a [`RouteKey`] and not a `&str`: it cannot be
    /// reached from a canonical venue without spelling [`RouteKey::sole_account_of`], so
    /// `git grep -c 'RouteKey::sole_account_of' -- crates/` IS the roster of sites a second account
    /// must revisit — a derived list rather than the prose one this doc used to carry. The
    /// exceptions, which pass [`RouteKey::declared`] because they carry a REAL route key, are
    /// [`Self::reconcile_reports`] (via `ReconcileReports::route`) and [`Self::confirm_recon`] (via
    /// the held alert's stored key).
    ///
    /// Zero-cost: `RouteKey<'_>` is a `&str`, so the per-event constructions in [`Self::route_event`]
    /// and `drain_market` allocate nothing and branch nowhere — the p99 fold is untouched.
    /// The engine that owns `(venue, symbol)` — the LAST-RESORT disambiguator [`Self::route_event`]
    /// uses when two engines share one exchange, and consulted from nowhere else.
    ///
    /// It matches on `ExecutionEngine::venue` (the exchange, shared by both accounts) plus the
    /// engine's own mounted symbol or one of its `extra_symbols`.
    ///
    /// ⚠ **It answers only when EXACTLY ONE engine claims the symbol, and that qualifier is a
    /// correction.** This used to return the FIRST match, and it was described as exact — because
    /// two active accounts of one venue could not be armed on one symbol. That refusal is gone
    /// (`vike_config::venue_accounts`' module doc: two accounts on one instrument is an ordinary
    /// spread), so a first-match answer would silently fold a second account's venue-tagged payload
    /// into the first account's book. Ambiguity is now `None`, which sends the caller to the venue
    /// lookup — the same place a symbol no engine claims has always gone.
    ///
    /// **What that leaves, declared rather than implied**: an ambiguous payload with no other
    /// handle lands on the venue's DEFAULT engine. In practice the handle is there —
    /// [`Self::route_event`] consults the payload's stamped route key (every coid-less payload:
    /// `AccountState`, `Funding`, `PositionLiquidated`) and then its client-order-id (resolved
    /// through the submit-time `coid_venue` map) BEFORE reaching here, so every payload belonging
    /// to an order this process placed, and every payload the mount stamped, routes exactly.
    ///
    /// ⚠ This residual was once stated as covering the funding and liquidation lanes too, on the
    /// strength of "every other payload carries a coid". It does not: those two carry a symbol and
    /// nothing else, so the residual silently included a labelled account's funding debits and its
    /// liquidations — see [`event_route_key`]. They are stamped now, and what is genuinely left is
    /// the one case nothing can name: a FOREIGN fill (an order this process did not place, so no
    /// `coid_venue` entry) reaching an UNSTAMPED lane, on a symbol two of its accounts both trade.
    /// That is reconcile's territory, not this lane's.
    fn engine_idx_for_venue_symbol(&self, venue: &str, symbol: &str) -> Option<usize> {
        let claims = |e: &ExecutionEngine<C>| {
            e.venue == venue && (e.symbol == symbol || e.extra_symbols.iter().any(|s| s == symbol))
        };
        let mut found = None;
        if claims(&self.engine) {
            found = Some(0);
        }
        for (i, (_, e)) in self.extra_engines.iter().enumerate() {
            if claims(e) {
                if found.is_some() {
                    // TWO accounts of one exchange on one symbol — legal, and not something this
                    // lookup can decide. Saying nothing is the whole point.
                    return None;
                }
                found = Some(i + 1);
            }
        }
        found
    }

    /// **Every engine of one EXCHANGE**, default account first — the routing answer for work that
    /// belongs to the venue rather than to one account of it.
    ///
    /// Exactly one element on every single-account core (and empty for a venue this core runs no
    /// engine for, which is the `None` the venue lookup returns there), so a caller that iterates
    /// it does on such a core precisely what the venue lookup made it do.
    fn engines_of_venue(&self, venue: &str) -> Vec<usize> {
        let primary = self.engine_idx_for_route_key(RouteKey::sole_account_of(venue));
        let mut out: Vec<usize> = primary.into_iter().collect();
        if self.multi_account {
            for i in 0..=self.extra_engines.len() {
                if Some(i) != primary && self.eng(i).venue == venue {
                    out.push(i);
                }
            }
        }
        out
    }

    fn engine_idx_for_route_key(&self, route_key: RouteKey<'_>) -> Option<usize> {
        let route_key = route_key.as_str();
        if route_key == self.engine.route_key {
            return Some(0);
        }
        self.extra_engines.iter().position(|(_, e)| e.route_key == route_key).map(|i| i + 1)
    }

    /// **THE engine one MOUNT trades on** — [`Self::mount_engine`] at that index, i.e. the account
    /// the mount declared, resolved once at assemble.
    ///
    /// Every strategy lane reads its `LiveBroker` context through this rather than re-deriving an
    /// index from the mount's venue STRING, and that is the structural half of the account seam: a
    /// venue-keyed lookup (`engine_idx_for_route_key(sole_account_of(venue))`) can only ever answer
    /// with a venue's DEFAULT account, so while the lanes asked that question a second account was
    /// unaddressable no matter what a mount declared — and the mount's READS would have kept
    /// pointing at the default account's book even once its WRITES moved, which is a worse defect
    /// and one no order-routing test would catch.
    fn mount_eng(&self, mount_idx: usize) -> usize {
        self.mount_engine[mount_idx]
    }

    /// **Lower an [`EngineRoute`] onto a concrete engine index**, given the payload's own venue.
    ///
    /// [`EngineRoute::Mount`] answers with the mount's engine when the payload is on that engine's
    /// own exchange, and defers to the payload otherwise — see that enum for why a cross-venue
    /// declared leg must not inherit the mount's account. `None` has the same meaning it has in
    /// [`Self::engine_idx_for_route_key`]: no engine claims this payload, and the caller applies
    /// its own historical fallback.
    fn route_of(&self, route: EngineRoute, payload_venue: &str) -> Option<usize> {
        let declared = match route {
            EngineRoute::Payload => None,
            EngineRoute::Mount(i) => Some(self.mount_eng(i)),
            EngineRoute::Engine(i) => Some(i),
        };
        if let Some(e) = declared {
            // The SAME foreign-venue deference `EngineRoute::Mount` needs, applied to
            // `EngineRoute::Engine` too: an engine index names an ACCOUNT of one exchange, so a
            // payload on a different exchange is not that account's business. It cannot fire for
            // either of today's `Engine` producers (both derive the venue FROM the engine they
            // name), which is exactly why it is spelled once, here, rather than assumed at each.
            if self.eng(e).venue == payload_venue {
                return Some(e);
            }
        }
        self.engine_idx_for_route_key(RouteKey::sole_account_of(payload_venue))
    }

    /// The CANONICAL venue whose declared capability row applies to a payload that has ALREADY been
    /// routed — the ROUTED engine's own `ExecutionEngine::venue`, which is the only thing
    /// `vike_model::caps_for` / `preflight_order_at` / `amend_semantics` are facts about.
    ///
    /// **Why this exists.** An order payload carries ONE venue string and it does two jobs: it is
    /// what [`Self::engine_idx_for_route_key`] routes on, and it is what the caps row is selected
    /// by. `ExecutionEngine` split those two jobs into two fields; a payload cannot, so the moment
    /// one has to carry a per-ACCOUNT routing key the caps lookup reads a string
    /// `vike_model::VENUES` does not contain — and `preflight_order`'s unknown-venue affordance
    /// answers `Ok(())`, i.e. every capability check SKIPPED (order kind, TIF, margin mode), for
    /// one account of one exchange, with no log line. Asking the engine instead of the payload
    /// removes that: the engine knows its own exchange whatever it is routed by.
    ///
    /// `routed` is [`Self::engine_idx_for_route_key`]'s VERBATIM answer, `None` included — do not
    /// pass `unwrap_or(0)`. A payload that resolves NO engine keeps its own string
    /// (`payload_venue`), because that is what the unknown-venue affordance is for (paper/sim
    /// engines behind non-roster ids) and because attributing it to the primary engine's caps row
    /// would start refusing traffic that flows today.
    ///
    /// Inert while every engine's `route_key` IS its `venue`: a routed payload's own venue string
    /// then EQUALS `self.eng(i).venue` by construction, so the answer is the same string it always
    /// was.
    fn caps_venue<'a>(&'a self, routed: Option<usize>, payload_venue: &'a str) -> &'a str {
        match routed {
            Some(i) => self.eng(i).venue.as_str(),
            None => payload_venue,
        }
    }

    /// **A venue PRICE belongs to every account of that exchange** — mirror one onto the venue's
    /// other engines, after the routed one has taken it.
    ///
    /// A mark, a bar close and a quote are facts about the EXCHANGE, not about an account: two
    /// accounts of one venue see the same book. The mark lanes route by venue, so with a second
    /// account mounted every price landed in the FIRST account's engine and the second account's
    /// `Account` priced its positions off nothing — its `resolved_equity` and
    /// `resolved_position_price` fell through to their fallbacks while the operator watched a
    /// correctly-routed order fill into a book that could not value it.
    ///
    /// ⚠ **Guarded on [`Self::multi_account`], which is `false` for every single-account process** —
    /// so this is one already-computed bool read on the measured p99 mark drain and the fold is
    /// byte-identical there. It writes through the SAME `Account::set_mark_from` the routed write
    /// used, so the mark-source precedence law is applied once per engine rather than bypassed.
    ///
    /// `board` mirrors the ROUTED write's own board behaviour and must not be inferred from
    /// `source`: the bar-close and venue-mark lanes file a board slot (`Some(ts)`), while the
    /// TRADE-TICK lane writes the account mark only and leaves the board's trade/quote slots to
    /// their own producers — filing a sub-bar print as a venue mark there would corrupt the price
    /// resolver's source tagging on the mirrored engine while the routed one stayed correct.
    fn mirror_venue_price(
        &mut self,
        venue: &str,
        symbol: &str,
        px: f64,
        source: MarkSource,
        now: i64,
        board: Option<i64>,
    ) {
        if !self.multi_account {
            return;
        }
        let primary = self.engine_idx_for_route_key(RouteKey::sole_account_of(venue)).unwrap_or(0);
        for i in 0..=self.extra_engines.len() {
            if i == primary || self.eng(i).venue != venue {
                continue;
            }
            let eng = if i == 0 { &mut self.engine } else { &mut self.extra_engines[i - 1].1 };
            eng.account.set_mark_from(venue, symbol, px, source, now);
            if let Some(ts) = board {
                match source {
                    MarkSource::BarClose => eng.price_board.set_bar_close(venue, symbol, px, ts),
                    _ => eng.price_board.set_mark(venue, symbol, px, ts),
                }
            }
        }
    }

    /// Read access to an engine by routing index (0 = primary).
    fn eng(&self, idx: usize) -> &ExecutionEngine<C> {
        if idx == 0 { &self.engine } else { &self.extra_engines[idx - 1].1 }
    }

    /// Mutable twin of [`Self::eng`].
    fn eng_mut(&mut self, idx: usize) -> &mut ExecutionEngine<C> {
        if idx == 0 { &mut self.engine } else { &mut self.extra_engines[idx - 1].1 }
    }

    /// The equity seed an engine folds against (primary = config.seed_cash).
    fn seed_of(&self, idx: usize) -> f64 {
        if idx == 0 { self.config.seed_cash } else { self.extra_engines[idx - 1].0 }
    }

    /// Publish one event to the engine at `idx` through the bus.
    fn publish_to(&mut self, idx: usize, ev: Event) {
        if idx == 0 {
            self.bus.publish(ev, &mut self.engine);
        } else {
            self.bus.publish(ev, &mut self.extra_engines[idx - 1].1);
        }
    }

    /// Route an inbound venue event: venue-tagged payloads by venue, order-lifecycle
    /// replies via the coid map, everything else (and every miss) to the primary —
    /// exactly today's behavior when no extra engines exist.
    ///
    /// The venue-tagged arms feed a venue string to
    /// [`Self::engine_idx_for_route_key`], which is correct while every engine's route key IS its
    /// canonical venue. The coid arm below is the shape that already survives a second account
    /// without a wire change: `coid_venue` maps a client-order-id to a routing INDEX, resolved at
    /// SUBMIT time when the engine was unambiguous, so an order-lifecycle reply needs nothing on
    /// the payload to find its engine.
    ///
    /// A venue-tagged payload reaches that coid map only through the two-account branch below,
    /// which closes the gap with the THREE answers a payload can carry, in order of exactness:
    ///
    /// 1. its own stamped route key ([`event_route_key`] — [`Event::AccountState`],
    ///    [`Event::Funding`] and [`Event::PositionLiquidated`]: every venue-tagged payload with no
    ///    CLIENT-ORDER-ID, which is that function's membership rule);
    /// 2. its CLIENT-ORDER-ID ([`event_coid`], which lists [`Event::Fill`] for exactly this reason),
    ///    resolved through `coid_venue` — exact for every order this process placed;
    /// 3. its symbol ([`event_symbol`]), and ONLY when exactly one engine of the venue claims it.
    ///
    /// ⚠ Step 1 said "[`Event::AccountState`] only, because it is the one such payload with
    /// neither a coid nor a symbol" until the commit that stamped the other two. Funding and
    /// liquidations DO carry a symbol — that was the whole basis for leaving them unstamped, and
    /// it is why they fell to step 3 and then, on a shared symbol, past it. Read
    /// [`event_route_key`]'s own doc for what that cost; the membership rule lives there, so this
    /// list must never restate it as a narrower one.
    ///
    /// ⚠ Step 3 used to be step 2 and was described as "exact by the mount's collision rule". That
    /// rule — no two active accounts of one venue armed on one symbol — is DELETED
    /// (`vike_config::venue_accounts`: two accounts on one instrument is an ordinary spread), so
    /// the symbol became the LAST resort and [`Self::engine_idx_for_venue_symbol`] answers `None`
    /// on ambiguity rather than taking the first match. That function's own doc carries the one
    /// residual this leaves (a FOREIGN fill on a shared symbol).
    fn route_event(&self, ev: &Event) -> usize {
        let venue = match ev {
            Event::Fill(f) => Some(f.venue.as_str()),
            Event::AccountState(a) => Some(a.venue.as_str()),
            Event::Funding(f) => Some(f.venue.as_str()),
            Event::PositionLiquidated(p) => Some(p.venue.as_str()),
            _ => None,
        };
        if let Some(v) = venue {
            // ⚠ TWO ACCOUNTS OF ONE EXCHANGE: the venue lookup above resolves the FIRST engine
            // whose route key matches, and a second account's route key is `venue#LABEL`, so every
            // venue-tagged payload of BOTH accounts would fold into the default account's book —
            // the exact defect `ExecutionEngine::route_key`'s own doc names as the reason the field
            // exists. The wire cannot help: a `FillEvent` labelled `"binance"` says nothing about
            // WHICH binance account it belongs to.
            //
            // ⚠ THAT RULE WAS RELAXED, and this comment used to say what to do about it: the
            // symbol was an exact account key only because `vike_config` refused to arm two ACTIVE
            // accounts of one venue on one symbol, and it was written that "if that rule is ever
            // relaxed, this branch stops being sound". The rule IS relaxed — two accounts on one
            // instrument is an ordinary spread — so the ORDER below changed and the symbol became
            // the last resort rather than the answer:
            //
            //   1. the payload's own stamped route key — `AccountState`, `Funding` and
            //      `PositionLiquidated`, i.e. EVERY payload with no coid, all stamped by the mount;
            //   2. its CLIENT-ORDER-ID, resolved through `coid_venue` — recorded at SUBMIT, when
            //      the engine was unambiguous. `Event::Fill` carries one, and this is exact with
            //      nothing added to any wire, which is why it is preferred over the symbol rather
            //      than added beside it;
            //   3. the symbol, and only when EXACTLY ONE engine of the venue claims it
            //      (`engine_idx_for_venue_symbol` now answers `None` on ambiguity).
            //
            // Guarded by [`Self::multi_account`], so a single-account process takes the same
            // `return` it always took, having paid one predictable compare.
            if self.multi_account {
                // THE PAYLOAD'S OWN ANSWER, consulted first because it is the only EXACT one that
                // needs no rule held elsewhere to stay sound. `AccountState` is the one venue-tagged
                // payload with no symbol — an account-wide balance snapshot — so before this it fell
                // through to the venue lookup below and a second account's balances folded into the
                // FIRST account's book while its fills and positions routed correctly.
                //
                // The key is stamped by the MOUNT (`vike_mount::account_event_sender`), not by a
                // bridge: a venue adapter holds one credential set and cannot name an account. A
                // key that names no mounted engine falls through to the symbol/venue lookup rather
                // than being dropped here — the same unknown-key affordance
                // `engine_idx_for_route_key` has everywhere else.
                //
                // ⚠ It covers `Funding` and `PositionLiquidated` as well as `AccountState`, and
                // that is the whole of what makes step 3 a genuine last resort rather than the
                // ONLY answer for two of the three coid-less payloads. See `event_route_key`.
                if let Some(key) = event_route_key(ev)
                    && let Some(i) = self.engine_idx_for_route_key(RouteKey::declared(key))
                {
                    return i;
                }
                // THE ORDER'S OWN ANSWER. `coid_venue` maps a client-order-id to a routing INDEX,
                // recorded at SUBMIT time when the engine was unambiguous, so a venue-tagged
                // payload naming one of OUR orders needs nothing on the wire to find its engine.
                // `Event::Fill` is the payload that carries a coid, and it is the one whose
                // misrouting moves a position into the wrong book.
                if let Some(coid) = event_coid(ev)
                    && let Some(&i) = self.coid_venue.get(coid)
                {
                    return i;
                }
                if let Some(symbol) = event_symbol(ev)
                    && let Some(i) = self.engine_idx_for_venue_symbol(v, symbol)
                {
                    return i;
                }
            }
            return self.engine_idx_for_route_key(RouteKey::sole_account_of(v)).unwrap_or(0);
        }
        if let Some(coid) = event_coid(ev)
            && let Some(&i) = self.coid_venue.get(coid)
        {
            return i;
        }
        0
    }

    fn drain_market(&mut self) {
        let (bar_close_ticks, ticks, forming) = {
            let mut st = self.market.state.lock().unwrap();
            st.marker_in_flight = false;
            (
                st.bar_close_slots.drain(..).map(|(_, t)| t).collect::<Vec<_>>(),
                st.slots.drain(..).map(|(_, t)| t).collect::<Vec<_>>(),
                st.forming.drain(..).collect::<Vec<_>>(),
            )
        };
        // ONE PRICE CONCEPT IN THE ACCOUNT MARK SLOT. The `PriceBoard` keeps a candle close and a
        // venue mark in DIFFERENT slots, so the resolver sees each under its true source — but the
        // account slot is a single untagged scalar the pre-trade gate, the margin-call law and
        // `LiveBroker.price` all read, so it must not alternate between concepts at whatever
        // cadence the feeds interleave. The precedence law lives INSIDE `Account::set_mark_from`
        // (see `MarkSource`) — not here — precisely so no lane can route around it; this arm just
        // names the concept it carries. Board slots are unconditional: they are source-tagged.
        let now = self.engine.now_ms;
        for t in bar_close_ticks {
            let eng = match self.engine_idx_for_route_key(RouteKey::sole_account_of(&t.venue)) {
                Some(i) if i > 0 => &mut self.extra_engines[i - 1].1,
                _ => &mut self.engine,
            };
            eng.account.set_mark_from(&t.venue, &t.symbol, t.px, MarkSource::BarClose, now);
            eng.price_board.set_bar_close(&t.venue, &t.symbol, t.px, t.ts);
            self.mirror_venue_price(
                &t.venue,
                &t.symbol,
                t.px,
                MarkSource::BarClose,
                now,
                Some(t.ts),
            );
            self.dirty = true;
        }
        for t in ticks {
            {
                let eng = match self.engine_idx_for_route_key(RouteKey::sole_account_of(&t.venue)) {
                    Some(i) if i > 0 => &mut self.extra_engines[i - 1].1,
                    _ => &mut self.engine,
                };
                eng.account.set_mark_from(&t.venue, &t.symbol, t.px, MarkSource::VenueMark, now);
                eng.price_board.set_mark(&t.venue, &t.symbol, t.px, t.ts);
            }
            self.mirror_venue_price(
                &t.venue,
                &t.symbol,
                t.px,
                MarkSource::VenueMark,
                now,
                Some(t.ts),
            );
            self.dirty = true;
            // Cross-symbol underlying-mark routing ("Option B"): a drained venue mark is ALSO the
            // underlying-spot feed for any maker mounted on a DIFFERENT (e.g. PM token) symbol that
            // declares THIS (venue, symbol) as its `underlying_symbol`. Early-returns inside when no
            // mount does, so a run with no underlying-anchored maker never leaves the mark write above.
            self.drive_strategy_mark(&t.venue, &t.symbol, t.px, t.ts);
            // Mark-lane conditional triggering (w2 `trigger_by`): a `Some(Mark)` arm evaluates
            // ONLY here, off the mark tick — the one lane that reproduces a mark-triggering
            // venue's (hyperliquid) SL/TP timing. Guarded by `has_mark_arms` inside, so a book
            // with no Mark arms costs one integer compare on this hot mark drain.
            self.fire_conditionals_at_mark(&t.venue, &t.symbol, t.px, t.ts);
        }
        for (key, bar) in forming {
            let series = self.bars.entry(key).or_default();
            // ignore a forming update older than the last close (reconnect race)
            if series.closed.last().is_none_or(|last| bar.ts > last.ts) {
                series.forming = Some(bar);
                self.dirty = true;
            }
        }
    }

    /// Pump venue events the in-process client synthesized (paper/test clients — `PaperExecution
    /// Client`, `TestExecutionClient`); real venue clients return `None` here, their events arrive
    /// over the ingest channel and are journaled at [`dispatch`](Self::dispatch)'s write-ahead site.
    ///
    /// SINK-ENABLEMENT: a client-synthesized event bypasses the ingest lane, so it would never hit
    /// that write-ahead site — historically the reason a *paper* session's fill stream was absent
    /// from the journal (documented in `tests/replay_fence.rs`'s module doc: a synthesizing client
    /// emits Submitted/Accepted/Fill/Filled INSIDE the submit dispatch, which "never reach the
    /// journal — replay could not reproduce them"). We journal each polled event HERE as the SAME
    /// `Ingest::Event` record a real venue event rides, so a paper session's fills are durable (the
    /// live tearsheet reads them back via `CommandJournal::read_all`) AND replay-reproducible. This
    /// runs on the cold bar-close / event-arm path — never the p99 event fold — and every hook is a
    /// single `Option::is_some` no-op when journaling is off, so the default path stays byte-identical.
    fn pump_client(&mut self) {
        // One wall-clock stamp for the whole drain: these synthesized events are a CONSEQUENCE of the
        // message dispatch that reached `pump_client`, so they share its `now_ms` (the same
        // "one now per message" discipline `dispatch` applies), not a fresh clock read per event.
        let now_ms = self.engine.now_ms;
        while let Some(ev) = self.engine.client.poll_events() {
            let ev = self.journal_pumped_event(now_ms, ev);
            // Live-runtime OTO/OCO: client-SYNTHESIZED fills (the paper exchange, and any in-process
            // client that emits via `poll_events`) surface HERE, not on the `Ingest::Event` lane — so
            // the contingency drive must run here too or a bracket never resolves on the paper/
            // backtest path. Classify BEFORE the bus consumes `ev`; drive AFTER it folds. The drive's
            // own submits/cancels queue further client events that THIS loop then drains (the drive
            // itself never pumps — see its doc). Gated on a non-empty book (byte-identical otherwise).
            //
            // ⚠ DELIBERATELY NOT GATED on the fold verdict, unlike the `Ingest::Event` venue lane.
            // `poll_events` is the IN-PROCESS client seam (the paper exchange and test clients);
            // real venue clients return `None` here, so nothing an attacker controls arrives on this
            // path and the hardening buys nothing. It would cost something, though:
            // `crates/vike-backtest/tests/r7_gate.rs` pins backtest == paper BIT-FOR-BIT, and this
            // is the paper side of that equality.
            let drive = self.contingency_terminal(&ev);
            self.bus.publish(ev, &mut self.engine);
            match drive {
                Some((coid, true)) => self.drive_contingency_on_fill(&coid, now_ms),
                Some((coid, false)) => self.drive_contingency_on_terminal(&coid),
                None => {}
            }
            self.maybe_cadence_snap(now_ms);
        }
        for i in 0..self.extra_engines.len() {
            while let Some(ev) = self.extra_engines[i].1.client.poll_events() {
                let ev = self.journal_pumped_event(now_ms, ev);
                let drive = self.contingency_terminal(&ev);
                self.bus.publish(ev, &mut self.extra_engines[i].1);
                match drive {
                    Some((coid, true)) => self.drive_contingency_on_fill(&coid, now_ms),
                    Some((coid, false)) => self.drive_contingency_on_terminal(&coid),
                    None => {}
                }
                self.maybe_cadence_snap(now_ms);
            }
        }
    }

    /// Classify a folded event for the live-runtime OTO/OCO drive: `Some((coid, true))` for a FULL
    /// fill (arm held children + cancel OCO siblings), `Some((coid, false))` for a terminal-WITHOUT-
    /// fill (cascade-drop held children / clean the dead leg), `None` for everything else. Gated on a
    /// non-empty book so the no-bracket path is a single `is_empty` read. Shared by `pump_client`
    /// (client-synthesized events), the `Ingest::Event` arm (real venue events), AND the synchronous
    /// submit outboxes (`publish_and_drive_outbox`) — the ONE classifier every fold site consults so
    /// they cannot drift. `OrderDenied` (a RiskGate veto / Halted, emitted SYNCHRONOUSLY by
    /// `submit_order`) is a terminal-without-fill here: a denied bracket entry must cascade-drop its
    /// held children, never orphan them.
    fn contingency_terminal(&self, ev: &Event) -> Option<(String, bool)> {
        if self.contingency.is_empty() {
            return None;
        }
        match ev {
            Event::OrderFilled(e) => Some((e.client_order_id.clone(), true)),
            Event::OrderCanceled(e) => Some((e.client_order_id.clone(), false)),
            Event::OrderRejected(e) => Some((e.client_order_id.clone(), false)),
            Event::OrderDenied(e) => Some((e.client_order_id.clone(), false)),
            Event::OrderExpired(e) => Some((e.client_order_id.clone(), false)),
            _ => None,
        }
    }

    /// Publish a SYNCHRONOUS submit outbox to engine `idx` and drive the contingency book for any
    /// terminal in it — the submit-site twin of the `Ingest::Event`/`pump_client` drive. A synchronous
    /// `submit_order`/`submit_order_batch` can emit `OrderDenied` (RiskGate veto / Halted) or a
    /// capability `OrderRejected` right here, drained by a bare `publish_to`; without driving it, a
    /// DENIED bracket entry would leave its held OTO children orphaned forever (never armed, never
    /// removed, re-Snapped every checkpoint). Publish FIRST (the engine folds the terminal), THEN
    /// drive — the same order the other two fold sites use. Byte-identical no-bracket path (the
    /// classifier early-returns on an empty book).
    ///
    /// ⚠ DELIBERATELY NOT GATED ON THE FOLD VERDICT, unlike the `Ingest::Event` venue lane. This is
    /// not an oversight to be tidied up: `gate_and_register` publishes `OrderDenied` for an order it
    /// deliberately never REGISTERED, so that event folds `Fold::Dropped` BY DESIGN (it lands on
    /// `on_event`'s unknown-coid branch and even moves `dropped_unknown_coid`). Gating here would
    /// therefore suppress exactly the cascade-drop this function exists to perform, orphaning a
    /// denied bracket entry's held children forever. These events are engine-synthesized and
    /// unreachable by a venue — `Event` is never deserialized from a venue payload — so the hostile
    /// -venue argument that gates the other lane does not apply.
    fn publish_and_drive_outbox(&mut self, idx: usize, mut outbox: Outbox, now: i64) {
        while let Some(ev) = outbox.0.pop_front() {
            let drive = self.contingency_terminal(&ev);
            self.publish_to(idx, ev);
            match drive {
                Some((coid, true)) => self.drive_contingency_on_fill(&coid, now),
                Some((coid, false)) => self.drive_contingency_on_terminal(&coid),
                None => {}
            }
        }
    }

    /// Write-ahead-journal a client-synthesized venue event as the `Ingest::Event` record a real
    /// venue event rides. Moves `ev` through a temporary `Ingest` wrapper so the append borrows it
    /// (no clone) and hands it straight back for the fold. No-op passthrough when journaling is off.
    fn journal_pumped_event(&mut self, now_ms: i64, ev: Event) -> Event {
        let Some(j) = self.journal.as_mut() else { return ev };
        let ingest = Ingest::Event(ev);
        j.append_cmd(now_ms, &ingest).expect("journal append");
        self.journaled_since_snap += 1;
        let Ingest::Event(ev) = ingest else { unreachable!("wrapped an Event just above") };
        ev
    }

    /// Mint the next emulated-conditional `arm_id`, `{coid_session}a{arm_seq}`. The coid-session
    /// prefix separates two concurrently-running cores; the suffix is the crate-local
    /// [`Self::arm_seq`] counter, NOT a coid — see that field for why.
    ///
    /// Uniqueness across a RESTART (which deliberately restores the same `coid_session`, and
    /// appends to the same journal directory) rests on `arm_seq` being resumed too, via
    /// [`CoreConfig::arm_seq`]/[`crate::replay::RestoredState::arm_seq`]. A caller that restores
    /// the coid session but leaves `arm_seq` at 0 WILL re-emit pre-restart ids — and `arm_id` IS
    /// the disarm key (emulator PR-2: [`vike_exec::OrderIntent::DisarmConditional`] targets the
    /// arm-id-keyed [`crate::emulator::ConditionalBook`]), so an ambiguous id would disarm the
    /// wrong arm. Restore both or neither.
    fn mint_arm_id(&mut self) -> String {
        let n = self.arm_seq;
        self.arm_seq += 1;
        format!("{}a{}", self.coid_gen.state().0, n)
    }

    /// Mint the next REFUSAL id, `{coid_session}r{refusal_seq}` — the handle an intent the drain
    /// refused is denied under. The [`Self::mint_arm_id`] idiom applied to a second non-order id
    /// sequence: session prefix (so two concurrently-running cores never collide) + its OWN
    /// crate-local counter ([`CoreThread::refusal_seq`]), never the coid generator.
    ///
    /// The separation is load-bearing, not cosmetic. A refused intent journals nothing and submits
    /// nothing, so a coid drawn here would be spent with no record of it existing: every later
    /// order's coid would run one ahead of the `coid_seq` a `Snap` records, and a restart resuming
    /// that journal would re-mint an already-used id. The `a`/`r` infixes also keep all three id
    /// spaces mutually unambiguous — a real coid is `{session}{digits}`.
    fn mint_refusal_id(&mut self) -> String {
        let n = self.refusal_seq;
        self.refusal_seq += 1;
        format!("{}r{}", self.coid_gen.state().0, n)
    }

    /// Journal one ARM's RESOLVED terms right after [`Self::mint_arm_id`] named it — the
    /// `append_minted_submit` precedent applied to `OrderIntent::ArmConditional` (the write-ahead
    /// `Cmd`/`StrategySubmit` for that intent cannot carry a TRAILING arm's extreme, which is
    /// seeded from a non-journaled mark). Counts against the same `journaled_since_snap` cadence
    /// as every other append — so an arming session reaches `snapshot_every` marginally sooner
    /// (one extra record per arm shifts the `Snap` cadence; arms are operator/strategy cadence, so
    /// this is a rounding effect on checkpoint spacing, never a write-amplification concern). NO-OP
    /// when journaling is off, so the desktop/no-journal path stays byte-identical; arming is
    /// order-cadence, never the per-market-message fold.
    fn journal_conditional_armed(
        &mut self,
        now_ms: i64,
        arm_id: &str,
        resolved: crate::journal::ConditionalRecord,
    ) {
        if let Some(j) = self.journal.as_mut() {
            j.append_conditional_armed(now_ms, arm_id, &resolved).expect("journal append");
            self.journaled_since_snap += 1;
        }
    }

    /// Journal one conditional's DISARM, write-ahead of the book mutation (the
    /// [`Self::journal_conditional_armed`] twin, emulator PR-2). Called ONLY for an actual disarm
    /// — an unknown-id refusal writes nothing. NO-OP when journaling is off; disarming is
    /// command cadence, never the per-market-message fold.
    fn journal_conditional_disarmed(&mut self, now_ms: i64, arm_id: &str) {
        if let Some(j) = self.journal.as_mut() {
            j.append_conditional_disarmed(now_ms, arm_id).expect("journal append");
            self.journaled_since_snap += 1;
        }
    }

    /// Cadence-snapshot check, shared with [`dispatch`](Self::dispatch)'s tail via the same
    /// `journaled_since_snap` counter: fire a full-state `Snap` once the write-ahead counter reaches
    /// `snapshot_every`. `pump_client` needs its own copy because its journaled events land AFTER
    /// `dispatch`'s tail check has already run (and, on the bar-close path, `dispatch`'s tail check
    /// is skipped entirely — that message is not exec-lane) — so without this, a paper-only workload
    /// would never checkpoint until shutdown. No-op when journaling is off.
    fn maybe_cadence_snap(&mut self, now_ms: i64) {
        if self.journal.is_some()
            && self.journaled_since_snap >= self.config.journal.as_ref().unwrap().snapshot_every
        {
            self.write_snap(now_ms);
        }
    }

    /// Push `msg` onto the bounded recent-events ring, trimming to `recent_events_cap`. Cold path
    /// only (drift/margin/drawdown sweeps + coalesced-publish drains) — never the per-message hot
    /// fold, so no hot-path logging budget applies.
    fn note(&mut self, msg: String) {
        self.push_line(msg.into());
    }

    /// The per-message twin of [`Self::note`]: push a DELIVERED event's rendered LINE onto the ring.
    ///
    /// ## Why this renders HERE, on the fold path (2026-07-29)
    ///
    /// #887 deferred rendering to publish, on the premise that "the ring is only ever READ at
    /// publish, which is coalesced to `snapshot_interval` (>= 16 ms), so rendering is pure waste on
    /// the fold thread". **That premise is false.** The runtime ALSO publishes whenever the core is
    /// about to go idle (`if self.dirty`, immediately before `blocking_recv`), so a core whose
    /// events arrive sporadically publishes PER EVENT. Deferring therefore never avoided the
    /// format — it relocated it, and added a capture plus per-publish `Arc` traffic on top of it.
    ///
    /// MEASURED on the latency box (shielded cores, SCHED_FIFO 50, `--test-threads=1`, 8 interleaved reps):
    /// rendering here instead of at publish takes the baseline core-hop p99 from **2,024 ns to
    /// 802 ns**, and p50 from 321 ns to 296 ns. [`crate::recent`] keeps the byte-identical
    /// rendering contract either way.
    pub(crate) fn note_event(&mut self, note: crate::recent::EventNote) {
        self.push_line(crate::recent::RecentNote::Event(note).render().into());
    }

    /// The salvaged-from-a-mid-fold-panic twin of [`Self::note_event`] (audit C4).
    pub(crate) fn note_lost(&mut self, note: crate::recent::EventNote) {
        self.push_line(crate::recent::RecentNote::Lost(note).render().into());
    }

    /// THE one ring writer: append, then trim to `recent_events_cap`. The ring holds RENDERED lines
    /// as `Arc<str>`, so publish clones a refcount per entry and never mutates the ring.
    fn push_line(&mut self, line: std::sync::Arc<str>) {
        self.recent.push_back(line);
        while self.recent.len() > self.config.recent_events_cap {
            self.recent.pop_front();
        }
    }

    /// Fold-thread half of the reconcile runtime driver (Option A: reports-in, compute-on-fold).
    /// The reconcile MANAGER thread (`vike_core::recon_manager`) did the blocking REST fetch OFF
    /// this thread and enqueued the raw reports as [`Command::ReconcileReports`]; HERE, on the
    /// single writer, we read THIS engine's own [`ExecutionEngine::local_view`] (cheap, same-thread
    /// — so no cross-thread local-state read and no staleness window), run the PURE
    /// [`vike_exec::recon::diff`] + [`vike_exec::recon::resolve`], and fold each synthesized event
    /// through the SAME [`Self::publish_to`] path real venue events use. An OCCASIONAL command
    /// (startup, plus Task 16's interval cadence), never the per-message hot fold, so the pure
    /// diff/resolve compute here is within budget — the identical reasoning that keeps
    /// `diff_snapshot` on this thread at the `ApplySnapshot` choke point.
    ///
    /// `recon.snapshot` is the `resolve` default (empty) for positions/orders: `resolve` seeds
    /// position state via the synthesized FILL events, not the snapshot, so applying THOSE fields
    /// would be a no-op at best (`apply_snapshot` skips empty positions) and DESTRUCTIVE at worst —
    /// `ExecutionEngine::apply_snapshot`'s order-reap terminalizes any live local order absent from
    /// `snapshot.open_orders` (see `crates/vike-exec/tests/recon/reconcile_reap.rs`), so routing an
    /// all-empty-except-balance snapshot through the full `Command::ApplySnapshot` arm would cancel
    /// EVERY live order on this venue as a side effect. Task 3 (balance activation) therefore does
    /// NOT reuse that arm: `reports.balance` (from [`vike_exec::recon::ReconClient::fetch_balance`])
    /// is seeded directly onto `Account::balance`/`balance_mode` below — the narrowest path that
    /// updates cash without touching positions or the order registry. `recon.snapshot.balance` is
    /// still set (mirrors the `Recon` shape / a future `CoreSnapshot.recon` surface) but is NOT
    /// itself applied.
    ///
    /// FORWARD-NOTE (Task 7): a canceled-with-zero-fills order that has already dropped out of the
    /// venue `openOrders` set produces NO report row, so this pass canNOT observe every terminal
    /// transition — the journal cross-check (Task 14) is the closer, not this.
    ///
    /// Task 17: `recon.events` (Synthesize / auto-Hybrid kinds) still fold immediately, byte-
    /// identical to before. `recon.alerts` (Quarantine / Hybrid-quarantined kinds) are no longer
    /// just a ring note — each is assigned a monotonic id, held in `self.recon_alerts` (its
    /// `proposed_events` kept for `Self::confirm_recon` to fold later), and surfaced structurally
    /// via `CoreSnapshot.recon.alerts` (built by `Self::recon_block` at the next publish). The
    /// ring note is kept too (cheap, and useful chronological context in `recent_events`).
    /// Alerts carrying a `dedup_key` (a by-design recurring divergence, e.g. an UnknownOrder under
    /// `generate_missing_orders`) refresh the matching held row in place instead of appending —
    /// see the alert loop below.
    ///
    /// ⚠ **Routing here reads [`ReconcileReports::route`], never `reports.venue`.** They are the
    /// same string for every pass this tree produces, which is exactly why the distinction has to
    /// be made by the TYPE rather than by reading: this one resolution decides which local view a
    /// pass diffs against, which book its synthesized fills fold into, and — through the
    /// `route_key` stored on every held alert below — which book an operator confirm folds into
    /// later. `venue` keeps every job that is genuinely about the exchange: the ring notes and log
    /// lines an operator reads, the `(venue, symbol)` coin-delta keys (whose venue comes off the
    /// position REPORT, minted canonically by an adapter), and the dedup identity of a held row.
    fn reconcile_reports(&mut self, reports: ReconcileReports) {
        // Resolve the target ENGINE first, off the payload's routing key, while the payload is
        // still whole — destructuring it below moves `venue` out and would leave `route()`
        // unreachable.
        let route = self.engine_idx_for_route_key(reports.route());
        let ReconcileReports {
            venue,
            since: _,
            orders,
            fills,
            positions,
            policy,
            balance,
            generate_missing_orders,
            reconcile_balance,
            balance_tol,
            route_key,
        } = reports;
        let Some(idx) = route else {
            self.note(format!("RECON skipped: no engine for venue {venue}"));
            return;
        };
        // Wave 5d: capture any venue-REPORTED per-position coin delta (Deribit `get_positions.delta`)
        // into the side map the snapshot publishes, so the greeks tool can fold a perp/future hedge
        // leg via `coin_delta × spot`. Only `Some(delta)` rows upsert; a venue that reports no delta
        // (every venue but Deribit today) leaves the map untouched and byte-identical.
        for pr in &positions {
            if let Some(d) = pr.delta {
                self.recon_coin_deltas.insert((pr.venue.clone(), pr.symbol.clone()), d);
            }
        }
        // local_view returns OWNED data — the &self borrow is released before the &mut publish.
        let local = self.eng(idx).local_view();
        // The engine's quote-asset selector for the synthesized/adopted balance (Feature 2). Read
        // before the &mut publish below; irrelevant when `reconcile_balance` is off.
        let quote_asset = self.eng(idx).quote_asset.clone();
        // Journal cross-check (#3): build the venue's JournalView from the materialized Tier-2 log
        // (via the app-supplied provider) and pass `Some(&journal)` to enable the three-way check.
        // `None` (provider unset — e.g. no store / VIKE_RECONCILE off) keeps this pass byte-identical
        // to the two-way (local-vs-venue) comparison. A bounded lookback query on this OCCASIONAL
        // path, not the hot fold.
        let journal = self.config.journal_view_provider.as_ref().map(|p| p(&venue));
        let local_view = local.as_view();
        let mut divergences =
            vike_exec::recon::diff(&orders, &fills, &positions, &local_view, journal.as_ref());
        // Feature 2 (`VIKE_RECONCILE_BALANCE`): promote balance to a first-class DIFFED dimension.
        // First-observation (Delta / never-synced) OR within-tolerance ⇒ `diff_balance` returns
        // `None` and we ADOPT (silent seed) below — keeping equity correct exactly like the legacy
        // path. A drift BEYOND tolerance ⇒ a `BalanceDrift` divergence that flows through the SAME
        // `resolve`/policy machinery as every other kind (quarantined by default: a surprise cash
        // move is surfaced for operator confirm, never auto-absorbed). Off (`false`, the default)
        // is byte-identical: the legacy silent seed runs after `resolve`, `diff_balance` unreached.
        let mut balance_adopt: Option<f64> = None;
        if reconcile_balance && let Some(b) = balance {
            match vike_exec::recon::diff_balance(
                &local_view,
                Some(b),
                &quote_asset,
                balance_tol,
                self.engine.now_ms,
            ) {
                Some(d) => divergences.push(d),
                None => balance_adopt = Some(b),
            }
        }
        // `generate_missing_orders` rode in on THIS pass's `ReconcileReports` — the fold thread has
        // no other view of `vike_core::ReconConfig` (see that struct's own doc). It becomes
        // `resolve`'s AdoptContext, built from the same pass's fill reports + the engine's seen
        // trade ids (mirrors `vike_exec::recon::run_pass`, the offline twin of this composition).
        let fill_order_ids: std::collections::HashSet<String> =
            fills.iter().map(|f| f.venue_order_id.to_string()).collect();
        let adopt = generate_missing_orders.then_some(vike_exec::recon::AdoptContext {
            pass_fill_order_ids: &fill_order_ids,
            seen_trade_ids: local_view.seen_trade_ids,
        });
        let mut recon = vike_exec::recon::resolve(divergences, &policy, None, adopt);
        // Balance application. Two mutually-exclusive paths, gated by `reconcile_balance`:
        //
        // - Feature 2 ON: any BEYOND-tolerance drift already resolved into `recon.events`
        //   (synthesize) or `recon.alerts` (quarantine/hybrid) above. Here we only apply the
        //   first-observation/within-tolerance ADOPT (`balance_adopt`) — the same authoritative
        //   seed as the legacy path, PLUS a snapshot of the realized-PnL baseline so subsequent
        //   passes DIFF against venue truth instead of re-adopting (and so `equity_all` keeps
        //   folding realized PnL in via the reseed, exactly as the legacy every-pass seed did).
        //
        // - Feature 2 OFF (default): the LEGACY silent authoritative seed, byte-identical to before
        //   this feature — NOT via `Command::ApplySnapshot` (that path's order-reap would
        //   terminalize every live order given an otherwise-empty snapshot; see this fn's doc).
        //
        // `None` balance (not reported / fetch failed) leaves balance/mode untouched in both.
        if reconcile_balance {
            if let Some(b) = balance {
                recon.snapshot.balance = b;
            }
            if let Some(b) = balance_adopt {
                let acct = &mut self.eng_mut(idx).account;
                acct.balance = b;
                acct.balance_mode = BalanceMode::Authoritative;
                acct.realized_pnl_at_balance_sync = Some(acct.realized_pnl);
                self.note(format!("RECON balance {venue}: adopted authoritative {b}"));
            }
        } else if let Some(b) = balance {
            recon.snapshot.balance = b;
            let acct = &mut self.eng_mut(idx).account;
            acct.balance = b;
            acct.balance_mode = BalanceMode::Authoritative;
            self.note(format!("RECON balance {venue}: seeded authoritative {b}"));
        }
        let n_events = recon.events.len();
        for ev in recon.events {
            self.publish_to(idx, ev);
        }
        // Mirror the Ingest::Event path: drain any events an in-process client synthesized from
        // the folded fills (real venue clients return None here).
        self.pump_client();
        let n_alerts = recon.alerts.len();
        // ⚠ A HELD divergence is a STATE, not an EVENT. Every alert below used to emit its own
        // `tracing::warn!` on EVERY pass, and under `quarantine` (what the the CI box daemon runs) a
        // divergence nothing heals is re-diffed and re-held every interval forever: 396 identical
        // WARN lines on 2026-08-25 and 345 on 2026-08-24, all one bybit `PositionOnlyExternal`.
        // So what is logged is now the TRANSITIONS of this venue's held set — the identity choice,
        // the summary cadence and the argument for both live in `crate::runtime::recon_held`.
        // Nothing about what folds, what holds, which alerts are STORED, their confirm ids or the
        // published snapshot changes here; only which of them reach the log.
        let raised: std::collections::BTreeSet<recon_held::HeldId> = recon
            .alerts
            .iter()
            .map(|a| {
                recon_held::HeldId::new(
                    &venue,
                    a.kind,
                    a.dedup_key.as_deref(),
                    a.identity_detail.as_deref().unwrap_or(&a.detail),
                    &a.proposed_events,
                )
            })
            .collect();
        let transitions = self.recon_announce.observe(&venue, self.engine.now_ms, raised);
        for id in &transitions.cleared {
            recon_held::info_cleared(id);
        }
        if let Some(summary) = &transitions.summary {
            recon_held::warn_summary(&venue, summary);
        }
        // The routing decision this pass made, in the owned form a held alert has to keep so an
        // operator confirm (minutes later, `Self::confirm_recon`) folds into the SAME engine. `None`
        // — every producer in this tree, and every pre-field journal — is "this venue's sole
        // account", so it stores the venue and the confirm resolves exactly what this pass did.
        let held_route_key = route_key.unwrap_or_else(|| venue.clone());
        for a in recon.alerts {
            // This alert's ANNOUNCEMENT identity (see the block above). Computed before `a` is
            // consumed, and with the SAME constructor `confirm_recon` uses, so a confirmed-then-
            // recurring divergence cannot be announced under a second key.
            let ident = recon_held::HeldId::new(
                &venue,
                a.kind,
                a.dedup_key.as_deref(),
                a.identity_detail.as_deref().unwrap_or(&a.detail),
                &a.proposed_events,
            );
            let announce = transitions.newly_held.contains(&ident);
            // ⚠ A recurring divergence REFRESHES its held row in place — same confirm id,
            // freshened payload — so a pass adds NO new alert row for something already held.
            // This used to apply only to alerts carrying a `dedup_key`; everything else appended a
            // row per pass with a fresh id, and nothing in this tree ever prunes the store, so a
            // quarantined divergence nothing heals grew it forever (see `HeldReconAlert::identity`
            // for the the CI box measurement).
            //
            // ⚠ SEMANTIC CHANGE, stated because it is not a refactor: collapsing N per-pass rows
            // into one means a single `Command::ConfirmRecon` now resolves what previously needed
            // N confirms. That is what an operator wants — the N rows described ONE divergence and
            // confirming one of them folded a stale snapshot of it — but it IS a change to the
            // confirm surface. A genuinely different divergence still gets its own row and its own
            // id; that is exactly what `recon_held::HeldId` is for.
            let existing =
                self.recon_alerts.iter().find(|(_, h)| h.identity == ident).map(|(&id, _)| id);
            if let Some(id) = existing {
                // A refreshed row is normally an ALREADY-announced identity, so this is silent. It
                // can only be `true` if the announcer forgot the identity (an operator confirm)
                // while the row survived — which is news by construction, and is announced against
                // the row's EXISTING confirm id rather than a fabricated one.
                if announce {
                    recon_held::warn_newly_held(id, &ident, &a.detail);
                }
                let held = self.recon_alerts.get_mut(&id).expect("found above");
                // ⚠ The payload is refreshed UNCONDITIONALLY, but the ring note is gated on the
                // operator-facing TEXT alone. `resolve`'s synthesized position legs bake the pass
                // clock into their `ts`/`trade_id`/`client_order_id`
                // (`vike_exec::recon::resolve`'s `synth_position_legs`), so `proposed_events`
                // differs on EVERY pass for an unchanged divergence — keeping it in the
                // note condition would move the per-minute repetition this batch is removing out
                // of the log and into the bounded recent-events ring, flushing real events out of
                // it. Refreshing the payload anyway is the point: a confirm must fold the LATEST
                // view of the divergence, not one from hours ago.
                let detail_changed = held.detail != a.detail;
                held.detail = a.detail;
                held.proposed_events = a.proposed_events;
                held.recover_orders = a.recover_orders;
                if detail_changed {
                    self.note(format!(
                        "RECON alert #{id} {:?} refreshed (awaiting confirm)",
                        a.kind
                    ));
                }
                continue;
            }
            let id = self.recon_next_alert_id;
            self.recon_next_alert_id += 1;
            self.note(format!("RECON alert #{id} {:?}: {} (awaiting confirm)", a.kind, a.detail));
            // ...and SAY SO where an operator can read it. `note` writes the in-memory ring, which
            // is reachable only through the control channel — and that channel is OFF on the
            // shipped daemon unless an operator turns it on. Measured on the live the CI box box
            // 2026-08-24: `reconcile pass folded venue=bybit events=0 alerts=2` every 60s for
            // hours, zero WARN lines, no way to learn WHICH two divergences were held. The count
            // reached the log and the content did not.
            //
            // ⚠ This is a per-BOUNDARY line, not a per-message one, so it does not violate the
            // hot-fold logging rule (`crates/vike-core/CLAUDE.md`): a reconcile pass runs on the
            // interval cadence, and a HELD divergence is a fault transition — exactly the class
            // that rule says to instrument. Raising is `warn` because it needs an operator.
            //
            // ⚠ ...but ONLY on the pass the divergence ENTERS the held set. The line above used to
            // fire unconditionally, which under `quarantine` meant every 60s forever for a
            // divergence nothing heals — measured on the CI box, 396 lines in a day for ONE bybit
            // `PositionOnlyExternal`. A repetition is not a transition, and burying the NEW
            // divergence under 396 copies of an old one is the opposite of instrumenting a fault.
            // `recon_held` owns that decision; the backlog stays visible through its periodic
            // summary, and a divergence going away is announced too.
            if announce {
                recon_held::warn_newly_held(id, &ident, &a.detail);
            }
            self.recon_alerts.insert(
                id,
                HeldReconAlert {
                    venue: venue.clone(),
                    route_key: held_route_key.clone(),
                    kind: a.kind,
                    detail: a.detail,
                    proposed_events: a.proposed_events,
                    recover_orders: a.recover_orders,
                    identity: ident,
                },
            );
        }
        self.recon_last_pass_ts = self.engine.now_ms;
        // ⚠ A quiet PASS is a STATE too, and this line was the last per-pass repetition left after
        // the WARN flood above was fixed. It fired on every pass carrying ANY alert, so under
        // `quarantine` it repeated a minute apart forever — `events=0 alerts=1` measured identical
        // on the CI box at 2026-08-25 11:41:43 and 11:42:43 — saying only "the same divergence is still
        // held", which `recon_held`'s announcer now reports properly. INFO, so on a daemon running
        // `VIKE_LOG_FILE_LEVEL=warn` it floods the JOURNAL rather than the log file: the same
        // defect one severity down, which is why it was deferred out of that fix.
        //
        // What still logs EVERY time: a pass that FOLDED events (that changed the book — a fact
        // about this pass, not about the held set, which is why it is ORed in here rather than
        // asked of the announcer), and a pass whose held set CHANGED. What no longer logs: a pass
        // that merely repeated the previous one. The backlog stays visible because `silent()` is
        // also false on the announcer's periodic summary pass, so this line rides that ONE clock
        // instead of a second rate limiter of its own — see
        // `crate::runtime::recon_held`'s `HeldTransitions::silent`.
        //
        // ⚠ The verdict is over the held-set IDENTITIES, not over `n_alerts`: two alerts sharing
        // one identity are one divergence (that is what bounds the store), so a pass whose COUNT
        // moved while its identity set did not is a repetition and stays quiet. And a venue with
        // nothing held at all is silent exactly as it was before this change — a heartbeat where
        // there was never a line is new noise, and `CoreSnapshot.recon.last_pass_ts` (set on the
        // line above, published every pass) is the liveness signal that answers structurally.
        if n_events > 0 || !transitions.silent() {
            // Pass-boundary log only (not per-event) — within the cold-path logging budget.
            tracing::info!(
                target: "vike_core::reconcile",
                venue = %venue,
                events = n_events,
                alerts = n_alerts,
                "reconcile pass folded"
            );
        }
    }

    /// Fold-thread handling of `Command::ConfirmRecon` (Task 17): operator approval of one held
    /// Quarantine/Hybrid-quarantined alert. Looks up its `proposed_events` and folds each through
    /// the SAME [`Self::publish_to`] path real venue events (and a Synthesize-mode reconcile pass)
    /// use — so a confirmed alert folds byte-identically to what an immediate Synthesize would
    /// have produced — then removes it from the held store, so a re-confirm of the same id hits
    /// the unknown-id no-op below. An unknown id (never held, already confirmed, or a replayed
    /// command from a prior process run — see [`Command::ConfirmRecon`]'s doc) is surfaced to the
    /// recent-events ring rather than silently dropped or panicking.
    ///
    /// ⚠ **This is the RETURN leg of the reconcile round trip, and it routes on
    /// [`HeldReconAlert::route_key`] — the key the raising pass resolved — not on `held.venue`.**
    /// The two are equal for every alert this tree raises, so the wrong one would look right and
    /// stay right until a second account of one exchange existed, at which point an operator's
    /// approval would fold synthesized fills and position deltas into the FIRST account's book.
    /// `held.venue` keeps the operator-facing label in the notes below, which is what an operator
    /// asked about.
    fn confirm_recon(&mut self, id: u64) {
        let Some(held) = self.recon_alerts.shift_remove(&id) else {
            self.note(format!("RECON confirm: unknown alert id {id}"));
            return;
        };
        // Drop this divergence's ANNOUNCEMENT identity along with its row — the SAME key the row
        // was stored under, so the two can never fall out of step. If the confirm did not actually
        // resolve the divergence (the venue keeps reporting it), the next pass re-raises a fresh
        // row — and without this it would do so in SILENCE, leaving the operator to believe an
        // action took that did not. Logging-only; the fold below is untouched.
        self.recon_announce.forget(&held.identity);
        let Some(idx) = self.engine_idx_for_route_key(RouteKey::declared(&held.route_key)) else {
            // Venues are fixed at spawn, so this should not happen; surface it rather than
            // silently dropping the operator-approved events.
            self.note(format!(
                "RECON confirm #{id}: no engine for venue {} (approved events dropped)",
                held.venue
            ));
            return;
        };
        for ev in held.proposed_events {
            self.publish_to(idx, ev);
        }
        // Order-loss recovery: re-register any venue orders local state lost (recon JournalDivergence).
        // INSERT-ONLY seed straight into the routed engine's registry — NOT the event fold (a
        // lifecycle event for an unknown coid is dropped), so this cannot ride `proposed_events`.
        let reregistered = if held.recover_orders.is_empty() {
            0
        } else {
            self.eng_mut(idx).reregister_orders(&held.recover_orders)
        };
        // Mirror reconcile_reports: drain any events an in-process client synthesized from the
        // now-folded fills.
        self.pump_client();
        if reregistered > 0 {
            self.note(format!(
                "RECON confirmed #{id} {:?}: {} (re-registered {reregistered} lost order(s))",
                held.kind, held.detail
            ));
        } else {
            self.note(format!("RECON confirmed #{id} {:?}: {}", held.kind, held.detail));
        }
    }
}

fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic (non-string payload)".to_string()
    }
}

#[cfg(test)]
mod safe_state_tests;
