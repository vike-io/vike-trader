//! Public historical-bar fetch seam (PR-3b). Owns a dedicated ibapi connection and pages
//! `historical_data` bounded below by `head_timestamp`, returning `vike_model::Bar`s so the
//! vike-backfill `ibkr_backfill` collector never names an ibapi type. Reuses the SAME symbology
//! (`parse_simplified`/`pump_contract`), bar-size mapping (`hist_bar_size`), and bar mapper
//! (`bar_from_historical`) as the PR-3a live feed. Behind `ibkr-socket` (where ibapi exists).
//! Ports the historical half of docs/superpowers/specs/2026-07-15-vike-ibkr-market-data-design.md §3.

use ibapi::client::blocking::Client;
use ibapi::market_data::TradingHours;
use ibapi::market_data::historical::{Duration as IbDuration, WhatToShow};
use time::OffsetDateTime;

use crate::config::IbkrConfig;
use crate::contract::parse_simplified;
use crate::error::IbkrError;
use crate::market_feed::map::bar_from_historical;
use crate::market_feed::{hist_bar_size, pump_contract};
use vike_model::Bar;

/// What series to fetch (maps to ibapi `WhatToShow`). Vike-owned so callers don't name ibapi.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HistWhat {
    Trades,
    MidPoint,
    Bid,
    Ask,
    BidAsk,
}

impl HistWhat {
    pub fn parse(s: &str) -> Option<HistWhat> {
        match s.trim().to_ascii_lowercase().as_str() {
            "trades" | "" => Some(HistWhat::Trades),
            "midpoint" | "mid" => Some(HistWhat::MidPoint),
            "bid" => Some(HistWhat::Bid),
            "ask" => Some(HistWhat::Ask),
            "bidask" | "bid_ask" => Some(HistWhat::BidAsk),
            _ => None,
        }
    }
    fn to_ibapi(self) -> WhatToShow {
        match self {
            HistWhat::Trades => WhatToShow::Trades,
            HistWhat::MidPoint => WhatToShow::MidPoint,
            HistWhat::Bid => WhatToShow::Bid,
            HistWhat::Ask => WhatToShow::Ask,
            HistWhat::BidAsk => WhatToShow::BidAsk,
        }
    }
}

/// A vike-owned mirror of ibapi's `Duration` (value + IB unit char) so the collector can size
/// paging windows without depending on ibapi.
#[derive(Clone, Copy, Debug)]
pub struct Window {
    pub value: i32,
    pub unit: char,
}

impl Window {
    pub fn days(n: i32) -> Window {
        Window { value: n, unit: 'D' }
    }
    pub fn weeks(n: i32) -> Window {
        Window { value: n, unit: 'W' }
    }
    pub fn months(n: i32) -> Window {
        Window { value: n, unit: 'M' }
    }
    pub fn years(n: i32) -> Window {
        Window { value: n, unit: 'Y' }
    }
    fn to_ibapi(self) -> IbDuration {
        match self.unit {
            'S' => IbDuration::seconds(self.value),
            'W' => IbDuration::weeks(self.value),
            'M' => IbDuration::months(self.value),
            'Y' => IbDuration::years(self.value),
            _ => IbDuration::days(self.value),
        }
    }
}

/// Owns a dedicated ibapi connection for one-shot historical paging.
pub struct HistoricalFetcher {
    client: Client,
}

impl HistoricalFetcher {
    /// Open a fresh ibapi connection on `cfg.data_client_id` and set the market-data type
    /// (best-effort — historical requests honor delayed just as the live feed does).
    pub fn connect(cfg: &IbkrConfig) -> Result<HistoricalFetcher, IbkrError> {
        let addr = format!("{}:{}", cfg.host, cfg.port);
        let client = Client::connect(&addr, cfg.data_client_id).map_err(|e| {
            IbkrError::Connect(format!("hist {addr} (client_id={}): {e}", cfg.data_client_id))
        })?;
        if let Err(e) = client.switch_market_data_type(cfg.mktdata_type.to_ibapi()) {
            tracing::warn!(error = %e, "ibkr hist switch_market_data_type failed; continuing");
        }
        Ok(HistoricalFetcher { client })
    }

    /// Earliest available epoch-ms for the contract (`reqHeadTimeStamp`). A missing/failed head
    /// timestamp surfaces as `IbkrError::DataUnavailable` — the caller then bounds by an explicit
    /// start.
    pub fn head_timestamp_ms(&self, symbol: &str, what: HistWhat) -> Result<i64, IbkrError> {
        let contract = parse_simplified(symbol)
            .ok_or_else(|| IbkrError::Unsupported(format!("unparseable symbol {symbol}")))?;
        let ib = pump_contract(&contract);
        let dt = self
            .client
            .head_timestamp(&ib, what.to_ibapi(), TradingHours::Regular)
            .map_err(|e| IbkrError::DataUnavailable(format!("head_timestamp {symbol}: {e}")))?;
        Ok(dt.unix_timestamp() * 1000)
    }

    /// One paged request ending at `end_ms`, covering `window` back, at `interval`'s bar size.
    /// Returns ts-ascending `vike_model::Bar`s (possibly empty for a gap window).
    pub fn fetch_window(
        &self,
        symbol: &str,
        interval: &str,
        end_ms: i64,
        window: Window,
        what: HistWhat,
    ) -> Result<Vec<Bar>, IbkrError> {
        let contract = parse_simplified(symbol)
            .ok_or_else(|| IbkrError::Unsupported(format!("unparseable symbol {symbol}")))?;
        let ib = pump_contract(&contract);
        let end = OffsetDateTime::from_unix_timestamp(end_ms / 1000)
            .map_err(|e| IbkrError::Unsupported(format!("bad end_ms {end_ms}: {e}")))?;
        let data = self
            .client
            .historical_data(&ib, hist_bar_size(interval))
            .ending(end)
            .duration(window.to_ibapi())
            .what_to_show(what.to_ibapi())
            .trading_hours(TradingHours::Regular)
            .fetch()
            .map_err(|e| {
                IbkrError::DataUnavailable(format!("historical_data {symbol}@{interval}: {e}"))
            })?;
        let mut bars: Vec<Bar> = data.bars.iter().map(bar_from_historical).collect();
        bars.sort_by_key(|b| b.ts);
        Ok(bars)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histwhat_parses_and_defaults() {
        assert_eq!(HistWhat::parse("trades"), Some(HistWhat::Trades));
        assert_eq!(HistWhat::parse("BIDASK"), Some(HistWhat::BidAsk));
        assert_eq!(HistWhat::parse("midpoint"), Some(HistWhat::MidPoint));
        assert_eq!(HistWhat::parse("nope"), None);
    }

    #[test]
    fn window_ctors_map_to_ibapi_units() {
        assert_eq!((Window::days(5).value, Window::days(5).unit), (5, 'D'));
        assert_eq!((Window::weeks(2).value, Window::weeks(2).unit), (2, 'W'));
        assert_eq!((Window::months(3).value, Window::months(3).unit), (3, 'M'));
        assert_eq!((Window::years(1).value, Window::years(1).unit), (1, 'Y'));
    }
}
