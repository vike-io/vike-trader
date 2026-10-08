//! vike-mount — the GENERIC venue mount: the fold every venue's `ExecutionEngine` is built
//! through, with no venue named in it
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! [`make_engine`] is the one entry point. Handed the REGISTRY
//! (`vike_tradehub::registry::REGISTRY`, one [`VenueRow`] per `vike_model::VENUES` id;
//! [`NodeConfig::registry`] for [`build_node`]), it runs, in order:
//!
//! 1. **The arming ceiling** (`policy.venues.<venue>`, per account), consulted FIRST: a `paper`
//!    venue returns the paper engine having loaded no credential and opened no socket. It can only
//!    REFUSE (`vike_config::VenueMode::cap` is `min`) — except that for the venues in
//!    `vike_secrets::live_means_mainnet::SWITCHED_VENUES` it also CHOOSES the network (decision
//!    0095) — and reaches a bridge only as `live_permitted: bool`.
//! 2. **The arming probe** — the bridge's `VenueMount::resolve`, PURE, behind two generic
//!    preconditions (a labelled account on a venue that addresses no named account; a `paper`
//!    ceiling). The arming screen reads the same function, so projection and mount agree.
//! 3. **The pre-connect budget refusal** for a venue the probe calls armed.
//! 4. **The process-exclusive claim**, for a venue whose declaration names one
//!    (`crates/vike-mount/src/exclusive.rs`'s `holder` and `claim`; dukascopy's JForex sidecar),
//!    kept only when the bridge's mount comes back `Live`.
//! 5. **The bridge's `VenueMount::mount`, and the fold of its outcome**: the symbol and leg grids,
//!    contract size and margin mode, operator budget and universal defaults, exposure and equity
//!    narrowing, fee schedule, the identity record at the tier the credentials bound — and a PAPER
//!    engine whenever the bridge declines, because only this crate builds one.
//!
//! A venue this build does not compile (`VenueRow::FeatureAbsent`) mounts paper and answers the
//! generic facts of `crates/vike-mount/src/registry.rs`'s `ABSENT_CLOCK`; an unknown id mounts
//! paper too.
//!
//! ⚠ **This crate names no venue, and two gates hold it.** At layer 35 an edge to a bridge (40/41)
//! points up (`crates/vike-ops/tests/architecture/layer_gate.rs`), and
//! `crates/vike-mount/tests/mount_names_no_venue_gate.rs` refuses a `vike_model::VENUES` id as
//! a string literal anywhere in this crate's non-test source. A venue's facts live in its bridge's
//! `VenueDeclaration`; its roster tests run in `vike-tradehub`, which holds the registry.
//!
//! Settings: [`make_engine`] takes a [`MountPolicy`] in its [`MountEnv`] — the projection of
//! `vike_config::Policy` this mount APPLIES, plus the account table and venue settings the
//! composition root read. This crate never reads `std::env` for it; `None` reads PAPER.
//!
//! Startup safety: [`preflight`] is the pure go/no-go gate and [`startup`] its real-probe wiring,
//! run once by [`build_node`] — the clock leg over each row's declared clock, the credential leg
//! over each row's `credential_probe`.
//!
//! # The node assembly and the strategy mounts
//!
//! The run/composition layer on the fold (the former `vike-run`,
//! docs/decisions/0098-vike-run-merges-into-vike-mount.md):
//!
//! - [`build_node`] (`src/node.rs`): one engine per wired market ([`NodeConfig::markets`],
//!   [`WiredMarket`]) through [`make_engine_accounts`], mounted in table order and held in
//!   engine-rank order; the reconcile legs, the single-writer core, the live-event forwarder; the
//!   pre-mount armed set ([`armed_live_venues`]) the live-account locks are claimed from; the
//!   `venue_mounted` journal records.
//! - The strategy mounts (`src/run.rs`): the A-S maker and any strategy on the live core, over the
//!   paper exchange ([`build_paper_maker_core`], [`build_paper_strategy_core_with`], …) or over
//!   [`build_node`] ([`build_live_maker_core`], [`build_live_strategy_core`], …).
//! - The cross-exchange maker mount (`src/xemm.rs`, [`build_paper_xemm_core`],
//!   [`build_live_xemm_core`]).
//! - The [`incident`] evidence collector, which the `incident` bin drives.
//!
//! None of it names a venue: the wired markets are rows the daemon hands in, like the registry.

use vike_model::accounts::account_keys::AccountLabel;

use arming::{
    account_ceiling, account_event_sender, account_route_key, ceiling_permits_live,
    multiplier_grid, report_unaddressable_accounts, venue_ceiling,
};
use budget::{
    BUDGET_EXAMPLES, EXAMPLE_PROFILE_PATH, arm_universal_defaults, merge_operator_budget,
};
use engine::MountParts;
use paper_fallback::{
    paper_engine, report_capped_to_paper, report_halt_admit, report_halt_admit_armed,
    venue_arming_migration,
};

mod arming;
pub mod book_identity;
mod budget;
mod contract;
mod engine;
mod error;
mod exclusive;
mod paper_fallback;
pub mod policy;
pub mod preflight;
mod registry;
pub mod server_time;
pub mod startup;
pub mod symbol_grid;

pub use arming::{
    known_accounts, margin_mode_grid, resolve_fee_schedule, shared_book_ceiling_note,
    shared_books_for, symbol_for_account, unaddressable_accounts_message,
    unaddressable_accounts_text, venue_account_arming, venue_arming, venue_arming_under,
    would_mount_live, would_mount_live_under, would_mount_live_under_policy,
};
pub use budget::require_live_risk_budget;
pub use engine::{
    EngineAndRecon, MountEnv, accounts_to_mount, make_engine, make_engine_accounts,
    make_engine_for_account, make_engine_with_legs,
};
pub use error::MountError;
// `paper_fallback` is private: the only name for the migration warning's text, read by the roster
// tests in `crates/vike-tradehub/tests/mount_roster.rs`.
pub use paper_fallback::venue_arming_migration_message;
pub use policy::MountPolicy;
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
/// `vike_exec::ExecutionClient`.
pub use vike_config::{ArmingBlock, VenueArming, VenueMode, VenuePolicy};

// ⚠ There is no `vike_mount::halt` re-export: every caller spells `vike_bridge_core::halt`, the one
// name of the HALT sentinel path.

// ---- the node assembly and the strategy mounts ----
// The modules are PRIVATE and their public items re-exported here, so each has exactly one public
// name (`vike_mount::X`); `incident` stays a public module because its bin addresses it by path.
pub mod incident;
mod node;
mod run;
mod xemm;

pub use node::{
    MOUNT_FAILED, MountAccount, Node, NodeConfig, NodeError, WiredMarket, account_symbols_for,
    armed_live_venues, build_node, build_node_with_preflight, journal_venue_mounts, mount_accounts,
    refuse_unarmed_live_venues,
};
pub use run::{
    BookFills, LiveMakerMount, MakerBreaker, MakerMount, MakerMountConfig, MakerSink, MakerSkew,
    MountSpec, MultiStrategyMount, PaperHalt, PaperMountOpts, StrategyMountSpec,
    TickBarSynthesizer, build_live_maker_core, build_live_multi_strategy_core,
    build_live_strategy_core, build_live_strategy_core_with_preflight, build_maker,
    build_paper_maker_core, build_paper_maker_core_with, build_paper_multi_strategy_core_with,
    build_paper_strategy_core_with,
};
/// The concrete maker [`build_maker`] returns, re-exported so a caller can NAME it without a direct
/// `vike-mm` edge: `vike-tradehub` holds it in an `-> Option<SpreadMaker>` so its default mount and
/// its `[strategy] name = "spread_maker"` mount are provably ONE construction.
pub use vike_mm::SpreadMaker;
/// The paper exchange's fill record, which [`MakerMount::fills`] and the xEMM mounts expose.
pub use vike_paper::PaperFill;
pub use xemm::{
    LiveXemmMount, PaperXemmMount, XemmConfigError, XemmMountConfig, XemmMountError,
    build_live_xemm_core, build_paper_xemm_core, build_paper_xemm_core_with, build_xemm_maker,
};

#[cfg(test)]
use budget::ARMED_MAX_ORDERS_PER_WINDOW;
#[cfg(test)]
use engine::engine_mode;

#[cfg(test)]
mod fee_schedule_tests;

/// The PRE-CONNECT budget refusal. The probe's roster tests (each venue's credential shapes through
/// its bridge's `resolve`, under the arming ceiling) run where the registry is:
/// `crates/vike-tradehub/tests/mount_roster.rs`'s `preconnect` module.
#[cfg(test)]
mod preconnect_tests;

#[cfg(test)]
mod account_event_lane_tests;

/// The generic fold over a `VenueRow::Mount` (`contract.rs`), driven through PLANTED venues: the
/// arming projection, the pre-connect refusal, the exclusive claim, the recon-trigger routing and
/// the outcome fold, with no real bridge.
#[cfg(test)]
mod contract_tests;
