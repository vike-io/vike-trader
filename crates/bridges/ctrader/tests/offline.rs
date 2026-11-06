//! `offline` -- vike-ctrader's offline suite: ONE test binary over every test that needs no venue --
//! the pure framing/oauth/config/mapper/parse units and the suites that drive the in-process fake
//! server (`vike_ctrader::test_support`, behind the `test-support` feature, plaintext on loopback).
//!
//! Cargo links a separate test BINARY per top-level `tests/*.rs` file, and linking is what a test
//! binary costs. Each former `tests/<name>.rs` member here is now `tests/offline/<name>.rs`,
//! included below as a plain module: the tests, their names and their bodies are byte-unchanged --
//! only the binary they link into moved, so a test's id is `<name>::<test>`.
//!
//! ```sh
//! cargo test -p vike-ctrader --test offline               # the whole group
//! cargo test -p vike-ctrader --test offline -- exec_halt  # one member, by module name
//! ```
//!
//! The grouping rule is `crates/vike-backtest/CLAUDE.md`'s: every member is plain -- no `#![cfg]`,
//! no `#[ignore]`, no proptest sidecar, no process-global mutation -- because plain `cargo test`
//! runs one binary's tests as THREADS in one process. Each fake server binds its own ephemeral port,
//! and each halt sentinel is a path unique per call, so no two members share a socket or a file.
//!
//! What stays its OWN binary: the two property suites (`tests/decoder_never_panics.rs`,
//! `tests/framing_props.rs`), whose failure seeds proptest keys by their file, and every live
//! `*_smoke.rs`, which `.github/workflows/live-smokes.yml` and `docs/ops/live-smoke-lane.md` run by
//! `--test <name>`. (The fake-server consumers and `exec_halt` were kept apart while the shared
//! helpers carried a self-re-exec harness; it left with decision 0099, and so did the reason.)

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod framing;` would resolve
// against `tests/` (the root's own directory), not `tests/offline/`.
#[path = "offline/catalog_capture.rs"]
mod catalog_capture;
#[path = "offline/cfd_units.rs"]
mod cfd_units;
#[path = "offline/client_adapter.rs"]
mod client_adapter;
#[path = "offline/config_env.rs"]
mod config_env;
#[path = "offline/conn_reconnect.rs"]
mod conn_reconnect;
#[path = "offline/conn_retry.rs"]
mod conn_retry;
#[path = "offline/connect_orphans.rs"]
mod connect_orphans;
#[path = "offline/data_bars.rs"]
mod data_bars;
#[path = "offline/data_holds.rs"]
mod data_holds;
#[path = "offline/data_mapper.rs"]
mod data_mapper;
#[path = "offline/data_spot.rs"]
mod data_spot;
#[path = "offline/exec.rs"]
mod exec;
#[path = "offline/exec_amend.rs"]
mod exec_amend;
#[path = "offline/exec_bound_wire.rs"]
mod exec_bound_wire;
#[path = "offline/exec_close.rs"]
mod exec_close;
#[path = "offline/exec_halt.rs"]
mod exec_halt;
#[path = "offline/exec_mapper.rs"]
mod exec_mapper;
#[path = "offline/exec_reject.rs"]
mod exec_reject;
#[path = "offline/exec_resting.rs"]
mod exec_resting;
#[path = "offline/exec_symbol_state.rs"]
mod exec_symbol_state;
#[path = "offline/framing.rs"]
mod framing;
#[path = "offline/handshake.rs"]
mod handshake;
#[path = "offline/history_wire.rs"]
mod history_wire;
#[path = "offline/klines.rs"]
mod klines;
#[path = "offline/oauth.rs"]
mod oauth;
#[path = "offline/quote_rate.rs"]
mod quote_rate;
#[path = "offline/recon_admitted.rs"]
mod recon_admitted;
#[path = "offline/recon_balance.rs"]
mod recon_balance;
#[path = "offline/recon_client_parse.rs"]
mod recon_client_parse;
#[path = "offline/recon_client_revive.rs"]
mod recon_client_revive;
#[path = "offline/recon_position_keys.rs"]
mod recon_position_keys;
#[path = "offline/shared_grant.rs"]
mod shared_grant;
#[path = "offline/symbol_state.rs"]
mod symbol_state;
#[path = "offline/token_rotation_journal.rs"]
mod token_rotation_journal;
