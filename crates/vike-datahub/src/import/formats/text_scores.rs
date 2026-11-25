//! **A text model's scores (`text-scores`)**, the files `vike-nlp score` writes (the book's W8b). The
//! decode, the day key and the one-owner rule are `crates/vike-backfill/src/text_scores.rs`'s
//! `ArchiveLaneTextScores`; this file maps them onto the lane's seam.
//!
//! - **A dataset is a model's name in upper case** (`FINBERT`, `ALL-MINILM-L6-V2`, the import's dataset
//!   grammar; `vike_data::text_score_log`'s `text_score_dataset`): the rows land under
//!   `kind=text_score/venue=news/symbol=<model>` (`finbert`), and every row's own label must name it.
//! - **One JSON-lines file per UTC day** of the scored texts' received time, named
//!   `vike_data::text_score_log`'s `text_score_file_name` (`2026-11-12.jsonl`), flat in the dataset.
//! - **The datahub only PARSES the file.** It links no model: the scores were computed by the separate
//!   `vike-nlp` tool, so this daemon's writes stay those of its own import lane
//!   (`docs/decisions/0084-only-the-datahub-touches-the-store.md`).

use std::sync::Arc;

use vike_backfill::archive_import::{ArchiveImport, ArchiveLane};
use vike_backfill::text_scores::ArchiveLaneTextScores;
use vike_data::SeriesCoverage;
use vike_data::archive_day::ArchiveDayRefusal;
use vike_data::text_log::TEXT_KIND_NEWS;
use vike_data::text_score_log::{TEXT_SCORE_FORMAT, text_score_file_day, text_score_model};
use vike_datahub_client::archive::{ArchiveDayClassWire, ArchiveDayResultWire, ImportSpec};
use vike_store::HistoryStoreDataFusion;

use super::{ImportStep, empty_payload, wire_result};
use crate::import::walk::FILE_TOO_LARGE;
use crate::import::{ArchiveFormat, ArchiveFormatLayoutFile, ImportSession};

/// The largest file read: a day of scored news is a few thousand rows, an embedding row about 8 KB.
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// What an `EmptyPayload` refusal says of a day's file with no row.
const EMPTY_FILE: &str = "the file holds no score. Nothing was stored and no commit key was spent, \
                          so the day stays free: run `vike-nlp score` over it again.";

/// The class a day's file gets when its rows are refused.
const SCORES_REFUSED: &str = "ScoresRefused";

/// A model's scores of the news, landing under `venue=news`.
pub(super) struct TextScores {
    store: Arc<HistoryStoreDataFusion>,
}

impl TextScores {
    pub(super) fn new(store: Arc<HistoryStoreDataFusion>) -> Self {
        Self { store }
    }
}

impl ArchiveFormat for TextScores {
    fn id(&self) -> &'static str {
        TEXT_SCORE_FORMAT
    }

    fn venue(&self) -> &'static str {
        TEXT_KIND_NEWS
    }

    fn admit(&self, dataset: &str) -> Result<String, String> {
        let dataset = text_score_model(dataset);
        Ok(format!(
            "the scores {dataset} gave the news, as `vike-nlp score` wrote them · stored as \
             kind=text_score {TEXT_KIND_NEWS} {dataset}, each row at its text's received time · one \
             YYYY-MM-DD.jsonl file per UTC day; a row whose model label is not {dataset}@… refuses \
             its day"
        ))
    }

    /// The day files sit flat in the model's folder.
    fn accepts_dir(&self, _rel: &[&str], _now_ms: i64) -> bool {
        false
    }

    fn classify_file(&self, rel: &[&str], _now_ms: i64) -> ArchiveFormatLayoutFile {
        match rel {
            [name] => text_score_file_day(name)
                .map_or(ArchiveFormatLayoutFile::Other, ArchiveFormatLayoutFile::Daily),
            _ => ArchiveFormatLayoutFile::Other,
        }
    }

    fn max_file_bytes(&self) -> u64 {
        MAX_FILE_BYTES
    }

    fn header_len(&self) -> usize {
        0
    }

    /// Plain text declares no row count; only the size is checked before the decode.
    fn read_header(&self, file_len: u64, _prefix: &[u8]) -> Result<Option<u64>, ArchiveDayRefusal> {
        if file_len > MAX_FILE_BYTES {
            return Err(ArchiveDayRefusal {
                class: FILE_TOO_LARGE.to_string(),
                detail: format!(
                    "the file is {file_len} bytes, over this format's {MAX_FILE_BYTES}-byte cap. \
                     Nothing was decoded or stored and no commit key was spent."
                ),
            });
        }
        Ok(None)
    }

    fn session<'a>(
        &'a self,
        spec: &ImportSpec,
        now_ms: i64,
    ) -> Result<Box<dyn ImportSession + 'a>, String> {
        let hist: &'a HistoryStoreDataFusion = &self.store;
        let lane = ArchiveLaneTextScores::new(TEXT_KIND_NEWS, &text_score_model(&spec.dataset));
        let store = if spec.writes() {
            ImportStep::Import(
                ArchiveImport::open(hist, lane.clone(), &spec.bars, now_ms)
                    .map_err(|e| e.to_string())?,
            )
        } else {
            let (coverage, keys) = hist.series_facts(&lane.series()).map_err(|e| e.to_string())?;
            ImportStep::Plan { owners: lane.owners(&keys), coverage, now_ms, replace: None }
        };
        Ok(Box::new(ScoreSession { lane, store }))
    }
}

struct ScoreSession<'h> {
    lane: ArchiveLaneTextScores,
    store: ImportStep<'h, ArchiveLaneTextScores>,
}

/// A day's file as rows, or the refusal naming the line.
fn decode(
    lane: &ArchiveLaneTextScores,
    file: &[u8],
) -> Result<Vec<vike_data::TextScoreRow>, ArchiveDayRefusal> {
    let text = std::str::from_utf8(file).map_err(|e| {
        ArchiveDayRefusal::new(SCORES_REFUSED, format!("the file is not UTF-8 text ({e})"))
    })?;
    lane.decode(text).map_err(|why| {
        ArchiveDayRefusal::new(
            SCORES_REFUSED,
            format!("{why}. Nothing was stored and no commit key was spent."),
        )
    })
}

impl ImportSession for ScoreSession<'_> {
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
        let result =
            import.import_day(day, || decode(lane, &read()?)).map_err(|e| e.to_string())?;
        Ok(wire_result(result, EMPTY_FILE))
    }

    /// Decode the day and check it, writing nothing: the import's checks, the day's bounds included.
    fn verify_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResultWire, String> {
        let rows = match read().and_then(|file| decode(&self.lane, &file)) {
            Ok(rows) => rows,
            Err(refusal) => return Ok(ArchiveDayResultWire::Refused(refusal)),
        };
        let end = day + vike_model::MS_PER_DAY - 1;
        if let Some(out) = rows.iter().find(|r| !(day..=end).contains(&r.received_ms)) {
            return Ok(ArchiveDayResultWire::Refused(ArchiveDayRefusal::new(
                SCORES_REFUSED,
                format!(
                    "a row's text was received at {}, outside the day the file names. Nothing was \
                     stored and no commit key was spent.",
                    out.received_ms
                ),
            )));
        }
        Ok(if rows.is_empty() {
            ArchiveDayResultWire::Refused(empty_payload(EMPTY_FILE))
        } else {
            ArchiveDayResultWire::Verified { ticks: rows.len() as u64, report: None }
        })
    }
}

#[path = "text_scores_tests.rs"]
#[cfg(test)]
mod text_scores_tests;
