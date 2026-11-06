//! `capture_seed` — the SYNTHETIC-STATE capture hook for the chart's drawing layer
//! (`VIKE_CHART_DRAW`): the one decision a marketing/QA capture needs that no other knob in the
//! family can reach, because it is about *content* rather than about which window is open.
//!
//! The rest of the capture vocabulary ([`crate::ui::startup`]'s `VIKE_SHOT_WIN`,
//! [`crate::ui::initial_arrange`]'s `VIKE_TOOL`/`VIKE_TOOLS`) decides WHICH surface renders. That
//! is enough for a surface whose content is a live feed or a local store, and it is NOT enough for
//! the chart's drawing layer: `.trader/shots/manifest.json` records it as a `capture_gap`.
//!
//! **Data in, decisions out** — the shape [`crate::ui::startup`] established and
//! [`crate::ui::initial_arrange`] repeated. Every function here is PURE: the already-loaded market
//! data in, a polyline out. `vike-desktop` performs the `ChartState` write and owns the
//! process-environment read, so no `Layer::Library` row joins `vike_config::settings`'
//! `LIBRARY_PIN` ratchet — the same reason the seven `initial_arrange` knobs are read in the binary.
//!
//! ## Why this hook writes the chart's state directly
//!
//! [`apply_drawings`] writes straight into `vike_chart::model::ChartState::overlays` because
//! there is nothing else it could do: that field is the chart's user-drawing layer, it is READ by
//! `crates/vike-chart/src/chart/price_render.rs`'s `paint_price_overlays` and folded into the
//! autofit extent by `chart::draw`, and **nothing in this workspace writes it**. ⚠ That is worth
//! reading twice, because `.trader/shots/manifest.json` and `scripts/marketing_shots.sh` both
//! once described the gap as "the drawing tools ... are click-only". They are not click-only; they
//! do not exist. `overlays` is a rendering surface with no producer, so a capture hook is not
//! reaching past a UI it could have driven — it is the FIRST producer.
//!
//! ## It is OFF unless asked for
//!
//! The knob has no default: an unset environment leaves the shell byte-identical, with no
//! overlay. See `crates/vike-desktop/src/main.rs`'s `CHART_DRAW_ENV` read.
//!
//! ⚠ TOMBSTONE — a second hook stood here, `VIKE_TRADE_SEED`, which was to submit two PAPER
//! orders so an Account-window capture showed a session. It could never submit, and it was
//! removed (2026-10-24) rather than kept "correct and tested": the desktop signs nothing and has
//! no local core (rulings 1 and 2 of the desktop split, 2026-09-09), so an order leaves the shell
//! only down a remote `Scope::Write` channel, and the hook's own guard ladder refuses outright
//! whenever one is mounted — the only time its frame-loop block ran. Seeding an order down that
//! channel would reach the remote node's venues, which a screenshot knob must never do, and the
//! alternative is no better: *"A screenshot of a fabricated `CoreSnapshot` would be a picture of
//! a code path that does not exist, which for the panel that shows a human their money is the
//! worst thing to publish."* So the poses that wanted it (`pipeline-3-golive.png`,
//! `grid-tearsheet.png`) have no way to show a working order and an open position; their
//! `.trader/shots/manifest.json` rows say so.

use vike_chart::model::Bar;

/// The `ChartState::overlays` key the trendline is written under.
///
/// A NAMED key, not an index, because `overlays` is a `BTreeMap`: re-writing under the same name
/// replaces rather than accumulates, so a hook that fired twice cannot stack sixty trendlines on
/// one chart. The name also reaches the render — `paint_price_overlays` passes it to
/// `egui_plot::Line::new` — so it says what it is rather than something like `"0"`.
pub const TREND_OVERLAY: &str = "capture · trend";

/// ...and the horizontal one. Two drawings rather than one because the manifest's `state` clause
/// for `grid-charting.png` asks for "an overlay/drawing visible" on a tile whose CLAIM is
/// multi-pane analysis: a lone diagonal reads as an indicator, a diagonal plus a level reads as
/// somebody having marked up a chart, which is what the claim is about.
pub const SUPPORT_OVERLAY: &str = "capture · support";

/// One capture drawing: the `ChartState::overlays` key it is written under, and its polyline in
/// the chart's own `[x, price]` plot space.
///
/// A named alias rather than the tuple spelled at the signature — `clippy::type_complexity` is a
/// merge gate here and refused the inline form, which is the right call: the pair is the unit
/// shell's writer (`vike-desktop`'s) loops over, so it deserves a name that says what it is.
pub type CaptureDrawing = (&'static str, Vec<[f64; 2]>);

/// The minimum number of bars a drawing is derived from. Below this the extremes are noise — two
/// bars produce a "trendline" between the only two points there are, which is a line through the
/// data rather than a reading of it.
const MIN_DRAW_BARS: usize = 8;

/// Derive the two capture drawings from `bars`, or `None` when there is not enough loaded series
/// to draw anything honest.
///
/// The geometry is deliberately the simplest reading a human would make and is a pure function of
/// the loaded bars, so the same series always yields the same picture:
///
///   * `TREND_OVERLAY` — the line joining the LOWEST low to the HIGHEST high, ordered by x so the
///     polyline runs left-to-right whichever extreme came first.
///   * `SUPPORT_OVERLAY` — a horizontal level at that lowest low, spanning the whole loaded range.
///
/// Points are `[x, price]` in the chart's own plot space: `x` is `Bar::t`, the BAR INDEX the
/// price pane plots against, taken from the bar rather than from the slice position — the two
/// agree today and only the bar's own field is guaranteed to keep agreeing, and
/// `paint_price_overlays` filters these points against the visible INDEX bounds.
///
/// `None` (fewer than [`MIN_DRAW_BARS`] bars, or a series carrying no finite extreme) writes
/// nothing at all, which leaves the chart byte-identical — the caller must not substitute an
/// empty polyline, because `paint_price_overlays` skips a `pts.len() < 2` entry and a
/// zero-length entry would sit in the map looking like a drawing that failed to render.
#[must_use]
pub fn trendline_overlay(bars: &[Bar]) -> Option<Vec<CaptureDrawing>> {
    if bars.len() < MIN_DRAW_BARS {
        return None;
    }
    let finite = |b: &&Bar| b.l.is_finite() && b.h.is_finite() && b.t.is_finite();
    let lo = bars.iter().filter(finite).min_by(|a, b| a.l.total_cmp(&b.l)).map(|b| (b.t, b.l))?;
    let hi = bars.iter().filter(finite).max_by(|a, b| a.h.total_cmp(&b.h)).map(|b| (b.t, b.h))?;
    // Degenerate series (every bar the same price) — the "trendline" would be a point, and the
    // support level says nothing. Draw nothing rather than a dot.
    if lo.1 >= hi.1 {
        return None;
    }
    let mut trend = vec![[lo.0, lo.1], [hi.0, hi.1]];
    trend.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let (x0, x1) = (bars.first()?.t, bars.last()?.t);
    Some(vec![(TREND_OVERLAY, trend), (SUPPORT_OVERLAY, vec![[x0, lo.1], [x1, lo.1]])])
}

/// Write the capture drawings onto one chart's state, reporting whether anything was drawn.
///
/// The caller's whole job is the loop over its chart map and the one-shot flag; this owns the
/// "derive, then insert under the stable names" half so the insert cannot drift from the derive.
/// `false` — the series is too short, or too flat, to draw honestly ([`trendline_overlay`]) —
/// leaves `overlays` untouched rather than inserting an empty polyline, which the render would
/// silently skip and a reader would mistake for a drawing that failed.
pub fn apply_drawings(state: &mut vike_chart::model::ChartState) -> bool {
    let Some(drawings) = trendline_overlay(&state.bars) else { return false };
    for (name, pts) in drawings {
        state.overlays.insert(name.to_string(), pts);
    }
    true
}

#[path = "capture_seed_tests.rs"]
#[cfg(test)]
mod capture_seed_tests;
