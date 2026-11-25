//! The ranked parameter-search table, every numeric cell rendered through the metric catalog, and a
//! single backtest's SUMMARY (decision 0078's amendment of 2026-11-18). The walk-forward report has its
//! own child, [`walkforward`].

use serde_json::Value;
use vike_backtesthub_client::summary::{ParamscanProfileSummaryWire, RunSummaryWire};

pub(super) mod walkforward;

/// Print a single backtest's SUMMARY, `backtest run`'s default answer: under `--json` the summary
/// document as one line, else every rendered metric row (the server's own text, the metric catalog's:
/// nothing is recomputed here) and where the rest of the run is — its trades and its whole equity curve
/// stay on the backtesthub, and `--whole` answers the whole report.
pub(super) fn print_summary(summary: &RunSummaryWire, json: bool) -> Result<(), String> {
    if json {
        let text = serde_json::to_string(summary)
            .map_err(|e| format!("cannot print the summary as JSON: {e}"))?;
        println!("{text}");
        return Ok(());
    }
    let width = summary.rows.iter().map(|(id, _)| id.len()).max().unwrap_or(0);
    for (id, text) in &summary.rows {
        println!("{id:<width$}  {text}");
    }
    // A portfolio's mounts, each with its own trades and win %, as its report's table prints them.
    if let Some(table) = summary.results.as_ref().and_then(|r| r.mounts_table()) {
        print!("{table}");
    }
    for line in &summary.data_warnings {
        println!("data warning: {line}");
    }
    // A strategy whose hooks failed: the one loud line its report prints (`vike_model::StrategyFailures`).
    if let Some(failures) = summary.results.as_ref().map(|r| &r.strategy_failures)
        && !failures.is_empty()
    {
        println!("{failures}");
    }
    for (reason, count) in &summary.refused {
        println!("refused {count}x: {reason}");
    }
    let saved = match (summary.answered_from_saved_run.as_str(), summary.saved_run.as_str()) {
        ("", "") => String::new(),
        ("", id) => format!(", saved as run {id}"),
        (id, _) => {
            format!(", answered from saved run {id} (an identical run; `--fresh` runs it again)")
        }
    };
    println!(
        "summary of run {}{saved}: {} closed trade(s) and the whole {}-step equity curve stay on \
         the backtesthub (the curve here is thinned to {} points); `--whole` answers the whole report",
        summary.run,
        summary.n_trades,
        summary.curve_len,
        summary.curve.len()
    );
    Ok(())
}

/// Print a SUMMARIZED parameter search (decision 0078's amendment of 2026-11-19), a search's default answer from a
/// backtesthub that has one: under `--json` the summary document as one line (every point's `overrides`, its
/// `report` metric table, `error` and `score`, the whole report's keys around them, and `saved_run`), else the ranked
/// table [`print_paramscan`] prints from the same keys, and where each point's whole report is.
pub(super) fn print_paramscan_summary(
    summary: &ParamscanProfileSummaryWire,
    json: bool,
) -> Result<(), String> {
    let encoded = |e: serde_json::Error| format!("cannot print the search's summary as JSON: {e}");
    if json {
        println!("{}", serde_json::to_string(summary).map_err(encoded)?);
        return Ok(());
    }
    print_paramscan(&serde_json::to_value(summary).map_err(encoded)?);
    let saved = match summary.saved_run.as_str() {
        "" => "nothing was saved".to_string(),
        id => format!(
            "every point's whole report (its trades included) is saved as run {id}, point N as its \
             `rows[N-1]` (`vike-cli backtest show {id}`)"
        ),
    };
    if !summary.answered_from_saved_run.is_empty() {
        println!(
            "answered from saved run {} (an identical search; `--fresh` runs it again)",
            summary.answered_from_saved_run
        );
    }
    println!(
        "summary of search {} ({} point(s)): {saved}; `--whole` answers the whole report",
        summary.run, summary.total
    );
    Ok(())
}

/// Print a server `ParamscanReport` as a human table — one row per grid point, in the order the SERVER
/// ranked them. Every number printed is read straight out of that row's `TradingResults`; nothing
/// is recomputed here, which is the workspace rule that no metric is reimplemented outside
/// `vike-backtest`.
///
/// # ⚠ The FIX this function carries: `max_drawdown` was published a hundred times too small
///
/// It printed `number(r, "max_drawdown")` under `{:>8.4}` — a bare fraction, no `* 100.0` and no `%`
/// — under a column headed `max_drawdown`, **directly beside a `return` column that WAS scaled and did
/// carry a `%`**. So a three-percent drawdown read `0.0310` on the row an operator sizes risk
/// from, next to `+5.00%`: one table, two conventions, and the smaller-looking number was the one
/// that matters. That is verbatim the failure
/// `vike_analytics::metric_catalog::MetricUnit::render`'s doc names — *"a renderer that drops the
/// `* 100.0` publishes a max drawdown of three percent as `0.0310%`, which reads as three basis
/// points"* — and `max_drawdown` has carried `MetricUnit::Percent` in that catalog all along.
/// Nothing pinned the numeric cells (`crates/vike-cli/tests/cli_verbs/search_walkforward_cli.rs`'s
/// `a_sweep_profile_routes_to_the_search_verb_and_prints_the_server_ranked_table` asserts the
/// banner, the rank name and one override), so it stayed green.
///
/// **Every cell now goes through [`cell`], i.e. through the catalog.** This function keeps the
/// COLUMN WIDTHS and nothing else about a number's appearance, which is the division
/// `MetricUnit::render`'s own doc draws: *"It also does not pad, align or label."*
///
/// ⚠ Three visible consequences, all deliberate, none of them a bug to "fix back":
///   * `return` and `max_drawdown` read at FOUR decimals with a `%` (the `Percent` unit's precision),
///     where `return` used to read at two.
///   * the explicit `+` on a positive return is gone; a negative one still renders its `-`.
///   * the header and the row now use IDENTICAL widths. They did not before — the header's
///     `{:>9}` for `return` was hand-matched against a row's `{:>+8.2}%`, i.e. eight plus a
///     percent sign, which is the kind of arithmetic that goes wrong the first time a width moves.
///
/// ⚠ **`show`'s and `report::stored`'s verbatim carry-through is NOT this defect and must not be
/// converted.** Those two read a stored `report.json` as `serde_json::Value` and print the
/// producer's own numbers unchanged, deliberately, because a `null` normalised to `0.0` would make
/// a broken run look like a flat one — their module docs carry that argument. This table is a
/// RENDERER of live server output with a header of its own, which is the case the catalog owns.
pub(super) fn print_paramscan(report: &Value) {
    let rows = report.get("rows").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    if rows.is_empty() {
        println!("(the search returned no grid points)");
        return;
    }
    let rank_by = report.get("rank_by").and_then(Value::as_str).unwrap_or("sharpe");
    println!("parameter search — {} point(s), ranked server-side by {rank_by}", rows.len());
    // ⚠ The METHOD's own cost line, when the method had one. The engine binary prints this to its
    // own stderr; a REMOTE run has no stderr to read, so before stage 7 a euler or tpe search
    // reported its budget nowhere. The grid carries none and prints none, exactly as before.
    if let Some(summary) = report.get("summary").and_then(Value::as_str) {
        println!("{summary}");
    }
    for line in search_data_warnings(report) {
        println!("data warning: {line}");
    }
    let failures = search_strategy_failures(report);
    if !failures.is_empty() {
        println!("{failures} (over every point of the search)");
    }
    // The money column names its currency when the sweep's ledger is not the dollar: the server
    // sends `account_currency` only then, so a dollar table keeps its header and width byte for byte.
    let equity_head = ledgered_head("final_equity", report);
    let w = equity_head.chars().count().max(14);
    println!(
        "{:>4}  {:<34}  {:>w$}  {:>10}  {:>8}  {:>9}  {:>7}",
        "rank", "overrides", equity_head, "return", "sharpe", "max_dd", "trades"
    );
    for (i, row) in rows.iter().enumerate() {
        let overrides = fmt_overrides(row.get("overrides"));
        match row.get("report").filter(|r| !r.is_null()) {
            Some(r) => println!(
                "{:>4}  {:<34}  {:>w$}  {:>10}  {:>8}  {:>9}  {:>7}",
                i + 1,
                overrides,
                cell(r, "final_equity"),
                cell(r, "total_return"),
                cell(r, "sharpe"),
                cell(r, "max_drawdown"),
                cell(r, "n_trades"),
            ),
            // A per-point failure never fails the whole search server-side — it comes back as this
            // row's `error`, and the row still prints (which point failed, and why).
            None => println!(
                "{:>4}  {:<34}  FAILED: {}",
                i + 1,
                overrides,
                row.get("error").and_then(Value::as_str).unwrap_or("(unknown error)")
            ),
        }
    }
}

/// Print a server `PermutationTestReport` (`[permutation]`): what was tested, the real score, how
/// many permuted histories scored at or above it, and Masters' p-value. Every number is the
/// server's; the per-history scores are the `--json` reader's.
pub(super) fn print_permutation(report: &Value) {
    for line in permutation_lines(report) {
        println!("{line}");
    }
}

/// [`print_permutation`]'s lines, returned so the rendering is assertable.
pub(super) fn permutation_lines(report: &Value) -> Vec<String> {
    let number = walkforward::number;
    let histories = report.get("permuted_scores").and_then(Value::as_array).map_or(0, Vec::len);
    let trained = report.get("trained").and_then(Value::as_bool).unwrap_or(false);
    let protocol = if trained { "permutation training" } else { "permutation test" };
    let seed = report.get("seed").and_then(Value::as_u64).unwrap_or(0);
    let mut lines = vec![format!(
        "{protocol}: the real history against {histories} permuted one(s), seed {seed}"
    )];
    lines.extend(
        report
            .get("data_warnings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|line| format!("data warning: {line}")),
    );
    if let Some(params) = report.get("real_params").filter(|p| !p.is_null()) {
        lines.push(format!("real winner: {}", fmt_overrides(Some(params))));
    }
    lines.push(format!(
        "real_score = {}   at_or_above = {} of {histories}   p_value = {}",
        vike_analytics::metric_catalog::MetricUnit::Ratio.render(number(report, "real_score")),
        report.get("at_or_above").and_then(Value::as_u64).unwrap_or(0),
        vike_analytics::metric_catalog::MetricUnit::Ratio.render(number(report, "p_value")),
    ));
    lines
}

/// What the server found in the search's data and ran anyway. Every point reads one slice, so the
/// first row that carries any speaks for them (a summarized search carries them at its top, its rows'
/// metric tables none); the table has no column for it and a remote run has no stderr to read. Empty
/// for a clean search, which prints as it always did.
pub(super) fn search_data_warnings(report: &Value) -> Vec<&str> {
    let rows = report.get("rows").and_then(Value::as_array).into_iter().flatten();
    report
        .get("data_warnings")
        .and_then(Value::as_array)
        .into_iter()
        .chain(rows.filter_map(|row| row.get("report")?.get("data_warnings")?.as_array()))
        .find(|lines| !lines.is_empty())
        .map(|lines| lines.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// What the strategy's hooks failed with over every point of the search: a summarized search's own block
/// (every point's together), else each row's report's, merged. Read TYPED (`vike_model::StrategyFailures`),
/// never off its words; empty for a search whose strategy never failed, which prints as it always did.
pub(super) fn search_strategy_failures(report: &Value) -> vike_model::StrategyFailures {
    let read = |block: Option<&Value>| {
        block.and_then(|b| serde_json::from_value::<vike_model::StrategyFailures>(b.clone()).ok())
    };
    if let Some(all) = read(report.get("strategy_failures")) {
        return all;
    }
    let mut all = vike_model::StrategyFailures::default();
    let rows = report.get("rows").and_then(Value::as_array).into_iter().flatten();
    for failures in rows.filter_map(|row| read(row.get("report")?.get("strategy_failures"))) {
        all.merge(&failures);
    }
    all
}

/// A money column's header with the report's ledger currency appended (`final_equity JPY`), or the
/// bare header when the report names none (the dollar, and every document older than the label).
pub(super) fn ledgered_head(head: &str, report: &Value) -> String {
    match report.get("account_currency").and_then(Value::as_str) {
        Some(currency) => format!("{head} {currency}"),
        None => head.to_string(),
    }
}

/// Render one row's `[[name, value], …]` overrides as `k=v, k=v`. The values are TOML scalars
/// serialized into JSON, so they print through [`Value`]'s own `Display` (nothing is re-typed).
fn fmt_overrides(overrides: Option<&Value>) -> String {
    let Some(pairs) = overrides.and_then(Value::as_array) else {
        return String::new();
    };
    pairs
        .iter()
        .filter_map(|p| {
            let pair = p.as_array()?;
            let (k, v) = (pair.first()?.as_str()?, pair.get(1)?);
            Some(format!("{k}={v}"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// One CATALOG metric of a server `TradingResults`, rendered the way that metric's own unit says it
/// must be read — the scaling and the precision from
/// `vike_analytics::metric_catalog::MetricUnit::render`, which is *"the ONE home"* for both, and
/// the column width from the caller.
///
/// ⚠ **The `None` arm is unreachable for every id [`print_paramscan`] passes**, and
/// [`tests::every_metric_the_ranked_table_prints_has_a_catalog_row`] is what keeps it so — a
/// `spec_for` that could silently answer nothing is how a column starts lying again. It renders
/// through `Ratio` rather than panicking because that is the one fallback that invents nothing: no
/// scaling applied, no suffix implied, the number as it arrived at four decimals. A panic here
/// would lose the whole ranked table an operator has already waited for a search to produce.
pub(super) fn cell(report: &Value, id: &str) -> String {
    let v = walkforward::number(report, id);
    match vike_analytics::metric_catalog::spec_for(id) {
        Some(spec) => spec.unit.render(v),
        None => vike_analytics::metric_catalog::MetricUnit::Ratio.render(v),
    }
}
