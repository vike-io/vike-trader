//! `render_html` — the self-contained HTML tearsheet: ONE document string (no external assets,
//! no scripts, no new deps) a browser renders directly.
//!
//! Composition, not computation: the metrics table renders the [`LiveTearsheet`] fields verbatim
//! (the same numbers the text tearsheet prints — nothing is re-derived), and the monthly-returns
//! table reuses `vike_analytics::periods::monthly_return_matrix` (the existing calendar-bucketing
//! oracle) over the SAME equity curve/timestamps the sheet was built from. The only math done
//! here is presentational: mapping series points onto hand-rolled inline-SVG path coordinates
//! (equity curve + underwater drawdown), plus the running-peak drawdown series those pixels plot.
//!
//! Styling is a minimal embedded stylesheet, dark-friendly via `color-scheme` +
//! `prefers-color-scheme` (no theme toggle, no fonts, no JS). Sections degrade gracefully: an
//! empty/short equity curve renders a "(no equity samples)" note instead of an SVG, and the
//! monthly table appears only when timestamps align 1:1 with the curve (calendar bucketing needs
//! a ts per sample).
//!
//! NON-FINITE INPUTS are a first-class degradation case, not an assumed-impossible one: a stored
//! `kind=equity` sample read through `--store` is data from another process, and a single
//! `NaN`/`inf` would otherwise reach the SVG `points`/`d` attribute as a literal `NaN`/`inf`
//! token — invalid SVG that browsers handle by silently truncating the chart (a blank/partial
//! plot, no error). Every charted series is therefore gated on [`chartable`] (finite AND >= 2
//! points), and a non-finite monthly-return cell renders as the empty marker rather than
//! `+NaN%`.

use std::fmt::Write as _;

use vike_analytics::periods::{monthly_return_matrix, period_key};

use crate::mtm::RuntimeStats;
use crate::report::LiveTearsheet;

/// Chart geometry: one fixed viewBox, responsive via CSS (`width:100%`).
const SVG_W: f64 = 860.0;
const SVG_H: f64 = 220.0;
const PAD: f64 = 12.0;

/// Minimal HTML escaping for text nodes/attribute values (name labels are free-form).
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Map series index -> x pixel.
fn x_at(i: usize, n: usize) -> f64 {
    if n <= 1 {
        SVG_W / 2.0
    } else {
        PAD + i as f64 * (SVG_W - 2.0 * PAD) / (n - 1) as f64
    }
}

/// Map value -> y pixel for a `[min, max]` range (a flat series centers on the mid-line).
fn y_at(v: f64, min: f64, max: f64) -> f64 {
    if max > min {
        PAD + (max - v) / (max - min) * (SVG_H - 2.0 * PAD)
    } else {
        SVG_H / 2.0
    }
}

/// Can this series be drawn? A chart needs at least two points AND every sample finite.
///
/// The finiteness half is the load-bearing one: [`y_at`] would map a `NaN`/`inf` sample straight
/// into the SVG `points`/`d` attribute as a literal `NaN`/`inf` token. That is invalid SVG, and
/// browsers do not report it — they truncate the shape at the bad token, silently rendering a
/// partial or blank chart. Non-finite samples ARE reachable here (a `kind=equity` series read
/// through `--store` is data some other process wrote), so a whole-series gate, degrading to a
/// note, is the honest handling: better an explicit "not chartable" than a plausible-looking
/// plot that quietly stops early.
fn chartable(values: &[f64]) -> bool {
    values.len() >= 2 && values.iter().all(|v| v.is_finite())
}

/// The note that replaces a chart that cannot be drawn — distinguishes "there is nothing to plot"
/// from "there are samples, but they are not plottable", so a corrupt series is visible in the
/// document instead of looking like an empty run.
fn chart_note(values: &[f64]) -> &'static str {
    if values.len() < 2 {
        "(no equity samples)"
    } else {
        "(equity samples are not finite — chart omitted)"
    }
}

/// `points="x1,y1 x2,y2 ..."` for an SVG polyline over the series.
fn polyline_points(values: &[f64], min: f64, max: f64) -> String {
    let n = values.len();
    let mut pts = String::new();
    for (i, &v) in values.iter().enumerate() {
        if i > 0 {
            pts.push(' ');
        }
        let _ = write!(pts, "{:.1},{:.1}", x_at(i, n), y_at(v, min, max));
    }
    pts
}

/// Running-peak underwater series: `equity[i] / running_peak - 1` (`<= 0`, in fraction terms).
/// Presentational only — the scalar `max_drawdown` metric stays `metrics::max_drawdown`.
fn underwater(equity: &[f64]) -> Vec<f64> {
    let mut peak = f64::NEG_INFINITY;
    equity
        .iter()
        .map(|&v| {
            peak = peak.max(v);
            if peak > 0.0 {
                v / peak - 1.0
            } else {
                0.0
            }
        })
        .collect()
}

/// One `<svg>` line chart (class-styled polyline). `filled` additionally closes the shape down
/// to the `baseline` value (used by the underwater chart to shade the drawdown area).
fn svg_chart(values: &[f64], class: &str, filled: bool, baseline: f64) -> String {
    let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let mut s = String::new();
    let _ = write!(
        s,
        "<svg viewBox=\"0 0 {SVG_W:.0} {SVG_H:.0}\" preserveAspectRatio=\"none\" \
         role=\"img\" class=\"chart\">"
    );
    if filled {
        // Polygon: baseline -> series -> baseline, closed. Shades the area under the line.
        let n = values.len();
        let y0 = y_at(baseline.min(max).max(min), min, max);
        let mut d = String::new();
        let _ = write!(d, "M {:.1},{:.1}", x_at(0, n), y0);
        for (i, &v) in values.iter().enumerate() {
            let _ = write!(d, " L {:.1},{:.1}", x_at(i, n), y_at(v, min, max));
        }
        let _ = write!(d, " L {:.1},{:.1} Z", x_at(n - 1, n), y0);
        let _ = write!(s, "<path class=\"{class}-fill\" d=\"{d}\" />");
    }
    let _ = write!(
        s,
        "<polyline class=\"{class}\" points=\"{}\" />",
        polyline_points(values, min, max)
    );
    s.push_str("</svg>");
    s
}

/// The `min`/`max` value captions under a chart.
fn range_caption(label: &str, min: f64, max: f64, unit: &str) -> String {
    format!("<p class=\"caption\">{label} range: {min:.2}{unit} … {max:.2}{unit}</p>")
}

/// The monthly-returns year x month table, or `None` when the inputs cannot be calendar-bucketed
/// (no timestamps, or a length mismatch with the curve).
fn monthly_table(equity: &[f64], ts: &[i64]) -> Option<String> {
    if equity.len() < 2 || equity.len() != ts.len() {
        return None;
    }
    let m = monthly_return_matrix(equity, ts);
    if m.years.is_empty() {
        return None;
    }

    let mut s = String::new();
    s.push_str("<table class=\"monthly\"><thead><tr><th>year</th>");
    const MONTHS: [&str; 12] =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    for name in MONTHS {
        let _ = write!(s, "<th>{name}</th>");
    }
    s.push_str("<th>year Σ</th></tr></thead><tbody>");
    for year in &m.years {
        let _ = write!(s, "<tr><th>{year}</th>");
        let months = &m.matrix[year];
        for month in 1..=12u32 {
            // A non-finite bucket renders as the EMPTY marker, not as a signed number: the naive
            // `r < 0.0` sign test is false for `NaN`, so it would class one `pos` and print a
            // confidently green "+NaN%".
            match months.get(&month).copied().flatten() {
                Some(r) if r.is_finite() => {
                    let class = if r < 0.0 { "neg" } else { "pos" };
                    let _ = write!(s, "<td class=\"{class}\">{:+.2}%</td>", r * 100.0);
                }
                _ => s.push_str("<td class=\"empty\">·</td>"),
            }
        }
        let annual = m.annual[year];
        if annual.is_finite() {
            let class = if annual < 0.0 { "neg" } else { "pos" };
            let _ = write!(s, "<td class=\"{class} annual\">{:+.2}%</td>", annual * 100.0);
        } else {
            s.push_str("<td class=\"empty annual\">·</td>");
        }
        s.push_str("</tr>");
    }
    s.push_str("</tbody></table>");
    Some(s)
}

/// Render the whole tearsheet as ONE self-contained HTML document string.
///
/// `equity_curve`/`equity_ts` are the SAME series the sheet was assembled from
/// (`LiveTearsheet::from_result_parts`'s inputs) — passed back in because the flat sheet does not
/// retain them. `equity_ts` may be empty (or mismatched) — the monthly table and date captions
/// are then omitted; the metrics table and charts render regardless (charts need >= 2 points).
///
/// This is the DEFAULT render: byte-identical to before the runtime-stats section existed. To
/// additionally render the [`RuntimeStats`] block, call [`render_html_with_stats`].
pub fn render_html(sheet: &LiveTearsheet, equity_curve: &[f64], equity_ts: &[i64]) -> String {
    render_html_inner(sheet, equity_curve, equity_ts, None)
}

/// [`render_html`] plus an appended "Runtime exposure" section rendering `runtime` (turnover,
/// peak gross/net exposure, peak margin). The section is APPENDED to the default body, so
/// everything up to it is byte-identical to [`render_html`]'s output.
pub fn render_html_with_stats(
    sheet: &LiveTearsheet,
    equity_curve: &[f64],
    equity_ts: &[i64],
    runtime: &RuntimeStats,
) -> String {
    render_html_inner(sheet, equity_curve, equity_ts, Some(runtime))
}

/// The shared body. `runtime.is_none()` reproduces the pre-runtime-stats document byte-for-byte
/// (the section is only emitted, and only ever appended just before `</main>`, when `Some`).
fn render_html_inner(
    sheet: &LiveTearsheet,
    equity_curve: &[f64],
    equity_ts: &[i64],
    runtime: Option<&RuntimeStats>,
) -> String {
    let name = esc(sheet.name.as_deref().unwrap_or("(unnamed)"));

    // Metrics rows: the LiveTearsheet fields verbatim, same precision family as its Display.
    let metrics: Vec<(&str, String)> = vec![
        ("trades", format!("{}", sheet.n_trades)),
        ("final equity", format!("{:.2}", sheet.final_equity)),
        ("total return", format!("{:.4}%", sheet.total_return * 100.0)),
        ("net profit", format!("{:.2}", sheet.net_profit)),
        ("gross profit", format!("{:.2}", sheet.gross_profit)),
        ("gross loss", format!("{:.2}", sheet.gross_loss)),
        ("total fees", format!("{:.4}", sheet.total_fees)),
        ("win rate", format!("{:.4}%", sheet.win_rate * 100.0)),
        ("profit factor", format!("{:.4}", sheet.profit_factor)),
        ("sharpe", format!("{:.4}", sheet.sharpe)),
        ("sortino", format!("{:.4}", sheet.sortino)),
        ("calmar", format!("{:.4}", sheet.calmar)),
        ("max drawdown", format!("{:.4}%", sheet.max_drawdown * 100.0)),
        ("cagr", format!("{:.4}%", sheet.cagr * 100.0)),
        ("sqn", format!("{:.4}", sheet.sqn)),
        ("avg win", format!("{:.2}", sheet.avg_win)),
        ("avg loss", format!("{:.2}", sheet.avg_loss)),
        ("payoff ratio", format!("{:.4}", sheet.payoff_ratio)),
        ("expected payoff", format!("{:.4}", sheet.expected_payoff)),
        ("largest win", format!("{:.2}", sheet.largest_win)),
        ("largest loss", format!("{:.2}", sheet.largest_loss)),
        ("consecutive wins", format!("{}", sheet.consecutive_wins)),
        ("consecutive losses", format!("{}", sheet.consecutive_losses)),
        ("VaR 95", format!("{:.4}", sheet.value_at_risk_95)),
        ("expected shortfall 95", format!("{:.4}", sheet.expected_shortfall_95)),
    ];

    let mut out = String::with_capacity(16 * 1024);
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    let _ = write!(out, "<title>Tearsheet — {name}</title>");
    out.push_str(STYLE);
    out.push_str("</head><body><main>");

    let _ = write!(out, "<h1>Tearsheet <span class=\"name\">{name}</span></h1>");
    // Date range caption (calendar labels reuse the periods.rs formatter — no date dep here).
    // Gated on >= 2 samples, matching the charts below: a no-trades journal yields the seed-only
    // `equity = [seed]` / `ts = [0]` pair, which a mere non-empty test would caption
    // "1970-01-01 … 1970-01-01 UTC · 1 equity samples" directly above two "(no equity samples)"
    // notes — an internally contradictory document.
    if equity_curve.len() >= 2 && equity_ts.len() == equity_curve.len() {
        let first = period_key(equity_ts[0], "daily");
        let last = period_key(equity_ts[equity_ts.len() - 1], "daily");
        let _ = write!(
            out,
            "<p class=\"caption\">{first} … {last} UTC · {} equity samples</p>",
            equity_curve.len()
        );
    }

    // --- metrics table ---
    out.push_str("<h2>Metrics</h2><table class=\"metrics\"><tbody>");
    for (label, value) in &metrics {
        let _ = write!(out, "<tr><th>{}</th><td>{}</td></tr>", esc(label), esc(value));
    }
    out.push_str("</tbody></table>");

    // --- equity curve ---
    out.push_str("<h2>Equity curve</h2>");
    if chartable(equity_curve) {
        out.push_str(&svg_chart(equity_curve, "equity", false, 0.0));
        let min = equity_curve.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = equity_curve.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        out.push_str(&range_caption("equity", min, max, ""));
    } else {
        let _ = write!(out, "<p class=\"caption\">{}</p>", chart_note(equity_curve));
    }

    // --- underwater drawdown ---
    // Gated on the DERIVED series, not the source: `underwater`'s running-peak division is what
    // would introduce a non-finite value even from a finite curve (a non-positive peak is already
    // special-cased to 0.0, but the gate is cheap and keeps the invariant local to what is drawn).
    out.push_str("<h2>Underwater drawdown</h2>");
    let dd_pct: Vec<f64> = underwater(equity_curve).iter().map(|d| d * 100.0).collect();
    if chartable(&dd_pct) {
        out.push_str(&svg_chart(&dd_pct, "dd", true, 0.0));
        let worst = dd_pct.iter().cloned().fold(f64::INFINITY, f64::min);
        out.push_str(&range_caption("drawdown", worst, 0.0, "%"));
    } else {
        let _ = write!(out, "<p class=\"caption\">{}</p>", chart_note(&dd_pct));
    }

    // --- monthly returns (only when the data supports calendar bucketing) ---
    if let Some(table) = monthly_table(equity_curve, equity_ts) {
        out.push_str("<h2>Monthly returns</h2>");
        out.push_str(&table);
    }

    // --- runtime exposure (ONLY when provided) — appended last, so the None path above is
    // byte-identical to the pre-runtime-stats document. ---
    if let Some(rs) = runtime {
        out.push_str(&runtime_section(rs));
    }

    out.push_str("</main></body></html>\n");
    out
}

/// The "Runtime exposure" metrics sub-table (reuses the `.metrics` styling — no new CSS). Ratios
/// render with a `×` leverage suffix; notional/margin as bare currency amounts.
fn runtime_section(rs: &RuntimeStats) -> String {
    let rows: [(&str, String); 5] = [
        ("turnover", format!("{:.4}×", rs.turnover)),
        ("traded notional", format!("{:.2}", rs.traded_notional)),
        ("peak gross exposure", format!("{:.4}×", rs.peak_gross_exposure)),
        ("peak net exposure", format!("{:.4}×", rs.peak_net_exposure)),
        ("peak margin used", format!("{:.2}", rs.peak_margin_used)),
    ];
    let mut s = String::new();
    s.push_str("<h2>Runtime exposure</h2><table class=\"metrics\"><tbody>");
    for (label, value) in &rows {
        let _ = write!(s, "<tr><th>{}</th><td>{}</td></tr>", esc(label), esc(value));
    }
    s.push_str("</tbody></table>");
    s
}

/// The embedded stylesheet: minimal, dark-friendly (`color-scheme` + `prefers-color-scheme`),
/// no external fonts/assets. NB: deliberately free of `<`/`>` characters (no child selectors),
/// so the document's tag structure stays trivially parseable/verifiable.
const STYLE: &str = "<style>\
:root{color-scheme:light dark;--bg:#ffffff;--fg:#191c22;--muted:#68707f;--line:#e2e5ea;\
--accent:#2f6fed;--pos:#1a7a4b;--neg:#b42318;--fill:#b4231826;}\
@media (prefers-color-scheme:dark){:root{--bg:#111318;--fg:#e5e8ef;--muted:#98a1b3;\
--line:#2b3040;--accent:#6ea0ff;--pos:#4cc38a;--neg:#ff8272;--fill:#ff827233;}}\
body{margin:0;background:var(--bg);color:var(--fg);\
font:14px/1.5 system-ui,-apple-system,sans-serif;}\
main{max-width:920px;margin:0 auto;padding:24px 16px 64px;}\
h1{font-size:1.4rem;margin:0 0 4px;}h1 .name{color:var(--muted);font-weight:normal;}\
h2{font-size:1.05rem;margin:28px 0 8px;}\
.caption{color:var(--muted);font-size:0.85rem;margin:4px 0 0;}\
table{border-collapse:collapse;width:100%;}\
th,td{padding:3px 10px;text-align:right;font-variant-numeric:tabular-nums;}\
tr{border-bottom:1px solid var(--line);}\
.metrics th{text-align:left;font-weight:normal;color:var(--muted);width:40%;}\
.monthly{font-size:0.85rem;}.monthly th{color:var(--muted);font-weight:normal;}\
.monthly .annual{font-weight:bold;}\
.pos{color:var(--pos);}.neg{color:var(--neg);}.empty{color:var(--muted);}\
.chart{display:block;width:100%;height:220px;margin-top:4px;\
border:1px solid var(--line);border-radius:6px;}\
.equity{fill:none;stroke:var(--accent);stroke-width:1.6;}\
.dd{fill:none;stroke:var(--neg);stroke-width:1.4;}\
.dd-fill{fill:var(--fill);stroke:none;}\
</style>";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mtm::RuntimeStats;
    use vike_model::Trade;

    fn trade(pnl: f64, exit_ts: i64) -> Trade {
        Trade {
            entry_price: 100.0,
            exit_price: 100.0 + pnl,
            size: 1.0,
            pnl,
            fees: 0.1,
            entry_ts: exit_ts - 1,
            exit_ts,
            symbol: "BTCUSDT".to_string(),
            mae: 0.0,
            mfe: 0.0,
            is_long: true,
        }
    }

    /// Three months of samples so the monthly table has real buckets.
    fn sample() -> (LiveTearsheet, Vec<f64>, Vec<i64>) {
        const DAY_MS: i64 = 86_400_000;
        let jan15: i64 = 1_705_276_800_000; // 2024-01-15 UTC
        let ts: Vec<i64> = (0..5).map(|i| jan15 + i * 30 * DAY_MS).collect();
        let equity = vec![1_000.0, 1_050.0, 990.0, 1_080.0, 1_120.0];
        let trades = vec![trade(50.0, ts[1]), trade(-60.0, ts[2]), trade(130.0, ts[4])];
        let sheet = LiveTearsheet::from_result_parts(
            Some("sess & <html>".into()),
            trades,
            equity.clone(),
            ts.clone(),
            252.0,
        );
        (sheet, equity, ts)
    }

    /// Tiny tag-balance checker: every open tag must be closed in LIFO order. Handles the
    /// doctype, self-closing (`... />`) and void (`meta`) tags. Sufficient because the renderer
    /// never emits `<` in text (values are escaped) and the stylesheet contains no `<`/`>`.
    fn assert_balanced(html: &str) {
        const VOID: [&str; 4] = ["meta", "br", "hr", "link"];
        let mut stack: Vec<String> = Vec::new();
        let mut rest = html;
        while let Some(i) = rest.find('<') {
            rest = &rest[i + 1..];
            let end = rest.find('>').expect("tag never closed with '>'");
            let tag = rest[..end].trim();
            rest = &rest[end + 1..];
            if tag.starts_with('!') {
                continue; // doctype
            }
            if let Some(name) = tag.strip_prefix('/') {
                let top = stack.pop().unwrap_or_else(|| panic!("stray closing </{name}>"));
                assert_eq!(top, name.trim(), "mismatched nesting: <{top}> closed by </{name}>");
            } else if !tag.ends_with('/') {
                let name: String = tag.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                if !VOID.contains(&name.as_str()) {
                    stack.push(name);
                }
            }
        }
        assert!(stack.is_empty(), "unclosed tags: {stack:?}");
    }

    #[test]
    fn html_smoke_structure_and_values() {
        let (sheet, eq, ts) = sample();
        let html = render_html(&sheet, &eq, &ts);

        // Structure: a full document with inline SVG charts and balanced nesting.
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert_eq!(html.matches("<svg").count(), 2, "equity + underwater charts");
        assert!(html.contains("<polyline"));
        assert!(html.contains("<style>"));
        assert_balanced(&html);

        // Metrics values render (same formatting family as the text tearsheet).
        assert!(html.contains(&format!("{:.2}", sheet.final_equity)), "final equity renders");
        assert!(html.contains(&format!("{:.4}", sheet.sharpe)), "sharpe renders");
        assert!(html.contains(&format!("{:.4}%", sheet.max_drawdown * 100.0)), "max_dd renders");

        // The free-form name is escaped, never raw.
        assert!(html.contains("sess &amp; &lt;html&gt;"));
        assert!(!html.contains("sess & <html>"));

        // Self-contained: no external fetches of any kind.
        assert!(!html.contains("http://") && !html.contains("https://"));
        assert!(!html.contains("<script"));
    }

    #[test]
    fn monthly_table_renders_calendar_buckets() {
        let (sheet, eq, ts) = sample();
        let html = render_html(&sheet, &eq, &ts);
        assert!(html.contains("Monthly returns"));
        assert!(html.contains("class=\"monthly\""));
        assert!(html.contains("<th>2024</th>"), "year row present");
        // Jan 2024: 1000 -> 1050 = +5.00% — the periods.rs oracle's number, rendered.
        assert!(html.contains("+5.00%"), "January bucket renders its return");
    }

    #[test]
    fn monthly_table_omitted_without_timestamps() {
        let (sheet, eq, _) = sample();
        let html = render_html(&sheet, &eq, &[]);
        assert!(!html.contains("Monthly returns"), "no ts -> no calendar bucketing");
        assert_balanced(&html);
        // Charts still render off the bare curve.
        assert_eq!(html.matches("<svg").count(), 2);
    }

    #[test]
    fn empty_curve_degrades_to_note_not_broken_svg() {
        let (sheet, _, _) = sample();
        let html = render_html(&sheet, &[], &[]);
        assert!(!html.contains("<svg"));
        assert!(html.contains("(no equity samples)"));
        assert_balanced(&html);
    }

    /// A non-finite equity sample must never reach the SVG. `y_at` would map it into the
    /// `points`/`d` attribute as a literal `NaN`/`inf` token — invalid SVG that browsers do not
    /// report, they just truncate the shape there (a partial/blank chart). Both charts degrade to
    /// the explicit note instead, which also names the reason.
    #[test]
    fn non_finite_equity_sample_degrades_to_note_not_broken_svg() {
        let (sheet, _, ts) = sample();
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let eq = vec![1_000.0, 1_050.0, bad, 1_080.0, 1_120.0];
            let html = render_html(&sheet, &eq, &ts);
            assert!(!html.contains("<svg"), "no chart for a non-finite series ({bad})");
            assert!(!html.contains("<polyline"), "no polyline coordinates at all ({bad})");
            assert!(!html.contains("points="), "no coordinate attribute to corrupt ({bad})");
            assert!(html.contains("are not finite"), "the note names the reason ({bad})");
            // The note must NOT claim there are no samples — there are five, they are unusable.
            assert!(
                !html.contains("(no equity samples)"),
                "wrong note for a corrupt curve ({bad})"
            );
            assert_balanced(&html);
        }
    }

    /// A finite curve still charts — the finiteness gate must not have broken the happy path.
    #[test]
    fn finite_curve_still_charts_after_the_gate() {
        let (sheet, eq, ts) = sample();
        let html = render_html(&sheet, &eq, &ts);
        assert_eq!(html.matches("<svg").count(), 2, "equity + underwater still render");
        assert!(!html.contains("are not finite"));
    }

    /// A non-finite monthly bucket renders as the EMPTY marker, never as a signed number:
    /// `NaN < 0.0` is false, so the naive sign test classed it `pos` and printed a green "+NaN%".
    #[test]
    fn non_finite_monthly_cell_renders_empty_not_nan_percent() {
        let (sheet, _, ts) = sample();
        let eq = vec![1_000.0, 1_050.0, f64::NAN, 1_080.0, 1_120.0];
        let html = render_html(&sheet, &eq, &ts);
        assert!(html.contains("Monthly returns"), "the table still renders: {html}");
        assert!(!html.contains("NaN%"), "no NaN cell may be printed as a percentage: {html}");
        assert!(!html.contains(">+NaN"), "and none classed positive: {html}");
        assert_balanced(&html);
    }

    /// The seed-only curve a NO-TRADES journal produces (`equity = [seed]`, `ts = [0]`) must not
    /// caption an epoch date range directly above two "(no equity samples)" notes.
    #[test]
    fn single_sample_curve_renders_no_epoch_date_caption() {
        let (sheet, _, _) = sample();
        let html = render_html(&sheet, &[1_000.0], &[0]);
        assert!(!html.contains("1970-01-01"), "no epoch date caption: {html}");
        assert!(!html.contains("equity samples</p>"), "no sample-count caption: {html}");
        assert!(html.contains("(no equity samples)"), "the charts still say so");
        assert_balanced(&html);
    }

    /// ... while a real 2+ sample curve keeps its date-range caption.
    #[test]
    fn multi_sample_curve_keeps_its_date_caption() {
        let (sheet, eq, ts) = sample();
        let html = render_html(&sheet, &eq, &ts);
        assert!(html.contains("2024-01-15"), "first sample date renders: {html}");
        assert!(html.contains("5 equity samples"), "sample count renders: {html}");
    }

    #[test]
    fn underwater_series_is_running_peak_relative() {
        let dd = underwater(&[100.0, 120.0, 90.0, 120.0, 130.0]);
        assert_eq!(dd[0], 0.0);
        assert_eq!(dd[1], 0.0);
        assert!((dd[2] - (90.0 / 120.0 - 1.0)).abs() < 1e-12);
        assert_eq!(dd[3], 0.0);
        assert_eq!(dd[4], 0.0);
    }

    fn sample_stats() -> RuntimeStats {
        RuntimeStats {
            traded_notional: 220.0,
            turnover: 0.5,
            peak_gross_exposure: 1.25,
            peak_net_exposure: 0.75,
            peak_margin_used: 5_000.0,
        }
    }

    /// OFF/default: `render_html` omits the runtime section entirely (the byte-identical path).
    #[test]
    fn default_render_html_has_no_runtime_section() {
        let (sheet, eq, ts) = sample();
        let html = render_html(&sheet, &eq, &ts);
        assert!(!html.contains("Runtime exposure"), "default render omits the runtime section");
        assert!(!html.contains("peak gross exposure"));
        assert_balanced(&html);
    }

    /// With stats: the section is APPENDED — everything up to `</main>` is byte-for-byte the
    /// default document, and the appended rows render and stay balanced.
    #[test]
    fn render_with_stats_appends_runtime_section_byte_identically() {
        let (sheet, eq, ts) = sample();
        let base = render_html(&sheet, &eq, &ts);
        let full = render_html_with_stats(&sheet, &eq, &ts, &sample_stats());

        let marker = "</main></body></html>\n";
        let base_head = base.strip_suffix(marker).expect("default ends with the main-close marker");
        assert!(full.starts_with(base_head), "stats render must not alter the default body");
        assert!(full.ends_with(marker));

        assert!(full.contains("Runtime exposure"));
        assert!(full.contains("turnover"));
        assert!(full.contains("peak gross exposure"));
        assert!(full.contains("peak net exposure"));
        assert!(full.contains("peak margin used"));
        assert!(full.contains("5000.00"), "peak margin renders as a currency amount");
        assert_balanced(&full);
    }
}
