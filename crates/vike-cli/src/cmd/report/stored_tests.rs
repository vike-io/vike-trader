use super::*;
use crate::cmd::report::schema::TOP_LEVEL_KEYS;
use crate::cmd::runs::selector::test_run_at as run_at;

/// A fixture run whose directory is REAL, so every lazy read has something to open.
fn seeded(dir: &std::path::Path) -> ScannedRun {
    std::fs::create_dir_all(dir).unwrap();
    let mut r = run_at("a-1-0", "backtest");
    r.dir = dir.to_path_buf();
    r
}

/// `n` daily samples from 2025-08-25T00:00:00Z, rising 100 a day, WHOLE (stride 1).
///
/// ⚠ `..Default::default()` rather than every field spelled out: `RunSeries` is a shared
/// document type and a field added to it must not redden this file for a value no test here
/// cares about.
fn whole_series(n: usize) -> RunSeries {
    let day = 86_400_000_i64;
    RunSeries {
        schema: vike_model::runs::SERIES_SCHEMA,
        equity: (0..n).map(|i| 10_000.0 + i as f64 * 100.0).collect(),
        equity_ts: (0..n).map(|i| 1_756_080_000_000 + i as i64 * day).collect(),
        stride: 1,
        source_len: n,
        ..Default::default()
    }
}

// ---- the hard rule ----

/// ⚠ **THE constraint this module exists around.** A thinned curve REFUSES `--breakdown`, and
/// the refusal has to carry the numbers: an operator who cannot see the stride has no way to
/// know whether the answer would have been off by a rounding or by a whole trough.
///
/// It is the FAILED rung and deliberately not the USAGE one — the command line is correct, the
/// artifact cannot answer, and a wrapper reading a `2` would go looking for a flag to fix.
#[test]
fn a_thinned_curve_refuses_a_breakdown_rather_than_printing_one() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seeded(&tmp.path().join("a-1-0"));
    let mut s = whole_series(8);
    s.stride = 21;
    s.source_len = 168;
    let state = SeriesState::Read(s);

    let e = breakdown_of(&run, &state, Period::Month).expect_err("a thinned curve may not");
    assert_eq!(e.exit, crate::exit::Exit::Failed, "the artifact cannot answer: {}", e.msg);
    assert!(e.msg.contains("stride 21"), "the stride must be named: {}", e.msg);
    assert!(e.msg.contains("168"), "…and what the run actually produced: {}", e.msg);
    assert!(
        e.msg.contains(&MAX_EQUITY_SAMPLES.to_string()),
        "…and the cap that caused it: {}",
        e.msg
    );
    assert!(e.msg.contains("--export equity"), "…and what DOES work: {}", e.msg);

    // …and the negative control: the same curve WHOLE answers.
    let whole = SeriesState::Read(whole_series(8));
    let doc = breakdown_of(&run, &whole, Period::Month).expect("a whole curve is exact");
    assert_eq!(doc["period"], json!("month"));
    assert!(!doc["rows"].as_array().unwrap().is_empty());
}

/// The `derived` half of the same rule: a thinned curve nulls the two statistics it cannot
/// answer, KEEPS the one decimation cannot damage, and says in one sentence why the nulls are
/// there. `exact` is the flag a machine consumer branches on.
#[test]
fn a_thinned_curve_nulls_what_it_cannot_answer_and_keeps_the_final_sample() {
    let mut s = whole_series(8);
    s.stride = 21;
    s.source_len = 168;
    let d = derived_of(&s);
    assert_eq!(d["exact"], json!(false));
    assert!(d["peak_equity"].is_null(), "the peak may be a dropped sample: {d}");
    assert!(d["max_drawdown"].is_null(), "…and a recomputed one can only be shallower: {d}");
    // ⚠ The one statistic decimation cannot damage — `decimate` keeps the LAST sample
    // unconditionally, so nulling this would be refusing to answer a question the record answers.
    assert_eq!(d["final_equity"], json!(10_700.0), "{d}");
    let why = d["unavailable"].as_str().expect("the nulls carry their reason");
    assert!(why.contains("DECIMATED"), "{why}");

    // WHOLE: every statistic is a number and nothing is unavailable.
    let d = derived_of(&whole_series(8));
    assert_eq!(d["exact"], json!(true));
    assert!(d["peak_equity"].is_number() && d["max_drawdown"].is_number(), "{d}");
    assert!(d["unavailable"].is_null(), "{d}");
}

/// The recomputed drawdown is the same fold as `vike_analytics::metrics::max_drawdown`, pinned
/// on the cases that distinguish the two plausible spellings: the `peak > 0.0` guard, and a
/// positive fraction rather than a signed one.
#[test]
fn the_drawdown_fold_matches_the_analytics_one() {
    assert_eq!(max_drawdown(&[]), 0.0);
    assert_eq!(max_drawdown(&[100.0]), 0.0, "one sample is no drawdown");
    assert_eq!(max_drawdown(&[100.0, 50.0]), 0.5, "a POSITIVE fraction");
    assert_eq!(max_drawdown(&[100.0, 50.0, 200.0, 100.0]), 0.5, "the WORST, not the last");
    // A non-positive peak is skipped rather than producing a meaningless ratio.
    assert_eq!(max_drawdown(&[-10.0, -20.0]), 0.0);
}

// ---- the other four refusals ----

/// Four artifacts that cannot be broken down, each refused on its own rung with its own
/// sentence. Folding any pair together is what makes an operator fix the wrong thing.
#[test]
fn every_unbreakable_artifact_is_refused_on_its_own_terms() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seeded(&tmp.path().join("a-1-0"));

    let e = breakdown_of(&run, &SeriesState::Absent, Period::Day).expect_err("no curve");
    assert_eq!(e.exit, crate::exit::Exit::Empty, "nothing was evaluated: {}", e.msg);
    assert!(e.msg.contains(SERIES_FILE), "{}", e.msg);

    let unusable = SeriesState::Unusable("cannot parse: expected value".to_string());
    let e = breakdown_of(&run, &unusable, Period::Day).expect_err("a broken curve");
    assert_eq!(e.exit, crate::exit::Exit::Failed);
    assert!(e.msg.contains("IS on disk"), "it must say the file is THERE: {}", e.msg);

    let e = breakdown_of(&run, &SeriesState::Read(whole_series(0)), Period::Day)
        .expect_err("no samples at all");
    assert_eq!(e.exit, crate::exit::Exit::Empty);

    let mut untimed = whole_series(4);
    untimed.equity_ts.clear();
    let e = breakdown_of(&run, &SeriesState::Read(untimed), Period::Day)
        .expect_err("a bucket is a function of TIME");
    assert_eq!(e.exit, crate::exit::Exit::Failed);
    assert!(e.msg.contains("no timestamps"), "{}", e.msg);

    let mut skewed = whole_series(4);
    skewed.equity_ts.pop();
    assert!(!skewed.is_aligned(), "the fixture really does break the invariant");
    let e = breakdown_of(&run, &SeriesState::Read(skewed), Period::Day)
        .expect_err("a misaligned document");
    assert_eq!(e.exit, crate::exit::Exit::Failed);
    assert!(e.msg.contains("invariant"), "{}", e.msg);
}

// ---- the breakdown itself ----

/// Rows CHAIN: each period's base is the previous period's close, so the first and last row
/// together describe the whole run. The FIRST row is the exception — it has no previous close,
/// so its base is the curve's own opening sample.
#[test]
fn rows_chain_and_the_first_row_opens_on_the_curve_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seeded(&tmp.path().join("a-1-0"));
    // Four daily samples starting 2025-08-25, so `day` gives four rows and `month` gives one.
    let doc = breakdown_of(&run, &SeriesState::Read(whole_series(4)), Period::Day).unwrap();
    let rows = doc["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 4, "{doc}");
    assert_eq!(rows[0]["start_equity"], json!(10_000.0), "the curve's own first sample");
    assert_eq!(rows[0]["end_equity"], json!(10_000.0), "a one-sample period opens and closes");
    assert_eq!(rows[1]["start_equity"], rows[0]["end_equity"], "row 2 opens on row 1's close");
    assert_eq!(rows[3]["end_equity"], json!(10_300.0), "…and the last row closes the run");

    let doc = breakdown_of(&run, &SeriesState::Read(whole_series(4)), Period::Month).unwrap();
    let rows = doc["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "four days in one month is ONE row: {doc}");
    assert_eq!(rows[0]["period"], json!("2025-08"));
}

/// The null convention, at the leaf that would otherwise produce an infinity: a zero base has no
/// return, and `null` is the answer rather than a number somebody reads as a result.
#[test]
fn a_degenerate_row_is_null_rather_than_an_infinity() {
    let row = period_row("2026-01", 0.0, 100.0);
    assert!(row["return"].is_null(), "a zero base divides into nothing: {row}");
    assert_eq!(row["start_equity"], json!(0.0), "…and the base itself is still reported");

    let row = period_row("2026-01", f64::NAN, 100.0);
    assert!(row["start_equity"].is_null() && row["return"].is_null(), "{row}");

    // ⚠ A TOLERANCE, not an exact literal: `110.0 / 100.0 - 1.0` is not the double nearest
    // `0.1`, and pinning the bit pattern would be a test about IEEE rounding rather than about
    // this row.
    let row = period_row("2026-01", 100.0, 110.0);
    let ret = row["return"].as_f64().expect("an ordinary row divides");
    assert!((ret - 0.1).abs() < 1e-12, "{row}");
}

/// A month label is FORMATTED from the calendar parts rather than sliced out of a date string,
/// so a year outside four digits cannot silently mislabel a row.
#[test]
fn a_period_label_is_built_from_the_calendar_not_from_a_string_slice() {
    // 2025-08-25T00:00:00Z
    assert_eq!(period_label(Period::Day, 1_756_080_000_000), "2025-08-25");
    assert_eq!(period_label(Period::Month, 1_756_080_000_000), "2025-08");
    // The epoch itself, and the millisecond before it — the flooring `civil_from_days` does.
    assert_eq!(period_label(Period::Day, 0), "1970-01-01");
    assert_eq!(period_label(Period::Day, -1), "1969-12-31");
}

/// `--breakdown`'s refusal names the PUBLISHED roster, so the parser and
/// `crate::cmd::report::schema::json_schema`'s `enum` cannot name different sets.
#[test]
fn an_unknown_period_is_refused_against_the_published_roster() {
    let e = Period::parse("week").expect_err("there is no ISO-week labeller");
    for p in PERIODS {
        assert!(e.contains(p), "the refusal must name `{p}`: {e}");
    }
    assert!(e.contains("week"), "…and echo what was typed: {e}");
    assert_eq!(Period::parse(" month ").unwrap(), Period::Month, "trimmed");
    assert_eq!(Period::parse("day").unwrap().as_str(), "day");
}

/// ⚠ **The other direction: every period the roster PUBLISHES is one [`Period::parse`]
/// ACCEPTS.** Nothing checked it, and `parse`'s own doc claimed it could not be broken.
///
/// Every existing check reads the roster into a STRING — the test above iterates `PERIODS`
/// over the refusal text, `crate::cmd::report`'s
/// `a_breakdown_period_is_checked_against_the_published_roster` over its own,
/// `the_usage_advertises_both_sources_and_the_schema` over the usage, and
/// `crate::cmd::report::schema`'s `the_period_roster_is_the_schema_enum_and_omits_week` over the
/// schema `enum`. None of them feeds a member to the parser, and `parse`'s `other =>` catch-all
/// means a missing accept arm compiles and passes all four: a third bucket would be published,
/// advertised and named in the refusal, then rejected here.
///
/// The ROUND TRIP is asserted rather than `is_ok()`, and the set size with it, because those
/// are the two ways the accept arms can be wrong while every arm still exists: a member that
/// parses to a variant spelling itself differently (so the document records a period the
/// operator did not type), and two members collapsing onto one variant (so one of them is
/// advertised and unreachable).
#[test]
fn every_published_period_is_one_this_parser_accepts() {
    let mut canonical = std::collections::BTreeSet::new();
    for p in PERIODS {
        let parsed = Period::parse(p).unwrap_or_else(|e| {
            panic!(
                "`{p}` is published in `crate::cmd::report::schema`'s PERIODS — the schema \
                     enum, this verb's usage and this parser's own refusal sentence all name it — \
                     and `Period::parse` refuses it: {e}. The refusal READS that roster; the \
                     accept arms are a hand copy of it, and this is the direction nothing else \
                     checks."
            )
        });
        assert_eq!(parsed.as_str(), p, "`{p}` must round-trip to its published spelling");
        canonical.insert(parsed.as_str());
    }
    assert_eq!(
        canonical.len(),
        PERIODS.len(),
        "two published periods parsed to one variant, so one of them is advertised and \
             unreachable"
    );
}

// ---- the document ----

/// ⚠ **The published schema is TRUE, in both directions.** Every key
/// `crate::cmd::report::schema`'s `TOP_LEVEL_KEYS` declares is a key this document emits, and
/// every key it emits is declared — which is the half `report::schema`'s own tests cannot check,
/// because a schema can be perfectly self-consistent about a document nobody writes.
#[test]
fn the_document_carries_exactly_the_keys_the_schema_publishes() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seeded(&tmp.path().join("a-1-0"));
    let doc = document(&run, &SeriesState::Read(whole_series(4)), None);
    let obj = doc.as_object().expect("an object");

    for key in TOP_LEVEL_KEYS {
        assert!(obj.contains_key(key), "the schema requires `{key}` and the document has none");
    }
    for key in obj.keys() {
        assert!(
            TOP_LEVEL_KEYS.contains(&key.as_str()),
            "`{key}` is emitted and the schema declares `additionalProperties: false`"
        );
    }
    assert_eq!(doc["document"], json!(DOCUMENT_ID));
    assert_eq!(doc["schema"], json!(REPORT_SCHEMA));
}

/// The key set does NOT change shape between runs: an absent section is `null` and the key is
/// still there. A consumer whose `jq` filter works on one run and fails on the next is the
/// defect a fixed key set exists to remove — `vike_model::runs::RunManifest::git_sha` makes the
/// same call for the same reason.
#[test]
fn an_absent_section_is_a_null_key_rather_than_a_missing_one() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seeded(&tmp.path().join("a-1-0"));
    let doc = document(&run, &SeriesState::Absent, None);
    let obj = doc.as_object().expect("an object");
    for key in TOP_LEVEL_KEYS {
        assert!(obj.contains_key(key), "`{key}` must be present even when it is null");
    }
    assert!(doc["curve"].is_null(), "no curve: {doc}");
    assert!(doc["derived"].is_null(), "…so nothing is derived: {doc}");
    assert!(doc["metrics"].is_null(), "this fixture wrote no report.json: {doc}");
    assert!(doc["breakdown"].is_null(), "nobody asked for one: {doc}");
}

/// ⚠ A curve that is ON DISK and unusable names ITSELF rather than reading as absent — the same
/// line `crate::cmd::runs::show`'s `ReportState::to_json` draws for `report.json`, because
/// `null` already means "the producer kept none" and a `jq` consumer must tell them apart.
#[test]
fn an_unusable_curve_is_an_error_object_not_a_null() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seeded(&tmp.path().join("a-1-0"));
    let state = SeriesState::Unusable("cannot parse series.json: expected value".to_string());
    let doc = document(&run, &state, None);
    assert_eq!(doc["curve"]["error"], json!("unusable"));
    assert!(doc["curve"]["detail"].as_str().unwrap().contains("expected value"));
    assert!(doc["derived"].is_null(), "there is nothing to derive from a broken curve: {doc}");
}

/// The document is read off the COMMON manifest and nothing here recomputes identity.
#[test]
fn the_run_block_is_the_manifest_verbatim() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seeded(&tmp.path().join("a-1-0"));
    let doc = document(&run, &SeriesState::Absent, None);
    assert_eq!(doc["run"]["run_id"], json!("a-1-0"));
    assert_eq!(doc["run"]["kind"], json!("backtest"));
    assert!(doc["run"]["dir"].is_string(), "the document says where it came from");
}

// ---- the human rendering ----

/// The text form carries the sibling's header and both of this verb's added sections, and it
/// prints a `null` as the WORD: the whole point of the convention is that "undefined" is
/// visibly not `0`, and a blank column reads as a rendering bug.
#[test]
fn the_text_rendering_carries_the_provenance_and_spells_a_null() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seeded(&tmp.path().join("a-1-0"));
    let mut s = whole_series(4);
    s.stride = 7;
    s.source_len = 28;
    let text = render_text(&run, &SeriesState::Read(s), None);
    assert!(text.contains("run     a-1-0"), "the sibling's header: {text}");
    assert!(text.contains("stride 7"), "{text}");
    assert!(text.contains("THINNED"), "{text}");
    assert!(text.contains("max_drawdown = null"), "a null prints as the word: {text}");
    assert!(text.contains("final_equity = 10300"), "…and the exact one prints: {text}");

    // …and the breakdown table renders when one was computed.
    let doc = breakdown_of(&run, &SeriesState::Read(whole_series(4)), Period::Day).unwrap();
    let text = render_text(&run, &SeriesState::Read(whole_series(4)), Some(&doc));
    assert!(text.contains("breakdown (day)"), "{text}");
    assert!(text.contains("2025-08-25"), "{text}");
}

/// A run with no curve says so in the text form too, rather than printing an empty table.
#[test]
fn a_run_with_no_curve_says_so_rather_than_rendering_an_empty_section() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seeded(&tmp.path().join("a-1-0"));
    let text = render_text(&run, &SeriesState::Absent, None);
    assert!(text.contains("no series.json"), "{text}");
    // ⚠ The HEADING, not the word: the absent sentence itself explains that nothing below is
    // derived, so a bare `contains("derived")` would be answered by the explanation and could
    // never fail.
    assert!(!text.contains("\nderived\n"), "nothing is derived, so no section: {text}");
}

// ---- the whole verb, over a real directory ----

/// End to end over a REAL run directory: the selector resolves, the curve is read off disk, and
/// the document is the one the schema publishes. This is the only test here that exercises
/// `read_series`, which is what proves the document is built from the ARTIFACT rather than from
/// a fixture struct.
#[test]
fn a_real_run_directory_renders_a_document_with_no_recompute() {
    let tmp = tempfile::tempdir().unwrap();
    let runs = tmp.path().join("runs");
    let dir = runs.join("1756080000-abcd-0");
    std::fs::create_dir_all(&dir).unwrap();
    // The manifest is the completion marker, so `scan_runs` needs it to see this as a run.
    std::fs::write(
            dir.join("manifest.json"),
            r#"{"schema":1,"run_id":"1756080000-abcd-0","kind":"backtest","produced_by":"backtest","started_at":"2025-08-25T00:00:00Z","finished_at":"2025-08-25T00:01:00Z","git_sha":null,"config":{"path":null,"name":null},"detail":null}"#,
        )
        .unwrap();
    std::fs::write(dir.join("report.json"), r#"{"sharpe":1.25,"profit_factor":null}"#).unwrap();
    std::fs::write(
            dir.join("series.json"),
            r#"{"schema":1,"equity":[10000.0,9000.0,11000.0],"equity_ts":[1756080000000,1756166400000,1756252800000],"stride":1,"source_len":3}"#,
        )
        .unwrap();

    // ⚠ `as_path()`, not `&runs`: the parameter is `Option<&Path>` and `Option<&PathBuf>` does
    // not coerce inside the `Option`.
    let root = Some(runs.as_path());
    let text = run_stored(root, None, "1756080000-abcd-0", Some(Period::Day), true).unwrap();
    let doc: Value = serde_json::from_str(&text).expect("the emitted document is JSON");
    assert_eq!(doc["schema"], json!(REPORT_SCHEMA));
    assert_eq!(doc["metrics"]["sharpe"], json!(1.25));
    // ⚠ `null` stays `null` — carried through verbatim, because a non-finite `profit_factor`
    // serializes to null and turning it into `0.0` would make a broken run look like a flat one.
    assert!(doc["metrics"]["profit_factor"].is_null(), "{doc}");
    assert_eq!(doc["curve"]["whole"], json!(true));
    assert_eq!(doc["derived"]["max_drawdown"], json!(0.1), "10000 → 9000 is a 10% drawdown");
    assert_eq!(doc["breakdown"]["rows"].as_array().unwrap().len(), 3);

    // …and a selector that resolves to nothing is a failure with nothing on stdout.
    let e = run_stored(root, None, "no-such-run", None, true).expect_err("no match");
    assert_eq!(e.exit, crate::exit::Exit::Failed);
}

/// No project above the working directory is an ORDINARY state, refused with a sentence that
/// names both ways out rather than with a panic or an empty document.
#[test]
fn no_runs_root_is_refused_with_the_two_ways_out() {
    let e = run_stored(None, None, "@last", None, false).expect_err("nowhere to read");
    assert_eq!(e.exit, crate::exit::Exit::Failed);
    assert!(e.msg.contains("vike-cli init"), "{}", e.msg);
    assert!(e.msg.contains("VIKE_USER_DATA_DIR"), "{}", e.msg);
}
