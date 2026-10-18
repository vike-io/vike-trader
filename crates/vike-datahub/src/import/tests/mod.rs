//! `import`'s unit tests: the shared fakes and helpers, the advertisement and the door.
//! The lane's own rules, over a FAKE format and real temporary directories — the default build,
//! so the roster lane runs them on every PR. The real Dukascopy format, a real store and the
//! real wire are `crates/vike-datahub/tests/archive_import.rs`'s, behind `backfill-serve`.

use std::collections::BTreeMap;
use std::fs;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use super::*;

const FAKE: &str = "fake-days";
const DAY: i64 = MS_PER_DAY;
/// 2024-01-15, a Monday.
const MON: i64 = 19_737 * DAY;
const WAIT: Duration = Duration::from_secs(10);

/// The fake grammar: any all-digit directory name at any depth (so the WALK's depth cap is the
/// only thing that bounds the descent), a file named `<n>.day` is the daily file of day `n`, and
/// `<n>.hour` a file of the other layout for day `n`. Everything else is "other".
struct FakeFormat {
    /// What the store side does, shared with the test.
    store: Arc<FakeStore>,
}

/// The fake store side: classes by day (FREE unless named), and a record of every decoded day.
#[derive(Default)]
struct FakeStore {
    classes: Mutex<BTreeMap<i64, DayClass>>,
    imported: Mutex<Vec<(i64, Vec<u8>)>>,
    sessions: AtomicUsize,
    /// When set, the FIRST `import_day` reports on `.0` and then waits on `.1` — the hook the
    /// slot test parks a running import on.
    park: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
}

impl FakeFormat {
    fn new() -> (Self, Arc<FakeStore>) {
        let store = Arc::new(FakeStore::default());
        (FakeFormat { store: Arc::clone(&store) }, store)
    }
}

fn fake_day(name: &str, suffix: &str) -> Option<i64> {
    let n: i64 = name.strip_suffix(suffix)?.parse().ok()?;
    Some(n * DAY)
}

impl ArchiveFormat for FakeFormat {
    fn id(&self) -> &'static str {
        FAKE
    }
    fn venue(&self) -> &'static str {
        "fakevenue"
    }
    fn admit(&self, dataset: &str) -> Result<String, String> {
        if dataset == "UNSCALED" { Err("no scale.".to_string()) } else { Ok("fake".to_string()) }
    }
    fn accepts_dir(&self, rel: &[&str], _now_ms: i64) -> bool {
        rel.last().is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    }
    fn classify_file(&self, rel: &[&str], _now_ms: i64) -> LayoutFile {
        let name = rel.last().copied().unwrap_or_default();
        if let Some(day) = fake_day(name, ".day") {
            LayoutFile::Daily(day)
        } else if let Some(day) = fake_day(name, ".hour") {
            LayoutFile::OtherLayout(day)
        } else {
            LayoutFile::Other
        }
    }
    fn max_file_bytes(&self) -> u64 {
        64
    }
    fn header_len(&self) -> usize {
        1
    }
    fn read_header(&self, _file_len: u64, prefix: &[u8]) -> Result<Option<u64>, DayRefusal> {
        match prefix.first() {
            Some(b'X') => Err(DayRefusal { class: "BadHeader".into(), detail: "x".into() }),
            Some(n) => Ok(Some(u64::from(*n))),
            None => Ok(Some(0)),
        }
    }
    fn session<'a>(
        &'a self,
        _dataset: &str,
        _bars: &[String],
        _now_ms: i64,
        _writes: bool,
    ) -> Result<Box<dyn ImportSession + 'a>, String> {
        self.store.sessions.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeSession { store: &self.store }))
    }
}

struct FakeSession<'a> {
    store: &'a FakeStore,
}

impl ImportSession for FakeSession<'_> {
    fn series(&self) -> Option<SeriesCoverage> {
        None
    }
    fn classify(&self, day: i64) -> DayClass {
        self.store.classes.lock().unwrap().get(&day).cloned().unwrap_or(DayClass::Free)
    }
    fn import_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, DayRefusal>,
    ) -> Result<DayResult, String> {
        if let Some((started, release)) = self.store.park.lock().unwrap().take() {
            started.send(()).unwrap();
            release.recv_timeout(WAIT).map_err(|_| "the test never released".to_string())?;
        }
        if !self.classify(day).is_importable() {
            return Ok(DayResult::ToppedUp { bars: Vec::new() });
        }
        match read() {
            Ok(bytes) => {
                let ticks = bytes.len() as u64;
                self.store.imported.lock().unwrap().push((day, bytes));
                Ok(DayResult::Imported { ticks, bars: Vec::new() })
            }
            Err(refusal) => Ok(DayResult::Refused(refusal)),
        }
    }
    fn verify_day(
        &mut self,
        _day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, DayRefusal>,
    ) -> Result<DayResult, String> {
        Ok(match read() {
            Ok(bytes) => DayResult::Verified { ticks: bytes.len() as u64 },
            Err(refusal) => DayResult::Refused(refusal),
        })
    }
}

/// An imports root with the fake format's directory and one dataset, `EURUSD`.
struct Tree {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Tree {
    fn new() -> Tree {
        let tmp = tempfile::tempdir().expect("a temp dir");
        let root = tmp.path().join("imports");
        fs::create_dir_all(root.join(FAKE).join("EURUSD")).unwrap();
        Tree { _tmp: tmp, root }
    }
    fn dataset(&self) -> PathBuf {
        self.root.join(FAKE).join("EURUSD")
    }
    /// A daily file for `day` under the bucket directory `bucket`, holding `bytes`.
    fn daily(&self, day: i64, bytes: &[u8]) -> PathBuf {
        let dir = self.dataset().join(format!("{}", day / DAY / 100));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.day", day / DAY));
        fs::write(&path, bytes).unwrap();
        path
    }
    fn lane(&self) -> (ImportLane, Arc<FakeStore>) {
        let (format, store) = FakeFormat::new();
        (ImportLane::new(self.root.clone(), vec![Box::new(format)]), store)
    }
}

fn spec(from: Option<i64>, to: Option<i64>, dry_run: bool, verify: bool) -> ImportSpec {
    ImportSpec {
        format: FAKE.to_string(),
        dataset: "EURUSD".to_string(),
        from_day: from,
        to_day: to,
        bars: Vec::new(),
        dry_run,
        verify,
    }
}

fn never() -> bool {
    false
}

fn done_of(response: Response) -> ImportDone {
    match response {
        Response::ArchiveImported(done) => *done,
        other => panic!("expected ArchiveImported, got {other:?}"),
    }
}

fn error_of(response: Response) -> String {
    match response {
        Response::Error(msg) => msg,
        other => panic!("expected Error, got {other:?}"),
    }
}

fn walk_of(tree: &Tree, caps: &WalkCaps) -> Walk {
    let (format, _) = FakeFormat::new();
    walk::walk_dataset(&tree.root, &format, "EURUSD", 0, caps).expect("under the caps")
}

// ---- the advertisement ------------------------------------------------------------------------

/// The capability and one `import_format=` entry per format, together — and NOTHING without a
/// lane. A lane is a runtime fact: this is what `served_features` pushes.
#[test]
fn the_advertisement_appears_exactly_when_a_lane_is_mounted() {
    assert!(advertised(None).is_empty(), "no lane, no capability and no format");
    let tree = Tree::new();
    let (lane, _) = tree.lane();
    let features = advertised(Some(&lane));
    assert_eq!(
        features,
        vec![
            FEATURE_ARCHIVE_IMPORT.to_string(),
            vike_datahub_client::proto::import_format_feature(FAKE),
        ]
    );
    assert_eq!(vike_datahub_client::proto::advertised_import_formats(&features), vec![FAKE]);
}

/// No format, no lane; no project, no lane; a project mounts one even before its root exists.
#[test]
fn the_lane_mounts_only_with_a_format_and_a_project() {
    let tmp = tempfile::tempdir().unwrap();
    let settings = tmp.path().join("settings");
    let (lane, line) = mount(Some(&settings), Vec::new());
    assert!(lane.is_none() && line.contains("backfill-serve"), "{line}");

    let (format, _) = FakeFormat::new();
    let (lane, line) = mount(None, vec![Box::new(format)]);
    assert!(lane.is_none() && line.contains("no project directory"), "{line}");

    let (format, _) = FakeFormat::new();
    let (lane, line) = mount(Some(&settings), vec![Box::new(format)]);
    let lane = lane.expect("a project mounts the lane");
    assert_eq!(lane.root(), tmp.path().join("market_data").join("imports"));
    assert!(line.contains("not created yet"), "{line}");

    // Once the root exists it is CANONICALIZED at mount.
    fs::create_dir_all(tmp.path().join("market_data").join("imports")).unwrap();
    let (format, _) = FakeFormat::new();
    let (lane, _) = mount(Some(&settings), vec![Box::new(format)]);
    let canonical = fs::canonicalize(tmp.path().join("market_data").join("imports")).unwrap();
    assert_eq!(lane.unwrap().root(), canonical);
}

// ---- the door ---------------------------------------------------------------------------------

#[test]
fn no_lane_answers_the_capability_refusal_naming_no_scope() {
    let msg = error_of(import_archive_verb(&spec(None, None, true, false), None, &never));
    assert!(msg.contains(FEATURE_ARCHIVE_IMPORT) && msg.contains("Nothing was read"), "{msg}");
    assert!(!msg.contains("scope"), "a lane-less server refuses on no scope grounds: {msg}");
}

/// The shared validator runs at THIS door too — a raw frame that skipped the client is refused,
/// and nothing is walked.
#[test]
fn the_server_runs_the_shared_validator_before_touching_the_directory() {
    let tree = Tree::new();
    let (lane, store) = tree.lane();
    for bad in ["..", "EUR/USD", "/ETC", "C:", ".HIDDEN", "EUR\0USD", "eurusd", "CON"] {
        let mut s = spec(None, None, true, false);
        s.dataset = bad.to_string();
        let msg = error_of(import_archive_verb(&s, Some(&lane), &never));
        assert!(msg.contains("import dataset"), "{bad:?}: {msg}");
    }
    let mut s = spec(None, None, true, false);
    s.dataset = "A".repeat(33);
    assert!(error_of(import_archive_verb(&s, Some(&lane), &never)).contains("32"));
    assert_eq!(store.sessions.load(Ordering::SeqCst), 0, "no request reached the store side");
}

#[test]
fn an_unregistered_format_is_refused_naming_the_registered_ones_and_never_echoed() {
    let tree = Tree::new();
    let (lane, _) = tree.lane();
    let mut s = spec(None, None, true, false);
    s.format = "../../etc".to_string();
    let msg = error_of(import_archive_verb(&s, Some(&lane), &never));
    assert!(msg.contains(FAKE) && !msg.contains("etc"), "{msg}");
}

#[test]
fn an_unadmitted_dataset_is_refused_before_anything_is_walked_or_opened() {
    let tree = Tree::new();
    let (lane, store) = tree.lane();
    let mut s = spec(None, None, false, false);
    s.dataset = "UNSCALED".to_string();
    let msg = error_of(import_archive_verb(&s, Some(&lane), &never));
    assert!(msg.contains("no scale") && msg.contains("Nothing was read"), "{msg}");
    assert_eq!(store.sessions.load(Ordering::SeqCst), 0);
}

#[cfg(test)]
mod plan_and_import;
#[cfg(test)]
mod walk_and_confinement;
