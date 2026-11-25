//! **One archive-import day's refusal and bar count**, the words the store half
//! (`vike_backfill::archive_import`) mints and the datahub's archive verb carries
//! (`vike_datahub_client::archive`) unchanged: one type each, here because both crates name this one,
//! the client sits below the store half and cannot name it, and the store half names no wire type.
//!
//! The serde derives ARE the wire's: `{"class", "detail"}` and `{"interval", "rows"}`, field names and
//! order as the archive verb has always sent them. A field added here is a wire change.
//!
//! Beside them, the import's FORMAT ids (the `*_FORMAT` constants): the word an import names its format
//! by on the wire (`ImportSpec::format`), the one the datahub's registry answers for it
//! (`ArchiveFormat::id`), and the word `vike-backfill`'s producer spells its day keys with; most are also
//! the `source=` lane `STORE_KINDS` files the days under. Here because the store half produces them and
//! the datahub and the CLI name them, and this crate is below all three. The text model's scores keep
//! theirs beside their row (`crate::text_score_log::TEXT_SCORE_FORMAT`).

use serde::{Deserialize, Serialize};

/// Dukascopy's daily `.bi5` tick files.
pub const DUKASCOPY_BI5_FORMAT: &str = "dukascopy-bi5";
/// Binance's daily aggregate-trades zips (`aggTrades`), from which the import derives bars.
pub const BINANCE_AGGREGATE_TRADES_FORMAT: &str = "binance-aggtrades";
/// Binance's daily klines zips: the venue's own candles, at the one interval a file holds.
pub const BINANCE_KLINES_FORMAT: &str = "binance-klines";
/// Binance's daily option-market zips of the hourly chain summary (`EOHSummary`).
pub const BINANCE_OPTION_SUMMARY_FORMAT: &str = "binance-eohsummary";
/// Binance's daily option-market zips of the volatility index (`BVOLIndex`).
pub const BINANCE_VOLATILITY_INDEX_FORMAT: &str = "binance-bvolindex";
/// A user's CSV of bars, stored only in a `user-<label>` lane (decision 0268).
pub const CSV_BARS_FORMAT: &str = "csv-bars";
/// SEC EDGAR's filing texts.
pub const SEC_FILINGS_FORMAT: &str = "sec-filings";
/// SEC EDGAR's company facts, the XBRL values.
pub const SEC_COMPANY_FACTS_FORMAT: &str = "sec-companyfacts";
/// SEC EDGAR's Form 13F lines, the institutional holdings.
pub const SEC_FORM_13F_FORMAT: &str = "sec-form13f";

/// The [`ArchiveDayRefusal::class`] of a day the store half refuses because it has not ended, or ended
/// inside its format's publication margin: its file may still be partial, so no commit key is spent and the
/// same day imports once it is old enough.
pub const TOO_RECENT: &str = "TooRecent";

/// Why ONE archive day was refused (or not wholly stored): a stable CLASS token and an operator's
/// sentence.
///
/// ⚠ **A string class rather than an enum, because of layering:** the classes are each FORMAT's own
/// (`AmbiguousTimeBase`, `NotMonotonic` for a Dukascopy daily file), minted below every format
/// (`TooRecent`, `Overlapped`, `OutsideDay` by the store half) or by a lane and a decoder, so an enum
/// would make each new decoder refusal a wire change. `class` is a CamelCase token a client may match
/// on; `detail` says what was wrong, what was and was not written, and what to do. Neither carries
/// file bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveDayRefusal {
    /// The refusal's stable class token — `AmbiguousTimeBase`, `NotMonotonic`, `MixedLayout`…
    pub class: String,
    /// An operator's sentence: what was wrong, and whether a commit key was spent.
    pub detail: String,
}

impl ArchiveDayRefusal {
    /// A refusal of `class` saying `detail`.
    pub fn new(class: &str, detail: String) -> Self {
        Self { class: class.to_string(), detail }
    }
}

/// How many bars ONE interval wrote for one day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveDayBars {
    /// The interval, as the request named it.
    pub interval: String,
    /// Bars written — `0` when the day's bars at this interval were already stored.
    pub rows: u64,
}
