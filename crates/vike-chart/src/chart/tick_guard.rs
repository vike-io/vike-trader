//! The tick labels a frame leaves undrawn. An axis label is painted by egui_plot for every mark whose
//! formatter returns text, and an EMPTY string means "no label": that is the only lever, and it is
//! pulled where the label is formatted, so [`TickGuard::hides`] answers for one mark of one axis.
//!
//! The formatter sees a mark's value and the axis' visible value range, never a pixel. The pixel
//! scale comes from the plot's own grid spacer instead: egui_plot calls it on every frame BEFORE it
//! formats that frame's labels (`render_axis_widgets`), with `base_step_size` = value-per-pixel x the
//! plot's minimum grid spacing, so a spacer that calls [`TickGuard::note_spacing`] hands the formatter
//! the exact pixels-per-value of THIS frame's transform, with no one-frame lag after a resize.
//! [`grid_spacing`] is that minimum, passed to every plot explicitly so the number divided by here is
//! the number the plot used. The chips an axis will carry are known BEFORE its labels are formatted
//! (the price pane's closure, which runs first, and the sub-panes' own values) and are set with
//! [`TickGuard::set_chips`]. A guard lives for one frame; unset, it hides nothing.
//!
//! A label is judged by its VISUAL extent (the glyphs' mesh bounds, which is what a reader sees and
//! what `TextShape::visual_bounding_rect` reports), not by its taller layout row, and a chip by the
//! rect its painter fills: a label that is visibly clear stays, so a frame without a collision is
//! unchanged.
//!
//! Owner rulings it carries out (TradingView style):
//! - a tick label never sticks out of its pane, and one that would is not drawn: a y label past the
//!   pane's top or bottom edge, a time label cut by the pane's left edge (which is the window's);
//! - where a price chip, a value tag or the crosshair's tags sit on a tick label, the CHIP wins and
//!   the label under it is not drawn. The chip is placed by the very function the painters use
//!   (`overlay::chip_centre`), so the label that gives way is the one the chip really covers.

use super::overlay::{Fit, chip_centre};
use egui::{Color32, FontId, Rangef, Rect, Vec2};
use egui_plot::GridInput;
use std::cell::Cell;
use std::ops::RangeInclusive;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::{TextRole, TextSize};

/// egui_plot's own default grid spacing (8pt between the thinnest grid lines, 300pt for the
/// strongest), spelled out so [`TickGuard::note_spacing`] divides by what the plot really used.
pub(crate) fn grid_spacing() -> Rangef {
    Rangef::new(8.0, 300.0)
}

/// Lays text out in the axis font (JetBrains Mono at the Caption role, which egui_plot's tick labels
/// and every chip use) to read its size and its visual bounds back. egui caches the layouts.
#[derive(Clone)]
pub(crate) struct Probe {
    painter: egui::Painter,
    font: FontId,
}

impl Probe {
    pub(crate) fn new(ui: &egui::Ui, ts: TextSize) -> Self {
        Probe { painter: ui.painter().clone(), font: FontId::monospace(ts.px(TextRole::Caption)) }
    }

    /// The layout size of `text`, and the bounds of its glyphs relative to the layout's top-left.
    fn lay(&self, text: &str) -> (Vec2, Rect) {
        let galley =
            self.painter.layout_no_wrap(text.to_owned(), self.font.clone(), Color32::TRANSPARENT);
        (galley.size(), galley.mesh_bounds)
    }
}

/// An axis chip as the guard needs it: where its value sits, how long it is along the axis, and how
/// its painter places it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Chip {
    value: f64,
    len: f32,
    fit: Fit,
}

/// Which way an axis runs on screen.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Direction {
    /// A price/value axis: the top edge is the range's end.
    Y,
    /// The time axis: the left edge is the range's start. `window` is how far right of the pane's
    /// left edge the window ends, the bound a time tag is kept inside.
    X { window: f32 },
}

/// One axis of one pane for one frame.
pub(crate) struct TickGuard {
    probe: Probe,
    dir: Direction,
    /// Pixels per value unit of the axis this frame; NaN until the spacer has run.
    px_per_unit: Cell<f64>,
    /// The chips painted on this axis this frame (the price axis has two: the last-price chip and
    /// the crosshair's price tag).
    chips: Cell<[Option<Chip>; 2]>,
}

impl TickGuard {
    fn new(probe: &Probe, dir: Direction) -> Self {
        TickGuard {
            probe: probe.clone(),
            dir,
            px_per_unit: Cell::new(f64::NAN),
            chips: Cell::new([None; 2]),
        }
    }

    /// The guard of a y axis.
    pub(crate) fn y(probe: &Probe) -> Self {
        Self::new(probe, Direction::Y)
    }

    /// The guard of the time axis of a chart `window` points wide, its pane flush with the left edge.
    pub(crate) fn x(probe: &Probe, window: f32) -> Self {
        Self::new(probe, Direction::X { window })
    }

    /// Record this frame's pixel scale from the axis' grid spacer (see the module doc).
    pub(crate) fn note_spacing(&self, input: &GridInput) {
        self.px_per_unit.set(f64::from(grid_spacing().min) / input.base_step_size);
    }

    /// The chips this frame paints on the axis; their tick labels are left undrawn.
    pub(crate) fn set_chips(&self, chips: [Option<Chip>; 2]) {
        self.chips.set(chips);
    }

    /// A chip on this y axis, as `overlay.rs`'s painters make one: a text row plus its vertical
    /// padding tall, at `value`, placed by `fit`.
    pub(crate) fn y_chip(&self, value: f64, fit: Fit) -> Chip {
        Chip { value, len: self.probe.lay("0").0.y + space::XS * 2.0, fit }
    }

    /// The crosshair time tag on this x axis: `text` wide plus its horizontal padding, kept inside
    /// the window.
    pub(crate) fn time_chip(&self, value: f64, text: &str) -> Chip {
        Chip { value, len: self.probe.lay(text).0.x + space::MD * 2.0, fit: Fit::Inside }
    }

    /// Is the label `text` of the mark at `value`, on an axis showing `range`, left undrawn? Not
    /// before the pixel scale is known, and never for an empty text. Otherwise when
    /// - a y label's glyphs stick out of the pane by more than half a point;
    /// - a time label's glyphs straddle the pane's left edge by more than half a point (one wholly
    ///   past it is clipped away unseen, and left alone; the right edge is not a cut: the gutter is
    ///   there);
    /// - a chip on this axis overlaps the glyphs by more than half a point along the axis.
    pub(crate) fn hides(&self, value: f64, range: &RangeInclusive<f64>, text: &str) -> bool {
        let ppu = self.px_per_unit.get();
        if text.is_empty() || !(ppu.is_finite() && ppu > 0.0) {
            return false;
        }
        let (lo, hi) = (*range.start(), *range.end());
        // Pixels from the pane's top edge (a y axis grows upwards) or left edge.
        let pos = |v: f64| -> f32 {
            match self.dir {
                Direction::Y => ((hi - v) * ppu) as f32,
                Direction::X { .. } => ((v - lo) * ppu) as f32,
            }
        };
        let extent = ((hi - lo) * ppu) as f32;
        // egui_plot centres a label on its mark (the layout row on a y mark, the layout width on an
        // x mark), so the glyphs sit `near..far` off it along the axis.
        let (size, glyphs) = self.probe.lay(text);
        let (near, far) = match self.dir {
            Direction::Y => (glyphs.min.y - size.y / 2.0, glyphs.max.y - size.y / 2.0),
            Direction::X { .. } => (glyphs.min.x - size.x / 2.0, glyphs.max.x - size.x / 2.0),
        };
        let s = pos(value);
        let out_of_pane = match self.dir {
            Direction::Y => s + near < -0.5 || s + far > extent + 0.5,
            Direction::X { .. } => s + near < -0.5 && s + far > 0.0,
        };
        let bounds = match self.dir {
            Direction::Y => (0.0, extent),
            Direction::X { window } => (0.0, window),
        };
        out_of_pane
            || self.chips.get().into_iter().flatten().any(|chip| {
                chip_centre(chip.fit, pos(chip.value), chip.len, bounds.0, bounds.1).is_some_and(
                    |c| {
                        (s + far).min(c + chip.len / 2.0) - (s + near).max(c - chip.len / 2.0) > 0.5
                    },
                )
            })
    }
}

#[path = "tick_guard_tests.rs"]
#[cfg(test)]
mod tick_guard_tests;
