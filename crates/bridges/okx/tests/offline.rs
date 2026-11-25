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
//! because plain `cargo test` runs one binary's tests as THREADS in one process. The one global a test sets is the
//! kept okx list, and every test sets the same one ([`user`]). The `#[ignore]`d
//! live smokes/soaks/checks stay out of this group (modules of `tests/integration.rs` or of `tests/okx_smokes/`, or a
//! binary of their own where `tests/integration.rs` says why).

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod time_in_force_gate;` would resolve
// against `tests/` (the root's own directory), not `tests/offline/`.
#[path = "offline/algo_amend.rs"]
mod algo_amend;
#[path = "offline/algo_channel.rs"]
mod algo_channel;
#[path = "offline/algo_stop.rs"]
mod algo_stop;
#[path = "offline/amend_result.rs"]
mod amend_result;
#[path = "offline/cancel_requested.rs"]
mod cancel_requested;
#[path = "offline/claim_prereqs.rs"]
mod claim_prereqs;
#[path = "offline/claim_restart.rs"]
mod claim_restart;
#[path = "offline/classes.rs"]
mod classes;
#[path = "offline/confirm_by_order_client_id.rs"]
mod confirm_by_order_client_id;
#[path = "offline/dial_bound.rs"]
mod dial_bound;
#[path = "offline/fee_not_read.rs"]
mod fee_not_read;
#[path = "offline/fill_pages.rs"]
mod fill_pages;
#[path = "offline/instrument_contract.rs"]
mod instrument_contract;
#[path = "offline/inverse_mount.rs"]
mod inverse_mount;
#[path = "offline/market_live_row.rs"]
mod market_live_row;
#[path = "offline/missing_fields.rs"]
mod missing_fields;
#[path = "offline/orders_pending_pages.rs"]
mod orders_pending_pages;
#[path = "offline/per_symbol.rs"]
mod per_symbol;
#[path = "offline/r6_okx_parity.rs"]
mod r6_okx_parity;
#[path = "offline/reconcile_admitted.rs"]
mod reconcile_admitted;
#[path = "offline/reconcile_client_parse.rs"]
mod reconcile_client_parse;
#[path = "offline/resync_admitted.rs"]
mod resync_admitted;
#[path = "offline/signer_clock.rs"]
mod signer_clock;
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
#[path = "offline/trailing.rs"]
mod trailing;
#[path = "offline/triggered_stop.rs"]
mod triggered_stop;
#[path = "offline/unreadable_reply.rs"]
mod unreadable_reply;

/// **The USER name of an okx `inst` id** (decision 0290: `BTCUSDT.P` for `BTC-USDT-SWAP`), off the kept list every test
/// of this binary holds alike (`vike_okx::instrument::kept_list_for_tests`, held here first): the name the engine
/// admits an instrument by and a report leaves the bridge under.
fn user(inst: &str) -> String {
    vike_okx::instrument::kept_list_for_tests();
    vike_catalog::kept_user_name_of_raw("okx", &inst.to_ascii_uppercase())
        .unwrap_or_else(|| panic!("no kept okx row of {inst}"))
}
