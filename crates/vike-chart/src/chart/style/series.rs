//! Per-series style helpers split out of `chart.rs` (chart refactor PR-1):
//! the Heikin-Ashi transform (streaming per-bar recurrence + the batch
//! wrapper, both named by `state/mod.rs`, over `vike_math::price_transforms`'
//! law) and the volume-pane style predicate.
//!
//! Chart refactor PR-2 (Block A) added [`resolve_series`]: the pure resolution
//! of this frame's render series + x-axis mark inputs that used to sit inline at
//! the top of [`crate::chart::draw`].

use crate::chart::state::ChartState;
use crate::chart::state::bar::ChartBar;
use crate::chart::state::marks::{ChartTimeGrain, day_mark_indices, grain, hour_mark_indices};
use crate::chart::style::ChartStyle;
use crate::chart::style::transforms::ChartStyleTransformParams;
use std::rc::Rc;
use vike_math::price_transforms::{average_price, heikin_ashi_open, heikin_ashi_range};
use vike_ui_theme::time_zone::ThemeDisplayTimeZone;

use crate::chart::axis::{forming_day_mark, forming_hour_mark};

/// One Heikin-Ashi bar from the previous HA bar (`None` for the first bar) + the raw bar.
/// The recurrence is pure per-bar — `out[i]` depends only on `out[i-1]` and `bars[i]` — which is
/// exactly what lets [`crate::chart::state::ChartState::transformed_shared`] cache the closed-prefix HA series
/// and append only the single forming HA bar each frame (chart-perf T6). The law is
/// `vike_math::price_transforms`' (the indicators' `heikin_ashi` runs it too), bit for bit.
pub(crate) fn heikin_ashi_bar(previous: Option<&ChartBar>, b: &ChartBar) -> ChartBar {
    let ha_close = average_price(b.o, b.h, b.l, b.c);
    let ha_open = heikin_ashi_open(previous.map(|p| (p.o, p.c)), b.o, b.c);
    let (h, l) = heikin_ashi_range(b.h, b.l, ha_open, ha_close);
    ChartBar { t: b.t, ot: b.ot, o: ha_open, h, l, c: ha_close, v: b.v }
}

pub(crate) fn heikin_ashi(bars: &[ChartBar]) -> Vec<ChartBar> {
    let mut out: Vec<ChartBar> = Vec::with_capacity(bars.len());
    for b in bars {
        let previous = out.last().copied();
        out.push(heikin_ashi_bar(previous.as_ref(), b));
    }
    out
}

/// Styles whose rendered series stays index-aligned with the raw bars (no
/// Renko/Kagi-style reindexing), so a volume pane can show per-bar volume.
pub(crate) fn style_preserves_volume(style: ChartStyle) -> bool {
    !matches!(
        style,
        ChartStyle::Renko
            | ChartStyle::Range
            | ChartStyle::LineBreak
            | ChartStyle::Kagi
            | ChartStyle::PointFigure
    )
}

/// This frame's RENDER SERIES + x-axis mark inputs (chart refactor PR-2, Block A),
/// resolved once at the top of [`crate::chart::draw`]. Bundles the read-only values
/// the old inline block produced.
///
/// OWNERSHIP (why the borrows aren't fields): `draw`'s `series`/`marks_ref`/
/// `day_marks_ref` are `&[..]` pointing into EITHER these owned buffers (transform
/// styles) OR `state`'s cached slices (raw styles). A struct can't hold both an
/// owned `Rc<Vec<ChartBar>>`/`Vec<f64>` AND a borrow into it (self-reference), so this
/// owns the transform buffers and `draw` rebuilds the three refs from them + `state`
/// after destructuring — byte-identical to the inline `match &owned_marks { .. }`.
///
/// PERF (load-bearing): `owned` comes from [`ChartState::transformed_shared`] — the
/// SAME two-tier O(delta) cache accessor the pre-extraction block called, returning
/// its `Rc` — so the incremental render path is untouched and Kagi/PnF's structured
/// results stay populated for the later `cached_{kagi,pnf}_shared` reads.
pub(crate) struct ChartStyleRenderSeries {
    /// The transformed proxy series (`Rc` from the shared cache) for a transform
    /// style; `None` for raw styles (`draw` renders `state.bars` directly). Doubles
    /// as the "is this a transform style" flag `draw` reads (`owned.is_none()`).
    pub(crate) owned: Option<Rc<Vec<ChartBar>>>,
    /// Hour-grid marks recomputed over `owned` for a transform style (its bar
    /// indices differ from the raw series); `None` ⇒ `draw` reads `state.hour_marks`.
    pub(crate) owned_marks: Option<Vec<f64>>,
    /// Day-divider marks recomputed over `owned` for a transform style; `None` ⇒
    /// `draw` reads `state.day_marks`.
    pub(crate) owned_day_marks: Option<Vec<f64>>,
    /// Forming-bar hour mark — `None` for a transform style (whose marks are whole-
    /// series recomputes with no separate forming probe), else the O(1) probe.
    pub(crate) forming_mark: Option<f64>,
    /// Forming-bar day mark — mirrors `forming_mark`.
    pub(crate) forming_day: Option<f64>,
    /// Time grain (SubMinute/Minute/HourPlus) from the closed-prefix median spacing.
    pub(crate) gr: ChartTimeGrain,
    /// Median index-gap between hour marks (the egui_plot grid-step hint).
    pub(crate) hs: f64,
    /// Median index-gap between day marks.
    pub(crate) day_step: f64,
}

/// Resolve [`ChartStyleRenderSeries`] for `style` (chart refactor PR-2, Block A) — a verbatim
/// extraction of `draw`'s transform-series + mark-input block. `time_zone` MUST be
/// `state.time_zone()` (draw's single-source-of-truth binding for the frame's display time_zone);
/// it is passed explicitly to keep this a pure function of its arguments.
pub(crate) fn resolve_series(
    state: &ChartState,
    style: ChartStyle,
    time_zone: ThemeDisplayTimeZone,
) -> ChartStyleRenderSeries {
    use ChartStyle::*;
    // Transform styles proxy through the two-tier `transformed_shared` cache (the
    // O(delta) render path); raw styles render `state.bars`. Params are the fixed
    // LineBreak-3 / PnF-reversal-3 the inline block used.
    let owned: Option<Rc<Vec<ChartBar>>> =
        matches!(style, HeikinAshi | Renko | Range | LineBreak | Kagi | PointFigure).then(|| {
            state.transformed_shared(
                style,
                ChartStyleTransformParams { line_break_n: 3, point_figure_reversal: 3 },
            )
        });
    // Transform styles recompute their hour/day marks over the (reindexed) proxy;
    // raw styles use `state`'s cached marks + an O(1) forming-bar probe. `forming_*`
    // is `None` exactly when the corresponding `owned_*` is `Some` (the transform
    // marks already span the whole proxy) — the same coupling the old match encoded.
    let owned_marks: Option<Vec<f64>> = owned.as_deref().map(|v| hour_mark_indices(v, time_zone));
    let owned_day_marks: Option<Vec<f64>> =
        owned.as_deref().map(|v| day_mark_indices(v, time_zone));
    let forming_mark: Option<f64> =
        if owned_marks.is_some() { None } else { forming_hour_mark(state) };
    let forming_day: Option<f64> =
        if owned_day_marks.is_some() { None } else { forming_day_mark(state) };
    ChartStyleRenderSeries {
        owned,
        owned_marks,
        owned_day_marks,
        forming_mark,
        forming_day,
        gr: grain(state.median_secs()),
        hs: state.hour_step(),
        day_step: state.day_step(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::make_state;
    use crate::testutil::wave;

    #[test]
    fn volume_pane_only_for_index_preserving_styles() {
        use ChartStyle::*;
        for s in [
            Candles,
            Hollow,
            HeikinAshi,
            VolumeCandles,
            Bars,
            HlcBars,
            HighLow,
            Line,
            LineMarkers,
            StepLine,
            Area,
            Baseline,
            HlcArea,
            Columns,
            Footprint,
        ] {
            assert!(style_preserves_volume(s), "{}", s.label());
        }
        for s in [Renko, Range, LineBreak, Kagi, PointFigure] {
            assert!(!style_preserves_volume(s), "{}", s.label());
        }
    }

    // --- chart refactor PR-2 (Block A): resolve_series ---

    #[test]
    fn resolve_series_raw_style_uses_state_bars_and_marks() {
        let s = make_state(wave(40));
        let rs = resolve_series(&s, ChartStyle::Candles, s.time_zone());
        // Candles is NOT a transform style ⇒ no owned proxy / no recomputed marks.
        assert!(rs.owned.is_none(), "candles renders state.bars directly");
        assert!(rs.owned_marks.is_none());
        assert!(rs.owned_day_marks.is_none());
        // The grid-step hints mirror the state getters (verbatim carry-over).
        assert_eq!(rs.gr, grain(s.median_secs()));
        assert_eq!(rs.hs, s.hour_step());
        assert_eq!(rs.day_step, s.day_step());
        // All-closed fixture ⇒ no forming bar ⇒ the O(1) probes yield None.
        assert!(rs.forming_mark.is_none());
        assert!(rs.forming_day.is_none());
    }

    #[test]
    fn resolve_series_heikin_ashi_applies_the_transform() {
        let s = make_state(wave(40));
        let rs = resolve_series(&s, ChartStyle::HeikinAshi, s.time_zone());
        let owned = rs.owned.expect("HeikinAshi is a transform style ⇒ owned proxy");
        // The proxy is exactly the HA transform of the raw bars.
        assert_eq!(&*owned, &heikin_ashi(&s.bars));
        // Transform styles recompute marks over the proxy and carry no forming probe.
        assert!(rs.owned_marks.is_some());
        assert!(rs.owned_day_marks.is_some());
        assert!(rs.forming_mark.is_none());
        assert!(rs.forming_day.is_none());
    }

    #[test]
    fn resolve_series_renko_is_a_reindexing_transform() {
        let s = make_state(wave(60));
        let rs = resolve_series(&s, ChartStyle::Renko, s.time_zone());
        // Renko reindexes ⇒ an owned proxy is produced (and its marks recomputed).
        assert!(rs.owned.is_some(), "Renko is a transform style");
        assert!(rs.owned_marks.is_some());
    }
}
