//! Number/date formatting for the chart. The pure number formatters (thousands grouping + the
//! compact volume label) now live in the shared `vike_ui_theme::fmt` leaf crate and are imported
//! here so the crate-internal call sites are unchanged; only the chart-SPECIFIC formatters — the
//! scale-aware axis/chip label formatter and the crosshair datetime — stay local, since they
//! depend on this crate's `ScaleMode`/`ScaleView`/`DisplayTz`. Bodies are verbatim.

use crate::scale::{ScaleMode, ScaleView};
use crate::tz::DisplayTz;

// The pure number formatters, canonicalized in vike-ui-theme (F35 dedup). All three are crate-local
// now. `mod fmt` is PRIVATE inside `chart`, so `fmt_thousands`'s old `pub` reached the outside only
// through the `pub use fmt::fmt_thousands` in `crates/vike-chart/src/chart/mod.rs` — and that alias
// had no consumer, so it went with it. Readers inside `chart` (`overlay.rs`, `fmt_scaled_view`
// below) are unaffected; everyone else spells `vike_ui_theme::fmt::fmt_thousands`, which is what
// every call site outside this module already did.
pub(crate) use vike_ui_theme::fmt::{fmt_compact, fmt_thousands, fmt_thousands_prec};

pub(crate) fn fmt_datetime(ot: i64, tz: DisplayTz) -> String {
    crate::tz::to_naive(ot, tz).map(|dt| dt.format("%m-%d %H:%M").to_string()).unwrap_or_default()
}

/// Format a MAPPED plot-space y-value as its axis/chip/tag label (chart-UX
/// bundle T2 §1): Percent shows the mapped percent number directly (fixed 2
/// decimals + '%'); Linear/Log unmap back to the raw price and reuse
/// `fmt_thousands` (comma-grouped). The §3 precision override (`prec = Some(p)`)
/// forces `p` fixed decimals for Linear/Log only — Percent stays `{:.2}%`
/// regardless (T6). NOT used for the OHLC legend — those stay raw price
/// readouts regardless of scale mode (data readouts, not positions).
pub(crate) fn fmt_scaled(mode: ScaleMode, mapped: f64, anchor: f64, prec: Option<u8>) -> String {
    fmt_scaled_view(ScaleView::new(mode, false), mapped, anchor, prec)
}

/// [`fmt_scaled`] over a full [`ScaleView`]: identical to the bare-mode version
/// when `invert == false`, and additionally handles the two new options —
/// **Invert** (the incoming `mapped` is in flipped plot-space, so it is un-flipped
/// via `view.flip` before formatting/unmapping) and **Indexed** (shows the mapped
/// index number directly, plain — around 100, no `%` — honoring the §3 precision
/// override like Linear/Log, unlike Percent's fixed `{:.2}%`).
pub(crate) fn fmt_scaled_view(
    view: ScaleView,
    mapped: f64,
    anchor: f64,
    prec: Option<u8>,
) -> String {
    // Un-flip first: every readback below wants the value in TRUE mapped space.
    let mapped = view.flip(mapped);
    match view.mode {
        ScaleMode::Percent => format!("{mapped:.2}%"),
        ScaleMode::Indexed => match prec {
            Some(p) => fmt_thousands_prec(mapped, p as usize),
            None => fmt_thousands(mapped),
        },
        ScaleMode::Linear | ScaleMode::Log => {
            let raw = view.mode.unmap(mapped, anchor);
            match prec {
                Some(p) => fmt_thousands_prec(raw, p as usize),
                None => fmt_thousands(raw),
            }
        }
    }
}
