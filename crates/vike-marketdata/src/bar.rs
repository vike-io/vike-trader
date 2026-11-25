//! The feed value types: [`Bar`] and the two tick types, [`TickQuote`] and [`TickTrade`].
//!
//! Timestamps are epoch milliseconds (UTC). Optional and later-added fields carry
//! `#[serde(default)]`, so an older serialized payload still deserialises.
//
// Engineering notes (not rendered): `#[serde(default)]` is the additive-serde contract the journal
// and the recorded streams ride on. Ported from `core/model.py` + `core/ticks.py`; vike is the
// reference since `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`.

use serde::{Deserialize, Serialize};

use crate::Symbol;

/// One OHLCV candle. Resolution-agnostic: a bar is just a timestamped price event, so the same
/// type carries a 1-second candle, a daily candle or a single tick promoted to a one-price bar.
///
/// ⚠ `#[repr(C)]` in this declaration order: a strategy file reads the host's bars in place, under
/// `crates/vike-rust-strategy/src/abi.rs`'s `layout`. Never reorder the fields (their serde form is by
/// position too).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[repr(C)]
pub struct Bar {
    /// epoch milliseconds (UTC) of the bar's OPEN: it closes at `timestamp` plus its interval
    /// ([`Bar::closed_at`]). Every stored bar lane keeps this convention (venue candles, archive imports
    /// and a box's own recorded closes alike), so a read of the bars with `timestamp <= t` can hold one
    /// still FORMING at `t`; a point-in-time reader keeps those [`Bar::closed_at`] `t`.
    pub timestamp: i64,
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
    // Engineering note: the symbol is attached by the engine dispatch. An interned `Symbol` (one
    // pointer, no allocation per bar), serialized as its text: the bytes a `String` wrote.
    /// Fully-qualified `"SYMBOL.VENUE"` instrument id, when the producer stamps one; `None` for a
    /// bare series.
    #[serde(default)]
    pub symbol: Option<Symbol>,
}

impl Bar {
    /// A plain OHLCV candle at `ts` (epoch milliseconds, UTC): the five prices and volume a candle
    /// feed carries. `funding`, `bid`, `ask` and `symbol` are `None`; build the struct literally
    /// when a bar carries any of them.
    pub fn ohlcv(ts: i64, open: f64, high: f64, low: f64, close: f64, volume: f64) -> Self {
        Self {
            timestamp: ts,
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

    /// Whether this bar has closed at `now_ms`, for a bar `step_ms` wide: its interval has ended
    /// once `ts + step_ms <= now_ms`, so a bar exactly at its close time is closed.
    #[must_use]
    pub fn closed_at(&self, step_ms: i64, now_ms: i64) -> bool {
        self.timestamp.saturating_add(step_ms) <= now_ms
    }

    /// The bar's PATH, the one walk of a bar every reader takes: the open, the extreme nearer the
    /// open (the low when the two are as near), the other extreme, the close. An assumption, as any
    /// order of prices inside one bar is.
    #[must_use]
    pub fn path_points(&self) -> [f64; 4] {
        let (near, far) = if self.high - self.open < self.open - self.low {
            (self.high, self.low)
        } else {
            (self.low, self.high)
        };
        [self.open, near, far, self.close]
    }
}

/// The two SIDES of a bar's window, folded from the venue's quotes: the path of the best bid and the
/// path of the best ask over the same window as the bar, each a [`Bar`] of that side's prices
/// (stamped as the bar they belong to). What a seller could get and what a buyer could pay, where the
/// bar itself is a path of mids or of trades. A bar no quote came beside (every stored bar) has none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BarSides {
    /// The best bid's path: what a SELL meets.
    pub bid: Bar,
    /// The best ask's path: what a BUY meets.
    pub ask: Bar,
}

/// One L1 quote update: best bid/ask.
///
/// ⚠ `#[repr(C)]` in this declaration order, as [`Bar`] is: a strategy file reads the host's tick in place
/// (`on_quote_tick`), under `crates/vike-rust-strategy/src/abi.rs`'s `layout`. Never reorder the fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[repr(C)]
pub struct TickQuote {
    /// epoch milliseconds (UTC)
    pub timestamp: i64,
    // Engineering note: dual-timestamp capture - feed latency is later REPLAYED, not modeled, and
    // it cannot be captured retroactively. 0 = not stamped (fixtures, backfill).
    /// Local receive time in epoch milliseconds (UTC), stamped by the machine that received the
    /// tick, so feed latency can be measured as `local_timestamp - ts`. `0` means "not stamped".
    #[serde(default)]
    pub local_timestamp: i64,
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
    /// instrument id — empty for single-symbol paths (an interned [`Symbol`], serialized as its text)
    #[serde(default)]
    pub symbol: Symbol,
}

impl TickQuote {
    /// Mid price: `(bid + ask) / 2`.
    pub fn mid(&self) -> f64 {
        (self.bid + self.ask) / 2.0
    }
}

/// One executed trade: price + size, with the aggressor flag where available.
///
/// ⚠ `#[repr(C)]` in this declaration order, as [`Bar`] is: a strategy file reads the host's trade in place
/// (`on_trade_tick`), under `crates/vike-rust-strategy/src/abi.rs`'s `layout`. Never reorder the fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[repr(C)]
pub struct TickTrade {
    /// epoch milliseconds (UTC)
    pub timestamp: i64,
    // Engineering note: dual-timestamp capture - feed latency is later REPLAYED, not modeled, and
    // it cannot be captured retroactively. 0 = not stamped (fixtures, backfill).
    /// Local receive time in epoch milliseconds (UTC), stamped by the machine that received the
    /// tick, so feed latency can be measured as `local_timestamp - ts`. `0` means "not stamped".
    #[serde(default)]
    pub local_timestamp: i64,
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
    /// instrument id — empty for single-symbol paths (an interned [`Symbol`], serialized as its text)
    #[serde(default)]
    pub symbol: Symbol,
}

#[path = "bar_tests.rs"]
#[cfg(test)]
mod bar_tests;
