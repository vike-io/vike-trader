// Seven items here have NO caller in any suite (`FIXTURE_ACCOUNTS` and `test_proc` as re-exports,
// `sql.rs`'s `object_sql`, `is_not_null`, `indexes`, `indexes_with_auto`, and `index_names`, the body
// those two share): the per-binary `allow`s this file carried hid that. Kept as they were by the move into one binary; deleting them is a
// separate change, and this `expect` fails the day they gain a caller or go.
#![expect(dead_code, unused_imports)]
//! **Shared integration-test fixture** — extracted from `tests/account_lifecycle.rs` so a second
//! suite (`tests/venue_table.rs`, now `accounts::venue_table`) could build the same seeded store
//! without a second copy of it to keep in step. Every suite that uses it is a module of the ONE
//! `secrets` binary (`crates/vike-secrets/tests/secrets.rs`), which declares it once, so it compiles
//! once.
//!
//! Moved verbatim; nothing here was "improved" on the way out of `account_lifecycle.rs`.

pub mod fixture;
pub mod sql;
pub mod store;
pub mod values;

pub use fixture::{FIXTURE_ACCOUNTS, FIXTURE_DEPLOYMENT_KEY, Fixture, fake_rows, key_names};
pub use store::{
    account_journal, credential_journal, scoped_store_in, store_from, store_in, test_proc,
};
pub use values::{fake_value, fake_value_folding_sandbox, hub_keys_in, is_hub_key};
