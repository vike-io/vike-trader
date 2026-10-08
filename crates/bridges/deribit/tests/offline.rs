//! `offline` -- vike-deribit's plain offline suite: ONE test binary over what used to be five.
//!
//! Cargo links a separate test BINARY per top-level `tests/*.rs` file. Each former
//! `tests/<name>.rs` member here is now `tests/offline/<name>.rs`, included below as a plain
//! module: the tests, their names and their bodies are byte-unchanged (`deribit_klines`'s
//! `include_str!` fixture path gained one `../`, because that macro is source-file-relative) --
//! only the binary they link into moved.
//!
//! ```sh
//! cargo test -p vike-deribit --test offline                   # the whole group
//! cargo test -p vike-deribit --test offline -- combo_submit   # one member, by module name
//! ```
//!
//! The grouping rule is `crates/vike-backtest/CLAUDE.md`'s: every member is plain -- no `#![cfg]`
//! feature gate, no `#[ignore]`d test, no `proptest` sidecar, no process-global mutation --
//! because plain `cargo test` runs one binary's tests as THREADS in one process. The `#[ignore]`d
//! live smokes/soaks/probes stay their own binaries.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod combo_submit;` would
// resolve against `tests/` (the root's own directory), not `tests/offline/`.
#[path = "offline/combo_fill_fixtures.rs"]
mod combo_fill_fixtures;
#[path = "offline/combo_submit.rs"]
mod combo_submit;
#[path = "offline/deribit_klines.rs"]
mod deribit_klines;
#[path = "offline/exec_ambiguous_submit.rs"]
mod exec_ambiguous_submit;
#[path = "offline/exec_resync_redial.rs"]
mod exec_resync_redial;
#[path = "offline/fake_deribit_ws.rs"]
mod fake_deribit_ws;
#[path = "offline/r6_deribit_parity.rs"]
mod r6_deribit_parity;
#[path = "offline/recon_client_parse.rs"]
mod recon_client_parse;
#[path = "offline/recon_client_redial.rs"]
mod recon_client_redial;
#[path = "offline/trade_id_gate.rs"]
mod trade_id_gate;
