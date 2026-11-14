//! **A user's CSV UPLOAD over the real wire** (decision 0272): `Request::CsvUpload`, its chunks and its
//! end, served by the real connection loop.
//!
//! 1. **On every build**: a datahub with no import lane refuses the verb by name and keeps the
//!    connection, and a chunk with no upload open is refused too; the client refuses locally, sending
//!    nothing, when `csv_upload` is not advertised.
//! 2. **`real_format`, behind `backfill-serve`**: the real `csv-bars` format over a real
//!    `HistoryStoreDataFusion` in a temporary directory — a round trip of a small file into its `user-*` lane
//!    and read back over the wire, under an OBSERVE key on a keyed server; a target that tries to name
//!    another lane still lands only in `user-<label>`; the size cap refused before a byte is sent; an
//!    upload broken off (by another frame, or a dropped connection) leaving nothing stored; and an
//!    unarmed lane refusing having read nothing.
//!
//! Nothing here touches a live project folder or a live datahub: every root is a temporary directory,
//! every server an ephemeral loopback port.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use vike_data::csv_bars::CsvBarsFormatWords;
use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_datahub::server::serve_with_import;
use vike_datahub_client::DatahubClient;
use vike_datahub_client::archive::ImportTarget;
use vike_datahub_client::proto::{FEATURE_CSV_UPLOAD, Request, Response, read_frame, write_frame};
use vike_datahub_client::upload::CsvUploadSpec;

fn target(label: &str) -> ImportTarget {
    ImportTarget {
        venue: "user".to_string(),
        symbol: "USDT-USD".to_string(),
        interval: "1d".to_string(),
        label: label.to_string(),
        file_format: CsvBarsFormatWords::default(),
        columns: None,
    }
}

fn ask(stream: &mut TcpStream, request: &Request) -> Response {
    write_frame(stream, request).expect("write");
    read_frame(stream).expect("answered, never dropped")
}

fn error_of(response: Response) -> String {
    match response {
        Response::Error(why) => why,
        other => panic!("not refused: {other:?}"),
    }
}

// ---- 1. on every build -------------------------------------------------------------------------

fn serve_no_lane() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("addr");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    thread::spawn(move || {
        let _ = serve_with_import(listener, store, None, None, None, None, None, None, false);
    });
    addr
}

#[test]
fn a_datahub_without_a_lane_refuses_the_upload_by_name_and_keeps_the_connection() {
    let addr = serve_no_lane();
    let mut stream = TcpStream::connect(addr).expect("connect");
    let spec = CsvUploadSpec { target: target("x"), bytes: 10, dry_run: true, head: false };
    let why = error_of(ask(&mut stream, &Request::CsvUpload(spec)));
    assert!(why.contains("takes no CSV upload") && why.contains("Nothing was read"), "{why}");
    let why = error_of(ask(&mut stream, &Request::CsvUploadChunk { text: "ts,close\n".into() }));
    assert!(why.contains("no CSV upload is open"), "{why}");
    assert!(matches!(ask(&mut stream, &Request::Ping), Response::Pong), "the connection survives");

    let mut client = DatahubClient::connect(addr).expect("handshake");
    assert!(!client.features().iter().any(|f| f == FEATURE_CSV_UPLOAD));
    let why = client.upload_csv(&target("x"), "ts,close\n1,2\n", true, false, &mut |_, _| {});
    let why = why.unwrap_err();
    assert!(why.contains("flags.datahub_csv_upload") && why.contains("nothing was sent"), "{why}");
}

// ---- 2. the real format ------------------------------------------------------------------------

#[cfg(feature = "backfill-serve")]
mod real_format {
    use std::io::Write;
    use std::net::Shutdown;

    use vike_data::TimestampRange;
    use vike_datahub::import::ImportLane;
    use vike_datahub::import::formats::real_import_registry;
    use vike_hub_protocol::auth::{HubKeys, Scope};
    use vike_model::MS_PER_DAY;
    use vike_store::HistoryStoreDataFusion;

    use super::*;

    /// 2024-01-15, a UTC midnight, long past.
    const DAY: i64 = 19_737 * MS_PER_DAY;

    struct Project {
        _tmp: tempfile::TempDir,
        store: Arc<HistoryStoreDataFusion>,
        root: std::path::PathBuf,
    }

    impl Project {
        fn new() -> Project {
            let tmp = tempfile::tempdir().expect("a temp dir");
            let store =
                Arc::new(HistoryStoreDataFusion::open(tmp.path().join("hist")).expect("a store"));
            let root = tmp.path().join("imports");
            Project { _tmp: tmp, store, root }
        }

        fn serve(&self, armed: bool, keys: Option<HubKeys>) -> SocketAddr {
            let lane = ImportLane::new(
                self.root.clone(),
                real_import_registry(Arc::clone(&self.store), None),
            )
            .with_upload_armed(armed);
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let addr = listener.local_addr().expect("addr");
            let served: Arc<dyn HistoryStore + Send + Sync> = self.store.clone();
            let lane = Arc::new(lane);
            thread::spawn(move || {
                let _ = serve_with_import(
                    listener,
                    served,
                    None,
                    keys,
                    None,
                    None,
                    None,
                    Some(lane),
                    false,
                );
            });
            addr
        }

        /// Every bar of `label`'s lane, and every bar of the venue's own series, of USDT-USD 1d.
        fn bars(&self, venue: &str, label: Option<&str>) -> usize {
            let range = TimestampRange::all();
            match label {
                Some(label) => {
                    let lane = vike_data::store::store_kind::user_lane(label).unwrap();
                    self.store.load_bars_sourced_capped(venue, &lane, "USDT-USD", "1d", range, None)
                }
                None => self.store.load_bars(venue, "USDT-USD", "1d", range),
            }
            .unwrap()
            .len()
        }
    }

    /// `days` daily rows of a rate from `DAY`, the user's own column names.
    fn rate_file(days: i64) -> String {
        let rows: String =
            (0..days).map(|d| format!("{},1.0{d}\n", DAY + d * MS_PER_DAY)).collect();
        format!("timestamp_ms,rate\n{rows}")
    }

    fn mapped(label: &str) -> ImportTarget {
        ImportTarget { columns: Some("time=timestamp_ms,close=rate".to_string()), ..target(label) }
    }

    fn keys() -> HubKeys {
        HubKeys::new(b"observe-key".to_vec(), b"control-key".to_vec())
    }

    /// The round trip: an OBSERVE key on a keyed server previews, then uploads a small file in chunks,
    /// and reads it back over the wire from its `user-*` lane; nothing lands in any other series.
    #[test]
    fn an_observe_key_uploads_a_file_into_its_user_lane_and_reads_it_back() {
        let project = Project::new();
        let addr = project.serve(true, Some(keys()));
        let observe = HubKeys::new(b"observe-key".to_vec(), Vec::new());
        let mut client =
            DatahubClient::connect_authed(addr, &observe, Scope::Read).expect("observe");
        assert!(client.features().iter().any(|f| f == FEATURE_CSV_UPLOAD), "an armed lane says so");
        let text = rate_file(5);
        let plan = client.upload_csv(&mapped("rates"), &text, true, false, &mut |_, _| {}).unwrap();
        assert_eq!((plan.bars, plan.stored_days, plan.header.len()), (5, 0, 2), "{plan:?}");
        assert_eq!(project.bars("user", Some("rates")), 0, "a dry run stores nothing");
        let mut progress = Vec::new();
        let done = client
            .upload_csv(&mapped("rates"), &text, false, false, &mut |sent, total| {
                progress.push((sent, total))
            })
            .unwrap();
        assert_eq!((done.stored_days, done.stored_bars), (5, 5), "{done:?}");
        assert_eq!(progress.last(), Some(&(text.len() as u64, text.len() as u64)));
        let lane = vike_data::store::store_kind::user_lane("rates").unwrap();
        let back = client
            .load_bars_sourced_ms("user", &lane, "USDT-USD", "1d", None, None, None)
            .expect("the lane reads back over the wire");
        assert_eq!(back.len(), 5);
        assert_eq!((back[0].timestamp, back[0].close, back[0].volume), (DAY, 1.00, 0.0));
        assert_eq!(project.bars("user", None), 0, "the venue's own series is untouched");
        // ...and a second upload of the same series is refused, naming the ways out.
        let why = client.upload_csv(&mapped("rates"), &text, false, false, &mut |_, _| {});
        let why = why.unwrap_err();
        assert!(why.contains("new label") && why.contains("rm"), "{why}");
    }

    /// No request names a source: a frame carrying one is read without it, and a label that spells
    /// another lane's name still lands in `user-<label>`, never in that lane or the venue's series.
    #[test]
    fn an_upload_writes_only_a_user_lane_whatever_its_frame_carries() {
        let project = Project::new();
        let addr = project.serve(true, None);
        let mut stream = TcpStream::connect(addr).expect("connect");
        let text = rate_file(2);
        let spec = CsvUploadSpec {
            target: mapped("aggtrades"),
            bytes: text.len() as u64,
            dry_run: false,
            head: false,
        };
        let mut frame = serde_json::to_value(Request::CsvUpload(spec)).unwrap();
        frame["CsvUpload"]["source"] = "aggtrades".into();
        frame["CsvUpload"]["target"]["source"] = "aggtrades".into();
        write_frame(&mut stream, &frame).unwrap();
        assert!(matches!(
            read_frame::<_, Response>(&mut stream).unwrap(),
            Response::CsvUploadReady
        ));
        assert!(matches!(
            ask(&mut stream, &Request::CsvUploadChunk { text: text.clone() }),
            Response::CsvUploadReceived { .. }
        ));
        assert!(matches!(ask(&mut stream, &Request::CsvUploadEnd), Response::CsvUploaded(_)));
        assert_eq!(project.bars("user", Some("aggtrades")), 2, "it lands in user-aggtrades");
        let range = TimestampRange::all();
        let raw = project.store.load_bars_sourced_capped(
            "user",
            "aggtrades",
            "USDT-USD",
            "1d",
            range,
            None,
        );
        assert!(raw.unwrap_or_default().is_empty(), "never in the aggtrades lane itself");
        assert_eq!(project.bars("user", None), 0, "never in the venue's series");
    }

    /// A file over the cap is refused on its declared size, before a byte is sent, and the connection
    /// goes on.
    #[test]
    fn a_file_over_the_cap_is_refused_before_a_byte_is_sent() {
        let project = Project::new();
        let addr = project.serve(true, None);
        let mut stream = TcpStream::connect(addr).expect("connect");
        let spec = CsvUploadSpec {
            target: mapped("big"),
            bytes: 600 * 1024 * 1024,
            dry_run: false,
            head: false,
        };
        let why = error_of(ask(&mut stream, &Request::CsvUpload(spec)));
        assert!(why.contains("over this datahub's cap of 536870912 bytes"), "{why}");
        assert!(matches!(ask(&mut stream, &Request::Ping), Response::Pong));
    }

    /// An upload broken off — by another request in the middle, or by a dropped connection — stores
    /// nothing and frees the slot: the same upload then goes through whole.
    #[test]
    fn a_broken_off_upload_stores_nothing_and_frees_the_slot() {
        let project = Project::new();
        let addr = project.serve(true, None);
        let text = rate_file(4);
        let head = &text[..text.len() / 2];
        let spec = || CsvUploadSpec {
            target: mapped("half"),
            bytes: text.len() as u64,
            dry_run: false,
            head: false,
        };

        // Another request in the middle: the upload is dropped, and that request is not served.
        let mut stream = TcpStream::connect(addr).expect("connect");
        assert!(matches!(ask(&mut stream, &Request::CsvUpload(spec())), Response::CsvUploadReady));
        assert!(matches!(
            ask(&mut stream, &Request::CsvUploadChunk { text: head.to_string() }),
            Response::CsvUploadReceived { .. }
        ));
        let why = error_of(ask(&mut stream, &Request::Ping));
        assert!(why.contains("nothing was stored"), "{why}");
        let why = error_of(ask(&mut stream, &Request::CsvUploadEnd));
        assert!(why.contains("no CSV upload is open"), "{why}");

        // A connection dropped in the middle.
        let mut stream = TcpStream::connect(addr).expect("connect");
        assert!(matches!(ask(&mut stream, &Request::CsvUpload(spec())), Response::CsvUploadReady));
        write_frame(&mut stream, &Request::CsvUploadChunk { text: head.to_string() }).unwrap();
        stream.flush().unwrap();
        let _ = stream.shutdown(Shutdown::Both);
        drop(stream);
        assert_eq!(
            project.bars("user", Some("half")),
            0,
            "nothing of a broken-off upload is stored"
        );

        // The slot is free again (the server notices the drop on its next read), and a whole upload
        // of the same series goes through.
        let mut client = DatahubClient::connect(addr).expect("handshake");
        let mut done = None;
        for _ in 0..50 {
            match client.upload_csv(&mapped("half"), &text, false, false, &mut |_, _| {}) {
                Ok(d) => {
                    done = Some(d);
                    break;
                }
                Err(why) if why.contains("ONE at a time") => {
                    thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(why) => panic!("{why}"),
            }
        }
        let done = done.expect("the slot frees once the dropped connection is read");
        assert_eq!(done.stored_days, 4);
    }

    /// An unarmed lane advertises no upload and refuses the frame having read nothing.
    #[test]
    fn an_unarmed_lane_refuses_having_read_nothing() {
        let project = Project::new();
        let addr = project.serve(false, None);
        let mut stream = TcpStream::connect(addr).expect("connect");
        let spec = CsvUploadSpec { target: mapped("off"), bytes: 10, dry_run: false, head: false };
        let why = error_of(ask(&mut stream, &Request::CsvUpload(spec)));
        assert!(why.contains("flags.datahub_csv_upload"), "{why}");
        let client = DatahubClient::connect(addr).expect("handshake");
        assert!(!client.features().iter().any(|f| f == FEATURE_CSV_UPLOAD));
    }
}
