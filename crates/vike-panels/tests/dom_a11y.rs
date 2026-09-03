//! Accessibility-tree tests for the DOM order-entry ladder ([`vike_panels::dom`]).
//!
//! `dom.rs`'s seventeen unit tests are all over the pure helpers (row keys, grouping, the
//! cost-to-fill walk, `drag_to_reprice_allowed`). None rendered a frame, so two things the trader
//! bets real money on were asserted nowhere:
//!
//! 1. **Which side a cancel button cancels.** `✕ Bids` and `✕ Offers` differ by ONE character in
//!    the source and by the SIGN of the `CancelSide` they push. Swapping them pulls the wrong half
//!    of a live quote book, and every pure test stays green — `CancelSide` is built in `footer`,
//!    which has no test at all.
//! 2. **The PAPER / LIVE badge.** The header's own comment calls it "where a click's order
//!    actually goes". Inverting the flag makes a live account read as safe.
//!
//! Both are queried off the accessibility tree rather than a pixel dump because both are TEXT
//! (`Role::Button` label / `Role::Label` value), and neither needs a GPU to be true.
//!
//! ⚠ `Harness::run` steps until repaints settle, so only the first frame of a `run()` sees a
//! click. [`Fixture::emitted`] ACCUMULATES and each test drains it before interacting; assigning
//! instead would let a trailing no-input frame silently blank the action under test.

use egui::accesskit::Role;
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use vike_model::{L2Book, VenueCaps};
use vike_panels::{DomAction, DomInputs, DomState};

/// Bids 99/98/97, asks 100/101/102 on a 1.0 tick — enough depth for the ladder to draw real rows
/// on both sides, which is what makes a side-specific cancel meaningful.
fn book() -> L2Book {
    let mut b = L2Book::new(1.0);
    b.apply_snapshot(
        1,
        &[(99.0, 1.0), (98.0, 2.0), (97.0, 3.0)],
        &[(100.0, 1.0), (101.0, 2.0), (102.0, 3.0)],
    );
    b
}

struct Fixture {
    dom: DomState,
    emitted: Vec<DomAction>,
}

/// `paper` picks the trading-mode badge the header renders; everything else is a fixed, realistic
/// two-sided book.
fn harness(paper: bool) -> Harness<'static, Fixture> {
    let book = book();
    let fixture = Fixture { dom: DomState::default(), emitted: Vec::new() };
    // Wide and tall enough that the footer's five right-aligned buttons all lay out — the DOM
    // draws its footer into an exact-size child rect, so a cramped harness would clip controls
    // out of the tree and turn a real assertion into a "not found" panic.
    Harness::builder().with_size(egui::vec2(900.0, 700.0)).build_ui_state(
        move |ui, f: &mut Fixture| {
            let inputs = DomInputs {
                book: &book,
                last: Some(99.5),
                orders: &[],
                position: None,
                stale: false,
                paper,
                caps: VenueCaps::UNSUPPORTED,
            };
            let actions = vike_panels::dom::draw(ui, &mut f.dom, &inputs);
            f.emitted.extend(actions);
        },
        fixture,
    )
}

fn settle(h: &mut Harness<'static, Fixture>) {
    h.run();
    h.state_mut().emitted.clear();
}

/// Each footer cancel button must cancel ITS OWN side, and `✕ All` must cancel both.
///
/// Reddens on swapping the two `CancelSide` signs in `footer` — a one-character edit that pulls
/// the wrong half of a live quote book and that no pure test in this crate can observe.
#[test]
fn each_cancel_button_routes_to_its_own_side_of_the_book() {
    let mut h = harness(true);
    settle(&mut h);

    h.get_by_label("✕ Bids").click();
    h.run();
    assert_eq!(
        h.state().emitted,
        vec![DomAction::CancelSide(1)],
        "'✕ Bids' must cancel the BUY side (+1)"
    );

    h.state_mut().emitted.clear();
    h.get_by_label("✕ Offers").click();
    h.run();
    assert_eq!(
        h.state().emitted,
        vec![DomAction::CancelSide(-1)],
        "'✕ Offers' must cancel the SELL side (-1)"
    );

    h.state_mut().emitted.clear();
    h.get_by_label("✕ All").click();
    h.run();
    assert_eq!(h.state().emitted, vec![DomAction::CancelAll]);
}

/// The trading-mode badge tells the trader where the next click's order goes. It is a plain
/// label, so egui files the text under the accessibility node's VALUE (`Role::Label` is the one
/// role whose text is not the `label` field — see `egui`'s `fill_accesskit_node_from_widget_info`).
///
/// Reddens on inverting the `paper` branch in `header` — the edit that makes a live account read
/// as safe.
#[test]
fn the_header_badge_names_the_trading_mode_it_was_given() {
    let mut paper = harness(true);
    paper.run();
    assert_eq!(
        badge(&paper),
        Some("PAPER"),
        "a paper mount must say PAPER and must not read as LIVE"
    );

    let mut live = harness(false);
    live.run();
    assert_eq!(
        badge(&live),
        Some("● LIVE"),
        "a live mount must say LIVE and must not read as PAPER"
    );
}

/// The trading-mode badge's text, or `None` if neither badge is on screen. Looks for the two
/// literal spellings `header` can produce rather than scanning for a substring, so a badge that
/// renders BOTH (or a third) is a failure rather than a lucky match.
///
/// ⚠ Matched on `Role::Label` specifically, NOT with `query_by_value`: egui gives a text widget a
/// child `Role::TextRun` node carrying the SAME value (that is how accesskit exposes character
/// positions), so a bare value query finds two nodes for one badge and panics as ambiguous.
fn badge(h: &Harness<'static, Fixture>) -> Option<&'static str> {
    let found: Vec<&'static str> = ["PAPER", "● LIVE"]
        .into_iter()
        .filter(|text| {
            h.root().children_recursive().any(|n| {
                let a = n.accesskit_node();
                a.role() == Role::Label && a.value().as_deref() == Some(*text)
            })
        })
        .collect();
    match found.as_slice() {
        [one] => Some(one),
        _ => None,
    }
}
