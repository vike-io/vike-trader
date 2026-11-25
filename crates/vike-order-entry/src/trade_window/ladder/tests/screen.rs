//! The ladder on screen (kittest; the accessibility tree and real pointer events): the harness, the
//! clicks that place and cancel, the toolbar and the Vol column; the presses, drags and hovers are
//! `press`'s.

use super::super::columns::{TradeWindowLadderColumn, TradeWindowLadderColumns};
use super::super::markers::{TradeWindowLadderMarkerState, marker_face, marker_room, paint_marker};
use super::super::rows::{group_tape, next_group, row_key};
use super::super::toolbar::{GROUP_LESS, GROUP_MORE, RECENTER, TradeWindowLadderToolbarGeometry};
use crate::trade_window::TradeWindowOrigin;
use crate::trade_window::tick_chart::TradeWindowTickChartPrint;
use crate::trade_window::{TradeWindowStatusLineKind, TradeWindowTradable};
use egui::accesskit::{Role, Toggled};
use egui::{Event, PointerButton};
use vike_model::AccountTier;
use vike_model::Grid;

use super::*;

/// Bids `mid − 0.5` and two below, asks `mid + 0.5` and two above, on a 1.0 tick.
fn book_at(mid: f64) -> L2Book {
    let (b, a) = (mid - 0.5, mid + 0.5);
    let mut book = L2Book::new(1.0);
    book.apply_snapshot(
        1,
        &[L2BookLevel::new(b, 1.0), L2BookLevel::new(b - 1.0, 2.0), L2BookLevel::new(b - 2.0, 3.0)],
        &[L2BookLevel::new(a, 1.0), L2BookLevel::new(a + 1.0, 2.0), L2BookLevel::new(a + 2.0, 3.0)],
    );
    book
}

/// Bids 99/98/97, asks 100/101/102: mid 99.5, so the rows centre on 100.
const MID: f64 = 99.5;
const GRID: Grid =
    Grid { tick_size: Some(1.0), lot_size: Some(0.001), min_notional: None, min_qty: Some(0.001) };
/// The size a synced window starts with on [`GRID`]: the middle quick size, ten lots.
const SIZE: f64 = 0.01;

/// What a scene varies; everything else is one account trading `BTCUSDT`.
struct Setup {
    tradable: TradeWindowTradable<'static>,
    orders: Vec<TradeWindowLadderOrder>,
    orders_why: Option<&'static str>,
    capabilities: VenueCapabilities,
    /// Whether the node carries a post-only order (`TradeWindowInputs::post_only_wire`).
    post_only_wire: bool,
    panel: TradeWindowTicketPlace,
    mode: Option<AccountTier>,
    last: Option<f64>,
    grid: Grid,
}

impl Default for Setup {
    fn default() -> Self {
        Setup {
            tradable: TradeWindowTradable::Yes,
            orders: Vec::new(),
            orders_why: None,
            capabilities: VenueCapabilities::UNSUPPORTED,
            post_only_wire: false,
            panel: TradeWindowTicketPlace::Beside,
            mode: Some(AccountTier::Demo),
            last: Some(MID),
            grid: GRID,
        }
    }
}

struct Scene {
    /// The book the ladder draws: a test may move the market between frames.
    book: L2Book,
    /// The account's working orders: a test may fill or place one between frames.
    orders: Vec<TradeWindowLadderOrder>,
    state: TradeWindowState,
    emitted: Vec<TradeWindowAction>,
    /// The rect the ladder was given and the tokens it drew with, on the last frame.
    drawn: Option<(Rect, ThemeTokens)>,
    /// Whether the book is stale: a test may flip it between frames, as the feed does.
    stale: bool,
    /// The prints on the tape, for the Vol column: none until a test says so.
    tape: Vec<TradeWindowTickChartPrint>,
}

/// The ladder alone, as `trade::draw` hands it its region: synced, then drawn.
fn scene(setup: Setup) -> Harness<'static, Scene> {
    let mut state = TradeWindowState::default();
    state.view.panel = setup.panel;
    let orders = setup.orders.clone();
    Harness::builder().with_size(egui::vec2(420.0, 560.0)).build_ui_state(
        move |ui, s: &mut Scene| {
            // The toolbar draws icons; their family is bound only by the app's type.
            if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                return;
            }
            let t = ThemeTokens::of(ui.ctx());
            let Scene { book, orders, state, emitted, drawn, stale, tape } = s;
            let inputs = TradeWindowInputs {
                venue: "binance",
                venue_label: "Test venue",
                product: "",
                account: None,
                symbol: "BTCUSDT",
                base: "BTC",
                quote: "USDT",
                mode: setup.mode,
                tradable: setup.tradable,
                grid: setup.grid,
                book,
                last: setup.last,
                stale: *stale,
                source: None,
                absence: None,
                top: None,
                orders: orders.as_slice(),
                orders_why: setup.orders_why,
                position: None,
                buying_power: None,
                value: vike_model::ContractValue::default(),
                capabilities: setup.capabilities,
                bracket_why: None,
                bracket_wire: false,
                post_only_wire: setup.post_only_wire,
                matches: &[],
                recent: &[],
                accounts: &[],
                accounts_why: None,
                unconnected: &[],
                tape: tape.as_slice(),
                status: None,
                held_says: None,
            };
            let mut acts = Vec::new();
            state.sync(&inputs, &mut acts);
            *drawn = Some((ui.available_rect_before_wrap(), t));
            draw(ui, &t, state, &inputs, &mut acts);
            emitted.extend(acts);
        },
        Scene {
            book: book_at(MID),
            orders,
            state,
            emitted: Vec::new(),
            drawn: None,
            stale: false,
            tape: Vec::new(),
        },
    )
}

/// A print of `size` at `price`: when and who bought do not matter to the Vol column.
fn print(price: f64, size: f64) -> TradeWindowTickChartPrint {
    TradeWindowTickChartPrint { ms: 0, price, size, buy: true }
}

/// A settled scene with nothing emitted yet.
fn settled(setup: Setup) -> Harness<'static, Scene> {
    let mut h = scene(setup);
    h.run();
    h.state_mut().emitted.clear();
    h
}

/// The rows' rect, from the geometry the ladder drew with.
fn rows_rect(h: &Harness<'static, Scene>) -> Rect {
    let (area, t) = h.state().drawn.expect("the ladder drew");
    parts(area, &t.metrics, h.state().state.view.panel == TradeWindowTicketPlace::Beside).rows
}

/// The centre of `col`'s cell on the row holding `price`, from the geometry the ladder drew
/// with (one tick per row, centred on the trader's own centre where one is set, else on
/// [`MID`]).
fn cell(h: &Harness<'static, Scene>, col: TradeWindowLadderColumn, price: f64) -> Pos2 {
    let row_h = h.state().drawn.expect("the ladder drew").1.metrics.row_height;
    let rows = rows_rect(h);
    let n_rows = (rows.height() / row_h).floor().max(1.0) as usize;
    let centre = h.state().state.center.unwrap_or(MID);
    let index = row_key(centre, 1.0, 1) + (n_rows / 2) as i64 - row_key(price, 1.0, 1);
    let y = rows.min.y + (index as f32 + 0.5) * row_h;
    let s = &h.state().state;
    let cols = TradeWindowLadderColumns::new(
        s.view.panel,
        s.view.panel == TradeWindowTicketPlace::Beside && s.volume,
        rows.width(),
        0.0,
    );
    pos2(cols.rect(col, rows).center().x, y)
}

fn press(h: &Harness<'static, Scene>, screen_position: Pos2, pressed: bool) {
    h.event(Event::PointerButton {
        pos: screen_position,
        button: PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    });
}

/// A primary click at `screen_position`, and what it emitted.
fn click_at(h: &mut Harness<'static, Scene>, screen_position: Pos2) -> Vec<TradeWindowAction> {
    h.event(Event::PointerMoved(screen_position));
    press(h, screen_position, true);
    press(h, screen_position, false);
    h.run();
    std::mem::take(&mut h.state_mut().emitted)
}

/// A primary click on `col`'s cell of the row holding `price`, and what it emitted.
fn click(
    h: &mut Harness<'static, Scene>,
    col: TradeWindowLadderColumn,
    price: f64,
) -> Vec<TradeWindowAction> {
    let screen_position = cell(h, col, price);
    click_at(h, screen_position)
}

/// A primary drag from `from` to `to` over several frames, and what it emitted.
fn drag_to(h: &mut Harness<'static, Scene>, from: Pos2, to: Pos2) -> Vec<TradeWindowAction> {
    h.event(Event::PointerMoved(from));
    press(h, from, true);
    h.step();
    for k in 1..=4 {
        h.event(Event::PointerMoved(from + (to - from) * (k as f32 / 4.0)));
        h.step();
    }
    press(h, to, false);
    h.run();
    std::mem::take(&mut h.state_mut().emitted)
}

/// A primary drag in `col` from the row holding `from` to the row holding `to`.
fn drag(
    h: &mut Harness<'static, Scene>,
    col: TradeWindowLadderColumn,
    from: f64,
    to: f64,
) -> Vec<TradeWindowAction> {
    let (from, to) = (cell(h, col, from), cell(h, col, to));
    drag_to(h, from, to)
}

/// Whether `got` is exactly one plain ladder order: `side`, `order_type` at `price`, `quantity`, no
/// reduce-only, no post-only and no exits (a ladder click never carries TP/SL, spec §3.6).
fn place_quantity(
    side: i32,
    order_type: TradeWindowTicketKind,
    price: f64,
    want: f64,
    got: &[TradeWindowAction],
) -> bool {
    let [
        TradeWindowAction::Place {
            side: s,
            order_type: o,
            price: p,
            quantity,
            reduce_only,
            post_only,
            exits,
            origin,
        },
    ] = got
    else {
        return false;
    };
    (*s, *o, *p, *reduce_only, *post_only, *exits, *origin)
        == (side, order_type, Some(price), false, false, None, TradeWindowOrigin::Ladder)
        && (quantity - want).abs() < 1e-12
}

/// [`place_quantity`] at the size a synced window starts with.
fn place(
    side: i32,
    order_type: TradeWindowTicketKind,
    price: f64,
    got: &[TradeWindowAction],
) -> bool {
    place_quantity(side, order_type, price, SIZE, got)
}

/// Review Focus 2's ladder half (pre-flight I8): a symbol the account does not trade takes no
/// order from the ladder. ⚠ The POSITIVE CONTROL comes first: the same four cells of the same
/// scene, tradable, each PLACE the order the click grammar names — so "nothing was emitted"
/// below cannot be true merely because no click reached a row.
#[test]
fn an_untradable_symbol_ignores_ladder_clicks() {
    let cases = [
        (TradeWindowLadderColumn::Buy, 98.0, 1, TradeWindowTicketKind::Limit),
        (TradeWindowLadderColumn::Bid, 101.0, 1, TradeWindowTicketKind::Stop),
        (TradeWindowLadderColumn::Ask, 101.0, -1, TradeWindowTicketKind::Limit),
        (TradeWindowLadderColumn::Sell, 98.0, -1, TradeWindowTicketKind::Stop),
    ];
    let mut h = settled(Setup::default());
    for (col, price, side, kind) in cases {
        let got = click(&mut h, col, price);
        assert!(place(side, kind, price, &got), "CONTROL: {col:?} @ {price} emitted {got:?}");
    }
    assert!(
        click(&mut h, TradeWindowLadderColumn::Price, 98.0).is_empty(),
        "the price column is inert"
    );

    let mut h = settled(Setup {
        tradable: TradeWindowTradable::No { trades: &[], why: None },
        ..Setup::default()
    });
    for (col, price, _, _) in cases {
        let got = click(&mut h, col, price);
        assert!(got.is_empty(), "untradable: {col:?} @ {price} emitted {got:?}");
        assert_eq!(h.state().state.held, None, "and holds nothing for a confirm either");
    }
}

/// **Post only reaches the ladder's clicks.** Ticked, a click that rests is a post-only LIMIT order,
/// and a click through the market, which the ladder would make a stop (a stop takes liquidity when
/// it fires), places nothing and says why. The CONTROL is the same two clicks with the toggle off:
/// a limit and a stop, as ever.
#[test]
fn a_ladder_click_keeps_post_only_or_is_refused_in_words() {
    let mut h = settled(Setup {
        capabilities: VenueCapabilities {
            supports_post_only: true,
            ..VenueCapabilities::UNSUPPORTED
        },
        post_only_wire: true,
        ..Setup::default()
    });
    let got = click(&mut h, TradeWindowLadderColumn::Buy, 98.0);
    assert!(place(1, TradeWindowTicketKind::Limit, 98.0, &got), "CONTROL: a resting buy: {got:?}");
    let got = click(&mut h, TradeWindowLadderColumn::Bid, 101.0);
    assert!(place(1, TradeWindowTicketKind::Stop, 101.0, &got), "CONTROL: a crossing buy: {got:?}");

    h.state_mut().state.post_only = true;
    let got = click(&mut h, TradeWindowLadderColumn::Buy, 98.0);
    match got.as_slice() {
        [
            TradeWindowAction::Place {
                side: 1,
                order_type: TradeWindowTicketKind::Limit,
                price: Some(p),
                post_only: true,
                exits: None,
                origin: TradeWindowOrigin::Ladder,
                ..
            },
        ] => assert!((p - 98.0).abs() < 1e-9, "{p}"),
        other => panic!("one post-only limit buy, got {other:?}"),
    }
    let got = click(&mut h, TradeWindowLadderColumn::Bid, 101.0);
    assert_eq!(
        got,
        [TradeWindowAction::Note {
            kind: TradeWindowStatusLineKind::Error,
            text: crate::trade_window::ticket::POST_LADDER_WHY.to_string()
        }],
        "a click through the market is refused, not turned into a stop"
    );
    assert_eq!(h.state().state.held, None, "and holds nothing for a confirm");
    assert!(h.state().state.post_only, "the trader's choice is left as it was");
}

/// Clicking a marker cancels EVERY order it stands for. On a node that does not say which
/// account an order is on (`orders_why`, Ruling R9) the ladder draws no marker, so the same
/// click places an order instead — never a cancel for an order it cannot attribute.
#[test]
fn a_marker_click_cancels_every_order_it_stands_for_and_an_unattributed_ladder_has_none() {
    let orders = vec![order("b1", 1, 98.0, 0.01, false), order("b2", 1, 98.0, 0.02, false)];
    let mut h = settled(Setup { orders: orders.clone(), ..Setup::default() });
    let got = click(&mut h, TradeWindowLadderColumn::Buy, 98.0);
    assert_eq!(got, [TradeWindowAction::Cancel(vec!["b1".to_string(), "b2".to_string()])]);
    let got = click(&mut h, TradeWindowLadderColumn::Bid, 98.0);
    assert!(
        place(1, TradeWindowTicketKind::Limit, 98.0, &got),
        "the Bid cell still trades: {got:?}"
    );

    let why = "This node does not say which account an order is on.";
    let mut h = settled(Setup { orders, orders_why: Some(why), ..Setup::default() });
    let got = click(&mut h, TradeWindowLadderColumn::Buy, 98.0);
    assert!(place(1, TradeWindowTicketKind::Limit, 98.0, &got), "no marker, so no cancel: {got:?}");
}

/// Dragging a marker moves every order it stands for to the row it is dropped on, and only
/// where the venue can modify an order: the same drag on a modify-less venue emits nothing.
#[test]
fn a_drag_reprices_only_where_the_venue_can_modify() {
    let orders = vec![order("b1", 1, 98.0, 0.01, false), order("b2", 1, 98.0, 0.02, false)];
    let mut h = settled(Setup {
        orders: orders.clone(),
        capabilities: vike_venues::venue_caps::BINANCE,
        ..Setup::default()
    });
    let got = drag(&mut h, TradeWindowLadderColumn::Buy, 98.0, 96.0);
    assert_eq!(
        got,
        [
            TradeWindowAction::Modify { order_client_id: "b1".to_string(), new_price: 96.0 },
            TradeWindowAction::Modify { order_client_id: "b2".to_string(), new_price: 96.0 },
        ]
    );
    assert_eq!(h.state().state.drag, None, "the drop ends the drag");

    let mut h = settled(Setup { orders, ..Setup::default() });
    let got = drag(&mut h, TradeWindowLadderColumn::Buy, 98.0, 96.0);
    assert!(got.is_empty(), "no modify on this venue, so no drag: {got:?}");
}

/// A drop outside the rows moves nothing (the row under it would be the edge row, clamped, a
/// price the pointer was never on), and neither does a drop on the row the orders sit on.
#[test]
fn a_drop_outside_the_rows_or_on_the_orders_own_row_moves_nothing() {
    let setup = || Setup {
        orders: vec![order("b1", 1, 98.0, 0.01, false)],
        capabilities: vike_venues::venue_caps::BINANCE,
        ..Setup::default()
    };
    let mut h = settled(setup());
    let got = drag(&mut h, TradeWindowLadderColumn::Buy, 98.0, 96.0);
    let moved = TradeWindowAction::Modify { order_client_id: "b1".to_string(), new_price: 96.0 };
    assert_eq!(got, [moved], "CONTROL: a drop on a row inside reprices");

    let mut h = settled(setup());
    let (from, rows) = (cell(&h, TradeWindowLadderColumn::Buy, 98.0), rows_rect(&h));
    for to in [pos2(from.x, rows.min.y - 4.0), pos2(from.x, rows.max.y + 4.0)] {
        let got = drag_to(&mut h, from, to);
        assert!(got.is_empty(), "a drop at {to:?}, outside {rows:?}: {got:?}");
        assert_eq!(h.state().state.drag, None, "and the drag is over");
    }
    let to = cell(&h, TradeWindowLadderColumn::Bid, 98.0);
    assert!(drag_to(&mut h, from, to).is_empty(), "the orders' own row: nothing moves");
}

/// An Escape while a marker is dragged CANCELS the drag: egui ends it as a `drag_stopped` with
/// the pointer where it is, and the drop used to read that as a drop there and reprice the
/// orders to the row under the pointer. ⚠ The CONTROL comes first: the same drag, released
/// instead, reprices — so "nothing moved" below cannot be true merely because the drag never
/// started.
#[test]
fn an_escape_during_a_drag_moves_nothing() {
    let setup = || Setup {
        orders: vec![order("b1", 1, 98.0, 0.01, false)],
        capabilities: vike_venues::venue_caps::BINANCE,
        ..Setup::default()
    };
    let mut h = settled(setup());
    let moved = TradeWindowAction::Modify { order_client_id: "b1".to_string(), new_price: 96.0 };
    assert_eq!(
        drag(&mut h, TradeWindowLadderColumn::Buy, 98.0, 96.0),
        [moved],
        "CONTROL: a release reprices"
    );

    let mut h = settled(setup());
    let (from, to) = (
        cell(&h, TradeWindowLadderColumn::Buy, 98.0),
        cell(&h, TradeWindowLadderColumn::Buy, 96.0),
    );
    h.event(Event::PointerMoved(from));
    press(&h, from, true);
    h.step();
    for k in 1..=4 {
        h.event(Event::PointerMoved(from + (to - from) * (k as f32 / 4.0)));
        h.step();
    }
    assert!(h.state().state.drag.is_some(), "the drag is under way");
    h.key_press(egui::Key::Escape);
    h.step();
    press(&h, to, false);
    h.run();
    let got = std::mem::take(&mut h.state_mut().emitted);
    assert!(got.is_empty(), "an Escape cancels the drag: {got:?}");
    assert_eq!(h.state().state.drag, None, "and the drag is over");
}

/// The W4 carry: a ladder click takes the TICKET's size rule (`ticket::checked_quantity`), so one
/// rule applies wherever an order is made. A size under the instrument's smallest order is
/// refused, says why on the strip, and sends nothing; the smallest order itself goes. Before,
/// the ladder checked only for one lot, and such a size reached the venue's own refusal.
#[test]
fn a_ladder_click_takes_the_tickets_size_rule() {
    let grid = Grid { min_qty: Some(0.05), ..GRID };
    let mut h = settled(Setup { grid, ..Setup::default() });
    h.state_mut().state.size = "0.01".to_string();
    let got = click(&mut h, TradeWindowLadderColumn::Bid, 98.0);
    match got.as_slice() {
        [TradeWindowAction::Note { kind: TradeWindowStatusLineKind::Error, text }] => {
            assert!(text.contains("smallest order size"), "{text}");
        }
        other => panic!("one error note and no order, got {other:?}"),
    }
    assert_eq!(h.state().state.held, None, "nothing is held for a confirm either");
    h.state_mut().state.size = "0.05".to_string();
    let got = click(&mut h, TradeWindowLadderColumn::Bid, 98.0);
    assert!(place_quantity(1, TradeWindowTicketKind::Limit, 98.0, 0.05, &got), "CONTROL: {got:?}");
}

/// W1 review hand-off: what `paint_marker` PAINTS, not only what `marker_words` chooses. In a
/// cell too narrow for its size a marker paints its COUNT, whole and inside the cell, with no
/// clip cutting it; in a wide one, the count and the size. Re-adding a clip that cut the full
/// words to the marker (the DOM's) fails here, where every pure test would still pass.
#[test]
fn a_marker_too_narrow_for_its_size_paints_its_count_unclipped() {
    let (a, b) = (order("a", 1, 98.0, 0.01, false), order("b", 1, 98.0, 0.02, false));
    let face = marker_face(&[&a, &b], Some(0.001));
    assert_eq!(face.full, "×2 0.030");
    for (cell_w, want) in [(36.0, "×2"), (120.0, "×2 0.030")] {
        let face = face.clone();
        let mut h = Harness::builder().with_size(egui::vec2(200.0, 80.0)).build_ui_state(
            move |ui, cell: &mut Option<Rect>| {
                if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                    return;
                }
                let t = ThemeTokens::of(ui.ctx());
                let at = ui.min_rect().min + vec2(10.0, 10.0);
                let c = Rect::from_min_size(at, vec2(cell_w, t.metrics.row_height));
                let state = TradeWindowLadderMarkerState {
                    draggable: true,
                    ..TradeWindowLadderMarkerState::default()
                };
                paint_marker(ui.painter(), &t, c, 1, &face, state);
                *cell = Some(c);
            },
            None,
        );
        h.run();
        let cell = h.state().expect("the marker was painted");
        let texts: Vec<(String, Rect, Rect)> = h
            .output()
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => {
                    Some((t.galley.text().to_string(), t.visual_bounding_rect(), c.clip_rect))
                }
                _ => None,
            })
            .collect();
        let [(text, drawn, clip)] = texts.as_slice() else {
            panic!("{cell_w} pt: one text, got {texts:?}");
        };
        assert_eq!(text, want, "{cell_w} pt");
        assert!(cell.expand(0.5).contains_rect(*drawn), "{cell_w} pt: {drawn:?} in {cell:?}");
        assert!(clip.contains_rect(*drawn), "{cell_w} pt: nothing of it is clipped away");
    }
}

/// A measurement for the owner, not a check: what a marker can say at the column widths the
/// window ships with (W1 review hand-off; the owner decides whether to widen the order columns
/// or list the orders on hover). Run it with
/// `cargo test -p vike-order-entry --lib print_marker_room -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement for the owner: what a marker can say at the shipped column widths"]
fn print_marker_room() {
    const WORDS: [&str; 5] = ["×2", "×2 0.030", "0.010", "×3 1.250", "1234.567"];
    let mut h = Harness::builder().build_ui_state(
        |ui, out: &mut Vec<String>| {
            if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                return;
            }
            out.clear();
            for density in ThemeDensity::ALL {
                let m = density.metrics();
                for text_size in ThemeTextSize::ALL {
                    let a = ThemeAppearance { density, text_size, ..ThemeAppearance::default() };
                    let t = ThemeTokens::from_appearance(&a);
                    for (panel, size) in [
                        (TradeWindowTicketPlace::Beside, trade::BESIDE_SIZE),
                        (TradeWindowTicketPlace::Under, trade::UNDER_SIZE),
                    ] {
                        // The desktop's tool-window body keeps 8 pt each side.
                        let body = Rect::from_min_size(Pos2::ZERO, size - vec2(16.0, 0.0));
                        let view = TradeWindowPanes { chart: false, ladder: true, panel };
                        let Some(ladder) =
                            layout::split(body, view, &m, layout::ticket_w(&t)).ladder
                        else {
                            continue;
                        };
                        let cols = TradeWindowLadderColumns::new(panel, false, ladder.width(), 0.0);
                        let col = cols.rect(TradeWindowLadderColumn::Buy, ladder);
                        let widths: Vec<String> = WORDS
                            .iter()
                            .map(|w| {
                                let font = t.mono(ThemeTextRole::Caption);
                                let g =
                                    ui.painter().layout_no_wrap(w.to_string(), font, t.theme.text);
                                format!("{w:?} {:.1}", g.size().x)
                            })
                            .collect();
                        out.push(format!(
                            "{density:?}/{text_size:?}/{panel:?} {}x{}: Buy column {:.1} pt, room \
                             {:.1} pt; {}",
                            size.x,
                            size.y,
                            col.width(),
                            marker_room(col),
                            widths.join(", ")
                        ));
                    }
                }
            }
        },
        Vec::new(),
    );
    h.run();
    for line in h.state() {
        println!("{line}");
    }
}

/// W4 fix round 2 (the NEW-1 audit, carried to the toolbar): the toolbar drops its word `Group`
/// when STALE needs the room, and STALE comes and goes with the FEED, not with the trader. A
/// press on "More price grouping" held across that is still a press on it: the grouping goes
/// up, and Recenter is not clicked. The ladder is sized so the toolbar holds the word without
/// STALE and not with it (measured as the toolbar measures, through [`TradeWindowLadderToolbarGeometry`]).
#[test]
fn a_press_held_while_stale_drops_the_word_group_still_groups() {
    let mut h = settled(Setup::default());
    let (_, t) = h.state().drawn.expect("the ladder drew");
    let m = t.metrics;
    let w = |text: &str, font: egui::FontId| {
        h.ctx.fonts_mut(|f| f.layout_no_wrap(text.to_string(), font, Color32::WHITE)).size().x
    };
    let number = w("1", t.mono(ThemeTextRole::Body)).max(trade::GROUP_NUMBER_WIDTH);
    let word = w("Group", t.font(ThemeTextRole::Caption));
    let stale = w("STALE", t.mono(ThemeTextRole::Caption)) + 2.0 * (0.75 * m.padding).round();
    let holds = |ladder_w: f32, stale_w: f32| {
        let band = Rect::from_min_size(Pos2::ZERO, vec2(ladder_w, m.control_height));
        TradeWindowLadderToolbarGeometry::new(band, &m, true, stale_w, number, word).word.is_some()
    };
    let width = (150..600)
        .map(|w| w as f32 + 0.5)
        .find(|w| holds(*w, 0.0) && !holds(*w, stale))
        .expect("a width that holds the word without STALE and not with it");
    // The harness keeps 8 pt each side of the ladder.
    h.set_size(egui::vec2(width + 16.0, 560.0));
    h.run();
    let has_word = |h: &Harness<'static, Scene>| labels(h).iter().any(|l| l == "Group");
    assert!(has_word(&h), "CONTROL: the word fits without STALE");
    let at = h.get_by_label(GROUP_MORE).rect().center();
    h.event(Event::PointerMoved(at));
    press(&h, at, true);
    h.step();
    h.state_mut().stale = true;
    h.step();
    h.step();
    press(&h, at, false);
    h.run();
    assert!(!has_word(&h), "CONTROL: STALE took the word's room under the press");
    assert_eq!(h.state().state.group, next_group(1), "the press grouped");
    assert!(h.query_by_label("STALE").is_some(), "and STALE is what is in the toolbar now");
}

/// The design's toolbar: the Vol toggle at the LEFT, and at the RIGHT, left to right, the word
/// `Group`, the − button, the grouping's number, the + button and Recenter. (The app had Group
/// at the left and no Vol toggle.) Every button keeps the density's control height as a click
/// target, whatever size it is drawn.
#[test]
fn the_toolbar_is_the_designs_volume_at_the_left_and_the_grouping_and_recenter_at_the_right() {
    let h = settled(Setup::default());
    let (_, t) = h.state().drawn.expect("the ladder drew");
    let x = |name: &str| h.get_by_label(name).rect();
    let (volume, less, more, recenter) =
        (x(VOLUME_SHOW), x(GROUP_LESS), x(GROUP_MORE), x(RECENTER));
    let at = |l: &str| {
        labels_at(&h).into_iter().find(|(s, _)| s == l).unwrap_or_else(|| panic!("{l}")).1
    };
    let (word, number) = (at("Group"), at("1"));
    assert!(volume.max.x < word.min.x, "Vol at the left, before everything else");
    assert!(word.max.x <= less.min.x && less.max.x <= number.min.x);
    assert!(number.max.x <= more.min.x && more.max.x <= recenter.min.x);
    let ladder = rows_rect(&h);
    assert!((recenter.max.x - ladder.max.x).abs() < 0.5, "Recenter is the last thing");
    for (name, r) in
        [(VOLUME_SHOW, volume), (GROUP_LESS, less), (GROUP_MORE, more), (RECENTER, recenter)]
    {
        assert!((r.height() - t.metrics.control_height).abs() < 0.5, "{name}: {r:?}");
    }
    // The toolbar and the captions are set off by a hairline each: the rows start under both.
    let (area, t) = h.state().drawn.expect("the ladder drew");
    let p = parts(area, &t.metrics, true);
    assert_eq!(p.toolbar.height(), t.metrics.control_height + stroke::HAIRLINE);
    assert_eq!(p.head.height(), t.metrics.row_height + stroke::HAIRLINE);
    assert_eq!(p.rows.min.y, p.head.max.y);
}

/// Under the ticket there is no Vol column and so no button for it, and no `Vol` caption even
/// where the state says on: the column is the side layout's.
#[test]
fn under_the_ticket_there_is_no_volume_button_and_no_volume_column() {
    let mut h = settled(Setup { panel: TradeWindowTicketPlace::Under, ..Setup::default() });
    h.state_mut().state.volume = true;
    h.run();
    assert!(h.query_by_label(VOLUME_SHOW).is_none() && h.query_by_label(VOLUME_HIDE).is_none());
    assert!(!painted(&h).iter().any(|s| s == "Vol" || s == NO_VOLUME), "{:?}", painted(&h));
}

/// The Vol toggle (the design's first toolbar button): off, the column and its caption are
/// not there; a click turns it on — the caption appears, the button reads pressed and offers to
/// hide it — and a second click removes both again.
#[test]
fn the_volume_toggle_shows_and_hides_the_column_and_its_caption() {
    let mut h = settled(Setup::default());
    let has = |h: &Harness<'static, Scene>, word: &str| painted(h).iter().any(|s| s == word);
    assert!(!h.state().state.volume, "off until the trader asks");
    assert!(!has(&h, "Vol") && !has(&h, NO_VOLUME), "off: no caption, no column");
    assert_eq!(h.get_by_label(VOLUME_SHOW).accesskit_node().toggled(), Some(Toggled::False));

    h.get_by_label(VOLUME_SHOW).click();
    h.run();
    assert!(h.state().state.volume);
    assert!(has(&h, "Vol"), "on: the caption");
    assert_eq!(h.get_by_label(VOLUME_HIDE).accesskit_node().toggled(), Some(Toggled::True));

    h.get_by_label(VOLUME_HIDE).click();
    h.run();
    assert!(!h.state().state.volume);
    assert!(!has(&h, "Vol") && !has(&h, NO_VOLUME), "off again: no caption, no column");
}

/// With the column on and nothing on the tape, every row prints a dash in `text_tertiary` and no number:
/// the column is honest about having nothing. With prints, the rows they fell in print their
/// volume (the lot's decimals, summed by the row) and every other row stays a dash.
#[test]
fn the_volume_column_prints_dashes_with_no_tape_and_only_what_the_tape_holds_with_one() {
    let mut h = settled(Setup::default());
    h.state_mut().state.volume = true;
    h.run();
    let n_rows = {
        let (_, t) = h.state().drawn.expect("the ladder drew");
        row_count(rows_rect(&h).height(), t.metrics.row_height)
    };
    let dashes =
        |h: &Harness<'static, Scene>| painted(h).iter().filter(|s| *s == NO_VOLUME).count();
    assert!(n_rows >= 10, "a ladder with rows to count: {n_rows}");
    assert_eq!(dashes(&h), n_rows, "no data: a dash in every row");

    // 100 and 99 are rows of the book; two prints at 100 make one row's 2.5, summed.
    h.state_mut().tape = vec![print(100.0, 2.0), print(100.0, 0.5), print(99.0, 4.25)];
    h.run();
    let texts = painted(&h);
    assert!(texts.iter().any(|s| s == "2.500") && texts.iter().any(|s| s == "4.250"), "{texts:?}");
    assert_eq!(dashes(&h), n_rows - 2, "the rows no print fell in stay dashes");
    // The grey of the numbers is the theme's caption grey, the dash's too.
    let (_, t) = h.state().drawn.expect("the ladder drew");
    let grey: Vec<egui::Color32> = h
        .output()
        .shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Text(s) if s.galley.text() == NO_VOLUME => {
                Some(s.galley.job.sections[0].format.color)
            }
            _ => None,
        })
        .collect();
    assert!(grey.iter().all(|c| *c == t.theme.text_tertiary), "{grey:?}");
}

/// [`TradeWindowInputs::tape`] sums into the rows by the grouping, as the book does, and a print that
/// is not a trade (a price or a size that is not a number, a size of zero or less) is left out.
#[test]
fn the_tape_is_summed_into_the_rows_by_the_grouping() {
    let tape = [
        print(100.0, 1.0),
        print(100.1, 2.0),
        print(100.2, 4.0),
        print(f64::NAN, 9.0),
        print(100.0, -5.0),
        print(100.0, f64::NAN),
        print(100.0, 0.0),
    ];
    let one = group_tape(&tape, 0.1, 1);
    assert_eq!(one.len(), 3);
    assert_eq!(one[&row_key(100.1, 0.1, 1)], 2.0);
    let two = group_tape(&tape, 0.1, 2);
    assert_eq!(two.len(), 2, "{two:?}");
    assert_eq!(two[&row_key(100.0, 0.1, 2)], 3.0, "100.0 and 100.1 are one row at two ticks");
    assert_eq!(two[&row_key(100.2, 0.1, 2)], 4.0);
    assert!(group_tape(&[], 0.1, 1).is_empty());
}

/// The depth prints to the LOT's decimals: `1.000` on a lot of 0.001 (the sizes of the book
/// are 1, 2 and 3), `1.00` on a lot of 0.01, and three decimals while the lot is not known —
/// never the size tiers' `1.00` and `0.7050`.
#[test]
fn the_depth_prints_the_lots_decimals() {
    for (lot, want, not) in [
        (0.001, ["1.000", "2.000", "3.000"], "1.00"),
        (0.01, ["1.00", "2.00", "3.00"], "1.000"),
        (0.0, ["1.000", "2.000", "3.000"], "1.00"),
    ] {
        let h = settled(Setup {
            grid: Grid { lot_size: grid_value(lot), min_qty: grid_value(lot), ..GRID },
            ..Setup::default()
        });
        let texts = painted(&h);
        for w in want {
            assert!(texts.iter().any(|s| s == w), "lot {lot}: {w:?} in {texts:?}");
        }
        assert!(!texts.iter().any(|s| s == not), "lot {lot}: {not:?} in {texts:?}");
    }
}

/// A bookless ladder reads its OWN link through `instrument_bar::link_of`, as the FEED badge does
/// (W2 review minor 7): a venue with no line is a link nobody reported on, so it shows the tray —
/// never the unreachable cloud, which would tell the trader a link is down that nobody said
/// anything about.
#[test]
fn a_bookless_ladder_whose_link_nobody_reported_shows_the_tray_not_the_cloud() {
    let mut h = scene(Setup::default());
    h.state_mut().book = L2Book::new(1.0);
    h.run();
    assert!(h.query_by_label("empty").is_some(), "the tray");
    assert!(h.query_by_label("unreachable").is_none(), "not the cloud");
}

/// Every text the last frame painted.
fn painted(h: &Harness<'static, Scene>) -> Vec<String> {
    h.output()
        .shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Text(t) => Some(t.galley.text().to_string()),
            _ => None,
        })
        .collect()
}

/// Every `Role::Label` value on screen (egui files a label's text under its VALUE).
fn labels(h: &Harness<'static, Scene>) -> Vec<String> {
    h.root()
        .children_recursive()
        .filter(|n| n.accesskit_node().role() == Role::Label)
        .map(|n| n.accesskit_node().value().as_deref().unwrap_or("").to_string())
        .collect()
}

/// [`labels`], each with the rect of its node.
fn labels_at(h: &Harness<'static, Scene>) -> Vec<(String, Rect)> {
    h.root()
        .children_recursive()
        .filter(|n| n.accesskit_node().role() == Role::Label)
        .map(|n| (n.accesskit_node().value().as_deref().unwrap_or("").to_string(), n.rect()))
        .collect()
}

#[cfg(test)]
mod press;
