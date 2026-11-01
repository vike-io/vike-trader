//! `trade` -- vike-gatewayhub-client's trade-half suites over the fake trade peer (feature `trade`): ONE test binary over what used to be 10.
//!
//! Cargo links a separate test BINARY per top-level `tests/*.rs` file, and every one of them links this
//! crate's whole dependency graph. Each former `tests/<name>.rs` member here is now
//! `tests/trade/<name>.rs`, included below as a plain module: its tests, bodies and module doc are
//! unchanged, and only the binary it links into moved (nextest names a test `trade <name>::<test>`).
//!
//! The grouping rule is `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": no member sets
//! process-global state, carries an `#[ignore]`d live or heavy test or a proptest regression file.
//! The `#![cfg(feature = "trade")]` below is the sanctioned GROUP-level gate: every member carries that IDENTICAL
//! gate, so a build without it compiles an empty binary and reads no member, as each was before.
#![cfg(feature = "trade")]

// The helpers several members share, compiled once here.
#[path = "trade/support.rs"]
mod support;

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod x;` would resolve against
// `tests/`, not `tests/trade/`.
#[path = "trade/cursor_resume.rs"]
mod cursor_resume;
#[path = "trade/halt_boundary.rs"]
mod halt_boundary;
#[path = "trade/lease_takeover.rs"]
mod lease_takeover;
#[path = "trade/link_disclosure.rs"]
mod link_disclosure;
#[path = "trade/never_sent.rs"]
mod never_sent;
#[path = "trade/recon_not_empty.rs"]
mod recon_not_empty;
#[path = "trade/told_symbols.rs"]
mod told_symbols;
#[path = "trade/trade_contract.rs"]
mod trade_contract;
#[path = "trade/trade_text_gate.rs"]
mod trade_text_gate;
#[path = "trade/unknown_resolution.rs"]
mod unknown_resolution;
