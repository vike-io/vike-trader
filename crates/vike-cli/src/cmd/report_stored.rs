//! `vike-cli report <run>` — **re-render a FINISHED run from what it left on disk**: the producer's
//! own metrics carried through verbatim, the curve's provenance, the statistics that can be
//! recomputed EXACTLY from what was kept, and an optional per-period breakdown.
//!
//! Artifact-only — no engine, no store, no socket, no node. The only inputs are the run directory
//! `vike_model::runs` owns and the marks store beside it, which is what lets this work on a laptop
//! with neither, on a run minted months ago, or on one somebody else produced.
//!
//! # Why this lives beside the NODE half rather than under `backtest`
//!
//! [`crate::cmd::report`] is the verb whose product is a TEARSHEET, and until now it had exactly one
//! source: a running `vike-tradehub` node's live journal. That made "re-report the backtest I ran
//! yesterday" unanswerable by the verb whose name is `report`, while
//! `crate::cmd::runs::show` — the verb whose name is `show` — was the only thing that could open the
//! artifact. Four of the ten competitor tools with any backtest surface can re-report a finished run
//! (Freqtrade's `backtesting-show`, LEAN's `lean report`, QuantRocket's `zipline tearsheet`); the
//! capability was missing, not the verb. So `report` gained a SECOND SOURCE rather than the backtest
//! plane gaining a second `show`.
//!
//! # ⚠ THE HARD RULE: a statistic recomputed from a THINNED curve is refused, never printed
//!
//! `vike_model::runs::MAX_EQUITY_SAMPLES` caps what a run record keeps, and
//! `vike_model::runs::decimate` thins the curve past it — `vike_model::runs::RunSeries::stride`
//! above `1` means samples were DROPPED. That constant's own declared residual is the whole of the
//! problem: *"a max-drawdown recomputed from a thinned curve can be SHALLOWER than the one in
//! `REPORT_FILE`"*. So a recomputed number can silently disagree with `report.json`'s own, which is
//! the one thing a re-report may never do — the operator is holding two documents about one run and
//! has no way to know which is wrong.
//!
//! ⚠ **This said `crate::cmd::runs::show`'s `refuse_an_unbuilt_renderer` "already draws this line
//! for `--breakdown`", and it does not.** That sentence read as though the two verbs applied one
//! rule to one condition; they do not, and the difference is the operator's whole experience.
//! `show` refuses `--breakdown` for EVERY run, thinned or not, because its guard sits in
//! `crate::cmd::backtest`'s `parse_read`, which sees argv alone — there is no run resolved there
//! and therefore no `stride` to test. So **this module is the only place the condition is actually
//! EVALUATED**, and it draws the line in two places rather than one:
//!
//! * [`breakdown_of`] REFUSES on a thinned curve, naming the stride, what the run produced and the
//!   cap. It is a refusal rather than a `null` because the operator ASKED — a null would read as
//!   "there were no periods".
//! * [`derived_of`] nulls the statistics a thinned curve cannot answer and says so in one sentence
//!   (`unavailable`), because nobody asked for those specifically and the document has a fixed key
//!   set. `crate::cmd::report_schema`'s `NULL_CONVENTION` is the published rule.
//!
//! ⚠ **One statistic survives decimation and it is worth knowing which**: `final_equity`.
//! `vike_model::runs::decimate` keeps the LAST sample unconditionally, *"so the two can never
//! disagree about where the run ENDED"*. It is therefore exact at every stride, and nulling it
//! would be this side refusing to answer a question the record answers.
//!
//! # What is RECOMPUTED, and why that is safe
//!
//! The recomputed drawdown IS `vike_analytics::metrics::max_drawdown` — CALLED, not transcribed.
//! It was a transcription until this crate gained a normal vike-analytics edge
//! (`crates/vike-cli/Cargo.toml` argues that row), and the copy is gone. What made the copy safe is
//! still the rule that decides the next one: it
//! performs no SUMMATION at all: a running peak and a running maximum, so there is no
//! `vike_model::py_sum` ordering question and no accumulated rounding. On a WHOLE curve it is the
//! same arithmetic over the same samples as the number in `report.json`, which is what licenses this
//! module to print it beside that one.
//!
//! Per-period returns are `end / start - 1` per bucket, which is likewise division and nothing else.
//!
//! # The three shared sections are RENDERED BY THE SIBLING, not re-worded here
//!
//! The run header, the metrics block and the trade ledger already have a renderer with three-way
//! honesty about a file that is absent, on disk and unreadable, or on disk and unparseable
//! (`crate::cmd::runs::show`'s `ReportState`). This module calls `show_text`, `report_state` and
//! `trades_json` rather than restating any of those sentences: two verbs printing different words
//! for the same missing file is exactly how an operator comes to believe a run did not finish when
//! its report is merely corrupt.

use std::path::Path;

use serde_json::{Value, json};
use vike_model::runs::{MAX_EQUITY_SAMPLES, RunReadError, RunSeries, SERIES_FILE, read_series};

use crate::cmd::report_schema::{DOCUMENT_ID, PERIODS, REPORT_SCHEMA};
use crate::cmd::runs::scan::{ScannedRun, scan_runs};
use crate::cmd::runs::selector::resolve_one;
use crate::cmd::runs::show::{report_state, show_text, trades_json};
use crate::exit::{CliError, CmdResult};

/// Which bucket `--breakdown` groups the curve into. The roster and the argument for what is NOT
/// here are `crate::cmd::report_schema`'s [`PERIODS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Period {
    /// One row per UTC calendar day.
    Day,
    /// One row per UTC calendar month.
    Month,
}

impl Period {
    /// The spelling the operator typed, which is also what the document records.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Period::Day => "day",
            Period::Month => "month",
        }
    }

    /// Parse `--breakdown`'s value, refusing an unknown one against the PUBLISHED roster — so the
    /// parser's sentence and `crate::cmd::report_schema::json_schema`'s `enum` can never name
    /// different sets.
    ///
    /// ⚠ **This claimed to refuse "against the PUBLISHED roster rather than against a second
    /// list", and the second list is directly below it**: the ACCEPT arms are hand-written literals
    /// with no link to [`PERIODS`] at all. Only the REFUSAL renders the roster, so the direction
    /// the doc promised held and the one it implied did not — a third bucket added to `PERIODS`
    /// would be published in the schema `enum`, advertised in `crate::cmd::report`'s `USAGE` and
    /// named in this very refusal, and then rejected by this `match`: a refusal that lists the
    /// period it is refusing. The ARGUMENT is untouched and is still why the refusal reads the
    /// roster rather than retyping it; what was missing was a test in the other direction, and
    /// `every_published_period_is_one_this_parser_accepts` is it. (The accept arms stay literals
    /// deliberately: a `PERIODS`-driven parse would have to map a string back to a variant, which
    /// is the same table written in a less readable place.)
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "day" => Ok(Period::Day),
            "month" => Ok(Period::Month),
            other => Err(format!(
                "--breakdown '{other}' is not a period this verb knows — {}. A WEEK is \
                 deliberately absent: `vike_model::time` is this workspace's one home for calendar \
                 math and has no ISO-week labeller, and adding one here would be a second calendar \
                 home in the crate that is meant to have none.",
                PERIODS.join(" | ")
            )),
        }
    }
}

/// What `series.json` turned out to be — the same THREE-outcome shape
/// `crate::cmd::runs::show`'s `ReportState` draws for `report.json`, and for the same reason: an
/// ABSENT curve is the ordinary state of a producer that keeps none, while one that is ON DISK and
/// unusable is a fault with a fix, and folding them together tells an operator their run kept no
/// curve when in fact their file is corrupt.
enum SeriesState {
    /// The producer wrote no `series.json`.
    Absent,
    /// It is there and could not be read or parsed — the reader's own words.
    Unusable(String),
    /// The document.
    Read(RunSeries),
}

/// Read the curve once and classify the outcome.
fn series_state(run: &ScannedRun) -> SeriesState {
    match read_series(&run.dir) {
        Ok(series) => SeriesState::Read(series),
        Err(RunReadError::Missing { .. }) => SeriesState::Absent,
        Err(other) => SeriesState::Unusable(other.to_string()),
    }
}

/// A number, or `null` when it is not finite — `crate::cmd::report_schema`'s `NULL_CONVENTION`
/// applied at the leaf.
///
/// ⚠ Spelled out rather than left to `serde_json`, which would do the same thing: `impl From<f64>
/// for Value` routes a non-finite value through `Number::from_f64` and lands on `Value::Null`
/// anyway (`crate::cmd::runs::jsondoc`'s `a_non_finite_number_cannot_reach_this_at_all` measures
/// that mechanism from the other side). Relying on it silently would make a reader think this
/// document had never considered the case.
fn finite_or_null(x: f64) -> Value {
    if x.is_finite() { json!(x) } else { Value::Null }
}

/// The curve's PROVENANCE — never its samples.
///
/// `--export equity` (`crate::cmd::runs::show`'s `export_document`) is where the samples themselves
/// come from, and duplicating 20,000 floats into a tearsheet would make the one document an
/// operator reads unreadable. What travels instead is the four numbers that say whether anything
/// computed from those samples can be trusted.
fn curve_of(series: &RunSeries) -> Value {
    json!({
        "samples": series.equity.len(),
        "source_len": series.source_len,
        "stride": series.stride,
        "whole": series.stride == 1,
        "cap": MAX_EQUITY_SAMPLES,
    })
}

/// The one-sentence reason every thinned-curve statistic is `null`. Shared by the document and the
/// human rendering so the two cannot word it differently.
fn thinned_sentence(series: &RunSeries) -> String {
    // ⚠ The CAP is an inline capture (`{MAX_EQUITY_SAMPLES}`) rather than a positional argument:
    // clippy's `uninlined_format_args` fires on a plain identifier passed positionally, and this
    // crate's clippy lane runs `-D warnings`. Field accesses and calls cannot be captured, so they
    // stay positional — the mix is the lint's own rule, not a style choice.
    format!(
        "the stored equity curve is DECIMATED (stride {}, {} samples kept of {}, cap \
         {MAX_EQUITY_SAMPLES}), so a statistic recomputed from it can disagree with report.json's \
         own — report.json is the authority for every scalar",
        series.stride,
        series.equity.len(),
        series.source_len
    )
}

/// ⚠ **This was a TRANSCRIPTION of `vike_analytics::metrics::max_drawdown`, and its own argument for
/// being one has expired.** It read "it is a transcription rather than a call because this crate
/// links no engine crate" — and vike-analytics is a normal dependency now, declared with its
/// rationale in this crate's manifest. So the copy is deleted and the authority is called.
///
/// The transcription was safe while it existed, and the reason is worth keeping because it is the
/// rule that decides the NEXT one: there is no summation in this fold. An accumulating fold would
/// owe the `vike_model::py_sum` ordering question this workspace's bit-parity rests on, and two
/// copies of THAT could diverge in the last bit. A running max owes nothing. A future fold that
/// sums may not be transcribed at all.
use vike_analytics::metrics::max_drawdown;

/// The statistics this verb RECOMPUTES, each either a number or `null` with a published reason.
///
/// Three rows and three different dispositions, which is the whole content of this function:
///
/// * `final_equity` — the last kept sample, and EXACT at every stride, because
///   `vike_model::runs::decimate` keeps the final sample unconditionally.
/// * `peak_equity` — `null` on a thinned curve: the peak may be one of the dropped samples.
/// * `max_drawdown` — `null` on a thinned curve, where it could only ever be SHALLOWER than
///   `report.json`'s (that constant's own declared residual).
fn derived_of(series: &RunSeries) -> Value {
    let whole = series.stride == 1;
    let exact_and_present = whole && !series.equity.is_empty();
    // Present at every stride — see this function's doc. `null` only when there is no sample at all,
    // which is a curve document with an empty vector rather than a thinned one.
    let final_equity = series.equity.last().copied().map_or(Value::Null, finite_or_null);
    let peak_equity = if exact_and_present {
        finite_or_null(series.equity.iter().copied().fold(f64::NEG_INFINITY, f64::max))
    } else {
        Value::Null
    };
    let max_dd =
        if exact_and_present { finite_or_null(max_drawdown(&series.equity)) } else { Value::Null };
    let unavailable = if whole { Value::Null } else { Value::String(thinned_sentence(series)) };
    json!({
        "exact": whole,
        "final_equity": final_equity,
        "peak_equity": peak_equity,
        "max_drawdown": max_dd,
        "unavailable": unavailable,
    })
}

/// The UTC bucket label a sample's timestamp falls in.
///
/// Built on `vike_model::time::civil_from_days` — this workspace's one home for calendar math, and
/// the same call `crate::cmd::mcp_trace`'s month bucketing makes. ⚠ It formats the parts itself
/// rather than slicing `vike_model::time::epoch_ms_to_utc_date`'s output: that string is
/// `{y:04}-{m:02}-{d:02}`, so a year outside four digits (or a negative one) changes its LENGTH and
/// a byte slice would silently mislabel the row.
fn period_label(period: Period, ts_ms: i64) -> String {
    let (y, mo, d) = vike_model::time::civil_from_days(ts_ms.div_euclid(86_400_000));
    match period {
        Period::Day => format!("{y:04}-{mo:02}-{d:02}"),
        Period::Month => format!("{y:04}-{mo:02}"),
    }
}

/// Per-period returns off the stored curve, or a REFUSAL naming what is wrong with the artifact.
///
/// Five refusals, each a different fact about the run and each answered on its own rung:
///
/// * no `series.json` — [`crate::exit::Exit::Empty`]. That rung exists so *"the gate passed"* and
///   *"the gate checked nothing"* stop sharing a number, and a breakdown over a run with no curve is
///   precisely nothing evaluated.
/// * an empty curve — the same rung, for the same reason.
/// * `series.json` on disk and unusable — [`crate::exit::Exit::Failed`], naming the reader's words.
/// * **a THINNED curve — `Exit::Failed`, and this is the refusal this module exists around.** See
///   the module doc: a period breakdown computed off dropped samples disagrees with `report.json`
///   and nothing says so.
/// * no timestamps, or a curve whose timestamps do not line up with it — `Exit::Failed`. A bucket
///   is a function of TIME, so an untimestamped curve cannot be bucketed at all;
///   `vike_model::runs::RunSeries::is_aligned` is the document's own invariant and a reader asserts
///   it rather than trusting it.
///
/// ⚠ **Not `Exit::Usage`, for any of them.** The command line is correct — it is the ARTIFACT that
/// cannot answer — and a wrapper that read a `2` here would go looking for a flag to fix.
///
/// # The FIRST row's base
///
/// Every row's `start_equity` is the PREVIOUS period's close, which is what makes consecutive
/// returns chain. The first row has no previous period, so its base is the curve's own first sample
/// — its return is therefore measured from the run's opening equity rather than from a prior close.
/// Stated because it is the one row whose meaning differs, and because the alternative (dropping
/// the first row) throws away the only period a short run has.
///
/// ⚠ A declared residual: rows are groups of CONSECUTIVE equal labels, so a `series.json` whose
/// timestamps are not ascending renders a label more than once, in curve order. Sorting or merging
/// would hide a corrupt artifact behind a tidy table, which is the trade this family refuses
/// everywhere else.
fn breakdown_of(run: &ScannedRun, series: &SeriesState, period: Period) -> CmdResult<Value> {
    let series = match series {
        SeriesState::Absent => {
            return Err(CliError::empty(format!(
                "{} wrote no {SERIES_FILE} — there is no equity curve to break down. This producer \
                 keeps no curve, or the run was minted before the run record held one; `report {}` \
                 without --breakdown still renders everything the artifact does hold.",
                run.run_id, run.run_id
            )));
        }
        SeriesState::Unusable(why) => {
            return Err(CliError::failed(format!(
                "{SERIES_FILE} IS on disk for {} and cannot be used ({why}) — the run itself \
                 finished; open the file or fix its permissions",
                run.run_id
            )));
        }
        SeriesState::Read(s) => s,
    };

    if series.equity.is_empty() {
        return Err(CliError::empty(format!(
            "{}'s {SERIES_FILE} holds no equity samples at all ({} produced) — nothing to break \
             down",
            run.run_id, series.source_len
        )));
    }
    // ⚠ THE RULE, and the one place it is ever tested against a run. This comment read "the same
    // refusal with the run's own numbers in it", which overstated the pairing:
    // `crate::cmd::runs::show`'s `refuse_an_unbuilt_renderer` cites this same ground for
    // `--breakdown` and then applies it to every run, having no `stride` to read — so this is not
    // that refusal with numbers added, it is the refusal that can be WRONG, and therefore the one
    // that fires only when the curve really was thinned.
    if series.stride != 1 {
        return Err(CliError::failed(format!(
            "--breakdown is refused for {}: {}. `--export equity` (`vike-cli backtest show {} \
             --export equity`) hands you the curve that IS stored, stride and all, so nothing is \
             hidden — what is refused is a NUMBER that would look exact and is not.",
            run.run_id,
            thinned_sentence(series),
            run.run_id
        )));
    }
    if series.equity_ts.is_empty() {
        return Err(CliError::failed(format!(
            "--breakdown is refused for {}: its {SERIES_FILE} carries no timestamps. \
             `vike_model::runs::RunSeries::equity_ts` is documented as EMPTY when the producer does \
             not track them, which every vector kernel does not — and a period is a function of \
             TIME, so an untimestamped curve cannot be bucketed at all. Numbering the samples \
             instead would invent periods the run never had.",
            run.run_id
        )));
    }
    if !series.is_aligned() {
        return Err(CliError::failed(format!(
            "--breakdown is refused for {}: its {SERIES_FILE} breaks its own invariant — {} \
             timestamps for {} equity samples, where \
             `vike_model::runs::RunSeries::is_aligned` requires the same length or none. The \
             document is misaligned, so every bucket boundary in it is a guess.",
            run.run_id,
            series.equity_ts.len(),
            series.equity.len()
        )));
    }

    let mut rows: Vec<Value> = Vec::new();
    let mut label = String::new();
    let mut open = false;
    let mut start = series.equity[0];
    let mut end = series.equity[0];
    for (&eq, &ts) in series.equity.iter().zip(series.equity_ts.iter()) {
        let this = period_label(period, ts);
        if !open {
            label = this;
            open = true;
            end = eq;
        } else if this == label {
            end = eq;
        } else {
            rows.push(period_row(&label, start, end));
            // The closed period's last sample is the next period's base — which is what makes
            // consecutive rows chain into the run's whole return.
            start = end;
            end = eq;
            label = this;
        }
    }
    if open {
        rows.push(period_row(&label, start, end));
    }
    Ok(json!({ "period": period.as_str(), "rows": rows }))
}

/// One breakdown row. `return` is `null` when the base is zero or non-finite — the null convention,
/// applied where the division would otherwise produce an infinity somebody reads as a result.
fn period_row(label: &str, start: f64, end: f64) -> Value {
    let ret = if start.is_finite() && start != 0.0 && end.is_finite() {
        finite_or_null(end / start - 1.0)
    } else {
        Value::Null
    };
    json!({
        "period": label,
        "start_equity": finite_or_null(start),
        "end_equity": finite_or_null(end),
        "return": ret,
    })
}

/// The document, with the FIXED top-level key set `crate::cmd::report_schema`'s `TOP_LEVEL_KEYS`
/// publishes: every key always present, an absent section `null`.
fn document(run: &ScannedRun, series: &SeriesState, breakdown: Option<&Value>) -> Value {
    let (curve, derived) = match series {
        SeriesState::Read(s) => (curve_of(s), derived_of(s)),
        SeriesState::Absent => (Value::Null, Value::Null),
        // The fault names ITSELF rather than reading as absent — the same shape
        // `crate::cmd::runs::show`'s `ReportState::to_json` uses, and for the same reason: `null`
        // is what "the producer kept no curve" means, and a `jq` consumer has to tell them apart.
        SeriesState::Unusable(why) => (json!({ "error": "unusable", "detail": why }), Value::Null),
    };
    let m = &run.manifest;
    json!({
        "document": DOCUMENT_ID,
        "schema": REPORT_SCHEMA,
        "run": {
            "run_id": run.run_id,
            "dir": run.dir.display().to_string(),
            "kind": m.kind,
            "produced_by": m.produced_by,
            "started_at": m.started_at,
            "finished_at": m.finished_at,
            "git_sha": m.git_sha,
        },
        "metrics": report_state(run).to_json(),
        "trades": trades_json(run),
        "curve": curve,
        "derived": derived,
        "breakdown": breakdown.cloned().unwrap_or(Value::Null),
    })
}

/// The human rendering: the sibling's header, metrics and ledger, then the three sections this verb
/// adds.
///
/// ⚠ The first three come from `crate::cmd::runs::show`'s `show_text` rather than being re-worded —
/// see this module's doc. The `config` section is deliberately NOT asked for: a resolved profile is
/// a page of TOML and a tearsheet is a page somebody reads, and `backtest show <id> --config`
/// already prints it.
fn render_text(run: &ScannedRun, series: &SeriesState, breakdown: Option<&Value>) -> String {
    let mut out = show_text(run, true, true, false);

    out.push_str("\ncurve\n");
    match series {
        SeriesState::Absent => out.push_str(&format!(
            "  no {SERIES_FILE} — this producer kept no equity curve, so nothing below is derived\n"
        )),
        SeriesState::Unusable(why) => out.push_str(&format!(
            "  {SERIES_FILE} IS on disk and cannot be used ({why}) — the run itself finished\n"
        )),
        SeriesState::Read(s) => {
            out.push_str(&format!(
                "  {} samples kept of {} (stride {}, cap {MAX_EQUITY_SAMPLES})\n",
                s.equity.len(),
                s.source_len,
                s.stride
            ));
            if s.stride == 1 {
                out.push_str("  WHOLE — a statistic recomputed from it is exact\n");
            } else {
                out.push_str(&format!("  THINNED — {}\n", thinned_sentence(s)));
            }
            out.push_str("\nderived\n");
            let d = derived_of(s);
            for key in ["final_equity", "peak_equity", "max_drawdown"] {
                // ⚠ A `null` prints as the WORD rather than as a blank: the whole point of the null
                // convention is that "undefined" is visibly not `0`, and a blank column reads as a
                // rendering bug.
                out.push_str(&format!("  {key} = {}\n", d[key]));
            }
            if let Some(why) = d["unavailable"].as_str() {
                out.push_str(&format!("  (the nulls above: {why})\n"));
            }
        }
    }

    if let Some(b) = breakdown {
        out.push_str(&format!("\nbreakdown ({})\n", b["period"].as_str().unwrap_or("-")));
        out.push_str(&format!("  {:<12}{:>16}{:>16}{:>14}\n", "period", "start", "end", "return"));
        for row in b["rows"].as_array().map(Vec::as_slice).unwrap_or_default() {
            // ⚠ Rendered to STRINGS first. `serde_json::Value`'s own `Display` writes straight to
            // the writer and honours none of the formatter's width or alignment flags, so
            // `{:>16}` on a `Value` compiles and silently pads nothing — a column that looks
            // aligned in the source and is ragged on screen.
            out.push_str(&format!(
                "  {:<12}{:>16}{:>16}{:>14}\n",
                row["period"].as_str().unwrap_or("-"),
                row["start_equity"].to_string(),
                row["end_equity"].to_string(),
                row["return"].to_string()
            ));
        }
    }
    out
}

/// Entry point [`crate::cmd::report`] routes the STORED source to.
///
/// `runs_root` and `marks_root` are PARAMETERS for the reason every path in this family is one: a
/// `src/cmd/` file may not resolve a project for itself — the rule
/// `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` ratchets down. `None` is a process
/// with no project above the working directory, which is an ordinary state and is refused with a
/// sentence rather than assumed away.
pub(crate) fn run_stored(
    runs_root: Option<&Path>,
    marks_root: Option<&Path>,
    selector: &str,
    breakdown: Option<Period>,
    json_out: bool,
) -> CmdResult<String> {
    let root = runs_root.ok_or_else(|| {
        CliError::failed(
            "no project directory above the working directory, so there is no runs directory to \
             read. `vike-cli init` creates one, or name it with VIKE_USER_DATA_DIR.",
        )
    })?;
    let scan = scan_runs(root);
    // ⚠ STDERR, in both modes: one unreadable run directory must not make a machine-readable
    // document unparseable. `crate::cmd::runs`'s module doc states the rule for the whole family.
    for problem in &scan.problems {
        eprintln!("vike-cli report: {problem}");
    }
    let run = resolve_one(&scan, marks_root, selector)?;
    let series = series_state(run);
    // ⚠ Computed BEFORE anything is printed, so a refusal exits with nothing on stdout. A document
    // that had already been emitted beside a non-zero rung would hand a `set -e` caller half an
    // answer.
    let breakdown = match breakdown {
        Some(p) => Some(breakdown_of(run, &series, p)?),
        None => None,
    };
    // ⚠ BOTH renderings end with exactly one newline, so the caller can `print!` the answer the way
    // `crate::cmd::runs::show`'s `run_show` does. The text form already ends with one; the JSON form
    // gets it added, because `to_string_pretty` does not and a document glued to the shell prompt is
    // a papercut on the surface whose whole audience is a script.
    if json_out {
        let doc = document(run, &series, breakdown.as_ref());
        let text = serde_json::to_string_pretty(&doc)
            .unwrap_or_else(|e| format!("{{\"error\":\"cannot render the document: {e}\"}}"));
        Ok(format!("{text}\n"))
    } else {
        Ok(render_text(run, &series, breakdown.as_ref()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::report_schema::TOP_LEVEL_KEYS;
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
    /// `crate::cmd::report_schema::json_schema`'s `enum` cannot name different sets.
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
    /// `crate::cmd::report_schema`'s `the_period_roster_is_the_schema_enum_and_omits_week` over the
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
                    "`{p}` is published in `crate::cmd::report_schema`'s PERIODS — the schema \
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
    /// `crate::cmd::report_schema`'s `TOP_LEVEL_KEYS` declares is a key this document emits, and
    /// every key it emits is declared — which is the half `report_schema`'s own tests cannot check,
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
}
