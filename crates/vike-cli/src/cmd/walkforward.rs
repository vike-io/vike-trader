//! The walk-forward REPORT RENDERER — what `vike-cli backtest run` prints when the profile
//! declares a window.
//!
//! ⚠ **This was a top-level VERB until decision 3 of
//! `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`.** Walking forward is how a
//! run is VALIDATED rather than a different kind of run (ruling 7), so it folded into
//! `crate::cmd::backtest`'s `run`, where the PROFILE selects it (`route_of`) exactly as a `[sweep]`
//! grid already selected a search — and declaring both COMPOSES rather than competing. What
//! survives here is the half that had nothing to do with the verb: the renderer for the server's
//! `WalkForwardReport`. The client, the address ladder and the argv parsing went; the ONE caller is
//! `crate::cmd::backtest`'s `execute`.
//!
//! ⚠ **The FILE is not renamed and the module is not deleted**, deliberately. Two citations name
//! this path from other crates, and a rename would redden every path-keyed gate row that named the
//! old one while leaving the new path unpinned — the #1754 both-directions failure the root
//! `CLAUDE.md`'s file-move bullet and `crates/vike-ops/tests/path_key_gate.rs` exist for. It is
//! `pub(crate)` now: nothing outside this crate may reach it, and nothing inside it but `backtest`
//! does.
//!
//! # ⚠ There are TWO walk-forward protocols, and this side has no flag for either
//!
//! `[walkforward].n_splits` alone is the FIXED-parameter walk: every window trades the profile's
//! own `[strategy.params]` and asks whether those settings were stable out of sample. Adding
//! `search = "sweep"` to that table asks the other question — each window re-searches the profile's
//! `[sweep]` grid on its OWN training half and trades only that window's winner, so what is being
//! validated is the PROCEDURE of fit-then-trade rather than one parameter set. `mode`
//! (`anchored` | `rolling`) picks the training shape and `rank_by` picks how a window scores its
//! candidates; `crates/vike-backtest/src/harness/profile.rs`'s `WalkforwardCfg` is the authority
//! for every spelling, and an unrecognized one fails server-side at load naming the valid set.
//!
//! ⚠ **`search` is the selector TODAY and ruling 4 of the 2026-09-13 owner rulings deletes it** —
//! after that, whether a window re-searches is whether the profile declares a grid at all. The
//! deletion is the walk-forward stage's work and has NOT landed:
//! `BacktestProfile::window_search` still reads the key, which is why this paragraph describes it
//! rather than the rule that replaces it. Either way it is a PROFILE fact and never a flag —
//! `crate::cmd::backtest`'s `refuse_a_walkforward_flag` refuses `--rank-by`/`--optimizer` on this
//! route by name for exactly that reason.
//!
//! ⚠ `mode` is inert without `search`, and that is a property of the protocol rather than a wiring
//! gap: the two modes differ ONLY in where a window's TRAINING half starts, and the fixed walk
//! discards the training half. Setting it alone parses, runs and returns the same numbers
//! (`crates/vike-backtest/src/harness/walkforward.rs`'s
//! `both_walk_modes_leave_the_fixed_walk_report_identical` pins that), so do not read a `rolling`
//! line in a profile as evidence that anything trained differently.
//!
//! **None of that is a flag, deliberately.** This side parses the profile only far enough to pick a
//! CARRIER (`crate::cmd::backtest`'s `route_of`) — the file's text is shipped whole and the server
//! runs the one `BacktestProfile::from_toml_str` — so an `--optimize` flag would be a second place
//! to say what the profile already says, and the two could then disagree with nothing to arbitrate.
//! `crates/vike-datahub/src/server.rs`'s `run_walkforward_profile` picks the driver from the
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
//! walk-forward mode at all** — `crates/vike-backtest/src/backtest_cli.rs` has one profile path,
//! which branches on `BacktestProfile::is_paramscan` and nothing else, so NEITHER driver
//! (`vike_backtest::harness::run_walkforward` or its optimizing sibling) is reachable from any
//! binary; both run from the datahub server alone. A `--local` here would have nothing to spawn.
//!
//! ⚠ **The refusal MOVED with the fold and is now a property of the PROFILE, not of a verb.** It
//! used to be that `--local` was simply an unknown argument on this file's parser; it is now
//! `crate::cmd::backtest`'s `WALKFORWARD_HAS_NO_LOCAL_ARM`, raised when the resolved profile
//! declares a `[walkforward]` table. That is a smaller lie than a flag that exists and always
//! fails, and this paragraph is where the absence is a decision rather than an oversight. What
//! would change it: a walk-forward entry point on the standalone engine — at which point
//! `backtest run --local` simply stops refusing.
//!
//! Like the parameter-search sibling — the flags `crate::cmd::backtest` absorbed when ruling 13
//! deleted the `sweep` verb — this replaced a client-side profile→DTO mapping whose
//! `WireSlice`/`WireEngineParams` pair could not carry `[engine].fee` and the rest of the
//! `[engine]` surface. The server now parses the profile with the ONE
//! `BacktestProfile::from_toml_str` parser, and the walk-forward honors the whole `[engine]`. The
//! Studio's DTO-shaped `RunWalkforward` wire verb is untouched and still serves the GUI.
//!
//! Server-side this is BAR mode over ONE series (the splitter divides a single bar series by index);
//! a tick or multi-symbol profile is a clean error, never a silent first-symbol fallback.

use serde_json::Value;
use vike_analytics::metric_catalog::MetricUnit;

/// Print the server's `WalkForwardReport` as a human table: one row per OOS window plus the summary
/// line. Every number is the server's own — nothing is recomputed here.
///
/// # ⚠ The scaling and the precision are the UNIT's, and this file used to spell them itself
///
/// Three numbers, and until this pass each of them carried its own arithmetic: `oos_return` was
/// `num(w, "oos_return") * 100.0` under `{:>+10.2}%` in the window rows and again under `{:+.2}%`
/// in the summary, `oos_sharpe` was `{:.3}`, and `wf_consistency` a third `* 100.0` under
/// `{:.1}%`. That is the rule [`MetricUnit::render`] was written to own — *"the ONE home for the
/// scaling and the precision"* — and its doc names what forgetting the `* 100.0` costs: a
/// drawdown of three percent published as `0.0310`, read as three basis points. Nothing here was
/// WRONG; the three hand-spellings simply were not the one home, and three precisions that match
/// no unit in the tree meant a walk-forward return read at two decimals where a `vike-cli report`
/// tearsheet read the same kind of number at four.
///
/// ⚠ **The unit is called by NAME rather than looked up by id, and that is forced rather than
/// chosen.** `oos_return`, `oos_sharpe` and `wf_consistency` are not
/// `vike_analytics::metric_catalog::METRICS` ids — MEASURED against that array, and its `ABSENT`
/// table declares only `exposure`, so neither says anything about them — so `spec_for` cannot
/// answer for them and there is no row to read a unit off. (No count is written here: the property
/// is that none of the three resolves, and
/// `the_rendered_numbers_come_from_their_unit_and_not_from_a_local_scaling` asserts exactly that,
/// so the day one of them GAINS a row this file goes red instead of quietly disagreeing with a
/// tearsheet.) Naming the unit outright is the shape
/// `crates/vike-analytics/src/periods.rs`'s `drawdown_table_text` already uses for a non-catalog
/// number (*"Scaled by `MetricUnit::Percent`, never by a local `* 100.0`"*), and it needs no
/// catalog row: a fraction is a fraction whether or not a report stores it under a declared id.
/// Giving these three real catalog rows is the larger fix and belongs to whoever owns
/// `crates/vike-analytics/src/metric_catalog.rs` — it would move `vike-cli report`'s selection
/// vocabulary too, which is a wider change than a renderer.
///
/// ⚠ The explicit `+` on a positive return is GONE with the hand-formatting, deliberately: a
/// negative number still renders its own `-`, and the sign flag was one more local decision about
/// a number the unit is supposed to answer for. Column WIDTH and alignment stay here, which is the
/// half [`MetricUnit::render`]'s doc says it does not own.
///
/// The `chosen` column appears only when at least one window carries `chosen_params`, i.e. only for
/// a walk whose `[walkforward].search` actually searched. That is not a tidiness rule: the fixed
/// walk and the optimizing walk return the SAME report type, this side parses no TOML and so cannot
/// know which one it asked for, and the per-window winners are the only thing in the answer that
/// distinguishes them. Printing an empty column on the fixed walk would make the two look like one
/// protocol with a blank field; omitting it keeps the two outputs visibly different, and keeps the
/// fixed walk's COLUMN SET what this command has always printed.
///
/// ⚠ That last clause read "byte-identical" and is narrowed rather than withdrawn: the column SET
/// is unchanged, and the argument it supports — that the two protocols must not look like one —
/// is untouched. What did move is the CELL, in the pass above: `oos_return` now reads `5.0000%`
/// where it read `+5.00%`. A rendering claim of byte-identity is a promise a renderer cannot keep
/// across a convention fix, so it is stated over the shape it is actually about.
pub(crate) fn print_walkforward(report: &Value) {
    let windows = report.get("windows").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    println!("walk-forward: {} out-of-sample window(s)", windows.len());
    // `chosen_params` is `skip_serializing_if = "Option::is_none"` on the server's `WfWindow`, so
    // the FIELD's presence — not its emptiness — is what says a window searched.
    let searched = windows.iter().any(|w| w.get("chosen_params").is_some());
    if searched {
        println!("{:>6}  {:<20}  {:>11}  chosen", "window", "test_range", "oos_return");
    } else {
        println!("{:>6}  {:<20}  {:>11}", "window", "test_range", "oos_return");
    }
    for (i, w) in windows.iter().enumerate() {
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

/// One window's row — the index, the test range as a half-open interval, and that window's
/// out-of-sample return.
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
    format!(
        "{:>6}  {:<20}  {:>11}",
        i + 1,
        format!("[{}, {})", bound(0), bound(1)),
        MetricUnit::Percent.render(num(w, "oos_return"))
    )
}

/// The stitched summary — the three numbers an operator judges the whole walk by, each rendered by
/// its own [`MetricUnit`]. Extracted for [`window_row`]'s reason.
fn summary_line(report: &Value) -> String {
    format!(
        "oos_return = {}   oos_sharpe = {}   wf_consistency = {}",
        MetricUnit::Percent.render(num(report, "oos_return")),
        MetricUnit::Ratio.render(num(report, "oos_sharpe")),
        MetricUnit::Percent.render(num(report, "wf_consistency"))
    )
}

/// One numeric field of the server report; `0.0` when absent or `null` (serde_json's encoding of a
/// non-finite f64), so a degenerate run prints instead of breaking the table.
fn num(v: &Value, field: &str) -> f64 {
    v.get(field).and_then(Value::as_f64).unwrap_or(0.0)
}

/// One window's SELECTED parameters as `key=value` pairs — what that window's own search chose on
/// its training half, and the whole diagnostic value of an optimizing walk: the stitched number
/// says what the procedure earned, these say whether it kept finding the same answer or wandered.
///
/// The pairs are rendered in the order they arrive and are not re-sorted here — they need no
/// sorting, because `crates/vike-backtest/src/harness/sweep.rs`'s `expand_paramscan` sorts the grid's
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

    /// A server `WalkForwardReport` JSON as the wire delivers it.
    const WF_JSON: &str = r#"{
      "windows": [{"test_range": [0, 60], "oos_return": 0.05},
                  {"test_range": [60, 120], "oos_return": -0.02}],
      "oos_equity_curve": [1000.0, 1050.0, 1029.0],
      "oos_return": 0.029,
      "oos_sharpe": 0.81,
      "wf_consistency": 0.5
    }"#;

    /// The renderer reads the SERVER's window rows + summary scalars verbatim.
    #[test]
    fn renders_the_server_windows_and_summary() {
        let report: Value = serde_json::from_str(WF_JSON).unwrap();
        assert_eq!(report["windows"].as_array().unwrap().len(), 2);
        assert_eq!(num(&report, "oos_sharpe"), 0.81);
        assert_eq!(num(&report["windows"][1], "oos_return"), -0.02);
        print_walkforward(&report);
    }

    /// A degenerate report (no windows, `null` scalars) renders rather than panicking.
    #[test]
    fn a_degenerate_report_renders() {
        let report: Value = serde_json::from_str(r#"{"windows": [], "oos_sharpe": null}"#).unwrap();
        assert_eq!(num(&report, "oos_sharpe"), 0.0);
        print_walkforward(&report);
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
            r#"{"test_range": [0, 60], "oos_return": 0.05,
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
        let fixed: Value = serde_json::from_str(WF_JSON).unwrap();
        assert_eq!(chosen(&fixed["windows"][0]), "-");
        let empty: Value = serde_json::from_str(r#"{"chosen_params": []}"#).unwrap();
        assert_eq!(chosen(&empty), "-");
    }

    /// ⚠ **Every number in this table comes from its `MetricUnit`, and a local `* 100.0` coming
    /// back fails HERE.**
    ///
    /// The literals are the whole assertion, so read them as the convention rather than as
    /// arithmetic: a `0.029` out-of-sample return is `2.9000%` — four decimals and a `%` — not
    /// `0.0290`, which is the "reads as three basis points" failure
    /// `vike_analytics::metric_catalog::MetricUnit::render`'s own doc argues against, and not
    /// `+2.90%`, which is what this file printed while it owned the scaling. `wf_consistency` is a
    /// fraction too and reads `50.0000%`; `oos_sharpe` is dimensionless and takes `Ratio`'s four
    /// decimals with no suffix.
    ///
    /// This is the test its printing neighbours in this module cannot be: they print and capture
    /// nothing, so they pass whatever the numbers say.
    #[test]
    fn the_rendered_numbers_come_from_their_unit_and_not_from_a_local_scaling() {
        let report: Value = serde_json::from_str(WF_JSON).unwrap();
        assert_eq!(
            summary_line(&report),
            "oos_return = 2.9000%   oos_sharpe = 0.8100   wf_consistency = 50.0000%"
        );
        // ...and the per-window column, which is the one an operator reads per split. A NEGATIVE
        // window keeps its own sign; what went with the hand-formatting is the explicit `+` on a
        // positive one.
        assert!(window_row(1, &report["windows"][1]).ends_with("-2.0000%"));
        assert!(window_row(0, &report["windows"][0]).ends_with("5.0000%"));
        // The range cell and the index are unchanged by the migration — asserted so a later edit to
        // the format string cannot move them silently while the number stays right.
        assert!(window_row(1, &report["windows"][1]).contains("[60, 120)"));
        assert!(window_row(1, &report["windows"][1]).starts_with("     2"));
        // ⚠ **The RATCHET on naming a unit instead of looking one up.** These three ids have no
        // catalog row, which is the whole reason this file calls `MetricUnit` by name (see
        // [`print_walkforward`]'s doc). The day one of them GAINS a row, `spec_for` becomes the
        // authority and a hand-named unit here could disagree with what a `vike-cli report`
        // tearsheet prints for the same id — so that day is a failure here rather than a silent
        // divergence, and the repair is to render through the row.
        for id in ["oos_return", "oos_sharpe", "wf_consistency"] {
            assert!(
                vike_analytics::metric_catalog::spec_for(id).is_none(),
                "`{id}` now has a catalog row — render it through that row's own unit here, so \
                 this table and a tearsheet cannot disagree about one id"
            );
        }
    }

    /// Both table shapes RENDER — the fixed walk with three columns, the optimized walk with four.
    ///
    /// ⚠ What this proves is only that neither path panics: `print_walkforward` writes to stdout
    /// and this test does not capture it, so the COLUMN suppression itself is unasserted here. The
    /// per-window assertion above is what actually pins the cell contents; this is the smoke test
    /// its two neighbours already are, extended to the branch that did not exist before.
    #[test]
    fn both_table_shapes_render() {
        print_walkforward(&serde_json::from_str(WF_JSON).unwrap());
        let optimized: Value = serde_json::from_str(
            r#"{"windows": [{"test_range": [0, 60], "oos_return": 0.05,
                             "chosen_params": [["fast", 5]]}],
                "oos_return": 0.05, "oos_sharpe": 1.1, "wf_consistency": 1.0}"#,
        )
        .unwrap();
        print_walkforward(&optimized);
    }
}
