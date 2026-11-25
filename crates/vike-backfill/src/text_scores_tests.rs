use super::*;

const DAY: i64 = 1_791_763_200_000; // 2026-10-12T00:00:00Z

fn row(received_ms: i64, model: &str) -> TextScoreRow {
    TextScoreRow {
        received_ms,
        text_id: format!("news\u{1f}CoinDesk\u{1f}https://a.test/{received_ms}"),
        chunk: 0,
        scored_ms: DAY + 2 * MS_PER_DAY,
        model: model.to_string(),
        values: TextScoreRow::joined_values(&[0.5, 0.25, 0.25]),
    }
}

fn file(rows: &[TextScoreRow]) -> String {
    rows.iter().map(|r| serde_json::to_string(r).unwrap()).collect::<Vec<_>>().join("\n")
}

#[test]
fn a_day_file_decodes_to_its_rows_and_a_bad_line_is_named() {
    let lane = ArchiveLaneTextScores::new("news", "finbert");
    let rows = vec![row(DAY + 1, "finbert@abc"), row(DAY + 2, "finbert@abc")];
    assert_eq!(lane.decode(&format!("{}\n\n", file(&rows))).unwrap(), rows, "blank lines skipped");

    let other_model = lane.decode(&file(&[row(DAY, "all-minilm-l6-v2@x")])).unwrap_err();
    assert!(other_model.contains("line 1") && other_model.contains("all-minilm"), "{other_model}");
    let mut nameless = row(DAY, "finbert@abc");
    nameless.text_id.clear();
    assert!(lane.decode(&file(&[nameless])).unwrap_err().contains("no text"));
    let mut garbled = row(DAY, "finbert@abc");
    garbled.values = "0.5 x".to_string();
    assert!(
        lane.decode(&file(&[row(DAY, "finbert@abc"), garbled])).unwrap_err().contains("line 2")
    );
    assert!(lane.decode("{not json").unwrap_err().contains("line 1"));
}

#[test]
fn the_lanes_day_keys_hold_their_days_and_any_other_key_meets_every_day() {
    use crate::archive_import::ArchiveDayClass;
    let lane = ArchiveLaneTextScores::new("news", "finbert");
    assert_eq!(lane.key(DAY), "text-scores:news:finbert:2026-10-12");
    let now = DAY + 10 * MS_PER_DAY;
    let own = lane.owners(&[lane.key(DAY)]);
    assert!(matches!(own.classify(DAY, now), ArchiveDayClass::HeldByArchive), "its own day");
    assert!(matches!(own.classify(DAY + MS_PER_DAY, now), ArchiveDayClass::Free), "the next day");
    assert!(matches!(own.classify(now, now), ArchiveDayClass::TooRecent), "a day not yet ended");
    // A key it cannot read as a day (a bare number) meets every day.
    let unread = lane.owners(&["text-scores:news:finbert:1234567890".to_string()]);
    assert!(matches!(unread.classify(DAY, now), ArchiveDayClass::Overlapped { .. }));
    assert_eq!(lane.series().kind, StoreKindId::TextScore);
}
