//! The six crypto venues with a datahub lane: every row built, keyless, its limits unread.

use super::{BARS, FUNDING_RATES, TRADES};
use crate::history::{HistoryChannel, HistoryChannelLane};

// ---------------------------------------------------------------------------------------------
// The venues with a datahub lane today. Every row is `Built` over a public endpoint and every
// limit is blank, because no vendor page was read for it and nothing was measured: the lane exists
// (the gate holds that), how far back it reaches is a later row-by-row PR's question.
// ---------------------------------------------------------------------------------------------

pub(crate) const BINANCE: &[HistoryChannel] = &[
    HistoryChannel::built_keyless("klines (spot and futures)", BARS, HistoryChannelLane::Klines),
    HistoryChannel::built_keyless(
        "funding-rate history",
        FUNDING_RATES,
        HistoryChannelLane::Funding,
    ),
];

pub(crate) const BYBIT: &[HistoryChannel] = &[
    HistoryChannel::built_keyless("kline history", BARS, HistoryChannelLane::Klines),
    HistoryChannel::built_keyless("funding/history", FUNDING_RATES, HistoryChannelLane::Funding),
];

pub(crate) const OKX: &[HistoryChannel] = &[
    HistoryChannel::built_keyless("history-candles", BARS, HistoryChannelLane::Klines),
    HistoryChannel::built_keyless(
        "funding-rate-history",
        FUNDING_RATES,
        HistoryChannelLane::Funding,
    ),
];

pub(crate) const ASTER: &[HistoryChannel] = &[
    HistoryChannel::built_keyless("klines", BARS, HistoryChannelLane::Klines),
    HistoryChannel::built_keyless("fundingRate", FUNDING_RATES, HistoryChannelLane::Funding),
];

pub(crate) const DERIBIT: &[HistoryChannel] = &[
    HistoryChannel::built_keyless(
        "chart data (get_tradingview_chart_data)",
        BARS,
        HistoryChannelLane::Klines,
    ),
    HistoryChannel::built_keyless(
        "get_funding_rate_history",
        FUNDING_RATES,
        HistoryChannelLane::Funding,
    ),
    HistoryChannel::built_keyless(
        "option trade log (get_last_trades_by_currency_and_time, history.deribit.com)",
        TRADES,
        HistoryChannelLane::OptionTrades,
    ),
];

pub(crate) const HYPERLIQUID: &[HistoryChannel] = &[
    HistoryChannel::built_keyless("candleSnapshot candles", BARS, HistoryChannelLane::Klines),
    HistoryChannel::built_keyless("funding history", FUNDING_RATES, HistoryChannelLane::Funding),
];
