//! **What a venue's stored HISTORY means**: what a stored series' volume counts ([`stored_volume_for`]) and whether
//! the venue publishes a funding history, and over what period one published point accrues
//! ([`funding_history_for`]). Facts of each bridge's `history.rs`, read by the backtest's volume rule and funding
//! charge, which link no bridge.
//!
//! Every roster venue is a NAMED row citing the bridge code it was read from (`crates/vike-venues/CLAUDE.md`'s
//! "Per-venue capability maps (the playbook)"). The readers: `crates/vike-backtest/src/run/dispatch.rs`'s
//! `volume_unit`, `crates/vike-backtest/src/run/dispatch/funding.rs`'s `FundingAccrual::of_venue`, deribit's funding
//! history (`crates/bridges/deribit/src/history.rs`) and the client crate's funding roster
//! (`crates/vike-gatewayhub-client/src/history.rs`). An unknown venue string answers the empty row (no coin volume,
//! no instrument size, no funding history).

/// What a venue's stored series' VOLUME counts, where it is not the order's own unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueStoredVolume {
    /// The venue's INVERSE candles (the bar lane) count the settle COIN, not the contracts its orders count.
    pub inverse_candles_count_the_coin: bool,
    /// The venue's LINEAR derivative trade prints (the tick lane) count contracts of a per-instrument size only the
    /// instrument's row states.
    pub linear_prints_count_an_instrument_size: bool,
    /// Why, citing the bridge code (a `path`'s `symbol`, never a line number).
    pub why: &'static str,
}

/// Whether a venue publishes a funding history this tree reads, and how a published point is owed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueFundingHistory {
    /// The venue's bridge serves a funding-rate history (a `vike_data::source::FundingRateSource`).
    pub publishes_history: bool,
    /// `Some(period)`: a point ACCRUES continuously over the period ending at its timestamp; `None`: it is SETTLED at
    /// its timestamp (or the venue publishes none).
    pub accrual_period_ms: Option<i64>,
    /// Why, citing the bridge code.
    pub why: &'static str,
}

const fn volume(coin: bool, instrument_size: bool, why: &'static str) -> VenueStoredVolume {
    VenueStoredVolume {
        inverse_candles_count_the_coin: coin,
        linear_prints_count_an_instrument_size: instrument_size,
        why,
    }
}

/// The row of a venue whose stored volume is the order's own unit everywhere.
const ORDER_UNIT: &str = "every stored series' volume is the order's own unit: the base of a linear series, the contracts of an inverse one";

/// What `venue`'s stored volume counts; an unknown venue string counts the order's own unit.
#[must_use]
pub fn stored_volume_for(venue: &str) -> VenueStoredVolume {
    match venue {
        "binance" => volume(
            true,
            false,
            "a COIN-M candle counts the coin (the kline's base-asset column, \
             `crates/bridges/binance/src/history.rs`'s `BINANCE_KLINE_COIN_M`); its prints count contracts",
        ),
        "okx" => volume(
            true,
            true,
            "an inverse candle counts the coin (`volCcy`) and a derivative's the base \
             (`crates/bridges/okx/src/history.rs`'s `volume_column`), while a linear swap's, future's or option's \
             print counts contracts of the instrument's `ctVal` (`sz`)",
        ),
        "deribit" => volume(
            true,
            false,
            "an inverse candle counts the coin (`volume`, `cost` being the dollars: \
             `crates/bridges/deribit/src/history.rs`'s `parse_deribit_klines`); a print counts dollars, the order's unit",
        ),
        "bybit" => volume(
            false,
            false,
            "an inverse candle counts contracts of one dollar, the order's unit (`volume`, `turnover` being the coin: \
             `crates/bridges/bybit/src/history.rs`'s `parse_bybit_klines`)",
        ),
        "aster" | "hyperliquid" | "polymarket" | "oanda" | "ig" | "fxcm" | "dukascopy"
        | "ctrader" | "alpaca" | "ibkr" => volume(false, false, ORDER_UNIT),
        // vike:new-venue:row "{venue}" => volume(false, false, ORDER_UNIT), // TODO(new-venue: {venue}): does its bridge store a volume in another unit than its orders'?
        _ => volume(false, false, "not a roster venue"),
    }
}

const fn settled(why: &'static str) -> VenueFundingHistory {
    VenueFundingHistory { publishes_history: true, accrual_period_ms: None, why }
}

const fn not_published(why: &'static str) -> VenueFundingHistory {
    VenueFundingHistory { publishes_history: false, accrual_period_ms: None, why }
}

/// The row of a venue that publishes no funding history this tree reads.
const NO_FUNDING_HISTORY: &str = "the bridge serves no funding-rate history";

/// Whether `venue` publishes a funding history, and how a point is owed; an unknown venue string publishes none.
#[must_use]
pub fn funding_history_for(venue: &str) -> VenueFundingHistory {
    match venue {
        "binance" => settled(
            "settled at each point (`crates/bridges/binance/src/history.rs`'s `FundingRateSourceBinance`)",
        ),
        "hyperliquid" => settled(
            "settled at each point (`crates/bridges/hyperliquid/src/history/funding.rs`'s \
             `FundingRateSourceHyperliquid`)",
        ),
        "bybit" => settled(
            "settled at each point (`crates/bridges/bybit/src/history.rs`'s `FundingRateSourceBybit`)",
        ),
        "okx" => settled(
            "settled at each point (`crates/bridges/okx/src/history.rs`'s `FundingRateSourceOkx`)",
        ),
        "deribit" => VenueFundingHistory {
            publishes_history: true,
            accrual_period_ms: Some(vike_model::MS_PER_HOUR),
            why: "funding accrues continuously and the venue publishes the hour ending at each point, MEASURED \
                  against `BTC-PERPETUAL` (`crates/bridges/deribit/src/history.rs`'s `FundingRateSourceDeribit`, \
                  which reads this period)",
        },
        "aster" => settled(
            "settled at each point (`crates/bridges/aster/src/history.rs`'s `FundingRateSourceAster`)",
        ),
        "polymarket" | "oanda" | "ig" | "fxcm" | "dukascopy" | "ctrader" | "alpaca" | "ibkr" => {
            not_published(NO_FUNDING_HISTORY)
        }
        // vike:new-venue:row "{venue}" => not_published(NO_FUNDING_HISTORY), // TODO(new-venue: {venue}): does its bridge serve a funding-rate history?
        _ => not_published("not a roster venue"),
    }
}

#[path = "venue_history_facts_tests.rs"]
#[cfg(test)]
mod venue_history_facts_tests;
