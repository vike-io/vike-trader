//! The upload's door and session over a FAKE format: the order of the refusals, the arming, the one
//! slot, the declared size held both ways, and the advertisement that is the arming. The real
//! `csv-bars` format's upload over a real store is `crates/vike-datahub/src/import/formats/csv_bars_tests.rs`'s,
//! and the round trip over the wire `crates/vike-datahub/tests/default_build/csv_upload.rs`'s.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use vike_data::archive_day::ArchiveDayRefusal;
use vike_data::csv_bars::CsvBarsFormatWords;
use vike_datahub_client::archive::{ImportSpec, ImportTarget};
use vike_datahub_client::proto::{FEATURE_CSV_COLUMNS, FEATURE_CSV_UPLOAD};

use super::*;
use crate::import::{ArchiveFormat, ArchiveFormatLayoutFile, ImportSession, advertised};

/// A format that takes an upload: it admits what `refuse` does not name, and records every text it
/// is asked to run.
#[derive(Default)]
struct FakeUpload {
    refuse: Option<String>,
    ran: Arc<Mutex<Vec<(String, bool)>>>,
}

impl ArchiveFormat for FakeUpload {
    fn id(&self) -> &'static str {
        "csv-bars"
    }
    fn venue(&self) -> &'static str {
        ""
    }
    fn admit(&self, _dataset: &str) -> Result<String, String> {
        Ok(String::new())
    }
    fn accepts_dir(&self, _rel: &[&str], _now_ms: i64) -> bool {
        false
    }
    fn classify_file(&self, _rel: &[&str], _now_ms: i64) -> ArchiveFormatLayoutFile {
        ArchiveFormatLayoutFile::Other
    }
    fn max_file_bytes(&self) -> u64 {
        100
    }
    fn header_len(&self) -> usize {
        0
    }
    fn read_header(&self, _len: u64, _prefix: &[u8]) -> Result<Option<u64>, ArchiveDayRefusal> {
        Ok(None)
    }
    fn session<'a>(
        &'a self,
        _spec: &ImportSpec,
        _now_ms: i64,
    ) -> Result<Box<dyn ImportSession + 'a>, String> {
        Err("the fake imports nothing".to_string())
    }
    fn uploads(&self) -> Option<&dyn CsvUploadFormat> {
        Some(self)
    }
}

impl CsvUploadFormat for FakeUpload {
    fn admit_upload(&self, _spec: &CsvUploadSpec) -> Result<(), String> {
        self.refuse.clone().map_or(Ok(()), Err)
    }
    fn run_upload(
        &self,
        spec: &CsvUploadSpec,
        text: &str,
        _now_ms: i64,
    ) -> Result<CsvUploadDone, String> {
        self.ran.lock().unwrap().push((text.to_string(), spec.dry_run));
        Ok(CsvUploadDone { bars: text.lines().count() as u64, ..CsvUploadDone::default() })
    }
    fn max_upload_bytes(&self) -> u64 {
        self.max_file_bytes()
    }
}

fn lane(format: FakeUpload, armed: bool) -> ImportLane {
    ImportLane::new(PathBuf::from("imports"), vec![Box::new(format)]).with_upload_armed(armed)
}

fn spec(bytes: u64) -> CsvUploadSpec {
    CsvUploadSpec {
        target: ImportTarget {
            venue: "user".to_string(),
            symbol: "USDT-USD".to_string(),
            interval: "1d".to_string(),
            label: "coinbase".to_string(),
            file_format: CsvBarsFormatWords::default(),
            columns: None,
            sparse: false,
            allow_coarse_grid: false,
        },
        bytes,
        dry_run: false,
        head: false,
    }
}

fn refusal(r: Result<CsvUploadSession<'_>, DatahubRefusal>) -> DatahubRefusal {
    match r {
        Ok(_) => panic!("the door let it through"),
        Err(refusal) => refusal,
    }
}

#[test]
fn a_lane_without_an_upload_format_or_unarmed_refuses_having_read_nothing() {
    assert_eq!(refusal(begin(None, spec(10))), DatahubRefusal::UploadNoLane);
    let unarmed =
        lane(FakeUpload { refuse: Some("never asked".into()), ..Default::default() }, false);
    // Unarmed comes before the format's own admission: nothing is asked of the store.
    assert_eq!(refusal(begin(Some(&unarmed), spec(10))), DatahubRefusal::UploadNotArmed);
}

#[test]
fn the_door_refuses_a_bad_spec_an_oversize_file_and_the_formats_refusal_in_order() {
    let armed = lane(FakeUpload::default(), true);
    let mut bad = spec(10);
    bad.target.label = "My File".to_string();
    let why = refusal(begin(Some(&armed), bad));
    assert!(
        matches!(&why, DatahubRefusal::UploadSpecRefused { why } if why.contains("short lowercase name"))
    );
    assert_eq!(
        refusal(begin(Some(&armed), spec(101))),
        DatahubRefusal::UploadTooLarge { bytes: 101, cap: 100 }
    );
    let taken =
        lane(FakeUpload { refuse: Some("the lane holds rows".into()), ..Default::default() }, true);
    let why = refusal(begin(Some(&taken), spec(10)));
    assert_eq!(why, DatahubRefusal::UploadFormatRefused { why: "the lane holds rows".into() });
    assert_eq!(
        why.to_string(),
        "CsvUpload: the lane holds rows",
        "the format's words, under the door's"
    );
}

#[test]
fn one_upload_holds_the_one_slot_until_it_drops() {
    let armed = lane(FakeUpload::default(), true);
    let first = begin(Some(&armed), spec(10)).expect("admitted");
    assert_eq!(refusal(begin(Some(&armed), spec(10))), DatahubRefusal::UploadBusy);
    assert!(armed.take_slot().is_none(), "the archive import waits on the same slot");
    drop(first);
    assert!(begin(Some(&armed), spec(10)).is_ok(), "a dropped session frees the slot");
}

#[test]
fn the_declared_size_is_held_both_ways_and_only_a_whole_file_runs() {
    let armed = lane(FakeUpload::default(), true);
    let mut s = begin(Some(&armed), spec(10)).unwrap();
    assert_eq!(s.push("ts,c\n").unwrap(), 5);
    assert_eq!(
        s.push("123456").unwrap_err(),
        DatahubRefusal::UploadOverrun { declared: 10, total: 11 }
    );
    drop(s);
    // An end before every byte arrived stores nothing and runs nothing.
    let mut s = begin(Some(&armed), spec(10)).unwrap();
    s.push("ts,c\n").unwrap();
    match s.end(0) {
        Response::Refused(frame) => {
            let short = DatahubRefusal::UploadShort { got: 5, declared: 10 };
            assert_eq!(frame, vike_hub_protocol::refusal::HubRefusal::from(short));
        }
        other => panic!("{other:?}"),
    }
    let mut s = begin(Some(&armed), spec(10)).unwrap();
    s.push("ts,c\n").unwrap();
    s.push("1,2\n\n").unwrap();
    match s.end(0) {
        Response::CsvUploaded(done) => assert_eq!(done.bars, 3),
        other => panic!("{other:?}"),
    }
}

/// Only a complete file reaches the format, whole and once; a short one never does.
#[test]
fn the_format_runs_only_the_whole_text() {
    let ran = Arc::new(Mutex::new(Vec::new()));
    let lane = lane(FakeUpload { ran: Arc::clone(&ran), ..Default::default() }, true);
    let mut short = begin(Some(&lane), spec(9)).unwrap();
    short.push("ts,c\n").unwrap();
    let _ = short.end(0);
    let mut s = begin(Some(&lane), spec(9)).unwrap();
    s.push("ts,c\n").unwrap();
    s.push("1,2\n").unwrap();
    let _ = s.end(0);
    assert_eq!(*ran.lock().unwrap(), [("ts,c\n1,2\n".to_string(), false)]);
}

#[test]
fn the_upload_is_advertised_exactly_when_armed() {
    let armed = advertised(Some(&lane(FakeUpload::default(), true)));
    assert!(armed.contains(&FEATURE_CSV_UPLOAD.to_string()), "{armed:?}");
    assert!(armed.contains(&FEATURE_CSV_COLUMNS.to_string()), "{armed:?}");
    let unarmed = advertised(Some(&lane(FakeUpload::default(), false)));
    assert!(!unarmed.contains(&FEATURE_CSV_UPLOAD.to_string()), "{unarmed:?}");
    assert!(unarmed.contains(&FEATURE_CSV_COLUMNS.to_string()), "the mapping is a build fact");
}
