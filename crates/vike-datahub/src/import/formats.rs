//! **The archive FORMAT registry** — the ONE place naming a format the import lane can read
//! (`docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` §6), behind `backfill-serve`.
//!
//! The `KLINE_SOURCES` shape, in its own file so `crates/vike-ops/tests/collector_dispatch_gate.rs`
//! keeps watching only the venue collectors in `crates/vike-datahub/src/backfill.rs`. It is a
//! CONSTRUCTOR over the served store rather than the design sketch's `static` of unit structs, for
//! the reason `super`'s module doc gives: the lane's seam is feature-free, so the concrete
//! `DataFusionHist` reaches a format through the instance, never through a trait signature.
//!
//! # The one format: Dukascopy's daily `.bi5` files (`dukascopy-bi5`)
//!
//! Each piece is consumed, not re-implemented:
//!
//! - **the grammar, the header, the decoder and the price-scale table** are the bridge's
//!   (`crates/bridges/dukascopy/src/archive.rs`, `crates/bridges/dukascopy/src/price_scale.rs`) —
//!   a dataset is ADMITTED only when the table holds a measured point value for it, and the decoder
//!   refuses rather than guesses (a time base it cannot place, a payload not a whole number of
//!   records, an offset past the day);
//! - **the day key, the one-owner table, the day-owner lock and the per-day store** are the store
//!   half's (`crates/vike-backfill/src/venues/dukascopy.rs`'s `ArchiveImport` and `DayOwners`) —
//!   which takes DECODED quotes through a closure, because `vike-backfill` names no bridge
//!   (`docs/decisions/0094-backfill-names-no-venue.md`). This file is where the bridge's decode
//!   meets the store half's import: inside the lock, only for a day the store half calls the
//!   closure for, so a HELD day's file is never opened.
//!
//! What this file adds is the mapping between the store half's vocabulary and the wire's — and the
//! one rule neither half owns: `verify`'s cross-check of the decoded scale against the store.

use std::sync::Arc;

use vike_backfill::venues::dukascopy::{
    ArchiveBars, ArchiveDayClass, ArchiveDayRefusal, ArchiveDayResult, ArchiveImport, DayOwners,
    VENUE, quote_series_facts,
};
use vike_data::{DataFusionHist, HistStore, SeriesCoverage, TsRange};
use vike_datahub_client::archive::{BarsWritten, DayClass, DayRefusal, DayResult};
use vike_dukascopy::{
    Bi5Day, Bi5Limits, Bi5Path, Bi5PriceScale, Bi5Refusal, Tick, bi5_price_scale,
    classify_bi5_path, decode_daily_file, is_bi5_layout_dir, read_bi5_header, tick_to_quote,
    unscaled_instrument_refusal,
};
use vike_model::MS_PER_DAY;

use super::{ArchiveFormat, ImportSession, LayoutFile};

/// The Dukascopy daily-file format's wire id AND its directory under the imports root.
pub const DUKASCOPY_BI5: &str = "dukascopy-bi5";

/// **Every archive format this daemon can import**, each over `store` — the SAME handle the server
/// serves, so an imported day is visible to the very next read on any connection.
pub fn real_import_registry(store: Arc<DataFusionHist>) -> Vec<Box<dyn ArchiveFormat>> {
    vec![Box::new(DukascopyBi5 { store })]
}

/// The LZMA-alone header a `.bi5` file opens with — one properties byte, the dictionary size as a
/// `u32` and the unpacked size as a `u64` — which `vike_dukascopy::read_bi5_header` reads.
const BI5_HEADER_BYTES: usize = 13;

/// How many days either side of a verified day `verify`'s scale cross-check looks for stored ticks.
const NEIGHBOUR_DAYS: i64 = 3;

/// The row budget of that look — a few days of one pair's ticks is far below it, and the median
/// needs no more.
const NEIGHBOUR_ROWS: usize = 500_000;

/// The ratio band the cross-check admits. A scale error is a POWER OF TEN, so anything inside a
/// factor of two is a market that moved, and anything outside it is a scale that is wrong.
const SCALE_RATIO_MIN: f64 = 0.5;
const SCALE_RATIO_MAX: f64 = 2.0;

/// The refusal class `verify` gives a first day whose median mid disagrees with the stored ticks by
/// more than a factor of two either way (`SCALE_RATIO_MIN`..`SCALE_RATIO_MAX`).
pub const SCALE_MISMATCH: &str = "ScaleMismatch";

/// The refusal class a file that decodes to NO ticks gets.
pub const EMPTY_PAYLOAD: &str = "EmptyPayload";

/// Dukascopy's daily `.bi5` files, landing under `venue=dukascopy`.
struct DukascopyBi5 {
    store: Arc<DataFusionHist>,
}

impl ArchiveFormat for DukascopyBi5 {
    fn id(&self) -> &'static str {
        DUKASCOPY_BI5
    }

    fn venue(&self) -> &'static str {
        VENUE
    }

    /// A dataset is the vendor's instrument folder, and it is admitted ONLY with a measured price
    /// scale (design §3.4): an instrument with no row is refused before a file is opened, quoting
    /// the vendor's own warning about silently wrong prices.
    fn admit(&self, dataset: &str) -> Result<String, String> {
        match bi5_price_scale(dataset) {
            Some(row) => Ok(format!("point value {} — {}", row.point_value(), row.evidence())),
            None => Err(unscaled_instrument_refusal(dataset)),
        }
    }

    fn accepts_dir(&self, rel: &[&str], now_ms: i64) -> bool {
        is_bi5_layout_dir(rel, now_ms)
    }

    fn classify_file(&self, rel: &[&str], now_ms: i64) -> LayoutFile {
        match classify_bi5_path(rel, now_ms) {
            Bi5Path::Daily(day) => LayoutFile::Daily(day.start_ms()),
            Bi5Path::Hourly { day, .. } => LayoutFile::OtherLayout(day.start_ms()),
            Bi5Path::Other => LayoutFile::Other,
        }
    }

    fn max_file_bytes(&self) -> u64 {
        Bi5Limits::DEFAULT.max_compressed_bytes as u64
    }

    fn header_len(&self) -> usize {
        BI5_HEADER_BYTES
    }

    /// The file's SIZE first (the compressed cap), then the header's declared size — both refused
    /// here, at plan time, before anything is decoded.
    fn read_header(&self, file_len: u64, prefix: &[u8]) -> Result<Option<u64>, DayRefusal> {
        let limits = Bi5Limits::DEFAULT;
        if file_len > limits.max_compressed_bytes as u64 {
            let len = usize::try_from(file_len).unwrap_or(usize::MAX);
            let cap = limits.max_compressed_bytes;
            return Err(decode_refusal(&Bi5Refusal::CompressedTooLarge { len, cap }));
        }
        read_bi5_header(prefix, &limits)
            .map(|header| header.declared_ticks())
            .map_err(|refusal| decode_refusal(&refusal))
    }

    /// An IMPORT holds the store half's day-owner lock from here to the end of the request
    /// (`ArchiveImport::begin` — which may wait for an HTTP-lane chunk's store step in flight); a
    /// DRY RUN reads the series' keys once, lock-free (`quote_series_facts`), and classifies against
    /// them exactly as the import would.
    fn session<'a>(
        &'a self,
        dataset: &str,
        bars: &[String],
        now_ms: i64,
        writes: bool,
    ) -> Result<Box<dyn ImportSession + 'a>, String> {
        let scale = bi5_price_scale(dataset).ok_or_else(|| unscaled_instrument_refusal(dataset))?;
        let hist: &'a DataFusionHist = &self.store;
        let store = if writes {
            Bi5Store::Import(
                ArchiveImport::begin(hist, dataset, bars, now_ms).map_err(|e| e.to_string())?,
            )
        } else {
            let (coverage, keys) = quote_series_facts(hist, dataset).map_err(|e| e.to_string())?;
            Bi5Store::Plan { owners: DayOwners::parse(dataset, &keys), coverage, now_ms }
        };
        Ok(Box::new(Bi5Session {
            hist,
            symbol: dataset.to_string(),
            scale,
            store,
            cross_checked: false,
        }))
    }
}

/// The store side of one request: lock-free for a dry run, the locked import session otherwise.
enum Bi5Store<'h> {
    Plan { owners: DayOwners, coverage: SeriesCoverage, now_ms: i64 },
    Import(ArchiveImport<'h>),
}

struct Bi5Session<'h> {
    hist: &'h DataFusionHist,
    symbol: String,
    scale: &'static Bi5PriceScale,
    store: Bi5Store<'h>,
    /// Set once `verify` has run its scale cross-check — on the request's first decoded day.
    cross_checked: bool,
}

impl ImportSession for Bi5Session<'_> {
    fn series(&self) -> Option<SeriesCoverage> {
        let coverage = match &self.store {
            Bi5Store::Plan { coverage, .. } => coverage,
            Bi5Store::Import(import) => import.coverage(),
        };
        (coverage.rows > 0).then(|| coverage.clone())
    }

    fn classify(&self, day: i64) -> DayClass {
        wire_class(match &self.store {
            Bi5Store::Plan { owners, now_ms, .. } => owners.classify(day, *now_ms),
            Bi5Store::Import(import) => import.classify(day),
        })
    }

    /// The store half's `import_day`, with a `load` that reads the day's file through the lane's
    /// safe open and decodes it with the bridge — called by the store half ONLY for a FREE or
    /// SUPERSEDE day, inside the lock.
    fn import_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, DayRefusal>,
    ) -> Result<DayResult, String> {
        let Bi5Store::Import(import) = &mut self.store else {
            return Err("a dry run's session cannot import — this is a lane defect".to_string());
        };
        let Some(bi5_day) = Bi5Day::from_start_ms(day) else {
            return Err(format!("{day} is not the start of a UTC day at or after 1970"));
        };
        let (scale, symbol) = (self.scale, self.symbol.as_str());
        let result = import
            .import_day(day, || {
                let blob = read().map_err(store_refusal)?;
                let ticks = decode_daily_file(&blob, bi5_day, scale, &Bi5Limits::DEFAULT)
                    .map_err(|refusal| store_refusal(decode_refusal(&refusal)))?;
                drop(blob);
                Ok(ticks.iter().map(|tick| tick_to_quote(tick, symbol)).collect())
            })
            .map_err(|e| e.to_string())?;
        Ok(wire_result(result))
    }

    /// Decode the day and check it — the bridge's refusals, the empty payload, and on the request's
    /// FIRST decoded day the scale cross-check — writing nothing.
    fn verify_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, DayRefusal>,
    ) -> Result<DayResult, String> {
        let Some(bi5_day) = Bi5Day::from_start_ms(day) else {
            return Err(format!("{day} is not the start of a UTC day at or after 1970"));
        };
        let blob = match read() {
            Ok(blob) => blob,
            Err(refusal) => return Ok(DayResult::Refused(refusal)),
        };
        let ticks = match decode_daily_file(&blob, bi5_day, self.scale, &Bi5Limits::DEFAULT) {
            Ok(ticks) => ticks,
            Err(refusal) => return Ok(DayResult::Refused(decode_refusal(&refusal))),
        };
        drop(blob);
        if ticks.is_empty() {
            return Ok(DayResult::Refused(empty_payload()));
        }
        if !self.cross_checked {
            self.cross_checked = true;
            if let Some(refusal) = self.scale_cross_check(day, &ticks)? {
                return Ok(DayResult::Refused(refusal));
            }
        }
        Ok(DayResult::Verified { ticks: ticks.len() as u64 })
    }
}

impl Bi5Session<'_> {
    /// **`verify`'s one cheap cross-check (design §3.4):** the median mid of the first decoded day
    /// against the median mid of the ticks the store already holds within [`NEIGHBOUR_DAYS`] of it
    /// (before it first, then after). A ratio outside [`SCALE_RATIO_MIN`]..[`SCALE_RATIO_MAX`] is a
    /// scale error — a power of ten — and the day is refused as [`SCALE_MISMATCH`]. No stored
    /// neighbour, no check: the series holds nothing to compare against, which a first import of a
    /// fresh instrument always meets.
    ///
    /// ⚠ The neighbour is whatever the store holds there, from EITHER lane — a stored row does not
    /// record which lane wrote it. The design names the HTTP lane's ticks; an archive-imported
    /// neighbour was scaled by this same table, so comparing against one can only agree, which is
    /// harmless rather than wrong.
    fn scale_cross_check(&self, day: i64, ticks: &[Tick]) -> Result<Option<DayRefusal>, String> {
        let Some(file_mid) = median(ticks.iter().map(|t| (t.bid + t.ask) / 2.0)) else {
            return Ok(None);
        };
        let Some(stored_mid) = self.neighbour_median(day)? else {
            return Ok(None);
        };
        let ratio = file_mid / stored_mid;
        if (SCALE_RATIO_MIN..=SCALE_RATIO_MAX).contains(&ratio) {
            return Ok(None);
        }
        Ok(Some(DayRefusal {
            class: SCALE_MISMATCH.to_string(),
            detail: format!(
                "the file's median mid ({file_mid}) is {ratio:.4} times the median mid of the ticks \
                 this store already holds beside it ({stored_mid}) — outside [{SCALE_RATIO_MIN}, \
                 {SCALE_RATIO_MAX}], which is what a wrong price scale (a power of ten) looks like. \
                 Point value {} is this instrument's table row; do not import until the two agree. \
                 Nothing was written.",
                self.scale.point_value()
            ),
        }))
    }

    /// The median mid of the stored ticks nearest `day`: the [`NEIGHBOUR_DAYS`] before it, else the
    /// [`NEIGHBOUR_DAYS`] after it, else `None`.
    fn neighbour_median(&self, day: i64) -> Result<Option<f64>, String> {
        let before = TsRange::of(day.saturating_sub(NEIGHBOUR_DAYS * MS_PER_DAY), day - 1);
        let after_start = day + MS_PER_DAY;
        let after = TsRange::of(after_start, after_start + NEIGHBOUR_DAYS * MS_PER_DAY - 1);
        for range in [before, after] {
            let rows = self
                .hist
                .scan_quotes_capped(VENUE, &self.symbol, range, Some(NEIGHBOUR_ROWS))
                .map_err(|e| e.to_string())?;
            if let Some(mid) = median(rows.iter().map(|q| (q.bid + q.ask) / 2.0)) {
                return Ok(Some(mid));
            }
        }
        Ok(None)
    }
}

/// The median of the finite, positive values — `None` when there is none.
fn median(values: impl Iterator<Item = f64>) -> Option<f64> {
    let mut v: Vec<f64> = values.filter(|x| x.is_finite() && *x > 0.0).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
}

/// A bridge refusal as the wire carries it: the class token and the bridge's own sentence, which
/// names sizes, caps and a record index — never payload bytes.
fn decode_refusal(refusal: &Bi5Refusal) -> DayRefusal {
    DayRefusal {
        class: refusal.class().to_string(),
        detail: format!("{refusal}. Nothing was stored and no commit key was spent."),
    }
}

/// The store half reports an empty payload as its own result; the wire has no such variant, and a
/// day that stored nothing is not "imported" — it is refused, with no key spent.
fn empty_payload() -> DayRefusal {
    DayRefusal {
        class: EMPTY_PAYLOAD.to_string(),
        detail: "the file decodes to no ticks — per the vendor, nothing was recorded that day. \
                 Nothing was stored and no commit key was spent, so the day stays free for a \
                 re-published file."
            .to_string(),
    }
}

fn store_refusal(refusal: DayRefusal) -> ArchiveDayRefusal {
    ArchiveDayRefusal { class: refusal.class, detail: refusal.detail }
}

/// The store half's day table onto the wire's — one variant for one, in the same order.
fn wire_class(class: ArchiveDayClass) -> DayClass {
    match class {
        ArchiveDayClass::TooRecent => DayClass::TooRecent,
        ArchiveDayClass::HeldByArchive => DayClass::HeldByArchive,
        ArchiveDayClass::HeldByHttp => DayClass::HeldByHttp,
        ArchiveDayClass::Supersede { key } => DayClass::Supersede { key },
        ArchiveDayClass::Overlapped { keys } => DayClass::Overlapped { keys },
        ArchiveDayClass::Free => DayClass::Free,
    }
}

fn wire_result(result: ArchiveDayResult) -> DayResult {
    match result {
        ArchiveDayResult::Imported { ticks, bars } => {
            DayResult::Imported { ticks: ticks as u64, bars: bars_written(bars) }
        }
        ArchiveDayResult::Empty => DayResult::Refused(empty_payload()),
        ArchiveDayResult::ToppedUp { bars } => DayResult::ToppedUp { bars: bars_written(bars) },
        ArchiveDayResult::Refused(refusal) => {
            DayResult::Refused(DayRefusal { class: refusal.class, detail: refusal.detail })
        }
    }
}

fn bars_written(bars: Vec<ArchiveBars>) -> Vec<BarsWritten> {
    bars.into_iter().map(|b| BarsWritten { interval: b.interval, rows: b.rows as u64 }).collect()
}
