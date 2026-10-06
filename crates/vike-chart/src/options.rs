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
//! options otherwise — and `crate::colors::ChartColors::resolve` turns them into
//! what the painters read. No plotting or interaction state lives here (pure data).

use crate::indicators::{LineDash, Source};
use egui::Color32;

/// Per-window chart appearance + behavior; the dialog mutates a working copy of this.
///
/// Every COLOUR is an override of the appearance (design system spec §3.5): `None` follows the
/// installed market colours or theme (`crate::colors::ChartColors::resolve` says which), `Some`
/// is a colour the user picked in Chart settings for THIS chart, and it stays through every theme
/// and market-colour change. An unset colour is not written to a workspace file.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
// Struct-level default (uses the custom `Default` impl below) so that when this
// struct is `#[serde(flatten)]`ed into a persisted `WinSnap` (vike-app-core workspace
// v2, T10), a field ABSENT from the JSON — e.g. every colour/flag in a v1 file
// that only carried `show_volume` — deserializes to its CUSTOM default value
// rather than a type-zero: an absent colour loads UNSET, an absent flag takes its default.
// A PRESENT colour equal to the pre-PR-7 compiled default also loads unset — the [`legacy`]
// rule, because every window used to save its colours whether or not the user touched them.
#[serde(default)]
pub struct ChartOptions {
    /// Candle body (up). `None` follows the market colours (design system spec §3.5); `Some` is a
    /// colour picked in Chart settings for THIS chart, and it stays.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "legacy::up")]
    pub up: Option<[u8; 3]>,
    /// Candle body (down). `None` follows the market colours.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "legacy::down")]
    pub down: Option<[u8; 3]>,
    /// Candle body BORDER (up) — TV "Borders" row. `None` follows the resolved body colour.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "legacy::border_up")]
    pub border_up: Option<[u8; 3]>,
    /// Candle body BORDER (down). `None` follows the resolved body colour.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "legacy::border_down")]
    pub border_down: Option<[u8; 3]>,
    /// Candle WICK (up) — TV "Wick" row. `None` follows the resolved body colour.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "legacy::wick_up")]
    pub wick_up: Option<[u8; 3]>,
    /// Candle WICK (down). `None` follows the resolved body colour.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "legacy::wick_down")]
    pub wick_down: Option<[u8; 3]>,
    /// UP as a graphic — histograms, markers, the baseline style, the last-price line. `None`
    /// follows the market colours; a pick here also colours up TEXT and volume.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "legacy::up_s")]
    pub up_s: Option<[u8; 3]>,
    /// DOWN as a graphic. `None` follows the market colours.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "legacy::down_s")]
    pub down_s: Option<[u8; 3]>,
    /// Line, area and step series. `None` follows the market "up".
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "legacy::line")]
    pub line: Option<[u8; 3]>,
    /// Gridlines. `None` follows the theme's hover step.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "legacy::grid")]
    pub grid: Option<[u8; 3]>,
    /// Crosshair. `None` follows the theme's secondary text.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "legacy::cross")]
    pub cross: Option<[u8; 3]>,
    /// Chart canvas background (TV Canvas "Background"); the gradient BOTTOM stop. `None` follows
    /// the theme's background.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "legacy::bg")]
    pub bg: Option<[u8; 3]>,
    /// TV "Background: Gradient" mode. When set, the canvas is a vertical gradient
    /// from `bg_top` (top of the chart) down to `bg` (bottom), painted ONCE behind
    /// every pane so the pane separators reveal the bands — exactly TradingView's
    /// continuous-across-panes background. ON by default (the subtle-lift look);
    /// set `false` for the old flat `bg` fill.
    pub bg_gradient: bool,
    /// The gradient's TOP stop (TV Background gradient, upper swatch); `bg` is the
    /// bottom stop. Ignored when `bg_gradient` is false. `None` follows the theme's gradient
    /// top — a soft lift above its background, a gentle fade, not a heavy vignette.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "legacy::bg_top")]
    pub bg_top: Option<[u8; 3]>,
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
    /// A fresh chart follows the appearance — every colour unset — with the always-on
    /// grid/last-price behaviors, auto precision and volume on. Verified by
    /// `tests::the_default_chart_follows_the_appearance`.
    fn default() -> Self {
        Self {
            up: None,
            down: None,
            border_up: None,
            border_down: None,
            wick_up: None,
            wick_down: None,
            up_s: None,
            down_s: None,
            line: None,
            grid: None,
            cross: None,
            bg: None,
            bg_gradient: true, // TV-style gradient ON by default (the "subtle lift" look)
            bg_top: None,
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
    /// default) is a bit-identical no-op. Applied by the vike-desktop apply path with a
    /// refold, exactly like a `params` change.
    pub source: Source,
    /// per-output `(color, width, visible, line_style)`, index-aligned to
    /// `Active::outputs`.
    pub lines: Vec<(Color32, f32, bool, LineDash)>,
    /// master show toggle for the oscillator reference bands (RSI 30/50/70, …).
    pub show_bands: bool,
    /// per-level reference bands `(value, color, show)`, index-aligned to
    /// `Active::bands` — each level has its OWN colour + show toggle (TradingView RSI
    /// "Style" parity). `color` is `BandLevel::color`'s override: `None` = no colour chosen,
    /// the theme's level line.
    pub bands: Vec<(f64, Option<Color32>, bool)>,
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

/// Spec §3.5's legacy rule. Before PR 7 every window saved the colours below whether or not the
/// user had touched them, so a stored value EQUAL to one of them is the default nobody chose: it
/// loads unset and follows the appearance. Any other value is the user's.
///
/// ⚠ These describe files already on disk. Never edit a value here: an edit silently turns every
/// untouched chart saved before PR 7 into one "the user picked".
mod legacy {
    use serde::{Deserialize, Deserializer};

    macro_rules! unset_if_legacy {
        ($($field:ident = $rgb:expr;)*) => {$(
            pub(super) fn $field<'de, D: Deserializer<'de>>(d: D) -> Result<Option<[u8; 3]>, D::Error> {
                Ok(Option::<[u8; 3]>::deserialize(d)?.filter(|c| *c != $rgb))
            }
        )*};
    }

    unset_if_legacy! {
        up = [91, 190, 145];
        down = [217, 84, 88];
        border_up = [91, 190, 145];
        border_down = [217, 84, 88];
        wick_up = [91, 190, 145];
        wick_down = [217, 84, 88];
        up_s = [64, 186, 80];
        down_s = [248, 82, 73];
        line = [64, 186, 80];
        grid = [34, 39, 47];
        cross = [154, 164, 177];
        bg = [13, 17, 23];
        bg_top = [22, 27, 36];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The thirteen colour keys, as a workspace file spells them.
    const COLOUR_KEYS: [&str; 13] = [
        "up",
        "down",
        "border_up",
        "border_down",
        "wick_up",
        "wick_down",
        "up_s",
        "down_s",
        "line",
        "grid",
        "cross",
        "bg",
        "bg_top",
    ];

    /// Every colour the chart compiled in before it followed the appearance, as a pre-PR-7
    /// workspace stores them.
    const LEGACY_JSON: &str = r#"{"up":[91,190,145],"down":[217,84,88],"border_up":[91,190,145],
        "border_down":[217,84,88],"wick_up":[91,190,145],"wick_down":[217,84,88],
        "up_s":[64,186,80],"down_s":[248,82,73],"line":[64,186,80],"grid":[34,39,47],
        "cross":[154,164,177],"bg":[13,17,23],"bg_top":[22,27,36]}"#;

    /// A fresh chart follows the appearance: every colour unset, the flags today's.
    #[test]
    fn the_default_chart_follows_the_appearance() {
        let o = ChartOptions::default();
        let colours = [
            o.up,
            o.down,
            o.border_up,
            o.border_down,
            o.wick_up,
            o.wick_down,
            o.up_s,
            o.down_s,
            o.line,
            o.grid,
            o.cross,
            o.bg,
            o.bg_top,
        ];
        assert!(colours.iter().all(Option::is_none), "{o:?}");
        assert!(o.bg_gradient && o.show_grid && o.show_vgrid && o.show_hgrid && o.show_last_price);
        assert!(o.show_volume && !o.color_bars_prev_close);
        assert_eq!((o.margin_top_pct, o.margin_bottom_pct, o.precision), (5.0, 5.0, None));
    }

    /// Spec §3.5: a stored colour equal to the old compiled default is the default the user never
    /// chose, so it loads UNSET.
    #[test]
    fn a_legacy_default_colour_loads_unset() {
        let o: ChartOptions = serde_json::from_str(LEGACY_JSON).expect("a pre-PR-7 file parses");
        assert_eq!(o, ChartOptions::default());
    }

    /// Any other stored colour is the user's — one level off the old default included.
    #[test]
    fn a_colour_the_user_picked_loads_as_an_override() {
        let o: ChartOptions =
            serde_json::from_str(r#"{"up":[1,2,3],"grid":[34,39,48],"bg":null}"#).unwrap();
        assert_eq!((o.up, o.grid, o.bg), (Some([1, 2, 3]), Some([34, 39, 48]), None));
    }

    /// An unset colour is not written, so an older build reading a new file keeps its own default.
    #[test]
    fn an_unset_colour_is_not_written() {
        let v = serde_json::to_value(ChartOptions::default()).unwrap();
        for k in COLOUR_KEYS {
            assert!(v.get(k).is_none(), "{k} was written: {v}");
        }
        let edited = ChartOptions { up: Some([1, 2, 3]), ..ChartOptions::default() };
        let back: ChartOptions =
            serde_json::from_str(&serde_json::to_string(&edited).unwrap()).unwrap();
        assert_eq!(back, edited);
    }

    /// Gradient is ON by default (TradingView-style): a subtle vertical fade from the top stop
    /// down to the bottom one, so a fresh chart reads like TV out of the box. Both stops are
    /// unset — they follow the theme's gradient top and background (`crate::colors`).
    #[test]
    fn default_background_is_gradient() {
        let o = ChartOptions::default();
        assert!(o.bg_gradient, "gradient on by default (TV-style)");
        assert_eq!((o.bg_top, o.bg), (None, None), "both stops follow the theme");
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
        assert_eq!(o.bg_top, None, "absent bg_top ⇒ unset: the theme's gradient top");
        assert_eq!((o.up, o.bg), (None, None), "old defaults load unset");
    }
}
