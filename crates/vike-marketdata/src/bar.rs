//! The feed value types: [`Bar`] and the two tick types, [`QuoteTick`] and [`TradeTick`].
//!
//! Timestamps are epoch milliseconds (UTC). Optional and later-added fields carry
//! `#[serde(default)]`, so an older serialized payload still deserialises.
//
// Engineering notes (not rendered): `#[serde(default)]` is the additive-serde contract the journal
// and the recorded streams ride on. Ported from `core/model.py` + `core/ticks.py`; vike is the
// reference since `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`.

use serde::{Deserialize, Serialize};

/// One OHLCV candle. Resolution-agnostic: a bar is just a timestamped price event, so the same
/// type carries a 1-second candle, a daily candle or a single tick promoted to a one-price bar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    /// epoch milliseconds (UTC)
    pub ts: i64,
    /// First price of the bar.
    pub open: f64,
    /// Highest price of the bar.
    pub high: f64,
    /// Lowest price of the bar.
    pub low: f64,
    /// Last price of the bar.
    pub close: f64,
    /// Traded volume over the bar (in the instrument's size unit). For a bar built from quote
    /// ticks by [`consolidate_quotes`](crate::consolidate_quotes) this is the tick count instead.
    #[serde(default)]
    pub volume: f64,
    /// perp funding rate at this bar, if available
    #[serde(default)]
    pub funding: Option<f64>,
    /// opening best-bid for the bar's window (tick-derived; None for OHLCV bars)
    #[serde(default)]
    pub bid: Option<f64>,
    /// opening best-ask for the bar's window (tick-derived; None for OHLCV bars)
    #[serde(default)]
    pub ask: Option<f64>,
    // Engineering note: the symbol is attached by the engine dispatch.
    /// Fully-qualified `"SYMBOL.VENUE"` instrument id, when the producer stamps one; `None` for a
    /// bare series.
    #[serde(default)]
    pub symbol: Option<String>,
}

impl Bar {
    /// A plain OHLCV candle at `ts` (epoch milliseconds, UTC): the five prices and volume a candle
    /// feed carries. `funding`, `bid`, `ask` and `symbol` are `None`; build the struct literally
    /// when a bar carries any of them.
    pub fn ohlcv(ts: i64, open: f64, high: f64, low: f64, close: f64, volume: f64) -> Self {
        Self {
            ts,
            open,
            high,
            low,
            close,
            volume,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }
}

/// One L1 quote update: best bid/ask.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuoteTick {
    /// epoch milliseconds (UTC)
    pub ts: i64,
    // Engineering note: dual-timestamp capture - feed latency is later REPLAYED, not modeled, and
    // it cannot be captured retroactively. 0 = not stamped (fixtures, backfill).
    /// Local receive time in epoch milliseconds (UTC), stamped by the machine that received the
    /// tick, so feed latency can be measured as `local_ts - ts`. `0` means "not stamped".
    #[serde(default)]
    pub local_ts: i64,
    /// Best bid price.
    pub bid: f64,
    /// Best ask price.
    pub ask: f64,
    /// Size available at the best bid (`0.0` when the feed does not carry it).
    #[serde(default)]
    pub bid_size: f64,
    /// Size available at the best ask (`0.0` when the feed does not carry it).
    #[serde(default)]
    pub ask_size: f64,
    /// instrument id — empty for single-symbol paths
    #[serde(default)]
    pub symbol: String,
}

impl QuoteTick {
    /// Mid price: `(bid + ask) / 2`.
    pub fn mid(&self) -> f64 {
        (self.bid + self.ask) / 2.0
    }
}

/// One executed trade: price + size, with the aggressor flag where available.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradeTick {
    /// epoch milliseconds (UTC)
    pub ts: i64,
    // Engineering note: dual-timestamp capture - feed latency is later REPLAYED, not modeled, and
    // it cannot be captured retroactively. 0 = not stamped (fixtures, backfill).
    /// Local receive time in epoch milliseconds (UTC), stamped by the machine that received the
    /// tick, so feed latency can be measured as `local_ts - ts`. `0` means "not stamped".
    #[serde(default)]
    pub local_ts: i64,
    /// Trade price.
    pub price: f64,
    /// Trade size (quantity), in the instrument's size unit.
    pub size: f64,
    /// Aggressor flag in the Binance aggTrades convention: `true` when the buyer was the resting
    /// maker (a bid that got hit), so the aggressor was the SELLER; `false` when the aggressor was
    /// the buyer. Defaults to `false` when the feed does not say. See
    /// [`aggressor::classify`](crate::aggressor::classify).
    #[serde(default)]
    pub is_buyer_maker: bool,
    /// instrument id — empty for single-symbol paths
    #[serde(default)]
    pub symbol: String,
}
