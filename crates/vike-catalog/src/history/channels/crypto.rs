//! The six crypto venues with a datahub lane: every row built, keyless, its limits unread.

use super::{BARS, FUNDING_RATES};
use crate::history::{HistoryChannel, HistoryLane};

// ---------------------------------------------------------------------------------------------
// The venues with a datahub lane today. Every row is `Built` over a public endpoint and every
// limit is blank, because no vendor page was read for it and nothing was measured: the lane exists
// (the gate holds that), how far back it reaches is a later row-by-row PR's question.
// ---------------------------------------------------------------------------------------------

pub(crate) const BINANCE: &[HistoryChannel] = &[
    HistoryChannel::built_keyless("klines (spot and futures)", BARS, HistoryLane::Klines),
    HistoryChannel::built_keyless("funding-rate history", FUNDING_RATES, HistoryLane::Funding),
];

pub(crate) const BYBIT: &[HistoryChannel] =
    &[HistoryChannel::built_keyless("kline history", BARS, HistoryLane::Klines)];

pub(crate) const OKX: &[HistoryChannel] =
    &[HistoryChannel::built_keyless("history-candles", BARS, HistoryLane::Klines)];

pub(crate) const ASTER: &[HistoryChannel] =
    &[HistoryChannel::built_keyless("klines", BARS, HistoryLane::Klines)];

pub(crate) const DERIBIT: &[HistoryChannel] = &[HistoryChannel::built_keyless(
    "chart data (get_tradingview_chart_data)",
    BARS,
    HistoryLane::Klines,
)];

pub(crate) const HYPERLIQUID: &[HistoryChannel] = &[
    HistoryChannel::built_keyless("candleSnapshot candles", BARS, HistoryLane::Klines),
    HistoryChannel::built_keyless("funding history", FUNDING_RATES, HistoryLane::Funding),
];
