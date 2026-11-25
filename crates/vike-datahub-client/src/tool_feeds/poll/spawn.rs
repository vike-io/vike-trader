//! The poll's background threads: the News and Calendar half and the Options half of the ONE
//! datahub poll ([`super::DatahubPoller`]), each on its own thread and its own cadence. Both ask
//! the backend's DATAHUB and nothing else; neither opens a socket to a provider or a venue.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::options::{OPTIONS_POLL_MS, OptionsLink, OptionsPollState, wait_for_next_poll};
use super::{DatahubPollClock, DatahubPollLink, DatahubPollSnapshot, DatahubPollState};
use super::{DatahubPoller, FEEDS_TICK};
use crate::dial::DatahubDial;

/// Spawn the two poll threads; each asks, updates the shared state and wakes the UI.
///
/// `dial` answers where the backend's datahub is and what to sign with, AT THE MOMENT a poll asks:
/// the composition root hands the one dial the Data Manager's catalog route already keeps current
/// (it follows the active backend record, which a switch replaces), so the datahub clients can
/// never sign with different keys. `None` is "no datahub", and each tool then says so rather than
/// asking anybody else.
///
/// The wake channels are installed in `state` as [`DatahubPollSnapshot::options_link`] and
/// [`DatahubPollSnapshot::feeds_link`]: the Refresh pills and the windows' recorded interest reach
/// the threads through them. Neither thread dials anything while no window of its tool is open.
pub fn spawn_tool_fetchers(
    state: Arc<Mutex<DatahubPollSnapshot>>,
    wake: impl Fn() + Send + Clone + 'static,
    dial: impl Fn() -> Option<DatahubDial> + Send + Clone + 'static,
) {
    // The News and Calendar tools' poll thread: the datahub's `NewsFeed`, `EconomicCalendar` and
    // `EquityCalendar` verbs (`super`'s module doc carries what it asks, how often and why there is
    // no fallback).
    let (feeds_wake_tx, feeds_wake_rx) = std::sync::mpsc::channel::<()>();
    state.lock().unwrap().feeds_link = DatahubPollLink::new(feeds_wake_tx);
    let (feeds_state, feeds_wake, feeds_dial) = (state.clone(), wake.clone(), dial.clone());
    std::thread::Builder::new()
        .name("ui-tool-feeds".into())
        .spawn(move || {
            let mut poller = DatahubPoller::<DatahubPollState>::default();
            loop {
                let dial_now = feeds_dial();
                if poller.run_once(&feeds_state, dial_now.as_ref(), &DatahubPollClock::system()) {
                    feeds_wake();
                }
                wait_for_next_poll(&feeds_wake_rx, FEEDS_TICK);
            }
        })
        .expect("spawn feeds thread");

    // The Options tool's poll thread: it asks the backend's DATAHUB, never Deribit
    // (`super::options`' module doc carries what it asks, how often and why there is no fallback).
    let (opt_wake_tx, opt_wake_rx) = std::sync::mpsc::channel::<()>();
    state.lock().unwrap().options_link = OptionsLink::new(opt_wake_tx);
    std::thread::Builder::new()
        .name("ui-tool-options".into())
        .spawn(move || {
            let mut poller = DatahubPoller::<OptionsPollState>::default();
            let cadence = Duration::from_millis(OPTIONS_POLL_MS);
            loop {
                let dial_now = dial();
                if poller.run_once(&state, dial_now.as_ref(), std::time::Instant::now()) {
                    wake();
                }
                wait_for_next_poll(&opt_wake_rx, cadence);
            }
        })
        .expect("spawn options thread");
}
