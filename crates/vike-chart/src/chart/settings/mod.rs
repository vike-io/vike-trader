//! Per-window chart appearance/behavior options + the settings-dialog state
//! (chart-UX bundle T6). Extracts the palette + always-on flags that used to
//! be hard-coded `Color32` consts in the painters (then `render/`) into ONE serde-able struct so
//! a per-window "Chart settings" dialog can drive them live, and so a future
//! task (T10) can persist the full set.
//!
//! `ChartSettings` is the single source of truth for the series colors
//! (candle up/down, semantic up/down, line, grid, crosshair) + the canvas
//! background, the grid toggles (master + per-axis vertical/horizontal) and the
//! top/bottom autofit margins, the last-price toggle, the price-precision
//! override (Linear/Log only — Percent keeps its fixed `{:.2}%`), and the
//! volume-pane toggle (moved here from
//! `WindowState`). `chart::draw` reads the EFFECTIVE options each frame —
//! `settings.working` while the dialog is open (live preview), the committed
//! options otherwise — and `crate::chart::colors::ChartColors::resolve` turns them into
//! what the painters read. No plotting or interaction state lives here (pure data); the dialog
//! that edits it is `dialog.rs`. The "Indicator settings" dialog's state is `crate::chart::study`'s.

pub(crate) mod dialog;

/// Per-window chart appearance + behavior; the dialog mutates a working copy of this.
///
/// Every COLOUR is an override of the appearance (design system spec §3.5): `None` follows the
/// installed market colours or theme (`crate::chart::colors::ChartColors::resolve` says which), `Some`
/// is a colour the user picked in Chart settings for THIS chart, and it stays through every theme
/// and market-colour change. An unset colour is not written to a workspace file, and a written
/// one is the user's, whatever its value.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
// Struct-level default (uses the custom `Default` impl below) so that when this
// struct is `#[serde(flatten)]`ed into a persisted `WindowSnapshot` (vike-app-core workspace
// v3), a field ABSENT from the JSON deserializes to its CUSTOM default value
// rather than a type-zero: an absent colour loads UNSET, an absent flag takes its default.
#[serde(default)]
pub struct ChartSettings {
    /// Candle body (up). `None` follows the market colours (design system spec §3.5); `Some` is a
    /// colour picked in Chart settings for THIS chart, and it stays.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub up: Option<[u8; 3]>,
    /// Candle body (down). `None` follows the market colours.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub down: Option<[u8; 3]>,
    /// Candle body BORDER (up) — TV "Borders" row. `None` follows the resolved body colour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_up: Option<[u8; 3]>,
    /// Candle body BORDER (down). `None` follows the resolved body colour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_down: Option<[u8; 3]>,
    /// Candle WICK (up) — TV "Wick" row. `None` follows the resolved body colour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wick_up: Option<[u8; 3]>,
    /// Candle WICK (down). `None` follows the resolved body colour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wick_down: Option<[u8; 3]>,
    /// UP as a graphic — histograms, markers, the baseline style, the last-price line. `None`
    /// follows the market colours; a pick here also colours up TEXT and volume.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub up_graphic: Option<[u8; 3]>,
    /// DOWN as a graphic. `None` follows the market colours.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub down_graphic: Option<[u8; 3]>,
    /// Line, area and step series. `None` follows the market "up".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<[u8; 3]>,
    /// Gridlines. `None` follows the theme's hover step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grid: Option<[u8; 3]>,
    /// Crosshair. `None` follows the theme's secondary text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crosshair: Option<[u8; 3]>,
    /// Chart canvas background (TV Canvas "Background"); the gradient BOTTOM stop. `None` follows
    /// the theme's background.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<[u8; 3]>,
    /// TV "Background: Gradient" mode. When set, the canvas is a vertical gradient
    /// from `background_top` (top of the chart) down to `background` (bottom), painted ONCE
    /// behind every pane so the pane separators reveal the bands — exactly TradingView's
    /// continuous-across-panes background. ON by default (the subtle-lift look);
    /// set `false` for the old flat `background` fill.
    pub background_gradient: bool,
    /// The gradient's TOP stop (TV Background gradient, upper swatch); `background` is the
    /// bottom stop. Ignored when `background_gradient` is false. `None` follows the theme's
    /// gradient top — a soft lift above its background, a gentle fade, not a heavy vignette.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_top: Option<[u8; 3]>,
    /// TV "Color bars based on previous close": when set, a candle's bull/bear
    /// classification (which drives its body/border/wick color) tests
    /// `close >= previous bar's close` instead of `close >= open`. The first
    /// drawn bar (no predecessor in the slice) falls back to `close >= open`.
    pub color_bars_previous_close: bool,
    pub show_grid: bool,
    /// Vertical gridlines (the x-axis grid — lines spaced along time). TV Canvas
    /// splits the single grid toggle into vertical + horizontal; both AND with
    /// `show_grid` so the legacy master toggle still hides everything.
    pub show_vertical_grid: bool,
    /// Horizontal gridlines (the y-axis grid — lines spaced along price).
    pub show_horizontal_grid: bool,
    /// Top chart margin as a % of the visible price span — the empty room above
    /// the highest bar when y-autofit is engaged (TV Canvas "Margins → Top").
    /// `5.0` is exactly today's fixed 5% autofit pad (see `extent::y_pad`).
    pub margin_top_percent: f32,
    /// Bottom chart margin as a % of the visible price span (TV "Margins → Bottom").
    pub margin_bottom_percent: f32,
    pub show_last_price: bool,
    /// Fixed decimal places for Linear/Log price readouts (axis / crosshair
    /// tag / last-price chip / OHLC legend). `None` = today's auto formatting
    /// (comma-grouped, 2 dp). Percent mode ignores this (spec §1).
    pub precision: Option<u8>,
    pub show_volume: bool,
}

impl Default for ChartSettings {
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
            up_graphic: None,
            down_graphic: None,
            line: None,
            grid: None,
            crosshair: None,
            background: None,
            background_gradient: true, // TV-style gradient ON by default (the "subtle lift" look)
            background_top: None,
            color_bars_previous_close: false,
            show_grid: true,
            show_vertical_grid: true,
            show_horizontal_grid: true,
            margin_top_percent: 5.0, // == today's fixed 5% autofit pad (extent::y_pad)
            margin_bottom_percent: 5.0,
            show_last_price: true,
            precision: None,
            show_volume: true,
        }
    }
}

impl ChartSettings {
    /// Per-axis grid visibility as an egui `Vec2b` — `x` = vertical lines
    /// (x-axis grid), `y` = horizontal lines (y-axis grid). Each AND's with the
    /// master `show_grid` so the legacy toggle keeps hiding the whole grid.
    pub(crate) fn grid_show(&self) -> egui::Vec2b {
        egui::Vec2b::new(
            self.show_grid && self.show_vertical_grid,
            self.show_grid && self.show_horizontal_grid,
        )
    }
}

/// The left-navigation sections of the "Chart settings" dialog, mirroring TradingView's
/// tabbed layout (its oracle: Symbol / Canvas / Scales, plus Trading/Alerts/Events
/// vike does not yet have). Each maps a slice of [`ChartSettings`] to one panel.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum ChartSettingsDialogTab {
    /// Candle colors + price precision (TV "Symbol").
    #[default]
    Symbol,
    /// Grid, crosshair, series/appearance colors (TV "Canvas").
    Canvas,
    /// Last-price line + volume pane (TV "Scales and lines").
    Scales,
}

impl ChartSettingsDialogTab {
    pub const ALL: [ChartSettingsDialogTab; 3] = [
        ChartSettingsDialogTab::Symbol,
        ChartSettingsDialogTab::Canvas,
        ChartSettingsDialogTab::Scales,
    ];
    pub fn label(self) -> &'static str {
        match self {
            ChartSettingsDialogTab::Symbol => "Symbol",
            ChartSettingsDialogTab::Canvas => "Canvas",
            ChartSettingsDialogTab::Scales => "Scales and lines",
        }
    }
}

/// Per-window "Chart settings" dialog state (chart-UX bundle T6, RESOLUTION 2 —
/// a working-copy design). `open` gates the `egui::Window`; `working` is a
/// scratch copy the dialog widgets mutate live; `tab` is the selected left-navigation
/// section. On OK the working copy is committed (`ChartActions::options_change`);
/// on Cancel/close it is discarded, so the committed options are never touched
/// mid-edit.
#[derive(Clone, Default)]
pub struct ChartSettingsDialog {
    pub open: bool,
    pub tab: ChartSettingsDialogTab,
    pub working: ChartSettings,
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
        "up_graphic",
        "down_graphic",
        "line",
        "grid",
        "crosshair",
        "background",
        "background_top",
    ];

    /// Every colour the chart compiled in before it followed the appearance (spec §3.5), spelled
    /// in the version-3 words.
    const OLD_DEFAULTS_JSON: &str = r#"{"up":[91,190,145],"down":[217,84,88],"border_up":[91,190,145],
        "border_down":[217,84,88],"wick_up":[91,190,145],"wick_down":[217,84,88],
        "up_graphic":[64,186,80],"down_graphic":[248,82,73],"line":[64,186,80],"grid":[34,39,47],
        "crosshair":[154,164,177],"background":[13,17,23],"background_top":[22,27,36]}"#;

    /// A fresh chart follows the appearance: every colour unset, the flags today's.
    #[test]
    fn the_default_chart_follows_the_appearance() {
        let o = ChartSettings::default();
        let colours = [
            o.up,
            o.down,
            o.border_up,
            o.border_down,
            o.wick_up,
            o.wick_down,
            o.up_graphic,
            o.down_graphic,
            o.line,
            o.grid,
            o.crosshair,
            o.background,
            o.background_top,
        ];
        assert!(colours.iter().all(Option::is_none), "{o:?}");
        assert!(o.background_gradient && o.show_grid && o.show_vertical_grid);
        assert!(o.show_horizontal_grid && o.show_last_price);
        assert!(o.show_volume && !o.color_bars_previous_close);
        assert_eq!((o.margin_top_percent, o.margin_bottom_percent, o.precision), (5.0, 5.0, None));
    }

    /// A version-3 file was written after colours followed the appearance, so every colour in it is
    /// one the user picked — one equal to a pre-PR-7 compiled default included. (Spec §3.5's legacy
    /// rule unset such a colour; it read files from before version 3, which are refused now.)
    #[test]
    fn a_colour_equal_to_an_old_compiled_default_stays_picked() {
        let o: ChartSettings = serde_json::from_str(OLD_DEFAULTS_JSON).expect("the colours parse");
        let picked = [o.up, o.grid, o.crosshair, o.background, o.background_top];
        let want = [[91, 190, 145], [34, 39, 47], [154, 164, 177], [13, 17, 23], [22, 27, 36]];
        assert_eq!(picked, want.map(Some));
        assert_ne!(o, ChartSettings::default());
    }

    /// Any other stored colour is the user's — one level off the old default included.
    #[test]
    fn a_colour_the_user_picked_loads_as_an_override() {
        let o: ChartSettings =
            serde_json::from_str(r#"{"up":[1,2,3],"grid":[34,39,48],"background":null}"#).unwrap();
        assert_eq!((o.up, o.grid, o.background), (Some([1, 2, 3]), Some([34, 39, 48]), None));
    }

    /// An unset colour is not written, so an older build reading a new file keeps its own default.
    #[test]
    fn an_unset_colour_is_not_written() {
        let v = serde_json::to_value(ChartSettings::default()).unwrap();
        for k in COLOUR_KEYS {
            assert!(v.get(k).is_none(), "{k} was written: {v}");
        }
        let edited = ChartSettings { up: Some([1, 2, 3]), ..ChartSettings::default() };
        let back: ChartSettings =
            serde_json::from_str(&serde_json::to_string(&edited).unwrap()).unwrap();
        assert_eq!(back, edited);
    }

    /// Gradient is ON by default (TradingView-style): a subtle vertical fade from the top stop
    /// down to the bottom one, so a fresh chart reads like TV out of the box. Both stops are
    /// unset — they follow the theme's gradient top and background (`crate::chart::colors`).
    #[test]
    fn default_background_is_gradient() {
        let o = ChartSettings::default();
        assert!(o.background_gradient, "gradient on by default (TV-style)");
        assert_eq!((o.background_top, o.background), (None, None), "both stops follow the theme");
    }

    /// A file without the gradient keys (`background_gradient`/`background_top`) picks up the
    /// default via struct-level `#[serde(default)]`, so it adopts the TV-style gradient. A user
    /// who had customized `background` keeps it as the bottom stop.
    #[test]
    fn json_without_gradient_fields_adopts_default_gradient() {
        let partial = r#"{"up":[91,190,145],"background":[13,17,23],"show_volume":true}"#;
        let o: ChartSettings = serde_json::from_str(partial).expect("a partial file deserializes");
        assert!(o.background_gradient, "absent background_gradient ⇒ default (gradient on)");
        assert_eq!(o.background_top, None, "absent background_top ⇒ unset: the theme's top");
        assert_eq!(o.up, Some([91, 190, 145]), "a written colour is the user's");
        assert_eq!(o.background, Some([13, 17, 23]), "…and stays the bottom stop");
    }
}
