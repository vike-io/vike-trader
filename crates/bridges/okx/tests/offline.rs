//! `offline` -- vike-okx's plain offline suite: ONE test binary over what used to be three.
//!
//! Cargo links a separate test BINARY per top-level `tests/*.rs` file. Each former
//! `tests/<name>.rs` member here is now `tests/offline/<name>.rs`, included below as a plain
//! module: the tests, their names, their bodies and their fixtures are byte-unchanged -- only the
//! binary they link into moved.
//!
//! ```sh
//! cargo test -p vike-okx --test offline                    # the whole group
//! cargo test -p vike-okx --test offline -- r6_okx_parity   # one member, by module name
//! ```
//!
//! The grouping rule is `crates/vike-backtest/CLAUDE.md`'s: every member is plain -- no `#![cfg]`
//! feature gate, no `#[ignore]`d test, no `proptest` sidecar, no process-global mutation --
//! because plain `cargo test` runs one binary's tests as THREADS in one process. The `#[ignore]`d
//! live smokes/soaks/checks stay their own binaries.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod tif_gate;` would resolve
// against `tests/` (the root's own directory), not `tests/offline/`.
#[path = "offline/r6_okx_parity.rs"]
mod r6_okx_parity;
#[path = "offline/recon_client_parse.rs"]
mod recon_client_parse;
#[path = "offline/tif_gate.rs"]
mod tif_gate;
#[path = "offline/trade_id_gate.rs"]
mod trade_id_gate;
