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

use crate::cmd::backtest::ReadArgs;
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
/// there — `vike_report::render_html` is exported under no feature at all — and what `vike-cli`
/// lacked was a DEPENDENCY EDGE. Promoting it took a manifest row, a render path, a second
/// document guard and the three gates that row reddened. So a row here means one of three things
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
///   no `stride` here to test. `crate::cmd::report_stored`'s `breakdown_of` is where the condition
///   is actually EVALUATED, off the same `series.json`, refusing only the runs that really were
///   thinned. So the arm now names that verb — which is what the `--drawdowns` arm has always done
///   for `--export equity`, and the convention this one was the only arm to omit.
/// * **`--html` claimed "a tearsheet renderer, which nothing in this tree has built".**
///   `crates/vike-report/src/html.rs`'s `render_html` is a complete one and is exported under no
///   feature at all; `crates/vike-report/Cargo.toml`'s `journal` feature says so in its own words,
///   calling this sentence "measurably false". The obstacle is a DEPENDENCY, not a renderer: this
///   crate declares no vike-report edge, so this binary does not link it. ⚠ **A declared residual:
///   nothing fails if somebody adds that edge**, so this arm and that manifest row have to move
///   together — the arm is what a PR adding the edge must fix in the same change.
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
/// ⚠ `pub(crate)` because a SECOND verb re-renders the same file: `crate::cmd::report_stored` builds
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
/// edge is the whole of what this change buys. Every other piece was already built and was built
/// FOR THIS: `vike_report::render_html` is exported under no feature at all, and
/// `vike_report::LiveTearsheet::from_report`'s own doc names this file by path — *"holds a
/// `report.json` and wants the HTML tearsheet; this is the whole conversion, and it needs no
/// journal, no store and no equity curve, which is why it compiles without the `journal`
/// feature."* So there is no conversion to write here, only a call.
///
/// ⚠ The edge is `default-features = false`, which is what makes it cheap enough for this crate to
/// take at all: `crates/vike-cli/Cargo.toml`'s row carries the measurement (ONE new package in the
/// normal closure, the path crate itself, and no external crate — the lockfile delta was a single
/// line), and `scripts/ci_feature_suite.sh`'s `report-journal-off` arm is what proves that
/// configuration builds and keeps the vike-core/vike-exec/vike-data closure out.
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
    let sheet = vike_report::LiveTearsheet::from_report(&report);
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
    Ok(vike_report::render_html(&sheet, &equity, &equity_ts))
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
    /// wrong answer on top of the first, and a third bucket in `crate::cmd::report_schema`'s
    /// `PERIODS` is published in the JSON Schema `enum` and in `crate::cmd::report`'s `USAGE`
    /// without anything reaching this sentence.
    #[test]
    fn the_breakdown_refusal_names_the_verb_that_answers_and_every_period_it_takes() {
        let msg = refuse_an_unbuilt_renderer("--breakdown");
        assert!(msg.contains("vike-cli report"), "it must name the verb that answers: {msg}");
        for period in crate::cmd::report_schema::PERIODS {
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
        let mut a = ReadArgs::empty(crate::cmd::backtest::ReadSub::Show);
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
        let base = ReadArgs::empty(crate::cmd::backtest::ReadSub::Show);
        // The negative control FIRST: with the listing off, nothing here is refused.
        let mut off = ReadArgs::empty(crate::cmd::backtest::ReadSub::Show);
        off.metrics = true;
        off.json = true;
        assert!(refuse_a_listing_beside_a_run_rendering(&off).is_ok(), "the guard is opt-in");

        for (flag, _) in run_rendering_flags_given(&base) {
            let mut a = ReadArgs::empty(crate::cmd::backtest::ReadSub::Show);
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
        let mut a = ReadArgs::empty(crate::cmd::backtest::ReadSub::Show);
        a.metrics_list = true;
        a.json = true;
        let e = refuse_a_listing_beside_a_run_rendering(&a).expect_err("no JSON rendering exists");
        assert!(e.contains("--json") && e.contains("cli.json"), "{e}");

        // …and `--out` is NOT refused: it names a file, not a document.
        let mut a = ReadArgs::empty(crate::cmd::backtest::ReadSub::Show);
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
    /// without a chart, which `vike_report::render_html` decides for itself by refusing to draw a
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
        let mut a = ReadArgs::empty(crate::cmd::backtest::ReadSub::Show);
        a.html = true;
        assert!(refuse_a_second_document(&a).is_ok(), "--html alone is the shipping case");

        for (flag, set) in [
            ("--metrics", (|a: &mut ReadArgs| a.metrics = true) as fn(&mut ReadArgs)),
            ("--trades", |a: &mut ReadArgs| a.trades = true),
            ("--config", |a: &mut ReadArgs| a.config = true),
            ("--export", |a: &mut ReadArgs| a.export = Some("trades".to_string())),
        ] {
            let mut a = ReadArgs::empty(crate::cmd::backtest::ReadSub::Show);
            a.html = true;
            set(&mut a);
            let e = refuse_a_second_document(&a).expect_err("two documents");
            assert!(e.contains("--html"), "{flag}: {e}");
            assert!(e.contains(flag), "{flag}: …and the flag it collided with: {e}");
            assert!(e.contains("two different documents"), "{flag}: …and WHY neither wins: {e}");
        }

        // `--json` is its own sentence: two SERIALIZATIONS rather than a document beside a section.
        let mut a = ReadArgs::empty(crate::cmd::backtest::ReadSub::Show);
        a.html = true;
        a.json = true;
        let e = refuse_a_second_document(&a).expect_err("two serializations");
        assert!(e.contains("--html") && e.contains("--json"), "{e}");
        assert!(e.contains("SERIALIZATIONS"), "…and says what kind of collision it is: {e}");

        // ...and `--out` COMPOSES, exactly as it does for --export and --metrics-list.
        let mut a = ReadArgs::empty(crate::cmd::backtest::ReadSub::Show);
        a.html = true;
        a.out = Some("sheet.html".to_string());
        assert!(refuse_a_second_document(&a).is_ok(), "--out names a file, not a document");
    }
}
