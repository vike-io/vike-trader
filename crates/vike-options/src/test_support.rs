//! The deribit instrument and chain builders the option-fold tests share: this crate's own
//! (`chain/fold/markprice_tests.rs`, `chain/fold/ticker_tests.rs`, `chain/index/tests.rs`) and
//! deribit's offline suite (`option_markprice_fold`, `option_ticker_fold`), which runs the venue's
//! decoders into the same folds and reaches this module through a dev edge with `test-support`.
//! Gated `#[cfg(any(test, feature = "test-support"))]`, so no shipped build compiles it; this crate
//! still names no venue crate, the `"deribit"` here is the index's own venue string.

use std::collections::BTreeMap;

use vike_marketdata::{
    Expiry, OptionChain, OptionKind, OptionQuote, StrikeRow, UnderlyingChains, UnderlyingKind,
};
use vike_model::{Contract, Instrument, OptionSpec};

use crate::OptionIndex;

/// The venue the builders stamp: [`OptionIndex`] indexes only this venue's rows.
const VENUE: &str = "deribit";

/// The fold tests' clock: 2026-06-02 08:00 UTC (chain.rs's NOW).
pub const NOW: i64 = 1_780_387_200_000;

/// A deribit option [`Instrument`] for tests, built the way the catalog builds one: the margining
/// follows the settlement (coin-settled = inverse, else linear), the expiry is 08:00 UTC of
/// `expiry_iso`.
pub fn test_option(
    name: &str,
    underlying: &str,
    expiry_iso: &str,
    strike: f64,
    right: OptionKind,
    settle: &str,
) -> Instrument {
    let margining = if settle == underlying {
        vike_model::Margining::Inverse
    } else {
        vike_model::Margining::Linear
    };
    Instrument::new(
        VENUE,
        name,
        underlying,
        settle,
        Contract::Option(OptionSpec {
            right,
            strike,
            expiry_ms: crate::expiry_ms(expiry_iso),
            style: vike_model::ExerciseStyle::European,
            margining,
            settle: settle.to_string(),
            value_ccy: Some(underlying.to_string()),
            underlying: Some(underlying.to_string()),
        }),
    )
}

/// A deribit perpetual or dated future [`Instrument`] for tests (`base` is its underlying).
pub fn test_delta_one(name: &str, base: &str) -> Instrument {
    Instrument::new(
        VENUE,
        name,
        base,
        "USD",
        Contract::Perp(vike_model::PerpSpec {
            margining: vike_model::Margining::Inverse,
            settle: base.to_string(),
            value_ccy: Some("USD".to_string()),
            funding_interval_ms: None,
            settling_ms: None,
        }),
    )
}

/// The option index every name the fold tests stream is looked up in: the coin-settled BTC/ETH
/// options (strikes on and off the grid, a fetched and an unfetched expiry), the USDC-settled SOL
/// put, and the BTC perpetual (a future: indexed, but no option address). The ticker channel
/// carries no strike, so for it the index is the ONLY source of one.
pub fn fold_index() -> OptionIndex {
    use OptionKind::{Call, Put};
    OptionIndex::from_instruments(&[
        test_option("BTC-27JUN26-100000-C", "BTC", "2026-06-27", 100_000.0, Call, "BTC"),
        test_option("BTC-27JUN26-100000-P", "BTC", "2026-06-27", 100_000.0, Put, "BTC"),
        test_option("BTC-27JUN26-999000-C", "BTC", "2026-06-27", 999_000.0, Call, "BTC"),
        test_option("BTC-25SEP26-100000-C", "BTC", "2026-09-25", 100_000.0, Call, "BTC"),
        test_option("ETH-27JUN26-3000-C", "ETH", "2026-06-27", 3_000.0, Call, "ETH"),
        test_option("SOL_USDC-25SEP26-45-P", "SOL", "2026-09-25", 45.0, Put, "USDC"),
        test_delta_one("BTC-PERPETUAL", "BTC"),
    ])
}

/// A one-strike BTC chain for 2026-06-27 (call+put), mimicking a REST-seeded grid the WS stream
/// then updates. Spot 100k; each quote carries a stale mark (1.0) and iv (0.10) the stream
/// overwrites, and `stale_book` as both its bid and ask (`None` for the mark-price channel, which
/// carries no book; the ticker tests seed one to watch it overwritten).
pub fn seeded_btc(stale_book: Option<f64>) -> BTreeMap<String, UnderlyingChains> {
    let mk = |name: &str, kind: OptionKind| OptionQuote {
        bid: stale_book,
        ask: stale_book,
        mark: Some(1.0),
        iv: Some(0.10),
        instrument_name: Some(name.to_string()),
        ..OptionQuote::new(100_000.0, kind)
    };
    let chain = OptionChain {
        underlying: "BTC".into(),
        underlying_kind: UnderlyingKind::Crypto,
        underlying_price: Some(100_000.0),
        expiry: Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() },
        asof_ms: NOW,
        source: "deribit".into(),
        rows: vec![StrikeRow {
            strike: 100_000.0,
            call: Some(mk("BTC-27JUN26-100000-C", OptionKind::Call)),
            put: Some(mk("BTC-27JUN26-100000-P", OptionKind::Put)),
        }],
    };
    let mut chains = BTreeMap::new();
    chains.insert("2026-06-27".to_string(), chain);
    let bundle = UnderlyingChains {
        default_expiry: "2026-06-27".into(),
        expiries: vec![Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() }],
        chains,
    };
    let mut by = BTreeMap::new();
    by.insert("BTC".to_string(), bundle);
    by
}
