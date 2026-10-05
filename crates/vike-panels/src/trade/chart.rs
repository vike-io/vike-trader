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
//! given every frame ([`History::observe`]) into two bounded rings, and it forgets them when the
//! window moves to another instrument ([`TradeState::sync`]). The prints are the app's: the tape it
//! keeps arrives through [`TradeInputs::tape`]. So a window that was just opened starts with an empty
//! chart that fills as it runs, and a venue that serves no trade stream shows the lines and the
//! heatmap and an empty Trades layer — never a frozen or made-up one.
//!
//! # ⚠ The rows are the ladder's
//!
//! [`RowFrame`] is what the ladder says about where its rows are (`RowFrame::of`, over `ladder::rows_region`); the chart
//! paints on it and nowhere else, so a price is one height in both panes. A ladder with no book has
//! no frame, and then the chart has no rows to paint on and says so.

use std::collections::VecDeque;

use egui::{Align2, Pos2, Rect, RichText, Sense, Shape, Stroke, pos2, vec2};
use vike_model::{BookLevel, L2Book};
use vike_ui_theme::color::faded;
use vike_ui_theme::components::Tokens;
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::heat;
use vike_ui_theme::type_scale::TextRole;

use super::{TradeInputs, TradeState, keyed, ladder};

/// What the pane's toolbar says before its three layers.
pub const TOOLBAR_LABEL: &str = "Ticks";

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
/// The bubbles' radius range, in multiples of the row height: a print of the window's smallest size
/// to its largest.
const BUBBLE_MIN_ROWS: f32 = 0.18;
const BUBBLE_MAX_ROWS: f32 = 0.5;
/// How translucent a bubble is, so two prints on one row both read.
const BUBBLE_ALPHA: f32 = 0.7;
/// The share of the held cells the heatmap's scale sits above: a cell at this rank is the hottest the
/// ramp draws, and anything above it is the same.
const HEAT_SCALE_AT: f64 = 0.95;
/// How much of a new column's own scale moves the running one: slow, so neighbouring columns agree.
const SCALE_FOLD: f64 = 0.1;
/// The step lines' weight.
const LINE_W: f32 = 1.5;

/// The pane's three independent layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layers {
    /// The best bid and the best ask over time, two step lines.
    pub bid_ask: bool,
    /// Trades as bubbles sized by size, filled by the aggressor.
    pub trades: bool,
    /// Resting size per row over time, a cool-to-warm ramp.
    pub heatmap: bool,
}

impl Default for Layers {
    /// The design's: the lines and the trades on, the heatmap off.
    fn default() -> Self {
        Layers { bid_ask: true, trades: true, heatmap: false }
    }
}

/// One print on the tape, as the chart needs it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Print {
    /// When the app received it, epoch milliseconds (the local clock the samples are stamped with).
    pub ms: i64,
    pub price: f64,
    pub size: f64,
    /// Whether the aggressor bought.
    pub buy: bool,
}

/// One sample of the top of the book.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Quote {
    ms: i64,
    bid: f64,
    ask: f64,
}

/// One column of the heatmap: the resting size at the levels the book held then.
#[derive(Clone, Debug, PartialEq)]
struct Column {
    ms: i64,
    levels: Vec<BookLevel>,
    /// The size one grouped row of this column's levels is drawn hottest at: fixed when the column is
    /// sampled ([`History::scale`]), so a cell keeps its colour for as long as it is on the chart.
    scale: f64,
}

/// What one window has seen of its book: the quotes and the depth of the last [`WINDOW_MS`].
#[derive(Clone, Debug, Default)]
pub struct History {
    quotes: VecDeque<Quote>,
    columns: VecDeque<Column>,
    /// The slowly moving size a column is scaled to: each new column's own high percentile folded in
    /// at [`SCALE_FOLD`]. Recomputed from what is on screen every frame, the scale shifted with every
    /// column that came or went, and every cell changed colour with it.
    scale: f64,
}

impl History {
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
            self.quotes.push_back(Quote { ms: now_ms, bid: bid.price, ask: ask.price });
        }
        if self.columns.back().is_none_or(|c| now_ms - c.ms >= HEAT_EVERY_MS) {
            let (bids, asks) = (book.top_n(HEAT_LEVELS).0, book.top_n(HEAT_LEVELS).1);
            let levels: Vec<BookLevel> = bids.into_iter().chain(asks).collect();
            let own = high_percentile(levels.iter().map(|l| l.qty));
            if own > 0.0 {
                self.scale = if self.scale > 0.0 {
                    self.scale * (1.0 - SCALE_FOLD) + own * SCALE_FOLD
                } else {
                    own
                };
            }
            self.columns.push_back(Column { ms: now_ms, levels, scale: self.scale });
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
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * HEAT_SCALE_AT).round() as usize]
}

/// Where the ladder's rows are, which is where the chart's must be ([`RowFrame::of`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowFrame {
    /// The rows' rect: its vertical extent is the chart plot's.
    pub region: Rect,
    /// The key of the top row ([`ladder::row_key`]).
    pub top_key: i64,
    pub tick: f64,
    pub group: i64,
    pub row_h: f32,
    pub n_rows: usize,
}

impl RowFrame {
    /// The frame of rows laid in `region` for this window: the same rows [`ladder`] lays there, by
    /// the same arithmetic (the row height, the tick, the grouping, and the mid or the manual centre
    /// the top row is counted from). `None` where the book has no level, as the ladder then has no
    /// rows.
    #[must_use]
    pub fn of(
        region: Rect,
        t: &Tokens,
        state: &TradeState,
        inputs: &TradeInputs<'_>,
    ) -> Option<RowFrame> {
        if inputs.book.bid_levels() + inputs.book.ask_levels() == 0 {
            return None;
        }
        let tick = ladder::row_tick(inputs.grid.tick, inputs.book.tick_size);
        let row_h = t.metrics.row_h;
        let n_rows = (region.height() / row_h).floor().max(1.0) as usize;
        let best_bid = inputs.book.best_bid().map(|l| l.price);
        let best_ask = inputs.book.best_ask().map(|l| l.price);
        let last =
            inputs.last.or_else(|| inputs.book.mid()).or(best_bid).or(best_ask).unwrap_or(0.0);
        let centre = state
            .center
            .or(state.anchor)
            .unwrap_or_else(|| inputs.book.mid().or(best_bid).or(best_ask).unwrap_or(last));
        let top_key =
            ladder::row_key(centre, tick, state.group).saturating_add((n_rows / 2) as i64);
        Some(RowFrame { region, top_key, tick, group: state.group, row_h, n_rows })
    }

    /// The row a price falls in, counted from the top, or `None` if it is above or below the rows.
    #[must_use]
    pub fn row_of(&self, price: f64) -> Option<usize> {
        if !price.is_finite() {
            return None;
        }
        let from_top = self.top_key.checked_sub(ladder::row_key(price, self.tick, self.group))?;
        usize::try_from(from_top).ok().filter(|i| *i < self.n_rows)
    }

    /// The vertical centre of the row a price falls in.
    #[must_use]
    pub fn y_of(&self, price: f64) -> Option<f32> {
        self.row_of(price).map(|i| self.region.min.y + (i as f32 + 0.5) * self.row_h)
    }

    /// The price the row `i` from the top is named by (its low edge).
    #[must_use]
    pub fn price_of(&self, i: usize) -> f64 {
        ladder::key_price(self.top_key - i as i64, self.tick, self.group)
    }
}

/// The rect a chart pane with no ladder beside it lays its rows in: the pane under its toolbar and
/// above the time axis it draws for itself.
#[must_use]
pub fn own_rows(pane: Rect, t: &Tokens) -> Rect {
    let top = pane.min.y + t.metrics.control_h + 2.0;
    Rect::from_min_max(
        pos2(pane.min.x, top),
        pos2(pane.max.x, (pane.max.y - t.metrics.row_h).max(top)),
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
pub fn draw(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    rows: Option<RowFrame>,
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
    p.rect_filled(plot, 0.0, t.theme.bg);
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
fn toolbar(ui: &mut egui::Ui, t: &Tokens, state: &mut TradeState, pane: Rect) {
    let key = ui.id();
    let band = Rect::from_min_size(pane.min, vec2(pane.width(), t.metrics.control_h + 2.0));
    let mut bar = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(band.shrink2(vec2(t.metrics.pad, 1.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    bar.shrink_clip_rect(band);
    bar.spacing_mut().item_spacing.x = t.metrics.gap;
    bar.label(RichText::new(TOOLBAR_LABEL).font(t.font(TextRole::Caption)).color(t.theme.text3));
    let layers = &mut state.layers;
    for (name, on) in [
        ("Bid / ask", &mut layers.bid_ask),
        ("Trades", &mut layers.trades),
        ("Heatmap", &mut layers.heatmap),
    ] {
        let button = ActionButton::secondary(name).selected(*on);
        if keyed(&mut bar, key.with(name), button).clicked() {
            *on = !*on;
        }
    }
}

/// A line of words centred in the plot, for the states that have nothing to draw.
fn say(ui: &egui::Ui, t: &Tokens, plot: Rect, words: &str) {
    ui.painter().text(
        plot.center(),
        Align2::CENTER_CENTER,
        words,
        t.font(TextRole::Caption),
        t.theme.text3,
    );
}

/// The ladder's zebra, row for row, and the time gridlines over it.
fn zebra_and_grid(p: &egui::Painter, t: &Tokens, plot: Rect, f: &RowFrame, now_ms: i64) {
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
        p.vline(x, plot.y_range(), Stroke::new(1.0, t.theme.hover));
        ms += GRID_EVERY_MS;
    }
}

/// The best bid and ask as step lines: level until the next sample, then a rise or a fall.
fn step_lines(p: &egui::Painter, t: &Tokens, plot: Rect, f: &RowFrame, h: &History, now_ms: i64) {
    for (pick, colour) in [
        (Quote::bid as fn(&Quote) -> f64, t.market.up),
        (Quote::ask as fn(&Quote) -> f64, t.market.down),
    ] {
        let mut points: Vec<Pos2> = Vec::new();
        for q in &h.quotes {
            let Some(y) = f.y_of(pick(q)) else {
                // Off the rows: the line breaks rather than clamping to an edge it never touched.
                if points.len() > 1 {
                    p.add(Shape::line(std::mem::take(&mut points), Stroke::new(LINE_W, colour)));
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
            p.add(Shape::line(points, Stroke::new(LINE_W, colour)));
        }
    }
}

impl Quote {
    fn bid(&self) -> f64 {
        self.bid
    }

    fn ask(&self) -> f64 {
        self.ask
    }
}

/// The prints as bubbles on their rows: sized by size, filled by the aggressor, translucent so two
/// prints on one row both read.
fn bubbles(p: &egui::Painter, t: &Tokens, plot: Rect, f: &RowFrame, tape: &[Print], now_ms: i64) {
    let visible: Vec<&Print> = tape.iter().filter(|x| now_ms - x.ms <= WINDOW_MS).collect();
    let biggest = visible.iter().map(|x| x.size).fold(0.0_f64, f64::max);
    if biggest <= 0.0 {
        return;
    }
    for x in visible {
        let Some(y) = f.y_of(x.price) else { continue };
        let share = (x.size / biggest).sqrt() as f32;
        let r = f.row_h * (BUBBLE_MIN_ROWS + share * (BUBBLE_MAX_ROWS - BUBBLE_MIN_ROWS));
        let base = if x.buy { t.market.up } else { t.market.down };
        p.circle_filled(pos2(x_of(plot, now_ms, x.ms), y), r, faded(base, BUBBLE_ALPHA));
    }
}

/// The heatmap: for each column and each row, the resting size at that row, in the ramp, against the
/// largest the visible rows hold.
fn heatmap(p: &egui::Painter, plot: Rect, f: &RowFrame, h: &History, now_ms: i64, ppp: f32) {
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
fn axes(p: &egui::Painter, t: &Tokens, plot: Rect, f: &RowFrame, now_ms: i64) {
    let font = t.mono(TextRole::Caption);
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
        p.text(pos2(x, plot.bottom() - t.metrics.gap), anchor, words, font.clone(), t.theme.text3);
    }
    for i in (0..f.n_rows).step_by(3) {
        let price = ladder::fmt_px(f.price_of(i), f.tick);
        let y = plot.min.y + (i as f32 + 0.5) * f.row_h;
        p.text(
            pos2(plot.right() - t.metrics.gap, y),
            Align2::RIGHT_CENTER,
            price,
            font.clone(),
            faded(t.theme.text3, 0.9),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(bid: f64, ask: f64) -> L2Book {
        let mut b = L2Book::new(0.1);
        b.apply_snapshot(1, &[BookLevel::new(bid, 2.0)], &[BookLevel::new(ask, 3.0)]);
        b
    }

    /// The history is bounded in time: what is older than the window is dropped as new looks arrive.
    #[test]
    fn the_history_holds_two_minutes_and_no_more() {
        let mut h = History::default();
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
        let mut h = History::default();
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
        let mut h = History::default();
        let mut one_sided = L2Book::new(0.1);
        one_sided.apply_snapshot(1, &[BookLevel::new(100.0, 1.0)], &[]);
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
        let mut h = History::default();
        let walls = |qty: f64| {
            let mut b = L2Book::new(0.1);
            b.apply_snapshot(
                1,
                &[BookLevel::new(100.0, qty), BookLevel::new(99.9, qty)],
                &[BookLevel::new(100.1, qty)],
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
    fn cells(h: &History, f: &RowFrame, plot: Rect, now_ms: i64) -> Vec<(Rect, egui::Color32)> {
        let ctx = egui::Context::default();
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
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
        let mut h = History::default();
        let walls = |qty: f64| {
            let mut b = L2Book::new(0.1);
            b.apply_snapshot(
                1,
                &[BookLevel::new(109.9, qty), BookLevel::new(109.8, 1.0)],
                &[BookLevel::new(110.0, qty * 3.0)],
            );
            b
        };
        for (k, qty) in [2.0, 5.0, 3.0, 9.0].into_iter().enumerate() {
            h.observe(k as i64 * HEAT_EVERY_MS, &walls(qty));
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

    fn frame() -> RowFrame {
        RowFrame {
            region: Rect::from_min_size(pos2(0.0, 100.0), vec2(300.0, 180.0)),
            top_key: ladder::row_key(110.0, 0.1, 1),
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
