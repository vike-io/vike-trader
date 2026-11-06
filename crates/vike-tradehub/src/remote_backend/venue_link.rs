//! **The venue-path watch: an engine whose bridge lost its path to the venue says so** (Dukascopy's JForex sidecar,
//! which its exec client restarts by itself while the lease stays up; cTrader's account session, which the venue ends
//! on a socket that stays up; OANDA's transactions stream, which its lane reopens by itself and whose loss costs no
//! order its way out, only the resting fills and venue-side cancels on the way back, in the bridge's own `effect` words).
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
//! 1. **Armed for the venues whose bridge states such a path** ([`PathWatch::armed`], [`reports_path`]: dukascopy's
//!    owned child process (`Wiring::SidecarExecRecon`), ctrader's account session and the venues in
//!    [`REPORTS_ITS_STREAM`], held equal to the gatewayhub lane's answer by `vike-gatewayhub`'s
//!    `bridge_trade_tests.rs`). Any other lease asks nothing, ever: `venue_link_wire.rs` pins that "every other venue
//!    is asked nothing, as before".
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

/// The venues whose exec stream is a path of its own beside the REST path their orders take, which their bridge reopens
/// by itself and reports. No wiring row says this: this node links no bridge, so it is a row here.
pub const REPORTS_ITS_STREAM: &[&str] = &["oanda"];

/// Whether `venue`'s bridge states a path it can lose and heal while the lease stays up: an exec that runs through a
/// child process the gatewayhub's bridge owns and restarts by itself, cTrader's account session, which the venue can
/// end on a socket that stays up (`crates/bridges/ctrader/CLAUDE.md`'s "A lapsed account session"), or a stream
/// ([`REPORTS_ITS_STREAM`]).
#[must_use]
pub fn reports_path(venue: &str) -> bool {
    venue == "ctrader"
        || matches!(wiring_for(venue), Some(Wiring::SidecarExecRecon { .. }))
        || REPORTS_ITS_STREAM.contains(&venue)
}

/// What a path's return means for `venue`, for the line that says so: only a stream's loss left the orders going out.
#[must_use]
pub fn back_words(venue: &str) -> &'static str {
    if REPORTS_ITS_STREAM.contains(&venue) {
        "the bridge reopened it and what the gap holds is read back"
    } else {
        "the bridge has it back and orders are accepted again"
    }
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
    /// The watch of a lease on `venue`, due at once; `None` for every venue that states no path ([`reports_path`]).
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
                effect: link.effect.clone(),
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
