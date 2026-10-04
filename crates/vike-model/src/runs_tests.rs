use super::*;
use serde_json::json;

fn a_manifest(run_id: &str) -> RunManifest {
    RunManifest {
        schema: MANIFEST_SCHEMA,
        run_id: run_id.to_string(),
        kind: BACKTEST_RUN_KIND.to_string(),
        produced_by: "backtest".to_string(),
        started_at: utc_rfc3339(1_756_000_000),
        finished_at: utc_rfc3339(1_756_000_012),
        git_sha: None,
        fingerprint: None,
        config: RunConfig {
            path: Some("profiles/sma.toml".to_string()),
            name: Some("sma cross".to_string()),
        },
        detail: json!({ "strategy": "sma_cross" }),
    }
}

/// The collision rule, stated as the property that matters: the SAME clock second, minted
/// twice, must not name one directory. The research producer's bare-seconds id failed exactly
/// this, which is why the rule is decided here rather than inherited.
#[test]
fn two_runs_starting_in_the_same_second_cannot_share_a_run_id() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");

    let first = create_run_dir(&runs, 1_756_000_000, None).unwrap();
    let second = create_run_dir(&runs, 1_756_000_000, None).unwrap();

    assert_ne!(first.run_id, second.run_id, "one clock second must not mint one id twice");
    assert_ne!(first.path, second.path);
    assert!(first.path.is_dir(), "minting a run id CREATES the directory — that is the check");
    assert!(second.path.is_dir());
}

/// Creation is the check, so an id whose directory already exists must be refused rather than
/// reused: handing one out twice lets the second run overwrite the first one's report. The
/// taken id is built with the SAME function the minter uses, so this test cannot drift away
/// from the id format.
#[test]
fn an_id_whose_directory_already_exists_is_never_handed_out() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    let taken = run_id_at(1_756_000_000, Some(FP), 0);
    std::fs::create_dir(runs.join(&taken)).unwrap();
    std::fs::write(runs.join(&taken).join(REPORT_FILE), b"the first run's result").unwrap();

    let minted = create_run_dir(&runs, 1_756_000_000, Some(FP)).unwrap();

    assert_ne!(minted.run_id, taken);
    assert!(
        minted.run_id.ends_with("-1"),
        "a taken id must advance to the NEXT seq, not to some unrelated name: {}",
        minted.run_id
    );
    assert_eq!(
        std::fs::read(runs.join(&taken).join(REPORT_FILE)).unwrap(),
        b"the first run's result",
        "the run already holding that id keeps its report"
    );
}

/// The id opens with the second the run started, so a plain directory listing sorts by when —
/// the one ordering a human scanning `user_data/runs/` actually wants. True in BOTH forms;
/// that is what makes a content address safe to put in the name at all.
#[test]
fn a_run_id_opens_with_the_second_its_run_started() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");

    for fp in [None, Some(FP)] {
        let minted = create_run_dir(&runs, 1_756_000_000, fp).unwrap();
        assert!(
            minted.run_id.starts_with("1756000000-"),
            "run id {} must open with its start second",
            minted.run_id
        );
    }
}

/// The whole reason this manifest is common: a listing renders a row off the TOP level, without
/// knowing what kind of run produced the file.
#[test]
fn every_common_field_sits_at_the_top_level_so_a_listing_needs_no_per_kind_parser() {
    let v = serde_json::to_value(a_manifest("r-1")).unwrap();
    let obj = v.as_object().expect("a manifest is a JSON object");

    for key in [
        "schema",
        "run_id",
        "kind",
        "produced_by",
        "started_at",
        "finished_at",
        "git_sha",
        "fingerprint",
        "config",
    ] {
        assert!(obj.contains_key(key), "a listing row needs `{key}` at the top level");
    }
}

/// Kind-specific detail nests BELOW the common fields. Beside them is the defect the nesting
/// exists to refuse — it forces every reader to know the kind before it can read the first key.
#[test]
fn kind_specific_detail_nests_below_the_common_fields_rather_than_beside_them() {
    let v = serde_json::to_value(a_manifest("r-1")).unwrap();
    let obj = v.as_object().unwrap();

    assert_eq!(obj["detail"]["strategy"], json!("sma_cross"));
    assert!(
        !obj.contains_key("strategy"),
        "`strategy` is a backtest's business and must not sit beside the common fields"
    );
}

/// `git_sha` is written even when the producer cannot name a build. A missing key would be
/// indistinguishable from a manifest predating the field; `null` says "this producer does not
/// know", which is a different and reportable answer.
#[test]
fn a_producer_that_cannot_name_a_build_still_writes_the_git_sha_key() {
    let v = serde_json::to_value(a_manifest("r-1")).unwrap();

    assert_eq!(v.as_object().unwrap().get("git_sha"), Some(&serde_json::Value::Null));
}

/// A run directory holds BOTH documents, and the report is stored verbatim — a report's JSON is
/// a machine contract, and persisting it must not reshape it.
#[test]
fn a_written_run_holds_the_report_verbatim_beside_its_manifest() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    let report = json!({ "name": "sma cross", "sharpe": 1.25, "trades": 42 });

    write_run(&run.path, &a_manifest(&run.run_id), &report).unwrap();

    let back: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run.path.join(REPORT_FILE)).unwrap())
            .unwrap();
    assert_eq!(back, report);
    let m: RunManifest =
        serde_json::from_str(&std::fs::read_to_string(run.path.join(MANIFEST_FILE)).unwrap())
            .unwrap();
    assert_eq!(m.run_id, run.run_id);
    assert_eq!(m.kind, "backtest");
}

/// Persisting is additive, never a new way to fail: a runs root that cannot be created comes
/// back as a VALUE naming the path, so the caller can print the result it already computed and
/// report the failure beside it.
#[test]
fn a_runs_root_that_cannot_be_created_is_reported_rather_than_panicking() {
    let root = tempfile::tempdir().unwrap();
    let blocked = root.path().join("blocked");
    std::fs::write(&blocked, b"a file where the runs directory would go").unwrap();

    let err = create_run_dir(&blocked, 1_756_000_000, None).unwrap_err();

    assert!(
        matches!(err, RunPersistError::Dir { .. }),
        "expected a directory failure, got {err:?}"
    );
    assert!(err.to_string().contains("blocked"), "the message must name the path: {err}");
}

/// The round trip that makes a listing possible at all: what [`write_run`] wrote comes back as
/// the same COMMON fields, through the reader that lives beside the writer rather than through
/// a second spelling of the schema somewhere up the dependency graph.
#[test]
fn a_written_manifest_reads_back_with_every_common_field_intact() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    let written = a_manifest(&run.run_id);
    write_run(&run.path, &written, &json!({ "sharpe": 1.25 })).unwrap();

    let back = read_manifest(&run.path).unwrap();

    assert_eq!(back.run_id, written.run_id);
    assert_eq!(back.kind, "backtest");
    assert_eq!(back.produced_by, "backtest");
    assert_eq!(back.started_at, written.started_at);
    assert_eq!(back.finished_at, written.finished_at);
    assert_eq!(back.git_sha, None);
    assert_eq!(back.config.path.as_deref(), Some("profiles/sma.toml"));
    assert_eq!(back.detail["strategy"], json!("sma_cross"));
}

/// The manifest is written LAST, so its ABSENCE is a distinct answer rather than a corrupt
/// file: the run is being written right now, or a process died between the two writes. A reader
/// that collapsed this into "unreadable" would make a listing call a running backtest broken.
#[test]
fn a_directory_whose_manifest_is_absent_reads_as_missing_not_as_unreadable() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    // The half-written shape exactly: the report landed, the manifest has not.
    std::fs::write(run.path.join(REPORT_FILE), b"{}\n").unwrap();

    let err = read_manifest(&run.path).unwrap_err();

    assert!(matches!(err, RunReadError::Missing { .. }), "expected Missing, got {err:?}");
    assert!(
        err.to_string().contains(MANIFEST_FILE),
        "the message must name the file it looked for: {err}"
    );
}

/// A corrupt manifest is REPORTED, never treated as absent — a run that vanishes from a listing
/// is worse than one that shows as broken, and the two have different fixes.
#[test]
fn a_manifest_that_is_not_json_is_reported_with_the_parser_error_and_the_path() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    std::fs::write(run.path.join(MANIFEST_FILE), b"{ not json").unwrap();

    let err = read_manifest(&run.path).unwrap_err();

    match &err {
        RunReadError::Parse { path, why } => {
            assert!(path.ends_with(MANIFEST_FILE), "the path must be the manifest: {path:?}");
            assert!(!why.is_empty(), "serde_json's own words must be carried through");
        }
        other => panic!("expected Parse, got {other:?}"),
    }
}

/// A manifest that parses as JSON but is missing a COMMON field is a parse failure too: the
/// common fields are what a listing renders a row from, so a document without them is not a
/// manifest, whatever it is.
#[test]
fn a_json_document_missing_a_common_field_is_a_parse_failure() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    std::fs::write(run.path.join(MANIFEST_FILE), br#"{ "run_id": "r-1" }"#).unwrap();

    let err = read_manifest(&run.path).unwrap_err();

    assert!(matches!(err, RunReadError::Parse { .. }), "expected Parse, got {err:?}");
}

/// A manifest that exists and cannot be READ is its own answer — a permissions bug must not
/// wear the "not written yet" reply, for the same reason the credential store refuses to.
#[test]
fn a_manifest_that_cannot_be_read_is_distinct_from_one_that_is_absent() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    // A DIRECTORY where the file belongs: unreadable as text on every platform this ships to,
    // and reached without asking a test to change file permissions.
    std::fs::create_dir(run.path.join(MANIFEST_FILE)).unwrap();

    let err = read_manifest(&run.path).unwrap_err();

    assert!(matches!(err, RunReadError::Read { .. }), "expected Read, got {err:?}");
    assert!(err.to_string().contains(MANIFEST_FILE), "the message must name the path: {err}");
}

/// Under the cap nothing is touched: a stride of `1` is the claim "this is the whole curve",
/// and a reader keys on it to know whether a statistic recomputed from the file is exact.
#[test]
fn a_curve_that_fits_under_the_cap_is_kept_whole_at_stride_one() {
    let samples: Vec<f64> = (0..100).map(f64::from).collect();

    let (kept, stride) = decimate(&samples, 1_000);

    assert_eq!(kept, samples, "nothing may be dropped under the cap");
    assert_eq!(stride, 1, "stride 1 IS the exactness claim");
}

/// The bound is the whole point: a curve far over the cap comes back bounded, and the stride
/// says by how much it was thinned.
#[test]
fn a_curve_over_the_cap_is_thinned_to_the_cap_and_says_by_how_much() {
    let samples: Vec<u32> = (0..100_000).collect();

    let (kept, stride) = decimate(&samples, 1_000);

    assert!(stride > 1, "a thinned curve must not claim stride 1");
    assert!(
        kept.len() <= 1_001,
        "the cap is soft by exactly one — the appended last sample: {}",
        kept.len()
    );
    assert_eq!(kept[0], 0, "the first sample is the run's opening equity");
}

/// The LAST sample is the run's OUTCOME. A decimation that dropped it would make
/// `final_equity` derived from the file disagree with the one in `report.json`, which is the
/// single comparison a reader is most likely to make.
#[test]
fn the_final_sample_survives_a_stride_that_does_not_land_on_it() {
    // 8 samples into a cap of 3 gives stride 3: indices 0, 3, 6 — and 7 is the one that must
    // be appended, because the stride does not land on it.
    let samples: Vec<u32> = (0..8).collect();

    let (kept, stride) = decimate(&samples, 3);

    assert_eq!(stride, 3);
    assert_eq!(*kept.last().unwrap(), 7, "the last sample must survive: {kept:?}");
    assert_eq!(kept, vec![0, 3, 6, 7]);
}

/// A cap of zero is "keep nothing" — the shape a future `--keep-series none` spells — and it
/// reports stride `0` so a reader can tell it apart from an empty run.
#[test]
fn a_cap_of_zero_keeps_nothing_and_says_so_with_stride_zero() {
    let samples: Vec<u32> = (0..10).collect();

    let (kept, stride) = decimate(&samples, 0);

    assert!(kept.is_empty());
    assert_eq!(stride, 0, "stride 0 means NOTHING was kept, not `kept everything`");
}

/// An empty curve is a real run (a slice with no rows), not an error, and it claims exactness.
#[test]
fn an_empty_curve_is_exact_rather_than_thinned() {
    let samples: Vec<f64> = Vec::new();

    let (kept, stride) = decimate(&samples, 1_000);

    assert!(kept.is_empty());
    assert_eq!(stride, 1);
}

fn a_series() -> RunSeries {
    RunSeries {
        schema: SERIES_SCHEMA,
        equity: vec![10_000.0, 10_100.0, 9_950.0],
        equity_ts: vec![1_756_000_000_000, 1_756_000_060_000, 1_756_000_120_000],
        per_symbol_equity: vec![("BTCUSDT".to_string(), vec![0.0, 100.0, -50.0])],
        stride: 1,
        source_len: 3,
        diagnostics: RunDiagnostics {
            warmup: 20,
            intrabar_both_hit: 1,
            stale_deferrals: 2,
            impact_unpriced: 3,
            session_deferrals: 4,
            below_min_reversals: 5,
            maker_fills: 6,
            taker_fills: 7,
            fees_paid: 8.5,
            dropped: vec![DroppedOrder {
                symbol: "BTCUSDT".to_string(),
                reason: "risk_gate:max_notional".to_string(),
                size: 0.5,
                weight: 1.0,
            }],
        },
    }
}

fn a_trade(pnl: f64) -> crate::Trade {
    crate::Trade {
        entry_price: 100.0,
        exit_price: 100.0 + pnl,
        size: 1.0,
        pnl,
        fees: 0.1,
        entry_ts: 1_756_000_000_000,
        exit_ts: 1_756_000_060_000,
        symbol: "BTCUSDT".to_string(),
        mae: 0.0,
        mfe: pnl.max(0.0),
        is_long: true,
    }
}

/// The whole point of the stage: what the run computed comes BACK, typed, through the reader
/// that lives beside the writer. A blob would have made the schema version meaningless.
#[test]
fn a_written_series_reads_back_with_every_sample_and_every_counter_intact() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    let written = a_series();
    let extras = RunExtras { series: Some(&written), ..Default::default() };

    write_run_with(&run.path, &a_manifest(&run.run_id), &json!({}), &extras).unwrap();
    let back = read_series(&run.path).unwrap();

    assert_eq!(back.schema, SERIES_SCHEMA);
    assert_eq!(back.equity, written.equity);
    assert_eq!(back.equity_ts, written.equity_ts);
    assert_eq!(back.per_symbol_equity, written.per_symbol_equity);
    assert_eq!(back.stride, 1);
    assert_eq!(back.source_len, 3);
    assert_eq!(back.diagnostics.warmup, 20);
    assert_eq!(back.diagnostics.stale_deferrals, 2);
    assert_eq!(back.diagnostics.dropped.len(), 1);
    assert_eq!(back.diagnostics.dropped[0].reason, "risk_gate:max_notional");
}

/// The parallel-vector hazard, refused by an invariant a reader can CHECK rather than by a
/// convention it has to trust: `vike_analytics::metrics::returns` SKIPS zero-denominator steps,
/// so a returns vector is not index-alignable with a timestamp vector — which is exactly why
/// this record persists the CURVE and lets a reader derive returns from it.
#[test]
fn a_series_is_aligned_when_its_timestamps_match_its_samples_or_are_absent() {
    assert!(a_series().is_aligned(), "equal lengths are aligned");

    let untimed = RunSeries { equity_ts: Vec::new(), ..a_series() };
    assert!(untimed.is_aligned(), "an untracked-timestamp run is the other valid shape");

    let ragged = RunSeries { equity_ts: vec![1, 2], ..a_series() };
    assert!(!ragged.is_aligned(), "two lengths that are neither equal nor empty is not a document");
}

/// A ledger is a PREFIX when it is bounded, never a sample: `source_len` is what tells a reader
/// it is holding part of a ledger rather than all of a short one.
#[test]
fn a_bounded_trade_ledger_says_how_many_trades_the_run_actually_closed() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    let written = RunTrades {
        schema: TRADES_SCHEMA,
        trades: vec![a_trade(1.0), a_trade(-2.0)],
        source_len: 900,
    };
    let extras = RunExtras { trades: Some(&written), ..Default::default() };

    write_run_with(&run.path, &a_manifest(&run.run_id), &json!({}), &extras).unwrap();
    let back = read_trades(&run.path).unwrap();

    assert_eq!(back.trades.len(), 2);
    assert_eq!(back.source_len, 900, "the ledger is a prefix and must say so");
    assert_eq!(back.trades[1].pnl, -2.0);
}

/// A run that kept no series is the ordinary shape of every run written before this stage, and
/// of any producer with no curve. It must read as MISSING — the same distinct answer the
/// manifest's absence already carries — never as a corrupt document.
#[test]
fn a_run_with_no_series_reads_as_missing_rather_than_broken() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    write_run(&run.path, &a_manifest(&run.run_id), &json!({ "sharpe": 1.0 })).unwrap();

    let err = read_series(&run.path).unwrap_err();

    assert!(matches!(err, RunReadError::Missing { .. }), "expected Missing, got {err:?}");
    assert!(err.to_string().contains(SERIES_FILE), "the message must name the file: {err}");
}

/// The ORDERING contract, extended to the new documents and proved rather than asserted in
/// prose: the manifest is the COMPLETION MARKER, so a failure part-way through must leave the
/// documents that DID land and no manifest. A directory where `report.json` belongs is an
/// unwritable path on every platform this ships to, reached without changing permissions.
#[test]
fn a_failure_writing_the_report_leaves_the_extras_and_no_manifest() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    std::fs::create_dir(run.path.join(REPORT_FILE)).unwrap();
    let series = a_series();
    let extras = RunExtras { series: Some(&series), ..Default::default() };

    let err = write_run_with(&run.path, &a_manifest(&run.run_id), &json!({}), &extras).unwrap_err();

    assert!(matches!(err, RunPersistError::Write { .. }), "expected Write, got {err:?}");
    assert!(run.path.join(SERIES_FILE).is_file(), "the series landed before the report");
    assert!(
        !run.path.join(MANIFEST_FILE).is_file(),
        "the manifest is the completion marker and must NOT exist after a failed write"
    );
}

/// Every `*_FILE` const named between `fn <name>(` and the first line that is a lone `}`.
///
/// Deliberately a scan over this module's OWN SOURCE rather than a list: the point of the test
/// below is that the roster cannot fall behind the writer, and a hand-written list of what the
/// writer writes is the thing that falls behind.
fn file_consts_in(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut tok = String::new();
    for c in text.chars() {
        if c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' {
            tok.push(c);
            continue;
        }
        if tok.ends_with("_FILE") && !out.contains(&tok) {
            out.push(tok.clone());
        }
        tok.clear();
    }
    if tok.ends_with("_FILE") && !out.contains(&tok) {
        out.push(tok);
    }
    out.sort();
    out
}

/// The source between `fn <name>(` and the first line that is a lone `}` at column zero — the
/// shape `crates/vike-backtest/tests/run_record_completeness.rs` already uses.
fn fn_body_of(text: &str, name: &str) -> String {
    // ⚠ `fn <name>` then `(` OR `<`. A needle carrying the paren (`fn write_run_with(`) matches
    // NOTHING on a GENERIC function — `write_run_with<R>` is one — and the harvest then returns an
    // empty body, which is a gate that has silently gone blind rather than one that fails.
    // Measured: the first spelling of this test reported `left: []`.
    let needle = format!("fn {name}");
    let mut out = String::new();
    let mut inside = false;
    for line in text.lines() {
        if !inside {
            // ⚠ The line must BE a definition, not merely mention one. The floor below
            // catches a ZERO harvest; it cannot catch a MIS-ANCHORED one, and this file
            // already contains the literal `fn write_run_with(` inside a comment. Anchored
            // there, the scan would sweep a region that happens to contain every name it
            // looks for, and the assertion would pass while measuring nothing.
            let def = line.trim_start();
            let is_definition = def.starts_with("fn ") || def.starts_with("pub fn ");
            // ONE condition, not three nested `if`s: clippy's `collapsible_if` refuses the
            // nested spelling at `-D warnings`, which is the merge gate. Edition 2024
            // let-chains are what make the whole test one expression.
            if is_definition
                && let Some(i) = line.find(&needle)
                && matches!(line[i + needle.len()..].chars().next(), Some('(') | Some('<'))
            {
                inside = true;
            }
            continue;
        }
        if line == "}" {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Every function in this module that writes a file INTO a run directory, by name.
///
/// ⚠ **It is a LIST because it stopped being one function**, and the change is the whole reason
/// this array exists. [`write_run_with`] writes the five documents a run is made of; [`add_tags`]
/// writes [`META_FILE`] long AFTER the run finished, which is exactly what lets a tag be
/// optional. Both land in the same namespace, so both must be reserved against, and the roster
/// is therefore "every name this MODULE writes" rather than "every name the run WRITER writes".
/// A third writer added here and not to [`RESERVED_FILES`] reddens
/// `every_document_this_module_writes_is_in_the_reserved_roster`; a third writer added to
/// NEITHER is the hole this array cannot see, and is why each entry is a deliberate act.
const RUN_DIRECTORY_WRITERS: &[&str] = &["write_run_with", "add_tags"];

/// One roster of reserved names, so a producer writing its own artifacts cannot collide with a
/// document this module writes. `crates/vike-studio-core/src/study_run.rs`'s `persist` checked
/// exactly two names and there are six.
///
/// ⚠ **DERIVED from the writers' own bodies, on both sides.** This test first compared a
/// hand-written array against [`RESERVED_FILES`] — which IS that array, so it compared a copy
/// with itself and a SIXTH document added to the writer and not to the roster passed it. That
/// is precisely the failure the roster exists to prevent, so the test reads the writers instead.
#[test]
fn every_document_this_module_writes_is_in_the_reserved_roster() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("runs.rs"),
    )
    .expect(
        "this module's own source — read at run time, never `include_str!`, so the \
                 published mirror can withhold a file without breaking the build",
    );

    let mut written: Vec<String> = Vec::new();
    for writer in RUN_DIRECTORY_WRITERS {
        let body = fn_body_of(&src, writer);
        let found = file_consts_in(&body);
        // ⚠ PER-WRITER, not only over the union: a name that was renamed or reshaped yields an
        // empty body, and the union's floor below would still pass on the other writer's five.
        assert!(
            !found.is_empty(),
            "the harvest found nothing in `{writer}` — it was renamed or reshaped, and this \
                 gate is now measuring one writer fewer than it claims"
        );
        for name in found {
            if !written.contains(&name) {
                written.push(name);
            }
        }
    }
    written.sort();
    let reserved = file_consts_in(&decl_of(&src, "pub const RESERVED_FILES"));

    // The floor: a harvester that has stopped matching passes every assertion by seeing
    // nothing, which is how a derived gate quietly becomes a no-op.
    assert!(
        written.len() >= 6,
        "the writer harvest found {written:?} across {RUN_DIRECTORY_WRITERS:?}"
    );
    assert!(reserved.len() >= 6, "the roster harvest found {reserved:?}");

    for name in &written {
        assert!(
            reserved.contains(name),
            "`{name}` is written by one of {RUN_DIRECTORY_WRITERS:?} and is NOT in \
                 RESERVED_FILES — a producer writing its own artifact under that name would \
                 silently overwrite a document this module writes. Add it to the roster.\n  \
                 writers: {written:?}\n  roster:  {reserved:?}"
        );
    }
    assert_eq!(
        written, reserved,
        "the roster and the writers must name the SAME set — a reserved name nothing writes \
             refuses a producer's artifact for no reason"
    );
}

/// The source between `head` and the first `;` — [`RESERVED_FILES`]'s declaration.
fn decl_of(text: &str, head: &str) -> String {
    let i = text.find(head).expect("declaration not found — was it renamed?");
    let rest = &text[i..];
    let end = rest.find(';').map(|e| i + e).unwrap_or(text.len());
    text[i..end].to_string()
}

/// The harvesters' own proof, over planted text: a scan that has gone blind passes the test
/// above by finding nothing on BOTH sides, and the length floors are a blunt instrument beside
/// this.
#[test]
fn the_writer_harvest_reads_a_body_and_takes_only_file_consts() {
    let planted = "\
pub fn write_run_with<R>(dir: &Path) -> u8 {
    write_text(&dir.join(CONFIG_FILE), toml)?;
    write_json(&dir.join(SERIES_FILE), SERIES_FILE, series)?;
    write_json(&dir.join(MANIFEST_FILE), MANIFEST_FILE, manifest)
}

pub fn something_else() {
    let _ = NOT_MINE_FILE;
}
";

    assert_eq!(
        file_consts_in(&fn_body_of(planted, "write_run_with")),
        vec!["CONFIG_FILE".to_string(), "MANIFEST_FILE".to_string(), "SERIES_FILE".to_string()],
        "every `*_FILE` const the body names, deduplicated and sorted, and nothing from the \
             function after it"
    );
    assert_eq!(
        file_consts_in(&decl_of(
            "pub const RESERVED_FILES: &[&str] = &[MANIFEST_FILE, CONFIG_FILE];\nnext",
            "pub const RESERVED_FILES"
        )),
        vec!["CONFIG_FILE".to_string(), "MANIFEST_FILE".to_string()],
        "and the roster side reads the same way"
    );
    assert!(
        !file_consts_in("let x = MAX_TRADES;").contains(&"MAX_TRADES".to_string()),
        "a SCREAMING const that is not a `*_FILE` must not join either side"
    );
}

/// ⚠ **THE BACK-COMPATIBILITY PROOF, and the reason both new fields carry
/// `#[serde(default)]`.** Every field but `detail` is required at deserialize time, so a
/// document written before these fields existed becomes `RunReadError::Parse` the moment one is
/// required — which `crates/vike-studio-core/src/listing.rs`'s `list_runs` renders as
/// `RunUnreadable` and DROPS. Written as TEXT rather than through the writer, deliberately: the
/// bytes on somebody's disk are what this test is about, and a round trip through the current
/// struct could never see the loss.
#[test]
fn a_manifest_written_before_the_schema_field_existed_still_reads() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    std::fs::write(
        run.path.join(MANIFEST_FILE),
        br#"{
  "run_id": "1756000000-4242-0",
  "kind": "backtest",
  "produced_by": "backtest",
  "started_at": "2026-08-24T09:15:04Z",
  "finished_at": "2026-08-24T09:15:16Z",
  "git_sha": null,
  "config": { "path": "profiles/sma.toml", "name": "sma cross" },
  "detail": { "strategy": "sma_cross" }
}
"#,
    )
    .unwrap();

    let back = read_manifest(&run.path).unwrap();

    assert_eq!(back.schema, 0, "absence means PRE-SCHEMA, which is a real and reportable answer");
    assert_eq!(back.fingerprint, None, "a manifest predating the address names none");
    assert_eq!(back.kind, "backtest", "and every field it DID carry is untouched");
}

/// A manifest this build writes states its own version, so a decoder never has to guess.
#[test]
fn a_freshly_written_manifest_states_the_schema_this_build_writes() {
    let v = serde_json::to_value(a_manifest("r-1")).unwrap();

    assert_eq!(v["schema"], json!(MANIFEST_SCHEMA));
}

/// `fingerprint` follows `git_sha`'s rule exactly: written even when the producer cannot name
/// one, because "does not know" and "wrote no such field" are different answers to a reader.
#[test]
fn a_producer_that_cannot_address_its_inputs_still_writes_the_fingerprint_key() {
    let v = serde_json::to_value(a_manifest("r-1")).unwrap();

    assert_eq!(v.as_object().unwrap().get("fingerprint"), Some(&serde_json::Value::Null));
}

/// The kind a backtest writes is EXPORTED now, so the two places that spell it cannot drift —
/// `crates/vike-studio/src/research.rs` carried its own copy and said in a comment that this
/// is what should replace it.
#[test]
fn the_backtest_kind_is_exported_and_is_what_this_producer_writes() {
    assert_eq!(BACKTEST_RUN_KIND, "backtest");
    assert_eq!(a_manifest("r-1").kind, BACKTEST_RUN_KIND);
}

const FP: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

/// The content-addressing property, stated as what it is FOR: two runs over the same inputs
/// carry the same address in their names, so a human scanning `user_data/runs/` sees at a
/// glance which runs are comparable. The full digest lives in the manifest; this is the
/// legible prefix of it.
#[test]
fn two_runs_with_the_same_inputs_carry_the_same_address_in_their_ids() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");

    let first = create_run_dir(&runs, 1_756_000_000, Some(FP)).unwrap();
    let second = create_run_dir(&runs, 1_756_000_000, Some(FP)).unwrap();

    assert_ne!(first.run_id, second.run_id, "two runs are still two directories");
    assert!(first.run_id.starts_with("1756000000-9f86d081884c7d65-"), "{}", first.run_id);
    assert!(second.run_id.starts_with("1756000000-9f86d081884c7d65-"), "{}", second.run_id);
    assert!(second.run_id.ends_with("-1"), "the second takes the next seq: {}", second.run_id);
}

/// ⚠ **THE ORDERING CONTRACT.** `crates/vike-studio-core/src/listing.rs`'s `list_runs` sorts by
/// DIRECTORY NAME and its module doc says that is chronological because the id opens with unix
/// seconds; `crates/vike-studio/src/research.rs` reverses that list to get "Newest first". A
/// bare content hash would make both arbitrary and silently wrong, which is why the address is
/// a SUFFIX.
#[test]
fn a_content_address_does_not_disturb_the_chronological_name_sort() {
    let mut ids = vec![
        run_id_at(1_756_000_200, Some("ffffffffffffffff"), 0),
        run_id_at(1_756_000_000, Some("0000000000000000"), 0),
        run_id_at(1_756_000_100, Some("aaaaaaaaaaaaaaaa"), 0),
    ];
    ids.sort();

    assert_eq!(
        ids,
        vec![
            "1756000000-0000000000000000-0".to_string(),
            "1756000100-aaaaaaaaaaaaaaaa-0".to_string(),
            "1756000200-ffffffffffffffff-0".to_string(),
        ],
        "a plain name sort must still be chronological whatever the addresses are"
    );
}

/// A producer that cannot address its inputs keeps the shape that was there before, pid and
/// all — so the study producer and every pre-address run sort and read exactly as they did.
#[test]
fn a_run_with_no_address_keeps_the_pid_form() {
    let pid = std::process::id();

    assert_eq!(run_id_at(1_756_000_000, None, 3), format!("1756000000-{pid}-3"));
}

/// The id is a DIRECTORY NAME, so an address is sanitized before it becomes one: a value
/// carrying separators would otherwise be a path traversal built out of something a producer
/// computed.
#[test]
fn an_address_that_is_not_bare_hex_cannot_escape_the_runs_root() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");

    let minted = create_run_dir(&runs, 1_756_000_000, Some("../../etc/passwd")).unwrap();

    assert_eq!(minted.path.parent(), Some(runs.as_path()), "minted outside the runs root");
    assert!(!minted.run_id.contains('/') && !minted.run_id.contains('\\'));
    assert!(!minted.run_id.contains(".."), "run id {}", minted.run_id);
}

/// An address with nothing usable in it is the same answer as no address at all, rather than
/// an empty segment that would make two ids collide on a name a human cannot read.
#[test]
fn an_address_with_no_usable_characters_falls_back_to_the_pid_form() {
    let pid = std::process::id();

    assert_eq!(run_id_at(1_756_000_000, Some("///"), 0), format!("1756000000-{pid}-0"));
}

/// ⚠ The formatter is no longer `chrono`'s — it is [`crate::time::civil_from_days`], because
/// this module moved into the crate at the BOTTOM of the graph and `chrono` may not follow it
/// there (`crates/vike-cli/Cargo.toml`'s whole identity is being light). These instants are the
/// pins: each was MEASURED against the chrono implementation this replaced, in the commit before
/// the move, so a divergence is a REGRESSION in a document already on people's disks rather than
/// a formatting preference.
#[test]
fn the_chrono_free_formatter_is_byte_identical_to_the_one_it_replaced() {
    for (secs, expect) in [
        (0_i64, "1970-01-01T00:00:00Z"),
        (1_756_000_000, "2025-08-24T01:46:40Z"),
        (1_756_000_012, "2025-08-24T01:46:52Z"),
        (951_782_400, "2000-02-29T00:00:00Z"),
        (-1, "1969-12-31T23:59:59Z"),
        (-86_400, "1969-12-31T00:00:00Z"),
        (253_402_300_799, "9999-12-31T23:59:59Z"),
    ] {
        assert_eq!(utc_rfc3339(secs), expect, "{secs}");
    }
}

/// The fallback the old doc promised: a second no calendar date can hold must not panic. With
/// integer-exact civil math there is no such second inside `i64::MIN/86_400`, so the guard is
/// the four-digit-year boundary instead — and it must still return a string rather than abort.
#[test]
fn an_unrepresentable_second_falls_back_to_the_raw_number_instead_of_panicking() {
    assert_eq!(utc_rfc3339(i64::MIN), i64::MIN.to_string());
    assert_eq!(utc_rfc3339(i64::MAX), i64::MAX.to_string());
}

// ─── the tag sidecar and the mark store ─────────────────────────────────────────────────────

/// A FINISHED run directory: a manifest, which is the completion marker this module's doc
/// describes. Written as TEXT rather than through `write_run`, so these cases pin the on-disk
/// document a later reader has to survive rather than a round trip of our own struct.
fn plant(runs: &Path, run_id: &str, kind: &str) -> PathBuf {
    let dir = runs.join(run_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(MANIFEST_FILE),
        format!(
            r#"{{"run_id":"{run_id}","kind":"{kind}","produced_by":"backtest",
                     "started_at":"2025-08-24T01:46:40Z","finished_at":"2025-08-24T01:46:41Z",
                     "git_sha":null,"config":{{"path":"p.toml","name":null}},"detail":null}}"#
        ),
    )
    .unwrap();
    dir
}

/// Tags are a SET with a stable order: adding one twice does not duplicate it, and the order is
/// FIRST-INSERT so a rendered row does not shuffle between calls.
#[test]
fn tags_dedupe_and_keep_first_insert_order() {
    let root = tempfile::tempdir().unwrap();
    let dir = plant(&root.path().join("runs"), "1756000000-1-0", "backtest");

    let m = add_tags(&dir, &["ci".into(), "fee-fix".into()], None, 1_756_000_000).unwrap();
    assert_eq!(m.tags, vec!["ci".to_string(), "fee-fix".to_string()]);
    let m = add_tags(&dir, &["fee-fix".into(), "green".into()], None, 1_756_000_001).unwrap();
    assert_eq!(m.tags, vec!["ci".to_string(), "fee-fix".to_string(), "green".to_string()]);
    assert_eq!(m.schema, META_SCHEMA, "the schema key ships WITH the document, never after it");
    // …and it is on DISK with the schema, not merely in the returned value.
    let on_disk = read_meta(&dir).unwrap();
    assert_eq!(on_disk.tags, m.tags);
    assert_eq!(on_disk.schema, META_SCHEMA);
}

/// Notes APPEND and never overwrite. A note is evidence — §7.2's "re-marking is explicit and
/// recorded" is the same instinct — so the second one must not delete the first.
#[test]
fn notes_append_with_their_own_timestamps() {
    let root = tempfile::tempdir().unwrap();
    let dir = plant(&root.path().join("runs"), "1756000000-1-0", "backtest");

    add_tags(&dir, &[], Some("first look"), 1_756_000_000).unwrap();
    let m = add_tags(&dir, &[], Some("after the fee fix"), 1_756_000_060).unwrap();
    assert_eq!(m.notes.len(), 2);
    assert_eq!(m.notes[0].text, "first look");
    assert_eq!(m.notes[1].at, utc_rfc3339(1_756_000_060));
}

/// An ABSENT sidecar is an EMPTY one, never an error: a run minted before tagging existed, or
/// one nobody has tagged, is the ordinary case and not a broken run.
#[test]
fn a_run_with_no_sidecar_reads_as_empty() {
    let root = tempfile::tempdir().unwrap();
    let dir = plant(&root.path().join("runs"), "1756000000-1-0", "backtest");
    let m = read_meta(&dir).unwrap();
    assert!(m.tags.is_empty() && m.notes.is_empty());
    assert!(!dir.join(META_FILE).exists(), "reading must not create one");
}

/// ⚠ A sidecar that EXISTS and will not parse is a REFUSAL, not a re-mint. The file is the only
/// copy of whatever somebody wrote in it, and overwriting it from an empty document would
/// delete their notes silently — the one outcome a metadata write may not have.
#[test]
fn an_unparseable_sidecar_is_refused_rather_than_overwritten() {
    let root = tempfile::tempdir().unwrap();
    let dir = plant(&root.path().join("runs"), "1756000000-1-0", "backtest");
    std::fs::write(dir.join(META_FILE), "not json at all").unwrap();

    let err = add_tags(&dir, &["ci".into()], None, 1_756_000_000).unwrap_err();
    assert!(err.to_string().contains(META_FILE), "the message names the file: {err}");
    assert_eq!(
        std::fs::read_to_string(dir.join(META_FILE)).unwrap(),
        "not json at all",
        "and the bytes on disk are untouched"
    );
}

/// A mark is a POINTER, and moving it RECORDS where it pointed — §7.2: "Re-marking is explicit
/// and recorded", which is only true if the move is kept where somebody can see it.
#[test]
fn re_marking_records_what_moved() {
    let root = tempfile::tempdir().unwrap();
    let marks = root.path().join("marks");

    let first =
        write_mark(&marks, "baseline/momentum", "1756000000-1-0", Some("v1"), 1_756_000_000)
            .unwrap();
    assert!(first.history.is_empty(), "the first mark has nothing to record");
    assert_eq!(first.schema, META_SCHEMA);

    let second =
        write_mark(&marks, "baseline/momentum", "1799999999-2-0", None, 1_799_999_999).unwrap();
    assert_eq!(second.run_id, "1799999999-2-0");
    assert_eq!(second.history.len(), 1, "the previous pointer is kept");
    assert_eq!(second.history[0].run_id, "1756000000-1-0");
    assert_eq!(second.history[0].note.as_deref(), Some("v1"));

    assert_eq!(read_mark(&marks, "baseline/momentum").unwrap().run_id, "1799999999-2-0");
    assert!(
        marks.join("baseline").join("momentum.json").is_file(),
        "a mark is a file NAMED for the mark"
    );
}

/// A mark NAME becomes a PATH, so traversal, invisibility and reserved device names are refused
/// at the door rather than left to the filesystem.
///
/// ⚠ The Windows device names matter here even though NO test in this workspace runs on Windows:
/// a mark called `con` would be written and resolved here and be unopenable there, and nothing
/// downstream would ever discover it.
#[test]
fn a_mark_name_that_would_escape_or_break_a_path_is_refused() {
    for ok in ["baseline/momentum", "prod", "v1.2_rc-3", "nightly/eu/open"] {
        assert!(valid_mark_name(ok).is_ok(), "`{ok}` must be accepted");
    }
    for bad in [
        "",
        "/leading",
        "trailing/",
        "a//b",
        "../escape",
        "a/../b",
        ".hidden",
        "with space",
        "with\\backslash",
        "with:colon",
        "a/b/c/d/e",
        "con",
        "COM1",
        "nul",
        "nul.baseline",
    ] {
        assert!(valid_mark_name(bad).is_err(), "`{bad}` must be refused");
    }
}

/// ⚠ A refused name never reaches the filesystem, in EITHER direction. `valid_mark_name` being
/// right is only half of it — the check has to be the first thing both doors do, or a traversal
/// typed on a command line becomes a write outside the marks root.
#[test]
fn a_refused_name_writes_nothing_and_reads_nothing() {
    let root = tempfile::tempdir().unwrap();
    let marks = root.path().join("marks");
    let outside = root.path().join("escaped.json");

    let err = write_mark(&marks, "../escaped", "1756000000-1-0", None, 1_756_000_000)
        .expect_err("a traversal is refused");
    assert!(matches!(&err, MarkError::BadName { .. }), "got {err:?}");
    assert!(!outside.exists(), "nothing was written outside the marks root");
    assert!(!marks.exists(), "…and the marks root was not even created");

    assert!(matches!(
        read_mark(&marks, "../escaped").expect_err("and the read door refuses too"),
        MarkError::BadName { .. }
    ));
}

/// A mark pointing at a run that is gone is DANGLING, which is a different answer from "no such
/// mark": the first says a prune or an `rm` took the run, the second says the name was never
/// set. They have different fixes, so they are different errors — and the SELECTOR layer that
/// distinguishes them lives in `crates/vike-cli/src/cmd/runs/selector.rs`, which is why this
/// case asserts only the two halves this module owns.
#[test]
fn a_missing_mark_and_a_bad_name_are_different_answers() {
    let root = tempfile::tempdir().unwrap();
    let marks = root.path().join("marks");
    write_mark(&marks, "baseline", "1799999999-2-0", None, 1_799_999_999).unwrap();

    assert!(matches!(read_mark(&marks, "nope").expect_err("never set"), MarkError::Missing { .. }));
    let err = read_mark(&marks, "nope").unwrap_err();
    assert!(err.to_string().contains("tag"), "it names the verb that sets one: {err}");
    assert_eq!(read_mark(&marks, "baseline").unwrap().run_id, "1799999999-2-0");
}

/// The history is BOUNDED. A mark moved on every CI run would otherwise grow one file forever,
/// which is the failure the log retention in this workspace already exists for.
#[test]
fn the_mark_history_is_capped() {
    let root = tempfile::tempdir().unwrap();
    let marks = root.path().join("marks");
    for i in 0..(MARK_HISTORY_MAX + 5) {
        write_mark(
            &marks,
            "baseline",
            &format!("1756000000-1-{i}"),
            None,
            1_756_000_000 + i as i64,
        )
        .unwrap();
    }
    let m = read_mark(&marks, "baseline").unwrap();
    assert_eq!(m.history.len(), MARK_HISTORY_MAX);
    assert_eq!(
        m.history[0].run_id,
        format!("1756000000-1-{}", MARK_HISTORY_MAX + 3),
        "most recent first — the oldest moves fall off the end"
    );
    assert_eq!(
        m.run_id,
        format!("1756000000-1-{}", MARK_HISTORY_MAX + 4),
        "…and the pointer itself is the last write, not a history row"
    );
}

/// The write is ATOMIC and leaves no droppings: an interrupted CI job must leave the OLD mark
/// rather than a truncated file, and a successful one must not leave the temp file behind for a
/// listing to trip over.
#[test]
fn a_written_mark_leaves_no_temporary_file_behind() {
    let root = tempfile::tempdir().unwrap();
    let marks = root.path().join("marks");
    write_mark(&marks, "baseline/m", "1756000000-1-0", None, 1_756_000_000).unwrap();
    write_mark(&marks, "baseline/m", "1756000001-1-0", None, 1_756_000_001).unwrap();

    let kept: Vec<String> = std::fs::read_dir(marks.join("baseline"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(kept, vec!["m.json".to_string()], "one file, and no `.tmp` beside it");
}
