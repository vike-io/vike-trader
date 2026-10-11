//! The archive import's store step: one request's hold on a `(store root, symbol)`, day by day.

use vike_data::StoreKindId;
use vike_data::{DataFusionHist, SeriesCoverage, SeriesId, TsRange};
use vike_model::{MS_PER_DAY, QuoteTick};

use crate::error::CollectError;

#[cfg(doc)]
use super::backfill_quotes_then_bars;
use super::chunks::archive_day_end;
use super::day_owner::{ArchiveDayClass, DayOwnerLock, DayOwners, quote_series_facts};
use super::report::refusal_remedy;
use super::{
    PUBLICATION_MARGIN_MS, VENUE, archive_quote_commit_key, provisional_quote_commit_key,
    resample_commit_key, resample_settled,
};

/// Why ONE archive day was not stored (or not wholly): a stable CLASS token and an operator's
/// sentence. The datahub carries both onto the wire's `DayRefusal` unchanged.
///
/// The classes this module mints are `TooRecent`, `Overlapped`, `OutsideDay` and
/// `SupersedeRefused`; a `load` closure handed to [`ArchiveImport::import_day`] mints its own — the
/// decoder's refusal classes — and the import passes them through untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveDayRefusal {
    /// The refusal's stable CamelCase class token.
    pub class: String,
    /// What was wrong, and what was and was not written.
    pub detail: String,
}

impl ArchiveDayRefusal {
    fn new(class: &str, detail: String) -> Self {
        Self { class: class.to_string(), detail }
    }
}

/// How many bars ONE interval wrote for one day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveBars {
    /// The interval, as the request named it.
    pub interval: String,
    /// Bars written — `0` when the day's bars at this interval were already stored.
    pub rows: usize,
}

/// What [`ArchiveImport::import_day`] did with one day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveDayResult {
    /// Stored: `ticks` quotes under the archive day key, and the bars derived from them.
    Imported {
        /// Quote rows stored.
        ticks: usize,
        /// Bars written per requested interval.
        bars: Vec<ArchiveBars>,
    },
    /// The file decoded to NO ticks — per the vendor, nothing was recorded that day. Nothing is
    /// stored and no key is spent, so the day stays [`ArchiveDayClass::Free`] and a later re-publish
    /// can still fill it.
    Empty,
    /// A HELD day: nothing decoded, and every requested interval derived from the stored ticks — `0`
    /// rows for one already stored.
    ToppedUp {
        /// Bars written per requested interval.
        bars: Vec<ArchiveBars>,
    },
    /// The day was refused, and the request carries on to the next. Its `detail` says whether its
    /// ticks were stored: a refusal before or AT the quote write spends no key; a refusal of a BAR
    /// supersede leaves the day's ticks stored and its bars at that interval not.
    Refused(ArchiveDayRefusal),
}

/// ONE archive import request's hold on a `(store root, symbol)`: the day-owner lock, the series'
/// keys read once under it, and the request's bar intervals — what [`Self::import_day`] stores each
/// day through. Dropping it releases the lock.
///
/// The datahub's shape is: walk and plan, then `begin`, then [`Self::import_day`] for every day of
/// the request, in order, then drop. The lock is held for the whole request — the design's §5 caps
/// a request at 31 days, which is the datahub's to enforce at its door — so a day this session
/// classified cannot be written by the HTTP lane before this session stores it: that lane takes the
/// same lock for each of its store steps. ⚠ So a thread holding one must not run
/// [`backfill_quotes_then_bars`] over the same series: the lock is not re-entrant.
pub struct ArchiveImport<'h> {
    hist: &'h DataFusionHist,
    symbol: String,
    bars: Vec<String>,
    now_ms: i64,
    coverage: SeriesCoverage,
    owners: DayOwners,
    _owner: DayOwnerLock,
}

impl<'h> ArchiveImport<'h> {
    /// Validate `bars`, take the day-owner lock for `(hist's root, symbol)` — waiting for any HTTP
    /// chunk's store step in flight — and read the quote series' coverage and keys ONCE.
    ///
    /// `bars` are the intervals derived per stored day, each through the HTTP lane's own
    /// `resample_settled` under its canonical `dukascopy-resample:` key, so bars resampled from
    /// archive ticks and from fetched ones share keys and are never written twice. Each must have a
    /// positive width that DIVIDES a UTC day; one that does not (`7m`) has buckets straddling
    /// midnight, which a day's import cannot build, and is REFUSED here, before anything is locked,
    /// read or written — `data hist fetch` derives it later from the stored ticks. An empty list
    /// imports ticks alone.
    ///
    /// `now_ms` is the one clock read the whole request is judged against — a day ending inside the
    /// publication margin of it is [`ArchiveDayClass::TooRecent`].
    pub fn begin(
        hist: &'h DataFusionHist,
        symbol: &str,
        bars: &[String],
        now_ms: i64,
    ) -> Result<Self, CollectError> {
        for interval in bars {
            let divides = vike_model::time::interval_ms(interval)
                .is_some_and(|step| step > 0 && MS_PER_DAY % step == 0);
            if !divides {
                return Err(CollectError::Refused(format!(
                    "dukascopy-archive {symbol}: interval {interval:?} does not divide a UTC day, so \
                     a day's import cannot build its bars — a bucket would straddle midnight. \
                     `data hist fetch` derives it from the stored ticks instead. Nothing was \
                     locked, read or written."
                )));
            }
        }
        let owner = DayOwnerLock::acquire(hist, symbol);
        let (coverage, keys) = quote_series_facts(hist, symbol)?;
        Ok(Self {
            hist,
            symbol: symbol.to_string(),
            bars: bars.to_vec(),
            now_ms,
            coverage,
            owners: DayOwners::parse(symbol, &keys),
            _owner: owner,
        })
    }

    /// The quote series' coverage as this session read it, under the lock — what a plan reports as
    /// the store's size before the import.
    pub fn coverage(&self) -> &SeriesCoverage {
        &self.coverage
    }

    /// [`DayOwners::classify`] for `day`, against the keys this session read and the days it has
    /// imported since, at the session's `now_ms`.
    pub fn classify(&self, day: i64) -> ArchiveDayClass {
        self.owners.classify(day, self.now_ms)
    }

    /// Import the UTC day starting at `day`: classify it, and act on its class — the design's §4.2
    /// table and §4.4:
    ///
    /// - [`ArchiveDayClass::Free`] / [`ArchiveDayClass::Supersede`]: call `load` (the decode), store
    ///   the ticks under the archive day key through `append_quotes_superseding` with the day's
    ///   PROVISIONAL key as the one to supersede — a pre-spend on a free day, the removal of the HTTP
    ///   lane's early fetch on a supersede day, one code path — then derive every requested interval.
    /// - [`ArchiveDayClass::HeldByArchive`] / [`ArchiveDayClass::HeldByHttp`]: `load` is NEVER called;
    ///   each requested interval still missing derives from the stored ticks — which is also how a
    ///   request killed between a day's ticks and its bars RESUMES.
    /// - [`ArchiveDayClass::TooRecent`] / [`ArchiveDayClass::Overlapped`]: `load` is never called and
    ///   nothing is written; the day is [`ArchiveDayResult::Refused`].
    ///
    /// The answer is per day, so the caller goes on to the next: `load`'s own refusal (a file that
    /// does not decode), an empty payload, a tick outside the day, and the store REFUSING a supersede
    /// it can no longer perform exactly (compaction folded the provisional part into a multi-key part)
    /// each cost that one day and no key. `Err` is reserved for what is not one day's problem — the
    /// store failing, or a `day` that is not a UTC midnight at or after 1970, which is the caller's
    /// mistake and is refused before anything is read.
    ///
    /// Holds at most ONE day's ticks: they are dropped once stored, before the resample reads the
    /// day back. Logs one line per day, the boundary.
    pub fn import_day(
        &mut self,
        day: i64,
        load: impl FnOnce() -> Result<Vec<QuoteTick>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResult, CollectError> {
        let symbol = self.symbol.clone();
        let Some(end) = archive_day_end(day) else {
            return Err(CollectError::Refused(format!(
                "dukascopy-archive {symbol}: {day} is not the start of a UTC day at or after 1970, \
                 which is what an archive day is. Nothing was read or written."
            )));
        };
        let result = match self.classify(day) {
            ArchiveDayClass::TooRecent => ArchiveDayResult::Refused(ArchiveDayRefusal::new(
                "TooRecent",
                format!(
                    "the day ends inside the {PUBLICATION_MARGIN_MS} ms publication margin, so the \
                     vendor may still be publishing it — import it again later. Nothing was read \
                     and no commit key was spent."
                ),
            )),
            ArchiveDayClass::Overlapped { keys } => {
                let keys = keys.join(", ");
                ArchiveDayResult::Refused(ArchiveDayRefusal::new(
                    "Overlapped",
                    format!(
                        "Dukascopy quote keys of other bounds already meet this day ({keys}), so \
                         importing it would store their ticks twice — it stays the HTTP lane's to \
                         fill. Nothing was read and no commit key was spent."
                    ),
                ))
            }
            ArchiveDayClass::HeldByArchive | ArchiveDayClass::HeldByHttp => {
                let DayBars { written, refused } = self.resample_day(day, end)?;
                if refused.is_empty() {
                    ArchiveDayResult::ToppedUp { bars: written }
                } else {
                    ArchiveDayResult::Refused(self.bar_refusal(
                        &refused,
                        &written,
                        "its ticks were already stored, and stay so",
                    ))
                }
            }
            ArchiveDayClass::Free | ArchiveDayClass::Supersede { .. } => {
                self.store_day(day, end, load)?
            }
        };
        tracing::info!("dukascopy-archive {symbol} [{day}, {end}]: {result:?}");
        Ok(result)
    }

    /// The store half of a FREE or SUPERSEDE day — see [`Self::import_day`].
    fn store_day(
        &mut self,
        day: i64,
        end: i64,
        load: impl FnOnce() -> Result<Vec<QuoteTick>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResult, CollectError> {
        let quotes = match load() {
            Ok(quotes) => quotes,
            Err(refusal) => return Ok(ArchiveDayResult::Refused(refusal)),
        };
        if quotes.is_empty() {
            return Ok(ArchiveDayResult::Empty);
        }
        // The decoder places every tick inside its file's day; this holds the one-owner rule even
        // against a caller that did not, because a stray tick would sit on a day this key does not
        // name — and another lane may own that day.
        if let Some(stray) = quotes.iter().find(|q| !(day..=end).contains(&q.ts)) {
            return Ok(ArchiveDayResult::Refused(ArchiveDayRefusal::new(
                "OutsideDay",
                format!(
                    "a tick at {} lies outside the day [{day}, {end}] the file names. Nothing was \
                     stored and no commit key was spent.",
                    stray.ts
                ),
            )));
        }
        let key = archive_quote_commit_key(&self.symbol, day, end);
        let provisional = provisional_quote_commit_key(&self.symbol, day, end);
        let stored = self.hist.append_quotes_superseding(
            VENUE,
            &self.symbol,
            &quotes,
            Some(&key),
            Some(&provisional),
        );
        let ticks = match stored {
            Ok(rows) => rows,
            Err(e) if e.is_supersede_refusal() => {
                // The remedy is the HTTP lane's, once per requested interval, since deleting the
                // quote series strands each interval's bars; with none requested, the quote series
                // alone.
                let remedy = if self.bars.is_empty() {
                    "The refusal is PERMANENT for this day — a re-run refuses the same way — and \
                     only deleting this symbol's dukascopy quote series and importing again \
                     clears it."
                        .to_string()
                } else {
                    let each = self.bars.iter().map(|iv| refusal_remedy(&self.symbol, iv));
                    each.collect::<Vec<_>>().join(" ")
                };
                return Ok(ArchiveDayResult::Refused(ArchiveDayRefusal::new(
                    "SupersedeRefused",
                    format!(
                        "{e}. The day's ticks were NOT stored and no commit key was spent. {remedy}"
                    ),
                )));
            }
            Err(e) => return Err(e.into()),
        };
        // The resample re-reads the day from the store: the decoded ticks are dead weight now.
        drop(quotes);
        self.owners.record_import(day, end, key.clone(), provisional);
        let DayBars { written, refused } = self.resample_day(day, end)?;
        if refused.is_empty() {
            return Ok(ArchiveDayResult::Imported { ticks, bars: written });
        }
        let stored = format!("its {ticks} ticks ARE stored under `{key}`, and stay so");
        Ok(ArchiveDayResult::Refused(self.bar_refusal(&refused, &written, &stored)))
    }

    /// Derive every requested interval for `[day, end]` from the stored ticks, skipping one whose
    /// canonical resample key is already spent without reading the day back — the logic of the HTTP
    /// lane's `resample_stored_chunk`, whose whole buckets for a day-dividing interval are the day.
    fn resample_day(&self, day: i64, end: i64) -> Result<DayBars, CollectError> {
        let range = TsRange::of(day, end);
        let mut bars = DayBars { written: Vec::new(), refused: Vec::new() };
        for interval in &self.bars {
            let series =
                SeriesId::per_symbol(StoreKindId::Bar, VENUE, &self.symbol, Some(interval.clone()));
            let key = resample_commit_key(&self.symbol, interval, range);
            let rows = if self.hist.series_has_commit(&series, &key)? {
                0
            } else {
                match resample_settled(self.hist, &self.symbol, interval, range) {
                    Ok(rows) => rows,
                    Err(CollectError::Data(e)) if e.is_supersede_refusal() => {
                        bars.refused.push((interval.clone(), e.to_string()));
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            };
            bars.written.push(ArchiveBars { interval: interval.clone(), rows });
        }
        Ok(bars)
    }

    /// The refusal a day ends in when the store refused the BAR-side supersede of some intervals:
    /// which ones and why, what the others wrote, `ticks` — whether the day's ticks are stored — and
    /// the remedy, once per refused interval.
    fn bar_refusal(
        &self,
        refused: &[(String, String)],
        written: &[ArchiveBars],
        ticks: &str,
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
                 exactly ({why}), so those bars were not written; {ticks}. Bars written for the \
                 other intervals: {others:?}. {remedy}"
            ),
        )
    }
}

/// What [`ArchiveImport`]'s per-day resample did, interval by interval.
struct DayBars {
    /// The bars written per requested interval — `0` for one already stored.
    written: Vec<ArchiveBars>,
    /// The intervals whose BAR-side supersede the store refused, each with the store's reason.
    refused: Vec<(String, String)>,
}
