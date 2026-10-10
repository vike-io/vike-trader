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
//! GUI reads on repaint — the lossy-observer seam. The idle publish is NOT interval-gated, so on a
//! sporadic feed the build runs once per event and its cost is inside the gated core hop.
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
    Bar, BookLevel, BracketSpec, Broker, Clock, EquitySample, FeedStatus, Fill, FlowToxicity,
    HftBroker, LiveClock, MarkTick, OrderRequest, Strategy, amount_to_order, build_bracket,
    units_from_percent, units_from_value,
};

use crate::emulator::ConditionalBook;

use vike_exec::lanes::{Conflated, ConflatedState}; // doc-hidden: vike-core is the lane consumer
use vike_exec::price_board::Resolution;
use vike_exec::{
    BalanceMode, BarSeed, BarSender, BarSeries, BarUpdate, BookUpdate, CancelIntent, Command,
    ConditionalIntent, ContingencyBook, CoreSnapshot, EventBus, EventSender, ExecutionClient,
    ExecutionEngine, FlowUpdate, Fold, Ingest, MarginUpdate, MarkSource, MarketSender, MountBudget,
    MountView, OrderIntent, Outbox, ParamsUpdate, PriceCfg, QuoteUpdate, ReconcileReports,
    ReconcileSnapshot, RouteKey, SeriesKey, StreamStatusUpdate, TickSender, TradeUpdate,
    TradingState, recon::DivergenceKind,
};

use crate::schedule::LiveSchedule;
use crate::timer_wheel::{DeadlineTimerWheel, TimerId};

// The runtime is split into sibling modules, one role each: the configuration (`config`), the
// mount types (`mount_types`), the spawn/assemble pair, the `CoreThread` struct (`core_thread`) and
// its `impl` regions (`run_loop`, `dispatch`, `mount_runtime`, `routing`, `ingest`, `journaling`,
// `refusals`, `reconcile`, plus `apply`, `strategy_drive` and `watchdog`), and the non-fold sides
// (`broker`, `handle`, `publish`, ...). The fold's code was MOVED into them, not changed: every
// method keeps its body, and no per-event path gained a call. Re-exported so
// `runtime::{LiveBroker, CoreHandle, ReconcileDriver, ...}` and the crate-root `vike_core::*` paths
// are unchanged.
mod apply;
mod assemble;
mod broker;
mod config;
mod core_thread;
mod deadman;
mod dispatch;
mod handle;
mod ingest;
mod journaling;
mod link_deadman;
mod mount_rows;
mod mount_runtime;
mod mount_types;
mod publish;
mod recon_held;
mod reconcile;
mod refusals;
mod routing;
mod run_loop;
mod spawn;
mod strategy_drive;
mod timers;
mod watchdog;

#[path = "tests/audience.rs"]
#[cfg(test)]
mod audience_tests;

#[path = "tests/deadman.rs"]
#[cfg(test)]
mod deadman_tests;

#[path = "tests/link_deadman.rs"]
#[cfg(test)]
mod link_deadman_tests;

#[path = "tests/mount_account.rs"]
#[cfg(test)]
mod mount_account_tests;

#[path = "tests/multi_mount.rs"]
#[cfg(test)]
mod multi_mount_tests;

#[path = "tests/recon_held.rs"]
#[cfg(test)]
mod recon_held_tests;

#[path = "tests/mount_rows.rs"]
#[cfg(test)]
mod mount_rows_tests;
#[path = "tests/order_owners.rs"]
#[cfg(test)]
mod order_owners_tests;
#[path = "tests/route_key.rs"]
#[cfg(test)]
mod route_key_tests;
#[path = "tests/runtime_mount.rs"]
#[cfg(test)]
mod runtime_mount_tests;

#[path = "tests/support.rs"]
#[cfg(test)]
mod test_support;

use assemble::{
    EngineRoute, assemble_core, mount_engine_idx, mount_engine_resolution, mounted_route_keys,
};
pub use broker::LiveBroker;
pub use config::{
    CoreConfig, DequeuedHook, EquitySampleHook, JournalConfig, JournalViewHook, StrategyFactory,
};
use core_thread::{ArmedBy, CoreThread, HeldReconAlert, PendingOwner};
use deadman::DeadMan;
pub use deadman::{DeadManAction, DeadManConfig};
pub use handle::{CommandSink, CoreHandle, ReconcileDriver};
pub use link_deadman::LinkDeadManConfig;
use link_deadman::{LinkDeadMan, LinkObservation};
use mount_rows::MountRowCache;
pub use mount_types::{
    BufferedBracket, BufferedConditional, BufferedModify, BufferedSubmit, MountLeg, StrategyMount,
};
// `pub(crate)`: `crate::order_owners` persists and restores this ledger.
pub(crate) use mount_types::MountAttribution;
use mount_types::MountState;
use routing::{event_coid, event_route_key, event_symbol};
use run_loop::panic_text;
pub use spawn::{spawn_core, spawn_core_multi};

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
    /// run [`CoreThread::sweep_link_deadman`] and re-arm the next sweep — the CONNECTION-state
    /// dead-man's cadence (M13). Armed at [`CoreThread::arm_boundary_timers`] ONLY when
    /// [`CoreConfig::link_deadman`] is `Some`; never armed at all (the default) otherwise.
    /// Self-rescheduling like [`Self::DeadManSweep`], and INDEPENDENT of it — either, both or
    /// neither switch may be armed, and the two evaluate different signals on their own cadences.
    LinkDeadManSweep,
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

impl<C: ExecutionClient> CoreThread<C> {
    /// **§4.2's table, ROW 1, `N ≥ 2` cell** — `Some(every candidate account)` when the sender named
    /// NO account and this process runs several accounts of the payload's venue, `None` when the
    /// destination is determined.
    ///
    /// This is the question the account-routing spec's ONE RULE reduces to: *"The destination must
    /// be determined by what the sender said. Where the sender's words admit more than one account,
    /// the system must refuse rather than pick."* It is asked at every RISK-INCREASING lowering site
    /// beside that site's existing [`Self::route_of`] call, rather than inside `route_of` itself,
    /// because `route_of` cannot express a refusal — its `None` already means *"no engine claims
    /// this payload; apply your historical fallback"*, and collapsing "nobody" into "everybody"
    /// would turn the refusal back into the `unwrap_or(0)` it replaces.
    ///
    /// # What it covers, and what it deliberately does not
    ///
    /// * **`EngineRoute::Payload`** — every EXTERNAL command (a DOM click, a tradehub ticket, a CLI
    ///   verb). This is the misroute the whole spec is about.
    /// * **§9 item 2, the FOREIGN-VENUE FALLTHROUGH** — an `EngineRoute::Mount`/`Engine` whose named
    ///   engine is on a DIFFERENT exchange than the payload. [`Self::routed_engine`] filters that
    ///   deference away (correctly — *"the mount's account is a fact about its OWN venue"*), and
    ///   before this guard the fallthrough landed on the foreign venue's DEFAULT account, so a
    ///   labelled mount's cross-venue declared leg could reach an account nobody named. Covered here
    ///   by construction: `routed_engine` answering `None` IS the fallthrough.
    /// * **NOT the risk-REDUCING venue verbs.** `MarketExit`, `MassCancel { venue: Some(_) }` and
    ///   `Flatten` resolve through [`Self::exit_scope_engines`] and never ask this — §4.5's law,
    ///   quoted at each of those arms.
    /// * **NOT the market-data or inbound-event lanes.** A price is a fact about the EXCHANGE
    ///   ([`Self::mirror_venue_price`]); refusing there would be a defect.
    ///
    /// # ⚠ `N = 0` keeps `unwrap_or(0)` and does NOT refuse
    ///
    /// §4.2's table says an unroutable venue refuses. This does not, deliberately: an unknown venue
    /// is not AMBIGUITY, it is a different question with a different blast radius — `caps_venue`'s
    /// unknown-venue affordance exists for paper/sim engines behind non-roster ids, and
    /// [`Self::exit_scope_engines`]'s own doc records that engine 0 there is *"load-bearing for the
    /// `MassCancel` arm's own tests"*. Refusing it would change SINGLE-account behaviour, which is
    /// the one thing Stage 1 may not do. It is named here so it is a decision rather than an
    /// oversight, and it belongs to whichever stage widens the table's other two rows.
    ///
    /// Cold path — one `Vec` per order-lowering call, sized by the accounts of one exchange —
    /// and short-circuited to a single bool test on every single-account process.
    fn ambiguous_accounts(&self, route: EngineRoute, payload_venue: &str) -> Option<Vec<String>> {
        if !self.multi_account || self.routed_engine(route, payload_venue).is_some() {
            return None;
        }
        let all = self.engines_of_venue(payload_venue);
        (all.len() > 1).then(|| all.into_iter().map(|i| self.eng(i).route_key.clone()).collect())
    }
}

#[path = "tests/safe_state.rs"]
#[cfg(test)]
mod safe_state_tests;
