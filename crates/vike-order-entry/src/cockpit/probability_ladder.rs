//! The probability DOM ladder — a 0–1 (¢) click-trade ladder for a Polymarket up/down market.
//! RUST-NATIVE (no Python twin — the oracle app has no cockpit).
//!
//! The sibling of the DOM ladder (`vike_panels::dom`, since replaced by the Trade window's
//! `vike_order_entry::trade_window::ladder`) adapted to a prediction market: instead of an unbounded price
//! grid, the rungs are the probabilities one market tick apart
//! ([`CockpitProbabilityLadderInputs::tick`]: 1..=99¢ at Polymarket's whole cent, 0.1..=99.9¢ on a
//! 0.001 market), 0 and 1 being resolved states, never a live rung ([`live_rungs`]). Each rung
//! carries its resting YES-token depth — a bid column (buying YES = Up) on the left, an ask column
//! (selling YES = buying NO = Down) on the right — Up and Down in the market colours the Appearance
//! setting picks. Each side paints a horizontal DEPTH BAR behind its column (width ∝ resting size)
//! whose ALPHA rises with size — a liquidity HEATMAP — so a wall of resting depth is the widest,
//! brightest rung on the ladder. Both the width and the alpha read from ONE fraction normalised
//! against the peak single-side size over the visible rungs (`depth_bar_fraction`); an empty/zero
//! book paints no bars, byte-identically to a depth-less ladder. Click a rung's column to rest a
//! limit there; click the inline cancel icon on your own resting-order marker to cancel it.
//!
//! Every translucent fill — the heatmap, the inside-market tint, the spread band, the hover wash
//! and the stale dim — is a design-system TOKEN painted through a faded painter (`at_alpha`), so
//! the ladder constructs and derives no colour of its own.
//!
//! Same seam as the DOM and the chain rail: a stateless-render [`draw_probability_ladder`] over
//! caller-owned [`CockpitProbabilityLadderState`] + borrowed [`CockpitProbabilityLadderInputs`],
//! emitting neutral [`CockpitProbabilityLadderAction`]s the app maps to `vike_execution::Command`s
//! (id-minting stays in the runtime). The rung↔price mapping, the visible-window math and the
//! click→rung hit-test are a pure, unit-tested core below the fold — no egui, so the ladder law is
//! verifiable without a frame.

use egui::{
    Align2, CornerRadius, Direction, FontId, Layout, Rect, Sense, Stroke, StrokeKind, UiBuilder,
    Vec2,
};
use vike_marketdata::L2BookLevel;
use vike_ui_theme::components::chip;
use vike_ui_theme::components::{ON_FILL, ThemeStatus, ThemeTokens};
use vike_ui_theme::icons;
use vike_ui_theme::maps;
use vike_ui_theme::metrics::{RADIUS, space, stroke};
use vike_ui_theme::side::ThemeSidePair;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::cockpit;

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

/// One rung's resting depth. `price_ticks` is the rung, the price in whole ticks of the market's
/// [`CockpitProbabilityLadderInputs::tick`] (one of [`live_rungs`]); the two sizes are the resting
/// YES-token depth in shares on each side. Holds f64s, so deliberately NOT `PartialEq`/`Eq`.
#[derive(Clone, Copy, Debug)]
pub struct CockpitProbabilityLadderLevel {
    /// the rung, in whole ticks ([`rung_of`] of its price)
    pub price_ticks: i64,
    /// resting bid depth (buy YES / Up) at this rung, in shares
    pub bid_size: f64,
    /// resting ask depth (sell YES / Down) at this rung, in shares
    pub ask_size: f64,
}

/// A marker the app draws on the ladder — a plain working limit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CockpitProbabilityLadderMarker {
    /// a working limit order (its marker is the inline ✕ cancel affordance)
    Resting,
}

/// A resting order the widget draws on a rung. Built by the app from its
/// `OrderView` (filtered to this market) — the widget names no `vike_execution` type, so this is the
/// lightweight local mirror.
#[derive(Clone, Debug)]
pub struct CockpitProbabilityLadderOrder {
    pub order_client_id: String,
    /// +1 Up/YES (bid column) / −1 Down/NO (ask column)
    pub side: i32,
    /// the rung the order rests on, in whole ticks ([`rung_of`] of its price)
    pub price_ticks: i64,
    pub quantity: f64,
    pub marker: CockpitProbabilityLadderMarker,
}

/// Everything the ladder needs to render one frame. Borrowed — the app owns the storage.
pub struct CockpitProbabilityLadderInputs<'a> {
    /// the market label shown in the header (e.g. `BTC ▲/▼ 5m`)
    pub asset: &'a str,
    /// the market's price tick, a probability step (Polymarket's whole cent 0.01, or 0.001 on a
    /// market near 0 or 1): every rung below is a whole number of it. A tick that leaves no live rung
    /// between 0 and 1 (not a positive number of at most a half) reads as the whole cent.
    pub tick: f64,
    /// per-rung resting depth (any subset of [`live_rungs`]; missing rungs read empty)
    pub levels: &'a [CockpitProbabilityLadderLevel],
    /// working orders for THIS market only
    pub orders: &'a [CockpitProbabilityLadderOrder],
    /// best bid rung (highest rung buying YES), if known — the inside market
    pub inside_bid: Option<i64>,
    /// best ask rung (lowest rung selling YES), if known — the inside market
    pub inside_ask: Option<i64>,
    /// the displayed book is stale (no live update within the freshness window, or none yet) —
    /// the widget dims the ladder and shows a badge so a frozen book never reads as live
    pub stale: bool,
}

/// Cross-frame view state for one ladder (owned by the app). Persisted so hover/selection stick.
#[derive(Clone, Debug, Default)]
pub struct CockpitProbabilityLadderState {
    /// the rung under the pointer this frame; `None` ⇒ not hovering a live rung
    pub hover: Option<i64>,
    /// the rung the trader last clicked; `None` ⇒ none pinned yet
    pub selected: Option<i64>,
}

/// A trader intent leaving the ladder. The app mints an order-client-id and maps this to a
/// `vike_execution::Command`. `side` is +1 Up/YES / −1 Down/NO; `price` is a probability in 0..1.
#[derive(Clone, PartialEq, Debug)]
pub enum CockpitProbabilityLadderAction {
    /// Rest a limit on `side` at `price` (the clicked rung, as a 0..1 probability on the tick grid).
    PlaceLimit { side: i32, price: f64 },
    /// Cancel one resting order by id (the inline ✕ on its marker).
    CancelOrder(String),
}

// ---------------------------------------------------------------------------
// Pure logic (unit-tested) — no egui, so the ladder law is verifiable.
// ---------------------------------------------------------------------------

/// The tick the rungs count: the market's, or Polymarket's whole cent (0.01) where it is not a
/// positive number of at most a half (a step that leaves no live rung between 0 and 1).
fn rung_tick(tick: f64) -> f64 {
    if tick > 0.0 && tick <= 0.5 { tick } else { 0.01 }
}

/// The rung a 0..1 probability sits on at `tick`: its tick index, as the price ladder keys a price
/// (`crate::trade_window::ladder::rows::row_key`).
pub fn rung_of(price: f64, tick: f64) -> i64 {
    crate::trade_window::ladder::rows::row_key(price, rung_tick(tick), 1)
}

/// The 0..1 probability rung `rung` names at `tick` — the price an order carries on the wire, the
/// grid's own number (`crate::trade_window::ladder::rows::key_price`: 996 ticks of 0.001 is 0.996).
fn price_of_rung(rung: i64, tick: f64) -> f64 {
    crate::trade_window::ladder::rows::key_price(rung, rung_tick(tick), 1)
}

/// The live rungs at `tick`: one tick up to one tick under 1 (0 and 1 are resolved states, never
/// a live rung) — 1..=99 at the whole cent, 1..=999 at 0.001.
pub fn live_rungs(tick: f64) -> std::ops::RangeInclusive<i64> {
    1..=((1.0 / rung_tick(tick)).round() as i64).saturating_sub(1)
}

/// Clamp a rung into the live range at `tick` ([`live_rungs`]).
fn clamp_rung(rung: i64, tick: f64) -> i64 {
    let live = live_rungs(tick);
    rung.clamp(*live.start(), *live.end())
}

/// The rung of ladder row `i` (0 = top), counting DOWN from `top_rung`.
fn rung_at_row(top_rung: i64, i: usize) -> i64 {
    top_rung - i as i64
}

/// The top rung to show so `center` sits ~halfway down the visible window, clamped so the top
/// never exceeds the last live rung. (Rungs that fall below the first near the bottom are skipped
/// at paint time.)
fn top_rung_for(center: i64, n_rows: usize, tick: f64) -> i64 {
    let half = (n_rows / 2) as i64;
    clamp_rung(center, tick).saturating_add(half).min(*live_rungs(tick).end())
}

/// The window's centre rung from the inside market: the mid of bid/ask when both are known, else
/// whichever side is known, else the rung at 0.5 (max uncertainty).
fn inside_center(bid: Option<i64>, ask: Option<i64>, tick: f64) -> i64 {
    match (bid, ask) {
        (Some(b), Some(a)) => clamp_rung((b + a) / 2, tick),
        (Some(b), None) => clamp_rung(b, tick),
        (None, Some(a)) => clamp_rung(a, tick),
        (None, None) => clamp_rung(rung_of(0.5, tick), tick),
    }
}

/// Is `rung` strictly inside the inside-market spread (between best bid and best ask)?
fn in_spread(rung: i64, bid: Option<i64>, ask: Option<i64>) -> bool {
    match (bid, ask) {
        (Some(b), Some(a)) => rung > b.min(a) && rung < b.max(a),
        _ => false,
    }
}

/// A rung's price in cents, to the tick's decimals: `62¢` at the whole cent, `99.6¢` at 0.001.
fn fmt_rung(rung: i64, tick: f64) -> String {
    let decimals = crate::trade_window::sizing::decimals_of(rung_tick(tick)).saturating_sub(2);
    format!("{:.*}¢", decimals, price_of_rung(rung, tick) * 100.0)
}

/// Which ladder column a pointer x falls in, given the ladder rect's left edge and width.
/// Left third = Up/bid, middle = price, right third = Down/ask.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CockpitProbabilityLadderColumn {
    Up,
    Price,
    Down,
}

fn col_at(x: f32, left: f32, width: f32) -> CockpitProbabilityLadderColumn {
    let rel = ((x - left) / width).clamp(0.0, 0.999);
    if rel < 0.34 {
        CockpitProbabilityLadderColumn::Up
    } else if rel < 0.66 {
        CockpitProbabilityLadderColumn::Price
    } else {
        CockpitProbabilityLadderColumn::Down
    }
}

/// Row index under pointer `y`, clamped into `0..n_rows`.
fn row_at_y(y: f32, top_y: f32, row_h: f32, n_rows: usize) -> usize {
    let index = ((y - top_y) / row_h).floor().max(0.0) as usize;
    index.min(n_rows.saturating_sub(1))
}

/// Bucket a book's `(price, qty)` levels onto the ladder's rungs at the market's `tick`
/// ([`rung_of`]), summing every level that falls on the same rung.
/// Out-of-range rungs (`0` and the rung at 1, i.e. a price on a resolved outcome) are DROPPED
/// ([`live_rungs`]) — the ladder only paints live probability rungs.
/// Ascending by rung (`BTreeMap` order), which is the order the ladder widget expects.
pub fn bucket_probability_levels(
    bids: &[L2BookLevel],
    asks: &[L2BookLevel],
    tick: f64,
) -> Vec<CockpitProbabilityLadderLevel> {
    let live = live_rungs(tick);
    let mut by_rung: std::collections::BTreeMap<i64, (f64, f64)> =
        std::collections::BTreeMap::new();
    for &L2BookLevel { price: px, qty } in bids {
        let rung = rung_of(px, tick);
        if live.contains(&rung) {
            by_rung.entry(rung).or_default().0 += qty;
        }
    }
    for &L2BookLevel { price: px, qty } in asks {
        let rung = rung_of(px, tick);
        if live.contains(&rung) {
            by_rung.entry(rung).or_default().1 += qty;
        }
    }
    by_rung
        .into_iter()
        .map(|(price_ticks, (bid_size, ask_size))| CockpitProbabilityLadderLevel {
            price_ticks,
            bid_size,
            ask_size,
        })
        .collect()
}

// ---- depth bars + liquidity heatmap (pure, unit-tested) ----

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
fn depth_bar_fraction(size: f64, max_size: f64) -> f32 {
    if !size.is_finite() || !max_size.is_finite() || size <= 0.0 || max_size <= 0.0 {
        return 0.0;
    }
    (size / max_size).min(1.0) as f32
}

/// Heatmap alpha (`0..=255`) for a depth bar of normalised intensity `frac` (from
/// [`depth_bar_fraction`]): a faint floor so any resting size is visible, ramping to a bright — but
/// deliberately text-safe — peak so a wall of liquidity reads instantly. `frac` is clamped into
/// `0.0..=1.0`, so an out-of-range value can never wrap the `u8`.
fn depth_bar_alpha(frac: f32) -> u8 {
    (cockpit::DEPTH_ALPHA_FLOOR + frac.clamp(0.0, 1.0) * cockpit::DEPTH_ALPHA_SPAN) as u8
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
    let size = Vec2::new(cockpit::MARKER_WIDTH, row.height() - 2.0 * space::SM);
    let x = if side > 0 {
        row.min.x + third - cockpit::MARKER_WIDTH - space::XS
    } else {
        row.min.x + third * 2.0 + space::XS
    };
    Rect::from_min_size(egui::pos2(x, row.min.y + space::SM), size)
}

fn opt_rung(rung: Option<i64>, tick: f64) -> String {
    match rung {
        Some(v) => fmt_rung(v, tick),
        None => "—".to_string(),
    }
}

/// Share-size label. KEPT local, not `vike_ui_theme::fmt::format_compact`, which diverges: uppercase
/// `K`, trailing-zero trim, and M/B tiers ("2.3M" vs our "2300.0k") — pinned in the tests.
fn fmt_size(q: f64) -> String {
    if q.abs() >= 1000.0 { format!("{:.1}k", q / 1000.0) } else { format!("{q:.0}") }
}

/// Draw one frame of the probability ladder and return the actions produced this frame.
///
/// `state` carries hover/selection across frames; `inputs` is the borrowed per-rung depth, the
/// resting orders, and the inside market. A click on a rung's Up/Down column rests a limit there
/// ([`CockpitProbabilityLadderAction::PlaceLimit`]); a click on the inline cancel icon of an own
/// resting-order marker cancels it ([`CockpitProbabilityLadderAction::CancelOrder`]).
pub fn draw_probability_ladder(
    ui: &mut egui::Ui,
    state: &mut CockpitProbabilityLadderState,
    inputs: &CockpitProbabilityLadderInputs,
) -> Vec<CockpitProbabilityLadderAction> {
    let t = ThemeTokens::of(ui.ctx());
    // the density's ladder row and header strip (spec §3.4)
    let (row_h, header_h) = (t.metrics.row_height, t.metrics.header_height);
    let mut actions: Vec<CockpitProbabilityLadderAction> = Vec::new();

    let full = ui.available_rect_before_wrap();
    let width = full.width();

    // --- header: market label + inside market (+ STALE badge) ---
    ladder_header(ui, inputs, &t);

    // --- ladder region ---
    let region_h = (full.height() - header_h).max(row_h * 3.0);
    let (region, _) = ui.allocate_exact_size(Vec2::new(width, region_h), Sense::hover());
    ui.painter().rect_filled(region, 0.0, t.theme.background);
    let ladder_rect = region;

    let n_rows = (region.height() / row_h).floor().max(1.0) as usize;
    let tick = inputs.tick;
    let live = live_rungs(tick);
    let center = inside_center(inputs.inside_bid, inputs.inside_ask, tick);
    let top_rung = top_rung_for(center, n_rows, tick);

    // per-rung depth lookup for O(1) row reads
    let mut lv: std::collections::HashMap<i64, (f64, f64)> = std::collections::HashMap::new();
    for l in inputs.levels {
        lv.insert(l.price_ticks, (l.bid_size, l.ask_size));
    }

    // depth-bar / heatmap scale: peak single-side resting size over the VISIBLE rungs, read from
    // `inputs.levels` via the `lv` lookup (nothing threaded in from outside). Left RAW — a peak of
    // 0 (an empty / all-zero book) is handled by `depth_bar_fraction`'s guard, which paints no bar, so
    // an empty ladder renders byte-identically to before.
    let mut max_size = 0.0_f64;
    for i in 0..n_rows {
        let rung = rung_at_row(top_rung, i);
        if let Some(&(b, a)) = lv.get(&rung) {
            max_size = max_size.max(b).max(a);
        }
    }

    let painter = ui.painter().clone();
    let third = ladder_rect.width() / 3.0;
    for i in 0..n_rows {
        let rung = rung_at_row(top_rung, i);
        let row = row_rect(ladder_rect, i, row_h);

        // row background: inside-market tint / spread band / zebra
        if Some(rung) == inputs.inside_bid {
            at_alpha(&painter, cockpit::INSIDE_ALPHA).rect_filled(
                row,
                0.0,
                maps::side::BID.colour.resolve(&t),
            );
        } else if Some(rung) == inputs.inside_ask {
            at_alpha(&painter, cockpit::INSIDE_ALPHA).rect_filled(
                row,
                0.0,
                maps::side::ASK.colour.resolve(&t),
            );
        } else if in_spread(rung, inputs.inside_bid, inputs.inside_ask) {
            // the boundary between plus and minus is the neutral analysis line (spec §3.2)
            at_alpha(&painter, cockpit::SPREAD_ALPHA).rect_filled(row, 0.0, t.theme.analysis_line);
        } else if i % 2 == 1 {
            painter.rect_filled(row, 0.0, t.theme.surface);
        }

        // rungs outside the live range carry no depth / price / markers
        if !live.contains(&rung) {
            continue;
        }

        // hover wash (the picked rung's selection edge is painted LAST in the rung, below)
        if state.selected != Some(rung) && state.hover == Some(rung) {
            at_alpha(&painter, cockpit::HOVER_ALPHA).rect_filled(row, 0.0, t.theme.text);
        }

        let (bidq, askq) = lv.get(&rung).copied().unwrap_or((0.0, 0.0));
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
            let frac = depth_bar_fraction(bidq, max_size);
            let w = frac * bidcol.width();
            let bar = Rect::from_min_max(
                egui::pos2(bidcol.max.x - w, row.min.y + space::HAIR),
                egui::pos2(bidcol.max.x, row.max.y - space::HAIR),
            );
            at_alpha(&painter, depth_bar_alpha(frac)).rect_filled(
                bar,
                0.0,
                maps::side::BID.colour.resolve(&t),
            );
            painter.text(
                egui::pos2(bidcol.max.x - t.metrics.padding, row.center().y),
                Align2::RIGHT_CENTER,
                fmt_size(bidq),
                t.mono(ThemeTextRole::Body),
                maps::side::BID.text.resolve(&t),
            );
        }
        if askq > 0.0 {
            let frac = depth_bar_fraction(askq, max_size);
            let w = frac * askcol.width();
            let bar = Rect::from_min_max(
                egui::pos2(askcol.min.x, row.min.y + space::HAIR),
                egui::pos2(askcol.min.x + w, row.max.y - space::HAIR),
            );
            at_alpha(&painter, depth_bar_alpha(frac)).rect_filled(
                bar,
                0.0,
                maps::side::ASK.colour.resolve(&t),
            );
            painter.text(
                egui::pos2(askcol.min.x + t.metrics.padding, row.center().y),
                Align2::LEFT_CENTER,
                fmt_size(askq),
                t.mono(ThemeTextRole::Body),
                maps::side::ASK.text.resolve(&t),
            );
        }

        // centre: the rung's price, in cents
        painter.text(
            row.center(),
            Align2::CENTER_CENTER,
            fmt_rung(rung, tick),
            t.mono(ThemeTextRole::Body),
            if state.selected == Some(rung) { t.theme.text } else { t.theme.text_secondary },
        );

        // resting-order markers on this rung
        for o in inputs.orders {
            if o.price_ticks != rung {
                continue;
            }
            let mr = marker_rect(row, o.side);
            let mcol = match o.marker {
                CockpitProbabilityLadderMarker::Resting => {
                    ThemeSidePair::BuySell.colour(o.side > 0, &t)
                }
            };
            // A resting order's marker is the cancel icon (a click on it cancels the order), at the
            // caption size, in the kit's on-fill black: the marker is a FILLED control in the
            // market colour.
            let round = CornerRadius::same(RADIUS);
            painter.rect_filled(mr, round, mcol);
            painter.rect_stroke(
                mr,
                round,
                Stroke::new(stroke::HAIRLINE, t.theme.text),
                StrokeKind::Middle,
            );
            let at = (mr.center(), Align2::CENTER_CENTER);
            let font = FontId::new(t.text.px(ThemeTextRole::Caption), icons::family());
            icons::CANCEL.paint(&painter, at.0, at.1, font, ON_FILL);
        }

        // The picked rung carries the accent's selected-row marker (spec §2), an edge `stroke::EDGE`
        // wide at its left, the same one the kit's table and rail draw — painted LAST in the rung, so
        // a full-width bid bar cannot cover it.
        if state.selected == Some(rung) {
            let edge = Rect::from_min_size(row.min, Vec2::new(stroke::EDGE, row.height()));
            painter.rect_filled(edge, 0.0, t.theme.accent);
        }
    }

    // stale overlay: dim the whole ladder so a frozen/unarrived book never reads as live
    if inputs.stale {
        at_alpha(&painter, cockpit::STALE_ALPHA).rect_filled(region, 0.0, t.theme.background);
    }

    // --- interaction ---
    let response = ui.interact(ladder_rect, ui.scope_id().with("prob_ladder"), Sense::click());
    state.hover = None;
    if let Some(screen_position) = response.hover_pos() {
        let index = row_at_y(screen_position.y, ladder_rect.min.y, row_h, n_rows);
        let rung = rung_at_row(top_rung, index);
        if live.contains(&rung) {
            state.hover = Some(rung);
        }
        if response.clicked() && live.contains(&rung) {
            let side = match col_at(screen_position.x, ladder_rect.min.x, ladder_rect.width()) {
                CockpitProbabilityLadderColumn::Up => 1,
                CockpitProbabilityLadderColumn::Down => -1,
                CockpitProbabilityLadderColumn::Price => 0,
            };
            if side != 0 {
                state.selected = Some(rung);
                let row = row_rect(ladder_rect, index, row_h);
                // inline ✕ on an own order cancels; anywhere else on the rung rests a limit
                let on_marker = inputs
                    .orders
                    .iter()
                    .find(|o| o.price_ticks == rung && o.side == side)
                    .filter(|o| marker_rect(row, o.side).contains(screen_position));
                if let Some(o) = on_marker {
                    actions.push(CockpitProbabilityLadderAction::CancelOrder(
                        o.order_client_id.clone(),
                    ));
                } else {
                    let price = price_of_rung(rung, tick);
                    actions.push(CockpitProbabilityLadderAction::PlaceLimit { side, price });
                }
            }
        }
    }

    actions
}

fn ladder_header(ui: &mut egui::Ui, inputs: &CockpitProbabilityLadderInputs, t: &ThemeTokens) {
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), t.metrics.header_height),
        Sense::hover(),
    );
    ui.painter().rect_filled(rect, 0.0, t.theme.background);
    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(stroke::HAIRLINE, t.theme.border),
    );
    ui.painter().text(
        egui::pos2(rect.min.x + t.metrics.padding, rect.center().y),
        Align2::LEFT_CENTER,
        inputs.asset,
        t.mono(ThemeTextRole::Strong),
        t.theme.text,
    );
    let inside = format!(
        "bid {} / ask {}",
        opt_rung(inputs.inside_bid, inputs.tick),
        opt_rung(inputs.inside_ask, inputs.tick)
    );
    ui.painter().text(
        egui::pos2(rect.max.x - t.metrics.padding, rect.center().y),
        Align2::RIGHT_CENTER,
        inside,
        t.mono(ThemeTextRole::Body),
        t.theme.text_tertiary,
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
        chip::badge(&mut badge, "STALE", ThemeStatus::Warning);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_rung_bounds_to_live_range() {
        assert_eq!(clamp_rung(0, 0.01), 1);
        assert_eq!(clamp_rung(1, 0.01), 1);
        assert_eq!(clamp_rung(50, 0.01), 50);
        assert_eq!(clamp_rung(99, 0.01), 99);
        assert_eq!(clamp_rung(100, 0.01), 99);
        assert_eq!(clamp_rung(1000, 0.01), 99);
        assert_eq!(clamp_rung(-5, 0.01), 1);
    }

    #[test]
    fn price_of_rung_maps_to_unit_interval() {
        assert_eq!(price_of_rung(62, 0.01), 0.62);
        assert_eq!(price_of_rung(1, 0.01), 0.01);
        assert_eq!(price_of_rung(99, 0.01), 0.99);
    }

    /// A market on Polymarket's 0.001 tick ladders every thousandth: a level at 0.996 is rung 996
    /// (the whole-cent ladder rounded it to 100, a resolved state, and dropped it), a click there
    /// sends 0.996, and the rung prints `99.6¢`. The whole cent still prints `62¢`, and a tick that
    /// leaves no live rung reads as the whole cent.
    #[test]
    fn a_thousandth_tick_ladders_every_thousandth() {
        assert_eq!(live_rungs(0.001), 1..=999);
        assert_eq!(live_rungs(0.01), 1..=99);
        assert_eq!(rung_of(0.996, 0.001), 996);
        assert!(live_rungs(0.001).contains(&rung_of(0.996, 0.001)));
        assert_eq!(price_of_rung(996, 0.001), 0.996);
        assert_eq!(fmt_rung(996, 0.001), "99.6¢");
        assert_eq!(fmt_rung(62, 0.01), "62¢");
        assert_eq!(top_rung_for(995, 20, 0.001), 999);
        assert_eq!(inside_center(None, None, 0.001), 500);
        for nonsense in [0.0, -0.01, f64::NAN, 1.0] {
            assert_eq!(live_rungs(nonsense), 1..=99, "{nonsense}");
        }
    }

    #[test]
    fn rung_at_row_counts_down_from_top() {
        assert_eq!(rung_at_row(60, 0), 60);
        assert_eq!(rung_at_row(60, 1), 59);
        assert_eq!(rung_at_row(60, 10), 50);
    }

    #[test]
    fn top_rung_centres_and_clamps_at_99() {
        // 20 rows, centre 60 → half = 10 → top 70
        assert_eq!(top_rung_for(60, 20, 0.01), 70);
        // a high centre clamps the top at 99 rather than showing a 0/100 rung
        assert_eq!(top_rung_for(95, 20, 0.01), 99);
        // centre gets clamped into range first
        assert_eq!(top_rung_for(200, 10, 0.01), 99);
    }

    #[test]
    fn inside_center_mid_and_fallbacks() {
        assert_eq!(inside_center(Some(40), Some(60), 0.01), 50);
        assert_eq!(inside_center(Some(40), None, 0.01), 40);
        assert_eq!(inside_center(None, Some(60), 0.01), 60);
        assert_eq!(inside_center(None, None, 0.01), 50);
        // out-of-range inputs are clamped
        assert_eq!(inside_center(Some(0), Some(0), 0.01), 1);
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
        assert_eq!(col_at(10.0, 0.0, 300.0), CockpitProbabilityLadderColumn::Up);
        assert_eq!(col_at(150.0, 0.0, 300.0), CockpitProbabilityLadderColumn::Price);
        assert_eq!(col_at(280.0, 0.0, 300.0), CockpitProbabilityLadderColumn::Down);
        // clamped at the edges
        assert_eq!(col_at(-50.0, 0.0, 300.0), CockpitProbabilityLadderColumn::Up);
        assert_eq!(col_at(9999.0, 0.0, 300.0), CockpitProbabilityLadderColumn::Down);
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
    /// instead of swapping to `vike_ui_theme::fmt::format_compact` (GUI audit F7 — a swap would
    /// change rendered text).
    #[test]
    fn fmt_size_scales_thousands() {
        assert_eq!(fmt_size(950.0), "950");
        assert_eq!(fmt_size(1500.0), "1.5k"); // lowercase k — format_compact prints "1.5K"
        assert_eq!(fmt_size(0.0), "0");
        assert_eq!(fmt_size(2_300_000.0), "2300.0k"); // no M tier — format_compact prints "2.3M"
    }

    #[test]
    fn state_defaults_to_no_hover_or_selection() {
        let s = CockpitProbabilityLadderState::default();
        assert_eq!(s.hover, None);
        assert_eq!(s.selected, None);
    }

    #[test]
    fn depth_bar_fraction_normalises_and_guards() {
        // a non-positive peak (empty / all-zero book) ⇒ no bar — the guard that keeps an empty
        // ladder byte-identical to the pre-heatmap render
        assert_eq!(depth_bar_fraction(100.0, 0.0), 0.0);
        assert_eq!(depth_bar_fraction(100.0, -5.0), 0.0);
        // size AT the visible peak fills the column (exactly 1.0)
        assert_eq!(depth_bar_fraction(50.0, 50.0), 1.0);
        // size ABOVE the peak clamps to 1.0 (a bar never overflows its third)
        assert_eq!(depth_bar_fraction(250.0, 100.0), 1.0);
        // an exactly-representable mid fraction is exact
        assert_eq!(depth_bar_fraction(25.0, 100.0), 0.25);
        // an inexact mid fraction (1/3 is not representable in f32) ⇒ epsilon, never `==`
        let third = depth_bar_fraction(30.0, 90.0);
        assert!((third - 1.0 / 3.0).abs() < 1e-6, "expected ~1/3 of the peak, got {third}");
        // a zero / negative / non-finite size ⇒ no bar
        assert_eq!(depth_bar_fraction(0.0, 100.0), 0.0);
        assert_eq!(depth_bar_fraction(-10.0, 100.0), 0.0);
        assert_eq!(depth_bar_fraction(f64::NAN, 100.0), 0.0);
        assert_eq!(depth_bar_fraction(f64::INFINITY, 100.0), 0.0);
        // a non-finite peak is guarded too
        assert_eq!(depth_bar_fraction(50.0, f64::NAN), 0.0);
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

    /// The whole cent.
    const CENT: f64 = 0.01;

    #[test]
    fn a_price_rounds_to_the_nearest_rung() {
        let rung = |p| rung_of(p, CENT);
        assert_eq!(rung(0.0), 0);
        assert_eq!(rung(0.014), 1);
        assert_eq!(rung(0.5), 50);
        assert_eq!(rung(0.996), 100);
    }

    /// On a market whose tick is 0.001 (the bridge's catalog pins such markets) a level at 0.996 is
    /// a live rung, 996, where the whole-cent law rounded it to 100, a resolved outcome, and dropped
    /// it; and two levels a thousandth apart stay two rungs.
    #[test]
    fn a_thousandth_tick_market_keeps_its_levels_near_one() {
        let levels = bucket_probability_levels(
            &[L2BookLevel::new(0.995, 3.0)],
            &[L2BookLevel::new(0.996, 2.0)],
            0.001,
        );
        let rungs: Vec<(i64, f64, f64)> =
            levels.iter().map(|l| (l.price_ticks, l.bid_size, l.ask_size)).collect();
        assert_eq!(rungs, vec![(995, 3.0, 0.0), (996, 0.0, 2.0)]);
    }

    #[test]
    fn bucket_prob_levels_sums_levels_that_share_a_cent() {
        // 0.501 and 0.5049 both round to rung 50 → one summed row.
        let bids = [
            L2BookLevel::new(0.501, 10.0),
            L2BookLevel::new(0.5049, 5.0),
            L2BookLevel::new(0.49, 3.0),
        ];
        let levels = bucket_probability_levels(&bids, &[], CENT);
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].price_ticks, 49);
        assert_eq!(levels[0].bid_size, 3.0);
        assert_eq!(levels[1].price_ticks, 50);
        assert_eq!(levels[1].bid_size, 15.0);
        assert_eq!(levels[1].ask_size, 0.0);
    }

    #[test]
    fn bucket_prob_levels_merges_both_sides_onto_one_rung() {
        let levels = bucket_probability_levels(
            &[L2BookLevel::new(0.42, 7.0)],
            &[L2BookLevel::new(0.42, 2.0), L2BookLevel::new(0.43, 4.0)],
            CENT,
        );
        assert_eq!(levels.len(), 2);
        assert_eq!((levels[0].price_ticks, levels[0].bid_size, levels[0].ask_size), (42, 7.0, 2.0));
        assert_eq!((levels[1].price_ticks, levels[1].bid_size, levels[1].ask_size), (43, 0.0, 4.0));
    }

    #[test]
    fn bucket_prob_levels_drops_the_resolved_ends() {
        // rung 0 and rung 100 are not tradable probability rungs — the ladder never paints them.
        let levels = bucket_probability_levels(
            &[L2BookLevel::new(0.001, 9.0)],
            &[L2BookLevel::new(0.9999, 9.0)],
            CENT,
        );
        assert!(levels.is_empty());
    }

    #[test]
    fn bucket_prob_levels_of_an_empty_book_is_empty() {
        assert!(bucket_probability_levels(&[], &[], CENT).is_empty());
    }

    #[test]
    fn levels_come_back_ascending_by_cent() {
        let levels = bucket_probability_levels(
            &[
                L2BookLevel::new(0.90, 1.0),
                L2BookLevel::new(0.10, 1.0),
                L2BookLevel::new(0.50, 1.0),
            ],
            &[],
            CENT,
        );
        let cents: Vec<i64> = levels.iter().map(|l| l.price_ticks).collect();
        assert_eq!(cents, vec![10, 50, 90]);
    }
}
