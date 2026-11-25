//! `archive.rs`'s tests: the three validators (one test per refusal the design's §8 names), the
//! request as a whole, and the wire shape of the request and a fully populated answer.

use std::io::Cursor;

use super::*;
use crate::proto::{Request, Response};
use vike_hub_protocol::frame::{read_frame, write_frame};

const DAY: i64 = MS_PER_DAY;
/// 2024-01-15 00:00 UTC — the day the design's measured file belongs to.
const D0: i64 = 19_737 * DAY;

fn spec() -> ImportSpec {
    ImportSpec {
        format: "dukascopy-bi5".to_string(),
        dataset: "EURUSD".to_string(),
        from_day: Some(D0),
        to_day: Some(D0 + 30 * DAY),
        bars: vec!["1m".to_string()],
        dry_run: false,
        verify: false,
        replace_holed: false,
        target: None,
    }
}

fn refused(dataset: &str) -> String {
    validate_import_dataset(dataset).expect_err("must be refused")
}

// ---- the dataset validator: one test per refusal the design's §8 names ----------------------------

#[test]
fn the_vendor_folder_names_are_accepted() {
    for good in [
        "EURUSD", "USDJPY", "XAUUSD", "E", "1", "US30.IDX", "BTC-USD", "EUR_USD",
        // A device-name PREFIX is not a device name: only the exact stem is reserved.
        "CONX", "COM10", "LPT0", "NULL",
    ] {
        validate_import_dataset(good).unwrap_or_else(|e| panic!("{good} must be accepted: {e}"));
    }
}

#[test]
fn dot_dot_is_refused() {
    assert!(refused("..").contains("must START"), "{}", refused(".."));
}

#[test]
fn a_separator_is_refused() {
    for bad in ["EUR/USD", "EUR\\USD", "EURUSD/"] {
        assert!(refused(bad).contains("outside the permitted set"), "{bad}: {}", refused(bad));
    }
}

#[test]
fn an_absolute_path_is_refused() {
    for bad in ["/ETC", "\\\\SERVER\\SHARE"] {
        assert!(refused(bad).contains("must START"), "{bad}: {}", refused(bad));
    }
}

#[test]
fn a_drive_letter_is_refused() {
    for bad in ["C:", "C:EURUSD", "C:\\EURUSD"] {
        assert!(refused(bad).contains("offset 1"), "{bad}: {}", refused(bad));
    }
}

#[test]
fn a_leading_dot_is_refused() {
    for bad in [".", ".EURUSD", ".HIDDEN"] {
        assert!(refused(bad).contains("must START"), "{bad}: {}", refused(bad));
    }
}

#[test]
fn a_nul_is_refused() {
    let err = refused("EUR\0USD");
    assert!(err.contains("offset 3"), "{err}");
    assert!(refused("\0").contains("must START"));
}

#[test]
fn thirty_three_bytes_are_refused_and_thirty_two_are_not() {
    let at_cap = "A".repeat(IMPORT_MAX_DATASET_BYTES);
    validate_import_dataset(&at_cap).expect("exactly at the cap is accepted");
    let over = "A".repeat(IMPORT_MAX_DATASET_BYTES + 1);
    let err = refused(&over);
    assert!(err.contains("IMPORT_MAX_DATASET_BYTES"), "{err}");
    assert_eq!(over.len(), 33);
}

#[test]
fn lower_case_is_refused() {
    assert!(refused("eurusd").contains("must START"));
    assert!(refused("EURusd").contains("offset 3"), "{}", refused("EURusd"));
}

#[test]
fn a_windows_device_name_is_refused() {
    for bad in ["CON", "PRN", "AUX", "NUL", "COM1", "COM9", "LPT1", "LPT9", "CON.TXT", "NUL.EURUSD"]
    {
        assert!(refused(bad).contains("DEVICE NAME"), "{bad}: {}", refused(bad));
    }
}

#[test]
fn an_empty_dataset_is_refused() {
    assert!(refused("").contains("EMPTY"));
}

#[test]
fn whitespace_is_refused() {
    for bad in ["EUR USD", "EURUSD\n", " EURUSD"] {
        let _ = refused(bad);
    }
}

/// The validator never ECHOES what it refused — the untrusted string would otherwise ride into the
/// server's log. Each input carries a token that appears in no refusal text.
#[test]
fn no_refusal_echoes_the_dataset() {
    let long = "Q".repeat(IMPORT_MAX_DATASET_BYTES + 5);
    for bad in
        ["EURusdSECRET", "/ETCPASSWD", "C:WINDOWSX", "COM1.PAYLOADX", "..PAYLOADX", long.as_str()]
    {
        let err = refused(bad);
        assert!(!err.contains(bad), "the refusal echoed {bad:?}: {err}");
    }
}

/// ⚠ THE DELEGATION CLAIM `validate_import_dataset`'s doc makes: once a name passes rules 1–3,
/// `vike_runs::valid_mark_name` can only refuse it as a DEVICE NAME. A rule that function
/// grows beyond the charset would mislabel a valid dataset, and this test says so. Exhaustive over
/// every name of up to three bytes from an alphabet covering the device-name letters, the digits
/// and the three separators, plus the 32-byte edge.
#[test]
fn a_name_that_passes_the_charset_fails_the_mark_rule_only_as_a_device_name() {
    let alphabet: Vec<char> = "ACLMNOPRTUX0129._-".chars().collect();
    let mut names: Vec<String> = Vec::new();
    for a in &alphabet {
        names.push(a.to_string());
        for b in &alphabet {
            names.push(format!("{a}{b}"));
            for c in &alphabet {
                names.push(format!("{a}{b}{c}"));
            }
        }
    }
    for dev in ["CON", "PRN", "AUX", "NUL", "COM1", "LPT9"] {
        names.push(format!("{dev}.X"));
        names.push(format!("{dev}.TXT.GZ"));
    }
    names.push("A".repeat(IMPORT_MAX_DATASET_BYTES));
    names.push(format!("{}.{}", "B".repeat(15), "C".repeat(16)));
    let mut checked = 0usize;
    for n in &names {
        let passes_charset = !n.is_empty()
            && n.len() <= IMPORT_MAX_DATASET_BYTES
            && (n.as_bytes()[0].is_ascii_uppercase() || n.as_bytes()[0].is_ascii_digit())
            && n.bytes().all(is_dataset_byte);
        if !passes_charset {
            continue;
        }
        checked += 1;
        if let Err(why) = vike_runs::valid_mark_name(n) {
            assert!(
                why.contains("device name"),
                "valid_mark_name refused {n:?} for a reason that is not a device name — the \
                 dataset validator's delegation would now mislabel it: {why}"
            );
        }
    }
    assert!(checked > 1_000, "the sweep must not be vacuous: {checked}");
}

// ---- the bar intervals ----------------------------------------------------------------------------

fn bars(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn day_dividing_canonical_intervals_are_accepted() {
    validate_import_bars(&[]).expect("no bars is `--bars none`");
    validate_import_bars(&bars(&["1m", "5m", "1h", "1d"])).expect("four day-dividing steps");
    validate_import_bars(&bars(&["86400s"])).expect("the longest day-dividing spelling");
    validate_import_bars(&bars(&["1440m"])).expect("a day, in minutes, divides a day");
}

#[test]
fn more_than_the_interval_cap_is_refused() {
    let err = validate_import_bars(&bars(&["1m", "5m", "15m", "1h", "1d"])).unwrap_err();
    assert!(err.contains("IMPORT_MAX_BAR_INTERVALS"), "{err}");
}

#[test]
fn an_interval_that_does_not_divide_a_day_is_refused() {
    for bad in ["7m", "13m", "5h", "7s"] {
        let err = validate_import_bars(&bars(&[bad])).unwrap_err();
        assert!(err.contains("does not divide a UTC day"), "{bad}: {err}");
    }
}

#[test]
fn a_non_canonical_or_unparseable_interval_is_refused() {
    for bad in ["01m", "0m", "1M", "1w", "1mo", "m", "", "1 m", "-1m", "+1m"] {
        let err = validate_import_bars(&bars(&[bad])).unwrap_err();
        assert!(err.contains("not a canonical bar step"), "{bad:?}: {err}");
    }
}

/// The length bound protects the PARSER: a nineteen-digit count would overflow `interval_ms`'s
/// multiplication. Refused by length, before it is parsed, so no build panics on it.
#[test]
fn an_over_long_interval_is_refused_before_it_is_parsed() {
    let err = validate_import_bars(&bars(&["9223372036854775807d"])).unwrap_err();
    assert!(err.contains("IMPORT_MAX_INTERVAL_BYTES"), "{err}");
    assert!(!err.contains("9223372036854775807"), "the over-long string is not echoed: {err}");
}

#[test]
fn a_repeated_interval_is_refused() {
    let err = validate_import_bars(&bars(&["1m", "5m", "1m"])).unwrap_err();
    assert!(err.contains("named twice"), "{err}");
}

// ---- the window --------------------------------------------------------------------------------

#[test]
fn an_open_window_is_accepted_in_every_mode() {
    for (dry_run, verify) in [(true, false), (true, true), (false, false)] {
        validate_import_window(None, None, dry_run, verify).expect("open bounds are the server's");
    }
}

#[test]
fn a_bound_that_is_not_a_day_start_is_refused() {
    let err = validate_import_window(Some(D0 + 1), None, true, false).unwrap_err();
    assert!(err.contains("`from_day`"), "{err}");
    let err = validate_import_window(None, Some(D0 + DAY - 1), true, false).unwrap_err();
    assert!(err.contains("`to_day`"), "{err}");
}

#[test]
fn an_inverted_window_is_refused() {
    let err = validate_import_window(Some(D0 + DAY), Some(D0), true, false).unwrap_err();
    assert!(err.contains("inverted"), "{err}");
}

#[test]
fn verify_without_dry_run_is_refused() {
    let err = validate_import_window(Some(D0), Some(D0), false, true).unwrap_err();
    assert!(err.contains("DRY RUN"), "{err}");
}

#[test]
fn a_decoding_request_over_the_day_cap_is_refused_and_one_at_it_is_not() {
    let last = D0 + (IMPORT_MAX_DAYS - 1) * DAY;
    validate_import_window(Some(D0), Some(last), false, false).expect("31 days import");
    validate_import_window(Some(D0), Some(last), true, true).expect("31 days verify");
    for (dry_run, verify) in [(false, false), (true, true)] {
        let err = validate_import_window(Some(D0), Some(last + DAY), dry_run, verify).unwrap_err();
        assert!(err.contains("IMPORT_MAX_DAYS"), "{err}");
        assert!(err.contains("32 days"), "{err}");
    }
}

#[test]
fn a_plan_only_dry_run_is_not_bounded_by_the_day_cap() {
    validate_import_window(Some(D0), Some(D0 + 3_650 * DAY), true, false)
        .expect("a plan reads headers and decodes nothing");
}

/// Two aligned bounds as far apart as an `i64` allows: a plain subtraction would overflow on a
/// server thread. Refused, never a panic.
#[test]
fn extreme_bounds_do_not_overflow() {
    let lo = (i64::MIN / DAY) * DAY;
    let hi = (i64::MAX / DAY) * DAY;
    let err = validate_import_window(Some(lo), Some(hi), false, false).unwrap_err();
    assert!(err.contains("IMPORT_MAX_DAYS"), "{err}");
    validate_import_window(Some(lo), Some(hi), true, false).expect("a plan may span anything");
}

// ---- the request as a whole --------------------------------------------------------------------

#[test]
fn the_spec_validator_runs_every_rule() {
    validate_import_spec(&spec()).expect("the canonical month import is valid");
    let mut s = spec();
    s.dataset = "../ETC".to_string();
    assert!(validate_import_spec(&s).unwrap_err().contains("must START"));
    let mut s = spec();
    s.bars = bars(&["7m"]);
    assert!(validate_import_spec(&s).unwrap_err().contains("does not divide"));
    let mut s = spec();
    s.to_day = Some(D0 + 31 * DAY);
    assert!(validate_import_spec(&s).unwrap_err().contains("IMPORT_MAX_DAYS"));
}

#[test]
fn decodes_and_writes_follow_the_two_flags() {
    let mut s = spec();
    assert!(s.decodes() && s.writes(), "an import decodes and writes");
    s.dry_run = true;
    assert!(!s.decodes() && !s.writes(), "a plan does neither");
    s.verify = true;
    assert!(s.decodes() && !s.writes(), "a verify decodes and writes nothing");
}

// ---- the wire shape ----------------------------------------------------------------------------

/// The design's STRUCT variant rides as a newtype over [`ImportSpec`], the same bytes; this pins
/// them: one outer tag, exactly these seven keys.
#[test]
fn the_import_frame_has_the_designed_shape() {
    let v = serde_json::to_value(Request::ImportArchive(spec())).expect("encode");
    let outer = v.as_object().expect("an externally-tagged object");
    assert_eq!(outer.keys().map(String::as_str).collect::<Vec<_>>(), vec!["ImportArchive"]);
    let inner = outer["ImportArchive"].as_object().expect("the payload is an object");
    let mut keys: Vec<&str> = inner.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["bars", "dataset", "dry_run", "format", "from_day", "to_day", "verify"],
        "{v}"
    );
}

/// The two optional fields ride the frame only when asked for, and round-trip when they do.
#[test]
fn replace_holed_and_a_target_ride_the_frame_only_when_set() {
    let target = ImportTarget {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        interval: "1h".to_string(),
        label: "my-file".to_string(),
        file_format: CsvBarsFormatWords {
            delimiter: Some(";".to_string()),
            decimal: Some(",".to_string()),
            ..CsvBarsFormatWords::default()
        },
        columns: Some("time=timestamp_ms,close=c".to_string()),
        sparse: false,
        allow_coarse_grid: false,
    };
    let asked = ImportSpec { replace_holed: true, target: Some(target), ..spec() };
    let v = serde_json::to_value(&asked).expect("encode");
    assert!(v.get("replace_holed").is_some() && v.get("target").is_some(), "{v}");
    let back: ImportSpec = serde_json::from_value(v).expect("decode");
    assert_eq!(back, asked);
    validate_import_spec(&asked).expect("a valid target");
}

/// ⚠ **A file format word the decoder does not have refuses the frame**, never reads as its default:
/// a hand-built `"timezone"` beside a text time would otherwise store Moscow's wall clock as UTC.
#[test]
fn a_file_format_key_that_is_no_word_does_not_decode() {
    let target = ImportTarget {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        interval: "1h".to_string(),
        label: "my-file".to_string(),
        file_format: CsvBarsFormatWords {
            time: Some("DD.MM.YYYY HH:mm".to_string()),
            ..CsvBarsFormatWords::default()
        },
        columns: None,
        sparse: false,
        allow_coarse_grid: false,
    };
    let asked = ImportSpec { target: Some(target), ..spec() };
    let mut v = serde_json::to_value(&asked).expect("encode");
    assert_eq!(v["target"]["file_format"], serde_json::json!({"time": "DD.MM.YYYY HH:mm"}));
    serde_json::from_value::<ImportSpec>(v.clone()).expect("the words it has decode");
    v["target"]["file_format"]["timezone"] = serde_json::json!("Europe/Moscow");
    let why = serde_json::from_value::<ImportSpec>(v).unwrap_err().to_string();
    assert!(why.contains("unknown field `timezone`"), "{why}");
}

/// A target's venue, symbol, interval and label are each held to their rule at the door.
#[test]
fn a_target_is_refused_for_each_rule_it_breaks() {
    let good = ImportTarget {
        venue: "binance".to_string(),
        symbol: "BTCUSDT.P".to_string(),
        interval: "1m".to_string(),
        label: "my_file-2".to_string(),
        file_format: CsvBarsFormatWords::default(),
        columns: None,
        sparse: false,
        allow_coarse_grid: false,
    };
    validate_import_target(&good).expect("a valid target");
    let plain = serde_json::to_value(&good).expect("encode");
    assert!(plain.get("file_format").is_none(), "a plain file's format is left off the frame");
    assert!(plain.get("columns").is_none(), "no mapping is left off the frame");
    // Decision 0272: the reserved user venue, and a mapping the decoder reads.
    let user = ImportTarget { venue: "user".to_string(), ..good.clone() };
    validate_import_target(&user).expect("the user venue is a target's venue");
    let mapped =
        ImportTarget { columns: Some("time=date,close=value".to_string()), ..good.clone() };
    validate_import_target(&mapped).expect("a mapping naming time and close");
    let why = validate_import_target(&ImportTarget { venue: "User".to_string(), ..good.clone() })
        .unwrap_err();
    assert!(why.contains("neither `user`"), "{why}");
    // `--allow-coarse-grid` is meaningful only beside `--sparse`; both ride the frame only when set.
    assert!(plain.get("sparse").is_none() && plain.get("allow_coarse_grid").is_none());
    let events = ImportTarget { sparse: true, allow_coarse_grid: true, ..good.clone() };
    validate_import_target(&events).expect("events at round times");
    let why = validate_import_target(&ImportTarget { allow_coarse_grid: true, ..good.clone() })
        .unwrap_err();
    assert!(why.contains("meaningful only with --sparse"), "{why}");
    let words = |delimiter: &str, decimal: &str| CsvBarsFormatWords {
        delimiter: Some(delimiter.to_string()),
        decimal: Some(decimal.to_string()),
        ..CsvBarsFormatWords::default()
    };
    for bad in [
        ImportTarget { venue: "nowhere".to_string(), ..good.clone() },
        ImportTarget { symbol: "BTC/USDT".to_string(), ..good.clone() },
        ImportTarget { symbol: "..".to_string(), ..good.clone() },
        ImportTarget { interval: "7m".to_string(), ..good.clone() },
        ImportTarget { interval: "1w".to_string(), ..good.clone() },
        ImportTarget { label: "My File".to_string(), ..good.clone() },
        ImportTarget { label: String::new(), ..good.clone() },
        ImportTarget { file_format: words("|", "."), ..good.clone() },
        ImportTarget { file_format: words(";", ";"), ..good.clone() },
        ImportTarget {
            file_format: CsvBarsFormatWords {
                tz: Some("Mars/Base".to_string()),
                ..words(",", ".")
            },
            ..good.clone()
        },
        ImportTarget { columns: Some("time=t,open=o,close=c".to_string()), ..good.clone() },
        ImportTarget { columns: Some("open=o".to_string()), ..good.clone() },
        ImportTarget { columns: Some(format!("time={},close=c", "t".repeat(600))), ..good.clone() },
        ImportTarget { allow_coarse_grid: true, ..good.clone() },
    ] {
        assert!(validate_import_target(&bad).is_err(), "{bad:?}");
        let spec = ImportSpec { target: Some(bad.clone()), ..spec() };
        assert!(validate_import_spec(&spec).is_err(), "{bad:?}");
    }
}

/// ⚠ A frame that OMITS `dry_run` does not decode — so the field that decides whether the store is
/// written cannot be dropped by accident and default to an import.
#[test]
fn a_frame_that_omits_dry_run_does_not_decode() {
    let body = r#"{"ImportArchive":{"format":"dukascopy-bi5","dataset":"EURUSD","from_day":null,"to_day":null,"bars":[],"verify":false}}"#;
    assert!(serde_json::from_str::<Request>(body).is_err(), "an omitted dry_run must not decode");
    let with = r#"{"ImportArchive":{"format":"dukascopy-bi5","dataset":"EURUSD","from_day":null,"to_day":null,"bars":[],"dry_run":true,"verify":false}}"#;
    match serde_json::from_str::<Request>(with).expect("the same frame WITH dry_run decodes") {
        Request::ImportArchive(s) => assert!(s.dry_run),
        other => panic!("expected ImportArchive, got {other:?}"),
    }
}

fn populated_done() -> ImportDone {
    ImportDone {
        plan: ImportPlan {
            format: "dukascopy-bi5".into(),
            dataset: "EURUSD".into(),
            venue: "dukascopy".into(),
            server_dir: "/srv/vike-<unit>/market_data/imports/dukascopy-bi5/EURUSD".into(),
            dir: ImportDatasetDirectory::Present,
            admission: "point value 100000 — vendor page, \"most FX pairs\"".into(),
            inventory: ImportPlanInventory {
                daily_files: 3,
                first_day: Some(D0),
                last_day: Some(D0 + 4 * DAY),
                other_layout_days: vec![D0 - DAY],
                other_objects: 2,
                other_bytes: 1_234,
                skipped: vec![
                    ImportPlanSkippedEntry {
                        path: "dukascopy-bi5/EURUSD/2024/00/16_ticks.bi5".into(),
                        class: ImportPlanEntryClass::Symlink,
                    },
                    ImportPlanSkippedEntry {
                        path: "dukascopy-bi5/EURUSD/2024/00/17_ticks.bi5".into(),
                        class: ImportPlanEntryClass::Fifo,
                    },
                ],
            },
            from_day: Some(D0),
            to_day: Some(D0 + 4 * DAY),
            days: vec![
                ImportPlanDay {
                    day: D0,
                    class: ArchiveDayClassWire::Free,
                    file_bytes: 20_298,
                    declared_ticks: Some(4_201),
                },
                ImportPlanDay {
                    day: D0 + DAY,
                    class: ArchiveDayClassWire::Supersede {
                        key: "dukascopy-provisional:EURUSD:x-y".into(),
                    },
                    file_bytes: 9,
                    declared_ticks: None,
                },
                ImportPlanDay {
                    day: D0 + 3 * DAY,
                    class: ArchiveDayClassWire::Overlapped {
                        keys: vec!["dukascopy:EURUSD:a-b".into()],
                    },
                    file_bytes: 7,
                    declared_ticks: Some(0),
                },
                ImportPlanDay {
                    day: D0 + 4 * DAY,
                    class: ArchiveDayClassWire::Refused(ArchiveDayRefusal {
                        class: "MixedLayout".into(),
                        detail: "a daily file and hourly files for one day".into(),
                    }),
                    file_bytes: 1,
                    declared_ticks: None,
                },
            ],
            gaps: vec![D0 + 2 * DAY],
            bars: vec!["1m".into()],
            series: Some(SeriesCoverage {
                first_timestamp: D0,
                last_timestamp: D0 + DAY,
                rows: 10,
                bytes: 126,
                parts: 1,
                dates: 1,
            }),
            preview: vec![ImportPlanPreviewRow::of(&vike_marketdata::Bar {
                timestamp: D0,
                open: 1.5,
                high: 2.0,
                low: 1.0,
                close: 1.75,
                volume: 3.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            })],
        },
        outcome: Some(ImportOutcome {
            days: vec![
                ImportOutcomeDay {
                    day: D0,
                    result: ArchiveDayResultWire::Imported {
                        ticks: 4_201,
                        bars: vec![ArchiveDayBars { interval: "1m".into(), rows: 60 }],
                        report: Some("2 exact repeats dropped".into()),
                    },
                },
                ImportOutcomeDay {
                    day: D0 + DAY,
                    result: ArchiveDayResultWire::ToppedUp { bars: vec![] },
                },
                ImportOutcomeDay {
                    day: D0 + 2 * DAY,
                    result: ArchiveDayResultWire::Verified {
                        ticks: 3,
                        report: Some("1 gap".into()),
                    },
                },
                ImportOutcomeDay {
                    day: D0 + 3 * DAY,
                    result: ArchiveDayResultWire::Refused(ArchiveDayRefusal {
                        class: "AmbiguousTimeBase".into(),
                        detail: "every tick lies in the first hour".into(),
                    }),
                },
            ],
        }),
    }
}

/// The request and a FULLY populated answer — every variant of every enum the plan and the outcome
/// carry — survive `write_frame` -> `read_frame` whole.
#[test]
fn the_request_and_a_populated_answer_survive_the_frame_codec() {
    let done = populated_done();
    let mut buffer: Vec<u8> = Vec::new();
    write_frame(&mut buffer, &Request::ImportArchive(spec())).unwrap();
    write_frame(&mut buffer, &Response::ArchiveImported(Box::new(done.clone()))).unwrap();
    // ...and the plan-only shape, with no outcome and an absent directory.
    let mut absent = done.clone();
    absent.plan.dir = ImportDatasetDirectory::Absent;
    absent.outcome = None;
    write_frame(&mut buffer, &Response::ArchiveImported(Box::new(absent.clone()))).unwrap();
    let mut unreadable = absent.clone();
    unreadable.plan.dir = ImportDatasetDirectory::Unreadable { why: "permission denied".into() };
    write_frame(&mut buffer, &Response::ArchiveImported(Box::new(unreadable.clone()))).unwrap();

    let mut cursor = Cursor::new(buffer);
    match read_frame::<_, Request>(&mut cursor).unwrap() {
        Request::ImportArchive(got) => assert_eq!(got, spec()),
        other => panic!("expected ImportArchive, got {other:?}"),
    }
    for want in [done, absent, unreadable] {
        match read_frame::<_, Response>(&mut cursor).unwrap() {
            Response::ArchiveImported(got) => assert_eq!(*got, want),
            other => panic!("expected ArchiveImported, got {other:?}"),
        }
    }
}

/// **A verified day from a server that predates `report` decodes as a clean one**, and a clean day
/// is still written in that server's bytes: the field is a pure addition to a reply (decision 0112,
/// verdict 3).
#[test]
fn a_verified_day_without_the_report_is_the_older_servers_bytes() {
    let old = r#"{"Verified":{"ticks":3}}"#;
    let decoded: ArchiveDayResultWire = serde_json::from_str(old).unwrap();
    assert_eq!(decoded, ArchiveDayResultWire::Verified { ticks: 3, report: None });
    assert_eq!(serde_json::to_string(&decoded).unwrap(), old);
}

/// **A client that predates `report` still decodes a reported day**, as verified: the key is unknown
/// to it and ignored. The old client's shape is spelled out as the enum looked before the field.
#[test]
fn a_reported_day_decodes_in_an_older_clients_shape() {
    #[derive(Debug, Deserialize)]
    enum Before {
        Verified { ticks: u64 },
    }
    let new =
        ArchiveDayResultWire::Verified { ticks: 3, report: Some("1 agg_trade_id gaps".into()) };
    let wire = serde_json::to_string(&new).unwrap();
    assert!(
        wire.contains(r#""report":"1 agg_trade_id gaps""#),
        "the new key is on the wire: {wire}"
    );
    let Before::Verified { ticks } =
        serde_json::from_str::<Before>(&wire).unwrap_or_else(|e| panic!("{wire}: {e}"));
    assert_eq!(ticks, 3);
}

/// **An imported day from a server that predates `Imported`'s `report` decodes as a clean one**, and
/// a clean day is still written in that server's bytes.
#[test]
fn an_imported_day_without_the_report_is_the_older_servers_bytes() {
    let old = r#"{"Imported":{"ticks":3,"bars":[{"interval":"1m","rows":2}]}}"#;
    let decoded: ArchiveDayResultWire = serde_json::from_str(old).unwrap();
    let bars = vec![ArchiveDayBars { interval: "1m".into(), rows: 2 }];
    assert_eq!(decoded, ArchiveDayResultWire::Imported { ticks: 3, bars, report: None });
    assert_eq!(serde_json::to_string(&decoded).unwrap(), old);
}

/// **A client that predates `Imported`'s `report` still decodes a reported day**, as imported, in
/// the enum's shape from before the field.
#[test]
fn a_reported_imported_day_decodes_in_an_older_clients_shape() {
    #[derive(Debug, Deserialize)]
    enum Before {
        Imported { ticks: u64, bars: Vec<ArchiveDayBars> },
    }
    let report = "107815 rows repeat an earlier row of the file exactly, and are dropped";
    let new =
        ArchiveDayResultWire::Imported { ticks: 3, bars: vec![], report: Some(report.into()) };
    let wire = serde_json::to_string(&new).unwrap();
    assert!(
        wire.contains(&format!(r#""report":"{report}""#)),
        "the new key is on the wire: {wire}"
    );
    let Before::Imported { ticks, bars } =
        serde_json::from_str::<Before>(&wire).unwrap_or_else(|e| panic!("{wire}: {e}"));
    assert_eq!((ticks, bars), (3, vec![]));
}

#[test]
fn only_free_and_supersede_days_are_importable() {
    let done = populated_done();
    assert_eq!(done.plan.importable_days(), 2, "{:?}", done.plan.days);
    assert!(!ArchiveDayClassWire::TooRecent.is_importable());
    assert!(!ArchiveDayClassWire::HeldByArchive.is_importable());
    assert!(!ArchiveDayClassWire::HeldByHttp.is_importable());
}
