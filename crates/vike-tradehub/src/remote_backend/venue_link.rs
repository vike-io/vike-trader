//! **The venue-path watch: an engine whose bridge lost its path to the venue says so** (Dukascopy's JForex sidecar,
//! which its exec client restarts by itself while the lease stays up).
//!
//! # The situation
//!
//! The lease to the gatewayhub is up through such an outage, the engine is `RUNNING`, and nothing on the observe
//! wire said otherwise: `trade doctor` read `PASS engine dukascopy RUNNING at tier demo` for the 2.5 hours of
//! 2026-10-26 in which the sidecar was dead (08:15 UTC; every order was refused by the bridge in words). The
//! gatewayhub's own lease supervision (`TradeLane::link_watch`) is IBKR's alone and ends the lease to make the
//! client's link go down, which is wrong for a bridge that heals itself; and the wire has no frame that pushes a
//! venue's state (`late_currency`'s module doc argues the same: a push needs a new frame). So the node asks, with
//! the verb it already has.
//!
//! # What it does
//!
//! 1. **Armed for the venue whose exec is an owned child process** ([`PathWatch::armed`]: `Wiring::SidecarExecRecon`,
//!    today dukascopy's alone, held equal to the gatewayhub lane's answer by `vike-gatewayhub`'s
//!    `bridge_trade_tests.rs`). Any other lease asks nothing, ever: `late_currency_wire.rs` pins that "every other
//!    venue is asked nothing, as before".
//! 2. **The ask** (`reteach.rs`'s `ask_path`, on the worker thread the lease already has): a `TradeStatus` every
//!    [`ASK_EVERY`]. The daemon's report carries `venue_link` for a lane that names a path
//!    (`TradeStatusReport::venue_link`).
//! 3. **The verdict** ([`PathWatch::hear`], pure): a report of a path that is down becomes the account's entry in
//!    `LeaseFacts::set_venue_link_down`, from which the publisher stamps every observe frame
//!    (`WireSnapshot::venue_link_down`) and the doctor's engine row degrades; a report of an up path clears it. A
//!    daemon that has no adapter for the account now says nothing (a restart is re-teaching: no verdict), and a
//!    daemon that reports no path at all (it predates the field, or the lane names none) ends the watch: the node
//!    never reads that silence as "up", it just stops asking.
//!
//! # What it never does
//!
//! It sends no order, cancels nothing, halts nothing and ends no lease; the bridge's own refusals and its own
//! restart are untouched. It is a reading. The reading is as old as the last ask (at most [`ASK_EVERY`], plus the
//! time the daemon takes to answer).

use std::time::{Duration, Instant};

use vike_gatewayhub_client::proto::TradeStatusReport;
use vike_tradehub_client::wire::WireVenueLinkDown;
use vike_venues::venue_wiring::{Wiring, wiring_for};

/// How often a lease that reports a venue path is asked for it. Short against an outage that lasts minutes, long
/// against a status request (one frame, answered from memory by the daemon).
pub const ASK_EVERY: Duration = Duration::from_secs(5);

/// Whether `venue`'s exec runs through a child process the gatewayhub's bridge owns and restarts by itself.
#[must_use]
pub fn reports_path(venue: &str) -> bool {
    matches!(wiring_for(venue), Some(Wiring::SidecarExecRecon { .. }))
}

/// What one report changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    /// No verdict: the daemon has no adapter for the account at this moment.
    Nothing,
    /// The path works (or the daemon reports none and the watch is over): the account has no outage. `was_down`
    /// = the last verdict said it was down, so the return is worth a line.
    Up { was_down: bool },
    /// The path is down. `newly` = the last verdict was not "down", so the loss is worth a line.
    Down { down: WireVenueLinkDown, newly: bool },
}

/// One lease's watch of its venue path.
#[derive(Debug)]
pub struct PathWatch {
    next: Instant,
    over: bool,
    down: bool,
}

impl PathWatch {
    /// The watch of a lease on `venue`, due at once; `None` for every venue whose exec is not an owned child.
    #[must_use]
    pub fn armed(venue: &str, now: Instant) -> Option<Self> {
        reports_path(venue).then_some(Self { next: now, over: false, down: false })
    }

    /// Whether an ask is due at `now`.
    #[must_use]
    pub fn due(&self, now: Instant) -> bool {
        now >= self.next
    }

    /// Note that an ask was made at `now`, whatever it answered.
    pub fn asked(&mut self, now: Instant) {
        self.next = now + ASK_EVERY;
    }

    /// The daemon reports no path for this venue: nothing more is asked.
    #[must_use]
    pub fn is_over(&self) -> bool {
        self.over
    }

    /// Judge one status report of the account `route_key`.
    pub fn hear(&mut self, route_key: &str, status: &TradeStatusReport) -> Reading {
        // Not an answer about the path: a daemon that restarted has no adapter until the re-teaching starts one,
        // and the report it gives then carries no `venue_link` for that reason, not because the venue has none.
        if !status.adapter_up {
            return Reading::Nothing;
        }
        let Some(link) = &status.venue_link else {
            self.over = true;
            return self.up();
        };
        if link.up {
            return self.up();
        }
        let newly = !std::mem::replace(&mut self.down, true);
        Reading::Down {
            down: WireVenueLinkDown {
                route_key: route_key.to_string(),
                why: link.why.clone(),
                failed_restarts: link.failed_restarts,
                retry_in_ms: link.retry_in_ms,
            },
            newly,
        }
    }

    fn up(&mut self) -> Reading {
        Reading::Up { was_down: std::mem::replace(&mut self.down, false) }
    }
}

#[path = "venue_link_tests.rs"]
#[cfg(test)]
mod venue_link_tests;
