//! The Trade window fits the window it is given — the DOM's fit sweep (its own test, deleted with
//! the DOM), carried onto the Trade window (spec §7, "Layout, run in CI").
//!
//! Every assertion reads positions off the accessibility tree of a REAL `trade::draw`, so a
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
//! 3. **Every region holds its own.** [`trade::draw`] names each region a group for a screen
//!    reader, so every control is attributed to the region that drew it, and it must lie inside
//!    that region's rect. The rects are DERIVED, not read back (pre-flight I11(f): the ladder's
//!    captions are painted, so "above the Buy caption" has no node to read): the strip is
//!    `status::height`, the bar `instrument::height` at the content width, the body
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
use vike_model::{BookLevel, L2Book, VenueCaps};
use vike_panels::trade::{
    self, AccountMode, AccountRow, Grid, LadderOrder, OrderType, Origin, Panel, Position,
    StatusKind, StatusLine, Tradable, TradeAction, TradeInputs, TradeState, instrument, layout,
    status, ticket,
};
use vike_ui_theme::appearance::Appearance;
use vike_ui_theme::components::Tokens;
use vike_ui_theme::metrics::Density;
use vike_ui_theme::type_scale::TextSize;
use vike_ui_theme::value::trade as trade_values;

/// Half a point: the tolerance for the rounding egui does to a rect's edges.
const EPS: f32 = 0.6;

/// The regions' names, as `trade::draw` gives them to a screen reader.
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

/// The three states of the ticket the sweep draws.
#[derive(Clone, Copy, Debug)]
enum Ticket {
    /// A limit at a typed price, TP/SL off, nothing held.
    Resting,
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

const TICKETS: [Ticket; 3] = [Ticket::Resting, Ticket::Held, Ticket::Bracket];

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
    /// sized to hold (`ticket::FINEST_QUICK`).
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
            Content::Swap | Content::Seven | Content::Eight => {
                Grid { tick: 0.1, lot: 0.001, min_qty: 0.001 }
            }
            Content::Tiny => Grid { tick: 1e-8, lot: 1.0, min_qty: 1.0 },
            Content::Index => Grid { tick: 25.0, lot: 1.0, min_qty: 1.0 },
            Content::Spot => Grid { tick: 0.01, lot: 0.000_01, min_qty: 0.000_01 },
        }
    }

    /// The last price as a trader types it into the price field: on the tick, no grouping.
    fn typed(self) -> String {
        format!("{:.*}", trade::sizing::decimals_of(self.grid().tick), self.last())
    }

    /// A size whose Buy and Sell labels are long, in lots of this grid.
    fn big_size(self) -> &'static str {
        if self.grid().lot < 1.0 { "1234.567" } else { "1234567" }
    }

    fn orders(self) -> usize {
        if matches!(self, Content::Seven) { 123 } else { 3 }
    }

    /// A lot multiple on the grid: twelve lots.
    fn qty(self) -> f64 {
        12.0 * self.grid().lot
    }
}

/// Three levels each side, a tick apart, round the last price.
fn book(c: Content) -> L2Book {
    let (last, tick) = (c.last(), c.grid().tick);
    let mut b = L2Book::new(tick);
    let level = |k: f64, q| BookLevel::new(last + k * tick, q);
    b.apply_snapshot(
        1,
        &[level(-1.0, 1.25), level(-2.0, 2.5), level(-3.0, 3.75)],
        &[level(1.0, 1.25), level(2.0, 2.5), level(3.0, 3.75)],
    );
    b
}

/// The working orders: limits and stops on both sides, a few ticks from the last price.
fn orders(c: Content) -> Vec<LadderOrder> {
    (0..c.orders())
        .map(|i| {
            let side = if i % 2 == 0 { 1 } else { -1 };
            let ticks = (i % 20 + 1) as f64 * -f64::from(side);
            LadderOrder {
                client_order_id: format!("o{i}"),
                side,
                price: c.last() + ticks * c.grid().tick,
                qty: c.qty(),
                is_stop: i % 5 == 0,
            }
        })
        .collect()
}

/// What a frame drew: the content rect and the tokens it was drawn with.
struct Fixture {
    state: TradeState,
    drawn: Option<(Rect, Tokens)>,
    /// The strip's height as `status::height` gives it for that frame's state and inputs: the call
    /// `draw` makes, made here so the strip is derived, not read back.
    strip_h: f32,
    /// The bar's height as `instrument::height_of` gives it for the rows `instrument::rows_for`
    /// measures for that frame's inputs: the call `draw` makes, made here.
    bar_h: f32,
}

/// The whole window under `look`, with the widest content, in the ticket state `ticket`. Built
/// ONCE per look, content and state — installing the bundled type is what a harness spends its
/// time on — and resized between widths.
fn window(look: Appearance, content: Content, ticket: Ticket) -> Harness<'static, Fixture> {
    let book = book(content);
    let orders = orders(content);
    let accounts = vec![AccountRow {
        venue: "hyperliquid",
        venue_label: VENUE_LABEL,
        product: "",
        account: Some(ACCOUNT),
        symbol: SYMBOL,
        name: ACCOUNT,
        mode: AccountMode::Live,
        why_not: None,
    }];
    let bracket = matches!(ticket, Ticket::Bracket);
    let refused = matches!(ticket, Ticket::Refused);
    let (last, grid) = (content.last(), content.grid());
    let mut h = Harness::builder().with_size(egui::vec2(900.0, 560.0)).build_ui_state(
        move |ui, f: &mut Fixture| {
            if !vike_ui_theme::harness::appearance_ready(ui.ctx(), &look) {
                return;
            }
            let inputs = TradeInputs {
                venue: "hyperliquid",
                venue_label: VENUE_LABEL,
                product: "",
                account: Some(ACCOUNT),
                symbol: SYMBOL,
                base: "BTC",
                quote: "USDT",
                mode: AccountMode::Live,
                tradable: Tradable::Yes,
                grid,
                book: &book,
                last: Some(last),
                stale: true,
                source: SOURCE,
                absence: None,
                orders: &orders,
                orders_why: None,
                position: Some(Position {
                    size: -content.qty(),
                    avg_px: last + 5.0 * grid.tick,
                    upnl: -123.45,
                }),
                buying_power: Some(123_456.78),
                caps: VenueCaps::UNSUPPORTED,
                bracket_why: match (bracket, refused) {
                    (_, true) => Some(LANE_WHY),
                    (true, false) => None,
                    (false, false) => Some(ticket::TPSL_ACCOUNT_WHY),
                },
                bracket_wire: bracket || refused,
                matches: &[],
                recent: &[],
                accounts: &accounts,
                accounts_why: None,
                unconnected: &[],
                tape: &[],
                status: (!refused).then_some(StatusLine { kind: StatusKind::Error, text: STATUS }),
            };
            f.drawn = Some((ui.available_rect_before_wrap(), Tokens::of(ui.ctx())));
            let _ = trade::draw(ui, &mut f.state, &inputs);
            let (content, t) = f.drawn.expect("just set");
            f.strip_h = status::height(ui.ctx(), &t, &f.state, &inputs, content.width());
            f.bar_h = instrument::height_of(
                instrument::rows_for(ui.ctx(), &t, &inputs, content.width()),
                &t,
            );
        },
        Fixture { state: TradeState::default(), drawn: None, strip_h: 0.0, bar_h: 0.0 },
    );
    // The first frames sync the state to the window's address, which clears the ticket; what a
    // trader typed is set after them.
    h.run();
    let s = &mut h.state_mut().state;
    s.order_type = OrderType::Limit;
    s.price = content.typed();
    match ticket {
        Ticket::Resting => {}
        Ticket::Held => {
            s.tpsl = true;
            s.held = Some(TradeAction::Place {
                side: -1,
                order_type: OrderType::Limit,
                price: Some(last),
                qty: content.qty(),
                reduce_only: false,
                exits: trade::sizing::exits(-1, last, 0.5, 0.3, grid.tick),
                origin: Origin::Ladder,
            });
        }
        Ticket::Bracket => {
            s.tpsl = true;
            s.size = content.big_size().to_string();
        }
        Ticket::Refused => s.tpsl = true,
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
/// back: the strip from `status::height`, the bar from `instrument::height`, and the ladder and the
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

fn bands(content: Rect, t: &Tokens, strip_h: f32, bar_h: f32, view: trade::View) -> Bands {
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
/// stale, so it says [`trade::ladder::HINT_STALE`]), and in the compact ticket's fixed rows the
/// position's facts and the order's value. Everything else cut short — an order label, the held
/// prompt, a reason, a price — is a violation.
fn cut_by_design(b: &Bands, text: &str, at: egui::Pos2) -> bool {
    use trade::ladder::{HINT, HINT_NO_MARKERS, HINT_STALE, HINT_STALE_NO_MARKERS};
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
            let pad = t.metrics.pad;
            if c.rect.min.x < b.ticket.min.x + pad - EPS
                || c.rect.max.x > b.ticket.max.x - pad + EPS
            {
                out.push(format!("{what} lies in the ticket's padding"));
            }
        }
        // 4. every click target keeps the control height
        if matches!(c.role, Role::Button | Role::CheckBox)
            && c.rect.height() < t.metrics.control_h - EPS
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
fn sweep(view: trade::View, height: f32, widths: RangeInclusive<u32>, content: Content) {
    let mut found: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for density in Density::ALL {
        for text_size in TextSize::ALL {
            let look = Appearance { density, text_size, ..Appearance::default() };
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

const BESIDE: trade::View = trade::View { chart: false, ladder: true, panel: Panel::Beside };
const UNDER: trade::View = trade::View { chart: false, ladder: true, panel: Panel::Under };
const ALONE: trade::View = trade::View { chart: false, ladder: false, panel: Panel::Beside };

// The ticket beside the ladder, 560 pt tall: from 260 pt — where the ladder gives the ticket the
// whole body, which is also the ticket-alone window (`layout::TICKET_ONLY_SIZE`, 280 pt) — to a
// wide window. Under the ladder, 680 pt tall, from 260 pt to well past the bottom-panel window's
// 320. The ticket alone, 560 pt tall, from 260 pt. Each in every content (spec §7); one test per
// view and content, so the runner spreads them.

#[test]
fn the_window_fits_beside_the_ladder_from_260_pt_up() {
    sweep(BESIDE, 560.0, 260..=900, Content::Swap);
}

#[test]
fn beside_the_ladder_a_seven_digit_price_and_123_orders_fit() {
    sweep(BESIDE, 560.0, 260..=900, Content::Seven);
}

#[test]
fn beside_the_ladder_an_eight_digit_price_fits() {
    sweep(BESIDE, 560.0, 260..=900, Content::Eight);
}

#[test]
fn beside_the_ladder_a_tiny_tick_fits() {
    sweep(BESIDE, 560.0, 260..=900, Content::Tiny);
}

#[test]
fn beside_the_ladder_a_large_tick_fits() {
    sweep(BESIDE, 560.0, 260..=900, Content::Index);
}

#[test]
fn the_window_fits_with_the_ticket_under_the_ladder_from_260_pt_up() {
    sweep(UNDER, 680.0, 260..=420, Content::Swap);
}

#[test]
fn under_the_ladder_a_seven_digit_price_and_123_orders_fit() {
    sweep(UNDER, 680.0, 260..=420, Content::Seven);
}

#[test]
fn under_the_ladder_an_eight_digit_price_fits() {
    sweep(UNDER, 680.0, 260..=420, Content::Eight);
}

#[test]
fn under_the_ladder_a_tiny_tick_fits() {
    sweep(UNDER, 680.0, 260..=420, Content::Tiny);
}

#[test]
fn under_the_ladder_a_large_tick_fits() {
    sweep(UNDER, 680.0, 260..=420, Content::Index);
}

#[test]
fn the_ticket_alone_fits_from_260_pt_up() {
    sweep(ALONE, 560.0, 260..=420, Content::Swap);
}

#[test]
fn the_ticket_alone_fits_a_seven_digit_price_and_123_orders() {
    sweep(ALONE, 560.0, 260..=420, Content::Seven);
}

#[test]
fn the_ticket_alone_fits_an_eight_digit_price() {
    sweep(ALONE, 560.0, 260..=420, Content::Eight);
}

#[test]
fn the_ticket_alone_fits_a_tiny_tick() {
    sweep(ALONE, 560.0, 260..=420, Content::Tiny);
}

#[test]
fn the_ticket_alone_fits_a_large_tick() {
    sweep(ALONE, 560.0, 260..=420, Content::Index);
}

/// What egui_kittest's central panel keeps each side (rule 1): a harness this much larger than a
/// body, each way, hands `draw` that body.
const HARNESS_MARGIN: f32 = 8.0;

/// The tokens the last frame drew with.
fn tokens(h: &Harness<'static, Fixture>) -> Tokens {
    h.state().drawn.expect("the window drew").1
}

/// Draw `h` as the desktop draws a window `size` large: `draw` is handed the window's body, its
/// size less what the window puts around it (`layout::chrome`). The sweeps above draw the harness
/// AS the window, which hands `draw` a rect up to 33 pt taller than the desktop does.
fn as_window(h: &mut Harness<'static, Fixture>, size: egui::Vec2) {
    let body = size - layout::chrome();
    h.set_size(body + egui::Vec2::splat(2.0 * HARNESS_MARGIN));
    h.run();
    let drawn = h.state().drawn.expect("the window drew").0.size();
    assert!((drawn - body).length() <= EPS, "the harness drew a {drawn:?} body for {body:?}");
}

/// Ruling B1 of the render check (2026-10-03): the window a trader opens shows the full ticket's
/// WHOLE form, down to TP/SL, without a scroll, in every look. The render check measured TP/SL
/// 61 pt below the fold of the 600 × 560 window at Comfortable density, Reduce only half cut, and
/// nothing on screen saying the form went on.
///
/// Drawn as the desktop draws the window ([`as_window`]), in the state a trader fills the ticket
/// from (a limit typed, TP/SL off: [`Ticket::Resting`]) and in every content, the spot lot whose
/// quick sizes are the widest among them: every control the form draws lies inside the form's
/// visible rect, so not one of them is a scroll away.
fn the_whole_form_shows_at_the_size_a_window_opens_at(view: trade::View) {
    let mut found = Vec::new();
    for density in Density::ALL {
        for text_size in TextSize::ALL {
            let look = Appearance { density, text_size, ..Appearance::default() };
            for content in CONTENTS {
                let mut h = window(look, content, Ticket::Resting);
                h.state_mut().state.view = view;
                let size = layout::window_size(view, &h.ctx);
                if matches!(content, Content::Swap) {
                    println!("{view:?} {density:?}/{text_size:?}: a window opens at {size:?}");
                }
                as_window(&mut h, size);
                let at = format!("{content:?}/{density:?}/{text_size:?} at {size:?}");
                let (cs, regions) = drawn(&h);
                let Some(form) = regions.get(FORM).copied() else {
                    found.push(format!("{at}: no order form"));
                    continue;
                };
                let mut tpsl = false;
                let mut last = form.min.y;
                for c in cs.iter().filter(|c| c.region.as_deref() == Some(FORM)) {
                    tpsl |= c.text == "TP/SL";
                    last = last.max(c.rect.max.y);
                    let below = c.rect.max.y - form.max.y;
                    if below > EPS {
                        found.push(format!("{at}: {:?} ends {below:.1} pt below the form", c.text));
                    }
                }
                // The room the form does not use (the second quick-size row a coarse lot leaves
                // empty, and the opening height's rounding up): recorded per look in spec §9.1.
                println!("{at}: {:.1} pt free under the form's last row", form.max.y - last);
                if !tpsl {
                    found.push(format!("{at}: the form draws no TP/SL"));
                }
            }
        }
    }
    assert!(found.is_empty(), "{} control(s) a scroll away:\n{}", found.len(), found.join("\n"));
}

#[test]
fn the_whole_form_shows_beside_the_ladder_at_the_size_a_window_opens_at() {
    the_whole_form_shows_at_the_size_a_window_opens_at(BESIDE);
}

#[test]
fn the_whole_form_shows_with_the_ticket_alone_at_the_size_a_window_opens_at() {
    the_whole_form_shows_at_the_size_a_window_opens_at(ALONE);
}

/// The dividers the last frame painted under the full ticket's form: horizontal lines in the
/// theme's border colour, across the whole ticket, in the gap between the form's visible bottom
/// and the rows pinned under it.
fn dividers(h: &Harness<'static, Fixture>) -> usize {
    let t = tokens(h);
    let (_, regions) = drawn(h);
    let (form, ticket) = (regions[FORM], regions[TICKET]);
    h.output()
        .shapes
        .iter()
        .filter(|c| match &c.shape {
            egui::Shape::LineSegment { points: [a, b], stroke } => {
                stroke.color == t.theme.border
                    && (a.y - b.y).abs() <= EPS
                    && a.y > form.max.y
                    && a.y < form.max.y + t.metrics.gap
                    && a.x.min(b.x) <= ticket.min.x + EPS
                    && a.x.max(b.x) >= ticket.max.x - EPS
            }
            _ => false,
        })
        .count()
}

/// Ruling B2: a form that goes on below its visible bottom SAYS so, with a divider in the gap
/// between the form and the rows pinned under it. A window the trader made shorter shows it; the
/// window as it opens, where nothing scrolls, shows none. The divider is painted, never laid out:
/// Buy starts a gap under the form with it or without it.
#[test]
fn a_form_that_scrolls_says_so_with_a_divider_and_one_that_does_not_draws_none() {
    for density in Density::ALL {
        for text_size in TextSize::ALL {
            let look = Appearance { density, text_size, ..Appearance::default() };
            let mut h = window(look, Content::Swap, Ticket::Resting);
            h.state_mut().state.view = BESIDE;
            let at = format!("{density:?}/{text_size:?}");
            for (size, want, what) in [
                (layout::window_size(BESIDE, &h.ctx), 0, "the window as it opens scrolls nothing"),
                (
                    vec2(trade_values::BESIDE_SIZE.x, 340.0),
                    1,
                    "a window made shorter scrolls its form",
                ),
            ] {
                as_window(&mut h, size);
                assert_eq!(dividers(&h), want, "{at} at {size:?}: {what}");
                let (cs, regions) = drawn(&h);
                let buy = cs.iter().find(|c| c.text.starts_with("Buy ")).expect("a Buy button");
                let gap = tokens(&h).metrics.gap;
                let under = buy.rect.min.y - regions[FORM].max.y;
                assert!((under - gap).abs() <= EPS, "{at} at {size:?}: Buy starts {under} under");
            }
        }
    }
}

/// The compact ticket's band under its cancel row (the ticket's bottom less the cancel row's), and
/// whether Buy and Sell are stacked, one to a row.
fn compact_band(h: &Harness<'static, Fixture>) -> (f32, bool) {
    let (cs, regions) = drawn(h);
    let at = |prefix: &str| button(&cs, prefix).map(|(_, rect)| rect);
    let (buy, sell) = (at("Buy ").expect("Buy"), at("Sell ").expect("Sell"));
    let cancel = at("Cancel all").expect("Cancel all");
    (regions[TICKET].max.y - cancel.max.y, (buy.min.y - sell.min.y).abs() > EPS)
}

/// Item C of the render check (2026-10-03), as the v3 design's compact ticket keeps it: the ticket's
/// height is fixed before it is drawn (`layout::under_ticket_h`) so that Buy and Sell can stack, one
/// to a row, without moving the ladder above it: a label NAMES the order and is never cut, so where
/// both do not fit side by side (a long size, a narrow window, a loose look) they stack and what is
/// under them moves down or gives up a row, inside the same ticket. A height that followed the
/// labels would move every ladder row whenever they stacked, and a quote-sized order's labels
/// change with every tick of the book. So: at the 320 × 680 window, side by side and stacked (a long
/// size), the ticket is the same rect and the cancel row, the last thing in it, lies inside it. The
/// band left under the cancel row is printed per look.
///
/// (This asserted that the band side by side was a whole row, `control_h`, while the ticket's room
/// was seven rows. The v3 ticket's rows are not equal — a position strip, tall Buy and Sell — and
/// the room it is given is `layout::under_ticket_h`'s; a band's height is not the property.)
#[test]
fn the_compact_ticket_is_one_height_whether_buy_and_sell_stack_or_not() {
    for density in Density::ALL {
        for text_size in TextSize::ALL {
            let look = Appearance { density, text_size, ..Appearance::default() };
            let at = format!("{density:?}/{text_size:?}");
            let mut h = window(look, Content::Swap, Ticket::Resting);
            h.state_mut().state.view = UNDER;
            as_window(&mut h, trade_values::UNDER_SIZE);
            let (band, stacked) = compact_band(&h);
            assert!(!stacked, "{at}: at 320 × 680 Buy and Sell sit side by side");
            assert!(band >= -EPS, "{at}: the cancel row lies inside the ticket ({band})");
            println!("{at}: side by side, the band under the cancel row is {band:.2} pt");
            let ticket = drawn(&h).1[TICKET];
            // A long size: "Buy 1234567890.123 MKT" and its Sell do not fit side by side.
            h.state_mut().state.size = "1234567890.123".to_string();
            h.run();
            let (band, stacked) = compact_band(&h);
            assert!(stacked, "{at}: labels this long stack");
            assert!(band >= -EPS, "{at}: stacked, the cancel row lies inside the ticket ({band})");
            println!("{at}: stacked, the band under the cancel row is {band:.2} pt");
            assert_eq!(drawn(&h).1[TICKET], ticket, "{at}: the ticket is the same rect stacked");
        }
    }
}

/// FW6, I3 (the review's minor 4): with nothing held and no line from the app, the strip says why
/// the ticket sends nothing, and grows for a sentence two Body lines cannot hold — a strip no other
/// scene draws, for they all carry the app's line. In every view, at Large text in every density,
/// at every point from 260 to 319 pt, with the longest refusal the ticket says (TP/SL ticked on an
/// engine whose lane cannot hold its stop-loss): the seven rules hold for the grown strip and nothing
/// in it is cut. CONTROLS: the strip says the sentence at every width, and it grew at some.
#[test]
fn the_grown_refusal_strip_fits_from_260_to_319_pt_at_large_text() {
    let refusal = ticket::tpsl_refusal(LANE_WHY);
    let mut found: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    let mut grew = false;
    for density in Density::ALL {
        let look = Appearance { density, text_size: TextSize::Standard, ..Appearance::default() };
        for (view, height) in [(BESIDE, 560.0), (UNDER, 680.0), (ALONE, 560.0)] {
            let mut h = window(look, Content::Swap, Ticket::Refused);
            h.state_mut().state.view = view;
            for w in 260..=319 {
                h.set_size(egui::vec2(w as f32, height));
                h.run();
                let mut seen = violations(&h);
                let (controls, _) = drawn(&h);
                if !controls.iter().any(|c| c.region.as_deref() == Some(STRIP) && c.text == refusal)
                {
                    seen.push("CONTROL: the strip does not say the refusal".to_string());
                }
                grew |= h.state().strip_h > least_strip_h(&h) + EPS;
                for v in seen {
                    found.entry(format!("{view:?}/{density:?}: {v}")).or_default().push(w);
                }
            }
        }
    }
    assert!(grew, "CONTROL: the refusal never grew the strip, so the sweep proved nothing");
    assert_none(found);
}

/// The strip's least height in the look the last frame drew, as `status::idle_height` (the window
/// module's own) counts it: one control row or two lines of Body text, whichever is taller, and a
/// point above and below.
fn least_strip_h(h: &Harness<'static, Fixture>) -> f32 {
    let t = tokens(h);
    let body = t.font(vike_ui_theme::type_scale::TextRole::Body);
    let row = h.ctx.fonts_mut(|f| f.row_height(&body));
    t.metrics.control_h.max(2.0 * row) + 2.0
}

/// Where the button whose name starts with `prefix` is, with its name.
fn button(cs: &[Control], prefix: &str) -> Option<(String, Rect)> {
    cs.iter()
        .find(|c| c.role == Role::Button && c.text.starts_with(prefix))
        .map(|c| (c.text.clone(), c.rect))
}

/// FW5 and FW6 together (the final merge, 2026-10-03): the window AS IT OPENS
/// (`layout::window_size`, drawn as the desktop draws it, [`as_window`]) with TP/SL then ticked and
/// refused on the account's lane ([`Ticket::Refused`]). The opening height counts the strip at its
/// least, and this is the state that grows it (by some 20 pt in the 280 pt ticket alone). In every
/// look, every content and each view: the seven rules hold, so the strip's reason is said whole;
/// where the full ticket's form goes on more than a point below what it shows, its divider says so,
/// and where it ends more than a point above, there is none (between the two egui's rounding
/// decides, so neither is asserted); Buy starts a gap under the form; and a click on the refused
/// Buy holds nothing (the account is LIVE, so an order that got through would wait in the strip)
/// and moves neither Buy nor Sell. CONTROLS: the strip says the refusal in every one, and it grew
/// in the ticket alone in some look.
#[test]
fn a_refused_tpsl_in_the_window_as_it_opens_is_said_whole_and_a_form_it_scrolls_says_so() {
    let refusal = ticket::tpsl_refusal(LANE_WHY);
    let mut found = Vec::new();
    let mut grew_alone = false;
    for density in Density::ALL {
        for text_size in TextSize::ALL {
            let look = Appearance { density, text_size, ..Appearance::default() };
            for content in CONTENTS {
                let mut h = window(look, content, Ticket::Refused);
                for view in [BESIDE, UNDER, ALONE] {
                    h.state_mut().state.view = view;
                    let size = layout::window_size(view, &h.ctx);
                    as_window(&mut h, size);
                    let at = format!("{view:?} {content:?}/{density:?}/{text_size:?} at {size:?}");
                    found.extend(violations(&h).into_iter().map(|v| format!("{at}: {v}")));
                    let grew = h.state().strip_h - least_strip_h(&h);
                    grew_alone |= view == ALONE && grew > 1.0;
                    let (cs, regions) = drawn(&h);
                    if !cs.iter().any(|c| c.region.as_deref() == Some(STRIP) && c.text == refusal) {
                        found.push(format!("{at}: CONTROL: the strip does not say the refusal"));
                    }
                    let (Some((buy, buy_at)), Some((_, sell_at))) =
                        (button(&cs, "Buy "), button(&cs, "Sell "))
                    else {
                        found.push(format!("{at}: no Buy or no Sell"));
                        continue;
                    };
                    if let Some(form) = regions.get(FORM).copied() {
                        let over = cs
                            .iter()
                            .filter(|c| c.region.as_deref() == Some(FORM))
                            .map(|c| c.rect.max.y - form.max.y)
                            .fold(f32::MIN, f32::max);
                        let shown = dividers(&h);
                        if (over > 1.0 && shown != 1) || (over < -1.0 && shown != 0) {
                            found.push(format!(
                                "{at}: the form ends {over:.1} pt past what it shows, with {shown} \
                                 divider(s)"
                            ));
                        }
                        let under = buy_at.min.y - form.max.y;
                        if (under - tokens(&h).metrics.gap).abs() > EPS {
                            found.push(format!("{at}: Buy starts {under:.1} pt under the form"));
                        }
                        println!(
                            "{at}: the strip grew {grew:.1} pt; the form ends {over:.1} pt past"
                        );
                    } else {
                        println!("{at}: the strip grew {grew:.1} pt (the compact ticket)");
                    }
                    h.get_by_label(&buy).click();
                    h.run();
                    // The pointer leaves, so no hover text is left open over the next view.
                    h.hover_at(pos2(1.0, 1.0));
                    h.run();
                    let (cs, _) = drawn(&h);
                    if h.state().state.held.is_some() {
                        found.push(format!("{at}: a click on the refused {buy:?} held an order"));
                    }
                    let moved = button(&cs, "Buy ").map(|b| b.1) != Some(buy_at)
                        || button(&cs, "Sell ").map(|s| s.1) != Some(sell_at);
                    if moved {
                        found.push(format!(
                            "{at}: a click on the refused {buy:?} moved Buy or Sell"
                        ));
                    }
                }
            }
        }
    }
    assert!(grew_alone, "CONTROL: the refusal never grew the ticket alone's strip");
    assert!(found.is_empty(), "{} finding(s):\n{}", found.len(), found.join("\n"));
}

/// The sweep's own positive control: a window that DOES spill is reported. A control is moved out
/// of its region by drawing the window into a content rect wider than the harness, so if the
/// checks above could never fail, this would pass with no violation at all.
#[test]
fn the_sweep_reports_a_window_that_spills() {
    let mut h = window(Appearance::default(), Content::Swap, Ticket::Resting);
    h.set_size(egui::vec2(600.0, 560.0));
    h.run();
    // Shift every derived band right of the window: the window still drew where it drew.
    let (content, t) = h.state().drawn.expect("the window drew");
    let moved = content.translate(vec2(content.width(), 0.0));
    h.state_mut().drawn = Some((moved, t));
    let found = violations(&h);
    assert!(
        found.iter().any(|v| v.contains("lies outside the window")),
        "a window drawn outside its content rect must be reported: {found:?}"
    );
}

/// ...and the elision rule's own positive control: a window too narrow for its strip's status line
/// on two lines cuts it, and that is reported (the rule is not an empty allow-list).
#[test]
fn the_sweep_reports_a_line_cut_short() {
    let mut h = window(Appearance::default(), Content::Swap, Ticket::Resting);
    h.set_size(egui::vec2(120.0, 560.0));
    h.run();
    let found = violations(&h);
    assert!(
        found.iter().any(|v| v.contains(&format!("{STATUS:?} is cut short"))),
        "a status line cut to two lines must be reported: {found:?}"
    );
}
