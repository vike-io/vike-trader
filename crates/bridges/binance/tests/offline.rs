//! `offline` -- vike-binance's plain offline suite: ONE test binary over what used to be ten.
//!
//! Cargo links a separate test BINARY per top-level `tests/*.rs` file. Each former
//! `tests/<name>.rs` member here is now `tests/offline/<name>.rs`, included below as a plain
//! module: the tests, their names, their bodies and their fixtures are byte-unchanged -- only the
//! binary they link into moved. Run one member's tests exactly as before, with `offline` in the
//! `--test` slot:
//!
//! ```sh
//! cargo test -p vike-binance --test offline               # the whole group
//! cargo test -p vike-binance --test offline -- time_in_force_gate   # one member, filtered by module name
//! ```
//!
//! The grouping rule is `crates/vike-backtest/CLAUDE.md`'s: every member is plain -- no `#![cfg]`
//! feature gate, no `#[ignore]`d live/heavy test, no `proptest` regression sidecar, no
//! process-global mutation (`env::set_var`/`set_current_dir`) -- because the feature lanes run
//! plain `cargo test`, whose harness runs one binary's tests as THREADS in one process. That is
//! why the `#[ignore]`d live smokes and soaks stay out of this group (modules of `tests/integration.rs` or of
//! `tests/binance_smokes/`, or a binary of their own where `tests/integration.rs` says why). `captured_wire_replay` is a
//! member: it has no `#[ignore]` (the fixture refresh is the capture smoke's) and shares this
//! binary's `exec` requirement.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod time_in_force_gate;` would resolve
// against `tests/` (the root's own directory), not `tests/offline/`.
#[path = "offline/algo_old_stop.rs"]
mod algo_old_stop;
#[path = "offline/algo_orders.rs"]
mod algo_orders;
#[path = "offline/amend_semantics_scope.rs"]
mod amend_semantics_scope;
#[path = "offline/cancel_umbrella.rs"]
mod cancel_umbrella;
#[path = "offline/captured_wire_replay.rs"]
mod captured_wire_replay;
#[path = "offline/coin_m_feed.rs"]
mod coin_m_feed;
#[path = "offline/coin_m_reads.rs"]
mod coin_m_reads;
#[path = "offline/confirm_by_order_client_id.rs"]
mod confirm_by_order_client_id;
#[path = "offline/delivery_gate.rs"]
mod delivery_gate;
#[path = "offline/dial_bound.rs"]
mod dial_bound;
#[path = "offline/family_wire_pins.rs"]
mod family_wire_pins;
#[path = "offline/fee_not_read.rs"]
mod fee_not_read;
#[path = "offline/fill_pages.rs"]
mod fill_pages;
#[path = "offline/fills.rs"]
mod fills;
#[path = "offline/foreign_broker_prefix.rs"]
mod foreign_broker_prefix;
#[path = "offline/funding_income.rs"]
mod funding_income;
#[path = "offline/futures_stream_paths.rs"]
mod futures_stream_paths;
#[path = "offline/key_permissions_parse.rs"]
mod key_permissions_parse;
#[path = "offline/kline_fixture.rs"]
mod kline_fixture;
#[path = "offline/missing_fields.rs"]
mod missing_fields;
#[path = "offline/order_words.rs"]
mod order_words;
#[path = "offline/per_symbol.rs"]
mod per_symbol;
#[path = "offline/perp_fill_wallet.rs"]
mod perp_fill_wallet;
#[path = "offline/plain_fill_naming.rs"]
mod plain_fill_naming;
#[path = "offline/r6_binance_parity.rs"]
mod r6_binance_parity;
#[path = "offline/r6_binance_perp_parity.rs"]
mod r6_binance_perp_parity;
#[path = "offline/r6_binance_userdata.rs"]
mod r6_binance_userdata;
#[path = "offline/reconcile_admitted.rs"]
mod reconcile_admitted;
#[path = "offline/reconcile_client_parse.rs"]
mod reconcile_client_parse;
#[path = "offline/resync_admitted.rs"]
mod resync_admitted;
#[path = "offline/signer_clock.rs"]
mod signer_clock;
#[path = "offline/spot_fee_in_quote.rs"]
mod spot_fee_in_quote;
#[path = "offline/spot_modify.rs"]
mod spot_modify;
#[path = "offline/submit_status_unknown.rs"]
mod submit_status_unknown;
#[path = "offline/time_in_force_gate.rs"]
mod time_in_force_gate;
#[path = "offline/trade_id_gate.rs"]
mod trade_id_gate;
#[path = "offline/trigger_gate.rs"]
mod trigger_gate;
