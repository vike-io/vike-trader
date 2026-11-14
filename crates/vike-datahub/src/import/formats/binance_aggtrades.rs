//! **Binance's daily aggTrades zips (`binance-aggtrades`).** The grammar, the zip reader, the day key
//! and the one-owner rule are `crates/vike-backfill/src/venues/binance_archive.rs`'s; this file maps
//! them onto the lane's seam and the wire. The operator's page is `docs/ops/binance-archive-import.md`.
//!
//! - **A dataset is the vendor's symbol folder** (`SOLUSDT`) and the rows land under the store's
//!   spelling of that perpetual, `venue=binance`, `symbol=SOLUSDT.P`.
//! - **Bars only when asked, and never among the candles.** The CLI asks for none unless told, and any
//!   this format derives land in the `source=aggtrades` lane. The plan's admission line says so;
//!   `crates/vike-backfill/src/venues/binance_archive.rs`'s module doc carries the measurement.
//! - **Daily zips only.** The lane is built on one file per UTC day. A monthly zip's name is not the
//!   grammar's, so it counts as an other object and is never read, and the plan's admission line says
//!   so.
//! - **What a verify reports.** A day whose aggregated ids or times do not run on is stored as published,
//!   because a gap is a fact about the vendor's file, and no remedy would ever free such a day if it
//!   were refused. A day holding exact repeats of its rows has them dropped and counted. So `verify`
//!   answers either day `ArchiveDayResultWire::Verified` with the sentence in its `report` (`DecodedDay::report`),
//!   and an import stores the day, logs the same sentence and answers it in `ArchiveDayResultWire::Imported`'s
//!   `report`. ⚠ A skipped trade id alone goes to the log
//!   and no further: `crates/vike-backfill/src/venues/binance_archive/decode.rs`'s `is_broken` says why.
//!   A repeated id whose row differs refuses the day (`ContradictoryRepeat`), in a verify and an import.
//! - **Its own sentence for an empty file** (`EMPTY_FILE`), never Dukascopy's.

use std::sync::{Arc, Mutex, PoisonError};

use vike_backfill::archive_import::{ArchiveImport, ArchiveLane};
use vike_backfill::venues::binance_archive::decode::{
    AggEdge, AggLimits, DayContinuity, DecodedDay, LOCAL_HEADER_BYTES, check_local_header,
    decode_daily_zip,
};
use vike_backfill::venues::binance_archive::{
    BinanceArchiveLane, SOURCE, VENUE, daily_member_name, daily_zip_day, trade_series_facts,
};
use vike_data::SeriesCoverage;
use vike_data::archive_day::ArchiveDayRefusal;
use vike_data::store::store_kind::AGGTRADES_LANE;
use vike_datahub_client::archive::{ArchiveDayClassWire, ArchiveDayResultWire, ImportSpec};
use vike_model::MS_PER_DAY;
use vike_store::HistoryStoreDataFusion;
use vike_venues::PERP_SUFFIX;

use super::{ImportStep, empty_payload, wire_class, wire_result};
use crate::import::walk::FILE_TOO_LARGE;
use crate::import::{ArchiveFormat, ArchiveFormatLayoutFile, ImportSession};

/// What an `EmptyPayload` refusal says of a zip with no row. Nothing here knows why Binance would
/// publish one, so it says what was found and what to do.
const EMPTY_FILE: &str = "the file holds no row. Nothing was stored and no commit key was spent, \
                          so the day stays free: download the file again and check it with its \
                          .CHECKSUM.";

/// The last row of the file read before this one: what the next file's first row must follow.
#[derive(Debug, Clone)]
struct PreviousFile {
    dataset: String,
    day: i64,
    last: AggEdge,
}

/// Binance's daily aggTrades zips, landing under `venue=binance`.
pub(super) struct BinanceAggTrades {
    store: Arc<HistoryStoreDataFusion>,
    /// The last file any request read. The client sends one request per calendar month, and this is
    /// what lets the first day of a month be checked against the last day of the month before.
    previous: Mutex<Option<PreviousFile>>,
}

impl BinanceAggTrades {
    pub(super) fn new(store: Arc<HistoryStoreDataFusion>) -> Self {
        Self { store, previous: Mutex::new(None) }
    }
}

impl ArchiveFormat for BinanceAggTrades {
    fn id(&self) -> &'static str {
        SOURCE
    }

    fn venue(&self) -> &'static str {
        VENUE
    }

    fn admit(&self, dataset: &str) -> Result<String, String> {
        if !dataset.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) {
            return Err("this format reads a USDⓈ-M perpetual's folder, named as the vendor names \
                        the symbol: letters and digits only, such as SOLUSDT. A dated future's name \
                        or the store's own `.P` spelling is not one."
                .to_string());
        }
        Ok(format!(
            "decimal prices as published, no scale to apply · stored as {VENUE} \
             {dataset}{PERP_SUFFIX} · --bars writes to the source={AGGTRADES_LANE} lane and never the \
             exchange's candles, because an aggregated trade carries its first fill's time, so these \
             bars differ from them · daily zips only: a monthly {dataset}-aggTrades-YYYY-MM.zip \
             counts as an other object and is never read · a row repeating an earlier row of its \
             file exactly is dropped, and --verify says how many a day held; a repeated \
             agg_trade_id with a different row refuses the day"
        ))
    }

    /// The vendor's daily files sit flat in one folder.
    fn accepts_dir(&self, _rel: &[&str], _now_ms: i64) -> bool {
        false
    }

    fn classify_file(&self, rel: &[&str], _now_ms: i64) -> ArchiveFormatLayoutFile {
        match rel {
            [name] => daily_zip_day(name)
                .map_or(ArchiveFormatLayoutFile::Other, ArchiveFormatLayoutFile::Daily),
            _ => ArchiveFormatLayoutFile::Other,
        }
    }

    fn max_file_bytes(&self) -> u64 {
        AggLimits::DEFAULT.max_file_bytes
    }

    fn header_len(&self) -> usize {
        LOCAL_HEADER_BYTES
    }

    /// The file's size, then that it opens as one deflated member. A zip declares no row count.
    fn read_header(&self, file_len: u64, prefix: &[u8]) -> Result<Option<u64>, ArchiveDayRefusal> {
        let cap = AggLimits::DEFAULT.max_file_bytes;
        if file_len > cap {
            return Err(ArchiveDayRefusal {
                class: FILE_TOO_LARGE.to_string(),
                detail: format!(
                    "the file is {file_len} bytes, over this format's {cap}-byte cap. Nothing was \
                     decoded or stored and no commit key was spent."
                ),
            });
        }
        check_local_header(prefix).map(|()| None)
    }

    fn session<'a>(
        &'a self,
        spec: &ImportSpec,
        now_ms: i64,
    ) -> Result<Box<dyn ImportSession + 'a>, String> {
        let dataset = spec.dataset.as_str();
        let symbol = format!("{dataset}{PERP_SUFFIX}");
        let hist: &'a HistoryStoreDataFusion = &self.store;
        let lane = BinanceArchiveLane::new(&symbol);
        let store = if spec.writes() {
            ImportStep::Import(
                ArchiveImport::open(hist, lane, &spec.bars, now_ms).map_err(|e| e.to_string())?,
            )
        } else {
            let (coverage, keys) = trade_series_facts(hist, &symbol).map_err(|e| e.to_string())?;
            ImportStep::Plan { owners: lane.owners(&keys), coverage, now_ms }
        };
        Ok(Box::new(AggSession {
            dataset: dataset.to_string(),
            store,
            memo: &self.previous,
            read_in_this_request: false,
        }))
    }
}

struct AggSession<'h> {
    dataset: String,
    store: ImportStep<'h, BinanceArchiveLane>,
    memo: &'h Mutex<Option<PreviousFile>>,
    read_in_this_request: bool,
}

/// Check `continuity`'s first row against the last file read, then remember this file.
///
/// Inside one request any earlier file counts, so a missing day shows as a gap. Across requests only
/// the day before does: a different window read earlier says nothing about the ids between the two.
fn note_previous_file(
    memo: &Mutex<Option<PreviousFile>>,
    read_in_this_request: &mut bool,
    dataset: &str,
    day: i64,
    continuity: &mut DayContinuity,
) {
    let mut memo = memo.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(previous) = memo.as_ref()
        && previous.dataset == dataset
        && previous.day < day
        && (*read_in_this_request || previous.day + MS_PER_DAY == day)
    {
        continuity.note_previous_file(previous.last);
    }
    if let Some(last) = continuity.last {
        *memo = Some(PreviousFile { dataset: dataset.to_string(), day, last });
        *read_in_this_request = true;
    }
}

impl ImportSession for AggSession<'_> {
    fn series(&self) -> Option<SeriesCoverage> {
        let coverage = self.store.coverage();
        (coverage.rows > 0).then(|| coverage.clone())
    }

    fn classify(&self, day: i64) -> ArchiveDayClassWire {
        wire_class(self.store.classify(day))
    }

    fn import_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResultWire, String> {
        let Self { dataset, store, memo, read_in_this_request } = self;
        let ImportStep::Import(import) = store else {
            return Err("a dry run's session cannot import — this is a lane defect".to_string());
        };
        let member = daily_member_name(dataset, day);
        let mut report = None;
        let result = import
            .import_day(day, || {
                let blob = read()?;
                let mut decoded = decode_daily_zip(&blob, &member, day, &AggLimits::DEFAULT)?;
                drop(blob);
                let continuity = &mut decoded.continuity;
                note_previous_file(memo, read_in_this_request, dataset, day, continuity);
                log_day(&member, &decoded);
                report = decoded.report();
                Ok(decoded.trades)
            })
            .map_err(|e| e.to_string())?;
        let mut result = wire_result(result, EMPTY_FILE);
        if let ArchiveDayResultWire::Imported { report: slot, .. } = &mut result {
            *slot = report;
        }
        Ok(result)
    }

    /// Decode the day and check it, writing nothing. See the module doc for what a report is.
    fn verify_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResultWire, String> {
        let blob = match read() {
            Ok(blob) => blob,
            Err(refusal) => return Ok(ArchiveDayResultWire::Refused(refusal)),
        };
        let member = daily_member_name(&self.dataset, day);
        let mut decoded = match decode_daily_zip(&blob, &member, day, &AggLimits::DEFAULT) {
            Ok(decoded) => decoded,
            Err(refusal) => return Ok(ArchiveDayResultWire::Refused(refusal)),
        };
        drop(blob);
        if decoded.trades.is_empty() {
            return Ok(ArchiveDayResultWire::Refused(empty_payload(EMPTY_FILE)));
        }
        note_previous_file(
            self.memo,
            &mut self.read_in_this_request,
            &self.dataset,
            day,
            &mut decoded.continuity,
        );
        log_day(&member, &decoded);
        Ok(ArchiveDayResultWire::Verified {
            ticks: decoded.trades.len() as u64,
            report: decoded.report(),
        })
    }
}

/// One line in the datahub's log for whatever the decode found: a warning with the verify's sentence
/// when an operator must read it, and otherwise the skipped trade ids a real file normally holds.
fn log_day(member: &str, decoded: &DecodedDay) {
    if let Some(report) = decoded.report() {
        tracing::warn!(%member, "{SOURCE}: {report}");
    } else if let Some(report) = decoded.continuity.report() {
        tracing::info!(%member, "{SOURCE}: {report}");
    }
}
