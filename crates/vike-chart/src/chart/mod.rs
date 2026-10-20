//! Chart panel — vike-style PriceChart: right price axis (thousands + price-tag
//! chip), time x-axis with hourly grid, dashed last-price line, crosshair with a
//! time tag, bottom nav buttons (− + ‹ › ⟳) + Auto, and a chart-STYLE selector.
//!
//! Orchestrates the price/volume/oscillator panes and their bounds/interaction
//! (follow-live-edge + visible-range y-autofit live in `interact.rs`); the
//! per-style series painters + indicator overlay/oscillator rendering live in
//! `render.rs`.

use crate::indicators::Active;
use crate::interact::follow_engaged;
use crate::model::{Bar, ChartState};
use crate::scale::{self, ScaleAssign, ScaleMode, ScaleView};
use crate::sync::{SyncIn, nearest_index_by_ts};
use egui::{FontId, Stroke};
use egui_plot::{GridInput, GridMark, Plot};
use std::cell::Cell;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::chart;

// --- chart refactor PR-1: the free helpers + consts that used to live inline in
// this file now sit in topical submodules under `chart/`. `draw` (below) and the
// retained public types are unchanged; only these `mod`/`use` lines are new. ---
mod canvas;
mod consts;
mod controls;
mod dialogs;
mod extent;
mod fmt;
pub mod harness;
mod marks;
mod overlay;
mod panes_layout;
mod price_pane;
mod price_render;
mod series;
pub mod style;
mod subpanes;

use canvas::paint_canvas_background;
use consts::Y_TICK_BUDGET;
use controls::{nav_row, scale_row, x_gutter, y_gutter};
use dialogs::{indicator_settings_dialog, settings_dialog_and_opts};
use extent::{Bounds, default_bounds, visible_ts_bounds};
use fmt::{fmt_scaled, fmt_scaled_view};
use marks::{XAxisCtx, attach_shared_x_axis};
use overlay::{
    paint_crosshair, paint_ghost_crosshair, paint_last_price_chip, paint_ohlc_legend,
    paint_price_tag, paint_scale_fallback_hint, paint_time_tag,
};
use panes_layout::{PaneLayout, pane_separator, resolve_pane_layout};
use price_pane::{PriceOut, PricePane, SubPaneOut, draw_sub_panes, price_pane};
use series::{RenderSeries, resolve_series};
use style::ChartStyle;

// Public API preserved verbatim across the split: `crate::chart::{heikin_ashi,
// heikin_ashi_bar}` (model.rs) must stay reachable at these exact paths.
//
// A `pub use fmt::fmt_thousands;` used to stand beside it, kept "reachable (vike-app)".
// That reason had already gone stale: the vike-app bodies moved into
// `crates/vike-app-core/src/ui/tool_views/data.rs` and
// `crates/vike-app-core/src/ui/tool_views/stored.rs`, and both spell
// `vike_ui_theme::fmt::fmt_thousands`. This crate's own last caller,
// `crates/vike-chart/src/options_chain.rs`, now names the leaf crate directly too, so the
// alias had no consumer at all and is gone.
pub(crate) use series::{heikin_ashi, heikin_ashi_bar};

// ⚠ `FollowLive`, the options and dialog types, the pane keys and targets and `ScaleAssign` are
// NOT re-exported here: each has ONE path, its own public module's (`crate::interact`,
// `crate::options`, `crate::panes`, `crate::scale`) or the crate-root vocabulary in
// `crates/vike-chart/src/lib.rs`. A `pub use` block here once gave them a third path, and
// `ScaleAssign`'s callers had split between `chart::` and `scale::`.
use crate::interact::FollowLive;
use crate::options::{ChartOptions, IndicatorDialog, IndicatorEdit, SettingsDialog};
use crate::panes::{MoveTarget, PaneFractions, PaneKey};

// `draw_style_icon` is not a second path: `render::style_icon` is crate-private, so `chart::` is
// its only public path (the desktop's style menu).
pub use crate::render::style_icon::draw_style_icon;

#[derive(Clone, Copy)]
pub enum Nav {
    ZoomIn,
    ZoomOut,
    PanLeft,
    PanRight,
    Reset,
    /// Final-review fix: the price-scale "Auto" button — Y-ONLY re-fit (the
    /// TradingView oracle). Unlike `Reset`, must NOT touch cx0/cx1 or
    /// `follow.on`; see the match arm below.
    AutoY,
}

/// One borrowed compare-series input (C2a multi-symbol): a second symbol's
/// render model overlaid on the PRICE pane as a %-normalized line. The `state`
/// is looked up (never owned) exactly like the primary's — `charts.get(key)` in
/// vike-desktop — and stays single-series (the one-venue-per-`ChartState` cache
/// contract holds). `symbol` is carried explicitly because `ChartState.symbol`
/// is write-only/unreliable for display; `color` is the per-series line color.
pub struct SeriesInput<'a> {
    pub symbol: &'a str,
    pub state: &'a ChartState,
    pub color: egui::Color32,
}

/// Inputs to [`draw`] — the window's chart-local state + this frame's pending
/// nav/style/indicators + the mutable follow-live-edge state (chart-UX bundle T0).
pub struct ChartInputs<'a> {
    pub state: &'a ChartState,
    pub style: ChartStyle,
    pub nav: Option<Nav>,
    pub indicators: &'a [Active],
    /// Tick-driven microstructure studies ([`crate::studies::ActiveStudy`] — VPIN,
    /// depth/weighted book imbalance, order-to-trade ratio), each rendered into the
    /// oscillator sub-pane it self-keys ([`crate::studies::ActiveStudy::pane_key`],
    /// which shares `indicators`' uid space so a study and an indicator can even share
    /// one pane). Unlike `indicators` these advance on TICKS, not bars: the app folds
    /// trades/books into them ([`crate::studies::ActiveStudy::on_trade`]/`on_book`) and
    /// calls `sync(closed_len, forming)` once per frame, which is what leaves the
    /// series bar-indexed like every other oscillator. Read-only here.
    ///
    /// **`&[]` is the default at every build site and is byte-identical to the
    /// pre-studies render**: the pane gate in `resolve_pane_layout` gains a disjunct
    /// that is `false` over an empty slice, and the sub-pane loop's per-study fold /
    /// render / legend loops all iterate nothing. A book-dependent study with no L2
    /// feed reports [`crate::studies::ActiveStudy::is_empty`] and is skipped by BOTH
    /// the gate and the render — its pane never opens, and no fabricated value is
    /// ever drawn. Use [`crate::studies::push_study_panes`] to author these into
    /// `sub_panes`.
    pub studies: &'a [crate::studies::ActiveStudy],
    pub follow: &'a mut FollowLive,
    /// The window's COMMITTED chart appearance/behavior (chart-UX bundle T6).
    /// Colors/flags/precision/volume-toggle all live here (the volume pane's
    /// old `show_volume` bool was absorbed). The EFFECTIVE options this frame
    /// are `settings.working` while the dialog is open (live preview), else
    /// this; the caller applies [`ChartActions::options_change`] back onto it.
    pub options: &'a ChartOptions,
    /// The window's "Chart settings" dialog state (chart-UX bundle T6). Held
    /// across frames by the caller, mutated here (open/close + the working
    /// copy the widgets edit). The `egui::Window` is drawn inside [`draw`].
    pub settings: &'a mut SettingsDialog,
    /// The window's "Indicator settings" dialog state (chart-UX bundle T8). Held
    /// across frames by the caller, mutated here (open/close + the working/snapshot
    /// edit copies). The entry points (an oscillator pane-header ⚙ here, plus the
    /// ƒx-picker per-row ⚙ in vike-app-core) set `open_uid`; this dialog seeds the edit
    /// copies and renders the `egui::Window`, signalling live-preview edits back via
    /// [`ChartActions::indicator_edit`] (the `indicators` slice itself stays
    /// immutable — the T0 contract). `indicators` is read-only here.
    pub indicator_dialog: &'a mut IndicatorDialog,
    /// Requested price-scale mode (chart-UX bundle T2/T3). May be downgraded
    /// for this frame by the sticky fallback (see
    /// [`ChartActions::scale_fallback`]) when the visible data can't support
    /// it. Set by the caller from its own persisted state; the caller applies
    /// [`ChartActions::scale_change`] back onto that state.
    pub scale: ScaleMode,
    /// TradingView "Invert scale": flip the price axis vertically. An ORTHOGONAL
    /// modifier (not a mode) — usable on top of any `scale` above. Persisted per
    /// window by the caller; the caller applies [`ChartActions::invert_change`]
    /// back onto that state. `false` ⇒ the whole render path is byte-identical to
    /// pre-invert (every scale seam delegates to the bare `ScaleMode`).
    pub invert: bool,
    /// Per-window, identity-keyed sub-pane height fractions (chart-UX bundle
    /// T9), replacing the old uniform `osc_h`/`price_h` clamp formula. Held
    /// across frames by the caller (`WinState::panes`) and mutated here: a
    /// never-before-seen pane id is pinned to a default share, a hidden pane
    /// (e.g. Volume toggled off) keeps its stored share untouched, and a
    /// separator drag transfers height between exactly the two adjacent
    /// panes. See `panes.rs` for the full contract.
    pub panes: &'a mut PaneFractions,
    /// Cross-window sync input (chart sync seam, task B7): a ghost crosshair
    /// position and/or an injected visible range, both timestamp-keyed (bar
    /// indices don't line up across windows on different symbols/intervals —
    /// `ot` does). `None` for a standalone window AND for the sync LEADER
    /// itself (the caller must never echo a window's own emitted
    /// `ChartActions::hover_ts`/`visible_ts` back into its own next-frame
    /// `sync` — that wiring lives in the app, task B8). See `sync.rs` for the
    /// pure resolution helpers this draws on.
    pub sync: Option<SyncIn>,
    /// Per-bar footprint substrate (SP2), index-aligned to `state.bars` — the single
    /// orderflow input CVD/profile/footprint are all derived from chart-side (see
    /// `orderflow.rs`). `None` when the app hasn't aggregated trades for this chart
    /// (default-off: no orderflow rendering happens without this).
    pub footprint: Option<&'a [vike_orderflow::FootprintBar]>,
    /// Monotonic generation of `footprint` (SP3 TB-fix — post-Task-B review finding, MEDIUM):
    /// the shell's `vike_orderflow::OrderflowAgg::generation()`, bumped every time that
    /// aggregator's footprint cache is actually rebuilt. `0` (the default) when `footprint` is
    /// `None`. Keys the CVD
    /// pane's recompute cache (`ChartState::cvd_shared`) instead of `footprint`'s own slice
    /// address, which had a narrow ABA-staleness gap across a CVD toggle-off/on cycle — see
    /// `model.rs`'s `CvdCacheKey` doc for the full story.
    pub footprint_gen: u64,
    /// Show the CVD sub-pane this frame (SP2). Default-off; has no effect while
    /// `footprint` is `None`.
    pub cvd_on: bool,
    /// Show the volume-profile overlay this frame (SP2). Default-off; has no effect
    /// while `footprint` is `None`.
    pub profile_on: bool,
    /// Volume-profile bucket width in raw price units (SP2, T4). `0.0` (the default) means
    /// "derive": the price closure picks a "nice" 1/2/5·10^k step sized to ~36 buckets across
    /// the visible price extent (`scale::nice_step_ceil` — the same rounding the y-axis
    /// gridlines use). A caller-supplied positive value pins the bucket width instead. Unused
    /// while `profile_on` is `false` or `footprint` is `None`.
    pub of_tick_size: f64,
    /// The authored UNIFIED sub-pane order (chart single-max default):
    /// `WinState::present_sub_panes()` — Volume, CVD, and study panes as PEERS
    /// in whatever top-to-bottom order the user arranged, already filtered to
    /// present panes. `resolve_pane_layout` gates each entry (Volume by
    /// `options.show_volume`+style, CVD by `cvd_on`+footprint, Study by ≥1
    /// visible study) and gathers every study assigned to a Study pane via
    /// `study_pane_of`. Previously this was a study-only list with Volume/CVD
    /// force-prepended; now their position is user-authored. See
    /// `panes::present_panes` for how it composes into the full pane sequence.
    pub sub_panes: &'a [PaneKey],
    /// Study uid → its authored pane (C1 Task 3), the grouping key the sub-pane
    /// loop filters on. Overlays are ABSENT (they always render on the price
    /// pane, never in a study pane, so they never appear here). On the default
    /// assignment every visible oscillator maps to its own `Study(uid)` pane.
    pub study_pane_of: &'a indexmap::IndexMap<u64, PaneKey>,
    /// C2a multi-symbol: compare series overlaid on the PRICE pane as
    /// %-normalized lines (each rebased to its own first-visible close). Only
    /// rendered — and only folded into the price pane's y-autofit — while the
    /// price scale resolves to `Percent` (compare mode), so every line shares
    /// one % axis. **EMPTY is the default and is byte-identical to the
    /// pre-C2 render**: the overlay loop, the autofit fold, and the legend
    /// append are each guarded on this being non-empty. The primary stays
    /// `state` (unchanged).
    pub overlays: &'a [SeriesInput<'a>],
    /// C2b multi-symbol: the authored order of compare-series panes
    /// (`WinState::present_series_panes()`), each a compare symbol moved OUT of
    /// the price-pane %-overlay into its OWN sub-pane below (mirrors
    /// `study_panes`). `present_panes` appends these AFTER every study pane, so
    /// the last one is the bottom-most pane (carries the shared time axis). The
    /// series sub-pane loop reverse-looks-up each pane's symbol in
    /// `series_pane_of`, finds that symbol's [`SeriesInput`] in `overlays`, and
    /// renders it as an ABSOLUTE-price line reindexed by open-time onto the
    /// primary's index domain. **EMPTY is the default and byte-identical to the
    /// pre-C2b render** — nothing enters `series_pane` until the Task-9 menu, so
    /// this stays `&[]`, the loop never runs, and the overlay skip below skips
    /// nothing.
    pub series_panes: &'a [PaneKey],
    /// C2b multi-symbol: compare symbol → its authored own-pane (mirrors
    /// `study_pane_of`, but keyed by symbol string, not a study uid). Two roles:
    /// (1) the price-pane overlay loop SKIPS any [`SeriesInput`] whose `symbol`
    /// is a key here (it renders in its own pane, not as a price overlay — a
    /// compare symbol is in EITHER `overlays`-only OR its own pane, never both);
    /// (2) the series sub-pane loop reverse-looks-up the symbol assigned to each
    /// `series_panes` entry. EMPTY by default ⇒ the skip skips nothing and no
    /// series pane resolves ⇒ byte-identical.
    pub series_pane_of: &'a indexmap::IndexMap<String, PaneKey>,
    /// C2b Task 7 ("Pin to scale ▸ Right"): per-overlay-symbol secondary-axis
    /// assignment (`WinState::series_scale`). ABSENT / [`ScaleAssign::Percent`]
    /// (the default) ⇒ the overlay is a %-line on the shared % axis (C2a,
    /// unchanged). [`ScaleAssign::Right`] ⇒ the overlay renders on its OWN
    /// absolute price axis: its visible reindexed close range is linearly
    /// remapped into the primary's RESOLVED plot-space y-range (`ty0..ty1`) via
    /// [`scale::remap_to_primary`] so it fills the pane on its own scale — drawn
    /// REGARDLESS of the primary's `eff_mode` (Linear/Log/Percent alike) and
    /// deliberately NOT folded into the primary's `auto_y` (the primary keeps
    /// its own range). A matching labeled right price gutter (showing that
    /// overlay's real prices) is drawn via a second egui_plot y-axis on the
    /// price pane; to keep the sub-panes x-aligned every pane reserves the SAME
    /// widened right gutter (see `pane_y_axes` + the builder-site note in `draw`).
    /// [`ScaleAssign::Left`] shares the same right-axis path in 7b (a distinct
    /// left gutter is out of 7b's single-secondary-axis scope). **EMPTY is the
    /// default and byte-identical to the C2a render**: every overlay resolves to
    /// `Percent` and the secondary-axis block is a no-op.
    pub series_scale: &'a indexmap::IndexMap<String, ScaleAssign>,
    /// GPU candle render seam (GPU Phase 2, Task 1). `None` (the default at EVERY build site)
    /// ⇒ the candle-family styles paint through the egui `draw_candles` painter exactly as
    /// before — byte-identical, the `GpuCandleItem` branch is never taken. `Some(build)` ⇒
    /// those same arms instead push a [`crate::render::candles::GpuCandleItem`] into `plot_ui.items` at
    /// the IDENTICAL z-slot (above background+grid, below overlays/last-price/crosshair),
    /// handing `build` this frame's screen-space [`crate::render::candles::CandleInstance`]s + the plot
    /// `Rect` so vike-desktop draws the candles on the GPU. Pixel-parity is guaranteed by
    /// construction: both paths derive geometry from the shared `render::candle_geom`. The
    /// `build` hook returns an already-formed `egui::Shape` (a wgpu paint callback) — vike-
    /// chart never names any wgpu/egui_wgpu type, so its Cargo.toml stays GPU-free. Only the
    /// candle bodies+wicks styles are GPU-eligible (Hollow / plain Candles + the Renko/Range/
    /// HeikinAshi/LineBreak transform families / the Footprint no-data fallback); Bars-style
    /// (`draw_bars`), Line/Area, Kagi/PnF, VolumeCandles, Footprint-with-data, etc. stay on
    /// the egui path unconditionally — GPU Phase 2 is candle bodies+wicks only.
    pub gpu_candles:
        Option<&'a dyn Fn(Vec<crate::render::candles::CandleInstance>, egui::Rect) -> egui::Shape>,
}

/// Outputs of [`draw`]: the hovered OHLC (for the window's title-bar readout),
/// a clicked nav button, and an oscillator uid the user asked to remove.
#[derive(Default)]
pub struct ChartActions {
    pub hovered: Option<[f64; 4]>,
    pub nav_out: Option<Nav>,
    pub remove_uid: Option<u64>,
    /// Set when the requested `ChartInputs::scale` was downgraded this frame
    /// by the sticky fallback (Log with non-positive visible prices, or
    /// Percent with a zero/non-finite anchor) — a status hint for the caller
    /// to surface; `None` when the requested mode was used as-is. Also
    /// painted directly inside the chart (chart-UX bundle T3) — the caller
    /// doesn't need to surface it separately.
    pub scale_fallback: Option<&'static str>,
    /// Set when the "Indicator settings" dialog produced a live-preview edit this
    /// frame (chart-UX bundle T8): the target `Active` uid + its new
    /// [`IndicatorEdit`]. Because `ChartInputs::indicators` is immutable, the
    /// caller applies it — `set_params` when the params differ (a debounced,
    /// pointer-release-triggered refold), and always writes each output line's
    /// colour+width (cheap, effectively immediate). Emitted on a param
    /// drag-release, on any colour/width change, on OK, and (as the SNAPSHOT) on
    /// Cancel/close to restore. `None` on frames with no edit.
    pub indicator_edit: Option<(u64, IndicatorEdit)>,
    /// Set when the user clicked the in-chart Log/% toggle this frame
    /// (chart-UX bundle T3): the new REQUESTED mode the caller should write
    /// back onto its own persisted state (`WinState::scale` in vike-app-core) for
    /// next frame's `ChartInputs::scale`. `None` on every frame without a
    /// click.
    pub scale_change: Option<ScaleMode>,
    /// Set when the user toggled "Invert scale" in the price-axis right-click menu
    /// this frame: the new REQUESTED invert flag the caller writes back onto its
    /// own persisted state (`WinState::invert` in vike-app-core) for next frame's
    /// [`ChartInputs::invert`]. `None` on every frame without a toggle.
    pub invert_change: Option<bool>,
    /// Set when the user pressed OK in the "Chart settings" dialog this frame
    /// (chart-UX bundle T6): the new committed `ChartOptions` the caller writes
    /// back onto its persisted state (`WinState::options` in vike-app-core). Cancel
    /// / dialog-close leave this `None` (the committed options are untouched).
    pub options_change: Option<ChartOptions>,
    /// Chart sync seam (task B7): the hovered bar's `ot` this frame — the
    /// value a sync LEADER broadcasts as the next frame's `SyncIn::crosshair_ts`
    /// for follower windows. `None` when this window has no local hover this
    /// frame (whether or not a ghost crosshair is being painted FROM another
    /// window's sync input).
    pub hover_ts: Option<i64>,
    /// Chart sync seam (task B7): `ot` of the first/last visible RENDERED bar
    /// (the style-transformed series when applicable — Renko/Kagi/etc. are
    /// ordinal, not 1:1 with the raw series), from the resolved post-`show`
    /// `(px0, px1)`. `None` when the series is empty or the resolved range
    /// doesn't cover a whole rendered bar.
    pub visible_ts: Option<(i64, i64)>,
    /// Chart sync seam (task B7): true when THIS window had its own pointer
    /// drag, a scroll/zoom gesture while hovered, a `Nav` click, or a queued
    /// gutter drag apply this frame — i.e. the user is actively driving this
    /// specific chart. A sync LEADER uses this to decide whether to broadcast
    /// `visible_ts` this frame; an injected `SyncIn::range_ts` is itself
    /// ignored (behavior 5) on any frame this is true, so a live local
    /// interaction always wins over a racing/stale injected range.
    pub interacted: bool,
    /// SP2: set when the user clicked the CVD sub-pane header's ✕ this frame
    /// — a fire-once-per-frame signal (not the desired next state) telling
    /// the caller to write its own persisted `cvd_on` to `false` next frame
    /// (mirrors `remove_uid`'s "the caller owns the source of truth" shape).
    /// `false` on every frame without a click. Harvested starting Task 7;
    /// unread by vike-app until then is fine — no orderflow input means this
    /// can never even fire (no CVD pane ⇒ no ✕ to click).
    pub cvd_toggle: bool,
    /// Volume-as-indicator: true when the volume pane header's ✕ was clicked
    /// this frame — the caller writes its persisted `ChartOptions::show_volume`
    /// to `false` next frame (mirrors `cvd_toggle`). `false` otherwise.
    pub volume_remove: bool,
    /// C1 Task 3: the study uid the user asked to relocate this frame + where to
    /// (a study pane header's ••• "Move to" menu). `None` until Task 4 wires
    /// that menu; the field exists now so the caller's harvest site and
    /// `WinState::move_study` have a stable type to compile against. The caller
    /// applies it to its authored pane model and drops any now-empty pane.
    pub move_study: Option<(u64, MoveTarget)>,
    /// Feature #1 (TradingView parity) — chart single-max default: the sub-pane
    /// the user asked to move with the header ↑/↓ controls this frame + direction
    /// (`true` = up). Volume, CVD, and study panes are all reorderable peers now.
    /// The caller applies it via `WinState::reorder_pane`. `None` on frames with
    /// no reorder click (fire-once, like `move_study`).
    pub reorder_pane: Option<(PaneKey, bool)>,
}

/// Draw the chart (price plot + volume + oscillator sub-panes). `inp.nav` =
/// pending nav; `inp.follow` = the window's follow-live-edge state (mutated
/// here); `inp.options.show_volume` gates the volume pane (also style-gated —
/// reindexed transform styles have no per-bar volume); `inp.settings` drives
/// the in-chart "Chart settings" dialog (chart-UX bundle T6). Returns the
/// hovered OHLC, nav clicked, and oscillator uid to remove via [`ChartActions`].
///
/// The body is the frame in its ORIGINAL order, one named phase wherever a region has a small
/// interface: [`settings_dialog_and_opts`], [`paint_canvas_background`], the price pane
/// ([`price_pane`] — [`price_x_range`], [`price_scale_and_overlays`], [`price_bounds_and_paint`] —
/// run inside the plot's content closure) and [`draw_sub_panes`]. What stays inline (colours, the
/// render series, the pane layout, the plot builder, the post-`show` chrome) is the shared state
/// every phase reads, so splitting it would only move a dozen locals across a signature.
///
/// [`price_pane`]: price_pane::price_pane
/// [`price_x_range`]: price_pane::price_x_range
/// [`price_scale_and_overlays`]: price_pane::price_scale_and_overlays
/// [`price_bounds_and_paint`]: price_pane::price_bounds_and_paint
pub fn draw(ui: &mut egui::Ui, inp: ChartInputs<'_>) -> ChartActions {
    let ChartInputs {
        state,
        style,
        nav: nav_in,
        indicators,
        studies: micro_studies,
        follow,
        options,
        settings,
        indicator_dialog,
        scale: requested_scale,
        invert: requested_invert,
        panes,
        sync,
        footprint,
        footprint_gen,
        cvd_on,
        profile_on,
        of_tick_size,
        sub_panes,
        study_pane_of,
        overlays,
        series_panes,
        series_pane_of,
        series_scale,
        gpu_candles,
    } = inp;
    // Single source of truth for this frame's display tz (design rule: tz lives ONLY on
    // ChartState, not ChartInputs) — every formatter/mark call below reads this one binding
    // rather than re-deriving it, so labels and gridlines can never disagree within a frame.
    let tz = state.tz();

    // --- Chart settings dialog (chart-UX bundle T6, RESOLUTION 2) ---
    // Drawn FIRST so this frame's widget edits to `settings.working` drive the
    // body's LIVE preview below (the effective `opts` is read AFTER this). OK
    // commits the working copy (returned as `options_change`, applied by the
    // caller to `WinState::options`); Cancel / window-close just close and
    // discard it, so the committed `options` are never touched mid-edit.
    let (opts, options_change) = settings_dialog_and_opts(ui, settings, options);
    // The one resolution of this frame's colours (design system spec §3.5): every painter below
    // reads `c`, so a Settings change reaches this chart on the next frame.
    let appearance = vike_ui_theme::appearance::current(ui.ctx());
    let c = crate::colors::ChartColors::resolve(&opts, &appearance);
    // ...and its text size: the chart's own text names a type role (spec §3.3, §3.5).
    let ts = appearance.text_size;
    // Copy scalars pulled out so the Plot builder/paint closures capture
    // cheap `Copy` values rather than re-borrowing `opts`.
    let precision = opts.precision;
    let show_last_price = opts.show_last_price;
    // Canvas grid (TV split): the colour is the resolved `c.grid`; per-axis
    // visibility is gated by `grid_show` (vertical = x-grid, horizontal = y-grid),
    // each AND'd with the master `show_grid`. Default (all true) ⇒ egui_plot's own
    // default ⇒ byte-identical to the pre-split render.
    let grid_show = opts.grid_show();
    // Canvas margins (TV): top/bottom % of the visible span applied to the price
    // pane's y-autofit pad. 5.0/5.0 == today's fixed 5% pad (byte-identical).
    let margin_top = opts.margin_top_pct;
    let margin_bottom = opts.margin_bottom_pct;

    // The axis labels are the Caption role in JetBrains Mono, following the text-size setting
    // (spec §3.5). egui_plot resolves TextStyle::Body for its tick labels, so Body is set for this
    // chart ui. The OHLC overlay + tags set explicit FontIds, and the pane legend NAME its own
    // semibold one, so neither is affected.
    ui.style_mut()
        .text_styles
        .insert(egui::TextStyle::Body, FontId::monospace(ts.px(TextRole::Caption)));
    // Render series + x-axis mark inputs for this frame (chart refactor PR-2, Block A).
    // `resolve_series` owns the transform proxy + its recomputed hour/day marks — built via
    // the two-tier `ChartState::transformed_shared` O(delta) cache (chart-perf T6: closed
    // prefix cached, only the forming tail recomputes; Kagi/PnF structured results are
    // populated there for the later `cached_{kagi,pnf}_shared` reads). `draw` rebuilds the
    // `series`/`marks_ref`/`day_marks_ref` borrows below from those owned buffers + `state`
    // (a `&[Bar]` can't live in the same struct as the `Rc` it points into). `gr`/`hs`/`ds`
    // (task A4's grain + median grid-step hints) are resolved ONCE and shared by all three
    // panes' spacer/formatter closures — the volume/oscillator panes slave to the price
    // pane's x-axis and must never derive a different grain/step from it.
    let RenderSeries { owned, owned_marks, owned_day_marks, forming_mark, forming_day, gr, hs, ds } =
        resolve_series(state, style, tz);
    let series: &[Bar] = owned.as_deref().map_or(&state.bars[..], |v| v.as_slice());
    let marks_ref: &[f64] = match &owned_marks {
        Some(om) => om.as_slice(),
        None => state.hour_marks.as_slice(),
    };
    let day_marks_ref: &[f64] = match &owned_day_marks {
        Some(od) => od.as_slice(),
        None => state.day_marks.as_slice(),
    };
    // The shared x-axis label + grid ctx (chart refactor PR-3): built once here (right
    // after the slices/marks it borrows are bound) and reused byte-identically at all 5
    // x-linked sites below (price + volume + cvd + study + series) — `Copy`, so each site
    // just passes it by value into `attach_shared_x_axis`.
    let xcx = XAxisCtx {
        bars: series,
        hour_marks: marks_ref,
        day_marks: day_marks_ref,
        forming_mark,
        forming_day,
        tz,
        grain: gr,
        hour_step: hs,
        day_step: ds,
    };

    // Pane layout for this frame (chart refactor PR-2, Block B): which sub-panes are
    // present (the C1 authored study panes filtered to those holding a VISIBLE study —
    // a hidden study must not leave a blank strip; the SP2 CVD pane, gated on real
    // footprint data; the C2b own-pane compare series resolved back to their live
    // inputs), their pixel heights, and the present/heights indices the four sub-pane
    // render loops address by (`osc_base`/`cvd_idx`/`series_base` — previously
    // recomputed inline in each loop, now resolved once here). `avail` is the one value
    // that reads `ui` — bound to the clip height because `available_height()` can over-
    // report inside a force-sized window (reflecting the screen, not the window) and
    // starve the panes — so it is computed here and passed in to keep the resolver pure.
    // `panes` is mutated exactly as before (a never-seen pane is pinned to its default
    // share). `chrome` is subtracted up front so `heights.sum() + chrome == avail`.
    // Reserve the bottom time-axis height (chart::AXIS_LABEL_H) OUT of the height budget:
    // a force-sized (maximized) window can report a height slightly past the visible
    // clip, which pushed the bottom pane's shared time axis off-screen at non-maximized
    // window sizes. Reserving it here guarantees the time axis stays visible.
    let avail =
        (ui.available_height().min((ui.clip_rect().bottom() - ui.cursor().top()).max(140.0))
            - chart::AXIS_LABEL_H)
            .max(140.0);
    let PaneLayout {
        visible_study_panes,
        n_reorderable,
        visible_series,
        present,
        n_below,
        axis_h,
        avail_for_panes,
        heights,
        price_h,
        ..
    } = resolve_pane_layout(
        avail,
        opts.show_volume,
        style,
        state,
        indicators,
        sub_panes,
        study_pane_of,
        micro_studies,
        cvd_on,
        footprint.is_some(),
        series_panes,
        series_pane_of,
        overlays,
        panes,
        follow.price_maximized,
    );

    // NO plot border box (vike's chart has none — egui_plot draws it from noninteractive.bg_stroke
    // at plot.rs:932; gridlines come from the grid colour separately, so killing this keeps the
    // grid).
    ui.visuals_mut().widgets.noninteractive.bg_stroke = Stroke::NONE;
    ui.visuals_mut().widgets.noninteractive.fg_stroke.color = c.axis;
    paint_canvas_background(ui, &opts, &c, avail);

    // While y-autofit is engaged, interactions are x-only (wheel zooms time,
    // drag pans time, y follows the data — the TradingView default); manual
    // y-manipulation modes return with the axis-drag work.
    let auto_y = follow.auto_y;
    // Chart-UX bundle T2: the frame's resolved (effective ScaleMode, Percent
    // anchor) is only knowable once cx0/cx1 are finalized INSIDE the `.show()`
    // content closure below (the visible x-range drives both the sticky
    // fallback and the anchor). The y_axis_formatter/y_grid_spacer closures
    // are attached to the Plot BUILDER, before `.show()` runs — but egui_plot
    // calls them to draw axes AFTER invoking our content closure (verified
    // against egui_plot 0.36's `show_dyn`: axis rendering follows `build_fn`),
    // so a `Cell` set as the very first thing inside that closure is read-safe
    // by the time these formatter/spacer closures fire. The same Cell is read
    // again after `.show()` returns, for the chip/tag labels below.
    // Holds the frame's resolved (effective `ScaleView` = mode + invert, Percent
    // anchor). Invert rides inside the view so every formatter/spacer/tag reads
    // one flip-aware transform.
    let scale_cell: Cell<(ScaleView, f64)> =
        Cell::new((ScaleView::new(ScaleMode::Linear, false), 0.0));
    // C2b Task 7b: a secondary ABSOLUTE-price axis (a Right/Left-pinned compare
    // overlay's OWN scale) is active this frame iff ≥1 overlay is pinned to a side
    // (not `%`, and not moved to its own sub-pane — the exact predicate the
    // `secondary_lines` block below draws for). Decided HERE from config, before
    // any Plot builder, so the SAME (doubled) right gutter is threaded into EVERY
    // pane for x-alignment — independently of whether the overlay actually has
    // visible data this frame (a data-dependent gutter would make the candles
    // jump as data comes and goes). EMPTY overlays / all-Percent ⇒ false ⇒ every
    // pane's gutter + axis set stays byte-identical to the pre-7b render.
    let sec_axis_active = overlays.iter().any(|s| {
        !series_pane_of.contains_key(s.symbol)
            && !matches!(
                series_scale.get(s.symbol).copied().unwrap_or_default(),
                ScaleAssign::Percent
            )
    });
    // The FIRST drawable Right/Left overlay's own visible range `(sec_lo, sec_hi)`
    // plus the primary's RESOLVED plot-space y-range `(ty0, ty1)`, published from
    // inside the price closure (where they are computed) and read back by the
    // secondary right-axis LABEL formatter AFTER `.show()` runs its build_fn (the
    // same read-after-build_fn timing egui_plot gives `scale_cell`). Sentinel NaNs
    // until set ⇒ the formatter emits NO labels (a reserved but unlabeled gutter)
    // for an absent / all-gap secondary — the gutter width never depends on it.
    let sec_axis_cell: Cell<(f64, f64, f64, f64)> =
        Cell::new((f64::NAN, f64::NAN, f64::NAN, f64::NAN));
    // The pre-T2 y-grid: egui_plot's own default spacer (recursive 3-tier
    // grid, pixel-adaptive density via GridInput::base_step_size, per-mark
    // culling). Linear mode must delegate to it VERBATIM — plot space IS
    // price space there, and "Linear stays behaviorally identical" is a T2
    // gate; `scale::nice_ticks_view` (single-tier, range-sized) exists for the
    // MAPPED modes, where the default would space gridlines in log/percent
    // units and label them with unmapped garbage.
    let default_y_spacer = egui_plot::log_grid_spacer(10);
    // NOTE deliberately NO egui_plot link_axis anywhere: the shared-group linking
    // is buggy upstream (egui_plot#14 — a pane holding placeholder bounds poisons
    // the group and x-collapses every linked pane). The price pane is the single
    // x-authority; volume/oscillator panes are slaved to its resolved x-range
    // manually each frame after `resp` below.
    // Feature #3 fix: the whole chart's rect — so a scroll over ANY pane (an
    // indicator sub-pane too, not just the price plot) zooms the shared time
    // axis, matching TradingView. Captured before the plots are laid out.
    // Zero the vertical inter-widget spacing for the whole pane stack. egui inserts
    // `item_spacing.y` before EVERY stacked widget (the price plot, each `chart::PANE_SEP_H`
    // separator, and each sub-pane plot), but the pane-layout `chrome` accounting only
    // reserves the explicit separators + the shared time axis — NOT this spacing. Each
    // added sub-pane therefore leaked ~item_spacing.y of unaccounted height, eating the
    // chart::AXIS_LABEL_H reserve until the bottom pane's shared time axis slid under the status
    // bar (the "timeline disappears with 4+ panes" bug). The panes own their gaps via the
    // explicit separators, so zeroing egui's implicit gap makes rendered height == `avail`.
    ui.spacing_mut().item_spacing.y = space::NONE;
    let chart_rect = ui.max_rect();
    let mut plot = Plot::new("price")
        .height(price_h)
        // Feature #3 (TradingView parity): egui_plot's built-in X-zoom reacts to
        // the pinch / horizontal gesture (`zoom_delta`), which made a LEFT-RIGHT
        // gesture zoom the time axis. TradingView zooms on the VERTICAL wheel,
        // so disable egui_plot's x-zoom entirely and own zoom in the closure
        // below (vertical scroll → x-zoom around the cursor). Y-zoom stays on in
        // manual-y mode (dragging the price gutter still works).
        .allow_zoom(egui::Vec2b::new(false, !auto_y))
        .allow_drag(egui::Vec2b::new(true, !auto_y))
        // egui_plot's built-in scroll maps the wheel/two-finger scroll to PAN;
        // we own scroll below so a vertical scroll zooms instead.
        .allow_scroll(false)
        .allow_boxed_zoom(!auto_y)
        // native gutter-drag + dbl-click reset bypass the tracked single-write (spec §2); we own the gutters below.
        .allow_axis_zoom_drag(false)
        .allow_double_click_reset(false)
        .show_x(false) // disable egui_plot's built-in cursor rulers — we draw our own crosshair
        .show_y(false)
        .y_axis_position(egui_plot::HPlacement::Right)
        .y_axis_min_width(chart::Y_AXIS_GUTTER_W) // uniform right gutter so panes align with price (vike parity)
        // egui_plot defaults grid to the BRIGHT text color — force vike's faint
        // grid (the option color). Per-axis visibility (TV vertical/horizontal
        // split) is gated by `show_grid` below, not the color.
        .grid_color(c.grid)
        .show_grid(grid_show)
        .y_axis_formatter(|m, _| {
            let (view, anchor) = scale_cell.get();
            fmt_scaled_view(view, m.value, anchor, precision)
        })
        // Linear → egui_plot's default spacer verbatim (pre-T2 behavior, see
        // `default_y_spacer` above); Log/Percent → custom nice-tick generator
        // in RAW price space (scale.rs), emitted mapped with fabricated
        // mapped step_sizes (T2 §1).
        .y_grid_spacer(|input: GridInput| {
            let (view, anchor) = scale_cell.get();
            // Plain Linear (no invert) delegates to egui_plot's default spacer
            // verbatim — plot space IS price space there. An INVERTED Linear axis
            // can't (the default spacer would place non-flipped ticks), so it
            // falls through to the flip-aware `nice_ticks_view` path below. Both
            // MAPPED modes (Log/Percent/Indexed) always take the custom path.
            if view.mode == ScaleMode::Linear && !view.invert {
                return default_y_spacer(input);
            }
            let (b0, b1) = input.bounds;
            let (rlo, rhi) = {
                let a = view.unmap(b0, anchor);
                let b = view.unmap(b1, anchor);
                if a <= b { (a, b) } else { (b, a) }
            };
            scale::nice_ticks_view(view, rlo, rhi, anchor, Y_TICK_BUDGET)
                .into_iter()
                .map(|t| GridMark { value: t.mapped, step_size: t.step_mapped })
                .collect()
        })
        // when oscillator panes exist, the time axis moves to the bottom-most pane (vike layout)
        .show_axes(egui::Vec2b::new(n_below == 0, true));
    plot = attach_shared_x_axis(plot, xcx);

    // Default/Reset plot bounds + the area/columns fill floor (chart refactor PR-2,
    // Block C). "Default" IS the full-series view (Reset sets cx0/cx1 to fx_min/fx_max),
    // so `default_bounds` resolves the effective mode + anchor from the FULL extent +
    // first bar's close — exactly what the in-closure per-frame resolution produces for
    // a Reset/first-ever frame. `owned.is_none()` gates the cached-`y_ext` fast path;
    // `follow`'s READ-ONLY `peek_scale` must not double-advance the sticky-fallback
    // latch the state-advancing `resolve_scale` in the closure below advances once per
    // frame. An empty series leaves the `(-1, 1, 0, 1)` defaults untouched and skips the
    // builder's default-bounds chain (`apply == false`) — byte-identical to the old
    // `if !series.is_empty()` guard.
    let Bounds { fx_min, fx_max, fy_min, fy_max, y_lo, apply } = default_bounds(
        series,
        owned.is_none(),
        state,
        follow,
        requested_scale,
        requested_invert,
        margin_top,
        margin_bottom,
    );
    if apply {
        plot = plot
            .default_x_bounds(fx_min, fx_max)
            .default_y_bounds(fy_min, fy_max)
            .auto_bounds(false);
    }

    // C2b Task 7b: the labeled RIGHT price gutter for a secondary (Right/Left-
    // pinned) compare overlay. When active, REPLACE the single right price axis
    // (configured above) with TWO right axes: the primary price stays the INNER
    // column — so its labels AND the last-price / crosshair-price chips (which
    // paint at `frame.right()`, i.e. against the data rect) stay mutually aligned
    // — and the compare series gets a labeled OUTER (far-right) column whose
    // formatter inverse-remaps each SHARED plot-space grid mark back to that
    // overlay's own price range (`scale::inverse_remap`, fed by `sec_axis_cell`).
    // `custom_y_axes` sets `y_axes` wholesale, so the primary is reconstructed
    // here IDENTICALLY to the `.y_axis_position(Right).y_axis_min_width(W)
    // .y_axis_formatter(..)` chain above (same placement / min_thickness /
    // formatter / default `AxisHints::new` label spacing). THE CROSS-PANE FIX:
    // this widens the price pane's right gutter by one `chart::Y_AXIS_GUTTER_W`; every
    // sub-pane reserves the SAME extra gutter (an empty spacer axis via
    // `pane_y_axes`), so all panes keep an identical data-rect x-span and the
    // candles stay x-aligned with the sub-pane bars. INACTIVE (the default) ⇒
    // this block is skipped and the single-axis builder stands ⇒ byte-identical.
    if sec_axis_active {
        let primary = egui_plot::AxisHints::new(egui_plot::Axis::Y)
            .placement(egui_plot::HPlacement::Right)
            .min_thickness(chart::Y_AXIS_GUTTER_W)
            .formatter(|m, _| {
                let (view, anchor) = scale_cell.get();
                fmt_scaled_view(view, m.value, anchor, precision)
            });
        let secondary = egui_plot::AxisHints::new(egui_plot::Axis::Y)
            .placement(egui_plot::HPlacement::Right)
            .min_thickness(chart::Y_AXIS_GUTTER_W)
            .formatter(|m, _| {
                let (sec_lo, sec_hi, ty0, ty1) = sec_axis_cell.get();
                // Absent / all-gap secondary (or a degenerate resolved range):
                // reserve the gutter but draw no labels.
                if [sec_lo, sec_hi, ty0, ty1].iter().any(|v| !v.is_finite()) {
                    return String::new();
                }
                // shared plot-space grid mark -> the compare series' own price,
                // shown with the app-standard 2dp grouping (`None` ⇒ `fmt_thousands`),
                // NOT the primary's own price precision — the primary's decimals describe
                // the primary symbol, not this unrelated-magnitude compare. Matches the
                // legend readout below (also `fmt_thousands`). Default (auto-precision
                // primary) ⇒ byte-identical, since `precision` is `None` there too.
                let price = scale::inverse_remap(m.value, sec_lo, sec_hi, ty0, ty1);
                fmt_scaled(ScaleMode::Linear, price, 0.0, None)
            });
        // primary INNER, secondary OUTER (far right) — `pane_y_axes` reserves the
        // matching OUTER spacer on every sub-pane so all data rects stay equal.
        plot = plot.custom_y_axes(vec![primary, secondary]);
    }

    let mut po = PriceOut::default();
    let last_t = series.last().map(|b| b.t);
    let pp = PricePane {
        state,
        style,
        series,
        owned_is_none: owned.is_none(),
        indicators,
        overlays,
        series_pane_of,
        series_scale,
        nav_in,
        sync,
        chart_rect,
        fx_min,
        fx_max,
        fy_min,
        fy_max,
        y_lo,
        last_t,
        requested_scale,
        requested_invert,
        margin_top,
        margin_bottom,
        auto_y,
        c: &c,
        color_bars_prev_close: opts.color_bars_prev_close,
        gpu_candles,
        footprint,
        of_tick_size,
        profile_on,
        show_last_price,
        scale_cell: &scale_cell,
        sec_axis_cell: &sec_axis_cell,
    };
    let resp = plot.show(ui, |plot_ui| price_pane(plot_ui, &pp, follow, &mut po));
    let PriceOut { scale_fallback, hovered, hover_xt, hover_y, overlay_legend, interacted } = po;

    let tr = &resp.transform;
    let frame = *tr.frame();
    // the price pane's resolved x-range — the sub-panes slave to this
    let (px0, px1) = (tr.bounds().min()[0], tr.bounds().max()[0]);
    // Sync seam (task B7) behavior 2: ot of the first/last visible RENDERED
    // bar, over the SAME `series` the price pane just drew (the transformed
    // proxy for Renko/Kagi/etc. styles) — see `visible_ts_bounds` below for
    // the clamp math (unit-tested).
    let visible_ts = visible_ts_bounds(series, px0, px1);
    // this frame's resolved (effective ScaleView = mode + invert, Percent anchor)
    // — set inside the closure above, read back here for the chip/tag labels
    // below. `eff_mode` (the bare mode) is the OHLC legend's precision selector.
    let (view, anchor) = scale_cell.get();
    let eff_mode = view.mode;

    // pending seed clears only once the Reset view demonstrably landed.
    if follow.pending_seed {
        let bx = tr.bounds();
        if (bx.min()[0] - fx_min).abs() < 1.0 && (bx.max()[0] - fx_max).abs() < 1.0 {
            follow.pending_seed = false;
        }
    } else if let Some(lt) = last_t {
        // follow-live engage/disengage from the post-interaction bounds: panning
        // away from the right edge disengages; Reset/Auto or panning back re-engages.
        follow.on = follow_engaged(tr.bounds().max()[0], lt);
    }

    // Declared before `y_gutter` so its right-click scale menu can emit into
    // the same out-params the bottom-center `scale_row`/`nav_row` overlays do.
    let mut nav_out = None;
    let mut scale_change: Option<ScaleMode> = None;
    let mut invert_change: Option<bool> = None;
    y_gutter(
        ui,
        frame,
        sec_axis_active,
        follow,
        requested_scale,
        requested_invert,
        &mut scale_change,
        &mut invert_change,
        &mut nav_out,
    );
    // Bottom-most pane's frame for the x-drag gutter below — starts as price
    // (the n_below == 0 case) and gets overwritten once a sub-pane's `.show`
    // proves it's the one actually carrying the shared time axis this frame.
    let mut bottom_frame = frame;

    paint_crosshair(ui, frame, tr, hover_y, hover_xt, c.cross);

    // Sync seam (task B7) behavior 4: ghost crosshair — no local hover this
    // frame (gated on `hover_xt`, exactly the condition under which
    // `ChartActions::hover_ts` below is `None`), but the sync leader has one.
    // Vertical-only, faded to 40% of the crosshair colour (`paint_ghost_crosshair`'s
    // `chart::GHOST_STRENGTH` — the same gamma-space fade the volume bars take): y
    // isn't synced, so no horizontal line and no price tag — just the x
    // position, mirrored via `nearest_index_by_ts` over the SAME rendered
    // `series` the price pane drew.
    let ghost_xt: Option<(f64, i64)> = if hover_xt.is_none() {
        sync.and_then(|s| s.crosshair_ts)
            .and_then(|ts| nearest_index_by_ts(series, ts).map(|i| (series[i].t, series[i].ot)))
    } else {
        None
    };
    paint_ghost_crosshair(ui, frame, tr, &c, ghost_xt);

    paint_last_price_chip(ui, frame, tr, show_last_price, state, view, anchor, precision, &c, ts);

    paint_ohlc_legend(ui, frame, state, hovered, &c, eff_mode, precision, &overlay_legend, ts);

    paint_scale_fallback_hint(ui, frame, scale_fallback, &c, ts);

    paint_price_tag(ui, frame, tr, hover_y, view, anchor, precision, &c, ts);

    // NOTE: the crosshair TIME tag is painted LATER (after the sub-pane loop), on the chart's
    // bottom-most axis (`bottom_frame`), not here on the price frame — otherwise it lands at the
    // price/volume boundary and overlaps the top of the first sub-pane. TradingView puts it on the
    // time axis at the very bottom of the chart.

    // bottom-center nav buttons + Auto (overlay) -------------------------------
    // (`nav_out`/`scale_change` are declared above, ahead of `y_gutter`, whose
    // right-click scale menu shares them.)
    nav_row(ui, frame, settings, options, &mut nav_out, &mut follow.price_maximized);
    scale_row(ui, frame, requested_scale, &mut scale_change, &mut nav_out);

    // T9: the boundary-0 separator sits between price and whatever pane comes
    // right after it (Volume if on, else the first oscillator) — drawn here,
    // unconditionally, right where the ui cursor lands after the price plot.
    if n_below > 0 {
        pane_separator(ui, panes, &present, 0, avail_for_panes, chart::MIN_PANE_PX, &c);
    }

    // The read-only ctx shared by all sub-pane render fns below (chart refactor
    // PR-6) — see `subpanes::SubPaneCtx`'s doc. `Copy`, built once here.
    let cx = subpanes::SubPaneCtx {
        px0,
        px1,
        heights: &heights,
        axis_h,
        colors: &c,
        text_size: ts,
        grid_show,
        sec_axis_active,
        xcx,
        present: &present,
        avail_for_panes,
        n_reorderable,
    };

    // Ordered sub-pane render dispatch (chart single-max default): iterate the
    // authored `present` order (skip index 0 = Price, drawn above) and dispatch
    // each pane by KIND at its OWN position. Volume/CVD/Study are peers now —
    // their top-to-bottom order is whatever `present` says, not a fixed
    // Volume→CVD→studies→series sequence. Each fn draws its header + plot, sets
    // `bottom_frame` when it's the last pane (carries the shared time axis), and
    // draws the separator that follows it (unless last). The per-pane mutated
    // outputs are the same fire-once locals the old four calls threaded through.
    let SubPaneOut { volume_remove, cvd_toggle, remove_uid, move_study, reorder_pane } =
        draw_sub_panes(
            ui,
            &cx,
            panes,
            &mut bottom_frame,
            &present,
            state,
            series,
            footprint,
            footprint_gen,
            indicators,
            study_pane_of,
            micro_studies,
            indicator_dialog,
            &visible_study_panes,
            &visible_series,
        );

    x_gutter(ui, bottom_frame, follow, &mut nav_out);

    // Crosshair time tag on the chart's BOTTOM axis (`bottom_frame`), painted after the pane
    // loop so it lands on the shared time axis below the last pane — never overlapping a sub-pane
    // top (the price panes are x-linked, so the price-pane `tr` still maps the tag's x correctly).
    paint_time_tag(ui, bottom_frame, tr, tz, hover_xt, ghost_xt, &c, ts);

    // --- Indicator settings dialog (chart-UX bundle T8) --- drawn last so a gear
    // clicked in an oscillator pane header THIS frame (setting `open_uid` above)
    // opens the window same-frame; the ƒx-picker gear (vike-app-core) sets it before
    // draw. `indicators` stays immutable — edits flow back via `indicator_edit`.
    let mut indicator_edit = None;
    indicator_settings_dialog(ui, indicators, indicator_dialog, &c, &mut indicator_edit);

    // Claim any leftover vertical space so egui measures this chart at the FULL window height
    // and does not auto-shrink the (tiled/maximized) window to its natural content height.
    // egui_plot paints the bottom pane's x-axis labels below the pane stack's allocated rect, so
    // without this the Ui measures ~chart::AXIS_LABEL_H short, egui trims the window's bottom edge — an
    // uneven gap to the status bar. Clamp the fill to the VISIBLE clip bottom (NOT
    // `available_height()`, which over-reports inside a force-sized window — reflecting the
    // screen, not the window — and would push the content, and thus the window, PAST the arena
    // into the status bar). Filling exactly to the clip makes all four margins uniform.
    //
    // ALSO clamp to `max_rect().bottom()` — the window's OWN content region. For a maximized /
    // desktop-filling window `clip == max_rect` so this is a no-op (byte-identical). But for a
    // SMALLER pinned chart window the clip is still the whole desktop while `max_rect` is the
    // window: filling to the desktop clip left the content taller than the window, so egui could
    // not hold the fixed HEIGHT (it collapsed to the content) — which broke maximize-restore
    // height and vertical/diagonal resize. Filling to `max_rect` makes content == window, so the
    // pinned height holds and the bottom resize handle sits on the visible edge.
    let fill_bottom = ui.clip_rect().bottom().min(ui.max_rect().bottom());
    let leftover = (fill_bottom - ui.cursor().top()).max(0.0);
    if leftover > 0.0 {
        ui.add_space(leftover);
    }

    ChartActions {
        hovered,
        nav_out,
        remove_uid,
        scale_fallback,
        scale_change,
        invert_change,
        options_change,
        indicator_edit,
        hover_ts: hover_xt.map(|(_, ot)| ot),
        visible_ts,
        interacted,
        cvd_toggle,
        volume_remove,
        // C1 Task 4: the study relocation the user picked from a pane header's
        // ••• "Move to" menu this frame (or `None`). The caller applies it via
        // `WinState::move_study`, then drops any now-empty pane.
        move_study,
        // Feature #1: pane ↑/↓ reorder picked this frame (or `None`), applied
        // by the caller via `WinState::reorder_pane`.
        reorder_pane,
    }
}
