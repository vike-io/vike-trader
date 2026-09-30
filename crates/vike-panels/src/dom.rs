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
//!
//! # ⚠ THE HONESTY CONTRACT — this widget never draws a ladder it does not have
//!
//! Two rules, both taken by [`draw`] rather than by its caller, because a caller got them wrong for
//! months and nothing on screen said so:
//!
//! 1. **No book ⇒ no ladder.** A [`DomInputs::book`] with no levels on either side renders an
//!    explicit empty state ([`BookAbsence`]) and nothing ladder-shaped at all — no rows, no depth
//!    bars, no price column, no spread line, no heatmap column, and no interaction region, so a
//!    click cannot place an order at a price this widget invented. Until 2026-09-15 the app handed
//!    in a FABRICATED book instead (`vike_app_core::orders::dom_math::synth_book`: 40 gapless, perfectly
//!    symmetric levels per side, uniform-random sizes from an LCG seeded on [`DomState::tick`] —
//!    the frame counter this function increments at its top, so the ladder re-rolled EVERY FRAME).
//!    It animated like a liquid market whose prices never moved, and the only tells were a dim
//!    overlay and a `● STALE` badge.
//! 2. **No price ⇒ a dash.** `last: None` prints `—`, never `0.00` and never a seeded guess (the
//!    same caller's `default_price` answered `62 800.00` for `BTCUSDT`, in the same amber and the
//!    same slot a venue's real last price uses).
//!
//! And one statement the widget now makes unconditionally: [`DomInputs::source`], the state of the
//! DOM's OWN market-data link, on its own strip. That is a DIFFERENT SOCKET from the one the shell's
//! status bar reports — see that field's doc — so a green `OBSERVING (connected)` up there says
//! nothing about whether this window has data.
//!
//! `crates/vike-panels/tests/dom_no_book.rs` gates all three off the accessibility tree.

use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind, Vec2};
use vike_model::feed_status::{ConnectionState, parse_feed_status};
use vike_model::{BookLevel, L2Book, VenueCaps};
use vike_ui_theme::components::button::{ActionButton, IconButton};
use vike_ui_theme::components::segmented::{self, Segment};
use vike_ui_theme::components::{ON_FILL, Status, Tokens, chip, toggle};
use vike_ui_theme::icons::{self, Icon};
use vike_ui_theme::metrics::{Density, Metrics, RADIUS};
use vike_ui_theme::type_scale::{TextRole, TextSize};

/// Each strip's height and the ladder's row pitch at one density (design system spec §3.4).
///
/// A strip that holds controls is [`stack_height`]: the density's CONTROL height for each of its
/// [`Rows`], plus [`STRIP_INSET`] above and below ([`FOOTER_INSET`] for the footer), so a kit control
/// fills a row exactly. The source strip and the ladder rows are the density's ROW height. On one row
/// each, at Normal, this is 26 / 18 / 26 / 28 — the four strip heights the DOM had before the design
/// system — and only the ladder's row moves, 20 → 18.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Strips {
    header: f32,
    source: f32,
    toolbar: f32,
    footer: f32,
    row: f32,
}

/// The gap between a strip's edge and the controls in it, above and below.
const STRIP_INSET: f32 = 1.0;
/// The footer's gap: one point more, as before the design system.
const FOOTER_INSET: f32 = 2.0;
/// The gap between two rows of one strip.
const ROW_GAP: f32 = 2.0;

/// How many rows each strip that can stack is laid out on, at one width.
///
/// One row each is the DOM the design system inherited and what a wide window still gets; a row is
/// added only where the strip's controls do not fit on the ones it has, so that at the 320 pt the
/// launcher opens a DOM at nothing is cut off and nothing lies on anything else. (Until then the
/// header lost the end of STALE, the toolbar lost half its controls — Reduce and Recenter among
/// them — and the footer's buttons sat on top of the position readout: finding F1 of the panels plan,
/// deferred by its decision 7.)
///
/// ⚠ **Chosen from the WIDTH and the appearance, and from nothing the frame draws** — and that is the
/// design, not a shortcut. The rows a strip needs depend on how wide its controls are, and each
/// obvious way to learn that is wrong here. Laying the controls out once to measure them puts a
/// second, invisible copy of every control in the accessibility tree (egui registers a widget's node
/// whether or not its `Ui` is visible), so each button would be announced twice and no test could ask
/// for one by name. Asking egui to discard the pass and lay out again re-runs this whole function,
/// and a click would be applied twice: `draw` turns clicks into ORDER actions. Reading last frame's
/// measured widths lags a frame and lets the digits of a price change a strip's height, so the ladder
/// would jump when one was added. A pure function of the width has none of that: the same window is
/// laid out the same way every frame, and [`rows_for`] is tested with no egui frame at all.
///
/// ⚠ **A row is ALLOCATED, never wrapped into.** A strip's height is [`stack_height`] and the ladder
/// region starts below it, so a row a strip adds pushes the ladder down. It is not a widget that
/// wrapped past the height it was given: that one would lie over the ladder, whose click-to-trade
/// region is laid out after the strips and wins any click on it (`one_line` keeps the kit's segmented
/// control from doing that inside a row).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rows {
    header: usize,
    toolbar: usize,
    footer: FooterRows,
}

/// The footer's rows: `readouts` rows for the position and, when the toolbar's C2F is on, the cost
/// readout; then `buttons` rows for the five buttons. `buttons` is 0 when they share the readouts'
/// one row, which is the footer the DOM has always had.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FooterRows {
    readouts: usize,
    buttons: usize,
}

impl FooterRows {
    /// The rows the footer is high.
    fn total(self) -> usize {
        self.readouts + self.buttons
    }
}

// What each strip's content needs, in points: the run from a row's first control to its last, WITHOUT
// the strip's own padding, which `rows_for` adds. MEASURED 2026-09-29 on Graphite / Normal / Standard
// (the panels plan's Appendix harness). `crates/vike-panels/tests/dom_fit.rs` re-measures every strip
// at every width in every appearance, so a control that grows fails THERE instead of clipping here.

/// The header on one row: the mode switch (72.4), the venue (61.6), the mode chip (PAPER, the wider
/// one, 43.9), a price of nine characters (76: a six-figure price and one decimal) and STALE (42),
/// with a 14 pt gap between each.
const HEADER_ONE_ROW: f32 = 352.0;
/// The toolbar on one row: the size readout, the five sizes, the price grouping, Recenter and the two
/// check boxes (x 16 → 614.5).
const TOOLBAR_ONE_ROW: f32 = 598.5;
/// The toolbar's first row when it has two: the size readout and the five sizes.
const TOOLBAR_TWO_ROWS: f32 = 333.7;
// The five sizes on a row of their own: their labels (177.7 at Standard text, and they grow with the
// text), ten segment edges (a `pad` each) and the four 1 pt gaps between segments. Kept apart from the
// crude scale below because this is the one row that decides whether a FOURTH toolbar row is needed at
// 320 pt, and the padding and the text grow at different rates.
const TOOLBAR_SIZES_TEXT: f32 = 177.7;
const TOOLBAR_SIZES_EDGES: f32 = 10.0;
const TOOLBAR_SIZES_GAPS: f32 = 4.0;
/// The footer's five buttons, Close through Offers (x 0.5 → 304.0).
const FOOTER_BUTTONS: f32 = 303.5;
/// The footer's position readout: `Pos +12.3456` and `P/L −12345.67`, with a budget for the digits.
const FOOTER_POSITION: f32 = 172.0;
/// The cost readout's `fill 0.0100`.
const FOOTER_COST_FILL: f32 = 76.0;
/// The cost readout's two costs, `B 65012.34 +50.3bp` and its `S` twin, with a budget for the digits.
const FOOTER_COST_SIDES: f32 = 270.0;

/// How many rows each strip uses in a strip `width` wide, under the appearance `m` and `text` name.
fn rows_for(width: f32, m: &Metrics, text: TextSize, cost_to_fill: bool) -> Rows {
    // The constants above are Normal density at Standard text. A denser look is narrower and keeps
    // them (a row is given up a little early, never late); a looser or larger one is wider, and its
    // controls grow with the padding, the control height and the text.
    let base = Density::Normal.metrics();
    let scale = (m.control_h / base.control_h)
        .max(m.pad / base.pad)
        .max(text.px(TextRole::Body) / TextSize::Standard.px(TextRole::Body))
        .max(1.0);
    let fits = |content: f32| width >= content * scale + 2.0 * m.pad;
    let text_scale = (text.px(TextRole::Body) / TextSize::Standard.px(TextRole::Body)).max(1.0);
    let sizes = TOOLBAR_SIZES_TEXT * text_scale + TOOLBAR_SIZES_EDGES * m.pad + TOOLBAR_SIZES_GAPS;
    // Everything the footer's readouts hold on one row: the position, and the cost readout after it
    // when C2F is on. Off, they never need a row of their own to fit.
    let readouts = FOOTER_POSITION + m.pad + FOOTER_COST_FILL + m.gap + FOOTER_COST_SIDES;
    let readout_rows = if cost_to_fill && !fits(readouts) { 2 } else { 1 };
    let one_row = if cost_to_fill { readouts } else { FOOTER_POSITION };
    Rows {
        header: if fits(HEADER_ONE_ROW) { 1 } else { 2 },
        toolbar: if fits(TOOLBAR_ONE_ROW) {
            1
        } else if fits(TOOLBAR_TWO_ROWS) {
            2
        } else if width >= sizes + 2.0 * m.pad {
            3
        } else {
            4
        },
        footer: FooterRows {
            readouts: readout_rows,
            buttons: if readout_rows == 1 && fits(one_row + m.gap + FOOTER_BUTTONS) {
                0
            } else if fits(FOOTER_BUTTONS) {
                1
            } else {
                2
            },
        },
    }
}

/// The height of a strip of `rows` rows: the control height for each, [`ROW_GAP`] between them, and
/// `inset` above and below.
fn stack_height(m: &Metrics, rows: usize, inset: f32) -> f32 {
    rows as f32 * m.control_h + rows.saturating_sub(1) as f32 * ROW_GAP + 2.0 * inset
}

fn strips(m: &Metrics, rows: Rows) -> Strips {
    Strips {
        header: stack_height(m, rows.header, STRIP_INSET),
        source: m.row_h,
        toolbar: stack_height(m, rows.toolbar, STRIP_INSET),
        footer: stack_height(m, rows.footer.total(), FOOTER_INSET),
        row: m.row_h,
    }
}

/// Allocate a strip `h` high across the available width and paint its ground and its rule. Returns
/// the strip's rect.
fn strip_ground(ui: &mut egui::Ui, t: &Tokens, h: f32, rule_at_bottom: bool) -> Rect {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, t.theme.bg);
    let rule_y = if rule_at_bottom { rect.bottom() } else { rect.top() };
    ui.painter().hline(rect.x_range(), rule_y, Stroke::new(1.0, t.theme.border));
    rect
}

/// A left-to-right child `Ui` over `content`, CLIPPED to `clip` — the strip it belongs to. Nothing
/// it lays out can be drawn — or clicked: egui senses a widget over its rect intersected with its
/// clip rect — outside the strip. That matters because the ladder's click-to-trade region is laid
/// out after the strips above it, and would win any click on a control that had spilled onto it.
fn strip_child(ui: &mut egui::Ui, t: &Tokens, content: Rect, clip: Rect) -> egui::Ui {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(content)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.shrink_clip_rect(clip);
    child.spacing_mut().item_spacing.x = t.metrics.gap;
    // A widget that wraps inside a row (the sizes, on the fourth toolbar row) starts its next line one
    // row pitch down — a control height and this — which is where the strip's next row begins.
    child.spacing_mut().item_spacing.y = ROW_GAP;
    // egui's `horizontal_wrapped` (the kit's `segmented` row) opens at `interact_size.y`, not at
    // whatever height this Ui offers — so without this, a wrapped row sizes itself off egui's
    // 18 pt default instead of the density's control height and sits low in the strip (Part B's
    // B13 would set this ambient value once at the appearance level; it did not ship, so it is
    // set here instead, the same kind of DOM-owned layout wrapping `one_line` already is).
    child.spacing_mut().interact_size.y = t.metrics.control_h;
    child
}

/// One strip `h` high: its ground and its rule, and a [`strip_child`] over what is left of it inside
/// `inset` above and below.
fn strip(ui: &mut egui::Ui, t: &Tokens, h: f32, inset: f32, rule_at_bottom: bool) -> egui::Ui {
    let rect = strip_ground(ui, t, h, rule_at_bottom);
    strip_child(ui, t, rect.shrink2(Vec2::new(t.metrics.pad, inset)), rect)
}

/// One strip of `rows` rows, under ONE ground and ONE rule: a [`strip_child`] for each row, top to
/// bottom. The strip is [`stack_height`] high, allocated before any row draws, so the ladder region
/// that follows starts below every one of them. One row is [`strip`] exactly.
fn strip_rows(
    ui: &mut egui::Ui,
    t: &Tokens,
    rows: usize,
    inset: f32,
    rule_at_bottom: bool,
) -> Vec<egui::Ui> {
    let m = &t.metrics;
    let rect = strip_ground(ui, t, stack_height(m, rows, inset), rule_at_bottom);
    (0..rows)
        .map(|i| {
            let top = rect.min.y + inset + i as f32 * (m.control_h + ROW_GAP);
            let row = Rect::from_min_size(
                egui::pos2(rect.min.x, top),
                Vec2::new(rect.width(), m.control_h),
            );
            strip_child(ui, t, row.shrink2(Vec2::new(m.pad, 0.0)), rect)
        })
        .collect()
}

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
    pub const fn label(self) -> &'static str {
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

/// The WORDS the DOM shows in place of a ladder it does not have.
///
/// ⚠ **It supplies the words and never the decision.** [`draw`] decides WHETHER to render the
/// empty state, and it decides from [`DomInputs::book`] itself — no levels on either side ⇒ empty
/// state, levels ⇒ ladder. So a caller can neither hide a populated book behind an explanation nor
/// paint a ladder over a book that is not there, which is the failure this whole type exists to
/// end: until 2026-09-15 the app handed the widget a FABRICATED 40-level-per-side book seeded from
/// a price table whenever the real one was absent, re-rolled from a frame counter on every frame,
/// and the only tells were a dim overlay and a `● STALE` badge. It animated convincingly while
/// nothing behind it was real.
///
/// `cause` is deliberately the market-data session's OWN status line rather than a re-derivation of
/// it: that line is written by the thread that dials, subscribes and reads, and this widget cannot
/// know anything it does not.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct BookAbsence<'a> {
    /// What is missing, in three or four words — the glance answer.
    pub headline: &'a str,
    /// WHY, in the words of whatever knows. Empty ⇒ the line is omitted rather than guessed at.
    pub cause: &'a str,
    /// What an operator can do about it. Empty ⇒ nothing is offered.
    pub next_step: &'a str,
}

/// Everything the widget needs to render one frame. Borrowed — the app owns the storage.
pub struct DomInputs<'a> {
    pub book: &'a L2Book,
    /// last traded price (drives the accent-outlined last-price row + auto-center). `None` ⇒ this
    /// window has NO price at all, and the header says so with a dash: it must never print a
    /// plausible number that came from a table rather than from a venue.
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
    /// The status line of the DOM's OWN market-data link, verbatim from whatever owns it.
    ///
    /// ⚠ **This is a DIFFERENT SOCKET from the one the shell's status bar describes.** That strip
    /// reads `tradehub [LIVE] — OBSERVING … (connected)`, and that connection carries no book at
    /// all: neither `vike_tradehub_client::wire::WireSnapshot` nor `vike_core::CoreSnapshot` has a
    /// book, depth, quote or tape field, and the core's `Ingest::Book` arm drops the `Arc<L2Book>`
    /// it folds. Depth reaches this ladder over the datahub market-data link instead. So a green
    /// observe badge says nothing whatever about whether this window has data, and until the source
    /// strip existed there was nowhere on screen that said so. Empty ⇒ the strip reads
    /// `source unknown` rather than inventing a state.
    pub source: &'a str,
    /// WHY there is no book — rendered ONLY when [`DomInputs::book`] is genuinely empty, because
    /// [`draw`] takes that decision from the book rather than from this field. `None` ⇒ a generic
    /// line. See [`BookAbsence`].
    pub absence: Option<BookAbsence<'a>>,
}

/// Whether the DOM may begin a drag-to-reprice on an own resting order for the given venue caps —
/// the modify-gate the widget consults at the drag-start, the drop, AND the marker affordance.
/// Extracted so the gate logic is unit-testable without an egui frame (audit br6).
#[inline]
fn drag_to_reprice_allowed(caps: &VenueCaps) -> bool {
    caps.allows_modify()
}

/// A marker's width; its height is the row's less 6 pt.
const MARKER_W: f32 = 14.0;

/// How one resting-order marker is painted: `(fill, outline, letter)`.
///
/// A LIMIT order is filled in its side's colour and carries the on-fill letter. A STOP is hollow —
/// the background inside, its side's colour around it and on its letter — where it used to be the
/// trading palette's amber (owner decision 2). The outline also says whether the marker can be
/// dragged (audit br6): the text colour (a stop's: its side's colour) when this venue can reprice,
/// the caption grey when it cannot. Dragging rings either kind in the accent — the focus ring, one
/// of the accent's shapes (spec §2).
fn marker_colours(
    side: i32,
    is_stop: bool,
    draggable: bool,
    dragging: bool,
    t: &Tokens,
) -> (Color32, Stroke, Color32) {
    let (graphic, text) = if side > 0 {
        (t.market.up, t.market.up_text)
    } else {
        (t.market.down, t.market.down_text)
    };
    let (fill, edge, letter) =
        if is_stop { (t.theme.bg, graphic, text) } else { (graphic, t.theme.text, ON_FILL) };
    let edge = if draggable { edge } else { t.theme.text3 };
    let outline = if dragging { Stroke::new(1.6, t.theme.accent) } else { Stroke::new(1.3, edge) };
    (fill, outline, letter)
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
fn group_book(levels: &[BookLevel], tick: f64, group: i64) -> std::collections::HashMap<i64, f64> {
    let mut m: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();
    for &BookLevel { price: px, qty } in levels {
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
    if side > 0 { price > best_ask } else { price < best_bid }
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
    // The installed appearance, read ONCE per frame and handed to every painter below — the row
    // loop and the heatmap included, so no loop reads the context. The kit's widgets read their own.
    let t = Tokens::of(ui.ctx());
    let best_bid = inputs.book.best_bid().map(|BookLevel { price: p, .. }| p);
    let best_ask = inputs.book.best_ask().map(|BookLevel { price: p, .. }| p);
    // `last` drives the outlined last-trade row; the ladder CENTERS on the live book's mid so the
    // real depth is always in view even when the last-trade mark (a lagging kline close) sits a
    // few ticks off the top of book. Fall back to `last` when the book has no two-sided top.
    // ⚠ Kept as an `Option` all the way to the header: a window with no book AND no mark has no
    // price, and `0.00` is a number a trader can read as one.
    let last_opt = inputs.last.or_else(|| inputs.book.mid()).or(best_bid).or(best_ask);
    let last = last_opt.unwrap_or(0.0);
    let book_center = inputs.book.mid().or(best_bid).or(best_ask).unwrap_or(last);

    let full = ui.available_rect_before_wrap();
    let width = full.width();
    // How many rows each strip needs at this width — from the width and the appearance alone, so the
    // same window is laid out the same way every frame (see [`Rows`]). Every strip is allocated at
    // its full height below, before the ladder region, which takes what is left.
    let rows = rows_for(width, &t.metrics, t.text, state.cost_to_fill);
    let s = strips(&t.metrics, rows);

    // ⚠ Computed BEFORE the header, because it decides what the header may claim.
    let bookless = inputs.book.bid_levels() + inputs.book.ask_levels() == 0;

    // --- header: mode switch + venue + PAPER/LIVE + symbol/last (+ STALE badge) ---
    //
    // ⚠ `STALE` is suppressed when there is no book, and that is not tidiness. That badge means
    // "the depth in front of you is older than the freshness window" — a claim about rows a reader
    // can see. With no rows it decorates an absence, and the empty state below says the true thing
    // in words. Worse, it was HALF the disclosure the synthetic ladder had: a dim overlay and this
    // badge were the whole of what distinguished 40 invented levels from a venue's book, so leaving
    // it here would keep the vocabulary that made a fabrication look merely late.
    header(ui, &t, rows.header, state, last_opt, inputs.stale && !bookless, inputs.paper);

    // --- source strip: WHERE this ladder's depth comes from, and what that link is doing ---
    // Parsed ONCE: the strip names the link's state, and a bookless DOM picks its rendering by it.
    let link = parse_feed_status(inputs.source.trim());
    source_strip(ui, &t, &s, inputs.source, link);

    // --- toolbar: qty presets, grouping, recenter, reduce-only ---
    toolbar(ui, &t, rows.toolbar, state);

    // --- ladder / heatmap region ---
    let region_h = (full.height() - s.header - s.source - s.toolbar - s.footer).max(s.row * 5.0);
    let (region, _) = ui.allocate_exact_size(Vec2::new(width, region_h), Sense::hover());
    ui.painter().rect_filled(region, 0.0, t.theme.bg);

    // ⚠ THE NO-BOOK BRANCH, AND IT IS TAKEN FROM THE BOOK ITSELF.
    //
    // A book with no levels on either side is not a ladder, so nothing ladder-shaped is drawn: no
    // rows, no depth bars, no price column, no spread line, no heatmap column, and no interaction
    // region — a click cannot place an order at a price this widget invented. `state.heat` is left
    // untouched for the same reason the rows are: a heatmap column pushed now would put fabricated
    // liquidity into a HISTORY that survives into the real book's first frames.
    //
    // The predecessor did the opposite. `vike_app_core::orders::dom_math::synth_book` built 40 gapless,
    // perfectly symmetric levels per side with uniform-random sizes from an LCG seeded on
    // `state.tick` — the counter this function increments at its top — so the whole ladder re-rolled
    // every frame while the prices never moved. It read as a quiet, liquid market.
    if bookless {
        no_book(ui, &t, region, inputs.absence, link);
        footer(ui, &t, rows.footer, state, inputs, &mut actions);
        return actions;
    }

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

    let n_rows = (region.height() / s.row).floor().max(1.0) as usize;
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
        paint_heatmap(ui, &t, hr, &state.heat, n_rows);
    }

    // --- ladder rows ---
    let colw = ladder_rect.width();
    let painter = ui.painter().clone();
    let draggable = drag_to_reprice_allowed(&inputs.caps);
    for i in 0..n_rows {
        let key = center_key + half - i as i64;
        let price = key_price(key, tick, state.group);
        let top = ladder_rect.min.y + i as f32 * s.row;
        let row = Rect::from_min_size(egui::pos2(ladder_rect.min.x, top), Vec2::new(colw, s.row));
        let is_last = key == last_key;
        let is_pos = pos_key == Some(key);
        // The position's average-price row is the neutral hover fill; odd rows are the kit table's
        // zebra (spec §4.2). The LAST row is an accent OUTLINE, painted after its contents below —
        // the spec's "last-price outline", never a tinted fill.
        if is_pos {
            painter.rect_filled(row, 0.0, t.theme.hover);
        } else if i % 2 == 1 {
            painter.rect_filled(row, 0.0, t.theme.surface);
        }

        let bidq = bid_map.get(&key).copied().unwrap_or(0.0);
        let askq = ask_map.get(&key).copied().unwrap_or(0.0);
        let bidcol = Rect::from_min_size(row.min, Vec2::new(colw / 3.0, s.row));
        let askcol = Rect::from_min_size(
            egui::pos2(row.min.x + colw * 2.0 / 3.0, top),
            Vec2::new(colw / 3.0, s.row),
        );
        // Depth bars grow outward from the price column: the market set's depth fills, at the
        // DOM's own alphas (spec §3.2). The sizes are the set's TEXT colours.
        if bidq > 0.0 {
            let w = (bidq / vmax) as f32 * bidcol.width();
            let bar = Rect::from_min_max(
                egui::pos2(bidcol.max.x - w, top + 1.0),
                egui::pos2(bidcol.max.x, top + s.row - 1.0),
            );
            painter.rect_filled(bar, 0.0, t.market.up_depth);
            painter.text(
                egui::pos2(bidcol.max.x - 4.0, row.center().y),
                Align2::RIGHT_CENTER,
                fmt_qty(bidq),
                t.mono(TextRole::Body),
                t.market.up_text,
            );
        }
        if askq > 0.0 {
            let w = (askq / vmax) as f32 * askcol.width();
            let bar = Rect::from_min_max(
                egui::pos2(askcol.min.x, top + 1.0),
                egui::pos2(askcol.min.x + w, top + s.row - 1.0),
            );
            painter.rect_filled(bar, 0.0, t.market.down_depth);
            painter.text(
                egui::pos2(askcol.min.x + 4.0, row.center().y),
                Align2::LEFT_CENTER,
                fmt_qty(askq),
                t.mono(TextRole::Body),
                t.market.down_text,
            );
        }
        // The price column: the last price in the text colour, every other rung secondary. A
        // number is never the accent (spec §2).
        let pcol = if is_last { t.theme.text } else { t.theme.text2 };
        painter.text(
            row.center(),
            Align2::CENTER_CENTER,
            fmt_px(price, tick),
            t.mono(TextRole::Body),
            pcol,
        );

        // working-order markers for this row
        for o in inputs.orders {
            if row_key(o.price, tick, state.group) != key {
                continue;
            }
            // marker letter: "T" = stop trigger (either side); else the side letter (B/S)
            let letter = if o.is_stop {
                "T"
            } else if o.side > 0 {
                "B"
            } else {
                "S"
            };
            let x = if o.side > 0 { bidcol.min.x + 2.0 } else { askcol.max.x - 2.0 - MARKER_W };
            let mrect =
                Rect::from_min_size(egui::pos2(x, top + 3.0), Vec2::new(MARKER_W, s.row - 6.0));
            let dragging = state.drag.as_deref() == Some(o.client_order_id.as_str());
            let (fill, outline, ink) = marker_colours(o.side, o.is_stop, draggable, dragging, &t);
            let ring = if dragging { mrect.expand(1.5) } else { mrect };
            painter.rect_filled(mrect, CornerRadius::same(RADIUS), fill);
            painter.rect_stroke(ring, CornerRadius::same(RADIUS), outline, StrokeKind::Middle);
            painter.text(
                mrect.center(),
                Align2::CENTER_CENTER,
                letter,
                t.mono(TextRole::Caption),
                ink,
            );
        }
        if is_last {
            painter.rect_stroke(row, 0.0, Stroke::new(1.0, t.theme.accent), StrokeKind::Inside);
        }
    }

    // The spread line at the last/mid boundary: the analysis line, the one neutral between plus
    // and minus (spec §3.2).
    let spread_y = ladder_rect.min.y + (center_key + half - last_key) as f32 * s.row + s.row;
    if spread_y > ladder_rect.min.y && spread_y < ladder_rect.max.y {
        painter.line_segment(
            [egui::pos2(ladder_rect.min.x, spread_y), egui::pos2(ladder_rect.max.x, spread_y)],
            Stroke::new(1.0, t.theme.analysis_line),
        );
    }

    // stale overlay: dim the whole book region into the theme, so a frozen book never reads as live
    if inputs.stale {
        painter.rect_filled(region, 0.0, t.theme.scrim());
    }

    // --- interaction over the ladder ---
    let resp = ui.interact(ladder_rect, ui.id().with("dom_ladder"), Sense::click_and_drag());
    let shift = ui.input(|i| i.modifiers.shift);
    let row_at = |y: f32| -> i64 {
        let idx = ((y - ladder_rect.min.y) / s.row).floor().clamp(0.0, (n_rows - 1) as f32) as i64;
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
        if resp.drag_started()
            && drag_to_reprice_allowed(&inputs.caps)
            && let Some(o) = own_order_at(key, col)
        {
            state.drag = Some(o.client_order_id.clone());
        }
        // drop: reprice to the row under the pointer. Belt-and-braces re-check of the gate so a
        // drag left dangling by a mid-drag venue switch can never emit a Modify.
        if resp.drag_stopped()
            && let Some(coid) = state.drag.take()
            && drag_to_reprice_allowed(&inputs.caps)
        {
            let new_price = key_price(row_at(pos.y), tick, state.group);
            actions.push(DomAction::Modify { coid, new_price });
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
                Col::Price => {} // price column is inert (recenter is the toolbar's recenter button)
            }
        }
        if resp.secondary_clicked()
            && let Some(o) = own_order_at(key, col_at(pos.x, ladder_rect.min.x, colw))
        {
            actions.push(DomAction::Cancel(o.client_order_id.clone()));
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
    footer(ui, &t, rows.footer, state, inputs, &mut actions);

    actions
}

/// Lay `add` out on ONE line, however narrow the strip. The kit's segmented control wraps when its
/// row is too narrow — the Data Manager's inspector wants that — but a DOM row is one control high,
/// and a second line inside it would lie over the next row or the ladder, whose click-to-trade
/// region is laid out after the strips and wins the click. Rows come from [`Rows`], as height the
/// strip is ALLOCATED, never from a widget wrapping. What does not fit even so runs past the strip's
/// edge and is clipped there, as every other control in the strip is (`strip_child`).
fn one_line<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let at = ui.cursor().min;
    let h = ui.available_height();
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_size(at, Vec2::new(ONE_LINE_W, h)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        add,
    )
    .inner
}

/// Wider than any screen, so [`one_line`]'s row never runs out of room and nothing in it wraps.
const ONE_LINE_W: f32 = 16_384.0;

/// The two surfaces, as the kit's segmented control offers them.
const MODES: [Segment<'static, DomMode>; 2] = [
    Segment { value: DomMode::Pro, label: DomMode::Pro.label(), why: "The click-trade ladder" },
    Segment {
        value: DomMode::Elite,
        label: DomMode::Elite.label(),
        why: "The ladder beside a time × price liquidity heatmap",
    },
];

/// The venue button's hover: the venues it cycles, in order. It named four of the five until
/// 2026-09-29; `the_venue_hint_names_every_venue_the_button_cycles` holds it to `DomVenue::next`.
const VENUE_CYCLE_HINT: &str = "Switch venue (Binance → Bybit → OKX → Aster → Hyperliquid)";

/// The header on `n_rows` rows ([`Rows::header`]): the mode switch, the venue and the trading-mode
/// chip, then the price and STALE. On one row they follow one another; on two, the price and STALE
/// take the second — they are the items whose widths follow the data, so a header that has to stack
/// moves THEM and leaves the controls where they always were.
fn header(
    ui: &mut egui::Ui,
    t: &Tokens,
    n_rows: usize,
    state: &mut DomState,
    last: Option<f64>,
    stale: bool,
    paper: bool,
) {
    let mut rows = strip_rows(ui, t, n_rows, STRIP_INSET, true).into_iter();
    let mut top = rows.next().expect("a strip has at least one row");
    one_line(&mut top, |ui| segmented::segmented(ui, &mut state.mode, &MODES));
    top.add_space(t.metrics.pad);
    if top
        .add(ActionButton::secondary(state.venue.label()))
        .on_hover_text(VENUE_CYCLE_HINT)
        .clicked()
    {
        state.venue = state.venue.next();
    }
    // Where a click's order actually goes. LIVE is one of the accent's own shapes (spec §2); PAPER,
    // where nothing leaves the box, a quiet outline (owner decision 3).
    top.add_space(t.metrics.pad);
    chip::mode(&mut top, if paper { chip::Mode::Paper } else { chip::Mode::Live });
    match rows.next() {
        None => {
            top.add_space(t.metrics.pad);
            header_price(&mut top, t, last, stale);
        }
        Some(mut second) => header_price(&mut second, t, last, stale),
    }
}

/// The last price and, when the book is stale, the badge that says so.
fn header_price(ui: &mut egui::Ui, t: &Tokens, last: Option<f64>, stale: bool) {
    // ⚠ A dash, not `0.00`, and not a seeded guess. This used to print
    // `vike_app_core::orders::dom_math::default_price`'s table value (62 800.00 for BTCUSDT) whenever no
    // book and no mark had arrived — a number with no venue behind it, rendered in the same amber
    // and the same place as a real one.
    let (ptxt, pcol) = match last {
        Some(p) => (fmt_px(p, 0.5), t.theme.text),
        None => ("—".to_string(), t.theme.text3),
    };
    ui.label(egui::RichText::new(ptxt).font(t.mono(TextRole::Title)).color(pcol)).on_hover_text(
        if last.is_some() {
            "Last price for this window's venue + symbol."
        } else {
            "No price: neither this venue's book nor the backend's mark has one for this symbol."
        },
    );
    if stale {
        ui.add_space(t.metrics.pad);
        chip::badge(ui, "STALE", Status::Warning);
    }
}

/// The source strip's glance word for one parsed link state.
///
/// ⚠ **Deliberately NOT the `LIVE`/`PAPER` vocabulary the header chip uses.** That chip is about
/// where an ORDER goes; this one is about where DEPTH comes from, and the two are answered by
/// different sockets that fail independently. Reusing a word would invite reading one as the other,
/// which is the confusion this strip was added to end.
fn source_word(state: ConnectionState) -> &'static str {
    use ConnectionState as C;
    match state {
        C::Connected => "FEED UP",
        C::Connecting => "FEED DIALLING",
        C::Disconnected => "FEED DOWN",
        C::Error => "FEED FAULT",
        C::Unknown => "FEED ?",
    }
}

/// The status the word is shown in — fixed colours, the same in every theme (spec §3.2).
///
/// ⚠ `Disconnected` is the ERROR red here, where the status bar's dots
/// (`crates/vike-app-core/src/ui/status_dot.rs`'s `dot_color_for`) paint it the muted grey. Those
/// dots have no words, so "nothing live" and "not known" can only share a colour there. This strip
/// names the state in words, and a DOM whose own depth link is down is the alarm the strip exists
/// to raise (owner decision 4, 2026-09-29).
fn source_status(state: ConnectionState) -> Status {
    use ConnectionState as C;
    match state {
        C::Connected => Status::Ok,
        C::Connecting => Status::Warning,
        C::Disconnected | C::Error => Status::Error,
        C::Unknown => Status::Muted,
    }
}

/// One always-present line naming the DOM's OWN data source and what it is doing.
///
/// It is unconditional on purpose. The states worth telling apart are not "book" and "no book" —
/// a ladder can be populated and its link already gone (the rows are simply the last ones that
/// arrived), and it can be empty because nobody has asked for that symbol yet. Only a line that is
/// always there can say which.
///
/// It is its own strip rather than a header badge because the header is six items wide with STALE,
/// and does not fit ONE row at the 320 px the launcher opens a DOM at (measured 2026-09-29; it stacks
/// onto two there now, see [`Rows`]) — and the one fact a crowded-out badge would drop is the one
/// this strip exists to state.
///
/// The classifier is `vike_model::feed_status::parse_feed_status` rather than a second one written
/// here: that function is the one authority the Connections tool and the headless daemon's health
/// gate both read, and a widget-local copy is exactly how two surfaces start disagreeing about one
/// venue.
fn source_strip(ui: &mut egui::Ui, t: &Tokens, s: &Strips, source: &str, link: ConnectionState) {
    let mut child = strip(ui, t, s.source, 1.0, true);
    let trimmed = source.trim();
    child.label(egui::RichText::new("depth").font(t.font(TextRole::Caption)).color(t.theme.text3));
    chip::badge(&mut child, source_word(link), source_status(link));
    let detail =
        if trimmed.is_empty() { "source unknown".to_string() } else { trimmed.to_string() };
    child
        .label(egui::RichText::new(detail).font(t.mono(TextRole::Caption)).color(t.theme.text2))
        .on_hover_text(
            "This ladder's depth arrives on the DATAHUB market-data link. It is not the tradehub \
         observe connection the status bar reports: that wire carries orders, positions and \
         equity and no book at all, so a green OBSERVING badge says nothing about whether this \
         window has data.",
        );
}

/// The icon a bookless DOM shows above its words, from its OWN link's state — the kit's three
/// renderings (spec §4.2), all of them STILL (owner decision 5). While connecting there is none: the
/// kit draws "still asking" as a spinner, and a DOM with no book must not move
/// (`dom_no_book.rs`'s `a_bookless_dom_does_not_animate`). The words are what a screen reader
/// hears for the icon (`icons::named`).
fn absence_icon(link: ConnectionState, t: &Tokens) -> Option<(Icon, Color32, &'static str)> {
    use ConnectionState as C;
    match link {
        C::Connecting => None,
        C::Connected | C::Unknown => Some((icons::EMPTY, t.theme.text3, "empty")),
        C::Disconnected | C::Error => {
            Some((icons::UNREACHABLE, Status::Warning.color(), "unreachable"))
        }
    }
}

/// The whole of what a bookless DOM shows. No ladder, no rows, no bars, no interaction.
///
/// Rendered as real `label` widgets inside a child `Ui` rather than as `painter.text`, so every
/// line lands in the accessibility tree and `crates/vike-panels/tests/dom_a11y.rs` can assert the
/// words on a GPU-less runner. A painted string is invisible to that test, and an empty state
/// nothing can gate is one refactor away from becoming a fabricated ladder again.
fn no_book(
    ui: &mut egui::Ui,
    t: &Tokens,
    region: Rect,
    absence: Option<BookAbsence<'_>>,
    link: ConnectionState,
) {
    // An inset frame: unmistakably NOT the ladder's own painted grid.
    ui.painter().rect_stroke(
        region.shrink(6.0),
        CornerRadius::same(RADIUS),
        Stroke::new(1.0, t.theme.border),
        StrokeKind::Inside,
    );
    let a = absence.unwrap_or_default();
    let headline = if a.headline.is_empty() { NO_BOOK_HEADLINE } else { a.headline };
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(region.shrink(18.0))
            .layout(egui::Layout::top_down(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing.y = t.metrics.gap;
    child.add_space((region.height() * 0.5 - 46.0).max(0.0));
    if let Some((icon, colour, words)) = absence_icon(link, t) {
        let glyph = icon.rich().size(t.text.px(TextRole::Heading)).color(colour);
        icons::named(child.label(glyph), words);
    }
    child.label(egui::RichText::new(headline).font(t.font(TextRole::Title)).color(t.theme.text));
    child.label(
        egui::RichText::new(NO_BOOK_SUBLINE).font(t.font(TextRole::Body)).color(t.theme.text2),
    );
    if !a.cause.is_empty() {
        // the link's own status line, read character by character: mono
        child.label(egui::RichText::new(a.cause).font(t.mono(TextRole::Body)).color(t.theme.text));
    }
    if !a.next_step.is_empty() {
        child.label(
            egui::RichText::new(a.next_step).font(t.font(TextRole::Body)).color(t.theme.text3),
        );
    }
}

/// The headline a bookless DOM falls back to when the caller supplied none.
pub const NO_BOOK_HEADLINE: &str = "NO ORDER BOOK";

/// The line that states, in so many words, that nothing is being drawn — the sentence whose
/// ABSENCE let a synthetic ladder pass for a real one for as long as it did.
pub const NO_BOOK_SUBLINE: &str =
    "nothing is drawn below: no depth has been received for this venue + symbol";

/// The toolbar on `n_rows` rows ([`Rows::toolbar`]). Five groups: the size the next click trades
/// (the readout and the five sizes), the price grouping, Recenter and the two check boxes. On ONE row
/// they follow one another, as they always have; on TWO the readout and sizes take the first row and
/// the rest the second; on THREE the sizes are a row of their own, then the readout with the grouping
/// and Recenter, then the check boxes — the widest a row can be at the 320 pt the launcher opens a
/// DOM at. On FOUR the sizes are too wide for a row even alone (Large text on Comfortable density at
/// that width): they wrap onto a second line, and the row that is reserved for it stays empty. A group
/// is otherwise never split across rows.
fn toolbar(ui: &mut egui::Ui, t: &Tokens, n_rows: usize, state: &mut DomState) {
    let mut rows = strip_rows(ui, t, n_rows, STRIP_INSET, true).into_iter();
    let mut row = || rows.next().expect("a strip has as many rows as it was asked for");
    let pad = t.metrics.pad;
    match n_rows {
        1 => {
            let mut only = row();
            qty_readout(&mut only, t, state.qty);
            order_sizes(&mut only, &mut state.qty, false);
            only.add_space(pad);
            price_grouping(&mut only, t, &mut state.group);
            only.add_space(pad);
            recenter_button(&mut only, &mut state.center);
            order_flags(&mut only, &mut state.reduce_only, &mut state.cost_to_fill);
        }
        2 => {
            let (mut first, mut second) = (row(), row());
            qty_readout(&mut first, t, state.qty);
            order_sizes(&mut first, &mut state.qty, false);
            price_grouping(&mut second, t, &mut state.group);
            second.add_space(pad);
            recenter_button(&mut second, &mut state.center);
            order_flags(&mut second, &mut state.reduce_only, &mut state.cost_to_fill);
        }
        3 => {
            let (mut first, mut second, mut third) = (row(), row(), row());
            order_sizes(&mut first, &mut state.qty, false);
            qty_readout(&mut second, t, state.qty);
            second.add_space(pad);
            price_grouping(&mut second, t, &mut state.group);
            second.add_space(pad);
            recenter_button(&mut second, &mut state.center);
            order_flags(&mut third, &mut state.reduce_only, &mut state.cost_to_fill);
        }
        _ => {
            let (mut first, _wrapped_into, mut third, mut fourth) = (row(), row(), row(), row());
            order_sizes(&mut first, &mut state.qty, true);
            qty_readout(&mut third, t, state.qty);
            third.add_space(pad);
            price_grouping(&mut third, t, &mut state.group);
            third.add_space(pad);
            recenter_button(&mut third, &mut state.center);
            order_flags(&mut fourth, &mut state.reduce_only, &mut state.cost_to_fill);
        }
    }
}

/// The size the next ladder click trades, stated first: it is the one place the chosen size is
/// spelled out as a number, wherever the segmented control's chosen label lands.
fn qty_readout(ui: &mut egui::Ui, t: &Tokens, qty: f64) {
    ui.label(
        egui::RichText::new(format!("qty {}", fmt_qty(qty)))
            .font(t.mono(TextRole::Body))
            .color(t.theme.text),
    );
}

/// The five order sizes: one segmented control, chosen size a Label, the others buttons. `wrap` lets
/// the kit's control wrap onto a second line, which only the toolbar's fourth row (with a row reserved
/// under it) may ask for; everywhere else it stays on ONE line, so it never lies over a row it was
/// not given.
fn order_sizes(ui: &mut egui::Ui, qty: &mut f64, wrap: bool) {
    let sizes = QTY_PRESETS.map(fmt_qty);
    let segments: Vec<Segment<'_, f64>> = QTY_PRESETS
        .iter()
        .zip(&sizes)
        .map(|(q, label)| Segment {
            value: *q,
            label: label.as_str(),
            why: "The size of the next ladder order",
        })
        .collect();
    if wrap {
        segmented::segmented(ui, qty, &segments);
    } else {
        one_line(ui, |ui| segmented::segmented(ui, qty, &segments));
    }
}

/// The price-consolidation stepper: its caption, the value, and a button each way.
fn price_grouping(ui: &mut egui::Ui, t: &Tokens, group: &mut i64) {
    ui.label(egui::RichText::new("group").font(t.font(TextRole::Caption)).color(t.theme.text3));
    if ui.add(IconButton::new(icons::DECREASE, "Decrease price grouping")).clicked() {
        *group = prev_group(*group);
    }
    ui.label(
        egui::RichText::new(group.to_string()).font(t.mono(TextRole::Body)).color(t.theme.text),
    );
    if ui.add(IconButton::new(icons::INCREASE, "Increase price grouping")).clicked() {
        *group = next_group(*group);
    }
}

/// Un-latch the ladder's manual centre.
fn recenter_button(ui: &mut egui::Ui, center: &mut Option<f64>) {
    // No "C": no C hotkey exists, so the letter advertised nothing (finding F3).
    if ui.add(IconButton::new(icons::RECENTER, "Recenter on last")).clicked() {
        *center = None;
    }
}

/// The two check boxes: reduce-only, and the footer's opt-in cost-to-fill readout.
fn order_flags(ui: &mut egui::Ui, reduce_only: &mut bool, cost_to_fill: &mut bool) {
    toggle::checkbox(ui, reduce_only, "Reduce")
        .on_hover_text("Reduce-only: the next ladder order may only shrink the open position");
    // Cost-to-fill readout (opt-in; off ⇒ the footer paints exactly as before)
    toggle::checkbox(ui, cost_to_fill, "C2F")
        .on_hover_text("Cost to fill: projected avg price + slippage vs mid for the order qty");
}

/// The footer on [`FooterRows`]: the readouts on the left and the five buttons on the right. On ONE
/// row they share it, as they always have. With more, the readouts take the first row — and, when the
/// toolbar's C2F is on and the row cannot hold the cost readout, the second, where the two costs go
/// (the position and `fill` stay together) — and the buttons a row of their own; and where five
/// buttons are wider than the window, as at 320 pt, they split into the position's (Close, Reverse)
/// and the cancels (All, Bids, Offers). The buttons are right-aligned on every row, in the order they
/// have always had.
fn footer(
    ui: &mut egui::Ui,
    t: &Tokens,
    f: FooterRows,
    state: &mut DomState,
    inputs: &DomInputs,
    actions: &mut Vec<DomAction>,
) {
    let mut rows = strip_rows(ui, t, f.total(), FOOTER_INSET, false).into_iter();
    let mut row = || rows.next().expect("a strip has as many rows as it was asked for");
    let right_to_left = egui::Layout::right_to_left(egui::Align::Center);
    let mut first = row();
    position_readout(&mut first, t, inputs);
    // OPT-IN cost-to-fill readout (toolbar "C2F"). Off ⇒ this block is skipped entirely: no book
    // walk, no labels, footer identical to before.
    if state.cost_to_fill {
        let cost = CostReadout::of(t, state, inputs);
        first.add_space(t.metrics.pad);
        cost.fill(&mut first, t);
        if f.readouts == 1 {
            cost.sides(&mut first, t);
        } else {
            cost.sides(&mut row(), t);
        }
    }
    match f.buttons {
        0 => {
            first.with_layout(right_to_left, |ui| {
                cancel_actions(ui, actions);
                position_actions(ui, actions);
            });
        }
        1 => {
            row().with_layout(right_to_left, |ui| {
                cancel_actions(ui, actions);
                position_actions(ui, actions);
            });
        }
        _ => {
            row().with_layout(right_to_left, |ui| position_actions(ui, actions));
            row().with_layout(right_to_left, |ui| cancel_actions(ui, actions));
        }
    }
}

/// The position and its P/L.
fn position_readout(ui: &mut egui::Ui, t: &Tokens, inputs: &DomInputs) {
    let body = t.mono(TextRole::Body);
    if let Some(p) = inputs.position {
        let (pcol, lbl) = if p.size > 0.0 {
            (t.market.up_text, format!("+{}", fmt_qty(p.size)))
        } else if p.size < 0.0 {
            (t.market.down_text, fmt_qty(p.size))
        } else {
            (t.theme.text2, "flat".to_string())
        };
        ui.label(egui::RichText::new(format!("Pos {lbl}")).font(body.clone()).color(pcol));
        let plcol = if p.upnl >= 0.0 { t.market.up_text } else { t.market.down_text };
        ui.label(
            egui::RichText::new(format!("P/L {:+.2}", p.upnl)).font(body.clone()).color(plcol),
        );
    } else {
        ui.label(egui::RichText::new("Pos flat").font(body.clone()).color(t.theme.text2));
    }
}

/// What the toolbar's C2F puts in the footer: the size the estimate is for and what a market order
/// of that size would cost on each side. Worked out once, so the two halves it can be drawn in
/// ([`CostReadout::fill`], [`CostReadout::sides`]) never walk the book twice.
struct CostReadout {
    qty: f64,
    /// `(text, hover, ink)` for the buy side, then the sell side.
    sides: [(String, String, Color32); 2],
}

impl CostReadout {
    fn of(t: &Tokens, state: &DomState, inputs: &DomInputs) -> CostReadout {
        let qty = state.cost_qty.unwrap_or(state.qty);
        let c = cost_to_fill(inputs.book, qty);
        let one = |label: &str, col: Color32, cost: Option<SideCost>| {
            let (txt, hover) = cost_label(label, qty, cost);
            let ink = if cost.is_some_and(|sc| sc.complete) { col } else { t.theme.text2 };
            (txt, hover, ink)
        };
        CostReadout {
            qty,
            sides: [one("B", t.market.up_text, c.buy), one("S", t.market.down_text, c.sell)],
        }
    }

    /// `fill 0.0100`: the size the two costs are for.
    fn fill(&self, ui: &mut egui::Ui, t: &Tokens) {
        ui.label(
            egui::RichText::new(format!("fill {}", fmt_qty(self.qty)))
                .font(t.mono(TextRole::Body))
                .color(t.theme.text3),
        );
    }

    /// The buy and sell costs.
    fn sides(&self, ui: &mut egui::Ui, t: &Tokens) {
        for (txt, hover, ink) in &self.sides {
            ui.label(egui::RichText::new(txt.as_str()).font(t.mono(TextRole::Body)).color(*ink))
                .on_hover_text(hover.as_str());
        }
    }
}

/// The three cancels, added into a right-to-left row: Offers, then Bids, then All.
fn cancel_actions(ui: &mut egui::Ui, actions: &mut Vec<DomAction>) {
    if ui.add(ActionButton::secondary((icons::CANCEL, "Offers"))).clicked() {
        actions.push(DomAction::CancelSide(-1));
    }
    if ui.add(ActionButton::secondary((icons::CANCEL, "Bids"))).clicked() {
        actions.push(DomAction::CancelSide(1));
    }
    if ui.add(ActionButton::secondary((icons::CANCEL, "All"))).clicked() {
        actions.push(DomAction::CancelAll);
    }
}

/// The position's two actions, added into a right-to-left row: Reverse, then Close.
fn position_actions(ui: &mut egui::Ui, actions: &mut Vec<DomAction>) {
    if ui.add(ActionButton::secondary("Reverse")).clicked() {
        actions.push(DomAction::Reverse);
    }
    if ui.add(ActionButton::secondary("Close")).clicked() {
        actions.push(DomAction::ClosePosition);
    }
}

/// What an empty heatmap strip says, in its two halves: what it will show, then along which axes.
const HEATMAP_CAPTION: [&str; 2] = ["liquidity heatmap", "time × price"];

/// The caption of a strip with no column to paint yet, centred on it: on ONE line, the halves joined
/// by a dot, where the strip holds that, and otherwise one half above the other and clipped to the
/// strip. The strip is the left 42% of the ladder area — 127 pt at the launcher's 320 pt — and the
/// one-line caption is a little wider than that, so it used to run out of the strip on both sides,
/// over the ladder on the right (`crates/vike-panels/tests/dom_heatmap_caption.rs`).
fn paint_heatmap_caption(p: &egui::Painter, t: &Tokens, rect: Rect) {
    let font = t.font(TextRole::Caption);
    let one_line = HEATMAP_CAPTION.join(" · ");
    let room = rect.width() - 2.0 * t.metrics.pad;
    if p.layout_no_wrap(one_line.clone(), font.clone(), t.theme.text3).size().x <= room {
        p.text(rect.center(), Align2::CENTER_CENTER, one_line, font, t.theme.text3);
        return;
    }
    let p = p.with_clip_rect(rect);
    let line_h =
        p.layout_no_wrap(HEATMAP_CAPTION[0].to_string(), font.clone(), t.theme.text3).size().y;
    for (i, half) in HEATMAP_CAPTION.into_iter().enumerate() {
        let at = egui::pos2(rect.center().x, rect.center().y + (i as f32 - 0.5) * line_h);
        p.text(at, Align2::CENTER_CENTER, half, font.clone(), t.theme.text3);
    }
}

/// Paint the Elite heatmap strip: the newest column flush right against the ladder, older ones to
/// the left. A cell is the shared heat ramp (`vike_ui_theme::heat::ramp`) at its normalised
/// liquidity; an empty cell stays the background.
fn paint_heatmap(ui: &egui::Ui, t: &Tokens, rect: Rect, heat: &DomHeatmap, n_rows: usize) {
    let p = ui.painter();
    p.rect_filled(rect, 0.0, t.theme.bg);
    if heat.is_empty() || n_rows == 0 {
        paint_heatmap_caption(p, t, rect);
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
            // `level`, not `t`: `t` is the tokens now
            let level = (v / heat.peak).clamp(0.0, 1.0);
            if level <= 0.01 {
                continue;
            }
            let cr = Rect::from_min_size(
                egui::pos2(x, rect.min.y + ri as f32 * row_h),
                Vec2::new(cell_w + 0.5, row_h + 0.5),
            );
            p.rect_filled(cr, 0.0, vike_ui_theme::heat::ramp(level));
        }
    }
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

#[path = "dom_tests.rs"]
#[cfg(test)]
mod dom_tests;
