//! The writer's pass over the DataFusion-free store double and a fake catalog table.

use std::sync::atomic::{AtomicUsize, Ordering};

use vike_data::{HistoryStoreMemory, TimestampRange};
use vike_marketdata::Bar;
use vike_model::{AssetClass, Contract, Margining};

use super::*;
use crate::catalog::{CatalogFn, CatalogTable};

const NOW_MS: i64 = 1_700_000_000_000;

fn grid() -> SymbolProperties {
    SymbolProperties {
        tick_size: 0.1,
        step_size: 0.001,
        min_qty: 0.001,
        max_qty: 1000.0,
        min_notional: 5.0,
        ..Default::default()
    }
}

fn row(symbol: &str, properties: SymbolProperties) -> Instrument {
    Instrument::new("binance", symbol, "BTC", "USDT", Contract::CryptoSpot)
        .with_properties(properties)
}

/// One venue's table over `answer`, and the count of how many times the venue was asked.
fn lane_of(
    venue: &str,
    answer: impl Fn() -> Result<Vec<Instrument>, String> + Send + Sync + 'static,
) -> (CatalogLane, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&asked);
    let provider: CatalogFn = Box::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
        answer().map_err(Into::into)
    });
    let table = CatalogTable::new(vec![(venue.to_string(), provider)]);
    (CatalogLane::new(table, CATALOG_TTL), asked)
}

fn binance_lane(
    answer: impl Fn() -> Result<Vec<Instrument>, String> + Send + Sync + 'static,
) -> (CatalogLane, Arc<AtomicUsize>) {
    lane_of("binance", answer)
}

/// A store holding one bar of `symbol`: what a run could trade.
fn store_holding(symbol: &str) -> Arc<HistoryStoreMemory> {
    store_holding_on("binance", symbol)
}

fn store_holding_on(venue: &str, symbol: &str) -> Arc<HistoryStoreMemory> {
    let store = Arc::new(HistoryStoreMemory::new());
    let bar = Bar {
        timestamp: 0,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    };
    store.append_bars(venue, symbol, "1h", &[bar], None).expect("the double stores bars");
    store
}

fn pass(lane: &CatalogLane, store: &Arc<HistoryStoreMemory>) -> bool {
    let recorder = PropertiesRecorder::new(Arc::clone(store) as _, true);
    record_pass(lane, store.as_ref(), &recorder, NOW_MS)
}

fn rows_of(store: &HistoryStoreMemory, symbol: &str) -> Vec<(i64, SymbolProperties)> {
    store
        .scan_symbol_properties("binance", symbol, TimestampRange::all())
        .expect("the double reads")
}

/// A held symbol's row lands with the grid the venue stated, the class from its contract and the
/// instant it was seen. A symbol the store holds no prices for gets none: a run cannot trade it, and
/// a venue's whole list is thousands of series every `inventory()` would then fold.
#[test]
fn a_pass_records_the_grid_of_a_symbol_the_store_holds_prices_for() {
    let store = store_holding("BTCUSDT");
    let (lane, asked) = binance_lane(|| Ok(vec![row("BTCUSDT", grid()), row("ETHUSDT", grid())]));

    assert!(pass(&lane, &store));

    let rows = rows_of(&store, "BTCUSDT");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0, NOW_MS, "the row's time is when it was seen");
    assert_eq!(rows[0].1, SymbolProperties { asset_class: Some(AssetClass::CryptoSpot), ..grid() });
    assert!(rows_of(&store, "ETHUSDT").is_empty(), "no prices held, no row");
    assert_eq!(asked.load(Ordering::SeqCst), 1);
}

/// Four passes a day lands one row a day, and the memo spares the venue the second ask.
#[test]
fn a_second_pass_on_one_day_adds_no_row_and_asks_the_venue_nothing() {
    let store = store_holding("BTCUSDT");
    let (lane, asked) = binance_lane(|| Ok(vec![row("BTCUSDT", grid())]));

    assert!(pass(&lane, &store));
    assert!(pass(&lane, &store));

    assert_eq!(rows_of(&store, "BTCUSDT").len(), 1);
    assert_eq!(asked.load(Ordering::SeqCst), 1);
}

/// A catalog row that states no tick or no lot is no grid: Polymarket's has a tick and no lot or hold,
/// and a recorded hold of `0` would read as a market with none. Dukascopy's and FXCM's bundled rows
/// state neither. The symbol stays "no record", which is what a run then says.
#[test]
fn a_row_with_no_tick_or_no_lot_is_not_recorded() {
    let store = store_holding("TOKEN");
    let (lane, _) = binance_lane(|| {
        Ok(vec![row(
            "TOKEN",
            SymbolProperties { tick_size: 0.01, min_qty: 5.0, ..Default::default() },
        )])
    });
    assert!(pass(&lane, &store));
    assert!(rows_of(&store, "TOKEN").is_empty());

    let store = store_holding("EURUSD");
    let (lane, _) = binance_lane(|| Ok(vec![row("EURUSD", SymbolProperties::default())]));
    assert!(pass(&lane, &store));
    assert!(rows_of(&store, "EURUSD").is_empty());
}

/// **Every venue of the catalog lane, every shape its list holds.** A fill's size is the BASE coin, so a
/// row is recorded only when its step and minimum count that. The rows are what each venue's own parser
/// builds, read from its catalog module (aster's is `vike_binance::family::catalog`'s). A tick is quote
/// per base on every one, so only the size columns decide.
#[test]
fn a_row_is_recorded_only_when_its_sizes_count_the_coin_a_fill_counts() {
    use vike_gatewayhub_client::catalog::CATALOG_VENUES;
    use vike_marketdata::OptionKind;
    use vike_model::{
        ContractFuture, ContractFutureDelivery, ContractFutureFamily, ContractFx,
        ContractOptionExerciseStyle,
    };
    use vike_model::{ContractOption, ContractPerp, ContractPredictionOutcome};

    let perp = |margining, settle: &str, value_ccy: &str| {
        Contract::Perp(ContractPerp {
            margining,
            settle: settle.to_string(),
            value_currency: Some(value_ccy.to_string()),
            funding_interval_ms: None,
            settling_ms: None,
        })
    };
    let future = |margining, settle: &str, value_ccy: &str| {
        Contract::Future(ContractFuture {
            margining,
            settle: settle.to_string(),
            value_currency: Some(value_ccy.to_string()),
            expiry_ms: NOW_MS * 2,
            tenor: None,
            delivery: ContractFutureDelivery::Unknown,
            family: ContractFutureFamily::Crypto,
        })
    };
    let option = Contract::Option(ContractOption {
        right: OptionKind::Call,
        strike: 100_000.0,
        expiry_ms: NOW_MS * 2,
        style: ContractOptionExerciseStyle::European,
        margining: Margining::Inverse,
        settle: "BTC".to_string(),
        value_currency: Some("BTC".to_string()),
        underlying: None,
    });
    let outcome = Contract::PredictionOutcome(ContractPredictionOutcome {
        condition_id: "0xc".to_string(),
        outcome: "Yes".to_string(),
        token_id: "1".to_string(),
        resolution_ms: None,
        neg_risk: None,
    });
    let linear_usdt = perp(Margining::Linear, "USDT", "BTC");
    let fx = Contract::Fx(ContractFx::default());

    // venue, symbol, contract, (tick, step, min, contract_size), recorded
    let rows: Vec<(&str, &str, Contract, [f64; 4], bool)> = vec![
        ("binance", "BTCUSDT", Contract::CryptoSpot, [0.01, 0.00001, 0.00001, 0.0], true),
        ("binance", "BTCUSDT.P", linear_usdt.clone(), [0.1, 0.001, 0.001, 0.0], true),
        ("aster", "BTCUSDT", Contract::CryptoSpot, [0.01, 0.00001, 0.00001, 0.0], true),
        ("aster", "BTCUSDT.P", linear_usdt.clone(), [0.1, 0.001, 0.001, 0.0], true),
        ("bybit", "BTCUSDT", Contract::CryptoSpot, [0.1, 0.000001, 0.000048, 0.0], true),
        ("bybit", "BTCUSDT.P", linear_usdt.clone(), [0.1, 0.001, 0.001, 0.0], true),
        // Counts US dollars, and states no `contract_size` to say so: `Margining` alone tells.
        ("bybit", "BTCUSD.P", perp(Margining::Inverse, "BTC", "USD"), [0.5, 1.0, 1.0, 0.0], false),
        ("okx", "BTC-USDT", Contract::CryptoSpot, [0.1, 0.00000001, 0.00001, 0.0], true),
        // `lotSz` and `minSz` count contracts of `ctVal` 0.01 BTC: the venue's step is 0.0001 BTC.
        ("okx", "BTC-USDT-SWAP", linear_usdt.clone(), [0.1, 0.01, 0.01, 0.01], false),
        (
            "okx",
            "BTC-USDT-261225",
            future(Margining::Linear, "USDT", "BTC"),
            [0.1, 0.01, 0.01, 0.01],
            false,
        ),
        (
            "okx",
            "BTC-USD-SWAP",
            perp(Margining::Inverse, "BTC", "USD"),
            [0.1, 1.0, 1.0, 100.0],
            false,
        ),
        (
            "okx",
            "BTC-USD-261225",
            future(Margining::Inverse, "BTC", "USD"),
            [0.1, 1.0, 1.0, 100.0],
            false,
        ),
        ("okx", "BTC-USD-261225-100000-C", option, [0.0001, 1.0, 1.0, 0.01], false),
        (
            "hyperliquid",
            "BTC",
            perp(Margining::Linear, "USDC", "BTC"),
            [0.1, 0.00001, 0.00001, 0.0],
            true,
        ),
        ("hyperliquid", "PURR/USDC", Contract::CryptoSpot, [0.00001, 0.1, 0.1, 0.0], true),
        // A tick and no lot, and a hold the list does not state.
        ("polymarket", "1", outcome, [0.01, 0.0, 5.0, 0.0], false),
        // Bundled names, no grid at all.
        ("dukascopy", "EURUSD", fx.clone(), [0.0, 0.0, 0.0, 0.0], false),
        ("fxcm", "EUR/USD", fx.clone(), [0.0, 0.0, 0.0, 0.0], false),
        // The held lists state a grid in units (`10^-tradeUnitsPrecision`); a pass never asks them
        // (`a_venue_whose_rows_are_never_recorded_is_not_asked`).
        ("oanda", "EUR_USD", fx.clone(), [0.00001, 1.0, 1.0, 0.0], true),
        ("ctrader", "EURUSD", fx, [0.00001, 0.01, 0.01, 0.0], true),
    ];

    let unread: Vec<&str> =
        CATALOG_VENUES.into_iter().filter(|v| !rows.iter().any(|r| r.0 == *v)).collect();
    assert!(unread.is_empty(), "a venue of the lane with no row here: {unread:?}");

    for (venue, symbol, contract, [tick_size, step_size, min_qty, contract_size], recorded) in rows
    {
        let properties =
            SymbolProperties { tick_size, step_size, min_qty, contract_size, ..Default::default() };
        let row =
            Instrument::new(venue, symbol, "BTC", "USDT", contract).with_properties(properties);
        let class = row.asset_class();
        let got = grid_of(row);
        if recorded {
            assert_eq!(
                got,
                Some((
                    symbol.to_string(),
                    SymbolProperties { asset_class: Some(class), ..properties }
                )),
                "{venue} {symbol}: recorded as the venue stated it"
            );
        } else {
            assert_eq!(got, None, "{venue} {symbol}: its sizes are not what a fill counts");
        }
    }
}

/// The row from the review, end to end: a pass over a store holding an okx SWAP records nothing, so the
/// symbol stays "no record", which is what a run then says on its report.
#[test]
fn a_swap_counted_in_contracts_stays_no_record() {
    let store = store_holding("BTC-USDT-SWAP");
    let swap = Contract::Perp(vike_model::ContractPerp {
        margining: Margining::Linear,
        settle: "USDT".to_string(),
        value_currency: Some("BTC".to_string()),
        funding_interval_ms: None,
        settling_ms: None,
    });
    let (lane, _) = binance_lane(move || {
        Ok(vec![
            Instrument::new("binance", "BTC-USDT-SWAP", "BTC", "USDT", swap.clone())
                .with_properties(SymbolProperties {
                    tick_size: 0.1,
                    step_size: 0.01,
                    min_qty: 0.01,
                    contract_size: 0.01,
                    ..Default::default()
                }),
        ])
    });

    assert!(pass(&lane, &store), "it answered, so the pass is clean");
    assert!(rows_of(&store, "BTC-USDT-SWAP").is_empty());
}

/// A store that holds nothing on a venue makes no call to it: Polymarket's list is forty pages.
#[test]
fn a_venue_the_store_holds_no_prices_on_is_not_asked() {
    let store = Arc::new(HistoryStoreMemory::new());
    let (lane, asked) = binance_lane(|| Ok(vec![row("BTCUSDT", grid())]));
    assert!(pass(&lane, &store));
    assert_eq!(asked.load(Ordering::SeqCst), 0);
}

/// A venue none of whose rows is ever recorded costs no fetch, even with its prices held: polymarket's
/// walk is up to forty pages a pass, four passes a day, for rows [`grid_of`] drops. oanda's and ctrader's
/// held lists are not asked either: the tape never asked them before the lane served them.
#[test]
fn a_venue_whose_rows_are_never_recorded_is_not_asked() {
    for venue in ["polymarket", "dukascopy", "fxcm", "oanda", "ctrader"] {
        let store = store_holding_on(venue, "TOKEN");
        let (lane, asked) = lane_of(venue, || Ok(Vec::new()));
        assert!(pass(&lane, &store), "{venue}: nothing failed");
        assert_eq!(asked.load(Ordering::SeqCst), 0, "{venue}");
    }
}

/// A venue or a gatewayhub that does not answer writes nothing and asks for the short wait, and the
/// next pass that gets an answer records.
#[test]
fn a_failed_ask_is_not_a_clean_pass_and_the_next_one_records() {
    let store = store_holding("BTCUSDT");
    let calls = AtomicUsize::new(0);
    let (lane, _) = binance_lane(move || {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("the gatewayhub is down".to_string())
        } else {
            Ok(vec![row("BTCUSDT", grid())])
        }
    });

    assert!(!pass(&lane, &store));
    assert!(rows_of(&store, "BTCUSDT").is_empty());

    assert!(pass(&lane, &store));
    assert_eq!(rows_of(&store, "BTCUSDT").len(), 1);
}
