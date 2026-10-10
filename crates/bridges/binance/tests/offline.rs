//! `offline` -- vike-binance's plain offline suite: ONE test binary over what used to be eight.
//!
//! Cargo links a separate test BINARY per top-level `tests/*.rs` file. Each former
//! `tests/<name>.rs` member here is now `tests/offline/<name>.rs`, included below as a plain
//! module: the tests, their names, their bodies and their fixtures are byte-unchanged -- only the
//! binary they link into moved. Run one member's tests exactly as before, with `offline` in the
//! `--test` slot:
//!
//! ```sh
//! cargo test -p vike-binance --test offline               # the whole group
//! cargo test -p vike-binance --test offline -- tif_gate   # one member, filtered by module name
//! ```
//!
//! The grouping rule is `crates/vike-backtest/CLAUDE.md`'s: every member is plain -- no `#![cfg]`
//! feature gate, no `#[ignore]`d live/heavy test, no `proptest` regression sidecar, no
//! process-global mutation (`env::set_var`/`set_current_dir`) -- because the feature lanes run
//! plain `cargo test`, whose harness runs one binary's tests as THREADS in one process. That is
//! why the `#[ignore]`d live smokes and soaks stay their own binaries, and why
//! `captured_wire_replay.rs` (its fixture-refresh arm is `#[ignore]`d) stays out too.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod tif_gate;` would resolve
// against `tests/` (the root's own directory), not `tests/offline/`.
#[path = "offline/amend_semantics_scope.rs"]
mod amend_semantics_scope;
#[path = "offline/key_permissions_parse.rs"]
mod key_permissions_parse;
#[path = "offline/r6_binance_parity.rs"]
mod r6_binance_parity;
#[path = "offline/r6_binance_perp_parity.rs"]
mod r6_binance_perp_parity;
#[path = "offline/r6_binance_userdata.rs"]
mod r6_binance_userdata;
#[path = "offline/recon_client_parse.rs"]
mod recon_client_parse;
#[path = "offline/tif_gate.rs"]
mod tif_gate;
#[path = "offline/trade_id_gate.rs"]
mod trade_id_gate;
#[path = "offline/trigger_gate.rs"]
mod trigger_gate;
