//! Number/date formatting for the chart. The pure number formatters (thousands grouping + the
//! compact volume label) now live in the shared `vike_ui_theme::fmt` leaf crate and are imported
//! here so the crate-internal call sites are unchanged; only the chart-SPECIFIC formatters — the
//! scale-aware axis/chip label formatter and the crosshair datetime — stay local, since they
//! depend on this crate's `ChartScaleMode`/`ChartScaleMapping`/`ThemeDisplayTimeZone`. Bodies are verbatim.

use crate::chart::scale::{ChartScaleMapping, ChartScaleMode};
use vike_ui_theme::time_zone::ThemeDisplayTimeZone;

// The pure number formatters, canonicalized in vike-ui-theme (F35 dedup). All three are crate-local
// now. `mod fmt` is PRIVATE inside `chart`, so `format_thousands`'s old `pub` reached the outside only
// through the `pub use fmt::format_thousands` in `crates/vike-chart/src/chart/mod.rs` — and that alias
// had no consumer, so it went with it. Readers inside `chart` (`overlay.rs`, `format_scaled_view`
// below) are unaffected; everyone else spells `vike_ui_theme::fmt::format_thousands`, which is what
// every call site outside this module already did.
pub(crate) use vike_ui_theme::fmt::{format_compact, format_thousands, format_thousands_precision};

pub(crate) fn format_datetime(ot: i64, time_zone: ThemeDisplayTimeZone) -> String {
    vike_ui_theme::time_zone::to_naive(ot, time_zone)
        .map(|dt| dt.format("%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// [`format_scaled_view`] for an axis TICK. A fixed 2 decimals (or the user's precision) makes every
/// mark of a narrow price range print alike — a 1e-5 price labelled `0.00`, `0.00`, `0.01`, ... — so
/// a Linear axis keeps at least as many decimals as its marks are fine
/// (`vike_math::steps::axis_mark_decimals`: marks sit on multiples of a power of ten, egui_plot's
/// default spacer, or of 1/2/5 times one, `crate::chart::scale::nice_ticks_view`; a step that is not a
/// positive number adds none); a mark whose step is 0.01 or more prints exactly as before. The
/// mapped modes (Log, Percent, Indexed) keep their own formats: their marks' steps are not raw
/// price steps.
pub(crate) fn format_scaled_tick(
    view: ChartScaleMapping,
    mark: egui_plot::GridMark,
    anchor: f64,
    prec: Option<u8>,
) -> String {
    if view.mode == ChartScaleMode::Linear {
        let marks = vike_math::steps::axis_mark_decimals(mark.step_size).unwrap_or(0);
        let decimals = usize::from(prec.unwrap_or(2)).max(marks);
        return format_thousands_precision(view.flip(mark.value), decimals);
    }
    format_scaled_view(view, mark.value, anchor, prec)
}

/// A study or compare-series pane's tick label: one decimal, more once its marks are closer than
/// 0.1 apart (an indicator on a 1e-5 price swings by 1e-7: `0.0`, `-0.0`, `0.0`, ... otherwise).
pub(crate) fn format_study_tick(v: f64, step: f64) -> String {
    format!("{:.*}", vike_math::steps::axis_mark_decimals(step).unwrap_or(0).max(1), v)
}

/// A volume or CVD pane's tick label: [`format_compact`], which prints a value under 1000 with no
/// decimals, so marks closer than 1 apart (a pane of fractional volumes: `0`, `0`, `1`) get the
/// decimals they need instead.
pub(crate) fn format_compact_tick(v: f64, step: f64) -> String {
    if v.abs() < 1e3 && step < 1.0 {
        format!("{:.*}", vike_math::steps::axis_mark_decimals(step).unwrap_or(0), v)
    } else {
        format_compact(v)
    }
}

/// Format a MAPPED plot-space y-value as its axis/chip/tag label (chart-UX
/// bundle T2 §1): Percent shows the mapped percent number directly (fixed 2
/// decimals + '%'); Linear/Log unmap back to the raw price and reuse
/// `format_thousands` (comma-grouped). The §3 precision override (`prec = Some(p)`)
/// forces `p` fixed decimals for Linear/Log only — Percent stays `{:.2}%`
/// regardless (T6). NOT used for the OHLC legend — those stay raw price
/// readouts regardless of scale mode (data readouts, not positions).
pub(crate) fn format_scaled(
    mode: ChartScaleMode,
    mapped: f64,
    anchor: f64,
    prec: Option<u8>,
) -> String {
    format_scaled_view(ChartScaleMapping::new(mode, false), mapped, anchor, prec)
}

/// [`format_scaled`] over a full [`ChartScaleMapping`]: identical to the bare-mode version
/// when `invert == false`, and additionally handles the two new options —
/// **Invert** (the incoming `mapped` is in flipped plot-space, so it is un-flipped
/// via `view.flip` before formatting/unmapping) and **Indexed** (shows the mapped
/// index number directly, plain — around 100, no `%` — honoring the §3 precision
/// override like Linear/Log, unlike Percent's fixed `{:.2}%`).
pub(crate) fn format_scaled_view(
    view: ChartScaleMapping,
    mapped: f64,
    anchor: f64,
    prec: Option<u8>,
) -> String {
    // Un-flip first: every readback below wants the value in TRUE mapped space.
    let mapped = view.flip(mapped);
    match view.mode {
        ChartScaleMode::Percent => format!("{mapped:.2}%"),
        ChartScaleMode::Indexed => match prec {
            Some(p) => format_thousands_precision(mapped, p as usize),
            None => format_thousands(mapped),
        },
        ChartScaleMode::Linear | ChartScaleMode::Logarithmic => {
            let raw = view.mode.unmap(mapped, anchor);
            match prec {
                Some(p) => format_thousands_precision(raw, p as usize),
                None => format_thousands(raw),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_plot::GridMark;

    fn mark(value: f64, step_size: f64) -> GridMark {
        GridMark { value, step_size }
    }

    /// A step that is not a positive number adds no decimals to a tick (the axis-mark decimals
    /// themselves are pinned in `crates/vike-math/src/steps_tests.rs`).
    #[test]
    fn a_tick_whose_step_is_not_a_number_prints_as_its_floor() {
        let lin = ChartScaleMapping::new(ChartScaleMode::Linear, false);
        for step in [0.0, f64::NAN] {
            assert_eq!(format_scaled_tick(lin, mark(1.5, step), 0.0, None), "1.50");
            assert_eq!(format_study_tick(1.5, step), "1.5");
            assert_eq!(format_compact_tick(1.5, step), "2");
        }
    }

    #[test]
    fn a_tick_with_a_coarse_step_prints_exactly_as_before() {
        let lin = ChartScaleMapping::new(ChartScaleMode::Linear, false);
        for (v, step) in [(100.0, 1.0), (101.5, 0.5), (0.25, 0.01), (-3.0, 10.0)] {
            assert_eq!(
                format_scaled_tick(lin, mark(v, step), 0.0, None),
                format_scaled_view(lin, v, 0.0, None)
            );
            assert_eq!(
                format_scaled_tick(lin, mark(v, step), 0.0, Some(3)),
                format_scaled_view(lin, v, 0.0, Some(3))
            );
        }
    }

    #[test]
    fn narrow_ranges_get_the_decimals_their_marks_need() {
        let lin = ChartScaleMapping::new(ChartScaleMode::Linear, false);
        // A 1e-5 price: marks 1e-6 apart, which 2 decimals print as 0.00 five times over.
        let texts: Vec<String> = (0..5)
            .map(|i| format_scaled_tick(lin, mark(1.0e-5 + i as f64 * 1.0e-6, 1.0e-6), 0.0, None))
            .collect();
        assert_eq!(texts, ["0.000010", "0.000011", "0.000012", "0.000013", "0.000014"]);
        // The user's own precision is a floor, never a ceiling; the inverted axis reads true prices.
        assert_eq!(format_scaled_tick(lin, mark(0.5, 0.1), 0.0, Some(4)), "0.5000");
        let inverted = ChartScaleMapping::new(ChartScaleMode::Linear, true);
        assert_eq!(format_scaled_tick(inverted, mark(-0.002, 0.001), 0.0, None), "0.002");
        // The mapped modes keep their own formats.
        let pct = ChartScaleMapping::new(ChartScaleMode::Percent, false);
        assert_eq!(format_scaled_tick(pct, mark(0.004, 0.001), 100.0, None), "0.00%");
    }

    #[test]
    fn pane_ticks_follow_their_step() {
        assert_eq!(format_study_tick(50.0, 10.0), "50.0");
        assert_eq!(format_study_tick(-1.0e-7, 1.0e-7), "-0.0000001");
        assert_eq!(format_compact_tick(950.0, 100.0), "950");
        assert_eq!(format_compact_tick(1_200.0, 100.0), "1.2K");
        // Fractional volumes: compact alone prints 0, 0, 1.
        assert_eq!(format_compact_tick(0.3, 0.1), "0.3");
        assert_eq!(format_compact_tick(0.5, 0.5), "0.5");
    }
}
