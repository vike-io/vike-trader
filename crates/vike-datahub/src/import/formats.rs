//! **The archive FORMAT registry** — the ONE place naming a format the import lane can read
//! (`docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` §6), behind `backfill-serve`.
//!
//! The `KLINE_SOURCES` shape, in its own file so `crates/vike-ops/tests/venues/collector_dispatch_gate.rs`
//! keeps watching only the venue collectors in the gatewayhub's registry (`crates/vike-gatewayhub/src/rest/kline_sources.rs`). It is a
//! CONSTRUCTOR over the served store rather than the design sketch's `static` of unit structs, for
//! the reason `super`'s module doc gives: the lane's seam is feature-free, so the concrete
//! `HistoryStoreDataFusion` reaches a format through the instance, never through a trait signature.
//!
//! A second format, Binance's daily aggTrades zips, is the child module [`binance_aggtrades`]; a
//! third, Binance's klines zips, the venue's own candles, is [`binance_klines`]; the fourth and
//! fifth, Binance's option-market daily zips (the hourly chain summary and the volatility index),
//! are [`binance_options`]; a text model's scores of the news (the files `vike-nlp score` writes) are
//! [`text_scores`]; the last, a user's CSV of bars, written only to a `user-<label>` lane, is
//! [`csv_bars`].
//!
//! # Dukascopy's daily `.bi5` files (`dukascopy-bi5`)
//!
//! Each piece is consumed, not re-implemented:
//!
//! - **the grammar, the header, the decoder and the price scale** are `vike-bi5`'s
//!   (`crates/vike-bi5/src/archive.rs`, `crates/vike-bi5/src/price_scale.rs`) —
//!   a dataset is ADMITTED only when the kept dukascopy instrument list (`crate::instrument_lists`,
//!   `<project>/instruments/dukascopy.json`) holds a row for it whose price step gives the point value
//!   (`vike_bi5::Bi5PriceScale::from_tick_size`; no instrument is compiled in, owner 2026-11-13), and the
//!   decoder refuses rather than guesses (a time base it cannot place, a payload not a whole number of
//!   records, an offset past the day). ⚠ The dataset is the VENDOR's folder (`AAPLUSUSD`) and names the
//!   files only: its row is found by `vike_catalog::bi5_dataset_row`, and the series is filed under the
//!   row's STORE name (`AAPL.CFD`, decision 0290), the name the HTTP tick lane and a backtest name too;
//! - **the day key, the one-owner table, the day-owner lock and the per-day store** are the store
//!   half's (`crates/vike-backfill/src/archive_import.rs`'s `ArchiveImport` and `DayOwners`, over
//!   `crates/vike-backfill/src/venues/dukascopy/archive.rs`'s `DukascopyLane`) —
//!   which takes DECODED quotes through a closure, because `vike-backfill` names no bridge
//!   (`docs/decisions/0094-backfill-names-no-venue.md`). This file is where the format crate's decode
//!   meets the store half's import: inside the lock, only for a day the store half calls the
//!   closure for, so a HELD day's file is never opened.
//!
//! What this file adds is the mapping between the store half's vocabulary and the wire's — and the
//! one rule neither half owns: `verify`'s cross-check of the decoded scale against the store.
//!
//! **`--replace-holed`** (decision 0268, point 4): a day the HTTP lane's keys cover only part of is
//! replaced whole, ticks and each requested interval's bars, by the file's day; a day covered whole
//! never is. The rule is the store half's (`holed_class`, over `DukascopyLane`'s `day_fill`), asked the
//! same way by a plan and an import (`ImportStep`), as for Binance's klines.

use std::path::PathBuf;
use std::sync::Arc;

use vike_backfill::archive_import::{
    ArchiveDayClass, ArchiveDayResult, ArchiveImport, ArchiveLane, DayOwners, holed_class,
};
use vike_backfill::venues::dukascopy::VENUE;
use vike_backfill::venues::dukascopy::archive::DukascopyLane;
use vike_backfill::venues::dukascopy::day_owner::quote_series_facts;
use vike_bi5::{
    Bi5Day, Bi5Limits, Bi5Path, Bi5PriceScale, Bi5Refusal, NO_INSTRUMENT_LIST, Tick,
    classify_bi5_path, decode_daily_file, is_bi5_layout_dir, read_bi5_header, tick_to_quote,
    unscaled_instrument_refusal,
};
use vike_data::archive_day::{ArchiveDayRefusal, DUKASCOPY_BI5_FORMAT};
use vike_data::{HistoryStore, SeriesCoverage, TimestampRange};
use vike_datahub_client::archive::{ArchiveDayClassWire, ArchiveDayResultWire, ImportSpec};
use vike_model::MS_PER_DAY;
use vike_store::HistoryStoreDataFusion;

use binance_options::BinanceOptionArchive;
use vike_backfill::venues::binance_archive::options::summary::OptionSummaryLane;
use vike_backfill::venues::binance_archive::options::volatility_index::VolatilityIndexLane;

use super::{ArchiveFormat, ArchiveFormatLayoutFile, ImportSession};

pub mod binance_aggtrades;
pub mod binance_klines;
pub mod binance_options;
pub mod csv_bars;
pub mod sec_edgar;
pub mod text_scores;

/// **Every archive format this daemon can import**, each over `store` — the SAME handle the server
/// serves, so an imported day is visible to the very next read on any connection. `instruments` is the
/// folder of the kept instrument lists (`<project>/instruments`, `crate::instrument_lists`), where the
/// Dukascopy format reads each dataset's price scale; `None` (no project) admits no Dukascopy dataset.
pub fn real_import_registry(
    store: Arc<HistoryStoreDataFusion>,
    instruments: Option<PathBuf>,
) -> Vec<Box<dyn ArchiveFormat>> {
    let mut formats: Vec<Box<dyn ArchiveFormat>> = vec![
        Box::new(DukascopyBi5 { store: Arc::clone(&store), instruments }),
        Box::new(binance_aggtrades::BinanceAggTrades::new(Arc::clone(&store))),
        Box::new(binance_klines::BinanceKlines::new(Arc::clone(&store))),
        Box::new(BinanceOptionArchive::<OptionSummaryLane>::new(Arc::clone(&store))),
        Box::new(BinanceOptionArchive::<VolatilityIndexLane>::new(Arc::clone(&store))),
        Box::new(text_scores::TextScores::new(Arc::clone(&store))),
    ];
    formats.extend(sec_edgar::sec_formats(&store));
    formats.push(Box::new(csv_bars::ArchiveFormatCsvBars::new(store)));
    formats
}

/// The LZMA-alone header a `.bi5` file opens with — one properties byte, the dictionary size as a
/// `u32` and the unpacked size as a `u64` — which `vike_bi5::read_bi5_header` reads.
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

/// The refusal class a file that decodes to NO ticks gets. Its sentence is each format's own.
pub const EMPTY_PAYLOAD: &str = "EmptyPayload";

/// What an [`EMPTY_PAYLOAD`] refusal says of a Dukascopy file: the vendor's own reading of one.
const BI5_EMPTY: &str = "the file decodes to no ticks — per the vendor, nothing was recorded that \
                         day. Nothing was stored and no commit key was spent, so the day stays free \
                         for a re-published file.";

/// Dukascopy's daily `.bi5` files, landing under `venue=dukascopy`.
struct DukascopyBi5 {
    store: Arc<HistoryStoreDataFusion>,
    /// The kept instrument lists' folder, read for a dataset's price scale and store name ([`DukascopyBi5::row_of`]).
    instruments: Option<PathBuf>,
}

impl DukascopyBi5 {
    /// **The price scale of `dataset`** (the vendor's folder name, `EURUSD` or `AAPLUSUSD`) **and the STORE name its
    /// series is filed under** (decision 0290: the row's, `EURUSD` or `AAPL.CFD`). Both come from its ONE row in the kept
    /// dukascopy list (`vike_catalog::bi5_dataset_row`): the point value its price step gives, and
    /// `vike_catalog::names_of_row`'s store name. No kept list at all is the one line that says what to do
    /// (`vike_bi5::NO_INSTRUMENT_LIST`); a list without the dataset's row is the refusal quoting the vendor's warning; a
    /// row the grammar cannot name is the translator's refusal. A row that states no step (zero) is never a scale
    /// (`Bi5PriceScale::from_tick_size` refuses it): beside one, the ONE row of the dataset that states a step answers
    /// (an operator row spelled otherwise beside a step-less venue row, DUK-6), which is `vike_catalog::bi5_dataset_row`'s
    /// own rule. The list is read at each ask, so a refreshed list admits a dataset without a restart. The
    /// bridge's HTTP lane scales by this same rule over the same row form
    /// (`crates/bridges/dukascopy/src/instrument/grid.rs`'s `history_point_value`).
    fn row_of(&self, dataset: &str) -> Result<(Bi5PriceScale, String), String> {
        let Some(list) = self
            .instruments
            .as_deref()
            .and_then(|dir| vike_catalog::read_kept_list(dir, VENUE))
            .filter(|list| !list.instruments.is_empty())
        else {
            return Err(format!("{dataset}: {NO_INSTRUMENT_LIST}"));
        };
        let Some((row, scale)) = vike_catalog::bi5_dataset_row(&list.instruments, dataset)
            .and_then(|row| {
                Some((row, Bi5PriceScale::from_tick_size(dataset, row.properties.tick_size)?))
            })
        else {
            return Err(unscaled_instrument_refusal(dataset));
        };
        Ok((
            scale,
            vike_paths::store_path::store_name_of_user_name(&vike_catalog::user_name_of_row(row)?),
        ))
    }
}

impl ArchiveFormat for DukascopyBi5 {
    fn id(&self) -> &'static str {
        DUKASCOPY_BI5_FORMAT
    }

    fn venue(&self) -> &'static str {
        VENUE
    }

    /// A dataset is the vendor's instrument folder, and it is admitted ONLY with a price scale its
    /// kept list row states (design §3.4): an instrument with no row is refused before a file is
    /// opened, quoting the vendor's own warning about silently wrong prices.
    fn admit(&self, dataset: &str) -> Result<String, String> {
        let (scale, _) = self.row_of(dataset)?;
        Ok(format!("point value {} — {}", scale.point_value(), scale.evidence()))
    }

    fn accepts_dir(&self, rel: &[&str], now_ms: i64) -> bool {
        is_bi5_layout_dir(rel, now_ms)
    }

    fn classify_file(&self, rel: &[&str], now_ms: i64) -> ArchiveFormatLayoutFile {
        match classify_bi5_path(rel, now_ms) {
            Bi5Path::Daily(day) => ArchiveFormatLayoutFile::Daily(day.start_ms()),
            Bi5Path::Hourly { day, .. } => ArchiveFormatLayoutFile::OtherLayout(day.start_ms()),
            Bi5Path::Other => ArchiveFormatLayoutFile::Other,
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
    fn read_header(&self, file_len: u64, prefix: &[u8]) -> Result<Option<u64>, ArchiveDayRefusal> {
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
        spec: &ImportSpec,
        now_ms: i64,
    ) -> Result<Box<dyn ImportSession + 'a>, String> {
        // The files are the dataset's (the vendor's folder); the series is the row's STORE name (decision 0290).
        let (scale, symbol) = self.row_of(&spec.dataset)?;
        let bars = spec.bars.as_slice();
        let hist: &'a HistoryStoreDataFusion = &self.store;
        let store = if spec.writes() {
            let import =
                ArchiveImport::begin(hist, &symbol, bars, now_ms).map_err(|e| e.to_string())?;
            ImportStep::Import(if spec.replace_holed { import.replacing_holed() } else { import })
        } else {
            let (coverage, keys) = quote_series_facts(hist, &symbol).map_err(|e| e.to_string())?;
            let owners = DayOwners::parse(&symbol, &keys);
            let replace = spec.replace_holed.then(|| (DukascopyLane::new(&symbol), hist));
            ImportStep::Plan { owners, coverage, now_ms, replace }
        };
        Ok(Box::new(Bi5Session { hist, symbol, scale, store, cross_checked: false }))
    }

    fn replaces_holed_days(&self) -> bool {
        true
    }
}

/// The store side of one request of every format: lock-free for a dry run, the locked import session
/// otherwise. `replace` is the lane and the store a dry run asks how full a day is, when the request
/// asked to replace a holed day (decision 0268, point 4); the import session carries the option itself
/// ([`ArchiveImport::replacing_holed`]).
enum ImportStep<'h, L> {
    Plan {
        owners: DayOwners,
        coverage: SeriesCoverage,
        now_ms: i64,
        replace: Option<(L, &'h HistoryStoreDataFusion)>,
    },
    Import(ArchiveImport<'h, L>),
}

impl<L: ArchiveLane> ImportStep<'_, L> {
    /// The series as the store holds it.
    fn coverage(&self) -> &SeriesCoverage {
        match self {
            Self::Plan { coverage, .. } => coverage,
            Self::Import(import) => import.coverage(),
        }
    }

    /// What the store half makes of `day`, on the wire. A plan and an import classify alike, the
    /// replace rule ([`holed_class`]) included, so a dry run lists exactly the days an import would
    /// replace; a store that cannot say how full a day is refuses that day.
    fn classify(&self, day: i64) -> ArchiveDayClassWire {
        let class = match self {
            Self::Plan { owners, now_ms, replace, .. } => {
                let class = owners.classify(day, *now_ms);
                match replace {
                    Some((lane, hist)) => holed_class(lane, hist, owners, day, class),
                    None => Ok(class),
                }
            }
            Self::Import(import) => import.classify_for_import(day),
        };
        match class {
            Ok(class) => wire_class(class),
            Err(e) => ArchiveDayClassWire::Refused(ArchiveDayRefusal {
                class: "StoreUnreadable".to_string(),
                detail: format!(
                    "the store could not say how full this day is ({e}). Nothing was read and no \
                     commit key was spent."
                ),
            }),
        }
    }
}

struct Bi5Session<'h> {
    hist: &'h HistoryStoreDataFusion,
    symbol: String,
    scale: Bi5PriceScale,
    store: ImportStep<'h, DukascopyLane>,
    /// Set once `verify` has run its scale cross-check — on the request's first decoded day.
    cross_checked: bool,
}

impl ImportSession for Bi5Session<'_> {
    fn series(&self) -> Option<SeriesCoverage> {
        let coverage = self.store.coverage();
        (coverage.rows > 0).then(|| coverage.clone())
    }

    fn classify(&self, day: i64) -> ArchiveDayClassWire {
        self.store.classify(day)
    }

    /// The store half's `import_day`, with a `load` that reads the day's file through the lane's
    /// safe open and decodes it with `vike-bi5` — called by the store half ONLY for a FREE,
    /// SUPERSEDE or REPLACE day, inside the lock.
    fn import_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResultWire, String> {
        let ImportStep::Import(import) = &mut self.store else {
            return Err("a dry run's session cannot import — this is a lane defect".to_string());
        };
        let Some(bi5_day) = Bi5Day::from_start_ms(day) else {
            return Err(format!("{day} is not the start of a UTC day at or after 1970"));
        };
        let (scale, symbol) = (&self.scale, self.symbol.as_str());
        let result = import
            .import_day(day, || {
                let blob = read()?;
                let ticks = decode_daily_file(&blob, bi5_day, scale, &Bi5Limits::DEFAULT)
                    .map_err(|refusal| decode_refusal(&refusal))?;
                drop(blob);
                Ok(ticks.iter().map(|tick| tick_to_quote(tick, symbol)).collect())
            })
            .map_err(|e| e.to_string())?;
        Ok(wire_result(result, BI5_EMPTY))
    }

    /// Decode the day and check it — `vike-bi5`'s refusals, the empty payload, and on the request's
    /// FIRST decoded day the scale cross-check — writing nothing.
    fn verify_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResultWire, String> {
        let Some(bi5_day) = Bi5Day::from_start_ms(day) else {
            return Err(format!("{day} is not the start of a UTC day at or after 1970"));
        };
        let blob = match read() {
            Ok(blob) => blob,
            Err(refusal) => return Ok(ArchiveDayResultWire::Refused(refusal)),
        };
        let ticks = match decode_daily_file(&blob, bi5_day, &self.scale, &Bi5Limits::DEFAULT) {
            Ok(ticks) => ticks,
            Err(refusal) => return Ok(ArchiveDayResultWire::Refused(decode_refusal(&refusal))),
        };
        drop(blob);
        if ticks.is_empty() {
            return Ok(ArchiveDayResultWire::Refused(empty_payload(BI5_EMPTY)));
        }
        if !self.cross_checked {
            self.cross_checked = true;
            if let Some(refusal) = self.scale_cross_check(day, &ticks)? {
                return Ok(ArchiveDayResultWire::Refused(refusal));
            }
        }
        Ok(ArchiveDayResultWire::Verified { ticks: ticks.len() as u64, report: None })
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
    /// neighbour was scaled by this same list's row, so comparing against one can only agree, which
    /// is harmless rather than wrong.
    fn scale_cross_check(
        &self,
        day: i64,
        ticks: &[Tick],
    ) -> Result<Option<ArchiveDayRefusal>, String> {
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
        Ok(Some(ArchiveDayRefusal {
            class: SCALE_MISMATCH.to_string(),
            detail: format!(
                "the file's median mid ({file_mid}) is {ratio:.4} times the median mid of the ticks \
                 this store already holds beside it ({stored_mid}) — outside [{SCALE_RATIO_MIN}, \
                 {SCALE_RATIO_MAX}], which is what a wrong price scale (a power of ten) looks like. \
                 Point value {} is this instrument's kept list row; do not import until the two agree. \
                 Nothing was written.",
                self.scale.point_value()
            ),
        }))
    }

    /// The median mid of the stored ticks nearest `day`: the [`NEIGHBOUR_DAYS`] before it, else the
    /// [`NEIGHBOUR_DAYS`] after it, else `None`.
    fn neighbour_median(&self, day: i64) -> Result<Option<f64>, String> {
        let before = TimestampRange::of(day.saturating_sub(NEIGHBOUR_DAYS * MS_PER_DAY), day - 1);
        let after_start = day + MS_PER_DAY;
        let after = TimestampRange::of(after_start, after_start + NEIGHBOUR_DAYS * MS_PER_DAY - 1);
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

/// The HIGH median of the finite, positive values — `None` when there is none.
fn median(values: impl Iterator<Item = f64>) -> Option<f64> {
    let mut v: Vec<f64> = values.filter(|x| x.is_finite() && *x > 0.0).collect();
    v.sort_by(f64::total_cmp);
    vike_math::statistics::high_median_of_sorted(&v)
}

/// A `vike-bi5` refusal as the wire carries it: the class token and its own sentence, which
/// names sizes, caps and a record index — never payload bytes.
fn decode_refusal(refusal: &Bi5Refusal) -> ArchiveDayRefusal {
    ArchiveDayRefusal {
        class: refusal.class().to_string(),
        detail: format!("{refusal}. Nothing was stored and no commit key was spent."),
    }
}

/// The store half reports an empty payload as its own result; the wire has no such variant, and a
/// day that stored nothing is not "imported" — it is refused, with no key spent. `detail` is the
/// format's own sentence, because what an empty file means is the vendor's.
fn empty_payload(detail: &str) -> ArchiveDayRefusal {
    ArchiveDayRefusal { class: EMPTY_PAYLOAD.to_string(), detail: detail.to_string() }
}

/// The store half's day table onto the wire's — one variant for one, in the same order.
fn wire_class(class: ArchiveDayClass) -> ArchiveDayClassWire {
    match class {
        ArchiveDayClass::TooRecent => ArchiveDayClassWire::TooRecent,
        ArchiveDayClass::HeldByArchive => ArchiveDayClassWire::HeldByArchive,
        ArchiveDayClass::HeldByHttp => ArchiveDayClassWire::HeldByHttp,
        ArchiveDayClass::Supersede { key } => ArchiveDayClassWire::Supersede { key },
        ArchiveDayClass::Overlapped { keys } => ArchiveDayClassWire::Overlapped { keys },
        ArchiveDayClass::Free => ArchiveDayClassWire::Free,
        ArchiveDayClass::Replace { keys, stored, full, .. } => {
            ArchiveDayClassWire::Replace { keys, stored: stored as u64, full: full as u64 }
        }
    }
}

/// The store half's day result onto the wire's; `empty` is the format's [`empty_payload`] sentence.
fn wire_result(result: ArchiveDayResult, empty: &str) -> ArchiveDayResultWire {
    match result {
        ArchiveDayResult::Imported { ticks, bars } => {
            ArchiveDayResultWire::Imported { ticks: ticks as u64, bars, report: None }
        }
        ArchiveDayResult::Empty => ArchiveDayResultWire::Refused(empty_payload(empty)),
        ArchiveDayResult::ToppedUp { bars } => ArchiveDayResultWire::ToppedUp { bars },
        // An imported day to every client: the sentence says what it replaced.
        ArchiveDayResult::Replaced { rows, stored, bars } => ArchiveDayResultWire::Imported {
            ticks: rows as u64,
            bars,
            report: Some(format!(
                "replaced the day whole: the {stored} rows the store held for it were removed and \
                 the file's {rows} stored in their place, under this format's day key, which owns \
                 the day from now on"
            )),
        },
        ArchiveDayResult::Refused(refusal) => ArchiveDayResultWire::Refused(refusal),
    }
}
