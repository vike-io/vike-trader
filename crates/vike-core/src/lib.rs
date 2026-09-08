//! The single-writer live core — one thread owns all mutable trading state; ingest via the
//! vike-exec lanes; GUI reads lossy arc-swap CoreSnapshots and never back-pressures. Extracted
//! from vike-exec (crate-reorg Phase 2, spec D3). Producer lanes live in vike_exec::lanes so
//! venue bridges never depend on this crate.

pub mod control;
pub mod core_sink;
pub mod counters;
pub mod emulator;
pub mod journal;
pub mod journal_lock;
// The store-backed builder for `CoreConfig::journal_view_provider`'s third reconcile leg. NOT the
// WAL (`journal` above) — that is the command journal; this reads the MATERIALIZED exec log back.
pub mod journal_view;
pub mod mount_topology;
pub mod portfolio;
pub mod recent;
pub mod recon_manager;
pub mod replay;
pub mod run_profile;
pub mod runtime;
pub mod schedule;
// The owned `/tmp` scratch guard this crate's journal tests allocate through. `#[cfg(test)]`, so a
// production build never compiles it; the `tests/` binaries pull the SAME file in with
// `#[path = "../src/scratch.rs"]`. Its module doc carries the leak this closed.
#[cfg(test)]
mod scratch;
pub mod snapshot;
pub mod strategy_state;
pub mod timer_wheel;

pub use core_sink::CoreLaneSink;
pub use counters::{Counters, CountersReport};
pub use emulator::{ConditionalBook, FiredConditional};
pub use journal_view::journal_view_from_store;
pub use portfolio::Portfolio;
pub use recent::{EventNote, RecentNote};
pub use recon_manager::{
    HealthProbe, ReconConfig, ReconDriver, ReconHealth, ReconManager, spawn_recon,
};
pub use run_profile::{
    Broker, BrokerKind, ConfirmGraceHazard, EventSource, EventSourceKind, GridSource, Guards,
    GuardsReport, Mode, ProfileError, ProfileMarginCall, ProfileRisk, ProfileTradingState,
    RunProfile, Sinks, journal_config_from_env, resolve_profile,
};
pub use runtime::{
    CommandRejected, CommandSink, CoreConfig, CoreHandle, DeadManAction, DeadManConfig,
    DequeuedHook, JournalConfig, JournalViewHook, LinkDeadManConfig, LiveBroker, MountBudget,
    MountLeg, ReconcileDriver, StrategyFactory, StrategyMount, spawn_core, spawn_core_multi,
};
pub use schedule::{LiveSchedule, TimeRule};
pub use snapshot::{
    CoreSnapshot, MountRowKind, MountView, OrderView, PositionView, ReconAlertView, ReconBlock,
    VenueBlock,
};
pub use timer_wheel::{DeadlineTimerWheel, TimerId};
// The live-parameter-plane types (defined in vike-model so the runtime's command lane — the
// `Command::UpdateParams` -> `ParamsUpdate` payload — can name them) re-exported here for
// GUI/caller convenience. The `SpreadMaker` they tune now lives in the `vike-mm` crate (the
// vike-mm extraction); these payload types stay in vike-model and are surfaced by the runtime.
// `ControllerParams` is the position-executor twin (stage 6): the `StrategyParams::PositionController`
// payload a mounted `ControllerHarness` hot-swaps.
pub use vike_model::{ControllerParams, SpreadMakerParams, StrategyParams};
