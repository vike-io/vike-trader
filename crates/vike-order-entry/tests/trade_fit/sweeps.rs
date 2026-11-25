//! The sweeps: every view and every content at every width from 260 pt, in every look; and the
//! sweep's own reports of a window that spills and a line cut short.

use super::*;

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

/// The sweep's own positive control: a window that DOES spill is reported. A control is moved out
/// of its region by drawing the window into a content rect wider than the harness, so if the
/// checks above could never fail, this would pass with no violation at all.
#[test]
fn the_sweep_reports_a_window_that_spills() {
    let mut h = window(ThemeAppearance::default(), Content::Swap, Ticket::Resting);
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
    let mut h = window(ThemeAppearance::default(), Content::Swap, Ticket::Resting);
    h.set_size(egui::vec2(120.0, 560.0));
    h.run();
    let found = violations(&h);
    assert!(
        found.iter().any(|v| v.contains(&format!("{STATUS:?} is cut short"))),
        "a status line cut to two lines must be reported: {found:?}"
    );
}
