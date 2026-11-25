//! Option TRADE rows — an options venue's public trade log (`kind=option_trade`), and the ONE rule
//! that derives point-in-time chain rows (`kind=chain`, [`crate::chain_log::ChainRow`]) from them.
//!
//! # Why its own kind
//!
//! An option trade carries three numbers a market print (`vike_marketdata::TickTrade`, `kind=trade`) has no
//! column for: the trade's implied volatility, the venue's mark price and its index price at the trade. They
//! are what a study builds an IV surface from after the venue stopped publishing whole chains, so they are
//! kept, and `TickTrade` is the published leaf crate's type (a field there is a breaking change for every
//! user). The row also carries the instrument's identity (underlying, expiry, strike, right), parsed by the
//! venue's bridge, which is the one place that reads the venue's spelling: a reader of these rows, the chain
//! derivation below included, never parses an instrument name.
//!
//! # Units, as the venue publishes them
//!
//! `price` and `mark_price` are in the venue's PREMIUM unit: the underlying coin for a coin-settled book
//! (`premium_in_coin`, Deribit's `BTC-…` and `ETH-…` options), the settlement currency for a linear book
//! (Deribit's `…_USDC-…` options). `iv` is DECIMAL (0.6337 = 63.37 %), the convention of
//! [`crate::chain_log::ChainRow::iv`]; the venue publishes percent and the bridge divides once.
//!
//! # The derived chain ([`hourly_chain_rows`])
//!
//! One chain row per (hour, instrument) that TRADED in that hour, stamped at the LAST millisecond of the
//! hour (the stamp `crates/vike-backfill/src/venues/binance_archive/options/summary.rs` gives Binance's
//! hourly summaries): a row describes a finished hour, so a point-in-time read
//! (`crate::HistoryStore`'s `chain_as_of_within`) never sees it before the hour is over, and the stamp keeps
//! hour 23 inside its own UTC day. Its `mark` (in USD) and `iv` are the latest values the hour's trades
//! published, its `volume` the hour's traded amount. A trade has no quote and no greeks, so `bid`, `ask`,
//! `open_interest` and the greeks stay ABSENT, never zero; a reader computes greeks from `iv` with
//! `vike_options`. An instrument that did not trade in an hour has no row for it: the reader's lookback
//! (`chain_as_of_within`'s `lookback_ms`) decides how stale a last trade may be.
//!
//! ⚠ **A coin premium is converted to USD at the trade's INDEX price, the live chain's at the expiry's
//! FORWARD.** A trade carries the venue's index and no forward, so this derivation has nothing else to
//! convert at; the live chain recorder scales by each row's `underlying_price`, the forward of its expiry
//! (`crates/bridges/deribit/src/feed/options/chain.rs`'s `build_chain_from_summary`). A chain series holding both lanes
//! therefore steps by the basis `F / S − 1` where one lane gives way to the other (small near expiry, larger
//! for a far one). That step is no market move: a study reading across it compares IVs, which both lanes
//! carry as the venue published them, or keeps to one lane.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use vike_marketdata::OptionKind;
use vike_model::MS_PER_HOUR;

use crate::chain_log::ChainRow;

/// One option trade, persisted as the `kind=option_trade` series keyed `venue=…`/`symbol=<currency>`
/// (the venue's currency the trade log was read for: `BTC`, `ETH`, `USDC`). See the module doc for the
/// units.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptionTradeRow {
    /// Trade time, epoch milliseconds (UTC).
    pub ts: i64,
    /// The chain this option belongs to, named as the venue's LIVE chain recorder names it (`BTC`, `ETH`, `SOL`
    /// for SOL's USDC book): the venue's bridge decides it by the live surface's own rule, and a linear book that
    /// surface does not serve keeps its own name (`BTC_USDC`), so two books never share a chain series.
    pub underlying: String,
    /// The exact venue instrument id, verbatim (`BTC-21JUN24-94000-C`, `XRP_USDC-26SEP25-1d6-P`).
    pub instrument: String,
    /// Expiry instant, epoch milliseconds (UTC).
    pub expiry_ms: i64,
    /// Strike price, in the quote currency (USD or USDC).
    pub strike: f64,
    /// Call or put.
    pub kind: OptionKind,
    /// The venue's trade id, verbatim (`303871231`, `ETH-156051774`): unique per venue.
    pub trade_id: String,
    /// Trade price, in the premium unit (module doc).
    pub price: f64,
    /// Trade size, in the venue's amount unit (contracts of one coin for a coin-settled option).
    pub amount: f64,
    /// `true` when the TAKER sold (the buyer was the resting maker): `vike_marketdata::TickTrade`'s
    /// aggressor convention.
    pub is_buyer_maker: bool,
    /// The trade's implied volatility, DECIMAL; absent when the venue published none.
    pub iv: Option<f64>,
    /// The venue's mark price at the trade, in the premium unit; absent when the venue published none
    /// (Deribit's 2017 trades carry `null`).
    pub mark_price: Option<f64>,
    /// The venue's index price of the underlying at the trade, in USD.
    pub index_price: f64,
    /// Whether `price` and `mark_price` are in units of the underlying coin (a coin-settled book) rather
    /// than in the settlement currency.
    pub premium_in_coin: bool,
}

/// The latest state of one instrument inside one hour, while [`hourly_chain_rows`] folds the trades.
struct HourState<'a> {
    trade: &'a OptionTradeRow,
    mark_usd: Option<f64>,
    iv: Option<f64>,
    volume: f64,
}

/// **The chain rows `trades` derive**: one per (hour, instrument) that traded, sorted `(ts, instrument)`
/// (the module doc's rule). Input order is not assumed: the trades are folded in time order (a stable
/// sort, so two trades of one millisecond keep their given order). `trades` of several underlyings give
/// rows of several underlyings, each row carrying its own.
pub fn hourly_chain_rows(trades: &[OptionTradeRow]) -> Vec<ChainRow> {
    let mut ordered: Vec<&OptionTradeRow> = trades.iter().collect();
    ordered.sort_by_key(|t| t.ts);
    let mut hours: BTreeMap<(i64, &str), HourState<'_>> = BTreeMap::new();
    for trade in ordered {
        // The derived chain's bucket is one hour.
        let stamp = trade.ts.div_euclid(MS_PER_HOUR) * MS_PER_HOUR + (MS_PER_HOUR - 1);
        let state = hours.entry((stamp, trade.instrument.as_str())).or_insert(HourState {
            trade,
            mark_usd: None,
            iv: None,
            volume: 0.0,
        });
        state.trade = trade;
        let to_usd = if trade.premium_in_coin { trade.index_price } else { 1.0 };
        if let Some(mark) = trade.mark_price {
            state.mark_usd = Some(mark * to_usd);
        }
        if trade.iv.is_some() {
            state.iv = trade.iv;
        }
        state.volume += trade.amount;
    }
    hours
        .into_iter()
        .map(|((stamp, _), state)| ChainRow {
            ts: stamp,
            underlying: state.trade.underlying.clone(),
            instrument: state.trade.instrument.clone(),
            expiry_ms: state.trade.expiry_ms,
            strike: state.trade.strike,
            kind: state.trade.kind,
            bid: None,
            ask: None,
            mark: state.mark_usd,
            iv: state.iv,
            open_interest: None,
            volume: Some(state.volume),
            delta: None,
            gamma: None,
            theta: None,
            vega: None,
        })
        .collect()
}

#[path = "option_trade_log_tests.rs"]
#[cfg(test)]
mod option_trade_log_tests;
