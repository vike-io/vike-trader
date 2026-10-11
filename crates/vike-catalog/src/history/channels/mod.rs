//! The history table's rows, one file per venue group, and the kind sets the rows share.

use crate::history::{HistoryChannel, HistoryKind};

mod crypto;
mod dukascopy;
mod ibkr;
mod oanda;
mod other;

pub(super) use crypto::{ASTER, BINANCE, BYBIT, DERIBIT, HYPERLIQUID, OKX};
pub(super) use dukascopy::DUKASCOPY;
pub(super) use ibkr::IBKR;
pub(super) use oanda::OANDA;
pub(super) use other::{FXCM, IG, POLYMARKET};

// The kind sets, named so a row reads as a declaration rather than a literal.
const BARS: &[HistoryKind] = &[HistoryKind::Bars];
const QUOTES: &[HistoryKind] = &[HistoryKind::Quotes];
const FUNDING_RATES: &[HistoryKind] = &[HistoryKind::Funding];
const QUOTES_AND_TRADES: &[HistoryKind] = &[HistoryKind::Quotes, HistoryKind::Trades];
const BOOK_TRADES_QUOTES: &[HistoryKind] =
    &[HistoryKind::Book, HistoryKind::Trades, HistoryKind::Quotes];

/// Nothing is declared — what an unknown venue string gets. Not the same as "none exist".
pub(super) const NOT_DECLARED: &[HistoryChannel] = &[];

/// A venue nobody has looked at, and the row `just new-venue` renders an arm pointing at. It is a
/// real arm's row too (cTrader and Alpaca, which no datahub lane serves and whose vendors were not
/// read), so the scaffold's answer is a tested one rather than a constant only its template
/// mentions — and the tag the scaffold's marker carries is what
/// `no_scaffold_placeholder_survives_into_main` refuses until a human replaces the arm with what
/// was read.
pub(super) const UNCLASSIFIED: &[HistoryChannel] = &[HistoryChannel::unclassified(
    "no datahub lane serves this venue's history and nobody has read the vendor's history limits",
)];
