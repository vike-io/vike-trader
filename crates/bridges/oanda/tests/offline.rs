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
//! deliberately stay their own binaries: the `decoder_never_panics` property suite and the three
//! `#[ignore]`d live smokes, `oanda_demo_smoke`, `oanda_reconcile_smoke` and `oanda_unit_semantics_smoke`.
//!
//! The ONE process-wide write is the oanda kept list a member of a name holds ([`kept_list_for_tests`]): every
//! member that holds it holds these SAME rows, so the order its threads run in changes nothing.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod candles;` would resolve
// against `tests/` (the root's own directory), not `tests/offline/`.
#[path = "offline/candles.rs"]
mod candles;
#[path = "offline/config_env.rs"]
mod config_env;
#[path = "offline/engine_fill.rs"]
mod engine_fill;
#[path = "offline/execution_actor_wiring.rs"]
mod execution_actor_wiring;
#[path = "offline/execution_body_and_mapping.rs"]
mod execution_body_and_mapping;
#[path = "offline/history_token.rs"]
mod history_token;
#[path = "offline/id_less_fills_are_refused.rs"]
mod id_less_fills_are_refused;
#[path = "offline/oanda_conversion_cost.rs"]
mod oanda_conversion_cost;
#[path = "offline/oanda_reconcile_admitted.rs"]
mod oanda_reconcile_admitted;
#[path = "offline/oanda_reconcile_parse.rs"]
mod oanda_reconcile_parse;
#[path = "offline/practice_instrument_list.rs"]
mod practice_instrument_list;
#[path = "offline/pricing_stream.rs"]
mod pricing_stream;
#[path = "offline/transaction_stream.rs"]
mod transaction_stream;

/// **Hold this binary's oanda kept list** (`vike_catalog::hold_kept_list`, process-wide; decision 0290): the
/// practice account's CAPTURED list and the docs-shaped fixture's metal and CFDs, parsed by the bridge's own
/// `vike_oanda::parse_instruments`, so the user name `EURUSD` is the wire `EUR_USD` and `SPX500.CFD` is `SPX500_USD`.
/// A member whose path names an instrument calls it first; a second call changes nothing.
fn kept_list_for_tests() {
    let parse = |body: &str| {
        vike_oanda::parse_instruments(&serde_json::from_str(body).expect("a fixture is JSON"))
    };
    let mut rows = parse(include_str!("fixtures/practice_instruments_2026-10-26.json"));
    rows.extend(
        parse(include_str!("fixtures/instruments.json"))
            .into_iter()
            .filter(|row| row.contract_type.as_deref() != Some("CURRENCY")),
    );
    vike_catalog::hold_kept_list("oanda", rows);
}
