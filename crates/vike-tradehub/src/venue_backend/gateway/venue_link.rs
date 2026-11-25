//! **The venue-path watch: an engine whose bridge lost its path to the venue says so** (Dukascopy's JForex sidecar,
//! which its exec client restarts by itself while the lease stays up; cTrader's account session, which the venue ends
//! on a socket that stays up; OANDA's transactions stream, which its lane reopens by itself and whose loss costs no
//! order its way out, only the resting fills and venue-side cancels on the way back, in the bridge's own `effect` words).
//!
//! # The situation
//!
//! The lease to the gatewayhub is up through such an outage, the engine is `RUNNING`, and nothing on the observe
//! wire said otherwise: `trade health` read `PASS engine dukascopy RUNNING at tier demo` for the 2.5 hours of
//! 2026-10-26 in which the sidecar was dead (08:15 UTC; every order was refused by the bridge in words). The
//! gatewayhub's own lease supervision (`TradeLane::link_watch`) is only for a bridge that declares its session cannot
//! heal itself (IBKR's gateway, FXCM's session once the SDK gave it up) and ends the lease to make the
//! client's link go down, which is wrong for a bridge that heals itself; and the wire has no frame that pushes a
//! venue's state (`late_currency`'s module doc argues the same: a push needs a new frame). So the node asks, with
//! the verb it already has.
//!
//! # What it does
//!
//! 1. **Armed for every lease** ([`PathWatch::armed`]), because a bridge on any venue can panic under a lease that stays
//!    up. What differs per venue is what a live adapter's report says: [`reports_path`] names the venues whose bridge
//!    states a path, read off the venue's connection row (`vike_venues::venue_connection`'s `path_report`: dukascopy's
//!    owned child process, ctrader's account session, a stream on the shared pump (OANDA's included), and the order
//!    sessions of fxcm, ibkr and ig), held equal to the gatewayhub lane's answer by `vike-gatewayhub`'s
//!    `bridge_trade_tests.rs`; any other (alpaca) reports none. A bridge that cannot vouch for its path either way right
//!    now sends no report, which reads not known, never up.
//! 2. **The ask** (`reteach.rs`'s `ask_path`, on the worker thread the lease already has): a `TradeStatus` every
//!    [`ASK_EVERY`]. The daemon's report carries `venue_link` for a lane that names a path
//!    (`TradeStatusReport::venue_link`).
//! 3. **The verdict** ([`PathWatch::hear`], pure): a report of a path that is down becomes the account's entry in
//!    `TradeLeaseRegistry::set_venue_link_down`, from which the publisher stamps every observe frame
//!    (`CoreSnapshotWire::venue_link_down`) and `trade health`'s engine row degrades; a report of an up path clears it and is
//!    stamped as such (`CoreSnapshotWire::venue_path_up`, for as long as the answers stay fresh). A daemon that has no adapter
//!    for the account now says nothing (a restart is re-teaching: no verdict) until [`NO_ADAPTER_ASKS`] asks in a row
//!    found none, and then that is the verdict ([`no_adapter`]): nothing in this node restarts a bridge that panicked. A
//!    live adapter whose report names no path (the lane names none, or the daemon predates the field) clears such a
//!    verdict and is never read as "up": the account stays not known, and the asking goes on, for the panic.
//! 4. **The lines read the path's STATE, not the venue** ([`PathWatch::hear`]'s `newly`, said by `reteach.rs`'s
//!    `say_path`): the wire's facts in the one
//!    connection-state vocabulary, by the same mapping the gatewayhub's translator uses
//!    (`vike_model::feed_status::order_path_state`). A failure (`Error`: an open, a restart or a session failed) is said
//!    DOWN at once; a reopen after the venue ended a session (`Connecting`) only when the ask before found it down too,
//!    so a 1.5 s routine reconnect (hyperliquid's testnet `1000 Expired`, every ~10 min) says nothing. The return is
//!    said only after a DOWN was. The verdict in the registry is NOT held back: `trade health` reads each ask's answer.
//!
//! # What it never does
//!
//! It sends no order, cancels nothing, halts nothing and ends no lease; the bridge's own refusals and its own
//! restart are untouched. It is a reading. The reading is as old as the last ask (at most [`ASK_EVERY`], plus the
//! time the daemon takes to answer).

use std::time::{Duration, Instant};

use vike_gatewayhub_client::proto::TradeStatusReport;
use vike_model::feed_status::{DataClientConnectionState, order_path_state};
use vike_tradehub_client::wire::VenueLinkDownWire;
use vike_venues::venue_connection::VenueConnectionPathReport::{
    AccountSession, NotReported, OwnSession, SharedPump, Sidecar,
};
use vike_venues::venue_connection::venue_wide_exec_for;

use super::trade_lease_registry::frame_words;

/// How often a lease is asked. Short against an outage that lasts minutes, long against a status request (one frame,
/// answered from memory by the daemon).
pub const ASK_EVERY: Duration = Duration::from_secs(5);

/// How many asks in a row may find no adapter before the watch calls it a verdict: about 30 s at [`ASK_EVERY`]. Fewer, and
/// the seconds a restarted daemon or a slow re-teaching takes would read as a dead adapter.
const NO_ADAPTER_ASKS: u32 = 6;

/// What a lease with no live adapter costs, for every way it comes to that.
const NO_ADAPTER_EFFECT: &str = "every order and cancel of this account is refused, and nothing here starts it again. \
                                 Fix the cause, then set the account's `account.active` off and on";

/// The entry that says the lease is up and the gatewayhub has no live adapter behind it.
#[must_use]
pub fn no_adapter(route_key: &str, why: &str) -> VenueLinkDownWire {
    VenueLinkDownWire {
        route_key: route_key.to_string(),
        why: frame_words(why),
        failed_restarts: 0,
        retry_in_ms: 0,
        effect: NO_ADAPTER_EFFECT.to_string(),
        no_adapter: true,
    }
}

/// Whether `venue`'s bridge states a path it can lose and heal while the lease stays up, by its connection row's
/// `path_report`: an exec that runs through a child process the gatewayhub's bridge owns and restarts by itself,
/// cTrader's account session, which the venue can end on a socket that stays up (`crates/bridges/ctrader/CLAUDE.md`'s
/// "A lapsed account session"), a stream (the shared user-data pump's, `vike_bridge_core::websocket_session::user::stream_link`),
/// or an order session (fxcm: the SDK's session status; ig: the REST session and the trade stream, two paths of its
/// one cell; ibkr: the socket's link watch), each written into the account's one cell
/// (`vike_bridge_core::venue_link::venue_link`). An order session's report says "not known" (no `venue_link`) while the
/// bridge cannot vouch either way.
#[must_use]
pub fn reports_path(venue: &str) -> bool {
    venue_wide_exec_for(venue).path_report != NotReported
}

/// What a path's return means for `venue`, for the line that says so: only a stream's loss left the orders going out,
/// and an order session's return says only that the bridge says it answers again.
#[must_use]
pub fn back_words(venue: &str) -> &'static str {
    match venue_wide_exec_for(venue).path_report {
        SharedPump => "the bridge reopened it and what the gap holds is read back",
        OwnSession => "the bridge says its session with the venue answers again",
        AccountSession | Sidecar | NotReported => {
            "the bridge has it back and orders are accepted again"
        }
    }
}

/// What one report changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    /// No verdict: the daemon has no adapter for the account at this moment.
    Nothing,
    /// The adapter is live and its report says no path is down (or names none): the account has no outage. `was_down`
    /// = its loss was said, so the return is worth a line.
    Up { was_down: bool },
    /// The path is down. `newly` = its loss is worth a line now and was not said yet: at once for a failure (`Error`), and
    /// for a reopen (`Connecting`) only when the ask before found it down too.
    Down { down: VenueLinkDownWire, newly: bool },
}

/// Whether a path reported down has FAILED (`Error`: an open, a restart or a session failed, or the bridge gave up),
/// rather than reopening after the venue ended a session (`Connecting`), read off the wire's facts by the one order-path
/// mapping the gatewayhub's translator uses.
#[must_use]
pub(super) fn failing(failed_restarts: u32) -> bool {
    order_path_state(Some(false), failed_restarts) == DataClientConnectionState::Error
}

/// One lease's watch of its adapter and its venue path.
#[derive(Debug, Clone, Copy)]
pub struct PathWatch {
    next: Instant,
    /// The last verdict was down.
    down: bool,
    /// The current outage's loss was said (a line): only then is its return said.
    said: bool,
    /// The asks in a row that found no adapter.
    no_adapter_asks: u32,
}

impl PathWatch {
    /// The watch of a lease, due at once.
    #[must_use]
    pub fn armed(now: Instant) -> Self {
        Self { next: now, down: false, said: false, no_adapter_asks: 0 }
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

    /// Judge one status report of the account `route_key`.
    pub fn hear(&mut self, route_key: &str, status: &TradeStatusReport) -> Reading {
        // Not an answer about the path: a daemon that restarted has no adapter until the re-teaching starts one,
        // and the report it gives then carries no `venue_link` for that reason, not because the venue has none. But
        // nothing here restarts an adapter whose bridge panicked, and the lease stays up over it, so after a while the
        // silence is the answer: this is the account that read `HEALTH: OK` with every order refused.
        if !status.adapter_up {
            self.no_adapter_asks = self.no_adapter_asks.saturating_add(1);
            if self.no_adapter_asks < NO_ADAPTER_ASKS {
                return Reading::Nothing;
            }
            self.down = true;
            let newly = !std::mem::replace(&mut self.said, true);
            let why =
                "the gatewayhub reports none on this lease; its journal says why (TRADE-LINK)";
            return Reading::Down { down: no_adapter(route_key, why), newly };
        }
        self.no_adapter_asks = 0;
        let Some(link) = status.venue_link.as_ref().filter(|link| !link.up) else {
            return self.up();
        };
        // A reopen after the venue ended a session is the pump's routine (hyperliquid's testnet ends one every ~10 min
        // and is back in about 1.5 s): it is said only when the ask before found the path down too, one `ASK_EVERY`.
        let held = std::mem::replace(&mut self.down, true);
        let newly = !self.said && (held || failing(link.failed_restarts));
        self.said |= newly;
        Reading::Down {
            down: VenueLinkDownWire {
                route_key: route_key.to_string(),
                why: link.why.clone(),
                failed_restarts: link.failed_restarts,
                retry_in_ms: link.retry_in_ms,
                effect: link.effect.clone(),
                no_adapter: false,
            },
            newly,
        }
    }

    fn up(&mut self) -> Reading {
        self.down = false;
        Reading::Up { was_down: std::mem::replace(&mut self.said, false) }
    }
}

#[path = "venue_link_tests.rs"]
#[cfg(test)]
mod venue_link_tests;
