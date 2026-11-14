//! `offline` -- vike-bybit's plain offline suite: ONE test binary over what used to be several.
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
//! live smokes/soaks/probes stay their own binaries. `captured_wire_replay` and `kline_fixture` are
//! members: neither has an `#[ignore]` (the fixture refresh is the capture smoke's).

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod time_in_force_gate;` would resolve
// against `tests/` (the root's own directory), not `tests/offline/`.
#[path = "offline/cancel_already_gone.rs"]
mod cancel_already_gone;
#[path = "offline/captured_wire_replay.rs"]
mod captured_wire_replay;
#[path = "offline/claim_prereqs.rs"]
mod claim_prereqs;
#[path = "offline/classes.rs"]
mod classes;
#[path = "offline/conditional_time_in_force.rs"]
mod conditional_time_in_force;
#[path = "offline/confirm_by_order_client_id.rs"]
mod confirm_by_order_client_id;
#[path = "offline/dial_bound.rs"]
mod dial_bound;
#[path = "offline/fast_execution.rs"]
mod fast_execution;
#[path = "offline/fee_not_read.rs"]
mod fee_not_read;
#[path = "offline/funding_window.rs"]
mod funding_window;
#[path = "offline/inverse_mount.rs"]
mod inverse_mount;
#[path = "offline/kline_fixture.rs"]
mod kline_fixture;
#[path = "offline/market_feed_scripted.rs"]
mod market_feed_scripted;
#[path = "offline/missing_fields.rs"]
mod missing_fields;
#[path = "offline/modes.rs"]
mod modes;
#[path = "offline/one_key_two_accounts.rs"]
mod one_key_two_accounts;
#[path = "offline/per_symbol.rs"]
mod per_symbol;
#[path = "offline/r6_bybit_parity.rs"]
mod r6_bybit_parity;
#[path = "offline/reconcile_admitted.rs"]
mod reconcile_admitted;
#[path = "offline/reconcile_client_parse.rs"]
mod reconcile_client_parse;
#[path = "offline/reconcile_paging.rs"]
mod reconcile_paging;
#[path = "offline/resync_admitted.rs"]
mod resync_admitted;
#[path = "offline/signer_clock.rs"]
mod signer_clock;
#[path = "offline/silent_bracket.rs"]
mod silent_bracket;
#[path = "offline/single_terminal.rs"]
mod single_terminal;
#[path = "offline/spot_conditional.rs"]
mod spot_conditional;
#[path = "offline/spot_fee_account.rs"]
mod spot_fee_account;
#[path = "offline/stop_limit.rs"]
mod stop_limit;
#[path = "offline/submit_status_unknown.rs"]
mod submit_status_unknown;
#[path = "offline/take_profit.rs"]
mod take_profit;
#[path = "offline/time_in_force_gate.rs"]
mod time_in_force_gate;
#[path = "offline/trade_id_gate.rs"]
mod trade_id_gate;
#[path = "offline/trigger_amend.rs"]
mod trigger_amend;
#[path = "offline/unreadable_reply.rs"]
mod unreadable_reply;
