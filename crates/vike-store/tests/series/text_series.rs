//! `kind=text` HistoryStore series — news items and calendar events as the datahub received them.
//! Split into `datafusion_tests` / `memory_tests` like the other kinds.
//!
//! What carries weight here:
//!
//! * [`datafusion_tests::text_series_roundtrips_datafusion`]: every one of the nineteen columns comes
//!   back as written, the empty strings included (a news row's calendar fields) and the columns only a
//!   later kind fills (a filing-shaped row).
//! * [`datafusion_tests::the_point_in_time_read_selects_on_the_received_time`]: `texts_as_of` never
//!   returns a row received after the decision time, whatever its `published_ms` says — the property
//!   this kind exists for.
//! * [`datafusion_tests::run_maintenance_handles_text_series`]: the compaction dispatch's `text` arm
//!   at RUNTIME (`store_kind_gate.rs` reads it as text only; a missing arm is an `Ok` pass with a
//!   `failed` row, the measurement `cohort_series.rs` carries).

use vike_data::TextRow;
use vike_data::text_log::{TEXT_KIND_ECONOMIC_CALENDAR, TEXT_KIND_NEWS};

const T0: i64 = 1_791_786_600_000;
const MINUTE: i64 = 60_000;

/// One news item received at `received_ms`, published an hour before.
fn news(received_ms: i64, url: &str) -> TextRow {
    TextRow {
        published_ms: received_ms - 60 * MINUTE,
        title: format!("headline {url}"),
        summary: "Prices rose \"5%\" — today".to_string(),
        url: url.to_string(),
        market: "crypto".to_string(),
        tags: TextRow::joined_tags(&["Markets", "Crypto & DeFi"]),
        ..TextRow::new(TEXT_KIND_NEWS, "CoinDesk", received_ms)
    }
}

/// One calendar event received at `received_ms`, scheduled at `T0`, with `actual`.
fn event(received_ms: i64, actual: &str) -> TextRow {
    TextRow {
        published_ms: T0,
        title: "Non-Farm Employment Change".to_string(),
        currency: "USD".to_string(),
        impact: "High".to_string(),
        forecast: "150K".to_string(),
        previous: "142K".to_string(),
        actual: actual.to_string(),
        ..TextRow::new(TEXT_KIND_ECONOMIC_CALENDAR, "ForexFactory", received_ms)
    }
}

/// A filing-shaped row: every column a later kind fills, so the round trip carries all nineteen.
fn filing(received_ms: i64) -> TextRow {
    TextRow {
        published_ms: received_ms - MINUTE,
        source_id: "0000320193-26-000077".to_string(),
        subject: "AAPL".to_string(),
        period: "FY2026".to_string(),
        title: "Annual report".to_string(),
        body: "Item 1. Business\nItem 7. Results".to_string(),
        amends: "0000320193-26-000001".to_string(),
        ..TextRow::new("sec_10k", "EDGAR", received_ms)
    }
}

mod datafusion_tests {
    use super::{MINUTE, T0, event, filing, news};
    use vike_data::text_log::{TEXT_KIND_ECONOMIC_CALENDAR, TEXT_KIND_NEWS};
    use vike_data::{HistoryStore, TextRow, TimestampRange};
    use vike_store::{CompactionConfig, HistoryStoreDataFusion, MaintenanceConfig};

    #[test]
    fn text_series_roundtrips_datafusion() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        let rows = vec![news(T0, "https://example.test/a"), news(T0, "https://example.test/b")];
        assert_eq!(store.append_texts(TEXT_KIND_NEWS, "rss", &rows, Some("k1")).unwrap(), 2);

        let got = store.scan_texts(TEXT_KIND_NEWS, "rss", TimestampRange::all()).unwrap();
        assert_eq!(got, rows, "every column back as written, in the written order");
        assert_eq!(got[0].tag_list().collect::<Vec<_>>(), ["Markets", "Crypto & DeFi"]);
        assert!(
            store
                .scan_texts(TEXT_KIND_ECONOMIC_CALENDAR, "rss", TimestampRange::all())
                .unwrap()
                .is_empty(),
            "another text kind is another series"
        );
        assert_eq!(
            store.append_texts(TEXT_KIND_NEWS, "rss", &rows, Some("k1")).unwrap(),
            0,
            "the same key twice is a no-op"
        );

        // The bounded read the datahub serves: a COMPLETE prefix of at least the budget's rows.
        let later = news(T0 + MINUTE, "https://example.test/c");
        store
            .append_texts(TEXT_KIND_NEWS, "rss", std::slice::from_ref(&later), Some("k2"))
            .unwrap();
        let all = store.scan_texts(TEXT_KIND_NEWS, "rss", TimestampRange::all()).unwrap();
        let head =
            store.scan_texts_capped(TEXT_KIND_NEWS, "rss", TimestampRange::all(), Some(1)).unwrap();
        assert!(!head.is_empty() && head[..] == all[..head.len()], "a prefix: {head:?}");

        // A later kind's columns (the source's id, subject, period, body, amends) come back too.
        let filings = vec![filing(T0)];
        store.append_texts("sec_10k", "edgar", &filings, Some("f1")).unwrap();
        assert_eq!(store.scan_texts("sec_10k", "edgar", TimestampRange::all()).unwrap(), filings);
    }

    /// ⚠ THE property this kind exists for: what a decision at `t` could have read. A row received
    /// after `t` is never returned, even though it was PUBLISHED long before `t`; a row received at
    /// `t` exactly is.
    #[test]
    fn the_point_in_time_read_selects_on_the_received_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        let early = news(T0, "https://example.test/early");
        let late = news(T0 + 10 * MINUTE, "https://example.test/late");
        store.append_texts(TEXT_KIND_NEWS, "rss", std::slice::from_ref(&early), Some("a")).unwrap();
        store.append_texts(TEXT_KIND_NEWS, "rss", std::slice::from_ref(&late), Some("b")).unwrap();
        assert!(late.published_ms < T0 + MINUTE, "published before the decision below");

        let at = |t: i64| store.texts_as_of(TEXT_KIND_NEWS, "rss", t, 60 * MINUTE).unwrap();
        assert_eq!(at(T0 + MINUTE), vec![early.clone()], "the late item was not received yet");
        assert_eq!(at(T0 + 10 * MINUTE), vec![early.clone(), late.clone()], "received at t: in");
        assert!(at(T0 - 1).is_empty(), "before anything was received");
        assert_eq!(
            store.texts_as_of(TEXT_KIND_NEWS, "rss", T0 + 10 * MINUTE, 0).unwrap(),
            vec![late],
            "a zero lookback is the instant only"
        );
    }

    /// A calendar revision is a row of its own: before the release the forecast is what was known,
    /// after it the actual. The last row per text id at a decision time is the version known then.
    #[test]
    fn a_calendar_revision_is_its_own_row_and_the_last_one_is_the_known_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        let before = event(T0 - 60 * MINUTE, "");
        let released = event(T0 + MINUTE, "175K");
        let k = TEXT_KIND_ECONOMIC_CALENDAR;
        store.append_texts(k, "forexfactory", std::slice::from_ref(&before), Some("x")).unwrap();
        store.append_texts(k, "forexfactory", std::slice::from_ref(&released), Some("y")).unwrap();

        let known_at = |t: i64| -> Option<TextRow> {
            store.texts_as_of(k, "forexfactory", t, 24 * 60 * MINUTE).unwrap().into_iter().last()
        };
        assert_eq!(known_at(T0).map(|r| r.actual), Some(String::new()), "not released yet");
        assert_eq!(known_at(T0 + MINUTE).map(|r| r.actual), Some("175K".to_string()));
        assert_eq!(before.text_id(), released.text_id(), "one event, two versions");
    }

    /// The compaction dispatch's `text` arm, at runtime. `min_parts: 1` forces the fragment through.
    #[test]
    fn run_maintenance_handles_text_series() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        let rows = vec![news(T0, "https://example.test/a"), news(T0 + MINUTE, "https://x.test/b")];
        store.append_texts(TEXT_KIND_NEWS, "rss", &rows, Some("kt")).unwrap();

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
        assert!(report.failed.is_empty(), "the text series was SKIPPED: {:?}", report.failed);
        assert_eq!(report.compaction.parts_written, 1, "the text series was actually compacted");
        assert_eq!(store.scan_texts(TEXT_KIND_NEWS, "rss", TimestampRange::all()).unwrap(), rows);
    }
}

mod memory_tests {
    use super::{MINUTE, T0, news};
    use vike_data::text_log::TEXT_KIND_NEWS;
    use vike_data::{HistoryStore, HistoryStoreMemory, TimestampRange};

    #[test]
    fn text_series_roundtrips_memory_and_reads_point_in_time() {
        let store = HistoryStoreMemory::new();
        let early = news(T0, "https://example.test/a");
        let late = news(T0 + 5 * MINUTE, "https://example.test/b");
        assert_eq!(store.append_texts(TEXT_KIND_NEWS, "rss", &[], Some("k")).unwrap(), 0);
        assert_eq!(
            store
                .append_texts(TEXT_KIND_NEWS, "rss", &[late.clone(), early.clone()], Some("k"))
                .unwrap(),
            2,
            "an empty batch did not burn the key"
        );
        assert_eq!(
            store.scan_texts(TEXT_KIND_NEWS, "rss", TimestampRange::all()).unwrap(),
            vec![early.clone(), late],
            "received-ascending"
        );
        assert_eq!(store.texts_as_of(TEXT_KIND_NEWS, "rss", T0, MINUTE).unwrap(), vec![early]);
        let listed = store.list_series().unwrap();
        let found =
            listed.iter().find(|s| s.kind == vike_model::StoreKindId::Text).expect("listed");
        assert_eq!((found.venue.as_str(), found.symbol.as_str()), (TEXT_KIND_NEWS, "rss"));
    }
}
