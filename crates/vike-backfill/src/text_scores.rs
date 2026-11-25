//! A text model's SCORES as an archive lane: the `text-scores` import's store half (the book's W8b).
//!
//! `vike-nlp score` (a separate tool: the datahub links no model) reads the stored texts over the
//! datahub wire, scores them on the CPU and writes one file per UTC day of the texts' received time
//! into `<project>/market_data/imports/text-scores/<MODEL>/` (`vike_data::text_score_log`'s
//! `text_score_file_name`). `crate::archive_import` runs each day, as for every archive format; this
//! file adds the lane's series and day key, the one-owner table and the check of a file's rows.
//!
//! - **The series**: `kind=text_score/venue=<text kind>/symbol=<model>`, one per model, each row keyed
//!   to its text by `text_id` and stamped with the text's received time (`vike_data::TextScoreRow`).
//! - **One owner per UTC day**: this lane's day key holds a day, and any other key the series holds
//!   meets every day, the conservative direction. A day stored once is not stored again; a new model
//!   REVISION is a new label in the same series, scored by a fresh run over days not yet held.
//! - **A file is checked before a byte is stored**: every line one `TextScoreRow`, a text id, a model
//!   label `<model>@<revision>` naming this lane's model, numbers that parse. A row outside the day is
//!   the archive import's own refusal.
//! - **No publication margin**: the scorer writes only days that have ended, and a day that has not is
//!   refused as too recent all the same, so a partial day never spends its key.

use vike_data::archive_day::{ArchiveDayBars, ArchiveDayRefusal};
use vike_data::text_score_log::{TEXT_SCORE_FORMAT, text_score_day};
use vike_data::{HistoryStore, SeriesId, TextScoreRow, TimestampRange};
use vike_model::MS_PER_DAY;
use vike_model::StoreKindId;
use vike_model::time::epoch_ms_to_utc_date;
use vike_store::HistoryStoreDataFusion;

use crate::archive_import::{ArchiveDayAppend, ArchiveLane, DayOwnerKey, DayOwnerRole, DayOwners};
use crate::error::CollectError;

/// A day's key, spent by its scores: `day` is `YYYY-MM-DD`.
pub fn day_commit_key(text_kind: &str, model: &str, day: &str) -> String {
    format!("text-scores:{text_kind}:{model}:{day}")
}

/// One model's scores of one text kind.
#[derive(Debug, Clone)]
pub struct ArchiveLaneTextScores {
    text_kind: String,
    model: String,
}

impl ArchiveLaneTextScores {
    /// The lane of `model`'s scores of `text_kind` texts.
    pub fn new(text_kind: &str, model: &str) -> Self {
        Self { text_kind: text_kind.to_string(), model: model.to_string() }
    }

    /// The series the lane's rows land in.
    pub fn series(&self) -> SeriesId {
        SeriesId::per_symbol(StoreKindId::TextScore, &self.text_kind, &self.model, None)
    }

    /// Read a day's file: one `TextScoreRow` as JSON per line (blank lines skipped). Refused, naming
    /// the line: a line that is not one, an empty text id, a model label that is not this lane's
    /// model, and numbers that do not parse.
    pub fn decode(&self, text: &str) -> Result<Vec<TextScoreRow>, String> {
        let label_head = format!("{}@", self.model);
        let mut rows = Vec::new();
        for (index, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            let at = index + 1;
            let row: TextScoreRow = serde_json::from_str(line)
                .map_err(|e| format!("line {at} is not a text score row: {e}"))?;
            if row.text_id.is_empty() {
                return Err(format!("line {at} names no text (an empty text_id)"));
            }
            if !row.model.starts_with(&label_head) {
                return Err(format!(
                    "line {at} was scored by {:?}, and this dataset is the model {:?}: put the \
                     file under that model's own folder",
                    row.model, self.model
                ));
            }
            row.value_list().map_err(|e| format!("line {at}: {e}"))?;
            rows.push(row);
        }
        Ok(rows)
    }

    fn key(&self, day: i64) -> String {
        day_commit_key(&self.text_kind, &self.model, &epoch_ms_to_utc_date(day))
    }
}

impl ArchiveLane for ArchiveLaneTextScores {
    type Row = TextScoreRow;

    fn source(&self) -> &str {
        TEXT_SCORE_FORMAT
    }

    fn symbol(&self) -> &str {
        &self.model
    }

    fn row_series(&self) -> SeriesId {
        self.series()
    }

    fn row_timestamp(row: &TextScoreRow) -> i64 {
        row.received_ms
    }

    /// See the module doc: this lane's day keys hold their days, and any other key meets every day.
    fn owners(&self, keys: &[String]) -> DayOwners {
        let own_head = day_commit_key(&self.text_kind, &self.model, "");
        let owners = keys
            .iter()
            .map(|key| match key.strip_prefix(&own_head).and_then(text_score_day) {
                Some(day) => DayOwnerKey {
                    lane: DayOwnerRole::Archive,
                    range: Some((day, day + MS_PER_DAY - 1)),
                    key: key.clone(),
                },
                None => DayOwnerKey { lane: DayOwnerRole::Other, range: None, key: key.clone() },
            })
            .collect();
        DayOwners::new(0, owners)
    }

    fn overlapped_detail(&self, keys: &str) -> String {
        format!(
            "the scores of {} already hold rows under other keys ({keys}), and a key this import \
             cannot read may hold this day, so it is not imported twice. Nothing was read and no \
             commit key was spent.",
            self.model
        )
    }

    fn append_day(
        &self,
        hist: &HistoryStoreDataFusion,
        rows: &[TextScoreRow],
        day: i64,
        _end: i64,
        _bars: &[String],
    ) -> Result<Result<ArchiveDayAppend, ArchiveDayRefusal>, CollectError> {
        let key = self.key(day);
        let rows = hist.append_text_scores(&self.text_kind, &self.model, rows, Some(&key))?;
        let spent = vec![(DayOwnerRole::Archive, key.clone())];
        Ok(Ok(ArchiveDayAppend { rows, key, spent }))
    }

    fn undividable_interval_remedy(&self) -> String {
        format!(
            "{TEXT_SCORE_FORMAT} derives no bars: its rows are numbers about texts, not prices."
        )
    }

    fn bar_series(&self, _interval: &str) -> SeriesId {
        self.series()
    }

    fn bar_commit_key(&self, _interval: &str, range: TimestampRange) -> String {
        self.key(range.start.unwrap_or(0))
    }

    /// Never reached: the format opens its import with no bar interval to derive.
    fn derive_bars(
        &self,
        _hist: &HistoryStoreDataFusion,
        _interval: &str,
        _range: TimestampRange,
    ) -> Result<usize, CollectError> {
        Err(CollectError::Refused(self.undividable_interval_remedy().into()))
    }

    fn bars_refused(
        &self,
        refused: &[(String, String)],
        _written: &[ArchiveDayBars],
        rows: &str,
    ) -> ArchiveDayRefusal {
        let why = refused.iter().map(|(iv, e)| format!("{iv}: {e}")).collect::<Vec<_>>().join("; ");
        ArchiveDayRefusal::new("BarsRefused", format!("the store refused ({why}); {rows}."))
    }
}

#[path = "text_scores_tests.rs"]
#[cfg(test)]
mod text_scores_tests;
