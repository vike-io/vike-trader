//! Text score rows — the `kind=text_score` series: the NUMBERS a host-side text model computed for a
//! stored text (`crate::text_log::TextRow`), so a study reads a sentiment or an embedding instead of
//! running a model it cannot load (a study is jailed with no file and no network, decision 0281).
//!
//! # Identity, and the clock a study selects on
//!
//! A series is `kind=text_score/venue=<text kind>/symbol=<model name>`: the scored texts' kind
//! ([`crate::text_log::TEXT_KIND_NEWS`]) and the model's pinned NAME (`finbert`,
//! `all-minilm-l6-v2`: `vike_nlp::pins::PINNED_MODELS`). A row names its text by
//! [`TextScoreRow::text_id`], the scored row's `TextRow::text_id`, and the exact model by
//! [`TextScoreRow::model`] (`name@revision`), so a pin bump writes new rows beside the old ones and a
//! reader can tell them apart.
//!
//! A row is the numbers of ONE CHUNK of its text ([`TextScoreRow::chunk`]): the scorer cuts each text
//! by `vike_rag::chunk::chunk_texts` (the book's W17) and scores every chunk, and a study re-cuts the
//! text with the same rule to pair a vector with its words. A text that fits one chunk (every ordinary
//! news item) has one row, chunk 0, scored on its whole text.
//!
//! The store's `ts` column is [`TextScoreRow::received_ms`], the SCORED TEXT's received time, not the
//! time the scorer ran ([`TextScoreRow::scored_ms`], provenance only). A pinned model reads nothing
//! but the text, so its numbers were computable the moment the text was held; a point-in-time read
//! ([`crate::HistoryStore::text_scores_as_of`]) therefore selects on the same clock as
//! [`crate::HistoryStore::texts_as_of`], and the two join on `(received_ms, text_id)`.
//!
//! # The numbers
//!
//! [`TextScoreRow::values`] is one string, the model's output as `f64`s joined by
//! [`TEXT_SCORE_SEPARATOR`] in their shortest round-trip form, so [`TextScoreRow::value_list`] reads
//! back the same bits. What position means what is the MODEL's: the sentiment model writes `positive
//! negative neutral`, the embedding model its unit-length vector (`f32`s widened to `f64` exactly, so
//! `as f32` of a value read back is the encoder's own number).
//!
//! Pure data, no I/O. The Parquet codec is `vike-store`'s, beside the other kinds'.

use serde::{Deserialize, Serialize};

/// What separates the numbers inside [`TextScoreRow::values`].
pub const TEXT_SCORE_SEPARATOR: char = ' ';

/// The archive format the datahub imports score files under (`vike-cli data history import text-scores
/// <MODEL>`), and so the folder under `<project>/market_data/imports/` the scorer writes them into.
pub const TEXT_SCORE_FORMAT: &str = "text-scores";

/// What ends a day's file name.
const TEXT_SCORE_FILE_SUFFIX: &str = ".jsonl";

/// **A model's DATASET in the import**: its folder under `<imports>/text-scores/`, the word `vike-cli data
/// history import text-scores <DATASET>` takes. The import's dataset grammar is upper case
/// (`vike_datahub_client::archive::validate_import_dataset`), so it is the model's name upper-cased
/// (`FINBERT`); the series symbol stays the model's own name ([`text_score_model`]).
pub fn text_score_dataset(model: &str) -> String {
    model.to_ascii_uppercase()
}

/// The model a [`text_score_dataset`] names: the series symbol its rows land under (`finbert`).
pub fn text_score_model(dataset: &str) -> String {
    dataset.to_ascii_lowercase()
}

/// **The file of one UTC day**, the one spelling both the scorer and the import read: `YYYY-MM-DD.jsonl`
/// for the day starting at `day_ms`, holding one [`TextScoreRow`] as JSON per line, every row's
/// `received_ms` inside that day.
pub fn text_score_file_name(day_ms: i64) -> String {
    format!("{}{TEXT_SCORE_FILE_SUFFIX}", vike_model::time::epoch_ms_to_utc_date(day_ms))
}

/// The start of the UTC day a [`text_score_file_name`] names; `None` for any other name.
pub fn text_score_file_day(name: &str) -> Option<i64> {
    text_score_day(name.strip_suffix(TEXT_SCORE_FILE_SUFFIX)?)
}

/// The start of the UTC day a `YYYY-MM-DD` label names (the day part of a file name and of the
/// import's day key); `None` for anything else, a bare number or a date that does not exist included.
pub fn text_score_day(label: &str) -> Option<i64> {
    let dashed = label.len() == 10 && label.as_bytes()[4] == b'-' && label.as_bytes()[7] == b'-';
    dashed.then(|| vike_model::time::parse_date_label(label).ok()).flatten()
}

/// One model's numbers for one stored text, persisted as the `kind=text_score` HistoryStore series.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextScoreRow {
    /// The scored text's `TextRow::received_ms`, epoch milliseconds UTC: the store's `ts`, what a
    /// point-in-time read selects on.
    pub received_ms: i64,
    /// The scored text's `TextRow::text_id`.
    pub text_id: String,
    /// Which chunk of the text these numbers are of: its position in `vike_rag::chunk::chunk_texts` of
    /// the text's title, summary and body (0 for a text that is one chunk).
    pub chunk: u32,
    /// When the scorer computed this row, epoch milliseconds UTC. Provenance; never a selector.
    pub scored_ms: i64,
    /// The model that computed it, `name@revision`.
    pub model: String,
    /// The model's output, joined by [`TEXT_SCORE_SEPARATOR`] ([`TextScoreRow::joined_values`]).
    pub values: String,
}

impl TextScoreRow {
    /// Numbers as one stored string, each in its shortest round-trip form.
    pub fn joined_values(values: &[f64]) -> String {
        let words: Vec<String> = values.iter().map(ToString::to_string).collect();
        words.join(&TEXT_SCORE_SEPARATOR.to_string())
    }

    /// The numbers [`TextScoreRow::values`] holds, in order; none for an empty string. A word that
    /// is not a number is an error naming it, never a silent zero.
    pub fn value_list(&self) -> Result<Vec<f64>, String> {
        self.values
            .split(TEXT_SCORE_SEPARATOR)
            .filter(|w| !w.is_empty())
            .map(|w| w.parse::<f64>().map_err(|e| format!("text score value {w:?}: {e}")))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip_bit_for_bit_and_an_f32_keeps_its_bits() {
        let probabilities = [0.875_f64, 0.1, 0.025];
        let mut row = TextScoreRow {
            received_ms: 1,
            text_id: "news\u{1f}S\u{1f}https://example.test/a".to_string(),
            chunk: 0,
            scored_ms: 2,
            model: "finbert@0".to_string(),
            values: TextScoreRow::joined_values(&probabilities),
        };
        assert_eq!(row.values, "0.875 0.1 0.025");
        assert_eq!(row.value_list().unwrap(), probabilities);

        let embedding = [0.123_456_79_f32, -1.0e-8, 1.0, f32::MIN_POSITIVE];
        let widened: Vec<f64> = embedding.iter().map(|&x| f64::from(x)).collect();
        row.values = TextScoreRow::joined_values(&widened);
        let back: Vec<f32> = row.value_list().unwrap().into_iter().map(|v| v as f32).collect();
        assert_eq!(back, embedding, "an f32 widened, written and read back keeps its bits");

        row.values = String::new();
        assert!(row.value_list().unwrap().is_empty());
        row.values = "0.5 x".to_string();
        assert!(row.value_list().unwrap_err().contains("\"x\""));
    }

    #[test]
    fn a_models_dataset_is_its_name_upper_cased_and_back() {
        for model in ["finbert", "all-minilm-l6-v2"] {
            assert_eq!(text_score_model(&text_score_dataset(model)), model);
        }
        assert_eq!(text_score_dataset("all-minilm-l6-v2"), "ALL-MINILM-L6-V2");
    }

    #[test]
    fn a_day_file_is_named_by_its_utc_date_and_nothing_else_names_one() {
        let day = 1_791_763_200_000; // 2026-10-12T00:00:00Z
        assert_eq!(text_score_file_name(day), "2026-10-12.jsonl");
        assert_eq!(text_score_file_day("2026-10-12.jsonl"), Some(day));
        for other in
            ["2026-10-12.tsv", "2026-10-12", "1791763200000.jsonl", "2026-02-30.jsonl", "x.jsonl"]
        {
            assert_eq!(text_score_file_day(other), None, "{other}");
        }
    }
}
