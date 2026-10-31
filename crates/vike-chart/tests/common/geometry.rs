//! The geometry half of the chart size / DPI matrix (`tests/chart_sizes.rs`): WHERE `chart::draw`
//! put the pane frames, tick labels and axis tag chips in one frame, and the pure checkers that say
//! whether those places are legal. Property assertions only — no goldens, no pixels — so a frame
//! at any size and any `pixels_per_point` is judged by what it must never do, not by what it once did.
//!
//! WHY THE FRAMES COME FROM `egui_plot::PlotMemory`: `draw` returns no geometry, and each pane's data
//! rect exists only inside it as its plot's `transform.frame()`. egui_plot persists that transform
//! under `ui.make_persistent_id(IdSalt::new(<plot id salt>))`, so a probe that runs inside the frame's
//! `ctx.run_ui` closure, right after `draw`, reads it back with the SAME `ui` (`plot_transform`;
//! `Case::run_probed` is the seam). The salts are the ones `crates/vike-chart/src/chart/mod.rs`
//! (`"price"`) and `crates/vike-chart/src/chart/subpanes.rs` (`"volume"`, `"cvd"`,
//! `format!("pane_{key:?}")`) pass to `Plot::new`; a salt that drifts makes `plot_transform` return
//! `None`, which the matrix turns into a loud panic rather than an empty (vacuously green) frame.
//!
//! HOW A SHAPE IS CLASSIFIED (no production change needed): tick labels are `Shape::Text`; an axis tag
//! is a `Shape::Rect` whose corner radius is `chart::AXIS_TAG_RADIUS` (used only by the four tag
//! painters in `overlay.rs`) sitting `space::HAIR` beside a pane's right edge (a price-side chip) or
//! under the bottom pane (the time tag), followed by the `Shape::Text` painted at `chip.min + pad`.
//! The crosshair price tag is the gutter chip filled with the theme's `tag_bg`; the last-price chip is
//! the FIRST other gutter chip in paint order (`paint_last_price_chip` runs before any sub-pane's
//! value tag); the rest are value tags.
//!
//! The kill proofs for every checker live in `tests/chart_sizes.rs` (a `#[test]` here would run once
//! per including test binary).

use egui::epaint::{RectShape, Shape, TextShape};
use egui::{Color32, CornerRadius, Rect, Vec2};
use vike_ui_theme::metrics::space;
use vike_ui_theme::value::chart;

/// Slack, in points. Embedded fonts make Windows and Linux text metrics agree, so this is only the
/// slop of f32 arithmetic and of a sub-pixel glyph bearing, not a platform allowance.
pub const TOL: f32 = 0.5;

// ============================ reading a frame ============================

/// A plot's last transform, read back inside the frame's `run_ui` closure with the same `ui` that
/// `draw` was given. `None` = no plot with that id salt has been shown on this context.
pub fn plot_transform(ui: &egui::Ui, id_salt: &str) -> Option<egui_plot::PlotTransform> {
    // egui_plot 0.38's own derivation: `ui.make_persistent_id(IdSalt::new(salt))` (0.37 hashed an `Id`).
    let id = ui.make_persistent_id(egui::IdSalt::new(id_salt.to_owned()));
    egui_plot::PlotMemory::load(ui.ctx(), id).map(|m| m.transform())
}

/// One pane's plot frame (its data rect), top to bottom in the order `draw` stacks them.
#[derive(Clone, Debug)]
pub struct Pane {
    pub name: String,
    pub frame: Rect,
}

/// One painted tick label: its text, the glyphs' visual rect, and the y its tick sits at.
#[derive(Clone, Debug)]
pub struct Label {
    pub text: String,
    pub rect: Rect,
    pub center_y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipKind {
    /// The coloured chip at the last close (`paint_last_price_chip`).
    LastPrice,
    /// The crosshair's price tag (`paint_price_tag`).
    CrosshairPrice,
    /// A sub-pane's current-value tag (`paint_value_tag`).
    Value,
    /// The crosshair's time tag under the bottom pane (`paint_time_tag`).
    Time,
}

#[derive(Clone, Debug)]
pub struct Chip {
    pub kind: ChipKind,
    pub rect: Rect,
    pub text: String,
}

/// Everything the matrix judges, read from one frame.
#[derive(Clone, Debug)]
pub struct Geometry {
    pub screen: Rect,
    pub panes: Vec<Pane>,
    /// Y tick labels, one list per pane, parallel to `panes`.
    pub y_labels: Vec<Vec<Label>>,
    /// The time labels under the bottom pane.
    pub x_labels: Vec<Label>,
    pub chips: Vec<Chip>,
}

/// Flatten `shapes` (groups recurse) into its rectangles and texts, each in paint order.
fn leaves(out: &egui::FullOutput) -> (Vec<RectShape>, Vec<TextShape>) {
    fn walk(shape: &Shape, rects: &mut Vec<RectShape>, texts: &mut Vec<TextShape>) {
        match shape {
            Shape::Rect(r) => rects.push(r.clone()),
            Shape::Text(t) => texts.push(t.clone()),
            Shape::Vec(v) => v.iter().for_each(|s| walk(s, rects, texts)),
            _ => {}
        }
    }
    let (mut rects, mut texts) = (Vec::new(), Vec::new());
    for cs in &out.shapes {
        walk(&cs.shape, &mut rects, &mut texts);
    }
    (rects, texts)
}

/// How far `y` is from the band `[top, bottom]` (0 inside it).
fn band_distance(y: f32, top: f32, bottom: f32) -> f32 {
    (top - y).max(y - bottom).max(0.0)
}

/// Read one frame. `panes` is every pane's frame top to bottom (price first); `tag_bg` is the fill of
/// the crosshair tags (`ChartColors::tag_bg`); `last_chip` says the frame paints a last-price chip
/// (it has bars and `show_last_price`), which is how the first plain gutter chip is told from a value
/// tag.
pub fn read_geometry(
    out: &egui::FullOutput,
    screen: Rect,
    panes: Vec<Pane>,
    tag_bg: Color32,
    last_chip: bool,
) -> Geometry {
    let (rects, texts) = leaves(out);
    let radius = CornerRadius::same(chart::AXIS_TAG_RADIUS.round() as u8);
    let bottom = panes.last().map_or(0.0, |p| p.frame.bottom());
    let right_min = panes.iter().map(|p| p.frame.right()).fold(f32::INFINITY, f32::min);
    let pad = Vec2::new(space::MD, space::XS);

    let mut chips = Vec::new();
    let mut chip_text = vec![false; texts.len()];
    let mut seen_last = false;
    for r in rects.iter().filter(|r| r.corner_radius == radius) {
        let in_gutter =
            panes.iter().any(|p| (r.rect.min.x - (p.frame.right() + space::HAIR)).abs() < 0.01);
        let kind = if (r.rect.min.y - (bottom + space::HAIR)).abs() < 0.01 {
            ChipKind::Time
        } else if !in_gutter {
            continue;
        } else if r.fill == tag_bg {
            ChipKind::CrosshairPrice
        } else if last_chip && !seen_last {
            seen_last = true;
            ChipKind::LastPrice
        } else {
            ChipKind::Value
        };
        let want = r.rect.min + pad;
        let text = texts.iter().position(|t| (t.pos - want).abs().max_elem() < 0.01).map(|i| {
            chip_text[i] = true;
            texts[i].galley.job.text.clone()
        });
        chips.push(Chip { kind, rect: r.rect, text: text.unwrap_or_default() });
    }

    let mut y_labels = vec![Vec::new(); panes.len()];
    let mut x_labels = Vec::new();
    for (t, _) in texts.iter().zip(&chip_text).filter(|(_, is_chip)| !**is_chip) {
        if t.angle != 0.0 {
            continue; // the rotated axis TITLE, never a tick label
        }
        let rect = t.visual_bounding_rect();
        let label = Label {
            text: t.galley.job.text.clone(),
            rect,
            center_y: t.pos.y + t.galley.size().y / 2.0,
        };
        // A time label is anything under the bottom pane — also one painted past the window's
        // side edge (the shared x spacer hands egui_plot EVERY hour mark, in view or not); a y
        // label sits beside a pane, at or right of its right edge.
        if rect.min.y >= bottom - TOL {
            x_labels.push(label);
        } else if rect.min.x >= right_min - TOL {
            let pane = panes
                .iter()
                .enumerate()
                .min_by(|a, b| {
                    let da = band_distance(label.center_y, a.1.frame.top(), a.1.frame.bottom());
                    let db = band_distance(label.center_y, b.1.frame.top(), b.1.frame.bottom());
                    da.total_cmp(&db)
                })
                .map(|(i, _)| i);
            if let Some(i) = pane {
                y_labels[i].push(label);
            }
        }
    }
    Geometry { screen, panes, y_labels, x_labels, chips }
}

// ============================ the pure checkers ============================

/// The overlap of two rects as `(dx, dy)`, `None` when they are disjoint or touch within [`TOL`].
pub fn overlap(a: Rect, b: Rect) -> Option<Vec2> {
    let dx = a.max.x.min(b.max.x) - a.min.x.max(b.min.x);
    let dy = a.max.y.min(b.max.y) - a.min.y.max(b.min.y);
    (dx > TOL && dy > TOL).then_some(Vec2::new(dx, dy))
}

/// Every pair `(i, j, overlap)` of `rects` that overlaps.
pub fn overlapping_pairs(rects: &[Rect]) -> Vec<(usize, usize, Vec2)> {
    let mut out = Vec::new();
    for (i, a) in rects.iter().enumerate() {
        for (j, b) in rects.iter().enumerate().skip(i + 1) {
            if let Some(o) = overlap(*a, *b) {
                out.push((i, j, o));
            }
        }
    }
    out
}

/// Every pair `(i in a, j in b, overlap)` where one rect of `a` lies over one of `b`.
pub fn cross_overlaps(a: &[Rect], b: &[Rect]) -> Vec<(usize, usize, Vec2)> {
    let mut out = Vec::new();
    for (i, ra) in a.iter().enumerate() {
        for (j, rb) in b.iter().enumerate() {
            if let Some(o) = overlap(*ra, *rb) {
                out.push((i, j, o));
            }
        }
    }
    out
}

/// The strings that occur more than once, each reported once.
pub fn duplicate_strings<'a>(texts: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut all: Vec<&str> = texts.into_iter().collect();
    all.sort_unstable();
    let mut dups: Vec<String> =
        all.windows(2).filter(|w| w[0] == w[1]).map(|w| w[0].to_owned()).collect();
    dups.dedup();
    dups
}

/// Rects that stick out of the horizontal band `[top, bottom]` by more than [`TOL`]:
/// `(index, protrusion)`.
pub fn outside_y_band(rects: &[Rect], top: f32, bottom: f32) -> Vec<(usize, f32)> {
    rects
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let p = (top - r.min.y).max(r.max.y - bottom);
            (p > TOL).then_some((i, p))
        })
        .collect()
}

/// Rects that stick out of `outer` on any side by more than [`TOL`]: `(index, protrusion)`.
pub fn outside_rect(rects: &[Rect], outer: Rect) -> Vec<(usize, f32)> {
    rects
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let p = (outer.min.x - r.min.x)
                .max(r.max.x - outer.max.x)
                .max(outer.min.y - r.min.y)
                .max(r.max.y - outer.max.y);
            (p > TOL).then_some((i, p))
        })
        .collect()
}

/// `max - min` of `values` (0 for fewer than two).
pub fn spread(values: &[f32]) -> f32 {
    let (lo, hi) = values
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    if values.len() < 2 { 0.0 } else { hi - lo }
}

/// Between each pair of consecutive `frames`, how far the gap is from `sep`: `(upper index, error)`
/// for every gap off by more than [`TOL`].
pub fn gap_errors(frames: &[Rect], sep: f32) -> Vec<(usize, f32)> {
    frames
        .windows(2)
        .enumerate()
        .filter_map(|(i, w)| {
            let err = (w[1].top() - w[0].bottom()) - sep;
            (err.abs() > TOL).then_some((i, err))
        })
        .collect()
}

/// Samples `(pane height, label count)` where a TALLER pane shows FEWER labels than a shorter one:
/// `(short height, short count, tall height, tall count)`. Heights within 1pt are the same pane.
pub fn tick_count_inversions(samples: &[(f32, usize)]) -> Vec<(f32, usize, f32, usize)> {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    sorted
        .windows(2)
        .filter(|w| w[1].0 - w[0].0 > 1.0 && w[1].1 < w[0].1)
        .map(|w| (w[0].0, w[0].1, w[1].0, w[1].1))
        .collect()
}

// ============================ the assertions over one frame ============================

/// Which assertion a [`Violation`] breaks (the numbering is `DESIGN-chart-matrix.md`'s).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// 1. Two tick labels of one axis overlap.
    LabelOverlap,
    /// 2. One axis prints the same tick text twice.
    DuplicateLabel,
    /// 3. An axis tag chip is painted over a tick label.
    ChipOverLabel,
    /// 4. A y tick label is not inside its own pane's y-range.
    LabelOutsidePane,
    /// 5. The crosshair price tag is outside the price frame or the window.
    CrosshairTagMisplaced,
    /// 6. The crosshair time tag is outside the window.
    TimeTagOutsideWindow,
    /// 7. The last-price chip is outside the price frame, or off its last close.
    LastChipOutsidePane,
    /// 8. The panes' right edges do not line up.
    PaneRightEdges,
    /// 9. The pane stack does not fit the window: a torn separator, an overflow, a time label below it.
    PaneStack,
    /// 10. A taller pane shows fewer ticks than a shorter one.
    TickCountInversion,
    /// 11. A price-side chip is cut by the window's side edge (chip text wider than the gutter).
    ChipOutsideWindow,
    /// 12. A time label is cut by the window's side edge (one wholly past it is merely clipped).
    TimeLabelCut,
}

#[derive(Clone, Debug)]
pub struct Violation {
    pub kind: Kind,
    /// What is wrong, with the numbers, in points.
    pub detail: String,
    /// The headline number (overlap or protrusion, in points) the worst-case summary sorts by.
    pub magnitude: f32,
}

impl Violation {
    fn new(kind: Kind, magnitude: f32, detail: String) -> Self {
        Violation { kind, detail, magnitude }
    }
}

/// What a cell knows about its frame that the shapes cannot say.
#[derive(Clone, Copy, Debug)]
pub struct Expect {
    /// Where the last close sits on screen, when the frame paints a last-price chip.
    pub last_close_y: Option<f32>,
}

fn rects_of(labels: &[Label]) -> Vec<Rect> {
    labels.iter().map(|l| l.rect).collect()
}

fn pt(r: Rect) -> String {
    format!("[{:.1},{:.1}]-[{:.1},{:.1}]", r.min.x, r.min.y, r.max.x, r.max.y)
}

/// Every assertion this module knows over one frame, in the design's order.
pub fn violations(g: &Geometry, ex: &Expect) -> Vec<Violation> {
    let mut v = Vec::new();
    let Some(price) = g.panes.first() else {
        return v;
    };

    // 1 + 2: same-axis tick labels never overlap, never repeat a text.
    let mut axes: Vec<(String, &[Label])> = g
        .panes
        .iter()
        .zip(&g.y_labels)
        .map(|(p, l)| (format!("{} y", p.name), l.as_slice()))
        .collect();
    axes.push(("time x".to_owned(), g.x_labels.as_slice()));
    for (name, labels) in &axes {
        for (i, j, o) in overlapping_pairs(&rects_of(labels)) {
            v.push(Violation::new(
                Kind::LabelOverlap,
                o.x.max(o.y),
                format!(
                    "{name} labels '{}' and '{}' overlap {:.1}x{:.1}pt",
                    labels[i].text, labels[j].text, o.x, o.y
                ),
            ));
        }
        for d in duplicate_strings(labels.iter().map(|l| l.text.as_str())) {
            v.push(Violation::new(
                Kind::DuplicateLabel,
                1.0,
                format!("{name} axis prints '{d}' more than once"),
            ));
        }
    }

    // 3: a chip never sits on a tick label (any pane's, or the time axis').
    let all_labels: Vec<(&str, &Label)> = axes
        .iter()
        .flat_map(|(name, labels)| labels.iter().map(move |l| (name.as_str(), l)))
        .collect();
    let label_rects: Vec<Rect> = all_labels.iter().map(|(_, l)| l.rect).collect();
    let chip_rects: Vec<Rect> = g.chips.iter().map(|c| c.rect).collect();
    for (ci, li, o) in cross_overlaps(&chip_rects, &label_rects) {
        let (chip, (axis, label)) = (&g.chips[ci], all_labels[li]);
        v.push(Violation::new(
            Kind::ChipOverLabel,
            o.x.max(o.y),
            format!(
                "{:?} chip '{}' {} covers {axis} label '{}' {} by {:.1}x{:.1}pt",
                chip.kind,
                chip.text,
                pt(chip.rect),
                label.text,
                pt(label.rect),
                o.x,
                o.y
            ),
        ));
    }

    // 4: a y tick label stays inside its own pane's y-range.
    for (p, labels) in g.panes.iter().zip(&g.y_labels) {
        for (i, protrusion) in outside_y_band(&rects_of(labels), p.frame.top(), p.frame.bottom()) {
            v.push(Violation::new(
                Kind::LabelOutsidePane,
                protrusion,
                format!(
                    "{} y label '{}' {} sticks {:.1}pt out of its pane [{:.1}, {:.1}]",
                    p.name,
                    labels[i].text,
                    pt(labels[i].rect),
                    protrusion,
                    p.frame.top(),
                    p.frame.bottom()
                ),
            ));
        }
    }

    // 5: the crosshair price tag is inside the price frame and the window.
    for c in g.chips.iter().filter(|c| c.kind == ChipKind::CrosshairPrice) {
        let cy = c.rect.center().y;
        let off = band_distance(cy, price.frame.top(), price.frame.bottom());
        if off > TOL {
            v.push(Violation::new(
                Kind::CrosshairTagMisplaced,
                off,
                format!(
                    "crosshair price tag '{}' {} is {:.1}pt outside the price frame [{:.1}, {:.1}]",
                    c.text,
                    pt(c.rect),
                    off,
                    price.frame.top(),
                    price.frame.bottom()
                ),
            ));
        }
        for (_, p) in outside_rect(&[c.rect], g.screen) {
            v.push(Violation::new(
                Kind::CrosshairTagMisplaced,
                p,
                format!(
                    "crosshair price tag '{}' {} is {:.1}pt outside the window {}",
                    c.text,
                    pt(c.rect),
                    p,
                    pt(g.screen)
                ),
            ));
        }
    }

    // 6: the time tag is inside the window.
    for c in g.chips.iter().filter(|c| c.kind == ChipKind::Time) {
        for (_, p) in outside_rect(&[c.rect], g.screen) {
            v.push(Violation::new(
                Kind::TimeTagOutsideWindow,
                p,
                format!(
                    "time tag '{}' {} is {:.1}pt outside the window {}",
                    c.text,
                    pt(c.rect),
                    p,
                    pt(g.screen)
                ),
            ));
        }
    }

    // 7: the last-price chip is inside the price frame, and on its last close when that is.
    if let Some(expected) = ex.last_close_y {
        match g.chips.iter().find(|c| c.kind == ChipKind::LastPrice) {
            None => v.push(Violation::new(
                Kind::LastChipOutsidePane,
                1.0,
                "the frame has bars and show_last_price but painted no last-price chip".to_owned(),
            )),
            Some(c) => {
                let cy = c.rect.center().y;
                let off = band_distance(cy, price.frame.top(), price.frame.bottom());
                if off > TOL {
                    v.push(Violation::new(
                        Kind::LastChipOutsidePane,
                        off,
                        format!(
                            "last-price chip '{}' {} (last close at y={:.1}) is {:.1}pt outside \
                             the price frame [{:.1}, {:.1}]",
                            c.text,
                            pt(c.rect),
                            expected,
                            off,
                            price.frame.top(),
                            price.frame.bottom()
                        ),
                    ));
                } else if band_distance(expected, price.frame.top(), price.frame.bottom()) <= TOL
                    && (cy - expected).abs() > 1.0
                {
                    v.push(Violation::new(
                        Kind::LastChipOutsidePane,
                        (cy - expected).abs(),
                        format!(
                            "last-price chip '{}' centre y={:.1} is {:.1}pt off the last close y={:.1}",
                            c.text,
                            cy,
                            (cy - expected).abs(),
                            expected
                        ),
                    ));
                }
            }
        }
    }

    // 11: a price-side chip is not cut by the window's side edge (its y is rule 7's business).
    for c in g.chips.iter().filter(|c| matches!(c.kind, ChipKind::LastPrice | ChipKind::Value)) {
        let p = side_cut(c.rect, g.screen);
        if p > TOL {
            v.push(Violation::new(
                Kind::ChipOutsideWindow,
                p,
                format!(
                    "{:?} chip '{}' {} is cut {:.1}pt by the window's side edge {}",
                    c.kind,
                    c.text,
                    pt(c.rect),
                    p,
                    pt(g.screen)
                ),
            ));
        }
    }

    // 12: a time label that is partly in the window is not cut by its side edge.
    for l in &g.x_labels {
        let p = side_cut(l.rect, g.screen);
        let visible = l.rect.max.x > g.screen.min.x && l.rect.min.x < g.screen.max.x;
        if visible && p > TOL {
            v.push(Violation::new(
                Kind::TimeLabelCut,
                p,
                format!(
                    "time label '{}' {} is cut {:.1}pt by the window's side edge {}",
                    l.text,
                    pt(l.rect),
                    p,
                    pt(g.screen)
                ),
            ));
        }
    }

    // 8: every pane's data rect ends at the same x.
    let rights: Vec<f32> = g.panes.iter().map(|p| p.frame.right()).collect();
    if spread(&rights) > TOL {
        v.push(Violation::new(
            Kind::PaneRightEdges,
            spread(&rights),
            format!("pane right edges differ by {:.1}pt: {rights:?}", spread(&rights)),
        ));
    }

    // 9: the stack fits the window: a separator between panes, the time axis under the last pane,
    // nothing below the window. It need not FILL the window: `draw` keeps the 18pt reserve
    // (`chart::AXIS_LABEL_H`) clear below the time labels on purpose, which MEASURED leaves ~14pt of
    // slack under the last label at every size and ppp — so only an OVERFLOW is a defect here.
    let frames: Vec<Rect> = g.panes.iter().map(|p| p.frame).collect();
    for (i, err) in gap_errors(&frames, chart::PANE_SEP_H) {
        v.push(Violation::new(
            Kind::PaneStack,
            err.abs(),
            format!(
                "gap between {} and {} is {:.1}pt off the {}pt separator",
                g.panes[i].name,
                g.panes[i + 1].name,
                err,
                chart::PANE_SEP_H
            ),
        ));
    }
    if let Some(last) = g.panes.last() {
        let stack_bottom = last.frame.bottom() + chart::AXIS_LABEL_H;
        let overflow = stack_bottom - g.screen.bottom();
        if overflow > 1.0 {
            v.push(Violation::new(
                Kind::PaneStack,
                overflow,
                format!(
                    "the stack (bottom pane ends {:.1} + {}pt time axis = {:.1}) overflows the \
                     window bottom {:.1} by {:.1}pt",
                    last.frame.bottom(),
                    chart::AXIS_LABEL_H,
                    stack_bottom,
                    g.screen.bottom(),
                    overflow
                ),
            ));
        }
    }
    for l in &g.x_labels {
        let p = (l.rect.max.y - g.screen.bottom()).max(g.screen.top() - l.rect.min.y);
        if p > TOL {
            v.push(Violation::new(
                Kind::PaneStack,
                p,
                format!(
                    "time label '{}' {} is {:.1}pt below/above the window {}",
                    l.text,
                    pt(l.rect),
                    p,
                    pt(g.screen)
                ),
            ));
        }
    }
    v
}

/// How far `r` sticks out of `window` sideways (0 when it does not).
fn side_cut(r: Rect, window: Rect) -> f32 {
    (r.max.x - window.max.x).max(window.min.x - r.min.x).max(0.0)
}
