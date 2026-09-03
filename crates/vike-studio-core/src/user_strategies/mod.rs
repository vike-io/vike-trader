//! **A user's strategies are FILES**, not rows in a blob — the loader for
//! `<project>/user_data/strategies/`, the resolver that turns one of its presets into the params a
//! strategy is constructed with, plus the one-time migration off the Studio's
//! `studio_strategies.json`.
//!
//! Ported from nothing: net-new Rust surface over the user-content directory
//! `crates/vike-model/src/state_path.rs`'s `PROJECT_USER_DATA_DIR` defines.
//!
//! # Why files at all
//!
//! Until this module every user-authored strategy lived in ONE pretty-JSON array —
//! `crates/vike-studio/src/saved.rs`'s `save_strategies`, written next to the DATA store as
//! `studio_strategies.json`. That shape denies a person every ordinary thing they do with their own
//! work: a script cannot be opened in an editor, diffed, committed, copied to another machine, or
//! sent to somebody else, and none of it is addressable by a path. Two properties make it actively
//! lossy rather than merely inconvenient:
//!
//! * **One corrupt byte loses the library.** `crates/vike-studio/src/saved.rs`'s `load_strategies`
//!   answers a parse failure with an EMPTY list, deliberately ("best-effort state") — which is the
//!   right call for a window layout and the wrong one for the only copy of somebody's work. With
//!   one file per strategy a bad byte costs exactly the strategy it is in.
//! * **It was filed under the wrong owner.** The blob sits at `store.root()`, i.e. inside the hist
//!   store — hundreds of gigabytes of re-downloadable market data whose whole lifecycle is
//!   "delete it and re-ingest". `state_path.rs`'s `PROJECT_USER_DATA_DIR` argues the split at
//!   length: delete `settings/state` and the program rewrites it; delete a strategy and the user's
//!   work is gone.
//!
//! # The layout, and the rules the loader depends on
//!
//! ```text
//! user_data/strategies/rhai/<name>/<name>.rhai      the ENTRY file — its stem IS the strategy name
//! user_data/strategies/rhai/<name>/*.toml           presets, flat beside the entry
//! user_data/strategies/rhai/<name>/presets/*.toml   …or filed, for the user whose sweep left forty
//! user_data/strategies/rust/<builtin>/*.toml        presets for a strategy whose code is in the BINARY
//! ```
//!
//! 1. **The entry file matches its folder**, so a folder holding several scripts is never
//!    ambiguous — the others are the script's own helpers and are read by nothing here.
//! 2. **A `.toml` beside the entry is a PRESET for that strategy.** A preset belongs to its
//!    strategy, so `rm -r <name>/` removes the whole thing and two strategies' presets can never be
//!    confused.
//! 3. **A preset IS the params table** — FLAT, the keys a profile's `[strategy.params]` receives one
//!    for one, which is also exactly what [`apply_migration`] writes. A file that wraps its knobs in
//!    a `[params]` container is REFUSED with the fix in the message rather than accepted and
//!    silently ignored downstream (`load.rs`'s `check_preset_shape` carries the argument).
//! 4. **A folder under `rust/` needs no entry file when its name is a BUILT-IN strategy** — the code
//!    is in the binary, resolved by NAME through
//!    `crates/vike-backtest/src/harness/registry.rs`'s `strategy_by_name`, and the folder holds
//!    nothing but presets for it. That is the shape [`plan_migration`] produces for every migrated
//!    native row, and treating it as a missing entry file was a real bug: `migrate.rs` predicted it
//!    in its own module doc before a rust-side loader existed.
//!
//! # A preset that nothing applies is not a preset
//!
//! [`resolve_preset`] is the other half: `<strategy>/<preset>` → a [`PresetRun`] carrying the spec
//! and overrides `crates/vike-studio-core/src/run.rs`'s `build_strategy_with` constructs the
//! strategy from. `preset.rs`'s module doc is the authority on why the two [`StrategyBody`] kinds
//! reach that destination differently, and on why the keys a Rhai script cannot receive are NAMED
//! ([`PresetRun::dropped`]) rather than dropped in silence.
//!
//! # Failure is DATA, and it is the feature
//!
//! A strategy loader that silently skips what it cannot use is unusable: "I dropped a file in and
//! nothing happened" has no answer, and the user's next move is to try the same thing again. So
//! nothing here is skipped silently. [`load_rhai_strategies`] returns a [`LoadReport`] carrying the
//! strategies it loaded AND one NAMED [`LoadDiagnostic`] per thing it could not — each one carrying
//! the path, the reason and the fix — and [`render_compile_log`] turns that report into the text a
//! caller appends to `user_data/logs/compile.log` (`state_path.rs`'s `user_logs_dir`).
//!
//! **This module performs no logging and opens no log file.** It reads the strategy tree and
//! returns what it found; the composition root decides where the text goes, exactly as
//! `crates/vike-model/src/state_path.rs` reads no environment and returns paths. The one filesystem
//! contact beyond reading the tree is [`apply_migration`]'s writes.
//!
//! # What the migration does, and the one thing it must never do
//!
//! [`plan_migration`] / [`apply_migration`] turn the legacy JSON rows into this layout ONCE. The
//! JSON is **read-only from here on**: nothing in this module opens it for writing, deletes it, or
//! moves it. A migration that removes its own source cannot be re-run, cannot be checked, and
//! turns a rollback into data loss — and this one has to survive being run by a build the user may
//! roll back off. Idempotency is the other half of that: a target that already exists is REPORTED
//! and never overwritten, so running it twice writes nothing and a user's edits to a migrated file
//! are safe.

pub(crate) mod load;
mod migrate;
mod preset;

pub use load::{
    load_rhai_strategies, load_user_strategies, render_compile_log, LoadDiagnostic, LoadReport,
    Preset, Severity, StrategyBody, UserStrategy,
};
pub use migrate::{
    apply_migration, plan_migration, LegacyBody, LegacyEntry, MigrationOutcome, MigrationPlan,
    MigrationSkip, PlannedFile,
};
pub use preset::{resolve_preset, PresetError, PresetRun};
