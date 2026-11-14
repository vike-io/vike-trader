//! Dukascopy's archive lane: what `crate::archive_import`'s shared per-day flow asks of this venue.
//! The key templates, the provisional-window supersede and the sentences that name the HTTP lane are
//! here; the flow, the one-owner table and the day-owner lock are not.

use vike_data::StoreKindId;
use vike_data::archive_day::{ArchiveDayBars, ArchiveDayRefusal};
use vike_data::{SeriesId, TimestampRange};
use vike_marketdata::TickQuote;
use vike_store::HistoryStoreDataFusion;

use crate::archive_import::{
    ArchiveDayAppend, ArchiveImport, ArchiveLane, DayOwnerRole, DayOwners,
};
use crate::error::CollectError;

use super::report::refusal_remedy;
use super::{
    VENUE, archive_quote_commit_key, provisional_quote_commit_key, quote_series,
    resample_commit_key, resample_settled,
};

/// Dukascopy's daily `.bi5` files, as quotes under `venue=dukascopy`.
pub struct DukascopyLane {
    symbol: String,
}

impl<'h> ArchiveImport<'h, DukascopyLane> {
    /// [`ArchiveImport::open`] over `symbol`'s Dukascopy quote series.
    ///
    /// Each of `bars` is derived through the HTTP lane's own `resample_settled` under its canonical
    /// `dukascopy-resample:` key, so bars resampled from archive ticks and from fetched ones share keys
    /// and are never written twice. An interval that does not divide a UTC day is refused, and
    /// `data hist fetch` derives it later from the stored ticks.
    ///
    /// `now_ms` is the one clock read the whole request is judged against — a day ending inside the
    /// publication margin of it is `TooRecent`.
    pub fn begin(
        hist: &'h HistoryStoreDataFusion,
        symbol: &str,
        bars: &[String],
        now_ms: i64,
    ) -> Result<Self, CollectError> {
        Self::open(hist, DukascopyLane { symbol: symbol.to_string() }, bars, now_ms)
    }
}

impl ArchiveLane for DukascopyLane {
    type Row = TickQuote;

    fn source(&self) -> &str {
        "dukascopy-archive"
    }

    fn symbol(&self) -> &str {
        &self.symbol
    }

    fn row_series(&self) -> SeriesId {
        quote_series(&self.symbol)
    }

    fn row_timestamp(row: &TickQuote) -> i64 {
        row.timestamp
    }

    fn owners(&self, keys: &[String]) -> DayOwners {
        DayOwners::parse(&self.symbol, keys)
    }

    fn overlapped_detail(&self, keys: &str) -> String {
        format!(
            "Dukascopy quote keys of other bounds already meet this day ({keys}), so \
             importing it would store their ticks twice — it stays the HTTP lane's to \
             fill. Nothing was read and no commit key was spent."
        )
    }

    /// Store the ticks under the archive day key through `append_quotes_superseding`, with the day's
    /// PROVISIONAL key as the one to supersede — a pre-spend on a free day, the removal of the HTTP
    /// lane's early fetch on a supersede day, one code path. The store REFUSING a supersede it can no
    /// longer perform exactly (compaction folded the provisional part into a multi-key part) costs
    /// this one day and no key.
    fn append_day(
        &self,
        hist: &HistoryStoreDataFusion,
        rows: &[TickQuote],
        day: i64,
        end: i64,
        bars: &[String],
    ) -> Result<Result<ArchiveDayAppend, ArchiveDayRefusal>, CollectError> {
        let symbol = &self.symbol;
        let key = archive_quote_commit_key(symbol, day, end);
        let provisional = provisional_quote_commit_key(symbol, day, end);
        let stored =
            hist.append_quotes_superseding(VENUE, symbol, rows, Some(&key), Some(&provisional));
        let rows = match stored {
            Ok(rows) => rows,
            Err(e) if e.is_supersede_refusal() => {
                // The remedy is the HTTP lane's, once per requested interval, since deleting the
                // quote series strands each interval's bars; with none requested, the quote series
                // alone.
                let remedy = if bars.is_empty() {
                    "The refusal is PERMANENT for this day — a re-run refuses the same way — and \
                     only deleting this symbol's dukascopy quote series and importing again \
                     clears it."
                        .to_string()
                } else {
                    let each = bars.iter().map(|iv| refusal_remedy(symbol, iv));
                    each.collect::<Vec<_>>().join(" ")
                };
                return Ok(Err(ArchiveDayRefusal::new(
                    "SupersedeRefused",
                    format!(
                        "{e}. The day's ticks were NOT stored and no commit key was spent. {remedy}"
                    ),
                )));
            }
            Err(e) => return Err(e.into()),
        };
        let spent =
            vec![(DayOwnerRole::Archive, key.clone()), (DayOwnerRole::Provisional, provisional)];
        Ok(Ok(ArchiveDayAppend { rows, key, spent }))
    }

    fn undividable_interval_remedy(&self) -> String {
        "`data hist fetch` derives it from the stored ticks instead.".to_string()
    }

    fn bar_series(&self, interval: &str) -> SeriesId {
        SeriesId::per_symbol(StoreKindId::Bar, VENUE, &self.symbol, Some(interval.to_string()))
    }

    /// The HTTP lane's own canonical key, whose whole buckets for a day-dividing interval are the day.
    fn bar_commit_key(&self, interval: &str, range: TimestampRange) -> String {
        resample_commit_key(&self.symbol, interval, range)
    }

    fn derive_bars(
        &self,
        hist: &HistoryStoreDataFusion,
        interval: &str,
        range: TimestampRange,
    ) -> Result<usize, CollectError> {
        resample_settled(hist, &self.symbol, interval, range)
    }

    /// Which intervals the store refused and why, what the others wrote, whether the day's ticks are
    /// stored, and the remedy, once per refused interval.
    fn bars_refused(
        &self,
        refused: &[(String, String)],
        written: &[ArchiveDayBars],
        rows: &str,
    ) -> ArchiveDayRefusal {
        let why = refused.iter().map(|(iv, e)| format!("{iv}: {e}")).collect::<Vec<_>>().join("; ");
        let others =
            written.iter().map(|b| format!("{} {}", b.rows, b.interval)).collect::<Vec<_>>();
        let remedy = refused
            .iter()
            .map(|(iv, _)| refusal_remedy(&self.symbol, iv))
            .collect::<Vec<_>>()
            .join(" ");
        ArchiveDayRefusal::new(
            "SupersedeRefused",
            format!(
                "the store REFUSED to supersede a provisional bar entry it could no longer remove \
                 exactly ({why}), so those bars were not written; {rows}. Bars written for the \
                 other intervals: {others:?}. {remedy}"
            ),
        )
    }
}
