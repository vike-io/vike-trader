//! `key_shapes` -- vike-connections' credential KEY-SHAPE suite: ONE test binary over what used to
//! be four.
//!
//! Cargo links a separate test BINARY per top-level `tests/*.rs` file. Each member here was its own
//! `tests/*.rs` file and is now `tests/key_shapes/<name>.rs`, included below as a plain module: the
//! tests, their names, their bodies and their fixtures are byte-unchanged -- only the binary they
//! link into moved. Every member keeps its own module doc whole; read those, not this list, for
//! what each gate proves.
//!
//! - `read_shapes` -- every venue's ROW reads the key names its own bridge reads, plus the
//!   byte-identity guard over the venues the read-side repair did not touch.
//! - `write_shapes` -- every venue's FORM composes the key names its own bridge reads, plus the
//!   editor-to-grid round trip.
//! - `account_status` -- the credential-status grid per ACCOUNT.
//! - `account_grids` -- the panel's account ENUMERATION.
//! - `support` -- the helpers more than one member uses (`REPAIRED`, `vars_of`, `tiers`, `alt`):
//!   one copy each where two members used to carry byte-identical twins. Not merged, on purpose:
//!   each member's frozen `baseline` module (independent computations are what keep each
//!   comparison a comparison), the two `row` helpers (different signatures), and
//!   `account_grids`' `vars` (the same body as `vars_of` under the name its test bodies call).
//!
//! ```sh
//! cargo test -p vike-connections --test key_shapes                   # the whole group
//! cargo test -p vike-connections --test key_shapes -- account_grids  # one member, by module name
//! ```
//!
//! ⚠ **These stay INTEGRATION tests: none may move into a `#[cfg(test)]` block under `src/`.**
//! Every fixture spells a LABELLED or COMPOSED credential key name that
//! `vike_ops::settings::SETTINGS` deliberately has no row for, and the settings-registry gate
//! harvests env-var-shaped literals out of `src/` alone -- a `tests/` file is a test region it does
//! not sweep. Each member's own doc carries the full argument.
//!
//! The grouping rule is `crates/vike-backtest/CLAUDE.md`'s: every member is plain -- no `#![cfg]`
//! feature gate, no `#[ignore]`d test, no `proptest` sidecar, no process-global mutation --
//! because plain `cargo test` runs one binary's tests as THREADS in one process. All four are pure
//! functions of an in-memory credential map: no store on disk, no egui context, no environment.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod read_shapes;` would resolve
// against `tests/` (the root's own directory), not `tests/key_shapes/`.
#[path = "key_shapes/account_grids.rs"]
mod account_grids;
#[path = "key_shapes/account_status.rs"]
mod account_status;
#[path = "key_shapes/read_shapes.rs"]
mod read_shapes;
#[path = "key_shapes/support.rs"]
mod support;
#[path = "key_shapes/write_shapes.rs"]
mod write_shapes;
