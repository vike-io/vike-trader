//! The store `instrument_graph` is proven on, ONE copy for both binaries that run the study
//! (`crates/vike-user-research/tests/pipeline.rs`, directly and across the study file's boundary, and
//! `crates/vike-strategy-sandbox/tests/study_child.rs`, in the real jail). `#[cfg(test)]`: a built
//! study never compiles it.
//!
//! The kept rows: binance's BTC spot pair and perpetual, bybit's and okx's BTC perpetuals, deribit's
//! BTC perpetual (no bars stored), March future and March call, okx's ETH spot pair and two outcome
//! tokens of one Polymarket neg-risk set. The bars: 32 one-minute bars whose returns are the first 31
//! decimals of π, `(digit - 4.5) / 1000`, on binance's perpetual and, the same prices, on its spot
//! pair; bybit's perpetual repeats them one bar later and okx's two bars later. So binance's two books
//! lead both others, bybit leads okx, and neither binance book leads the other. Each spot pair and
//! perpetual stores its user name as a kept list's writer stores it (decision 0290), and its bars are filed
//! under that name's store name: okx's `BTC-USDT-SWAP` is the series `BTCUSDT.P`.

use std::sync::Arc;

use vike_data::{HistoryStore, HistoryStoreMemory, TimestampRange};
use vike_marketdata::{Bar, OptionKind};
use vike_model::{
    Contract, ContractFuture, ContractFutureDelivery, ContractFutureFamily, ContractOption,
    ContractOptionExerciseStyle, ContractPerp, ContractPredictionOutcome, Instrument,
    InstrumentNaming, InstrumentVenueIds, Margining,
};

const MINUTE: i64 = 60_000;
const MARCH: i64 = 1_774_598_400_000;
/// The first 31 decimals of π.
const DIGITS: [u8; 31] =
    [1, 4, 1, 5, 9, 2, 6, 5, 3, 5, 8, 9, 7, 9, 3, 2, 3, 8, 4, 6, 2, 6, 4, 3, 3, 8, 3, 2, 7, 9, 5];

/// `row` storing the user name `user`.
fn named(row: Instrument, user: &str) -> Instrument {
    Instrument { naming: Some(InstrumentNaming::User(user.into())), ..row }
}

fn spot(venue: &str, symbol: &str, base: &str, quote: &str) -> Instrument {
    let user = format!("{base}{quote}");
    named(Instrument::new(venue, symbol, base, quote, Contract::CryptoSpot), &user)
}

fn perp(venue: &str, symbol: &str, base: &str, quote: &str) -> Instrument {
    let contract = Contract::Perp(ContractPerp {
        margining: Margining::Linear,
        settle: quote.into(),
        value_currency: None,
        funding_interval_ms: None,
        settling_ms: None,
    });
    named(Instrument::new(venue, symbol, base, quote, contract), &format!("{base}{quote}.P"))
}

fn outcome(token: &str, label: &str) -> Instrument {
    let contract = Contract::PredictionOutcome(ContractPredictionOutcome {
        condition_id: format!("0xcondition{token}"),
        outcome: label.into(),
        token_id: token.into(),
        resolution_ms: None,
        neg_risk: Some(true),
    });
    Instrument::new("polymarket", token, label, "USDC", contract).with_wire(InstrumentVenueIds {
        asset_id: None,
        coin: None,
        group: Some("0xset".into()),
    })
}

/// The kept rows, in the order a store serves them.
pub fn rows() -> Vec<Instrument> {
    let future = Contract::Future(ContractFuture {
        margining: Margining::Inverse,
        settle: "BTC".into(),
        value_currency: None,
        expiry_ms: MARCH,
        tenor: None,
        delivery: ContractFutureDelivery::Cash,
        family: ContractFutureFamily::Crypto,
    });
    let call = Contract::Option(ContractOption {
        right: OptionKind::Call,
        strike: 90_000.0,
        expiry_ms: MARCH,
        style: ContractOptionExerciseStyle::European,
        margining: Margining::Inverse,
        settle: "BTC".into(),
        value_currency: None,
        underlying: Some("BTC".into()),
    });
    vec![
        spot("binance", "BTCUSDT", "BTC", "USDT"),
        perp("binance", "BTCUSDT.P", "BTC", "USDT"),
        perp("bybit", "BTCUSDT.P", "BTC", "USDT"),
        perp("okx", "BTC-USDT-SWAP", "BTC", "USDT"),
        perp("deribit", "BTC-PERPETUAL", "BTC", "USD"),
        Instrument::new("deribit", "BTC-27MAR26", "BTC", "USD", future),
        Instrument::new("deribit", "BTC-27MAR26-90000-C", "BTC", "USD", call),
        spot("okx", "ETH-USDT", "ETH", "USDT"),
        outcome("101", "Yes"),
        outcome("102", "Yes"),
    ]
}

/// The leader's closes: 100, then each bar's return a decimal of π.
fn closes() -> Vec<f64> {
    let mut price = 100.0;
    let mut out = vec![price];
    for digit in DIGITS {
        price *= 1.0 + (f64::from(digit) - 4.5) / 1000.0;
        out.push(price);
    }
    out
}

/// The leader's closes `delay` bars later, the first bars held at the first close.
fn bars(delay: usize) -> Vec<Bar> {
    let closes = closes();
    (0..closes.len())
        .map(|t| {
            let close = closes[t.saturating_sub(delay)];
            Bar::ohlcv(t as i64 * MINUTE, close, close, close, close, 1.0)
        })
        .collect()
}

/// The fixture store: the kept rows and the bars.
pub fn store() -> Arc<dyn HistoryStore + Send + Sync> {
    let store = HistoryStoreMemory::new();
    // One commit key per append: a store holds a key once, whatever the series.
    for (venue, symbol, delay) in [
        ("binance", "BTCUSDT", 0),
        ("binance", "BTCUSDT.P", 0),
        ("bybit", "BTCUSDT.P", 1),
        ("okx", "BTCUSDT.P", 2),
    ] {
        let bars = bars(delay);
        let key = format!("{venue}-{symbol}");
        assert_eq!(store.append_bars(venue, symbol, "1m", &bars, Some(&key)).unwrap(), bars.len());
    }
    store.serve_kept_instruments(rows());
    Arc::new(store)
}

/// The study window: every bar.
pub fn window() -> TimestampRange {
    TimestampRange::of(0, 31 * MINUTE)
}

/// The recipe the proofs run.
pub fn recipe() -> String {
    "asset = \"BTC\"\ninterval = \"1m\"\nmax_lag = 3\n".to_string()
}
