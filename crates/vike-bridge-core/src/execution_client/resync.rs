//! **The history replay's walk over an account's symbols** (the A3 replay of `crate::exec_actor::run_loop`'s
//! gap-sentinel and of `crate::websocket_session::user::run_user_resync`), for a venue whose history reads name ONE symbol.
//!
//! The mounted symbol is read first, with the requests the venue has always made for it; then each foreign symbol
//! (the engine's admitted ones for the reconnect replay, the ones a command touched for the gap-sentinel; decision
//! 0174's reopener) that the venue `admit`s. A failed read is an empty array, so it costs its own rows only. A read
//! the venue answered "slow down" (`slow_down`, the venue's own reading of its errors) ENDS the walk once that
//! symbol's reads are made: the reconnect of fifty accounts at once must not turn into an IP ban, and whatever is
//! left unread is reconcile's, as it was before the walk existed. A slow-down on the MOUNTED read ends it before the
//! first foreign symbol. Each foreign symbol is charged to the venue's read budget before its reads
//! (`crate::read_budget`; the mounted one is the replay's own charge, its caller's), and a refusal ends the walk the
//! same way: the rest is reconcile's.
//!
//! The venue's facts stay its own, as parameters: which symbols it reads (`admit`: binance's lane), what says
//! "slow down" (binance: `ErrorKind::RateLimited`), and the reads and mapping themselves.

use serde_json::Value;
use vike_model::events::Event;

use crate::transport::VenueApiError;

/// One read's answer as the rows a replay maps: the rows, or an empty array for a failed read.
pub type ReplayRows<'a> = &'a dyn Fn(Result<Value, VenueApiError>) -> Value;

/// The replay of `mounted` and then of each of `foreign` (see the module doc). `read_mounted(rows)` is the venue's
/// reads of the mounted symbol; `read(label, rows)` its reads of one foreign label, each answering its events and
/// passing every read's answer through `rows`. A repeat (any case), an empty label, the mounted symbol and a label the
/// venue does not `admit` are skipped before any request; an admitted one is charged to `venue`'s read budget first.
pub fn replay_foreign(
    venue: &str,
    mounted: &str,
    foreign: &[String],
    admit: impl Fn(&str) -> bool,
    slow_down: impl Fn(&VenueApiError) -> bool,
    read_mounted: impl FnOnce(ReplayRows<'_>) -> Vec<Event>,
    mut read: impl FnMut(&str, ReplayRows<'_>) -> Vec<Event>,
) -> Vec<Event> {
    let limited = std::cell::Cell::new(false);
    let rows = |answer: Result<Value, VenueApiError>| {
        answer.unwrap_or_else(|e| {
            limited.set(limited.get() || slow_down(&e));
            serde_json::json!([])
        })
    };
    let mut events = read_mounted(&rows);
    let mut seen: Vec<&str> = vec![mounted];
    let mut last = mounted;
    for label in foreign {
        if limited.get() {
            break;
        }
        if label.is_empty() || seen.iter().any(|s| s.eq_ignore_ascii_case(label)) {
            continue;
        }
        seen.push(label);
        if !admit(label) {
            continue;
        }
        if !crate::read_budget::charge(venue, label, 1) {
            break; // refused by the request budget: the rest is reconcile's (the module doc)
        }
        events.extend(read(label, &rows));
        last = label;
    }
    if limited.get() {
        tracing::warn!(
            symbol = %last,
            "history replay: the venue said slow down; the admitted symbols after this one are left to the reconcile pass"
        );
    }
    events
}

#[cfg(test)]
#[path = "resync_tests.rs"]
mod resync_tests;
