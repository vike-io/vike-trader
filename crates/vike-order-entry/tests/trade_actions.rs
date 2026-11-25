//! What the Trade window's clicks produce: accessibility-tree tests over the real `trade_window::draw`,
//! on the CPU like the DOM's before them (spec §§3.4–3.11).
//!
//! Every on-screen test draws the WHOLE window (the instrument bar, the ladder, the ticket and the
//! status strip) into a kittest harness the size the window opens at, finds a control by the words
//! a person would use, and clicks or hovers it there. `ticket::order` is the money path: a test
//! that clicks a Buy or Sell button checks what it SENT, and a test of a refusal checks that
//! nothing was sent AND nothing was held for a confirm, with a positive control beside it, so a
//! button that is simply dead cannot pass.
//!
//! ⚠ `Harness::run` steps until repaints settle, so [`Fixture::emitted`] ACCUMULATES: each test
//! drains it before the click it measures. Every harness sets egui's tooltip delay to zero
//! (pre-flight I10): its 0.5 s default outlasts `Harness::run`, so a hover would show nothing.

use egui::accesskit::{Role, Toggled};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use std::assert_matches;
use vike_marketdata::{L2Book, L2BookLevel};
use vike_model::feed_status::{DataClientConnectionState, DataClientStatus};
use vike_model::{AccountTier, Grid};
use vike_order_entry::trade_window::{
    self, TradeWindowAccountPickerRow, TradeWindowAction, TradeWindowInputs,
    TradeWindowLadderOrder, TradeWindowOrigin, TradeWindowPosition, TradeWindowSizingUnit,
    TradeWindowState, TradeWindowStatusLine, TradeWindowStatusLineKind, TradeWindowTicketExits,
    TradeWindowTicketKind, TradeWindowTicketPlace, TradeWindowTradable, grid_value, layout, ticket,
};
use vike_ui_theme::appearance::ThemeAppearance;
use vike_ui_theme::components::ThemeStatus;
use vike_ui_theme::value::trade;
use vike_venues::VenueCapabilities;

/// A BTC perpetual's grid: a 0.1 tick and a 0.001 lot. The window seeds its size from the middle
/// quick size, ten lots: `0.010`.
const BTC: Grid =
    Grid { tick_size: Some(0.1), lot_size: Some(0.001), min_notional: None, min_qty: Some(0.001) };
/// The grid of an instrument whose catalog row has not arrived: no lot, so no size.
const NO_LOT: Grid =
    Grid { tick_size: Some(0.1), lot_size: None, min_notional: None, min_qty: None };
/// What an older node's window says about its own orders (Ruling R9): test words, not the app's.
const UNATTRIBUTED: &str = "This node does not say which account each order belongs to.";

/// The scene one test draws.
#[derive(Clone)]
struct Scene {
    mode: Option<AccountTier>,
    tradable: bool,
    account: Option<&'static str>,
    /// Why a bracket cannot reach this account, as the app states it; `None` where one can.
    bracket_why: Option<&'static str>,
    /// Whether the node can carry a bracket at all.
    bracket_wire: bool,
    /// Whether the node carries a post-only order (it advertises the lifetime word).
    post_only_wire: bool,
    /// The venue's declared capabilities: Post only needs `supports_post_only`.
    capabilities: VenueCapabilities,
    grid: Grid,
    last: Option<f64>,
    /// `false` draws the window over an empty book.
    book: bool,
    /// Draw [`book`]'s one side only: `+1` its bids, `−1` its asks. The other side has no price.
    one_side: Option<i32>,
    orders: Vec<TradeWindowLadderOrder>,
    orders_why: Option<&'static str>,
    accounts: Vec<TradeWindowAccountPickerRow<'static>>,
    /// What the account trades, which an untradable window names.
    trades: Vec<String>,
    /// The cause the app STATES for an untradable window (`TradeWindowTradable::No::why`): `None` where the
    /// account simply trades other symbols.
    why: Option<&'static str>,
    /// The strip's line, as the app composes it.
    status: Option<(TradeWindowStatusLineKind, &'static str)>,
    /// What the node says of a held order (`TradeWindowInputs::held_says`).
    held_says: Option<&'static str>,
    /// A book round this mid instead (`book_around`), which a test may move between frames.
    mid: Option<f64>,
    /// The open position, which a test may open or close between frames (a fill).
    position: Option<TradeWindowPosition>,
    /// The account's free buying power.
    buying_power: Option<f64>,
    /// The appearance the window is drawn under, installed on the first frame.
    look: ThemeAppearance,
}

struct Fixture {
    state: TradeWindowState,
    scene: Scene,
    emitted: Vec<TradeWindowAction>,
    /// The content rect the last frame drew into.
    content: egui::Rect,
    /// The strip's height as `status_line::height` gives it for the last frame's state and inputs.
    strip_h: f32,
}

/// Bids 99.9 and 99.8, asks 100.0 and 100.1.
fn book() -> L2Book {
    let mut b = L2Book::new(0.1);
    b.apply_snapshot(
        1,
        &[L2BookLevel::new(99.9, 1.0), L2BookLevel::new(99.8, 2.0)],
        &[L2BookLevel::new(100.0, 1.0), L2BookLevel::new(100.1, 2.0)],
    );
    b
}

/// A DEMO window on the venue's single default account: one-click on, and TP/SL can be used.
fn demo() -> Scene {
    Scene {
        mode: Some(AccountTier::Demo),
        tradable: true,
        account: None,
        bracket_why: None,
        bracket_wire: true,
        post_only_wire: false,
        capabilities: VenueCapabilities::UNSUPPORTED,
        grid: BTC,
        last: Some(100.0),
        book: true,
        one_side: None,
        orders: Vec::new(),
        orders_why: None,
        accounts: Vec::new(),
        trades: vec!["ETHUSDT".to_string()],
        why: None,
        status: None,
        held_says: None,
        mid: None,
        position: Some(TradeWindowPosition {
            size: 0.05,
            average_price: 99.0,
            unrealized_pnl: 0.05,
        }),
        buying_power: Some(10_000.0),
        look: ThemeAppearance::default(),
    }
}

/// The same DEMO window on a venue with a post-only order, on a node that carries it: Post only is a
/// live toggle.
fn maker() -> Scene {
    Scene {
        capabilities: VenueCapabilities {
            supports_post_only: true,
            ..VenueCapabilities::UNSUPPORTED
        },
        post_only_wire: true,
        ..demo()
    }
}

/// [`book`]'s bids alone (`side` `+1`) or its asks alone (`−1`).
fn one_side_of_book(side: i32) -> L2Book {
    let mut b = L2Book::new(0.1);
    let bids = [L2BookLevel::new(99.9, 1.0), L2BookLevel::new(99.8, 2.0)];
    let asks = [L2BookLevel::new(100.0, 1.0), L2BookLevel::new(100.1, 2.0)];
    if side > 0 {
        b.apply_snapshot(1, &bids, &[]);
    } else {
        b.apply_snapshot(1, &[], &asks);
    }
    b
}

/// Bids 0.05 and 0.06 under `mid`, asks 0.05 and 0.06 over it, on a 0.01 tick.
fn book_around(mid: f64) -> L2Book {
    let mut b = L2Book::new(0.01);
    b.apply_snapshot(
        1,
        &[L2BookLevel::new(mid - 0.05, 1.0), L2BookLevel::new(mid - 0.06, 2.0)],
        &[L2BookLevel::new(mid + 0.05, 1.0), L2BookLevel::new(mid + 0.06, 2.0)],
    );
    b
}

/// The same window on a LIVE account: one-click starts off.
fn live() -> Scene {
    Scene { mode: Some(AccountTier::Live), ..demo() }
}

/// The whole window, beside layout, at the size it opens at.
fn harness(scene: Scene) -> Harness<'static, Fixture> {
    harness_at(scene, trade::BESIDE_SIZE, TradeWindowTicketPlace::Beside)
}

/// The whole window at `size`, the ticket in `panel`.
fn harness_at(
    scene: Scene,
    size: egui::Vec2,
    panel: TradeWindowTicketPlace,
) -> Harness<'static, Fixture> {
    let (two_sided, empty) = (book(), L2Book::new(0.1));
    let (bids_only, asks_only) = (one_side_of_book(1), one_side_of_book(-1));
    let link = DataClientStatus::new(
        DataClientConnectionState::Connected,
        "datahub 127.0.0.1:7878 — 1/1 stream(s) live",
    );
    let mut h = Harness::builder().with_size(size).build_ui_state(
        move |ui, f: &mut Fixture| {
            if !vike_ui_theme::harness::appearance_ready(ui.ctx(), &f.scene.look) {
                return;
            }
            ui.ctx().all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
            let s = &f.scene;
            let around;
            let book = match (s.mid, s.one_side) {
                (Some(m), _) => {
                    around = book_around(m);
                    &around
                }
                (None, Some(side)) if side > 0 => &bids_only,
                (None, Some(_)) => &asks_only,
                (None, None) if s.book => &two_sided,
                (None, None) => &empty,
            };
            let inputs = TradeWindowInputs {
                venue: "binance",
                venue_label: "Binance",
                product: "",
                account: s.account,
                symbol: "BTCUSDT",
                base: "BTC",
                quote: "USDT",
                mode: s.mode,
                tradable: if s.tradable {
                    TradeWindowTradable::Yes
                } else {
                    TradeWindowTradable::No { trades: &s.trades, why: s.why }
                },
                grid: s.grid,
                book,
                last: s.last,
                stale: false,
                source: Some(&link),
                absence: None,
                top: None,
                orders: &s.orders,
                orders_why: s.orders_why,
                position: s.position,
                buying_power: s.buying_power,
                value: vike_model::ContractValue::default(),
                capabilities: s.capabilities,
                bracket_why: s.bracket_why,
                bracket_wire: s.bracket_wire,
                post_only_wire: s.post_only_wire,
                matches: &[],
                recent: &[],
                accounts: &s.accounts,
                accounts_why: None,
                unconnected: &[],
                tape: &[],
                status: s.status.map(|(kind, text)| TradeWindowStatusLine { kind, text }),
                held_says: s.held_says,
            };
            let content = ui.available_rect_before_wrap();
            let acts = trade_window::draw(ui, &mut f.state, &inputs);
            f.emitted.extend(acts);
            let t = vike_ui_theme::components::ThemeTokens::of(ui.ctx());
            f.content = content;
            f.strip_h =
                trade_window::status_line::height(ui.ctx(), &t, &f.state, &inputs, content.width());
        },
        Fixture {
            state: TradeWindowState::default(),
            scene,
            emitted: Vec::new(),
            content: egui::Rect::NOTHING,
            strip_h: 0.0,
        },
    );
    h.state_mut().state.view.panel = panel;
    h
}

/// A window drawn until it stops asking for frames, with nothing emitted yet.
fn settled(scene: Scene) -> Harness<'static, Fixture> {
    let mut h = harness(scene);
    h.run();
    h.state_mut().emitted.clear();
    h
}

/// Change the ticket's state as a trader would by typing, let the window draw it, and drain.
fn set(h: &mut Harness<'static, Fixture>, change: impl FnOnce(&mut TradeWindowState)) {
    change(&mut h.state_mut().state);
    h.run();
    h.state_mut().emitted.clear();
}

fn places(f: &Fixture) -> Vec<&TradeWindowAction> {
    f.emitted.iter().filter(|a| matches!(a, TradeWindowAction::Place { .. })).collect()
}

/// Whether the control named exactly `label` is disabled.
fn disabled(h: &Harness<'_, Fixture>, label: &str) -> bool {
    h.get_by_label(label).accesskit_node().is_disabled()
}

/// Hover the control named exactly `label`; whether that ADDED `words`, word for word, to the
/// screen. The pointer first rests where nothing is, so a tooltip an earlier hover left open with
/// the same words cannot answer for this one, and words already on screen (the ticket's own
/// untradable line) do not count.
fn says_on_hover(h: &mut Harness<'static, Fixture>, label: &str, words: &str) -> bool {
    h.hover_at(egui::pos2(1.0, 1.0));
    h.run();
    let before = h.query_all_by_label(words).count();
    h.get_by_label(label).hover();
    h.run();
    h.query_all_by_label(words).count() > before
}

/// Click the control named exactly `label`, and let the window answer.
fn click(h: &mut Harness<'static, Fixture>, label: &str) {
    h.get_by_label(label).click();
    h.run();
}

/// A ticket state, changed the way a test needs it.
fn state_with(change: impl FnOnce(&mut TradeWindowState)) -> TradeWindowState {
    let mut s = TradeWindowState::default();
    change(&mut s);
    s
}

/// The inputs of a DEMO window on `grid`, without a frame: for `ticket::order` called directly.
fn inputs_on(book: &L2Book, grid: Grid) -> TradeWindowInputs<'_> {
    TradeWindowInputs {
        venue: "binance",
        venue_label: "Binance",
        product: "",
        account: None,
        symbol: "BTCUSDT",
        base: "BTC",
        quote: "USDT",
        mode: Some(AccountTier::Demo),
        tradable: TradeWindowTradable::Yes,
        grid,
        book,
        last: Some(100.0),
        stale: false,
        source: None,
        absence: None,
        top: None,
        orders: &[],
        orders_why: None,
        position: None,
        buying_power: Some(10_000.0),
        value: vike_model::ContractValue::default(),
        capabilities: VenueCapabilities::UNSUPPORTED,
        bracket_why: None,
        bracket_wire: true,
        post_only_wire: false,
        matches: &[],
        recent: &[],
        accounts: &[],
        accounts_why: None,
        unconnected: &[],
        tape: &[],
        status: None,
        held_says: None,
    }
}

/// A change to a ticket's state.
type Tweak = fn(&mut TradeWindowState);

/// Every place TP/SL is refused, with its own words: the node, a named account, the default
/// account of a venue that runs a second one, a Stop entry, reduce-only (Rulings R3 and R8, spec
/// §3.6).
fn refused_take_profit_stop_loss() -> [(&'static str, Scene, Tweak, &'static str); 5] {
    [
        (
            "a named account",
            Scene {
                account: Some("SUB"),
                bracket_why: Some(ticket::TAKE_PROFIT_STOP_LOSS_ACCOUNT_WHY),
                ..demo()
            },
            |_| {},
            ticket::TAKE_PROFIT_STOP_LOSS_ACCOUNT_WHY,
        ),
        (
            "the default account of a venue that runs a second one",
            Scene { bracket_why: Some(ticket::TAKE_PROFIT_STOP_LOSS_ACCOUNT_WHY), ..demo() },
            |_| {},
            ticket::TAKE_PROFIT_STOP_LOSS_ACCOUNT_WHY,
        ),
        (
            "a Stop entry",
            demo(),
            |s| s.order_type = TradeWindowTicketKind::Stop,
            ticket::TAKE_PROFIT_STOP_LOSS_STOP_WHY,
        ),
        ("reduce-only", demo(), |s| s.reduce_only = true, ticket::TAKE_PROFIT_STOP_LOSS_REDUCE_WHY),
        (
            "a node with no wire form for a bracket, which outranks the account",
            Scene {
                account: Some("SUB"),
                bracket_why: Some(ticket::TAKE_PROFIT_STOP_LOSS_ACCOUNT_WHY),
                bracket_wire: false,
                ..demo()
            },
            |_| {},
            ticket::TAKE_PROFIT_STOP_LOSS_WIRE_WHY,
        ),
    ]
}

fn working(order_client_id: &str, side: i32, price: f64) -> TradeWindowLadderOrder {
    TradeWindowLadderOrder {
        order_client_id: order_client_id.to_string(),
        side,
        price,
        quantity: 0.01,
        is_stop: false,
        words: None,
    }
}

/// The window's region a node was drawn in: its OUTERMOST ancestor named as a group, so a control
/// in the full ticket's scrolling form (a group of its own) counts as the ticket's.
fn region_of(node: &egui_kittest::Node<'_>) -> Option<String> {
    let mut found = None;
    let mut at = node.parent();
    while let Some(n) = at {
        let a = n.accesskit_node();
        if a.role() == Role::Group
            && let Some(name) = a.label()
        {
            found = Some(name);
        }
        at = n.parent();
    }
    found
}

/// Every `Role::Label` value the region named `region` drew.
fn labels_in(h: &Harness<'_, Fixture>, region: &str) -> Vec<String> {
    h.root()
        .children_recursive()
        .filter(|n| n.accesskit_node().role() == Role::Label)
        .filter(|n| region_of(n).as_deref() == Some(region))
        .map(|n| n.accesskit_node().value().unwrap_or_default())
        .collect()
}

/// The rect of the region `name` draws: its group's bounds.
fn region_rect(h: &Harness<'_, Fixture>, name: &str) -> egui::Rect {
    h.root()
        .children_recursive()
        .find(|n| {
            let a = n.accesskit_node();
            a.role() == Role::Group && a.label().as_deref() == Some(name)
        })
        .map(|n| n.rect())
        .unwrap_or_else(|| panic!("no region named {name:?}"))
}

/// The label of the one button whose words start with `prefix` and end with `suffix`.
fn label_of(h: &Harness<'_, Fixture>, prefix: &str, suffix: &str) -> String {
    let found: Vec<String> = h
        .root()
        .children_recursive()
        .filter(|n| n.accesskit_node().role() == Role::Button)
        .filter_map(|n| n.accesskit_node().label())
        .filter(|l| l.starts_with(prefix) && l.ends_with(suffix))
        .collect();
    match found.as_slice() {
        [one] => one.clone(),
        other => panic!("one button {prefix}…{suffix}, found {other:?}"),
    }
}

/// An open position, for the flips below.
const OPEN: TradeWindowPosition =
    TradeWindowPosition { size: 0.05, average_price: 99.0, unrealized_pnl: 0.05 };

/// One text the last frame painted: its words, each of its sections' words with the colour they
/// were painted in, where it lies, and whether the kit cut it short.
#[derive(Debug)]
struct Painted {
    text: String,
    sections: Vec<(String, egui::Color32)>,
    rect: egui::Rect,
    elided: bool,
}

/// Every shape the last frame painted, `Shape::Vec`s flattened.
fn flat_shapes(h: &Harness<'_, Fixture>) -> Vec<egui::Shape> {
    fn flatten(s: &egui::Shape, out: &mut Vec<egui::Shape>) {
        match s {
            egui::Shape::Vec(v) => v.iter().for_each(|s| flatten(s, out)),
            s => out.push(s.clone()),
        }
    }
    let mut out = Vec::new();
    for c in &h.output().shapes {
        flatten(&c.shape, &mut out);
    }
    out
}

/// Every text the last frame painted. A section's colour is the one the painter resolves: its own,
/// else the shape's fallback, under any override.
fn painted(h: &Harness<'_, Fixture>) -> Vec<Painted> {
    flat_shapes(h)
        .into_iter()
        .filter_map(|s| match s {
            egui::Shape::Text(t) => {
                let job = &t.galley.job;
                let sections = job
                    .sections
                    .iter()
                    .map(|s| {
                        let own = Some(s.format.color).filter(|c| *c != egui::Color32::PLACEHOLDER);
                        let colour =
                            t.override_text_color.unwrap_or(own.unwrap_or(t.fallback_color));
                        let bytes = s.byte_range.start.0..s.byte_range.end.0;
                        (job.text[bytes].to_string(), colour)
                    })
                    .collect();
                Some(Painted {
                    text: t.galley.text().to_string(),
                    sections,
                    rect: t.visual_bounding_rect(),
                    elided: t.galley.elided,
                })
            }
            _ => None,
        })
        .collect()
}

/// The strip's rect as `status_line::height` derives it for the last frame.
fn strip_rect(h: &Harness<'_, Fixture>) -> egui::Rect {
    let (content, strip_h) = (h.state().content, h.state().strip_h);
    egui::Rect::from_min_max(egui::pos2(content.min.x, content.max.y - strip_h), content.max)
}

/// The texts the last frame painted in the strip.
fn in_strip(h: &Harness<'_, Fixture>) -> Vec<Painted> {
    let strip = strip_rect(h);
    painted(h).into_iter().filter(|p| strip.contains(p.rect.center())).collect()
}

#[path = "trade_actions/controls.rs"]
mod controls;
#[path = "trade_actions/exits_and_sizes.rs"]
mod exits_and_sizes;
#[path = "trade_actions/held_orders.rs"]
mod held_orders;
#[path = "trade_actions/live_window.rs"]
mod live_window;
#[path = "trade_actions/presses.rs"]
mod presses;
#[path = "trade_actions/ticket_look.rs"]
mod ticket_look;
#[path = "trade_actions/ticket_orders.rs"]
mod ticket_orders;
#[path = "trade_actions/untradable.rs"]
mod untradable;
