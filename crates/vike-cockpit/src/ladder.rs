//! The probability DOM ladder — a 0–1 (¢) click-trade ladder for a Polymarket up/down market.
//! RUST-NATIVE (no Python twin — the oracle app has no cockpit).
//!
//! The sibling of `vike_panels::dom` adapted to a prediction market: instead of an unbounded price
//! grid, the rungs are the integer-cent probabilities 1..=99¢ (0 and 100 are resolved states,
//! never a live rung). Each rung carries its resting YES-token depth — a bid column (buying YES =
//! Up) on the left, an ask column (selling YES = buying NO = Down) on the right — Up and Down in
//! the market colours the Appearance setting picks. Each side paints a horizontal DEPTH BAR behind
//! its column (width ∝ resting size) whose ALPHA rises with size — a liquidity HEATMAP — so a wall
//! of resting depth is the widest, brightest rung on the ladder. Both the width and the alpha read
//! from ONE fraction normalised against the peak single-side size over the visible rungs
//! (`depth_bar_frac`); an empty/zero book paints no bars, byte-identically to a depth-less ladder.
//! Click a rung's column to rest a limit there; click the inline cancel icon on your own
//! resting-order marker to cancel it.
//!
//! Every translucent fill — the heatmap, the inside-market tint, the spread band, the hover wash
//! and the stale dim — is a design-system TOKEN painted through a faded painter (`at_alpha`), so
//! the ladder constructs and derives no colour of its own.
//!
//! Same seam as the DOM and the chain rail: a stateless-render [`draw`] over caller-owned
//! [`ProbLadderState`] + borrowed [`ProbLadderInputs`], emitting neutral [`ProbLadderAction`]s the
//! app maps to `vike_exec::Command`s (id-minting stays in the runtime). The rung↔price mapping,
//! the visible-window math and the click→rung hit-test are a pure, unit-tested core below the
//! fold — no egui, so the ladder law is verifiable without a frame.

use egui::{
    Align2, CornerRadius, Direction, FontId, Layout, Rect, Sense, Stroke, StrokeKind, UiBuilder,
    Vec2,
};
use vike_ui_theme::components::chip;
use vike_ui_theme::components::{ON_FILL, Status, Tokens};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::RADIUS;
use vike_ui_theme::type_scale::TextRole;

/// The inside-market rung's tint: the market colour at this alpha (it was `dim(UP | DOWN, 30)`).
const INSIDE_ALPHA: u8 = 30;
/// The spread band: the analysis line at this alpha, about a fifth.
const SPREAD_ALPHA: u8 = 51;
/// The hover wash: the text colour at this alpha (it was white at 8).
const HOVER_ALPHA: u8 = 8;
/// The stale dim over the whole ladder: the background at this alpha (it was a near-black at 150).
const STALE_ALPHA: u8 = 150;
/// The selected-row marker's width (spec §2: the accent's selected-row marker; the kit's table and
/// rail draw the same 2 px edge).
const SELECTED_EDGE_W: f32 = 2.0;
/// A resting-order marker's width, its inset from the column edge, and its inset from the rung's
/// top and bottom.
const MARKER_W: f32 = 14.0;
const MARKER_INSET: f32 = 2.0;
const MARKER_TOP: f32 = 3.0;

/// A painter that paints everything at `alpha` (0–255). It is how this crate paints a TOKEN
/// translucently with no colour of its own (the migration plan's decision 6). egui scales every
/// colour of the shape by the painter's opacity, so an opaque token comes out as that token at
/// `alpha`, to one step of rounding: the colour the deleted `dim` helper built by hand.
fn at_alpha(painter: &egui::Painter, alpha: u8) -> egui::Painter {
    let mut p = painter.clone();
    p.multiply_opacity(f32::from(alpha) / 255.0);
    p
}

// ---------------------------------------------------------------------------
// Widget types
// ---------------------------------------------------------------------------

/// One rung's resting depth. `price_cents` is the integer cent (1..=99); the two sizes are the
/// resting YES-token depth in shares on each side. Holds f64s, so deliberately NOT `PartialEq`/`Eq`.
#[derive(Clone, Copy, Debug)]
pub struct ProbLevel {
    /// rung, integer cents 1..=99
    pub price_cents: i64,
    /// resting bid depth (buy YES / Up) at this cent, in shares
    pub bid_size: f64,
    /// resting ask depth (sell YES / Down) at this cent, in shares
    pub ask_size: f64,
}

/// A marker the app draws on the ladder — a plain working limit, or a bracket leg.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProbMarker {
    /// a working limit order (its marker is the inline ✕ cancel affordance)
    Resting,
    /// take-profit bracket leg
    TakeProfit,
    /// stop-loss bracket leg
    StopLoss,
}

/// A resting order (or bracket marker) the widget draws on a rung. Built by the app from its
/// `OrderView` (filtered to this market) — vike-cockpit can't see `vike_core` types, so this is the
/// lightweight local mirror.
#[derive(Clone, Debug)]
pub struct ProbOrder {
    pub client_order_id: String,
    /// +1 Up/YES (bid column) / −1 Down/NO (ask column)
    pub side: i32,
    /// rung the order rests on, integer cents 1..=99
    pub price_cents: i64,
    pub qty: f64,
    pub marker: ProbMarker,
}

/// Everything the ladder needs to render one frame. Borrowed — the app owns the storage.
pub struct ProbLadderInputs<'a> {
    /// the market label shown in the header (e.g. `BTC ▲/▼ 5m`)
    pub asset: &'a str,
    /// per-cent resting depth (any subset of 1..=99; missing rungs read empty)
    pub levels: &'a [ProbLevel],
    /// working orders + bracket markers for THIS market only
    pub orders: &'a [ProbOrder],
    /// best bid cent (highest cent buying YES), if known — the inside market
    pub inside_bid: Option<i64>,
    /// best ask cent (lowest cent selling YES), if known — the inside market
    pub inside_ask: Option<i64>,
    /// the displayed book is stale (no live update within the freshness window, or none yet) —
    /// the widget dims the ladder and shows a badge so a frozen book never reads as live
    pub stale: bool,
}

/// Cross-frame view state for one ladder (owned by the app). Persisted so hover/selection stick.
#[derive(Clone, Debug, Default)]
pub struct ProbLadderState {
    /// cent of the rung under the pointer this frame; `None` ⇒ not hovering a live rung
    pub hover: Option<i64>,
    /// cent of the rung the trader last clicked; `None` ⇒ none pinned yet
    pub selected: Option<i64>,
}

/// A trader intent leaving the ladder. The app mints a client-order-id and maps this to a
/// `vike_exec::Command`. `side` is +1 Up/YES / −1 Down/NO; `price` is a probability in 0..1.
#[derive(Clone, PartialEq, Debug)]
pub enum ProbLadderAction {
    /// Rest a limit on `side` at `price` (the clicked rung's cent, as a 0..1 probability).
    PlaceLimit { side: i32, price: f64 },
    /// Cancel one resting order by id (the inline ✕ on its marker).
    CancelOrder(String),
}

// ---------------------------------------------------------------------------
// Pure logic (unit-tested) — no egui, so the ladder law is verifiable.
// ---------------------------------------------------------------------------

/// Clamp a cent into the valid live-probability range 1..=99 (0 and 100 are resolved states).
fn clamp_cents(c: i64) -> i64 {
    c.clamp(1, 99)
}

/// A cent price as a 0..1 probability — the price an order carries on the wire.
fn cents_to_prob(c: i64) -> f64 {
    c as f64 / 100.0
}

/// The cent price of ladder row `i` (0 = top), counting DOWN from `top_cents`.
fn rung_at_row(top_cents: i64, i: usize) -> i64 {
    top_cents - i as i64
}

/// The top cent to show so `center` sits ~halfway down the visible window, clamped so the top
/// never exceeds 99¢. (Rungs that fall below 1¢ near the bottom are skipped at paint time.)
fn top_cents_for(center: i64, n_rows: usize) -> i64 {
    let half = (n_rows / 2) as i64;
    (clamp_cents(center) + half).min(99)
}

/// The window's centre cent from the inside market: the mid of bid/ask when both are known, else
/// whichever side is known, else 50¢ (max uncertainty).
fn inside_center(bid: Option<i64>, ask: Option<i64>) -> i64 {
    match (bid, ask) {
        (Some(b), Some(a)) => clamp_cents((b + a) / 2),
        (Some(b), None) => clamp_cents(b),
        (None, Some(a)) => clamp_cents(a),
        (None, None) => 50,
    }
}

/// Is `cents` strictly inside the inside-market spread (between best bid and best ask)?
fn in_spread(cents: i64, bid: Option<i64>, ask: Option<i64>) -> bool {
    match (bid, ask) {
        (Some(b), Some(a)) => cents > b.min(a) && cents < b.max(a),
        _ => false,
    }
}

/// Which ladder column a pointer x falls in, given the ladder rect's left edge and width.
/// Left third = Up/bid, middle = price, right third = Down/ask.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Col {
    Up,
    Price,
    Down,
}

fn col_at(x: f32, left: f32, width: f32) -> Col {
    let rel = ((x - left) / width).clamp(0.0, 0.999);
    if rel < 0.34 {
        Col::Up
    } else if rel < 0.66 {
        Col::Price
    } else {
        Col::Down
    }
}

/// Row index under pointer `y`, clamped into `0..n_rows`.
fn row_at_y(y: f32, top_y: f32, row_h: f32, n_rows: usize) -> usize {
    let idx = ((y - top_y) / row_h).floor().max(0.0) as usize;
    idx.min(n_rows.saturating_sub(1))
}

// ---- depth bars + liquidity heatmap (pure, unit-tested) ----

/// Heatmap alpha floor: even a sliver of resting size still paints a faintly-visible bar.
const DEPTH_ALPHA_FLOOR: f32 = 30.0;
/// Heatmap alpha added on top of the floor at the visible peak size. Floor + span = 150 stays well
/// under opaque, so the size label painted ON TOP of the bar never washes out — the DOM's subtle
/// depth tint, now graded by size.
const DEPTH_ALPHA_SPAN: f32 = 120.0;

/// The width / intensity fraction (`0.0..=1.0`) of a rung's depth bar: this side's resting `size`
/// normalised against `max_size`, the peak single-side size over the VISIBLE rungs. ONE fraction
/// drives BOTH the bar width (∝ size) and its heatmap alpha (via [`depth_bar_alpha`]), so the
/// deepest rung is at once the widest and the brightest — a liquidity wall read at a glance.
///
/// Guards every degenerate book a feed can hand us, so a bar is only ever painted for real depth:
/// a non-positive `max_size` (an empty / all-zero book), and a non-finite or non-positive `size`,
/// all collapse to `0.0` (no bar) — which is what keeps an empty ladder byte-identical to the
/// pre-heatmap render. A `size` above the peak is clamped to `1.0`, so a bar never overflows its
/// column.
fn depth_bar_frac(size: f64, max_size: f64) -> f32 {
    if !size.is_finite() || !max_size.is_finite() || size <= 0.0 || max_size <= 0.0 {
        return 0.0;
    }
    (size / max_size).min(1.0) as f32
}

/// Heatmap alpha (`0..=255`) for a depth bar of normalised intensity `frac` (from
/// [`depth_bar_frac`]): a faint floor so any resting size is visible, ramping to a bright — but
/// deliberately text-safe — peak so a wall of liquidity reads instantly. `frac` is clamped into
/// `0.0..=1.0`, so an out-of-range value can never wrap the `u8`.
fn depth_bar_alpha(frac: f32) -> u8 {
    (DEPTH_ALPHA_FLOOR + frac.clamp(0.0, 1.0) * DEPTH_ALPHA_SPAN) as u8
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The rect of ladder row `i` within the ladder region, `row_h` tall — paint and hit-test share one
/// layout.
fn row_rect(ladder: Rect, i: usize, row_h: f32) -> Rect {
    Rect::from_min_size(
        egui::pos2(ladder.min.x, ladder.min.y + i as f32 * row_h),
        Vec2::new(ladder.width(), row_h),
    )
}

/// The small inline-marker rect (the ✕ cancel affordance) at the inner edge of a rung's column,
/// inset from the rung's top and bottom so it follows the density's row height. `side` picks the
/// Up (left) or Down (right) third; paint and hit-test call the same fn.
fn marker_rect(row: Rect, side: i32) -> Rect {
    let third = row.width() / 3.0;
    let size = Vec2::new(MARKER_W, row.height() - 2.0 * MARKER_TOP);
    let x = if side > 0 {
        row.min.x + third - MARKER_W - MARKER_INSET
    } else {
        row.min.x + third * 2.0 + MARKER_INSET
    };
    Rect::from_min_size(egui::pos2(x, row.min.y + MARKER_TOP), size)
}

fn opt_cents(c: Option<i64>) -> String {
    match c {
        Some(v) => format!("{v}¢"),
        None => "—".to_string(),
    }
}

/// Share-size label. KEPT local, not `vike_ui_theme::fmt::fmt_compact`, which diverges: uppercase
/// `K`, trailing-zero trim, and M/B tiers ("2.3M" vs our "2300.0k") — pinned in the tests.
fn fmt_size(q: f64) -> String {
    if q.abs() >= 1000.0 { format!("{:.1}k", q / 1000.0) } else { format!("{q:.0}") }
}

/// Draw one frame of the probability ladder and return the actions produced this frame.
///
/// `state` carries hover/selection across frames; `inputs` is the borrowed per-cent depth, the
/// resting orders, and the inside market. A click on a rung's Up/Down column rests a limit there
/// ([`ProbLadderAction::PlaceLimit`]); a click on the inline cancel icon of an own resting-order
/// marker cancels it ([`ProbLadderAction::CancelOrder`]).
pub fn draw(
    ui: &mut egui::Ui,
    state: &mut ProbLadderState,
    inputs: &ProbLadderInputs,
) -> Vec<ProbLadderAction> {
    let t = Tokens::of(ui.ctx());
    // the density's ladder row and header strip (spec §3.4)
    let (row_h, header_h) = (t.metrics.row_h, t.metrics.header_h);
    let mut actions: Vec<ProbLadderAction> = Vec::new();

    let full = ui.available_rect_before_wrap();
    let width = full.width();

    // --- header: market label + inside market (+ STALE badge) ---
    ladder_header(ui, inputs, &t);

    // --- ladder region ---
    let region_h = (full.height() - header_h).max(row_h * 3.0);
    let (region, _) = ui.allocate_exact_size(Vec2::new(width, region_h), Sense::hover());
    ui.painter().rect_filled(region, 0.0, t.theme.bg);
    let ladder_rect = region;

    let n_rows = (region.height() / row_h).floor().max(1.0) as usize;
    let center = inside_center(inputs.inside_bid, inputs.inside_ask);
    let top_cents = top_cents_for(center, n_rows);

    // per-cent depth lookup for O(1) row reads
    let mut lv: std::collections::HashMap<i64, (f64, f64)> = std::collections::HashMap::new();
    for l in inputs.levels {
        lv.insert(l.price_cents, (l.bid_size, l.ask_size));
    }

    // depth-bar / heatmap scale: peak single-side resting size over the VISIBLE rungs, read from
    // `inputs.levels` via the `lv` lookup (nothing threaded in from outside). Left RAW — a peak of
    // 0 (an empty / all-zero book) is handled by `depth_bar_frac`'s guard, which paints no bar, so
    // an empty ladder renders byte-identically to before.
    let mut max_size = 0.0_f64;
    for i in 0..n_rows {
        let cents = rung_at_row(top_cents, i);
        if let Some(&(b, a)) = lv.get(&cents) {
            max_size = max_size.max(b).max(a);
        }
    }

    let painter = ui.painter().clone();
    let third = ladder_rect.width() / 3.0;
    for i in 0..n_rows {
        let cents = rung_at_row(top_cents, i);
        let row = row_rect(ladder_rect, i, row_h);

        // row background: inside-market tint / spread band / zebra
        if Some(cents) == inputs.inside_bid {
            at_alpha(&painter, INSIDE_ALPHA).rect_filled(row, 0.0, t.market.up);
        } else if Some(cents) == inputs.inside_ask {
            at_alpha(&painter, INSIDE_ALPHA).rect_filled(row, 0.0, t.market.down);
        } else if in_spread(cents, inputs.inside_bid, inputs.inside_ask) {
            // the boundary between plus and minus is the neutral analysis line (spec §3.2)
            at_alpha(&painter, SPREAD_ALPHA).rect_filled(row, 0.0, t.theme.analysis_line);
        } else if i % 2 == 1 {
            painter.rect_filled(row, 0.0, t.theme.surface);
        }

        // rungs outside the live range carry no depth / price / markers
        if !(1..=99).contains(&cents) {
            continue;
        }

        // hover wash (the picked rung's selection edge is painted LAST in the rung, below)
        if state.selected != Some(cents) && state.hover == Some(cents) {
            at_alpha(&painter, HOVER_ALPHA).rect_filled(row, 0.0, t.theme.text);
        }

        let (bidq, askq) = lv.get(&cents).copied().unwrap_or((0.0, 0.0));
        let bidcol = Rect::from_min_size(row.min, Vec2::new(third, row_h));
        let askcol = Rect::from_min_size(
            egui::pos2(row.min.x + third * 2.0, row.min.y),
            Vec2::new(third, row_h),
        );

        // Depth bars: the bid bar grows leftward from the price column, the ask bar rightward (the
        // DOM's direction), each WIDTH ∝ its side's resting size and ALPHA ∝ the same normalised
        // fraction (the liquidity heatmap) — so the deepest rung is both the widest and the
        // brightest. Width is unchanged from the plain depth bar; only the alpha now tracks size.
        // A size-0 side takes neither branch, so it paints no bar and no size text (an empty book
        // is byte-identical to before).
        if bidq > 0.0 {
            let frac = depth_bar_frac(bidq, max_size);
            let w = frac * bidcol.width();
            let bar = Rect::from_min_max(
                egui::pos2(bidcol.max.x - w, row.min.y + 1.0),
                egui::pos2(bidcol.max.x, row.max.y - 1.0),
            );
            at_alpha(&painter, depth_bar_alpha(frac)).rect_filled(bar, 0.0, t.market.up);
            painter.text(
                egui::pos2(bidcol.max.x - t.metrics.pad, row.center().y),
                Align2::RIGHT_CENTER,
                fmt_size(bidq),
                t.mono(TextRole::Body),
                t.market.up_text,
            );
        }
        if askq > 0.0 {
            let frac = depth_bar_frac(askq, max_size);
            let w = frac * askcol.width();
            let bar = Rect::from_min_max(
                egui::pos2(askcol.min.x, row.min.y + 1.0),
                egui::pos2(askcol.min.x + w, row.max.y - 1.0),
            );
            at_alpha(&painter, depth_bar_alpha(frac)).rect_filled(bar, 0.0, t.market.down);
            painter.text(
                egui::pos2(askcol.min.x + t.metrics.pad, row.center().y),
                Align2::LEFT_CENTER,
                fmt_size(askq),
                t.mono(TextRole::Body),
                t.market.down_text,
            );
        }

        // centre: the rung's cent price
        painter.text(
            row.center(),
            Align2::CENTER_CENTER,
            format!("{cents}¢"),
            t.mono(TextRole::Body),
            if state.selected == Some(cents) { t.theme.text } else { t.theme.text2 },
        );

        // resting-order / bracket markers on this rung
        for o in inputs.orders {
            if o.price_cents != cents {
                continue;
            }
            let mr = marker_rect(row, o.side);
            let mcol = match o.marker {
                ProbMarker::Resting => {
                    if o.side > 0 {
                        t.market.up
                    } else {
                        t.market.down
                    }
                }
                ProbMarker::TakeProfit => t.market.up,
                ProbMarker::StopLoss => t.market.down,
            };
            // A resting order's marker is the cancel icon (a click on it cancels the order); a
            // bracket leg's is its letter, as text. Both at the caption size, in the kit's
            // on-fill black: the marker is a FILLED control in the market colour.
            let letter = match o.marker {
                ProbMarker::Resting => None,
                ProbMarker::TakeProfit => Some("T"),
                ProbMarker::StopLoss => Some("S"),
            };
            let round = CornerRadius::same(RADIUS);
            painter.rect_filled(mr, round, mcol);
            painter.rect_stroke(mr, round, Stroke::new(1.0, t.theme.text), StrokeKind::Middle);
            let at = (mr.center(), Align2::CENTER_CENTER);
            match letter {
                None => {
                    let font = FontId::new(t.text.px(TextRole::Caption), icons::family());
                    icons::CANCEL.paint(&painter, at.0, at.1, font, ON_FILL);
                }
                Some(l) => {
                    painter.text(at.0, at.1, l, t.mono(TextRole::Caption), ON_FILL);
                }
            }
        }

        // The picked rung carries the accent's selected-row marker, a 2 px edge at its left —
        // painted LAST in the rung, so a full-width bid bar cannot cover it.
        if state.selected == Some(cents) {
            let edge = Rect::from_min_size(row.min, Vec2::new(SELECTED_EDGE_W, row.height()));
            painter.rect_filled(edge, 0.0, t.theme.accent);
        }
    }

    // stale overlay: dim the whole ladder so a frozen/unarrived book never reads as live
    if inputs.stale {
        at_alpha(&painter, STALE_ALPHA).rect_filled(region, 0.0, t.theme.bg);
    }

    // --- interaction ---
    let resp = ui.interact(ladder_rect, ui.id().with("prob_ladder"), Sense::click());
    state.hover = None;
    if let Some(pos) = resp.hover_pos() {
        let idx = row_at_y(pos.y, ladder_rect.min.y, row_h, n_rows);
        let cents = rung_at_row(top_cents, idx);
        if (1..=99).contains(&cents) {
            state.hover = Some(cents);
        }
        if resp.clicked() && (1..=99).contains(&cents) {
            let side = match col_at(pos.x, ladder_rect.min.x, ladder_rect.width()) {
                Col::Up => 1,
                Col::Down => -1,
                Col::Price => 0,
            };
            if side != 0 {
                state.selected = Some(cents);
                let row = row_rect(ladder_rect, idx, row_h);
                // inline ✕ on an own order cancels; anywhere else on the rung rests a limit
                let on_marker = inputs
                    .orders
                    .iter()
                    .find(|o| o.price_cents == cents && o.side == side)
                    .filter(|o| marker_rect(row, o.side).contains(pos));
                if let Some(o) = on_marker {
                    actions.push(ProbLadderAction::CancelOrder(o.client_order_id.clone()));
                } else {
                    let price = cents_to_prob(cents);
                    actions.push(ProbLadderAction::PlaceLimit { side, price });
                }
            }
        }
    }

    actions
}

fn ladder_header(ui: &mut egui::Ui, inputs: &ProbLadderInputs, t: &Tokens) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), t.metrics.header_h), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, t.theme.bg);
    ui.painter()
        .line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, t.theme.border));
    ui.painter().text(
        egui::pos2(rect.min.x + t.metrics.pad, rect.center().y),
        Align2::LEFT_CENTER,
        inputs.asset,
        t.mono(TextRole::Strong),
        t.theme.text,
    );
    let inside =
        format!("bid {} / ask {}", opt_cents(inputs.inside_bid), opt_cents(inputs.inside_ask));
    ui.painter().text(
        egui::pos2(rect.max.x - t.metrics.pad, rect.center().y),
        Align2::RIGHT_CENTER,
        inside,
        t.mono(TextRole::Body),
        t.theme.text3,
    );
    // A frozen or unarrived book says so in the WARNING status (the migration plan's decision 5),
    // centred on the strip: the kit's badge, in a child `Ui` over the header, so the header's one
    // allocation stays one.
    if inputs.stale {
        let mut badge = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::centered_and_justified(Direction::LeftToRight)),
        );
        chip::badge(&mut badge, "STALE", Status::Warning);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_cents_bounds_to_live_range() {
        assert_eq!(clamp_cents(0), 1);
        assert_eq!(clamp_cents(1), 1);
        assert_eq!(clamp_cents(50), 50);
        assert_eq!(clamp_cents(99), 99);
        assert_eq!(clamp_cents(100), 99);
        assert_eq!(clamp_cents(1000), 99);
        assert_eq!(clamp_cents(-5), 1);
    }

    #[test]
    fn cents_to_prob_maps_to_unit_interval() {
        assert!((cents_to_prob(62) - 0.62).abs() < 1e-12);
        assert!((cents_to_prob(1) - 0.01).abs() < 1e-12);
        assert!((cents_to_prob(99) - 0.99).abs() < 1e-12);
    }

    #[test]
    fn rung_at_row_counts_down_from_top() {
        assert_eq!(rung_at_row(60, 0), 60);
        assert_eq!(rung_at_row(60, 1), 59);
        assert_eq!(rung_at_row(60, 10), 50);
    }

    #[test]
    fn top_cents_centres_and_clamps_at_99() {
        // 20 rows, centre 60 → half = 10 → top 70
        assert_eq!(top_cents_for(60, 20), 70);
        // a high centre clamps the top at 99 rather than showing a 0/100 rung
        assert_eq!(top_cents_for(95, 20), 99);
        // centre gets clamped into range first
        assert_eq!(top_cents_for(200, 10), 99);
    }

    #[test]
    fn inside_center_mid_and_fallbacks() {
        assert_eq!(inside_center(Some(40), Some(60)), 50);
        assert_eq!(inside_center(Some(40), None), 40);
        assert_eq!(inside_center(None, Some(60)), 60);
        assert_eq!(inside_center(None, None), 50);
        // out-of-range inputs are clamped
        assert_eq!(inside_center(Some(0), Some(0)), 1);
    }

    #[test]
    fn in_spread_strictly_between_bid_ask() {
        assert!(in_spread(50, Some(48), Some(52)));
        assert!(!in_spread(48, Some(48), Some(52))); // bid edge is not "inside"
        assert!(!in_spread(52, Some(48), Some(52))); // ask edge is not "inside"
        assert!(!in_spread(50, None, Some(52)));
        assert!(!in_spread(50, Some(48), None));
    }

    #[test]
    fn col_at_thirds() {
        // left=0, width=300 → [0,102)=Up, [102,198)=Price, [198,300)=Down
        assert_eq!(col_at(10.0, 0.0, 300.0), Col::Up);
        assert_eq!(col_at(150.0, 0.0, 300.0), Col::Price);
        assert_eq!(col_at(280.0, 0.0, 300.0), Col::Down);
        // clamped at the edges
        assert_eq!(col_at(-50.0, 0.0, 300.0), Col::Up);
        assert_eq!(col_at(9999.0, 0.0, 300.0), Col::Down);
    }

    #[test]
    fn row_at_y_maps_and_clamps() {
        // top_y=100, row_h=20, 10 rows
        assert_eq!(row_at_y(100.0, 100.0, 20.0, 10), 0);
        assert_eq!(row_at_y(115.0, 100.0, 20.0, 10), 0);
        assert_eq!(row_at_y(125.0, 100.0, 20.0, 10), 1);
        // above the top clamps to row 0, far below clamps to the last row
        assert_eq!(row_at_y(50.0, 100.0, 20.0, 10), 0);
        assert_eq!(row_at_y(9999.0, 100.0, 20.0, 10), 9);
    }

    /// Pins the rendered strings, INCLUDING the divergences that keep this formatter local
    /// instead of swapping to `vike_ui_theme::fmt::fmt_compact` (GUI audit F7 — a swap would
    /// change rendered text).
    #[test]
    fn fmt_size_scales_thousands() {
        assert_eq!(fmt_size(950.0), "950");
        assert_eq!(fmt_size(1500.0), "1.5k"); // lowercase k — fmt_compact prints "1.5K"
        assert_eq!(fmt_size(0.0), "0");
        assert_eq!(fmt_size(2_300_000.0), "2300.0k"); // no M tier — fmt_compact prints "2.3M"
    }

    #[test]
    fn state_defaults_to_no_hover_or_selection() {
        let s = ProbLadderState::default();
        assert_eq!(s.hover, None);
        assert_eq!(s.selected, None);
    }

    #[test]
    fn depth_bar_frac_normalises_and_guards() {
        // a non-positive peak (empty / all-zero book) ⇒ no bar — the guard that keeps an empty
        // ladder byte-identical to the pre-heatmap render
        assert_eq!(depth_bar_frac(100.0, 0.0), 0.0);
        assert_eq!(depth_bar_frac(100.0, -5.0), 0.0);
        // size AT the visible peak fills the column (exactly 1.0)
        assert_eq!(depth_bar_frac(50.0, 50.0), 1.0);
        // size ABOVE the peak clamps to 1.0 (a bar never overflows its third)
        assert_eq!(depth_bar_frac(250.0, 100.0), 1.0);
        // an exactly-representable mid fraction is exact
        assert_eq!(depth_bar_frac(25.0, 100.0), 0.25);
        // an inexact mid fraction (1/3 is not representable in f32) ⇒ epsilon, never `==`
        let third = depth_bar_frac(30.0, 90.0);
        assert!((third - 1.0 / 3.0).abs() < 1e-6, "expected ~1/3 of the peak, got {third}");
        // a zero / negative / non-finite size ⇒ no bar
        assert_eq!(depth_bar_frac(0.0, 100.0), 0.0);
        assert_eq!(depth_bar_frac(-10.0, 100.0), 0.0);
        assert_eq!(depth_bar_frac(f64::NAN, 100.0), 0.0);
        assert_eq!(depth_bar_frac(f64::INFINITY, 100.0), 0.0);
        // a non-finite peak is guarded too
        assert_eq!(depth_bar_frac(50.0, f64::NAN), 0.0);
    }

    #[test]
    fn depth_bar_alpha_ramps_floor_to_peak() {
        // zero intensity ⇒ the faint floor; peak intensity ⇒ the bright (text-safe) cap
        assert_eq!(depth_bar_alpha(0.0), 30);
        assert_eq!(depth_bar_alpha(1.0), 150);
        // brighter with more size, and always a valid non-washout alpha
        assert!(depth_bar_alpha(0.25) < depth_bar_alpha(0.75));
        let mid = depth_bar_alpha(0.5);
        assert!((30..=150).contains(&mid), "mid alpha {mid} stays within the band");
        // an out-of-range fraction is clamped, never wrapping the u8
        assert_eq!(depth_bar_alpha(-1.0), 30);
        assert_eq!(depth_bar_alpha(2.0), 150);
    }
}
