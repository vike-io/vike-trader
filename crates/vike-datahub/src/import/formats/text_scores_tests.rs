//! The `text-scores` format through the lane's own verb, over a real store: a day's file lands in the
//! model's series at its texts' received time, a second import stores nothing twice, and a file whose
//! rows name another model writes nothing.

use std::sync::Arc;

use vike_data::{HistoryStore, TextScoreRow, TimestampRange};
use vike_datahub_client::archive::ImportDone;
use vike_datahub_client::proto::Response;
use vike_model::MS_PER_DAY;

use super::*;
use crate::import::{ImportLane, import_archive_verb};

/// 2024-01-15, a UTC midnight, long ended.
const DAY: i64 = 19_737 * MS_PER_DAY;

struct Project {
    tmp: tempfile::TempDir,
    store: Arc<HistoryStoreDataFusion>,
}

impl Project {
    fn new() -> Project {
        let tmp = tempfile::tempdir().expect("a temp dir");
        let store =
            Arc::new(HistoryStoreDataFusion::open(tmp.path().join("hist")).expect("a store"));
        Project { tmp, store }
    }

    fn put(&self, model: &str, day: i64, rows: &[TextScoreRow]) {
        let dataset = vike_data::text_score_log::text_score_dataset(model);
        let dir = self.tmp.path().join("imports").join(TEXT_SCORE_FORMAT).join(dataset);
        std::fs::create_dir_all(&dir).unwrap();
        let lines: Vec<String> = rows.iter().map(|r| serde_json::to_string(r).unwrap()).collect();
        let name = vike_data::text_score_log::text_score_file_name(day);
        std::fs::write(dir.join(name), lines.join("\n")).unwrap();
    }

    fn run(&self, model: &str) -> ImportDone {
        let root = self.tmp.path().join("imports");
        let lane = ImportLane::new(
            root,
            super::super::real_import_registry(Arc::clone(&self.store), None),
        );
        let spec = ImportSpec {
            format: TEXT_SCORE_FORMAT.to_string(),
            dataset: vike_data::text_score_log::text_score_dataset(model),
            from_day: None,
            to_day: None,
            bars: Vec::new(),
            dry_run: false,
            verify: false,
            replace_holed: false,
            target: None,
        };
        match import_archive_verb(&spec, Some(&lane), &|| false) {
            Response::ArchiveImported(done) => *done,
            other => panic!("not an import's answer: {other:?}"),
        }
    }

    fn stored(&self, model: &str) -> Vec<TextScoreRow> {
        self.store.scan_text_scores(TEXT_KIND_NEWS, model, TimestampRange::all()).unwrap()
    }
}

fn row(received_ms: i64, model: &str) -> TextScoreRow {
    TextScoreRow {
        received_ms,
        text_id: format!("news\u{1f}CoinDesk\u{1f}https://a.test/{received_ms}"),
        chunk: 0,
        scored_ms: DAY + 3 * MS_PER_DAY,
        model: format!("{model}@0bc2ac10"),
        values: TextScoreRow::joined_values(&[0.875, 0.0625, 0.0625]),
    }
}

#[test]
fn a_days_file_lands_in_the_models_series_once() {
    let project = Project::new();
    let rows = vec![row(DAY + 1_000, "finbert"), row(DAY + 2_000, "finbert")];
    project.put("finbert", DAY, &rows);
    assert!(project.run("finbert").outcome.is_some());
    assert_eq!(project.stored("finbert"), rows, "the rows as written, at their texts' times");
    project.run("finbert");
    assert_eq!(project.stored("finbert").len(), 2, "a held day is not stored twice");
    assert!(project.stored("all-minilm-l6-v2").is_empty(), "another model is another series");
}

#[test]
fn a_file_scored_by_another_model_writes_nothing() {
    let project = Project::new();
    project.put("finbert", DAY, &[row(DAY + 1_000, "all-minilm-l6-v2")]);
    project.run("finbert");
    assert!(project.stored("finbert").is_empty());
    let format = TextScores::new(Arc::clone(&project.store));
    assert_eq!(
        format.classify_file(&["2024-01-15.jsonl"], DAY),
        ArchiveFormatLayoutFile::Daily(DAY)
    );
    assert_eq!(format.classify_file(&["notes.txt"], DAY), ArchiveFormatLayoutFile::Other);
}
