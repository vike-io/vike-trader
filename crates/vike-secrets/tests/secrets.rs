//! **Every integration suite of this crate but one, ONE test binary** (`--test secrets`): the
//! `store`, `accounts`, `reads` and `gates` groups, each a folder whose `mod.rs` declares its
//! children.
//!
//! One binary so that `tests/support/` compiles ONCE and the crate links one test binary, per
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" section: as four group binaries,
//! the standalone read-only-store proof and the two credential-door suites, five of the seven
//! compiled their own copy of `support`. A test id is its module path, so a substring filter selects
//! a group (`cargo test -p vike-secrets --test secrets -- accounts::`). No member sets the process
//! environment or working directory; `store::read_only_store` keeps its own `#![cfg(unix)]`.
//! `tests/settings_store_ddl_gate.rs` stays its own binary: the public mirror withholds it whole.

mod support;

mod accounts;
mod gates;
mod reads;
mod store;
