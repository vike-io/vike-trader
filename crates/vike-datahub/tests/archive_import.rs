//! **The ARCHIVE IMPORT lane over the real wire** — `Request::ImportArchive` served by the real
//! connection loop (`docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` §8, the datahub
//! rows; task T4).
//!
//! Two halves, the `backfill_readback.rs` shape:
//!
//! 1. **The lane's wire behaviour over a FAKE format** — no feature needed, so the derived roster lane
//!    runs it on every PR: the advertisement, the scope (key-less loopback served; keyed Observe
//!    refused BY SCOPE; keyed Control served), the shared validator at the server's own door, and a
//!    client that goes away stopping the import at a day boundary with a close and no reply.
//! 2. **`real_format`, behind `backfill-serve`** — the real Dukascopy daily-file format over a real
//!    `DataFusionHist` in a temporary directory, with REAL `.bi5` files written by `lzma-rs`: the
//!    plan, the import and its repeat, `verify`, the refusals that keep an uncertain input from
//!    spending a key, and a planted symlink that imports nothing from outside the root. The
//!    `hist-datafusion` job runs it (`cargo test -p vike-datahub --features backfill-serve`).
//!
//! The walk-and-open confinement rows (a symlinked dataset, year and file, a FIFO, a hard link, the
//! swap between the walk and the open) are in `crates/vike-datahub/src/import/mod.rs`'s test module,
//! where the walk's own output is reachable; one end-to-end symlink row is repeated here.
//!
//! Nothing here touches a live project folder or a live datahub: every root is a temporary
//! directory, every server an ephemeral loopback port.

use std::io::Read;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use vike_data::{HistStore, MemHistStore, SeriesCoverage};
use vike_datahub::import::{ArchiveFormat, ImportLane, ImportSession, LayoutFile};
use vike_datahub::server::serve_with_import;
use vike_datahub_client::archive::{DayClass, DayRefusal, DayResult, ImportSpec};
use vike_datahub_client::proto::{
    FEATURE_ARCHIVE_IMPORT, Request, Response, advertised_import_formats, write_frame,
};
use vike_datahub_client::{DatahubClient, read_frame};
use vike_model::MS_PER_DAY;
use vike_node_proto::auth::{NodeKeys, Scope};

const DAY: i64 = MS_PER_DAY;
/// 2024-01-15 00:00 UTC, a Monday.
const MON: i64 = 19_737 * DAY;
const WAIT: Duration = Duration::from_secs(10);
/// Long enough for a FIN written on loopback to be in the peer's kernel buffer — margin, not a
/// measurement (the `backfill_cancel.rs` value).
const SETTLE: Duration = Duration::from_millis(200);

const FAKE: &str = "fake-days";

// ------------------------------------------------------------------------------------------------
// The fake format: `<bucket>/<n>.day` is day `n`'s daily file; every decoded day is recorded, and
// the FIRST import can be parked so a test decides what the client has done by the time the stop
// probe is asked.
// ------------------------------------------------------------------------------------------------

#[derive(Default)]
struct FakeStore {
    imported: Mutex<Vec<i64>>,
    park: Mutex<Option<(Sender<()>, Receiver<()>)>>,
}

struct FakeFormat(Arc<FakeStore>);

impl ArchiveFormat for FakeFormat {
    fn id(&self) -> &'static str {
        FAKE
    }
    fn venue(&self) -> &'static str {
        "fakevenue"
    }
    fn admit(&self, _dataset: &str) -> Result<String, String> {
        Ok("fake".to_string())
    }
    fn accepts_dir(&self, rel: &[&str], _now_ms: i64) -> bool {
        rel.len() == 1 && rel[0].bytes().all(|b| b.is_ascii_digit())
    }
    fn classify_file(&self, rel: &[&str], _now_ms: i64) -> LayoutFile {
        match rel {
            [_, name] => name
                .strip_suffix(".day")
                .and_then(|n| n.parse::<i64>().ok())
                .map_or(LayoutFile::Other, |n| LayoutFile::Daily(n * DAY)),
            _ => LayoutFile::Other,
        }
    }
    fn max_file_bytes(&self) -> u64 {
        1024
    }
    fn header_len(&self) -> usize {
        0
    }
    fn read_header(&self, _file_len: u64, _prefix: &[u8]) -> Result<Option<u64>, DayRefusal> {
        Ok(None)
    }
    fn session<'a>(
        &'a self,
        _dataset: &str,
        _bars: &[String],
        _now_ms: i64,
        _writes: bool,
    ) -> Result<Box<dyn ImportSession + 'a>, String> {
        Ok(Box::new(FakeSession(&self.0)))
    }
}

struct FakeSession<'a>(&'a FakeStore);

impl ImportSession for FakeSession<'_> {
    fn series(&self) -> Option<SeriesCoverage> {
        None
    }
    fn classify(&self, _day: i64) -> DayClass {
        DayClass::Free
    }
    fn import_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, DayRefusal>,
    ) -> Result<DayResult, String> {
        if let Some((parked, release)) = self.0.park.lock().unwrap().take() {
            parked.send(()).unwrap();
            release.recv_timeout(WAIT).map_err(|_| "never released".to_string())?;
        }
        let bytes = read().map_err(|r| r.detail)?;
        self.0.imported.lock().unwrap().push(day);
        Ok(DayResult::Imported { ticks: bytes.len() as u64, bars: Vec::new() })
    }
    fn verify_day(
        &mut self,
        _day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, DayRefusal>,
    ) -> Result<DayResult, String> {
        Ok(DayResult::Verified { ticks: read().map_err(|r| r.detail)?.len() as u64 })
    }
}

/// A temporary imports root holding `days` of the fake format's daily files for dataset `EURUSD`.
fn fake_root(days: &[i64]) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("a temp dir");
    let root = tmp.path().join("imports");
    let bucket = root.join(FAKE).join("EURUSD").join("1");
    std::fs::create_dir_all(&bucket).unwrap();
    for day in days {
        std::fs::write(bucket.join(format!("{}.day", day / DAY)), b"ticks").unwrap();
    }
    (tmp, root)
}

/// Serve `lane` over an empty in-memory store on an ephemeral loopback port.
fn serve_lane(lane: ImportLane, keys: Option<NodeKeys>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("addr");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    let lane = Arc::new(lane);
    thread::spawn(move || {
        let _ = serve_with_import(listener, store, None, keys, None, None, None, Some(lane));
    });
    addr
}

fn fake_spec(dry_run: bool) -> ImportSpec {
    ImportSpec {
        format: FAKE.to_string(),
        dataset: "EURUSD".to_string(),
        from_day: Some(MON),
        to_day: Some(MON + 4 * DAY),
        bars: Vec::new(),
        dry_run,
        verify: false,
    }
}

fn keys() -> NodeKeys {
    NodeKeys::new(b"observe-key".to_vec(), b"control-key".to_vec())
}

// ---- 1. the lane's wire behaviour --------------------------------------------------------------

/// The capability and the format entry ride the `Welcome` of a server whose lane is MOUNTED, and a
/// key-less loopback server serves the verb itself — `Backfill`'s posture, not `DeleteSeries`'.
#[test]
fn a_keyless_loopback_server_with_a_lane_advertises_it_and_serves_the_verb() {
    let (_tmp, root) = fake_root(&[MON, MON + DAY]);
    let store = Arc::new(FakeStore::default());
    let addr =
        serve_lane(ImportLane::new(root, vec![Box::new(FakeFormat(Arc::clone(&store)))]), None);

    let mut client = DatahubClient::connect(addr).expect("handshake");
    assert!(
        client.features().iter().any(|f| f == FEATURE_ARCHIVE_IMPORT),
        "{:?}",
        client.features()
    );
    assert_eq!(advertised_import_formats(client.features()), vec![FAKE]);

    let plan = client.import_archive(&fake_spec(true)).expect("a dry run is served key-less");
    assert_eq!(plan.plan.days.len(), 2);
    assert!(plan.outcome.is_none() && store.imported.lock().unwrap().is_empty());

    let done = client.import_archive(&fake_spec(false)).expect("an import is served key-less");
    assert_eq!(done.outcome.expect("an import has an outcome").days.len(), 2);
    assert_eq!(*store.imported.lock().unwrap(), vec![MON, MON + DAY]);
}

/// On a KEYED server the verb is Control: an Observe connection is refused BY SCOPE — naming the
/// verb among the Control-only writes — and the connection survives; a Control one is served.
#[test]
fn a_keyed_server_refuses_observe_by_scope_and_serves_control() {
    let (_tmp, root) = fake_root(&[MON]);
    let store = Arc::new(FakeStore::default());
    let addr = serve_lane(
        ImportLane::new(root, vec![Box::new(FakeFormat(Arc::clone(&store)))]),
        Some(keys()),
    );

    let mut observe =
        DatahubClient::connect_authed(addr, &keys(), Scope::Read).expect("observe auth");
    let err = observe.import_archive(&fake_spec(false)).expect_err("Observe may not import");
    assert!(err.contains("requires the Control scope"), "{err}");
    assert!(err.contains("ImportArchive store WRITE"), "the refusal names this verb: {err}");
    assert!(store.imported.lock().unwrap().is_empty(), "nothing was imported");
    observe.ping().expect("the connection survived the scope refusal");

    let mut control =
        DatahubClient::connect_authed(addr, &keys(), Scope::Write).expect("control auth");
    let done = control.import_archive(&fake_spec(false)).expect("Control may import");
    assert_eq!(done.outcome.unwrap().days.len(), 1);
    assert_eq!(*store.imported.lock().unwrap(), vec![MON]);
}

/// The SHARED validator runs at the server's own door: a raw frame that skipped the client's copy
/// is refused before the directory is touched, and the connection survives each refusal.
#[test]
fn a_raw_frame_with_an_invalid_dataset_is_refused_at_the_servers_door() {
    let (_tmp, root) = fake_root(&[MON]);
    let store = Arc::new(FakeStore::default());
    let addr =
        serve_lane(ImportLane::new(root, vec![Box::new(FakeFormat(Arc::clone(&store)))]), None);
    let mut stream = TcpStream::connect(addr).expect("connect");
    let long = "A".repeat(33);
    for bad in
        ["..", "EUR/USD", "/ABS", "C:EURUSD", ".EURUSD", "EUR\0USD", long.as_str(), "eurusd", "CON"]
    {
        let mut spec = fake_spec(false);
        spec.dataset = bad.to_string();
        write_frame(&mut stream, &Request::ImportArchive(spec)).expect("send");
        match read_frame::<_, Response>(&mut stream).expect("an answer") {
            Response::Error(msg) => assert!(msg.contains("import dataset"), "{bad:?}: {msg}"),
            other => panic!("{bad:?} was answered {other:?}"),
        }
    }
    // ...and a 32-day execute with explicit bounds, the validator's day cap.
    let mut spec = fake_spec(false);
    spec.to_day = Some(MON + 31 * DAY);
    write_frame(&mut stream, &Request::ImportArchive(spec)).expect("send");
    match read_frame::<_, Response>(&mut stream).expect("an answer") {
        Response::Error(msg) => assert!(msg.contains("IMPORT_MAX_DAYS"), "{msg}"),
        other => panic!("a 32-day execute was answered {other:?}"),
    }
    write_frame(&mut stream, &Request::Ping).expect("ping");
    assert!(matches!(read_frame::<_, Response>(&mut stream), Ok(Response::Pong)));
    assert!(store.imported.lock().unwrap().is_empty());
}

/// **A client that goes away stops the import at the next DAY** — through the real connection
/// loop. The first day is parked until the client has closed its side; once released, the request's
/// stop probe sees the FIN before the second day, the import ends there, and the connection closes
/// with NO reply.
#[test]
fn a_client_that_goes_away_stops_the_import_at_a_day_boundary_and_gets_no_reply() {
    let days: Vec<i64> = (0..5).map(|i| MON + i * DAY).collect();
    let (_tmp, root) = fake_root(&days);
    let store = Arc::new(FakeStore::default());
    let (parked_tx, parked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *store.park.lock().unwrap() = Some((parked_tx, release_rx));
    let addr =
        serve_lane(ImportLane::new(root, vec![Box::new(FakeFormat(Arc::clone(&store)))]), None);

    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::ImportArchive(fake_spec(false))).expect("send");
    parked_rx.recv_timeout(WAIT).expect("the first day is running");
    // The client is gone: a half-close is all a peek can see, and it reads as gone (the probe's doc).
    stream.shutdown(Shutdown::Write).expect("half-close");
    thread::sleep(SETTLE);
    release_tx.send(()).unwrap();

    stream.set_read_timeout(Some(WAIT)).unwrap();
    let mut buf = Vec::new();
    let read = stream.read_to_end(&mut buf);
    assert!(
        read.is_ok() && buf.is_empty(),
        "no reply is written to a gone client: {read:?} {buf:?}"
    );
    assert_eq!(*store.imported.lock().unwrap(), vec![MON], "one day, then the boundary");
}

// ---- 2. the real Dukascopy format, a real store -------------------------------------------------

#[cfg(feature = "backfill-serve")]
mod real_format {
    use std::collections::BTreeMap;

    use vike_data::{DataFusionHist, TsRange};
    use vike_datahub::import::formats::{
        DUKASCOPY_BI5, EMPTY_PAYLOAD, SCALE_MISMATCH, real_import_registry,
    };
    use vike_datahub_client::archive::DatasetDir;

    use super::*;

    /// A project in a temporary directory: an imports root, and a real store beside it.
    struct Project {
        tmp: tempfile::TempDir,
        store: Arc<DataFusionHist>,
    }

    impl Project {
        fn new() -> Project {
            let tmp = tempfile::tempdir().expect("a temp dir");
            let store = Arc::new(DataFusionHist::open(tmp.path().join("hist")).expect("a store"));
            Project { tmp, store }
        }
        fn root(&self) -> PathBuf {
            self.tmp.path().join("imports")
        }
        fn dataset(&self, symbol: &str) -> PathBuf {
            self.root().join(DUKASCOPY_BI5).join(symbol)
        }
        /// Write `bytes` as `symbol`'s daily file for 2024-01-`dd` — the vendor's layout, January
        /// being month `00`.
        fn daily(&self, symbol: &str, dd: u32, bytes: &[u8]) -> PathBuf {
            let dir = self.dataset(symbol).join("2024").join("00");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("{dd:02}_ticks.bi5"));
            std::fs::write(&path, bytes).unwrap();
            path
        }
        fn serve(&self) -> SocketAddr {
            let lane = ImportLane::new(self.root(), real_import_registry(Arc::clone(&self.store)));
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let addr = listener.local_addr().expect("addr");
            let served: Arc<dyn HistStore + Send + Sync> = self.store.clone();
            let lane = Arc::new(lane);
            thread::spawn(move || {
                let _ =
                    serve_with_import(listener, served, None, None, None, None, None, Some(lane));
            });
            addr
        }
        fn quotes(&self, symbol: &str) -> usize {
            self.store.scan_quotes("dukascopy", symbol, TsRange::all()).expect("scan").len()
        }
        fn bars(&self, symbol: &str, interval: &str) -> usize {
            self.store.load_bars("dukascopy", symbol, interval, TsRange::all()).expect("load").len()
        }
    }

    /// 2024-01-`dd` as the epoch-ms of its UTC midnight.
    fn jan(dd: u32) -> i64 {
        MON + (i64::from(dd) - 15) * DAY
    }

    /// A REAL daily `.bi5` file: big-endian `>IIIff` records (ms offset, ask points, bid points, ask
    /// volume, bid volume) in an LZMA-alone stream that DECLARES its unpacked size, as the measured
    /// feed file does.
    fn bi5(records: &[(u32, u32, u32)]) -> Vec<u8> {
        let mut raw = Vec::with_capacity(records.len() * 20);
        for &(ms, ask, bid) in records {
            raw.extend_from_slice(&ms.to_be_bytes());
            raw.extend_from_slice(&ask.to_be_bytes());
            raw.extend_from_slice(&bid.to_be_bytes());
            raw.extend_from_slice(&1.5f32.to_be_bytes());
            raw.extend_from_slice(&2.5f32.to_be_bytes());
        }
        let options = lzma_rs::compress::Options {
            unpacked_size: lzma_rs::compress::UnpackedSize::WriteToHeader(Some(raw.len() as u64)),
        };
        let mut out = Vec::new();
        lzma_rs::lzma_compress_with_options(&mut raw.as_slice(), &mut out, &options)
            .expect("encode");
        out
    }

    /// Three EURUSD ticks across two 1m buckets, at 10:00:00, 10:00:01 and 14:30:00 UTC — a day a
    /// time-base check accepts (not all in the first hour), at the 100,000 point value.
    fn a_day(scale_up: u32) -> Vec<u8> {
        bi5(&[
            (36_000_000, 109_469 * scale_up, 109_465 * scale_up),
            (36_001_000, 109_470 * scale_up, 109_466 * scale_up),
            (52_200_000, 109_480 * scale_up, 109_476 * scale_up),
        ])
    }

    fn spec(symbol: &str, from: u32, to: u32, dry_run: bool, verify: bool) -> ImportSpec {
        ImportSpec {
            format: DUKASCOPY_BI5.to_string(),
            dataset: symbol.to_string(),
            from_day: Some(jan(from)),
            to_day: Some(jan(to)),
            bars: vec!["1m".to_string()],
            dry_run,
            verify,
        }
    }

    fn results(done: &vike_datahub_client::archive::ImportDone) -> BTreeMap<i64, DayResult> {
        let outcome = done.outcome.as_ref().expect("an outcome");
        outcome.days.iter().map(|d| (d.day, d.result.clone())).collect()
    }

    /// The plan over the vendor's real layout: daily files planned FREE with the tick count their
    /// header DECLARES, a day of hourly files reported (not imported), a day holding both refused,
    /// an unexpected file counted, the missing weekday a gap — and nothing written.
    #[test]
    fn a_dry_run_plans_the_vendor_layout_and_writes_nothing() {
        let project = Project::new();
        project.daily("EURUSD", 15, &a_day(1));
        project.daily("EURUSD", 16, &a_day(1));
        project.daily("EURUSD", 19, &a_day(1));
        // Thursday the 18th: hourly files only. Friday the 19th: both layouts.
        for dd in [18, 19] {
            let hour_dir =
                project.dataset("EURUSD").join("2024").join("00").join(format!("{dd:02}"));
            std::fs::create_dir_all(&hour_dir).unwrap();
            std::fs::write(hour_dir.join("10h_ticks.bi5"), a_day(1)).unwrap();
        }
        std::fs::write(project.dataset("EURUSD").join("2024").join("notes.txt"), b"hi").unwrap();

        let mut client = DatahubClient::connect(project.serve()).expect("handshake");
        let done = client.import_archive(&spec("EURUSD", 15, 19, true, false)).expect("a plan");
        let plan = &done.plan;
        assert!(done.outcome.is_none());
        assert_eq!(plan.venue, "dukascopy");
        assert!(plan.admission.contains("100000"), "{}", plan.admission);
        assert!(plan.server_dir.ends_with("EURUSD"), "{}", plan.server_dir);
        assert_eq!(plan.inventory.daily_files, 3);
        assert_eq!(plan.inventory.other_layout_days, vec![jan(18)]);
        assert_eq!((plan.inventory.other_objects, plan.inventory.other_bytes), (1, 2));
        assert_eq!(plan.gaps, vec![jan(17)], "Wednesday has no file of either layout");
        let days: Vec<_> =
            plan.days.iter().map(|d| (d.day, d.class.clone(), d.declared_ticks)).collect();
        assert_eq!(days[0], (jan(15), DayClass::Free, Some(3)), "3 records declared by the header");
        assert_eq!(days[1], (jan(16), DayClass::Free, Some(3)));
        assert!(matches!(&days[2].1, DayClass::Refused(r) if r.class == "MixedLayout"), "{days:?}");
        assert_eq!(plan.series, None, "the store holds nothing yet");
        assert_eq!(project.quotes("EURUSD"), 0, "a dry run writes nothing");
    }

    /// An import stores the day's ticks and its 1m bars; a REPEAT finds the day held, decodes
    /// nothing — the file is garbage by then, and would be refused if it were read — and writes
    /// nothing.
    #[test]
    fn an_import_stores_ticks_and_bars_and_a_repeat_decodes_nothing() {
        let project = Project::new();
        let path = project.daily("EURUSD", 15, &a_day(1));
        let mut client = DatahubClient::connect(project.serve()).expect("handshake");

        let done = client.import_archive(&spec("EURUSD", 15, 15, false, false)).expect("an import");
        match &results(&done)[&jan(15)] {
            DayResult::Imported { ticks, bars } => {
                assert_eq!(*ticks, 3);
                assert_eq!((bars[0].interval.as_str(), bars[0].rows), ("1m", 2));
            }
            other => panic!("expected Imported, got {other:?}"),
        }
        assert_eq!((project.quotes("EURUSD"), project.bars("EURUSD", "1m")), (3, 2));

        std::fs::write(&path, b"not an lzma stream at all").unwrap();
        let again = client.import_archive(&spec("EURUSD", 15, 15, false, false)).expect("a repeat");
        assert_eq!(again.plan.days[0].class, DayClass::HeldByArchive);
        match &results(&again)[&jan(15)] {
            DayResult::ToppedUp { bars } => assert_eq!(bars[0].rows, 0, "nothing was missing"),
            other => panic!("a held day must be topped up, never decoded: {other:?}"),
        }
        assert_eq!((project.quotes("EURUSD"), project.bars("EURUSD", "1m")), (3, 2), "one copy");
    }

    /// `verify` decodes every importable day and writes NOTHING: a good file verifies, an
    /// hour-relative payload filed under a day name is refused as the import would refuse it, and an
    /// empty file is refused as storing nothing — with no key spent, so it stays FREE.
    #[test]
    fn verify_decodes_writes_nothing_and_reports_what_the_import_would_refuse() {
        let project = Project::new();
        project.daily("EURUSD", 15, &a_day(1));
        // Every tick in the first hour: exactly what one hour-relative payload looks like.
        project.daily("EURUSD", 16, &bi5(&[(1_000, 109_469, 109_465), (2_000, 109_470, 109_466)]));
        project.daily("EURUSD", 17, b"");
        let mut client = DatahubClient::connect(project.serve()).expect("handshake");

        let done = client.import_archive(&spec("EURUSD", 15, 17, true, true)).expect("a verify");
        let r = results(&done);
        assert_eq!(r[&jan(15)], DayResult::Verified { ticks: 3 });
        assert!(
            matches!(&r[&jan(16)], DayResult::Refused(x) if x.class == "AmbiguousTimeBase"),
            "{r:?}"
        );
        assert!(matches!(&r[&jan(17)], DayResult::Refused(x) if x.class == EMPTY_PAYLOAD), "{r:?}");
        assert_eq!(project.quotes("EURUSD"), 0, "a verify writes nothing");

        // The empty day, imported for real, spends no key: a re-plan still finds it FREE.
        let done = client.import_archive(&spec("EURUSD", 17, 17, false, false)).expect("an import");
        assert!(
            matches!(&results(&done)[&jan(17)], DayResult::Refused(x) if x.class == EMPTY_PAYLOAD)
        );
        let plan = client.import_archive(&spec("EURUSD", 17, 17, true, false)).expect("a plan");
        assert_eq!(plan.plan.days[0].class, DayClass::Free);
    }

    /// `verify`'s cross-check: a day whose prices are a power of ten off the ticks already stored
    /// beside it is refused as a scale mismatch — the error a wrong point value makes.
    #[test]
    fn verify_refuses_a_day_a_power_of_ten_off_the_stored_neighbour() {
        let project = Project::new();
        project.daily("EURUSD", 15, &a_day(1));
        project.daily("EURUSD", 16, &a_day(10));
        let mut client = DatahubClient::connect(project.serve()).expect("handshake");
        client.import_archive(&spec("EURUSD", 15, 15, false, false)).expect("the neighbour lands");

        let done = client.import_archive(&spec("EURUSD", 16, 16, true, true)).expect("a verify");
        assert!(
            matches!(&results(&done)[&jan(16)], DayResult::Refused(x) if x.class == SCALE_MISMATCH),
            "{:?}",
            results(&done)
        );
        // ...while a day at the right scale beside it verifies.
        project.daily("EURUSD", 16, &a_day(1));
        let done = client.import_archive(&spec("EURUSD", 16, 16, true, true)).expect("a verify");
        assert_eq!(results(&done)[&jan(16)], DayResult::Verified { ticks: 3 });
    }

    /// An instrument with no measured price scale is refused BEFORE a file is opened, quoting the
    /// vendor's own warning — however valid its files.
    #[test]
    fn an_unscaled_instrument_is_refused_before_anything_is_read() {
        let project = Project::new();
        project.daily("XAUUSD", 15, &a_day(1));
        let mut client = DatahubClient::connect(project.serve()).expect("handshake");
        let err =
            client.import_archive(&spec("XAUUSD", 15, 15, false, false)).expect_err("refused");
        assert!(err.contains("silently applying 100,000"), "{err}");
        assert!(err.contains("Nothing was read"), "{err}");
        assert_eq!(project.quotes("XAUUSD"), 0);
    }

    /// A planted symlink — a year directory pointing at a valid archive OUTSIDE the root — is
    /// skipped, never followed, and imports nothing.
    #[cfg(unix)]
    #[test]
    fn a_planted_symlink_imports_nothing_from_outside_the_root() {
        let project = Project::new();
        let outside = project.tmp.path().join("outside").join("00");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("15_ticks.bi5"), a_day(1)).unwrap();
        std::fs::create_dir_all(project.dataset("EURUSD")).unwrap();
        std::os::unix::fs::symlink(
            outside.parent().unwrap(),
            project.dataset("EURUSD").join("2024"),
        )
        .unwrap();

        let mut client = DatahubClient::connect(project.serve()).expect("handshake");
        let done = client.import_archive(&spec("EURUSD", 15, 15, false, false)).expect("answered");
        assert_eq!(done.plan.dir, DatasetDir::Present);
        assert!(done.plan.days.is_empty(), "the link was not followed: {:?}", done.plan.days);
        let skipped = &done.plan.inventory.skipped;
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].class, vike_datahub_client::archive::EntryClass::Symlink);
        assert_eq!(skipped[0].path, format!("{DUKASCOPY_BI5}/EURUSD/2024"));
        assert_eq!(project.quotes("EURUSD"), 0);
    }

    /// A dataset nobody synced plans as ABSENT, naming the server's own directory.
    #[test]
    fn an_absent_dataset_names_the_servers_directory() {
        let project = Project::new();
        let mut client = DatahubClient::connect(project.serve()).expect("handshake");
        let done = client.import_archive(&spec("EURUSD", 15, 15, true, false)).expect("a plan");
        assert_eq!(done.plan.dir, DatasetDir::Absent);
        assert_eq!(PathBuf::from(&done.plan.server_dir), project.dataset("EURUSD"));
    }
}
