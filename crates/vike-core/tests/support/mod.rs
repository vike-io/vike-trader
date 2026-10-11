//! `kit` — the shared builders and test doubles of vike-core's integration suites: ONE spelling of
//! the engines, strategy mounts, core configs, events, requests and doubles that the `journal`,
//! `recon`, `wiring` and `runtime_smoke` binaries had each re-typed, file by file.
//!
//! Included by each of those four test-target roots as `#[path = "support/mod.rs"] mod kit;` — the
//! roots are `#[path]` crate roots, the same idiom as their `#[path = "../src/scratch.rs"]` — and
//! reached from a member as `crate::kit::<module>::<item>`. Cargo builds no target from this
//! directory: it auto-discovers only `tests/*.rs` and `tests/<dir>/main.rs`. Deliberately NOT
//! included by `runtime_latency.rs` (the CI latency gate's own binary, which takes no shared edit
//! without a re-measure) nor by `drawdown_measures_own_pnl.rs` (its `eng`/`adopt_wallet` builders are
//! that file's subject).
//!
//! # The contract: every builder fixes its DEFAULTS, and says which
//!
//! The contract of `vike_marketdata::test_support`, applied to this crate's vocabulary. A builder
//! takes only the fields a test varies and pins every other field to the value its own doc names, so
//! a hand-written copy may be replaced by the builder exactly when its signature AND every pinned
//! field match. A test that READS a pinned field — a venue string, a seed, a `BalanceMode`, a risk
//! limit, a mark, the trade-id bytes (the fill dedup key), a journal knob — keeps building that value
//! itself, so the field it depends on stays visible at its own site. Where two shapes differ in one
//! pinned field, each gets its own builder whose NAME says the difference (`sim_engine` /
//! `dyn_engine`), never one builder with a default that is wrong for half its callers.
//!
//! # Rules for this module
//!
//! - `#![allow(dead_code)]` below is required, not tidiness: each binary uses a subset of the kit —
//!   the reason `crates/vike-core/src/scratch.rs` carries the same attribute.
//! - No statics and no shared mutable state. A grouped binary runs its tests as THREADS in one
//!   process under a plain `cargo test`, so a shared cell here would couple tests that are
//!   independent today.
//! - No re-exports and no aliases: one name per symbol. A call site names
//!   `vike_exec::testing::RecordingClient` or `crate::scratch::Scratch` itself.
//!
//! # What is deliberately NOT here
//!
//! - Every `#[test]` body, and every scenario builder (the `build_*_journal` fixtures, the probe and
//!   recorder strategies, the journaled `core_config`s whose snapshot cadence decides where the BASE
//!   snap falls).
//! - A builder whose pinned value IS a test's subject: an Authoritative wallet, an overridden
//!   `route_key`, `extra_symbols`, a fill whose `mark_price` is `None`, a trade-id format.
//! - A builder on a venue a file pins for its own reasons (`dispatch_lanes.rs`'s binance `limit`,
//!   `sizing_equity_ceiling.rs`'s binance `long_fill`).
//! - Each file's `unique_dir` wrapper (the tag prefix is attribution, and it is already one line
//!   over `Scratch::reserved`), and the two `RestingClient`s, whose cancel reasons still differ.

#![allow(dead_code)]

pub(crate) mod doubles;
pub(crate) mod engines;
pub(crate) mod events;
pub(crate) mod handle;
pub(crate) mod journal;
pub(crate) mod mounts;
pub(crate) mod requests;
