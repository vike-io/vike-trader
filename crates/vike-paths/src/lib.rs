//! vike-paths — the PROJECT'S FILESYSTEM LAYOUT: where program-written state, the stores and
//! scratch live when nobody said, and the one project-root walk every `<project>`-relative path in
//! the workspace starts from.
//!
//! # The modules
//!
//! * [`state_path`] owns the project-root walk (`project_settings_dir`, `workspace_root`) and every
//!   directory resolved under it: `settings/` and its `state/`, `user_data/` (runs, studies,
//!   strategies, indicators, plugins, logs), `market_data/`, `bin/` and `tmp/`.
//! * [`store_path`] is where the HIST store lives, and [`tick_store_path`] where the LIVE-TICK store
//!   lives: each one resolver with its own argued precedence, and each answer TAGGED with the rung
//!   that chose it so the calling binary can log it.
//! * [`scratch`] is the retention rule for `<project>/tmp`: [`scratch::ScratchDir`], an owned
//!   directory removed when its guard drops, and [`scratch::sweep`], the bounded startup sweep that
//!   catches what ownership cannot (a killed process).
//!
//! # The contract every module keeps
//!
//! * **Pure with respect to the ENVIRONMENT.** No module reads a variable: the caller (a binary)
//!   reads the process environment and passes the values in, per the rule
//!   `crates/vike-ops/tests/settings_secrets/settings_registry.rs` enforces. The `*_ENV` constants
//!   name the variables; they read nothing.
//! * **No clock, no logging.** A resolver returns its answer as DATA (the rung, a `Swept` count) and
//!   the caller logs it; scratch uniqueness and the sweep's order need no wall-clock read
//!   (`crates/vike-ops/tests/architecture/clock_pin.rs`'s `DETERMINISM_CRITICAL_CRATES` holds this
//!   crate).
//! * **std only.** No dependency, normal or dev: a dependency here would ride into every crate that
//!   resolves a path.
//!
//! Filesystem contact is the walk's `is_file`/`is_dir` probes and its manifest reads,
//! `state_path::write_path`'s lazy `create_dir_all`, and the scratch guard's create, sweep and
//! removals.
//!
//! The store-plane classifier (whether a store kind holds market data or an account's) is NOT
//! here: it does no I/O and is domain vocabulary, so it stays `vike-model`'s
//! (`crates/vike-model/src/store_plane.rs`).
//!
//! Layout and the traps a change here meets — the project-root walk's rule, and what reads these
//! files by path: `crates/vike-paths/CLAUDE.md`.

#![warn(unreachable_pub)]

pub mod scratch;
pub mod state_path;
pub mod store_path;
pub mod tick_store_path;
