//! Calendar-period returns and drawdown table — port of `analysis/periods.py`.
//!
//! - [`periodic_returns`]: group an equity curve into calendar buckets and compute the period
//!   return for each bucket.
//! - [`monthly_return_matrix`]: produce the year x month heatmap data structure.
//! - [`drawdown_table`]: find the top-N drawdown episodes with peak/trough/recovery timestamps
//!   and bar-counts.
//!
//! # The DOORS onto the two analyses, and why they live here
//!
//! [`periodic_returns`] and [`drawdown_table`] are complete and were, until this module grew the
//! three functions below, callable by nobody: they had ZERO callers anywhere in the tree while
//! `vike-cli`'s `--breakdown` and `--drawdowns` refused as unbuilt. ⚠ That clause is HISTORY on
//! both halves now: `--breakdown` is answered by `vike-cli report <run> --breakdown day|month`,
//! and `--drawdowns` still refuses but no longer blames a missing renderer — it names the WIRING,
//! and [`drawdown_table_text`] has a caller in `crates/vike-cli/src/cmd/runs/show.rs` that proves
//! the refusal cannot go back to claiming the renderer does not exist. A PRODUCTION caller is
//! still owed. What was missing was never the
//! analysis — it was the two things a command-line caller needs and a pure fold does not provide.
//!
//! * **A non-panicking door onto the period roster.** [`period_key`] PANICS on an unknown period,
//!   which is right for a library invariant and catastrophic for a flag value an operator typed:
//!   `--breakdown fortnight` would abort the process instead of printing a usage error.
//!   [`parse_period`] is the refusal, and [`PERIODS`] is the ONE roster both it and the panic
//!   message read, so a period added here cannot be missing from either.
//! * **A rendering.** Put it at the call site and the NEXT caller writes a second one — the failure
//!   `crate::metric_catalog`'s own module doc records, where it names every hand copy of the metric
//!   roster that had to be deleted. [`periodic_returns_text`] and [`drawdown_table_text`] are
//!   therefore pure formatters of the analyses' own return types, taking the rows rather than the
//!   curve: a caller that wants both a table and `--json` folds ONCE and renders from the same
//!   values, so its two outputs cannot disagree.
//!
//! ⚠ That bullet carried the COUNT — "happening three times to the metric roster" — and the count
//! rotted inside this same branch: the catalog's doc records FOUR copies now and names each, the
//! fourth being an eleven-entry array of report keys typed out in
//! `crates/vike-cli/src/cmd/runs/show.rs` and since replaced by that file's `report_key_order`,
//! which asks the catalog for the ids instead. So the number is GONE from here rather than
//! corrected to four: a cross-reference restating another document's count is one more hand copy
//! of exactly the kind this bullet is about, and the bullet's argument never needed a number.
//!
//! ⚠ **The doors are BUILT and CALLED BY NOBODY, and the section above reads as though building
//! them had closed the hole.** "Callable by nobody" was a claim about CALLABILITY — still exactly
//! true, and silent about the half a reader of this section wants. MEASURED on this tree: the only
//! consumers of this module anywhere outside this file are `crates/vike-analytics/src/html.rs`
//! (vike-report's until it moved into this crate on 2026-09-28), which takes
//! [`monthly_return_matrix`] and [`period_key`], and the simulator's `DateRule` in
//! `crates/vike-sim/src/schedule.rs`, whose `PeriodStart` rule calls [`period_key`] directly. (It
//! reached it through a one-line `period_key` delegate of its own until that second name was
//! deleted when the simulator split out of `vike-backtest`, docs/decisions/0087.)
//! [`periodic_returns`], [`drawdown_table`],
//! [`parse_period`], [`periodic_returns_text`] and [`drawdown_table_text`] are reached from this
//! file's `#[cfg(test)] mod tests` and from nothing else. So the two bullets above argue for the
//! SHAPE these doors were given; they are not evidence that anything has come through one.
//!
//! ⚠ **The per-bucket return fold now exists TWICE, and the copy an operator can reach is the other
//! one.** `crates/vike-cli/src/cmd/report/stored.rs`'s `breakdown_of` groups consecutive
//! `period_label` runs and computes each row's return as `end / start - 1` in its own `period_row`,
//! against its own roster — `crates/vike-cli/src/cmd/report/schema.rs`'s `PERIODS`, which publishes
//! `day|month` — and prints the raw fraction rather than routing it through
//! [`crate::metric_catalog::MetricUnit`]. Two of those divergences are ARGUED on that side and this
//! module disputes neither: `report::schema`'s `PERIODS` argues why `week` is absent — the crate
//! reaches `vike_model::time` for every calendar answer, that module has no ISO-week labeller, and
//! inventing one there would be a second calendar home in a crate meant to have none — and
//! `report::stored`'s `period_label` argues the same rule for building its labels out of
//! `vike_model::time::civil_from_days`. What is argued NOWHERE is the duplicated return
//! arithmetic, precisely the drift the second bullet cites the metric roster's own hand copies to
//! prevent — so the FOLD is what a later PR should collapse onto [`periodic_returns`], leaving that
//! side's roster and its labeller where their own docs put them.
//!
//! ⚠ Both renderers scale percentages through [`crate::metric_catalog::MetricUnit::render`] rather
//! than a local `* 100.0`, for the reason that method exists: a period return of `0.0234` printed
//! as `0.0234` reads as two basis points instead of two percent, and every hand-rolled renderer in
//! this tree has made that exact mistake at least once.

use chrono::{DateTime, Datelike, Utc};
use indexmap::IndexMap;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::metric_catalog::MetricUnit;

fn epoch_ms_to_utc(epoch_ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(epoch_ms).expect("epoch_ms out of range for DateTime<Utc>")
}

/// The calendar buckets [`period_key`] understands — **the one roster**, in coarsening order.
///
/// It exists because the set was written twice before it was written once: inside [`period_key`]'s
/// `match` and again, as a quoted list, inside that function's own panic message. A bucket added to
/// the match without being added to the message produced a refusal that named four periods while
/// five worked, which is the shape of wrongness an operator cannot debug — the tool is telling them
/// their correct input is invalid. Both the match's fallthrough and [`parse_period`] now read this
/// array, so a new bucket joins every door at once or none.
///
/// ⚠ **Declaration order is the order a caller offering a CHOICE should offer them in**, coarsening
/// left to right. Nothing depends on it, so this is a convention rather than an invariant; it is
/// stated because the array is the natural place a caller reaches for such a list, and reversing it
/// silently would make one.
pub const PERIODS: &[&str] = &["daily", "weekly", "monthly", "quarterly", "yearly"];

/// Resolve an operator's period spelling to the canonical [`PERIODS`] entry, or say why not.
///
/// **This is the door a command-line caller must come through** — a RULE with no caller obeying it
/// yet (the module doc carries the measurement) — and the reason is [`period_key`]'s panic: a
/// bucket name arriving from `argv` or a config file is UNTRUSTED input, and a library invariant
/// enforced with `panic!` turns a typo into an aborted process with a backtrace instead of a usage
/// error. Nothing else about the analyses is fallible — a curve is either empty or it is not — so
/// this is the whole of the validation surface.
///
/// Matching is ASCII-case-insensitive and the return is always the CANONICAL spelling, so the
/// operator's casing reaches nothing downstream: a `--breakdown Monthly` and a `--breakdown
/// monthly` produce byte-identical labels, and no second spelling of a period exists to rot. The
/// error names the whole roster (read from [`PERIODS`], never retyped) because "invalid period" on
/// its own sends the reader to the source.
///
/// ⚠ It deliberately accepts NO abbreviations or aliases — not `day`, not `mo`, not `1M`. A caller
/// whose own published flag values differ owes the mapping to a canonical spelling on its OWN side:
/// an alias table here would make "the name of a period" a question with two answers, and this
/// module would own neither.
///
/// ⚠ **That last sentence used to be phrased as something a caller ALREADY DOES** — "`vike-cli`'s
/// `report` verb publishes `day|month` … maps its spelling to a canonical one on its own side" —
/// and no such mapping exists anywhere. `crates/vike-cli/src/cmd/report/stored.rs`'s
/// `Period::parse` matches `"day"` and `"month"` in a `match` of its own and `Period::as_str` hands
/// the same two strings straight back out; nothing on that side resolves either to
/// `daily`/`monthly`, and nothing on that side calls this function or [`period_key`] at all. The
/// rule survives the correction untouched — it is what a caller coming through here OWES — but read
/// it as an obligation rather than as a description of the tree.
pub fn parse_period(spec: &str) -> Result<&'static str, String> {
    PERIODS
        .iter()
        .copied()
        .find(|p| p.eq_ignore_ascii_case(spec))
        .ok_or_else(|| format!("unknown period {spec:?} — expected one of {}", PERIODS.join(", ")))
}

/// A comparable label for the calendar period containing `ts_ms` (UTC).
///
/// `period` is one of `"daily"`, `"weekly"`, `"monthly"`, `"quarterly"`, `"yearly"`:
/// - daily     -> `"2024-03-15"`
/// - weekly    -> `"2024-W11"` (ISO week)
/// - monthly   -> `"2024-03"`
/// - quarterly -> `"2024-Q1"`
/// - yearly    -> `"2024"`
///
/// # Panics
///
/// Panics if `period` is not one of the supported values. That is inherited behaviour: the
/// ported `analysis/periods.py` raised `ValueError` here. Provenance only — nothing compares
/// this function against Python any more. ⚠ A caller holding a period that came from a HUMAN
/// must go through [`parse_period`] first — that function's doc carries why a panic is the wrong
/// answer to a typed flag value.
pub fn period_key(ts_ms: i64, period: &str) -> String {
    // Delegate "daily" to the workspace's one day-law home (`vike_model::time`, div_euclid-based)
    // so this bucket key can never drift from the store's `date=` partition math or the EOD/klines
    // day-floor, rather than re-deriving Y-M-D via `chrono` locally (dedup A-class fix). Output is
    // identical: both format `{y:04}-{m:02}-{d:02}` off the same UTC calendar day.
    if period == "daily" {
        return vike_model::time::epoch_ms_to_utc_date(ts_ms);
    }
    let dt = epoch_ms_to_utc(ts_ms);
    match period {
        "weekly" => {
            let iso = dt.iso_week();
            format!("{:04}-W{:02}", iso.year(), iso.week())
        }
        "monthly" => format!("{:04}-{:02}", dt.year(), dt.month()),
        "quarterly" => {
            let q = (dt.month() - 1) / 3 + 1;
            format!("{:04}-Q{}", dt.year(), q)
        }
        "yearly" => format!("{}", dt.year()),
        // The roster is read from `PERIODS`, never retyped here — see that constant for the
        // message that named four periods while five worked.
        other => {
            panic!("period must be one of {}; got {other:?}", PERIODS.join(", "))
        }
    }
}

/// `(label, return_fraction)` pairs grouped by calendar period, in chronological order.
///
/// `return_fraction = last_equity_in_period / first_equity_at_start_of_period - 1`.
///
/// # Panics
///
/// Panics if `equity_curve` and `timestamps` differ in length, or `period` is unsupported.
pub fn periodic_returns(
    equity_curve: &[f64],
    timestamps: &[i64],
    period: &str,
) -> Vec<(String, f64)> {
    assert_eq!(
        equity_curve.len(),
        timestamps.len(),
        "equity_curve and timestamps must have equal length"
    );
    if equity_curve.is_empty() {
        return Vec::new();
    }

    // Validate period early (panics for unknown values) — inherited from the ported
    // `analysis/periods.py`, whose `_period_label()` call ran just as early.
    let _ = period_key(timestamps[0], period);

    // label -> (first_equity, last_equity); IndexMap preserves first-insertion (chronological)
    // order, mirroring Python's insertion-ordered dict + explicit label_order list.
    let mut groups: IndexMap<String, (f64, f64)> = IndexMap::new();
    let mut prev_label: Option<String> = None;
    let mut prev_equity = equity_curve[0];

    for (&eq, &ts) in equity_curve.iter().zip(timestamps.iter()) {
        let lbl = period_key(ts, period);
        if let Some(&(entry_eq, _)) = groups.get(&lbl) {
            groups.insert(lbl.clone(), (entry_eq, eq));
        } else {
            let entry_eq = if prev_label.is_some() { prev_equity } else { equity_curve[0] };
            groups.insert(lbl.clone(), (entry_eq, eq));
        }
        prev_label = Some(lbl);
        prev_equity = eq;
    }

    groups
        .into_iter()
        .map(|(lbl, (entry_eq, last_eq))| {
            let ret = if entry_eq == 0.0 { 0.0 } else { last_eq / entry_eq - 1.0 };
            (lbl, ret)
        })
        .collect()
}

/// Year x month return heatmap, mirroring Python's `monthly_return_matrix()` dict.
#[derive(Debug, Clone, PartialEq)]
pub struct MonthlyReturnMatrix {
    pub years: Vec<i32>,
    pub matrix: BTreeMap<i32, BTreeMap<u32, Option<f64>>>,
    pub annual: BTreeMap<i32, f64>,
}

/// Build a year x month heatmap of returns.
///
/// # Panics
///
/// Panics if `equity_curve` and `timestamps` differ in length.
pub fn monthly_return_matrix(equity_curve: &[f64], timestamps: &[i64]) -> MonthlyReturnMatrix {
    let monthly = periodic_returns(equity_curve, timestamps, "monthly");

    let mut matrix: BTreeMap<i32, BTreeMap<u32, Option<f64>>> = BTreeMap::new();
    for (label, ret) in &monthly {
        let year: i32 = label[..4].parse().unwrap();
        let month: u32 = label[5..7].parse().unwrap();
        matrix.entry(year).or_default().insert(month, Some(*ret));
    }

    let mut annual: BTreeMap<i32, f64> = BTreeMap::new();
    for (&year, months) in &matrix {
        let mut product = 1.0;
        for m in 1..=12u32 {
            if let Some(Some(r)) = months.get(&m) {
                product *= 1.0 + r;
            }
        }
        annual.insert(year, product - 1.0);
    }

    let years: Vec<i32> = matrix.keys().copied().collect();
    MonthlyReturnMatrix { years, matrix, annual }
}

/// One drawdown episode, mirroring Python's `drawdown_table()` dict entries.
///
/// `Serialize` so a machine consumer gets the PRODUCER's field names — `depth`, `peak_ts`,
/// `trough_ts`, `recovery_ts`, `length`, `recovery` — rather than a second hand-maintained
/// document shape beside them. It is the same rule `crate::report::BacktestReport` follows and the
/// same one `vike_tradehub_client::proto`'s `Response::Tearsheet` carries JSON text for: one schema,
/// owned where the numbers are computed. ⚠ A non-finite `depth` serializes as JSON `null` (serde's
/// behaviour for non-finite floats, not a choice made here) — the house sentinel convention
/// `crate::metrics` documents, and it cannot arise from a finite equity curve.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DrawdownEpisode {
    /// fractional drawdown from peak (positive, e.g. 0.20 == 20%)
    pub depth: f64,
    pub peak_ts: i64,
    pub trough_ts: i64,
    /// `None` if never recovered within the curve
    pub recovery_ts: Option<i64>,
    /// bars from peak to trough (inclusive)
    pub length: i64,
    /// bars from trough to recovery (inclusive); `None` if not recovered
    pub recovery: Option<i64>,
}

/// Find the top-N drawdown episodes sorted by depth (worst first).
///
/// # Panics
///
/// Panics if `equity_curve` and `timestamps` differ in length.
pub fn drawdown_table(
    equity_curve: &[f64],
    timestamps: &[i64],
    top_n: usize,
) -> Vec<DrawdownEpisode> {
    assert_eq!(
        equity_curve.len(),
        timestamps.len(),
        "equity_curve and timestamps must have equal length"
    );
    if equity_curve.is_empty() {
        return Vec::new();
    }

    let n = equity_curve.len();
    let mut peak_val = equity_curve[0];
    let mut peak_idx = 0i64;
    let mut in_drawdown = false;
    let mut trough_val = equity_curve[0];
    let mut trough_idx = 0i64;
    let mut episodes: Vec<DrawdownEpisode> = Vec::new();

    let close_episode = |peak_val: f64,
                         peak_idx: i64,
                         trough_val: f64,
                         trough_idx: i64,
                         recovery_idx: Option<i64>| {
        let depth = if peak_val != 0.0 { (peak_val - trough_val) / peak_val } else { 0.0 };
        let length = trough_idx - peak_idx;
        let recovery = recovery_idx.map(|r| r - trough_idx);
        DrawdownEpisode {
            depth,
            peak_ts: timestamps[peak_idx as usize],
            trough_ts: timestamps[trough_idx as usize],
            recovery_ts: recovery_idx.map(|r| timestamps[r as usize]),
            length,
            recovery,
        }
    };

    for (i, &eq) in equity_curve.iter().enumerate().take(n) {
        if eq >= peak_val {
            if in_drawdown && trough_val < peak_val {
                let ep = close_episode(peak_val, peak_idx, trough_val, trough_idx, Some(i as i64));
                if ep.depth > 0.0 {
                    episodes.push(ep);
                }
            }
            peak_val = eq;
            peak_idx = i as i64;
            trough_val = eq;
            trough_idx = i as i64;
            in_drawdown = false;
        } else {
            in_drawdown = true;
            if eq < trough_val {
                trough_val = eq;
                trough_idx = i as i64;
            }
        }
    }

    if in_drawdown && trough_val < peak_val {
        let ep = close_episode(peak_val, peak_idx, trough_val, trough_idx, None);
        if ep.depth > 0.0 {
            episodes.push(ep);
        }
    }

    episodes.sort_by(|a, b| b.depth.partial_cmp(&a.depth).unwrap());
    episodes.truncate(top_n);
    episodes
}

/// Render [`periodic_returns`]' rows as an aligned two-column table.
///
/// Takes the ROWS, not the curve, so a caller that also emits JSON folds once and renders both
/// outputs from the same values — two folds of one curve are deterministic and therefore equal
/// today, but they are two call sites that can be given different `period` arguments, and a table
/// and a document disagreeing about one run is the failure this shape forecloses.
///
/// `period` is used for the HEADING only (the labels already carry the bucketing), so it is taken
/// as it was resolved rather than re-derived — pass what [`parse_period`] returned.
///
/// ⚠ **An empty `rows` renders a SENTENCE, never an empty string.** `periodic_returns` returns no
/// rows only for an empty curve, so the one thing the caller must not print is blank output: an
/// operator who asked for a breakdown and got nothing reads it as a broken command, and the real
/// fact — there is no equity curve to bucket — is the answer they need.
#[must_use]
pub fn periodic_returns_text(period: &str, rows: &[(String, f64)]) -> String {
    if rows.is_empty() {
        return format!(
            "no {period} periods — the equity curve has no samples, so there is nothing to \
             bucket.\n"
        );
    }
    const PERIOD_HEAD: &str = "period";
    const RETURN_HEAD: &str = "return";
    let rendered: Vec<(&str, String)> =
        rows.iter().map(|(l, r)| (l.as_str(), MetricUnit::Percent.render(*r))).collect();
    let lw = rendered.iter().map(|(l, _)| l.len()).chain([PERIOD_HEAD.len()]).max().unwrap_or(0);
    let rw = rendered.iter().map(|(_, v)| v.len()).chain([RETURN_HEAD.len()]).max().unwrap_or(0);
    let mut out = String::with_capacity(32 * rendered.len() + 96);
    let _ = writeln!(out, "{period} returns — {} periods", rendered.len());
    let _ = writeln!(out, "  {PERIOD_HEAD:lw$}  {RETURN_HEAD:>rw$}");
    for (label, value) in &rendered {
        let _ = writeln!(out, "  {label:lw$}  {value:>rw$}");
    }
    out
}

/// Render [`drawdown_table`]'s episodes as an aligned table, deepest first (the order that function
/// already sorted them into — this renderer re-sorts nothing, so the rows a caller shows and the
/// rows it serializes are the same rows in the same order).
///
/// Timestamps print through `vike_model::time::epoch_ms_to_utc_timestamp` rather than a local
/// `chrono` format, for the reason [`period_key`]'s `"daily"` arm delegates to that module: the
/// workspace has ONE UTC day-and-instant law, and a second spelling of it here could disagree with
/// the store's `date=` partitions and with every other rendered instant about which day a
/// pre-1970 or midnight-boundary bar belongs to. Second resolution, because a drawdown episode is
/// bounded by BAR timestamps and sub-second precision would be noise in a table a human reads.
///
/// ⚠ **An unrecovered episode is the row that matters most, so it is spelled out rather than
/// blanked.** `recovery_ts: None` means the curve ENDED while still under water — the drawdown is
/// open, not zero-length — and an empty cell reads as "no data". It prints `still open`, and its
/// bar count prints `n/a`, which is a different claim from a `0`.
///
/// An empty `episodes` renders the sentence, never blank output, for the same reason
/// [`periodic_returns_text`] does — and the sentence is a genuinely different fact: a curve that
/// never fell below a prior peak has no episodes, which is not the same as having no curve.
#[must_use]
pub fn drawdown_table_text(episodes: &[DrawdownEpisode]) -> String {
    if episodes.is_empty() {
        return "no drawdown episodes — the equity curve never closed below a prior peak.\n"
            .to_string();
    }
    // Headings, and the alignment of each column. The numeric columns are RIGHT-aligned so depths
    // and bar counts compare down the column by eye; the instants are left-aligned because they
    // are fixed-width already and a right-aligned timestamp column just moves the gutter.
    const HEAD: [&str; 7] = [
        "#",
        "depth_from_peak",
        "peak",
        "trough",
        "bars_to_trough",
        "recovery",
        "bars_to_recovery",
    ];
    const RIGHT: [bool; 7] = [true, true, false, false, true, false, true];

    let ts = vike_model::time::epoch_ms_to_utc_timestamp;
    let rows: Vec<[String; 7]> = episodes
        .iter()
        .enumerate()
        .map(|(i, e)| {
            [
                format!("{}", i + 1),
                // `depth` is documented POSITIVE — a FALL from the peak — so the column is named
                // for the direction rather than signed, and the legend below repeats it. Scaled by
                // `MetricUnit::Percent`, never by a local `* 100.0`.
                MetricUnit::Percent.render(e.depth),
                ts(e.peak_ts),
                ts(e.trough_ts),
                format!("{}", e.length),
                e.recovery_ts.map_or_else(|| "still open".to_string(), ts),
                e.recovery.map_or_else(|| "n/a".to_string(), |r| format!("{r}")),
            ]
        })
        .collect();

    let mut w = [0usize; 7];
    for (i, head) in HEAD.iter().enumerate() {
        w[i] = rows.iter().map(|r| r[i].len()).chain([head.len()]).max().unwrap_or(0);
    }
    let mut out = String::with_capacity(96 * rows.len() + 192);
    let _ = writeln!(out, "top {} drawdown episodes, deepest first", rows.len());
    let write_row = |cells: &[String; 7], out: &mut String| {
        for (i, cell) in cells.iter().enumerate() {
            let width = w[i];
            let _ = if RIGHT[i] {
                write!(out, "  {cell:>width$}")
            } else {
                write!(out, "  {cell:<width$}")
            };
        }
        let _ = writeln!(out);
    };
    write_row(&HEAD.map(|h| h.to_string()), &mut out);
    for row in &rows {
        write_row(row, &mut out);
    }
    let _ = writeln!(
        out,
        "  depth_from_peak is the FALL below the running peak (positive); bar counts are \
         peak→trough and trough→recovery inclusive."
    );
    out
}

#[path = "periods_tests.rs"]
#[cfg(test)]
mod periods_tests;
