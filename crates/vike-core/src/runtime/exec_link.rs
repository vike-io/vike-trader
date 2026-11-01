//! **The trade link's state, as the stuck-order watchdog reads it** (`Ingest::ExecLink`).
//!
//! An account whose orders travel over a trade link to a venue gateway can have that link go down
//! while its orders are in flight. An order submitted just before the loss is not stuck: its ack
//! has nowhere to arrive, and the watchdog's ladder (confirm at `submit_ack_timeout`, a synthesized
//! `OrderRejected` after the confirm grace) would read the outage as a wedged adapter and reject an
//! order that may well be live at the venue, stranding the position it opens. So while an account's
//! link is down the ladder DEFERS for that account's orders, and when the link comes back it counts
//! afresh from the link-up instant (the values, `submit_ack_timeout` plus twice the confirm grace,
//! are unchanged): the same patience a freshly submitted order gets.
//!
//! ## What this file does and does not decide
//!
//! * [`CoreThread::note_exec_link`] files one transition per account route key (the bare venue id
//!   for a venue's default account, exactly as `GatewayEvent::route_key` spells it). The producer
//!   fires a message only on a TRANSITION; a repeat is tolerated and moves nothing.
//! * [`CoreThread::exec_link_resumes`] is the ONE question the sweeps ask: from when does an order of
//!   this account count? It answers `None` while the link is down, the later of the order's creation
//!   and the link-up instant otherwise, and the creation unchanged for an account that never lost a
//!   link (an empty map, one `is_empty` test). Both ladder rungs ask it
//!   ([`CoreThread::sweep_stuck_orders`], [`CoreThread::sweep_inflight_confirms`]), so they keep
//!   agreeing about an order's age and the hand-off between them stays exact.
//! * Nothing here cancels, halts or rejects anything (decision D6: no auto-cancel of resting orders
//!   on a trade-link loss). The link dead-man is a separate latch ([`super::link_deadman`]) fed by
//!   the producer's `StreamStatus`, not by this message.
//! * The clock is the core's own dispatch clock, the one every order's `created_ms` was stamped
//!   with; the update's `at_ts_ms` is the producer's wall clock and only reaches the operator's log.
//! * Not journaled and not in the state hash: a transport fact about this process's connection, not
//!   order state. A restart begins with every link presumed up until its client discloses otherwise.

use super::*;

/// One account's trade-link state. An entry exists only for an account that has been disclosed
/// down at least once, so a box whose links never drop carries an EMPTY map for the life of the
/// process (and one entry per account that did, bounded by the account roster).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ExecLinkGate {
    /// `true` from the `up: false` transition until the next `up: true`.
    down: bool,
    /// The dispatch-clock instant the link last came back; orders count from here. `0` before the
    /// first recovery, which no creation time precedes.
    resumed_ms: i64,
}

impl<C: ExecutionClient> CoreThread<C> {
    /// File one transition of an account's trade link.
    ///
    /// An update for a route key NO engine of this core carries is logged and ignored: filing it
    /// would grow the map from a producer's typo, and no sweep could ever read it.
    pub(crate) fn note_exec_link(&mut self, u: vike_exec::ExecLinkUpdate, now: i64) {
        let key = u.route_key.as_str();
        let carried = self.engine.route_key == key
            || self.extra_engines.iter().any(|(_, e)| e.route_key == key);
        if !carried {
            tracing::warn!(
                target: "vike_core::watchdog",
                route_key = key,
                venue = u.venue.as_str(),
                up = u.up,
                "trade-link transition for an account this core runs no engine for — ignored"
            );
            return;
        }
        if u.up {
            // A recovery of a link never filed down is a no-op: nothing to resume.
            let Some(gate) = self.exec_link.get_mut(key) else { return };
            if !gate.down {
                return;
            }
            gate.down = false;
            gate.resumed_ms = now;
            tracing::info!(
                target: "vike_core::watchdog",
                route_key = key,
                venue = u.venue.as_str(),
                at_ts_ms = u.at_ts_ms,
                "trade link up: the stuck-order watchdog counts this account's pre-ack orders afresh from now"
            );
            self.note(format!("trade link up for {key}: stuck-order watchdog resumes"));
            return;
        }
        let gate = self
            .exec_link
            .entry(key.to_string())
            .or_insert(ExecLinkGate { down: false, resumed_ms: 0 });
        if gate.down {
            return; // already down: a repeat moves nothing (the first loss stays the loss)
        }
        gate.down = true;
        // The ladder restarts when the link returns, so a confirm issued BEFORE the loss must not
        // stand in for one after it: a stale entry would let the post-recovery reject skip its
        // confirm and fire on the extended deadline alone.
        let engine = if self.engine.route_key == key {
            &self.engine
        } else {
            let idx = self.extra_engines.iter().position(|(_, e)| e.route_key == key);
            &self.extra_engines[idx.expect("`carried` found an engine above")].1
        };
        for (coid, _) in engine.registry.iter() {
            self.confirm_issued_ms.remove(coid);
        }
        tracing::warn!(
            target: "vike_core::watchdog",
            route_key = key,
            venue = u.venue.as_str(),
            at_ts_ms = u.at_ts_ms,
            "trade link DOWN: the stuck-order watchdog defers this account's pre-ack orders until it returns \
             (resting orders are left at the venue; reconcile follows recovery)"
        );
        self.note(format!("trade link down for {key}: stuck-order watchdog deferred"));
    }

    /// Whether the account `route_key`'s trade link is disclosed DOWN right now. Cold: the hot stop asks it to
    /// tell a link-dead-man window the stopped account opened from one a surviving account still owns.
    pub(crate) fn exec_link_is_down(&self, route_key: &str) -> bool {
        self.exec_link.get(route_key).is_some_and(|gate| gate.down)
    }

    /// From when does an order of the account `route_key`, created at `created_ms`, count toward the
    /// stuck-order ladder? `None` while the account's trade link is down (skip the order this
    /// sweep); otherwise the later of its creation and the link's last recovery.
    ///
    /// Allocation-free; one `is_empty` branch for every account that never lost a link.
    pub(crate) fn exec_link_resumes(&self, route_key: &str, created_ms: i64) -> Option<i64> {
        if self.exec_link.is_empty() {
            return Some(created_ms);
        }
        match self.exec_link.get(route_key) {
            None => Some(created_ms),
            Some(gate) if gate.down => None,
            Some(gate) => Some(created_ms.max(gate.resumed_ms)),
        }
    }
}
