//! Accessibility-tree tests for the one-click ticket ([`vike_cockpit::ticket`]).
//!
//! The ticket's four unit tests all cover [`vike_cockpit::payout_preview`], the pure arithmetic.
//! Nothing rendered a frame, so the property the widget actually EXISTS for — "disarmed, a buy
//! button does not send" — was asserted nowhere. That guard is one `&& state.armed` per side in
//! `draw`; deleting it is a two-token edit no pure test can see, and its blast radius is a market
//! order for the current stake on a mis-click.
//!
//! ⚠ Read the disarmed case precisely: the button is still present, still labelled the same and
//! still ENABLED in the accessibility tree — `draw` gates the ACTION, not the widget (the arm
//! state is encoded in the fill colour, see the module doc). So the two harnesses below are
//! structurally identical and differ only in what a click produces, which is exactly why this
//! needs a driven frame rather than a tree snapshot.
//!
//! ⚠ `Harness::run` steps until the app stops requesting repaints, so it may run SEVERAL frames
//! per call and only the first sees the click. [`Fixture::emitted`] therefore ACCUMULATES and each
//! test drains it explicitly before interacting — an `=` assignment there would let a trailing
//! no-input frame overwrite the action under test and pass every assertion vacuously.

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use vike_cockpit::{TicketAction, TicketInputs, TicketState};

/// Up at 60¢ / Down at 40¢ — the prices land on the button faces, so the queries below name the
/// label a trader actually reads.
const INPUTS: TicketInputs = TicketInputs {
    up_price: Some(0.60),
    dn_price: Some(0.40),
    up_win_payout: None,
    dn_win_payout: None,
};

/// The button face `buy_button` renders for `INPUTS.up_price` (label, two spaces, `{:.2}`).
const BUY_UP: &str = "BUY UP  0.60";
/// The same for the Down side.
const BUY_DOWN: &str = "BUY DOWN  0.40";

/// Caller-owned state plus every action drawn since the last drain — the app's side of the
/// data-in / actions-out seam, which is what these tests assert over.
struct Fixture {
    ticket: TicketState,
    emitted: Vec<TicketAction>,
}

fn harness(armed: bool) -> Harness<'static, Fixture> {
    let fixture =
        Fixture { ticket: TicketState { armed, ..TicketState::default() }, emitted: Vec::new() };
    Harness::builder().with_size(egui::vec2(400.0, 300.0)).build_ui_state(
        |ui, f: &mut Fixture| {
            let actions = vike_cockpit::draw_ticket(ui, &mut f.ticket, &INPUTS);
            f.emitted.extend(actions);
        },
        fixture,
    )
}

/// Settle the opening frames, then throw away anything they produced so an assertion below can
/// only be about the click it just made.
fn settle(h: &mut Harness<'static, Fixture>) {
    h.run();
    h.state_mut().emitted.clear();
}

/// THE safety property: while the ticket is disarmed, clicking a buy button sends NOTHING.
///
/// Reddens on deleting either `&& state.armed` in `draw`'s buy row — the exact edit that would
/// turn a mis-click on a disarmed ticket into a live market order.
#[test]
fn a_disarmed_ticket_emits_no_order_when_a_buy_button_is_clicked() {
    let mut h = harness(false);
    settle(&mut h);

    h.get_by_label(BUY_UP).click();
    h.run();
    assert!(
        h.state().emitted.is_empty(),
        "disarmed BUY UP must send nothing, got {:?}",
        h.state().emitted
    );

    h.state_mut().emitted.clear();
    h.get_by_label(BUY_DOWN).click();
    h.run();
    assert!(
        h.state().emitted.is_empty(),
        "disarmed BUY DOWN must send nothing, got {:?}",
        h.state().emitted
    );
}

/// The other half of the same guard — the widget is not merely inert. An armed ticket routes each
/// button to ITS OWN side, so a swapped pair (the up button buying the down outcome) reddens too.
#[test]
fn an_armed_ticket_routes_each_buy_button_to_its_own_side() {
    let mut h = harness(true);
    settle(&mut h);

    h.get_by_label(BUY_UP).click();
    h.run();
    assert_eq!(h.state().emitted, vec![TicketAction::BuyUp]);

    h.state_mut().emitted.clear();
    h.get_by_label(BUY_DOWN).click();
    h.run();
    assert_eq!(h.state().emitted, vec![TicketAction::BuyDown]);
}

/// The arm toggle is the only control that moves the ticket between the two states above, so it is
/// worth proving it both EMITS and FLIPS: one click arms the ticket and relabels the control
/// (`ARM` → `● ARMED`), which is the trader's confirmation that the buy buttons are now live.
#[test]
fn the_arm_toggle_flips_the_state_and_relabels_itself() {
    let mut h = harness(false);
    settle(&mut h);
    assert!(h.query_by_label("● ARMED").is_none(), "a fresh ticket must not read as armed");

    h.get_by_label("ARM").click();
    h.run();

    assert_eq!(h.state().emitted, vec![TicketAction::ToggleArm]);
    assert!(h.state().ticket.armed, "the toggle must flip the caller's state, not just report it");
    h.get_by_label("● ARMED"); // panics unless the control now reads as armed
}
