//! `key_shapes` -- vike-connections' credential SHAPE suite, as ONE test binary: what a form
//! writes and what a cell reads, by FIELD, against a REAL settings database.
//!
//! Decision 0156 item 6 took the NAME out of the credential: a credential is an account row's
//! `credential.account_id` + `field`, the tier is the row's, and nothing here composes or parses a
//! name. What this binary still proves is the half of the old suite that never depended on a name:
//! the form writes exactly the fields the loader reads (the venue's row of
//! `vike_model::credential_schema`), and a cell lights exactly when its account row holds them.
//!
//! - `schema_parity` -- every form IS the schema row's form, every required field is offered, and
//!   a Save of the form's fields completes the cell (dropping any ONE required field un-completes
//!   it), round-tripped through a store.
//! - `account_grids` -- the panel's account ENUMERATION and cells, read through a real
//!   `vike_secrets::CredentialStore` handle: labels from rows, presence by field, the stored name a
//!   Remove retires, the venue's application rows, the live-key scope, and a store that would not
//!   open.
//! - `support` -- the store helpers both members use.
//!
//! ```sh
//! cargo test -p vike-connections --test key_shapes                   # the whole group
//! cargo test -p vike-connections --test key_shapes -- account_grids  # one member, by module name
//! ```
//!
//! The grouping rule is `crates/vike-backtest/CLAUDE.md`'s: every member is plain -- no `#![cfg]`
//! feature gate, no `#[ignore]`d test, no `proptest` sidecar, no process-global mutation --
//! because plain `cargo test` runs one binary's tests as THREADS in one process. Every store is a
//! self-deleting `vike_paths::scratch::ScratchDir` of its own.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod account_grids;` would resolve
// against `tests/` (the root's own directory), not `tests/key_shapes/`.
#[path = "key_shapes/account_grids.rs"]
mod account_grids;
#[path = "key_shapes/schema_parity.rs"]
mod schema_parity;
#[path = "key_shapes/support.rs"]
mod support;
