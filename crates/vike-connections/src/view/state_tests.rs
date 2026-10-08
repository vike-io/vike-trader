//! Unit tests of `EditState`'s form lifecycle across a venue switch.

use super::*;

/// ⚠ Switching VENUE closes an open form, exactly as switching ACCOUNT does — the buffers
/// were typed against the venue selected when the form opened, and `account_fields` composes
/// them against whatever venue is selected at Save time.
#[test]
fn switching_venue_closes_an_open_form() {
    let mut state = EditState { venue: "binance".to_string(), ..EditState::default() };
    state.open("binance", "LIVE");
    assert!(state.target.is_some());

    state.select_venue("binance");
    assert!(state.target.is_some(), "re-selecting the same venue is a no-op");

    state.select_venue("bybit");
    assert!(state.target.is_none(), "the form must close on a venue switch");
    assert!(state.buffers.is_empty());
}

/// `EditState::open` sizes its buffer vec to `edit_fields`'s length — 4 empty buffers for
/// dukascopy/DEMO (two accounts' login pairs; the two `_SERVER` fields left with decision 0095's
/// Task 7). Every buffer starts empty (rule: an existing secret's plaintext is never read back into
/// the UI). ⚠ This is not a restatement of the field-list test: `render_edit_form` INDEXES
/// `buffers` by field position and `expect`s the entry, so a buffer vec that lagged the table
/// would panic the panel rather than render a short form.
#[test]
fn edit_state_open_sizes_buffers_for_dukascopy_demo() {
    let mut state = EditState::default();
    state.open("dukascopy", "DEMO");
    assert_eq!(state.buffers.len(), 4);
    assert!(state.buffers.iter().all(String::is_empty));
    assert_eq!(state.target, Some(("dukascopy".to_string(), "DEMO".to_string())));
}
