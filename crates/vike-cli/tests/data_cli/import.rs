//! `import`: a vendor archive read on the datahub's own side, one request per calendar month.

use std::collections::BTreeSet;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use vike_data::{HistStore, MemHistStore, SeriesCoverage};
use vike_datahub::import::{ArchiveFormat, ImportLane, ImportSession, LayoutFile};
use vike_datahub::server::serve_with_import;
use vike_datahub_client::archive::{BarsWritten, DayClass, DayRefusal, DayResult};

use super::support::{DAY_MS, spawn_seeded_datahub, utc_day};
use super::*;

// ─── `import`: a vendor archive, read on the datahub's own box ─────────────────────────────────

/// The format id the shipped binary is driven with — the datahub registry's own id for Dukascopy's
/// daily files (`crates/vike-datahub/src/import/formats.rs`'s `DUKASCOPY_BI5`).
const BI5: &str = "dukascopy-bi5";

/// One request the planted lane served, as its store saw it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct LaneRequest {
    /// Whether the request may write — an import — as the datahub opened its store session.
    writes: bool,
    /// The days it stored, in order.
    imported: Vec<i64>,
    /// The days it decoded without writing, in order.
    verified: Vec<i64>,
}

/// Everything the planted lane saw: every request in order, and every day it has stored — the
/// lane's whole store, which is what makes a re-run find its days HELD.
#[derive(Debug, Default)]
struct ArchiveLog {
    requests: Vec<LaneRequest>,
    stored: BTreeSet<i64>,
}

/// **The vendor's own LAYOUT and format id over a RECORDING store.** The walk, the safe open, the
/// plan, the one-import slot, the day cap and the wire are all the datahub's real ones
/// (`vike_datahub::import`); this replaces only the two halves of a format that need a store — the
/// decode and the append. The real Dukascopy format decodes into a `DataFusionHist` behind the
/// datahub's `backfill-serve` feature, which this crate's test build deliberately does not take: it
/// stays DataFusion-free (the dev-dependency note in this crate's manifest). The planted directory
/// is the vendor's real layout — `SYMBOL/YYYY/MM/DD_ticks.bi5`, the month ZERO-based — so the walk
/// the shipped binary is driven against is the one an operator's synced folder meets.
///
/// A file whose bytes start `REFUSE` is refused as `AmbiguousTimeBase`, the class the real decoder
/// gives one hour of ticks filed under a day. `moved` is a day an HTTP fetch "stores" after the
/// FIRST request this datahub serves, so the preview plans it free and every later plan holds it.
struct VendorLayout {
    log: Arc<Mutex<ArchiveLog>>,
    moved: Option<i64>,
}

fn all_digits(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit())
}

impl ArchiveFormat for VendorLayout {
    fn id(&self) -> &'static str {
        BI5
    }
    fn venue(&self) -> &'static str {
        "dukascopy"
    }
    fn admit(&self, _dataset: &str) -> Result<String, String> {
        Ok("point value 100000 — the planted test layout".to_string())
    }
    fn accepts_dir(&self, rel: &[&str], _now_ms: i64) -> bool {
        match rel {
            [year] => all_digits(year, 4),
            [_, month] => all_digits(month, 2),
            _ => false,
        }
    }
    fn classify_file(&self, rel: &[&str], _now_ms: i64) -> LayoutFile {
        let [year, month, file] = rel else { return LayoutFile::Other };
        let Some(day) = file.strip_suffix("_ticks.bi5") else { return LayoutFile::Other };
        match (year.parse::<i64>(), month.parse::<u32>(), day.parse::<u32>()) {
            (Ok(y), Ok(m), Ok(d)) => LayoutFile::Daily(utc_day(y, m + 1, d)),
            _ => LayoutFile::Other,
        }
    }
    fn max_file_bytes(&self) -> u64 {
        1024
    }
    fn header_len(&self) -> usize {
        0
    }
    /// Each planted file "declares" one tick per byte, so the plan's count is exact.
    fn read_header(&self, file_len: u64, _prefix: &[u8]) -> Result<Option<u64>, DayRefusal> {
        Ok(Some(file_len))
    }
    fn session<'a>(
        &'a self,
        _dataset: &str,
        _bars: &[String],
        _now_ms: i64,
        writes: bool,
    ) -> Result<Box<dyn ImportSession + 'a>, String> {
        let mut log = self.log.lock().unwrap();
        let moved = if log.requests.is_empty() { None } else { self.moved };
        log.requests.push(LaneRequest { writes, ..LaneRequest::default() });
        Ok(Box::new(Recorder { log: Arc::clone(&self.log), moved }))
    }
}

/// The store side of one request: it classifies against what it has stored, and records each day.
struct Recorder {
    log: Arc<Mutex<ArchiveLog>>,
    moved: Option<i64>,
}

/// What a `REFUSE` file is refused as.
fn ambiguous_time_base() -> DayResult {
    DayResult::Refused(DayRefusal {
        class: "AmbiguousTimeBase".to_string(),
        detail: "every tick lies in the day's first hour; no commit key was spent".to_string(),
    })
}

impl ImportSession for Recorder {
    fn series(&self) -> Option<SeriesCoverage> {
        None
    }
    fn classify(&self, day: i64) -> DayClass {
        if self.log.lock().unwrap().stored.contains(&day) {
            DayClass::HeldByArchive
        } else if self.moved == Some(day) {
            DayClass::HeldByHttp
        } else {
            DayClass::Free
        }
    }
    fn import_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, DayRefusal>,
    ) -> Result<DayResult, String> {
        if self.classify(day) != DayClass::Free {
            return Ok(DayResult::ToppedUp { bars: Vec::new() });
        }
        let bytes = match read() {
            Ok(bytes) => bytes,
            Err(refusal) => return Ok(DayResult::Refused(refusal)),
        };
        if bytes.starts_with(b"REFUSE") {
            return Ok(ambiguous_time_base());
        }
        let mut log = self.log.lock().unwrap();
        log.stored.insert(day);
        log.requests.last_mut().expect("a session").imported.push(day);
        Ok(DayResult::Imported {
            ticks: bytes.len() as u64,
            bars: vec![BarsWritten { interval: "1m".to_string(), rows: 1 }],
        })
    }
    fn verify_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, DayRefusal>,
    ) -> Result<DayResult, String> {
        let bytes = match read() {
            Ok(bytes) => bytes,
            Err(refusal) => return Ok(DayResult::Refused(refusal)),
        };
        self.log.lock().unwrap().requests.last_mut().expect("a session").verified.push(day);
        if bytes.starts_with(b"REFUSE") {
            Ok(ambiguous_time_base())
        } else {
            Ok(DayResult::Verified { ticks: bytes.len() as u64 })
        }
    }
}

/// Tuesday 30 January to Friday 2 February 2024 — four weekdays across a MONTH boundary.
fn four_days() -> [i64; 4] {
    [utc_day(2024, 1, 30), utc_day(2024, 1, 31), utc_day(2024, 2, 1), utc_day(2024, 2, 2)]
}

/// An imports root holding `days` of `EURUSD` in the vendor's layout — each file five bytes, or
/// `REFUSE` for a day in `refused`.
fn plant_archive(days: &[i64], refused: &[i64]) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("imports");
    for &day in days {
        let (y, m, d) = vike_model::time::civil_from_days(day / DAY_MS);
        let month_dir =
            root.join(BI5).join("EURUSD").join(format!("{y:04}")).join(format!("{:02}", m - 1));
        std::fs::create_dir_all(&month_dir).expect("plant a month");
        let bytes: &[u8] = if refused.contains(&day) { b"REFUSE" } else { b"ticks" };
        std::fs::write(month_dir.join(format!("{d:02}_ticks.bi5")), bytes).expect("plant a day");
    }
    (tmp, root)
}

/// A key-less loopback datahub whose import lane reads `root` through [`VendorLayout`] — the
/// configuration an end user runs on their own box, which serves the verb as it serves `Backfill`.
fn spawn_archive_datahub(
    root: PathBuf,
    moved: Option<i64>,
) -> (SocketAddr, Arc<Mutex<ArchiveLog>>) {
    let log = Arc::new(Mutex::new(ArchiveLog::default()));
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    let format = VendorLayout { log: Arc::clone(&log), moved };
    let lane = Arc::new(ImportLane::new(root, vec![Box::new(format)]));
    thread::spawn(move || {
        let _ = serve_with_import(listener, store, None, None, None, None, None, Some(lane));
    });
    (addr, log)
}

/// `data hist import dukascopy-bi5 EURUSD --addr ADDR` plus `extra`.
fn import_line<'a>(addr: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["data", "hist", "import", BI5, "EURUSD", "--addr", addr];
    args.extend_from_slice(extra);
    args
}

fn requests(log: &Arc<Mutex<ArchiveLog>>) -> Vec<LaneRequest> {
    log.lock().unwrap().requests.clone()
}

/// A writing request that stored `imported`.
fn imported(days: &[i64]) -> LaneRequest {
    LaneRequest { writes: true, imported: days.to_vec(), verified: Vec::new() }
}

/// **The verb, end to end through the shipped binary.** One plan-only request for the whole window,
/// then — confirmed by `--yes` — ONE REQUEST PER CALENDAR MONTH, each holding only its month's days,
/// a progress line on stderr as each answers, and the summary. Run again, the same command RESUMES:
/// every day comes back held, the months are still sent (they top up a held day's missing bars),
/// and nothing is imported twice.
#[test]
fn an_import_plans_then_sends_one_request_per_calendar_month_and_a_rerun_finds_it_held() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let days = four_days();
    let (_archive, root) = plant_archive(&days, &[]);
    let (addr, log) = spawn_archive_datahub(root.clone(), None);
    let addr = addr.to_string();

    let out = run(scratch.path(), &import_line(&addr, &["--yes"]));
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        requests(&log),
        vec![LaneRequest::default(), imported(&days[..2]), imported(&days[2..])],
        "the plan, then one request per calendar month"
    );
    let text = stdout(&out);
    let dir = root.join(BI5).join("EURUSD");
    for needle in [
        format!("{BI5} · EURUSD — datahub at {addr}"),
        format!("server dir  {}   (on the DATAHUB's box)", dir.display()),
        "files       4 daily files 2024-01-30 .. 2024-02-02".to_string(),
        "import      4 days · 20 ticks (exact from 4 file headers)".to_string(),
        "bars        1m, resampled per day from the stored ticks".to_string(),
        "done — 4 days imported (20 ticks, 4 bars) · 0 refused · 0 overlapped · 0 gaps · 0 \
         already held"
            .to_string(),
    ] {
        assert!(text.contains(&needle), "missing {needle:?}:\n{text}");
    }
    let err = stderr(&out);
    for needle in [
        "importing dukascopy-bi5 EURUSD [2024-01-30 .. 2024-02-02] as 2 requests, one per \
         calendar month",
        "  1/2  2024-01  2 days  10 ticks  0 refused",
        "  2/2  2024-02  2 days  10 ticks  0 refused",
    ] {
        assert!(err.contains(needle), "missing {needle:?}:\n{err}");
    }

    // The same command again — a RESUME.
    let out = run(scratch.path(), &import_line(&addr, &["--yes"]));
    assert!(out.status.success(), "{}", stderr(&out));
    let reqs = requests(&log);
    assert_eq!(reqs.len(), 6, "the re-run planned and still sent both months: {reqs:?}");
    assert!(reqs[3..].iter().all(|r| r.imported.is_empty()), "imported twice: {reqs:?}");
    assert!(reqs[4].writes && reqs[5].writes, "the held months top up their bars: {reqs:?}");
    let text = stdout(&out);
    assert!(text.contains("held        4 days (4 by this lane · 0 by the HTTP lane)"), "{text}");
    assert!(text.contains("import      0 days"), "{text}");
    assert!(text.contains("· 4 already held"), "{text}");
}

/// **A non-terminal without `--yes` is REFUSED before anything is sent** — not even the plan — on
/// the usage rung, as `rm` refuses one: a confirmation read from a pipe is not a confirmation.
#[test]
fn an_import_without_yes_and_without_a_terminal_is_refused_before_anything_is_sent() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let (_archive, root) = plant_archive(&four_days(), &[]);
    let (addr, log) = spawn_archive_datahub(root, None);
    let addr = addr.to_string();
    let out = run(scratch.path(), &import_line(&addr, &[]));
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("refusing to import without --yes"), "{}", stderr(&out));
    assert_eq!(stdout(&out), "", "a refused run printed a plan");
    assert_eq!(requests(&log), Vec::new(), "the datahub was asked something");
    assert!(log.lock().unwrap().stored.is_empty());
}

/// **`--dry-run` sends the plan and nothing else** — `--yes` beside it loses, as on `rm` — and
/// **`--dry-run --verify` decodes every importable day, one request per month, writing nothing**.
#[test]
fn a_dry_run_sends_only_the_plan_and_verify_decodes_month_by_month_writing_nothing() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let days = four_days();
    let (_archive, root) = plant_archive(&days, &[]);
    let (addr, log) = spawn_archive_datahub(root, None);
    let addr = addr.to_string();

    let out = run(scratch.path(), &import_line(&addr, &["--dry-run", "--yes"]));
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(requests(&log), vec![LaneRequest::default()], "one plan-only request");
    assert!(
        stdout(&out).contains("--dry-run: nothing was decoded and nothing was written"),
        "{}",
        stdout(&out)
    );

    let out = run(scratch.path(), &import_line(&addr, &["--dry-run", "--verify"]));
    assert!(out.status.success(), "{}", stderr(&out));
    let verified =
        |days: &[i64]| LaneRequest { writes: false, imported: Vec::new(), verified: days.to_vec() };
    assert_eq!(
        requests(&log)[1..],
        [LaneRequest::default(), verified(&days[..2]), verified(&days[2..])],
        "the plan, then every importable day decoded, a month per request, none writing"
    );
    assert!(log.lock().unwrap().stored.is_empty(), "a dry run wrote");
    assert!(
        stdout(&out).contains(
            "verified — 4 days decoded clean (20 ticks) · 0 refused · nothing was written"
        ),
        "{}",
        stdout(&out)
    );
    assert!(
        stderr(&out).contains("verifying dukascopy-bi5 EURUSD [2024-01-30 .. 2024-02-02] as 2")
    );
}

/// **`--from` and `--to` are INCLUSIVE days**: 31 January to 1 February imports exactly those two
/// days, one per month request. Under `--json` stdout is the one document — the plan as the datahub
/// sent it, the months, both importable counts — while the plan and progress lines go to stderr.
#[test]
fn the_window_is_inclusive_days_and_json_is_the_whole_of_stdout() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let days = four_days();
    let (_archive, root) = plant_archive(&days, &[]);
    let (addr, log) = spawn_archive_datahub(root, None);
    let addr = addr.to_string();
    let window = ["--from", "2024-01-31", "--to", "2024-02-01", "--yes", "--json"];
    let out = run(scratch.path(), &import_line(&addr, &window));
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        requests(&log),
        vec![LaneRequest::default(), imported(&days[1..2]), imported(&days[2..3])],
        "both bounds inclusive, one request per month"
    );
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));
    assert_eq!(doc["mode"], "import");
    assert_eq!(doc["dataset"], "EURUSD");
    assert_eq!(doc["addr"], addr.as_str());
    assert_eq!(doc["plan"]["from_day"], days[1]);
    assert_eq!(doc["plan"]["to_day"], days[2]);
    assert_eq!(doc["months"].as_array().map(Vec::len), Some(2));
    assert_eq!(doc["months"][0]["to_day"], days[1]);
    assert_eq!(doc["months"][1]["from_day"], days[2]);
    assert_eq!(doc["summary"]["imported"], 2);
    assert_eq!(doc["summary"]["preview_importable"], 2);
    assert_eq!(doc["summary"]["server_importable"], 2);
    let err = stderr(&out);
    assert!(err.contains("server dir") && err.contains("  1/2  2024-01  1 day"), "{err}");
    assert!(err.contains("done — 2 days imported"), "{err}");
}

/// **A day refused for what is in its FILE fails the run, and only that day.** `--dry-run --verify`
/// lists it and exits 1 having written nothing; the import stores the other three, names the
/// refused day on its month's line and in the summary, and exits 1 — no key was spent for it.
#[test]
fn a_day_refused_for_its_file_is_listed_and_fails_the_run_while_the_rest_imports() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let days = four_days();
    let (_archive, root) = plant_archive(&days, &[days[2]]);
    let (addr, log) = spawn_archive_datahub(root, None);
    let addr = addr.to_string();

    let out = run(scratch.path(), &import_line(&addr, &["--dry-run", "--verify"]));
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("refused — 1 day, and no commit key was spent"), "{text}");
    assert!(text.contains("2024-02-01  AmbiguousTimeBase — every tick lies"), "{text}");
    assert!(text.contains("verified — 3 days decoded clean (15 ticks) · 1 refused"), "{text}");
    assert!(stderr(&out).contains("would be refused by an import"), "{}", stderr(&out));
    assert!(log.lock().unwrap().stored.is_empty(), "a verify wrote");

    let out = run(scratch.path(), &import_line(&addr, &["--yes"]));
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let stored: Vec<i64> = log.lock().unwrap().stored.iter().copied().collect();
    assert_eq!(stored, vec![days[0], days[1], days[3]], "the rest of the window imported");
    let err = stderr(&out);
    assert!(err.contains("  2/2  2024-02  1 day  5 ticks  1 refused"), "{err}");
    assert!(err.contains("(2024-02-01: AmbiguousTimeBase — see --dry-run --verify)"), "{err}");
    assert!(err.contains("refused for what is in their files"), "{err}");
    assert!(stdout(&out).contains("done — 3 days imported (15 ticks, 3 bars) · 1 refused"));
}

/// **Two plans, one authority.** A day an HTTP fetch stores between the preview and the import is
/// HELD in the datahub's own month plan, so the preview's count and the datahub's differ: the
/// summary prints BOTH and names the datahub's as the authority, the month whose plan moved says so
/// on its line, and the day is not imported.
#[test]
fn when_the_datahub_replans_differently_both_counts_are_printed() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let days = four_days();
    let (_archive, root) = plant_archive(&days, &[]);
    let (addr, log) = spawn_archive_datahub(root, Some(days[2]));
    let addr = addr.to_string();
    let out = run(scratch.path(), &import_line(&addr, &["--yes"]));
    assert!(out.status.success(), "{}", stderr(&out));
    let stored: Vec<i64> = log.lock().unwrap().stored.iter().copied().collect();
    assert_eq!(stored, vec![days[0], days[1], days[3]], "the moved day stays the HTTP lane's");
    let text = stdout(&out);
    assert!(
        text.contains(
            "the datahub's plans differ from the preview: the preview counted 4 days importable, \
             the datahub counted 3 days as it went month by month"
        ),
        "{text}"
    );
    assert!(text.contains("The datahub's count is the authority"), "{text}");
    assert!(text.contains("· 1 already held"), "{text}");
    let err = stderr(&out);
    assert!(err.contains("(the datahub planned 1 importable here, the preview 2)"), "{err}");
}

/// **A dataset that is not on the datahub's box** names the SERVER's own directory and the two ways
/// to put the files there — paste-ready, with the dataset substituted — and exits 1 having read and
/// written nothing.
#[test]
fn an_absent_dataset_names_the_servers_directory_and_how_to_fill_it() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let (_archive, root) = plant_archive(&four_days(), &[]);
    let (addr, log) = spawn_archive_datahub(root.clone(), None);
    let addr = addr.to_string();
    let out = run(
        scratch.path(),
        &["data", "hist", "import", BI5, "GBPUSD", "--addr", &addr, "--dry-run"],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let dir = root.join(BI5).join("GBPUSD");
    let text = stdout(&out);
    for needle in [
        format!("server dir  {}   NOT FOUND on the datahub's box", dir.display()),
        format!("aws s3 sync s3://cfg-public-proper-wallaby/GBPUSD/ {}/ --region", dir.display()),
        format!("rsync -a ./GBPUSD/ <box>:{}/", dir.display()),
    ] {
        assert!(text.contains(&needle), "missing {needle:?}:\n{text}");
    }
    assert!(stderr(&out).contains("no such directory on the datahub's box"), "{}", stderr(&out));
    assert!(log.lock().unwrap().stored.is_empty());
}

/// **A datahub with no import lane is refused with NOTHING SENT**, on the run-failure rung, naming
/// the capability — and the refusal says what to DO, because no flag on this side can change it.
#[test]
fn a_datahub_without_the_lane_is_refused_with_nothing_sent_and_says_what_to_do() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let out = run(scratch.path(), &import_line(&addr, &["--dry-run"]));
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    for needle in [
        "archive_import",
        "nothing was sent",
        "what to do",
        "v0.1.41",
        "ARCHIVE IMPORT lane mounted",
    ] {
        assert!(err.contains(needle), "missing {needle:?}: {err}");
    }
    assert_eq!(stdout(&out), "", "it printed a plan it did not get");
}
