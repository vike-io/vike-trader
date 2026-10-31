//! `offline` -- vike-bybit's plain offline suite: ONE test binary over what used to be four.
//!
//! Cargo links a separate test BINARY per top-level `tests/*.rs` file. Each former
//! `tests/<name>.rs` member here is now `tests/offline/<name>.rs`, included below as a plain
//! module: the tests, their names, their bodies and their fixtures are byte-unchanged -- only the
//! binary they link into moved.
//!
//! ```sh
//! cargo test -p vike-bybit --test offline                     # the whole group
//! cargo test -p vike-bybit --test offline -- fast_execution   # one member, by module name
//! ```
//!
//! The grouping rule is `crates/vike-backtest/CLAUDE.md`'s: every member is plain -- no `#![cfg]`
//! feature gate, no `#[ignore]`d test, no `proptest` sidecar, no process-global mutation --
//! because plain `cargo test` runs one binary's tests as THREADS in one process. The `#[ignore]`d
//! live smokes/soaks/probes and `captured_wire_replay.rs` (its fixture-refresh arm is
//! `#[ignore]`d) stay their own binaries.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod tif_gate;` would resolve
// against `tests/` (the root's own directory), not `tests/offline/`.
#[path = "offline/claim_prereqs.rs"]
mod claim_prereqs;
#[path = "offline/classes.rs"]
mod classes;
#[path = "offline/confirm_by_coid.rs"]
mod confirm_by_coid;
#[path = "offline/fast_execution.rs"]
mod fast_execution;
#[path = "offline/fee_not_read.rs"]
mod fee_not_read;
#[path = "offline/market_feed_scripted.rs"]
mod market_feed_scripted;
#[path = "offline/modes.rs"]
mod modes;
#[path = "offline/per_symbol.rs"]
mod per_symbol;
#[path = "offline/r6_bybit_parity.rs"]
mod r6_bybit_parity;
#[path = "offline/recon_admitted.rs"]
mod recon_admitted;
#[path = "offline/recon_client_parse.rs"]
mod recon_client_parse;
#[path = "offline/recon_paging.rs"]
mod recon_paging;
#[path = "offline/resync_admitted.rs"]
mod resync_admitted;
#[path = "offline/single_terminal.rs"]
mod single_terminal;
#[path = "offline/submit_status_unknown.rs"]
mod submit_status_unknown;
#[path = "offline/tif_gate.rs"]
mod tif_gate;
#[path = "offline/trade_id_gate.rs"]
mod trade_id_gate;
