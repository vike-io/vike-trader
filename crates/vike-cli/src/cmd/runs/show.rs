//! `vike-cli backtest show <id>` — **re-render one stored run in any shape, with no recompute**
//! (spec §6.2). Artifact-only: no engine, no store, no socket.
//!
//! # ⚠ What the RUN RECORD holds decides which §6.2 flags can exist, and it grew
//!
//! The plan behind this stage was drafted against a record that was a manifest beside a
//! ten-scalar report, and said so: with no equity curve, no trade ledger and no per-bar returns,
//! six of §6.2's flags were refusals. The record has since grown `series.json` (a decimated equity
//! curve plus per-symbol curves and the run's diagnostic counters), `trades.json` (a bounded,
//! chronological-PREFIX ledger) and `config.toml` — `vike_model::runs::RESERVED_FILES` is the
//! roster. So the line moved, and it moved by READING the artifact rather than by re-reading the
//! spec:
//!
//! * **`--trades` SHIPS.** The ledger is on disk (`vike_model::runs::read_trades`), it says how
//!   many trades the run actually closed even when the kept list is a prefix, and rendering it
//!   needs nothing that was dropped.
//! * **`--export` stays refused, with a CORRECTED reason.** §6.2's value set is
//!   `trades,equity,fills` and `fills` is not stored at ANY size — an export that silently dropped
//!   a third of what its own flag advertises is worse than one that refuses — and the flag has no
//!   destination grammar in the spec beyond `--out`, which this verb already spends on the
//!   rendered document.
//! * **`--drawdowns N`, `--breakdown`, `--attribution` and `--html` stay refused**, and two of
//!   those four reasons had to be rewritten: the equity curve is no longer missing, so `--drawdowns`
//!   is refused because the drawdown TABLE is a renderer nobody has built, and `--breakdown` because
//!   a DECIMATED curve cannot give back per-bar returns (`RunSeries::stride` above `1` means samples
//!   were dropped, and a period breakdown computed off a thinned curve is a number that disagrees
//!   with `report.json`'s own).
//!
//! They are refused BY NAME rather than falling into "unknown option", because an operator typing
//! one is reading them off the design document: "unknown option --export" says the flag does not
//! exist, when the truth is that some of the data does not. [`refuse_an_unbuilt_renderer`] owns
//! every sentence, and `crate::cmd::backtest`'s `parse_read` is its only caller.
//!
//! # ⚠ `--config` says what is NOT recorded
//!
//! §6.2 promises "the resolved profile that produced the run, including every `--set` override".
//! `vike_model::runs::RunConfig` holds the profile's PATH (as the operator spelled it, deliberately
//! not canonicalized) and its NAME. The resolved TEXT is written beside it as
//! `vike_model::runs::CONFIG_FILE` by a producer that has one — and a producer that does not writes
//! no such file, so `--config` prints what is there and SAYS when the resolved profile is absent. A
//! heading that read "resolved profile" over a bare path would be positive confirmation of
//! something false, which is the defect `vike_config::CONSUMPTION` exists to catch one layer up.
//!
//! # `report.json` is read as `serde_json::Value`
//!
//! `vike_analytics::report::BacktestReport` derives `Serialize` only — there is no `Deserialize` —
//! and `crate::cmd::backtest`'s `execute` already re-parses for the same reason. `profit_factor`
//! serializes as `null` when non-finite and `zero_trade` is skipped when absent; both are carried
//! through VERBATIM rather than normalised, because a null that became `0.0` would make a broken
//! run look like a flat one.

use vike_model::runs::{
    RunReadError, SERIES_FILE, TRADES_FILE, read_config_toml, read_series, read_trades,
};

use crate::cmd::backtest::ReadArgs;
use crate::cmd::runs::scan::{ScannedRun, scan_runs};
use crate::cmd::runs::{Ctx, selector::resolve_one};
use crate::exit::{CliError, CmdResult};

/// The §6.2 renderers this stage cannot build. One roster, named in one place, and every entry
/// carries the reason it is here in [`refuse_an_unbuilt_renderer`] rather than in prose.
///
/// ⚠ Two flags that were on it are NOT any more, and both left for the same reason: the run record
/// grew what they needed. `--trades` renders `vike_model::runs::TRADES_FILE`, and `--export` serves
/// its per-VALUE subset through [`export_document`] — a whole-flag refusal over one unavailable
/// VALUE was the wrong shape beside `--drawdowns` and `--breakdown`, which are refused narrowly.
pub(crate) const UNBUILT_RENDERERS: [&str; 4] =
    ["--html", "--breakdown", "--attribution", "--drawdowns"];

/// The sentence each one is refused with. `crate::cmd::backtest`'s `parse_read` is the only caller.
pub(crate) fn refuse_an_unbuilt_renderer(flag: &str) -> String {
    let needs = match flag {
        "--drawdowns" => {
            "a drawdown-table renderer, which nothing in this tree has built. The EQUITY CURVE it \
             would read is in the run record now (series.json), so this is the renderer that is \
             missing rather than the data — `--export equity` hands you the curve today"
        }
        "--breakdown" => {
            "the PER-BAR RETURNS. The stored equity curve is DECIMATED once a run exceeds the \
             sample cap (series.json's `stride` above 1 means samples were dropped), so a period \
             breakdown computed from it would disagree with report.json's own numbers"
        }
        "--attribution" => "a per-source attribution the engine does not compute at all",
        "--html" => "a tearsheet renderer, which nothing in this tree has built",
        other => return format!("unknown option '{other}'"),
    };
    format!(
        "{flag} is not available yet — it needs {needs}. What `show` can render today: --metrics, \
         --trades, --config, --export trades|equity, --json, --out."
    )
}

/// `--export`'s values, and which of the three the run record can actually serve.
///
/// ⚠ **The refusal is per-VALUE, not per-flag, and that is the correction.** `--export` was refused
/// whole because ONE of its three values is unavailable — which is inconsistent with its two
/// neighbours, both refused for narrow, per-flag reasons (`--drawdowns` for a missing renderer,
/// `--breakdown` for a decimated curve). `trades` and `equity` are FULLY stored
/// (`vike_model::runs::TRADES_FILE` / `SERIES_FILE`), each is a single document, and each writes
/// through the `--out` this verb already has — so shipping them invents no grammar at all.
/// `fills` is the one that cannot be served, and it is refused BY NAME with the reason.
const EXPORTABLE: [&str; 2] = ["trades", "equity"];

/// Parse `--export VALUE` into a document, or a refusal that names what is wrong with the VALUE.
///
/// ⚠ **A multi-value list is refused by name too**, and the reason is `--out`: `--export
/// trades,equity` is two documents and this verb writes ONE. Silently picking the first, or
/// concatenating two JSON documents into a file nothing can parse, are both worse than saying so.
/// Run the verb twice.
fn export_document(run: &ScannedRun, spec: &str) -> Result<String, CliError> {
    let value = spec.trim();
    if value.contains(',') {
        return Err(CliError::usage(format!(
            "--export '{value}': one value at a time. Each export is a single document and `--out` \
             names a single file, so a list would either be truncated to its first value or written \
             as two documents nothing can parse. Run `show` once per value."
        )));
    }
    match value {
        "trades" => {
            let ledger = read_trades(&run.dir).map_err(|e| match e {
                RunReadError::Missing { .. } => CliError::failed(format!(
                    "{} wrote no {} — this producer keeps no trade ledger (a study, or a run minted \
                     before the run artifact existed)",
                    run.run_id, TRADES_FILE
                )),
                other => CliError::failed(other.to_string()),
            })?;
            Ok(serde_json::to_string_pretty(&ledger)
                .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}")))
        }
        "equity" => {
            let series = read_series(&run.dir).map_err(|e| match e {
                RunReadError::Missing { .. } => CliError::failed(format!(
                    "{} wrote no {} — this producer keeps no equity curve",
                    run.run_id, SERIES_FILE
                )),
                other => CliError::failed(other.to_string()),
            })?;
            // ⚠ `stride` and `source_len` ride along and are NOT decoration: a curve thinned above
            // stride 1 is a SAMPLE, and a statistic recomputed from it can disagree with
            // report.json's own. An export that dropped them would hand somebody a curve with no way
            // to know that.
            Ok(serde_json::to_string_pretty(&series)
                .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}")))
        }
        // The one value the record cannot serve, refused BY NAME rather than by refusing the flag.
        "fills" => Err(CliError::usage(
            "--export fills is not available — fills are not stored at ANY size. A backtest \
             computes them, consumes them into report.json's scalars and drops them; only the trade \
             LEDGER and the equity curve survive. `--export trades` and `--export equity` both work \
             today."
                .to_string(),
        )),
        other => Err(CliError::usage(format!(
            "--export '{other}' is not a value this verb knows — §6.2's set is trades, equity and \
             fills, of which {} are stored (fills are not: see `--export fills`)",
            EXPORTABLE.join(" and ")
        ))),
    }
}

/// What `report.json` turned out to be. **Three outcomes, never two** — the distinction
/// `crate::cmd::runs::scan` already draws for the MANIFEST, and `vike_model::runs::RunReadError`
/// models one crate down.
///
/// ⚠ An ABSENT report is the ordinary shape of an unfinished run (the manifest is written LAST, so
/// a run that stopped mid-write has no report). A report that is ON DISK and cannot be read, or
/// cannot be parsed, is a FAULT with a fix, and folding it into "absent" tells an operator their run
/// did not finish when in fact their file is corrupt or unreadable. That conflation is exactly what
/// this family's own doctrine refuses, and `show` had it: both landed as `"report": null`.
enum ReportState {
    /// No `report.json` beside the manifest.
    Absent,
    /// It is there and the bytes would not come back (permissions, a vanished file, an I/O error).
    Unreadable(String),
    /// It is there, it was read, and it is not JSON.
    Unparseable(String),
    /// The document, verbatim.
    Read(serde_json::Value),
}

/// Read `report.json` once and classify the outcome. Never a typed parse — see this module's doc.
fn report_state(run: &ScannedRun) -> ReportState {
    let Some(path) = &run.report else { return ReportState::Absent };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => return ReportState::Unreadable(e.to_string()),
    };
    match serde_json::from_str(&text) {
        Ok(v) => ReportState::Read(v),
        Err(e) => ReportState::Unparseable(e.to_string()),
    }
}

impl ReportState {
    /// The `--json` half. ⚠ A fault is an OBJECT naming itself rather than `null`, because `null`
    /// is what an absent report is and a consumer must be able to tell them apart with `jq`.
    fn to_json(&self) -> serde_json::Value {
        match self {
            ReportState::Absent => serde_json::Value::Null,
            ReportState::Read(v) => v.clone(),
            ReportState::Unreadable(why) => {
                serde_json::json!({ "error": "unreadable", "detail": why })
            }
            ReportState::Unparseable(why) => {
                serde_json::json!({ "error": "unparseable", "detail": why })
            }
        }
    }
}

/// `{ run_id, dir, manifest, report }`, plus `trades` when `--trades` was asked for — the artifact,
/// verbatim.
///
/// ⚠ **`trades` is present only when the flag is, and that is deliberate.** The ledger is a SEPARATE
/// file from the report, so a document that always carried it would open `trades.json` for every
/// `show --json`; and a `--trades --json` that silently carried none would answer
/// `jq '.trades'` with `null` on exit `0`, which is indistinguishable from a run whose producer
/// wrote no ledger. Absent flag ⇒ absent key; flag ⇒ a key that says which of the three things it
/// found.
pub(crate) fn show_json(run: &ScannedRun, trades: bool) -> String {
    let mut doc = serde_json::json!({
        "run_id": run.run_id,
        "dir": run.dir.display().to_string(),
        "manifest": run.manifest,
        "report": report_state(run).to_json(),
    });
    if let Some(obj) = doc.as_object_mut().filter(|_| trades) {
        obj.insert("trades".to_string(), trades_json(run));
    }
    serde_json::to_string_pretty(&doc).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

/// The ledger as a document, with the same three-way honesty [`ReportState`] applies to the report:
/// `null` when the producer wrote none, an `error` object when it is there and unusable, and
/// otherwise the kept trades beside `closed`, the TRUE count — which is not `trades.len()` once a
/// run exceeds the cap, because the kept list is a chronological PREFIX.
fn trades_json(run: &ScannedRun) -> serde_json::Value {
    match read_trades(&run.dir) {
        Ok(ledger) => serde_json::json!({
            "closed": ledger.source_len,
            "kept": ledger.trades.len(),
            "trades": ledger.trades,
        }),
        Err(RunReadError::Missing { .. }) => serde_json::Value::Null,
        Err(e) => serde_json::json!({ "error": "unreadable", "detail": e.to_string() }),
    }
}

/// ⚠ `report.json`'s keys are rendered in `vike_analytics::report::BacktestReport`'s DECLARATION
/// order, with any key not on this list printed after it, alphabetically. A `serde_json::Map`
/// iterates in insertion order only with the `preserve_order` feature, which this workspace does not
/// enable; without a declared order the block would reorder itself between serde versions.
const REPORT_KEY_ORDER: [&str; 11] = [
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
];

/// The human rendering. `metrics`/`trades`/`config` select SECTIONS; with none of them set, every
/// section the artifact can fill is rendered — the common case is "show me this run".
pub(crate) fn show_text(run: &ScannedRun, metrics: bool, trades: bool, config: bool) -> String {
    let all = !metrics && !trades && !config;
    let m = &run.manifest;
    let mut out = String::new();
    out.push_str(&format!("run     {}\n", run.run_id));
    out.push_str(&format!("kind    {}\n", m.kind));
    out.push_str(&format!("by      {}\n", m.produced_by));
    out.push_str(&format!("started {}\n", m.started_at));
    out.push_str(&format!("ended   {}\n", m.finished_at));
    out.push_str(&format!("git     {}\n", m.git_sha.as_deref().unwrap_or("-")));
    out.push_str(&format!("dir     {}\n", run.dir.display()));

    if metrics || all {
        out.push_str("\nmetrics\n");
        match report_state(run) {
            ReportState::Read(serde_json::Value::Object(map)) => {
                for key in REPORT_KEY_ORDER {
                    if let Some(v) = map.get(key) {
                        out.push_str(&format!("  {key} = {v}\n"));
                    }
                }
                let mut extra: Vec<&String> =
                    map.keys().filter(|k| !REPORT_KEY_ORDER.contains(&k.as_str())).collect();
                extra.sort();
                for key in extra {
                    out.push_str(&format!("  {key} = {}\n", map[key]));
                }
            }
            // Valid JSON that is not an object — a producer wrote something else under the common
            // name. Not "no report", and not a parse failure either.
            ReportState::Read(other) => out.push_str(&format!(
                "  report.json is not an object — it holds {other}, which no reader can render as \
                 metrics\n"
            )),
            // The manifest is written LAST, so a run with no report is one that stopped before
            // finishing — it still SHOWS, under its manifest, rather than being refused.
            ReportState::Absent => out.push_str(
                "  no report — this run has no report.json on disk (it stopped before writing one)\n",
            ),
            // ⚠ ON DISK and unusable. Saying "no report" here would tell an operator their run did
            // not finish when the truth is that their file is corrupt or unreadable — two different
            // problems with two different fixes, and only one of them is about the run.
            ReportState::Unreadable(why) => out.push_str(&format!(
                "  report.json IS on disk and cannot be read ({why}) — fix its permissions; the run \
                 itself finished\n"
            )),
            ReportState::Unparseable(why) => out.push_str(&format!(
                "  report.json IS on disk and is not valid JSON ({why}) — open it; the run itself \
                 finished\n"
            )),
        }
    }

    if trades || all {
        out.push_str("\ntrades\n");
        match read_trades(&run.dir) {
            Ok(ledger) => {
                out.push_str(&format!(
                    "  {} closed, {} kept\n",
                    ledger.source_len,
                    ledger.trades.len()
                ));
                for t in &ledger.trades {
                    let side = if t.is_long { "long " } else { "short" };
                    let symbol = if t.symbol.is_empty() { "-" } else { t.symbol.as_str() };
                    out.push_str(&format!(
                        "  {side} {symbol} size={} entry={} exit={} pnl={} fees={}\n",
                        t.size, t.entry_price, t.exit_price, t.pnl, t.fees
                    ));
                }
            }
            Err(RunReadError::Missing { .. }) => out.push_str(
                "  no trades.json — this producer wrote no ledger (a study, or a run minted \
                 before the run artifact existed)\n",
            ),
            Err(e) => out.push_str(&format!("  unreadable: {e}\n")),
        }
    }

    if config || all {
        out.push_str("\nconfig\n");
        out.push_str(&format!("  path = {}\n", m.config.path.as_deref().unwrap_or("-")));
        out.push_str(&format!("  name = {}\n", m.config.name.as_deref().unwrap_or("-")));
        match read_config_toml(&run.dir) {
            Ok(text) => {
                out.push_str("  resolved profile:\n");
                for line in text.lines() {
                    out.push_str(&format!("    {line}\n"));
                }
            }
            // ⚠ SAY it. Printing a path under a heading that reads "resolved profile" would be
            // positive confirmation of something false.
            Err(_) => out.push_str(
                "  the resolved profile itself is not stored for this run — only the path and \
                 name above\n",
            ),
        }
        if !m.detail.is_null() {
            out.push_str(&format!(
                "  detail = {}\n",
                serde_json::to_string(&m.detail).unwrap_or_default()
            ));
        }
    }
    out
}

pub(crate) fn run_show(ctx: &Ctx<'_>, a: &ReadArgs) -> CmdResult<()> {
    let root = ctx.runs_root.ok_or_else(|| {
        CliError::failed(
            "no project directory above the working directory, so there is no runs directory to \
             read. `vike-cli init` creates one, or name it with VIKE_USER_DATA_DIR.",
        )
    })?;
    let scan = scan_runs(root);
    for problem in &scan.problems {
        eprintln!("vike-cli backtest show: {problem}");
    }
    let selector = a.selector.as_deref().unwrap_or_default();
    let run = resolve_one(&scan, ctx.marks_root, selector)?;

    // ⚠ `--export` is its own DOCUMENT and replaces the rendering entirely: it is the raw artifact,
    // not a view of it, which is what makes `--out` a file somebody's tooling can read. It composes
    // with `--out` and with nothing else here; a value it cannot serve is refused by NAME.
    let text = match &a.export {
        Some(spec) => export_document(run, spec)?,
        None if a.json => show_json(run, a.trades),
        None => show_text(run, a.metrics, a.trades, a.config),
    };
    match &a.out {
        Some(file) => std::fs::write(file, format!("{text}\n"))
            .map_err(|e| CliError::failed(format!("cannot write {file}: {e}")))?,
        None => print!("{text}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
        assert_eq!(UNBUILT_RENDERERS.len(), 4);
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

    /// ⚠ `--export` is refused per VALUE. `fills` is the one the record cannot serve; a LIST is
    /// refused because one `--out` writes one document; and the two stored values are served.
    #[test]
    fn export_refuses_the_value_it_cannot_serve_rather_than_the_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a-1-0");
        let run = seeded(&dir, None);
        std::fs::write(dir.join("trades.json"), r#"{"schema":1,"trades":[],"source_len":0}"#)
            .unwrap();

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
        let run =
            seeded(&tmp.path().join("a-1-0"), Some(r#"{"sharpe":1.25,"profit_factor":null}"#));

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
        assert_eq!(
            doc["trades"]["closed"],
            serde_json::json!(9),
            "the TRUE count, not the kept one"
        );
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
}
