//! The Trade window fits the window it is given — the DOM's fit sweep (its own test, deleted with
//! the DOM), carried onto the Trade window (spec §7, "Layout, run in CI").
//!
//! Every assertion reads positions off the accessibility tree of a REAL `trade_window::draw`, so a
//! GPU-less runner proves it. The window is drawn at a sweep of widths from 260 pt — under the
//! ticket-alone window's 280 — up, in every density and text size, in both layouts, with the widest
//! content each region holds (a swap's thirteen-character symbol, a long venue and account, a
//! stale book, a short position at a loss, working orders, a venue's rejection on the strip), in
//! five contents (spec §7: a seven-character price on a 0.1 tick; a seven-DIGIT price with 123
//! working orders; an index-like eight-digit price; a 1e-8 tick; a 25 tick), and in three states
//! of the ticket: an order resting in it, a LIVE order held for a confirm with a ticked TP/SL the
//! account refuses, and TP/SL on with both exit prices shown and a size whose Buy and Sell labels
//! are long. At every width:
//!
//! 1. **Nothing is cut off.** Every control lies inside the content area, `x` from 8 to
//!    `width − 8` (`egui_kittest`'s central panel keeps an 8 pt margin, as a tool window's body
//!    keeps 8 pt each side).
//! 2. **Nothing lies on anything else.** No two controls' visible parts overlap.
//! 3. **Every region holds its own.** [`trade_window::draw`] names each region a group for a screen
//!    reader, so every control is attributed to the region that drew it, and it must lie inside
//!    that region's rect. The rects are DERIVED, not read back (pre-flight I11(f): the ladder's
//!    captions are painted, so "above the Buy caption" has no node to read): the strip is
//!    `status_line::height`, the bar `instrument_bar::height` at the content width, the body
//!    `layout::split` of what lies between them, as `draw` computes it, and the groups' own bounds
//!    must equal them.
//! 4. **Every click target keeps the control height** (spec §7): every button and check box is at
//!    least the density's control height tall.
//! 5. **Buy, Sell and the cancel row never scroll.** The full ticket scrolls (a loose look makes it
//!    taller than a short window), so the part of it that scrolls is a group of its own, `Order
//!    form` (`ticket::FORM_NAME`; a full ticket that names none is a violation): a control there
//!    may lie below the form's visible rect — scrolled out of view, a scroll away — but never
//!    beside it, where no vertical scroll can reach, and only its VISIBLE part is checked against
//!    the controls pinned under it. Buy, Sell and the cancel row are pinned under the form, and a
//!    sweep that found one of them in it would report it.
//! 6. **No text is cut off** (spec §7): a galley the kit elided is a violation, but for the lines
//!    the window cuts by design with their whole text on hover (`cut_by_design`: the ladder's hint,
//!    and the compact ticket's position facts and order value). An order label, the held prompt, a
//!    reason or a price cut short is a finding.
//! 7. **The ticket keeps its padding.** The v3 design's ticket has the density's pad round its
//!    controls; a control that outgrew its row lands in that pad, which is inside the ticket's
//!    region and so rule 3's blind spot (a padlock 10 pt past its row at Comfortable density).
//!
//! Each sweep collects EVERY violation and reports them together, grouped by kind with the widths
//! each broke at, so one run says where a threshold is wrong and by how much. A violation is a
//! layout finding, not a flake: fix the widget, never widen the tolerance.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use egui::accesskit::Role;
use egui::{Rect, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use vike_marketdata::{L2Book, L2BookLevel};
use vike_model::feed_status::{DataClientConnectionState, DataClientStatus};
use vike_model::{AccountTier, Grid};
use vike_order_entry::trade_window::{
    self, TradeWindowAccountPickerRow, TradeWindowAction, TradeWindowInputs,
    TradeWindowLadderOrder, TradeWindowOrigin, TradeWindowPosition, TradeWindowState,
    TradeWindowStatusLine, TradeWindowStatusLineKind, TradeWindowTicketKind,
    TradeWindowTicketPlace, TradeWindowTradable, instrument_bar, layout, status_line, ticket,
};
use vike_ui_theme::appearance::ThemeAppearance;
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::metrics::ThemeDensity;
use vike_ui_theme::type_scale::ThemeTextSize;
use vike_ui_theme::value::trade;
use vike_venues::VenueCapabilities;

/// Half a point: the tolerance for the rounding egui does to a rect's edges.
const EPS: f32 = 0.6;

/// The regions' names, as `trade_window::draw` gives them to a screen reader.
const BAR: &str = "Instrument bar";
const LADDER: &str = "Ladder";
const TICKET: &str = "Order ticket";
const STRIP: &str = "Status strip";
/// The full ticket's scrolling part, inside the ticket: the ticket's own name for it, so a rename
/// cannot silently turn rule 5 off.
const FORM: &str = ticket::FORM_NAME;
const REGIONS: [&str; 5] = [BAR, LADDER, TICKET, STRIP, FORM];

/// The widest content each region holds: a swap's symbol, a long venue and account.
const SYMBOL: &str = "BTC-USDT-SWAP";
const VENUE_LABEL: &str = "Hyperliquid";
const ACCOUNT: &str = "sub-account-2";
const STATUS: &str = "Rejected by Binance: post-only order would cross the book.";
/// The app's words for TP/SL on an engine whose lane cannot hold the stop-loss (test words: this
/// crate cannot see the glue's), the longest cause the ticket's refusal says.
const LANE_WHY: &str = "TP/SL is not available on this account yet: its engine trades the spot \
                        market, where the exchange cannot hold the stop-loss.";
const SOURCE: &str = "datahub 127.0.0.1:7878 — 1/1 stream(s) live";

/// One control's role, accessible name, rect, and the region that drew it.
#[derive(Debug)]
struct Control {
    role: Role,
    text: String,
    rect: Rect,
    region: Option<String>,
}

/// The states of the ticket the sweep draws.
#[derive(Clone, Copy, Debug)]
enum Ticket {
    /// A limit at a typed price, TP/SL off, nothing held.
    Resting,
    /// The same limit with Post only PRESSED, on a venue and a node that offer it: its check makes the
    /// toggles row the widest the ticket draws, so it is swept at every width (the opening-size tests
    /// draw [`Ticket::Resting`], a window as it opens, with nothing ticked).
    Maker,
    /// A LIVE sell held for a confirm with both exits (the longest prompt), and TP/SL ticked on an
    /// account that refuses it (the refusal written under the toggle).
    Held,
    /// TP/SL on where it is allowed, with both exit prices shown, and a size whose Buy and Sell
    /// labels are long.
    Bracket,
    /// TP/SL ticked where the account's engine cannot carry it, nothing held and no line from the
    /// app: the strip says the ticket's refusal, the longest sentence it holds, and grows for it
    /// in a narrow window (FW6, I3). Drawn by its own tests, not with [`TICKETS`].
    Refused,
}

const TICKETS: [Ticket; 4] = [Ticket::Resting, Ticket::Maker, Ticket::Held, Ticket::Bracket];

/// What the window trades (spec §7: "tick sizes from very small to very large, and prices with
/// seven digits before the decimal point"): its last price, its grid, and how many orders it
/// works.
#[derive(Clone, Copy, Debug)]
enum Content {
    /// A seven-character price on a 0.1 tick, `65,432.1`, and three working orders.
    Swap,
    /// A seven-DIGIT price, `1,234,567.8`, and 123 working orders (a three-digit count).
    Seven,
    /// An index-like eight-digit price, `12,345,678.9`.
    Eight,
    /// A very small tick and price: `0.00001234` on a 1e-8 tick, a whole-unit lot.
    Tiny,
    /// A very large tick: `43,275` on a 25 tick.
    Index,
    /// A spot market's grid, binance's BTCUSDT: `65,432.10` on a 0.01 tick and a lot of five
    /// decimals, whose quick sizes (`0.00001` to `0.00100`) are the widest the default window is
    /// sized to hold (`ticket::full::FINEST_QUICK`).
    Spot,
}

/// Every content, for a check that is not a width sweep.
const CONTENTS: [Content; 6] =
    [Content::Swap, Content::Seven, Content::Eight, Content::Tiny, Content::Index, Content::Spot];

impl Content {
    fn last(self) -> f64 {
        match self {
            Content::Swap => 65_432.1,
            Content::Seven => 1_234_567.8,
            Content::Eight => 12_345_678.9,
            Content::Tiny => 0.000_012_34,
            Content::Index => 43_275.0,
            Content::Spot => 65_432.1,
        }
    }

    fn grid(self) -> Grid {
        match self {
            Content::Swap | Content::Seven | Content::Eight => Grid {
                tick_size: Some(0.1),
                lot_size: Some(0.001),
                min_notional: None,
                min_qty: Some(0.001),
            },
            Content::Tiny => Grid {
                tick_size: Some(1e-8),
                lot_size: Some(1.0),
                min_notional: None,
                min_qty: Some(1.0),
            },
            Content::Index => Grid {
                tick_size: Some(25.0),
                lot_size: Some(1.0),
                min_notional: None,
                min_qty: Some(1.0),
            },
            Content::Spot => Grid {
                tick_size: Some(0.01),
                lot_size: Some(0.000_01),
                min_notional: None,
                min_qty: Some(0.000_01),
            },
        }
    }

    /// The grid's tick and lot, which every content's grid gives.
    fn tick(self) -> f64 {
        self.grid().tick_size.expect("every content has a tick")
    }
    fn lot(self) -> f64 {
        self.grid().lot_size.expect("every content has a lot")
    }

    /// The last price as a trader types it into the price field: on the tick, no grouping.
    fn typed(self) -> String {
        format!("{:.*}", trade_window::sizing::decimals_of(self.tick()), self.last())
    }

    /// A size whose Buy and Sell labels are long, in lots of this grid.
    fn big_size(self) -> &'static str {
        if self.lot() < 1.0 { "1234.567" } else { "1234567" }
    }

    fn orders(self) -> usize {
        if matches!(self, Content::Seven) { 123 } else { 3 }
    }

    /// A lot multiple on the grid: twelve lots.
    fn quantity(self) -> f64 {
        12.0 * self.lot()
    }
}

/// Three levels each side, a tick apart, round the last price.
fn book(c: Content) -> L2Book {
    let (last, tick) = (c.last(), c.tick());
    let mut b = L2Book::new(tick);
    let level = |k: f64, q| L2BookLevel::new(last + k * tick, q);
    b.apply_snapshot(
        1,
        &[level(-1.0, 1.25), level(-2.0, 2.5), level(-3.0, 3.75)],
        &[level(1.0, 1.25), level(2.0, 2.5), level(3.0, 3.75)],
    );
    b
}

/// The working orders: limits and stops on both sides, a few ticks from the last price.
fn orders(c: Content) -> Vec<TradeWindowLadderOrder> {
    (0..c.orders())
        .map(|i| {
            let side = if i % 2 == 0 { 1 } else { -1 };
            let ticks = (i % 20 + 1) as f64 * -f64::from(side);
            TradeWindowLadderOrder {
                order_client_id: format!("o{i}"),
                side,
                price: c.last() + ticks * c.tick(),
                quantity: c.quantity(),
                is_stop: i % 5 == 0,
                words: None,
            }
        })
        .collect()
}

/// What a frame drew: the content rect and the tokens it was drawn with.
struct Fixture {
    state: TradeWindowState,
    drawn: Option<(Rect, ThemeTokens)>,
    /// The strip's height as `status_line::height` gives it for that frame's state and inputs: the call
    /// `draw` makes, made here so the strip is derived, not read back.
    strip_h: f32,
    /// The bar's height as `instrument_bar::height_of` gives it for the rows `instrument_bar::rows_for`
    /// measures for that frame's inputs: the call `draw` makes, made here.
    bar_h: f32,
}

/// The whole window under `look`, with the widest content, in the ticket state `ticket`. Built
/// ONCE per look, content and state — installing the bundled type is what a harness spends its
/// time on — and resized between widths.
fn window(look: ThemeAppearance, content: Content, ticket: Ticket) -> Harness<'static, Fixture> {
    let book = book(content);
    let orders = orders(content);
    let source = DataClientStatus::new(DataClientConnectionState::Connected, SOURCE);
    let accounts = vec![TradeWindowAccountPickerRow {
        venue: "hyperliquid",
        venue_label: VENUE_LABEL,
        product: "",
        account: Some(ACCOUNT),
        symbol: SYMBOL,
        name: ACCOUNT,
        mode: Some(AccountTier::Live),
        why_not: None,
        lost: None,
    }];
    let bracket = matches!(ticket, Ticket::Bracket);
    let refused = matches!(ticket, Ticket::Refused);
    let (last, grid) = (content.last(), content.grid());
    let mut h = Harness::builder().with_size(egui::vec2(900.0, 560.0)).build_ui_state(
        move |ui, f: &mut Fixture| {
            if !vike_ui_theme::harness::appearance_ready(ui.ctx(), &look) {
                return;
            }
            let inputs = TradeWindowInputs {
                venue: "hyperliquid",
                venue_label: VENUE_LABEL,
                product: "",
                account: Some(ACCOUNT),
                symbol: SYMBOL,
                base: "BTC",
                quote: "USDT",
                mode: Some(AccountTier::Live),
                tradable: TradeWindowTradable::Yes,
                grid,
                book: &book,
                last: Some(last),
                stale: true,
                source: Some(&source),
                absence: None,
                top: None,
                orders: &orders,
                orders_why: None,
                position: Some(TradeWindowPosition {
                    size: -content.quantity(),
                    average_price: last + 5.0 * grid.tick_size.expect("every content has a tick"),
                    unrealized_pnl: -123.45,
                }),
                buying_power: Some(123_456.78),
                value: vike_model::ContractValue::default(),
                // A venue with a post-only order, on a node that carries it: the Post only toggle is
                // a live one (pressed in [`Ticket::Maker`], the widest it draws), not the greyed one.
                capabilities: VenueCapabilities {
                    supports_post_only: true,
                    ..VenueCapabilities::UNSUPPORTED
                },
                bracket_why: match (bracket, refused) {
                    (_, true) => Some(LANE_WHY),
                    (true, false) => None,
                    (false, false) => Some(ticket::TAKE_PROFIT_STOP_LOSS_ACCOUNT_WHY),
                },
                bracket_wire: bracket || refused,
                post_only_wire: true,
                matches: &[],
                recent: &[],
                accounts: &accounts,
                accounts_why: None,
                unconnected: &[],
                tape: &[],
                // An Info line keeps to its two lines, which the cut-short control below reads; an
                // error takes as many as it needs (`trade_actions`' `a_long_refusal_on_the_strip_is_never_cut`).
                status: (!refused).then_some(TradeWindowStatusLine {
                    kind: TradeWindowStatusLineKind::Info,
                    text: STATUS,
                }),
                held_says: None,
            };
            f.drawn = Some((ui.available_rect_before_wrap(), ThemeTokens::of(ui.ctx())));
            let _ = trade_window::draw(ui, &mut f.state, &inputs);
            let (content, t) = f.drawn.expect("just set");
            f.strip_h = status_line::height(ui.ctx(), &t, &f.state, &inputs, content.width());
            f.bar_h = instrument_bar::height_of(
                instrument_bar::rows_for(ui.ctx(), &t, &inputs, content.width()),
                &t,
            );
        },
        Fixture { state: TradeWindowState::default(), drawn: None, strip_h: 0.0, bar_h: 0.0 },
    );
    // The first frames sync the state to the window's address, which clears the ticket; what a
    // trader typed is set after them.
    h.run();
    let s = &mut h.state_mut().state;
    s.order_type = TradeWindowTicketKind::Limit;
    s.price = content.typed();
    match ticket {
        Ticket::Resting => {}
        Ticket::Maker => s.post_only = true,
        Ticket::Held => {
            s.take_profit_stop_loss = true;
            s.held = Some(TradeWindowAction::Place {
                side: -1,
                order_type: TradeWindowTicketKind::Limit,
                price: Some(last),
                quantity: content.quantity(),
                reduce_only: false,
                post_only: false,
                exits: trade_window::sizing::exits(-1, last, 0.5, 0.3, grid.tick_size),
                origin: TradeWindowOrigin::Ladder,
            });
        }
        Ticket::Bracket => {
            s.take_profit_stop_loss = true;
            s.size = content.big_size().to_string();
        }
        Ticket::Refused => s.take_profit_stop_loss = true,
    }
    h
}

/// The region a node was drawn in: its nearest ancestor named as one.
fn region_of(node: &egui_kittest::Node<'_>) -> Option<String> {
    let mut at = node.parent();
    while let Some(n) = at {
        let a = n.accesskit_node();
        if a.role() == Role::Group
            && let Some(name) = a.label().filter(|l| REGIONS.contains(&l.as_str()))
        {
            return Some(name);
        }
        at = n.parent();
    }
    None
}

/// Every button, check box, label and text field the last frame drew, and every named region's
/// rect, as the groups report them.
fn drawn(h: &Harness<'static, Fixture>) -> (Vec<Control>, BTreeMap<String, Rect>) {
    let mut regions = BTreeMap::new();
    let mut controls = Vec::new();
    for n in h.root().children_recursive() {
        let a = n.accesskit_node();
        let role = a.role();
        if role == Role::Group
            && let Some(name) = a.label().filter(|l| REGIONS.contains(&l.as_str()))
        {
            regions.insert(name, n.rect());
            continue;
        }
        if !matches!(role, Role::Button | Role::CheckBox | Role::Label | Role::TextInput) {
            continue;
        }
        let text: String = a.label().or_else(|| a.value()).unwrap_or_default();
        let text = if text.is_empty() { format!("<{role:?}>") } else { text };
        controls.push(Control { role, text, rect: n.rect(), region: region_of(&n) });
    }
    (controls, regions)
}

/// Every text the last frame painted CUT SHORT (`galley.elided`: the kit's truncation), with
/// where it was painted.
fn elided(h: &Harness<'static, Fixture>) -> Vec<(String, egui::Pos2)> {
    fn walk(s: &egui::Shape, out: &mut Vec<(String, egui::Pos2)>) {
        match s {
            egui::Shape::Text(t) if t.galley.elided => {
                out.push((t.galley.text().to_string(), t.visual_bounding_rect().center()));
            }
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for c in &h.output().shapes {
        walk(&c.shape, &mut out);
    }
    out
}

/// The regions as `draw` derives them, every one from the functions `draw` calls and none read
/// back: the strip from `status_line::height`, the bar from `instrument_bar::height`, and the ladder and the
/// ticket from `layout::split` of what lies between them.
struct Bands {
    content: Rect,
    bar: Rect,
    strip: Rect,
    ladder: Option<Rect>,
    ticket: Rect,
    /// Whether the ticket is the compact one (`layout::compact_ticket`).
    compact: bool,
}

fn bands(
    content: Rect,
    t: &ThemeTokens,
    strip_h: f32,
    bar_h: f32,
    view: trade_window::TradeWindowPanes,
) -> Bands {
    let bar = Rect::from_min_size(content.min, vec2(content.width(), bar_h));
    let strip_top = (content.max.y - strip_h).max(bar.max.y);
    let strip = Rect::from_min_max(pos2(content.min.x, strip_top), content.max);
    let body = Rect::from_min_max(pos2(content.min.x, bar.max.y), pos2(content.max.x, strip.min.y));
    let rects = layout::split(body, view, &t.metrics, layout::ticket_w(t));
    let compact = layout::compact_ticket(view, &rects);
    Bands { content, bar, strip, ladder: rects.ladder, ticket: rects.ticket, compact }
}

/// Whether `c` is one of the controls the full ticket PINS under its form: Buy, Sell, and the cancel
/// row with its padlock (W3 review hand-off 1).
fn is_pinned(c: &Control) -> bool {
    let t = c.text.as_str();
    c.role == Role::Button
        && (t.starts_with("Buy ")
            || t.starts_with("Sell ")
            || t.starts_with("Cancel all")
            || t == "Bids"
            || t == "Asks"
            || t.starts_with("One-click trading"))
}

/// Whether the text `text`, painted cut short at `at`, is one the window cuts BY DESIGN, its whole
/// text on hover: the ladder's hint line (in whichever of its words it says: the sweep's book is
/// stale, so it says [`trade_window::ladder::HINT_STALE`]), and in the compact ticket's fixed rows the
/// position's facts and the order's value. Everything else cut short — an order label, the held
/// prompt, a reason, a price — is a violation.
fn cut_by_design(b: &Bands, text: &str, at: egui::Pos2) -> bool {
    use trade_window::ladder::{HINT, HINT_NO_MARKERS, HINT_STALE, HINT_STALE_NO_MARKERS};
    let hint = [HINT, HINT_NO_MARKERS, HINT_STALE, HINT_STALE_NO_MARKERS].contains(&text);
    let compact_line = ["SHORT", "LONG", "FLAT", "≈ "].iter().any(|p| text.starts_with(p));
    (hint && b.ladder.is_some_and(|l| l.contains(at)))
        || (compact_line && b.compact && b.ticket.contains(at))
}

/// Every way the seven properties fail at one width, worded without the width so the same failure
/// at neighbouring widths is ONE kind.
fn violations(h: &Harness<'static, Fixture>) -> Vec<String> {
    let mut out = Vec::new();
    let (content, t) = h.state().drawn.expect("the window drew");
    let view = h.state().state.view;
    let (cs, regions) = drawn(h);
    let b = bands(content, &t, h.state().strip_h, h.state().bar_h, view);
    // The groups ARE the derived rects: `draw` lays the regions out with these functions.
    let derived =
        [(BAR, Some(b.bar)), (TICKET, Some(b.ticket)), (LADDER, b.ladder), (STRIP, Some(b.strip))];
    for (name, want) in derived {
        let got = regions.get(name).copied();
        let same = match (got, want) {
            (Some(g), Some(w)) => g.min.distance(w.min) <= EPS && g.max.distance(w.max) <= EPS,
            (None, None) => true,
            _ => false,
        };
        if !same {
            out.push(format!("region {name:?} is at {got:?}, derived {want:?}"));
        }
    }
    let form = regions.get(FORM).copied();
    match form {
        // A full ticket that named no form would turn rule 5 off without a word.
        None if !b.compact => out.push("the full ticket names no order form".to_string()),
        Some(f) if !b.ticket.expand(EPS).contains_rect(f) => {
            out.push("the order form lies outside the ticket".to_string());
        }
        _ => {}
    }
    // A control's visible part: a form control is clipped to the form, which it may scroll past.
    let visible = |c: &Control| match (c.region.as_deref(), form) {
        (Some(FORM), Some(f)) => c.rect.intersect(f),
        _ => c.rect,
    };
    for c in &cs {
        let what = format!("{:?} ({:?})", c.text, c.role);
        // 1. nothing is cut off
        if c.rect.min.x < b.content.min.x - EPS || c.rect.max.x > b.content.max.x + EPS {
            out.push(format!("{what} lies outside the window"));
        }
        // 3. every region holds its own
        let inside = |r: Rect| r.expand(EPS).contains_rect(c.rect);
        let held = match c.region.as_deref() {
            Some(BAR) => inside(b.bar),
            Some(LADDER) => b.ladder.is_some_and(inside),
            Some(TICKET) => inside(b.ticket),
            Some(STRIP) => inside(b.strip),
            Some(FORM) => form.is_some_and(|f| {
                c.rect.min.x >= f.min.x - EPS
                    && c.rect.max.x <= f.max.x + EPS
                    && c.rect.min.y >= f.min.y - EPS
            }),
            _ => {
                out.push(format!("{what} lies in no named region"));
                true
            }
        };
        if !held {
            out.push(format!("{what} lies outside its region {:?}", c.region));
        }
        // 7. the ticket keeps its padding
        if matches!(c.region.as_deref(), Some(TICKET | FORM)) {
            let pad = t.metrics.padding;
            if c.rect.min.x < b.ticket.min.x + pad - EPS
                || c.rect.max.x > b.ticket.max.x - pad + EPS
            {
                out.push(format!("{what} lies in the ticket's padding"));
            }
        }
        // 4. every click target keeps the control height
        if matches!(c.role, Role::Button | Role::CheckBox)
            && c.rect.height() < t.metrics.control_height - EPS
        {
            out.push(format!("{what} is {:.1} pt tall", c.rect.height()));
        }
        // 5. Buy, Sell and the cancel row are pinned: never in the form, where they would scroll
        if is_pinned(c) && c.region.as_deref() == Some(FORM) {
            out.push(format!("{what} scrolls with the form"));
        }
    }
    // 2. nothing lies on anything else
    for (i, a) in cs.iter().enumerate() {
        for c in &cs[i + 1..] {
            let both = visible(a).intersect(visible(c));
            if both.width() > EPS && both.height() > EPS {
                out.push(format!("{:?} lies over {:?}", a.text, c.text));
            }
        }
    }
    // 6. no text is cut off (spec §7), but for the lines cut by design
    for (text, at) in elided(h) {
        if !cut_by_design(&b, &text, at) {
            out.push(format!("{text:?} is cut short"));
        }
    }
    out
}

/// Fails, if `all` is not empty, with the violations grouped by kind: the widths each broke at.
fn assert_none(all: BTreeMap<String, Vec<u32>>) {
    let lines: Vec<String> =
        all.iter().take(150).map(|(what, widths)| format!("{what} at {widths:?}")).collect();
    assert!(all.is_empty(), "{} kind(s) of violation:\n{}", all.len(), lines.join("\n"));
}

/// One sweep: every look, every ticket state, `content`, the view `view` at `height`, every width
/// in `widths`.
fn sweep(
    view: trade_window::TradeWindowPanes,
    height: f32,
    widths: RangeInclusive<u32>,
    content: Content,
) {
    let mut found: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for density in ThemeDensity::ALL {
        for text_size in ThemeTextSize::ALL {
            let look = ThemeAppearance { density, text_size, ..ThemeAppearance::default() };
            for ticket in TICKETS {
                let mut h = window(look, content, ticket);
                h.state_mut().state.view = view;
                for w in widths.clone().step_by(10) {
                    h.set_size(egui::vec2(w as f32, height));
                    h.run();
                    for v in violations(&h) {
                        let key = format!("{content:?}/{density:?}/{text_size:?}/{ticket:?}: {v}");
                        found.entry(key).or_default().push(w);
                    }
                }
            }
        }
    }
    assert_none(found);
}

const BESIDE: trade_window::TradeWindowPanes = trade_window::TradeWindowPanes {
    chart: false,
    ladder: true,
    panel: TradeWindowTicketPlace::Beside,
};
const UNDER: trade_window::TradeWindowPanes = trade_window::TradeWindowPanes {
    chart: false,
    ladder: true,
    panel: TradeWindowTicketPlace::Under,
};
const ALONE: trade_window::TradeWindowPanes = trade_window::TradeWindowPanes {
    chart: false,
    ladder: false,
    panel: TradeWindowTicketPlace::Beside,
};

#[path = "trade_fit/opening.rs"]
mod opening;
#[path = "trade_fit/sweeps.rs"]
mod sweeps;
