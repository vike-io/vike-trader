// Shared by `account_lifecycle.rs` and `venue_table.rs`, which cargo compiles as INDEPENDENT test
// binaries. Each uses a subset of `Fixture`'s methods, so per-binary dead-code analysis flags the
// rest — expected, and the same rationale `crates/bridges/ctrader/tests/common/mod.rs` carries for
// the same shape.
#![allow(dead_code)]
// The same per-binary subset argument covers the re-exports below: a binary that uses none of a
// child's names would otherwise warn on the `pub use` that makes them reachable at `support::X`.
#![allow(unused_imports)]
//! **Shared integration-test fixture** — extracted from `tests/account_lifecycle.rs` so a second
//! test binary (`tests/venue_table.rs`) can build the same seeded store without a second copy of
//! it to keep in step. A `tests/support/mod.rs` file is not itself a test target — `cargo test`
//! only builds binaries for files directly under `tests/`, so each binary that needs this fixture
//! declares `mod support;` and gets its own compiled copy, the ordinary shape for shared test code.
//!
//! Moved verbatim; nothing here was "improved" on the way out of `account_lifecycle.rs`.

pub mod classify;
pub mod fixture;
pub mod sql;

pub use classify::{
    Matching, Rule, classifier, classifier_over, classify, classify_by, classify_over, fake_value,
    fake_value_folding_sandbox, is_node_key, node_keys_in,
};
pub use fixture::{FIXTURE_KEYS, Fixture, fake_rows, key_names};
