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
//! * **`--drawdowns N`, `--breakdown`, `--attribution` and `--html` stay refused** — and their
//!   reasons have now been rewritten TWICE. The first pass moved `--drawdowns` off "the curve is
//!   missing" once `series.json` existed. ⚠ **The second found that three of the four sentences
//!   this bullet summarised no longer held** — two of them flatly false against the tree they
//!   describe, the third true of a condition it never tests — and the record of what changed lives
//!   on [`refuse_an_unbuilt_renderer`] rather than here: this paragraph was a second copy of that
//!   function's reasons, and a second copy is precisely what rotted. Only `--attribution` is still
//!   refused for the reason it was first given.
//!
//! They are refused BY NAME rather than falling into "unknown option", because an operator typing
//! one is reading them off the design document: "unknown option --export" says the flag does not
//! exist, when the truth is that some of the data does not. [`refuse_an_unbuilt_renderer`] owns
//! every sentence, and `crate::cmd::backtest`'s `parse_read` is its only caller.
//!
//! # ⚠ `--metrics-list` is the one flag here that is not about a run at all
//!
//! Every other flag on this verb narrows, renders or exports ONE STORED RUN. `--metrics-list`
//! prints `vike_analytics::metric_catalog::metric_list_text` — the metric TAXONOMY, every id with
//! where it is stored and what it measures, plus the declared-absent rows — and answers "what can
//! I ask for". So it takes no selector, reads no runs directory and returns from the TOP of
//! [`run_show`], above the `runs_root` requirement: a listing that refused on a box with no project
//! above the working directory would refuse on exactly the box where somebody is deciding what to
//! type.
//!
//! It exists because three refusals in that module NAME it as a command to run, and until it
//! shipped every one of them handed an operator a line that exited "unknown option" —
//! `metric_catalog::parse_metric_selection`'s own doc measured that and called it the trap. What it
//! is NOT is the selection half: `--metrics` is still a bare switch, and that function's doc
//! carries why widening it is a separate change. [`refuse_a_listing_beside_a_run_rendering`] keeps
//! the two from being asked for at once, because the listing short-circuits and a section flag
//! beside it would silently do nothing.
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

use crate::cmd::backtest::read::ReadArgs;
use crate::cmd::runs::scan::{ScannedRun, scan_runs};
use crate::cmd::runs::{Ctx, selector::resolve_one};
use crate::exit::{CliError, CmdResult};

/// The §6.2 renderers this verb does not serve. One roster, named in one place, and every entry
/// carries the reason it is here in [`refuse_an_unbuilt_renderer`] rather than in prose.
///
/// ⚠ THREE flags that were on it are not any more, and the third left for a DIFFERENT reason than
/// the first two, which is the fact this doc keeps getting wrong. `--trades` and `--export` left
/// because the run record grew what they needed. `--trades` renders
/// `vike_model::runs::TRADES_FILE`, and `--export` serves
/// its per-VALUE subset through [`export_document`] — a whole-flag refusal over one unavailable
/// VALUE was the wrong shape beside `--drawdowns` and `--breakdown`, which are refused narrowly.
///
/// ⚠ **This doc-comment said "cannot build", and for two of the four rows that is no longer what
/// the row means.** The roster's entries used to be claims about the ARTIFACT — a renderer waiting
/// on data the record did not keep — which is what made "when the record grows what one of them
/// needs, promoting it is one edit in one file" true, and `--trades`/`--export` are the two that
/// left exactly that way.
///
/// ⚠ **`--html` then left for a THIRD reason and cost more than one edit**, which is why the
/// sentence above is now history rather than a rule. Its data and its renderer were BOTH already
/// there — the renderer was exported under no feature at all (it was `vike_report::render_html`
/// then, and is `vike_analytics::render_html` since it moved) — and what `vike-cli` lacked was a
/// DEPENDENCY EDGE. Promoting it took a manifest row, a render path, a second document guard and
/// the three gates that row reddened. So a row here means one of three things
/// now, and [`refuse_an_unbuilt_renderer`] is where each says which: `--attribution` is a
/// renderer nobody has written, `--drawdowns` has its data AND its renderer and wants WIRING, and
/// `--breakdown` is conditional on a run this guard cannot resolve because it reads argv alone.
pub(crate) const UNBUILT_RENDERERS: [&str; 3] = ["--breakdown", "--attribution", "--drawdowns"];

/// The sentence each one is refused with. `crate::cmd::backtest`'s `parse_read` is the only caller.
///
/// # ⚠ Three of the four rows this once held did not hold — two flatly false, one true of a
/// condition it never tests
///
/// ⚠ The roster is THREE rows now: `--html` was one of the two flatly-false ones and has shipped,
/// so what follows is the record of what each claim cost, and only the `--drawdowns` and
/// `--breakdown` halves still describe a live refusal.
///
/// Each was written true and then rotted as the tree grew under it — the failure mode this crate's
/// rosters are shaped to prevent, and a refusal is the worst place for it, because the operator it
/// lands on has no way to check it. What each claimed, and what is true now:
///
/// * **`--drawdowns` claimed "a drawdown-table renderer, which nothing in this tree has built".**
///   `crates/vike-analytics/src/periods.rs`'s `drawdown_table_text` IS that renderer — an aligned
///   table, with a `still open`/`n/a` convention for an episode the curve never recovered from —
///   and it reaches this binary through the non-optional vike-analytics edge
///   `crates/vike-cli/Cargo.toml` argues. The old sentence's ARGUMENT survives unchanged and was
///   the right shape of answer ("this is the renderer that is missing rather than the data"); only
///   the fact moved, and what is missing now is the WIRING — a `--drawdowns N` value on
///   `crate::cmd::backtest`'s `ReadArgs` and a call from [`run_show`].
///   [`tests::the_drawdown_renderer_this_refusal_names_is_linked_and_callable_here`] CALLS that
///   renderer from inside this crate, so the old claim cannot return without reddening a test.
/// * **`--breakdown`'s reason is CONDITIONAL while this refusal is not.** The decimation sentence
///   is exact and still the right reason to refuse — but it holds only for a run past
///   `vike_model::runs::MAX_EQUITY_SAMPLES`, and this guard sits inside
///   `crate::cmd::backtest`'s `parse_read`, which sees argv alone: no run is resolved, so there is
///   no `stride` here to test. `crate::cmd::report::stored`'s `breakdown_of` is where the condition
///   is actually EVALUATED, off the same `series.json`, refusing only the runs that really were
///   thinned. So the arm now names that verb — which is what the `--drawdowns` arm has always done
///   for `--export equity`, and the convention this one was the only arm to omit.
/// * **`--html` claimed "a tearsheet renderer, which nothing in this tree has built".**
///   `crates/vike-analytics/src/html.rs`'s `render_html` is a complete one and is exported under
///   no feature at all (it lived in vike-report when this bullet was written, and vike-report's
///   old `journal` feature called this sentence "measurably false" in its own words). The obstacle
///   was a DEPENDENCY, not a renderer: this crate declared no edge onto the renderer's crate, so
///   this binary did not link it. That edge landed — first onto vike-report, and since
///   2026-09-28 as this crate's existing vike-analytics edge, which the renderer moved into — and
///   the `--html` row left the roster with it.
pub(crate) fn refuse_an_unbuilt_renderer(flag: &str) -> String {
    let needs = match flag {
        "--drawdowns" => {
            "its WIRING here, not a renderer: `vike_analytics::periods::drawdown_table` folds the \
             episodes and `drawdown_table_text` renders the table, both linked into this binary, \
             and the EQUITY CURVE they read is in the run record (series.json). What is missing is \
             a `--drawdowns N` value on this verb and a call to them. `--export equity` hands you \
             the curve today, and `vike-cli report <run>` recomputes its single WORST drawdown"
        }
        "--breakdown" => {
            "the PER-BAR RETURNS, and a run to test the reason against. The stored equity curve is \
             DECIMATED once a run exceeds the sample cap (series.json's `stride` above 1 means \
             samples were dropped), so a period breakdown computed from it would disagree with \
             report.json's own numbers — but this guard reads the command line alone, before any \
             run is resolved, so it cannot tell whether YOUR run was thinned. `vike-cli report \
             <run> --breakdown day|month` computes the breakdown from the same series.json and \
             refuses only the runs that really were"
        }
        "--attribution" => "a per-source attribution the engine does not compute at all",
        other => return format!("unknown option '{other}'"),
    };
    format!(
        "{flag} is not available yet — it needs {needs}. What `show` can render today: --metrics, \
         --trades, --config, --export trades|equity, --json, --out."
    )
}

/// The `show` flags that render or select some part of ONE STORED RUN, each paired with whether
/// this command line gave it — the set `--metrics-list` cannot be combined with, because it answers
/// about the metric CATALOG instead and there is no reading under which both are honoured.
///
/// ⚠ **ONE home, and it is a function rather than a const because the ANSWER is per-command-line.**
/// [`refuse_a_listing_beside_a_run_rendering`] walks it to NAME the flag that collided, and
/// `the_listing_refuses_every_run_rendering_flag_by_name` walks the same array to drive each one
/// through the real parser — so a fifth section flag added here without a test row fails that
/// test's length agreement rather than going unrefused. A hand-written chain of `||` in the refusal
/// would answer "these are incompatible" without saying which flag it saw.
///
/// ⚠ `--out` is deliberately ABSENT: it names WHERE a rendered document is written, not WHAT is
/// rendered, so `show --metrics-list --out catalog.txt` is a coherent request and is served. So is
/// a selector — see `crate::cmd::backtest`'s `parse_read`, which keeps one and does not use it.
/// `--json` is absent too and is refused SEPARATELY, for a different reason the function states.
pub(crate) fn run_rendering_flags_given(a: &ReadArgs) -> [(&'static str, bool); 5] {
    [
        ("--metrics", a.metrics),
        ("--trades", a.trades),
        ("--config", a.config),
        ("--export", a.export.is_some()),
        ("--html", a.html),
    ]
}

/// **Refuse `--html` beside anything else that decides what is rendered.**
///
/// ⚠ **The hole this closes is one this file has already fallen into once.** Promoting
/// `--trades` out of [`UNBUILT_RENDERERS`] without giving it a JSON half left a `--trades
/// --json` that changed only the text renderer and answered `jq '.trades'` with `null` on exit
/// `0` — indistinguishable from a run whose producer wrote none. The lesson is not about JSON:
/// a promoted renderer owes an answer for every OTHER document flag, and the answer that is
/// never right is SILENCE.
///
/// So `--html` is refused beside each of [`run_rendering_flags_given`]'s other entries — a
/// section flag would be ignored, because the HTML document is not assembled from sections —
/// and beside `--json`, which is a second SERIALIZATION of the same run. `--out` composes,
/// exactly as it does for `--export` and `--metrics-list`: it names a file, not a document.
/// `--metrics-list` beside `--html` is caught by the sibling rule below rather than here, and
/// that is why `--html` is an entry in that shared array rather than a special case beside it.
pub(crate) fn refuse_a_second_document(a: &ReadArgs) -> Result<(), String> {
    if !a.html {
        return Ok(());
    }
    let other = run_rendering_flags_given(a)
        .into_iter()
        .find(|(flag, present)| *present && *flag != "--html");
    if let Some((flag, _)) = other {
        return Err(format!(
            "--html and {flag} ask for two different documents: --html renders this run's whole \n             tearsheet as one HTML page, and {flag} selects or exports part of it. Which one \n             wins would be a guess, so neither does — run `backtest show <run> --html --out \n             sheet.html`, then `backtest show <run> {flag}`."
        ));
    }
    if a.json {
        return Err(
            "--html and --json are two SERIALIZATIONS of the same run, and this verb writes one \n             document: --html is the rendered tearsheet a human opens, --json is the machine \n             shape (`{run_id, dir, manifest, report}`). Run it twice, or take the JSON and \n             render it elsewhere — every number in the HTML comes from the same report.json \n             --json prints."
                .to_string(),
        );
    }
    Ok(())
}

/// Refuse `--metrics-list` beside a flag that renders the RUN, naming what each flag does.
///
/// ⚠ **The alternative was a silent precedence rule, and that is the defect this avoids.** The
/// listing short-circuits in [`run_show`] before a run is resolved, so a combination would have
/// made `--metrics-list` win and the section flag do nothing at all — a DIFFERENT ANSWER rather
/// than a refusal, on a verb whose whole product is a rendering. Which of the two should win is
/// genuinely a guess: both readings are defensible, which is the same reason
/// `vike_analytics::metric_catalog::parse_metric_selection` refuses a keyword mixed with ids.
///
/// ⚠ `--json` is refused too and is a different case, so it gets its own sentence: the listing has
/// no JSON rendering at all. `metric_list_text` is an aligned human table and nothing in this tree
/// serializes the catalog — and per the convention `refuse_an_unbuilt_renderer`'s `--drawdowns` arm
/// follows, the message names what DOES answer the machine-readable version of the question: the
/// published `cli.json` asset, whose `rosters` carry the metric ids `--metrics` accepts.
pub(crate) fn refuse_a_listing_beside_a_run_rendering(a: &ReadArgs) -> Result<(), String> {
    if !a.metrics_list {
        return Ok(());
    }
    let rendering = run_rendering_flags_given(a);
    if let Some((flag, _)) = rendering.iter().find(|(_, present)| *present) {
        return Err(format!(
            "--metrics-list and {flag} ask for two different documents: the first lists what this \
             tree CAN measure (the catalog, with no run involved at all), the second renders part \
             of one stored run. Which of the two wins would be a guess, so neither does — run \
             `backtest show --metrics-list` on its own, then `backtest show <run> {flag}`."
        ));
    }
    if a.json {
        return Err(
            "--metrics-list has no --json rendering: it is an aligned table for a human, and \
             nothing in this tree serializes the metric catalog. For the machine-readable answer \
             use the published surface asset — `vike-cli surface --out DIR` writes cli.json, whose \
             `rosters` carry the metric ids. `backtest show --metrics-list` prints the table."
                .to_string(),
        );
    }
    Ok(())
}

/// `--export`'s values, and which of the three the run record can actually serve.
///
/// ⚠ **The refusal is per-VALUE, not per-flag, and that is the correction.** `--export` was refused
/// whole because ONE of its three values is unavailable — which is inconsistent with its two
/// neighbours, both refused for narrow, per-flag reasons (`--drawdowns` for an UNWIRED renderer,
/// `--breakdown` for a decimated curve). ⚠ That first parenthesis read "a missing renderer" until
/// the renderer was found sitting in vike-analytics with no caller; [`refuse_an_unbuilt_renderer`]
/// carries the correction, and what this sentence is arguing — that a NARROW reason is the right
/// shape — is untouched by which narrow reason it turns out to be. `trades` and `equity` are FULLY
/// stored (`vike_model::runs::TRADES_FILE` / `SERIES_FILE`), each is a single document, and each
/// writes through the `--out` this verb already has — so shipping them invents no grammar at all.
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
///
/// ⚠ `pub(crate)` because a SECOND verb re-renders the same file: `crate::cmd::report::stored` builds
/// its stored-run tearsheet on this classification rather than re-wording it, so the two verbs
/// cannot print different sentences about one missing report — which is exactly how an operator
/// comes to believe a run did not finish when its report is merely corrupt.
pub(crate) enum ReportState {
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
pub(crate) fn report_state(run: &ScannedRun) -> ReportState {
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
    pub(crate) fn to_json(&self) -> serde_json::Value {
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
pub(crate) fn trades_json(run: &ScannedRun) -> serde_json::Value {
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

/// `report.json`'s keys in render order, with any key not named here printed after it,
/// alphabetically. A `serde_json::Map` iterates in insertion order only with the `preserve_order`
/// feature, which this workspace does not enable; without a declared order the block would reorder
/// itself between serde versions.
///
/// # ⚠ The eight metric keys are DERIVED now, and that is the whole change
///
/// This was `REPORT_KEY_ORDER`, an eleven-entry array, and `vike_analytics::metric_catalog`'s own
/// module doc named it as one of the FOUR hand copies of the metric roster that rotted — the
/// fourth to be deleted, after that crate's HTML row vector, its finiteness test and the GUI
/// tearsheet panel's. Eight of those eleven entries are exactly the catalog's `MetricHome::Compact`
/// ids in the catalog's own declaration order, so they come from
/// `vike_analytics::metric_catalog::MetricSelection::Compact` here and a compact metric added
/// upstream reaches this block with no edit at all.
///
/// The three that remain are spelled locally because they are NOT metrics and will never be rows
/// of that table: `name` is the profile's own label, and `per_symbol_pnl`/`zero_trade` are
/// structural blocks of the document. Keys the order does not name — `extended`, `honesty`,
/// `realism` — still print after it, alphabetically, exactly as before.
fn report_key_order() -> Vec<&'static str> {
    let mut keys = vec!["name"];
    keys.extend(vike_analytics::metric_catalog::MetricSelection::Compact.ids());
    keys.extend(["per_symbol_pnl", "zero_trade"]);
    keys
}

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
                let order = report_key_order();
                for key in &order {
                    if let Some(v) = map.get(*key) {
                        out.push_str(&format!("  {key} = {v}\n"));
                    }
                }
                let mut extra: Vec<&String> =
                    map.keys().filter(|k| !order.contains(&k.as_str())).collect();
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

/// **`--html`: this run's tearsheet as a standalone HTML document.** The promotion that closes the
/// third of the four `UNBUILT_RENDERERS` rows, and the one whose old refusal was most exactly
/// false.
///
/// # What was actually missing, and it was never the renderer
///
/// That row's sentence said `--html` "needs a vike-report EDGE, not a renderer" — correct, and the
/// edge was the whole of what this change bought. Every other piece was already built and was built
/// FOR THIS: `vike_analytics::render_html` is exported under no feature at all, and
/// `vike_analytics::LiveTearsheet::from_report`'s own doc names this file by path as the caller
/// that "holds a `report.json` and wants the HTML tearsheet; this is the whole conversion, and it
/// needs no journal, no store and no equity curve". So there is no conversion to write here, only
/// a call.
///
/// ⚠ **Both lived in vike-report when this function was written, and the edge that made them
/// cheap enough to take is GONE.** It was `vike-report = { default-features = false }`, with a CI
/// lane of its own proving that feature-off build kept the vike-journal/vike-exec/vike-data closure
/// out of this crate. On 2026-09-28 the renderer and the document moved into vike-analytics — an
/// edge this crate already had, for the metric catalog — so the vike-report edge, its feature and
/// that lane were deleted, and the two calls below name vike-analytics' crate root. The closure is
/// now kept out structurally rather than by a feature: `crates/vike-cli/Cargo.toml`'s
/// vike-analytics row carries the argument.
///
/// # The equity curve is OPTIONAL, and the renderer already owns that decision
///
/// `render_html` draws the chart only when the curve has at least two points and its timestamps
/// agree in length, so a run with no `series.json` renders the metrics table and no chart. That is
/// the honest document for such a run rather than an error —
/// `vike_model::runs::read_series`'s own doc fixes `Missing` as an ORDINARY answer, unlike for the
/// manifest — and the absence is reported on stderr so nobody mistakes a chartless tearsheet for a
/// flat account.
///
/// ⚠ **A DECIMATED curve is drawn as it is stored and is not corrected for.** Past
/// `vike_model::runs::MAX_EQUITY_SAMPLES` the stored series is thinned (`stride` above 1), so the
/// chart is a picture of the thinned curve. That is a different claim from `--breakdown`'s, which
/// is refused because a STATISTIC recomputed off a thinned curve would disagree with
/// `report.json`'s own numbers: every number in this document comes from `report.json`, so none of
/// them is recomputed and none can disagree. Only the drawing is coarser, and `series.json` records
/// the stride for anyone who needs to know by how much.
fn html_document(run: &ScannedRun) -> CmdResult<String> {
    let Some(path) = &run.report else {
        return Err(CliError::failed(
            "--html needs this run's report.json and it has none — the run stopped before writing \
             one (the manifest is written LAST). `backtest show` without --html still renders \
             everything the manifest holds.",
        ));
    };
    let text = std::fs::read_to_string(path)
        .map_err(|e| CliError::failed(format!("--html cannot read {}: {e}", path.display())))?;
    let report: vike_analytics::report::BacktestReport =
        serde_json::from_str(&text).map_err(|e| {
            CliError::failed(format!(
                "--html cannot parse {} as a backtest report ({e}) — the file IS on disk and is \
                 not one. `backtest show --json` prints what it holds so the fault can be seen.",
                path.display()
            ))
        })?;
    let sheet = vike_analytics::LiveTearsheet::from_report(&report);
    let (equity, equity_ts) = match vike_model::runs::read_series(&run.dir) {
        Ok(series) => (series.equity, series.equity_ts),
        Err(_) => {
            eprintln!(
                "vike-cli backtest show: this run stored no equity series, so the tearsheet \
                 carries its metrics and no chart"
            );
            (Vec::new(), Vec::new())
        }
    };
    Ok(vike_analytics::render_html(&sheet, &equity, &equity_ts))
}

pub(crate) fn run_show(ctx: &Ctx<'_>, a: &ReadArgs) -> CmdResult<()> {
    // ⚠ **FIRST, above the runs-root requirement and above the scan — which is the whole point of
    // the flag.** `vike_analytics::metric_catalog::metric_list_text` answers "what can I ask for"
    // out of `METRICS` and `ABSENT`, so it needs no run, no runs directory and no project above the
    // working directory. Putting it below that `ok_or_else` would make the listing refuse on
    // exactly the box where somebody is deciding what to type, with a message about a directory
    // they were not asking about.
    //
    // ⚠ It is the command `parse_metric_selection`'s three refusals name, and until this arm
    // existed every one of them handed an operator a line that exited "unknown option" — a message
    // about the TOOL being broken, delivered on the rung where the operator is being corrected.
    // That function's doc carried the measurement; this is the half of it that is now false.
    //
    // ⚠ `--out` composes (it names a file, not a document) and every other rendering flag is
    // already REFUSED beside this one by [`refuse_a_listing_beside_a_run_rendering`] at parse time,
    // so there is no combination left for this early return to swallow.
    if a.metrics_list {
        let text = vike_analytics::metric_catalog::metric_list_text();
        return match &a.out {
            Some(file) => std::fs::write(file, &text)
                .map_err(|e| CliError::failed(format!("cannot write {file}: {e}"))),
            None => {
                print!("{text}");
                Ok(())
            }
        };
    }
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
    // ⚠ `--html` sits ABOVE `--export` in this selection and both are DOCUMENTS, which is why
    // the two are refused together at parse time rather than ordered here: an order would make
    // one of them silently win. Same for `--json` — the `--trades` promotion out of
    // `UNBUILT_RENDERERS` created exactly that hole once (a `--trades --json` that changed only
    // the text renderer answered `jq .trades` with `null` on exit 0), and the lesson is that a
    // promoted renderer owes an answer for every OTHER document flag, not just its own.
    let text = match &a.export {
        _ if a.html => html_document(run)?,
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

#[path = "show_tests.rs"]
#[cfg(test)]
mod show_tests;
