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
//! because plain `cargo test` runs one binary's tests as THREADS in one process. The one global a test sets is the
//! kept deribit list, and every test that sets it sets the same one ([`user`]); every client is built over it
//! (`fake_deribit_websocket::transport_against`), so every test reads USER names. A test that held none and expected
//! the venue's name passed under nextest (a process per test, CI) and failed under plain `cargo test` (a lane) once a
//! sibling held the list. The `#[ignore]`d
//! live smokes/soaks/probes stay out of this group (modules of `tests/integration.rs` or of `tests/deribit_smokes/`).
//!
//! The three `option_*` suites came from `vike-options`'
//! unit tests (2026-10-11): they run this crate's decoders (`catalog::parse_instruments`,
//! `options_feed::parse_ticker` / `parse_markprice_options`) into `vike_options::OptionIndex` and
//! its folds, and live HERE so the layer-15 maths crate names no venue, not even on a dev edge.
//! Test names and bodies are unchanged; only `use` lines and the fixture path moved. One test is
//! new: `option_index_payloads`' USDC-book row (2026-10-11).
//! Their instrument and chain builders are `vike_options::test_support`'s (a dev edge with
//! `test-support`), the ones that crate's own fold tests use.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod combo_submit;` would
// resolve against `tests/` (the root's own directory), not `tests/offline/`.
#[path = "offline/class_admission.rs"]
mod class_admission;
#[path = "offline/combo_fill_fixtures.rs"]
mod combo_fill_fixtures;
#[path = "offline/combo_submit.rs"]
mod combo_submit;
#[path = "offline/crossing_edit.rs"]
mod crossing_edit;
#[path = "offline/deribit_klines.rs"]
mod deribit_klines;
#[path = "offline/dial_bound.rs"]
mod dial_bound;
#[path = "offline/execution_ambiguous_submit.rs"]
mod execution_ambiguous_submit;
#[path = "offline/execution_resync_redial.rs"]
mod execution_resync_redial;
#[path = "offline/fake_deribit_websocket.rs"]
mod fake_deribit_websocket;
#[path = "offline/foreign_instruments.rs"]
mod foreign_instruments;
#[path = "offline/funding.rs"]
mod funding;
#[path = "offline/iceberg_and_market_limit.rs"]
mod iceberg_and_market_limit;
#[path = "offline/native_stops.rs"]
mod native_stops;
#[path = "offline/option_index_payloads.rs"]
mod option_index_payloads;
#[path = "offline/option_markprice_fold.rs"]
mod option_markprice_fold;
#[path = "offline/option_ticker_fold.rs"]
mod option_ticker_fold;
#[path = "offline/order_event_order.rs"]
mod order_event_order;
#[path = "offline/r6_deribit_parity.rs"]
mod r6_deribit_parity;
#[path = "offline/reconcile_client_parse.rs"]
mod reconcile_client_parse;
#[path = "offline/reconcile_client_redial.rs"]
mod reconcile_client_redial;
#[path = "offline/settlement.rs"]
mod settlement;
#[path = "offline/stop_events.rs"]
mod stop_events;
#[path = "offline/trade_id_gate.rs"]
mod trade_id_gate;
#[path = "offline/venue_read.rs"]
mod venue_read;

/// A reduce-only SELL stop of 10 contracts on [`fake_deribit_websocket::SYMBOL`] (named by its USER name, as the
/// engine orders it), id `order_client_id`, triggering at `trigger`, stamped `ts`: the stop `native_stops` and
/// `stop_events` each drive.
fn sell_stop(order_client_id: &str, trigger: f64, ts: i64) -> vike_model::OrderRequest {
    vike_model::OrderRequest {
        order_client_id: order_client_id.to_string(),
        venue: "deribit".to_string(),
        symbol: user(fake_deribit_websocket::SYMBOL),
        side: -1,
        qty: 10.0,
        order_type: vike_model::OrderType::Stop,
        trigger_price: Some(trigger),
        reduce_only: true,
        timestamp: ts,
        ..Default::default()
    }
}

/// **The USER name of a deribit venue name** (decision 0290: `BTCUSD.P` for `BTC-PERPETUAL`), off the kept list
/// `vike_deribit::names::hold_deribit_kept_rows` holds (held here first: every test that holds a list holds that one):
/// the name the engine orders, admits and reads an instrument by.
fn user(venue: &str) -> String {
    vike_deribit::names::hold_deribit_kept_rows();
    vike_catalog::kept_user_name_of_raw("deribit", venue)
        .unwrap_or_else(|| panic!("no kept deribit row of {venue}"))
}

/// **A clock before every dated row the fixtures capture** (2023-11-14): what a client signing on a captured dated
/// future or option judges the delivery guard at (`VenueRestDeribit::with_clock`), so a capture whose day has passed
/// on the wall clock still tests what it was captured for.
fn before_the_captures() -> i64 {
    1_700_000_000_000
}
