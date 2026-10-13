//! The verb's arithmetic and words, over plans and outcomes built here. The SHIPPED binary
//! against a real datahub with a planted imports directory is
//! `crates/vike-cli/tests/data_cli.rs`'s, and the grammar is `crate::cmd::data`'s `data_tests`.

use vike_datahub_client::archive::{
    ArchiveInventory, BarsWritten, DatasetDir, DayOutcome, DayPlan, ImportOutcome,
};
use vike_datahub_client::proto::FEATURE_ARCHIVE_IMPORT;

use super::plan::{
    actionable, confirmation, dir_failure, dir_lines, months, months_of, plan_lines,
};
use super::run::{
    document, plans_differ_line, progress_line, refusal, refused_failure, run_months, summary_lines,
};
use super::*;

/// Midnight UTC of `y-m-d`, in epoch-ms.
fn day(y: i64, m: u32, d: u32) -> i64 {
    days_from_civil(y, m, d) * MS_PER_DAY
}

fn refusal_of(class: &str) -> DayRefusal {
    DayRefusal { class: class.to_string(), detail: format!("{class} detail") }
}

fn plan_day(d: i64, class: DayClass, declared: Option<u64>) -> DayPlan {
    DayPlan { day: d, class, file_bytes: 100, declared_ticks: declared }
}

/// A present dataset spanning January–February 2024 with every class in it.
fn plan(days: Vec<DayPlan>) -> ImportPlan {
    ImportPlan {
        format: DUKASCOPY_FORMAT.to_string(),
        dataset: "EURUSD".to_string(),
        venue: "dukascopy".to_string(),
        server_dir: "/opt/project/market_data/imports/dukascopy-bi5/EURUSD".to_string(),
        dir: DatasetDir::Present,
        admission: "point value 100000 — vendor page, \"most FX pairs\"".to_string(),
        inventory: ArchiveInventory {
            daily_files: days.len() as u64,
            first_day: days.first().map(|d| d.day),
            last_day: days.last().map(|d| d.day),
            other_layout_days: vec![day(2024, 1, 3)],
            other_objects: 2,
            other_bytes: 40,
            skipped: Vec::new(),
        },
        from_day: days.first().map(|d| d.day),
        to_day: days.last().map(|d| d.day),
        days,
        gaps: vec![day(2024, 1, 4)],
        bars: vec!["1m".to_string()],
        series: None,
    }
}

// ─── the grammar's two parsers ─────────────────────────────────────────────────────────────

/// **`--to` is INCLUSIVE**: both bounds parse to the START of the day they name, so
/// `--from D --to D` is the one day D, whole. A day label, or the epoch-ms of a midnight.
#[test]
fn a_day_bound_is_the_start_of_the_day_it_names() {
    assert_eq!(parse_day("--to", "2024-01-15"), Ok(day(2024, 1, 15)));
    assert_eq!(parse_day("--from", "2024-01-15"), Ok(day(2024, 1, 15)));
    assert_eq!(parse_day("--to", &day(2024, 1, 15).to_string()), Ok(day(2024, 1, 15)));
    assert_eq!(parse_day("--to", "1970-01-01"), Ok(0));
}

/// An instant inside a day, an hour label, a day that does not exist and nonsense are all
/// REFUSED rather than rounded or rolled over.
#[test]
fn a_bound_that_is_not_one_real_day_is_refused() {
    let inside = (day(2024, 1, 15) + 1).to_string();
    let e = parse_day("--to", &inside).unwrap_err();
    assert!(e.contains("not the START of a UTC day") && e.contains("--to"), "{e}");
    let e = parse_day("--from", "2024-02-30").unwrap_err();
    assert!(e.contains("not a calendar date"), "{e}");
    let e = parse_day("--from", "2024-01-15T10").unwrap_err();
    assert!(e.contains("YYYY-MM-DD"), "{e}");
    assert!(parse_day("--from", "yesterday").is_err());
    assert_eq!(parse_day("--from", "2024-02-29"), Ok(day(2024, 2, 29)), "a real leap day");
}

/// `--bars`: absent is `1m`, `none` is no bars, a list is held to the wire's own rules here.
#[test]
fn bars_default_to_1m_none_is_empty_and_a_list_meets_the_wires_rules() {
    assert_eq!(parse_bars(None), Ok(vec!["1m".to_string()]));
    assert_eq!(parse_bars(Some("none")), Ok(Vec::<String>::new()));
    assert_eq!(parse_bars(Some("1m,5m, 1h")), Ok(vec!["1m".into(), "5m".into(), "1h".into()]));
    for (bad, needle) in [
        ("7m", "does not divide a UTC day"),
        ("1m,1m", "named twice"),
        ("01m", "canonical"),
        ("1m,5m,15m,1h,4h", "IMPORT_MAX_BAR_INTERVALS"),
        ("none,1m", "stands alone"),
        ("", "EMPTY"),
        ("1m,,5m", "empty entry"),
    ] {
        let e = parse_bars(Some(bad)).unwrap_err();
        assert!(e.contains(needle), "{bad:?} must say {needle:?}: {e}");
    }
}

// ─── the month cut ─────────────────────────────────────────────────────────────────────────

/// **A window over a month boundary is TWO requests**, cut at the 1st, each keeping the
/// window's own outer bound — and the pieces tile it.
#[test]
fn a_window_over_a_month_boundary_is_two_requests() {
    assert_eq!(
        months(day(2024, 1, 30), day(2024, 2, 2)),
        vec![(day(2024, 1, 30), day(2024, 1, 31)), (day(2024, 2, 1), day(2024, 2, 2))]
    );
}

/// The cut's other edges: one day is one request; a whole leap February is one; December rolls
/// into January; and over a long window every piece is inside one month, at most 31 days, and
/// the pieces tile the window with no day twice and none missing.
#[test]
fn the_months_tile_the_window_and_none_is_over_the_wires_day_cap() {
    assert_eq!(
        months(day(2024, 1, 15), day(2024, 1, 15)),
        vec![(day(2024, 1, 15), day(2024, 1, 15))]
    );
    assert_eq!(
        months(day(2024, 2, 1), day(2024, 2, 29)),
        vec![(day(2024, 2, 1), day(2024, 2, 29))]
    );
    assert_eq!(
        months(day(2023, 12, 31), day(2024, 1, 1)),
        vec![(day(2023, 12, 31), day(2023, 12, 31)), (day(2024, 1, 1), day(2024, 1, 1))]
    );
    let (from, to) = (day(2003, 5, 4), day(2026, 9, 27));
    let pieces = months(from, to);
    assert_eq!(pieces.len(), (2026 - 2003) * 12 + (9 - 5) + 1);
    assert_eq!(pieces.first().map(|p| p.0), Some(from));
    assert_eq!(pieces.last().map(|p| p.1), Some(to));
    for pair in pieces.windows(2) {
        assert_eq!(pair[0].1 + MS_PER_DAY, pair[1].0, "a gap or an overlap: {pair:?}");
        assert_eq!(civil_from_days(pair[1].0 / MS_PER_DAY).2, 1, "a cut off the 1st: {pair:?}");
    }
    for (a, b) in &pieces {
        assert_eq!(month_label(*a), month_label(*b), "a piece spans two months");
        let span = (b - a) / MS_PER_DAY + 1;
        assert!((1..=31).contains(&span), "{span} days: over IMPORT_MAX_DAYS");
    }
    assert!(months(day(2024, 2, 1), day(2024, 1, 1)).is_empty(), "an inverted window is none");
}

/// The months requested are the server's resolved window clipped to the dataset's own files —
/// a `--from 1970-01-01` does not cost six hundred empty requests.
#[test]
fn the_months_are_clipped_to_the_datasets_own_files() {
    let mut p = plan(vec![
        plan_day(day(2024, 1, 30), DayClass::Free, None),
        plan_day(day(2024, 2, 2), DayClass::Free, None),
    ]);
    p.from_day = Some(0);
    p.to_day = Some(day(2030, 1, 1));
    assert_eq!(
        months_of(&p),
        vec![(day(2024, 1, 30), day(2024, 1, 31)), (day(2024, 2, 1), day(2024, 2, 2))]
    );
    p.inventory.first_day = None;
    p.inventory.last_day = None;
    assert!(months_of(&p).is_empty(), "no daily file: nothing to request");
}

/// [`run_months`] sends exactly the cut, in order, and stops at the first failure — naming the
/// month, what the months before it did, and how to resume.
#[test]
fn the_months_are_sent_in_order_and_a_failure_stops_the_run() {
    let p = plan(vec![
        plan_day(day(2024, 1, 30), DayClass::Free, None),
        plan_day(day(2024, 2, 1), DayClass::Free, None),
        plan_day(day(2024, 3, 1), DayClass::Free, None),
    ]);
    let cut = months_of(&p);
    assert_eq!(cut.len(), 3);
    let mut sent = Vec::new();
    let mut lines = Vec::new();
    let answer = |from: i64| ImportDone {
        plan: plan(vec![plan_day(from, DayClass::Free, None)]),
        outcome: Some(ImportOutcome {
            days: vec![DayOutcome {
                day: from,
                result: DayResult::Imported { ticks: 5, bars: Vec::new() },
            }],
        }),
    };
    let failed = run_months(
        &cut,
        &p,
        Mode::Import,
        "HEADER",
        |from, to| {
            sent.push((from, to));
            if from == day(2024, 3, 1) {
                Err("the store failed".to_string())
            } else {
                Ok(answer(from))
            }
        },
        |line| lines.push(line.to_string()),
    )
    .unwrap_err();
    assert_eq!(sent, cut, "every month up to the failed one, in order");
    assert_eq!(lines.len(), 3, "the header and one line per answered month: {lines:?}");
    assert!(lines[1].starts_with("  1/3  2024-01  1 day  5 ticks  0 refused"), "{lines:?}");
    let msg = failed.message(Mode::Import);
    assert!(msg.starts_with("month 3 of 3 (2024-03) failed"), "{msg}");
    assert!(msg.contains("the 2 months before it imported 2 days, which stay stored"), "{msg}");
    assert!(msg.contains("Re-run the same command to resume") && msg.ends_with("the store failed"));
}

// ─── the plan ──────────────────────────────────────────────────────────────────────────────

#[test]
fn digits_are_grouped_in_threes() {
    for (n, want) in [(0, "0"), (999, "999"), (1_000, "1,000"), (96_312_004, "96,312,004")] {
        assert_eq!(thousands(n), want);
    }
}

/// The plan draws every class the server sent, with its count — and the confirmation names the
/// IMPORTABLE count, never the held or the refused.
#[test]
fn the_plan_names_every_class_and_the_confirmation_names_the_importable_count() {
    let p = plan(vec![
        plan_day(day(2024, 1, 2), DayClass::HeldByArchive, None),
        plan_day(day(2024, 1, 5), DayClass::HeldByHttp, None),
        plan_day(day(2024, 1, 8), DayClass::Free, Some(1_000)),
        plan_day(day(2024, 1, 9), DayClass::Supersede { key: "k".into() }, Some(2_000)),
        plan_day(day(2024, 1, 10), DayClass::Overlapped { keys: vec!["a".into()] }, None),
        plan_day(day(2024, 1, 11), DayClass::Refused(refusal_of("MixedLayout")), None),
        plan_day(day(2024, 1, 12), DayClass::TooRecent, None),
    ]);
    let text = plan_lines(&p, "127.0.0.1:7878").join("\n");
    for needle in [
        "dukascopy-bi5 · EURUSD — datahub at 127.0.0.1:7878",
        "server dir  /opt/project/market_data/imports/dukascopy-bi5/EURUSD   (on the DATAHUB's box)",
        "files       7 daily files 2024-01-02 .. 2024-01-12 · 1 day in another layout",
        "window      2024-01-02 .. 2024-01-12 → 7 daily files",
        "held        2 days (1 by this lane · 1 by the HTTP lane)",
        "overlapped  1 day, each met by an earlier fetch's",
        "2024-01-10",
        "too recent  1 day inside the vendor's publication margin",
        "refused     1 day — no commit key is spent",
        "2024-01-11  MixedLayout — MixedLayout detail",
        "import      2 days (1 of them supersede a provisional fetch window) · 3,000 ticks (exact \
         from 2 file headers)",
        "bars        1m, resampled per day from the stored ticks",
        "gaps        1 weekday with no file",
        "growth unknown until the first month lands",
        "price       point value 100000",
    ] {
        assert!(text.contains(needle), "missing {needle:?}:\n{text}");
    }
    assert_eq!(confirmation(&p), "import 2 days");
    assert_eq!(actionable(&p, Mode::Import), 4, "the importable AND the held: the top-up");
    assert_eq!(actionable(&p, Mode::Verify), 2, "a verify decodes the importable alone");
    assert_eq!(actionable(&p, Mode::Plan), 0);
}

/// The store line measures, and never guesses: bytes per tick from the series the store holds,
/// times the ticks the headers declared.
#[test]
fn the_store_growth_is_measured_on_the_series_or_said_to_be_unknown() {
    let mut p = plan(vec![plan_day(day(2024, 1, 8), DayClass::Free, Some(100_000_000))]);
    p.series = Some(vike_data::SeriesCoverage {
        first_ts: 0,
        last_ts: 1,
        rows: 1_000,
        bytes: 12_600,
        parts: 1,
        dates: 1,
    });
    let text = plan_lines(&p, "a:1").join("\n");
    assert!(
        text.contains("store       ≈ +1.3 GB (12.6 bytes/tick, measured on this series)"),
        "{text}"
    );
    p.days[0].declared_ticks = None;
    let text = plan_lines(&p, "a:1").join("\n");
    assert!(text.contains("tick count unknown"), "{text}");
    assert!(text.contains("12.6 bytes/tick measured on this series"), "{text}");
}

/// Long lists are capped in the human plan and say how many they withheld.
#[test]
fn a_long_list_is_capped_and_says_what_it_withheld() {
    let mut p = plan(vec![plan_day(day(2024, 1, 2), DayClass::Free, None)]);
    p.gaps = (0..25).map(|n| day(2024, 3, 1) + n * MS_PER_DAY).collect();
    let text = plan_lines(&p, "a:1").join("\n");
    assert!(text.contains("25 weekdays with no file"), "{text}");
    assert!(text.contains("… and 15 more (the --json document carries every one)"), "{text}");
    assert!(!text.contains("2024-03-11"), "the eleventh date must be withheld: {text}");
}

/// An absent directory names the SERVER's path and the two ways to fill it, paste-ready with
/// the dataset and the directory substituted; an unreadable one says what to fix; and the plan
/// stops at the directory line for both.
#[test]
fn an_absent_dataset_names_the_servers_path_and_the_two_ways_to_fill_it() {
    let mut p = plan(Vec::new());
    p.dir = DatasetDir::Absent;
    let plan_text = plan_lines(&p, "127.0.0.1:7878");
    assert_eq!(plan_text.len(), 2, "{plan_text:?}");
    assert!(plan_text[1].contains("NOT FOUND on the datahub's box"), "{plan_text:?}");
    let text = dir_lines(&p, "127.0.0.1:7878").join("\n");
    for needle in [
        "the files must be on the DATAHUB's box",
        "answered at 127.0.0.1:7878",
        "aws s3 sync s3://cfg-public-proper-wallaby/EURUSD/ \
         /opt/project/market_data/imports/dukascopy-bi5/EURUSD/ --region eu-west-1 \
         --request-payer requester --profile dukascopy --exclude \"*\" --include \"*_ticks.bi5\"",
        "rsync -a ./EURUSD/ <box>:/opt/project/market_data/imports/dukascopy-bi5/EURUSD/",
        "then run this command again",
    ] {
        assert!(text.contains(needle), "missing {needle:?}:\n{text}");
    }
    assert!(!text.contains("--delete"), "a sync line must never carry --delete: {text}");
    assert!(dir_failure(&p).contains("no such directory on the datahub's box"));

    p.format = "another-format".to_string();
    let text = dir_lines(&p, "a:1").join("\n");
    assert!(!text.contains("aws s3"), "no vendor line for a format with no hint: {text}");
    assert!(text.contains("rsync -a ./EURUSD/"), "{text}");

    p.dir = DatasetDir::Unreadable { why: "PermissionDenied".to_string() };
    assert!(
        plan_lines(&p, "a:1")[1]
            .contains("EXISTS, and the datahub cannot read it: PermissionDenied")
    );
    assert!(dir_lines(&p, "a:1")[0].contains("ProtectHome=yes"));
    assert!(dir_failure(&p).contains("cannot read its directory"));
}

/// The one format id this side keys a hint on is the SERVER's registry id, read from that
/// crate's source rather than restated — this crate's test build cannot link the registry,
/// which lives behind `backfill-serve`.
#[test]
fn the_sync_hint_names_the_servers_own_format_id() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../vike-datahub/src/import/formats.rs");
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert!(
        src.contains(&format!("pub const DUKASCOPY_BI5: &str = \"{DUKASCOPY_FORMAT}\";")),
        "the server's registry id moved, and the sync hint would never be printed"
    );
}

// ─── the months' outcome ───────────────────────────────────────────────────────────────────

fn month(from: i64, days: Vec<DayPlan>, rows: Vec<DayOutcome>, preview: u64) -> MonthDone {
    MonthDone {
        from,
        to: from,
        done: ImportDone { plan: plan(days), outcome: Some(ImportOutcome { days: rows }) },
        preview_importable: preview,
        elapsed: Duration::from_millis(1_500),
    }
}

/// A refused day is a DEFECT only when the server's plan did not already name it too recent,
/// overlapped or refused: an overlapped day's echo in the outcome never fails a run, a
/// plan-time refusal is counted once, and a day the plan called importable that the decode
/// refused is a defect.
#[test]
fn only_a_refusal_for_what_is_in_the_file_is_a_defect() {
    let (d1, d2, d3, d4) = (day(2024, 1, 8), day(2024, 1, 9), day(2024, 1, 10), day(2024, 1, 11));
    let m = month(
        d1,
        vec![
            plan_day(d1, DayClass::Free, Some(4)),
            plan_day(d2, DayClass::Free, Some(4)),
            plan_day(d3, DayClass::Overlapped { keys: vec!["k".into()] }, None),
            plan_day(d4, DayClass::Refused(refusal_of("MixedLayout")), None),
        ],
        vec![
            DayOutcome {
                day: d1,
                result: DayResult::Imported {
                    ticks: 4,
                    bars: vec![BarsWritten { interval: "1m".into(), rows: 3 }],
                },
            },
            DayOutcome { day: d2, result: DayResult::Refused(refusal_of("AmbiguousTimeBase")) },
            DayOutcome { day: d3, result: DayResult::Refused(refusal_of("Overlapped")) },
            DayOutcome { day: d4, result: DayResult::Refused(refusal_of("MixedLayout")) },
        ],
        2,
    );
    let t = Tally::of_month(&m.done);
    assert_eq!((t.imported, t.ticks, t.bars, t.overlapped), (1, 4, 3, 1));
    let classes: Vec<&str> = t.refused.iter().map(|(_, r)| r.class.as_str()).collect();
    assert_eq!(classes, vec!["AmbiguousTimeBase", "MixedLayout"], "{t:?}");
    let line = progress_line(1, 1, &m, Mode::Import);
    assert!(line.starts_with("  1/1  2024-01  1 day  4 ticks  2 refused  1.5s"), "{line}");
    assert!(line.ends_with("(2024-01-09: AmbiguousTimeBase — see --dry-run --verify)"), "{line}");
    assert!(is_a_defect(None), "a day the plan never named is a defect if refused");
    assert!(is_a_defect(Some(&DayClass::HeldByArchive)), "a failed top-up is a defect");
}

/// **Two plans, one authority**: when the datahub's month plans count a different number of
/// importable days than the preview, the summary prints BOTH and names the datahub's as the
/// authority — and the month whose plan moved says so on its own line. Equal counts print
/// nothing about it.
#[test]
fn when_the_two_plans_differ_both_counts_are_printed() {
    let (d1, d2) = (day(2024, 1, 8), day(2024, 1, 9));
    let preview =
        plan(vec![plan_day(d1, DayClass::Free, None), plan_day(d2, DayClass::Free, None)]);
    let moved = month(
        d1,
        vec![plan_day(d1, DayClass::Free, None), plan_day(d2, DayClass::HeldByHttp, None)],
        vec![
            DayOutcome { day: d1, result: DayResult::Imported { ticks: 1, bars: Vec::new() } },
            DayOutcome { day: d2, result: DayResult::ToppedUp { bars: Vec::new() } },
        ],
        2,
    );
    let t = Tally::of(std::slice::from_ref(&moved));
    let line = plans_differ_line(&preview, &t).expect("the counts differ");
    assert!(line.contains("the preview counted 2 days importable"), "{line}");
    assert!(line.contains("the datahub counted 1 day as it went"), "{line}");
    assert!(line.contains("The datahub's count is the authority"), "{line}");
    let summary = summary_lines(&preview, &t, Mode::Import).join("\n");
    assert!(summary.contains(&line), "{summary}");
    assert!(summary.ends_with(
        "done — 1 day imported (1 ticks, 0 bars) · 0 refused · 0 overlapped · 1 gap · 1 already held"
    ), "{summary}");
    assert!(
        progress_line(1, 1, &moved, Mode::Import)
            .contains("(the datahub planned 1 importable here, the preview 2)"),
    );

    let same =
        plan(vec![plan_day(d1, DayClass::Free, None), plan_day(d2, DayClass::HeldByHttp, None)]);
    assert_eq!(plans_differ_line(&same, &t), None, "equal counts say nothing");
}

/// A verify's summary counts decoded days and says nothing was written.
#[test]
fn a_verify_summary_counts_decoded_days_and_says_nothing_was_written() {
    let d1 = day(2024, 1, 8);
    let m = month(
        d1,
        vec![plan_day(d1, DayClass::Free, None)],
        vec![DayOutcome { day: d1, result: DayResult::Verified { ticks: 1_234 } }],
        1,
    );
    let t = Tally::of(std::slice::from_ref(&m));
    let preview = plan(vec![plan_day(d1, DayClass::Free, None)]);
    let summary = summary_lines(&preview, &t, Mode::Verify);
    assert_eq!(
        summary.last().map(String::as_str),
        Some("verified — 1 day decoded clean (1,234 ticks) · 0 refused · nothing was written")
    );
    assert!(
        progress_line(1, 1, &m, Mode::Verify).starts_with("  1/1  2024-01  1 day  1,234 ticks")
    );
}

/// The `--json` document carries the preview in the wire's own words, one object per month
/// with the server's outcome, both importable counts and the defect days — and a plan-only run
/// has no months and no summary.
#[test]
fn the_document_carries_the_plan_the_months_and_both_counts() {
    let d1 = day(2024, 1, 8);
    let preview = plan(vec![plan_day(d1, DayClass::Free, Some(9))]);
    let m = month(
        d1,
        vec![plan_day(d1, DayClass::Free, Some(9))],
        vec![DayOutcome { day: d1, result: DayResult::Refused(refusal_of("NotMonotonic")) }],
        1,
    );
    let doc = document("127.0.0.1:7878", Mode::Import, &preview, std::slice::from_ref(&m));
    assert_eq!(doc["mode"], "import");
    assert_eq!(doc["confirmation"], "import 1 days");
    assert_eq!(doc["plan"]["server_dir"], preview.server_dir.as_str());
    assert_eq!(doc["plan"]["days"][0]["class"], "Free");
    assert_eq!(doc["months"][0]["month"], "2024-01");
    assert_eq!(
        doc["months"][0]["outcome"]["days"][0]["result"]["Refused"]["class"],
        "NotMonotonic"
    );
    assert_eq!(doc["summary"]["preview_importable"], 1);
    assert_eq!(doc["summary"]["server_importable"], 1);
    assert_eq!(doc["summary"]["refused"][0]["date"], "2024-01-08");

    let plan_only = document("a:1", Mode::Plan, &preview, &[]);
    assert_eq!(plan_only["months"], serde_json::json!([]));
    assert_eq!(plan_only["summary"], serde_json::Value::Null);
    assert_eq!(plan_only["confirmation"], serde_json::Value::Null);
}

/// The capability refusal gains what to DO; every other refusal is the server's own sentence.
#[test]
fn the_capability_refusal_says_what_to_do_and_nothing_else_is_rewritten() {
    let missing = "datahub server does not advertise `archive_import` (advertised: []) — nothing \
                   was sent.";
    let e = refusal(missing.to_string(), "127.0.0.1:7878");
    assert!(e.starts_with(missing), "{e}");
    assert!(e.contains("the datahub at 127.0.0.1:7878 has no archive import lane"), "{e}");
    assert!(e.contains("v0.1.41") && e.contains("ARCHIVE IMPORT lane mounted"), "{e}");
    let other = "ImportArchive: another archive import (or `verify`) is already running";
    assert_eq!(refusal(other.to_string(), "a:1"), other);
}

/// Every `vike-cli …` line this verb's answers tell an operator to type PARSES — held to the
/// real grammar through `crate::cmd::accepts`, never to a second copy of its spelling.
#[test]
fn every_command_these_answers_tell_an_operator_to_type_parses() {
    let backticked = |line: &str| -> Vec<String> {
        line.split('`').skip(1).step_by(2).map(String::from).collect()
    };
    let p = plan(vec![plan_day(day(2024, 1, 8), DayClass::Free, None)]);
    let mut texts: Vec<String> = plan_lines(&p, "a:1");
    texts.extend(dir_lines(
        &{
            let mut a = p.clone();
            a.dir = DatasetDir::Absent;
            a
        },
        "a:1",
    ));
    texts.push(refused_failure(1, Mode::Import));
    texts.push(refusal(format!("does not advertise `{FEATURE_ARCHIVE_IMPORT}`"), "a:1"));
    let commands: Vec<String> =
        texts.iter().flat_map(|t| backticked(t)).filter(|c| c.starts_with("vike-cli ")).collect();
    for command in &commands {
        let argv: Vec<&str> = command.split_whitespace().collect();
        crate::cmd::accepts(&argv[1..])
            .unwrap_or_else(|e| panic!("this answer tells an operator to run `{command}`: {e}"));
    }
    // The pointer the progress line prints is a FLAG pair, not a whole command: it must name
    // flags the verb takes together.
    crate::cmd::accepts(&[
        "data",
        "hist",
        "import",
        DUKASCOPY_FORMAT,
        "EURUSD",
        "--dry-run",
        "--verify",
    ])
    .expect("`--dry-run --verify` is a line the import verb accepts");
}
