//! `data` -- vike-gatewayhub-client's data-half suites over the fake peer (feature `data`): ONE test binary over what used to be 8.
//!
//! Cargo links a separate test BINARY per top-level `tests/*.rs` file, and every one of them links this
//! crate's whole dependency graph. Each former `tests/<name>.rs` member here is now
//! `tests/data/<name>.rs`, included below as a plain module: its tests, bodies and module doc are
//! unchanged, and only the binary it links into moved (nextest names a test `data <name>::<test>`).
//!
//! The grouping rule is `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": no member sets
//! process-global state, carries an `#[ignore]`d live or heavy test or a proptest regression file.
//! The `#![cfg(feature = "data")]` below is the sanctioned GROUP-level gate: every member carries that IDENTICAL
//! gate, so a build without it compiles an empty binary and reads no member, as each was before.
#![cfg(feature = "data")]

// The helpers several members share, compiled once here.
#[path = "data/support.rs"]
mod support;

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod x;` would resolve against
// `tests/`, not `tests/data/`.
#[path = "data/dataclient_contract.rs"]
mod dataclient_contract;
#[path = "data/delivery_class.rs"]
mod delivery_class;
#[path = "data/errors_are_not_empty.rs"]
mod errors_are_not_empty;
#[path = "data/feature_refusal.rs"]
mod feature_refusal;
#[path = "data/fetch_paging.rs"]
mod fetch_paging;
#[path = "data/link_loss.rs"]
mod link_loss;
#[path = "data/remote_seams.rs"]
mod remote_seams;
#[path = "data/session_edge_cases.rs"]
mod session_edge_cases;
