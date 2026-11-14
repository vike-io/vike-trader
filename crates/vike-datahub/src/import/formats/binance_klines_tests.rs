//! The `binance-klines` format through the lane's own verb, over a real store and real files: the
//! recorded vendor zips equal the REST lane's reading of the same days, the checksum refusals, the
//! interval folder, the monthly zip, and the replacement of a holed day.

use std::path::PathBuf;
use std::sync::Arc;

use vike_backfill::testing::zip_of;
use vike_data::{HistoryStore, TimestampRange};
use vike_datahub_client::archive::{
    ArchiveDayClassWire, ArchiveDayResultWire, ImportDone, ImportSpec,
};
use vike_datahub_client::proto::Response;
use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_store::HistoryStoreDataFusion;

use super::*;
use crate::import::{COMPANION_MISSING, ImportLane, import_archive_verb};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/binance_klines");
/// 2025-06-01: the recorded SPOT day, whose file writes its open times in microseconds.
const SPOT_DAY: i64 = 1_748_736_000_000;
/// 2024-01-01: the recorded USDⓈ-M day, in milliseconds, with the futures header line.
const UM_DAY: i64 = 1_704_067_200_000;
const HOUR: i64 = 3_600_000;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURES}/{name}")).expect("a recorded fixture")
}

/// What the REST lane stores for the recorded day: the bridge's own reading of its response.
fn rest(name: &str) -> Vec<Bar> {
    let body = String::from_utf8(fixture(name)).expect("utf-8");
    vike_binance::family::history::klines::parse_klines(&body).expect("a REST klines body")
}

/// A project in a temporary directory: an imports root, and a real store beside it.
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

    fn folder(&self, dataset: &str) -> PathBuf {
        let dir = self.tmp.path().join("imports").join(SOURCE).join(dataset).join("1h");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn put(&self, dataset: &str, name: &str, bytes: &[u8]) {
        std::fs::write(self.folder(dataset).join(name), bytes).unwrap();
    }

    /// A zip and the `.CHECKSUM` the vendor publishes beside it.
    fn put_checked(&self, dataset: &str, name: &str, zip: &[u8]) {
        self.put(dataset, name, zip);
        let line = format!("{}  {name}", vike_hub_protocol::hash::sha256_hex(zip));
        self.put(dataset, &format!("{name}.CHECKSUM"), line.as_bytes());
    }

    fn answer(&self, spec: &ImportSpec) -> Response {
        let root = self.tmp.path().join("imports");
        let lane =
            ImportLane::new(root, vec![Box::new(BinanceKlines::new(Arc::clone(&self.store)))]);
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

    fn bars(&self, dataset: &str, day: i64) -> Vec<Bar> {
        let range = TimestampRange::of(day, day + MS_PER_DAY - 1);
        self.store.load_bars(VENUE, dataset, "1h", range).expect("load")
    }

    /// The REST lane's day: `bars` under its window key over exactly the day.
    fn rest_day(&self, dataset: &str, day: i64, bars: &[Bar]) {
        let key = format!("{VENUE}:{dataset}:1h:{day}-{}", day + MS_PER_DAY - 1);
        self.store.append_bars(VENUE, dataset, "1h", bars, Some(&key)).expect("append");
    }
}

fn spec(dataset: &str, day: i64, dry_run: bool, replace_holed: bool) -> ImportSpec {
    ImportSpec {
        format: SOURCE.to_string(),
        dataset: dataset.to_string(),
        from_day: Some(day),
        to_day: Some(day),
        bars: vec!["1h".to_string()],
        dry_run,
        verify: false,
        replace_holed,
        target: None,
    }
}

fn result(done: &ImportDone, day: i64) -> &ArchiveDayResultWire {
    let outcome = done.outcome.as_ref().expect("an outcome");
    &outcome.days.iter().find(|d| d.day == day).expect("the day").result
}

fn class(done: &ImportDone, day: i64) -> &ArchiveDayClassWire {
    &done.plan.days.iter().find(|d| d.day == day).expect("the day").class
}

/// **A `binance-klines` day equals the same day read from Binance's REST klines**, for a spot file in
/// microseconds and a USDⓈ-M file in milliseconds with its header line: recorded files, no network.
#[test]
fn a_binance_klines_day_equals_the_same_day_read_from_the_rest_klines() {
    let project = Project::new();
    for (dataset, zip, response, day) in [
        ("BTCUSDT", "BTCUSDT-1h-2025-06-01.zip", "rest-spot-BTCUSDT-1h-2025-06-01.json", SPOT_DAY),
        ("BTCUSDT.P", "BTCUSDT-1h-2024-01-01.zip", "rest-um-BTCUSDT-1h-2024-01-01.json", UM_DAY),
    ] {
        project.put(dataset, zip, &fixture(zip));
        let checksum = format!("{zip}.CHECKSUM");
        project.put(dataset, &checksum, &fixture(&checksum));
        let done = project.run(&spec(dataset, day, false, false));
        assert!(done.plan.server_dir.ends_with("1h"), "{}", done.plan.server_dir);
        assert!(
            matches!(result(&done, day), ArchiveDayResultWire::Imported { ticks: 24, .. }),
            "{dataset}: {done:?}"
        );
        assert_eq!(project.bars(dataset, day), rest(response), "{dataset}");
    }
}

/// A zip with no `.CHECKSUM`, or one that does not vouch for it, is refused and writes nothing.
#[test]
fn a_zip_without_its_checksum_or_with_a_wrong_one_is_refused() {
    let zip = fixture("BTCUSDT-1h-2025-06-01.zip");
    let name = "BTCUSDT-1h-2025-06-01.zip";
    let good = vike_hub_protocol::hash::sha256_hex(&zip);
    let flipped = if good.ends_with('0') { '1' } else { '0' };
    let wrong = format!("{}{flipped}", &good[..63]);
    for (checksum, want) in [
        (None, COMPANION_MISSING),
        (Some(format!("{wrong}  {name}")), CHECKSUM_MISMATCH),
        (Some(format!("{good}  BTCUSDT-1h-2025-06-02.zip")), CHECKSUM_FOREIGN),
        (Some("not a checksum".to_string()), CHECKSUM_UNREADABLE),
    ] {
        let project = Project::new();
        project.put("BTCUSDT", name, &zip);
        if let Some(line) = &checksum {
            project.put("BTCUSDT", &format!("{name}.CHECKSUM"), line.as_bytes());
        }
        let done = project.run(&spec("BTCUSDT", SPOT_DAY, false, false));
        let ArchiveDayResultWire::Refused(refusal) = result(&done, SPOT_DAY) else {
            panic!("{checksum:?} must refuse the day: {done:?}");
        };
        assert_eq!(refusal.class, want, "{checksum:?}");
        assert!(project.bars("BTCUSDT", SPOT_DAY).is_empty(), "{checksum:?} wrote nothing");
    }
    // The missing one is named at plan time, before anything is read.
    let project = Project::new();
    project.put("BTCUSDT", name, &zip);
    let plan = project.run(&spec("BTCUSDT", SPOT_DAY, true, false));
    assert!(
        matches!(class(&plan, SPOT_DAY), ArchiveDayClassWire::Refused(r) if r.class == COMPANION_MISSING)
    );
}

/// One interval per request, a store spelling the format admits, and nothing else.
#[test]
fn a_request_names_one_interval_and_a_spot_or_perpetual_dataset() {
    let project = Project::new();
    for bars in [vec![], vec!["1h".to_string(), "1m".to_string()]] {
        let why = project.refusal(&ImportSpec { bars, ..spec("BTCUSDT", SPOT_DAY, true, false) });
        assert!(why.contains("ONE interval"), "{why}");
    }
    let why = project.refusal(&spec("BTCUSDT_240329", SPOT_DAY, true, false));
    assert!(why.contains("USDⓈ-M perpetual"), "{why}");
    let done = project.run(&spec("BTCUSDT.P", SPOT_DAY, true, false));
    assert!(done.plan.admission.contains("USDⓈ-M perpetual"), "{}", done.plan.admission);
}

/// A monthly zip fills every day of its month no daily zip covers, and a day with both reads the
/// daily one.
#[test]
fn a_monthly_zip_fills_the_days_no_daily_zip_covers() {
    let project = Project::new();
    let row = |day: i64, h: i64, close: &str| {
        let open = day + h * HOUR;
        format!("{open},10,20,1,{close},3,{},0,1,0,0,0\n", open + HOUR - 1)
    };
    let month: String = (0..3)
        .flat_map(|d| (0..24).map(move |h| (d, h)))
        .map(|(d, h)| row(UM_DAY + d * MS_PER_DAY, h, "5"))
        .collect();
    project.put_checked(
        "BTCUSDT.P",
        "BTCUSDT-1h-2024-01.zip",
        &zip_of("BTCUSDT-1h-2024-01.csv", month.as_bytes()),
    );
    let second = UM_DAY + MS_PER_DAY;
    let daily: String = (0..24).map(|h| row(second, h, "6")).collect();
    project.put_checked(
        "BTCUSDT.P",
        "BTCUSDT-1h-2024-01-02.zip",
        &zip_of("BTCUSDT-1h-2024-01-02.csv", daily.as_bytes()),
    );
    let window = ImportSpec {
        to_day: Some(UM_DAY + 2 * MS_PER_DAY),
        ..spec("BTCUSDT.P", UM_DAY, false, false)
    };
    let done = project.run(&window);
    for d in 0..3 {
        let day = UM_DAY + d * MS_PER_DAY;
        assert!(
            matches!(result(&done, day), ArchiveDayResultWire::Imported { ticks: 24, .. }),
            "{done:?}"
        );
        let close = if day == second { 6.0 } else { 5.0 };
        assert!(project.bars("BTCUSDT.P", day).iter().all(|b| b.close == close), "day {d}");
    }
}

/// **`--replace-holed`**: a plain import leaves a holed day another lane holds as it is; with the
/// option the plan lists it, the import replaces it whole with the verified file's day, and a whole
/// day stays as it was.
#[test]
fn a_holed_day_is_replaced_only_when_asked_and_a_whole_day_never() {
    let project = Project::new();
    for (dataset, zip) in
        [("BTCUSDT.P", "BTCUSDT-1h-2024-01-01.zip"), ("BTCUSDT", "BTCUSDT-1h-2025-06-01.zip")]
    {
        project.put(dataset, zip, &fixture(zip));
        let checksum = format!("{zip}.CHECKSUM");
        project.put(dataset, &checksum, &fixture(&checksum));
    }
    let moved =
        |bars: Vec<Bar>| bars.into_iter().map(|b| Bar { close: 1.0, ..b }).collect::<Vec<_>>();
    let holed: Vec<Bar> =
        moved(rest("rest-um-BTCUSDT-1h-2024-01-01.json")).into_iter().take(20).collect();
    project.rest_day("BTCUSDT.P", UM_DAY, &holed);
    let whole = moved(rest("rest-spot-BTCUSDT-1h-2025-06-01.json"));
    project.rest_day("BTCUSDT", SPOT_DAY, &whole);

    let plain = project.run(&spec("BTCUSDT.P", UM_DAY, true, false));
    assert_eq!(class(&plain, UM_DAY), &ArchiveDayClassWire::HeldByHttp);
    let done = project.run(&spec("BTCUSDT.P", UM_DAY, false, false));
    assert!(matches!(result(&done, UM_DAY), ArchiveDayResultWire::ToppedUp { .. }), "{done:?}");
    assert_eq!(project.bars("BTCUSDT.P", UM_DAY), holed, "a plain import changes nothing");

    let listed = project.run(&spec("BTCUSDT.P", UM_DAY, true, true));
    assert!(
        matches!(class(&listed, UM_DAY), ArchiveDayClassWire::Replace { stored: 20, full: 24, .. }),
        "{listed:?}"
    );
    assert!(
        listed.outcome.is_none() && project.bars("BTCUSDT.P", UM_DAY) == holed,
        "a dry run writes nothing"
    );
    let done = project.run(&spec("BTCUSDT.P", UM_DAY, false, true));
    let ArchiveDayResultWire::Imported { ticks: 24, report: Some(report), .. } =
        result(&done, UM_DAY)
    else {
        panic!("the holed day is replaced: {done:?}");
    };
    assert!(report.contains("replaced the day whole"), "{report}");
    assert_eq!(project.bars("BTCUSDT.P", UM_DAY), rest("rest-um-BTCUSDT-1h-2024-01-01.json"));

    let kept = project.run(&spec("BTCUSDT", SPOT_DAY, false, true));
    assert_eq!(
        class(&kept, SPOT_DAY),
        &ArchiveDayClassWire::HeldByHttp,
        "a whole day is never replaced"
    );
    assert_eq!(project.bars("BTCUSDT", SPOT_DAY), whole);
}
