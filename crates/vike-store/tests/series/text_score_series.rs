//! `kind=text_score` HistoryStore series — a host-side text model's numbers for the stored texts.
//! Split into `datafusion_tests` / `memory_tests` like the other kinds.
//!
//! What carries weight here:
//!
//! * [`datafusion_tests::text_score_series_roundtrips_datafusion`]: the six columns come back as
//!   written (a chunk index included), and a vector of `f64`s survives the one-string `values` column
//!   bit for bit.
//! * [`datafusion_tests::scores_read_point_in_time_on_their_texts_received_time`]: a score is
//!   selected by its TEXT's received time, never by when the scorer ran, so it joins the
//!   `texts_as_of` window of the same decision.
//! * [`datafusion_tests::run_maintenance_handles_text_score_series`]: the compaction dispatch's arm
//!   at RUNTIME (`text_series.rs` says why a text-only gate cannot see a missing one).

use vike_data::TextScoreRow;

const T0: i64 = 1_791_786_600_000;
const MINUTE: i64 = 60_000;
const FINBERT: &str = "finbert";

/// FinBERT's three numbers for the text received at `received_ms`, scored a day later.
fn score(received_ms: i64, url: &str, values: &[f64]) -> TextScoreRow {
    TextScoreRow {
        received_ms,
        text_id: format!("news\u{1f}CoinDesk\u{1f}{url}"),
        chunk: 0,
        scored_ms: received_ms + 24 * 60 * MINUTE,
        model: "finbert@0bc2ac10dda6929bcb1f77ccc43c7ec5c424f031".to_string(),
        values: TextScoreRow::joined_values(values),
    }
}

mod datafusion_tests {
    use super::{FINBERT, MINUTE, T0, score};
    use vike_data::text_log::TEXT_KIND_NEWS;
    use vike_data::{HistoryStore, TimestampRange};
    use vike_store::{CompactionConfig, HistoryStoreDataFusion, MaintenanceConfig};

    #[test]
    fn text_score_series_roundtrips_datafusion() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        let third = 1.0 / 3.0;
        let rows = vec![
            score(T0, "https://example.test/a", &[0.1, 0.2, 0.7]),
            vike_data::TextScoreRow {
                chunk: 2,
                ..score(T0, "https://example.test/b", &[third, third, 1.0 - 2.0 * third])
            },
        ];
        assert_eq!(
            store.append_text_scores(TEXT_KIND_NEWS, FINBERT, &rows, Some("k1")).unwrap(),
            2
        );

        let got = store.scan_text_scores(TEXT_KIND_NEWS, FINBERT, TimestampRange::all()).unwrap();
        assert_eq!(got, rows, "every column back as written, in the written order");
        let values = got[1].value_list().unwrap();
        assert_eq!(values[0].to_bits(), third.to_bits(), "an f64 survives the text column exactly");
        assert!(
            store
                .scan_text_scores(TEXT_KIND_NEWS, "all-minilm-l6-v2", TimestampRange::all())
                .unwrap()
                .is_empty(),
            "another model is another series"
        );
        assert_eq!(
            store.append_text_scores(TEXT_KIND_NEWS, FINBERT, &rows, Some("k1")).unwrap(),
            0,
            "the same key twice is a no-op"
        );
    }

    /// A score is as old as its text: scored a day later, it is still read at the decision that
    /// could read the text, and never before the text was received.
    #[test]
    fn scores_read_point_in_time_on_their_texts_received_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        let early = score(T0, "https://example.test/early", &[0.9, 0.05, 0.05]);
        let late = score(T0 + 10 * MINUTE, "https://example.test/late", &[0.05, 0.9, 0.05]);
        store
            .append_text_scores(TEXT_KIND_NEWS, FINBERT, &[late.clone(), early.clone()], None)
            .unwrap();
        assert!(early.scored_ms > T0 + 10 * MINUTE, "both were scored after both decisions below");

        let at = |t: i64| store.text_scores_as_of(TEXT_KIND_NEWS, FINBERT, t, 60 * MINUTE).unwrap();
        assert_eq!(at(T0 + MINUTE), vec![early.clone()], "the late text was not received yet");
        assert_eq!(at(T0 + 10 * MINUTE), vec![early, late], "received-ascending");
        assert!(at(T0 - 1).is_empty(), "before any text was received");
    }

    /// The compaction dispatch's `text_score` arm, at runtime. `min_parts: 1` forces the fragment
    /// through.
    #[test]
    fn run_maintenance_handles_text_score_series() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        let rows = vec![
            score(T0, "https://example.test/a", &[0.1, 0.2, 0.7]),
            score(T0 + MINUTE, "https://example.test/b", &[0.3, 0.3, 0.4]),
        ];
        store.append_text_scores(TEXT_KIND_NEWS, FINBERT, &rows, Some("ks")).unwrap();

        let config = MaintenanceConfig {
            compaction: CompactionConfig {
                target_bytes: 1 << 20,
                min_parts: 1,
                ..Default::default()
            },
            retention: None,
            retention_by_kind: Default::default(),
        };
        let report = store.run_maintenance(&config).unwrap();
        assert!(report.failed.is_empty(), "the score series was SKIPPED: {:?}", report.failed);
        assert_eq!(report.compaction.parts_written, 1, "the score series was actually compacted");
        assert_eq!(
            store.scan_text_scores(TEXT_KIND_NEWS, FINBERT, TimestampRange::all()).unwrap(),
            rows
        );
    }
}

mod memory_tests {
    use super::{FINBERT, MINUTE, T0, score};
    use vike_data::text_log::TEXT_KIND_NEWS;
    use vike_data::{HistoryStore, HistoryStoreMemory, TimestampRange};

    #[test]
    fn text_score_series_roundtrips_memory_and_reads_point_in_time() {
        let store = HistoryStoreMemory::new();
        let early = score(T0, "https://example.test/a", &[0.1, 0.2, 0.7]);
        let late = score(T0 + 5 * MINUTE, "https://example.test/b", &[0.7, 0.2, 0.1]);
        assert_eq!(store.append_text_scores(TEXT_KIND_NEWS, FINBERT, &[], Some("k")).unwrap(), 0);
        assert_eq!(
            store
                .append_text_scores(
                    TEXT_KIND_NEWS,
                    FINBERT,
                    &[late.clone(), early.clone()],
                    Some("k")
                )
                .unwrap(),
            2,
            "an empty batch did not burn the key"
        );
        assert_eq!(
            store.scan_text_scores(TEXT_KIND_NEWS, FINBERT, TimestampRange::all()).unwrap(),
            vec![early.clone(), late],
            "received-ascending"
        );
        assert_eq!(
            store.text_scores_as_of(TEXT_KIND_NEWS, FINBERT, T0, MINUTE).unwrap(),
            vec![early]
        );
        let listed = store.list_series().unwrap();
        let found =
            listed.iter().find(|s| s.kind == vike_model::StoreKindId::TextScore).expect("listed");
        assert_eq!((found.venue.as_str(), found.symbol.as_str()), (TEXT_KIND_NEWS, FINBERT));
    }
}
