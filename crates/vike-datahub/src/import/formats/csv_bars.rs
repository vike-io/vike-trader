//! **A user's CSV of bars (`csv-bars`)**, written ONLY into the lane `user-<label>`, never the venue's
//! series (decision 0268, point 3). The decode is the one CSV decoder (`vike_data::csv_bars`), and the
//! lane, its day key and its one-owner table are `crates/vike-backfill/src/user_bars.rs`'s
//! `ArchiveLaneUserBars`; this file maps them onto the lane's seam.
//!
//! - **The series is the operator's word**: the request's `target` names the venue, the symbol, the
//!   interval and the lane's label, and how the file is written when it is a regional export (the
//!   field delimiter, the decimal mark, the thousands separator, a text time's pattern and its zone),
//!   all checked at the wire's door before a file is read
//!   (`vike_datahub_client::archive::validate_import_target`). Nothing reads this box's locale.
//! - **A dataset is a folder of `.csv` files**, flat. Which days a file holds is in its rows, not its
//!   name, so each is read and decoded at plan time ([`ArchiveFormat::probe`]): a file the decoder
//!   refuses refuses the whole request, naming the line and the rule, before anything is written.
//! - **Each day is stored on its own** under the lane's day key, from the file that holds it, which is
//!   decoded once per request however many of its days the request stores.
//! - **The same file may be SENT instead** (`Request::CsvUpload`, decision 0272, [`CsvUploadFormat`]): the
//!   text arrives in this daemon's memory, is decoded once, and lands under the same lane, day keys and
//!   one-owner table. An upload only CREATES a series: one that holds rows is refused at the door, and
//!   a new user series past [`UPLOAD_MAX_USER_SERIES`] too.

use std::sync::Arc;

use vike_backfill::archive_import::{
    ArchiveDayClass, ArchiveDayResult, ArchiveImport, ArchiveLane,
};
use vike_backfill::user_bars::{ArchiveLaneUserBars, SOURCE};
use vike_data::SeriesCoverage;
use vike_data::archive_day::ArchiveDayRefusal;
use vike_data::csv_bars::{CsvBarsFormat, header_cells};
use vike_data::store::store_kind::USER_LANE_PREFIX;
use vike_datahub_client::archive::{
    ArchiveDayClassWire, ArchiveDayResultWire, IMPORT_PREVIEW_ROWS, ImportOutcomeDay,
    ImportPlanPreviewRow, ImportSpec, ImportTarget,
};
use vike_datahub_client::upload::{CsvUploadDone, CsvUploadSpec};
use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_store::HistoryStoreDataFusion;

use super::{ImportStep, empty_payload, wire_class, wire_result};
use crate::import::upload::CsvUploadFormat;
use crate::import::{ArchiveFormat, ArchiveFormatLayoutFile, ArchiveFormatProbe, ImportSession};

/// The most `user-*` series a datahub holds before an upload of a NEW one is refused: the COUNT that
/// bounds the two names an upload's request may choose freely, the symbol and the label (decision 0058's
/// bounded leg; decision 0272). It counts every user series, an on-box import's too, and refuses only
/// an upload: an operator holding the control key imports on the box past it.
pub const UPLOAD_MAX_USER_SERIES: usize = 512;

/// The largest file read: a decade of one-minute bars is about 300 MB of CSV, so this is generous for
/// a day-bounded import, and every byte of it is held in memory while it decodes.
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;

/// What an `EmptyPayload` refusal says of a day the file holds no bar of.
const EMPTY_DAY: &str = "the file holds no bar of this day. Nothing was stored and no commit key was \
                         spent.";

/// The class a file gets at import time when the decoder refuses it there (it changed since the plan).
pub const CSV_REFUSED: &str = "CsvRefused";

/// A user's CSV of bars, landing in the lane its request names.
pub(super) struct ArchiveFormatCsvBars {
    store: Arc<HistoryStoreDataFusion>,
}

impl ArchiveFormatCsvBars {
    pub(super) fn new(store: Arc<HistoryStoreDataFusion>) -> Self {
        Self { store }
    }
}

/// The lane a request names. The door let only a request with a target through.
fn lane_of(spec: &ImportSpec) -> Result<ArchiveLaneUserBars, String> {
    let Some(t) = &spec.target else {
        return Err(format!("{SOURCE} was sent no target — this is a lane defect"));
    };
    lane_of_target(t)
}

/// The lane a target names, read in its file's format and column mapping.
fn lane_of_target(t: &ImportTarget) -> Result<ArchiveLaneUserBars, String> {
    let format = CsvBarsFormat::parse(&t.file_format)?;
    ArchiveLaneUserBars::new(&t.venue, &t.symbol, &t.interval, &t.label, format, t.columns()?)
}

/// A file's bytes as text and decoded, or the sentence that refuses it.
fn decode(lane: &ArchiveLaneUserBars, file: &[u8]) -> Result<Vec<Bar>, String> {
    let text = std::str::from_utf8(file)
        .map_err(|e| format!("the file is not UTF-8 text (at byte {})", e.valid_up_to()))?;
    lane.decode(text)
}

impl ArchiveFormat for ArchiveFormatCsvBars {
    fn id(&self) -> &'static str {
        SOURCE
    }

    /// Never the plan's: the request's target names the venue.
    fn venue(&self) -> &'static str {
        ""
    }

    fn admit(&self, _dataset: &str) -> Result<String, String> {
        Ok("a user's file of bars, read by the one CSV decoder (a header naming time, open, high, \
            low, close and volume; epoch times in s, ms or µs UTC, or text times in the pattern and \
            zone you gave; one interval; low ≤ open, close ≤ high): stored ONLY in the lane \
            user-<label> of the venue and symbol you named, never in the venue's own series, one \
            owner per UTC day inside the lane"
            .to_string())
    }

    fn takes_target(&self) -> bool {
        true
    }

    /// The files sit flat in the dataset.
    fn accepts_dir(&self, _rel: &[&str], _now_ms: i64) -> bool {
        false
    }

    fn classify_file(&self, rel: &[&str], _now_ms: i64) -> ArchiveFormatLayoutFile {
        match rel {
            [name] if name.to_ascii_lowercase().ends_with(".csv") => ArchiveFormatLayoutFile::Probe,
            _ => ArchiveFormatLayoutFile::Other,
        }
    }

    /// The days the file holds a bar of (a day inside its span with none, a weekend of a stock, is no
    /// day of the file, so it is not planned), and its first bars as the decoder read them.
    fn probe(
        &self,
        spec: &ImportSpec,
        file_name: &str,
        file: &[u8],
    ) -> Result<ArchiveFormatProbe, String> {
        let lane = lane_of(spec)?;
        let bars =
            decode(&lane, file).map_err(|why| format!("`{file_name}` is refused: {why}."))?;
        let mut days: Vec<i64> =
            bars.iter().map(|b| b.timestamp.div_euclid(MS_PER_DAY) * MS_PER_DAY).collect();
        days.dedup();
        if days.is_empty() {
            return Err(format!("`{file_name}` holds no bar."));
        }
        let preview = bars.into_iter().take(IMPORT_PREVIEW_ROWS).collect();
        Ok(ArchiveFormatProbe { days, preview })
    }

    fn max_file_bytes(&self) -> u64 {
        MAX_FILE_BYTES
    }

    fn header_len(&self) -> usize {
        0
    }

    /// A CSV declares no row count, and its size was checked when it was probed.
    fn read_header(
        &self,
        _file_len: u64,
        _prefix: &[u8],
    ) -> Result<Option<u64>, ArchiveDayRefusal> {
        Ok(None)
    }

    fn session<'a>(
        &'a self,
        spec: &ImportSpec,
        now_ms: i64,
    ) -> Result<Box<dyn ImportSession + 'a>, String> {
        let lane = lane_of(spec)?;
        let hist: &'a HistoryStoreDataFusion = &self.store;
        let store = if spec.writes() {
            ImportStep::Import(
                ArchiveImport::open(hist, lane.clone(), &[], now_ms).map_err(|e| e.to_string())?,
            )
        } else {
            let (coverage, keys) = hist.series_facts(&lane.series()).map_err(|e| e.to_string())?;
            ImportStep::Plan { owners: lane.owners(&keys), coverage, now_ms }
        };
        Ok(Box::new(ImportSessionCsvBars { lane, store, decoded: None }))
    }

    fn uploads(&self) -> Option<&dyn CsvUploadFormat> {
        Some(self)
    }
}

/// The class and sentence of a day an upload does not store: only a day that has not ended can be one,
/// since the door admitted an empty series and every day of the file is its own.
fn upload_day_refusal(class: ArchiveDayClass) -> ArchiveDayRefusal {
    match class {
        ArchiveDayClass::TooRecent => ArchiveDayRefusal {
            class: "TooRecent".to_string(),
            detail: "the day has not ended yet, so its bars may still change: it is not stored. \
                     Nothing of it was written and no commit key was spent."
                .to_string(),
        },
        other => ArchiveDayRefusal {
            class: "Held".to_string(),
            detail: format!("the lane already owns this day ({other:?}): it is not stored again."),
        },
    }
}

impl CsvUploadFormat for ArchiveFormatCsvBars {
    fn admit_upload(&self, spec: &CsvUploadSpec) -> Result<(), String> {
        let lane = lane_of_target(&spec.target)?;
        let (coverage, _) = self.store.series_facts(&lane.series()).map_err(|e| e.to_string())?;
        if coverage.rows > 0 {
            let t = &spec.target;
            return Err(format!(
                "the lane {} of {} {} {} already holds {} bars ({} .. {}). An upload creates a \
                 series and never adds to or replaces one, so nothing was read or written. Upload \
                 under a new label, or remove this lane on the datahub's box first: `vike-cli data \
                 hist rm --kind bar --venue {} --symbol {} --interval {} --source {}`.",
                lane.lane(),
                t.venue,
                t.symbol,
                t.interval,
                coverage.rows,
                vike_model::time::epoch_ms_to_utc_date(coverage.first_timestamp),
                vike_model::time::epoch_ms_to_utc_date(coverage.last_timestamp),
                t.venue,
                t.symbol,
                t.interval,
                lane.lane(),
            ));
        }
        let users = self.store.list_series().map_err(|e| e.to_string())?;
        let users = users
            .iter()
            .filter(|s| s.source.as_deref().is_some_and(|s| s.starts_with(USER_LANE_PREFIX)))
            .count();
        if users >= UPLOAD_MAX_USER_SERIES {
            return Err(format!(
                "this datahub holds {users} user series, at its cap of {UPLOAD_MAX_USER_SERIES} for \
                 an upload: nothing was read or written. Remove user lanes nobody reads on the \
                 datahub's box (`vike-cli data hist ls` lists them, `rm … --source user-<label>` \
                 removes one), or import the file there."
            ));
        }
        Ok(())
    }

    fn run_upload(
        &self,
        spec: &CsvUploadSpec,
        text: &str,
        now_ms: i64,
    ) -> Result<CsvUploadDone, String> {
        let lane = lane_of_target(&spec.target)?;
        // A preview's head was cut anywhere: read it up to its last whole line.
        let text = match (spec.head, text.rfind('\n')) {
            (true, Some(end)) => &text[..=end],
            _ => text,
        };
        let format = CsvBarsFormat::parse(&spec.target.file_format)?;
        let mut done = CsvUploadDone { header: header_cells(text, &format), ..Default::default() };
        let bars = match lane.decode(text) {
            Ok(bars) => bars,
            Err(why) => {
                done.refused = Some(why);
                return Ok(done);
            }
        };
        let days: Vec<&[Bar]> =
            bars.chunk_by(|a, b| day_of(a.timestamp) == day_of(b.timestamp)).collect();
        done.preview =
            bars.iter().take(IMPORT_PREVIEW_ROWS).map(ImportPlanPreviewRow::of).collect();
        done.bars = bars.len() as u64;
        done.days = days.len() as u64;
        done.first_day = bars.first().map(|b| day_of(b.timestamp));
        done.last_day = bars.last().map(|b| day_of(b.timestamp));
        if spec.dry_run {
            let (_, keys) = self.store.series_facts(&lane.series()).map_err(|e| e.to_string())?;
            let owners = lane.owners(&keys);
            for day in &days {
                let day = day_of(day[0].timestamp);
                let class = owners.classify(day, now_ms);
                if !class.is_importable() {
                    let result = ArchiveDayResultWire::Refused(upload_day_refusal(class));
                    done.not_stored.push(ImportOutcomeDay { day, result });
                }
            }
            return Ok(done);
        }
        let hist: &HistoryStoreDataFusion = &self.store;
        let mut import = ArchiveImport::open(hist, lane, &[], now_ms).map_err(|e| e.to_string())?;
        // The door's check again, under the day-owner lock: a series that gained rows since stays whole.
        if import.coverage().rows > 0 {
            return Err(
                "the lane gained bars while the file was being sent; nothing was stored. Upload \
                 under a new label."
                    .to_string(),
            );
        }
        for day_bars in days {
            let day = day_of(day_bars[0].timestamp);
            let result = import.import_day(day, || Ok(day_bars.to_vec())).map_err(|e| {
                format!(
                    "the store failed at {}: {e}. The {} days before it are stored and stay so, \
                     and the lane now holds them: send the rest under a new label, or remove the \
                     lane on the datahub's box and upload the file again.",
                    vike_model::time::epoch_ms_to_utc_date(day),
                    done.stored_days
                )
            })?;
            match result {
                ArchiveDayResult::Imported { ticks, .. } => {
                    done.stored_days += 1;
                    done.stored_bars += ticks as u64;
                }
                ArchiveDayResult::Refused(r) if r.class == "TooRecent" => {
                    let result = ArchiveDayResultWire::Refused(upload_day_refusal(
                        ArchiveDayClass::TooRecent,
                    ));
                    done.not_stored.push(ImportOutcomeDay { day, result });
                }
                other => done
                    .not_stored
                    .push(ImportOutcomeDay { day, result: wire_result(other, EMPTY_DAY) }),
            }
        }
        Ok(done)
    }

    fn max_upload_bytes(&self) -> u64 {
        MAX_FILE_BYTES
    }
}

/// The epoch-ms start of the UTC day holding `ts`.
fn day_of(ts: i64) -> i64 {
    ts.div_euclid(MS_PER_DAY) * MS_PER_DAY
}

struct ImportSessionCsvBars<'h> {
    lane: ArchiveLaneUserBars,
    store: ImportStep<'h, ArchiveLaneUserBars>,
    /// The last file decoded, by its digest, so a file's days decode it once per request.
    decoded: Option<(String, Vec<Bar>)>,
}

impl ImportSessionCsvBars<'_> {
    /// `day`'s bars from the file `read` hands over, decoding it only if it is not the one decoded
    /// last.
    fn day_bars(
        lane: &ArchiveLaneUserBars,
        decoded: &mut Option<(String, Vec<Bar>)>,
        file: &[u8],
        day: i64,
    ) -> Result<Vec<Bar>, ArchiveDayRefusal> {
        let digest = vike_hub_protocol::hash::sha256_hex(file);
        if decoded.as_ref().is_none_or(|(d, _)| *d != digest) {
            let bars = decode(lane, file).map_err(|why| {
                ArchiveDayRefusal::new(
                    CSV_REFUSED,
                    format!(
                        "{why}. The file changed since the plan read it. Nothing was stored and no \
                         commit key was spent."
                    ),
                )
            })?;
            *decoded = Some((digest, bars));
        }
        let bars = decoded.as_ref().map_or(&[][..], |(_, bars)| bars.as_slice());
        let end = day + MS_PER_DAY - 1;
        Ok(bars.iter().filter(|b| (day..=end).contains(&b.timestamp)).cloned().collect())
    }
}

impl ImportSession for ImportSessionCsvBars<'_> {
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
        let Self { lane, store, decoded } = self;
        let ImportStep::Import(import) = store else {
            return Err("a dry run's session cannot import — this is a lane defect".to_string());
        };
        let result = import
            .import_day(day, || {
                let file = read()?;
                Self::day_bars(lane, decoded, &file, day)
            })
            .map_err(|e| e.to_string())?;
        Ok(wire_result(result, EMPTY_DAY))
    }

    fn verify_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResultWire, String> {
        let file = match read() {
            Ok(file) => file,
            Err(refusal) => return Ok(ArchiveDayResultWire::Refused(refusal)),
        };
        Ok(match Self::day_bars(&self.lane, &mut self.decoded, &file, day) {
            Ok(bars) if bars.is_empty() => ArchiveDayResultWire::Refused(empty_payload(EMPTY_DAY)),
            Ok(bars) => ArchiveDayResultWire::Verified { ticks: bars.len() as u64, report: None },
            Err(refusal) => ArchiveDayResultWire::Refused(refusal),
        })
    }
}

#[path = "csv_bars_tests.rs"]
#[cfg(test)]
mod csv_bars_tests;
