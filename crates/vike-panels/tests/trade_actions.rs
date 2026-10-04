//! What the Trade window's clicks produce: accessibility-tree tests over the real `trade::draw`,
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
use vike_model::{BookLevel, L2Book, VenueCaps};
use vike_panels::trade::{
    self, AccountMode, AccountRow, Exits, Grid, LadderOrder, OrderType, Origin, Panel, Position,
    SizeUnit, StatusKind, StatusLine, Tradable, TradeAction, TradeInputs, TradeState, layout,
    ticket,
};
use vike_ui_theme::appearance::Appearance;

/// A BTC perpetual's grid: a 0.1 tick and a 0.001 lot. The window seeds its size from the middle
/// quick size, ten lots: `0.010`.
const BTC: Grid = Grid { tick: 0.1, lot: 0.001, min_qty: 0.001 };
/// The grid of an instrument whose catalog row has not arrived: no lot, so no size.
const NO_LOT: Grid = Grid { tick: 0.1, lot: 0.0, min_qty: 0.0 };
/// What an older node's window says about its own orders (Ruling R9): test words, not the app's.
const UNATTRIBUTED: &str = "This node does not say which account each order belongs to.";

/// The scene one test draws.
#[derive(Clone)]
struct Scene {
    mode: AccountMode,
    tradable: bool,
    account: Option<&'static str>,
    /// Why a bracket cannot reach this account, as the app states it; `None` where one can.
    bracket_why: Option<&'static str>,
    /// Whether the node can carry a bracket at all.
    bracket_wire: bool,
    grid: Grid,
    last: Option<f64>,
    /// `false` draws the window over an empty book.
    book: bool,
    /// Draw [`book`]'s one side only: `+1` its bids, `−1` its asks. The other side has no price.
    one_side: Option<i32>,
    orders: Vec<LadderOrder>,
    orders_why: Option<&'static str>,
    accounts: Vec<AccountRow<'static>>,
    /// What the account trades, which an untradable window names.
    trades: Vec<String>,
    /// The cause the app STATES for an untradable window (`Tradable::No::why`): `None` where the
    /// account simply trades other symbols.
    why: Option<&'static str>,
    /// The strip's line, as the app composes it.
    status: Option<(StatusKind, &'static str)>,
    /// A book round this mid instead (`book_around`), which a test may move between frames.
    mid: Option<f64>,
    /// The open position, which a test may open or close between frames (a fill).
    position: Option<Position>,
    /// The account's free buying power.
    buying_power: Option<f64>,
    /// The appearance the window is drawn under, installed on the first frame.
    look: Appearance,
}

struct Fixture {
    state: TradeState,
    scene: Scene,
    emitted: Vec<TradeAction>,
    /// The content rect the last frame drew into.
    content: egui::Rect,
    /// The strip's height as `status::height` gives it for the last frame's state and inputs.
    strip_h: f32,
}

/// Bids 99.9 and 99.8, asks 100.0 and 100.1.
fn book() -> L2Book {
    let mut b = L2Book::new(0.1);
    b.apply_snapshot(
        1,
        &[BookLevel::new(99.9, 1.0), BookLevel::new(99.8, 2.0)],
        &[BookLevel::new(100.0, 1.0), BookLevel::new(100.1, 2.0)],
    );
    b
}

/// A DEMO window on the venue's single default account: one-click on, and TP/SL can be used.
fn demo() -> Scene {
    Scene {
        mode: AccountMode::Demo,
        tradable: true,
        account: None,
        bracket_why: None,
        bracket_wire: true,
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
        mid: None,
        position: Some(Position { size: 0.05, avg_px: 99.0, upnl: 0.05 }),
        buying_power: Some(10_000.0),
        look: Appearance::default(),
    }
}

/// [`book`]'s bids alone (`side` `+1`) or its asks alone (`−1`).
fn one_side_of_book(side: i32) -> L2Book {
    let mut b = L2Book::new(0.1);
    let bids = [BookLevel::new(99.9, 1.0), BookLevel::new(99.8, 2.0)];
    let asks = [BookLevel::new(100.0, 1.0), BookLevel::new(100.1, 2.0)];
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
        &[BookLevel::new(mid - 0.05, 1.0), BookLevel::new(mid - 0.06, 2.0)],
        &[BookLevel::new(mid + 0.05, 1.0), BookLevel::new(mid + 0.06, 2.0)],
    );
    b
}

/// The same window on a LIVE account: one-click starts off.
fn live() -> Scene {
    Scene { mode: AccountMode::Live, ..demo() }
}

/// The whole window, beside layout, at the size it opens at.
fn harness(scene: Scene) -> Harness<'static, Fixture> {
    harness_at(scene, layout::BESIDE_SIZE, Panel::Beside)
}

/// The whole window at `size`, the ticket in `panel`.
fn harness_at(scene: Scene, size: egui::Vec2, panel: Panel) -> Harness<'static, Fixture> {
    let (two_sided, empty) = (book(), L2Book::new(0.1));
    let (bids_only, asks_only) = (one_side_of_book(1), one_side_of_book(-1));
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
            let inputs = TradeInputs {
                venue: "binance",
                venue_label: "Binance",
                account: s.account,
                symbol: "BTCUSDT",
                base: "BTC",
                quote: "USDT",
                mode: s.mode,
                tradable: if s.tradable {
                    Tradable::Yes
                } else {
                    Tradable::No { trades: &s.trades, why: s.why }
                },
                grid: s.grid,
                book,
                last: s.last,
                stale: false,
                source: "datahub 127.0.0.1:7878 — 1/1 stream(s) live",
                absence: None,
                orders: &s.orders,
                orders_why: s.orders_why,
                position: s.position,
                buying_power: s.buying_power,
                caps: VenueCaps::UNSUPPORTED,
                bracket_why: s.bracket_why,
                bracket_wire: s.bracket_wire,
                matches: &[],
                recent: &[],
                accounts: &s.accounts,
                accounts_why: None,
                unconnected: &[],
                status: s.status.map(|(kind, text)| StatusLine { kind, text }),
            };
            let content = ui.available_rect_before_wrap();
            let acts = trade::draw(ui, &mut f.state, &inputs);
            f.emitted.extend(acts);
            let t = vike_ui_theme::components::Tokens::of(ui.ctx());
            f.content = content;
            f.strip_h = trade::status::height(ui.ctx(), &t, &f.state, &inputs, content.width());
        },
        Fixture {
            state: TradeState::default(),
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
fn set(h: &mut Harness<'static, Fixture>, change: impl FnOnce(&mut TradeState)) {
    change(&mut h.state_mut().state);
    h.run();
    h.state_mut().emitted.clear();
}

fn places(f: &Fixture) -> Vec<&TradeAction> {
    f.emitted.iter().filter(|a| matches!(a, TradeAction::Place { .. })).collect()
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
fn state_with(change: impl FnOnce(&mut TradeState)) -> TradeState {
    let mut s = TradeState::default();
    change(&mut s);
    s
}

/// The inputs of a DEMO window on `grid`, without a frame: for `ticket::order` called directly.
fn inputs_on(book: &L2Book, grid: Grid) -> TradeInputs<'_> {
    TradeInputs {
        venue: "binance",
        venue_label: "Binance",
        account: None,
        symbol: "BTCUSDT",
        base: "BTC",
        quote: "USDT",
        mode: AccountMode::Demo,
        tradable: Tradable::Yes,
        grid,
        book,
        last: Some(100.0),
        stale: false,
        source: "",
        absence: None,
        orders: &[],
        orders_why: None,
        position: None,
        buying_power: Some(10_000.0),
        caps: VenueCaps::UNSUPPORTED,
        bracket_why: None,
        bracket_wire: true,
        matches: &[],
        recent: &[],
        accounts: &[],
        accounts_why: None,
        unconnected: &[],
        status: None,
    }
}

/// The Buy button sends a limit at the typed price (one-click is on for DEMO).
#[test]
fn the_ticket_buy_sends_a_limit_at_the_typed_price() {
    let mut h = settled(demo());
    set(&mut h, |s| s.price = "99.5".to_string());
    h.get_by_label_contains("Buy 0.010 limit").click();
    h.run();
    match places(h.state()).as_slice() {
        [
            TradeAction::Place {
                side: 1,
                order_type: OrderType::Limit,
                price: Some(p),
                qty,
                reduce_only: false,
                exits: None,
                origin: Origin::Ticket,
            },
        ] => {
            assert!((p - 99.5).abs() < 1e-9 && (qty - 0.01).abs() < 1e-12, "{p} {qty}");
        }
        other => panic!("one limit buy, got {other:?}"),
    }
}

/// One-click starts on for DEMO and off for LIVE; on LIVE a click is HELD for a confirm, and the
/// confirm sends exactly the order that was held.
#[test]
fn a_live_account_holds_every_order_for_a_confirm() {
    let mut h = settled(live());
    assert!(!h.state().state.one_click, "LIVE starts with one-click off");
    set(&mut h, |s| s.price = "99.5".to_string());
    h.get_by_label_contains("Buy 0.010 limit").click();
    h.run();
    assert!(places(h.state()).is_empty(), "nothing is sent before the confirm");
    let held = h.state().state.held.clone().expect("the order is held");
    click(&mut h, "Place");
    assert_eq!(places(h.state()), [&held], "the confirm sends the held order");
}

/// Changing the instrument drops a held order, so a confirm can never send it to the new one
/// (Review Focus 4).
#[test]
fn an_instrument_change_drops_the_held_order() {
    let mut h = settled(live());
    h.state_mut().state.held = Some(TradeAction::CancelAll);
    h.state_mut().scene.account = Some("SUB");
    h.run();
    assert!(h.state().state.held.is_none());
}

/// A symbol the account does not trade produces no order, and the ticket says why (Review Focus 2,
/// pre-flight I8's ticket half). POSITIVE CONTROL first: the same click on the same window, where
/// the account does trade the symbol, places the order, so the untradable half cannot pass by
/// clicking nothing.
#[test]
fn an_untradable_symbol_produces_no_order_and_says_why() {
    let reason = "This account does not trade BTCUSDT. It trades ETHUSDT.";
    for tradable in [true, false] {
        let mut h = settled(Scene { tradable, ..demo() });
        set(&mut h, |s| s.price = "99.5".to_string());
        for side in ["Buy 0.010 limit", "Sell 0.010 limit"] {
            let button = h.get_by_label(side);
            assert_eq!(button.accesskit_node().is_disabled(), !tradable, "{side}, {tradable}");
            button.click();
            h.run();
        }
        if tradable {
            assert_eq!(places(h.state()).len(), 2, "the positive control: {:?}", h.state().emitted);
        } else {
            assert!(h.state().emitted.is_empty(), "{:?}", h.state().emitted);
            assert!(h.state().state.held.is_none(), "nothing is held either");
            assert!(h.query_all_by_label(reason).next().is_some(), "the reason is on screen");
            assert!(says_on_hover(&mut h, "Buy 0.010 limit", reason), "and on the Buy button");
        }
    }
}

/// Post only and Leverage are drawn, disabled, with the owner's words on hover, and a click on
/// either produces no intent at all (spec §§3.7 and 7, Ruling R2, pre-flight Minor 13).
#[test]
fn post_only_and_leverage_are_drawn_disabled_say_why_and_send_nothing() {
    let mut h = settled(demo());
    for (name, why) in [("Post only", ticket::POST_WHY), ("Leverage", ticket::LEVERAGE_WHY)] {
        assert!(disabled(&h, name), "{name} is disabled");
        assert!(says_on_hover(&mut h, name, why), "{name}: the owner's words show on hover");
        click(&mut h, name);
        assert!(h.state().emitted.is_empty(), "{name}: {:?}", h.state().emitted);
        assert!(h.state().state.held.is_none(), "{name}: nothing is held");
    }
}

/// An account the settings database holds but the server does not run is listed greyed, says why
/// on hover, and takes no click. This is spec §3.3 and the owner's ruling of 2026-09-30 that the
/// list comes from the database.
#[test]
fn a_greyed_account_row_says_why_and_takes_no_click() {
    let row = |account, name, why_not| AccountRow {
        venue: "binance",
        venue_label: "Binance",
        account,
        symbol: "BTCUSDT",
        name,
        mode: AccountMode::Demo,
        why_not,
    };
    let accounts = vec![
        row(None, "main", None),
        row(Some("HEDGE"), "HEDGE", Some("Not running on the server.")),
    ];
    let mut h = settled(Scene { accounts, ..demo() });
    h.get_by_label_contains("Binance · main").click();
    h.run();
    assert!(
        says_on_hover(&mut h, "Binance · HEDGE", "Not running on the server."),
        "the reason shows on hover"
    );
    click(&mut h, "Binance · HEDGE");
    assert!(
        !h.state().emitted.iter().any(|a| matches!(a, TradeAction::PickAccount { .. })),
        "a greyed row picks nothing: {:?}",
        h.state().emitted
    );
}

/// A change to a ticket's state.
type Tweak = fn(&mut TradeState);

/// Every place TP/SL is refused, with its own words: the node, a named account, the default
/// account of a venue that runs a second one, a Stop entry, reduce-only (Rulings R3 and R8, spec
/// §3.6).
fn refused_tpsl() -> [(&'static str, Scene, Tweak, &'static str); 5] {
    [
        (
            "a named account",
            Scene { account: Some("SUB"), bracket_why: Some(ticket::TPSL_ACCOUNT_WHY), ..demo() },
            |_| {},
            ticket::TPSL_ACCOUNT_WHY,
        ),
        (
            "the default account of a venue that runs a second one",
            Scene { bracket_why: Some(ticket::TPSL_ACCOUNT_WHY), ..demo() },
            |_| {},
            ticket::TPSL_ACCOUNT_WHY,
        ),
        ("a Stop entry", demo(), |s| s.order_type = OrderType::Stop, ticket::TPSL_STOP_WHY),
        ("reduce-only", demo(), |s| s.reduce_only = true, ticket::TPSL_REDUCE_WHY),
        (
            "a node with no wire form for a bracket, which outranks the account",
            Scene {
                account: Some("SUB"),
                bracket_why: Some(ticket::TPSL_ACCOUNT_WHY),
                bracket_wire: false,
                ..demo()
            },
            |_| {},
            ticket::TPSL_WIRE_WHY,
        ),
    ]
}

/// TP/SL is offered on a venue's single default book for a Market or Limit entry (the POSITIVE
/// CONTROL, pre-flight Minor 12: a toggle that is always disabled fails here). Everywhere else,
/// while it is OFF, it cannot be turned on: it is disabled and says why.
#[test]
fn tpsl_is_offered_only_on_a_single_default_book_for_a_market_or_limit_entry() {
    for order_type in [OrderType::Limit, OrderType::Market] {
        let mut h = settled(demo());
        set(&mut h, |s| s.order_type = order_type);
        assert!(!disabled(&h, "TP/SL"), "{order_type:?}: TP/SL is offered");
    }
    for (what, scene, tweak, why) in refused_tpsl() {
        let mut h = settled(scene);
        set(&mut h, tweak);
        let toggle = h.get_by_label("TP/SL").accesskit_node();
        assert!(toggle.is_disabled(), "{what}: TP/SL cannot be turned on");
        assert_eq!(toggle.toggled(), Some(Toggled::False), "{what}");
        assert!(says_on_hover(&mut h, "TP/SL", why), "{what}: {why}");
    }
}

/// Fix round 1, I-3 (RULED): TP/SL TICKED where it is refused refuses the ORDER, never sends it
/// without its exits. The toggle stays ticked (it is the trader's setting), is enabled only to be
/// turned off, and the reason is written under it, not only on hover — and, since FW6 (I3), the
/// status strip says it too, where it said "Ready". Buy and Sell send nothing and hold nothing;
/// once TP/SL is off the same order goes, plain.
#[test]
fn a_ticked_tpsl_that_is_refused_refuses_the_order_until_it_is_turned_off() {
    for (what, scene, tweak, why) in refused_tpsl() {
        let mut h = settled(scene);
        set(&mut h, |s| {
            tweak(s);
            s.price = "100".to_string();
            s.tpsl = true;
        });
        let refusal = ticket::tpsl_refusal(why);
        assert!(refusal.contains("Turn TP/SL off"), "{refusal}");
        let toggle = h.get_by_label("TP/SL").accesskit_node();
        assert!(!toggle.is_disabled(), "{what}: it can be turned off");
        assert_eq!(toggle.toggled(), Some(Toggled::True), "{what}: the trader's setting shows");
        let written = |region| labels_in(&h, region).contains(&refusal);
        assert!(written("Order ticket"), "{what}: the reason is written, not hovered");
        assert!(written("Status strip"), "{what}: and the strip says it, not Ready");
        let kind = if h.state().state.order_type == OrderType::Stop { "stop" } else { "limit" };
        let (buy, sell) = (format!("Buy 0.010 {kind}"), format!("Sell 0.010 {kind}"));
        for button in [&buy, &sell] {
            assert!(disabled(&h, button), "{what}: {button}");
            click(&mut h, button);
        }
        assert!(h.state().emitted.is_empty(), "{what}: {:?}", h.state().emitted);
        assert!(h.state().state.held.is_none(), "{what}: nothing is held either");
        assert!(says_on_hover(&mut h, &buy, &refusal), "{what}: on the button");
        click(&mut h, "TP/SL");
        assert!(!h.state().state.tpsl, "{what}: the click turned it off");
        assert!(disabled(&h, "TP/SL"), "{what}: and it cannot be turned back on here");
        click(&mut h, &buy);
        match places(h.state()).as_slice() {
            [TradeAction::Place { side: 1, exits: None, .. }] => {}
            other => panic!("{what}: one plain order once TP/SL is off, got {other:?}"),
        }
    }
}

/// Fix round 1, I-3 case (a): a trader with TP/SL set moves the window to a named account. The
/// setting survives the move, and the order is then REFUSED rather than sent without its exits.
#[test]
fn a_move_to_a_named_account_keeps_tpsl_and_refuses_the_order() {
    let mut h = settled(live());
    set(&mut h, |s| s.tpsl = true);
    h.state_mut().scene.account = Some("SUB");
    h.state_mut().scene.bracket_why = Some(ticket::TPSL_ACCOUNT_WHY);
    h.run();
    set(&mut h, |s| s.price = "100".to_string());
    assert!(h.state().state.tpsl, "the trader's setting survives the move");
    assert!(disabled(&h, "Buy 0.010 limit"));
    click(&mut h, "Buy 0.010 limit");
    assert!(h.state().emitted.is_empty(), "{:?}", h.state().emitted);
    assert!(h.state().state.held.is_none(), "nothing is held for a confirm either");
    let refusal = ticket::tpsl_refusal(ticket::TPSL_ACCOUNT_WHY);
    assert!(says_on_hover(&mut h, "Buy 0.010 limit", &refusal));
}

/// Fix round 1, I-3: the confirm prompt says what the held order carries: its exits, or that it
/// has none.
#[test]
fn the_confirm_prompt_names_the_exits_or_says_there_are_none() {
    for (tpsl, words) in [(true, "TP 100.5 / SL 99.7"), (false, "no TP/SL")] {
        let mut h = settled(live());
        set(&mut h, |s| {
            s.price = "100".to_string();
            s.tpsl = tpsl;
        });
        click(&mut h, "Buy 0.010 limit");
        assert!(h.state().state.held.is_some(), "{tpsl}: held");
        assert!(h.query_all_by_label_contains(words).next().is_some(), "{tpsl}: {words}");
    }
}

/// Pre-flight Minor 8: the reasons rank wire > account > Stop entry > reduce-only.
#[test]
fn the_tpsl_refusals_rank_node_account_entry_reduce_only() {
    let b = book();
    let all = state_with(|s| s.reduce_only = true);
    let free = state_with(|_| {});
    let i = |wire, why| TradeInputs { bracket_wire: wire, bracket_why: why, ..inputs_on(&b, BTC) };
    let (stop, stated) = (OrderType::Stop, Some(ticket::TPSL_ACCOUNT_WHY));
    assert_eq!(ticket::tpsl_block(&all, &i(false, stated), stop), Some(ticket::TPSL_WIRE_WHY));
    assert_eq!(ticket::tpsl_block(&all, &i(true, stated), stop), stated);
    assert_eq!(ticket::tpsl_block(&all, &i(true, None), stop), Some(ticket::TPSL_STOP_WHY));
    let limit = OrderType::Limit;
    assert_eq!(ticket::tpsl_block(&all, &i(true, None), limit), Some(ticket::TPSL_REDUCE_WHY));
    assert_eq!(ticket::tpsl_block(&free, &i(true, None), limit), None, "the positive control");
    // The stated cause is said as stated, whatever it is: the glue's lane reason, for one.
    let lane = "TP/SL is not available on this account yet: its engine trades a spot market.";
    assert_eq!(ticket::tpsl_block(&free, &i(true, Some(lane)), limit), Some(lane));
}

/// Fix round 1, I-1: a held order is dropped, and the window says so, the moment the window can
/// no longer trade (the account stops trading the symbol, halts or faults): its Place must never
/// reach a venue past that. POSITIVE CONTROL: the same steps on a window that still trades keep
/// the order held.
#[test]
fn a_held_order_is_dropped_when_the_window_can_no_longer_trade() {
    for still_tradable in [true, false] {
        let mut h = settled(live());
        set(&mut h, |s| s.price = "99.5".to_string());
        click(&mut h, "Buy 0.010 limit");
        assert!(h.state().state.held.is_some(), "held for a confirm");
        h.state_mut().emitted.clear();
        h.state_mut().scene.tradable = still_tradable;
        h.run();
        if still_tradable {
            assert!(h.state().state.held.is_some(), "the positive control keeps it");
            assert!(h.query_by_label("Place").is_some());
            assert!(h.state().emitted.is_empty(), "{:?}", h.state().emitted);
        } else {
            assert!(h.state().state.held.is_none(), "dropped");
            assert!(h.query_by_label("Place").is_none(), "nothing left to confirm");
            match h.state().emitted.as_slice() {
                [TradeAction::Note { kind: StatusKind::Error, text }] => {
                    assert!(text.starts_with("Not sent:"), "{text}");
                }
                other => panic!("one error note, got {other:?}"),
            }
        }
    }
}

/// Fix round 1, I-2 (RULED): a typed price off the instrument's tick is REFUSED, never sent as
/// typed (the core would round it, past the trader's limit, and its exits would be measured from
/// a price the order does not sit at). A tick multiple is sent, whatever float noise the tick
/// carries; a tick nobody gave is not checked (no lot is known then either, so nothing is sent).
#[test]
fn an_off_tick_price_is_refused_and_a_tick_multiple_is_sent() {
    let b = book();
    let on = |tick| inputs_on(&b, Grid { tick, ..BTC });
    let limit = |price: &str, tpsl: bool| {
        let price = price.to_string();
        state_with(move |s| {
            s.size = "0.010".to_string();
            s.price = price;
            s.tpsl = tpsl;
        })
    };
    for (price, tpsl) in [("100.4", true), ("100.6", false)] {
        assert_eq!(
            ticket::order(1, &limit(price, tpsl), &on(1.0)),
            Err(format!("{price} is not on the 1 tick: use a multiple of 1.")),
            "{price}"
        );
    }
    let stop = state_with(|s| {
        s.size = "0.010".to_string();
        s.price = "100.6".to_string();
        s.order_type = OrderType::Stop;
    });
    assert!(ticket::order(1, &stop, &on(1.0)).is_err(), "a Stop's trigger too");
    for (tick, price) in [
        (1.0, "100"),
        (1.0, "101"),
        (0.05, "100.15"),
        (0.05, "0.15"),
        (0.1, "100.3"),
        (0.1, "65432.1"),
        (0.25, "99.75"),
        (0.5, "1234567.5"),
        (0.01, "1234567.89"),
        (1e-8, "0.00001234"),
    ] {
        assert!(
            matches!(
                ticket::order(1, &limit(price, false), &on(tick)),
                Ok(TradeAction::Place { .. })
            ),
            "{price} on a {tick} tick is on the grid: {:?}",
            ticket::order(1, &limit(price, false), &on(tick))
        );
    }
    // On screen: the Buy button refuses and says why, and nothing is sent.
    let mut h = settled(Scene { grid: Grid { tick: 1.0, ..BTC }, ..demo() });
    set(&mut h, |s| {
        s.price = "100.4".to_string();
        s.tpsl = true;
    });
    assert!(disabled(&h, "Buy 0.010 limit"));
    click(&mut h, "Buy 0.010 limit");
    assert!(h.state().emitted.is_empty(), "{:?}", h.state().emitted);
    let why = "100.4 is not on the 1 tick: use a multiple of 1.";
    assert!(says_on_hover(&mut h, "Buy 0.010 limit", why));
}

/// Fix round 1, I-4: a size typed in the quote currency converts at the price EACH order is placed
/// at: a market buy at the ask, a market sell at the bid, a limit at its own price, and never at
/// the last trade (95 here, which would buy 10.526). The button names what the order sends.
#[test]
fn a_quote_size_converts_at_the_price_each_order_is_placed_at() {
    let mut h = settled(Scene { last: Some(95.0), ..demo() });
    set(&mut h, |s| {
        s.unit = SizeUnit::Quote;
        s.size = "1000".to_string();
        s.order_type = OrderType::Market;
    });
    for (button, side, want) in [("Buy 10.000 market", 1, 10.0), ("Sell 10.010 market", -1, 10.01)]
    {
        click(&mut h, button);
        match places(h.state()).as_slice() {
            [TradeAction::Place { side: s, order_type: OrderType::Market, qty, .. }] => {
                assert!(*s == side && (qty - want).abs() < 1e-9, "{button}: {qty}");
            }
            other => panic!("{button}: one market order, got {other:?}"),
        }
        h.state_mut().emitted.clear();
    }
    set(&mut h, |s| {
        s.order_type = OrderType::Limit;
        s.price = "80".to_string();
    });
    click(&mut h, "Buy 12.500 limit");
    match places(h.state()).as_slice() {
        [TradeAction::Place { price: Some(p), qty, .. }] => {
            assert!((p - 80.0).abs() < 1e-9 && (qty - 12.5).abs() < 1e-9, "{p} {qty}");
        }
        other => panic!("one limit at 80, got {other:?}"),
    }
}

/// Fix round 1, Minors 3 and 4: a size too large to be a number of lots, and a size under the
/// instrument's smallest order, are refused with a reason naming what is wrong; neither reaches a
/// `Place`.
#[test]
fn a_size_too_large_or_under_the_smallest_order_is_refused() {
    let b = book();
    let btc = inputs_on(&b, BTC);
    let sized = |size: &str| {
        let size = size.to_string();
        state_with(move |s| {
            s.size = size;
            s.price = "100".to_string();
        })
    };
    assert_eq!(trade::sizing::floor_to_lot(1e306, 0.001), 0.0, "never infinite lots");
    match ticket::order(1, &sized("1e306"), &btc) {
        Err(why) => assert!(why.contains("too large"), "{why}"),
        other => panic!("refused, got {other:?}"),
    }
    let min = inputs_on(&b, Grid { min_qty: 0.005, ..BTC });
    assert_eq!(
        ticket::order(1, &sized("0.002"), &min),
        Err("Below the smallest order size: enter at least 0.005 BTC.".to_string())
    );
    assert!(matches!(ticket::order(1, &sized("0.005"), &min), Ok(TradeAction::Place { .. })));
}

/// TP/SL on a limit Buy sends ONE bracket entry with both exits on the tick, and the ticket shows
/// the prices it will send them at, for a buy and for a sell (spec §3.6, pre-flight Minor 16).
#[test]
fn tpsl_on_a_limit_buy_sends_exits_on_the_tick_and_shows_their_prices() {
    let mut h = settled(demo());
    set(&mut h, |s| {
        s.price = "100".to_string();
        s.tpsl = true;
    });
    assert!(h.query_by_label("Buy: TP 100.5 · SL 99.7").is_some(), "a buy's exits are shown");
    assert!(h.query_by_label("Sell: TP 99.5 · SL 100.3").is_some(), "a sell's exits are shown");
    h.get_by_label_contains("Buy 0.010 limit").click();
    h.run();
    match places(h.state()).as_slice() {
        [TradeAction::Place { side: 1, order_type: OrderType::Limit, exits: Some(e), .. }] => {
            assert!(
                (e.take_profit - 100.5).abs() < 1e-9 && (e.stop_loss - 99.7).abs() < 1e-9,
                "{e:?}"
            );
        }
        other => panic!("one bracket entry, got {other:?}"),
    }
}

/// A bracket is BOTH legs or no order. With TP/SL on, a leg that is blank, not a number or not
/// above zero refuses the order with a reason and sends NOTHING; the old ticket sent exactly that
/// order without its exits, as a plain one (carry from the bracket review). The positive control
/// is the test above: both legs set, the bracket goes.
#[test]
fn tpsl_with_a_leg_missing_sends_nothing_not_a_plain_order() {
    for (what, tp, sl) in [
        ("TP blank", "", "0.3"),
        ("SL blank", "0.5", ""),
        ("SL not a number", "0.5", "abc"),
        ("SL zero", "0.5", "0"),
        ("SL below zero", "0.5", "-0.3"),
    ] {
        let mut h = settled(demo());
        set(&mut h, |s| {
            s.price = "100".to_string();
            s.tpsl = true;
            s.tp_text = tp.to_string();
            s.sl_text = sl.to_string();
        });
        assert!(disabled(&h, "Buy 0.010 limit"), "{what}: Buy is refused");
        click(&mut h, "Buy 0.010 limit");
        assert!(h.state().emitted.is_empty(), "{what}: {:?}", h.state().emitted);
        assert!(h.state().state.held.is_none(), "{what}: nothing is held either");
        assert!(says_on_hover(&mut h, "Buy 0.010 limit", ticket::TPSL_LEGS_WHY), "{what}");
    }
}

/// The money path called directly: each refusal carries its reason, and none of them is an order
/// sent without what the trader asked for.
#[test]
fn the_order_refuses_with_its_reason_and_never_drops_an_exit() {
    let b = book();
    let btc = inputs_on(&b, BTC);
    let limit_at_100 = |s: &mut TradeState| {
        s.size = "0.010".to_string();
        s.price = "100".to_string();
    };
    // Both legs set: a bracket entry (the positive control).
    let both = state_with(|s| {
        limit_at_100(s);
        s.tpsl = true;
    });
    match ticket::order(1, &both, &btc) {
        Ok(TradeAction::Place { exits: Some(Exits { take_profit, stop_loss }), .. }) => {
            assert!((take_profit - 100.5).abs() < 1e-9 && (stop_loss - 99.7).abs() < 1e-9);
        }
        other => panic!("a bracket entry, got {other:?}"),
    }
    // One leg missing, not a number or not above zero: refused.
    for (tp, sl) in [("", "0.3"), ("0.5", ""), ("0.5", "abc"), ("0.5", "0"), ("abc", "-1")] {
        let s = state_with(|s| {
            limit_at_100(s);
            s.tpsl = true;
            s.tp_text = tp.to_string();
            s.sl_text = sl.to_string();
        });
        assert_eq!(
            ticket::order(1, &s, &btc),
            Err(ticket::TPSL_LEGS_WHY.to_string()),
            "{tp:?} {sl:?}"
        );
    }
    // No tick grid: an exit would sit off the grid, so no bracket and no order.
    let no_tick = inputs_on(&b, Grid { tick: 0.0, ..BTC });
    assert_eq!(ticket::order(1, &both, &no_tick), Err(ticket::TPSL_ROOM_WHY.to_string()));
    // TP/SL ticked where it is refused (a Stop entry) refuses the ORDER (fix round 1, I-3): it is
    // never sent without the exits the trader set. POSITIVE CONTROL: the same Stop with TP/SL off
    // goes, plain.
    let stop = |tpsl| {
        state_with(move |s| {
            limit_at_100(s);
            s.tpsl = tpsl;
            s.order_type = OrderType::Stop;
        })
    };
    assert_eq!(
        ticket::order(1, &stop(true), &btc),
        Err(ticket::tpsl_refusal(ticket::TPSL_STOP_WHY))
    );
    assert!(matches!(
        ticket::order(1, &stop(false), &btc),
        Ok(TradeAction::Place { order_type: OrderType::Stop, exits: None, .. })
    ));
    // No lot (pre-flight I18): refused, and the reason never prints the lot as "(0)".
    let no_lot = ticket::order(1, &state_with(limit_at_100), &inputs_on(&b, NO_LOT));
    assert_eq!(no_lot, Err(ticket::no_lot_reason("BTCUSDT")));
    assert!(!ticket::no_lot_reason("BTCUSDT").contains("(0)"));
}

/// Showing the ticket under the ladder asks for the bottom-panel window size; hiding the ladder
/// asks for the ticket's own (pre-flight I9: through real clicks, in the app's type, where the
/// plan's version ran the controls on a bare context and could only panic).
#[test]
fn the_view_controls_ask_for_the_new_window_size() {
    struct Controls {
        state: TradeState,
        asked: Vec<Option<egui::Vec2>>,
    }
    let mut h = Harness::builder().with_size(egui::vec2(240.0, 60.0)).build_ui_state(
        |ui, c: &mut Controls| {
            if !vike_ui_theme::harness::appearance_ready(ui.ctx(), &Appearance::default()) {
                return;
            }
            ui.horizontal(|ui| c.asked.push(trade::view_controls(ui, &mut c.state)));
        },
        Controls { state: TradeState::default(), asked: Vec::new() },
    );
    h.run();
    let asked = |h: &mut Harness<'_, Controls>| -> Vec<egui::Vec2> {
        let sizes = h.state().asked.iter().flatten().copied().collect();
        h.state_mut().asked.clear();
        sizes
    };
    assert!(asked(&mut h).is_empty(), "no click, no resize");
    h.get_by_label("Ticket under the ladder").click();
    h.run();
    assert_eq!(asked(&mut h), [layout::UNDER_SIZE]);
    assert_eq!(h.state().state.view.panel, Panel::Under);
    assert_eq!(
        h.get_by_label("Ticket under the ladder").accesskit_node().toggled(),
        Some(Toggled::True),
        "the chosen place reads as pressed"
    );
    h.get_by_label("Hide the ladder").click();
    h.run();
    // The ticket alone asks for its own width and the height its whole form needs in this look
    // (FW5 B1), which at Normal density is more than the spec's 560.
    let alone = layout::window_size(trade::View { ladder: false, panel: Panel::Under }, &h.ctx);
    assert_eq!(asked(&mut h), [alone]);
    assert_eq!(alone.x, layout::TICKET_ONLY_SIZE.x);
    assert!(alone.y > layout::TICKET_ONLY_SIZE.y, "the whole form needs more than {alone:?}");
    assert!(!h.state().state.view.ladder);
    // With the ladder hidden the ticket is alone, beside or under: a move asks for no new size,
    // so a window the trader has sized is left as it is.
    h.get_by_label("Ticket beside the ladder").click();
    h.run();
    assert!(asked(&mut h).is_empty(), "the ticket alone asks for no new size");
    assert_eq!(h.state().state.view.panel, Panel::Beside);
}

fn working(coid: &str, side: i32, price: f64) -> LadderOrder {
    LadderOrder { client_order_id: coid.to_string(), side, price, qty: 0.01, is_stop: false }
}

/// Cancel all, Bids and Asks each send their own cancel at once, even on a LIVE account (a cancel
/// is never held). Where there is nothing to cancel they say so; and on a node that cannot say
/// which account an order belongs to (Ruling R9) every one is disabled with THAT reason and claims
/// no count, where it used to say "No working orders" about orders it could not see (pre-flight
/// Minor 15).
#[test]
fn the_cancel_row_cancels_each_side_and_says_why_it_cannot() {
    let orders = vec![working("b1", 1, 99.0), working("s1", -1, 101.0)];
    let mut h = settled(Scene { orders, ..live() });
    for (label, act) in [
        ("Cancel all 2", TradeAction::CancelAll),
        ("Bids", TradeAction::CancelSide(1)),
        ("Asks", TradeAction::CancelSide(-1)),
    ] {
        click(&mut h, label);
        assert_eq!(h.state().emitted, [act], "{label}");
        h.state_mut().emitted.clear();
    }
    let mut h = settled(Scene { orders_why: Some(UNATTRIBUTED), ..demo() });
    assert!(h.query_by_label("Cancel all 0").is_none(), "no count it cannot know");
    for label in ["Cancel all", "Bids", "Asks"] {
        assert!(disabled(&h, label), "{label}");
        assert!(says_on_hover(&mut h, label, UNATTRIBUTED), "{label}");
    }
    let mut h = settled(demo());
    assert!(disabled(&h, "Cancel all 0"));
    assert!(says_on_hover(&mut h, "Cancel all 0", ticket::NO_ORDERS_WHY));
}

/// The one-click padlock reads as PRESSED while one-click trading is on (the state its name
/// states), and a click turns it off; a LIVE window starts unpressed (pre-flight Minor 17).
#[test]
fn the_padlock_is_pressed_while_one_click_trading_is_on() {
    let lock = |h: &Harness<'_, Fixture>| {
        h.get_by_label_contains("One-click trading").accesskit_node().toggled()
    };
    let mut h = settled(demo());
    assert!(h.state().state.one_click);
    assert_eq!(lock(&h), Some(Toggled::True));
    h.get_by_label_contains("One-click trading").click();
    h.run();
    assert!(!h.state().state.one_click, "the click turned it off");
    assert_eq!(lock(&h), Some(Toggled::False));
    assert_eq!(lock(&settled(live())), Some(Toggled::False), "LIVE starts with it off");
}

/// The compact ticket sends at market without touching the order type the trader chose in the full
/// one (pre-flight Minor 14), and it shows the order's value (spec §3.6, Minor 16).
#[test]
fn the_compact_ticket_sends_at_market_and_keeps_the_traders_order_type() {
    let mut h = harness_at(demo(), layout::UNDER_SIZE, Panel::Under);
    h.run();
    set(&mut h, |s| s.order_type = OrderType::Stop);
    assert_eq!(h.state().state.order_type, OrderType::Stop, "the compact ticket left it alone");
    assert!(h.query_all_by_label_contains("≈ 1.00 USDT").next().is_some(), "the order's value");
    h.get_by_label_contains("Buy 0.010 MKT").click();
    h.run();
    match places(h.state()).as_slice() {
        [TradeAction::Place { side: 1, order_type: OrderType::Market, price: None, qty, .. }] => {
            assert!((qty - 0.01).abs() < 1e-12, "{qty}");
        }
        other => panic!("one market buy, got {other:?}"),
    }
    h.state_mut().state.view.panel = Panel::Beside;
    h.run();
    assert_eq!(h.state().state.order_type, OrderType::Stop, "beside the ladder it is still Stop");
}

/// A window opened before its catalog row arrived has no lot: its quick sizes are disabled and
/// say why rather than read `0`, its size is `0`, and nothing can be sent. Once the lot is known
/// the size is the default, and the order goes (W1 review fix 1b, pre-flight I18).
#[test]
fn a_window_opened_before_its_lot_is_known_takes_the_default_size_once_it_is() {
    let mut h = settled(Scene { grid: NO_LOT, ..demo() });
    set(&mut h, |s| s.price = "99.5".to_string());
    assert_eq!(h.state().state.size, "0");
    let quick: Vec<bool> = h
        .query_all_by_role_and_label(Role::Button, "—")
        .map(|n| n.accesskit_node().is_disabled())
        .collect();
    assert_eq!(quick, [true; 5], "five quick sizes, each disabled, none reading 0");
    h.get_all_by_role_and_label(Role::Button, "—").next().expect("a quick size").hover();
    h.run();
    let why = ticket::no_lot_reason("BTCUSDT");
    assert!(h.query_all_by_label(&why).next().is_some(), "a quick size says why");
    assert!(disabled(&h, "Buy — limit"), "no size, no order");
    click(&mut h, "Buy — limit");
    assert!(h.state().emitted.is_empty(), "{:?}", h.state().emitted);
    h.state_mut().scene.grid = BTC;
    h.run();
    assert_eq!(h.state().state.size, "0.010", "the default size, once the lot is known");
    h.state_mut().emitted.clear();
    click(&mut h, "Buy 0.010 limit");
    assert_eq!(places(h.state()).len(), 1, "{:?}", h.state().emitted);
}

/// The unit switch converts the size and back to the same size; with no price to convert at it is
/// refused, and the size field never holds the dash a quote value without a price prints.
#[test]
fn a_unit_switch_converts_the_size_and_never_writes_a_dash() {
    let mut h = settled(demo());
    click(&mut h, "Enter the size in USDT instead");
    assert_eq!((h.state().state.unit, h.state().state.size.as_str()), (SizeUnit::Quote, "1.00"));
    click(&mut h, "Enter the size in BTC instead");
    assert_eq!((h.state().state.unit, h.state().state.size.as_str()), (SizeUnit::Base, "0.010"));
    let mut h = settled(Scene { last: None, book: false, ..demo() });
    assert!(disabled(&h, "Enter the size in USDT instead"), "no price to convert at");
    click(&mut h, "Enter the size in USDT instead");
    assert_eq!((h.state().state.unit, h.state().state.size.as_str()), (SizeUnit::Base, "0.010"));
}

// ---- W4: the window's own words where it does not trade, and the DOM's last two a11y gates ----

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

/// The causes the app STATES for a window that takes no order (`Tradable::No::why`), each in the
/// words the app's glue puts on its status strip for it, and how that strip reads it (test words
/// here: this crate cannot see the glue; the glue's own tests pin that it hands the widget its
/// strip's words): a stopped core, a halted account, an account text that names no account (each
/// an ERROR there), an account the server does not run and a node that has said nothing yet (each
/// an Info line, which the window's own last order or rejection outranks).
const STATED: [(&str, &str, StatusKind); 5] = [
    ("a stopped core", "The server has stopped trading: a handler panicked.", StatusKind::Error),
    (
        "a halted account",
        "Trading is halted on this account: the server opens nothing, and this window sends \
         nothing.",
        StatusKind::Error,
    ),
    (
        "an account text that names no account",
        "This window's account is not one an order can name, so it sends nothing. Pick an \
         account.",
        StatusKind::Error,
    ),
    ("an account the server does not run", "Not running on the server.", StatusKind::Info),
    ("a node that has said nothing yet", "Waiting for the node.", StatusKind::Info),
];

/// An error on the strip that is NOT why the window takes no order: the window's own last
/// rejection, which outranks an Info cause on the app's strip.
const UNRELATED: &str = "Rejected by the venue: insufficient margin.";

/// Hover at `at` (the pointer first rests where nothing is); whether that ADDED `words` to the
/// screen. For a region with no accessible name of its own: the ladder's rows.
fn says_on_hover_at(h: &mut Harness<'static, Fixture>, at: egui::Pos2, words: &str) -> bool {
    h.hover_at(egui::pos2(1.0, 1.0));
    h.run();
    let before = h.query_all_by_label(words).count();
    h.hover_at(at);
    h.run();
    h.query_all_by_label(words).count() > before
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

/// The F wave's reason slot (W3 review hand-off 3, W4 fix round 2's N6): a window the app makes
/// untradable for a cause it STATES says that cause, in the app's words, in every place a
/// refusal is worded — the full ticket's line, the ladder's hint and the ladder's hover, and the
/// hover of every disabled order control (Buy, Sell, Close, Reverse, TP/SL); under the ladder, the
/// compact ticket's line and Buy and Sell's hovers — the SAME text everywhere, and never "does
/// not trade". The strip carries what the app's strip would: the cause itself for an error, and
/// for an Info cause the window's own last rejection, which outranks it there — so a widget that
/// read the strip for the cause would say the rejection.
#[test]
fn each_stated_cause_is_said_in_its_own_words_everywhere_never_does_not_trade() {
    for (what, cause, kind) in STATED {
        let error = kind == StatusKind::Error;
        let status = if error { cause } else { UNRELATED };
        // A fault or a halt leaves the account's list naming the symbol; the others have no list.
        let trades = if error { vec!["BTCUSDT".to_string()] } else { Vec::new() };
        let scene = Scene {
            tradable: false,
            trades,
            why: Some(cause),
            status: Some((StatusKind::Error, status)),
            ..demo()
        };
        let mut h = settled(scene.clone());
        set(&mut h, |s| s.price = "99.5".to_string());
        let ticket = labels_in(&h, "Order ticket");
        assert!(ticket.iter().any(|l| l == cause), "{what}: the ticket's line: {ticket:?}");
        let ladder = labels_in(&h, "Ladder");
        assert!(ladder.iter().any(|l| l == cause), "{what}: the ladder's hint: {ladder:?}");
        let all: Vec<String> = [ticket, ladder].concat();
        assert!(!all.iter().any(|l| l.contains("does not trade")), "{what}: {all:?}");
        assert!(!all.iter().any(|l| l == UNRELATED), "{what}: the strip's error: {all:?}");
        for control in ["Buy 0.010 limit", "Sell 0.010 limit", "Close", "Reverse", "TP/SL"] {
            assert!(disabled(&h, control), "{what}: {control} is disabled");
            assert!(says_on_hover(&mut h, control, cause), "{what}: {control}'s hover");
        }
        let rows = region_rect(&h, "Ladder").center();
        assert!(says_on_hover_at(&mut h, rows, cause), "{what}: the ladder's hover");
        click(&mut h, "Buy 0.010 limit");
        assert!(h.state().emitted.is_empty(), "{what}: {:?}", h.state().emitted);

        let mut h = harness_at(scene, layout::UNDER_SIZE, Panel::Under);
        h.run();
        let compact = labels_in(&h, "Order ticket");
        assert!(compact.iter().any(|l| l == cause), "{what}: the compact line: {compact:?}");
        assert!(!compact.iter().any(|l| l.contains("does not trade")), "{what}: {compact:?}");
        for control in ["Buy 0.010 MKT", "Sell 0.010 MKT"] {
            assert!(says_on_hover(&mut h, control, cause), "{what}: {control}'s hover");
        }
    }
}

/// W4 fix round 2, N6 — the two wrong cases the reason slot ends. With no cause STATED, an
/// unrelated error on the strip (the window's own last rejection) was shown as "the cause" for an
/// EMPTY list and for a list that NAMES the symbol. Now each says the account cannot trade the
/// symbol now, on the ticket's line, the ladder's hint and Buy's hover, and the rejection stays on
/// the strip, where it belongs. POSITIVE CONTROL: an account that trades OTHER symbols says it
/// does not trade this one, with the same error on the strip — the one case that is true.
#[test]
fn an_unrelated_error_on_the_strip_is_never_said_as_the_cause() {
    let now = "This account cannot trade BTCUSDT now.".to_string();
    for (what, trades, want) in [
        ("an empty list", Vec::new(), now.clone()),
        ("a list naming the symbol", vec!["BTCUSDT".to_string()], now.clone()),
        (
            "CONTROL: a list naming other symbols",
            vec!["ETHUSDT".to_string()],
            "This account does not trade BTCUSDT. It trades ETHUSDT.".to_string(),
        ),
    ] {
        let scene = Scene {
            tradable: false,
            trades,
            status: Some((StatusKind::Error, UNRELATED)),
            ..demo()
        };
        let mut h = settled(scene);
        set(&mut h, |s| s.price = "99.5".to_string());
        let ticket = labels_in(&h, "Order ticket");
        assert!(ticket.contains(&want), "{what}: the ticket says {want:?}: {ticket:?}");
        assert!(!ticket.iter().any(|l| l == UNRELATED), "{what}: {ticket:?}");
        assert!(!ticket.iter().any(|l| l.contains("It trades BTCUSDT")), "{what}: {ticket:?}");
        let ladder = labels_in(&h, "Ladder");
        assert!(ladder.contains(&want), "{what}: the ladder's hint says it: {ladder:?}");
        assert!(says_on_hover(&mut h, "Buy 0.010 limit", &want), "{what}: Buy says it on hover");
        let strip = labels_in(&h, "Status strip");
        assert!(strip.iter().any(|l| l == UNRELATED), "{what}: the strip keeps its line");
        // F2 review minor 9, the dropped assertion: whatever the words, Buy sends nothing.
        click(&mut h, "Buy 0.010 limit");
        assert!(h.state().emitted.is_empty(), "{what}: Buy sends nothing: {:?}", h.state().emitted);
        assert!(h.state().state.held.is_none(), "{what}: and holds nothing");
    }
}

/// W4 fix round 1, I-3: an EMPTY symbol list with no cause stated (on the app: an older node's
/// account that is not its primary market) never proves "does not trade". The ticket, the ladder
/// and every disabled button say the account cannot trade the symbol NOW, whatever Info line the
/// strip shows.
#[test]
fn an_empty_symbol_list_says_cannot_trade_now_never_does_not_trade() {
    let waiting = "Waiting for the node.";
    let scene = Scene {
        tradable: false,
        trades: Vec::new(),
        status: Some((StatusKind::Info, waiting)),
        ..demo()
    };
    let mut h = settled(scene);
    set(&mut h, |s| s.price = "99.5".to_string());
    let now = "This account cannot trade BTCUSDT now.";
    let ticket = labels_in(&h, "Order ticket");
    assert!(ticket.iter().any(|l| l == now), "{ticket:?}");
    assert!(labels_in(&h, "Ladder").iter().any(|l| l == now), "the ladder's hint");
    assert!(labels_in(&h, "Status strip").iter().any(|l| l == waiting), "the strip's own line");
    let all: Vec<String> =
        ["Order ticket", "Ladder"].iter().flat_map(|r| labels_in(&h, r)).collect();
    assert!(!all.iter().any(|l| l.contains("does not trade")), "{all:?}");
    assert!(says_on_hover(&mut h, "Buy 0.010 limit", now), "Buy says it on hover");
}

/// W3 review hand-off 2: the compact ticket WRITES why it takes no order (spec §4.3), as the full
/// one does; under the ladder there is no hint line, so without it the reason was hover-only.
#[test]
fn an_untradable_compact_ticket_writes_why() {
    let reason = "This account does not trade BTCUSDT. It trades ETHUSDT.";
    let mut h = harness_at(Scene { tradable: false, ..demo() }, layout::UNDER_SIZE, Panel::Under);
    h.run();
    let ticket = labels_in(&h, "Order ticket");
    assert!(ticket.iter().any(|l| l == reason), "the compact ticket writes it: {ticket:?}");
    assert!(disabled(&h, "Buy 0.010 MKT"));
    click(&mut h, "Buy 0.010 MKT");
    assert!(h.state().emitted.is_empty(), "{:?}", h.state().emitted);
    let mut h = harness_at(demo(), layout::UNDER_SIZE, Panel::Under);
    h.run();
    assert!(
        !labels_in(&h, "Order ticket").iter().any(|l| l == reason),
        "CONTROL: a window that trades writes no such line"
    );
}

/// W3 review minor 7: TP/SL is an order control, so a window that takes no order offers no TP/SL
/// either: disabled, with the window's reason on hover, even where TP/SL is otherwise allowed. The
/// POSITIVE CONTROL is the same window, tradable: the toggle is enabled.
#[test]
fn tpsl_is_offered_only_where_the_window_trades() {
    let h = settled(demo());
    assert!(!disabled(&h, "TP/SL"), "CONTROL: a window that trades offers TP/SL");
    let mut h = settled(Scene { tradable: false, ..demo() });
    assert!(disabled(&h, "TP/SL"), "an untradable window offers no TP/SL");
    let reason = "This account does not trade BTCUSDT. It trades ETHUSDT.";
    assert!(says_on_hover(&mut h, "TP/SL", reason));
}

/// The W3 fix-round re-review: the TP/SL lines PREVIEW the exits the order would carry — in the
/// full ticket while TP/SL is ticked, and in the compact toggle's hover whether it is ticked or not
/// — and, where the order itself cannot be built, say why instead. Before this the compact hover
/// with TP/SL off read "Buy: no TP/SL", which previews nothing.
#[test]
fn the_tpsl_lines_preview_the_exits_or_say_why_the_order_cannot_carry_them() {
    // The full ticket, ticked, with a valid order: its exits.
    let mut h = settled(demo());
    set(&mut h, |s| {
        s.price = "100".to_string();
        s.tpsl = true;
    });
    assert!(h.query_by_label("Buy: TP 100.5 · SL 99.7").is_some(), "a valid order's exits");
    // ...and with no size: the order's refusal, never a price it would not carry.
    set(&mut h, |s| s.size.clear());
    let why = "Buy: Enter a size of at least one lot (0.001 BTC).";
    assert!(h.query_by_label(why).is_some(), "an order that cannot be built says why");
    // The compact toggle's hover, TP/SL OFF: what it WOULD set, at the market (ask 100.0).
    let mut h = harness_at(demo(), layout::UNDER_SIZE, Panel::Under);
    h.run();
    assert!(!h.state().state.tpsl);
    h.get_by_label("TP/SL").hover();
    h.run();
    let shown = h.query_all_by_label_contains("Buy: TP 100.5 · SL 99.7").count();
    assert!(shown > 0, "the hover previews the exits a buy would carry");
    assert_eq!(h.query_all_by_label_contains("no TP/SL").count(), 0, "never 'no TP/SL'");
}

/// The W3 fix-round re-review: under the ladder, a ticked TP/SL the account refuses replaces Post
/// only and Leverage with "Turn TP/SL off to send." (the whole reason on hover; seven fixed rows),
/// and Buy MKT and Sell MKT send nothing. A Stop entry is NOT refused here — the compact ticket
/// sends at market whatever type the full one shows — so that scene is the POSITIVE CONTROL: its
/// Buy MKT sends a market order WITH both exits.
#[test]
fn the_compact_ticket_refuses_a_ticked_tpsl_it_cannot_send() {
    for (what, scene, tweak, why) in refused_tpsl() {
        let mut h = harness_at(scene, layout::UNDER_SIZE, Panel::Under);
        h.run();
        set(&mut h, |s| {
            tweak(s);
            s.tpsl = true;
        });
        if why == ticket::TPSL_STOP_WHY {
            assert!(h.query_by_label("Turn TP/SL off to send.").is_none(), "{what}");
            click(&mut h, "Buy 0.010 MKT");
            match places(h.state()).as_slice() {
                [TradeAction::Place { order_type: OrderType::Market, exits: Some(_), .. }] => {}
                other => panic!("{what}: one market bracket, got {other:?}"),
            }
            continue;
        }
        assert!(h.query_by_label("Turn TP/SL off to send.").is_some(), "{what}: the words");
        for button in ["Buy 0.010 MKT", "Sell 0.010 MKT"] {
            assert!(disabled(&h, button), "{what}: {button}");
            click(&mut h, button);
        }
        assert!(h.state().emitted.is_empty(), "{what}: {:?}", h.state().emitted);
        assert!(h.state().state.held.is_none(), "{what}: nothing is held either");
        let refusal = ticket::tpsl_refusal(why);
        assert!(says_on_hover(&mut h, "Buy 0.010 MKT", &refusal), "{what}: on the button");
    }
}

/// Ported from the DOM's accessibility tests (pre-flight I11(g)): each cancel button cancels ITS
/// OWN side, and Cancel all both. Swapping the two `CancelSide` signs pulls the wrong half of a
/// live quote book, and nothing but a click on the drawn row can see it. Orders are present on
/// both sides, so every button is enabled.
#[test]
fn each_cancel_button_routes_to_its_own_side_of_the_book() {
    let orders = vec![working("b1", 1, 99.0), working("b2", 1, 98.0), working("s1", -1, 101.0)];
    let mut h = settled(Scene { orders, ..demo() });
    for (label, act, side) in [
        ("Bids", TradeAction::CancelSide(1), "'Bids' must cancel the BUY side (+1)"),
        ("Asks", TradeAction::CancelSide(-1), "'Asks' must cancel the SELL side (-1)"),
        ("Cancel all 3", TradeAction::CancelAll, "'Cancel all' cancels both"),
    ] {
        click(&mut h, label);
        assert_eq!(h.state().emitted, [act], "{side}");
        h.state_mut().emitted.clear();
    }
}

/// Ported from the DOM's accessibility tests (pre-flight I11(g)): the mode chip tells the trader
/// where the next click's order goes, and reads the mode it was GIVEN (spec §7, "the mode chip
/// reads the published mode") — exactly one mode word on screen, the given one, so a chip that
/// rendered two words or the wrong one fails. Inverting it would make a live account read as safe;
/// an unknown mode reads `MODE ?`, never PAPER (pre-flight C5).
#[test]
fn the_header_badge_names_the_trading_mode_it_was_given() {
    const WORDS: [&str; 4] = ["PAPER", "DEMO", "LIVE", "MODE ?"];
    for (mode, word) in [
        (AccountMode::Paper, "PAPER"),
        (AccountMode::Demo, "DEMO"),
        (AccountMode::Live, "LIVE"),
        (AccountMode::Unknown, "MODE ?"),
    ] {
        let h = settled(Scene { mode, ..demo() });
        let bar = labels_in(&h, "Instrument bar");
        let found: Vec<&String> = bar.iter().filter(|l| WORDS.contains(&l.as_str())).collect();
        assert_eq!(found, [word], "{mode:?}: the bar's mode words: {bar:?}");
    }
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

/// W4 fix round 1, I-1 (MONEY PATH): a press is released as a click on the button it went down
/// on, whatever moved in between. egui gives the click to the id the PRESS landed on, and a kit
/// button's own id is its position in its row; so when Buy and Sell stack between a press and its
/// release — a quote-sized market order's labels widen on a tick of the book — the button that
/// took the pressed one's place took its click, and sent a SELL.
///
/// The compact ticket, under the ladder: Buy and Sell share a row and the cancel row is the next
/// (owner decision of 2026-10-03, card 5 (ii)). It is sized in the quote currency, in
/// a window where "Buy … MKT" and "Sell … MKT" fit side by side with one point to spare (measured
/// from the labels it draws, as the kit sizes a button). The press goes down on Cancel all; the
/// market moves so that each label gains a digit and Buy and Sell stack, Sell taking the cancel
/// row's place; then the release: it cancels all, and nothing else. CONTROLS: before, Buy and Sell
/// share a row; under the press they stacked, and Cancel all moved down a row.
#[test]
fn a_press_held_while_buy_and_sell_stack_is_never_released_as_another_button() {
    let grid = Grid { tick: 0.01, lot: 0.000_001, min_qty: 0.000_001 };
    let orders = vec![working("b1", 1, 99.0), working("s1", -1, 101.0)];
    let scene = Scene { grid, mid: Some(100.0), orders, ..demo() };
    let mut h = harness_at(scene, egui::vec2(600.0, 680.0), Panel::Under);
    h.run();
    set(&mut h, |s| {
        s.unit = SizeUnit::Quote;
        s.size = "1000".to_string();
    });
    let both = kit_button_w(&h, &label_of(&h, "Buy ", " MKT"))
        + vike_ui_theme::components::Tokens::of(&h.ctx).metrics.gap
        + kit_button_w(&h, &label_of(&h, "Sell ", " MKT"));
    h.set_size(egui::vec2(both + 16.0 + 1.0, 680.0));
    h.run();
    let row = |h: &Harness<'_, Fixture>, prefix: &str| centre_of(h, prefix, " MKT").y;
    assert_eq!(row(&h, "Buy "), row(&h, "Sell "), "CONTROL: side by side, with a point to spare");
    let at = centre_of(&h, "Cancel all", "");
    // A tick: the book moves to a tenth, and a thousand USDT is ten times the coins.
    let sent = press_change_release(&mut h, at, |f| f.scene.mid = Some(10.0));
    assert_ne!(row(&h, "Buy "), row(&h, "Sell "), "CONTROL: the rows flipped under the press");
    assert!(centre_of(&h, "Cancel all", "").y > at.y, "CONTROL: Cancel all moved down a row");
    assert_eq!(sent, [TradeAction::CancelAll], "Cancel all's own action, and nothing else");
}

/// Press at `at`, let `change` move the window under the held button, then release there; what
/// the release sent. Every step is a frame of its own, as a trader's hand gives them.
fn press_change_release(
    h: &mut Harness<'static, Fixture>,
    at: egui::Pos2,
    change: impl FnOnce(&mut Fixture),
) -> Vec<TradeAction> {
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    h.state_mut().emitted.clear();
    h.event(egui::Event::PointerMoved(at));
    h.event(button(true));
    h.step();
    change(h.state_mut());
    h.step();
    h.step();
    h.event(button(false));
    h.run();
    std::mem::take(&mut h.state_mut().emitted)
}

/// Where the button whose words start with `prefix` and end with `suffix` is, its centre.
fn centre_of(h: &Harness<'_, Fixture>, prefix: &str, suffix: &str) -> egui::Pos2 {
    h.get_by_label(&label_of(h, prefix, suffix)).rect().center()
}

/// How wide the kit draws a button reading `words` under the harness's look.
fn kit_button_w(h: &Harness<'_, Fixture>, words: &str) -> f32 {
    let t = vike_ui_theme::components::Tokens::of(&h.ctx);
    let strong = t.font(vike_ui_theme::type_scale::TextRole::Strong);
    let g = h.ctx.fonts_mut(|f| f.layout_no_wrap(words.to_string(), strong, egui::Color32::WHITE));
    g.size().x + 2.0 * t.metrics.pad
}

/// W4 fix round 2, N4: the REVERSE of the press above, which only a keyed SELL survives. Buy and
/// Sell stand a row each; the press goes down on Sell; the market moves so that each label loses
/// two digits and they go back side by side, the cancel row taking Sell's old row; then the
/// release. Sell is still on screen, so the release is Sell's — exactly one market sell — and
/// never the button that took its row. With Sell keyed by position, its press went to that
/// button, or to nothing at all.
#[test]
fn a_press_on_sell_held_while_buy_and_sell_unstack_sells_and_nothing_else() {
    let grid = Grid { tick: 0.01, lot: 0.000_001, min_qty: 0.000_001 };
    let scene = Scene { grid, mid: Some(100.0), ..demo() };
    let mut h = harness_at(scene, egui::vec2(600.0, 680.0), Panel::Under);
    h.run();
    set(&mut h, |s| {
        s.unit = SizeUnit::Quote;
        s.size = "1000".to_string();
    });
    // Side by side at mid 100 with room for the two digits the stacking's margin asks for (and
    // half a point), where mid 1 adds two digits to EACH label.
    let t = vike_ui_theme::components::Tokens::of(&h.ctx);
    let both = kit_button_w(&h, &label_of(&h, "Buy ", " MKT"))
        + t.metrics.gap
        + kit_button_w(&h, &label_of(&h, "Sell ", " MKT"));
    let digits = kit_button_w(&h, "00") - 2.0 * t.metrics.pad;
    h.set_size(egui::vec2(both + digits + 0.5 + 16.0, 680.0));
    h.state_mut().scene.mid = Some(1.0);
    h.run();
    let row = |h: &Harness<'_, Fixture>, prefix: &str| centre_of(h, prefix, " MKT").y;
    assert_ne!(row(&h, "Buy "), row(&h, "Sell "), "CONTROL: at mid 1 they stand a row each");
    let at = centre_of(&h, "Sell ", " MKT");
    let sent = press_change_release(&mut h, at, |f| f.scene.mid = Some(100.0));
    assert_eq!(row(&h, "Buy "), row(&h, "Sell "), "CONTROL: they went back side by side");
    match sent.as_slice() {
        [TradeAction::Place { side: -1, order_type: OrderType::Market, .. }] => {}
        other => panic!("the pressed Sell sells, and nothing else is sent: {other:?}"),
    }
}

/// W4 fix round 2, N4 (the finding's FIRST example): in the full ticket a press on Cancel all,
/// held while Buy and Sell stack above it, cancels all — and never sells. The pinned rows were
/// positional: stacking put Sell's row where the cancel row had been, and Sell took its press.
/// Exactly the one action: Cancel all's own.
#[test]
fn a_press_on_cancel_all_held_while_buy_and_sell_stack_cancels_all_and_nothing_else() {
    let grid = Grid { tick: 0.01, lot: 0.000_001, min_qty: 0.000_001 };
    let orders = vec![working("b1", 1, 99.0), working("s1", -1, 101.0)];
    let scene = Scene { grid, mid: Some(100.0), orders, ..demo() };
    let mut h = harness_at(scene, egui::vec2(600.0, 560.0), Panel::Beside);
    h.state_mut().state.view.ladder = false;
    h.run();
    set(&mut h, |s| {
        s.order_type = OrderType::Market;
        s.unit = SizeUnit::Quote;
        s.size = "1000".to_string();
    });
    let both = kit_button_w(&h, &label_of(&h, "Buy ", " market"))
        + vike_ui_theme::components::Tokens::of(&h.ctx).metrics.gap
        + kit_button_w(&h, &label_of(&h, "Sell ", " market"));
    h.set_size(egui::vec2(both + 16.0 + 1.0, 560.0));
    h.run();
    let row = |h: &Harness<'_, Fixture>, prefix: &str| centre_of(h, prefix, " market").y;
    assert_eq!(row(&h, "Buy "), row(&h, "Sell "), "CONTROL: side by side, a point to spare");
    let at = h.get_by_label("Cancel all 2").rect().center();
    let sent = press_change_release(&mut h, at, |f| f.scene.mid = Some(10.0));
    assert_ne!(row(&h, "Buy "), row(&h, "Sell "), "CONTROL: they stacked under the press");
    assert_eq!(sent, [TradeAction::CancelAll], "Cancel all's own action, and nothing else");
}

/// W4 fix round 2, N4: a press on Asks held while its OWN row re-packs — the count gains a digit (9
/// working orders become 10), `Cancel all 10` no longer leaves Asks room beside it, and Asks moves
/// to the next row with the padlock — still cancels the asks, exactly once. Asks, because it is the
/// cancel that MOVES: Cancel all leads its row in every packing, so a press on it proves nothing
/// about the keys (the first version of this test pressed it, and passed un-keyed).
#[test]
fn a_press_on_asks_held_while_the_count_gains_a_digit_cancels_the_asks() {
    let mut nine: Vec<LadderOrder> = (0..8).map(|i| working(&format!("o{i}"), 1, 99.0)).collect();
    nine.push(working("o8", -1, 101.0));
    let mut h =
        harness_at(Scene { orders: nine, ..demo() }, egui::vec2(600.0, 560.0), Panel::Beside);
    h.state_mut().state.view.ladder = false;
    h.run();
    let gap = vike_ui_theme::components::Tokens::of(&h.ctx).metrics.gap;
    let row = kit_button_w(&h, "Cancel all 9")
        + kit_button_w(&h, "Bids")
        + kit_button_w(&h, "Asks")
        + 2.0 * gap;
    h.set_size(egui::vec2(row + 16.0 + 1.0, 560.0));
    h.run();
    let y = |h: &Harness<'_, Fixture>, name: &str| h.get_by_label(name).rect().center().y;
    assert_eq!(y(&h, "Asks"), y(&h, "Cancel all 9"), "CONTROL: Asks fits beside Cancel all 9");
    let at = h.get_by_label("Asks").rect().center();
    let sent = press_change_release(&mut h, at, |f| f.scene.orders.push(working("o9", 1, 99.0)));
    assert_ne!(y(&h, "Asks"), y(&h, "Cancel all 10"), "CONTROL: Asks moved under the press");
    assert_eq!(sent, [TradeAction::CancelSide(-1)], "the asks' own cancel, exactly once");
}

/// The text field in the ticket whose text is exactly `text`, its centre.
fn field_reading(h: &Harness<'_, Fixture>, text: &str) -> egui::Pos2 {
    let found: Vec<egui::Pos2> = h
        .root()
        .children_recursive()
        .filter(|n| n.accesskit_node().role() == Role::TextInput)
        .filter(|n| n.accesskit_node().value().as_deref() == Some(text))
        .map(|n| n.rect().center())
        .collect();
    match found.as_slice() {
        [one] => *one,
        other => panic!("one field reading {text:?}, found {other:?}"),
    }
}

/// Click the field at `at` and type `words` into it, a frame for each.
fn type_into(h: &mut Harness<'static, Fixture>, at: egui::Pos2, words: &str) {
    h.event(egui::Event::PointerMoved(at));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
    }
    h.run();
    h.event(egui::Event::Text(words.to_string()));
    h.run();
}

/// W4 fix round 2, NEW-1: a fill that opens or closes the position while the trader types a price
/// keeps the price field's focus. The form's controls took their ids from their position in it,
/// and "No position on this account" is drawn only while there is none: a fill shifted every id
/// below it, egui dropped the focus of a field whose id vanished, and the rest of the typing was
/// lost — a TRUNCATED price, which Buy then sent (its label names the size, not the price).
#[test]
fn a_fill_while_the_price_is_typed_keeps_the_fields_focus() {
    for (before, after) in [
        (Some(Position { size: 0.05, avg_px: 99.0, upnl: 0.05 }), None),
        (None, Some(Position { size: 0.05, avg_px: 99.0, upnl: 0.05 })),
    ] {
        let mut h = settled(Scene { position: before, ..demo() });
        let at = field_reading(&h, "");
        type_into(&mut h, at, "654");
        h.state_mut().scene.position = after;
        h.run();
        h.event(egui::Event::Text("32".to_string()));
        h.run();
        assert_eq!(h.state().state.price, "65432", "{before:?} -> {after:?}: every keystroke kept");
    }
}

/// W4 fix round 2, NEW-1: a press on a quick size held across a fill sets THAT size, not the
/// control the shifted ids put in its place (a share of the buying power, Reduce only).
#[test]
fn a_fill_while_a_quick_size_is_pressed_still_sets_that_size() {
    let mut h = settled(demo());
    assert_eq!(h.state().state.size, "0.010");
    let at = h.get_by_label("0.001").rect().center();
    let sent = press_change_release(&mut h, at, |f| f.scene.position = None);
    assert_eq!(h.state().state.size, "0.001", "the quick size pressed: {sent:?}");
    assert!(!h.state().state.reduce_only, "and nothing else was clicked");
    assert!(sent.is_empty(), "{sent:?}");
}

/// W4 fix round 2, NEW-1: the same for the OTHER line the form draws only sometimes — why the
/// window takes no order. An account that stops trading the symbol while the trader types a size
/// keeps the size field's focus.
#[test]
fn an_account_that_stops_trading_while_the_size_is_typed_keeps_the_fields_focus() {
    let mut h = settled(demo());
    set(&mut h, |s| {
        s.price = "99.5".to_string();
        s.size.clear();
    });
    let at = field_reading(&h, "");
    type_into(&mut h, at, "1");
    h.state_mut().scene.tradable = false;
    h.run();
    h.event(egui::Event::Text("2".to_string()));
    h.run();
    assert_eq!(h.state().state.size, "12", "every keystroke kept");
}

/// W4 fix round 2, N2: a mark tick that adds a digit to the P/L (+9.99 to +10.01, +99.99 to
/// +100.01, +999.99 to +1000.01) never moves the form below the position line. Over every average
/// price from one digit to twelve, so that, whatever the type's widths, some line sits at the
/// ticket's edge: a wrapping line that held the P/L moved Close and everything under it by a line.
#[test]
fn a_tick_that_adds_a_digit_to_the_pl_never_moves_the_form() {
    let mut h = settled(demo());
    let close_y = |h: &Harness<'_, Fixture>| h.get_by_label("Close").rect().center().y;
    let mut moved = Vec::new();
    for digits in 1..=12 {
        let avg_px = 10f64.powi(digits) - 1.0;
        for (before, after) in [(9.99, 10.01), (99.99, 100.01), (999.99, 1000.01)] {
            h.state_mut().scene.position = Some(Position { size: 0.05, avg_px, upnl: before });
            h.run();
            let y = close_y(&h);
            h.state_mut().scene.position = Some(Position { size: 0.05, avg_px, upnl: after });
            h.run();
            if close_y(&h) != y {
                moved.push(format!("avg {avg_px}: {before} -> {after}"));
            }
        }
    }
    assert!(moved.is_empty(), "a P/L tick moved the form: {moved:?}");
}

/// W4 fix round 2, N3: in the one case the held prompt wraps past the strip's two lines (an
/// eight-digit price with its exits, a 260 pt window, Large text), the pass that HOLDS the order
/// laid the strip out before it saw the click. The FRAME that holds it — the click's own frame,
/// nothing after it — shows every line of the prompt inside the strip `status::height` derives:
/// `draw` discards that pass and draws it again.
///
/// ⚠ Asserted after `step` (the click's own frames), never after `run`: egui draws the frame after
/// any frame with input whatever the window asks (`InputState::wants_repaint_after`), so a test
/// that let the window settle passed on the code that showed the cut prompt for one frame (the
/// first two versions of this test did, the second with egui's animations off).
#[test]
fn a_prompt_that_wraps_is_drawn_whole_in_the_frame_that_holds_the_order() {
    use vike_ui_theme::metrics::Density;
    use vike_ui_theme::type_scale::TextSize;
    let look = Appearance {
        density: Density::Comfortable,
        text_size: TextSize::Large,
        ..Appearance::default()
    };
    let scene = Scene { look, mid: Some(12_345_678.9), last: Some(12_345_678.9), ..live() };
    let mut h = harness_at(scene, egui::vec2(260.0, 560.0), Panel::Beside);
    h.run();
    set(&mut h, |s| {
        s.price = "12345678.9".to_string();
        s.tpsl = true;
    });
    h.get_by_label("Buy 0.010 limit").click();
    // The click's frames — the pointer moving there, the press, the release — and no other.
    h.step();
    assert!(h.state().state.held.is_some(), "CONTROL: the order is held");
    let (content, strip_h) = (h.state().content, h.state().strip_h);
    let strip =
        egui::Rect::from_min_max(egui::pos2(content.min.x, content.max.y - strip_h), content.max);
    let lines: Vec<(String, egui::Rect)> = h
        .root()
        .children_recursive()
        .filter(|n| n.accesskit_node().role() == Role::Label)
        .filter(|n| region_of(n).as_deref() == Some("Status strip"))
        .map(|n| (n.accesskit_node().value().unwrap_or_default(), n.rect()))
        .collect();
    assert!(lines.len() > 2, "CONTROL: the prompt wrapped past two lines: {lines:?}");
    for (line, r) in &lines {
        assert!(strip.expand(0.6).contains_rect(*r), "{line:?} at {r:?} lies outside {strip:?}");
    }
}

// ---- F2: W4's parked tests ----------------------------------------------------------------------

/// W4 fix round 2's parked minor 3: RELEASING a wrapped prompt. Place on a prompt the strip grew to
/// hold (an eight-digit price with its exits, a 260 pt window, Large text) lets go of the order, so
/// the strip shrinks back in that pass, and `draw` discards the pass and draws it again
/// (`request_discard`). The frame of the click sends the held order EXACTLY ONCE — egui hands the
/// second pass no input, so the click is not seen twice — and that frame already shows the strip
/// without the prompt (the CONTROLS: no Place left, the strip back to its height). Asserted after
/// ONE `step`, the click's own frame, never after the window settles.
#[test]
fn releasing_a_wrapped_prompt_sends_the_order_once_in_the_frame_of_the_click() {
    use vike_ui_theme::metrics::Density;
    use vike_ui_theme::type_scale::TextSize;
    let look = Appearance {
        density: Density::Comfortable,
        text_size: TextSize::Large,
        ..Appearance::default()
    };
    let scene = Scene { look, mid: Some(12_345_678.9), last: Some(12_345_678.9), ..live() };
    let mut h = harness_at(scene, egui::vec2(260.0, 560.0), Panel::Beside);
    h.run();
    set(&mut h, |s| {
        s.price = "12345678.9".to_string();
        s.tpsl = true;
    });
    click(&mut h, "Buy 0.010 limit");
    let held = h.state().state.held.clone().expect("CONTROL: the order is held");
    let lines = labels_in(&h, "Status strip").len();
    assert!(lines > 2, "CONTROL: the prompt wrapped past two lines: {lines}");
    let tall = region_rect(&h, "Status strip").height();
    h.state_mut().emitted.clear();
    h.get_by_label("Place").click();
    h.step();
    assert_eq!(places(h.state()), [&held], "sent exactly once: {:?}", h.state().emitted);
    assert!(h.state().state.held.is_none(), "and let go");
    assert!(h.query_by_label("Place").is_none(), "CONTROL: the click's frame has no prompt left");
    let short = region_rect(&h, "Status strip").height();
    assert!(short < tall, "CONTROL: the strip shrank in the click's own frame: {tall} -> {short}");
}

/// An open position, for the flips below.
const OPEN: Position = Position { size: 0.05, avg_px: 99.0, upnl: 0.05 };

/// One flip: what it is, the scene it starts from, and the change of that scene alone.
struct Flip {
    what: &'static str,
    scene: Scene,
    change: fn(&mut Scene),
}

/// What comes and goes ABOVE the full ticket's form sections between two frames without the
/// trader's doing (W4 fix round 2's NEW-1, and its parked minor 5): a fill opens or closes the
/// position, and the account stops or starts taking orders, which adds or takes away the form's
/// untradable line.
fn flips() -> [Flip; 4] {
    [
        Flip { what: "a fill closes the position", scene: demo(), change: |s| s.position = None },
        Flip {
            what: "a fill opens one",
            scene: Scene { position: None, ..demo() },
            change: |s| s.position = Some(OPEN),
        },
        Flip { what: "the account stops trading", scene: demo(), change: |s| s.tradable = false },
        Flip {
            what: "the account trades again",
            scene: Scene { tradable: false, ..demo() },
            change: |s| s.tradable = true,
        },
    ]
}

/// `form_price`: a price being typed keeps every keystroke across every flip — the untradable line
/// appearing or going above it would have moved the field's id, and with it the focus.
#[test]
fn every_flip_while_the_price_is_typed_keeps_the_fields_focus() {
    for Flip { what, scene, change: flip } in flips() {
        let mut h = settled(scene);
        let at = field_reading(&h, "");
        type_into(&mut h, at, "654");
        flip(&mut h.state_mut().scene);
        h.run();
        h.event(egui::Event::Text("32".to_string()));
        h.run();
        assert_eq!(h.state().state.price, "65432", "{what}: every keystroke kept");
    }
}

/// `form_quick`: a press on a quick size held across every flip sets THAT size, and clicks nothing
/// else.
#[test]
fn every_flip_while_a_quick_size_is_pressed_still_sets_that_size() {
    for Flip { what, scene, change: flip } in flips() {
        let mut h = settled(scene);
        assert_eq!(h.state().state.size, "0.010", "{what}: CONTROL: the default size");
        let at = h.get_by_label("0.001").rect().center();
        let sent = press_change_release(&mut h, at, |f| flip(&mut f.scene));
        assert_eq!(h.state().state.size, "0.001", "{what}: the quick size pressed: {sent:?}");
        assert!(!h.state().state.reduce_only, "{what}: and nothing else was clicked");
        assert!(sent.is_empty(), "{what}: {sent:?}");
    }
}

/// `form_shares`: a press on a share of the buying power held across every flip sets THAT share:
/// 10 % of 10,000 USDT at the last price, 100, is 10 BTC.
#[test]
fn every_flip_while_a_share_is_pressed_still_sets_that_share() {
    for Flip { what, scene, change: flip } in flips() {
        let mut h = settled(scene);
        let at = h.get_by_label("10%").rect().center();
        let sent = press_change_release(&mut h, at, |f| flip(&mut f.scene));
        assert_eq!(h.state().state.size, "10.000", "{what}: the share pressed: {sent:?}");
        assert!(!h.state().state.reduce_only, "{what}: and nothing else was clicked");
        assert!(sent.is_empty(), "{what}: {sent:?}");
    }
}

/// `form_tpsl`: the TP distance being typed keeps every keystroke across a fill, and a press on the
/// TP/SL toggle held across a fill turns TP/SL on. Held across the account STOPPING trading, the
/// press does nothing — the toggle is an order control, disabled once the window takes no order —
/// and clicks nothing else. (The TP field is not drawn in a window that takes no order, and a press
/// on the disabled toggle starts no click, so the account trading again has no case here.)
#[test]
fn a_flip_while_tpsl_is_set_keeps_its_field_and_its_toggle_their_meaning() {
    for Flip { what, scene, change: flip } in flips().into_iter().take(2) {
        let mut h = settled(scene);
        set(&mut h, |s| {
            s.price = "99.5".to_string();
            s.tpsl = true;
            s.tp_text.clear();
        });
        let at = field_reading(&h, "");
        type_into(&mut h, at, "1");
        flip(&mut h.state_mut().scene);
        h.run();
        h.event(egui::Event::Text("2".to_string()));
        h.run();
        assert_eq!(h.state().state.tp_text, "12", "{what}: every keystroke kept");
    }
    for (Flip { what, scene, change: flip }, turned_on) in
        flips().into_iter().zip([true, true, false])
    {
        let mut h = settled(scene);
        let at = h.get_by_label("TP/SL").rect().center();
        let sent = press_change_release(&mut h, at, |f| flip(&mut f.scene));
        assert_eq!(h.state().state.tpsl, turned_on, "{what}: {sent:?}");
        assert!(!h.state().state.reduce_only, "{what}: nothing else was clicked");
        assert!(sent.is_empty(), "{what}: {sent:?}");
    }
}

/// W4 fix round 2's parked minor 5, the P/L line's sibling (N2): the full ticket's order VALUE moves
/// with every tick of a Market order's price, and gains a digit at 1,000, 10,000, …. The window is
/// sized so the value line, as the ticket wrote it, fills the form's width to half a point; then a
/// tick takes it from 999.00 to 1,001.00. Nothing under it — the quick sizes, the shares, Reduce
/// only, TP/SL — moves: a line that wrapped on the new digit moved all of them by a line.
#[test]
fn a_tick_that_adds_a_digit_to_the_order_value_never_moves_the_form() {
    let scene = Scene { book: false, last: Some(9.99), buying_power: Some(500.0), ..demo() };
    let mut h = harness_at(scene, egui::vec2(layout::TICKET_ONLY_SIZE.x, 900.0), Panel::Beside);
    h.state_mut().state.view.ladder = false;
    h.run();
    set(&mut h, |s| {
        s.order_type = OrderType::Market;
        s.size = "100".to_string();
    });
    let value = |h: &Harness<'_, Fixture>| -> (String, egui::Rect) {
        let found: Vec<(String, egui::Rect)> = h
            .root()
            .children_recursive()
            .filter(|n| n.accesskit_node().role() == Role::Label)
            .filter(|n| region_of(n).as_deref() == Some("Order ticket"))
            .filter_map(|n| {
                let v = n.accesskit_node().value()?;
                v.starts_with("≈ ").then(|| (v, n.rect()))
            })
            .collect();
        match found.as_slice() {
            [one] => one.clone(),
            other => panic!("one value line, found {other:?}"),
        }
    };
    let t = vike_ui_theme::components::Tokens::of(&h.ctx);
    let font = t.mono(vike_ui_theme::type_scale::TextRole::Caption);
    let (before, _) = value(&h);
    let w = h
        .ctx
        .fonts_mut(|f| f.layout_no_wrap(before.clone(), font.clone(), egui::Color32::WHITE))
        .size()
        .x;
    let form = region_rect(&h, ticket::FORM_NAME).width();
    h.set_size(egui::vec2(layout::TICKET_ONLY_SIZE.x + (w + 0.5 - form), 900.0));
    h.run();
    let row = h.ctx.fonts_mut(|f| f.row_height(&font));
    let (_, line) = value(&h);
    assert!(line.height() < 1.5 * row, "CONTROL: the value line fits the form: {line:?}");
    let controls = ["0.001", "10%", "Reduce only", "TP/SL"];
    let ys = |h: &Harness<'_, Fixture>| controls.map(|c| h.get_by_label(c).rect().center().y);
    let at_rest = ys(&h);
    h.state_mut().scene.last = Some(10.01);
    h.run();
    let (after, _) = value(&h);
    assert!(after.len() > before.len(), "CONTROL: the tick added a digit: {before} -> {after}");
    let moved: Vec<String> = controls
        .iter()
        .zip(at_rest.iter().zip(ys(&h)))
        .filter(|(_, (a, b))| (**a - *b).abs() > 0.5)
        .map(|(c, (a, b))| format!("{c}: {a} -> {b}"))
        .collect();
    assert!(moved.is_empty(), "{before} -> {after} moved the form: {moved:?}");
}

// ---- the final fix wave (FW1) ------------------------------------------------------------------

/// The owner's decision of 2026-10-03 (card 5 (ii)): neither ticket offers Join, the one-click
/// order at the best bid or ask. The full ticket's Bid and Ask stay: they only TYPE the best price
/// into the price field, and send nothing.
#[test]
fn neither_ticket_offers_join_and_bid_and_ask_only_type_the_price() {
    for (size, panel) in [(layout::BESIDE_SIZE, Panel::Beside), (layout::UNDER_SIZE, Panel::Under)]
    {
        let mut h = harness_at(demo(), size, panel);
        h.run();
        let joins: Vec<String> = h
            .query_all_by_label_contains("Join")
            .filter_map(|n| n.accesskit_node().label())
            .collect();
        assert!(joins.is_empty(), "{panel:?}: {joins:?}");
    }
    let mut h = settled(demo());
    click(&mut h, "Bid");
    assert_eq!(h.state().state.price, "99.9", "Bid types the best bid");
    click(&mut h, "Ask");
    assert_eq!(h.state().state.price, "100.0", "Ask types the best ask");
    assert!(h.state().emitted.is_empty(), "and neither sends anything: {:?}", h.state().emitted);
}

/// Final review A, minor 2: the confirm prompt says "reduce only" when the order it holds is: the
/// order keeps the flag it was made with, whatever the box shows by the time the trader confirms.
/// CONTROL: an order that is not reduce-only does not say it.
#[test]
fn the_confirm_prompt_says_reduce_only_when_the_order_is() {
    for reduce in [true, false] {
        let mut h = settled(live());
        set(&mut h, |s| {
            s.price = "100".to_string();
            s.reduce_only = reduce;
        });
        click(&mut h, "Buy 0.010 limit");
        assert!(
            matches!(&h.state().state.held, Some(TradeAction::Place { reduce_only, .. }) if *reduce_only == reduce),
            "{reduce}: held {:?}",
            h.state().state.held
        );
        let strip = labels_in(&h, "Status strip");
        assert_eq!(strip.iter().any(|l| l.contains("reduce only")), reduce, "{reduce}: {strip:?}");
    }
}

/// Final review A, minor 3 (MONEY PATH): a held Close or Reverse names the side and size it sends
/// (the dispatcher sizes it from the position at the confirm: the position's size, twice it for a
/// Reverse) and is DROPPED, with a note, the moment the position's size changes under it: a
/// Reverse held while a resting buy filled would have sent twice the NEW position. CONTROL: a tick
/// of the market moves the P/L, not the size, and keeps it.
#[test]
fn a_held_close_or_reverse_names_its_size_and_is_dropped_when_the_position_changes() {
    for (button, words) in [
        ("Close", "Close the position: sell 0.050 at market"),
        ("Reverse", "Reverse the position: sell 0.100 at market"),
    ] {
        let mut h = settled(live());
        click(&mut h, button);
        assert!(h.state().state.held.is_some(), "{button}: held for a confirm");
        let strip = labels_in(&h, "Status strip");
        assert!(strip.iter().any(|l| l.contains(words)), "{button}: {strip:?}");
        h.state_mut().scene.position = Some(Position { upnl: 0.07, ..OPEN });
        h.run();
        assert!(h.state().state.held.is_some(), "{button}: CONTROL: a tick keeps it");
        assert!(h.state().emitted.is_empty(), "{button}: {:?}", h.state().emitted);
        h.state_mut().scene.position = Some(Position { size: 0.08, ..OPEN });
        h.run();
        assert!(h.state().state.held.is_none(), "{button}: dropped");
        assert!(h.query_by_label("Place").is_none(), "{button}: nothing left to confirm");
        match h.state().emitted.as_slice() {
            [TradeAction::Note { kind: StatusKind::Error, text }] => {
                assert!(text.starts_with("Not sent:") && text.contains("position"), "{text}");
            }
            other => panic!("{button}: one error note, got {other:?}"),
        }
    }
}

/// Final review A, minor 4: the one-click padlock lets go of a held order AND says so, "Not
/// sent.", as the strip's Cancel does. CONTROL: with nothing held, the padlock says nothing.
#[test]
fn the_padlock_letting_go_of_a_held_order_says_not_sent() {
    let mut h = settled(live());
    h.get_by_label_contains("One-click trading").click();
    h.run();
    assert!(h.state().state.one_click, "CONTROL: the click turned it on");
    assert!(h.state().emitted.is_empty(), "CONTROL: nothing held, nothing said");
    let mut h = settled(live());
    set(&mut h, |s| s.price = "99.5".to_string());
    click(&mut h, "Buy 0.010 limit");
    assert!(h.state().state.held.is_some(), "held for a confirm");
    h.state_mut().emitted.clear();
    h.get_by_label_contains("One-click trading").click();
    h.run();
    assert!(h.state().state.held.is_none(), "let go");
    assert_eq!(
        h.state().emitted,
        [TradeAction::Note { kind: StatusKind::Info, text: "Not sent.".to_string() }]
    );
}

/// F2 review minor 3: the full ticket's max never hides a KNOWN zero behind the dash that means
/// "not known": no buying power, or less than one lot's worth, says so. The dash is for a max
/// nobody can compute (no price to divide by); a buying power nobody gave draws no max at all.
#[test]
fn the_max_says_a_known_zero_and_a_dash_only_for_what_is_not_known() {
    for (what, scene, want) in [
        (
            "no buying power",
            Scene { buying_power: Some(0.0), ..demo() },
            "max 0 BTC · no buying power",
        ),
        ("a debt", Scene { buying_power: Some(-5.0), ..demo() }, "max 0 BTC · no buying power"),
        (
            "under one lot",
            Scene { buying_power: Some(0.05), ..demo() },
            "max 0 BTC · under one lot",
        ),
        ("CONTROL: enough", demo(), "max 100.000 BTC"),
        ("no price", Scene { book: false, last: None, ..demo() }, "max — BTC"),
    ] {
        let h = settled(scene);
        let ticket = labels_in(&h, "Order ticket");
        assert!(ticket.iter().any(|l| l == want), "{what}: {want:?} in {ticket:?}");
    }
    let h = settled(Scene { buying_power: None, ..demo() });
    let ticket = labels_in(&h, "Order ticket");
    assert!(!ticket.iter().any(|l| l.starts_with("max ")), "not known: no max: {ticket:?}");
}

/// The owner's decision of 2026-10-03 (round 2, item 9): a Stop whose trigger is on the wrong side
/// of the market — a buy stop below the ask, a sell stop above the bid — triggers at once. The
/// ticket WARNS before the click, on a line of its own with the real numbers, and the confirm
/// prompt says the same words; the order is NOT refused (venues differ on whether they fire it or
/// reject it), and it is sent. CONTROL: a side whose trigger is on the right side warns nothing.
#[test]
fn a_stop_on_the_wrong_side_of_the_market_is_warned_and_still_sent() {
    let buy_warning = "Stop BUY 98.0 is below the market 100.0: it triggers at once.";
    let sell_warning = "Stop SELL 101.0 is above the market 99.9: it triggers at once.";
    let mut h = settled(live());
    set(&mut h, |s| {
        s.order_type = OrderType::Stop;
        s.price = "101".to_string();
    });
    let ticket = labels_in(&h, "Order ticket");
    assert!(ticket.iter().any(|l| l == sell_warning), "a sell stop above the bid: {ticket:?}");
    assert!(!ticket.iter().any(|l| l.starts_with("Stop BUY")), "CONTROL: {ticket:?}");
    set(&mut h, |s| s.price = "98".to_string());
    let ticket = labels_in(&h, "Order ticket");
    assert!(ticket.iter().any(|l| l == buy_warning), "the ticket warns first: {ticket:?}");
    assert!(!ticket.iter().any(|l| l.starts_with("Stop SELL")), "CONTROL: {ticket:?}");
    assert!(!disabled(&h, "Buy 0.010 stop"), "a warning, not a refusal");
    click(&mut h, "Buy 0.010 stop");
    assert!(h.state().state.held.is_some(), "held for a confirm");
    let strip = labels_in(&h, "Status strip");
    assert!(strip.iter().any(|l| l == buy_warning), "the confirm says the same words: {strip:?}");
    click(&mut h, "Place");
    match places(h.state()).as_slice() {
        [TradeAction::Place { side: 1, order_type: OrderType::Stop, price: Some(p), .. }] => {
            assert!((p - 98.0).abs() < 1e-9, "{p}");
        }
        other => panic!("one buy stop at 98, got {other:?}"),
    }
}

/// The owner's decision of 2026-10-03 (round 2, item 10): a market Buy or Sell is REFUSED, with
/// the reason on the button, where there is no price for it at all — no side of the book to fill
/// against and no last price: such an order goes out blind, and the app's notional cap, which needs
/// a price, cannot hold it either. In both tickets. CONTROL: a last price alone is enough.
#[test]
fn a_market_order_with_no_book_and_no_last_price_is_refused() {
    let b = L2Book::new(0.1);
    let market = state_with(|s| {
        s.size = "0.010".to_string();
        s.order_type = OrderType::Market;
    });
    let blind = TradeInputs { last: None, ..inputs_on(&b, BTC) };
    for side in [1, -1] {
        assert_eq!(
            ticket::order(side, &market, &blind),
            Err(ticket::NO_MARKET_PRICE_WHY.to_string()),
            "{side}"
        );
    }
    assert!(
        matches!(
            ticket::order(1, &market, &inputs_on(&b, BTC)),
            Ok(TradeAction::Place { order_type: OrderType::Market, .. })
        ),
        "CONTROL: a last price is enough"
    );
    // PER SIDE (the FW1 review's pin): a one-sided book with no last price refuses only the side
    // it has no price for; the other side fills against the side that is there.
    let one_sided = |bids: &[BookLevel], asks: &[BookLevel]| {
        let mut b = L2Book::new(0.1);
        b.apply_snapshot(1, bids, asks);
        b
    };
    let bids = one_sided(&[BookLevel::new(99.9, 1.0)], &[]);
    let asks = one_sided(&[], &[BookLevel::new(100.0, 1.0)]);
    for (what, book, priced, blind_side) in
        [("bids only", &bids, -1, 1), ("asks only", &asks, 1, -1)]
    {
        let on = TradeInputs { last: None, ..inputs_on(book, BTC) };
        assert!(
            matches!(
                ticket::order(priced, &market, &on),
                Ok(TradeAction::Place { order_type: OrderType::Market, .. })
            ),
            "{what}: the side with a price is sent"
        );
        assert_eq!(
            ticket::order(blind_side, &market, &on),
            Err(ticket::NO_MARKET_PRICE_WHY.to_string()),
            "{what}: the side with none is refused"
        );
    }
    let scene = || Scene { book: false, last: None, ..demo() };
    let mut h = settled(scene());
    set(&mut h, |s| s.order_type = OrderType::Market);
    for button in ["Buy 0.010 market", "Sell 0.010 market"] {
        assert!(disabled(&h, button), "{button}");
        click(&mut h, button);
    }
    assert!(h.state().emitted.is_empty(), "{:?}", h.state().emitted);
    assert!(says_on_hover(&mut h, "Buy 0.010 market", ticket::NO_MARKET_PRICE_WHY));
    let mut h = harness_at(scene(), layout::UNDER_SIZE, Panel::Under);
    h.run();
    for button in ["Buy 0.010 MKT", "Sell 0.010 MKT"] {
        assert!(disabled(&h, button), "under the ladder: {button}");
        click(&mut h, button);
    }
    assert!(h.state().emitted.is_empty(), "{:?}", h.state().emitted);
}

/// The FW1 review's pins on round 2, item 9: a trigger AT the market is on no wrong side and warns
/// nothing (the boundary: a buy stop at the ask, a sell stop at the bid); and with no book the
/// warning measures against the last price. CONTROL: the side the same trigger IS wrong for warns.
#[test]
fn a_stop_at_the_market_warns_nothing_and_with_no_book_the_last_price_is_the_market() {
    let mut h = settled(live());
    set(&mut h, |s| {
        s.order_type = OrderType::Stop;
        s.price = "100.0".to_string();
    });
    let ticket = labels_in(&h, "Order ticket");
    assert!(!ticket.iter().any(|l| l.starts_with("Stop BUY")), "a buy stop at the ask: {ticket:?}");
    let control = "Stop SELL 100.0 is above the market 99.9: it triggers at once.";
    assert!(ticket.iter().any(|l| l == control), "CONTROL: {ticket:?}");
    set(&mut h, |s| s.price = "99.9".to_string());
    let ticket = labels_in(&h, "Order ticket");
    assert!(
        !ticket.iter().any(|l| l.starts_with("Stop SELL")),
        "a sell stop at the bid: {ticket:?}"
    );
    let control = "Stop BUY 99.9 is below the market 100.0: it triggers at once.";
    assert!(ticket.iter().any(|l| l == control), "CONTROL: {ticket:?}");

    let mut h = settled(Scene { book: false, last: Some(100.0), ..live() });
    set(&mut h, |s| {
        s.order_type = OrderType::Stop;
        s.price = "98".to_string();
    });
    let ticket = labels_in(&h, "Order ticket");
    let below = "Stop BUY 98.0 is below the market 100.0: it triggers at once.";
    assert!(ticket.iter().any(|l| l == below), "no book: the last price: {ticket:?}");
    set(&mut h, |s| s.price = "101".to_string());
    let ticket = labels_in(&h, "Order ticket");
    let above = "Stop SELL 101.0 is above the market 100.0: it triggers at once.";
    assert!(ticket.iter().any(|l| l == above), "no book: the last price: {ticket:?}");
}

/// The FW1 review's check 5, the owner's round-2 item 10 applied to the position card: a REVERSE
/// with no price on the side it trades and no last price is refused, with the same stated reason
/// as a market Buy or Sell. Its second half opens a position the size of the current one, blind,
/// and the app's notional cap holds no request without a price. A CLOSE stays: it only takes away
/// what is held. What the trader sees: Reverse disabled, the reason on its hover; Close working.
/// In both tickets. CONTROL: a last price alone gives Reverse its price.
#[test]
fn a_reverse_with_no_price_is_refused_and_a_close_still_closes() {
    let scene = || Scene { book: false, last: None, ..demo() };
    for (panel, size) in [(Panel::Beside, layout::BESIDE_SIZE), (Panel::Under, layout::UNDER_SIZE)]
    {
        let mut h = harness_at(scene(), size, panel);
        h.run();
        h.state_mut().emitted.clear();
        assert!(disabled(&h, "Reverse"), "{panel:?}: Reverse is disabled");
        click(&mut h, "Reverse");
        assert!(h.state().emitted.is_empty(), "{panel:?}: {:?}", h.state().emitted);
        assert!(says_on_hover(&mut h, "Reverse", ticket::NO_MARKET_PRICE_WHY), "{panel:?}");
        assert!(!disabled(&h, "Close"), "{panel:?}: Close stays");
        click(&mut h, "Close");
        assert_eq!(h.state().emitted, [TradeAction::ClosePosition], "{panel:?}: Close closes");
    }
    let h = settled(Scene { book: false, last: Some(100.0), ..demo() });
    assert!(!disabled(&h, "Reverse"), "CONTROL: a last price is enough");
}

/// What a trader at the confirm does: click Place, where the strip still offers it.
fn confirm(h: &mut Harness<'static, Fixture>) {
    if h.query_by_label("Place").is_some() {
        click(h, "Place");
    }
}

/// `emitted` is the one note a held order leaves when the price it needs went while it waited:
/// "Not sent: …", ending in [`ticket::NO_MARKET_PRICE_WHY`] word for word, the sentence the
/// refusal at the click says (one source for it).
fn assert_dropped_for_no_price(what: &str, emitted: &[TradeAction]) {
    match emitted {
        [TradeAction::Note { kind: StatusKind::Error, text }] => assert!(
            text.starts_with("Not sent:") && text.ends_with(ticket::NO_MARKET_PRICE_WHY),
            "{what}: {text}"
        ),
        other => panic!("{what}: one error note, got {other:?}"),
    }
}

/// The FW4 dispatch, MONEY PATH (the owner's round-2 item 10, its HELD half): a REVERSE waiting for
/// its confirm is checked for a price at every frame, not only at the click. One-click is forced off
/// on LIVE, so there every Reverse waits in the strip; if the book and the last price go while it
/// waits, Place would send a market order nobody can price, which the app's notional cap cannot
/// hold. It is dropped with a note naming the cause, and the confirm sends nothing. CONTROL: while
/// the price is there it waits, and Place sends it.
#[test]
fn a_held_reverse_is_dropped_when_its_price_goes_and_kept_while_it_has_one() {
    for priced in [true, false] {
        let mut h = settled(live());
        click(&mut h, "Reverse");
        assert_eq!(h.state().state.held, Some(TradeAction::Reverse), "{priced}: held");
        if !priced {
            h.state_mut().scene.book = false;
            h.state_mut().scene.last = None;
        }
        h.run();
        confirm(&mut h);
        let emitted = &h.state().emitted;
        if priced {
            assert_eq!(emitted, &[TradeAction::Reverse], "CONTROL: priced, it waits and is sent");
            continue;
        }
        assert!(
            !emitted.contains(&TradeAction::Reverse),
            "the Reverse went out blind: {emitted:?}"
        );
        assert_eq!(h.state().state.held, None, "dropped");
        assert!(h.query_by_label("Place").is_none(), "nothing left to confirm");
        assert_dropped_for_no_price("a Reverse", emitted);
    }
}

/// FW4, the same for a held MARKET Buy or Sell, per side: on a book that loses one side, with no
/// last price, the order that fills against the side that went (a Buy takes the asks, a Sell the
/// bids) is dropped with the same note and Place sends nothing; the other side's order still has
/// its price, waits, and Place sends it as held (the CONTROL on the same book). A market order is
/// checked at the click (`ticket::order`) and again while it waits.
#[test]
fn a_held_market_order_whose_side_loses_its_price_is_dropped_and_the_other_side_kept() {
    // `one_side` keeps that side of the book: `+1` the bids alone, so the asks are gone.
    for (what, button, one_side, dropped) in [
        ("a Buy, the asks gone", "Buy 0.010 market", 1, true),
        ("CONTROL: a Sell, the asks gone", "Sell 0.010 market", 1, false),
        ("a Sell, the bids gone", "Sell 0.010 market", -1, true),
        ("CONTROL: a Buy, the bids gone", "Buy 0.010 market", -1, false),
    ] {
        let mut h = settled(Scene { last: None, ..live() });
        set(&mut h, |s| s.order_type = OrderType::Market);
        click(&mut h, button);
        let held = h.state().state.held.clone().expect("held for a confirm");
        assert!(
            matches!(held, TradeAction::Place { order_type: OrderType::Market, .. }),
            "{what}: {held:?}"
        );
        h.state_mut().scene.one_side = Some(one_side);
        h.run();
        confirm(&mut h);
        if dropped {
            assert!(places(h.state()).is_empty(), "{what}: sent blind: {:?}", h.state().emitted);
            assert_eq!(h.state().state.held, None, "{what}: dropped");
            assert_dropped_for_no_price(what, &h.state().emitted);
        } else {
            assert_eq!(h.state().emitted, [held], "{what}: kept, and Place sends it as held");
        }
    }
}

/// A way a test puts an order in the strip for a confirm.
type Hold = fn(&mut Harness<'static, Fixture>);

/// FW4, what the price check leaves alone: a held CLOSE only takes away what is held (reduce-only),
/// so with no book and no last price it still waits and Place still sends it; a held LIMIT or STOP
/// carries its own price, so it waits too and Place sends it as held.
#[test]
fn a_held_close_limit_or_stop_is_kept_when_the_price_goes() {
    let holds: [(&str, Hold); 3] = [
        ("a Close", |h| click(h, "Close")),
        ("a limit", |h| {
            set(h, |s| s.price = "99.5".to_string());
            click(h, "Buy 0.010 limit");
        }),
        ("a stop", |h| {
            set(h, |s| {
                s.order_type = OrderType::Stop;
                s.price = "101".to_string();
            });
            click(h, "Buy 0.010 stop");
        }),
    ];
    for (what, hold) in holds {
        let mut h = settled(live());
        hold(&mut h);
        let held = h.state().state.held.clone().expect("held for a confirm");
        h.state_mut().emitted.clear();
        h.state_mut().scene.book = false;
        h.state_mut().scene.last = None;
        h.run();
        assert_eq!(h.state().state.held.as_ref(), Some(&held), "{what}: it still waits");
        assert!(h.state().emitted.is_empty(), "{what}: {:?}", h.state().emitted);
        click(&mut h, "Place");
        assert_eq!(h.state().emitted, [held], "{what}: Place sends it as held");
    }
}

// ---- FW6: a LIVE window is unmistakable, and the strip says why the ticket sends nothing --------

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

/// Every rectangle the last frame painted, and its fill.
fn filled(h: &Harness<'_, Fixture>) -> Vec<(egui::Rect, egui::Color32)> {
    flat_shapes(h)
        .into_iter()
        .filter_map(|s| match s {
            egui::Shape::Rect(r) => Some((r.rect, r.fill)),
            _ => None,
        })
        .collect()
}

/// The fill of the chip the instrument bar painted `word` on: the smallest filled rectangle under
/// that word, the chip's own pill (the bar's background lies under it too). `None` where the bar
/// paints no such word.
fn chip_fill(h: &Harness<'_, Fixture>, word: &str) -> Option<egui::Color32> {
    let bar = region_rect(h, "Instrument bar");
    let at = painted(h).into_iter().find(|p| p.text == word && bar.contains(p.rect.center()))?;
    filled(h)
        .into_iter()
        .filter(|(r, fill)| *fill != egui::Color32::TRANSPARENT && r.contains(at.rect.center()))
        .min_by(|a, b| a.0.area().total_cmp(&b.0.area()))
        .map(|(_, fill)| fill)
}

/// The strip's rect as `status::height` derives it for the last frame.
fn strip_rect(h: &Harness<'_, Fixture>) -> egui::Rect {
    let (content, strip_h) = (h.state().content, h.state().strip_h);
    egui::Rect::from_min_max(egui::pos2(content.min.x, content.max.y - strip_h), content.max)
}

/// The texts the last frame painted in the strip.
fn in_strip(h: &Harness<'_, Fixture>) -> Vec<Painted> {
    let strip = strip_rect(h);
    painted(h).into_iter().filter(|p| strip.contains(p.rect.center())).collect()
}

/// A window the render check drew: what it is, its size, where its ticket sits, whether its ladder
/// is shown.
type Window = (&'static str, egui::Vec2, Panel, bool);

/// The three windows the second render check drew: the default one (the ladder beside the full
/// ticket), the bottom-panel one (the compact ticket under the ladder) and the ticket alone.
const WINDOWS: [Window; 3] = [
    ("the 600 pt window", layout::BESIDE_SIZE, Panel::Beside, true),
    ("the 320 pt window", layout::UNDER_SIZE, Panel::Under, true),
    ("the 280 pt ticket alone", layout::TICKET_ONLY_SIZE, Panel::Beside, false),
];

/// `scene` drawn in `window`, settled, with nothing emitted yet.
fn in_window(scene: Scene, (_, size, panel, ladder): Window) -> Harness<'static, Fixture> {
    let mut h = harness_at(scene, size, panel);
    h.state_mut().state.view.ladder = ladder;
    h.run();
    h.state_mut().emitted.clear();
    h
}

/// Move a drawn window to `window`, as a trader resizing it and choosing its view would.
fn move_to(h: &mut Harness<'static, Fixture>, (_, size, panel, ladder): Window) {
    h.state_mut().state.view = trade::View { ladder, panel };
    h.set_size(size);
    h.run();
}

/// Every look the fit sweep draws: each density at each text size.
fn looks() -> Vec<Appearance> {
    use vike_ui_theme::metrics::Density;
    use vike_ui_theme::type_scale::TextSize;
    Density::ALL
        .into_iter()
        .flat_map(|density| {
            TextSize::ALL.map(|text_size| Appearance {
                density,
                text_size,
                ..Appearance::default()
            })
        })
        .collect()
}

/// The owner's ruling B (2026-10-03): the LIVE chip is filled in the theme's ACCENT, the design
/// system's "LIVE is an accent shape" (spec §2), and not in the danger red (`Status::Error`) the
/// build had given it (FW6, I1). No other mode's chip is filled in the accent: the CONTROLS, DEMO,
/// PAPER and an unknown mode, in whose bars nothing at all is filled in it.
#[test]
fn a_live_window_fills_its_mode_chip_in_the_accent_and_no_other_does() {
    use vike_ui_theme::components::{Status, Tokens};
    let danger = Status::Error.color();
    for (mode, word) in [
        (AccountMode::Live, "LIVE"),
        (AccountMode::Demo, "DEMO"),
        (AccountMode::Paper, "PAPER"),
        (AccountMode::Unknown, "MODE ?"),
    ] {
        let h = settled(Scene { mode, ..demo() });
        let accent = Tokens::of(&h.ctx).theme.accent;
        assert_ne!(danger, accent, "CONTROL: the accent is not the danger red");
        let bar = region_rect(&h, "Instrument bar");
        let chip = painted(&h)
            .into_iter()
            .find(|p| p.text == word && bar.contains(p.rect.center()))
            .unwrap_or_else(|| panic!("{word} is painted in the bar"));
        let under: Vec<egui::Color32> = filled(&h)
            .into_iter()
            .filter(|(r, _)| r.contains(chip.rect.center()))
            .map(|(_, fill)| fill)
            .collect();
        if mode == AccountMode::Live {
            assert!(under.contains(&accent), "LIVE sits on the accent fill: {under:?}");
            assert!(!under.contains(&danger), "and not on the danger red: {under:?}");
        } else {
            let lit = filled(&h).into_iter().any(|(r, fill)| fill == accent && bar.intersects(r));
            assert!(!lit, "{word}: CONTROL: nothing in the bar is filled in the accent");
        }
    }
}

/// FW6, I1: an order held for a confirm on a LIVE account no longer looks like one on a DEMO
/// account. Its prompt's region is WASHED in the LIVE chip's own wash (`chip::Mode::wash`: a fill,
/// which takes no width from the words) and it names the account LIVE in the colour the bar's
/// LIVE chip is PAINTED in, read off the chip as drawn — one source, so a change to the chip's
/// fill moves the prompt's marks with it. That the chip's fill IS the accent (the owner's ruling
/// B, 2026-10-03) is the chip tests' to hold:
/// `a_live_window_fills_its_mode_chip_in_the_accent_and_no_other_does` here, and the kit's
/// `live_is_the_accent_demo_the_warning_paper_the_border`. An account whose mode is not known is
/// treated as LIVE wherever safety is at stake: its prompt is washed too, and never CALLED live.
/// CONTROL: a DEMO prompt has neither.
///
/// The word MOVES NOTHING (the look at 320 pt caught a first version giving it room by turning the
/// answers into ✓ and ✕): in all three windows at every look the fit sweep draws, a LIVE prompt and
/// an unknown mode's are drawn beside the same answers as the DEMO one — the words Place and
/// Cancel, or ✓ and ✕ — in a strip of the same height. The word heads the first line where that
/// line still fits beside those answers, else the second; at the owner's looks (Normal and
/// Comfortable density, Standard text) it is there in every window, for the render check's prompt
/// (a five-digit price, reduce only), whose first line leaves no room for it at 280 pt.
#[test]
fn a_live_prompt_is_washed_in_the_accent_says_live_and_moves_nothing() {
    use vike_ui_theme::components::{Tokens, chip};
    use vike_ui_theme::metrics::Density;
    use vike_ui_theme::type_scale::TextSize;
    for look in looks() {
        for window in WINDOWS {
            let mut demo_form = None;
            // DEMO first: the control the other two are measured against.
            for mode in [AccountMode::Demo, AccountMode::Live, AccountMode::Unknown] {
                let what =
                    format!("{mode:?} in {}, {:?}/{:?}", window.0, look.density, look.text_size);
                let mut h = in_window(Scene { mode, look, ..demo() }, window);
                set(&mut h, |s| {
                    s.one_click = false;
                    s.reduce_only = true;
                    s.price = "84606.9".to_string();
                });
                let buy = label_of(&h, "Buy 0.010 ", "");
                click(&mut h, &buy);
                assert!(h.state().state.held.is_some(), "{what}: CONTROL: held for a confirm");
                let wash = chip::Mode::Live.wash(&Tokens::of(&h.ctx));
                let strip = strip_rect(&h);
                let words = in_strip(&h);
                let first = words
                    .iter()
                    .find(|p| p.text.contains("Buy 0.010"))
                    .unwrap_or_else(|| panic!("{what}: the prompt is painted: {words:?}"));
                let washed = filled(&h).into_iter().any(|(r, fill)| {
                    fill == wash
                        && strip.expand(0.6).contains_rect(r)
                        && r.contains(first.rect.center())
                });
                let live: Vec<&(String, egui::Color32)> = words
                    .iter()
                    .flat_map(|p| &p.sections)
                    .filter(|(s, _)| s.contains("LIVE"))
                    .collect();
                let form = (h.state().strip_h, words.iter().any(|p| p.text == "Place"));
                match mode {
                    AccountMode::Demo => {
                        assert!(!washed, "{what}: CONTROL: a DEMO prompt is not washed");
                        assert!(live.is_empty(), "{what}: CONTROL: nor called LIVE: {words:?}");
                        demo_form = Some(form);
                        continue;
                    }
                    AccountMode::Live => {
                        assert!(washed, "{what}: the prompt is washed in the LIVE chip's wash");
                        let owners = look.text_size == TextSize::Standard
                            && matches!(look.density, Density::Normal | Density::Comfortable);
                        if owners {
                            assert!(!live.is_empty(), "{what}: the word fits here: {words:?}");
                        }
                        if !live.is_empty() {
                            // It heads a line of the prompt (painted top down, before the
                            // answers): the first, else the one after it.
                            let at = words.iter().position(|p| p.text.starts_with("LIVE"));
                            assert!(
                                matches!(at, Some(0 | 1)),
                                "{what}: LIVE heads the first or second line: {words:?}"
                            );
                            let chip = chip_fill(&h, "LIVE");
                            assert!(
                                live.iter().all(|(_, c)| Some(*c) == chip),
                                "{what}: LIVE is said in the chip's own fill {chip:?}: {live:?}"
                            );
                        }
                    }
                    _ => {
                        assert!(washed, "{what}: treated as LIVE, the prompt is washed");
                        assert!(live.is_empty(), "{what}: and never called LIVE: {words:?}");
                    }
                }
                assert_eq!(
                    Some(form),
                    demo_form,
                    "{what}: (strip height, words for answers) as on DEMO: {words:?}"
                );
            }
        }
    }
}

/// FW6, I2: a held Stop whose trigger is on the wrong side of the market says "it triggers at once"
/// in the WARNING colour in both of the prompt's forms — on a line of its own where the prompt's
/// lines fit beside the answers, and among the wrapped words where they do not (at 320 and 280 pt
/// they wrap, and every wrapped word was painted in the text colour). In the compact ticket, which
/// has no trigger field, the strip is the only place the warning shows. Read off the painted
/// sections, in the three windows at every look the fit sweep draws: the words painted in the
/// warning colour are the warning, word for word, and nothing else; and BOTH forms were drawn, so
/// the test cannot pass on one alone.
#[test]
fn a_held_wrong_side_stop_keeps_its_warning_colour_in_both_forms() {
    let warning = "Stop BUY 98.0 is below the market 100.0: it triggers at once.";
    let amber = vike_ui_theme::components::Status::Warning.color();
    let stop = TradeAction::Place {
        side: 1,
        order_type: OrderType::Stop,
        price: Some(98.0),
        qty: 0.01,
        reduce_only: false,
        exits: None,
        origin: Origin::Ticket,
    };
    let (mut whole, mut wrapped) = (Vec::new(), Vec::new());
    for look in looks() {
        for window in WINDOWS {
            let what = format!("{}, {:?}/{:?}", window.0, look.density, look.text_size);
            let mut h = in_window(Scene { look, ..live() }, window);
            h.state_mut().state.held = Some(stop.clone());
            h.run();
            assert!(h.state().state.held.is_some(), "{what}: CONTROL: it waits for a confirm");
            let words = in_strip(&h);
            let warned: Vec<&str> = words
                .iter()
                .flat_map(|p| &p.sections)
                .filter(|(_, c)| *c == amber)
                .map(|(s, _)| s.as_str())
                .collect();
            let said = warned.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
            assert_eq!(said, warning, "{what}: the words in the warning colour: {words:?}");
            if words.iter().any(|p| p.text == warning) {
                whole.push(what);
            } else {
                wrapped.push(what);
            }
        }
    }
    assert!(
        !whole.is_empty() && !wrapped.is_empty(),
        "both forms are drawn: whole in {whole:?}, wrapped in {wrapped:?}"
    );
}

/// The glue's words for TP/SL on an engine whose lane cannot hold the stop-loss (test words: this
/// crate cannot see the glue's `TPSL_LANE_WHY`).
const LANE_WHY: &str = "TP/SL is not available on this account yet: its engine trades the spot \
                        market, where the exchange cannot hold the stop-loss.";

/// FW6, I3 and I4: with no order waiting and no line from the app, the strip says WHY the ticket's
/// Buy and Sell send nothing — the very sentence the form says it in — and "Ready" only when they
/// can send: a ticked TP/SL the account cannot carry, a TP/SL with a leg left blank, an instrument
/// with no lot size, and an account that trades other symbols (the shortened list included, I4).
/// A Stop entry refuses a ticked TP/SL in the FULL ticket only: the compact one sends at market,
/// which carries one, so under the ladder the strip is Ready (the control on which ticket it reads).
/// In the three windows, at every look the fit sweep draws, and never cut short: where two lines
/// do not hold the sentence it wraps on, and the strip grows to hold it.
#[test]
fn the_strip_says_why_the_ticket_sends_nothing_and_ready_only_when_it_can() {
    let five: Vec<String> =
        ["ETH-PERPETUAL", "SOL-PERPETUAL", "XRP-PERPETUAL", "DOGE-PERPETUAL", "ADA-PERPETUAL"]
            .map(String::from)
            .to_vec();
    let all = |why: String| [Some(why.clone()), Some(why.clone()), Some(why)];
    let ticked: Tweak = |s| s.tpsl = true;
    let cases: Vec<(&str, Scene, Tweak, [Option<String>; 3])> = vec![
        (
            "TP/SL ticked where the account's lane cannot carry it",
            Scene { bracket_why: Some(LANE_WHY), ..demo() },
            ticked,
            all(ticket::tpsl_refusal(LANE_WHY)),
        ),
        (
            "TP/SL ticked with its stop-loss left blank",
            demo(),
            |s| {
                s.tpsl = true;
                s.sl_text.clear();
            },
            all(ticket::TPSL_LEGS_WHY.to_string()),
        ),
        (
            "no lot size",
            Scene { grid: NO_LOT, ..demo() },
            |_| {},
            all(ticket::no_lot_reason("BTCUSDT")),
        ),
        (
            "an account that trades another symbol",
            Scene { tradable: false, ..demo() },
            |_| {},
            all("This account does not trade BTCUSDT. It trades ETHUSDT.".to_string()),
        ),
        (
            "an account that trades five other symbols",
            Scene { tradable: false, trades: five, ..demo() },
            |_| {},
            all("This account does not trade BTCUSDT. It trades ETH-PERPETUAL, SOL-PERPETUAL and \
                 3 more."
                .to_string()),
        ),
        (
            "TP/SL ticked on a Stop entry",
            demo(),
            |s| {
                s.tpsl = true;
                s.order_type = OrderType::Stop;
            },
            {
                let stop = Some(ticket::tpsl_refusal(ticket::TPSL_STOP_WHY));
                [stop.clone(), None, stop]
            },
        ),
        ("CONTROL: nothing refused", demo(), |_| {}, [None, None, None]),
    ];
    let mut found = Vec::new();
    for look in looks() {
        for (what, scene, tweak, want) in &cases {
            let mut h =
                harness_at(Scene { look, ..scene.clone() }, layout::BESIDE_SIZE, Panel::Beside);
            h.run();
            set(&mut h, *tweak);
            for (window, want) in WINDOWS.into_iter().zip(want) {
                move_to(&mut h, window);
                let at = format!("{what}, {}, {:?}/{:?}", window.0, look.density, look.text_size);
                let strip = labels_in(&h, "Status strip");
                let cut: Vec<String> =
                    in_strip(&h).into_iter().filter(|p| p.elided).map(|p| p.text).collect();
                if !cut.is_empty() {
                    found.push(format!("{at}: cut short: {cut:?}"));
                }
                let ready = strip.iter().any(|l| l.starts_with("Ready"));
                match want {
                    Some(why) if !strip.contains(why) || ready => {
                        found.push(format!("{at}: the strip should say {why:?}: {strip:?}"));
                    }
                    None if !ready => found.push(format!("{at}: CONTROL: Ready: {strip:?}")),
                    _ => {}
                }
            }
        }
    }
    assert!(found.is_empty(), "{} finding(s):\n{}", found.len(), found.join("\n"));
}
