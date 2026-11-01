//! **How often an operator's READ may reach a venue** (`TradeUp::VenueRead`).
//!
//! A read is asked by a person or a script, not by the engine, so nothing in the engine paces it: a loop
//! of them would spend a venue's request budget on a wallet's balance. The
//! daemon bounds it before it reaches the lane, and a refused request is answered in words and never
//! touches the venue:
//!
//! * **one in flight per venue**: a second read of the same venue, whichever account or kind, is
//!   refused while the first runs (the slot is held until the lane's call returns, which the bridge's own
//!   HTTP timeouts bound);
//! * **a minimum gap per account and kind**: the same read of the same account is refused for
//!   [`MIN_GAP`] after it was last admitted (a refusal does not move the mark, a failing read does).
//!
//! The gate keeps only reads admitted inside the last [`MIN_GAP`], so it never grows with the number of
//! accounts a daemon has seen.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_model::VenueReadKind;

/// The shortest time between two admitted reads of one account's one kind.
pub(crate) const MIN_GAP: Duration = Duration::from_secs(2);

type Mark = (String, Option<String>, VenueReadKind);

#[derive(Default)]
struct State {
    /// Venues with a read under way.
    in_flight: HashSet<String>,
    /// When each `(venue, account, kind)` was last admitted, for the reads inside [`MIN_GAP`].
    last: HashMap<Mark, Instant>,
}

/// The gate. One per daemon, shared by every session.
#[derive(Default)]
pub(crate) struct ReadGate {
    state: Mutex<State>,
}

/// A venue's one slot, given back when it is dropped (a panic included).
pub(crate) struct ReadSlot {
    gate: Arc<ReadGate>,
    venue: String,
}

impl Drop for ReadSlot {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.in_flight.remove(&self.venue);
    }
}

impl ReadGate {
    /// Admit a read of `kind` for `venue`'s `account` at `now`, or say why not.
    pub(crate) fn admit(
        self: &Arc<Self>,
        venue: &str,
        account: Option<&str>,
        kind: VenueReadKind,
        now: Instant,
    ) -> Result<ReadSlot, String> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.last.retain(|_, at| now.saturating_duration_since(*at) < MIN_GAP);
        if state.in_flight.contains(venue) {
            return Err(format!(
                "{venue}: another read of this venue is under way (one at a time per venue); retry in a moment"
            ));
        }
        let mark = (venue.to_string(), account.map(str::to_string), kind);
        if let Some(at) = state.last.get(&mark) {
            let wait = MIN_GAP.saturating_sub(now.saturating_duration_since(*at));
            return Err(format!(
                "{venue}: this account's {} was read a moment ago; reads of one account are held {} s apart \
                 to spare the venue's API, retry in {} s",
                kind.word(),
                MIN_GAP.as_secs(),
                wait.as_secs() + 1
            ));
        }
        state.in_flight.insert(venue.to_string());
        state.last.insert(mark, now);
        Ok(ReadSlot { gate: Arc::clone(self), venue: venue.to_string() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate() -> Arc<ReadGate> {
        Arc::new(ReadGate::default())
    }

    /// One read per venue at a time: a second read of the same venue is refused whatever its account or
    /// kind, another venue is not held up, and the slot comes back the moment the first read ends.
    #[test]
    fn one_read_at_a_time_per_venue() {
        let (g, t0) = (gate(), Instant::now());
        let first = g.admit("polymarket", None, VenueReadKind::Balance, t0).expect("admitted");
        let busy = g
            .admit("polymarket", Some("alt"), VenueReadKind::OpenOrders, t0)
            .err()
            .expect("the venue's slot is taken");
        assert!(busy.contains("another read of this venue is under way"), "{busy}");
        assert!(g.admit("hyperliquid", None, VenueReadKind::Balance, t0).is_ok());
        drop(first);
        assert!(g.admit("polymarket", Some("alt"), VenueReadKind::OpenOrders, t0).is_ok());
    }

    /// The same read of the same account waits out the gap; another kind or account does not, and the
    /// refusal says how long.
    #[test]
    fn the_same_read_of_one_account_waits_out_the_gap() {
        let (g, t0) = (gate(), Instant::now());
        drop(g.admit("polymarket", None, VenueReadKind::Balance, t0).unwrap());
        let soon = t0 + Duration::from_millis(500);
        let held = g.admit("polymarket", None, VenueReadKind::Balance, soon).err().expect("held");
        assert!(held.contains("held 2 s apart") && held.contains("retry in 2 s"), "{held}");
        assert!(g.admit("polymarket", None, VenueReadKind::OpenOrders, soon).is_ok());
        assert!(g.admit("polymarket", Some("alt"), VenueReadKind::Balance, soon).is_ok());
        let later = t0 + MIN_GAP;
        assert!(
            g.admit("polymarket", None, VenueReadKind::Balance, later).is_ok(),
            "the gap is over"
        );
    }

    /// A refused request moves no mark, and the table holds only reads inside the gap.
    #[test]
    fn a_refusal_moves_no_mark_and_old_marks_are_forgotten() {
        let (g, t0) = (gate(), Instant::now());
        drop(g.admit("polymarket", None, VenueReadKind::Balance, t0).unwrap());
        assert!(
            g.admit("polymarket", None, VenueReadKind::Balance, t0 + Duration::from_millis(1900))
                .is_err()
        );
        // Had that refusal moved the mark to 1.9 s, the gap would still be running at 2 s.
        let t2 = t0 + MIN_GAP;
        drop(g.admit("polymarket", None, VenueReadKind::Balance, t2).expect("the gap ran from t0"));
        // Everything admitted long before `now` is dropped from the table on the next admit.
        let far = t2 + Duration::from_secs(60);
        drop(g.admit("hyperliquid", None, VenueReadKind::OpenOrders, far).unwrap());
        let held = g.state.lock().unwrap().last.len();
        assert_eq!(held, 1, "only the read inside the gap is kept");
    }
}
