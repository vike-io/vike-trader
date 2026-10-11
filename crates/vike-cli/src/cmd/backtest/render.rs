//! The ranked parameter-search table, every numeric cell rendered through the metric catalog.

use serde_json::Value;

/// Print a server `ParamscanReport` as a human table — one row per grid point, in the order the SERVER
/// ranked them. Every number printed is read straight out of that row's `BacktestReport`; nothing
/// is recomputed here, which is the workspace rule that no metric is reimplemented outside
/// `vike-backtest`.
///
/// # ⚠ The FIX this function carries: `max_dd` was published a hundred times too small
///
/// It printed `num(r, "max_drawdown")` under `{:>8.4}` — a bare fraction, no `* 100.0` and no `%`
/// — under a column headed `max_dd`, **directly beside a `return` column that WAS scaled and did
/// carry a `%`**. So a three-percent drawdown read `0.0310` on the row an operator sizes risk
/// from, next to `+5.00%`: one table, two conventions, and the smaller-looking number was the one
/// that matters. That is verbatim the failure
/// `vike_analytics::metric_catalog::MetricUnit::render`'s doc names — *"a renderer that drops the
/// `* 100.0` publishes a max drawdown of three percent as `0.0310%`, which reads as three basis
/// points"* — and `max_drawdown` has carried `MetricUnit::Percent` in that catalog all along.
/// Nothing pinned the numeric cells (`crates/vike-cli/tests/search_walkforward_cli.rs`'s
/// `a_sweep_profile_routes_to_the_search_verb_and_prints_the_server_ranked_table` asserts the
/// banner, the rank name and one override), so it stayed green.
///
/// **Every cell now goes through [`cell`], i.e. through the catalog.** This function keeps the
/// COLUMN WIDTHS and nothing else about a number's appearance, which is the division
/// `MetricUnit::render`'s own doc draws: *"It also does not pad, align or label."*
///
/// ⚠ Three visible consequences, all deliberate, none of them a bug to "fix back":
///   * `return` and `max_dd` read at FOUR decimals with a `%` (the `Percent` unit's precision),
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
    println!(
        "{:>4}  {:<34}  {:>14}  {:>10}  {:>8}  {:>9}  {:>7}",
        "rank", "overrides", "final_equity", "return", "sharpe", "max_dd", "trades"
    );
    for (i, row) in rows.iter().enumerate() {
        let overrides = fmt_overrides(row.get("overrides"));
        match row.get("report").filter(|r| !r.is_null()) {
            Some(r) => println!(
                "{:>4}  {:<34}  {:>14}  {:>10}  {:>8}  {:>9}  {:>7}",
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

/// One numeric field of a server `BacktestReport`; `0.0` when absent or non-numeric (a
/// `"sharpe": null` — serde_json's encoding of a NaN Sharpe over a flat curve — prints as `0.0000`
/// rather than breaking the table).
fn num(report: &Value, field: &str) -> f64 {
    report.get(field).and_then(Value::as_f64).unwrap_or(0.0)
}

/// One CATALOG metric of a server `BacktestReport`, rendered the way that metric's own unit says it
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
    let v = num(report, id);
    match vike_analytics::metric_catalog::spec_for(id) {
        Some(spec) => spec.unit.render(v),
        None => vike_analytics::metric_catalog::MetricUnit::Ratio.render(v),
    }
}
