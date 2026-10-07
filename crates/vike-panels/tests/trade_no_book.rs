//! **The Trade window must not draw a ladder it does not have** — the headless contract behind the
//! 2026-09-15 fix, asserted off the accessibility tree so it runs on the GPU-less CI runners like
//! any other test. Carried over from the DOM's own gate of the same name, test for test (spec
//! §4.2, "the honesty contract of the DOM carries over unchanged").
//!
//! # What was wrong
//!
//! The DOM's glue in vike-app-core (the `tool_views` module's DOM file, deleted with the DOM window)
//! used to fall back to `vike_app_core::orders::dom_math::synth_book` whenever the real
//! `(venue, symbol)` book was absent or empty. That helper built **40 gapless, perfectly symmetric
//! levels per side** around a seeded price, with uniform-random sizes from a counter-seeded LCG —
//! and its seed was `DomState::tick`, the repaint counter the DOM's `draw` incremented at its own
//! top. So the entire ladder **re-rolled on every frame**: it animated convincingly while the
//! prices never moved, and the only tells were a dim overlay and a `● STALE` badge. The mark fell
//! back to `default_price`, a table answering `62_800.0` for `BTCUSDT`, printed in the header in
//! the same amber and the same slot a venue's real last price uses.
//!
//! Deleting the two helpers fixed the app. These tests fix the **widget**, so no future caller can
//! reintroduce the behaviour by handing an invented book in through the seam: `trade::draw` takes
//! the no-book decision from the book it is given, and every assertion here is over what it renders
//! when that book is empty.
//!
//! # And the second question these answer
//!
//! The window's depth arrives on the **datahub** market-data link. The shell's status bar reports
//! the **tradehub** observe connection, which carries no book at all —
//! `vike_tradehub_client::wire::WireSnapshot` has no book, depth, quote or tape field,
//! `vike_core::CoreSnapshot` has none either, and `vike_core`'s `Ingest::Book` arm folds the
//! `Arc<L2Book>` and drops it. So `tradehub [LIVE] — OBSERVING … (connected)` says nothing whatever
//! about whether this window has data, and until the DOM's source strip existed nothing on screen
//! said so. In the Trade window that strip is the instrument bar's **FEED badge** (Ruling R1): its
//! word classifies the link, and its hover carries the link's own status line verbatim.
//! [`the_window_always_states_its_own_market_data_link`] is that gate.
//!
//! ⚠ `Harness::run` steps until repaints settle, so only the first frame of a `run()` sees a click,
//! and `Fixture::emitted` ACCUMULATES — every test drains it before interacting. Every harness sets
//! egui's tooltip delay to zero (pre-flight I10): its 0.5 s default outlasts `Harness::run`.

use egui::accesskit::Role;
use egui::{Pos2, Rect, Shape, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use vike_model::{BookLevel, L2Book, VenueCaps};
use vike_panels::trade::ladder::{NO_BOOK_HEADLINE, NO_BOOK_SUBLINE};
use vike_panels::trade::{
    self, AccountMode, BookAbsence, Grid, LadderOrder, Panel, Position, Tradable, TradeAction,
    TradeInputs, TradeState, View, instrument, layout,
};
use vike_ui_theme::appearance::Appearance;
use vike_ui_theme::components::Tokens;
use vike_ui_theme::value::trade as trade_values;

/// A 1.0 tick and a 0.001 lot: the window seeds its size at ten lots, `0.010`.
const GRID: Grid = Grid { tick: 1.0, lot: 0.001, min_qty: 0.001 };
const LIVE_SOURCE: &str = "datahub 127.0.0.1:7878 — 1/1 stream(s) live";
/// The window's size, beside layout: the size it opens at.
const SIZE: egui::Vec2 = trade_values::BESIDE_SIZE;

/// What one window draws, owned so the harness can lend it to `TradeInputs` every frame.
#[derive(Clone)]
struct Scene {
    book: L2Book,
    source: &'static str,
    absence: Option<BookAbsence<'static>>,
    last: Option<f64>,
    stale: bool,
    mode: AccountMode,
    position: Option<Position>,
    orders: Vec<LadderOrder>,
}

struct Fixture {
    state: TradeState,
    emitted: Vec<TradeAction>,
    /// How many frames the window has drawn: the Trade window keeps no frame counter of its own,
    /// so the harness counts (pre-flight I11(b)).
    frames: u64,
    /// The content rect and the tokens of the last frame, for the geometry `draw` derives.
    drawn: Option<(Rect, Tokens)>,
    /// The bar's height as `draw` derives it for the last frame's inputs.
    bar_h: f32,
}

/// A book with NO levels on either side — the state the widget must refuse to draw a ladder over.
/// `source` and `absence` are what the app's glue would supply. DEMO, so a click sends at once and
/// nothing a click did can hide in a held order; no position and no orders.
fn bookless(source: &'static str, absence: Option<BookAbsence<'static>>) -> Scene {
    Scene {
        book: L2Book::new(1.0),
        source,
        absence,
        last: None,
        stale: false,
        mode: AccountMode::Demo,
        position: None,
        orders: Vec::new(),
    }
}

/// Bids 99/98/97, asks 100/101/102 on a 1.0 tick — a real two-sided book, so the same harness can
/// serve as the POSITIVE CONTROL for every "and this is what it does when there IS one" half.
fn with_book(source: &'static str) -> Scene {
    let mut b = L2Book::new(1.0);
    b.apply_snapshot(
        1,
        &[BookLevel::new(99.0, 1.0), BookLevel::new(98.0, 2.0), BookLevel::new(97.0, 3.0)],
        &[BookLevel::new(100.0, 1.0), BookLevel::new(101.0, 2.0), BookLevel::new(102.0, 3.0)],
    );
    Scene { book: b, last: Some(99.5), ..bookless(source, None) }
}

/// The whole window, beside layout, at the size it opens at, drawing `scene`.
fn window(scene: Scene) -> Harness<'static, Fixture> {
    let fixture = Fixture {
        state: TradeState::default(),
        emitted: Vec::new(),
        frames: 0,
        drawn: None,
        bar_h: 0.0,
    };
    Harness::builder().with_size(SIZE).build_ui_state(
        move |ui, f: &mut Fixture| {
            if !vike_ui_theme::harness::appearance_ready(ui.ctx(), &Appearance::default()) {
                return;
            }
            ui.ctx().all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
            f.frames += 1;
            let s = &scene;
            let inputs = TradeInputs {
                venue: "binance",
                venue_label: "Binance",
                product: "",
                account: None,
                symbol: "BTCUSDT",
                base: "BTC",
                quote: "USDT",
                mode: s.mode,
                tradable: Tradable::Yes,
                grid: GRID,
                book: &s.book,
                last: s.last,
                stale: s.stale,
                source: s.source,
                absence: s.absence,
                orders: &s.orders,
                orders_why: None,
                position: s.position,
                buying_power: None,
                caps: VenueCaps::UNSUPPORTED,
                bracket_why: None,
                bracket_wire: false,
                matches: &[],
                recent: &[],
                accounts: &[],
                accounts_why: None,
                unconnected: &[],
                tape: &[],
                status: None,
            };
            let t = Tokens::of(ui.ctx());
            let content = ui.available_rect_before_wrap();
            f.bar_h = instrument::height_of(
                instrument::rows_for(ui.ctx(), &t, &inputs, content.width()),
                &t,
            );
            f.drawn = Some((content, t));
            let actions = trade::draw(ui, &mut f.state, &inputs);
            f.emitted.extend(actions);
        },
        fixture,
    )
}

/// A window drawn until it settles, with nothing emitted yet.
fn settled(scene: Scene) -> Harness<'static, Fixture> {
    let mut h = window(scene);
    h.run();
    h.state_mut().emitted.clear();
    h
}

/// Every `Role::Label` value currently on screen. `Role::Label` is the one role whose text egui
/// files under the node's VALUE rather than its label (see egui's
/// `fill_accesskit_node_from_widget_info`), which is why every query here goes through it.
fn labels(h: &Harness<'static, Fixture>) -> Vec<String> {
    labels_at(h).into_iter().map(|(l, _)| l).collect()
}

/// Every `Role::Label` value on screen, with the rect of its node.
fn labels_at(h: &Harness<'static, Fixture>) -> Vec<(String, Rect)> {
    h.root()
        .children_recursive()
        .filter(|n| n.accesskit_node().role() == Role::Label)
        .map(|n| (n.accesskit_node().value().as_deref().unwrap_or("").to_string(), n.rect()))
        .collect()
}

/// Every text the last frame PAINTED, with where: the ladder's captions and prices are painted
/// text, which has no node, so "no caption" and "no price" are asked of the shapes.
fn painted(h: &Harness<'static, Fixture>) -> Vec<(String, Pos2)> {
    fn walk(s: &Shape, out: &mut Vec<(String, Pos2)>) {
        match s {
            Shape::Text(t) => {
                out.push((t.galley.text().to_string(), t.visual_bounding_rect().center()))
            }
            Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for c in &h.output().shapes {
        walk(&c.shape, &mut out);
    }
    out
}

/// The bar's band and the body's split, as `draw` derives them from the last frame's content rect
/// (pre-flight I11(f)): `instrument::height` for the bar, `layout::split` for the ladder and the
/// ticket. The strip's top is read back from its named region, which `draw` sizes.
struct Geometry {
    bar: Rect,
    ladder: Option<Rect>,
    ticket: Rect,
}

fn geometry(h: &Harness<'static, Fixture>) -> Geometry {
    let (content, t) = h.state().drawn.expect("the window drew");
    let strip = h
        .root()
        .children_recursive()
        .find(|n| {
            let a = n.accesskit_node();
            a.role() == Role::Group && a.label().as_deref() == Some("Status strip")
        })
        .map_or(content.max.y, |n| n.rect().min.y);
    let bar = Rect::from_min_size(content.min, vec2(content.width(), h.state().bar_h));
    let body = Rect::from_min_max(pos2(content.min.x, bar.max.y), pos2(content.max.x, strip));
    let view = View { chart: false, ladder: true, panel: Panel::Beside };
    let rects = layout::split(body, view, &t.metrics, layout::ticket_w(&t));
    Geometry { bar, ladder: rects.ladder, ticket: rects.ticket }
}

/// A primary click at an ARBITRARY point, which `kittest::Node::click` cannot do — it always aims
/// at a node's centre, and the whole point here is to click where a LADDER ROW would be rather than
/// at a widget that exists.
fn click_at(h: &Harness<'static, Fixture>, pos: Pos2) {
    h.event(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
    }
}

/// Mid-ladder, in the BID column — a real, clickable row when a book is present — from the
/// geometry `draw` derives: the ladder's rect from `layout::split`, its Bid column the second of
/// five at a fifth of its width in. The control in
/// [`a_bookless_ladder_offers_no_price_to_click`] is what keeps that claim honest.
fn ladder_point(h: &Harness<'static, Fixture>) -> Pos2 {
    let ladder = geometry(h).ladder.expect("the window opens with room for a ladder");
    pos2(ladder.min.x + 0.25 * ladder.width(), ladder.center().y)
}

/// A window with no book must SAY it has no book, and must say WHY — the cause and the next step
/// the caller supplied, both on screen verbatim, plus the line stating outright that nothing is
/// drawn.
///
/// Reddens on removing the ladder's empty-book branch (the whole empty state goes with it) and on
/// dropping any line out of its words.
#[test]
fn a_bookless_ladder_states_the_absence_its_cause_and_the_next_step() {
    let absence = BookAbsence {
        headline: "NO ORDER BOOK — MARKET-DATA LINK DOWN",
        cause: "datahub 127.0.0.1:7878 — no streams wanted on this venue",
        next_step: "Check the DATAHUB address in Connections.",
    };
    let h = settled(bookless("idle", Some(absence)));
    let ls = labels(&h);
    for want in [absence.headline, NO_BOOK_SUBLINE, absence.cause, absence.next_step] {
        assert!(ls.iter().any(|l| l == want), "the empty state must carry {want:?}; saw {ls:?}");
    }
}

/// With no [`BookAbsence`] at all the widget still refuses to draw a ladder: it falls back to its
/// own headline rather than to anything book-shaped — no column caption and no price in the
/// ladder's rect. The captions and the prices are PAINTED (they have no node), so this asks the
/// painted shapes; the POSITIVE CONTROL is the same window over a real book, where both ARE
/// painted there, so the absence below cannot be true merely because nothing was looked at.
///
/// This is the belt for a caller that forgets the field — which is the exact shape of the original
/// defect, where the app had no answer and invented one.
#[test]
fn a_bookless_ladder_with_no_explanation_still_draws_no_ladder() {
    const CAPTIONS: [&str; 5] = ["Buy", "Bid", "Price", "Ask", "Sell"];
    let price_like = |s: &str| s.starts_with(|c: char| c.is_ascii_digit());
    // Everything painted in the ladder's rect UNDER its toolbar (one control high and a point
    // above and below), which holds the grouping's own number, `1`, in both states.
    let in_ladder = |h: &Harness<'static, Fixture>| -> Vec<String> {
        let ladder = geometry(h).ladder.expect("a ladder's rect");
        let toolbar = h.state().drawn.expect("the window drew").1.metrics.control_h + 2.0;
        let below = Rect::from_min_max(pos2(ladder.min.x, ladder.min.y + toolbar), ladder.max);
        painted(h).into_iter().filter(|(_, at)| below.contains(*at)).map(|(s, _)| s).collect()
    };

    let control = settled(with_book(LIVE_SOURCE));
    let drawn = in_ladder(&control);
    assert!(CAPTIONS.iter().all(|c| drawn.iter().any(|d| d == c)), "CONTROL: {drawn:?}");
    assert!(drawn.iter().any(|d| price_like(d)), "CONTROL: prices are painted: {drawn:?}");

    let h = settled(bookless("", None));
    let ls = labels(&h);
    assert!(
        ls.iter().any(|l| l == NO_BOOK_HEADLINE),
        "a bookless window with no supplied words must still announce the absence; saw {ls:?}"
    );
    assert!(
        ls.iter().any(|l| l == NO_BOOK_SUBLINE),
        "and must still state that nothing is drawn; saw {ls:?}"
    );
    let drawn = in_ladder(&h);
    assert!(!drawn.iter().any(|d| CAPTIONS.contains(&d.as_str())), "no caption: {drawn:?}");
    assert!(!drawn.iter().any(|d| price_like(d)), "no price: {drawn:?}");
}

/// ⚠ **THE ANIMATION PROOF** — the assertion closest to the reported defect.
///
/// The old fallback re-rolled all 80 levels every frame off the DOM's frame counter. If anything in
/// the bookless path were frame-seeded, two batches of frames over the same (absent) book would
/// render differently. The Trade window keeps no counter, so the HARNESS counts the frames it drew,
/// and that the count really advances between the batches is asserted FIRST: without it this test
/// could pass by no frame having been drawn at all, which would make it green for a reason that has
/// nothing to do with the property it claims.
#[test]
fn a_bookless_ladder_does_not_animate() {
    let connecting = "datahub 127.0.0.1:7878 — connecting, 0/1 stream(s)";
    let mut h = window(bookless(
        connecting,
        Some(BookAbsence {
            headline: "NO ORDER BOOK YET — CONNECTING",
            cause: connecting,
            next_step: "Nothing to do.",
        }),
    ));
    h.run();
    let first = labels(&h);
    let frames_after_first = h.state().frames;
    h.run();
    let second = labels(&h);
    assert!(
        h.state().frames > frames_after_first,
        "the window must actually draw again between the two batches, or this test proves nothing \
         about frame-seeded content (was {frames_after_first}, now {})",
        h.state().frames
    );
    assert!(!first.is_empty(), "the empty state must render something");
    assert_eq!(
        first, second,
        "a window with no book must render the SAME thing on every frame — the old synthetic ladder \
         re-rolled all 80 levels from a frame counter and animated a market that was not there"
    );
}

/// A bookless window offers no price to click: there is no ladder to hit-test, so a click in the
/// middle of where its rows would be cannot place an order at a price the widget invented.
///
/// ⚠ The POSITIVE CONTROL is half this test. The same point over the same geometry with a REAL book
/// must emit a `Place` — otherwise "no action was emitted" would be equally true of a test whose
/// input never reached the widget at all, and would gate nothing.
#[test]
fn a_bookless_ladder_offers_no_price_to_click() {
    let mut control = settled(with_book(LIVE_SOURCE));
    let at = ladder_point(&control);
    click_at(&control, at);
    control.run();
    assert!(
        control.state().emitted.iter().any(|a| matches!(a, TradeAction::Place { .. })),
        "CONTROL FAILED: the click never reached a ladder row, so the assertion below would be \
         vacuous. Emitted: {:?}",
        control.state().emitted
    );

    let mut h = settled(bookless("idle", None));
    click_at(&h, at);
    h.run();
    assert!(
        h.state().emitted.is_empty(),
        "no book ⇒ no placeable price, but got {:?}",
        h.state().emitted
    );
    assert!(h.state().state.held.is_none(), "and nothing is held for a confirm either");
}

/// The bar must not print a fabricated price: with no book and no mark it shows a dash, in the bar.
///
/// Reddens on restoring a `default_price` fallback, which answered `62 800.00` for BTCUSDT and
/// rendered in the same slot as a venue's real last. The scene carries no position (pre-flight
/// I11(d)), so the whole window is asked, the ticket included. The second half is the control — a
/// real mark is still printed, so the dash is not a blanket blank.
#[test]
fn no_price_renders_a_dash_and_a_real_mark_still_renders() {
    let h = settled(Scene { stale: true, ..bookless("idle", None) });
    let g = geometry(&h);
    let at = labels_at(&h);
    assert!(
        at.iter().any(|(l, r)| l == "—" && g.bar.contains(r.center())),
        "a priceless window must show a dash in the bar; saw {at:?}"
    );
    // A rendered price is a label that STARTS with a digit and carries a decimal point. Asked of
    // the WHOLE window, the ticket included (W4 fix round 1, M5): with no position the ticket holds
    // no such label — the size is a text field, the quick sizes are buttons, and the value line
    // reads `≈ — USDT` — so no part of the window is set aside. (The DOM's port set the ticket
    // aside, and an exception that excuses nothing would hide a price drawn there.)
    let price_like = |l: &str| l.starts_with(|c: char| c.is_ascii_digit()) && l.contains('.');
    assert!(
        g.ticket.width() > 0.0 && at.iter().any(|(_, r)| g.ticket.contains(r.center())),
        "the ticket's labels are asked too: {at:?}"
    );
    assert!(
        !at.iter().any(|(l, _)| price_like(l)),
        "no formatted price may appear when there is none; saw {at:?}"
    );
    let ls = labels(&h);
    assert!(
        !ls.iter().any(|l| l == "STALE"),
        "a STALE badge claims the depth in front of you is old; with no depth it decorates an \
         absence, and it was half of what disclosed the synthetic ladder. Saw {ls:?}"
    );

    let priced = settled(Scene { last: Some(62_800.0), ..bookless("idle", None) });
    let pl = labels(&priced);
    assert!(
        pl.iter().any(|l| l.replace([',', ' '], "").contains("62800")),
        "a real mark must still be shown; saw {pl:?}"
    );

    // ...and the badge control: a REAL book flagged stale must still say so. Without this, the
    // suppression above would be indistinguishable from having deleted the badge outright.
    let stale = settled(Scene { stale: true, ..with_book("idle") });
    let sl = labels(&stale);
    assert!(
        sl.iter().any(|l| l == "STALE"),
        "an actual stale ladder must keep its badge — the rows ARE old and a trader must know; \
         saw {sl:?}"
    );
}

/// Hover the FEED badge reading `word`; whether that ADDED `words` to the screen (the badge's hover
/// is where the link's own status line is, verbatim, since Ruling R1). The pointer first rests
/// where nothing is, so a tooltip an earlier hover left open cannot answer for this one.
fn badge_says(h: &mut Harness<'static, Fixture>, word: &str, words: &str) -> bool {
    h.hover_at(pos2(1.0, 1.0));
    h.run();
    let before = h.query_all_by_label_contains(words).count();
    h.get_by_label(word).hover();
    h.run();
    h.query_all_by_label_contains(words).count() > before
}

/// ⚠ **THE TWO-SOCKET GATE.** The window must state its OWN link's state, because the shell's
/// status bar describes a DIFFERENT connection that carries no book (see this file's module doc).
/// The FEED badge is asserted present in EVERY state, including over a populated ladder — a link
/// that showed only when the book was missing could not tell a trader that the rows in front of
/// them are the last ones that arrived before the link died. Its word is one of
/// `instrument::feed_word`'s five, and hovering it says the link's own status line verbatim
/// (pre-flight I11(e): R1 moved the verbatim half into the badge's hover, and a port that did not
/// hover would silently drop it).
#[test]
fn the_window_always_states_its_own_market_data_link() {
    use vike_model::feed_status::ConnectionState as C;
    let words: Vec<&str> = [C::Connected, C::Connecting, C::Disconnected, C::Error, C::Unknown]
        .map(instrument::feed_word)
        .to_vec();
    // over a POPULATED ladder
    let mut live = settled(with_book(LIVE_SOURCE));
    assert!(labels(&live).iter().any(|l| l == "FEED UP"), "{:?}", labels(&live));
    assert!(badge_says(&mut live, "FEED UP", LIVE_SOURCE), "the source line, verbatim, on hover");

    // ...and over an EMPTY one, in each state the market-data session can report
    for (source, word) in [
        (LIVE_SOURCE, "FEED UP"),
        ("datahub 127.0.0.1:7878 — connecting, 0/1 stream(s)", "FEED DIALLING"),
        ("idle", "FEED DOWN"),
        ("datahub error: connection refused", "FEED FAULT"),
        ("datahub 127.0.0.1:7878 — no streams wanted on this venue", "FEED ?"),
    ] {
        assert!(words.contains(&word), "{word} is one of the badge's five words");
        let mut h = settled(bookless(source, None));
        let ls = labels(&h);
        assert!(
            ls.iter().any(|l| l == word),
            "source {source:?} must classify as {word:?}; saw {ls:?}"
        );
        assert!(badge_says(&mut h, word, source), "{source:?}: its words verbatim on hover");
    }
}

/// An empty source string is an absence of information, not a state: the badge reads `FEED ?`, not
/// `FEED DOWN`, and its hover says `source unknown` rather than borrow the look of any of the five
/// real ones.
#[test]
fn an_unknown_source_says_so_rather_than_guessing() {
    let mut h = settled(bookless("", None));
    let ls = labels(&h);
    assert!(ls.iter().any(|l| l == "FEED ?"), "an empty status reads as unknown; saw {ls:?}");
    assert!(!ls.iter().any(|l| l == "FEED DOWN"), "never as a dead link; saw {ls:?}");
    assert!(badge_says(&mut h, "FEED ?", "source unknown"), "and its hover says so");
}

/// The ticket stays live over an empty book, and that is deliberate rather than an oversight:
/// Close and the cancels act on a POSITION and on RESTING ORDERS, both of which exist independently
/// of whether this client currently has depth. A trader whose link just dropped with a position
/// open must still be able to flatten it. The scene carries a position and one working order, on a
/// DEMO account, so neither is disabled for want of one nor HELD for a confirm (pre-flight I11(c)).
///
/// It is also the input-plumbing control for the whole file — if this failed, every "emitted
/// nothing" assertion above would be true for the wrong reason.
#[test]
fn a_bookless_window_can_still_flatten_and_cancel() {
    let working = LadderOrder {
        client_order_id: "b1".to_string(),
        side: 1,
        price: 98.0,
        qty: 0.01,
        is_stop: false,
    };
    let scene = Scene {
        position: Some(Position { size: 0.05, avg_px: 99.0, upnl: 0.05 }),
        orders: vec![working],
        ..bookless("idle", None)
    };
    let mut h = settled(scene);
    h.get_by_label("Cancel all 1").click();
    h.run();
    assert_eq!(
        h.state().emitted,
        vec![TradeAction::CancelAll],
        "a position and resting orders outlive the book feed; the ticket must keep working"
    );

    h.state_mut().emitted.clear();
    h.get_by_label("Close").click();
    h.run();
    assert_eq!(h.state().emitted, vec![TradeAction::ClosePosition]);
}
