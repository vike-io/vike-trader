//! **The DOM must not draw a ladder it does not have** — the headless contract behind the
//! 2026-09-15 fix, asserted off the accessibility tree so it runs on the GPU-less CI runners like
//! any other test.
//!
//! # What was wrong
//!
//! `crates/vike-app-core/src/tool_views/dom.rs` used to fall back to
//! `vike_app_core::dom_math::synth_book` whenever the real `(venue, symbol)` book was absent or
//! empty. That helper built **40 gapless, perfectly symmetric levels per side** around a seeded
//! price, with uniform-random sizes from a counter-seeded LCG — and its seed was `DomState::tick`,
//! the repaint counter `vike_panels::dom::draw` increments at its own top. So the entire ladder
//! **re-rolled on every frame**: it animated convincingly while the prices never moved, and the
//! only tells were a dim overlay and a `● STALE` badge. The mark fell back to `default_price`, a
//! table answering `62_800.0` for `BTCUSDT`, printed in the header in the same amber and the same
//! slot a venue's real last price uses.
//!
//! Deleting the two helpers fixes today's app. These tests fix the **widget**, so no future caller
//! can reintroduce the behaviour by handing an invented book in through the seam: `draw` now takes
//! the no-book decision from the book it is given, and every assertion here is over what it renders
//! when that book is empty.
//!
//! # And the second question these answer
//!
//! The DOM's depth arrives on the **datahub** market-data link. The shell's status bar reports the
//! **tradehub** observe connection, which carries no book at all —
//! `vike_tradehub_client::wire::WireSnapshot` has no book, depth, quote or tape field,
//! `vike_core::CoreSnapshot` has none either, and `vike_core`'s `Ingest::Book` arm folds the
//! `Arc<L2Book>` and drops it. So `tradehub [LIVE] — OBSERVING … (connected)` says nothing whatever
//! about whether this window has data, and until the source strip existed nothing on screen said
//! so. [`the_dom_always_states_its_own_market_data_link`] is that gate.
//!
//! ⚠ `Harness::run` steps until repaints settle, so only the first frame of a `run()` sees a click,
//! and `Fixture::emitted` ACCUMULATES — every test drains it before interacting.

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use vike_model::{BookLevel, L2Book, VenueCaps};
use vike_panels::{BookAbsence, DomAction, DomInputs, DomState, NO_BOOK_HEADLINE, NO_BOOK_SUBLINE};

struct Fixture {
    dom: DomState,
    emitted: Vec<DomAction>,
}

/// A book with NO levels on either side — the state the widget must refuse to draw a ladder over.
/// `source` and `absence` are what `dom_tool_content` would supply.
fn bookless(
    source: &'static str,
    absence: Option<BookAbsence<'static>>,
    last: Option<f64>,
) -> Harness<'static, Fixture> {
    build(L2Book::new(1.0), source, absence, last, true)
}

/// Bids 99/98/97, asks 100/101/102 on a 1.0 tick — a real two-sided book, so the same harness can
/// serve as the POSITIVE CONTROL for every "and this is what it does when there IS one" half.
fn with_book(source: &'static str) -> Harness<'static, Fixture> {
    let mut b = L2Book::new(1.0);
    b.apply_snapshot(
        1,
        &[BookLevel::new(99.0, 1.0), BookLevel::new(98.0, 2.0), BookLevel::new(97.0, 3.0)],
        &[BookLevel::new(100.0, 1.0), BookLevel::new(101.0, 2.0), BookLevel::new(102.0, 3.0)],
    );
    build(b, source, None, Some(99.5), false)
}

/// The same real book, flagged STALE — the control for
/// [`no_price_renders_a_dash_and_a_real_mark_still_renders`]'s badge half.
fn with_stale_book() -> Harness<'static, Fixture> {
    let mut b = L2Book::new(1.0);
    b.apply_snapshot(1, &[BookLevel::new(99.0, 1.0)], &[BookLevel::new(100.0, 1.0)]);
    build(b, "idle", None, Some(99.5), true)
}

/// Wide and tall enough that the footer's five right-aligned buttons all lay out — the DOM draws
/// its footer into an exact-size child rect, so a cramped harness would clip controls out of the
/// tree and turn a real assertion into a "not found" panic.
fn build(
    book: L2Book,
    source: &'static str,
    absence: Option<BookAbsence<'static>>,
    last: Option<f64>,
    stale: bool,
) -> Harness<'static, Fixture> {
    let fixture = Fixture { dom: DomState::default(), emitted: Vec::new() };
    Harness::builder().with_size(egui::vec2(900.0, 700.0)).build_ui_state(
        move |ui, f: &mut Fixture| {
            let inputs = DomInputs {
                book: &book,
                last,
                orders: &[],
                position: None,
                stale,
                paper: true,
                caps: VenueCaps::UNSUPPORTED,
                source,
                absence,
            };
            let actions = vike_panels::dom::draw(ui, &mut f.dom, &inputs);
            f.emitted.extend(actions);
        },
        fixture,
    )
}

/// Every `Role::Label` value currently on screen. `Role::Label` is the one role whose text egui
/// files under the node's VALUE rather than its label (see egui's
/// `fill_accesskit_node_from_widget_info`), which is why every query here goes through it.
fn labels(h: &Harness<'static, Fixture>) -> Vec<String> {
    h.root()
        .children_recursive()
        .filter_map(|n| {
            let a = n.accesskit_node();
            if a.role() != Role::Label {
                return None;
            }
            Some(a.value().as_deref().unwrap_or("").to_string())
        })
        .collect()
}

/// A primary click at an ARBITRARY point, which `kittest::Node::click` cannot do — it always aims
/// at a node's centre, and the whole point here is to click where a LADDER ROW would be rather than
/// at a widget that exists.
fn click_at(h: &Harness<'static, Fixture>, pos: egui::Pos2) {
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

/// Mid-ladder, in the BID third. With the harness at 900×700 and kittest's 8 px outer margin the
/// ladder region spans roughly x∈[8, 892], y∈[78, 664], and `col_at` puts the left 34 % on the bid
/// side — so this lands on a real, clickable row when a book is present. The control in
/// [`a_bookless_dom_offers_no_price_to_click`] is what keeps that claim honest.
const LADDER_POINT: egui::Pos2 = egui::Pos2 { x: 300.0, y: 350.0 };

/// A DOM with no book must SAY it has no book, and must say WHY — the cause and the next step the
/// caller supplied, both on screen verbatim, plus the line stating outright that nothing is drawn.
///
/// Reddens on removing `draw`'s empty-book branch (the whole empty state goes with it) and on
/// dropping any line out of `no_book`.
#[test]
fn a_bookless_dom_states_the_absence_its_cause_and_the_next_step() {
    let absence = BookAbsence {
        headline: "NO ORDER BOOK — MARKET-DATA LINK DOWN",
        cause: "datahub 127.0.0.1:7878 — no streams wanted on this venue",
        next_step: "Check the DATAHUB address in Connections.",
    };
    let mut h = bookless("idle", Some(absence), None);
    h.run();
    let ls = labels(&h);
    for want in [absence.headline, NO_BOOK_SUBLINE, absence.cause, absence.next_step] {
        assert!(ls.iter().any(|l| l == want), "the empty state must carry {want:?}; saw {ls:?}");
    }
}

/// With no [`BookAbsence`] at all the widget still refuses to draw a ladder: it falls back to its
/// own headline rather than to anything book-shaped.
///
/// This is the belt for a caller that forgets the field — which is the exact shape of the original
/// defect, where the app had no answer and invented one.
#[test]
fn a_bookless_dom_with_no_explanation_still_draws_no_ladder() {
    let mut h = bookless("", None, None);
    h.run();
    let ls = labels(&h);
    assert!(
        ls.iter().any(|l| l == NO_BOOK_HEADLINE),
        "a bookless DOM with no supplied words must still announce the absence; saw {ls:?}"
    );
    assert!(
        ls.iter().any(|l| l == NO_BOOK_SUBLINE),
        "and must still state that nothing is drawn; saw {ls:?}"
    );
}

/// ⚠ **THE ANIMATION PROOF** — the assertion closest to the reported defect.
///
/// The old fallback re-rolled all 80 levels every frame off `DomState::tick`. If anything in the
/// bookless path were frame-seeded, two batches of frames over the same (absent) book would render
/// differently. The counter really does advance between the batches, and that is asserted FIRST:
/// without it this test could pass by the counter having stood still, which would make it green for
/// a reason that has nothing to do with the property it claims.
#[test]
fn a_bookless_dom_does_not_animate() {
    let mut h = bookless(
        "datahub 127.0.0.1:7878 — connecting, 0/1 stream(s)",
        Some(BookAbsence {
            headline: "NO ORDER BOOK YET — CONNECTING",
            cause: "datahub 127.0.0.1:7878 — connecting, 0/1 stream(s)",
            next_step: "Nothing to do.",
        }),
        None,
    );
    h.run();
    let first = labels(&h);
    let tick_after_first = h.state().dom.tick;
    h.run();
    let second = labels(&h);
    assert!(
        h.state().dom.tick > tick_after_first,
        "the frame counter must actually advance between the two batches, or this test proves \
         nothing about frame-seeded content (was {tick_after_first}, now {})",
        h.state().dom.tick
    );
    assert!(!first.is_empty(), "the empty state must render something");
    assert_eq!(
        first, second,
        "a DOM with no book must render the SAME thing on every frame — the old synthetic ladder \
         re-rolled all 80 levels from this very counter and animated a market that was not there"
    );
}

/// A bookless DOM offers no price to click: there is no ladder to hit-test, so a click in the
/// middle of the region cannot place an order at a price the widget invented.
///
/// ⚠ The POSITIVE CONTROL is half this test. The same coordinates over the same harness geometry
/// with a REAL book must emit a `Place` — otherwise "no action was emitted" would be equally true of
/// a test whose input never reached the widget at all, and would gate nothing.
#[test]
fn a_bookless_dom_offers_no_price_to_click() {
    // Control: with a book, this point IS a placeable row.
    let mut control = with_book("datahub 127.0.0.1:7878 — 1/1 stream(s) live");
    control.run();
    control.state_mut().emitted.clear();
    click_at(&control, LADDER_POINT);
    control.run();
    assert!(
        control.state().emitted.iter().any(|a| matches!(a, DomAction::Place { .. })),
        "CONTROL FAILED: the click never reached a ladder row, so the assertion below would be \
         vacuous. Emitted: {:?}",
        control.state().emitted
    );

    // The property: the same click over an EMPTY book emits nothing at all.
    let mut h = bookless("idle", None, None);
    h.run();
    h.state_mut().emitted.clear();
    click_at(&h, LADDER_POINT);
    h.run();
    assert!(
        h.state().emitted.is_empty(),
        "no book ⇒ no placeable price, but got {:?}",
        h.state().emitted
    );
}

/// The header must not print a fabricated price: with no book and no mark it shows a dash.
///
/// Reddens on restoring the `default_price` fallback, which answered `62 800.00` for BTCUSDT and
/// rendered in the same amber and the same slot as a venue's real last. The second half is the
/// control — a real mark is still printed, so the dash is not a blanket blank.
#[test]
fn no_price_renders_a_dash_and_a_real_mark_still_renders() {
    let mut h = bookless("idle", None, None);
    h.run();
    let ls = labels(&h);
    assert!(ls.iter().any(|l| l == "—"), "a priceless DOM must show a dash; saw {ls:?}");
    // A rendered price is the only label that both STARTS with a digit and carries a decimal point
    // (`fmt_px`'s whole output). The toolbar's `qty 0.0100` starts with a letter and the grouping
    // readout is a bare integer, so neither is caught — which is what makes this a real check rather
    // than one tuned to pass.
    assert!(
        !ls.iter().any(|l| { l.starts_with(|c: char| c.is_ascii_digit()) && l.contains('.') }),
        "no formatted price may appear when there is none; saw {ls:?}"
    );
    assert!(
        !ls.iter().any(|l| l == "● STALE"),
        "a STALE badge claims the depth in front of you is old; with no depth it decorates an \
         absence, and it was half of what disclosed the synthetic ladder. Saw {ls:?}"
    );

    let mut priced = bookless("idle", None, Some(62_800.0));
    priced.run();
    let pl = labels(&priced);
    assert!(
        pl.iter().any(|l| l.replace([',', ' '], "").contains("62800")),
        "a real mark must still be shown; saw {pl:?}"
    );

    // ...and the badge control: a REAL book flagged stale must still say so. Without this, the
    // suppression above would be indistinguishable from having deleted the badge outright.
    let mut stale = with_stale_book();
    stale.run();
    let sl = labels(&stale);
    assert!(
        sl.iter().any(|l| l == "● STALE"),
        "an actual stale ladder must keep its badge — the rows ARE old and a trader must know; \
         saw {sl:?}"
    );
}

/// ⚠ **THE TWO-SOCKET GATE.** The DOM must state its OWN link's state, because the shell's status
/// bar describes a DIFFERENT connection that carries no book (see this file's module doc). The
/// strip is asserted present in EVERY state, including over a populated ladder — a source line that
/// appeared only when the book was missing could not tell a trader that the rows in front of them
/// are the last ones that arrived before the link died.
#[test]
fn the_dom_always_states_its_own_market_data_link() {
    // over a POPULATED ladder
    let mut live = with_book("datahub 127.0.0.1:7878 — 1/1 stream(s) live");
    live.run();
    let ll = labels(&live);
    assert!(
        ll.iter().any(|l| l == "datahub 127.0.0.1:7878 — 1/1 stream(s) live"),
        "the source line must be on screen even when the ladder is full; saw {ll:?}"
    );
    assert!(ll.iter().any(|l| l == "FEED UP"), "and must classify it; saw {ll:?}");

    // ...and over an EMPTY one, in each state `MdSession::refresh_statuses` can report
    for (source, word) in [
        ("datahub 127.0.0.1:7878 — 1/1 stream(s) live", "FEED UP"),
        ("datahub 127.0.0.1:7878 — connecting, 0/1 stream(s)", "FEED DIALLING"),
        ("idle", "FEED DOWN"),
        ("datahub error: connection refused", "FEED FAULT"),
        ("datahub 127.0.0.1:7878 — no streams wanted on this venue", "FEED ?"),
    ] {
        let mut h = bookless(source, None, None);
        h.run();
        let ls = labels(&h);
        assert!(
            ls.iter().any(|l| l == word),
            "source {source:?} must classify as {word:?}; saw {ls:?}"
        );
        assert!(
            ls.iter().any(|l| l == source),
            "and must show the link's own words verbatim; saw {ls:?}"
        );
    }
}

/// An empty source string is an absence of information, not a state. It must read `source unknown`
/// rather than borrow the look of any of the five real ones.
#[test]
fn an_unknown_source_says_so_rather_than_guessing() {
    let mut h = bookless("", None, None);
    h.run();
    let ls = labels(&h);
    assert!(
        ls.iter().any(|l| l == "source unknown"),
        "an empty status must read as unknown; saw {ls:?}"
    );
}

/// The data-plane vocabulary must not collide with the trading-mode badge's. `FEED UP` and
/// `● LIVE` / `PAPER` answer different questions about different sockets — where DEPTH comes from,
/// and where an ORDER goes — and a shared word is how one gets read as the other.
///
/// Asserts exactly ONE trading-mode badge is on screen in every source state, so a strip that
/// started saying `LIVE` would be caught as a second badge rather than passing on a substring.
#[test]
fn the_source_strip_never_reuses_the_trading_mode_words() {
    for source in [
        "datahub 127.0.0.1:7878 — 1/1 stream(s) live",
        "datahub 127.0.0.1:7878 — connecting, 0/1 stream(s)",
        "idle",
        "datahub error: connection refused",
        "datahub 127.0.0.1:7878 — no streams wanted on this venue",
    ] {
        let mut h = bookless(source, None, None);
        h.run();
        let found: Vec<String> =
            labels(&h).into_iter().filter(|l| l == "PAPER" || l == "● LIVE").collect();
        assert_eq!(
            found,
            vec!["PAPER".to_string()],
            "source {source:?} must leave exactly one trading-mode badge on screen, the header's"
        );
    }
}

/// The footer stays live over an empty book, and that is deliberate rather than an oversight: Close,
/// Reverse and the three cancels act on a POSITION and on RESTING ORDERS, both of which exist
/// independently of whether this client currently has depth. A trader whose link just dropped with
/// a position open must still be able to flatten it.
///
/// It is also the input-plumbing control for the whole file — if this failed, every "emitted nothing"
/// assertion above would be true for the wrong reason.
#[test]
fn a_bookless_dom_can_still_flatten_and_cancel() {
    let mut h = bookless("idle", None, None);
    h.run();
    h.state_mut().emitted.clear();
    h.get_by_label("✕ All").click();
    h.run();
    assert_eq!(
        h.state().emitted,
        vec![DomAction::CancelAll],
        "a position and resting orders outlive the book feed; the footer must keep working"
    );

    h.state_mut().emitted.clear();
    h.get_by_label("Close").click();
    h.run();
    assert_eq!(h.state().emitted, vec![DomAction::ClosePosition]);
}
