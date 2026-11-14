//! The `csv-bars` format through the lane's own verb, over a real store: a user's file lands in its
//! lane and nowhere else, and a file, a label or a target the door refuses writes nothing.

use std::sync::Arc;

use vike_data::csv_bars::CsvBarsFormatWords;
use vike_data::{HistoryStore, TimestampRange};
use vike_datahub_client::archive::{ArchiveDayResultWire, ImportDone, ImportTarget};
use vike_datahub_client::proto::Response;
use vike_store::HistoryStoreDataFusion;

use super::*;
use crate::import::{ImportLane, import_archive_verb};

/// 2024-01-15, a UTC midnight.
const DAY: i64 = 19_737 * MS_PER_DAY;
const HOUR: i64 = 3_600_000;

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

    fn put(&self, name: &str, text: &str) {
        let dir = self.tmp.path().join("imports").join(SOURCE).join("MYFILES");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), text).unwrap();
    }

    fn answer(&self, spec: &ImportSpec) -> Response {
        let root = self.tmp.path().join("imports");
        let lane = ImportLane::new(
            root,
            super::super::real_import_registry(Arc::clone(&self.store), None),
        );
        import_archive_verb(spec, Some(&lane), &|| false)
    }

    fn run(&self, spec: &ImportSpec) -> ImportDone {
        match self.answer(spec) {
            Response::ArchiveImported(done) => *done,
            other => panic!("not an import's answer: {other:?}"),
        }
    }

    fn refusal(&self, spec: &ImportSpec) -> String {
        match self.answer(spec) {
            Response::Error(why) => why,
            other => panic!("not refused: {other:?}"),
        }
    }

    fn laned(&self, label: &str) -> Vec<Bar> {
        let lane = vike_data::store::store_kind::user_lane(label).unwrap();
        let range = TimestampRange::of(DAY - MS_PER_DAY, DAY + 3 * MS_PER_DAY);
        self.store.load_bars_sourced_capped("binance", &lane, "BTCUSDT", "1h", range, None).unwrap()
    }

    fn candles(&self) -> Vec<Bar> {
        let range = TimestampRange::of(DAY - MS_PER_DAY, DAY + 3 * MS_PER_DAY);
        self.store.load_bars("binance", "BTCUSDT", "1h", range).unwrap()
    }
}

/// `hours` one-hour bars from `start`, in milliseconds, under a header.
fn csv(start: i64, hours: i64) -> String {
    let rows: String = (0..hours).map(|h| format!("{},10,20,5,15,2\n", start + h * HOUR)).collect();
    format!("time,open,high,low,close,volume\n{rows}")
}

fn target(label: &str) -> ImportTarget {
    ImportTarget {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        interval: "1h".to_string(),
        label: label.to_string(),
        file_format: CsvBarsFormatWords::default(),
        columns: None,
    }
}

/// Excel with Russian settings, Moscow's wall clock: 03:00 there is 00:00 UTC on 2024-01-15.
const EXCEL_RU: &str = "time;open;high;low;close;volume\n\
                        15.01.2024 03:00;1 234,5;1 240;1 230,25;1 238,75;12,5\n\
                        15.01.2024 04:00;1 238,75;1 250;1 238;1 249,5;7\n";

/// The words that read [`EXCEL_RU`].
fn excel_ru() -> CsvBarsFormatWords {
    CsvBarsFormatWords {
        delimiter: Some(";".to_string()),
        decimal: Some(",".to_string()),
        thousands: Some("space".to_string()),
        time: Some("DD.MM.YYYY HH:mm".to_string()),
        tz: Some("Europe/Moscow".to_string()),
    }
}

/// A regional export imports with its format words, carried by the request to the datahub's decoder,
/// and the plan shows its first rows as read, in UTC; without the words it is refused, the decoder
/// naming the option to set, and nothing is written.
#[test]
fn a_regional_file_imports_with_its_format_words_and_the_plan_shows_it_in_utc() {
    let project = Project::new();
    project.put("excel.csv", EXCEL_RU);
    let why = project.refusal(&spec(true, Some(target("ru"))));
    assert!(why.contains("excel.csv") && why.contains("line"), "{why}");
    let regional = ImportTarget { file_format: excel_ru(), ..target("ru") };
    let plan = project.run(&spec(true, Some(regional.clone()))).plan;
    let read: Vec<(i64, &str, &str)> =
        plan.preview.iter().map(|r| (r.timestamp, r.open.as_str(), r.volume.as_str())).collect();
    assert_eq!(read, vec![(DAY, "1234.5", "12.5"), (DAY + HOUR, "1238.75", "7")]);
    let done = project.run(&spec(false, Some(regional)));
    assert!(done.outcome.is_some());
    let bars = project.laned("ru");
    assert_eq!(
        bars.iter().map(|b| (b.timestamp, b.close)).collect::<Vec<_>>(),
        vec![(DAY, 1238.75), (DAY + HOUR, 1249.5)]
    );
}

fn spec(dry_run: bool, target: Option<ImportTarget>) -> ImportSpec {
    ImportSpec {
        format: SOURCE.to_string(),
        dataset: "MYFILES".to_string(),
        from_day: None,
        to_day: None,
        bars: Vec::new(),
        dry_run,
        verify: false,
        replace_holed: false,
        target,
    }
}

/// Two days of a file land in the lane, under the venue and symbol the operator named, and the
/// venue's own series stays empty.
#[test]
fn a_users_file_lands_in_its_lane_and_never_among_the_candles() {
    let project = Project::new();
    project.put("btc.csv", &csv(DAY, 48));
    let plan = project.run(&spec(true, Some(target("my-file")))).plan;
    assert_eq!((plan.venue.as_str(), plan.days.len()), ("binance", 2), "{plan:?}");
    let first: Vec<(i64, &str, &str)> =
        plan.preview.iter().map(|b| (b.timestamp, b.close.as_str(), b.volume.as_str())).collect();
    assert_eq!(first, vec![(DAY, "15", "2"), (DAY + HOUR, "15", "2"), (DAY + 2 * HOUR, "15", "2")]);
    let done = project.run(&spec(false, Some(target("my-file"))));
    for day in [DAY, DAY + MS_PER_DAY] {
        let outcome = done.outcome.as_ref().unwrap().days.iter().find(|d| d.day == day).unwrap();
        assert!(
            matches!(outcome.result, ArchiveDayResultWire::Imported { ticks: 24, .. }),
            "{done:?}"
        );
    }
    assert_eq!(project.laned("my-file").len(), 48);
    assert!(project.candles().is_empty(), "the venue's series is never written");
}

/// A file the decoder refuses refuses the request with its line, and nothing is written.
#[test]
fn a_refused_file_writes_nothing() {
    let project = Project::new();
    let bad = csv(DAY, 3).replacen(",10,20,5,15,2\n", ",10,1,5,15,2\n", 1);
    project.put("btc.csv", &bad);
    let why = project.refusal(&spec(false, Some(target("my-file"))));
    assert!(why.contains("btc.csv") && why.contains("line 2"), "{why}");
    assert!(project.laned("my-file").is_empty() && project.candles().is_empty());
}

/// A label the lane grammar refuses, a request with no target, a target sent to a format that names
/// its own series, and two files holding one day: each refused, and nothing written.
#[test]
fn a_bad_label_a_missing_target_and_two_files_of_one_day_are_refused() {
    let project = Project::new();
    project.put("btc.csv", &csv(DAY, 24));
    let why = project.refusal(&spec(false, Some(target("My File"))));
    assert!(why.contains("label"), "{why}");
    let why = project.refusal(&spec(false, None));
    assert!(why.contains("--label"), "{why}");
    let klines = ImportSpec {
        format: "binance-klines".to_string(),
        bars: vec!["1h".to_string()],
        ..spec(false, Some(target("my-file")))
    };
    assert!(project.refusal(&klines).contains("takes no venue"));
    project.put("again.csv", &csv(DAY + 12 * HOUR, 3));
    let why = project.refusal(&spec(false, Some(target("my-file"))));
    assert!(why.contains("both hold bars"), "{why}");
    assert!(project.laned("my-file").is_empty() && project.candles().is_empty());
}

// ---- the upload (decision 0272): the same file SENT, over the same lane --------------------------

mod upload {
    use vike_datahub_client::upload::{CsvUploadDone, CsvUploadSpec};

    use super::*;
    use crate::import::upload::begin;

    /// Long after the test days: none of them is too recent.
    const LATER: i64 = DAY + 400 * MS_PER_DAY;

    /// The real registry over the project's store, its upload armed.
    fn armed_lane(project: &Project) -> ImportLane {
        let formats = super::super::super::real_import_registry(Arc::clone(&project.store), None);
        ImportLane::new(project.tmp.path().join("imports"), formats).with_upload_armed(true)
    }

    /// One upload of `text` through the door, the session and the end, judged at `now`.
    fn send_at(
        project: &Project,
        target: ImportTarget,
        text: &str,
        head: bool,
        dry_run: bool,
        now: i64,
    ) -> Response {
        let lane = armed_lane(project);
        let spec = CsvUploadSpec { target, bytes: text.len() as u64, dry_run, head };
        match begin(Some(&lane), spec) {
            Err(why) => Response::Error(why),
            Ok(mut session) => {
                for piece in vike_datahub_client::upload::chunks(text) {
                    session.push(piece).expect("within the declared size");
                }
                session.end(now)
            }
        }
    }

    fn send(project: &Project, target: ImportTarget, text: &str, dry_run: bool) -> Response {
        send_at(project, target, text, false, dry_run, LATER)
    }

    fn done(response: Response) -> CsvUploadDone {
        match response {
            Response::CsvUploaded(done) => *done,
            other => panic!("not an upload's answer: {other:?}"),
        }
    }

    fn refused(response: Response) -> String {
        match response {
            Response::Error(why) => why,
            other => panic!("not refused: {other:?}"),
        }
    }

    /// A sent file lands in its lane and nowhere else; a dry run of it stores nothing first.
    #[test]
    fn a_sent_file_lands_in_its_lane_and_a_dry_run_stores_nothing() {
        let project = Project::new();
        let plan = done(send(&project, target("sent"), &csv(DAY, 48), true));
        assert_eq!((plan.bars, plan.days, plan.stored_days), (48, 2, 0), "{plan:?}");
        assert_eq!((plan.first_day, plan.last_day), (Some(DAY), Some(DAY + MS_PER_DAY)));
        assert_eq!(plan.header, ["time", "open", "high", "low", "close", "volume"]);
        assert!(project.laned("sent").is_empty(), "a dry run writes nothing");
        let stored = done(send(&project, target("sent"), &csv(DAY, 48), false));
        assert_eq!((stored.stored_days, stored.stored_bars), (2, 48), "{stored:?}");
        assert!(stored.not_stored.is_empty());
        assert_eq!(project.laned("sent").len(), 48);
        assert!(project.candles().is_empty(), "the venue's series is never written");
    }

    /// The same series again, whoever wrote it, is refused before a byte is read: an upload creates a
    /// series and never adds to or replaces one. Another label is a new series.
    #[test]
    fn a_series_that_holds_rows_is_refused_and_another_label_is_not() {
        let project = Project::new();
        done(send(&project, target("once"), &csv(DAY, 24), false));
        let why = refused(send(&project, target("once"), &csv(DAY + MS_PER_DAY, 24), false));
        assert!(
            why.contains("already holds 24 bars") && why.contains("--source user-once"),
            "{why}"
        );
        assert!(why.contains("new label"), "{why}");
        assert_eq!(project.laned("once").len(), 24, "the lane is unchanged");
        let again = done(send(&project, target("twice"), &csv(DAY + MS_PER_DAY, 24), false));
        assert_eq!(again.stored_days, 1);
    }

    /// A file the decoder refuses is answered with the line and the rule AND the header, so a client
    /// can offer its columns; nothing is stored. Mapped, one value per row, it lands under `user`.
    #[test]
    fn a_refused_file_answers_its_header_and_a_mapping_reads_it() {
        let project = Project::new();
        let text = format!("timestamp_ms,value\n{DAY},71\n{},64\n", DAY + MS_PER_DAY);
        let daily = || ImportTarget { interval: "1d".to_string(), ..target("index") };
        let plan = done(send(&project, daily(), &text, false));
        assert!(plan.refused.as_deref().is_some_and(|r| r.contains("no time column")), "{plan:?}");
        assert_eq!(plan.header, ["timestamp_ms", "value"]);
        assert_eq!(plan.stored_days, 0);
        let mapped = ImportTarget {
            venue: "user".to_string(),
            columns: Some("time=timestamp_ms,close=value".to_string()),
            ..daily()
        };
        let stored = done(send(&project, mapped, &text, false));
        assert_eq!(stored.stored_days, 2, "{stored:?}");
        let lane = vike_data::store::store_kind::user_lane("index").unwrap();
        let bars = project
            .store
            .load_bars_sourced_capped("user", &lane, "BTCUSDT", "1d", TimestampRange::all(), None)
            .unwrap();
        let first = (bars[0].open, bars[0].high, bars[0].close, bars[0].volume);
        assert_eq!((bars.len(), first), (2, (71.0, 71.0, 71.0, 0.0)));
    }

    /// A day that has not ended is not stored, and says why; the days before it are.
    #[test]
    fn a_day_that_has_not_ended_is_not_stored() {
        let project = Project::new();
        // "Now" is noon of the second day: the first day has ended, the second has not.
        let noon = DAY + MS_PER_DAY + 12 * HOUR;
        let stored = done(send_at(&project, target("today"), &csv(DAY, 36), false, false, noon));
        assert_eq!(stored.stored_days, 1, "{stored:?}");
        assert_eq!(stored.not_stored.len(), 1);
        assert_eq!(stored.not_stored[0].day, DAY + MS_PER_DAY);
        assert!(
            matches!(&stored.not_stored[0].result, ArchiveDayResultWire::Refused(r) if r.class == "TooRecent")
        );
        assert_eq!(project.laned("today").len(), 24);
    }

    /// A preview's head is read up to its last whole line, and stores nothing.
    #[test]
    fn a_head_is_read_to_its_last_whole_line() {
        let project = Project::new();
        let whole = csv(DAY, 5);
        let head = &whole[..whole.len() - 7];
        let plan = done(send_at(&project, target("peek"), head, true, true, LATER));
        assert_eq!((plan.bars, plan.refused.as_deref()), (4, None), "{plan:?}");
        assert_eq!(plan.preview.len(), 3);
        assert!(project.laned("peek").is_empty());
    }
}
