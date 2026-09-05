//! Per-window chart appearance/behavior options + the settings-dialog state
//! (chart-UX bundle T6). Extracts the palette + always-on flags that used to
//! be hard-coded `Color32` consts in `render.rs` into ONE serde-able struct so
//! a per-window "Chart settings" dialog can drive them live, and so a future
//! task (T10) can persist the full set.
//!
//! `ChartOptions` is the single source of truth for the series colors
//! (candle up/down, semantic up/down, line, grid, crosshair) + the canvas
//! background, the grid toggles (master + per-axis vertical/horizontal) and the
//! top/bottom autofit margins, the last-price toggle, the price-precision
//! override (Linear/Log only — Percent keeps its fixed `{:.2}%`), and the
//! volume-pane toggle (moved here from
//! `WinState`). `chart::draw` reads the EFFECTIVE options each frame —
//! `settings.working` while the dialog is open (live preview), the committed
//! options otherwise — and threads the colors into the `render.rs` painters.
//! No plotting or interaction state lives here (pure data + color accessors).

use crate::indicators::{LineDash, Source};
use egui::Color32;

/// `[r, g, b]` -> `Color32` (the palette's storage form is a serde-friendly
/// byte triple; egui wants a `Color32`).
pub(crate) fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// `[r, g, b]` + alpha -> unmultiplied `Color32` — the derivation used by the
/// alpha-FILL literals (baseline/area/columns fills that were mechanically a
/// base color at a fixed alpha).
pub(crate) fn rgba(c: [u8; 3], a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], a)
}

/// Per-window chart appearance + behavior. The `Default` is EXACTLY today's
/// hard-coded `render.rs` palette/flags (the T6 acceptance record — see
/// `tests` below); the dialog mutates a working copy of this.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
// Struct-level default (uses the custom `Default` impl below) so that when this
// struct is `#[serde(flatten)]`ed into a persisted `WinSnap` (vike-app workspace
// v2, T10), a field ABSENT from the JSON — e.g. every colour/flag in a v1 file
// that only carried `show_volume` — deserializes to its CUSTOM default value
// (up = [91,190,145], …) rather than a type-zero. Field-level `#[serde(default)]`
// would instead give [0,0,0]; the struct-level form is what makes v1 forward-compat
// preserve the real palette. Deserialize-only; serialization still emits all fields.
#[serde(default)]
pub struct ChartOptions {
    pub up: [u8; 3],          // CANDLE_UP — candle body fill (up)
    pub down: [u8; 3],        // CANDLE_DOWN — candle body fill (down)
    pub border_up: [u8; 3],   // candle body BORDER (up) — TV "Borders" row
    pub border_down: [u8; 3], // candle body BORDER (down)
    pub wick_up: [u8; 3],     // candle WICK (up) — TV "Wick" row
    pub wick_down: [u8; 3],   // candle WICK (down)
    pub up_s: [u8; 3],        // UP (semantic) — histograms/markers/baseline-up
    pub down_s: [u8; 3],      // DOWN (semantic)
    pub line: [u8; 3],        // line/area/step series color
    pub grid: [u8; 3],        // faint gridlines
    pub cross: [u8; 3],       // crosshair
    pub bg: [u8; 3], // chart canvas background (TV Canvas "Background"); the gradient BOTTOM stop
    /// TV "Background: Gradient" mode. When set, the canvas is a vertical gradient
    /// from `bg_top` (top of the chart) down to `bg` (bottom), painted ONCE behind
    /// every pane so the pane separators reveal the bands — exactly TradingView's
    /// continuous-across-panes background. ON by default (the subtle-lift look);
    /// set `false` for the old flat `bg` fill.
    pub bg_gradient: bool,
    /// The gradient's TOP stop (TV Background gradient, upper swatch); `bg` is the
    /// bottom stop. Ignored when `bg_gradient` is false. Defaults to `#161B24`, a
    /// soft lift above the `#0D1117` bottom — a gentle fade, not a heavy vignette.
    pub bg_top: [u8; 3],
    /// TV "Color bars based on previous close": when set, a candle's bull/bear
    /// classification (which drives its body/border/wick color) tests
    /// `close >= previous bar's close` instead of `close >= open`. The first
    /// drawn bar (no predecessor in the slice) falls back to `close >= open`.
    pub color_bars_prev_close: bool,
    pub show_grid: bool,
    /// Vertical gridlines (the x-axis grid — lines spaced along time). TV Canvas
    /// splits the single grid toggle into vertical + horizontal; both AND with
    /// `show_grid` so the legacy master toggle still hides everything.
    pub show_vgrid: bool,
    /// Horizontal gridlines (the y-axis grid — lines spaced along price).
    pub show_hgrid: bool,
    /// Top chart margin as a % of the visible price span — the empty room above
    /// the highest bar when y-autofit is engaged (TV Canvas "Margins → Top").
    /// `5.0` is exactly today's fixed 5% autofit pad (see `extent::y_pad`).
    pub margin_top_pct: f32,
    /// Bottom chart margin as a % of the visible price span (TV "Margins → Bottom").
    pub margin_bottom_pct: f32,
    pub show_last_price: bool,
    /// Fixed decimal places for Linear/Log price readouts (axis / crosshair
    /// tag / last-price chip / OHLC legend). `None` = today's auto formatting
    /// (comma-grouped, 2 dp). Percent mode ignores this (spec §1).
    pub precision: Option<u8>,
    pub show_volume: bool,
}

impl Default for ChartOptions {
    /// Exactly today's `crates/vike-chart/src/render.rs` consts + the always-on
    /// grid/last-price behaviors + auto precision + volume-on. Verified
    /// field-by-field by `tests::default_matches_render_consts`.
    fn default() -> Self {
        Self {
            up: [91, 190, 145],
            down: [217, 84, 88],
            // Borders + wicks default to the SAME colors as the body up/down, so
            // a fresh chart is byte-identical to today's single-color candle until
            // a user edits these rows.
            border_up: [91, 190, 145],
            border_down: [217, 84, 88],
            wick_up: [91, 190, 145],
            wick_down: [217, 84, 88],
            up_s: [64, 186, 80],
            down_s: [248, 82, 73],
            line: [64, 186, 80],
            grid: [34, 39, 47],
            cross: [154, 164, 177],
            bg: [13, 17, 23],     // theme::BG (#0D1117) — the gradient BOTTOM stop
            bg_gradient: true,    // TV-style gradient ON by default (the "subtle lift" look)
            bg_top: [22, 27, 36], // #161B24 — a soft lift above `bg`; the gradient TOP stop
            color_bars_prev_close: false,
            show_grid: true,
            show_vgrid: true,
            show_hgrid: true,
            margin_top_pct: 5.0, // == today's fixed 5% autofit pad (extent::y_pad)
            margin_bottom_pct: 5.0,
            show_last_price: true,
            precision: None,
            show_volume: true,
        }
    }
}

impl ChartOptions {
    pub(crate) fn up_col(&self) -> Color32 {
        rgb(self.up)
    }
    pub(crate) fn down_col(&self) -> Color32 {
        rgb(self.down)
    }
    pub(crate) fn border_up_col(&self) -> Color32 {
        rgb(self.border_up)
    }
    pub(crate) fn border_down_col(&self) -> Color32 {
        rgb(self.border_down)
    }
    pub(crate) fn wick_up_col(&self) -> Color32 {
        rgb(self.wick_up)
    }
    pub(crate) fn wick_down_col(&self) -> Color32 {
        rgb(self.wick_down)
    }
    pub(crate) fn up_s_col(&self) -> Color32 {
        rgb(self.up_s)
    }
    pub(crate) fn down_s_col(&self) -> Color32 {
        rgb(self.down_s)
    }
    pub(crate) fn line_col(&self) -> Color32 {
        rgb(self.line)
    }
    pub(crate) fn grid_col(&self) -> Color32 {
        rgb(self.grid)
    }
    pub(crate) fn cross_col(&self) -> Color32 {
        rgb(self.cross)
    }
    pub(crate) fn bg_col(&self) -> Color32 {
        rgb(self.bg)
    }
    /// The active gradient stops `(top, bottom)` when [`Self::bg_gradient`] is on,
    /// else `None` (solid — the caller paints the flat [`Self::bg_col`]). This is the
    /// single branch the renderer keys off, so the solid path stays byte-identical.
    pub(crate) fn bg_gradient_stops(&self) -> Option<(Color32, Color32)> {
        self.bg_gradient.then(|| (rgb(self.bg_top), rgb(self.bg)))
    }
    /// Per-axis grid visibility as an egui `Vec2b` — `x` = vertical lines
    /// (x-axis grid), `y` = horizontal lines (y-axis grid). Each AND's with the
    /// master `show_grid` so the legacy toggle keeps hiding the whole grid.
    pub(crate) fn grid_show(&self) -> egui::Vec2b {
        egui::Vec2b::new(self.show_grid && self.show_vgrid, self.show_grid && self.show_hgrid)
    }
}

/// The left-nav sections of the "Chart settings" dialog, mirroring TradingView's
/// tabbed layout (its oracle: Symbol / Canvas / Scales, plus Trading/Alerts/Events
/// vike does not yet have). Each maps a slice of [`ChartOptions`] to one panel.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum SettingsTab {
    /// Candle colors + price precision (TV "Symbol").
    #[default]
    Symbol,
    /// Grid, crosshair, series/appearance colors (TV "Canvas").
    Canvas,
    /// Last-price line + volume pane (TV "Scales and lines").
    Scales,
}

impl SettingsTab {
    pub const ALL: [SettingsTab; 3] =
        [SettingsTab::Symbol, SettingsTab::Canvas, SettingsTab::Scales];
    pub fn label(self) -> &'static str {
        match self {
            SettingsTab::Symbol => "Symbol",
            SettingsTab::Canvas => "Canvas",
            SettingsTab::Scales => "Scales and lines",
        }
    }
}

/// Per-window "Chart settings" dialog state (chart-UX bundle T6, RESOLUTION 2 —
/// a working-copy design). `open` gates the `egui::Window`; `working` is a
/// scratch copy the dialog widgets mutate live; `tab` is the selected left-nav
/// section. On OK the working copy is committed (`ChartActions::options_change`);
/// on Cancel/close it is discarded, so the committed options are never touched
/// mid-edit.
#[derive(Clone, Default)]
pub struct SettingsDialog {
    pub open: bool,
    pub tab: SettingsTab,
    pub working: ChartOptions,
}

/// One indicator's editable surface for the "Indicator settings" dialog
/// (chart-UX bundle T8): its parameter values (index-aligned to
/// `IndicatorMeta::params`) plus per-output-line paint (colour + stroke width +
/// show/hide, index-aligned to `Active::outputs`), the reference-band toggle, and
/// the whole-indicator visibility. It's the payload of both the live-preview edit
/// signal ([`crate::ChartActions::indicator_edit`]) and the Cancel-restore
/// snapshot — decoupled from `Active` so the immutable `&[Active]` chart input
/// stays immutable (T0 contract): the dialog edits a copy and signals deltas back.
#[derive(Clone, PartialEq)]
pub struct IndicatorEdit {
    pub params: Vec<f64>,
    /// TradingView "Inputs → Source" (chart source selector): the price series the
    /// indicator computes off (mirrors `Active::source`). [`Source::Close`] (the
    /// default) is a bit-identical no-op. Applied by the vike-app apply path with a
    /// refold, exactly like a `params` change.
    pub source: Source,
    /// per-output `(color, width, visible, line_style)`, index-aligned to
    /// `Active::outputs`.
    pub lines: Vec<(Color32, f32, bool, LineDash)>,
    /// master show toggle for the oscillator reference bands (RSI 30/50/70, …).
    pub show_bands: bool,
    /// per-level reference bands `(value, color, show)`, index-aligned to
    /// `Active::bands` — each level has its OWN colour + show toggle (TradingView RSI
    /// "Style" parity).
    pub bands: Vec<(f64, Color32, bool)>,
    /// shade the overbought (above top band) / oversold (below bottom band) zones
    /// with a translucent fill, mirroring `Active::show_ob_os_fill`.
    pub show_ob_os_fill: bool,
    /// overbought / oversold fill colours, mirroring `Active::ob_fill`/`os_fill`.
    pub ob_fill: Color32,
    pub os_fill: Color32,
    /// whole-indicator visibility (the "Visibility" tab toggle).
    pub visible: bool,
}

/// The tabs of the "Indicator settings" dialog, mirroring TradingView's oscillator
/// settings (Inputs / Style / Visibility).
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum IndicatorTab {
    /// Numeric parameters (TV "Inputs").
    #[default]
    Inputs,
    /// Per-plot color / width / show-hide + reference bands (TV "Style").
    Style,
    /// Whole-indicator visibility (TV "Visibility").
    Visibility,
}

impl IndicatorTab {
    pub const ALL: [IndicatorTab; 3] =
        [IndicatorTab::Inputs, IndicatorTab::Style, IndicatorTab::Visibility];
    pub fn label(self) -> &'static str {
        match self {
            IndicatorTab::Inputs => "Inputs",
            IndicatorTab::Style => "Style",
            IndicatorTab::Visibility => "Visibility",
        }
    }
}

/// Per-window "Indicator settings" dialog state (chart-UX bundle T8 — mirrors T6's
/// [`SettingsDialog`]). At most ONE indicator dialog is open per window. `open_uid`
/// names the target `Active`; `working` is the live-edited copy the widgets mutate;
/// `snapshot` is the on-open state restored on Cancel/close. All `None` when closed.
/// An entry point sets only `open_uid` (leaving `working`/`snapshot` `None`); the
/// chart's dialog renderer seeds `working`+`snapshot` from the live `Active` on the
/// first frame it sees a target with no working copy.
#[derive(Clone, Default)]
pub struct IndicatorDialog {
    pub open_uid: Option<u64>,
    pub tab: IndicatorTab,
    pub working: Option<IndicatorEdit>,
    pub snapshot: Option<IndicatorEdit>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The RED-test acceptance record: `ChartOptions::default()` must reproduce
    /// the exact `crates/vike-chart/src/render.rs` consts field-by-field, so the
    /// const-to-option extraction is a pure refactor (no pixel changes).
    #[test]
    fn default_matches_render_consts() {
        let o = ChartOptions::default();
        assert_eq!(o.up, [91, 190, 145], "up (CANDLE_UP)");
        assert_eq!(o.down, [217, 84, 88], "down (CANDLE_DOWN)");
        // Borders + wicks default to the body colors → byte-identical until edited.
        assert_eq!(o.border_up, [91, 190, 145], "border_up defaults to up");
        assert_eq!(o.border_down, [217, 84, 88], "border_down defaults to down");
        assert_eq!(o.wick_up, [91, 190, 145], "wick_up defaults to up");
        assert_eq!(o.wick_down, [217, 84, 88], "wick_down defaults to down");
        assert!(!o.color_bars_prev_close, "color_bars_prev_close off by default");
        assert_eq!(o.up_s, [64, 186, 80], "up_s (semantic UP)");
        assert_eq!(o.down_s, [248, 82, 73], "down_s (semantic DOWN)");
        assert_eq!(o.line, [64, 186, 80], "line (== up_s)");
        assert_eq!(o.grid, [34, 39, 47], "grid");
        assert_eq!(o.cross, [154, 164, 177], "cross");
        assert_eq!(o.bg, [13, 17, 23], "bg (theme::BG canvas — byte-identical default)");
        assert!(o.show_grid, "show_grid on by default (always-on today)");
        assert!(o.show_vgrid, "vertical grid on by default (== today's single grid)");
        assert!(o.show_hgrid, "horizontal grid on by default (== today's single grid)");
        assert_eq!(o.margin_top_pct, 5.0, "top margin == today's fixed 5% autofit pad");
        assert_eq!(o.margin_bottom_pct, 5.0, "bottom margin == today's fixed 5% autofit pad");
        assert!(o.show_last_price, "show_last_price on by default (always-on today)");
        assert_eq!(o.precision, None, "precision auto by default");
        assert!(o.show_volume, "show_volume on by default (moved from WinState)");
    }

    /// Gradient is ON by default (TradingView-style): a subtle vertical fade from
    /// `bg_top` (#161B24) at the top of the chart down to `bg` (#0D1117) at the
    /// bottom, so a fresh chart reads like TV out of the box. `bg_gradient_stops()`
    /// yields those two stops.
    #[test]
    fn default_background_is_gradient() {
        let o = ChartOptions::default();
        assert!(o.bg_gradient, "gradient on by default (TV-style)");
        assert_eq!(o.bg_top, [22, 27, 36], "bg_top = #161B24 (subtle lift)");
        assert_eq!(o.bg, [13, 17, 23], "bg = #0D1117 (bottom stop, unchanged)");
        assert_eq!(
            o.bg_gradient_stops(),
            Some((Color32::from_rgb(22, 27, 36), Color32::from_rgb(13, 17, 23))),
            "default stops = (#161B24 top, #0D1117 bottom)",
        );
    }

    /// With gradient enabled, `bg_gradient_stops()` returns `(top, bottom)` where
    /// top = `bg_top` and bottom = `bg` — the single decision the renderer branches
    /// on to paint the vertical mesh.
    #[test]
    fn gradient_stops_when_enabled() {
        let o = ChartOptions {
            bg_gradient: true,
            bg_top: [22, 27, 36],
            bg: [13, 17, 23],
            ..Default::default()
        };
        assert_eq!(
            o.bg_gradient_stops(),
            Some((Color32::from_rgb(22, 27, 36), Color32::from_rgb(13, 17, 23))),
            "top = bg_top, bottom = bg",
        );
    }

    /// Forward-compat: a legacy workspace file written before the gradient fields
    /// existed (no `bg_gradient`/`bg_top` keys) picks up the NEW default via
    /// struct-level `#[serde(default)]` — so old layouts also adopt the TV-style
    /// gradient. A user who had customized `bg` keeps it as the bottom stop.
    #[test]
    fn legacy_json_without_gradient_fields_adopts_default_gradient() {
        let v1 = r#"{"up":[91,190,145],"bg":[13,17,23],"show_volume":true}"#;
        let o: ChartOptions = serde_json::from_str(v1).expect("v1 file deserializes");
        assert!(o.bg_gradient, "absent bg_gradient ⇒ default (gradient on)");
        assert_eq!(o.bg_top, [22, 27, 36], "absent bg_top ⇒ default (#161B24)");
        assert_eq!(
            o.bg_gradient_stops(),
            Some((Color32::from_rgb(22, 27, 36), Color32::from_rgb(13, 17, 23))),
        );
    }
}
