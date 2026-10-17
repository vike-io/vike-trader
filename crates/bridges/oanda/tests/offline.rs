//! `offline` -- vike-oanda's plain offline suite: ONE test binary over what used to be nine.
//!
//! Cargo links a separate test BINARY per top-level `tests/*.rs` file. Each former
//! `tests/<name>.rs` member here is now `tests/offline/<name>.rs`, included below as a plain
//! module: the tests, their names and their bodies are byte-unchanged -- only the binary they
//! link into moved.
//!
//! ```sh
//! cargo test -p vike-oanda --test offline                  # the whole group
//! cargo test -p vike-oanda --test offline -- config_env    # one member, by module name
//! ```
//!
//! The grouping rule is `crates/vike-backtest/CLAUDE.md`'s: every member is plain -- no `#![cfg]`
//! feature gate, no `#[ignore]`d test, no `proptest` sidecar, no process-global mutation --
//! because plain `cargo test` runs one binary's tests as THREADS in one process. So three files
//! deliberately stay their own binaries: the `decoder_never_panics` property suite and the two
//! `#[ignore]`d live smokes, `oanda_demo_smoke` and `oanda_reconcile_smoke`.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod candles;` would resolve
// against `tests/` (the root's own directory), not `tests/offline/`.
#[path = "offline/candles.rs"]
mod candles;
#[path = "offline/config_env.rs"]
mod config_env;
#[path = "offline/exec_actor_wiring.rs"]
mod exec_actor_wiring;
#[path = "offline/exec_body_and_mapping.rs"]
mod exec_body_and_mapping;
#[path = "offline/history_token.rs"]
mod history_token;
#[path = "offline/id_less_fills_are_refused.rs"]
mod id_less_fills_are_refused;
#[path = "offline/oanda_reconcile_parse.rs"]
mod oanda_reconcile_parse;
#[path = "offline/pricing_stream.rs"]
mod pricing_stream;
#[path = "offline/transaction_stream.rs"]
mod transaction_stream;
