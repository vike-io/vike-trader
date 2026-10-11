//! Accessibility-tree tests for the one-click ticket ([`vike_cockpit::ticket`]).
//!
//! The ticket's unit tests cover [`vike_cockpit::payout_preview`], the pure arithmetic. These drive
//! a real frame, because the property the widget EXISTS for — "disarmed, a buy button does not
//! send" — lives in `draw`.
//!
//! Since the design-system migration that property has TWO guards, and a test for each:
//! - a disarmed buy button is DISABLED (the kit's `ActionButton::disabled_because`), so the tree
//!   reports it disabled and no pointer input reaches it —
//!   `a_disarmed_ticket_disables_its_buy_buttons`;
//! - `draw` still pushes a buy action only `&& state.armed` — which, with the first guard, is what
//!   `a_disarmed_ticket_emits_no_order_when_a_buy_button_is_clicked` holds.
//!
//! ⚠ What the kill proofs show, stated rather than implied. Deleting `disabled_because` reddens
//! `a_disarmed_ticket_disables_its_buy_buttons` on its `is_disabled` assertion, while the second
//! guard keeps `a_disarmed_ticket_emits_no_order_when_a_buy_button_is_clicked` green. Deleting BOTH
//! reddens that test too, on "must send nothing". Deleting `&& state.armed` ALONE reddens nothing,
//! because no pointer input reaches a disabled button. That is the honest description of the second
//! guard: defence in depth.
//!
//! ⚠ `Harness::run` steps until the app stops requesting repaints, so it may run SEVERAL frames per
//! call and only the first sees the click. [`Fixture::emitted`] therefore ACCUMULATES, and each
//! test drains it before interacting. An `=` assignment there would let a trailing no-input frame
//! overwrite the action under test and pass every assertion vacuously.

use egui::accesskit::Toggled;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use vike_cockpit::{TicketAction, TicketInputs, TicketState};

/// Up at 60¢ / Down at 40¢. The prices land on the button faces, so the queries below name the
/// label a trader actually reads.
const INPUTS: TicketInputs = TicketInputs {
    up_price: Some(0.60),
    dn_price: Some(0.40),
    up_win_payout: None,
    dn_win_payout: None,
};

/// The face `buy_button` renders for `INPUTS.up_price` (the label, two spaces, `{:.2}`).
const BUY_UP: &str = "BUY UP  0.60";
/// The same for the Down side.
const BUY_DOWN: &str = "BUY DOWN  0.40";
/// The arm switch's name (`ticket.rs`'s `ARMED_LABEL`).
const ARMED: &str = "Armed";

/// Caller-owned state plus every action drawn since the last drain: the app's side of the
/// data-in / actions-out seam.
struct Fixture {
    ticket: TicketState,
    emitted: Vec<TicketAction>,
}

fn harness(armed: bool) -> Harness<'static, Fixture> {
    let fixture =
        Fixture { ticket: TicketState { armed, ..TicketState::default() }, emitted: Vec::new() };
    Harness::builder().with_size(egui::vec2(400.0, 300.0)).build_ui_state(
        |ui, f: &mut Fixture| {
            // The app's own type, so the widgets are measured as they ship.
            if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                return;
            }
            let actions = vike_cockpit::draw_ticket(ui, &mut f.ticket, &INPUTS);
            f.emitted.extend(actions);
        },
        fixture,
    )
}

/// Settle the opening frames, then throw away anything they produced, so an assertion below can
/// only be about the click it just made.
fn settle(h: &mut Harness<'static, Fixture>) {
    h.run();
    h.state_mut().emitted.clear();
}

/// THE safety property, its first guard: while the ticket is disarmed, each buy button is DISABLED
/// in the tree — a control with nothing behind it is disabled and says why (spec §4.2).
#[test]
fn a_disarmed_ticket_disables_its_buy_buttons() {
    let mut h = harness(false);
    settle(&mut h);
    for label in [BUY_UP, BUY_DOWN] {
        assert!(
            h.get_by_label(label).accesskit_node().is_disabled(),
            "disarmed, {label} is disabled"
        );
    }
}

/// THE safety property itself: while the ticket is disarmed, a click on either buy button sends
/// NOTHING. Either guard alone holds it — the disabled button, or `draw`'s `&& state.armed` — so it
/// reddens only when both are gone (the module doc's kill proofs).
#[test]
fn a_disarmed_ticket_emits_no_order_when_a_buy_button_is_clicked() {
    let mut h = harness(false);
    settle(&mut h);
    for label in [BUY_UP, BUY_DOWN] {
        h.get_by_label(label).click();
        h.run();
        assert!(
            h.state().emitted.is_empty(),
            "disarmed {label} must send nothing, got {:?}",
            h.state().emitted
        );
        h.state_mut().emitted.clear();
    }
}

/// The other half of the same guard: the widget is not merely inert. An armed ticket routes each
/// button to ITS OWN side, so a swapped pair (the up button buying the down outcome) reddens too.
#[test]
fn an_armed_ticket_routes_each_buy_button_to_its_own_side() {
    let mut h = harness(true);
    settle(&mut h);
    assert!(!h.get_by_label(BUY_UP).accesskit_node().is_disabled());

    h.get_by_label(BUY_UP).click();
    h.run();
    assert_eq!(h.state().emitted, vec![TicketAction::BuyUp]);

    h.state_mut().emitted.clear();
    h.get_by_label(BUY_DOWN).click();
    h.run();
    assert_eq!(h.state().emitted, vec![TicketAction::BuyDown]);
}

/// The switch is the only control that moves the ticket between the two states above. One click
/// EMITS, FLIPS the caller's state, and the tree reports the switch as on.
#[test]
fn the_armed_switch_flips_the_state_and_reports_it() {
    let mut h = harness(false);
    settle(&mut h);
    assert_eq!(h.get_by_label(ARMED).accesskit_node().toggled(), Some(Toggled::False));

    h.get_by_label(ARMED).click();
    h.run();

    assert_eq!(h.state().emitted, vec![TicketAction::ToggleArm]);
    assert!(h.state().ticket.armed, "the switch must flip the caller's state, not just report it");
    assert_eq!(h.get_by_label(ARMED).accesskit_node().toggled(), Some(Toggled::True));
}
