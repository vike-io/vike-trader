use super::*;
use crate::cmd::runs::selector::test_run_at as run_at;

/// A fixture run whose directory is REAL, so the lazy reads have something to open.
fn seeded(dir: &std::path::Path, report: Option<&str>) -> ScannedRun {
    std::fs::create_dir_all(dir).unwrap();
    let mut r = run_at("a-1-0", "backtest");
    r.dir = dir.to_path_buf();
    if let Some(json) = report {
        std::fs::write(dir.join("report.json"), json).unwrap();
        r.report = Some(dir.join("report.json"));
    }
    r
}

#[test]
fn every_unbuilt_renderer_is_refused_by_name_with_what_it_would_need() {
    for flag in UNBUILT_RENDERERS {
        let msg = refuse_an_unbuilt_renderer(flag);
        assert!(msg.contains(flag), "{flag}: {msg}");
        // ⚠ It must say what is MISSING, not merely that the flag is unsupported — an operator
        // reading the design document needs to know what is absent, not that the parser is.
        assert!(
            msg.contains("fill")
                || msg.contains("PER-BAR")
                || msg.contains("renderer")
                || msg.contains("attribution"),
            "{flag} must name what is missing: {msg}"
        );
    }
}

/// ⚠ **Two flags LEFT this roster, and a roster still carrying either would refuse a flag this
/// file implements.** `--trades` renders `trades.json`; `--export` serves its stored VALUES
/// through `export_document` and refuses only the one it cannot (`fills`). Both left because the
/// run record grew what they needed — the roster is a claim about the ARTIFACT, not a permanent
/// list.
#[test]
fn the_refusal_roster_no_longer_names_a_renderer_that_ships() {
    assert_eq!(UNBUILT_RENDERERS.len(), 3);
    for shipped in ["--trades", "--export"] {
        assert!(
            !UNBUILT_RENDERERS.contains(&shipped),
            "{shipped} ships — a roster row would refuse it"
        );
    }
    for flag in UNBUILT_RENDERERS {
        assert!(refuse_an_unbuilt_renderer(flag).contains(flag));
    }
    // An unknown flag falls through to the ordinary message, so this function can be the
    // parser's whole arm without swallowing typos.
    assert!(refuse_an_unbuilt_renderer("--nope").contains("unknown option"));
}

/// ⚠ **The `--drawdowns` refusal claimed its renderer had never been built. This test CALLS
/// that renderer from inside this crate, so the claim is checked against the binary rather
/// than pinned as text.**
///
/// The sentence was true when written and rotted: `crates/vike-analytics/src/periods.rs` grew
/// `drawdown_table_text` beside the pre-existing `drawdown_table`, and this crate gained a
/// non-optional vike-analytics edge — so the renderer is linked here and has no production
/// caller anywhere. Every test over that roster reads the MESSAGE (that it names the flag, that
/// it is not "unknown option"), which is why the false sentence survived; this one reads the
/// TREE. If the edge is dropped or either function renamed, this stops compiling and the
/// refusal is re-examined, which is the direction the old wording had no gate in at all.
#[test]
fn the_drawdown_renderer_this_refusal_names_is_linked_and_callable_here() {
    // 100 → 50 → 100 is one closed episode: a 50% fall from the peak, recovered on the last
    // sample. Both halves of the pair are exercised — the fold and the formatter.
    let equity = [100.0, 50.0, 100.0];
    let ts = [0_i64, 1, 2];
    let episodes = vike_analytics::periods::drawdown_table(&equity, &ts, 3);
    assert_eq!(episodes.len(), 1, "one fall below a prior peak: {episodes:?}");
    let table = vike_analytics::periods::drawdown_table_text(&episodes);
    assert!(table.contains("depth_from_peak"), "an aligned table with headings: {table}");

    // ...so the refusal may not say nobody has built one.
    let msg = refuse_an_unbuilt_renderer("--drawdowns");
    assert!(
        !msg.contains("nothing in this tree has built"),
        "the renderer called above is in this binary: {msg}"
    );
}

/// ⚠ **The `--breakdown` refusal names a SIBLING VERB, and the period list it advertises is a
/// hand copy of a published roster** — so it is held against that roster here.
///
/// The refusal is unconditional by construction (`crate::cmd::backtest`'s `parse_read` sees
/// argv alone and has no `stride` to test), while the sibling refuses only a genuinely thinned
/// curve. Sending an operator there with an incomplete list of what it takes would be a second
/// wrong answer on top of the first, and a third bucket in `crate::cmd::report::schema`'s
/// `PERIODS` is published in the JSON Schema `enum` and in `crate::cmd::report`'s `USAGE`
/// without anything reaching this sentence.
#[test]
fn the_breakdown_refusal_names_the_verb_that_answers_and_every_period_it_takes() {
    let msg = refuse_an_unbuilt_renderer("--breakdown");
    assert!(msg.contains("vike-cli report"), "it must name the verb that answers: {msg}");
    for period in crate::cmd::report::schema::PERIODS {
        assert!(
            msg.contains(period),
            "the sibling accepts `{period}` and this refusal omits it: {msg}"
        );
    }
}

/// ⚠ `--export` is refused per VALUE. `fills` is the one the record cannot serve; a LIST is
/// refused because one `--out` writes one document; and the two stored values are served.
#[test]
fn export_refuses_the_value_it_cannot_serve_rather_than_the_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("a-1-0");
    let run = seeded(&dir, None);
    std::fs::write(dir.join("trades.json"), r#"{"schema":1,"trades":[],"source_len":0}"#).unwrap();

    let e = export_document(&run, "fills").expect_err("not stored at any size");
    assert_eq!(e.exit, crate::exit::Exit::Usage);
    assert!(e.msg.contains("fills"), "{}", e.msg);

    let e = export_document(&run, "trades,equity").expect_err("one --out, one document");
    assert_eq!(e.exit, crate::exit::Exit::Usage);
    assert!(e.msg.contains("one value at a time"), "{}", e.msg);

    let e = export_document(&run, "nonsense").expect_err("not a value");
    assert_eq!(e.exit, crate::exit::Exit::Usage);

    // ...and a value the record DOES hold comes back as a document.
    let doc: serde_json::Value =
        serde_json::from_str(&export_document(&run, "trades").unwrap()).unwrap();
    assert_eq!(doc["source_len"], serde_json::json!(0));

    // A value that is exportable in principle and absent for THIS run is the FAILED rung — a
    // fact about the run, not about the command line.
    let e = export_document(&run, "equity").expect_err("this run wrote no series.json");
    assert_eq!(e.exit, crate::exit::Exit::Failed);

    // Every EXPORTABLE value really is served, so the const and the match cannot drift.
    for value in EXPORTABLE {
        assert!(
            !matches!(export_document(&run, value), Err(ref e) if e.exit == crate::exit::Exit::Usage),
            "{value} is advertised as exportable and was refused as a USAGE error"
        );
    }
}

#[test]
fn the_json_document_carries_the_manifest_and_the_report_verbatim() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seeded(&tmp.path().join("a-1-0"), Some(r#"{"sharpe":1.25,"profit_factor":null}"#));

    let doc: serde_json::Value = serde_json::from_str(&show_json(&run, false)).unwrap();
    assert_eq!(doc["run_id"], serde_json::json!("a-1-0"));
    assert_eq!(doc["manifest"]["kind"], serde_json::json!("backtest"));
    assert_eq!(doc["report"]["sharpe"], serde_json::json!(1.25));
    // ⚠ `null` stays `null`. It is what a non-finite `profit_factor` serializes to, and turning
    // it into `0.0` here would make a broken run look like a flat one.
    assert!(doc["report"]["profit_factor"].is_null());
    assert!(doc["dir"].is_string(), "the document names where it came from");
}

/// A run with NO report still shows: the manifest is the half that always exists, and a listing
/// that refused to render an unfinished run would hide it.
#[test]
fn a_run_with_no_report_still_shows_its_manifest_and_says_the_report_is_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seeded(&tmp.path().join("a-1-0"), None);
    let doc: serde_json::Value = serde_json::from_str(&show_json(&run, false)).unwrap();
    assert!(doc["report"].is_null());
    let text = show_text(&run, true, false, false);
    assert!(text.contains("no report"), "{text}");
}

/// `--config` prints what is RECORDED and says what is not. A run whose producer wrote no
/// `config.toml` has only a path and a name, and printing a path under a heading that says
/// "resolved profile" would hand the operator positive confirmation of something false.
#[test]
fn config_prints_what_is_recorded_and_names_what_is_not() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seeded(&tmp.path().join("a-1-0"), None);
    let text = show_text(&run, false, false, true);
    assert!(text.contains("profiles/sma.toml"), "{text}");
    assert!(text.contains("sma cross"), "{text}");
    assert!(
        text.contains("not stored"),
        "it must say the resolved profile itself is absent: {text}"
    );
    // ...and the kind-specific detail IS recorded, so it is shown.
    assert!(text.contains("sma_cross"), "the detail subtree renders: {text}");
}

/// ...and when the producer DID write one, its text is what `--config` shows — the promise §6.2
/// makes, honoured for every run minted since the record grew a `config.toml`.
#[test]
fn a_stored_resolved_profile_is_printed_rather_than_its_path() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("a-1-0");
    let run = seeded(&dir, None);
    std::fs::write(dir.join("config.toml"), "[engine]\nfee_rate = 0.001\n").unwrap();
    let text = show_text(&run, false, false, true);
    assert!(text.contains("fee_rate = 0.001"), "{text}");
    assert!(!text.contains("not stored"), "{text}");
}

/// ⚠ `--trades` renders the LEDGER and says how many trades the run actually closed, because
/// the kept list is a chronological PREFIX once a run exceeds the cap. A count taken from the
/// rendered rows would understate a long run.
#[test]
fn trades_renders_the_ledger_and_states_the_true_closed_count() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("a-1-0");
    let run = seeded(&dir, None);
    std::fs::write(
            dir.join("trades.json"),
            r#"{"schema":1,"trades":[{"entry_price":100.0,"exit_price":110.0,"size":2.0,"pnl":20.0,"symbol":"BTCUSDT","is_long":true}],"source_len":9}"#,
        )
        .unwrap();

    let text = show_text(&run, false, true, false);
    assert!(text.contains("9 closed, 1 kept"), "{text}");
    assert!(text.contains("BTCUSDT"), "{text}");
    assert!(text.contains("pnl=20"), "{text}");
}

/// A producer that wrote no ledger says so rather than rendering an empty one — an empty table
/// and an absent document are different facts about a run.
#[test]
fn a_run_with_no_ledger_says_so_rather_than_rendering_an_empty_table() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seeded(&tmp.path().join("a-1-0"), None);
    let text = show_text(&run, false, true, false);
    assert!(text.contains("no trades.json"), "{text}");
}

/// With neither section flag, `show` renders everything the artifact has.
#[test]
fn no_section_flag_renders_everything_the_artifact_has() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seeded(&tmp.path().join("a-1-0"), Some(r#"{"sharpe":1.25}"#));
    let all = show_text(&run, false, false, false);
    assert!(all.contains("sharpe"), "{all}");
    assert!(all.contains("profiles/sma.toml"), "{all}");
    assert!(all.contains("trades"), "{all}");
}

/// ⚠ **`--trades --json` must CARRY the ledger.** The document is `{run_id, dir, manifest,
/// report}` and the ledger is a SEPARATE file, so a `--trades` that only changed the text
/// renderer answered `jq '.trades'` with `null` on exit `0` — indistinguishable from a run whose
/// producer wrote none. That hole was created by promoting the flag out of `UNBUILT_RENDERERS`
/// without giving it a JSON half.
#[test]
fn trades_under_json_carries_the_ledger_and_the_true_closed_count() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("a-1-0");
    let run = seeded(&dir, None);
    std::fs::write(
            dir.join("trades.json"),
            r#"{"schema":1,"trades":[{"entry_price":100.0,"exit_price":110.0,"size":2.0,"pnl":20.0,"symbol":"BTCUSDT","is_long":true}],"source_len":9}"#,
        )
        .unwrap();

    let doc: serde_json::Value = serde_json::from_str(&show_json(&run, true)).unwrap();
    assert_eq!(doc["trades"]["closed"], serde_json::json!(9), "the TRUE count, not the kept one");
    assert_eq!(doc["trades"]["kept"], serde_json::json!(1));
    assert_eq!(doc["trades"]["trades"][0]["symbol"], serde_json::json!("BTCUSDT"));

    // ...and WITHOUT the flag the key is absent outright, so nothing opens `trades.json` for an
    // ordinary `show --json` and no consumer reads an absent key as an empty ledger.
    let doc: serde_json::Value = serde_json::from_str(&show_json(&run, false)).unwrap();
    assert!(doc.get("trades").is_none(), "absent flag ⇒ absent key: {doc}");
}

/// A producer that wrote no ledger answers `null` under the flag — distinct from the `error`
/// object a broken one gets, and distinct from the key being absent because nobody asked.
#[test]
fn a_run_with_no_ledger_is_null_under_the_flag_rather_than_an_empty_list() {
    let tmp = tempfile::tempdir().unwrap();
    let run = seeded(&tmp.path().join("a-1-0"), None);
    let doc: serde_json::Value = serde_json::from_str(&show_json(&run, true)).unwrap();
    // ⚠ The key must be PRESENT and null, not merely index to null. `Value["missing"]` also
    // yields `Null`, so a bare `doc["trades"].is_null()` stays green with the whole trades half
    // deleted — measured: it survived the kill proof that reddened every other assertion here.
    assert!(
        doc.as_object().expect("an object").contains_key("trades"),
        "the flag was given, so the key must be THERE: {doc}"
    );
    assert!(doc["trades"].is_null(), "{doc}");
}

/// ⚠ **A report that is ON DISK and unusable is NOT "no report".** Both used to land as
/// `"report": null` and as the sentence an unfinished run gets, which tells an operator their
/// run did not finish when the truth is that their file is corrupt. `crate::cmd::runs::scan`
/// draws exactly this line for the manifest; this is the report's half of it.
#[test]
fn an_unparseable_report_is_told_apart_from_an_absent_one() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("a-1-0");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("report.json"), "not json at all").unwrap();
    let mut run = run_at("a-1-0", "backtest");
    run.dir = dir.clone();
    run.report = Some(dir.join("report.json"));

    let text = show_text(&run, true, false, false);
    assert!(text.contains("IS on disk"), "it must say the file is THERE: {text}");
    assert!(text.contains("not valid JSON"), "{text}");
    assert!(!text.contains("no report —"), "the absent sentence must not be reused: {text}");

    // ...and under `--json` it is an OBJECT naming itself, never `null` — `null` is what an
    // ABSENT report is, and a `jq` consumer has to be able to tell them apart.
    let doc: serde_json::Value = serde_json::from_str(&show_json(&run, false)).unwrap();
    assert_eq!(doc["report"]["error"], serde_json::json!("unparseable"));

    // The negative control: an ABSENT report still reads as absent, in both renderers.
    let absent = seeded(&tmp.path().join("b-1-0"), None);
    assert!(show_text(&absent, true, false, false).contains("no report —"));
    let doc: serde_json::Value = serde_json::from_str(&show_json(&absent, false)).unwrap();
    assert!(doc["report"].is_null());
}

/// ⚠ **The derived order is byte-identical to the eleven-entry array it replaced.**
///
/// The literal below is the DELETED `REPORT_KEY_ORDER`, kept here and nowhere else: as a
/// regression pin it is worth exactly what a hand copy in production code is not, because
/// nothing reads it and a disagreement is a test failure rather than a wrong report. It says
/// that swapping eight typed names for `MetricSelection::Compact` moved no key and reordered
/// none — so `backtest show --metrics` prints the same rows in the same sequence as before.
///
/// A compact metric added to the catalog SHOULD redden this, and the response is to add it to
/// the literal after checking the new key really is a top-level `report.json` field.
#[test]
fn the_derived_key_order_is_the_order_the_hand_copy_declared() {
    assert_eq!(
        report_key_order(),
        [
            "name",
            "final_equity",
            "total_return",
            "n_trades",
            "win_rate",
            "sharpe",
            "max_drawdown",
            "profit_factor",
            "funding_paid",
            "per_symbol_pnl",
            "zero_trade",
        ]
    );
}

/// Every metric key the order names is a key `BacktestReport` actually SERIALIZES at the top
/// level — the claim that makes the derivation legitimate rather than merely shorter.
///
/// `MetricSelection::Compact` answers with the catalog's `MetricHome::Compact` ids, and that
/// enum's own doc fixes `Compact` as "a scalar field of `BacktestReport` itself". This asserts
/// it against the real serialization instead of trusting the doc, so a catalog row promoted to
/// `Compact` without a matching report field is caught here rather than silently printing
/// nothing.
#[test]
fn every_metric_key_in_the_order_is_a_top_level_report_field() {
    // Built through the REAL constructor over an empty result, not a literal: `BacktestReport`
    // derives no `Default`, and a hand-written literal here would be one more copy to rot.
    let report = vike_analytics::report::BacktestReport::from_result(
        Some("r".into()),
        &vike_analytics::BacktestResult::default(),
        252.0,
    );
    let json = serde_json::to_value(&report).unwrap();
    let obj = json.as_object().expect("a report serializes as an object");
    for key in vike_analytics::metric_catalog::MetricSelection::Compact.ids() {
        assert!(
            obj.contains_key(key),
            "the catalog calls `{key}` compact, the report has no \
                such top-level field — see this test's doc"
        );
    }
}

/// ⚠ **THE LISTING DOOR IS REACHABLE WITH NO RUNS ROOT AT ALL, and that is the whole reason
/// the short-circuit sits at the TOP of [`run_show`].**
///
/// [`Ctx::runs_root`] is `None` here — the state of a box with no project above the working
/// directory — which is exactly the box where somebody is deciding what to type. Every other
/// `show` line refuses on that `ok_or_else`; this one must not.
///
/// ⚠ The mutation this fails on, in PRODUCTION: move the `a.metrics_list` block BELOW the
/// `let root = ctx.runs_root.ok_or_else(…)` line. The listing then refuses with a message about
/// a runs directory nobody asked about — and nothing else in this crate would have noticed,
/// because every other test of this verb seeds a project first.
///
/// It asserts the DOCUMENT too, against the catalog itself rather than against a literal: every
/// id in `METRICS` appears, so a door that printed the wrong renderer (a `show_text` with no
/// run resolved, say) cannot satisfy it.
#[test]
fn the_metric_listing_prints_the_catalog_with_no_project_and_no_run() {
    let ctx = Ctx { runs_root: None, marks_root: None, configured_addr: None, keys: None };
    let mut a = ReadArgs::empty(crate::cmd::backtest::read::ReadSub::Show);
    a.metrics_list = true;
    run_show(&ctx, &a).expect("a listing needs no runs root, no run and no report");

    // …and `--out` carries the same bytes to a file, verbatim — `metric_list_text` already ends
    // with a newline, so the door must not add one.
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("catalog.txt");
    a.out = Some(out.to_str().expect("utf-8").to_string());
    run_show(&ctx, &a).expect("--out composes with the listing");
    let written = std::fs::read_to_string(&out).expect("the file was written");
    assert_eq!(
        written,
        vike_analytics::metric_catalog::metric_list_text(),
        "the door writes the catalog verbatim — no added newline, no re-rendering"
    );
    assert!(written.ends_with('\n'), "the renderer's own trailing newline survives");
    for m in vike_analytics::metric_catalog::METRICS {
        assert!(written.contains(m.id), "{} is missing from the printed listing", m.id);
    }
}

/// The combination refusal, driven from the PRODUCTION array rather than a literal condition —
/// so the sentence names the flag that collided, and a fifth row in
/// [`run_rendering_flags_given`] cannot be added without a message for it.
///
/// ⚠ The mutation this fails on, in PRODUCTION: make
/// [`refuse_a_listing_beside_a_run_rendering`] return `Ok(())` unconditionally. Every assertion
/// below reddens, and `show <run> --metrics-list --trades` would print the catalog while the
/// operator waited for a ledger. (The `crate::cmd::backtest` twin drives the same property
/// through the real ARGV parser; this one drives the function, so a deleted CALL and a deleted
/// BODY are caught in different files.)
#[test]
fn the_listing_refusal_names_the_flag_it_collided_with() {
    let base = ReadArgs::empty(crate::cmd::backtest::read::ReadSub::Show);
    // The negative control FIRST: with the listing off, nothing here is refused.
    let mut off = ReadArgs::empty(crate::cmd::backtest::read::ReadSub::Show);
    off.metrics = true;
    off.json = true;
    assert!(refuse_a_listing_beside_a_run_rendering(&off).is_ok(), "the guard is opt-in");

    for (flag, _) in run_rendering_flags_given(&base) {
        let mut a = ReadArgs::empty(crate::cmd::backtest::read::ReadSub::Show);
        a.metrics_list = true;
        match flag {
            "--metrics" => a.metrics = true,
            "--trades" => a.trades = true,
            "--config" => a.config = true,
            "--export" => a.export = Some("trades".to_string()),
            "--html" => a.html = true,
            other => panic!(
                "{other} joined `run_rendering_flags_given` without an arm here — add it, do \
                     not delete the check"
            ),
        }
        let e = refuse_a_listing_beside_a_run_rendering(&a)
            .expect_err("a listing beside a run rendering must be refused");
        assert!(e.contains("--metrics-list"), "{flag}: {e}");
        assert!(e.contains(flag), "{flag}: the refusal must name it: {e}");
    }

    // `--json` is the separate case with its own reason: there is no JSON rendering, and the
    // sentence names what DOES answer that question.
    let mut a = ReadArgs::empty(crate::cmd::backtest::read::ReadSub::Show);
    a.metrics_list = true;
    a.json = true;
    let e = refuse_a_listing_beside_a_run_rendering(&a).expect_err("no JSON rendering exists");
    assert!(e.contains("--json") && e.contains("cli.json"), "{e}");

    // …and `--out` is NOT refused: it names a file, not a document.
    let mut a = ReadArgs::empty(crate::cmd::backtest::read::ReadSub::Show);
    a.metrics_list = true;
    a.out = Some("catalog.txt".to_string());
    assert!(refuse_a_listing_beside_a_run_rendering(&a).is_ok());
}

/// ⚠ **`--html` renders a real HTML document off a stored run, with no journal and no store.**
/// The promotion out of [`UNBUILT_RENDERERS`] is only honest if this passes: the refusal it
/// replaced said the flag "needs a vike-report EDGE, not a renderer", so the thing to prove is
/// that the edge is all it needed.
///
/// ⚠ The mutation this fails on, in PRODUCTION: delete the `_ if a.html` arm from
/// [`run_show`]'s document selection. The flag then parses, the guard passes, and the operator
/// gets the TEXT rendering under a flag that asked for HTML — the silent-wrong-answer shape
/// this file's `--trades` promotion already paid for once.
#[test]
fn html_renders_the_stored_run_as_a_document_with_no_store_and_no_journal() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("a-1-0");
    let run = seeded(
        &dir,
        Some(
            r#"{"name":"bh","final_equity":10500.0,"total_return":0.05,"n_trades":3,
                    "win_rate":0.66,"sharpe":1.25,"max_drawdown":0.031,"profit_factor":2.0}"#,
        ),
    );
    std::fs::write(
        dir.join("series.json"),
        r#"{"schema":1,"stride":1,"equity":[10000.0,10200.0,10500.0],
                "equity_ts":[1,2,3],"per_symbol_equity":[]}"#,
    )
    .unwrap();

    let html = html_document(&run).expect("a report and a series are all it needs");
    assert!(html.contains("<html"), "it is an HTML document: {}", &html[..80.min(html.len())]);
    // The metrics come from report.json and are RENDERED by the catalog's own units, which is
    // what makes this document and `report <run>`'s text agree about every number.
    assert!(html.contains("max_drawdown"), "the metric ids are the catalog's");
    assert!(html.contains("3.1000%"), "…and a drawdown of 0.031 reads as a PERCENT");
}

/// An absent `series.json` is the ORDINARY answer for a run that stopped early
/// (`vike_model::runs::read_series`'s own doc fixes that), so the tearsheet still renders —
/// without a chart, which `vike_analytics::render_html` decides for itself by refusing to draw a
/// curve shorter than two points.
///
/// ⚠ The mutation this fails on: turn the `Err(_)` arm of [`html_document`]'s `read_series`
/// match into a `?`. A run with no stored curve then gets an ERROR where it should get its
/// metrics.
#[test]
fn html_renders_without_a_chart_when_the_run_stored_no_series() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("a-1-0");
    let run = seeded(
        &dir,
        Some(
            r#"{"name":"bh","final_equity":10500.0,"total_return":0.05,"n_trades":3,
                    "win_rate":0.66,"sharpe":1.25,"max_drawdown":0.031,"profit_factor":2.0}"#,
        ),
    );
    let html = html_document(&run).expect("no series is not an error");
    assert!(html.contains("<html"), "still a document");
    assert!(html.contains("max_drawdown"), "still carries the metrics");
}

/// A run with NO report is refused by name, and the message says why there is none rather than
/// blaming the flag — the manifest is written LAST, so such a run stopped before finishing.
#[test]
fn html_over_a_run_with_no_report_refuses_and_says_why_there_is_none() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("a-1-0");
    let run = seeded(&dir, None);
    let e = html_document(&run).expect_err("no report is a refusal");
    let msg = format!("{e:?}");
    assert!(msg.contains("--html"), "{msg}");
    assert!(msg.contains("report.json"), "{msg}");
}

/// ⚠ **`--html` is refused beside every OTHER document and section flag, by name.** The rule
/// walks the same production array the listing rule does, which is why `--html` is an entry in
/// it rather than a special case beside it.
///
/// ⚠ The mutation this fails on, in PRODUCTION: delete the
/// `crate::cmd::runs::show::refuse_a_second_document` call from `crate::cmd::backtest`'s
/// `parse_read`. Every combination below then parses `Ok`, and `--html --metrics` writes an
/// HTML page while the operator waited for a metrics table.
#[test]
fn html_is_refused_beside_every_other_document_by_name() {
    let mut a = ReadArgs::empty(crate::cmd::backtest::read::ReadSub::Show);
    a.html = true;
    assert!(refuse_a_second_document(&a).is_ok(), "--html alone is the shipping case");

    for (flag, set) in [
        ("--metrics", (|a: &mut ReadArgs| a.metrics = true) as fn(&mut ReadArgs)),
        ("--trades", |a: &mut ReadArgs| a.trades = true),
        ("--config", |a: &mut ReadArgs| a.config = true),
        ("--export", |a: &mut ReadArgs| a.export = Some("trades".to_string())),
    ] {
        let mut a = ReadArgs::empty(crate::cmd::backtest::read::ReadSub::Show);
        a.html = true;
        set(&mut a);
        let e = refuse_a_second_document(&a).expect_err("two documents");
        assert!(e.contains("--html"), "{flag}: {e}");
        assert!(e.contains(flag), "{flag}: …and the flag it collided with: {e}");
        assert!(e.contains("two different documents"), "{flag}: …and WHY neither wins: {e}");
    }

    // `--json` is its own sentence: two SERIALIZATIONS rather than a document beside a section.
    let mut a = ReadArgs::empty(crate::cmd::backtest::read::ReadSub::Show);
    a.html = true;
    a.json = true;
    let e = refuse_a_second_document(&a).expect_err("two serializations");
    assert!(e.contains("--html") && e.contains("--json"), "{e}");
    assert!(e.contains("SERIALIZATIONS"), "…and says what kind of collision it is: {e}");

    // ...and `--out` COMPOSES, exactly as it does for --export and --metrics-list.
    let mut a = ReadArgs::empty(crate::cmd::backtest::read::ReadSub::Show);
    a.html = true;
    a.out = Some("sheet.html".to_string());
    assert!(refuse_a_second_document(&a).is_ok(), "--out names a file, not a document");
}
