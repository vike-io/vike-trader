//! The DOM (depth-of-market) order-entry ladder — the "Pro" click-trade ladder plus
//! the "Elite" order-flow surface (a time×price liquidity heatmap strip), switchable in
//! the window header. RUST-NATIVE (no Python twin — the oracle app has no DOM).
//!
//! Dependency contract holds: this is pure `egui` painting over `vike_model::L2Book`,
//! with NO dependency on the execution core. The widget is a stateless-render function
//! ([`draw`]) over caller-owned [`DomState`] + borrowed [`DomInputs`]; every trader
//! intent leaves as a neutral [`DomAction`] the app maps to a `vike_exec::Command`
//! (id-minting stays in the runtime). Same seam the chart uses: data in, actions out.
//!
//! Interaction grammar (see the platform research): **side-bound** click-to-trade —
//! the column decides the side (Buy/Bid left, Sell/Ask right), the row decides the
//! price, and limit-vs-stop resolves by side of market (a buy above the ask / sell
//! below the bid is a stop), with `Shift` forcing a stop. Clicking your own resting
//! order cancels it; dragging it reprices (emits `Modify`). Footer carries
//! Close/Reverse + cancel-side buttons. This mirrors NinjaTrader SuperDOM / Tealstreet /
//! Quantower DOM Trader, adapted to crypto.

use egui::{Align2, Color32, FontFamily, FontId, Rect, Sense, Stroke, StrokeKind, Vec2};
use vike_model::{L2Book, Level, VenueCaps};
// The shared dark trading-terminal palette (one pinned home; this file used to hand-copy the
// const block AND its own `bid_dim`/`ask_dim` alpha helpers, now `dim` — GUI audit F3). The DOM
// keeps its side-language names: BID/ASK/LAST are the trading palette's UP/DOWN/ACCENT.
use vike_ui_theme::palette::trading::{
    dim, ACCENT as LAST, DOWN as ASK, FAINT, MUTED, PANEL, PANEL2, RULE, TXT, UP as BID,
};

const ROW_H: f32 = 20.0;
const HEADER_H: f32 = 26.0;
const TOOLBAR_H: f32 = 26.0;
const FOOTER_H: f32 = 28.0;
/// Coin-qty presets shown in the toolbar (BTC-scale defaults; caller may override later).
const QTY_PRESETS: [f64; 5] = [0.001, 0.005, 0.01, 0.05, 0.1];
/// Grouping steps in ticks (the price-consolidation selector).
const GROUP_STEPS: [i64; 6] = [1, 2, 5, 10, 25, 50];

/// Which surface the DOM window renders. Switched in the header — the app mirrors it into
/// the window title.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DomMode {
    /// Classic click-trade ladder.
    Pro,
    /// Ladder + time×price liquidity heatmap strip.
    Elite,
}

impl DomMode {
    pub fn label(self) -> &'static str {
        match self {
            DomMode::Pro => "Pro",
            DomMode::Elite => "Elite",
        }
    }
}

/// Which venue's live book the DOM displays. The app maps this to that venue's feed + symbol and
/// hands back the matching book; the widget only renders what it's given. (Order ENTRY still routes
/// to the configured execution core — live cross-venue routing is a separate follow-up.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DomVenue {
    Binance,
    Bybit,
    Okx,
    Aster,
    Hyperliquid,
}

impl DomVenue {
    pub fn label(self) -> &'static str {
        match self {
            DomVenue::Binance => "Binance",
            DomVenue::Bybit => "Bybit",
            DomVenue::Okx => "OKX",
            DomVenue::Aster => "Aster",
            DomVenue::Hyperliquid => "Hyperliquid",
        }
    }
    /// Next venue in the header cycle.
    pub fn next(self) -> Self {
        match self {
            DomVenue::Binance => DomVenue::Bybit,
            DomVenue::Bybit => DomVenue::Okx,
            DomVenue::Okx => DomVenue::Aster,
            DomVenue::Aster => DomVenue::Hyperliquid,
            DomVenue::Hyperliquid => DomVenue::Binance,
        }
    }
}

/// A trader intent leaving the widget. The app mints a client-order-id and maps this to a
/// `vike_exec::Command` (`Place`→`Submit`, `Modify`→`Modify`, cancels→`Cancel`/`CancelBatch`/
/// `MassCancel`). Prices are absolute; `side` is +1 buy / −1 sell.
#[derive(Clone, PartialEq, Debug)]
pub enum DomAction {
    /// Place a new order. `stop` selects stop(true)/limit(false); `reduce_only` from the toolbar.
    Place { side: i32, price: f64, qty: f64, stop: bool, reduce_only: bool },
    /// Reprice a resting order (drag) — the app issues `Command::Modify { new_price }`.
    Modify { coid: String, new_price: f64 },
    /// Cancel one resting order by id.
    Cancel(String),
    /// Cancel every resting order on one side (+1 buys / −1 sells).
    CancelSide(i32),
    /// Cancel every resting order (pull all quotes).
    CancelAll,
    /// Flatten the open position at market (reduce-only).
    ClosePosition,
    /// Close and re-open the opposite position at market.
    Reverse,
}

/// A resting order the widget draws on the ladder. Built by the app from `OrderView`
/// (filtered to this symbol) — vike-chart can't see `vike_core` types, so this is the
/// lightweight local mirror.
#[derive(Clone, Debug)]
pub struct DomOrder {
    pub client_order_id: String,
    /// +1 buy / −1 sell
    pub side: i32,
    /// resting price (limit price, or the stop trigger)
    pub price: f64,
    pub qty: f64,
    pub is_stop: bool,
    pub filled_qty: f64,
}

/// The open position for this symbol (signed size, avg entry). Local mirror of `PositionView`.
#[derive(Clone, Copy, Debug)]
pub struct DomPosition {
    /// signed: + long / − short (0 ⇒ flat, not shown)
    pub size: f64,
    pub avg_px: f64,
    /// unrealized P/L in quote currency (already computed by the app)
    pub upnl: f64,
}

/// Everything the widget needs to render one frame. Borrowed — the app owns the storage.
pub struct DomInputs<'a> {
    pub book: &'a L2Book,
    /// last traded price (drives the amber row + auto-center)
    pub last: Option<f64>,
    /// working orders for THIS symbol only
    pub orders: &'a [DomOrder],
    /// open position for this symbol, if any
    pub position: Option<DomPosition>,
    /// the displayed book is stale (no live update within the freshness window, or none yet) —
    /// the widget dims the ladder and shows a badge so a frozen book never reads as live
    pub stale: bool,
    /// this venue's orders route to a PAPER engine (true) vs a LIVE venue client (false) — the
    /// header badge makes the trading mode unmistakable before a click sends anything
    pub paper: bool,
    /// the DECLARED capabilities of the selected venue's adapter (audit br6). The widget consults
    /// `caps.allows_modify()` to enable/grey the drag-to-reprice control BEFORE offering it, rather
    /// than letting an unsupported modify become a post-hoc reject. The app fills this from
    /// [`vike_model::caps_for`]; the default [`VenueCaps::UNSUPPORTED`] offers nothing.
    pub caps: VenueCaps,
}

/// Whether the DOM may begin a drag-to-reprice on an own resting order for the given venue caps —
/// the modify-gate the widget consults at the drag-start, the drop, AND the marker affordance.
/// Extracted so the gate logic is unit-testable without an egui frame (audit br6).
#[inline]
fn drag_to_reprice_allowed(caps: &VenueCaps) -> bool {
    caps.allows_modify()
}

/// A fixed-capacity time×price liquidity ring buffer for the Elite heatmap. Each column is a
/// per-row intensity snapshot (top→bottom, aligned to the ladder's visible rows), pushed on a
/// throttled cadence; the oldest column falls off the left. Normalised by a running peak.
///
/// This is the modest, egui-painter form of the Bookmap/Quantower-DOM-Surface heatmap: O(rows)
/// per push, rendered as rects. A GPU/texture upgrade stays a later, feature-gated change (the
/// render-layer plan's density trigger) — the ring buffer here is the data layer it would read.
#[derive(Clone, Debug)]
pub struct DomHeatmap {
    cols: std::collections::VecDeque<Vec<f32>>,
    max_cols: usize,
    peak: f32,
}

impl Default for DomHeatmap {
    fn default() -> Self {
        DomHeatmap { cols: std::collections::VecDeque::new(), max_cols: 180, peak: 1.0 }
    }
}

impl DomHeatmap {
    /// Append one time-slice (per-row intensities, top→bottom). Bounds the ring to `max_cols`
    /// and ratchets the running peak for normalisation.
    pub fn push(&mut self, col: Vec<f32>) {
        for &v in &col {
            if v > self.peak {
                self.peak = v;
            }
        }
        self.cols.push_back(col);
        while self.cols.len() > self.max_cols {
            self.cols.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.cols.len()
    }
    pub fn is_empty(&self) -> bool {
        self.cols.is_empty()
    }
}

/// Cross-frame view state for one DOM window (owned by the app, mirrored into `ToolView`).
#[derive(Clone, Debug)]
pub struct DomState {
    pub mode: DomMode,
    /// which venue's book to display (the app routes the feed + hands back the book)
    pub venue: DomVenue,
    /// order quantity in coin units
    pub qty: f64,
    /// grouping in ticks (price consolidation)
    pub group: i64,
    /// reduce-only arm (toolbar toggle)
    pub reduce_only: bool,
    /// manual center price; `None` ⇒ auto-center on last/mid
    pub center: Option<f64>,
    /// id of a resting order currently being dragged to reprice
    pub drag: Option<String>,
    /// OPT-IN cost-to-fill readout (footer): projected avg fill price + slippage vs mid for a
    /// hypothetical market order of [`DomState::cost_qty`] on EACH side. `false` (default) ⇒
    /// the render path is unchanged — no walk, no extra painting, no layout change.
    pub cost_to_fill: bool,
    /// Hypothetical size for the cost-to-fill readout; `None` ⇒ follow the toolbar order qty
    /// (`state.qty`), which is what a trader is about to click with.
    pub cost_qty: Option<f64>,
    /// Elite heatmap history
    pub heat: DomHeatmap,
    /// repaint counter, for throttling heatmap pushes off the render cadence
    pub tick: u64,
}

impl Default for DomState {
    fn default() -> Self {
        DomState {
            mode: DomMode::Pro,
            venue: DomVenue::Binance,
            qty: 0.01,
            group: 1,
            reduce_only: false,
            center: None,
            drag: None,
            cost_to_fill: false,
            cost_qty: None,
            heat: DomHeatmap::default(),
            tick: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Pure logic (unit-tested) — no egui, so the interaction grammar is verifiable.
// ---------------------------------------------------------------------------

/// Quantize a price to its grouped row key: the tick index divided (floor) by the group size.
/// A row key spans `group` ticks; `key_price` is its inverse (the row's aligned low edge).
fn row_key(price: f64, tick: f64, group: i64) -> i64 {
    let g = group.max(1);
    let ti = (price / tick).round() as i64;
    ti.div_euclid(g)
}

fn key_price(key: i64, tick: f64, group: i64) -> f64 {
    let g = group.max(1);
    key as f64 * g as f64 * tick
}

/// Aggregate raw `(price, qty)` levels into grouped rows keyed by [`row_key`], summing qty.
/// Returns a map of `row_key → total_qty` — the render/hit-test view of one book side.
fn group_book(levels: &[Level], tick: f64, group: i64) -> std::collections::HashMap<i64, f64> {
    let mut m: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();
    for &(px, qty) in levels {
        *m.entry(row_key(px, tick, group)).or_insert(0.0) += qty;
    }
    m
}

/// Projected cost of taking `qty` on ONE side, walked over the DISPLAYED book
/// (`vike_model::L2Book::simulate_fill`). Estimate only: hidden flow, latency and queue
/// dynamics can only make the real fill worse.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SideCost {
    /// VWAP over what the displayed book could fill
    pub avg_px: f64,
    /// signed cost vs mid in bps — POSITIVE = worse than mid for the taker. `None` when the
    /// book is one-sided (no mid) or mid is 0.
    pub slippage_bps: Option<f64>,
    /// deepest price the walk touched
    pub worst_px: f64,
    /// how much of `qty` displayed depth covers
    pub filled: f64,
    /// the walk covered the full requested size
    pub complete: bool,
    /// price levels consumed
    pub levels: usize,
}

/// Cost-to-fill for a hypothetical market order of `qty`, both sides. Each side is `None` when
/// nothing at all could fill there (empty side, or a non-positive/NaN qty).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct DomCostToFill {
    /// the hypothetical size this was computed for
    pub qty: f64,
    /// BUY: lifts asks low→high
    pub buy: Option<SideCost>,
    /// SELL: hits bids high→low
    pub sell: Option<SideCost>,
}

fn side_cost(book: &L2Book, side: i32, qty: f64) -> Option<SideCost> {
    let sim = book.simulate_fill(side, qty);
    Some(SideCost {
        avg_px: sim.avg_px?,
        slippage_bps: sim.slippage_bps_vs_mid,
        worst_px: sim.worst_px?,
        filled: sim.total_filled,
        complete: sim.remaining == 0.0,
        levels: sim.levels_consumed,
    })
}

/// Walk both sides of the book for a hypothetical `qty`. PURE — the unit-testable core behind
/// the opt-in footer readout ([`DomState::cost_to_fill`]); nothing calls it when the toggle is off.
///
/// SAME MATH AS THE GATE, deliberately: this is `L2Book::simulate_fill` verbatim, which is also
/// what `vike_exec::impact_veto` walks, so the bps a trader reads here is the bps a
/// `max_slippage_bps` gate compares to its budget (pinned by
/// `cost_to_fill_matches_the_gates_simulate_fill`). vike-chart does NOT depend on vike-exec, so
/// the shared primitive in vike-model is the seam that keeps them from drifting. Two known
/// divergences, both disclosed to the user by [`cost_label`]: an incomplete walk (rendered `∞`,
/// which the gate treats as not-fillable), and lot rounding (the gate rounds size to the venue
/// lot grid, this walks the size as typed — the readout prints the size it walked).
pub fn cost_to_fill(book: &L2Book, qty: f64) -> DomCostToFill {
    DomCostToFill { qty, buy: side_cost(book, 1, qty), sell: side_cost(book, -1, qty) }
}

/// PURE label + hover text for ONE side of the footer readout. Extracted so the wording is
/// unit-testable without an egui frame.
///
/// The key rule (review): an INCOMPLETE walk must NOT read as a finite, affordable cost. The
/// displayed book cannot cover the size, so the true cost is unbounded — and
/// `vike_exec::impact_veto` denies exactly this case as `impact-not-fillable`. Printing the
/// optimistic VWAP of the fillable slice next to a slippage figure invites "40bp, well under my
/// 100bp budget" right before the gate rejects the order. So a partial walk shows `∞` where the
/// bps would go, with the covered fraction spelled out in the hover.
fn cost_label(label: &str, qty: f64, side: Option<SideCost>) -> (String, String) {
    let Some(s) = side else {
        return (
            format!("{label} —"),
            format!("no displayed depth on this side for {}", fmt_qty(qty)),
        );
    };
    if !s.complete {
        return (
            format!("{label} ~{:.2} ∞", s.avg_px),
            format!(
                "displayed depth covers only {} of {} — cost past the book is UNBOUNDED. \
                 The shown price is the VWAP of the covered part only; a slippage/fillable risk \
                 gate rejects this size as not-fillable.",
                fmt_qty(s.filled),
                fmt_qty(qty)
            ),
        );
    }
    let slip = match s.slippage_bps {
        Some(b) => format!("{b:+.1}bp"),
        None => "—".to_string(),
    };
    let hover = match s.slippage_bps {
        Some(b) => format!(
            "VWAP {:.2} over {} level(s), worst {:.2}, {b:+.1} bp vs mid — the SAME walk a \
             max_slippage_bps risk gate compares against its budget.",
            s.avg_px, s.levels, s.worst_px
        ),
        None => format!(
            "VWAP {:.2} over {} level(s), worst {:.2}. One-sided book ⇒ no mid ⇒ no slippage \
             number (never a fabricated 0).",
            s.avg_px, s.levels, s.worst_px
        ),
    };
    (format!("{label} {:.2} {slip}", s.avg_px), hover)
}

/// Side-bound auto order-type: does a click at `price` in the `side` column resolve to a STOP?
/// A buy above the best ask (or sell below the best bid) would cross ⇒ stop; otherwise a limit.
/// `force_stop` (the Shift modifier) forces a stop regardless.
fn resolve_is_stop(side: i32, price: f64, best_bid: f64, best_ask: f64, force_stop: bool) -> bool {
    if force_stop {
        return true;
    }
    if side > 0 {
        price > best_ask
    } else {
        price < best_bid
    }
}

/// Which ladder column a pointer x falls in, given the ladder rect's left edge and width.
/// Left third = Bid/Buy, middle = Price, right third = Ask/Sell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Col {
    Bid,
    Price,
    Ask,
}

fn col_at(x: f32, left: f32, width: f32) -> Col {
    let rel = ((x - left) / width).clamp(0.0, 0.999);
    if rel < 0.34 {
        Col::Bid
    } else if rel < 0.66 {
        Col::Price
    } else {
        Col::Ask
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Draw one frame of the DOM and return the trader actions produced this frame.
///
/// `state` carries mode/qty/grouping/center/drag/heatmap across frames; `inputs` is the
/// borrowed book + orders + position for this symbol. Actions are drained by the app into
/// `vike_exec::Command`s.
pub fn draw(ui: &mut egui::Ui, state: &mut DomState, inputs: &DomInputs) -> Vec<DomAction> {
    let mut actions: Vec<DomAction> = Vec::new();
    state.tick = state.tick.wrapping_add(1);

    let tick = inputs.book.tick_size.max(f64::MIN_POSITIVE);
    let best_bid = inputs.book.best_bid().map(|(p, _)| p);
    let best_ask = inputs.book.best_ask().map(|(p, _)| p);
    // `last` drives the amber last-trade row; the ladder CENTERS on the live book's mid so the
    // real depth is always in view even when the last-trade mark (a lagging kline close) sits a
    // few ticks off the top of book. Fall back to `last` when the book has no two-sided top.
    let last = inputs.last.or_else(|| inputs.book.mid()).or(best_bid).or(best_ask).unwrap_or(0.0);
    let book_center = inputs.book.mid().or(best_bid).or(best_ask).unwrap_or(last);

    let full = ui.available_rect_before_wrap();
    let width = full.width();

    // --- header: mode switch + venue + PAPER/LIVE + symbol/last (+ STALE badge) ---
    header(ui, state, last, inputs.stale, inputs.paper);

    // --- toolbar: qty presets, grouping, recenter, reduce-only ---
    toolbar(ui, state);

    // --- ladder / heatmap region ---
    let region_h = (full.height() - HEADER_H - TOOLBAR_H - FOOTER_H).max(ROW_H * 5.0);
    let (region, _) = ui.allocate_exact_size(Vec2::new(width, region_h), Sense::hover());
    ui.painter().rect_filled(region, 0.0, PANEL);

    // Elite splits the region: heatmap strip on the left, ladder on the right (shared row Y).
    let (heat_rect, ladder_rect) = if state.mode == DomMode::Elite {
        let hw = (region.width() * 0.42).floor();
        (
            Some(Rect::from_min_size(region.min, Vec2::new(hw, region.height()))),
            Rect::from_min_max(egui::pos2(region.min.x + hw + 1.0, region.min.y), region.max),
        )
    } else {
        (None, region)
    };

    let n_rows = (region.height() / ROW_H).floor().max(1.0) as usize;
    let half = (n_rows / 2) as i64;

    // Center key: manual latch (scroll/drag), else the live book's mid.
    let center = state.center.unwrap_or(book_center);
    let center_key = row_key(center, tick, state.group);

    let (bids, asks) = inputs.book.top_n(400);
    let bid_map = group_book(&bids, tick, state.group);
    let ask_map = group_book(&asks, tick, state.group);

    // Peak grouped qty over the visible window → depth-bar scale.
    let mut vmax = 0.0_f64;
    for i in 0..n_rows {
        let key = center_key + half - i as i64;
        let q =
            bid_map.get(&key).copied().unwrap_or(0.0) + ask_map.get(&key).copied().unwrap_or(0.0);
        if q > vmax {
            vmax = q;
        }
    }
    let vmax = if vmax <= 0.0 { 1.0 } else { vmax };

    let last_key = row_key(last, tick, state.group);
    let pos_key = inputs.position.map(|p| row_key(p.avg_px, tick, state.group));

    // Elite: push a heatmap column of this frame's visible liquidity (throttled).
    if let Some(hr) = heat_rect {
        if state.tick.is_multiple_of(6) {
            let mut col = Vec::with_capacity(n_rows);
            for i in 0..n_rows {
                let key = center_key + half - i as i64;
                let q = bid_map.get(&key).copied().unwrap_or(0.0)
                    + ask_map.get(&key).copied().unwrap_or(0.0);
                col.push(q as f32);
            }
            state.heat.push(col);
        }
        paint_heatmap(ui, hr, &state.heat, n_rows);
    }

    // --- ladder rows ---
    let colw = ladder_rect.width();
    let painter = ui.painter().clone();
    for i in 0..n_rows {
        let key = center_key + half - i as i64;
        let price = key_price(key, tick, state.group);
        let top = ladder_rect.min.y + i as f32 * ROW_H;
        let row = Rect::from_min_size(egui::pos2(ladder_rect.min.x, top), Vec2::new(colw, ROW_H));
        let is_last = key == last_key;
        let is_pos = pos_key == Some(key);
        if is_last {
            painter.rect_filled(row, 0.0, LAST.linear_multiply(0.14));
        } else if is_pos {
            painter.rect_filled(row, 0.0, dim(BID, 28));
        } else if i % 2 == 1 {
            painter.rect_filled(row, 0.0, PANEL2);
        }

        let bidq = bid_map.get(&key).copied().unwrap_or(0.0);
        let askq = ask_map.get(&key).copied().unwrap_or(0.0);
        let bidcol = Rect::from_min_size(row.min, Vec2::new(colw / 3.0, ROW_H));
        let askcol = Rect::from_min_size(
            egui::pos2(row.min.x + colw * 2.0 / 3.0, top),
            Vec2::new(colw / 3.0, ROW_H),
        );
        // depth bars (bid grows leftward from the price column; ask rightward)
        if bidq > 0.0 {
            let w = (bidq / vmax) as f32 * bidcol.width();
            let bar = Rect::from_min_max(
                egui::pos2(bidcol.max.x - w, top + 1.0),
                egui::pos2(bidcol.max.x, top + ROW_H - 1.0),
            );
            painter.rect_filled(bar, 0.0, dim(BID, 48));
            painter.text(
                egui::pos2(bidcol.max.x - 4.0, row.center().y),
                Align2::RIGHT_CENTER,
                fmt_qty(bidq),
                FontId::new(11.0, FontFamily::Monospace),
                BID,
            );
        }
        if askq > 0.0 {
            let w = (askq / vmax) as f32 * askcol.width();
            let bar = Rect::from_min_max(
                egui::pos2(askcol.min.x, top + 1.0),
                egui::pos2(askcol.min.x + w, top + ROW_H - 1.0),
            );
            painter.rect_filled(bar, 0.0, dim(ASK, 46));
            painter.text(
                egui::pos2(askcol.min.x + 4.0, row.center().y),
                Align2::LEFT_CENTER,
                fmt_qty(askq),
                FontId::new(11.0, FontFamily::Monospace),
                ASK,
            );
        }
        // price column
        let pcol = if is_last { LAST } else { MUTED };
        painter.text(
            row.center(),
            Align2::CENTER_CENTER,
            fmt_px(price, tick),
            FontId::new(11.5, FontFamily::Monospace),
            pcol,
        );
        // position avg marker
        if is_pos {
            painter.text(
                egui::pos2(row.center().x, row.center().y),
                Align2::CENTER_CENTER,
                "",
                FontId::new(11.5, FontFamily::Monospace),
                pcol,
            );
        }

        // working-order markers for this row
        for o in inputs.orders {
            if row_key(o.price, tick, state.group) != key {
                continue;
            }
            // marker glyph: "T" = stop trigger (either side); else the side letter (B/S)
            let glyph = if o.is_stop {
                "T"
            } else if o.side > 0 {
                "B"
            } else {
                "S"
            };
            let (mrect, mcol) = if o.side > 0 {
                (
                    Rect::from_min_size(
                        egui::pos2(bidcol.min.x + 2.0, top + 3.0),
                        Vec2::new(14.0, ROW_H - 6.0),
                    ),
                    if o.is_stop { LAST } else { BID },
                )
            } else {
                (
                    Rect::from_min_size(
                        egui::pos2(askcol.max.x - 16.0, top + 3.0),
                        Vec2::new(14.0, ROW_H - 6.0),
                    ),
                    if o.is_stop { LAST } else { ASK },
                )
            };
            let dragging = state.drag.as_deref() == Some(o.client_order_id.as_str());
            painter.rect_filled(mrect, 2.0, mcol);
            // bright outline so the marker pops against the same-colour depth bar behind it
            let (bstroke, brect) = if dragging {
                (Stroke::new(1.6, LAST), mrect.expand(1.5))
            } else {
                // a venue whose adapter can't modify greys the outline so the marker never reads
                // as draggable (audit br6) — cancel-by-click still works, only reprice is gated
                let outline = if drag_to_reprice_allowed(&inputs.caps) {
                    Color32::from_gray(235)
                } else {
                    FAINT
                };
                (Stroke::new(1.3, outline), mrect)
            };
            painter.rect_stroke(brect, 2.0, bstroke, StrokeKind::Middle);
            painter.text(
                mrect.center(),
                Align2::CENTER_CENTER,
                glyph,
                FontId::new(9.0, FontFamily::Monospace),
                Color32::from_rgb(10, 13, 17),
            );
        }
    }

    // spread line at the last/mid boundary
    let spread_y = ladder_rect.min.y + (center_key + half - last_key) as f32 * ROW_H + ROW_H;
    if spread_y > ladder_rect.min.y && spread_y < ladder_rect.max.y {
        painter.line_segment(
            [egui::pos2(ladder_rect.min.x, spread_y), egui::pos2(ladder_rect.max.x, spread_y)],
            Stroke::new(1.0, LAST.linear_multiply(0.7)),
        );
    }

    // stale overlay: dim the whole book region so a frozen/unarrived book never reads as live
    if inputs.stale {
        painter.rect_filled(region, 0.0, Color32::from_rgba_unmultiplied(8, 11, 15, 150));
    }

    // --- interaction over the ladder ---
    let resp = ui.interact(ladder_rect, ui.id().with("dom_ladder"), Sense::click_and_drag());
    let shift = ui.input(|i| i.modifiers.shift);
    let row_at = |y: f32| -> i64 {
        let idx = ((y - ladder_rect.min.y) / ROW_H).floor().clamp(0.0, (n_rows - 1) as f32) as i64;
        center_key + half - idx
    };
    let own_order_at = |key: i64, col: Col| -> Option<&DomOrder> {
        inputs.orders.iter().find(|o| {
            row_key(o.price, tick, state.group) == key
                && ((o.side > 0 && col == Col::Bid) || (o.side < 0 && col == Col::Ask))
        })
    };

    if let Some(pos) = resp.interact_pointer_pos() {
        let key = row_at(pos.y);
        let col = col_at(pos.x, ladder_rect.min.x, colw);
        // begin drag on an own order — ONLY when this venue's adapter wires a native modify (audit
        // br6); a non-modify venue never grabs the order, so no unsupported Modify is ever issued
        // and it can't become a post-hoc reject.
        if resp.drag_started() && drag_to_reprice_allowed(&inputs.caps) {
            if let Some(o) = own_order_at(key, col) {
                state.drag = Some(o.client_order_id.clone());
            }
        }
        // drop: reprice to the row under the pointer. Belt-and-braces re-check of the gate so a
        // drag left dangling by a mid-drag venue switch can never emit a Modify.
        if resp.drag_stopped() {
            if let Some(coid) = state.drag.take() {
                if drag_to_reprice_allowed(&inputs.caps) {
                    let new_price = key_price(row_at(pos.y), tick, state.group);
                    actions.push(DomAction::Modify { coid, new_price });
                }
            }
        }
        // click: cancel own order, else place
        if resp.clicked() && state.drag.is_none() {
            match col {
                Col::Bid | Col::Ask => {
                    let side = if col == Col::Bid { 1 } else { -1 };
                    if let Some(o) = own_order_at(key, col) {
                        actions.push(DomAction::Cancel(o.client_order_id.clone()));
                    } else {
                        let price = key_price(key, tick, state.group);
                        let stop = resolve_is_stop(
                            side,
                            price,
                            best_bid.unwrap_or(price),
                            best_ask.unwrap_or(price),
                            shift,
                        );
                        actions.push(DomAction::Place {
                            side,
                            price,
                            qty: state.qty,
                            stop,
                            reduce_only: state.reduce_only,
                        });
                    }
                }
                Col::Price => {} // price column is inert (recenter is the toolbar C button)
            }
        }
        if resp.secondary_clicked() {
            if let Some(o) = own_order_at(key, col_at(pos.x, ladder_rect.min.x, colw)) {
                actions.push(DomAction::Cancel(o.client_order_id.clone()));
            }
        }
    }
    // scroll wheel nudges the manual center (latches it)
    let scroll = ui.input(|i| i.smooth_scroll_delta.y);
    if resp.hovered() && scroll.abs() > 0.5 {
        let step = key_price(1, tick, state.group);
        let base = state.center.unwrap_or(last);
        state.center = Some(base + (scroll.signum() as f64) * step);
    }

    // --- footer: position + cancels + close/reverse ---
    footer(ui, state, inputs, &mut actions);

    actions
}

fn header(ui: &mut egui::Ui, state: &mut DomState, last: f64, stale: bool, paper: bool) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), HEADER_H), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, PANEL2);
    ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, RULE));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(8.0, 3.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 4.0;
    for m in [DomMode::Pro, DomMode::Elite] {
        let on = state.mode == m;
        let txt = egui::RichText::new(m.label()).monospace().size(12.0).color(if on {
            Color32::from_rgb(18, 16, 10)
        } else {
            MUTED
        });
        let btn = egui::Button::new(txt)
            .fill(if on { LAST } else { PANEL })
            .stroke(Stroke::new(1.0, RULE));
        if child.add(btn).clicked() {
            state.mode = m;
        }
    }
    // venue cycle button (Binance → Bybit → OKX → Aster → …)
    child.add_space(6.0);
    let vtxt = egui::RichText::new(state.venue.label()).monospace().size(11.0).color(TXT);
    if child
        .add(egui::Button::new(vtxt).fill(PANEL).stroke(Stroke::new(1.0, RULE)))
        .on_hover_text("Switch venue (Binance / Bybit / OKX / Aster)")
        .clicked()
    {
        state.venue = state.venue.next();
    }
    // trading-mode badge: PAPER (amber, safe) vs LIVE (red) — where a click's order actually goes
    child.add_space(4.0);
    let (mtxt, mcol) = if paper { ("PAPER", LAST) } else { ("● LIVE", ASK) };
    child.label(egui::RichText::new(mtxt).monospace().size(11.0).color(mcol));
    child.add_space(8.0);
    child.label(egui::RichText::new(fmt_px(last, 0.5)).monospace().size(13.0).color(LAST));
    if stale {
        child.add_space(8.0);
        child.label(egui::RichText::new("● STALE").monospace().size(11.0).color(ASK));
    }
}

fn toolbar(ui: &mut egui::Ui, state: &mut DomState) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), TOOLBAR_H), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, PANEL2);
    ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, RULE));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(8.0, 3.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 4.0;
    child.label(
        egui::RichText::new(format!("qty {}", fmt_qty(state.qty)))
            .monospace()
            .size(11.0)
            .color(TXT),
    );
    for q in QTY_PRESETS {
        let on = (state.qty - q).abs() < f64::EPSILON;
        let txt = egui::RichText::new(fmt_qty(q)).monospace().size(11.0).color(if on {
            LAST
        } else {
            MUTED
        });
        if child.add(egui::Button::new(txt).fill(PANEL).stroke(Stroke::new(1.0, RULE))).clicked() {
            state.qty = q;
        }
    }
    child.add_space(8.0);
    // grouping
    child.label(egui::RichText::new("group").monospace().size(11.0).color(FAINT));
    if child
        .add(
            egui::Button::new(egui::RichText::new("−").monospace().size(12.0).color(TXT))
                .fill(PANEL),
        )
        .clicked()
    {
        state.group = prev_group(state.group);
    }
    child.label(egui::RichText::new(state.group.to_string()).monospace().size(11.0).color(TXT));
    if child
        .add(
            egui::Button::new(egui::RichText::new("+").monospace().size(12.0).color(TXT))
                .fill(PANEL),
        )
        .clicked()
    {
        state.group = next_group(state.group);
    }
    child.add_space(8.0);
    // recenter
    if child
        .add(
            egui::Button::new(egui::RichText::new("C ⌖").monospace().size(11.0).color(TXT))
                .fill(PANEL)
                .stroke(Stroke::new(1.0, RULE)),
        )
        .on_hover_text("Recenter on last")
        .clicked()
    {
        state.center = None;
    }
    // reduce-only arm
    let ro = state.reduce_only;
    let rotxt =
        egui::RichText::new("Reduce").monospace().size(11.0).color(if ro { LAST } else { MUTED });
    if child
        .add(
            egui::Button::new(rotxt)
                .fill(if ro { LAST.linear_multiply(0.18) } else { PANEL })
                .stroke(Stroke::new(1.0, RULE)),
        )
        .clicked()
    {
        state.reduce_only = !ro;
    }
    // cost-to-fill readout arm (opt-in; default off ⇒ footer paints exactly as before)
    let c2f = state.cost_to_fill;
    let c2ftxt =
        egui::RichText::new("C2F").monospace().size(11.0).color(if c2f { LAST } else { MUTED });
    if child
        .add(
            egui::Button::new(c2ftxt)
                .fill(if c2f { LAST.linear_multiply(0.18) } else { PANEL })
                .stroke(Stroke::new(1.0, RULE)),
        )
        .on_hover_text("Cost to fill: projected avg price + slippage vs mid for the order qty")
        .clicked()
    {
        state.cost_to_fill = !c2f;
    }
}

fn footer(
    ui: &mut egui::Ui,
    state: &mut DomState,
    inputs: &DomInputs,
    actions: &mut Vec<DomAction>,
) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), FOOTER_H), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, PANEL2);
    ui.painter().line_segment([rect.left_top(), rect.right_top()], Stroke::new(1.0, RULE));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(8.0, 3.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 5.0;
    if let Some(p) = inputs.position {
        let (pcol, lbl) = if p.size > 0.0 {
            (BID, format!("+{}", fmt_qty(p.size)))
        } else if p.size < 0.0 {
            (ASK, fmt_qty(p.size))
        } else {
            (MUTED, "flat".to_string())
        };
        child.label(egui::RichText::new(format!("Pos {lbl}")).monospace().size(11.0).color(pcol));
        let plcol = if p.upnl >= 0.0 { BID } else { ASK };
        child.label(
            egui::RichText::new(format!("P/L {:+.2}", p.upnl)).monospace().size(11.0).color(plcol),
        );
    } else {
        child.label(egui::RichText::new("Pos flat").monospace().size(11.0).color(MUTED));
    }
    // OPT-IN cost-to-fill readout (toolbar "C2F"). Off ⇒ this block is skipped entirely: no
    // book walk, no labels, footer identical to before.
    if state.cost_to_fill {
        let q = state.cost_qty.unwrap_or(state.qty);
        let c = cost_to_fill(inputs.book, q);
        child.add_space(8.0);
        child.label(
            egui::RichText::new(format!("fill {}", fmt_qty(q))).monospace().size(11.0).color(FAINT),
        );
        for (label, col, side) in [("B", BID, c.buy), ("S", ASK, c.sell)] {
            let (txt, hover) = cost_label(label, q, side);
            let resp = child.label(
                egui::RichText::new(txt)
                    .monospace()
                    .size(11.0)
                    .color(if side.is_some_and(|s| s.complete) { col } else { MUTED }),
            );
            resp.on_hover_text(hover);
        }
    }
    child.with_layout(egui::Layout::right_to_left(egui::Align::Center), |child| {
        if child.add(fbtn("✕ Offers", ASK)).clicked() {
            actions.push(DomAction::CancelSide(-1));
        }
        if child.add(fbtn("✕ Bids", BID)).clicked() {
            actions.push(DomAction::CancelSide(1));
        }
        if child.add(fbtn("✕ All", MUTED)).clicked() {
            actions.push(DomAction::CancelAll);
        }
        if child.add(fbtn("Reverse", ASK)).clicked() {
            actions.push(DomAction::Reverse);
        }
        if child.add(fbtn("Close", BID)).clicked() {
            actions.push(DomAction::ClosePosition);
        }
    });
}

fn fbtn(label: &str, col: Color32) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(label.to_string()).monospace().size(11.0).color(col))
        .fill(PANEL)
        .stroke(Stroke::new(1.0, RULE))
}

/// Paint the Elite heatmap strip: newest column flush-right against the ladder, older to the
/// left. Cell colour ramps amber→hot with normalised intensity; empty cells stay panel-dark.
fn paint_heatmap(ui: &egui::Ui, rect: Rect, heat: &DomHeatmap, n_rows: usize) {
    let p = ui.painter();
    p.rect_filled(rect, 0.0, PANEL2);
    if heat.is_empty() || n_rows == 0 {
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "liquidity heatmap · time × price",
            FontId::new(10.0, FontFamily::Monospace),
            FAINT,
        );
        return;
    }
    let cell_w = (rect.width() / heat.max_cols as f32).max(1.0);
    let row_h = rect.height() / n_rows as f32;
    let n = heat.cols.len();
    for (ci, col) in heat.cols.iter().enumerate() {
        // newest (last) column at the right edge
        let x = rect.max.x - (n - ci) as f32 * cell_w;
        if x + cell_w < rect.min.x {
            continue;
        }
        for (ri, &v) in col.iter().enumerate() {
            if ri >= n_rows {
                break;
            }
            let t = (v / heat.peak).clamp(0.0, 1.0);
            if t <= 0.01 {
                continue;
            }
            let cr = Rect::from_min_size(
                egui::pos2(x, rect.min.y + ri as f32 * row_h),
                Vec2::new(cell_w + 0.5, row_h + 0.5),
            );
            p.rect_filled(cr, 0.0, heat_color(t));
        }
    }
}

/// Intensity → colour: cold (transparent-ish blue) → amber → hot white, matching the mockup.
fn heat_color(t: f32) -> Color32 {
    let r = (20.0 + t * 235.0) as u8;
    let g = (15.0 + t * 150.0) as u8;
    let b = (34.0 + t * 18.0) as u8;
    let a = (40.0 + t * 200.0).min(255.0) as u8;
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

// ---- formatting helpers ----

/// Coin-qty label with size-tiered decimals. KEPT local, not `vike_ui_theme::fmt::fmt_compact`:
/// a DOM qty needs sub-unit precision ("0.0050"), and large sizes stay plain digits ("1234",
/// never "1.23K") — pinned in the tests.
fn fmt_qty(q: f64) -> String {
    let a = q.abs();
    if a >= 1000.0 {
        format!("{:.0}", q)
    } else if a >= 1.0 {
        format!("{:.2}", q)
    } else {
        format!("{:.4}", q)
    }
}

/// Tick-aware price label: decimals follow the venue tick size. KEPT local — no shared
/// `vike_ui_theme::fmt` helper is tick-aware (`fmt_thousands_prec` takes a precision, not a tick,
/// and adds comma grouping) — pinned in the tests.
fn fmt_px(px: f64, tick: f64) -> String {
    let decimals = if tick >= 1.0 {
        0
    } else if tick >= 0.1 {
        1
    } else if tick >= 0.01 {
        2
    } else {
        4
    };
    format!("{:.*}", decimals, px)
}

fn next_group(g: i64) -> i64 {
    GROUP_STEPS.iter().copied().find(|&s| s > g).unwrap_or(g)
}
fn prev_group(g: i64) -> i64 {
    GROUP_STEPS.iter().rev().copied().find(|&s| s < g).unwrap_or(g)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_key_price_roundtrip_group1() {
        let tick = 0.5;
        // group 1: each key spans one tick; price snaps to the tick grid
        let k = row_key(62_804.0, tick, 1);
        assert_eq!(key_price(k, tick, 1), 62_804.0);
        // a price mid-tick rounds to the nearest tick key
        assert_eq!(row_key(62_804.2, tick, 1), row_key(62_804.0, tick, 1));
    }

    #[test]
    fn row_key_groups_ticks_into_buckets() {
        let tick = 1.0;
        // group 10: prices 100..109 all fall in the same bucket; 110 is the next
        let base = row_key(100.0, tick, 10);
        for p in 100..110 {
            assert_eq!(row_key(p as f64, tick, 10), base, "price {p} should share the bucket");
        }
        assert_eq!(row_key(110.0, tick, 10), base + 1);
        // the bucket's aligned edge price
        assert_eq!(key_price(base, tick, 10), 100.0);
    }

    #[test]
    fn group_book_sums_qty_into_rows() {
        let tick = 1.0;
        let levels: Vec<Level> = vec![(100.0, 1.0), (101.0, 2.0), (105.0, 4.0), (109.0, 8.0)];
        // group 10 → all four collapse into one row summing to 15
        let m = group_book(&levels, tick, 10);
        assert_eq!(m.len(), 1);
        let key = row_key(100.0, tick, 10);
        assert_eq!(m.get(&key).copied(), Some(15.0));
        // group 5 → {100,101} and {105,109} split into two rows
        let m5 = group_book(&levels, tick, 5);
        assert_eq!(m5.len(), 2);
        assert_eq!(m5.get(&row_key(100.0, tick, 5)).copied(), Some(3.0));
        assert_eq!(m5.get(&row_key(105.0, tick, 5)).copied(), Some(12.0));
    }

    #[test]
    fn resolve_is_stop_side_bound() {
        let (bid, ask) = (100.0, 101.0);
        // buy below/at the ask = limit; buy above the ask = stop
        assert!(!resolve_is_stop(1, 100.0, bid, ask, false));
        assert!(!resolve_is_stop(1, 101.0, bid, ask, false));
        assert!(resolve_is_stop(1, 102.0, bid, ask, false));
        // sell above/at the bid = limit; sell below the bid = stop
        assert!(!resolve_is_stop(-1, 101.0, bid, ask, false));
        assert!(!resolve_is_stop(-1, 100.0, bid, ask, false));
        assert!(resolve_is_stop(-1, 99.0, bid, ask, false));
    }

    #[test]
    fn resolve_is_stop_shift_forces_stop() {
        let (bid, ask) = (100.0, 101.0);
        // Shift forces a stop even where the auto rule says limit
        assert!(resolve_is_stop(1, 100.0, bid, ask, true));
        assert!(resolve_is_stop(-1, 101.0, bid, ask, true));
    }

    #[test]
    fn col_at_thirds() {
        // left=0, width=300 → [0,102)=Bid, [102,198)=Price, [198,300)=Ask
        assert_eq!(col_at(10.0, 0.0, 300.0), Col::Bid);
        assert_eq!(col_at(150.0, 0.0, 300.0), Col::Price);
        assert_eq!(col_at(280.0, 0.0, 300.0), Col::Ask);
        // clamped at the edges
        assert_eq!(col_at(-50.0, 0.0, 300.0), Col::Bid);
        assert_eq!(col_at(9999.0, 0.0, 300.0), Col::Ask);
    }

    #[test]
    fn heatmap_ring_bounds_and_peak() {
        let mut h = DomHeatmap { max_cols: 4, ..Default::default() };
        for i in 0..10 {
            h.push(vec![i as f32, (i * 2) as f32]);
        }
        // only the last 4 columns are retained
        assert_eq!(h.len(), 4);
        // peak ratchets to the global max seen (9*2 = 18)
        assert_eq!(h.peak, 18.0);
        // oldest surviving column is index 6 → [6, 12]
        assert_eq!(h.cols.front().unwrap(), &vec![6.0, 12.0]);
    }

    #[test]
    fn group_steps_navigation() {
        assert_eq!(next_group(1), 2);
        assert_eq!(next_group(5), 10);
        assert_eq!(next_group(50), 50); // clamps at the top
        assert_eq!(prev_group(10), 5);
        assert_eq!(prev_group(1), 1); // clamps at the bottom
    }

    /// Pins the rendered strings, INCLUDING the divergences that keep this formatter local
    /// instead of swapping to `vike_ui_theme::fmt::fmt_compact` (GUI audit F7 — a swap would
    /// change rendered text).
    #[test]
    fn fmt_qty_tiers_decimals_by_size() {
        // sub-unit qtys keep four decimals (the toolbar presets) — fmt_compact would print "0"
        assert_eq!(fmt_qty(0.001), "0.0010");
        assert_eq!(fmt_qty(0.05), "0.0500");
        assert_eq!(fmt_qty(0.1), "0.1000");
        // unit-scale: two decimals; signed for position sizes
        assert_eq!(fmt_qty(2.5), "2.50");
        assert_eq!(fmt_qty(-2.5), "-2.50");
        // large: plain digits, never a K/M compaction — fmt_compact would print "1.23K"
        assert_eq!(fmt_qty(1000.0), "1000");
        assert_eq!(fmt_qty(1234.0), "1234");
    }

    /// The tick-aware price formatter has no shared-fmt twin (GUI audit F7's named KEEP) — its
    /// decimal count follows the venue tick, pinned per tier.
    #[test]
    fn fmt_px_follows_tick_decimals() {
        assert_eq!(fmt_px(100.0, 1.0), "100"); // tick ≥ 1 → integer prices
        assert_eq!(fmt_px(62_804.5, 0.5), "62804.5"); // tick ≥ 0.1 → one decimal
        assert_eq!(fmt_px(62_804.25, 0.01), "62804.25"); // tick ≥ 0.01 → two decimals
        assert_eq!(fmt_px(0.62, 0.01), "0.62");
        assert_eq!(fmt_px(1.0625, 0.0001), "1.0625"); // finer → four decimals
    }

    /// The DOM's drag-to-reprice gate (audit br6): the widget offers the control ONLY when the
    /// venue's declared caps wire a native modify. A modify-less venue (or the unknown/default
    /// caps) blocks it — the exact condition that stops an unsupported `Command::Modify`.
    #[test]
    fn drag_to_reprice_gate_follows_caps() {
        // venues whose adapters wire a native amend → the drag control is offered
        assert!(drag_to_reprice_allowed(&vike_model::venue_caps::BINANCE));
        assert!(drag_to_reprice_allowed(&vike_model::venue_caps::BYBIT));
        assert!(drag_to_reprice_allowed(&vike_model::venue_caps::OKX));
        // no native modify → blocked, so the marker is greyed and no Modify is emitted
        assert!(!drag_to_reprice_allowed(&vike_model::venue_caps::OANDA));
        assert!(!drag_to_reprice_allowed(&vike_model::venue_caps::POLYMARKET));
        assert!(!drag_to_reprice_allowed(&VenueCaps::UNSUPPORTED));
    }

    // ---- cost-to-fill (opt-in footer readout) ----

    /// asks 100@1, 101@2, 102@3 ; bids 99@1, 98@2, 97@3 ⇒ mid 99.5, tick 1.0
    fn c2f_book() -> L2Book {
        let mut b = L2Book::new(1.0);
        b.apply_snapshot(
            1,
            &[(99.0, 1.0), (98.0, 2.0), (97.0, 3.0)],
            &[(100.0, 1.0), (101.0, 2.0), (102.0, 3.0)],
        );
        b
    }

    #[test]
    fn cost_to_fill_walks_both_sides() {
        let c = cost_to_fill(&c2f_book(), 3.0);
        assert_eq!(c.qty, 3.0);
        // BUY 3 = 100×1 + 101×2 = 302 / 3
        let b = c.buy.expect("asks can fill 3");
        assert!((b.avg_px - 302.0 / 3.0).abs() < 1e-12, "avg {}", b.avg_px);
        assert_eq!((b.worst_px, b.filled, b.complete, b.levels), (101.0, 3.0, true, 2));
        // slippage vs mid 99.5, positive = worse for the taker
        let bs = b.slippage_bps.unwrap();
        assert!((bs - (302.0 / 3.0 - 99.5) / 99.5 * 10_000.0).abs() < 1e-9, "bps {bs}");
        // SELL 3 = 99×1 + 98×2 = 295 / 3, also positive (below mid)
        let s = c.sell.expect("bids can fill 3");
        assert!((s.avg_px - 295.0 / 3.0).abs() < 1e-12, "avg {}", s.avg_px);
        assert_eq!((s.worst_px, s.complete, s.levels), (98.0, true, 2));
        assert!(s.slippage_bps.unwrap() > 0.0, "a sell below mid costs the taker");
    }

    #[test]
    fn cost_to_fill_flags_partial_and_empty() {
        let book = c2f_book();
        // 6 is exactly the displayed depth per side; 9 overruns it
        assert!(cost_to_fill(&book, 6.0).buy.unwrap().complete);
        let over = cost_to_fill(&book, 9.0);
        let b = over.buy.unwrap();
        assert!(!b.complete && b.filled == 6.0 && b.levels == 3, "{b:?}");
        assert!(!over.sell.unwrap().complete);
        // empty book / non-positive qty ⇒ nothing to show on either side
        let empty = cost_to_fill(&L2Book::new(1.0), 1.0);
        assert!(empty.buy.is_none() && empty.sell.is_none());
        let zero = cost_to_fill(&book, 0.0);
        assert!(zero.buy.is_none() && zero.sell.is_none());
    }

    #[test]
    fn cost_to_fill_one_sided_book_has_no_slippage() {
        let mut b = L2Book::new(1.0);
        b.apply_snapshot(1, &[], &[(100.0, 5.0)]);
        let c = cost_to_fill(&b, 2.0);
        let buy = c.buy.expect("asks fill");
        assert_eq!(buy.avg_px, 100.0);
        assert_eq!(buy.slippage_bps, None, "no mid ⇒ no slippage number, never a fabricated 0");
        assert!(c.sell.is_none(), "no bids ⇒ nothing to sell into");
    }

    /// REGRESSION (review minor): a partial walk must not read as a finite, affordable cost —
    /// the gate denies that same order as not-fillable.
    #[test]
    fn cost_label_flags_a_partial_walk_as_unbounded() {
        let book = c2f_book();
        let over = cost_to_fill(&book, 9.0);
        let (txt, hover) = cost_label("B", 9.0, over.buy);
        assert!(txt.contains('∞'), "partial walk must not show a finite bp figure: {txt}");
        assert!(!txt.contains("bp"), "no bps at all for an unbounded cost: {txt}");
        assert!(hover.contains("not-fillable"), "hover must name the gate verdict: {hover}");
        // a COMPLETE walk still shows the number, and says it is the gate's number
        let (txt, hover) = cost_label("B", 3.0, cost_to_fill(&book, 3.0).buy);
        assert!(txt.contains("bp") && !txt.contains('∞'), "{txt}");
        assert!(hover.contains("max_slippage_bps"), "{hover}");
        // empty side
        let (txt, _) = cost_label("S", 1.0, None);
        assert_eq!(txt, "S —");
    }

    /// The readout and `vike_exec::impact_veto` must judge the same number. vike-chart cannot
    /// depend on vike-exec, so this pins the shared primitive both go through.
    #[test]
    fn cost_to_fill_matches_the_gates_simulate_fill() {
        let book = c2f_book();
        for qty in [1.0, 3.0, 6.0, 9.0] {
            for side in [1, -1] {
                let sim = book.simulate_fill(side, qty);
                let c = cost_to_fill(&book, qty);
                let got = if side > 0 { c.buy } else { c.sell };
                match (got, sim.avg_px) {
                    (Some(g), Some(avg)) => {
                        assert_eq!(g.avg_px, avg, "qty {qty} side {side}");
                        assert_eq!(g.slippage_bps, sim.slippage_bps_vs_mid);
                        // this is the exact predicate `impact_veto` denies NotFillable on
                        assert_eq!(g.complete, sim.remaining == 0.0 && sim.total_filled > 0.0);
                    }
                    (None, None) => {}
                    other => panic!("readout/gate disagree at qty {qty} side {side}: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn cost_to_fill_defaults_off() {
        let s = DomState::default();
        assert!(!s.cost_to_fill, "the readout must default off (render path unchanged)");
        assert_eq!(s.cost_qty, None, "size follows the toolbar order qty by default");
    }
}
