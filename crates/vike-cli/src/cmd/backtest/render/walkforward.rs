//! The walk-forward REPORT RENDERER — what `vike-cli backtest run` prints when the profile
//! declares a window.
//!
//! ⚠ **This was a top-level VERB until decision 3 of
//! `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`.** Walking forward is how a
//! run is VALIDATED rather than a different kind of run (ruling 7), so it folded into
//! `crate::cmd::backtest`'s `run`, where the PROFILE selects it (`route_of`) exactly as a `[sweep]`
//! grid already selected a search — and declaring both COMPOSES rather than competing. What
//! survives here is the half that had nothing to do with the verb: the renderer for the server's
//! `WalkforwardResults` (`vike_analytics::walkforward`'s: every step's results, the walk's
//! `overall` results, the stitched `equity_curve` and the `profitable_step_share`). The client, the
//! address ladder and the argv parsing went; the ONE caller is `crate::cmd::backtest`'s `execute`.
//!
//! ⚠ **The module is not deleted**, deliberately. It moved under `backtest/render/` (the stage-5 layout
//! change, every path-keyed row and citation repaired in the same change — the #1754 both-directions
//! failure the root `CLAUDE.md`'s file-move bullet and `crates/vike-ops/tests/hygiene/path_key_gate.rs`
//! exist for). It is `pub(crate)` now: nothing outside this crate may reach it, and nothing inside it
//! but `backtest` does.
//!
//! # ⚠ There are TWO walk-forward protocols, and this side has no flag for either
//!
//! `[walkforward].n_splits` alone is the FIXED-parameter walk: every window trades the profile's
//! own `[strategy.params]` and asks whether those settings were stable out of sample. Adding
//! `search = "sweep"` to that table asks the other question — each window re-searches the profile's
//! `[sweep]` grid on its OWN training half and trades only that window's winner, so what is being
//! validated is the PROCEDURE of fit-then-trade rather than one parameter set. `mode`
//! (`anchored` | `rolling`) picks the training shape and `rank_by` picks how a window scores its
//! candidates; `crates/vike-backtest/src/run/profile/walkforward_config.rs`'s `ProfileWalkforward` is
//! the authority for every spelling, and an unrecognized one fails server-side at load naming the
//! valid set.
//!
//! ⚠ **`search` is the selector TODAY and ruling 4 of the 2026-09-13 owner rulings deletes it** —
//! after that, whether a window re-searches is whether the profile declares a grid at all. The
//! deletion is the walk-forward stage's work and has NOT landed:
//! `ProfileBacktest::window_search` still reads the key, which is why this paragraph describes it
//! rather than the rule that replaces it. Either way it is a PROFILE fact and never a flag —
//! `crate::cmd::backtest`'s `refuse_a_walkforward_flag` refuses `--rank-by`/`--optimizer` on this
//! route by name for exactly that reason.
//!
//! ⚠ `mode` is inert without `search`, and that is a property of the protocol rather than a wiring
//! gap: the two modes differ ONLY in where a window's TRAINING half starts, and the fixed walk
//! discards the training half. Setting it alone parses, runs and returns the same numbers
//! (`crates/vike-backtest/src/walkforward/runner_tests.rs`'s
//! `both_walk_modes_leave_the_fixed_walk_report_identical` pins that), so do not read a `rolling`
//! line in a profile as evidence that anything trained differently.
//!
//! **None of that is a flag, deliberately.** This side parses the profile only far enough to pick a
//! CARRIER (`crate::cmd::backtest`'s `route_of`) — the file's text is shipped whole and the server
//! runs the one `ProfileBacktest::from_toml_str` — so an `--optimize` flag would be a second place
//! to say what the profile already says, and the two could then disagree with nothing to arbitrate.
//! `crates/vike-datahub/src/server/features.rs`'s `run_walkforward_profile` picks the driver from the
//! profile and from nothing else.
//!
//! The consequence for reading the output: this side cannot say which protocol ran by inspecting
//! what it sent, and does not try to. It learns it from the ANSWER — a window that searched carries
//! the parameters it chose, which the table below renders in a `chosen` column that a fixed-
//! parameter walk does not print at all. That column's absence is the honest signal that no search
//! happened; it is not a rendering option.
//!
//! # ⚠ There is deliberately NO `--local` for a walk-forward
//!
//! A plain backtest grew one because the standalone engine can do what it asks: it runs a profile,
//! and a profile with a `[sweep]` table is the same run with its grid expanded. **The engine has no
//! walk-forward mode at all** — `crates/vike-backtesthub/src/backtest_cli.rs` has one profile path,
//! which branches on `ProfileBacktest::is_paramscan` and nothing else, so NEITHER driver
//! (`vike_backtest::walkforward::runner::run_walkforward` or its optimizing sibling) is reachable from any
//! binary; both run from the datahub server alone. A `--local` here would have nothing to spawn.
//!
//! ⚠ **The refusal MOVED with the fold and is now a property of the PROFILE, not of a verb.** It
//! used to be that `--local` was simply an unknown argument on this file's parser; it is now
//! `crate::refusals`'s `LocalHasNoWalkforward`, raised when the resolved profile
//! declares a `[walkforward]` table. That is a smaller lie than a flag that exists and always
//! fails, and this paragraph is where the absence is a decision rather than an oversight. What
//! would change it: a walk-forward entry point on the standalone engine — at which point
//! `backtest run --local` simply stops refusing.
//!
//! Like the parameter-search sibling — the flags `crate::cmd::backtest` absorbed when ruling 13
//! deleted the `sweep` verb — this replaced a client-side profile→DTO mapping whose
//! `DataSliceWire`/`StrategyEngineConfigWire` pair could not carry most of the `[engine]` surface. The
//! server now parses the profile with the ONE
//! `ProfileBacktest::from_toml_str` parser, and the walk-forward honors the whole `[engine]`. The
//! Studio's DTO-shaped `RunWalkforward` wire verb is untouched and still serves the GUI.
//!
//! Server-side this is BAR mode over ONE series (the splitter divides a single bar series by index);
//! a tick or multi-symbol profile is a clean error, never a silent first-symbol fallback.

use serde_json::Value;
use vike_analytics::metric_catalog::{MetricUnit, spec_for};

/// Print the server's `WalkforwardResults` as a human table: one row per OOS step plus the summary
/// line. Every number is the server's own — nothing is recomputed here.
///
/// # ⚠ The scaling and the precision are the UNIT's, never this file's
///
/// [`MetricUnit::render`] is *"the ONE home for the scaling and the precision"*, and its doc names
/// what forgetting a `* 100.0` costs: a drawdown of three percent published as `0.0310`, read as
/// three basis points. A step's and the walk's `total_return` and the walk's `sharpe` are
/// `vike_analytics::metric_catalog::METRICS` ids, so they render through their own rows' units
/// ([`catalog_unit`]): this table and a `vike-cli report` tearsheet cannot disagree about one id.
/// `profitable_step_share` is not a catalog id, so its unit is named outright, the shape
/// `crates/vike-analytics/src/periods.rs`'s `drawdown_table_text` uses for a non-catalog number;
/// `the_rendered_numbers_come_from_their_unit_and_not_from_a_local_scaling` goes red the day it
/// gains a row.
///
/// ⚠ A positive return carries no explicit `+`: a negative number renders its own `-`. Column WIDTH
/// and alignment stay here, which is the half [`MetricUnit::render`]'s doc says it does not own.
///
/// The `chosen` column appears only when at least one step carries `chosen_params`, i.e. only for
/// a walk whose `[walkforward].search` actually searched. That is not a tidiness rule: the fixed
/// walk and the optimizing walk return the SAME results type, this side parses no TOML and so cannot
/// know which one it asked for, and the per-step winners are the only thing in the answer that
/// distinguishes them. Printing an empty column on the fixed walk would make the two look like one
/// protocol with a blank field; omitting it keeps the two outputs visibly different.
pub(crate) fn print_walkforward(report: &Value) {
    let steps = report.get("steps").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    println!("walk-forward: {} out-of-sample window(s)", steps.len());
    if let Some(line) = ledger_line(report) {
        println!("{line}");
    }
    for line in data_warning_lines(report) {
        println!("{line}");
    }
    if let Some(line) = strategy_failure_line(report) {
        println!("{line}");
    }
    // `chosen_params` is `skip_serializing_if = "Option::is_none"` on the server's
    // `WalkforwardResultsStep`, so the FIELD's presence — not its emptiness — is what says a step searched.
    let searched = steps.iter().any(|w| w.get("chosen_params").is_some());
    if searched {
        println!("{:>6}  {:<20}  {:>11}  chosen", "window", "test_range", "total_return");
    } else {
        println!("{:>6}  {:<20}  {:>11}", "window", "test_range", "total_return");
    }
    for (i, w) in steps.iter().enumerate() {
        let row = window_row(i, w);
        if searched {
            println!("{row}  {}", chosen(w));
        } else {
            println!("{row}");
        }
    }
    println!();
    println!("{}", summary_line(report));
}

/// Print the server's `CombinatorialWalkforwardReport` (`[walkforward]` form (d)): the cut, then one
/// summary row per PATH. Each path is a whole walk-forward over every group, rendered by the same
/// [`summary_line`] a plain walk ends on; its steps and chosen parameters are the `--json`
/// reader's.
pub(crate) fn print_combinatorial_walkforward(report: &Value) {
    for line in combinatorial_lines(report) {
        println!("{line}");
    }
}

/// [`print_combinatorial_walkforward`]'s lines, returned so the rendering is assertable.
fn combinatorial_lines(report: &Value) -> Vec<String> {
    let paths = report.get("paths").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    let count = |key: &str| report.get(key).and_then(Value::as_u64).unwrap_or(0);
    let mut lines = vec![format!(
        "combinatorial walk-forward: {} group(s), {} tested per split, {} split(s), {} path(s), \
         embargo {} bar(s)",
        count("groups"),
        count("test_groups"),
        count("splits"),
        paths.len(),
        count("embargo_bars")
    )];
    // Every path comes out of one pre-flight, so the first speaks for the ledger and the data.
    if let Some(first) = paths.first() {
        lines.extend(ledger_line(first));
        lines.extend(data_warning_lines(first));
    }
    for (i, path) in paths.iter().enumerate() {
        lines.push(format!("path {:>3}  {}", i + 1, summary_line(path)));
        // Each path ran the strategy over its own windows: its failures are its own.
        lines.extend(strategy_failure_line(path).map(|line| format!("path {:>3}  {line}", i + 1)));
    }
    lines
}

/// The walk's strategy failures (its `overall` results' `strategy_failures`, every window's together), read
/// TYPED (`vike_model::StrategyFailures`) and printed as its one loud line; `None` for a walk whose strategy
/// never failed (the server sends the block only then).
fn strategy_failure_line(report: &Value) -> Option<String> {
    let block = report.get("overall")?.get("strategy_failures")?;
    let failures: vike_model::StrategyFailures = serde_json::from_value(block.clone()).ok()?;
    Some(failures.to_string())
}

/// What the server found in the walk's data and ran anyway (its `overall` results'
/// `data_warnings`), one line each. The server sends the key only when there is something in it, so
/// a clean walk's table carries none.
fn data_warning_lines(report: &Value) -> Vec<String> {
    let lines =
        report.get("overall").and_then(|o| o.get("data_warnings")).and_then(Value::as_array);
    lines
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|line| format!("data warning: {line}"))
        .collect()
}

/// The one line that names the walk's ledger currency, or `None` for the dollar. The server sends
/// `account_currency` only off the dollar (`WalkforwardResults`), so a dollar table carries no such
/// line. Every number this table prints is a ratio, but the `equity_curve` a `--json` reader gets is
/// an amount of that currency, and the table is where an operator would otherwise assume dollars.
fn ledger_line(report: &Value) -> Option<String> {
    let currency = report.get("account_currency").and_then(Value::as_str)?;
    Some(format!(
        "ledger currency: {currency} (the returns below are ratios; equity_curve is in {currency})"
    ))
}

/// The unit a `vike_analytics::metric_catalog::METRICS` id renders in: its row's own.
fn catalog_unit(id: &str) -> MetricUnit {
    spec_for(id).map_or(MetricUnit::Ratio, |spec| spec.unit)
}

/// One step's row — the index, the test range as a half-open interval, and that step's
/// out-of-sample return (its `results.total_return`).
///
/// ⚠ **Extracted from [`print_walkforward`] so the RENDERING is assertable, and that is the point
/// rather than tidiness.** Every test in this file that exercised the table did it by PRINTING,
/// and none captures stdout — `both_table_shapes_render`'s own doc says all it proves is that
/// neither path panics — so a local `* 100.0` coming back, or a precision drifting, could never
/// redden anything here. A
/// function returning the line can be compared to a literal, which is what
/// `the_rendered_numbers_come_from_their_unit_and_not_from_a_local_scaling` does.
fn window_row(i: usize, w: &Value) -> String {
    let range = w.get("test_range").and_then(Value::as_array);
    let bound = |k: usize| {
        range.and_then(|r| r.get(k)).and_then(Value::as_i64).unwrap_or_default().to_string()
    };
    let results = w.get("results").unwrap_or(&Value::Null);
    format!(
        "{:>6}  {:<20}  {:>11}",
        i + 1,
        format!("[{}, {})", bound(0), bound(1)),
        catalog_unit("total_return").render(number(results, "total_return"))
    )
}

/// The stitched summary — the three numbers an operator judges the whole walk by, each rendered by
/// its own [`MetricUnit`]: the walk's `overall` return and Sharpe, and its profitable step share.
/// Extracted for [`window_row`]'s reason.
fn summary_line(report: &Value) -> String {
    let overall = report.get("overall").unwrap_or(&Value::Null);
    format!(
        "total_return = {}   sharpe = {}   profitable_step_share = {}",
        catalog_unit("total_return").render(number(overall, "total_return")),
        catalog_unit("sharpe").render(number(overall, "sharpe")),
        MetricUnit::Percent.render(number(report, "profitable_step_share"))
    )
}

/// One numeric field of a server report; `0.0` when absent or non-numeric (a `"sharpe": null` —
/// serde_json's encoding of a non-finite f64, e.g. a NaN Sharpe over a flat curve — prints as
/// `0.0000` rather than breaking the table). The ONE reader for both report renderers: this
/// module's `WalkforwardResults` rows and `crate::cmd::backtest`'s ranked `ParamscanReport` table
/// (`render.rs`'s `cell`).
pub(crate) fn number(v: &Value, field: &str) -> f64 {
    v.get(field).and_then(Value::as_f64).unwrap_or(0.0)
}

/// One window's SELECTED parameters as `key=value` pairs — what that window's own search chose on
/// its training half, and the whole diagnostic value of an optimizing walk: the stitched number
/// says what the procedure earned, these say whether it kept finding the same answer or wandered.
///
/// The pairs are rendered in the order they arrive and are not re-sorted here — they need no
/// sorting, because `crates/vike-backtest/src/search/paramscan.rs`'s `expand_paramscan` sorts the grid's
/// keys before it builds a point, so two windows' cells already line up term by term even when the
/// chosen values differ. Rendering is JSON `Display` — compact, the
/// server's own bytes — except that a string value loses its quotes, because this is the human
/// table and `--json` is the exact answer for anyone who must tell `5` from `"5"`.
///
/// `-` for a window that recorded no choice. Reachable only in a MIXED report, which today's server
/// cannot produce (a window whose every candidate failed is a hard error, not a blank row) — so
/// this is a rendering that cannot be reached rather than a case that is handled, and it exists
/// because the alternative is an empty cell that reads as a missing column instead of a missing
/// choice.
fn chosen(w: &Value) -> String {
    let pairs = w.get("chosen_params").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    if pairs.is_empty() {
        return "-".to_string();
    }
    pairs
        .iter()
        .map(|pair| {
            let kv = pair.as_array().map(Vec::as_slice).unwrap_or(&[]);
            let key = kv.first().and_then(Value::as_str).unwrap_or("?");
            match kv.get(1) {
                Some(Value::String(s)) => format!("{key}={s}"),
                Some(v) => format!("{key}={v}"),
                None => format!("{key}=?"),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server `WalkforwardResults` JSON as the wire delivers it, each results object cut to the keys
    /// this renderer reads.
    const WALKFORWARD_JSON: &str = r#"{
      "steps": [{"test_range": [0, 60], "results": {"total_return": 0.05}},
                {"test_range": [60, 120], "results": {"total_return": -0.02}}],
      "overall": {"total_return": 0.029, "sharpe": 0.81},
      "equity_curve": [1000.0, 1050.0, 1029.0],
      "profitable_step_share": 0.5
    }"#;

    /// A walk booked off the dollar names its ledger currency under the heading; a dollar walk
    /// (no `account_currency` key at all) prints nothing extra.
    #[test]
    fn a_non_dollar_walk_names_its_ledger_currency() {
        let dollar: Value = serde_json::from_str(WALKFORWARD_JSON).unwrap();
        assert_eq!(ledger_line(&dollar), None);
        let mut yen = dollar.clone();
        yen["account_currency"] = serde_json::json!("JPY");
        let line = ledger_line(&yen).expect("a yen walk is labelled");
        assert!(line.contains("JPY") && line.contains("equity_curve"), "{line}");
        yen["account_currency"] = serde_json::json!(7);
        assert_eq!(ledger_line(&yen), None, "a non-string names nothing");
    }

    #[test]
    fn a_walks_data_warnings_get_a_line_each_and_a_clean_walk_gets_none() {
        let mut report: Value = serde_json::from_str(WALKFORWARD_JSON).unwrap();
        assert!(data_warning_lines(&report).is_empty());
        report["overall"]["data_warnings"] = serde_json::json!(["the tape stops short"]);
        assert_eq!(data_warning_lines(&report), ["data warning: the tape stops short"]);
    }

    /// A walk whose strategy failed prints the failures' one line, read from the typed block; a clean
    /// walk prints none.
    #[test]
    fn a_walks_strategy_failures_get_their_line_and_a_clean_walk_gets_none() {
        let mut report: Value = serde_json::from_str(WALKFORWARD_JSON).unwrap();
        assert_eq!(strategy_failure_line(&report), None);
        let failures = vike_model::StrategyFailures {
            count: 3,
            first_at: None,
            messages: vec!["on_bar: x".to_string()],
            disabled: true,
        };
        report["overall"]["strategy_failures"] = serde_json::to_value(&failures).unwrap();
        assert_eq!(strategy_failure_line(&report), Some(failures.to_string()));
    }

    /// The renderer reads the SERVER's step rows + summary scalars verbatim.
    #[test]
    fn renders_the_server_windows_and_summary() {
        let report: Value = serde_json::from_str(WALKFORWARD_JSON).unwrap();
        assert_eq!(report["steps"].as_array().unwrap().len(), 2);
        assert_eq!(number(&report["overall"], "sharpe"), 0.81);
        assert_eq!(number(&report["steps"][1]["results"], "total_return"), -0.02);
        print_walkforward(&report);
    }

    /// A degenerate report (no steps, `null` scalars) renders rather than panicking.
    #[test]
    fn a_degenerate_report_renders() {
        let report: Value =
            serde_json::from_str(r#"{"steps": [], "overall": {"sharpe": null}}"#).unwrap();
        assert_eq!(number(&report["overall"], "sharpe"), 0.0);
        assert_eq!(
            summary_line(&report),
            "total_return = 0.0000%   sharpe = 0.0000   profitable_step_share = 0.0000%"
        );
        print_walkforward(&report);
    }

    /// [`number`] is the one JSON number reader for this renderer AND the backtest ranked table
    /// (`crate::cmd::backtest`'s `render.rs` had a byte-identical private copy). Pinned to the
    /// exact answers both copies gave: a number as `f64`, and `0.0` for everything that is not one —
    /// absent, `null`, a numeric STRING (never parsed), a bool, and a field read off a non-object.
    #[test]
    fn number_reads_a_json_number_and_answers_zero_for_anything_else() {
        let v: Value = serde_json::from_str(
            r#"{"f": -0.25, "i": 3, "nul": null,
                "s": "1.5", "b": true, "arr": [1.0], "obj": {"x": 2.0}}"#,
        )
        .unwrap();
        assert_eq!(number(&v, "f"), -0.25);
        assert_eq!(number(&v, "i"), 3.0);
        assert_eq!(number(&v, "absent"), 0.0);
        assert_eq!(number(&v, "nul"), 0.0);
        assert_eq!(number(&v, "s"), 0.0);
        assert_eq!(number(&v, "b"), 0.0);
        assert_eq!(number(&v, "arr"), 0.0);
        assert_eq!(number(&v, "obj"), 0.0);
        assert_eq!(number(&v["arr"], "f"), 0.0);
        assert_eq!(number(&Value::Null, "f"), 0.0);
    }

    /// An OPTIMIZED walk's window carries what it chose, and the renderer states it in the
    /// profile's own vocabulary — `key=value`, one term per swept axis, key-sorted as sent.
    ///
    /// The four value shapes are the four a `[sweep]` axis can hold, and each is pinned because
    /// each is a different `serde_json::Value` arm: the string one is the case worth watching,
    /// since it is the only one whose rendering DIFFERS from `Value`'s own `Display` (quotes off).
    #[test]
    fn a_searched_window_renders_the_parameters_it_chose() {
        let w: Value = serde_json::from_str(
            r#"{"test_range": [0, 60], "results": {"total_return": 0.05},
                "chosen_params": [["flag", true], ["label", "fast-lane"],
                                  ["size", 1.5], ["slow", 30]]}"#,
        )
        .unwrap();
        assert_eq!(chosen(&w), "flag=true label=fast-lane size=1.5 slow=30");
    }

    /// A FIXED-parameter window renders `-`, and so does an empty choice list. This is the whole
    /// signal an operator has for "which protocol did the server actually run", so it is asserted
    /// rather than left to the eye: the server omits `chosen_params` entirely on the fixed walk
    /// (`skip_serializing_if`), which is the FIRST case here, and the second is the same answer
    /// reached from a shape today's server does not emit.
    #[test]
    fn a_fixed_parameter_window_renders_no_choice() {
        let fixed: Value = serde_json::from_str(WALKFORWARD_JSON).unwrap();
        assert_eq!(chosen(&fixed["steps"][0]), "-");
        let empty: Value = serde_json::from_str(r#"{"chosen_params": []}"#).unwrap();
        assert_eq!(chosen(&empty), "-");
    }

    /// ⚠ **Every number in this table comes from its `MetricUnit`, and a local `* 100.0` coming
    /// back fails HERE.**
    ///
    /// The literals are the whole assertion, so read them as the convention rather than as
    /// arithmetic: a `0.029` out-of-sample return is `2.9000%` — four decimals and a `%` — not
    /// `0.0290`, which is the "reads as three basis points" failure
    /// `vike_analytics::metric_catalog::MetricUnit::render`'s own doc argues against.
    /// `profitable_step_share` is a fraction too and reads `50.0000%`; `sharpe` is dimensionless and
    /// takes `Ratio`'s four decimals with no suffix.
    ///
    /// This is the test its printing neighbours in this module cannot be: they print and capture
    /// nothing, so they pass whatever the numbers say.
    #[test]
    fn the_rendered_numbers_come_from_their_unit_and_not_from_a_local_scaling() {
        let report: Value = serde_json::from_str(WALKFORWARD_JSON).unwrap();
        assert_eq!(
            summary_line(&report),
            "total_return = 2.9000%   sharpe = 0.8100   profitable_step_share = 50.0000%"
        );
        // ...and the per-step column, which is the one an operator reads per split. A NEGATIVE
        // step keeps its own sign.
        assert!(window_row(1, &report["steps"][1]).ends_with("-2.0000%"));
        assert!(window_row(0, &report["steps"][0]).ends_with("5.0000%"));
        // The range cell and the index — asserted so a later edit to the format string cannot move
        // them silently while the number stays right.
        assert!(window_row(1, &report["steps"][1]).contains("[60, 120)"));
        assert!(window_row(1, &report["steps"][1]).starts_with("     2"));
        // ⚠ **The RATCHET on naming a unit instead of looking one up.** `total_return` and `sharpe`
        // render through their catalog rows; `profitable_step_share` has none, which is the whole
        // reason this file names its `MetricUnit` (see [`print_walkforward`]'s doc). The day it
        // GAINS a row, `spec_for` becomes the authority, so that day is a failure here rather than
        // a silent divergence, and the repair is to render through the row.
        assert!(spec_for("total_return").is_some() && spec_for("sharpe").is_some());
        assert!(
            spec_for("profitable_step_share").is_none(),
            "`profitable_step_share` now has a catalog row — render it through that row's own unit \
             here, so this table and a tearsheet cannot disagree about one id"
        );
    }

    /// A combinatorial answer renders its cut and one summary row per path, each through the same
    /// unit-rendered summary a plain walk ends on.
    #[test]
    fn a_combinatorial_walk_renders_its_cut_and_one_row_per_path() {
        let path: Value = serde_json::from_str(WALKFORWARD_JSON).unwrap();
        let report = serde_json::json!({
            "groups": 4, "test_groups": 2, "embargo_bars": 3, "splits": 6,
            "paths": [path.clone(), path],
        });
        let lines = combinatorial_lines(&report);
        assert_eq!(
            lines[0],
            "combinatorial walk-forward: 4 group(s), 2 tested per split, 6 split(s), 2 path(s), \
             embargo 3 bar(s)"
        );
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(lines[1], format!("path   1  {}", summary_line(&report["paths"][0])));
        assert!(lines[2].starts_with("path   2  total_return = 2.9000%"), "{lines:?}");
    }

    /// Both table shapes RENDER — the fixed walk with three columns, the optimized walk with four.
    ///
    /// ⚠ What this proves is only that neither path panics: `print_walkforward` writes to stdout
    /// and this test does not capture it, so the COLUMN suppression itself is unasserted here. The
    /// per-window assertion above is what actually pins the cell contents; this is the smoke test
    /// its two neighbours already are, extended to the branch that did not exist before.
    #[test]
    fn both_table_shapes_render() {
        print_walkforward(&serde_json::from_str(WALKFORWARD_JSON).unwrap());
        let optimized: Value = serde_json::from_str(
            r#"{"steps": [{"test_range": [0, 60], "results": {"total_return": 0.05},
                           "chosen_params": [["fast", 5]]}],
                "overall": {"total_return": 0.05, "sharpe": 1.1}, "profitable_step_share": 1.0}"#,
        )
        .unwrap();
        print_walkforward(&optimized);
    }
}
