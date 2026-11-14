//! **Binance's klines zips (`binance-klines`), the venue's own candles.** The file names, the row
//! grammar, the day key and the one-owner rule are the klines lane's
//! (`crates/vike-backfill/src/venues/binance_archive/klines.rs`'s `BinanceKlinesLane`); this file
//! maps them onto the lane's seam and adds the one rule that makes them the venue's
//! series: every zip is checked against its `.CHECKSUM` before a byte of it is decoded (decision
//! 0268, point 2). The operator's page is `docs/ops/binance-archive-import.md`.
//!
//! - **A dataset is the store's spelling of the instrument**, and that is how spot and the perpetual
//!   are told apart: `BTCUSDT` is the spot pair and reads the vendor's `data/spot/` files,
//!   `BTCUSDT.P` the USDⓈ-M perpetual and reads its `data/futures/um/` files. Both land under
//!   `venue=binance` as named, in the SOURCELESS series, beside the REST backfill's candles.
//! - **One interval per request, in its own folder**: `<dataset>/<interval>/`, the vendor's
//!   `<SYMBOL>/<interval>/` folder, holding the daily zips and the monthly ones together with their
//!   `.CHECKSUM`s. The request's `bars` names the interval ([`ArchiveFormat::request_dir`]), and these
//!   bars are the rows themselves: nothing is derived.
//! - **Daily and monthly zips.** A monthly zip is listed under each day of its month no daily zip
//!   covers, and each day reads only its own rows from it.
//! - **A zip with no `.CHECKSUM`, a `.CHECKSUM` naming another file, or a digest that disagrees** is
//!   refused, and nothing of it is decoded.
//! - **`--replace-holed`**: a day another lane holds whose stored bars are fewer than a whole day's is
//!   replaced whole by the verified file's (decision 0268, point 4); a whole day never is.

use std::sync::Arc;

use vike_backfill::archive_import::{ArchiveImport, ArchiveLane, DayOwners, holed_class};
use vike_backfill::venues::binance_archive::VENUE;
use vike_backfill::venues::binance_archive::decode::{
    AggLimits, LOCAL_HEADER_BYTES, check_local_header,
};
use vike_backfill::venues::binance_archive::klines::{
    BinanceKlinesLane, SOURCE, kline_series_facts, kline_zip_days,
};
use vike_data::SeriesCoverage;
use vike_data::archive_day::ArchiveDayRefusal;
use vike_datahub_client::archive::{ArchiveDayClassWire, ArchiveDayResultWire, ImportSpec};
use vike_store::HistoryStoreDataFusion;
use vike_venues::PERP_SUFFIX;

use super::{empty_payload, wire_class, wire_result};
use crate::import::walk::FILE_TOO_LARGE;
use crate::import::{ArchiveFormat, ArchiveFormatLayoutFile, ImportSession};

/// What an `EmptyPayload` refusal says of a zip with no row of the day.
const EMPTY_FILE: &str = "the file holds no candle of this day. Nothing was stored and no commit \
                          key was spent, so the day stays free: download the file again and check \
                          it with its .CHECKSUM.";

/// The refusal classes of the checksum check.
pub const CHECKSUM_UNREADABLE: &str = "ChecksumUnreadable";
/// See [`CHECKSUM_UNREADABLE`].
pub const CHECKSUM_FOREIGN: &str = "ChecksumForeign";
/// See [`CHECKSUM_UNREADABLE`].
pub const CHECKSUM_MISMATCH: &str = "ChecksumMismatch";

/// Binance's klines zips, landing under `venue=binance` in the venue's own series.
pub(super) struct BinanceKlines {
    store: Arc<HistoryStoreDataFusion>,
}

impl BinanceKlines {
    pub(super) fn new(store: Arc<HistoryStoreDataFusion>) -> Self {
        Self { store }
    }

    /// One request's store side; `replace_holed` arms decision 0268's point 4.
    fn open<'a>(
        &'a self,
        dataset: &str,
        bars: &[String],
        now_ms: i64,
        writes: bool,
        replace_holed: bool,
    ) -> Result<Box<dyn ImportSession + 'a>, String> {
        let interval = one_interval(bars)?;
        let vendor = dataset.strip_suffix(PERP_SUFFIX).unwrap_or(dataset);
        let lane = BinanceKlinesLane::new(dataset, vendor, interval)?;
        let hist: &'a HistoryStoreDataFusion = &self.store;
        let store = if writes {
            let import =
                ArchiveImport::open(hist, lane.clone(), &[], now_ms).map_err(|e| e.to_string())?;
            KlineStore::Import(if replace_holed { import.replacing_holed() } else { import })
        } else {
            let (coverage, keys) =
                kline_series_facts(hist, dataset, interval).map_err(|e| e.to_string())?;
            let owners = lane.owners(&keys);
            KlineStore::Plan { hist, owners, coverage, now_ms, replace_holed }
        };
        Ok(Box::new(KlineSession { lane, store }))
    }
}

/// The one interval a request reads, or the refusal that names how to give it.
fn one_interval(bars: &[String]) -> Result<&str, String> {
    match bars {
        [interval] => Ok(interval),
        _ => Err(format!(
            "{SOURCE} reads ONE interval, the vendor's folder under the dataset \
             (<dataset>/<interval>/): name it with --bars, such as `--bars 1m`. These bars are the \
             rows themselves, read as published, and nothing is derived from them."
        )),
    }
}

impl ArchiveFormat for BinanceKlines {
    fn id(&self) -> &'static str {
        SOURCE
    }

    fn venue(&self) -> &'static str {
        VENUE
    }

    fn admit(&self, dataset: &str) -> Result<String, String> {
        let pair = dataset.strip_suffix(PERP_SUFFIX).unwrap_or(dataset);
        if pair.is_empty() || !pair.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) {
            return Err(format!(
                "this format reads a spot pair or a USDⓈ-M perpetual, named as the store names it: \
                 the vendor's symbol for spot (BTCUSDT), or with `{PERP_SUFFIX}` for the perpetual \
                 (BTCUSDT{PERP_SUFFIX}). A dated future's or a COIN-M contract's name is not one."
            ));
        }
        let market = if pair == dataset {
            "the spot pair, from the vendor's data/spot/ files"
        } else {
            "the USDⓈ-M perpetual, from the vendor's data/futures/um/ files"
        };
        Ok(format!(
            "the venue's own candles as published, no scale to apply · stored as {VENUE} {dataset}, \
             {market}, in the series a chart, a backtest and `data hist fetch` read, one owner per \
             UTC day · every zip is checked against its .CHECKSUM (SHA-256) before it is read, and \
             one without is refused · daily and monthly zips, and a day that has both reads its \
             daily one · open times in milliseconds or microseconds (the vendor's spot files from \
             2025), decided per file · --replace-holed replaces a day another lane holds with fewer \
             bars than a whole day"
        ))
    }

    fn request_dir(&self, bars: &[String]) -> Result<Option<String>, String> {
        let interval = one_interval(bars)?;
        BinanceKlinesLane::new("X", "X", interval)?;
        Ok(Some(interval.to_string()))
    }

    /// The vendor's files sit flat in the interval folder.
    fn accepts_dir(&self, _rel: &[&str], _now_ms: i64) -> bool {
        false
    }

    fn classify_file(&self, rel: &[&str], _now_ms: i64) -> ArchiveFormatLayoutFile {
        match rel {
            [name] => match kline_zip_days(name) {
                Some((first, last)) if first == last => ArchiveFormatLayoutFile::Daily(first),
                Some((first, last)) => ArchiveFormatLayoutFile::Span { first, last },
                None => ArchiveFormatLayoutFile::Other,
            },
            _ => ArchiveFormatLayoutFile::Other,
        }
    }

    fn companion(&self, file_name: &str) -> Option<String> {
        Some(format!("{file_name}.CHECKSUM"))
    }

    /// The vendor's `.CHECKSUM` is `sha256sum`'s line: the hex digest, two spaces, the zip's name.
    fn check_companion(
        &self,
        file_name: &str,
        file: &[u8],
        companion: &[u8],
    ) -> Result<(), ArchiveDayRefusal> {
        let refusal = |class: &str, what: String| ArchiveDayRefusal {
            class: class.to_string(),
            detail: format!(
                "{what}. Nothing of the zip was decoded and no commit key was spent: download the \
                 zip and its .CHECKSUM again."
            ),
        };
        let line = std::str::from_utf8(companion).ok().map(str::trim).unwrap_or_default();
        let digest = line.split_once(char::is_whitespace).and_then(|(hex, name)| {
            let name = name.trim_start().trim_start_matches('*');
            let hex_ok = hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit());
            (hex_ok && !line.contains('\n')).then_some((hex.to_ascii_lowercase(), name))
        });
        let Some((want, named)) = digest else {
            return Err(refusal(
                CHECKSUM_UNREADABLE,
                format!("`{file_name}.CHECKSUM` is not one `<sha256>  <file>` line"),
            ));
        };
        if named != file_name {
            return Err(refusal(
                CHECKSUM_FOREIGN,
                format!("`{file_name}.CHECKSUM` names another file, not `{file_name}`"),
            ));
        }
        let got = vike_hub_protocol::hash::sha256_hex(file);
        if got != want {
            return Err(refusal(
                CHECKSUM_MISMATCH,
                format!(
                    "`{file_name}` hashes to {got} and its .CHECKSUM says {want}: a partial or \
                     damaged download"
                ),
            ));
        }
        Ok(())
    }

    fn max_file_bytes(&self) -> u64 {
        AggLimits::DEFAULT.max_file_bytes
    }

    fn header_len(&self) -> usize {
        LOCAL_HEADER_BYTES
    }

    /// The file's size, then that it opens as one deflated member. A zip declares no row count.
    fn read_header(&self, file_len: u64, prefix: &[u8]) -> Result<Option<u64>, ArchiveDayRefusal> {
        let cap = self.max_file_bytes();
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
        self.open(&spec.dataset, &spec.bars, now_ms, spec.writes(), spec.replace_holed)
    }

    fn replaces_holed_days(&self) -> bool {
        true
    }
}

/// The store side of one request: lock-free for a dry run, the locked import session otherwise.
enum KlineStore<'h> {
    Plan {
        hist: &'h HistoryStoreDataFusion,
        owners: DayOwners,
        coverage: SeriesCoverage,
        now_ms: i64,
        replace_holed: bool,
    },
    Import(ArchiveImport<'h, BinanceKlinesLane>),
}

struct KlineSession<'h> {
    /// The decode's copy of the lane: the import holds its own.
    lane: BinanceKlinesLane,
    store: KlineStore<'h>,
}

impl ImportSession for KlineSession<'_> {
    fn series(&self) -> Option<SeriesCoverage> {
        let coverage = match &self.store {
            KlineStore::Plan { coverage, .. } => coverage,
            KlineStore::Import(import) => import.coverage(),
        };
        (coverage.rows > 0).then(|| coverage.clone())
    }

    /// A plan and an import classify alike, the replace rule included, so a dry run lists exactly
    /// the days an import would replace.
    fn classify(&self, day: i64) -> ArchiveDayClassWire {
        let class = match &self.store {
            KlineStore::Plan { hist, owners, now_ms, replace_holed, .. } => {
                let class = owners.classify(day, *now_ms);
                if *replace_holed {
                    holed_class(&self.lane, hist, owners, day, class)
                } else {
                    Ok(class)
                }
            }
            KlineStore::Import(import) => import.classify_for_import(day),
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

    fn import_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResultWire, String> {
        let Self { lane, store } = self;
        let KlineStore::Import(import) = store else {
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
        let blob = match read() {
            Ok(blob) => blob,
            Err(refusal) => return Ok(ArchiveDayResultWire::Refused(refusal)),
        };
        Ok(match self.lane.decode(&blob, day) {
            Ok(bars) if bars.is_empty() => ArchiveDayResultWire::Refused(empty_payload(EMPTY_FILE)),
            Ok(bars) => ArchiveDayResultWire::Verified { ticks: bars.len() as u64, report: None },
            Err(refusal) => ArchiveDayResultWire::Refused(refusal),
        })
    }
}

#[path = "binance_klines_tests.rs"]
#[cfg(test)]
mod binance_klines_tests;
