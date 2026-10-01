//! Accessibility-tree tests for the DOM order-entry ladder ([`vike_panels::dom`]).
//!
//! `dom.rs`'s seventeen unit tests are all over the pure helpers (row keys, grouping, the
//! cost-to-fill walk, `drag_to_reprice_allowed`). None rendered a frame, so two things the trader
//! bets real money on were asserted nowhere:
//!
//! 1. **Which side a cancel button cancels.** The `Bids` and `Offers` cancel buttons differ by ONE
//!    word in the source and by the SIGN of the `CancelSide` they push. Swapping them pulls the wrong half
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
//!
//! Its sibling `dom_no_book.rs` gates the OTHER thing a trader bets on and this file does not
//! reach: that a DOM with no order book says so instead of drawing one. Every harness here supplies
//! a populated book, so none of these tests can see that path.

use egui::accesskit::Role;
use egui::accesskit::Toggled;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use vike_model::{BookLevel, L2Book, VenueCaps};
use vike_panels::{DomAction, DomInputs, DomState};

/// Bids 99/98/97, asks 100/101/102 on a 1.0 tick — enough depth for the ladder to draw real rows
/// on both sides, which is what makes a side-specific cancel meaningful.
fn book() -> L2Book {
    let mut b = L2Book::new(1.0);
    b.apply_snapshot(
        1,
        &[BookLevel::new(99.0, 1.0), BookLevel::new(98.0, 2.0), BookLevel::new(97.0, 3.0)],
        &[BookLevel::new(100.0, 1.0), BookLevel::new(101.0, 2.0), BookLevel::new(102.0, 3.0)],
    );
    b
}

struct Fixture {
    dom: DomState,
    emitted: Vec<DomAction>,
}

/// Wide and tall enough that the footer's five right-aligned buttons all lay out — the DOM
/// draws its footer into an exact-size child rect, so a cramped harness would clip controls
/// out of the tree and turn a real assertion into a "not found" panic.
fn harness(paper: bool) -> Harness<'static, Fixture> {
    harness_at(egui::vec2(900.0, 700.0), paper)
}

/// `paper` picks the trading-mode chip the header renders; `size` is the window's. Everything else
/// is a fixed, realistic two-sided book.
fn harness_at(size: egui::Vec2, paper: bool) -> Harness<'static, Fixture> {
    let book = book();
    let fixture = Fixture { dom: DomState::default(), emitted: Vec::new() };
    Harness::builder().with_size(size).build_ui_state(
        move |ui, f: &mut Fixture| {
            // The DOM draws icons; their family is bound only by the app's type.
            if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                return;
            }
            let inputs = DomInputs {
                book: &book,
                last: Some(99.5),
                orders: &[],
                position: None,
                stale: false,
                paper,
                caps: VenueCaps::UNSUPPORTED,
                source: "datahub 127.0.0.1:7878 — 1/1 stream(s) live",
                absence: None,
            };
            let actions = vike_panels::dom::draw(ui, &mut f.dom, &inputs);
            f.emitted.extend(actions);
        },
        fixture,
    )
}

/// A footer cancel button's accessible name: the cancel icon, then its side's word.
fn cancel(side: &str) -> String {
    vike_ui_theme::icons::CANCEL.accessible_label(side)
}

fn settle(h: &mut Harness<'static, Fixture>) {
    h.run();
    h.state_mut().emitted.clear();
}

/// Each footer cancel button must cancel ITS OWN side, and the `All` one must cancel both.
///
/// Reddens on swapping the two `CancelSide` signs in `footer` — a one-character edit that pulls
/// the wrong half of a live quote book and that no pure test in this crate can observe.
#[test]
fn each_cancel_button_routes_to_its_own_side_of_the_book() {
    let mut h = harness(true);
    settle(&mut h);

    h.get_by_label(&cancel("Bids")).click();
    h.run();
    assert_eq!(
        h.state().emitted,
        vec![DomAction::CancelSide(1)],
        "'Bids' must cancel the BUY side (+1)"
    );

    h.state_mut().emitted.clear();
    h.get_by_label(&cancel("Offers")).click();
    h.run();
    assert_eq!(
        h.state().emitted,
        vec![DomAction::CancelSide(-1)],
        "'Offers' must cancel the SELL side (-1)"
    );

    h.state_mut().emitted.clear();
    h.get_by_label(&cancel("All")).click();
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
    assert_eq!(badge(&live), Some("LIVE"), "a live mount must say LIVE and must not read as PAPER");
}

/// The trading-mode badge's text, or `None` if neither badge is on screen. Looks for the two
/// literal spellings `header` can produce rather than scanning for a substring, so a badge that
/// renders BOTH (or a third) is a failure rather than a lucky match. The two spellings are the
/// kit's mode chip's (`vike_ui_theme::components::chip::mode`: `LIVE`, `PAPER`); the DOM's own
/// `● LIVE` word went with the design system.
///
/// ⚠ Matched on `Role::Label` specifically, NOT with `query_by_value`: egui gives a text widget a
/// child `Role::TextRun` node carrying the SAME value (that is how accesskit exposes character
/// positions), so a bare value query finds two nodes for one badge and panics as ambiguous.
fn badge(h: &Harness<'static, Fixture>) -> Option<&'static str> {
    let found: Vec<&'static str> = ["PAPER", "LIVE"]
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

/// Every `Role::Label` value on screen (egui files a Label's text under its VALUE).
fn label_values(h: &Harness<'static, Fixture>) -> Vec<String> {
    h.root()
        .children_recursive()
        .filter(|n| n.accesskit_node().role() == Role::Label)
        .filter_map(|n| n.accesskit_node().value().map(|v| v.to_string()))
        .collect()
}

/// Every `Role::Button` name on screen.
fn button_labels(h: &Harness<'static, Fixture>) -> Vec<String> {
    h.root()
        .children_recursive()
        .filter(|n| n.accesskit_node().role() == Role::Button)
        .filter_map(|n| n.accesskit_node().label().map(|v| v.to_string()))
        .collect()
}

/// Pro/Elite is the kit's segmented control: the chosen mode is a LABEL and the other a button
/// (spec §4.2 — "which mode am I in" is answered by role), and choosing switches it.
#[test]
fn the_chosen_mode_is_a_label_and_the_other_is_a_button() {
    let mut h = harness(true);
    settle(&mut h);
    assert!(label_values(&h).contains(&"Pro".to_string()), "{:?}", label_values(&h));
    assert!(button_labels(&h).contains(&"Elite".to_string()), "{:?}", button_labels(&h));
    h.get_by_label("Elite").click();
    h.run();
    assert_eq!(h.state().dom.mode, vike_panels::DomMode::Elite);
    assert!(label_values(&h).contains(&"Elite".to_string()));
    assert!(button_labels(&h).contains(&"Pro".to_string()));
}

/// The five order sizes are ONE segmented control: the chosen size is a Label, the other four
/// buttons, and choosing one switches the size the next ladder click trades (owner decision 6).
#[test]
fn the_chosen_order_size_is_a_label_and_the_others_are_buttons() {
    let mut h = harness(true);
    settle(&mut h);
    assert!(label_values(&h).contains(&"0.0100".to_string()), "{:?}", label_values(&h));
    let offered = button_labels(&h);
    for size in ["0.0010", "0.0050", "0.0500", "0.1000"] {
        assert!(offered.contains(&size.to_string()), "{size} is not offered: {offered:?}");
    }
    assert!(!offered.contains(&"0.0100".to_string()), "the chosen size is not a button");
    h.get_by_label("0.0500").click();
    h.run();
    assert_eq!(h.state().dom.qty, 0.05);
    assert!(label_values(&h).contains(&"0.0500".to_string()));
}

/// Reduce and C2F are checkboxes: the accessibility tree says whether each is on. Until this PR
/// their state was only a colour.
#[test]
fn reduce_and_cost_to_fill_are_checkboxes_that_say_whether_they_are_on() {
    let toggled = |h: &Harness<'static, Fixture>, name: &str| {
        h.root()
            .children_recursive()
            .find(|n| {
                n.accesskit_node().role() == Role::CheckBox
                    && n.accesskit_node().label().as_deref() == Some(name)
            })
            .and_then(|n| n.accesskit_node().toggled())
    };
    let mut h = harness(true);
    settle(&mut h);
    for name in ["Reduce", "C2F"] {
        assert_eq!(toggled(&h, name), Some(Toggled::False), "{name} starts off");
    }
    h.get_by_label("Reduce").click();
    h.run();
    assert!(h.state().dom.reduce_only);
    assert_eq!(toggled(&h, "Reduce"), Some(Toggled::True));
    h.get_by_label("C2F").click();
    h.run();
    assert!(h.state().dom.cost_to_fill);
    assert_eq!(toggled(&h, "C2F"), Some(Toggled::True));
}

/// ⚠ **The five order sizes are one segmented control, and it stays on ONE line.** At the launcher's
/// 320 × 560 the toolbar no longer runs past the window: it stacks onto rows, and `dom_fit.rs` gates
/// that nothing is cut off, nothing lies on anything else and the ladder keeps its own band. What is
/// gated here is the hazard that stacking must not open. The kit's segmented control WRAPS when its
/// row is too narrow, and a wrapped line of sizes would lie over the row below or over the ladder,
/// whose click-to-trade region is laid out after the toolbar and wins the click — so a click on what
/// looks like a size button would place an order at that row's price.
///
/// Every size must sit on the same line. A wrapped one sits a whole control height (20 pt or more)
/// lower, far outside the 3 pt allowed for centring.
#[test]
fn a_narrow_dom_keeps_every_order_size_on_the_toolbar_row() {
    let mut h = harness_at(egui::vec2(320.0, 560.0), true);
    settle(&mut h);
    let centre_y = |want: &str| {
        h.root()
            .children_recursive()
            .find(|n| {
                let a = n.accesskit_node();
                a.label().as_deref() == Some(want) || a.value().as_deref() == Some(want)
            })
            .unwrap_or_else(|| panic!("{want} is not in the tree"))
            .rect()
            .center()
            .y
    };
    let ys: Vec<f32> =
        ["0.0010", "0.0050", "0.0100", "0.0500", "0.1000"].into_iter().map(centre_y).collect();
    let (top, bottom) =
        ys.iter().fold((f32::MAX, f32::MIN), |(lo, hi), y| (lo.min(*y), hi.max(*y)));
    assert!(bottom - top <= 3.0, "the five sizes are not on one line: their centres are at {ys:?}");
}
