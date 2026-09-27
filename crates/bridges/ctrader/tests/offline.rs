//! `offline` -- vike-ctrader's `tests/common/`-free offline suite: ONE test binary over what used
//! to be six (the pure framing/oauth/config/mapper/parse units -- no fake server, no socket).
//!
//! Cargo links a separate test BINARY per top-level `tests/*.rs` file. Each former
//! `tests/<name>.rs` member here is now `tests/offline/<name>.rs`, included below as a plain
//! module: the tests, their names and their bodies are byte-unchanged -- only the binary they
//! link into moved.
//!
//! ```sh
//! cargo test -p vike-ctrader --test offline               # the whole group
//! cargo test -p vike-ctrader --test offline -- framing    # one member, by module name
//! ```
//!
//! The grouping rule is `crates/vike-backtest/CLAUDE.md`'s: every member is plain -- no `#![cfg]`,
//! no `#[ignore]`, no proptest sidecar, no process-global mutation -- because plain `cargo test`
//! runs one binary's tests as THREADS in one process.
//!
//! WARNING NON-GOAL: every `mod common` consumer stays its own binary, deliberately. `tests/common/`'s
//! `assert_indifferent_to_an_engaged_halt_sentinel` is a SELF-RE-EXEC harness -- it re-runs the
//! CURRENT test binary (`current_exe()`) with `--test-threads=1 --skip HALT_INDIFFERENCE`, so
//! folding more tests into a binary that carries a caller changes what the re-exec sub-run
//! executes. `exec_halt.rs`, the halt kill-switch suite, rides the same `mod common` and stays
//! out with the rest.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod framing;` would resolve
// against `tests/` (the root's own directory), not `tests/offline/`.
#[path = "offline/config_env.rs"]
mod config_env;
#[path = "offline/data_mapper.rs"]
mod data_mapper;
#[path = "offline/exec_mapper.rs"]
mod exec_mapper;
#[path = "offline/framing.rs"]
mod framing;
#[path = "offline/oauth.rs"]
mod oauth;
#[path = "offline/recon_client_parse.rs"]
mod recon_client_parse;
