use super::*;

const DAY: i64 = 1_791_763_200_000; // 2026-10-12T00:00:00Z
const HOUR: i64 = 3_600_000;

fn news(received_ms: i64, title: &str, summary: &str) -> TextRow {
    TextRow {
        title: title.to_string(),
        summary: summary.to_string(),
        url: format!("https://a.test/{received_ms}"),
        ..TextRow::new(vike_data::text_log::TEXT_KIND_NEWS, "CoinDesk", received_ms)
    }
}

fn scratch(tag: &str) -> vike_paths::scratch::ScratchDir {
    vike_paths::scratch::ScratchDir::create_in(&std::env::temp_dir(), &format!("vike_nlp_{tag}"))
        .expect("scratch dir")
}

#[test]
fn a_news_item_is_scored_as_its_title_then_its_summary_one_chunk() {
    let texts = [news(DAY, "Stocks rallied", ""), news(DAY, "Stocks rallied", "The pound gained.")];
    let mut asked = Vec::new();
    let rows = score_rows(&texts.iter().collect::<Vec<_>>(), "m@0", 7, |text| {
        asked.push(text.to_string());
        Ok(vec![1.0])
    })
    .unwrap();
    assert_eq!(asked, ["Stocks rallied", "Stocks rallied\nThe pound gained."]);
    assert_eq!(rows.iter().map(|r| r.chunk).collect::<Vec<_>>(), [0, 0]);
}

#[test]
fn a_long_text_is_scored_chunk_by_chunk_under_one_text_id() {
    let body: Vec<String> = (0..300).map(|i| format!("w{i}")).collect();
    let text = TextRow { body: body.join(" "), ..news(DAY, "Item 7", "") };
    let mut asked = Vec::new();
    let rows = score_rows(&[&text], "m@0", 7, |words| {
        asked.push(words.to_string());
        Ok(vec![asked.len() as f64])
    })
    .unwrap();
    assert_eq!(asked, vike_rag::chunk::chunk_texts("Item 7", "", &text.body), "the one rule");
    assert_eq!(rows.iter().map(|r| r.chunk).collect::<Vec<_>>(), [0, 1, 2]);
    assert!(rows.iter().all(|r| r.text_id == text.text_id() && r.received_ms == DAY));
    assert_eq!(rows[2].value_list().unwrap(), [3.0]);
}

#[test]
fn texts_group_by_their_received_day_and_each_row_names_its_text_and_model() {
    let texts = vec![
        news(DAY + HOUR, "a", ""),
        news(DAY + 25 * HOUR, "b", ""),
        news(DAY + 2 * HOUR, "c", ""),
    ];
    let days = texts_by_day(&texts);
    assert_eq!(days.keys().copied().collect::<Vec<_>>(), [DAY, DAY + 24 * HOUR]);
    let first = &days[&DAY];
    assert_eq!(first.len(), 2);

    let mut asked = Vec::new();
    let rows = score_rows(first, "finbert@abc", 7, |text| {
        asked.push(text.to_string());
        Ok(vec![0.5, 0.25, 0.25])
    })
    .unwrap();
    assert_eq!(asked, ["a", "c"], "one text per call, in order");
    assert_eq!(rows[0].received_ms, DAY + HOUR);
    assert_eq!(rows[0].text_id, texts[0].text_id());
    assert_eq!((rows[1].scored_ms, rows[1].model.as_str()), (7, "finbert@abc"));
    assert_eq!(rows[1].value_list().unwrap(), [0.5, 0.25, 0.25]);
}

#[test]
fn a_days_file_is_json_lines_the_import_reads_and_is_written_once() {
    let dir = scratch("day_file");
    let models = dir.path().join("finbert");
    let texts = [news(DAY + HOUR, "a", "")];
    let rows =
        score_rows(&texts.iter().collect::<Vec<_>>(), "finbert@abc", 7, |_| Ok(vec![1.0])).unwrap();
    assert!(write_day_file(&models, DAY, &rows).unwrap(), "written");
    let path = day_file(&models, DAY);
    assert_eq!(path.file_name().unwrap(), "2026-10-12.jsonl");
    let text = std::fs::read_to_string(&path).unwrap();
    let back: Vec<TextScoreRow> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(back, rows);
    assert!(!write_day_file(&models, DAY, &[]).unwrap(), "a day already written is kept");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
}
