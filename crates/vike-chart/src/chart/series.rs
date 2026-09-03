//! Per-series style helpers split out of `chart.rs` (chart refactor PR-1):
//! the Heikin-Ashi transform (streaming per-bar recurrence + the batch
//! wrapper, both re-exported from `chart` for `model.rs`) and the
//! volume-pane style predicate. Bodies are verbatim.
//!
//! Chart refactor PR-2 (Block A) added [`resolve_series`]: the pure resolution
//! of this frame's render series + x-axis mark inputs that used to sit inline at
//! the top of [`crate::chart::draw`].

use crate::chart::ChartStyle;
use crate::model::{
    day_mark_indices, grain, hour_mark_indices, Bar, ChartState, TimeGrain, TransformParams,
};
use crate::tz::DisplayTz;
use std::rc::Rc;

use super::marks::{forming_day_mark, forming_hour_mark};

/// One Heikin-Ashi bar from the previous HA bar (`None` for the first bar) + the raw bar.
/// The recurrence is pure per-bar — `out[i]` depends only on `out[i-1]` and `bars[i]` — which is
/// exactly what lets [`crate::model::ChartState::transformed`] cache the closed-prefix HA series
/// and append only the single forming HA bar each frame (chart-perf T6). Byte-identical to the
/// old inline body; keep the two in lockstep.
pub(crate) fn heikin_ashi_bar(prev: Option<&Bar>, b: &Bar) -> Bar {
    let ha_close = (b.o + b.h + b.l + b.c) / 4.0;
    let ha_open = match prev {
        None => (b.o + b.c) / 2.0,
        Some(p) => (p.o + p.c) / 2.0,
    };
    Bar {
        t: b.t,
        ot: b.ot,
        o: ha_open,
        h: b.h.max(ha_open).max(ha_close),
        l: b.l.min(ha_open).min(ha_close),
        c: ha_close,
        v: b.v,
    }
}

pub(crate) fn heikin_ashi(bars: &[Bar]) -> Vec<Bar> {
    let mut out: Vec<Bar> = Vec::with_capacity(bars.len());
    for b in bars {
        let prev = out.last().copied();
        out.push(heikin_ashi_bar(prev.as_ref(), b));
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
/// owned `Rc<Vec<Bar>>`/`Vec<f64>` AND a borrow into it (self-reference), so this
/// owns the transform buffers and `draw` rebuilds the three refs from them + `state`
/// after destructuring — byte-identical to the inline `match &owned_marks { .. }`.
///
/// PERF (load-bearing): `owned` comes from [`ChartState::transformed_shared`] — the
/// SAME two-tier O(delta) cache accessor the pre-extraction block called, returning
/// its `Rc` — so the incremental render path is untouched and Kagi/PnF's structured
/// results stay populated for the later `cached_{kagi,pnf}_shared` reads.
pub(crate) struct RenderSeries {
    /// The transformed proxy series (`Rc` from the shared cache) for a transform
    /// style; `None` for raw styles (`draw` renders `state.bars` directly). Doubles
    /// as the "is this a transform style" flag `draw` reads (`owned.is_none()`).
    pub(crate) owned: Option<Rc<Vec<Bar>>>,
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
    /// Time grain (Sub60/Minute/HourPlus) from the closed-prefix median spacing.
    pub(crate) gr: TimeGrain,
    /// Median index-gap between hour marks (the egui_plot grid-step hint).
    pub(crate) hs: f64,
    /// Median index-gap between day marks.
    pub(crate) ds: f64,
}

/// Resolve [`RenderSeries`] for `style` (chart refactor PR-2, Block A) — a verbatim
/// extraction of `draw`'s transform-series + mark-input block. `tz` MUST be
/// `state.tz()` (draw's single-source-of-truth binding for the frame's display tz);
/// it is passed explicitly to keep this a pure function of its arguments.
pub(crate) fn resolve_series(state: &ChartState, style: ChartStyle, tz: DisplayTz) -> RenderSeries {
    use ChartStyle::*;
    // Transform styles proxy through the two-tier `transformed_shared` cache (the
    // O(delta) render path); raw styles render `state.bars`. Params are the fixed
    // LineBreak-3 / PnF-reversal-3 the inline block used.
    let owned: Option<Rc<Vec<Bar>>> =
        matches!(style, HeikinAshi | Renko | Range | LineBreak | Kagi | PointFigure).then(|| {
            state.transformed_shared(style, TransformParams { line_break_n: 3, pnf_reversal: 3 })
        });
    // Transform styles recompute their hour/day marks over the (reindexed) proxy;
    // raw styles use `state`'s cached marks + an O(1) forming-bar probe. `forming_*`
    // is `None` exactly when the corresponding `owned_*` is `Some` (the transform
    // marks already span the whole proxy) — the same coupling the old match encoded.
    let owned_marks: Option<Vec<f64>> = owned.as_deref().map(|v| hour_mark_indices(v, tz));
    let owned_day_marks: Option<Vec<f64>> = owned.as_deref().map(|v| day_mark_indices(v, tz));
    let forming_mark: Option<f64> =
        if owned_marks.is_some() { None } else { forming_hour_mark(state) };
    let forming_day: Option<f64> =
        if owned_day_marks.is_some() { None } else { forming_day_mark(state) };
    RenderSeries {
        owned,
        owned_marks,
        owned_day_marks,
        forming_mark,
        forming_day,
        gr: grain(state.median_secs()),
        hs: state.hour_step(),
        ds: state.day_step(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A gently-varying, all-closed OHLCV series (deterministic) — safe for every
    /// style incl. the path-dependent transforms. Mirrors the characterization
    /// harness's `wave_bars`.
    fn wave(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let base = 100.0 + (i as f64 * 0.30).sin() * 6.0;
                Bar {
                    t: i as f64,
                    ot: 1_700_000_000_000 + i as i64 * 60_000,
                    o: base,
                    h: base + 2.5,
                    l: base - 2.5,
                    c: base + (i as f64 * 0.7).cos(),
                    v: 10.0 + (i % 7) as f64,
                }
            })
            .collect()
    }

    /// Fully-closed [`ChartState`] with the caches (`y_ext`/marks/steps) seeded.
    fn st(bars: Vec<Bar>) -> ChartState {
        let mut s = ChartState::default();
        let n = bars.len();
        s.bars = bars;
        s.closed_len = n;
        s.refresh_caches();
        s
    }

    #[test]
    fn resolve_series_raw_style_uses_state_bars_and_marks() {
        let s = st(wave(40));
        let rs = resolve_series(&s, ChartStyle::Candles, s.tz());
        // Candles is NOT a transform style ⇒ no owned proxy / no recomputed marks.
        assert!(rs.owned.is_none(), "candles renders state.bars directly");
        assert!(rs.owned_marks.is_none());
        assert!(rs.owned_day_marks.is_none());
        // The grid-step hints mirror the state getters (verbatim carry-over).
        assert_eq!(rs.gr, grain(s.median_secs()));
        assert_eq!(rs.hs, s.hour_step());
        assert_eq!(rs.ds, s.day_step());
        // All-closed fixture ⇒ no forming bar ⇒ the O(1) probes yield None.
        assert!(rs.forming_mark.is_none());
        assert!(rs.forming_day.is_none());
    }

    #[test]
    fn resolve_series_heikin_ashi_applies_the_transform() {
        let s = st(wave(40));
        let rs = resolve_series(&s, ChartStyle::HeikinAshi, s.tz());
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
        let s = st(wave(60));
        let rs = resolve_series(&s, ChartStyle::Renko, s.tz());
        // Renko reindexes ⇒ an owned proxy is produced (and its marks recomputed).
        assert!(rs.owned.is_some(), "Renko is a transform style");
        assert!(rs.owned_marks.is_some());
    }
}
