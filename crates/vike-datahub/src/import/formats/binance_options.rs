//! **Binance's option-market daily zips: `binance-eohsummary` (the hourly chain summary, into
//! `kind=chain`) and `binance-bvolindex` (the implied-volatility index, into `kind=trade`).** One
//! format over each lane of `vike_backfill::venues::binance_archive::options`, whose module doc
//! carries the row rules, what lands where and what the vendor publishes; this file maps a lane onto
//! the import's seam and the wire. The operator's page is `docs/ops/binance-archive-import.md`.
//!
//! - **A dataset is the vendor's folder name** (`BTCUSDT` for the summary, `BTCBVOLUSDT` for the
//!   index), and its daily zips sit flat in it with their `.CHECKSUM`s, as the vendor names them:
//!   `binance-eohsummary/BTCUSDT/BTCUSDT-EOHSummary-2023-10-23.zip`.
//! - **Every zip is checked against its `.CHECKSUM`** before a byte of it is decoded, the klines
//!   format's check (`check_vendor_checksum`); a zip without one is refused in the plan.
//! - **No bars**: a request naming `--bars` is refused before any directory is touched.
//! - **Daily zips only**: any other name in the folder is an other object, never read.

use std::marker::PhantomData;
use std::sync::Arc;

use vike_backfill::archive_import::{ArchiveImport, ArchiveLane};
use vike_backfill::venues::binance_archive::decode::{AggLimits, LOCAL_HEADER_BYTES};
use vike_backfill::venues::binance_archive::options::OptionArchiveLane;
use vike_backfill::venues::binance_archive::{VENUE, dataset_zip_day};
use vike_data::SeriesCoverage;
use vike_data::archive_day::ArchiveDayRefusal;
use vike_datahub_client::archive::{ArchiveDayClassWire, ArchiveDayResultWire, ImportSpec};
use vike_store::HistoryStoreDataFusion;

use super::binance_klines::{check_vendor_checksum, read_zip_header};
use super::{ImportStep, empty_payload, wire_result};
use crate::import::{ArchiveFormat, ArchiveFormatLayoutFile, ImportSession};

/// What an `EmptyPayload` refusal says of a zip with no row.
const EMPTY_FILE: &str = "the file holds no row. Nothing was stored and no commit key was spent, \
                          so the day stays free: download the file again and check it with its \
                          .CHECKSUM.";

/// One of Binance's option-market datasets, over the lane `L`, landing under `venue=binance`.
pub(super) struct BinanceOptionArchive<L> {
    store: Arc<HistoryStoreDataFusion>,
    lane: PhantomData<fn() -> L>,
}

impl<L> BinanceOptionArchive<L> {
    pub(super) fn new(store: Arc<HistoryStoreDataFusion>) -> Self {
        Self { store, lane: PhantomData }
    }
}

impl<L: OptionArchiveLane + 'static> ArchiveFormat for BinanceOptionArchive<L> {
    fn id(&self) -> &'static str {
        L::SOURCE
    }

    fn venue(&self) -> &'static str {
        VENUE
    }

    fn admit(&self, dataset: &str) -> Result<String, String> {
        L::for_dataset(dataset).map(|lane| lane.admission())
    }

    /// The files sit in the dataset's own folder; a request naming bars is refused here, before any
    /// directory is touched.
    fn request_dir(&self, _dataset: &str, bars: &[String]) -> Result<Option<String>, String> {
        if bars.is_empty() {
            return Ok(None);
        }
        Err(format!(
            "{} derives no bars: import it with no --bars (or `--bars none`), and resample in the \
             study.",
            L::SOURCE
        ))
    }

    /// The vendor's daily files sit flat in the dataset's folder.
    fn accepts_dir(&self, _rel: &[&str], _now_ms: i64) -> bool {
        false
    }

    fn classify_file(&self, rel: &[&str], _now_ms: i64) -> ArchiveFormatLayoutFile {
        match rel {
            [name] => dataset_zip_day(name, L::FILE_WORD)
                .map_or(ArchiveFormatLayoutFile::Other, ArchiveFormatLayoutFile::Daily),
            _ => ArchiveFormatLayoutFile::Other,
        }
    }

    fn companion(&self, file_name: &str) -> Option<String> {
        Some(format!("{file_name}.CHECKSUM"))
    }

    fn check_companion(
        &self,
        file_name: &str,
        file: &[u8],
        companion: &[u8],
    ) -> Result<(), ArchiveDayRefusal> {
        check_vendor_checksum(file_name, file, companion)
    }

    fn max_file_bytes(&self) -> u64 {
        AggLimits::DEFAULT.max_file_bytes
    }

    fn header_len(&self) -> usize {
        LOCAL_HEADER_BYTES
    }

    fn read_header(&self, file_len: u64, prefix: &[u8]) -> Result<Option<u64>, ArchiveDayRefusal> {
        read_zip_header(self.max_file_bytes(), file_len, prefix)
    }

    fn session<'a>(
        &'a self,
        spec: &ImportSpec,
        now_ms: i64,
    ) -> Result<Box<dyn ImportSession + 'a>, String> {
        let lane = L::for_dataset(&spec.dataset)?;
        let hist: &'a HistoryStoreDataFusion = &self.store;
        let store = if spec.writes() {
            ImportStep::Import(
                ArchiveImport::open(hist, lane.clone(), &[], now_ms).map_err(|e| e.to_string())?,
            )
        } else {
            let (coverage, keys) = lane.series_facts(hist).map_err(|e| e.to_string())?;
            ImportStep::Plan { owners: lane.owners(&keys), coverage, now_ms, replace: None }
        };
        Ok(Box::new(OptionSession { lane, store }))
    }
}

struct OptionSession<'h, L: ArchiveLane> {
    /// The decode's copy of the lane: the import holds its own.
    lane: L,
    store: ImportStep<'h, L>,
}

impl<L: OptionArchiveLane> ImportSession for OptionSession<'_, L> {
    fn series(&self) -> Option<SeriesCoverage> {
        let coverage = self.store.coverage();
        (coverage.rows > 0).then(|| coverage.clone())
    }

    fn classify(&self, day: i64) -> ArchiveDayClassWire {
        self.store.classify(day)
    }

    fn import_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResultWire, String> {
        let Self { lane, store } = self;
        let ImportStep::Import(import) = store else {
            return Err("a dry run's session cannot import — this is a lane defect".to_string());
        };
        let result = import
            .import_day(day, || {
                let blob = read()?;
                lane.decode(&blob, day)
            })
            .map_err(|e| e.to_string())?;
        Ok(wire_result(result, EMPTY_FILE))
    }

    /// Decode the day and check it, writing nothing: the checksum has been checked by then.
    fn verify_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResultWire, String> {
        let rows = match read().and_then(|blob| self.lane.decode(&blob, day)) {
            Ok(rows) => rows,
            Err(refusal) => return Ok(ArchiveDayResultWire::Refused(refusal)),
        };
        if rows.is_empty() {
            return Ok(ArchiveDayResultWire::Refused(empty_payload(EMPTY_FILE)));
        }
        Ok(ArchiveDayResultWire::Verified { ticks: rows.len() as u64, report: None })
    }
}

#[cfg(test)]
#[path = "binance_options_tests.rs"]
mod binance_options_tests;
