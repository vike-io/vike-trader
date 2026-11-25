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
//!    `HistoryStoreDataFusion` in a temporary directory, with REAL `.bi5` files written by `lzma-rs`: the
//!    plan, the import and its repeat, `verify`, the refusals that keep an uncertain input from
//!    spending a key, and a planted symlink that imports nothing from outside the root. The
//!    `test` job runs it (a test build has every shipped feature on, `backfill-serve` among them).
//!
//! The walk-and-open confinement rows (a symlinked dataset, year and file, a FIFO, a hard link, the
//! swap between the walk and the open) are in `crates/vike-datahub/src/import/mod.rs`'s test module,
//! where the walk's own output is reachable; one end-to-end symlink row is repeated here.
//!
//! Nothing here touches a live project folder or a live datahub: every root is a temporary
//! directory, every server an ephemeral loopback port.

use std::assert_matches;
use std::io::Read;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use vike_data::archive_day::ArchiveDayRefusal;
use vike_data::{HistoryStore, HistoryStoreMemory, SeriesCoverage};
use vike_datahub::import::{ArchiveFormat, ArchiveFormatLayoutFile, ImportLane, ImportSession};
use vike_datahub::server::serve_with_import;
use vike_datahub_client::DatahubClient;
use vike_datahub_client::archive::{ArchiveDayClassWire, ArchiveDayResultWire, ImportSpec};
use vike_datahub_client::proto::{
    FEATURE_ARCHIVE_IMPORT, Request, Response, advertised_import_formats,
};
use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_hub_protocol::frame::{read_frame, write_frame};
use vike_model::MS_PER_DAY;

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
    fn classify_file(&self, rel: &[&str], _now_ms: i64) -> ArchiveFormatLayoutFile {
        match rel {
            [_, name] => name
                .strip_suffix(".day")
                .and_then(|n| n.parse::<i64>().ok())
                .map_or(ArchiveFormatLayoutFile::Other, |n| {
                    ArchiveFormatLayoutFile::Daily(n * DAY)
                }),
            _ => ArchiveFormatLayoutFile::Other,
        }
    }
    fn max_file_bytes(&self) -> u64 {
        1024
    }
    fn header_len(&self) -> usize {
        0
    }
    fn read_header(
        &self,
        _file_len: u64,
        _prefix: &[u8],
    ) -> Result<Option<u64>, ArchiveDayRefusal> {
        Ok(None)
    }
    fn session<'a>(
        &'a self,
        _spec: &ImportSpec,
        _now_ms: i64,
    ) -> Result<Box<dyn ImportSession + 'a>, String> {
        Ok(Box::new(FakeSession(&self.0)))
    }
}

struct FakeSession<'a>(&'a FakeStore);

impl ImportSession for FakeSession<'_> {
    fn series(&self) -> Option<SeriesCoverage> {
        None
    }
    fn classify(&self, _day: i64) -> ArchiveDayClassWire {
        ArchiveDayClassWire::Free
    }
    fn import_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResultWire, String> {
        if let Some((parked, release)) = self.0.park.lock().unwrap().take() {
            parked.send(()).unwrap();
            release.recv_timeout(WAIT).map_err(|_| "never released".to_string())?;
        }
        let bytes = read().map_err(|r| r.detail)?;
        self.0.imported.lock().unwrap().push(day);
        Ok(ArchiveDayResultWire::Imported {
            ticks: bytes.len() as u64,
            bars: Vec::new(),
            report: None,
        })
    }
    fn verify_day(
        &mut self,
        _day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResultWire, String> {
        let ticks = read().map_err(|r| r.detail)?.len() as u64;
        Ok(ArchiveDayResultWire::Verified { ticks, report: None })
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
fn serve_lane(lane: ImportLane, keys: Option<HubKeys>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("addr");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
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
        replace_holed: false,
        target: None,
    }
}

fn keys() -> HubKeys {
    HubKeys::new(b"observe-key".to_vec(), b"control-key".to_vec())
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
    assert_matches!(read_frame::<_, Response>(&mut stream), Ok(Response::Pong));
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
    let mut buffer = Vec::new();
    let read = stream.read_to_end(&mut buffer);
    assert!(
        read.is_ok() && buffer.is_empty(),
        "no reply is written to a gone client: {read:?} {buffer:?}"
    );
    assert_eq!(*store.imported.lock().unwrap(), vec![MON], "one day, then the boundary");
}

// ---- 2. the real Dukascopy format, a real store -------------------------------------------------

#[cfg(feature = "backfill-serve")]
mod real_format {
    use std::assert_matches;
    use std::collections::BTreeMap;

    use vike_data::TimestampRange;
    use vike_data::archive_day::DUKASCOPY_BI5_FORMAT;
    use vike_datahub::import::formats::{EMPTY_PAYLOAD, SCALE_MISMATCH, real_import_registry};
    use vike_datahub_client::archive::ImportDatasetDirectory;
    use vike_store::HistoryStoreDataFusion;

    use super::*;

    /// A project in a temporary directory: an imports root, a real store beside it, and the kept
    /// dukascopy instrument list the format reads a dataset's price scale from: EUR/USD alone, at the
    /// price step its row states (0.00001), as the datahub keeps it from the sidecar's login.
    struct Project {
        tmp: tempfile::TempDir,
        store: Arc<HistoryStoreDataFusion>,
    }

    impl Project {
        fn new() -> Project {
            let tmp = tempfile::tempdir().expect("a temp dir");
            let store =
                Arc::new(HistoryStoreDataFusion::open(tmp.path().join("hist")).expect("a store"));
            let eurusd = vike_model::Instrument::new(
                "dukascopy",
                "EURUSD",
                "EUR",
                "USD",
                vike_model::Contract::Fx(vike_model::ContractFx::default()),
            )
            .with_properties(vike_model::SymbolProperties {
                tick_size: 0.00001,
                ..vike_model::SymbolProperties::default()
            });
            // A share CFD as the SDK names it, whose `.bi5` dataset is `AAPLUSUSD` and whose user (and store)
            // name is `AAPL.CFD` (decision 0290).
            let aapl = vike_model::Instrument::new(
                "dukascopy",
                "AAPL.US/USD",
                "",
                "USD",
                vike_model::Contract::Cfd(vike_model::ContractCfd {
                    underlying_kind: vike_model::ContractCfdUnderlying::Share,
                    underlying: None,
                    product: vike_model::ContractCfdProduct::Cfd,
                    expiry: vike_model::ContractCfdExpiry::Unknown,
                    margin_rate: None,
                    swap_long: None,
                    swap_short: None,
                }),
            )
            .with_properties(vike_model::SymbolProperties {
                tick_size: 0.00001,
                ..vike_model::SymbolProperties::default()
            });
            let list = vike_catalog::VenueInstrumentList {
                venue: "dukascopy".to_string(),
                refreshed_ms: Some(1),
                truncated: false,
                last_error: None,
                last_try_ms: Some(1),
                instruments: vec![eurusd, aapl],
                added_by_operator: Vec::new(),
            };
            vike_datahub::instrument_lists::write_list(&tmp.path().join("instruments"), &list)
                .expect("the kept list");
            Project { tmp, store }
        }
        fn root(&self) -> PathBuf {
            self.tmp.path().join("imports")
        }
        fn dataset(&self, symbol: &str) -> PathBuf {
            self.root().join(DUKASCOPY_BI5_FORMAT).join(symbol)
        }
        /// Write `bytes` as `symbol`'s daily file for 2024-01-`drawdown` — the vendor's layout, January
        /// being month `00`.
        fn daily(&self, symbol: &str, drawdown: u32, bytes: &[u8]) -> PathBuf {
            let dir = self.dataset(symbol).join("2024").join("00");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("{drawdown:02}_ticks.bi5"));
            std::fs::write(&path, bytes).unwrap();
            path
        }
        fn serve(&self) -> SocketAddr {
            self.serve_lane(|lane| lane)
        }
        /// [`Self::serve`] over the lane `edit` returns: one with a download mounted.
        fn serve_lane(&self, edit: impl FnOnce(ImportLane) -> ImportLane) -> SocketAddr {
            let lane = edit(ImportLane::new(
                self.root(),
                real_import_registry(
                    Arc::clone(&self.store),
                    Some(self.tmp.path().join("instruments")),
                ),
            ));
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let addr = listener.local_addr().expect("addr");
            let served: Arc<dyn HistoryStore + Send + Sync> = self.store.clone();
            let lane = Arc::new(lane);
            thread::spawn(move || {
                let _ =
                    serve_with_import(listener, served, None, None, None, None, None, Some(lane));
            });
            addr
        }
        fn quotes(&self, symbol: &str) -> usize {
            self.store.scan_quotes("dukascopy", symbol, TimestampRange::all()).expect("scan").len()
        }
        fn bars(&self, symbol: &str, interval: &str) -> usize {
            self.store
                .load_bars("dukascopy", symbol, interval, TimestampRange::all())
                .expect("load")
                .len()
        }
    }

    /// 2024-01-`drawdown` as the epoch-ms of its UTC midnight.
    fn jan(drawdown: u32) -> i64 {
        MON + (i64::from(drawdown) - 15) * DAY
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
            format: DUKASCOPY_BI5_FORMAT.to_string(),
            dataset: symbol.to_string(),
            from_day: Some(jan(from)),
            to_day: Some(jan(to)),
            bars: vec!["1m".to_string()],
            dry_run,
            verify,
            replace_holed: false,
            target: None,
        }
    }

    fn results(
        done: &vike_datahub_client::archive::ImportDone,
    ) -> BTreeMap<i64, ArchiveDayResultWire> {
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
        for drawdown in [18, 19] {
            let hour_dir =
                project.dataset("EURUSD").join("2024").join("00").join(format!("{drawdown:02}"));
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
        assert_eq!(
            days[0],
            (jan(15), ArchiveDayClassWire::Free, Some(3)),
            "3 records declared by the header"
        );
        assert_eq!(days[1], (jan(16), ArchiveDayClassWire::Free, Some(3)));
        assert_matches!(&days[2].1, ArchiveDayClassWire::Refused(r) if r.class == "MixedLayout", "{days:?}");
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
            ArchiveDayResultWire::Imported { ticks, bars, report: None } => {
                assert_eq!(*ticks, 3);
                assert_eq!((bars[0].interval.as_str(), bars[0].rows), ("1m", 2));
            }
            other => panic!("expected Imported, got {other:?}"),
        }
        assert_eq!((project.quotes("EURUSD"), project.bars("EURUSD", "1m")), (3, 2));

        std::fs::write(&path, b"not an lzma stream at all").unwrap();
        let again = client.import_archive(&spec("EURUSD", 15, 15, false, false)).expect("a repeat");
        assert_eq!(again.plan.days[0].class, ArchiveDayClassWire::HeldByArchive);
        match &results(&again)[&jan(15)] {
            ArchiveDayResultWire::ToppedUp { bars } => {
                assert_eq!(bars[0].rows, 0, "nothing was missing")
            }
            other => panic!("a held day must be topped up, never decoded: {other:?}"),
        }
        assert_eq!((project.quotes("EURUSD"), project.bars("EURUSD", "1m")), (3, 2), "one copy");
    }

    /// **`--replace-holed`** (decision 0268, point 4) over a day a `fetch --days N` cut: its ragged key
    /// holds the 15th from 10:00:00.500 on, two ticks of its own and the one whole 1m bucket they give
    /// (14:30), so the 10:00 seam bucket has no bar. A plain import refuses the day as `Overlapped` and
    /// a replacing dry run lists it, both writing nothing; the replacing import stores the file's day
    /// whole, ticks and 1m bars with the seam bar, under the archive key, which owns the day from then
    /// on. The 16th, which an HTTP key holds whole, is never replaced.
    #[test]
    fn a_ragged_day_is_replaced_whole_only_when_asked_and_a_whole_day_never() {
        use vike_backfill::venues::dukascopy::{
            archive_quote_commit_key, ingest_quotes, quote_commit_key, resample_and_store_bars,
        };
        use vike_data::SeriesId;
        use vike_marketdata::TickQuote;
        use vike_model::StoreKindId;

        let project = Project::new();
        project.daily("EURUSD", 15, &a_day(1));
        project.daily("EURUSD", 16, &a_day(1));
        let hist = project.store.as_ref();
        let http_tick = |timestamp: i64| TickQuote {
            timestamp,
            local_timestamp: 0,
            bid: 1.2,
            ask: 1.2001,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: "EURUSD".into(),
        };
        let (cut, end) = (jan(15) + 36_000_500, jan(16) - 1);
        let ragged_ticks = [http_tick(jan(15) + 36_001_000), http_tick(jan(15) + 52_200_000)];
        ingest_quotes(hist, "EURUSD", cut, end, &ragged_ticks).unwrap();
        let whole_buckets = TimestampRange::of(jan(15) + 36_060_000, end);
        resample_and_store_bars(hist, "EURUSD", "1m", whole_buckets).unwrap();
        let whole_ticks = [http_tick(jan(16) + 36_000_000), http_tick(jan(16) + 52_200_000)];
        ingest_quotes(hist, "EURUSD", jan(16), jan(17) - 1, &whole_ticks).unwrap();
        let ragged = quote_commit_key("EURUSD", cut, end);

        let day = |start: i64| TimestampRange::of(start, start + DAY - 1);
        let ticks = |start: i64| hist.scan_quotes("dukascopy", "EURUSD", day(start)).unwrap();
        let bars = |start: i64| hist.load_bars("dukascopy", "EURUSD", "1m", day(start)).unwrap();
        let bar_series =
            SeriesId::per_symbol(StoreKindId::Bar, "dukascopy", "EURUSD", Some("1m".into()));
        let stored_bars = || hist.series_facts(&bar_series).unwrap().0.rows;
        let replacing =
            |dry_run| ImportSpec { replace_holed: true, ..spec("EURUSD", 15, 16, dry_run, false) };
        let mut client = DatahubClient::connect(project.serve()).expect("handshake");

        // No flag: the day is refused, as it always was, and nothing is written.
        let plain = client.import_archive(&spec("EURUSD", 15, 16, false, false)).expect("import");
        let overlapped = ArchiveDayClassWire::Overlapped { keys: vec![ragged.clone()] };
        assert_eq!(plain.plan.days[0].class, overlapped);
        assert_matches!(
            &results(&plain)[&jan(15)],
            ArchiveDayResultWire::Refused(refusal) if refusal.class == "Overlapped"
        );
        assert_eq!((ticks(jan(15)).len(), bars(jan(15)).len()), (2, 1));

        // The dry run lists the day it would replace, with the milliseconds its key covers, and
        // writes nothing; the whole day is not listed.
        let listed = client.import_archive(&replacing(true)).expect("a dry run");
        let replace = ArchiveDayClassWire::Replace {
            keys: vec![ragged.clone()],
            stored: (end - cut + 1) as u64,
            full: DAY as u64,
        };
        assert_eq!(listed.plan.days[0].class, replace);
        assert_eq!(listed.plan.days[1].class, ArchiveDayClassWire::HeldByHttp);
        assert!(listed.outcome.is_none());
        assert_eq!((ticks(jan(15)).len(), bars(jan(15)).len()), (2, 1));
        let bars_before = stored_bars();

        let done = client.import_archive(&replacing(false)).expect("a replacing import");
        match &results(&done)[&jan(15)] {
            ArchiveDayResultWire::Imported { ticks: 3, bars, report: Some(report) } => {
                assert_eq!((bars[0].interval.as_str(), bars[0].rows), ("1m", 2));
                assert!(report.contains("replaced the day whole"), "{report}");
            }
            other => panic!("the ragged day is replaced: {other:?}"),
        }
        let replaced = ticks(jan(15));
        assert!(replaced.len() == 3 && replaced.iter().all(|q| q.bid < 1.1), "{replaced:?}");
        let day_bars = bars(jan(15));
        let times: Vec<i64> = day_bars.iter().map(|b| b.timestamp - jan(15)).collect();
        assert_eq!(times, vec![36_000_000, 52_200_000], "the 10:00 seam bar is there");
        assert!(day_bars.iter().all(|b| b.close < 1.1), "the file's bars: {day_bars:?}");
        assert_eq!(stored_bars(), bars_before + 1, "the HTTP bar went, two came: no stale copy");
        let kept: Vec<f64> = ticks(jan(16)).iter().map(|q| q.bid).collect();
        assert_eq!(kept, vec![1.2, 1.2], "the whole day stays the HTTP lane's");
        let quote_series = SeriesId::per_symbol(StoreKindId::Quote, "dukascopy", "EURUSD", None);
        let keys = hist.series_commits(&quote_series).unwrap();
        let archive = archive_quote_commit_key("EURUSD", jan(15), end);
        assert!(keys.contains(&archive) && keys.contains(&ragged), "both spent: {keys:?}");

        let again = client.import_archive(&replacing(false)).expect("a repeat");
        assert_eq!(again.plan.days[0].class, ArchiveDayClassWire::HeldByArchive);
        assert_eq!(ticks(jan(15)).len(), 3);
    }

    /// **A dataset is the vendor's folder, and its series is the kept row's STORE name** (decision 0290): the
    /// files of `AAPLUSUSD` import into `AAPL.CFD`, the ticks stamped with it, and nothing is filed under the
    /// folder's name.
    #[test]
    fn a_bi5_dataset_imports_into_its_rows_store_name() {
        let project = Project::new();
        project.daily("AAPLUSUSD", 15, &a_day(1));
        let mut client = DatahubClient::connect(project.serve()).expect("handshake");

        let done =
            client.import_archive(&spec("AAPLUSUSD", 15, 15, false, false)).expect("an import");
        assert_matches!(&results(&done)[&jan(15)], ArchiveDayResultWire::Imported { ticks: 3, .. });
        assert_eq!((project.quotes("AAPL.CFD"), project.bars("AAPL.CFD", "1m")), (3, 2));
        let stamped = project
            .store
            .scan_quotes("dukascopy", "AAPL.CFD", TimestampRange::all())
            .expect("scan");
        assert!(stamped.iter().all(|q| q.symbol == "AAPL.CFD"), "{stamped:?}");
        assert_eq!(project.quotes("AAPLUSUSD"), 0, "the folder's name is no series");
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
        assert_eq!(r[&jan(15)], ArchiveDayResultWire::Verified { ticks: 3, report: None });
        assert_matches!(
            &r[&jan(16)], ArchiveDayResultWire::Refused(x) if x.class == "AmbiguousTimeBase",
            "{r:?}"
        );
        assert_matches!(
            &r[&jan(17)],
            ArchiveDayResultWire::Refused(x) if x.class == EMPTY_PAYLOAD && x.detail.contains("per the vendor"),
            "Dukascopy's own reading of an empty file: {r:?}"
        );
        assert_eq!(project.quotes("EURUSD"), 0, "a verify writes nothing");

        // The empty day, imported for real, spends no key: a re-plan still finds it FREE.
        let done = client.import_archive(&spec("EURUSD", 17, 17, false, false)).expect("an import");
        assert_matches!(
            &results(&done)[&jan(17)], ArchiveDayResultWire::Refused(x) if x.class == EMPTY_PAYLOAD
        );
        let plan = client.import_archive(&spec("EURUSD", 17, 17, true, false)).expect("a plan");
        assert_eq!(plan.plan.days[0].class, ArchiveDayClassWire::Free);
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
        assert_matches!(
            &results(&done)[&jan(16)], ArchiveDayResultWire::Refused(x) if x.class == SCALE_MISMATCH,
            "{:?}",
            results(&done)
        );
        // ...while a day at the right scale beside it verifies.
        project.daily("EURUSD", 16, &a_day(1));
        let done = client.import_archive(&spec("EURUSD", 16, 16, true, true)).expect("a verify");
        assert_eq!(
            results(&done)[&jan(16)],
            ArchiveDayResultWire::Verified { ticks: 3, report: None }
        );
    }

    /// An instrument the kept list has no row for (so no price scale) is refused BEFORE a file is
    /// opened, quoting the vendor's own warning — however valid its files.
    #[test]
    fn an_unscaled_instrument_is_refused_before_anything_is_read() {
        // no kept list at all (no dukascopy account has logged in): every dataset is refused with the one line that says
        // what to do, before anything is read
        let bare = Project::new();
        std::fs::remove_dir_all(bare.tmp.path().join("instruments")).expect("the kept list goes");
        bare.daily("EURUSD", 15, &a_day(1));
        let mut client = DatahubClient::connect(bare.serve()).expect("handshake");
        let err =
            client.import_archive(&spec("EURUSD", 15, 15, false, false)).expect_err("no list");
        assert!(err.contains("no Dukascopy instrument list yet"), "{err}");
        assert!(err.contains("vike-cli account add --venue dukascopy"), "{err}");
        assert_eq!(bare.quotes("EURUSD"), 0);

        let project = Project::new();
        project.daily("XAUUSD", 15, &a_day(1));
        let mut client = DatahubClient::connect(project.serve()).expect("handshake");
        let err =
            client.import_archive(&spec("XAUUSD", 15, 15, false, false)).expect_err("refused");
        assert!(err.contains("silently applying 100,000"), "{err}");
        assert!(err.contains("Nothing was read"), "{err}");
        assert_eq!(project.quotes("XAUUSD"), 0);
    }

    /// **A kept row with no price step is never a scale, and never beats the operator's row** (the CI box's list of
    /// 2026-11-12: EUR/USD at step 0.0, IMP-D2). Alone it is refused as a dataset with no row. The operator's row
    /// stating the step, folded in by `with_operator_rows`, takes its place and the dataset is admitted at that
    /// step; so is an operator row spelled another way (`EURUSD` beside the venue's `EUR/USD`).
    #[test]
    fn a_kept_row_with_no_price_step_gives_way_to_the_operators_row() {
        use vike_datahub::instrument_lists::{with_operator_rows, write_list};
        let row = |symbol: &str, tick_size: f64| {
            vike_model::Instrument::new(
                "dukascopy",
                symbol,
                "EUR",
                "USD",
                vike_model::Contract::Fx(vike_model::ContractFx::default()),
            )
            .with_properties(vike_model::SymbolProperties {
                tick_size,
                ..vike_model::SymbolProperties::default()
            })
        };
        let project = Project::new();
        let dir = project.tmp.path().join("instruments");
        let mut kept = vike_catalog::read_kept_list(&dir, "dukascopy").expect("the kept list");
        kept.instruments = vec![row("EUR/USD", 0.0)];
        write_list(&dir, &kept).expect("written");
        project.daily("EURUSD", 15, &a_day(1));
        let mut client = DatahubClient::connect(project.serve()).expect("handshake");
        let plan = |client: &mut DatahubClient| {
            client.import_archive(&spec("EURUSD", 15, 15, true, false))
        };
        let err = plan(&mut client).expect_err("a step of 0 is no scale");
        assert!(err.contains("silently applying 100,000"), "{err}");

        let merged =
            with_operator_rows(Some(kept.clone()), "dukascopy", &[row("EUR/USD", 0.00001)])
                .expect("a list");
        assert_eq!(merged.added_by_operator, ["EUR/USD"]);
        assert_eq!(merged.instruments, [row("EUR/USD", 0.00001)], "the hand row took its place");
        write_list(&dir, &merged).expect("written");
        plan(&mut client).expect("admitted at the operator's step");

        kept.instruments.push(row("EURUSD", 0.00001));
        write_list(&dir, &kept).expect("written");
        plan(&mut client).expect("the step-less row is read past");
        assert_eq!(project.quotes("EURUSD"), 0, "a plan stores nothing");
    }

    /// **An index CFD the venue gives no name is imported once the operator's row names it** (decision 0290's O-6;
    /// the CI box's dukascopy list of 2026-11-18: `USA500.IDX/USD`, base `USA500.IDX`, which is no word). Alone its dataset is
    /// refused in the grammar's words. The operator's row of it (base `SPX500`, folded by `with_operator_rows`) names
    /// the venue's row `SPX500.CFD`: the row keeps its own step, stays a listing row (not an added one), and the dataset
    /// `USA500IDXUSD` is admitted.
    #[test]
    fn an_index_cfd_the_venue_names_not_is_imported_once_the_operators_row_names_it() {
        use vike_datahub::instrument_lists::{split_operator_rows, with_operator_rows, write_list};
        let row = |base: &str| {
            vike_model::Instrument::new(
                "dukascopy",
                "USA500.IDX/USD",
                base,
                "USD",
                vike_model::Contract::Cfd(vike_model::ContractCfd {
                    underlying_kind: vike_model::ContractCfdUnderlying::Index,
                    underlying: None,
                    product: vike_model::ContractCfdProduct::Cfd,
                    expiry: vike_model::ContractCfdExpiry::Rolling,
                    margin_rate: None,
                    swap_long: None,
                    swap_short: None,
                }),
            )
            .with_properties(vike_model::SymbolProperties {
                tick_size: 0.001,
                ..vike_model::SymbolProperties::default()
            })
        };
        let project = Project::new();
        let dir = project.tmp.path().join("instruments");
        let mut kept = vike_catalog::read_kept_list(&dir, "dukascopy").expect("the kept list");
        kept.instruments = vec![row("USA500.IDX")];
        write_list(&dir, &kept).expect("written");
        project.daily("USA500IDXUSD", 15, &a_day(1));
        let mut client = DatahubClient::connect(project.serve()).expect("handshake");
        let plan = |client: &mut DatahubClient| {
            client.import_archive(&spec("USA500IDXUSD", 15, 15, true, false))
        };
        let err = plan(&mut client).expect_err("the venue's row has no name");
        assert!(err.contains("has no user name"), "{err}");

        let merged =
            with_operator_rows(Some(kept.clone()), "dukascopy", &[row("SPX500")]).expect("a list");
        assert!(merged.added_by_operator.is_empty(), "the venue's row is named, not replaced");
        let named = &merged.instruments[0];
        assert_eq!((named.base.as_str(), named.properties.tick_size), ("USA500.IDX", 0.001));
        assert_eq!(vike_catalog::user_name_of_row(named).as_deref(), Ok("SPX500.CFD"));
        let (back, carried) = split_operator_rows(Some(merged.clone()));
        assert!(carried.is_empty(), "a named listing row is no hand row");
        assert_eq!(
            with_operator_rows(back, "dukascopy", &[row("SPX500")]),
            Some(merged.clone()),
            "a second fold changes nothing"
        );
        write_list(&dir, &merged).expect("written");
        plan(&mut client).expect("admitted under the operator's name");
        assert_eq!(project.quotes("SPX500.CFD"), 0, "a plan stores nothing");
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
        assert_eq!(done.plan.dir, ImportDatasetDirectory::Present);
        assert!(done.plan.days.is_empty(), "the link was not followed: {:?}", done.plan.days);
        let skipped = &done.plan.inventory.skipped;
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].class, vike_datahub_client::archive::ImportPlanEntryClass::Symlink);
        assert_eq!(skipped[0].path, format!("{DUKASCOPY_BI5_FORMAT}/EURUSD/2024"));
        assert_eq!(project.quotes("EURUSD"), 0);
    }

    /// A dataset nobody synced plans as ABSENT, naming the server's own directory.
    #[test]
    fn an_absent_dataset_names_the_servers_directory() {
        let project = Project::new();
        let mut client = DatahubClient::connect(project.serve()).expect("handshake");
        let done = client.import_archive(&spec("EURUSD", 15, 15, true, false)).expect("a plan");
        assert_eq!(done.plan.dir, ImportDatasetDirectory::Absent);
        assert_eq!(PathBuf::from(&done.plan.server_dir), project.dataset("EURUSD"));
    }

    /// The second format over the same wire: Binance's daily aggTrades zips, a real store and real
    /// zips. A test writes no compressor: a deflate stream may be one STORED block, and the reader
    /// takes that like any other.
    mod binance {
        use vike_data::archive_day::BINANCE_AGGREGATE_TRADES_FORMAT;
        use vike_marketdata::TickTrade;

        use super::*;

        const SYMBOL: &str = "SOLUSDT.P";

        fn crc32(bytes: &[u8]) -> u32 {
            let mut crc = !0u32;
            for &byte in bytes {
                crc ^= u32::from(byte);
                for _ in 0..8 {
                    crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
                }
            }
            !crc
        }

        /// A one-member zip whose deflate stream is one final STORED block.
        fn zip_of(name: &str, body: &[u8]) -> Vec<u8> {
            let name_len = u16::try_from(name.len()).unwrap().to_le_bytes();
            let block_len = u16::try_from(body.len()).unwrap();
            let mut out = b"PK\x03\x04\x14\x00\x00\x00\x08\x00".to_vec();
            out.extend([0u8; 16]);
            out.extend(name_len);
            out.extend([0u8; 2]);
            out.extend(name.as_bytes());
            out.push(1);
            out.extend(block_len.to_le_bytes());
            out.extend((!block_len).to_le_bytes());
            out.extend(body);
            out.extend(b"PK\x01\x02");
            out.extend([0u8; 12]);
            out.extend(crc32(body).to_le_bytes());
            out.extend([0u8; 8]);
            out.extend(name_len);
            out.extend([0u8; 16]);
            out.extend(name.as_bytes());
            out.extend(b"PK\x05\x06");
            out.extend([0u8; 18]);
            out
        }

        /// `count` rows of `drawdown`, with consecutive ids from `first_id`, one a minute. It opens with the
        /// vendor's header on an even day and has none on an odd one, as its files do over the years.
        fn day_csv(drawdown: u32, first_id: u64, count: u64) -> String {
            let mut out = String::new();
            if drawdown.is_multiple_of(2) {
                out.push_str(
                    "agg_trade_id,price,quantity,first_trade_id,last_trade_id,transact_time,\
                     is_buyer_maker\n",
                );
            }
            for n in 0..count {
                let id = first_id + n;
                let ts = jan(drawdown) + n as i64 * 60_000;
                out.push_str(&format!("{id},89.5,2.0,{id},{id},{ts},true\n"));
            }
            out
        }

        fn place(project: &Project, name: &str, bytes: &[u8]) {
            let dir = project.root().join(BINANCE_AGGREGATE_TRADES_FORMAT).join(SYMBOL);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(name), bytes).unwrap();
        }

        fn place_day(project: &Project, drawdown: u32, csv: &str) {
            let name = format!("SOLUSDT-aggTrades-2024-01-{drawdown:02}");
            place(project, &format!("{name}.zip"), &zip_of(&format!("{name}.csv"), csv.as_bytes()));
        }

        fn spec(from: u32, to: u32, dry_run: bool, verify: bool) -> ImportSpec {
            ImportSpec {
                format: BINANCE_AGGREGATE_TRADES_FORMAT.to_string(),
                dataset: SYMBOL.to_string(),
                from_day: Some(jan(from)),
                to_day: Some(jan(to)),
                bars: Vec::new(),
                dry_run,
                verify,
                replace_holed: false,
                target: None,
            }
        }

        /// **Over the real wire, the candle and the lane read apart.** Both versions of one minute,
        /// with different values: a plain read answers the venue's candle, and a read that names the
        /// lane answers the bar built from trades. The plan says where they go before anything runs,
        /// and a repeat writes no bar twice.
        #[test]
        fn bars_land_in_the_lane_and_a_plain_read_answers_the_candle() {
            let project = Project::new();
            let candle = vike_marketdata::Bar {
                timestamp: jan(15),
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            };
            let key = format!("binance:{SYMBOL}:1m:{}-{}", jan(15), jan(16));
            let one = std::slice::from_ref(&candle);
            project.store.append_bars("binance", SYMBOL, "1m", one, Some(&key)).unwrap();
            place_day(&project, 15, &day_csv(15, 1, 2));

            let mut client = DatahubClient::connect(project.serve()).expect("handshake");
            let plan = client.import_archive(&spec(15, 15, true, false)).expect("a plan").plan;
            assert!(plan.admission.contains("source=aggtrades"), "{}", plan.admission);
            assert!(plan.admission.contains("first fill's time"), "{}", plan.admission);

            let request = ImportSpec { bars: vec!["1m".to_string()], ..spec(15, 15, false, false) };
            let done = client.import_archive(&request).expect("an import");
            let ArchiveDayResultWire::Imported { ticks: 2, bars, .. } = &results(&done)[&jan(15)]
            else {
                panic!("{done:?}");
            };
            assert_eq!(bars[0].rows, 2);

            let plain =
                client.load_bars("binance", SYMBOL, "1m", TimestampRange::all(), None).unwrap();
            assert_eq!(plain, vec![candle], "the plain read is the candle, alone");
            let lane = client
                .load_bars_sourced(
                    "binance",
                    "aggtrades",
                    SYMBOL,
                    "1m",
                    TimestampRange::all(),
                    None,
                )
                .unwrap();
            assert_eq!(lane.len(), 2);
            assert_eq!((lane[0].timestamp, lane[0].open, lane[0].volume), (jan(15), 89.5, 2.0));

            let again = client.import_archive(&request).expect("a repeat");
            let ArchiveDayResultWire::ToppedUp { bars } = &results(&again)[&jan(15)] else {
                panic!("{again:?}");
            };
            assert_eq!(bars[0].rows, 0, "no bar is written twice");
            let lane = client
                .load_bars_sourced(
                    "binance",
                    "aggtrades",
                    SYMBOL,
                    "1m",
                    TimestampRange::all(),
                    None,
                )
                .unwrap();
            assert_eq!(lane.len(), 2);
        }

        /// No bar unless asked; a later request naming bars builds them from the stored trades, with
        /// no file read, which is also how a run killed between the two resumes.
        #[test]
        fn a_held_days_bars_are_built_from_the_stored_trades() {
            let project = Project::new();
            place_day(&project, 15, &day_csv(15, 1, 2));
            let mut client = DatahubClient::connect(project.serve()).expect("handshake");
            client.import_archive(&spec(15, 15, false, false)).expect("an import");
            let none = client
                .load_bars_sourced(
                    "binance",
                    "aggtrades",
                    SYMBOL,
                    "1m",
                    TimestampRange::all(),
                    None,
                )
                .unwrap();
            assert!(none.is_empty(), "no bar unless asked");

            place(&project, "SOLUSDT-aggTrades-2024-01-15.zip", b"ruined after the import");
            let request = ImportSpec { bars: vec!["1m".to_string()], ..spec(15, 15, false, false) };
            let done = client.import_archive(&request).expect("a top-up");
            let ArchiveDayResultWire::ToppedUp { bars } = &results(&done)[&jan(15)] else {
                panic!("{done:?}");
            };
            assert_eq!(bars[0].rows, 2);
        }

        fn trades(project: &Project) -> usize {
            project.store.scan_trades("binance", SYMBOL, TimestampRange::all()).expect("scan").len()
        }

        fn refusal_class(result: &ArchiveDayResultWire) -> &str {
            match result {
                ArchiveDayResultWire::Refused(r) => r.class.as_str(),
                other => panic!("expected a refusal, got {other:?}"),
            }
        }

        /// Daily zips are days. A monthly zip and a `.CHECKSUM` are other objects, never read, and
        /// the admission line says so.
        #[test]
        fn a_monthly_zip_is_an_other_object_and_the_plan_says_so() {
            let project = Project::new();
            place_day(&project, 15, &day_csv(15, 1, 2));
            place(&project, "SOLUSDT-aggTrades-2024-01-15.zip.CHECKSUM", b"abc");
            place(&project, "SOLUSDT-aggTrades-2024-02.zip", b"not even a zip");

            let mut client = DatahubClient::connect(project.serve()).expect("handshake");
            let done = client.import_archive(&spec(15, 15, true, false)).expect("a plan");
            let plan = &done.plan;
            assert_eq!(plan.venue, "binance");
            assert_eq!(plan.inventory.daily_files, 1);
            assert_eq!(plan.inventory.other_objects, 2);
            assert!(plan.admission.contains("monthly"), "{}", plan.admission);
            assert!(plan.admission.contains(SYMBOL), "{}", plan.admission);
            assert_eq!(plan.days[0].class, ArchiveDayClassWire::Free);
            assert_eq!(plan.days[0].declared_ticks, None, "a zip declares no row count");
            assert_eq!(trades(&project), 0, "a dry run writes nothing");
        }

        /// An import stores the rows of a file with a header and one without, under the store's
        /// spelling of the perpetual; a repeat reads no file, even a ruined one.
        #[test]
        fn an_import_stores_trades_and_a_repeat_reads_no_file() {
            let project = Project::new();
            place_day(&project, 15, &day_csv(15, 1, 3));
            place_day(&project, 16, &day_csv(16, 4, 2));
            let mut client = DatahubClient::connect(project.serve()).expect("handshake");
            let done = client.import_archive(&spec(15, 16, false, false)).expect("an import");
            let days = results(&done);
            assert_matches!(&days[&jan(15)], ArchiveDayResultWire::Imported { ticks: 3, .. });
            assert_matches!(&days[&jan(16)], ArchiveDayResultWire::Imported { ticks: 2, .. });
            assert_eq!(trades(&project), 5);
            let bars = project.store.load_bars("binance", SYMBOL, "1m", TimestampRange::all());
            assert_eq!(bars.expect("load").len(), 0, "and no bar among the candles");
            let first: TickTrade =
                project.store.scan_trades("binance", SYMBOL, TimestampRange::all()).unwrap()[0]
                    .clone();
            assert_eq!((first.timestamp, first.local_timestamp, first.size), (jan(15), 0, 2.0));
            assert!(first.is_buyer_maker);

            place(&project, "SOLUSDT-aggTrades-2024-01-15.zip", b"ruined after the import");
            let again = client.import_archive(&spec(15, 16, false, false)).expect("a repeat");
            assert_eq!(again.plan.days[0].class, ArchiveDayClassWire::HeldByArchive);
            assert_matches!(&results(&again)[&jan(15)], ArchiveDayResultWire::ToppedUp { .. });
            assert_eq!(trades(&project), 5);
        }

        /// **One owner per day against the live recorder**: a day its key meets is `Overlapped` on
        /// the plan, its file is never read, and the rows it holds are not stored twice.
        #[test]
        fn a_day_the_recorder_wrote_is_overlapped_and_never_stored_twice() {
            let project = Project::new();
            let tick = TickTrade {
                timestamp: jan(15) + 5,
                local_timestamp: jan(15) + 6,
                price: 89.5,
                size: 1.0,
                is_buyer_maker: false,
                symbol: Default::default(),
            };
            let key =
                format!("live-binance-{SYMBOL}-trade-{}-{}-0", tick.timestamp, tick.timestamp);
            project.store.append_trades("binance", SYMBOL, &[tick], Some(&key)).unwrap();
            place(
                &project,
                "SOLUSDT-aggTrades-2024-01-15.zip",
                b"would be refused if it were read",
            );
            place_day(&project, 16, &day_csv(16, 1, 2));

            let mut client = DatahubClient::connect(project.serve()).expect("handshake");
            let plan = client.import_archive(&spec(15, 16, true, false)).expect("a plan").plan;
            assert_eq!(
                plan.days[0].class,
                ArchiveDayClassWire::Overlapped { keys: vec![key.clone()] }
            );
            assert_eq!(plan.days[1].class, ArchiveDayClassWire::Free);
            assert_eq!(plan.series.expect("the recorder's rows").rows, 1);

            let done = client.import_archive(&spec(15, 16, false, false)).expect("an import");
            let days = results(&done);
            assert_eq!(refusal_class(&days[&jan(15)]), "Overlapped");
            assert_matches!(&days[&jan(16)], ArchiveDayResultWire::Imported { ticks: 2, .. });
            let on_15 = TimestampRange::of(jan(15), jan(16) - 1);
            let rows = project.store.scan_trades("binance", SYMBOL, on_15).unwrap();
            assert_eq!(rows.len(), 1, "the recorded row, and no copy of the day");
            assert_eq!(trades(&project), 3);
        }

        /// A verify writes nothing, passes a clean day, and names a gap inside a day and between two
        /// days in a VERIFIED day's report, never a refusal, since the import then stores it as
        /// published.
        #[test]
        fn a_verify_reports_gaps_and_an_import_stores_the_day_as_published() {
            let project = Project::new();
            place_day(&project, 15, &day_csv(15, 1, 3));
            // Between the two days the ids jump from 3 to 10; inside the 17th from 21 to 23.
            place_day(&project, 16, &day_csv(16, 10, 3));
            let mut gap = day_csv(17, 20, 2);
            gap.push_str(&format!("23,89.5,2.0,23,23,{},true\n", jan(17) + 600_000));
            place_day(&project, 17, &gap);

            let mut client = DatahubClient::connect(project.serve()).expect("handshake");
            let done = client.import_archive(&spec(15, 17, true, true)).expect("a verify");
            let days = results(&done);
            assert_matches!(
                &days[&jan(15)],
                ArchiveDayResultWire::Verified { ticks: 3, report: None }
            );
            let report = |drawdown: u32| match &days[&jan(drawdown)] {
                ArchiveDayResultWire::Verified { ticks: 3, report: Some(report) } => {
                    report.as_str()
                }
                other => panic!("expected a verified day with a report, got {other:?}"),
            };
            assert!(report(16).contains("3 -> 10 (6 missing"), "{}", report(16));
            assert!(report(17).contains("21 -> 23 (1 missing"), "{}", report(17));
            assert!(report(17).contains("An import stores this day"), "{}", report(17));
            assert_eq!(trades(&project), 0, "a verify writes nothing");

            let done = client.import_archive(&spec(15, 17, false, false)).expect("an import");
            assert_matches!(
                &results(&done)[&jan(17)],
                ArchiveDayResultWire::Imported { ticks: 3, .. }
            );
            assert_eq!(trades(&project), 9, "every row, gaps and all");
        }

        /// **The measured case, a file holding every row twice**: a verify and an import each keep
        /// every trade once and say how many rows they dropped, in the day's `report`, and the day
        /// is verified, then imported. A repeat whose row differs refuses its day in both, and spends
        /// no key.
        #[test]
        fn a_doubled_day_stores_each_trade_once_and_says_so_and_a_contradiction_is_refused() {
            let project = Project::new();
            let doubled: String =
                day_csv(15, 1, 3).lines().flat_map(|row| [row, "\n", row, "\n"]).collect();
            place_day(&project, 15, &doubled);
            let mut contradiction = day_csv(17, 1, 3);
            contradiction.push_str(&format!("2,89.5,2.5,2,2,{},true\n", jan(17) + 60_000));
            place_day(&project, 17, &contradiction);
            let dropped = "3 rows repeat an earlier row of the file exactly, and are dropped. An \
                           import stores each of the 3 trades once, as published.";

            let mut client = DatahubClient::connect(project.serve()).expect("handshake");
            let done = client.import_archive(&spec(15, 17, true, true)).expect("a verify");
            let days = results(&done);
            let ArchiveDayResultWire::Verified { ticks: 3, report: Some(report) } = &days[&jan(15)]
            else {
                panic!("{done:?}");
            };
            assert_eq!(report, dropped);
            assert_eq!(refusal_class(&days[&jan(17)]), "ContradictoryRepeat");

            let done = client.import_archive(&spec(15, 17, false, false)).expect("an import");
            let days = results(&done);
            let ArchiveDayResultWire::Imported { ticks: 3, report: Some(report), .. } =
                &days[&jan(15)]
            else {
                panic!("{done:?}");
            };
            assert_eq!(report, dropped);
            assert_eq!(refusal_class(&days[&jan(17)]), "ContradictoryRepeat");
            assert_eq!(trades(&project), 3, "each trade once, and nothing of the 17th");
        }

        /// A skipped trade id alone is what a real file normally holds, so a verify passes the day: a
        /// report on every day would be read by nobody, and it would say an import refuses what it stores.
        #[test]
        fn a_skipped_trade_id_alone_passes_a_verify() {
            let project = Project::new();
            let (a, b) = (jan(15), jan(15) + 60_000);
            let csv = format!("1,89.5,2.0,100,100,{a},true\n2,89.5,2.0,102,102,{b},true\n");
            place_day(&project, 15, &csv);
            let mut client = DatahubClient::connect(project.serve()).expect("handshake");
            let done = client.import_archive(&spec(15, 15, true, true)).expect("a verify");
            assert_matches!(
                &results(&done)[&jan(15)],
                ArchiveDayResultWire::Verified { ticks: 2, report: None }
            );
        }

        /// A zip with no row is refused in this format's words, never Dukascopy's "per the vendor",
        /// and spends no key.
        #[test]
        fn an_empty_file_is_refused_in_this_formats_words() {
            let project = Project::new();
            place_day(&project, 16, &day_csv(16, 1, 0));
            let mut client = DatahubClient::connect(project.serve()).expect("handshake");
            for (dry_run, verify) in [(true, true), (false, false)] {
                let done =
                    client.import_archive(&spec(16, 16, dry_run, verify)).expect("an answer");
                let ArchiveDayResultWire::Refused(r) = &results(&done)[&jan(16)] else {
                    panic!("{done:?}")
                };
                assert_eq!(r.class, EMPTY_PAYLOAD);
                assert_eq!(
                    r.detail,
                    "the file holds no row. Nothing was stored and no commit key was spent, so the \
                     day stays free: download the file again and check it with its .CHECKSUM.",
                    "this format's own words, never Dukascopy's"
                );
            }
            assert_eq!(trades(&project), 0);
        }

        /// A row that does not parse refuses its day and spends no key; the other days import, and
        /// a fixed file then lands. Another instrument's file is refused by what it holds.
        #[test]
        fn a_bad_file_refuses_its_day_and_spends_no_key() {
            let project = Project::new();
            place_day(&project, 15, "1,89.5,2.0,1,1,not-a-time,true\n");
            place_day(&project, 16, &day_csv(16, 1, 2));
            let foreign = zip_of("BTCUSDT-aggTrades-2024-01-17.csv", day_csv(17, 1, 1).as_bytes());
            place(&project, "SOLUSDT-aggTrades-2024-01-17.zip", &foreign);

            let mut client = DatahubClient::connect(project.serve()).expect("handshake");
            let done = client.import_archive(&spec(15, 17, false, false)).expect("an import");
            let days = results(&done);
            assert_eq!(refusal_class(&days[&jan(15)]), "BadRow");
            assert_matches!(&days[&jan(16)], ArchiveDayResultWire::Imported { ticks: 2, .. });
            assert_eq!(refusal_class(&days[&jan(17)]), "ForeignMember");
            assert_eq!(trades(&project), 2);

            place_day(&project, 15, &day_csv(15, 7, 2));
            let done = client.import_archive(&spec(15, 15, false, false)).expect("a retry");
            assert_matches!(
                &results(&done)[&jan(15)],
                ArchiveDayResultWire::Imported { ticks: 2, .. }
            );
            assert_eq!(trades(&project), 4);
        }

        /// **BIN-2: one grammar with the klines format.** The dataset is the store's name of the
        /// perpetual (`SOLUSDT.P`): the vendor's bare symbol is the SPOT pair's name there, so it is
        /// refused with the perpetual's spelling, and so is a dated future's, before any directory is
        /// touched.
        #[test]
        fn a_dataset_that_is_not_a_perpetuals_store_name_is_refused() {
            let project = Project::new();
            let mut client = DatahubClient::connect(project.serve()).expect("handshake");
            for dataset in ["SOLUSDT", "BTCUSDT_250328.P"] {
                let request =
                    ImportSpec { dataset: dataset.to_string(), ..spec(15, 15, true, false) };
                let refusal = client.import_archive(&request).expect_err(dataset).to_string();
                assert!(refusal.contains("USDⓈ-M perpetual"), "{refusal}");
            }
            let request =
                ImportSpec { dataset: "SOLUSDT".to_string(), ..spec(15, 15, true, false) };
            let refusal = client.import_archive(&request).expect_err("spot").to_string();
            assert!(refusal.contains("spot pair") && refusal.contains(SYMBOL), "{refusal}");
        }
    }

    /// **The archive DOWNLOAD, over the real wire, against a loopback stand-in for the bucket** (the
    /// owner's ruling of 2026-11-13): `Request::FetchArchive` lists, plans and fetches into the
    /// dataset's folder with the real S3 client and its signature, and the import then reads the
    /// files unchanged. The stand-in answers `ListObjectsV2` and `GetObject` the way S3 does, and
    /// refuses with S3's own 403 any request whose `Authorization` is not a SigV4 header for the
    /// stored key id and the bucket's region, or that does not say the requester pays.
    mod download {
        use std::io::{BufRead, BufReader, Write};

        use vike_datahub::import::download::{
            ACCESS_KEY_ID_NAME, DukascopyDownload, SECRET_ACCESS_KEY_NAME,
        };
        use vike_datahub::tool_feeds::{ToolFeedKeyRead, ToolFeedKeyReader};
        use vike_datahub_client::archive_fetch::FetchSpec;
        use vike_datahub_client::proto::FEATURE_ARCHIVE_FETCH;
        use vike_model::secret::Secret;

        use super::*;

        const KEY_ID: &str = "AKIDTESTFIXTURE";

        /// The key the reader answers: both names, or neither.
        struct Keys(bool);

        impl ToolFeedKeyReader for Keys {
            fn read(&self, name: &str) -> ToolFeedKeyRead {
                match (self.0, name) {
                    (true, ACCESS_KEY_ID_NAME) => {
                        ToolFeedKeyRead::Present(Secret::new(KEY_ID.to_string()))
                    }
                    (true, SECRET_ACCESS_KEY_NAME) => {
                        ToolFeedKeyRead::Present(Secret::new("fixture-secret".to_string()))
                    }
                    _ => ToolFeedKeyRead::Absent,
                }
            }
        }

        /// The stand-in bucket: its objects by key, and every request line it was sent.
        struct Bucket {
            base: String,
            seen: Arc<Mutex<Vec<String>>>,
        }

        /// `%2F` and friends back to bytes: the listing's prefix arrives encoded.
        fn decode(s: &str) -> String {
            let mut out = Vec::new();
            let b = s.as_bytes();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' && i + 2 < b.len() {
                    let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap();
                    out.push(u8::from_str_radix(hex, 16).unwrap());
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            String::from_utf8(out).unwrap()
        }

        fn answer(stream: &mut TcpStream, status: &str, body: &[u8]) {
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(body).unwrap();
        }

        fn bucket(objects: Vec<(String, Vec<u8>)>) -> Bucket {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let base = format!("http://{}", listener.local_addr().unwrap());
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::clone(&seen);
            let objects = Arc::new(objects);
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let (objects, log) = (Arc::clone(&objects), Arc::clone(&log));
                    thread::spawn(move || {
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        let mut headers = Vec::new();
                        loop {
                            let mut h = String::new();
                            reader.read_line(&mut h).unwrap();
                            if h.trim().is_empty() {
                                break;
                            }
                            headers.push(h.trim().to_ascii_lowercase());
                        }
                        let target = line.split_whitespace().nth(1).unwrap_or("").to_string();
                        log.lock().unwrap().push(target.clone());
                        let signed = headers.iter().any(|h| {
                            h.starts_with(&format!(
                                "authorization: aws4-hmac-sha256 credential={}/",
                                KEY_ID.to_ascii_lowercase()
                            )) && h.contains("/eu-west-1/s3/aws4_request, signedheaders=host;x-amz-content-sha256;x-amz-date;x-amz-request-payer, signature=")
                        });
                        let pays = headers.iter().any(|h| h == "x-amz-request-payer: requester");
                        if !(signed && pays) {
                            let body = b"<Error><Code>AccessDenied</Code></Error>";
                            return answer(&mut stream, "403 Forbidden", body);
                        }
                        if let Some(query) = target.strip_prefix("/?") {
                            let prefix = query
                                .split('&')
                                .find_map(|p| p.strip_prefix("prefix="))
                                .map(decode)
                                .unwrap_or_default();
                            let mut xml = String::from(
                                "<?xml version=\"1.0\"?><ListBucketResult><IsTruncated>false</IsTruncated>",
                            );
                            for (key, bytes) in
                                objects.iter().filter(|(k, _)| k.starts_with(&prefix))
                            {
                                xml.push_str(&format!(
                                    "<Contents><Key>{key}</Key><Size>{}</Size></Contents>",
                                    bytes.len()
                                ));
                            }
                            xml.push_str("</ListBucketResult>");
                            return answer(&mut stream, "200 OK", xml.as_bytes());
                        }
                        let key = decode(target.trim_start_matches('/'));
                        match objects.iter().find(|(k, _)| *k == key) {
                            Some((_, bytes)) => answer(&mut stream, "200 OK", bytes),
                            None => answer(
                                &mut stream,
                                "404 Not Found",
                                b"<Error><Code>NoSuchKey</Code></Error>",
                            ),
                        }
                    });
                }
            });
            Bucket { base, seen }
        }

        fn fetch(from: u32, to: u32, go: bool) -> FetchSpec {
            FetchSpec {
                format: DUKASCOPY_BI5_FORMAT.to_string(),
                dataset: "EURUSD".to_string(),
                from_day: jan(from),
                to_day: jan(to),
                go,
                concurrency: 4,
            }
        }

        /// List, plan, fetch, then import: the window crosses a month (two prefixes), a day out of
        /// the window, an hourly file and a day already in the folder are never fetched, the files
        /// land where the import reads them, and the import stores their ticks.
        #[test]
        fn list_plan_fetch_then_import() {
            let project = Project::new();
            // January 31 and February 1 (month `01`, zero-based) in the bucket; February 2 is out
            // of the window; an hourly file of January 30 is no daily file.
            let bucket = bucket(vec![
                ("EURUSD/2024/00/31_ticks.bi5".to_string(), a_day(1)),
                ("EURUSD/2024/01/01_ticks.bi5".to_string(), a_day(1)),
                ("EURUSD/2024/01/02_ticks.bi5".to_string(), a_day(1)),
                ("EURUSD/2024/00/30/10h_ticks.bi5".to_string(), a_day(1)),
                ("EURUSD/2024/00/29_ticks.bi5".to_string(), a_day(1)),
            ]);
            // January 29 is in the folder already.
            project.daily("EURUSD", 29, &a_day(1));
            let download = DukascopyDownload::new(&bucket.base, "eu-west-1", Arc::new(Keys(true)));
            let addr = project.serve_lane(|lane| lane.with_download(Box::new(download)));
            let mut client = DatahubClient::connect(addr).expect("handshake");
            assert!(client.features().iter().any(|f| f == FEATURE_ARCHIVE_FETCH));

            let plan = client.fetch_archive(&fetch(29, 32, false)).expect("a plan");
            assert!(plan.fetched.is_none(), "a plan downloads nothing");
            let plan = plan.plan;
            assert_eq!((plan.listed, plan.held, plan.list_requests), (3, 1, 2), "{plan:?}");
            let missing: Vec<i64> = plan.missing.iter().map(|f| f.day).collect();
            assert_eq!(missing, vec![jan(31), jan(32)]);
            assert!(!project.dataset("EURUSD").join("2024").join("01").exists(), "nothing written");
            let gets = |seen: &[String]| seen.iter().filter(|t| !t.starts_with("/?")).count();
            assert_eq!(gets(&bucket.seen.lock().unwrap()), 0, "a plan sends no GET");

            // One request per month, as the CLI sends them.
            let jan_done = client.fetch_archive(&fetch(29, 31, true)).expect("January");
            let feb_done = client.fetch_archive(&fetch(32, 32, true)).expect("February");
            for done in [&jan_done, &feb_done] {
                let got = done.fetched.as_ref().expect("an outcome");
                assert_eq!(
                    (got.fetched, got.get_requests, got.failed.len()),
                    (1, 1, 0),
                    "{done:?}"
                );
            }
            assert_eq!(gets(&bucket.seen.lock().unwrap()), 2, "only the two missing files");
            let again = client.fetch_archive(&fetch(29, 32, false)).expect("a second plan");
            assert!(again.plan.missing.is_empty(), "every file is in the folder now");

            let imported =
                client.import_archive(&spec("EURUSD", 29, 32, false, false)).expect("import");
            for day in [29, 31, 32] {
                assert_matches!(
                    results(&imported)[&jan(day)],
                    ArchiveDayResultWire::Imported { ticks: 3, .. },
                    "day {day}"
                );
            }
            assert_eq!(project.quotes("EURUSD"), 9);
        }

        /// No key stored: the refusal names the two `secrets set` lines, and the bucket is never
        /// asked. A key the bucket refuses: the 403 names the rights it lacks.
        #[test]
        fn no_key_and_a_refused_key_say_what_to_do() {
            let project = Project::new();
            let bucket = bucket(vec![]);
            let download = DukascopyDownload::new(&bucket.base, "eu-west-1", Arc::new(Keys(false)));
            let addr = project.serve_lane(|lane| lane.with_download(Box::new(download)));
            let mut client = DatahubClient::connect(addr).expect("handshake");
            let refusal = client.fetch_archive(&fetch(29, 29, false)).expect_err("no key");
            assert!(
                refusal.contains("vike-cli secrets set DUKASCOPY_ARCHIVE_AWS_ACCESS_KEY_ID"),
                "{refusal}"
            );
            assert!(bucket.seen.lock().unwrap().is_empty(), "no request without a key");

            // A stand-in in another region than the signature's refuses every request.
            let download = DukascopyDownload::new(&bucket.base, "us-east-1", Arc::new(Keys(true)));
            let addr = project.serve_lane(|lane| lane.with_download(Box::new(download)));
            let mut client = DatahubClient::connect(addr).expect("handshake");
            let refusal = client.fetch_archive(&fetch(29, 29, false)).expect_err("refused");
            assert!(refusal.contains("HTTP 403 (AccessDenied)"), "{refusal}");
            assert!(
                refusal.contains("s3:ListBucket") && refusal.contains("s3:GetObject"),
                "{refusal}"
            );
        }

        /// **The stored pair reaches the bucket through the datahub's own scope** (IMP-D2's defect: every
        /// `--fetch` answered "this datahub holds no AWS key"). The REAL path, no scripted reader: the two
        /// approved names stored as `vike-cli secrets set` files them (key-only DEPLOYMENT rows), the handle
        /// the datahub's boot opens (`CredentialScope::NoVenueKeys`, its `Credentials::DeferredNoVenueKeys`
        /// arm, which `crates/vike-ops/tests/settings_secrets/hub_key_store_gate.rs` holds), the per-request
        /// reader `with_archive_download` hands the download, and then the S3 client: the stand-in refuses
        /// any request not signed for the stored key id, so a plan proves the client held the stored key.
        #[test]
        fn the_stored_pair_reaches_the_bucket_through_the_datahubs_own_scope() {
            assert_eq!(
                [ACCESS_KEY_ID_NAME, SECRET_ACCESS_KEY_NAME],
                vike_venues::credential_keys::ARCHIVE_DOWNLOAD_KEYS,
                "the names the scope lets through are the names the download reads"
            );
            let settings = tempfile::tempdir().expect("a temp dir");
            vike_secrets::create_store(Some(settings.path().to_str().unwrap()))
                .expect("create the store");
            vike_secrets::save_credentials_to_store_unjournalled(
                settings.path(),
                vike_secrets::StoreTable::Credential,
                &[
                    (ACCESS_KEY_ID_NAME.to_string(), KEY_ID.to_string()),
                    (SECRET_ACCESS_KEY_NAME.to_string(), "fixture-secret".to_string()),
                ],
                vike_secrets::CredentialUpsertIntent::Add,
            )
            .expect("store the pair");
            let store = vike_secrets::CredentialStore::open(
                Some(vike_secrets::StoreLocation::new(settings.path())),
                vike_secrets::CredentialScope::NoVenueKeys,
            );
            let keys = vike_datahub::tool_feeds::ToolFeedKeyReaderStore::new(store);

            let project = Project::new();
            let bucket = bucket(vec![("EURUSD/2024/00/29_ticks.bi5".to_string(), a_day(1))]);
            let download = DukascopyDownload::new(&bucket.base, "eu-west-1", Arc::new(keys));
            let addr = project.serve_lane(|lane| lane.with_download(Box::new(download)));
            let mut client = DatahubClient::connect(addr).expect("handshake");
            let plan = client
                .fetch_archive(&fetch(29, 29, false))
                .expect("the stored key signs the listing")
                .plan;
            assert_eq!((plan.listed, plan.missing.len()), (1, 1), "{plan:?}");
        }

        /// A key handed in by the harness, answered for both names.
        struct HandedKey(Secret<String>, Secret<String>);

        impl ToolFeedKeyReader for HandedKey {
            fn read(&self, name: &str) -> ToolFeedKeyRead {
                match name {
                    ACCESS_KEY_ID_NAME => ToolFeedKeyRead::Present(self.0.clone()),
                    SECRET_ACCESS_KEY_NAME => ToolFeedKeyRead::Present(self.1.clone()),
                    _ => ToolFeedKeyRead::Absent,
                }
            }
        }

        /// **ONE real day of the vendor's bucket, downloaded and imported** (`#[ignore]`d: it spends
        /// a few cents of the operator's AWS account). The bucket is the RELEASE's
        /// `venue_url.dukascopy.live.history.rest.archive` row. The key arrives on FILE DESCRIPTOR 3,
        /// two lines (the access key id, then the secret), so it is on no argv, in no environment
        /// variable and in no file of this test's; without fd 3 the test says so and passes having
        /// sent nothing. On a box whose AWS CLI holds the key (`[default]`), from a lane checkout:
        ///
        /// ```sh
        /// cargo test -p vike-datahub --test default_build -- --ignored --exact \
        ///   archive_import::real_format::download::one_real_day_downloads_and_imports --nocapture \
        ///   3< <(awk -F= '/^\[/{s=$0;next} s=="[default]"{gsub(/[ \t\r]/,"");
        ///          if($1=="aws_access_key_id")i=$2; if($1=="aws_secret_access_key")k=$2}
        ///          END{print i; print k}' ~/.aws/credentials)
        /// ```
        #[test]
        #[ignore = "downloads one real day from Dukascopy's Requester-Pays bucket on the operator's key"]
        fn one_real_day_downloads_and_imports() {
            let Ok(handed) = std::fs::read_to_string("/dev/fd/3") else {
                eprintln!("SKIPPED: no key on file descriptor 3; nothing was sent");
                return;
            };
            let mut lines = handed.lines().map(str::trim).filter(|l| !l.is_empty());
            let (Some(id), Some(secret)) = (lines.next(), lines.next()) else {
                panic!(
                    "file descriptor 3 must carry two lines: the access key id, then the secret"
                );
            };
            let key = HandedKey(Secret::new(id.to_string()), Secret::new(secret.to_string()));
            let seed = vike_venues::venue_urls::VenueUrls::seed("dukascopy");
            let base = seed
                .url(
                    vike_venues::venue_urls::UrlType::Live,
                    vike_venues::venue_urls::UrlChannel::History,
                    vike_venues::venue_urls::UrlProtocol::Rest,
                    vike_datahub::import::download::URL_PRODUCT,
                )
                .expect("the release seeds the archive row")
                .to_string();
            let (region, bucket) =
                vike_datahub::import::s3::region_and_bucket(&base).expect("a virtual-hosted base");
            eprintln!("bucket {bucket} in {region}");
            let project = Project::new();
            let download = DukascopyDownload::new(&base, &region, Arc::new(key));
            let addr = project.serve_lane(|lane| lane.with_download(Box::new(download)));
            let mut client = DatahubClient::connect(addr).expect("handshake");
            // 2024-01-02, a Tuesday: one daily file.
            let day = fetch(2, 2, false);
            let plan = client.fetch_archive(&day).expect("the bucket's listing").plan;
            eprintln!(
                "plan: {} listed ({} bytes), {} LIST requests, {} missing ({} bytes)",
                plan.listed,
                plan.listed_bytes,
                plan.list_requests,
                plan.missing.len(),
                plan.missing_bytes()
            );
            assert_eq!((plan.listed, plan.missing.len()), (1, 1), "{plan:?}");
            let started = std::time::Instant::now();
            let done =
                client.fetch_archive(&FetchSpec { go: true, ..day.clone() }).expect("fetched");
            let got = done.fetched.expect("an outcome");
            eprintln!(
                "fetched {} file, {} bytes, {} GET, in {:?}",
                got.fetched,
                got.bytes,
                got.get_requests,
                started.elapsed()
            );
            assert_eq!((got.fetched, got.failed.len()), (1, 0), "{got:?}");
            let imported = client
                .import_archive(&ImportSpec { bars: vec![], ..spec("EURUSD", 2, 2, false, false) })
                .expect("the import");
            match &results(&imported)[&jan(2)] {
                ArchiveDayResultWire::Imported { ticks, .. } => {
                    eprintln!("imported {ticks} EURUSD ticks of 2024-01-02");
                    assert!(*ticks > 10_000, "a real FX day holds tens of thousands of ticks");
                    assert_eq!(project.quotes("EURUSD") as u64, *ticks);
                }
                other => panic!("expected Imported, got {other:?}"),
            }
        }

        /// A lane without a download is refused by name, and a dataset the format cannot scale is
        /// refused before the bucket is asked.
        #[test]
        fn the_door_refuses_before_the_bucket_is_asked() {
            let project = Project::new();
            let mut client = DatahubClient::connect(project.serve()).expect("handshake");
            assert!(!client.features().iter().any(|f| f == FEATURE_ARCHIVE_FETCH));
            let refusal = client.fetch_archive(&fetch(29, 29, false)).expect_err("not advertised");
            assert!(refusal.contains(FEATURE_ARCHIVE_FETCH), "{refusal}");

            let bucket = bucket(vec![]);
            let download = DukascopyDownload::new(&bucket.base, "eu-west-1", Arc::new(Keys(true)));
            let addr = project.serve_lane(|lane| lane.with_download(Box::new(download)));
            let mut client = DatahubClient::connect(addr).expect("handshake");
            let unlisted = FetchSpec { dataset: "GBPUSD".to_string(), ..fetch(29, 29, false) };
            let refusal = client.fetch_archive(&unlisted).expect_err("not admitted");
            assert!(refusal.contains("Nothing was listed"), "{refusal}");
            assert!(bucket.seen.lock().unwrap().is_empty());
        }
    }
}
