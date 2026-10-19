//! The state a HEADLESS caller of [`super::draw`] owns across its frames, lent out as one
//! [`ChartInputs`], and the font prerequisite of every headless frame.
//!
//! [`HeadlessChart`] owns the per-frame mutable state (`FollowLive`, the two dialogs,
//! `PaneFractions`) and the empty maps a plain chart passes. [`HeadlessChart::inputs`] fills every
//! other field with the default [`ChartInputs`] documents as byte-identical (no nav, no studies,
//! overlays or sub-panes, no footprint, no sync, no GPU seam, Linear, not inverted), so a harness
//! spells only the fields it varies, by struct update:
//! `ChartInputs { scale, ..chart.inputs(state, style, indicators, &options) }`.
//!
//! It exists so the 25-field literal is not copied into every harness:
//! `crates/vike-chart/tests/common/mod.rs`'s `Case` and `crates/vike-chart/examples/export_png.rs`'s
//! `render_frames` use it. A harness whose fixture owns this state itself (so its tests can pose and
//! read it) keeps its own literal. Production (`vike-desktop`) spells its own: its state lives in
//! its windows, not here.

use indexmap::IndexMap;

use super::ChartInputs;
use super::style::ChartStyle;
use crate::indicators::Active;
use crate::interact::FollowLive;
use crate::model::ChartState;
use crate::options::{ChartOptions, IndicatorDialog, SettingsDialog};
use crate::panes::{PaneFractions, PaneKey};
use crate::scale::{ScaleAssign, ScaleMode};

/// The mutable state [`super::draw`] needs across one headless scenario's frames. Create one per
/// scenario (`Default`) so scenarios stay independent; it persists across that scenario's frames.
#[derive(Default)]
pub struct HeadlessChart {
    follow: FollowLive,
    settings: SettingsDialog,
    indicator_dialog: IndicatorDialog,
    panes: PaneFractions,
    study_pane_of: IndexMap<u64, PaneKey>,
    series_pane_of: IndexMap<String, PaneKey>,
    series_scale: IndexMap<String, ScaleAssign>,
}

impl HeadlessChart {
    /// This frame's [`ChartInputs`]: the caller's chart, style, indicators and options, this
    /// value's mutable state, and the byte-identical default for every other field.
    pub fn inputs<'a>(
        &'a mut self,
        state: &'a ChartState,
        style: ChartStyle,
        indicators: &'a [Active],
        options: &'a ChartOptions,
    ) -> ChartInputs<'a> {
        ChartInputs {
            state,
            style,
            nav: None,
            indicators,
            studies: &[],
            follow: &mut self.follow,
            options,
            settings: &mut self.settings,
            indicator_dialog: &mut self.indicator_dialog,
            scale: ScaleMode::Linear,
            invert: false,
            panes: &mut self.panes,
            sync: None,
            footprint: None,
            footprint_gen: 0,
            cvd_on: false,
            profile_on: false,
            of_tick_size: 0.0,
            sub_panes: &[],
            study_pane_of: &self.study_pane_of,
            overlays: &[],
            series_panes: &[],
            series_pane_of: &self.series_pane_of,
            series_scale: &self.series_scale,
            gpu_candles: None,
        }
    }
}

/// Installs the app's type — `vike_ui_theme::appearance::install_type`, the bundled faces AND the
/// role text sizes — so a headless frame measures the app's widths, and the named families
/// (`"semibold"`, `"bold"`) that [`super::draw`] and [`crate::options_chain::draw`] ask for are
/// bound: epaint PANICS on an unbound one. It changes no action value either returns.
///
/// ⚠ For a tessellation golden this is also the whole reason TEXT GEOMETRY is reproducible: the set
/// is font data EMBEDDED in the binary, never a system-font lookup, so text layout is the same
/// pure-Rust arithmetic on every machine.
pub fn bind_chart_font_families(ctx: &egui::Context) {
    vike_ui_theme::appearance::install_type(ctx, vike_ui_theme::type_scale::TextSize::Small);
}
