//! The tick chart pane (the owner's v3 design, spec §3.2 and §3.5): a pane to the LEFT of the ladder
//! that shares the ladder's price rows, so a price is one height in both. A toolbar `Ticks` carries
//! three independent layers over a plot whose time runs from two minutes ago to now, newest at the
//! edge beside the ladder:
//!
//! - **Bid / ask** — the best bid and the best ask as two step lines, green and red;
//! - **Trades** — each print a bubble sized by its size, filled green for a buy and red for a sell,
//!   translucent so overlaps read;
//! - **Heatmap** — the resting size at each row, in the theme's cool-to-warm ramp
//!   ([`vike_ui_theme::heat::ramp`]).
//!
//! # Where the history comes from
//!
//! The widget keeps the best bid and ask and the book's depth itself, sampled from the book it is
//! given every frame ([`TradeWindowTickChartHistory::observe`]) into two bounded rings, and it forgets them when the
//! window moves to another instrument ([`TradeWindowState::sync`]). The prints are the app's: the tape it
//! keeps arrives through [`TradeWindowInputs::tape`]. So a window that was just opened starts with an empty
//! chart that fills as it runs, and a venue that serves no trade stream shows the lines and the
//! heatmap and an empty Trades layer — never a frozen or made-up one.
//!
//! # ⚠ The rows are the ladder's
//!
//! `TradeWindowLadderRowFrame` is what the ladder says about where its rows are (`TradeWindowLadderRowFrame::of`, over `ladder::rows_region`); the chart
//! paints on it and nowhere else, so a price is one height in both panes. A ladder with no book has
//! no frame, and then the chart has no rows to paint on and says so.

use std::collections::VecDeque;

use egui::{Align2, Pos2, Rect, RichText, Sense, Shape, Stroke, pos2, vec2};
use vike_marketdata::{L2Book, L2BookLevel};
use vike_ui_theme::color::faded;
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::heat;
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::side::ThemeSidePair;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::trade;

use super::ladder::rows::TradeWindowLadderRowFrame;
use super::{TradeWindowInputs, TradeWindowState, keyed, ladder};

/// What the pane's toolbar says before its three layers.
pub(crate) const TOOLBAR_LABEL: &str = "Ticks";

/// How far back the plot reaches: the design's `−2:00`.
pub const WINDOW_MS: i64 = 120_000;

/// The least time between two samples of the best bid and ask.
const QUOTE_EVERY_MS: i64 = 250;
/// The least time between two columns of the heatmap (a column is about this wide on the design's
/// 300 pt pane).
const HEAT_EVERY_MS: i64 = 4_000;
/// How many levels each side of the book a heatmap column keeps: the rows a ladder shows are a few
/// dozen, and a column is replaced whole.
const HEAT_LEVELS: usize = 60;
/// The time axis's gridlines: every 30 s, as the design's.
const GRID_EVERY_MS: i64 = 30_000;
// The bubbles' radius range, in multiples of the row height, a print of the window's smallest size to
// its largest (`trade::BUBBLE_MIN_ROWS`, `trade::BUBBLE_MAX_ROWS`), and how translucent a bubble is, so
// two prints on one row both read (`trade::BUBBLE_ALPHA`).
/// The share of the held cells the heatmap's scale sits above: a cell at this rank is the hottest the
/// ramp draws, and anything above it is the same.
const HEAT_SCALE_AT: f64 = 0.95;
/// How much of a new column's own scale moves the running one: slow, so neighbouring columns agree.
const SCALE_FOLD: f64 = 0.1;

/// The pane's three independent layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TradeWindowTickChartLayers {
    /// The best bid and the best ask over time, two step lines.
    pub bid_ask: bool,
    /// Trades as bubbles sized by size, filled by the aggressor.
    pub trades: bool,
    /// Resting size per row over time, a cool-to-warm ramp.
    pub heatmap: bool,
}

impl Default for TradeWindowTickChartLayers {
    /// The design's: the lines and the trades on, the heatmap off.
    fn default() -> Self {
        TradeWindowTickChartLayers { bid_ask: true, trades: true, heatmap: false }
    }
}

/// One print on the tape, as the chart needs it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TradeWindowTickChartPrint {
    /// When the app received it, epoch milliseconds (the local clock the samples are stamped with).
    pub ms: i64,
    pub price: f64,
    pub size: f64,
    /// Whether the aggressor bought.
    pub buy: bool,
}

/// One sample of the top of the book.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TradeWindowTickChartQuote {
    ms: i64,
    bid: f64,
    ask: f64,
}

/// One column of the heatmap: the resting size at the levels the book held then.
#[derive(Clone, Debug, PartialEq)]
struct TradeWindowTickChartColumn {
    ms: i64,
    levels: Vec<L2BookLevel>,
    /// The size one grouped row of this column's levels is drawn hottest at: fixed when the column is
    /// sampled ([`TradeWindowTickChartHistory::scale`]), so a cell keeps its colour for as long as it is on the chart.
    scale: f64,
}

/// What one window has seen of its book: the quotes and the depth of the last [`WINDOW_MS`].
#[derive(Clone, Debug, Default)]
pub struct TradeWindowTickChartHistory {
    quotes: VecDeque<TradeWindowTickChartQuote>,
    columns: VecDeque<TradeWindowTickChartColumn>,
    /// The slowly moving size a column is scaled to: each new column's own high percentile folded in
    /// at [`SCALE_FOLD`]. Recomputed from what is on screen every frame, the scale shifted with every
    /// column that came or went, and every cell changed colour with it.
    scale: f64,
}

impl TradeWindowTickChartHistory {
    /// Take one look at `book` at `now_ms`. A book with one side or none is no sample: a line needs
    /// both prices, and a heatmap column of one side would read as a hole in the market.
    pub fn observe(&mut self, now_ms: i64, book: &L2Book) {
        let cutoff = now_ms - WINDOW_MS;
        while self.quotes.front().is_some_and(|q| q.ms < cutoff) {
            self.quotes.pop_front();
        }
        while self.columns.front().is_some_and(|c| c.ms < cutoff) {
            self.columns.pop_front();
        }
        let (Some(bid), Some(ask)) = (book.best_bid(), book.best_ask()) else { return };
        if self.quotes.back().is_none_or(|q| now_ms - q.ms >= QUOTE_EVERY_MS) {
            self.quotes.push_back(TradeWindowTickChartQuote {
                ms: now_ms,
                bid: bid.price,
                ask: ask.price,
            });
        }
        if self.columns.back().is_none_or(|c| now_ms - c.ms >= HEAT_EVERY_MS) {
            let (bids, asks) = (book.top_n(HEAT_LEVELS).0, book.top_n(HEAT_LEVELS).1);
            let levels: Vec<L2BookLevel> = bids.into_iter().chain(asks).collect();
            let own = high_percentile(levels.iter().map(|l| l.qty));
            if own > 0.0 {
                self.scale = if self.scale > 0.0 {
                    self.scale * (1.0 - SCALE_FOLD) + own * SCALE_FOLD
                } else {
                    own
                };
            }
            self.columns.push_back(TradeWindowTickChartColumn {
                ms: now_ms,
                levels,
                scale: self.scale,
            });
        }
    }

    /// Forget everything: a window on another instrument starts a new chart.
    pub fn clear(&mut self) {
        self.quotes.clear();
        self.columns.clear();
    }

    /// How many quote samples are held.
    #[must_use]
    pub fn quotes(&self) -> usize {
        self.quotes.len()
    }

    /// How many heatmap columns are held.
    #[must_use]
    pub fn columns(&self) -> usize {
        self.columns.len()
    }
}

/// The [`HEAT_SCALE_AT`] percentile of the positive values, or `0` where there are none.
fn high_percentile(values: impl Iterator<Item = f64>) -> f64 {
    let mut v: Vec<f64> = values.filter(|q| *q > 0.0 && q.is_finite()).collect();
    v.sort_by(f64::total_cmp);
    vike_math::statistics::nearest_rank_of_sorted(&v, HEAT_SCALE_AT).unwrap_or(0.0)
}

/// The rect a chart pane with no ladder beside it lays its rows in: the pane under its toolbar and
/// above the time axis it draws for itself.
#[must_use]
pub(crate) fn own_rows(pane: Rect, t: &ThemeTokens) -> Rect {
    let top = pane.min.y + t.metrics.control_height + 2.0 * space::HAIR;
    Rect::from_min_max(
        pos2(pane.min.x, top),
        pos2(pane.max.x, (pane.max.y - t.metrics.row_height).max(top)),
    )
}

/// The plot's x for a time, newest at the right: `now_ms` is the right edge, `now_ms − WINDOW_MS`
/// the left.
fn x_of(plot: Rect, now_ms: i64, ms: i64) -> f32 {
    let age = (now_ms - ms) as f32 / WINDOW_MS as f32;
    plot.right() - age.clamp(0.0, 1.0) * plot.width()
}

/// Draw the pane into `ui`, which is clipped to the pane's rect. `rows` is where the ladder's rows
/// are; `None` where the ladder has none to give (no book, or no ladder drawn).
pub(crate) fn draw(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    rows: Option<TradeWindowLadderRowFrame>,
) {
    let pane = ui.max_rect();
    toolbar(ui, t, state, pane);
    // The plot shares the rows' vertical extent; with no rows it is what is left under the toolbar.
    let plot = match rows {
        Some(r) => {
            Rect::from_min_max(pos2(pane.min.x, r.region.min.y), pos2(pane.max.x, r.region.max.y))
        }
        None => Rect::from_min_max(pos2(pane.min.x, ui.cursor().min.y), pane.max),
    };
    ui.allocate_rect(plot, Sense::hover());
    let p = ui.painter().with_clip_rect(plot);
    p.rect_filled(plot, 0.0, t.theme.background);
    let now_ms = vike_model::now_ms();
    // The window's own clock keeps the plot sliding while no book update comes.
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(QUOTE_EVERY_MS as u64));
    let Some(frame) = rows else {
        say(ui, t, plot, "No order book, so no rows to chart on.");
        return;
    };
    zebra_and_grid(&p, t, plot, &frame, now_ms);
    let layers = state.layers;
    if layers.heatmap {
        heatmap(&p, plot, &frame, &state.history, now_ms, ui.ctx().pixels_per_point());
    }
    if layers.bid_ask {
        step_lines(&p, t, plot, &frame, &state.history, now_ms);
    }
    if layers.trades {
        bubbles(&p, t, plot, &frame, inputs.tape, now_ms);
    }
    if !state.view.ladder {
        axes(&p, t, plot, &frame, now_ms);
    }
    let empty = state.history.quotes() == 0 && inputs.tape.is_empty();
    if empty {
        say(ui, t, plot, "Waiting for the first quotes.");
    }
}

/// The toolbar: `Ticks` and the three layer toggles, in the band above the plot.
fn toolbar(ui: &mut egui::Ui, t: &ThemeTokens, state: &mut TradeWindowState, pane: Rect) {
    let key = ui.scope_id();
    let band = Rect::from_min_size(
        pane.min,
        vec2(pane.width(), t.metrics.control_height + 2.0 * space::HAIR),
    );
    let mut bar = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(band.shrink2(vec2(t.metrics.padding, space::HAIR)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    bar.shrink_clip_rect(band);
    bar.spacing_mut().item_spacing.x = t.metrics.gap;
    bar.label(
        RichText::new(TOOLBAR_LABEL)
            .font(t.font(ThemeTextRole::Caption))
            .color(t.theme.text_tertiary),
    );
    let layers = &mut state.layers;
    for (name, on) in [
        ("Bid / ask", &mut layers.bid_ask),
        ("Trades", &mut layers.trades),
        ("Heatmap", &mut layers.heatmap),
    ] {
        let button = ThemeButtonAction::secondary(name).selected(*on);
        if keyed(&mut bar, key.with(name), button).clicked() {
            *on = !*on;
        }
    }
}

/// A line of words centred in the plot, for the states that have nothing to draw.
fn say(ui: &egui::Ui, t: &ThemeTokens, plot: Rect, words: &str) {
    ui.painter().text(
        plot.center(),
        Align2::CENTER_CENTER,
        words,
        t.font(ThemeTextRole::Caption),
        t.theme.text_tertiary,
    );
}

/// The ladder's zebra, row for row, and the time gridlines over it.
fn zebra_and_grid(
    p: &egui::Painter,
    t: &ThemeTokens,
    plot: Rect,
    f: &TradeWindowLadderRowFrame,
    now_ms: i64,
) {
    for i in 0..f.n_rows {
        if i % 2 == 1 {
            let top = plot.min.y + i as f32 * f.row_h;
            p.rect_filled(
                Rect::from_min_size(pos2(plot.min.x, top), vec2(plot.width(), f.row_h)),
                0.0,
                t.theme.surface,
            );
        }
    }
    let first = (now_ms - WINDOW_MS).div_euclid(GRID_EVERY_MS) * GRID_EVERY_MS + GRID_EVERY_MS;
    let mut ms = first;
    while ms < now_ms {
        let x = x_of(plot, now_ms, ms);
        p.vline(x, plot.y_range(), Stroke::new(stroke::HAIRLINE, t.theme.hover));
        ms += GRID_EVERY_MS;
    }
}

/// The best bid and ask as step lines, `stroke::LINE` wide: level until the next sample, then a
/// rise or a fall.
fn step_lines(
    p: &egui::Painter,
    t: &ThemeTokens,
    plot: Rect,
    f: &TradeWindowLadderRowFrame,
    h: &TradeWindowTickChartHistory,
    now_ms: i64,
) {
    let (bid, ask) = ThemeSidePair::BidAsk.colours(t);
    for (pick, colour) in [
        (TradeWindowTickChartQuote::bid as fn(&TradeWindowTickChartQuote) -> f64, bid),
        (TradeWindowTickChartQuote::ask as fn(&TradeWindowTickChartQuote) -> f64, ask),
    ] {
        let line = Stroke::new(stroke::LINE, colour);
        let mut points: Vec<Pos2> = Vec::new();
        for q in &h.quotes {
            let Some(y) = f.y_of(pick(q)) else {
                // Off the rows: the line breaks rather than clamping to an edge it never touched.
                if points.len() > 1 {
                    p.add(Shape::line(std::mem::take(&mut points), line));
                }
                points.clear();
                continue;
            };
            let x = x_of(plot, now_ms, q.ms);
            if let Some(last) = points.last().copied() {
                points.push(pos2(x, last.y));
            }
            points.push(pos2(x, y));
        }
        if let Some(last) = points.last().copied() {
            points.push(pos2(plot.right(), last.y));
        }
        if points.len() > 1 {
            p.add(Shape::line(points, line));
        }
    }
}

impl TradeWindowTickChartQuote {
    fn bid(&self) -> f64 {
        self.bid
    }

    fn ask(&self) -> f64 {
        self.ask
    }
}

/// The prints as bubbles on their rows: sized by size, filled by the aggressor, translucent so two
/// prints on one row both read.
fn bubbles(
    p: &egui::Painter,
    t: &ThemeTokens,
    plot: Rect,
    f: &TradeWindowLadderRowFrame,
    tape: &[TradeWindowTickChartPrint],
    now_ms: i64,
) {
    let visible: Vec<&TradeWindowTickChartPrint> =
        tape.iter().filter(|x| now_ms - x.ms <= WINDOW_MS).collect();
    let biggest = visible.iter().map(|x| x.size).fold(0.0_f64, f64::max);
    if biggest <= 0.0 {
        return;
    }
    for x in visible {
        let Some(y) = f.y_of(x.price) else { continue };
        let share = (x.size / biggest).sqrt() as f32;
        let r = f.row_h
            * (trade::BUBBLE_MIN_ROWS + share * (trade::BUBBLE_MAX_ROWS - trade::BUBBLE_MIN_ROWS));
        let base = ThemeSidePair::BuySell.colour(x.buy, t);
        p.circle_filled(pos2(x_of(plot, now_ms, x.ms), y), r, faded(base, trade::BUBBLE_ALPHA));
    }
}

/// The heatmap: for each column and each row, the resting size at that row, in the ramp, against the
/// largest the visible rows hold.
fn heatmap(
    p: &egui::Painter,
    plot: Rect,
    f: &TradeWindowLadderRowFrame,
    h: &TradeWindowTickChartHistory,
    now_ms: i64,
    ppp: f32,
) {
    // A cell lands on whole pixels, so the plot sliding left under it never leaves a half-covered
    // edge to shimmer.
    let snap = |v: f32| (v * ppp).round() / ppp;
    for (k, c) in h.columns.iter().enumerate() {
        let x0 = snap(x_of(plot, now_ms, c.ms));
        // Each column runs from its own sample to the next one's (the newest to now), so they tile:
        // no sliver between two samples and no gap at the edge beside the ladder.
        let x1 = snap(h.columns.get(k + 1).map_or(plot.right(), |n| x_of(plot, now_ms, n.ms)));
        if x1 <= x0 || c.scale <= 0.0 {
            continue;
        }
        let mut per_row = vec![0.0_f64; f.n_rows];
        for l in &c.levels {
            if let Some(i) = f.row_of(l.price) {
                per_row[i] += l.qty;
            }
        }
        // Against the column's own scale, grouped rows holding `group` levels' worth: square-rooted, so
        // the middle of a heavy-tailed book is seen, not only its walls.
        let full = c.scale * f.group.max(1) as f64;
        for (i, q) in per_row.iter().enumerate() {
            if *q <= 0.0 {
                continue;
            }
            let top = snap(plot.min.y + i as f32 * f.row_h);
            let bottom = snap(plot.min.y + (i + 1) as f32 * f.row_h);
            let cell = Rect::from_min_max(pos2(x0, top), pos2(x1, bottom));
            p.rect_filled(cell, 0.0, heat::ramp((*q / full).sqrt().min(1.0) as f32));
        }
    }
}

/// The time axis along the plot's bottom and a price every third row down its right edge: the
/// chart's own axes, drawn where the ladder is not there to read them off.
fn axes(
    p: &egui::Painter,
    t: &ThemeTokens,
    plot: Rect,
    f: &TradeWindowLadderRowFrame,
    now_ms: i64,
) {
    let font = t.mono(ThemeTextRole::Caption);
    for k in 0..=4_i64 {
        let ms = now_ms - WINDOW_MS + k * GRID_EVERY_MS;
        let words = if k == 4 {
            "now".to_string()
        } else {
            let left = (4 - k) * 30;
            format!("−{}:{:02}", left / 60, left % 60)
        };
        let (anchor, x) = match k {
            0 => (Align2::LEFT_BOTTOM, plot.left() + t.metrics.gap),
            4 => (Align2::RIGHT_BOTTOM, plot.right() - t.metrics.gap),
            _ => (Align2::CENTER_BOTTOM, x_of(plot, now_ms, ms)),
        };
        p.text(
            pos2(x, plot.bottom() - t.metrics.gap),
            anchor,
            words,
            font.clone(),
            t.theme.text_tertiary,
        );
    }
    for i in (0..f.n_rows).step_by(3) {
        let price = ladder::fmt_price(f.price_of(i), Some(f.tick));
        let y = plot.min.y + (i as f32 + 0.5) * f.row_h;
        p.text(
            pos2(plot.right() - t.metrics.gap, y),
            Align2::RIGHT_CENTER,
            price,
            font.clone(),
            faded(t.theme.text_tertiary, trade::PRICE_LABEL_ALPHA),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(bid: f64, ask: f64) -> L2Book {
        let mut b = L2Book::new(0.1);
        b.apply_snapshot(1, &[L2BookLevel::new(bid, 2.0)], &[L2BookLevel::new(ask, 3.0)]);
        b
    }

    /// The history is bounded in time: what is older than the window is dropped as new looks arrive.
    #[test]
    fn the_history_holds_two_minutes_and_no_more() {
        let mut h = TradeWindowTickChartHistory::default();
        let b = book(100.0, 100.1);
        for k in 0..2_000 {
            h.observe(k * 250, &b);
        }
        let newest = 1_999 * 250;
        assert!(h.quotes.front().is_some_and(|q| q.ms >= newest - WINDOW_MS));
        assert!(h.quotes() <= (WINDOW_MS / QUOTE_EVERY_MS) as usize + 1, "{}", h.quotes());
        assert!(h.columns() <= (WINDOW_MS / HEAT_EVERY_MS) as usize + 1, "{}", h.columns());
    }

    /// Looks closer together than the sampling interval add nothing, so a fast book does not
    /// outgrow the ring.
    #[test]
    fn looks_inside_the_interval_are_not_samples() {
        let mut h = TradeWindowTickChartHistory::default();
        let b = book(100.0, 100.1);
        for ms in [0, 10, 100, 249] {
            h.observe(ms, &b);
        }
        assert_eq!(h.quotes(), 1);
        h.observe(250, &b);
        assert_eq!(h.quotes(), 2);
    }

    /// One side of the book is no sample, and a cleared history is empty: a window on another
    /// instrument must not draw the last one's lines.
    #[test]
    fn a_one_sided_book_is_no_sample_and_clear_forgets() {
        let mut h = TradeWindowTickChartHistory::default();
        let mut one_sided = L2Book::new(0.1);
        one_sided.apply_snapshot(1, &[L2BookLevel::new(100.0, 1.0)], &[]);
        h.observe(0, &one_sided);
        assert_eq!((h.quotes(), h.columns()), (0, 0));
        h.observe(0, &book(100.0, 100.1));
        assert_eq!(h.quotes(), 1);
        h.clear();
        assert_eq!((h.quotes(), h.columns()), (0, 0));
    }

    /// A cell keeps its colour: a column's scale is fixed when it is sampled and the running scale
    /// moves slowly, so a later look at the book does not recolour the columns already on the chart.
    #[test]
    fn a_columns_scale_is_fixed_when_it_is_sampled() {
        let mut h = TradeWindowTickChartHistory::default();
        let walls = |quantity: f64| {
            let mut b = L2Book::new(0.1);
            b.apply_snapshot(
                1,
                &[L2BookLevel::new(100.0, quantity), L2BookLevel::new(99.9, quantity)],
                &[L2BookLevel::new(100.1, quantity)],
            );
            b
        };
        h.observe(0, &walls(2.0));
        let first = h.columns[0].scale;
        assert!(first > 0.0);
        h.observe(HEAT_EVERY_MS, &walls(40.0));
        assert_eq!(h.columns[0].scale, first, "the first column is not recoloured");
        let second = h.columns[1].scale;
        assert!(second > first && second < 40.0, "the running scale moves, but slowly: {second}");
    }

    /// What `heatmap` paints at `now_ms`: each cell's rect and fill.
    fn cells(
        h: &TradeWindowTickChartHistory,
        f: &TradeWindowLadderRowFrame,
        plot: Rect,
        now_ms: i64,
    ) -> Vec<(Rect, egui::Color32)> {
        let context = egui::Context::default();
        let out = context.run_ui(egui::RawInput::default(), |ui| {
            heatmap(&ui.painter().clone(), plot, f, h, now_ms, 1.5);
        });
        let cells = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Rect(r) => Some((r.rect, r.fill)),
                _ => None,
            })
            .collect();
        out.drop_without_applying_deltas();
        cells
    }

    /// The owner watched the heatmap "redraw all the time". Time passing must move the cells and
    /// nothing else: over a run of frames with no new sample, the same cells come out, each in the
    /// colour it had, only slid left, and two columns side by side share an edge — no sliver, no
    /// overlap — at every frame.
    #[test]
    fn time_passing_moves_the_cells_and_recolours_none() {
        let mut h = TradeWindowTickChartHistory::default();
        let walls = |quantity: f64| {
            let mut b = L2Book::new(0.1);
            b.apply_snapshot(
                1,
                &[L2BookLevel::new(109.9, quantity), L2BookLevel::new(109.8, 1.0)],
                &[L2BookLevel::new(110.0, quantity * 3.0)],
            );
            b
        };
        for (k, quantity) in [2.0, 5.0, 3.0, 9.0].into_iter().enumerate() {
            h.observe(k as i64 * HEAT_EVERY_MS, &walls(quantity));
        }
        let (f, plot) = (frame(), Rect::from_min_size(pos2(0.0, 100.0), vec2(300.0, 180.0)));
        let start = 4 * HEAT_EVERY_MS;
        let first = cells(&h, &f, plot, start);
        assert!(first.len() >= 8, "four columns of two or three rows: {}", first.len());
        for step in 1..=60 {
            let now = cells(&h, &f, plot, start + step * 16);
            assert_eq!(now.len(), first.len(), "frame {step}: the cells are the same set");
            for ((a, fa), (b, fb)) in first.iter().zip(&now) {
                assert_eq!(fa, fb, "frame {step}: a cell keeps its colour");
                assert_eq!((a.min.y, a.max.y), (b.min.y, b.max.y), "frame {step}: and its row");
                assert!(b.min.x <= a.min.x, "frame {step}: time only slides cells left");
            }
            // A column tiles into the next: a cell's right edge is some later cell's left edge, or
            // the plot's own right edge for the newest column.
            for (r, _) in &now {
                let edge = |x: f32| {
                    x == plot.right() || now.iter().any(|(o, _)| (o.min.x - x).abs() < 0.01)
                };
                assert!(edge(r.max.x), "frame {step}: {r:?} leaves a gap on its right");
            }
        }
    }

    fn frame() -> TradeWindowLadderRowFrame {
        TradeWindowLadderRowFrame {
            region: Rect::from_min_size(pos2(0.0, 100.0), vec2(300.0, 180.0)),
            top_key: ladder::rows::row_key(110.0, 0.1, 1),
            tick: 0.1,
            group: 1,
            row_h: 18.0,
            n_rows: 10,
        }
    }

    /// A price is the same height here as in the ladder: the top row is the top key's, a row is the
    /// row height, and a price off the rows has none.
    #[test]
    fn a_price_is_one_height_in_both_panes() {
        let f = frame();
        assert_eq!(f.row_of(110.0), Some(0));
        assert_eq!(f.y_of(110.0), Some(100.0 + 9.0));
        assert_eq!(f.row_of(109.9), Some(1));
        assert_eq!(f.y_of(109.1), Some(100.0 + 9.5 * 18.0));
        assert_eq!(f.row_of(109.0), None, "below the last row");
        assert_eq!(f.row_of(110.1), None, "above the first");
        assert_eq!(f.row_of(f64::NAN), None);
    }

    /// Time runs to the right: now is the right edge, the window's start the left.
    #[test]
    fn newest_is_at_the_right_edge() {
        let plot = Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 100.0));
        assert_eq!(x_of(plot, 1_000_000, 1_000_000), 300.0);
        assert_eq!(x_of(plot, 1_000_000, 1_000_000 - WINDOW_MS), 0.0);
        assert_eq!(x_of(plot, 1_000_000, 1_000_000 - WINDOW_MS / 2), 150.0);
        assert_eq!(x_of(plot, 1_000_000, 0), 0.0, "older than the window clamps to its edge");
    }
}
