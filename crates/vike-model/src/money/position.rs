//! Position / Fill / Trade value objects. Exact port of `core/model.py`.
//! `Position` uses a SIGNED size: > 0 long, < 0 short, 0 flat.

use serde::{Deserialize, Serialize};

/// A net position in one instrument.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Position {
    pub size: f64,
    pub avg_price: f64,
}

impl Position {
    /// Mark-to-market PnL at `price` (handles long and short via the signed size).
    pub fn unrealized_pnl(&self, price: f64) -> f64 {
        (price - self.avg_price) * self.size
    }
}

/// One execution delivered to `Strategy::on_fill` (both engines fire it per applied fill).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fill {
    /// +1 buy / -1 sell
    pub side: i32,
    pub size: f64,
    /// fill price after slippage
    pub price: f64,
    pub fee: f64,
    /// epoch milliseconds (UTC)
    pub ts: i64,
    #[serde(default)]
    pub is_maker: bool,
    #[serde(default)]
    pub symbol: String,
}

/// A completed round-trip. `pnl` is gross price PnL; `fees` is the round-trip cost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trade {
    pub entry_price: f64,
    pub exit_price: f64,
    pub size: f64,
    pub pnl: f64,
    #[serde(default)]
    pub fees: f64,
    /// fill timestamp of the opening order (epoch ms)
    #[serde(default)]
    pub entry_ts: i64,
    /// fill timestamp of the closing order (epoch ms)
    #[serde(default)]
    pub exit_ts: i64,
    /// originating symbol ("" for single-symbol engine)
    #[serde(default)]
    pub symbol: String,
    /// max adverse excursion as a fraction of entry price (portfolio mode only; 0.0 = untracked)
    #[serde(default)]
    pub mae: f64,
    /// max favorable excursion as a fraction of entry price (portfolio mode only; 0.0 = untracked)
    #[serde(default)]
    pub mfe: f64,
    /// true if the opening side of this trade was a buy
    #[serde(default)]
    pub is_long: bool,
}
