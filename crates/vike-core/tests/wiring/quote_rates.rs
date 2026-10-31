//! The PUBLISHED quote-rate table (multi-currency ledger, phase 1): what a reader of the snapshot
//! sees of the rates behind a foreign-quoted symbol's valuation.
//!
//! `VenueBlock::quote_rates` holds one `QuoteRateView` per quote currency in use (first-use order),
//! `quote_rates_in` names the currency the rates are expressed in (the engine's `quote_asset`), and
//! `contract_values` already carries each symbol's EFFECTIVE value (the venue's unit times the rate).
//! The `stale` flag is judged against the CORE's clock, which these tests step by hand: the core
//! reads `CoreConfig::clock` once per message, so after the clock moves the test sends one harmless
//! command and waits for the snapshot that command publishes, and the row it reads is the verdict
//! at the new instant. An account with no foreign-quoted symbol publishes an empty table.
//!
//! Every rate is a dyadic rational and compared by `to_bits`.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::{CoreConfig, CoreHandle, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Command, CoreSnapshot, DEFAULT_QUOTE_RATE_TTL_MS, QuoteRateView, SymbolAdmission, VenueBlock,
};
use vike_model::{ContractValue, SymbolGrid, TradingState};

use crate::kit::engines::engine_on;
use crate::kit::handle::wait_for_snapshot;

const T0: i64 = 1_000_000;
const JPY_PAIR: &str = "USD_JPY";
const R1: f64 = 1.0 / 128.0;
const R2: f64 = 65.0 / 8192.0;

fn block(snap: &CoreSnapshot) -> &VenueBlock {
    &snap.portfolio.venues[0]
}

/// [`block`] for a wait predicate: the placeholder snapshot the cell holds before the first real
/// build has no venue block at all.
fn try_block(snap: &CoreSnapshot) -> Option<&VenueBlock> {
    snap.portfolio.venues.first()
}

/// A USD core on the sim venue whose clock is the shared `now`.
fn spawn(now: &Arc<AtomicI64>) -> CoreHandle {
    let mut engine = engine_on("sim", "BTCUSDT", RecordingClient::default());
    engine.quote_asset = "USD".into();
    let clock = Arc::clone(now);
    spawn_core(
        engine,
        CoreConfig {
            seed_cash: 10_000.0,
            clock: Box::new(move || clock.load(Ordering::Relaxed)),
            ..CoreConfig::default()
        },
    )
}

/// An admission of `symbol` quoted in `ccy` at `rate`, observed at `as_of_ms`; `base` is the venue's
/// own unit (`None` is the default 1.0).
fn admit(
    symbol: &str,
    ccy: &str,
    rate: f64,
    as_of_ms: i64,
    base: Option<ContractValue>,
) -> Command {
    Command::AdmitSymbol(Box::new(SymbolAdmission {
        route_key: "sim".into(),
        symbol: symbol.into(),
        grid: SymbolGrid { tick_size: Some(0.25), lot_size: Some(1.0), ..SymbolGrid::default() },
        contract_value: base,
        initial_mark: Some(100.0),
        quote_ccy: Some(ccy.into()),
        quote_rate: Some(rate),
        rate_as_of_ms: Some(as_of_ms),
        im_requirement: None,
        release: false,
        margin_mode: None,
    }))
}

/// Move the core clock to `at`, then send one harmless command and wait for the snapshot it
/// publishes (the next `seq`): the engine stamps `now_ms` at that message, and `build` judges every
/// rate against it.
fn advance(now: &Arc<AtomicI64>, handle: &CoreHandle, at: i64) -> Arc<CoreSnapshot> {
    let cell = handle.snapshot_cell();
    let before = cell.load().seq;
    now.store(at, Ordering::Relaxed);
    handle.send_command(Command::SetTradingState(TradingState::Active));
    wait_for_snapshot(&cell, 5, "the snapshot after the clock moved", |s| s.seq > before);
    cell.load_full()
}

#[test]
fn a_yen_symbol_publishes_one_row_that_ages_on_the_core_clock() {
    let now = Arc::new(AtomicI64::new(T0));
    let handle = spawn(&now);
    let cell = handle.snapshot_cell();
    handle.send_command(admit(JPY_PAIR, "JPY", R1, T0, None));
    wait_for_snapshot(&cell, 5, "the JPY row", |s| {
        try_block(s).is_some_and(|b| !b.quote_rates.is_empty())
    });

    let snap = cell.load_full();
    let b = block(&snap);
    assert_eq!(
        b.quote_rates,
        vec![QuoteRateView {
            ccy: "JPY".into(),
            rate: R1,
            as_of_ms: T0,
            stale: false,
            held: false
        }],
        "one row, the rate, when it was observed, and fresh"
    );
    assert_eq!(b.quote_rates[0].rate.to_bits(), R1.to_bits());
    assert_eq!(b.quote_rates_in, "USD", "the rates are account-currency units per yen");
    assert!(!b.quote_rates_stale());
    assert_eq!(
        b.contract_value_of(JPY_PAIR),
        ContractValue::linear(R1),
        "the grid carries the EFFECTIVE value: the venue's unit 1.0 times the rate"
    );
    assert_eq!(b.contract_value_of(JPY_PAIR).per_unit.to_bits(), R1.to_bits());
    assert!(
        b.contract_value_of("BTCUSDT").is_default(),
        "the mounted symbol is quoted in the account's currency: untouched"
    );

    // Exactly at the window's edge the rate is still fresh (the verdict is `age > ttl`)…
    let snap = advance(&now, &handle, T0 + DEFAULT_QUOTE_RATE_TTL_MS);
    assert!(!block(&snap).quote_rates[0].stale, "age == ttl is not stale");
    assert!(!block(&snap).quote_rates_stale());

    // …one millisecond past it, it is stale: the row says so, and so does the one-bit answer.
    let snap = advance(&now, &handle, T0 + DEFAULT_QUOTE_RATE_TTL_MS + 1);
    let b = block(&snap);
    assert!(b.quote_rates[0].stale);
    assert!(b.quote_rates_stale());
    assert_eq!(b.quote_rates[0].rate.to_bits(), R1.to_bits(), "the stale row still names its rate");
    assert_eq!(b.quote_rates[0].as_of_ms, T0, "…and when it was observed");
    assert_eq!(
        b.contract_value_of(JPY_PAIR),
        ContractValue::linear(R1),
        "valuation keeps the last rate: staleness denies openings, it does not zero or re-par a value"
    );
    assert_eq!(b.quote_rates_in, "USD");

    // A renewed rate (a repeat admission, newer) makes the row fresh and re-prices the value.
    let renewed = T0 + DEFAULT_QUOTE_RATE_TTL_MS + 1_000;
    now.store(renewed, Ordering::Relaxed);
    handle.send_command(admit(JPY_PAIR, "JPY", R2, renewed, None));
    wait_for_snapshot(&cell, 5, "the renewed rate", |s| {
        try_block(s).is_some_and(|b| b.quote_rates.first().is_some_and(|r| r.rate == R2))
    });
    let snap = cell.load_full();
    let b = block(&snap);
    assert_eq!(
        b.quote_rates,
        vec![QuoteRateView {
            ccy: "JPY".into(),
            rate: R2,
            as_of_ms: renewed,
            stale: false,
            held: false
        }]
    );
    assert!(!b.quote_rates_stale());
    assert_eq!(b.contract_value_of(JPY_PAIR), ContractValue::linear(R2));
    handle.shutdown_and_join();
}

#[test]
fn an_account_with_no_foreign_quoted_symbol_publishes_an_empty_table() {
    let now = Arc::new(AtomicI64::new(T0));
    let handle = spawn(&now);
    let cell = handle.snapshot_cell();

    // Before anything is admitted…
    let snap = advance(&now, &handle, T0);
    assert!(block(&snap).quote_rates.is_empty());
    assert!(block(&snap).quote_rates_in.is_empty(), "no table, no currency named");
    assert!(!block(&snap).quote_rates_stale());

    // …and with an ordinary symbol admitted (no quote currency, a non-default unit): still empty,
    // and however far the clock runs on, nothing ages because there is nothing to age.
    handle.send_command(Command::AdmitSymbol(Box::new(SymbolAdmission {
        route_key: "sim".into(),
        symbol: "ETHUSDT".into(),
        grid: SymbolGrid { tick_size: Some(0.01), lot_size: Some(0.0001), ..SymbolGrid::default() },
        contract_value: Some(ContractValue::linear(10.0)),
        initial_mark: Some(3000.0),
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: None,
        release: false,
        margin_mode: None,
    })));
    wait_for_snapshot(&cell, 5, "ETH admitted", |s| {
        try_block(s).is_some_and(|b| b.trades("ETHUSDT"))
    });
    let snap = advance(&now, &handle, T0 + 10 * DEFAULT_QUOTE_RATE_TTL_MS);
    let b = block(&snap);
    assert!(b.trades("ETHUSDT"));
    assert!(b.quote_rates.is_empty());
    assert!(b.quote_rates_in.is_empty());
    assert!(!b.quote_rates_stale());
    assert_eq!(b.contract_value_of("ETHUSDT"), ContractValue::linear(10.0));
    handle.shutdown_and_join();
}

/// Two currencies, one row each in first-use order; a refresh through ONE symbol re-prices EVERY
/// symbol quoted in that currency; and each row ages on its own clock.
#[test]
fn rows_are_per_currency_and_a_refresh_reprices_every_symbol_of_the_currency() {
    let now = Arc::new(AtomicI64::new(T0));
    let handle = spawn(&now);
    let cell = handle.snapshot_cell();
    handle.send_command(admit(JPY_PAIR, "JPY", R1, T0, None));
    handle.send_command(admit("GBP_JPY", "JPY", R1, T0, Some(ContractValue::linear(2.0))));
    handle.send_command(admit("EUR_CHF", "CHF", 1.25, T0, None));
    wait_for_snapshot(&cell, 5, "both currencies", |s| {
        try_block(s).is_some_and(|b| b.quote_rates.len() == 2)
    });

    let snap = cell.load_full();
    let b = block(&snap);
    let ccys: Vec<&str> = b.quote_rates.iter().map(|r| r.ccy.as_str()).collect();
    assert_eq!(ccys, ["JPY", "CHF"], "one row per currency, in first-use order");
    assert_eq!(b.contract_value_of(JPY_PAIR), ContractValue::linear(R1));
    assert_eq!(
        b.contract_value_of("GBP_JPY").per_unit.to_bits(),
        (2.0 * R1).to_bits(),
        "a venue unit of 2.0 times the rate"
    );
    assert_eq!(b.contract_value_of("EUR_CHF").per_unit.to_bits(), 1.25_f64.to_bits());

    // Refresh JPY through USD_JPY alone, 1000 ms later: GBP_JPY follows, CHF does not move.
    let later = T0 + 1_000;
    now.store(later, Ordering::Relaxed);
    handle.send_command(admit(JPY_PAIR, "JPY", R2, later, None));
    wait_for_snapshot(&cell, 5, "the JPY refresh", |s| {
        try_block(s).is_some_and(|b| b.quote_rates.first().is_some_and(|r| r.rate == R2))
    });
    let snap = cell.load_full();
    let b = block(&snap);
    assert_eq!(b.contract_value_of(JPY_PAIR).per_unit.to_bits(), R2.to_bits());
    assert_eq!(
        b.contract_value_of("GBP_JPY").per_unit.to_bits(),
        (2.0 * R2).to_bits(),
        "the sibling symbol of the same currency is re-priced by the one refresh"
    );
    assert_eq!(b.contract_value_of("EUR_CHF").per_unit.to_bits(), 1.25_f64.to_bits());
    assert_eq!(b.quote_rates[0].as_of_ms, later);
    assert_eq!(b.quote_rates[1].as_of_ms, T0);

    // Each row has its own age: the yen was renewed 1000 ms after the franc, so just past the
    // franc's window the franc is stale and the yen is not.
    let snap = advance(&now, &handle, T0 + DEFAULT_QUOTE_RATE_TTL_MS + 500);
    let b = block(&snap);
    assert!(!b.quote_rates[0].stale, "JPY is {} ms old", DEFAULT_QUOTE_RATE_TTL_MS - 500);
    assert!(b.quote_rates[1].stale, "CHF is past its window");
    assert!(b.quote_rates_stale(), "any stale row makes the one-bit answer true");
    handle.shutdown_and_join();
}
