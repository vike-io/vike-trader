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
//! cargo test -p vike-ctrader --test offline -- execution_halt  # one member, by module name
//! ```
//!
//! The grouping rule is `crates/vike-backtest/CLAUDE.md`'s: every member is plain -- no `#![cfg]`,
//! no `#[ignore]`, no proptest sidecar, no process-global mutation -- because plain `cargo test`
//! runs one binary's tests as THREADS in one process. Each fake server binds its own ephemeral port,
//! and each halt sentinel is a path unique per call, so no two members share a socket or a file. The one
//! process-wide write is the kept ctrader list a USER name is read through (decision 0290), and every member
//! holds the SAME rows (`vike_ctrader::test_support::kept_list_for_tests`, which reading a fake's address calls),
//! so no two members can disagree about it; a "no list" case is tested on the pure `rows`-taking core.
//!
//! What stays OUT of this group: the two property suites (`tests/decoder_never_panics.rs`,
//! `tests/framing_property_tests.rs`), whose failure seeds proptest keys by their file, and every live
//! `*_smoke.rs`. They are modules of `tests/integration.rs`, but for the two binaries of their own that its module
//! doc names (the lane's `ctrader_reconcile_smoke`, which `.github/workflows/live-smokes.yml` runs by
//! `--test <name>`, and `ctrader_cfd_units_smoke`). (The fake-server consumers and `execution_halt` were kept apart while the shared
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
#[path = "offline/connect_orphans.rs"]
mod connect_orphans;
#[path = "offline/connection_reconnect.rs"]
mod connection_reconnect;
#[path = "offline/connection_retry.rs"]
mod connection_retry;
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
#[path = "offline/execution_amend.rs"]
mod execution_amend;
#[path = "offline/execution_bound_wire.rs"]
mod execution_bound_wire;
#[path = "offline/execution_close.rs"]
mod execution_close;
#[path = "offline/execution_halt.rs"]
mod execution_halt;
#[path = "offline/execution_mapper.rs"]
mod execution_mapper;
#[path = "offline/execution_reject.rs"]
mod execution_reject;
#[path = "offline/execution_resting.rs"]
mod execution_resting;
#[path = "offline/execution_symbol_state.rs"]
mod execution_symbol_state;
#[path = "offline/execution_wire_wait.rs"]
mod execution_wire_wait;
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
#[path = "offline/reconcile_admitted.rs"]
mod reconcile_admitted;
#[path = "offline/reconcile_balance.rs"]
mod reconcile_balance;
#[path = "offline/reconcile_client_parse.rs"]
mod reconcile_client_parse;
#[path = "offline/reconcile_client_revive.rs"]
mod reconcile_client_revive;
#[path = "offline/reconcile_fee_rate.rs"]
mod reconcile_fee_rate;
#[path = "offline/reconcile_position_keys.rs"]
mod reconcile_position_keys;
#[path = "offline/session_lapse.rs"]
mod session_lapse;
#[path = "offline/shared_grant.rs"]
mod shared_grant;
#[path = "offline/symbol_state.rs"]
mod symbol_state;
#[path = "offline/token_rotation_journal.rs"]
mod token_rotation_journal;
#[path = "offline/trendbar_close.rs"]
mod trendbar_close;
#[path = "offline/trendbar_outage.rs"]
mod trendbar_outage;
#[path = "offline/user_names.rs"]
mod user_names;
