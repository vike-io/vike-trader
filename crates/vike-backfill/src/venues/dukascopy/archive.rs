//! Dukascopy's archive lane: what `crate::archive_import`'s shared per-day flow asks of this venue.
//! The key templates, the provisional-window supersede and the sentences that name the HTTP lane are
//! here; the flow, the one-owner table and the day-owner lock are not.
//!
//! A tick has no whole-day count, so a day is HOLED (decision 0268, point 4) when the HTTP lane's keys
//! meeting it leave part of it uncovered: the ragged first chunk of a `--days N` fetch is one. Under
//! `--replace-holed` such a day's ticks, and each requested interval's bars, are replaced whole by the
//! file's (`day_fill`, `replace_day_bars`, `replace_day`); a day the keys cover whole is never.

use vike_data::archive_day::{ArchiveDayBars, ArchiveDayRefusal};
use vike_data::{HistoryStore, SeriesId, TimestampRange};
use vike_marketdata::{TickQuote, consolidate_quotes};
use vike_model::MS_PER_DAY;
use vike_model::StoreKindId;
use vike_model::time::epoch_ms_to_utc_date;
use vike_store::HistoryStoreDataFusion;

use crate::archive_import::{
    ArchiveDayAppend, ArchiveDayFill, ArchiveImport, ArchiveLane, DayOwnerRole, DayOwners,
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

impl DukascopyLane {
    /// The lane over `symbol`'s quote series: what a dry run asks how full a day is
    /// ([`ArchiveLane::day_fill`]); an import builds its own through [`ArchiveImport::begin`].
    pub fn new(symbol: &str) -> Self {
        Self { symbol: symbol.to_string() }
    }
}

impl<'h> ArchiveImport<'h, DukascopyLane> {
    /// [`ArchiveImport::open`] over `symbol`'s Dukascopy quote series.
    ///
    /// Each of `bars` is derived through the HTTP lane's own `resample_settled` under its canonical
    /// `dukascopy-resample:` key, so bars resampled from archive ticks and from fetched ones share keys
    /// and are never written twice. An interval that does not divide a UTC day is refused, and
    /// `data history fetch` derives it later from the stored ticks.
    ///
    /// `now_ms` is the one clock read the whole request is judged against — a day ending inside the
    /// publication margin of it is `TooRecent`.
    pub fn begin(
        hist: &'h HistoryStoreDataFusion,
        symbol: &str,
        bars: &[String],
        now_ms: i64,
    ) -> Result<Self, CollectError> {
        Self::open(hist, DukascopyLane::new(symbol), bars, now_ms)
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
             fill; --replace-holed replaces the day whole when those keys leave part of it \
             uncovered. Nothing was read and no commit key was spent."
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
        "`data history fetch` derives it from the stored ticks instead.".to_string()
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

    /// The milliseconds of the day the keys meeting it cover, against a whole day's, and the ticks
    /// stored on it. A day covered whole is answered `None` without reading a tick: it is never
    /// replaced.
    fn day_fill(
        &self,
        hist: &HistoryStoreDataFusion,
        owners: &DayOwners,
        day: i64,
        end: i64,
    ) -> Result<Option<ArchiveDayFill>, CollectError> {
        let covered = owners.covered_ms(day, end);
        if covered >= MS_PER_DAY {
            return Ok(None);
        }
        let rows = hist.scan_quotes(VENUE, &self.symbol, TimestampRange::of(day, end))?.len();
        let (stored, full) = (covered as usize, MS_PER_DAY as usize);
        Ok(Some(ArchiveDayFill { stored, full, rows }))
    }

    /// The file's ticks folded into `interval`'s bars, which replace the day's in the bar series
    /// under the key [`Self::derive_bars`] spends for a day, so a later request finds it spent.
    fn replace_day_bars(
        &self,
        hist: &HistoryStoreDataFusion,
        interval: &str,
        rows: &[TickQuote],
        day: i64,
        end: i64,
    ) -> Result<usize, CollectError> {
        let Some(step) = vike_marketdata::interval::bar_step_ms(interval) else {
            return Err(CollectError::Refused(format!(
                "dukascopy-archive {}: interval {interval:?} has no positive bar width. Nothing was \
                 written.",
                self.symbol
            ).into()));
        };
        let key = resample_commit_key(&self.symbol, interval, TimestampRange::of(day, end));
        let bars = consolidate_quotes(rows, step);
        let date = epoch_ms_to_utc_date(day);
        Ok(hist.replace_bars_day(VENUE, &self.symbol, interval, &date, &bars, &key)?)
    }

    /// The file's ticks in place of every tick the store holds on the day, under the archive day key,
    /// in one publish (`vike_store::HistoryStoreDataFusion`'s `replace_quotes_day`), every removed key
    /// still spent.
    fn replace_day(
        &self,
        hist: &HistoryStoreDataFusion,
        rows: &[TickQuote],
        day: i64,
        end: i64,
    ) -> Result<Result<ArchiveDayAppend, ArchiveDayRefusal>, CollectError> {
        let key = archive_quote_commit_key(&self.symbol, day, end);
        let date = epoch_ms_to_utc_date(day);
        let rows = hist.replace_quotes_day(VENUE, &self.symbol, &date, rows, &key)?;
        let spent = vec![(DayOwnerRole::Archive, key.clone())];
        Ok(Ok(ArchiveDayAppend { rows, key, spent }))
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
