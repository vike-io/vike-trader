//! Domain model: Bar + the tick value types. Exact port of `core/model.py` + `core/ticks.py`.

use serde::{Deserialize, Serialize};

/// One OHLCV candle. Resolution-agnostic: a bar is just a timestamped price event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    /// epoch milliseconds (UTC)
    pub ts: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
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
    /// fully-qualified "SYMBOL.VENUE" id, attached by the engine dispatch
    #[serde(default)]
    pub symbol: Option<String>,
}

/// One L1 quote update: best bid/ask.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuoteTick {
    /// epoch milliseconds (UTC)
    pub ts: i64,
    /// machine receive epoch-ms (dual-timestamp capture — feed latency is later REPLAYED,
    /// not modeled; uncapturable retroactively). 0 = not stamped (fixtures, backfill).
    #[serde(default)]
    pub local_ts: i64,
    pub bid: f64,
    pub ask: f64,
    #[serde(default)]
    pub bid_size: f64,
    #[serde(default)]
    pub ask_size: f64,
    /// instrument id — empty for single-symbol paths
    #[serde(default)]
    pub symbol: String,
}

impl QuoteTick {
    pub fn mid(&self) -> f64 {
        (self.bid + self.ask) / 2.0
    }
}

/// One executed trade: price + size, with the aggressor flag where available.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradeTick {
    /// epoch milliseconds (UTC)
    pub ts: i64,
    /// machine receive epoch-ms (dual-timestamp capture — feed latency is later REPLAYED,
    /// not modeled; uncapturable retroactively). 0 = not stamped (fixtures, backfill).
    #[serde(default)]
    pub local_ts: i64,
    pub price: f64,
    pub size: f64,
    #[serde(default)]
    pub is_buyer_maker: bool,
    /// instrument id — empty for single-symbol paths
    #[serde(default)]
    pub symbol: String,
}
