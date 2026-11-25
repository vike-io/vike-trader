//! The fill a mapper's event list carries, read one way by the fee suites.

use vike_model::events::{Event, FillEvent};

/// The one bare `Event::Fill` among `events`; a list without one is a failed test.
pub fn bare_fill(events: &[Event]) -> FillEvent {
    events
        .iter()
        .find_map(|e| match e {
            Event::Fill(f) => Some(f.clone()),
            _ => None,
        })
        .expect("a bare Event::Fill")
}
