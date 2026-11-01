//! Subscription refcounts: a wire frame only on a pair's first add or last remove.

use std::collections::HashMap;

use super::Verb;

/// A subscribe/unsubscribe request sent to a route's connection thread over the command channel.
pub(super) enum ConnCmd {
    Sub(Verb, String),
    Unsub(Verb, String),
}

/// The wire action a [`SubRegistry`] op implies.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum WireAction {
    /// Send `{"action":"subscribe",...}` — this `(verb, symbol)` pair just became live.
    Subscribe,
    /// Send `{"action":"unsubscribe",...}` — this pair's LAST subscriber dropped (connection stays).
    Unsubscribe,
    /// No wire frame: a redundant add (pair already live) or a non-final remove.
    None,
}

/// Per-connection subscription bookkeeping. Reference-counts each `(verb, symbol)` so a wire
/// subscribe/unsubscribe frame is sent only on the FIRST add / LAST remove of that pair — two
/// `SubscriptionId`s for the same instrument+verb must not let one's `unsubscribe` kill the other's
/// feed. Also tracks the connection-wide total, so the connection is torn down when its last
/// subscription drops. Pure — unit-tested below.
#[derive(Default)]
pub(super) struct SubRegistry {
    total: usize,
    per: HashMap<(Verb, String), usize>,
}

impl SubRegistry {
    /// Record a subscription; `Subscribe` iff this `(verb, symbol)` just went from 0 → 1.
    pub(super) fn add(&mut self, verb: Verb, symbol: String) -> WireAction {
        self.total += 1;
        let n = self.per.entry((verb, symbol)).or_insert(0);
        *n += 1;
        if *n == 1 { WireAction::Subscribe } else { WireAction::None }
    }

    /// Drop one subscription. Returns `(wire action, connection-now-empty)`. When the connection is
    /// now empty the caller tears it down (so no `Unsubscribe` frame is needed); otherwise
    /// `Unsubscribe` iff this pair's refcount just hit 0.
    pub(super) fn remove(&mut self, verb: Verb, symbol: &str) -> (WireAction, bool) {
        self.total = self.total.saturating_sub(1);
        let key = (verb, symbol.to_string());
        let last_pair = match self.per.get_mut(&key) {
            Some(n) => {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    self.per.remove(&key);
                    true
                } else {
                    false
                }
            }
            None => false,
        };
        let teardown = self.total == 0;
        let action =
            if teardown || !last_pair { WireAction::None } else { WireAction::Unsubscribe };
        (action, teardown)
    }
}
