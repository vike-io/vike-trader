//! Score stored texts into the DAY FILES the datahub's `text-scores` import reads.
//!
//! The owner's route (2026-11-16): this tool reads the texts over the datahub wire, scores them on the
//! CPU, and writes one file per UTC day of the texts' RECEIVED time into
//! `<project>/market_data/imports/text-scores/<MODEL>/`; the operator then runs `vike-cli data history
//! import text-scores <MODEL>`, and the datahub, which links no model, parses the files into
//! `kind=text_score`. The file grammar is `vike_data::text_score_log`'s, the one spelling both sides
//! read.
//!
//! - **What is scored**: each CHUNK of a text, cut by `vike_rag::chunk::chunk_texts` (the book's
//!   W17: the one rule a study re-cuts by to pair a vector with its words). A news item that fits is
//!   one chunk: its title, then its summary on the next line when it has one (the tokenizer reads a
//!   line break as a space). One chunk per forward pass (the crate doc).
//! - **What a row says**: [`vike_data::TextScoreRow`]: the text's `received_ms` and `text_id`, the
//!   chunk's index, the time this run scored it, the model's `name@revision`, and its numbers: the
//!   sentiment model's `positive negative neutral`, the embedding model's unit vector.
//! - **A day's file is written once**: [`write_day_file`] keeps a file already there (whole, as
//!   `write_atomic` leaves every file), so a re-run scores only the days still missing. A day is
//!   scored once it has ENDED: the caller refuses a window reaching into a day that has not.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use vike_data::{TextRow, TextScoreRow};

use crate::embedding::EmbeddingModel;
use crate::error::NlpError;
use crate::pins::{ModelTask, PinnedModel};
use crate::sentiment::SentimentModel;

/// A loaded model of either task, as the numbers one text gets.
pub enum Scorer {
    Sentiment(SentimentModel),
    Embedding(EmbeddingModel),
}

impl Scorer {
    /// Load `model` from its verified folder under `models_dir`, whichever task it has.
    pub fn load(model: &PinnedModel, models_dir: &Path) -> Result<Self, NlpError> {
        Ok(match model.task {
            ModelTask::Sentiment => Scorer::Sentiment(SentimentModel::load(model, models_dir)?),
            ModelTask::Embedding => Scorer::Embedding(EmbeddingModel::load(model, models_dir)?),
        })
    }

    /// The numbers of ONE text: `positive negative neutral`, or the embedding widened exactly to `f64`.
    pub fn values(&self, text: &str) -> Result<Vec<f64>, NlpError> {
        Ok(match self {
            Scorer::Sentiment(model) => {
                let s = model.score(text)?;
                vec![s.positive, s.negative, s.neutral]
            }
            Scorer::Embedding(model) => model.embed(text)?.into_iter().map(f64::from).collect(),
        })
    }
}

/// The texts grouped by the UTC day of their received time, ascending.
pub fn texts_by_day(texts: &[TextRow]) -> BTreeMap<i64, Vec<&TextRow>> {
    let mut days: BTreeMap<i64, Vec<&TextRow>> = BTreeMap::new();
    for text in texts {
        days.entry(vike_model::time::floor_ms_to_utc_day(text.received_ms)).or_default().push(text);
    }
    days
}

/// One score row per CHUNK of each text (`vike_rag::chunk::chunk_texts`), in the texts' order then the
/// chunks', each scored by `values` (a [`Scorer::values`]) and stamped `model_label` (`name@revision`)
/// and `scored_ms`. A text with no word has no row.
pub fn score_rows(
    texts: &[&TextRow],
    model_label: &str,
    scored_ms: i64,
    mut values: impl FnMut(&str) -> Result<Vec<f64>, NlpError>,
) -> Result<Vec<TextScoreRow>, NlpError> {
    let mut rows = Vec::new();
    for text in texts {
        let text_id = text.text_id();
        let chunks = vike_rag::chunk::chunk_texts(&text.title, &text.summary, &text.body);
        for (chunk, words) in (0u32..).zip(&chunks) {
            rows.push(TextScoreRow {
                received_ms: text.received_ms,
                text_id: text_id.clone(),
                chunk,
                scored_ms,
                model: model_label.to_string(),
                values: TextScoreRow::joined_values(&values(words)?),
            });
        }
    }
    Ok(rows)
}

/// The path of `day`'s file in `dir`.
pub fn day_file(dir: &Path, day: i64) -> PathBuf {
    dir.join(vike_data::text_score_log::text_score_file_name(day))
}

/// Write `rows` (one UTC day's, the day starting at `day`) as that day's file in `dir`, one row as JSON
/// per line, atomically; `dir` is created. Returns `false`, writing nothing, when the file is already
/// there.
pub fn write_day_file(dir: &Path, day: i64, rows: &[TextScoreRow]) -> Result<bool, NlpError> {
    let path = day_file(dir, day);
    if path.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(dir)
        .map_err(|source| NlpError::Io { path: dir.to_path_buf(), source })?;
    let mut text = String::new();
    for row in rows {
        let line = serde_json::to_string(row)
            .map_err(|e| NlpError::Model(format!("encode a score row: {e}")))?;
        text.push_str(&line);
        text.push('\n');
    }
    vike_utility::atomic_file::write_atomic(&path, text.as_bytes())
        .map_err(|source| NlpError::Io { path, source })?;
    Ok(true)
}

#[cfg(test)]
#[path = "score_texts_tests.rs"]
mod tests;
