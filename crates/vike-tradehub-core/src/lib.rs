//! vike-tradehub-core — the GENERIC venue mount: the fold every venue's `ExecutionEngine` is built
//! through, with no venue named in it
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! [`make_engine`] is the one entry point. Handed the REGISTRY
//! (`vike_tradehub::registry::REGISTRY`, one [`VenueRow`] per `vike_model::VENUES` id;
//! [`RuntimeConfig::registry`] for [`assemble_runtime`]), it runs, in order:
//!
//! ⚠ **An account runs at the tier its active row states, or it does not run**
//! (`docs/decisions/0156-an-account-runs-at-its-tier-or-not-at-all-and-accounttier-is-the-one-tier-type.md`).
//! Every outcome but a `paper` row on the paper simulator and a `demo`/`live` row armed at exactly
//! that tier is NOT RUNNING ([`NotRunning`]): no engine, its `ArmingBlock` as the reason, and never
//! a paper engine in its place.
//!
//! 1. **The account's tier** (the `account` table's `tier` while the row is `active`,
//!    `crates/vike-tradehub-core/src/arming.rs`'s `account_tier`), consulted FIRST: no row, an inactive row
//!    or two active non-paper tiers do not run, and a `paper` tier returns the paper simulator —
//!    each having loaded no credential and opened no socket. For binance, bybit, okx and
//!    hyperliquid the tier also CHOOSES the network (decision 0095), each bridge deciding for
//!    itself; it reaches a bridge as `AdapterInputs::tier`, the account's `vike_model::AccountTier`.
//! 2. **The arming probe** — the bridge's `VenueAdapter::resolve`, PURE, behind two generic
//!    preconditions (a labelled account on a venue that addresses no named account; a `paper`
//!    tier), and the no-downgrade rule: a `live` account whose bridge could bind only demo does not
//!    run. `vike-backend venues` reads the same function, so projection and mount agree.
//! 3. **The pre-connect budget refusal** for a venue the probe calls armed.
//! 4. **The process-exclusive claim**, for a venue whose declaration names one
//!    (`crates/vike-tradehub-core/src/exclusive.rs`'s `holder` and `claim`; dukascopy's JForex sidecar),
//!    kept only when the bridge's mount comes back `Live`.
//! 5. **The bridge's `VenueAdapter::start`, and the fold of its outcome**: the symbol and leg grids,
//!    contract size and margin mode, operator budget and universal defaults, exposure and equity
//!    narrowing, fee schedule, the identity record at the tier the credentials bound, the engine at
//!    the account's tier — and NOT RUNNING whenever the bridge declines. An arm whose probe said it
//!    sends no order (`HeldBelowLive::OrdersHeld`) keeps its bound tier and has its client wrapped
//!    so no order reaches it (`crates/vike-tradehub-core/src/orders_held.rs`'s `hold_orders`).
//!
//! A venue this build does not compile (`VenueRow::FeatureAbsent`) runs no `demo`/`live` account
//! and answers the generic facts of `crates/vike-tradehub-core/src/registry.rs`'s `ABSENT_CLOCK`; an
//! unknown id runs none either. A `paper` account of either runs the simulator.
//!
//! ⚠ **This crate names no venue, and two gates hold it.** At layer 35 an edge to a bridge (40/41)
//! points up (`crates/vike-ops/tests/architecture/layer_gate.rs`), and
//! `crates/vike-tradehub-core/tests/adapter_names_no_venue_gate.rs` refuses a `vike_model::VENUES` id as
//! a string literal anywhere in this crate's non-test source. A venue's facts live in its bridge's
//! `VenueDeclaration`; its roster tests run in `vike-tradehub`, which holds the registry.
//!
//! Settings: [`make_engine`] takes a [`AdapterPolicy`] in its [`AdapterEnv`] — the projection of
//! `vike_config::Policy` this mount APPLIES, plus the account table and venue settings the
//! composition root read. This crate never reads `std::env` for it; `None` runs no account.
//!
//! Startup safety: [`preflight`] is the pure go/no-go gate and [`startup`] its real-probe wiring,
//! run once by [`assemble_runtime`] — the clock leg over each row's declared clock, the credential leg
//! over each row's `credential_probe`.
//!
//! # The runtime assembly and the strategy mounts
//!
//! The run/composition layer on the fold (the former `vike-run`,
//! docs/decisions/0098-vike-run-merges-into-vike-mount.md):
//!
//! - [`assemble_runtime`] (`src/node.rs`): one engine per wired market ([`wired_markets`],
//!   [`WiredMarket`]) through [`make_engine_accounts`], mounted in table order and held in
//!   engine-rank order; the reconcile legs, the single-writer core, the live-event forwarder; the
//!   pre-assembly armed set ([`armed_live_venues`]) the live-account locks are claimed from; the
//!   `venue_mounted` journal records.
//! - The strategy mounts (`src/run.rs`): the A-S maker and any strategy on the live core, over the
//!   paper exchange ([`build_paper_maker_core`], [`build_paper_strategy_core_with`], …) or over
//!   [`assemble_runtime`] ([`build_live_maker_core`], [`build_live_strategy_core`], …).
//! - The cross-exchange maker mount (`src/xemm.rs`, [`build_paper_xemm_core`],
//!   [`build_live_xemm_core`]).
//! - The [`incident`] evidence collector, which the `incident` bin drives.
//! - The host-side drivers over the core's handle (they came in from `vike-core`, wave 5 P6a):
//!   [`CoreLaneSink`] (`src/core_sink.rs`, a feed's `LiveDataSink` onto the core's ingest lanes),
//!   [`spawn_recon`] with [`ReconManager`], [`ReconDriver`] and [`ReconLeg`] (`src/recon_manager.rs`,
//!   the reconcile fetch thread) and [`journal_view_from_store`] (`src/journal_view.rs`, the
//!   reconcile cross-check's third leg).
//!
//! None of it names a venue: the wired markets are rows [`wired_markets`] derives from `vike_model`'s
//! `VENUE_WIRING` and the registry the daemon hands in.

#![warn(unreachable_pub)]

use vike_model::accounts::account_keys::AccountLabel;

use arming::{
    account_event_sender, account_max_exposure, account_route_key, multiplier_grid,
    report_unaddressable_accounts, venue_tier,
};
use budget::{BUDGET_EXAMPLES, arm_universal_defaults, merge_operator_budget};
use engine::AdapterParts;
use paper_fallback::{
    paper_engine, report_halt_admit, report_halt_admit_armed, report_not_running,
    report_table_decision,
};

mod arming;
pub mod book_identity;
mod budget;
mod contract;
mod engine;
mod error;
mod exclusive;
pub mod halt;
mod orders_held;
mod paper_fallback;
pub mod policy;
pub mod preflight;
mod profile_apply;
mod registry;
pub mod server_time;
pub mod startup;
pub mod symbol_grid;

pub use arming::{
    account_tier, known_accounts, margin_mode_grid, resolve_fee_schedule, shared_book_ceiling_note,
    shared_books_for, stopped_by_the_live_switch, symbol_for_account,
    unaddressable_accounts_message, unaddressable_accounts_text, venue_account_arming,
    venue_arming, venue_arming_under, would_go_live, would_go_live_under,
    would_go_live_under_policy,
};
pub use budget::require_live_risk_budget;
pub use engine::{
    AccountEngines, AdapterEnv, EngineAndRecon, accounts_to_run, make_engine, make_engine_accounts,
    make_engine_for_account, make_engine_with_legs,
};
pub use error::{EngineError, NotRunning};
pub use policy::AdapterPolicy;
pub use profile_apply::{
    ConfirmGraceHazard, GuardsReport, apply_guards_and_sinks, initial_trading_state_to_send,
    journal_config_from, send_initial_trading_state, tune_journal,
};
// Crate-root vocabulary: `registry` is private, so this is the only name for the row type and its
// lookup.
pub use registry::{VenueRow, row_of};
pub use symbol_grid::declared_grid_source;
/// The arming VOCABULARY, re-exported because it appears in THIS crate's public signatures:
/// [`venue_arming`] returns `Vec<vike_config::VenueArming>`, and a consumer should not need a
/// `vike-config` manifest edge just to NAME what it already holds.
///
/// ⚠ Not a `pub use` SHIM in the sense `CLAUDE.md` forbids — nothing MOVED here and no old spelling
/// is kept alive. It is that rule's module-vocabulary exception, the shape of
/// `vike_exec::ExecutionClient`. The tier is NOT re-exported: every caller names
/// `vike_model::AccountTier`, the one tier vocabulary (decision 0129).
pub use vike_config::{ArmingBlock, VenueArming};

// ⚠ The HALT sentinel path has ONE name, `vike_tradehub_core::halt::X` (the module is `pub`); nothing
// re-exports its items at the crate root.

// ---- the runtime assembly and the strategy mounts ----
// The modules are PRIVATE and their public items re-exported here, so each has exactly one public
// name (`vike_tradehub_core::X`); `incident` stays a public module because its bin addresses it by path.
mod core_sink;
pub mod incident;
mod journal_view;
mod node;
mod recon_manager;
mod run;
mod xemm;

pub use core_sink::CoreLaneSink;
pub use journal_view::journal_view_from_store;
pub use node::{
    AssembledRuntime, RuntimeAccount, RuntimeConfig, RuntimeError, WiredMarket,
    account_symbols_for, armed_live_venues, assemble_runtime, assemble_runtime_with_preflight,
    journal_venue_mounts, refuse_unarmed_live_venues, runnable_accounts, wired_markets,
};
pub use recon_manager::{
    HealthProbe, ReconConfig, ReconDriver, ReconHealth, ReconLeg, ReconManager, spawn_recon,
};
pub use run::{
    BookFills, LiveMakerCore, MakerBreaker, MakerConfig, MakerSink, MakerSkew, MountSpec,
    MultiStrategyCore, PaperCoreOpts, PaperHalt, PaperMakerCore, StrategyMountSpec,
    TickBarSynthesizer, build_live_maker_core, build_live_multi_strategy_core,
    build_live_strategy_core, build_live_strategy_core_with_preflight, build_maker,
    build_paper_maker_core, build_paper_maker_core_with, build_paper_multi_strategy_core_with,
    build_paper_strategy_core_with,
};
/// The concrete maker [`build_maker`] returns, re-exported so a caller can NAME it without a direct
/// `vike-mm` edge: `vike-tradehub` holds it in an `-> Option<SpreadMaker>` so its default mount and
/// its `[strategy] name = "spread_maker"` mount are provably ONE construction.
pub use vike_mm::SpreadMaker;
/// The paper exchange's fill record, which [`PaperMakerCore::fills`] and the xEMM mounts expose.
pub use vike_paper::PaperFill;
pub use xemm::{
    LiveXemmCore, PaperXemmCore, XemmConfig, XemmConfigError, XemmCoreError, build_live_xemm_core,
    build_paper_xemm_core, build_paper_xemm_core_with, build_xemm_maker,
};

#[cfg(test)]
mod fee_schedule_tests;

#[cfg(test)]
mod account_event_lane_tests;

/// The generic fold over a `VenueRow::Adapter` (`contract.rs`), driven through PLANTED venues: the
/// arming projection, the pre-connect refusal, the exclusive claim, the recon-trigger routing and
/// the outcome fold, with no real bridge.
#[cfg(test)]
mod contract_tests;

/// The venue-client doubles this crate's unit tests share (`NoopClient`, `EmptyRecon`).
#[cfg(test)]
mod testutil;
