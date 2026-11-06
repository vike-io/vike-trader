//! Unit tests of `EditState`'s form lifecycle across a venue switch.

use super::*;
use vike_model::AccountTier;

const LIVE: FormPlace = FormPlace::Tier(AccountTier::Live);

/// ⚠ Switching VENUE closes an open form, exactly as switching ACCOUNT does — the buffers
/// were typed against the venue selected when the form opened.
#[test]
fn switching_venue_closes_an_open_form() {
    let mut state = EditState { venue: "binance".to_string(), ..EditState::default() };
    state.open("binance", LIVE, Some(7));
    assert!(state.target.is_some());

    state.select_venue("binance");
    assert!(state.target.is_some(), "re-selecting the same venue is a no-op");

    state.select_venue("bybit");
    assert!(state.target.is_none(), "the form must close on a venue switch");
    assert!(state.buffers.is_empty());
}

/// `EditState::open` sizes its buffer vec to `form_fields`' length, and records the account row the
/// form writes. Every buffer starts empty (rule: an existing secret's plaintext is never read back
/// into the UI). ⚠ This is not a restatement of the field-list test: `render_edit_form` INDEXES
/// `buffers` by field position and `expect`s the entry, so a buffer vec that lagged the form would
/// panic the panel rather than render a short form.
#[test]
fn edit_state_open_sizes_buffers_for_the_form_it_opens() {
    let mut state = EditState::default();
    let demo = FormPlace::Tier(AccountTier::Demo);
    state.open("dukascopy", demo, Some(3));
    assert_eq!(state.buffers.len(), form_fields("dukascopy", demo).len());
    assert_eq!(state.buffers.len(), 2, "one account's login pair");
    assert!(state.buffers.iter().all(String::is_empty));
    assert_eq!(
        state.target,
        Some(FormTarget { venue: "dukascopy".to_string(), place: demo, account: Some(3) })
    );

    // The venue's application group: cTrader's client pair, written to no account.
    state.open("ctrader", FormPlace::VenueApp, None);
    assert_eq!(state.buffers.len(), 2);
    assert_eq!(state.target.as_ref().and_then(|t| t.account), None);
}

/// ⚠ The armed Remove is a TWO-CLICK with a clock: it stays armed for `RETIRE_ARM`, reports
/// how long is left (so the panel can ask for a repaint at the deadline), and disarms itself once
/// the time is up — a confirm the operator walked away from must not stay live.
#[test]
fn an_armed_remove_disarms_itself_after_its_window() {
    let mut state = EditState::default();
    assert_eq!(state.tick_retire_arm(100.0), None, "nothing armed, nothing to expire");

    state.retire_armed = Some(("SOME_FIELD".to_string(), 100.0));
    let left = state.tick_retire_arm(101.0).expect("still armed one second in");
    assert!((left - (RETIRE_ARM.as_secs_f64() - 1.0)).abs() < 1e-9, "{left}");
    assert!(state.retire_armed.is_some());

    assert_eq!(
        state.tick_retire_arm(100.0 + RETIRE_ARM.as_secs_f64()),
        None,
        "the deadline itself expires it"
    );
    assert!(state.retire_armed.is_none(), "…and the arm is gone, not merely unreported");
}

/// Closing the form (a switch of account or venue does) drops an armed Remove and the list of
/// fields removed in it: a confirm armed against one selection must never fire against another.
#[test]
fn closing_the_form_disarms_a_remove_and_forgets_the_removed_fields() {
    let mut state = EditState { venue: "binance".to_string(), ..EditState::default() };
    state.open("binance", LIVE, Some(7));
    state.retire_armed = Some(("SOME_FIELD".to_string(), 1.0));
    state.retired.push("SOME_FIELD".to_string());

    state.select_venue("bybit");
    assert!(state.retire_armed.is_none());
    assert!(state.retired.is_empty());

    // …and opening a fresh form starts clean too.
    state.retire_armed = Some(("SOME_FIELD".to_string(), 1.0));
    state.retired.push("SOME_FIELD".to_string());
    state.open("bybit", LIVE, Some(8));
    assert!(state.retire_armed.is_none() && state.retired.is_empty());
}
