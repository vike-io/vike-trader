//! `VenueTimings` — the STATIC per-venue CONNECTION and LIVENESS timing table (step-1 declaration).
//!
//! How long a bridge waits to dial, to be acknowledged, to hear anything before it calls a session
//! dead, and before it dials again, are **facts about the venue's protocol**: a server ping cadence
//! (Binance spot pings every 20 s, its perp stream every 180 s, Aster's every 300 s), a key expiry
//! (a listenKey lives 60 minutes), how slowly a platform logs a client in (JForex took over a minute
//! on a cold cache). They used to be local constants, one set per bridge crate under a different
//! name in each (`HANDSHAKE_TIMEOUT`, `CONNECT_TIMEOUT`, `RECV_IDLE_TIMEOUT`, `PING_EVERY`,
//! `HEARTBEAT_INTERVAL`, `HANDSHAKE_BUDGET`, `REDIAL`, …), so nothing let a reader put two venues
//! side by side. This table is that place: every venue uses the SAME field names, and where two
//! venues differ the difference sits on adjacent lines, each value with its reason. It follows the
//! same playbook as [`crate::venue_rate_limits`] — one NAMED row per venue citing the
//! adapter code it was read from, a verbatim matrix pin test, a completeness test iterating
//! [`vike_model::VENUES`] — and decision 0160 is the record.
//!
//! ## A compiled FACT table, not a setting
//! Nothing here is configurable. There is no environment variable (decision 0095) and no settings
//! row (decision 0086): an operator who could stretch an idle threshold would be choosing how long a
//! dead order stream stays unnoticed, which is not a preference. A timing changes by editing a row
//! in a PR, with the reason beside it.
//!
//! ## Equal numbers do NOT mean equal reasons
//! Five exec lanes idle out at 60 s, and that one number has three origins (three 20 s pings, four
//! 15 s pings, six 10 s pings). Collapsing them into one shared constant would erase exactly
//! what a retune needs: the reason. So every declared value is a [`VenueTimingsValue`](crate::venue_timings::VenueTimingsValue), a duration PLUS its
//! `why`, and `every_timing_cites_its_reason` refuses a row without one. The numbers that DO share a
//! reason — a stop-poll tick, a first backoff, a resync settle, a fill sentinel — have none to put in
//! a `why`, and are constants of `vike-bridge-core` instead (the mechanism's defaults), not rows.
//!
//! ## The vocabulary
//! One [`VenueTimingsLane`](crate::venue_timings::VenueTimingsLane) describes one connection kind of a venue, with these fields and no others:
//! - `dial_timeout` — the bound on the TCP connect.
//! - `handshake_timeout` — ONE deadline covering every acknowledgement the handshake waits for
//!   (login ack, then subscribe ack), started once the socket is up.
//! - `subscribe_acknowledgement_timeout` — the shared market pump's subscribe-acknowledgement watchdog: how long the
//!   pump waits for the venue to acknowledge the subscribe frame it replays on an open session. A wait after the
//!   handshake, not a second spelling of it: ig's market lane waits 15 s for `CONOK` (its handshake) and then 10 s
//!   for `SUBOK` (this).
//! - `response_timeout` — the wait for ONE reply on an open connection, or for an HTTP response
//!   head. A per-reply wait, never an alias of the dial or the handshake: oanda's
//!   `timeout_recv_response` and cTrader's per-response wait are this.
//! - `keepalive_every` — the cadence of a client frame that makes the peer answer.
//! - `session_renew_every` — the cadence of a REST renewal of a server-side session or key that
//!   produces NO inbound frame on the socket (a listenKey PUT, IBKR's tickle). It is deliberately not
//!   a keepalive: it proves nothing about the socket, so a liveness threshold never counts it.
//! - `idle_threshold` — the longest silence, of any frame kind, before the session is declared dead.
//! - `auth_probe_every` — the cadence of a small account-scoped request whose answer proves the venue
//!   still holds the SESSION as authorized (an ended session refuses it). Deliberately not a
//!   keepalive: frames keep flowing on a socket whose session the venue has ended without a word
//!   (cTrader's lapsed access token), so no socket-level fact can see it.
//! - `lost_after` — how long a counted part of the session may stay broken before the whole link is
//!   called lost.
//! - `probation` — how long a session must run with nothing counted broken before an outage is
//!   forgotten. Longer than `lost_after` whenever both are declared, or a session would be forgiven
//!   just before it could be judged.
//! - `reconnect` — the wait before dialling again ([`VenueTimingsReconnect`](crate::venue_timings::VenueTimingsReconnect)): `initial` growing to `max`, and
//!   `initial == max` means a fixed wait.
//! - `history_lag` — how long after a fill the venue's REST history (the reconnect replay's source) may
//!   still not list it. The reconnect resync (`vike_bridge_core::websocket_session::user::run_user_resync`)
//!   replays once after its settle and, when this is declared, a second time `history_lag` later, then
//!   nudges reconcile. `None` is one replay, not a missing facility, so no accessor panics on it: a
//!   bridge reads the field.
//! - `session_lifetime` — the longest the venue documents it keeps one session open before it cuts it (a
//!   24-hour connection). DECLARED ONLY: nothing reads it, nothing rotates a session ahead of the cut, and
//!   the lane's reconnect and replay meet it after the fact. `None` is no documented limit, so, like
//!   `history_lag`, it has no panicking accessor.
//! - `submit` — an exec lane's worst-case SUBMIT CHAIN ([`VenueTimingsSubmit`](crate::venue_timings::VenueTimingsSubmit)):
//!   how old an order may be when its write goes, every call from the write to the bridge's answer at its bound, and
//!   the bridge's own hold over those calls. Its [`VenueTimingsSubmit::answered_by`](crate::venue_timings::VenueTimingsSubmit::answered_by)
//!   is held under the core's last-resort reject (`vike_model::submit_ack_last_resort` at the shipped defaults, the
//!   backstop of `crates/vike-core/src/runtime/watchdog/orders.rs`'s `sweep_stuck_orders`, decision 0171) by
//!   `every_declared_submit_chain_answers_before_the_cores_last_resort`, so a slow but legitimate submit is answered
//!   before the backstop rejects it. Declared for the four chains longer than the shared exec loop's (one POST and two
//!   re-queries, pinned beside `vike_bridge_core::exec_actor::MAX_SUBMIT_WAIT`): IG's confirm ladder, IBKR's Client
//!   Portal reply chain, Alpaca's token mint and Polymarket's serial queue; each bridge pins its own constants to its
//!   row in a `const` assert that reads the field (an absent chain fails it). `None` on every other lane, and no
//!   panicking accessor.
//!
//! Three lanes per venue ([`VenueTimings`](crate::venue_timings::VenueTimings)): `exec` (the order and fill stream), `market` (the live
//! market-data connection) and `depth` ([`VenueTimingsDepth`](crate::venue_timings::VenueTimingsDepth), the L2 book driver's four knobs, which the
//! depth driver takes as arguments rather than as a lane).
//!
//! ## Absence is declared, never guessed
//! `None` is "the venue has no such facility, or the bridge sets no bound": binance's spot socket
//! has no dial bound because `tungstenite::connect` takes none, polymarket's user channel has no
//! handshake because the venue sends no ack. The `const fn` accessors a bridge reads
//! ([`VenueTimingsLane::idle_threshold()`](crate::venue_timings::VenueTimingsLane::idle_threshold), [`VenueTimings::exec()`](crate::venue_timings::VenueTimings::exec), …) PANIC on `None` — the
//! [`crate::venue_rate_limits::VenueRateLimitsMeter::published`] precedent — so a bridge constant that reads
//! a facility the row does not declare fails the BUILD rather than inventing a number. The fields are
//! `pub`, so a caller that wants to branch reads the `Option` directly.
//!
//! ## What lives elsewhere
//! - **The nine pump venues' market lanes** (binance, aster, bybit, okx, hyperliquid, polymarket,
//!   deribit, ig, alpaca) are declared HERE (their keepalive, their subscribe-acknowledgement watchdog, their idle
//!   limit and their redial), and the shared market pump reads them (`vike_bridge_core::websocket_session::spec::row`
//!   names no venue: B3, 2026-11-22). The dial bound and the read poll stay the pump's mechanism defaults (`CONNECT_10S`,
//!   `READ_POLL`), no row of any venue: `dial_timeout` is `None` on these lanes, as on the exec lanes that take the
//!   same window.
//! - **Retry policies that are not liveness**: the REST and pager `RetryBackoffPolicy`, deribit's token
//!   refresh, polymarket's dead-man heartbeat, cTrader's connect retry and the depth driver's own
//!   keepalive are bridge-local and not modelled.
//! - **Per-call bounds** (a listenKey POST's 5 s, a reconcile read's deadline) are properties of one
//!   REST call, not of the connection, and are not modelled either, except as the calls of a declared
//!   `submit` chain (above), whose sum is what the core's backstop must outlast.
//! - **Secondary feeds** a venue runs beside its main one — deribit's options-chain and DVOL pumps,
//!   polymarket's RTDS socket — keep their local constants: the `market` lane is the main feed's.
//!
//! ## Honesty rule (mirrors `venue_rate_limits`)
//! Every value records TODAY'S REALITY — what the adapter does on the wire right now. A known
//! imperfection is declared in the row's doc, never quietly corrected: changing a number is a
//! retune, and a retune is a separate change from moving where the number lives.

use std::time::Duration;

use crate::venue_rate_limits::VenueRateLimitsMarket;

/// One declared duration and the reason it has that value.
///
/// `why` names the source — the adapter file the value was read from, or the measurement that
/// produced it — and `every_timing_cites_its_reason` refuses an empty or token one. It is the field
/// that keeps two equal numbers from being read as one fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueTimingsValue {
    /// The duration itself.
    pub value: Duration,
    /// Why it is this long, and where it was read from.
    pub why: &'static str,
}

impl VenueTimingsValue {
    /// A whole-second timing — what almost every row declares.
    #[must_use]
    pub const fn secs(secs: u64, why: &'static str) -> VenueTimingsValue {
        VenueTimingsValue { value: Duration::from_secs(secs), why }
    }

    /// A millisecond timing, for the few sub-second values (cTrader's first redial).
    #[must_use]
    pub const fn millis(millis: u64, why: &'static str) -> VenueTimingsValue {
        VenueTimingsValue { value: Duration::from_millis(millis), why }
    }
}

/// How a lane waits before it dials again after a drop or a failed dial: `initial` for the first
/// wait, growing to `max` (doubling, on the lanes that grow), and `initial == max` a fixed wait.
///
/// One `why` for the pair: the two numbers are one policy, and splitting the reason would let the
/// ceiling be retuned without anyone reading why the start was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueTimingsReconnect {
    /// The first wait.
    pub initial: Duration,
    /// The ceiling the wait grows to (equal to `initial` for a fixed wait).
    pub max: Duration,
    /// Why this policy, and where it was read from.
    pub why: &'static str,
}

impl VenueTimingsReconnect {
    /// A wait growing from `initial` to `max`.
    #[must_use]
    pub const fn new(initial: Duration, max: Duration, why: &'static str) -> VenueTimingsReconnect {
        VenueTimingsReconnect { initial, max, why }
    }

    /// A fixed wait: `initial == max`.
    #[must_use]
    pub const fn fixed(every: Duration, why: &'static str) -> VenueTimingsReconnect {
        VenueTimingsReconnect { initial: every, max: every, why }
    }

    /// Is this a fixed wait (`initial == max`)?
    #[must_use]
    pub const fn is_fixed(&self) -> bool {
        self.initial.as_nanos() == self.max.as_nanos()
    }

    /// Is the policy usable: a non-zero first wait that does not exceed the ceiling? A zero wait is
    /// a hot reconnect loop against the venue.
    #[must_use]
    pub const fn is_sane(&self) -> bool {
        !self.initial.is_zero() && self.initial.as_nanos() <= self.max.as_nanos()
    }
}

/// One kind of call in a submit's chain ([`VenueTimingsSubmit`]): what it is, how many of it one submit makes at
/// most, and the bound on each (its agent's timeout, or the pause between two).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueTimingsSubmitCall {
    /// What the call is, in words.
    pub what: &'static str,
    /// How many of it one submit makes at most.
    pub count: u32,
    /// The bound on each one.
    pub each: Duration,
}

impl VenueTimingsSubmitCall {
    /// `count` calls of `what`, each bounded by `each`.
    #[must_use]
    pub const fn new(what: &'static str, count: u32, each: Duration) -> VenueTimingsSubmitCall {
        VenueTimingsSubmitCall { what, count, each }
    }
}

/// **The worst case of ONE submit** on an exec lane, from the core's registration of the order to the bridge's
/// answer (the module doc's `submit`): the order's age when its write goes, then every call to the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueTimingsSubmit {
    /// The oldest an order may be when its write is sent (the shared exec queue's wait, or the bridge's own age
    /// limit). Every read in front of the write counts against it: a read that outlives it refuses the order unsent.
    pub write_by: Duration,
    /// Every call from the write to the answer, in order, each at its bound.
    pub calls: &'static [VenueTimingsSubmitCall],
    /// The bridge's own deadline over `calls`, counted from the write: a call that could not end inside it is not
    /// made, and the order's outcome is then the write's answer, or unknown (never a guess). `None`: every call runs.
    pub hold: Option<Duration>,
    /// Why this chain, and where it was read from.
    pub why: &'static str,
}

impl VenueTimingsSubmit {
    /// Every call of the chain at its bound, end to end (saturating).
    #[must_use]
    pub const fn calls_at_their_bounds(&self) -> Duration {
        let mut total = Duration::ZERO;
        let mut i = 0;
        while i < self.calls.len() {
            let call = self.calls[i];
            total = total.saturating_add(call.each.saturating_mul(call.count));
            i += 1;
        }
        total
    }

    /// **The latest the bridge answers**, counted from the order's registration: `write_by`, then the calls at their
    /// bounds or the hold, whichever is shorter.
    #[must_use]
    pub const fn answered_by(&self) -> Duration {
        let calls = self.calls_at_their_bounds();
        let after_the_write = match self.hold {
            Some(hold) if hold.as_nanos() < calls.as_nanos() => hold,
            _ => calls,
        };
        self.write_by.saturating_add(after_the_write)
    }

    /// Is the chain usable: a write age, at least one call, every call made at least once with a bound, and a hold
    /// (where one is kept) that is not zero?
    #[must_use]
    pub const fn is_sane(&self) -> bool {
        let mut i = 0;
        while i < self.calls.len() {
            if self.calls[i].count == 0 || self.calls[i].each.is_zero() {
                return false;
            }
            i += 1;
        }
        let hold_ok = match self.hold {
            Some(hold) => !hold.is_zero(),
            None => true,
        };
        !self.write_by.is_zero() && !self.calls.is_empty() && hold_ok
    }
}

/// The timings of ONE connection kind of a venue. Every field is `None` unless the venue (or the
/// bridge) has the facility, and the row says why in its doc; see the module doc for each field's
/// meaning. Build a row with struct-update syntax from [`VenueTimingsLane::NONE`], so a field added here costs
/// one edit and not one per row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueTimingsLane {
    /// The TCP connect bound.
    pub dial_timeout: Option<VenueTimingsValue>,
    /// One deadline covering every handshake acknowledgement.
    pub handshake_timeout: Option<VenueTimingsValue>,
    /// The market pump's wait for the venue to acknowledge a replayed subscribe frame.
    pub subscribe_acknowledgement_timeout: Option<VenueTimingsValue>,
    /// The wait for one reply on an open connection, or for an HTTP response head.
    pub response_timeout: Option<VenueTimingsValue>,
    /// The cadence of a client frame that makes the peer answer.
    pub keepalive_every: Option<VenueTimingsValue>,
    /// The cadence of a REST renewal of a server-side session or key (no inbound frame results).
    pub session_renew_every: Option<VenueTimingsValue>,
    /// The longest silence, of any frame kind, before the session is declared dead.
    pub idle_threshold: Option<VenueTimingsValue>,
    /// The cadence of an account-scoped request that proves the session is still authorized.
    pub auth_probe_every: Option<VenueTimingsValue>,
    /// How long a counted part of the session may stay broken before the link is called lost.
    pub lost_after: Option<VenueTimingsValue>,
    /// How long a session must run with nothing broken before an outage is forgotten.
    pub probation: Option<VenueTimingsValue>,
    /// The wait before dialling again.
    pub reconnect: Option<VenueTimingsReconnect>,
    /// How long the venue's REST history may lag a fill: the reconnect resync's second replay comes
    /// this long after its first. `None`: one replay.
    pub history_lag: Option<VenueTimingsValue>,
    /// The longest the venue documents it keeps one session open. Declared only: nothing reads it.
    pub session_lifetime: Option<VenueTimingsValue>,
    /// An exec lane's worst-case submit chain, where it is longer than the shared exec loop's.
    pub submit: Option<VenueTimingsSubmit>,
}

impl VenueTimingsLane {
    /// A lane with nothing declared — the base every row's struct-update starts from.
    pub const NONE: VenueTimingsLane = VenueTimingsLane {
        dial_timeout: None,
        handshake_timeout: None,
        subscribe_acknowledgement_timeout: None,
        response_timeout: None,
        keepalive_every: None,
        session_renew_every: None,
        idle_threshold: None,
        auth_probe_every: None,
        lost_after: None,
        probation: None,
        reconnect: None,
        history_lag: None,
        session_lifetime: None,
        submit: None,
    };

    /// The TCP connect bound. **Panics** when the lane declares none: in a `const` initializer —
    /// how every bridge reads this — that is a COMPILE error.
    #[must_use]
    pub const fn dial_timeout(&self) -> Duration {
        match self.dial_timeout {
            Some(t) => t.value,
            None => panic!("this lane declares no dial_timeout"),
        }
    }

    /// The handshake deadline. **Panics** when the lane declares none.
    #[must_use]
    pub const fn handshake_timeout(&self) -> Duration {
        match self.handshake_timeout {
            Some(t) => t.value,
            None => panic!("this lane declares no handshake_timeout: it has no handshake to bound"),
        }
    }

    /// The subscribe-acknowledgement watchdog. **Panics** when the lane declares none.
    #[must_use]
    pub const fn subscribe_acknowledgement_timeout(&self) -> Duration {
        match self.subscribe_acknowledgement_timeout {
            Some(t) => t.value,
            None => panic!(
                "this lane declares no subscribe_acknowledgement_timeout: the pump watches no subscribe acknowledgement"
            ),
        }
    }

    /// The per-reply wait. **Panics** when the lane declares none.
    #[must_use]
    pub const fn response_timeout(&self) -> Duration {
        match self.response_timeout {
            Some(t) => t.value,
            None => {
                panic!("this lane declares no response_timeout: the bridge sets no reply bound")
            }
        }
    }

    /// The client keepalive cadence. **Panics** when the lane declares none.
    #[must_use]
    pub const fn keepalive_every(&self) -> Duration {
        match self.keepalive_every {
            Some(t) => t.value,
            None => panic!("this lane declares no keepalive_every: no client frame keeps it alive"),
        }
    }

    /// The session-renewal cadence. **Panics** when the lane declares none.
    #[must_use]
    pub const fn session_renew_every(&self) -> Duration {
        match self.session_renew_every {
            Some(t) => t.value,
            None => panic!("this lane declares no session_renew_every: no server session to renew"),
        }
    }

    /// The silent-stall threshold. **Panics** when the lane declares none.
    #[must_use]
    pub const fn idle_threshold(&self) -> Duration {
        match self.idle_threshold {
            Some(t) => t.value,
            None => {
                panic!("this lane declares no idle_threshold: the bridge runs no stall watchdog")
            }
        }
    }

    /// The session-authorization probe cadence. **Panics** when the lane declares none.
    #[must_use]
    pub const fn auth_probe_every(&self) -> Duration {
        match self.auth_probe_every {
            Some(t) => t.value,
            None => {
                panic!("this lane declares no auth_probe_every: the bridge sends no auth probe")
            }
        }
    }

    /// How long a counted part may stay broken. **Panics** when the lane declares none.
    #[must_use]
    pub const fn lost_after(&self) -> Duration {
        match self.lost_after {
            Some(t) => t.value,
            None => panic!("this lane declares no lost_after: the bridge runs no link watchdog"),
        }
    }

    /// How long a clean session runs before an outage is forgotten. **Panics** when the lane declares
    /// none.
    #[must_use]
    pub const fn probation(&self) -> Duration {
        match self.probation {
            Some(t) => t.value,
            None => panic!("this lane declares no probation: the bridge forgets no outage"),
        }
    }

    /// The reconnect policy. **Panics** when the lane declares none.
    #[must_use]
    pub const fn reconnect(&self) -> VenueTimingsReconnect {
        match self.reconnect {
            Some(r) => r,
            None => panic!("this lane declares no reconnect policy: the bridge does not redial"),
        }
    }

    /// Is every declared value usable: no zero duration, a sane reconnect, an idle threshold at
    /// least twice the keepalive cadence, and a probation longer than `lost_after`, whenever both
    /// of a pair are set? Backs the compile-time invariants below.
    ///
    /// The twofold margin is the one structural fact about liveness the rows share: a threshold
    /// under two keepalive periods trips on a single late pong, tearing down a healthy socket
    /// forever (the failure `perp_user_data.rs` warns about by name).
    #[must_use]
    pub const fn is_sane(&self) -> bool {
        let reconnect_ok = match self.reconnect {
            Some(r) => r.is_sane(),
            None => true,
        };
        let submit_ok = match self.submit {
            Some(s) => s.is_sane(),
            None => true,
        };
        positive(self.dial_timeout)
            && positive(self.handshake_timeout)
            && positive(self.subscribe_acknowledgement_timeout)
            && positive(self.response_timeout)
            && positive(self.keepalive_every)
            && positive(self.session_renew_every)
            && positive(self.idle_threshold)
            && positive(self.auth_probe_every)
            && positive(self.lost_after)
            && positive(self.probation)
            && positive(self.history_lag)
            && positive(self.session_lifetime)
            && reconnect_ok
            && submit_ok
            && idle_outlasts_keepalive(self.idle_threshold, self.keepalive_every)
            && probation_outlasts_lost_after(self.lost_after, self.probation)
    }
}

/// `probation > lost_after` whenever both are declared.
const fn probation_outlasts_lost_after(
    lost_after: Option<VenueTimingsValue>,
    probation: Option<VenueTimingsValue>,
) -> bool {
    match (lost_after, probation) {
        (Some(lost), Some(probation)) => probation.value.as_nanos() > lost.value.as_nanos(),
        _ => true,
    }
}

/// An undeclared timing is fine; a declared one must not be zero.
const fn positive(timing: Option<VenueTimingsValue>) -> bool {
    match timing {
        Some(t) => !t.value.is_zero(),
        None => true,
    }
}

/// `idle >= 2 x keepalive` whenever both are declared.
const fn idle_outlasts_keepalive(
    idle: Option<VenueTimingsValue>,
    keepalive: Option<VenueTimingsValue>,
) -> bool {
    match (idle, keepalive) {
        (Some(idle), Some(keepalive)) => idle.value.as_nanos() >= 2 * keepalive.value.as_nanos(),
        _ => true,
    }
}

/// The four knobs of the L2 depth driver (`vike_bridge_core::depth::run_depth`), which it takes
/// as arguments and not as a lane: the dead-transport watchdog, the data-freshness threshold, the
/// optional timed re-seed and the reconnect pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueTimingsDepth {
    /// No frame of ANY kind for this long: the socket is dead, so reconnect and re-seed.
    pub idle_threshold: VenueTimingsValue,
    /// The book has applied no update for this long behind a live socket: disclose `Stale`.
    pub freshness_threshold: VenueTimingsValue,
    /// Forced fresh-snapshot cadence for a venue with no book checksum; `None` = no timed re-seed.
    pub reseed_interval: Option<VenueTimingsValue>,
    /// The pause between a fault and the next dial. A timed re-seed re-seeds in place on the open
    /// socket and dials nothing (`vike_bridge_core::depth`'s `run_depth_session`).
    pub backoff: VenueTimingsValue,
}

impl VenueTimingsDepth {
    /// The dead-transport watchdog threshold.
    #[must_use]
    pub const fn idle_threshold(&self) -> Duration {
        self.idle_threshold.value
    }

    /// The data-freshness threshold.
    #[must_use]
    pub const fn freshness_threshold(&self) -> Duration {
        self.freshness_threshold.value
    }

    /// The timed re-seed cadence. **Panics** when the venue is not re-seeded on a timer; the
    /// driver's `reseed_interval` argument is an `Option`, so a bridge that declares none passes
    /// `None` itself (and reads the field to branch), exactly as it does today.
    #[must_use]
    pub const fn reseed_interval(&self) -> Duration {
        match self.reseed_interval {
            Some(t) => t.value,
            None => {
                panic!(
                    "this depth block declares no reseed_interval: no timed re-seed for the venue"
                )
            }
        }
    }

    /// The reconnect pause.
    #[must_use]
    pub const fn backoff(&self) -> Duration {
        self.backoff.value
    }

    /// Is every declared value non-zero? Backs the compile-time invariants below.
    #[must_use]
    pub const fn is_sane(&self) -> bool {
        positive(Some(self.idle_threshold))
            && positive(Some(self.freshness_threshold))
            && positive(self.reseed_interval)
            && positive(Some(self.backoff))
    }
}

/// The declared timing row for ONE (venue, market): three optional facilities. `Copy`, so a bridge
/// holds it by value in a `const`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueTimings {
    /// The order and fill stream — the lane whose silence strands an order.
    pub exec: Option<VenueTimingsLane>,
    /// The live market-data connection (the module doc says which venues declare one).
    pub market: Option<VenueTimingsLane>,
    /// The L2 depth driver's knobs, for the venues that run it.
    pub depth: Option<VenueTimingsDepth>,
}

impl VenueTimings {
    /// The row for a venue with nothing declared — the fail-closed fallback: every accessor panics.
    pub const NOT_DECLARED: VenueTimings = VenueTimings { exec: None, market: None, depth: None };

    /// The exec lane. **Panics** when the venue declares none.
    #[must_use]
    pub const fn exec(&self) -> VenueTimingsLane {
        match self.exec {
            Some(lane) => lane,
            None => panic!("this venue declares no exec lane timings"),
        }
    }

    /// The market lane. **Panics** when the venue declares none: ctrader and IBKR's socket product, whose
    /// quotes ride their exec session, and fxcm and dukascopy.
    #[must_use]
    pub const fn market(&self) -> VenueTimingsLane {
        match self.market {
            Some(lane) => lane,
            None => panic!("this venue declares no market lane timings here"),
        }
    }

    /// The depth driver's knobs. **Panics** when the venue runs no depth driver.
    #[must_use]
    pub const fn depth(&self) -> VenueTimingsDepth {
        match self.depth {
            Some(depth) => depth,
            None => panic!("this venue declares no depth driver timings"),
        }
    }

    /// Does every lane in the row pass its own check? Backs the compile-time invariants below.
    #[must_use]
    pub const fn is_sane(&self) -> bool {
        let execution_ok = match self.exec {
            Some(lane) => lane.is_sane(),
            None => true,
        };
        let market_ok = match self.market {
            Some(lane) => lane.is_sane(),
            None => true,
        };
        let depth_ok = match self.depth {
            Some(depth) => depth.is_sane(),
            None => true,
        };
        execution_ok && market_ok && depth_ok
    }
}

impl Default for VenueTimings {
    fn default() -> Self {
        VenueTimings::NOT_DECLARED
    }
}

// ---------------------------------------------------------------------------------------------
// The per-venue rows. Each cites the adapter code its numbers were read from.
// ---------------------------------------------------------------------------------------------

/// The user-data pump's reconnect backoff: a 1 s first wait doubling to a 30 s ceiling. Eight exec
/// lanes run it with the same numbers and eight different sources, so the numbers are written once
/// here and each row supplies its own `why`. They are `vike-bridge-core`'s mechanism defaults for the
/// pump (no venue reason of their own), and a test there holds this function to them.
const fn user_data_backoff(why: &'static str) -> VenueTimingsReconnect {
    VenueTimingsReconnect::new(Duration::from_secs(1), Duration::from_secs(30), why)
}

/// Binance's depth-driver knobs. Spot and perp run the same `depth_main`, and so does aster
/// (see [`ASTER_SPOT`]).
const BINANCE_DEPTH: VenueTimingsDepth = VenueTimingsDepth {
    idle_threshold: VenueTimingsValue::secs(
        30,
        "no depth frame of ANY kind, a diff or a server Ping, for this long means a silently dead \
         socket; sized well above the ~100 ms diff cadence and the server-ping interval, so a \
         healthy book never false-trips and only true transport silence does \
         (read by `crates/bridges/binance/src/family/feed/mod.rs`'s `depth_main`)",
    ),
    freshness_threshold: VenueTimingsValue::secs(
        120,
        "MEASURED 2026-07-11 on the binance, bybit and okx keyless depth feeds: liquid pairs \
         (BTC/ETH) update in ~0.1 s, mid-caps gap ~3.6 s at most, and the thinnest actively-traded \
         pairs gapped 44-47 s (a near-dead pair updated only twice in 5 min), so 120 s clears the \
         worst case by ~2.5x and only a genuinely near-dead book trips; kept at that measured \
         floor, not a placeholder, because one value must cover the thinnest subscribable symbol \
         (read by `crates/bridges/binance/src/family/feed/mod.rs`'s `depth_main`)",
    ),
    reseed_interval: Some(VenueTimingsValue::secs(
        300,
        "binance publishes no book checksum, so a dropped diff that keeps the U/u chain intact \
         corrupts the book silently (no gap, no idle, no freshness trip) and would stay wrong until \
         the next natural reconnect, hours later; a forced re-snapshot every 5 minutes bounds that \
         window, well above the reconnect plus weight-50 snapshot cost, and discloses no transport \
         gap (read by `crates/bridges/binance/src/family/feed/mod.rs`'s `depth_main`)",
    )),
    backoff: VenueTimingsValue::secs(
        3,
        "throttles the re-seed after a socket error or sequence gap so a persistent gap cannot \
         hammer the weight-50 depth snapshot into a rate-limit ban \
         (read by `crates/bridges/binance/src/family/feed/mod.rs`'s `depth_main`)",
    ),
};

/// The shared market pump's classic fixed 3 s redial: eight of the nine pump venues' market lanes run it, the same
/// number with each row's own `why` (polymarket's grows instead).
const fn market_pump_redial(why: &'static str) -> VenueTimingsReconnect {
    VenueTimingsReconnect::fixed(Duration::from_secs(3), why)
}

/// The market lane BOTH binance markets run (the kline, mark and trade lanes on the shared market pump, which reads
/// it through `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`): the subscription rides the
/// URL path, so there is no subscribe frame to acknowledge, and the venue's server pings, so no client keepalive.
///
/// The idle limit's measurement (the idle-limit audit of 2026-11-22, from the dev box on the public sockets, 900 s per
/// lane, answering pings only as the lane does): the server pinged every 19.8-20.1 s on spot (`stream.binance.com`)
/// and every 179.8-180.1 s on USDⓈ-M (`fstream`, `/market` and `/public`) and COIN-M (`dstream`), as each book
/// documents (spot cuts a socket without a pong within a minute, futures within 10 minutes). On a quiet book the
/// longest silence of every frame kind was 20.1 s on spot, 135.3 s on a USDⓈ-M `@trade` and 180.1 s on a COIN-M
/// `@aggTrade` with no trade; a kline lane heard its bar at least every 60.1 s and a mark lane every 1.4 s; no socket
/// was closed. One row serves every book, so a half-open SPOT socket waits the futures' 540 s too. the CI box's gatewayhub
/// logs of 2026-11-21..22: the `BTCUSDT.P` trades lane logged no reset or error.
const BINANCE_MARKET: VenueTimingsLane = VenueTimingsLane {
    idle_threshold: Some(VenueTimingsValue::secs(
        540,
        "three of the USDⓈ-M and COIN-M books' 180 s server pings, inside their 600 s pong cut, MEASURED in the \
         idle-limit audit of 2026-11-22 (a quiet USDⓈ-M `@trade` socket silent at most 135.3 s, a quiet COIN-M \
         `@aggTrade` one 180.1 s, a spot one 20.1 s); one value serves every book (read by \
         `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
    )),
    reconnect: Some(market_pump_redial(
        "the shared market pump's fixed 3 s redial of binance's kline, mark and trade lanes (read by \
         `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
    )),
    ..VenueTimingsLane::NONE
};

/// Binance **spot** (the WS-API private stream on `ws-api.binance.com`).
///
/// - `exec` — a signed subscribe+auth session: ONE combined frame, answered by an id-matched ack, under
///   one handshake deadline; afterwards the only liveness an idle account produces is the venue's 20 s
///   server ping, which tungstenite pongs. There is no app-level ping (`keepalive_every` is `None`), no
///   REST renewal (`session_renew_every` is `None`), and the dial takes the venue-free
///   `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `CONNECT_10S`, no row of its own (`dial_timeout` is
///   `None`).
/// - `market` — the shared market pump's lanes, one lane for both markets (`BINANCE_MARKET`).
/// - `depth` — the family's depth driver knobs, shared with the perp row and with aster.
pub const BINANCE_SPOT: VenueTimings = VenueTimings {
    exec: Some(VenueTimingsLane {
        handshake_timeout: Some(VenueTimingsValue::secs(
            10,
            "one deadline for the single signed subscribe frame and its id-matched ack (read by \
             `crates/bridges/binance/src/execution/user_data/spot.rs`'s `open_binance_user_data_websocket`)",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            60,
            "3x the 20 s WS-API server ping, MEASURED over a 900 s window: 45 inbound frames, every \
             gap 19-20 s (`crates/bridges/binance/tests/binance_spot_userdata_soak.rs`); read as \
             `crates/bridges/binance/src/execution/user_data/spot.rs`'s `IDLE_THRESHOLD`",
        )),
        reconnect: Some(user_data_backoff(
            "the user-data pump's doubling backoff; the spot session passes the 30 s ceiling at both \
             of its spawn sites in `crates/bridges/binance/src/execution/user_data/spot.rs`",
        )),
        session_lifetime: Some(VenueTimingsValue::secs(
            86_400,
            "binance's WebSocket API documents one connection as valid for 24 hours, then cut (the \
             connection survey of 2026-11-21); declared only: the spot pump meets the cut by its \
             reconnect and replay, and nothing rotates the session ahead of it",
        )),
        ..VenueTimingsLane::NONE
    }),
    market: Some(BINANCE_MARKET),
    depth: Some(BINANCE_DEPTH),
};

/// Binance **USDⓈ-M futures** (the listenKey stream on `fstream.binance.com`) — a SEPARATE row from
/// spot because the two sockets are different mechanisms with different measured cadences.
///
/// - `exec` — mint a listenKey over REST, connect `<ws>/<key>`: no subscribe frame and no ack, so
///   `handshake_timeout` is `None`. The only inbound traffic on an idle account is the venue's own
///   server ping, MEASURED at 180 s (spot's is 20 s). The 1800 s listenKey PUT never touches the
///   socket and proves nothing about it, so it is `session_renew_every`, not `keepalive_every`
///   (`None`). The dial takes `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `CONNECT_10S` (`dial_timeout`
///   is `None`).
/// - `market` — the one lane both markets run (`BINANCE_MARKET`). `depth` — the family's knobs, shared with spot.
///
/// ⚠ Measured on the demo socket (`fstream.binancefuture.com`); if mainnet ever pings slower, the
/// idle threshold is the number to raise, and the soak test is how to find out.
pub const BINANCE_PERP: VenueTimings = VenueTimings {
    exec: Some(VenueTimingsLane {
        session_renew_every: Some(VenueTimingsValue::secs(
            1800,
            "a listenKey PUT inside the venue's 60-minute key expiry; it never touches the socket, so \
             it renews the key and proves no liveness (read by the shared pump, \
             `crates/bridges/binance/src/family/execution/user_data.rs`'s `spawn_user_data_with_resync`)",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            720,
            "4x the 180 s perp server ping, MEASURED on the demo socket over 900 s \
             (`crates/bridges/binance/tests/binance_userdata_soak.rs`); a shorter value would tear \
             down a healthy socket every window (read by \
             `crates/bridges/binance/src/execution/user_data/perp.rs`'s `spec`)",
        )),
        reconnect: Some(user_data_backoff(
            "the user-data pump's doubling backoff to its 30 s ceiling, which the listenKey pump \
             runs on (`crates/bridges/binance/src/family/execution/user_data.rs`'s `spawn_user_data_with_resync`, \
             over this file's `user_data_backoff`)",
        )),
        session_lifetime: Some(VenueTimingsValue::secs(
            86_400,
            "binance's USDⓈ-M user data stream documents one connection as valid for 24 hours (the \
             connection survey of 2026-11-21), apart from the 60-minute key the PUT renews; declared \
             only: the listenKey pump meets the cut by its reconnect and replay",
        )),
        ..VenueTimingsLane::NONE
    }),
    market: Some(BINANCE_MARKET),
    depth: Some(BINANCE_DEPTH),
};

/// The exec lane BOTH aster markets run: one listenKey pump (`binance`'s
/// `family::listenkey`), signed instead of header-authed. Aster's server ping is 300 s, so its
/// idle threshold is the roster's widest.
const ASTER_EXECUTION: VenueTimingsLane = VenueTimingsLane {
    keepalive_every: Some(VenueTimingsValue::secs(
        60,
        "a quarter of the 240 s after which aster's mainnet resets a socket that has sent it \
         nothing, MEASURED on the live stream 2026-11-07: of 62 sessions 12 ended at exactly 240.0 s \
         and the rest 240 s after a 300 s server ping, none past 480 s, while the testnet soak \
         outlived its 900 s; the server's ping cannot keep a socket up by itself, it comes too \
         late; read by \
         `crates/bridges/aster/src/execution/user_data/spot.rs`'s `spec` and \
         `crates/bridges/aster/src/execution/user_data/perp.rs`'s `spec`",
    )),
    session_renew_every: Some(VenueTimingsValue::secs(
        1800,
        "the same listenKey PUT as binance perp, run by the one shared pump for both aster markets \
         (`crates/bridges/binance/src/family/execution/user_data.rs`'s `spawn_user_data_with_resync`, which reads binance \
         perp's row for the cadence; that file's `the_renewal_cadence_is_one_fact_for_the_family` \
         holds this row equal to it); it renews the key and proves no liveness",
    )),
    idle_threshold: Some(VenueTimingsValue::secs(
        1200,
        "4x aster's MEASURED 300 s server ping (gaps of 299.998-300.004 s over 900 s, \
         `crates/bridges/aster/tests/aster_userdata_soak.rs`); the 60 s other venues use would tear \
         down a healthy socket five times per ping (read by \
         `crates/bridges/aster/src/execution/user_data/spot.rs`'s `spec` and \
         `crates/bridges/aster/src/execution/user_data/perp.rs`'s `spec`)",
    )),
    reconnect: Some(user_data_backoff(
        "the user-data pump's doubling backoff with the shared listenKey pump's 30 s ceiling, the \
         same code for both aster markets",
    )),
    session_lifetime: Some(VenueTimingsValue::secs(
        86_400,
        "aster's user data stream (the binance fork's futures v3 rules) documents a 24-hour \
         connection (the connection survey of 2026-11-21); declared only: the listenKey pump of \
         both aster markets meets the cut by its reconnect and replay",
    )),
    ..VenueTimingsLane::NONE
};

/// The market lane BOTH aster markets run, read by the shared market pump through
/// `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`: URL-subscribed like binance's, so no subscribe
/// frame to acknowledge.
///
/// The measurement (the idle-limit audit of 2026-11-22, binance's method): the server pinged every 300.0 s on both
/// hosts, as aster documents (cut without a pong within 15 minutes), and a kline lane heard its bar at least every
/// 60.1 s; but a quiet perp `@aggTrade` socket on `fstream` was cut, with no close frame, 240.0-240.9 s after our last
/// frame, seven times over three sockets, while a quiet spot one on `sstream` ran silent 300 s at a time: the perp
/// host resets a socket its client sent nothing on for 240 s, as on its private stream (`ASTER_EXECUTION`). Sending
/// `LIST_SUBSCRIPTIONS` every 60 s kept a quiet perp and a quiet spot socket up for 600 s, answered every time, the
/// longest inbound gap 60.8 s. The `{"method":"PONG"}` aster's spot page documents as a keepalive kept the perp socket
/// up too but is never answered, so a watchdog could hear only the 300 s ping. No aster market lane runs on the CI box.
const ASTER_MARKET: VenueTimingsLane = VenueTimingsLane {
    keepalive_every: Some(VenueTimingsValue::secs(
        60,
        "aster's documented `LIST_SUBSCRIPTIONS` request (`crates/bridges/aster/src/feed/mod.rs`'s `KEEPALIVE`) \
         every 60 s, a quarter of the 240 s after which aster's perp host resets a market socket its client has \
         sent nothing on (MEASURED in the idle-limit audit of 2026-11-22: seven cuts at 240.0-240.9 s on quiet \
         `@aggTrade` sockets, none while pinged)",
    )),
    idle_threshold: Some(VenueTimingsValue::secs(
        180,
        "three of aster's keepalive answers, inside the 240 s cut; the server's own 300 s ping comes too late to \
         watch (read by `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
    )),
    reconnect: Some(market_pump_redial(
        "the shared market pump's fixed 3 s redial of aster's lanes, the binance family's classic (the aster arm \
         of `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
    )),
    ..VenueTimingsLane::NONE
};

/// Aster's depth-driver knobs: aster runs binance's `depth_main` verbatim.
const ASTER_DEPTH: VenueTimingsDepth = VenueTimingsDepth {
    idle_threshold: VenueTimingsValue::secs(
        30,
        "aster runs binance's depth_main verbatim, so it takes the same 30 s dead-socket watchdog \
         (`crates/bridges/aster/src/feed/mod.rs` calls it)",
    ),
    freshness_threshold: VenueTimingsValue::secs(
        120,
        "measured on binance and carried to aster, whose own liquidity profile is UNVERIFIED \
         against it until its testnet smoke runs: provisional \
         (`crates/bridges/binance/src/family/feed/mod.rs`)",
    ),
    reseed_interval: Some(VenueTimingsValue::secs(
        300,
        "aster, a binance fork, publishes no book checksum either, so it takes binance's 5-minute \
         timed re-snapshot against silent book corruption",
    )),
    backoff: VenueTimingsValue::secs(
        3,
        "binance's resync throttle, carried to aster along with the depth driver it protects",
    ),
};

/// Aster DEX **spot** (`sapi.asterdex.com`). Aster has a row per market because its rate limits
/// split, but both markets run ONE listenKey pump and its depth driver, so this row EQUALS
/// [`ASTER_PERP`] — pinned, so a future divergence is a deliberate edit. `exec` is the same shape as
/// binance perp (no ack, no handshake, a REST renewal, a bare dial); `market` is the one lane both markets run.
pub const ASTER_SPOT: VenueTimings = VenueTimings {
    exec: Some(ASTER_EXECUTION),
    market: Some(ASTER_MARKET),
    depth: Some(ASTER_DEPTH),
};

/// Aster DEX **USDⓈ-M futures** (`fapi.asterdex.com`). See [`ASTER_SPOT`]: the same row.
pub const ASTER_PERP: VenueTimings = VenueTimings {
    exec: Some(ASTER_EXECUTION),
    market: Some(ASTER_MARKET),
    depth: Some(ASTER_DEPTH),
};

/// Bybit V5 (one row for spot and linear perp).
///
/// - `exec` — connect, auth, ack, subscribe, ack, under ONE handshake deadline started once the socket
///   is up; bybit's server does not protocol-ping, so the client sends `{"op":"ping"}` and the reply
///   is what keeps an idle account's socket audible. The dial takes
///   `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `CONNECT_10S` (`dial_timeout` is `None`); there is no
///   REST renewal and no per-reply bound the bridge sets. `history_lag` is the roster's first: bybit's
///   `execution/list` was seen NOT to list a fill 3 s after it (2026-11-20), so the reconnect resync replays twice.
/// - `market` — the kline, mark and trades lanes on the shared market pump: an `{op:"subscribe"}` frame the venue
///   acknowledges, and since connection-sync C6 the private stream's ping behind its silence limit (bybit's depth
///   driver keepalive is the driver's own). MEASURED in the idle-limit audit of 2026-11-22 on the public sockets,
///   600 s per lane: a quiet `publicTrade` socket with no ping was dropped 59.7 s after its last frame (no close
///   frame; bybit documents 10 minutes and asks for a ping every 20 s) and a quiet `kline.1` gapped 56.0 s; pinged,
///   the quiet trades socket ran the whole window, its longest inbound gap 20.7 s, the pong round trip at most
///   0.57 s, so the 60 s limit is about 3x the longest pinged gap. the CI box's gatewayhub logs of 2026-11-20..22:
///   `BTCUSDT.P@1m` and its mark lane reset 3 times each ("Connection reset by peer"), back in about 4 s.
pub const BYBIT: VenueTimings = VenueTimings {
    exec: Some(VenueTimingsLane {
        handshake_timeout: Some(VenueTimingsValue::secs(
            10,
            "one deadline covering the auth ack and the subscribe ack, started once the socket is up; \
             this row is the bound `crates/bridges/bybit/src/execution/user_data.rs` gives its handshake",
        )),
        keepalive_every: Some(VenueTimingsValue::secs(
            20,
            "an app-level {op:ping} every 20 s because bybit's server does not protocol-ping; this \
             row is the cadence `crates/bridges/bybit/src/execution/user_data.rs` sends it at",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            60,
            "3x the ping cadence: bybit answers every ping, so even an idle account delivers a frame \
             every ~20 s; this row is the silence `crates/bridges/bybit/src/execution/user_data.rs` tolerates \
             before it reconnects",
        )),
        reconnect: Some(user_data_backoff(
            "the user-data pump's doubling backoff; bybit passes the 30 s ceiling at both spawn \
             sites of `crates/bridges/bybit/src/execution/user_data.rs`",
        )),
        history_lag: Some(VenueTimingsValue::secs(
            30,
            "MEASURED read-only on bybit#DEMO from what the node had recorded (2026-11-21): a fill at \
             2026-11-20 20:40:00.046, lost to a private-stream reset 82 ms later, was NOT listed by \
             execution/list at the reconnect replay 3.0-4.1 s after it (the reopen at 20:40:02.074, \
             the nudge after the replay at 20:40:04.157) and WAS listed by the reconcile pass folded \
             at 20:40:20.491, at most 20.4 s after it; the tradehub journal's replay rows of 11-19 \
             (12:31-23:07) and 11-21 (10:02-13:52) show 95 fills listed 2.0-10.3 s after them (the \
             gap sentinel replays 5 s after the last command, so a younger listing was rarely there to \
             see). That bounds the lag and gives no tail, so the second replay waits 1.5x the one \
             miss's 20 s bound: it costs two REST reads per reconnect (about six a day on the production node), and \
             a shorter wait repeats the 11-20 miss (read by \
             `crates/bridges/bybit/src/execution/user_data.rs`'s `spawn_perp`)",
        )),
        ..VenueTimingsLane::NONE
    }),
    market: Some(VenueTimingsLane {
        subscribe_acknowledgement_timeout: Some(VenueTimingsValue::secs(
            10,
            "bybit acknowledges an `{op:\"subscribe\"}` frame, so a refused subscription is caught in 10 s rather \
             than waited out (read by `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
        )),
        keepalive_every: Some(VenueTimingsValue::secs(
            20,
            "bybit's own `{op:ping}` (`crates/bridges/bybit/src/feed/tick/mod.rs`'s `KEEPALIVE`) every 20 s, its \
             private stream's cadence and bybit's documented ask (connection-sync C6): pinged, a quiet \
             `publicTrade` socket ran 600 s with its longest gap 20.7 s, unpinged it was cut after 59.7 s",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            60,
            "the private stream's silence limit over the pong every bybit market ping hears, about 3x the longest \
             pinged gap measured on 2026-11-22 (read by \
             `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
        )),
        reconnect: Some(market_pump_redial(
            "the shared market pump's fixed 3 s redial of bybit's kline, mark and trades lanes; the production box's \
             gatewayhub logged the `BTCUSDT.P@1m` lane back about 4 s after each reset of 2026-11-20..22",
        )),
        ..VenueTimingsLane::NONE
    }),
    depth: Some(VenueTimingsDepth {
        idle_threshold: VenueTimingsValue::secs(
            30,
            "no orderbook frame of ANY kind, a delta or the pong to the driver's keepalive, for this \
             long means a silently dead socket; well above the driver's keepalive cadence; this row \
             is the watchdog `crates/bridges/bybit/src/feed/depth.rs` hands the depth driver",
        ),
        freshness_threshold: VenueTimingsValue::secs(
            120,
            "MEASURED 2026-07-11 on the binance, bybit and okx keyless depth feeds: the thinnest \
             actively-traded pairs gapped 44-47 s, and 120 s clears that by ~2.5x; this row is the \
             freshness limit `crates/bridges/bybit/src/feed/depth.rs` hands the depth driver",
        ),
        reseed_interval: Some(VenueTimingsValue::secs(
            300,
            "bybit's orderbook.200 carries no checksum, so a mis-applied delta that keeps the seq \
             chain would corrupt the book silently; ending the session every 5 minutes rebuilds it \
             from a fresh snapshot; this row is the cadence \
             `crates/bridges/bybit/src/feed/depth.rs` hands the depth driver",
        )),
        backoff: VenueTimingsValue::secs(
            3,
            "the reconnect pause after a socket error or a sequence gap, the family's 3 s; this row \
             is the pause `crates/bridges/bybit/src/feed/depth.rs` hands the depth driver",
        ),
    }),
};

/// OKX v5 (one row for spot and SWAP).
///
/// - `exec` — login (epoch SECONDS), ack, subscribe, ack, under ONE handshake deadline; the client
///   sends a raw-text `ping` and okx answers `pong`. The dial carries the client's own user-agent and takes
///   `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `CONNECT_10S` (`dial_timeout` is `None`).
/// - `market` — the kline, mark and trades lanes on the shared market pump: a subscribe frame the venue
///   acknowledges, and the private stream's raw-text ping behind its silence limit (connection-sync C6). MEASURED
///   in the idle-limit audit of 2026-11-22 on the public sockets, 600 s per lane: a quiet instrument's `trades`
///   socket with no ping was closed 30.0 s after its last frame (close 4004 "No data received in 30s.", okx's v5
///   "Connect" rule); pinged every 15 s, half that cut, it ran the whole window, its longest inbound gap 15.5 s and
///   the pong round trip at most 0.33 s, so the 60 s limit is about 4x it. No okx market lane runs on the CI box.
pub const OKX: VenueTimings = VenueTimings {
    exec: Some(VenueTimingsLane {
        handshake_timeout: Some(VenueTimingsValue::secs(
            10,
            "one deadline covering the login ack and the subscribe ack, started once the socket is up; \
             this row is the bound `crates/bridges/okx/src/execution/user_data.rs` gives its handshake",
        )),
        keepalive_every: Some(VenueTimingsValue::secs(
            15,
            "a raw-text ping every 15 s, which okx answers with a pong; this row is the cadence \
             `crates/bridges/okx/src/execution/user_data.rs` sends it at",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            60,
            "4x the 15 s ping: a healthy socket delivers a frame every ~15 s even with no orders; \
             this row is the silence `crates/bridges/okx/src/execution/user_data.rs` tolerates before it \
             reconnects",
        )),
        reconnect: Some(user_data_backoff(
            "the user-data pump's doubling backoff; okx passes the 30 s ceiling at both spawn sites \
             of `crates/bridges/okx/src/execution/user_data.rs`",
        )),
        ..VenueTimingsLane::NONE
    }),
    market: Some(VenueTimingsLane {
        subscribe_acknowledgement_timeout: Some(VenueTimingsValue::secs(
            10,
            "okx acknowledges a subscribe frame, so a refused subscription is caught in 10 s rather than waited out \
             (read by `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
        )),
        keepalive_every: Some(VenueTimingsValue::secs(
            15,
            "okx's raw-text `ping` (`crates/bridges/okx/src/feed/tick/mod.rs`'s `KEEPALIVE`) every 15 s, half the \
             30 s after which okx closes a quiet socket (close 4004 \"No data received in 30s.\", MEASURED \
             2026-11-22); pinged, a quiet `trades` socket's longest gap was 15.5 s",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            60,
            "okx's private stream's silence limit, 4x its market ping, which every pong answers (read by \
             `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
        )),
        reconnect: Some(market_pump_redial(
            "the shared market pump's fixed 3 s redial of okx's kline, mark and trades lanes (read by \
             `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
        )),
        ..VenueTimingsLane::NONE
    }),
    depth: Some(VenueTimingsDepth {
        idle_threshold: VenueTimingsValue::secs(
            30,
            "no books frame of ANY kind, an update or the raw pong to the driver's keepalive, for \
             this long means a silently dead socket; this row is the watchdog \
             `crates/bridges/okx/src/feed/depth.rs` hands the depth driver",
        ),
        freshness_threshold: VenueTimingsValue::secs(
            120,
            "MEASURED 2026-07-11 on the binance, bybit and okx keyless depth feeds: the thinnest \
             actively-traded pairs gapped 44-47 s, and 120 s clears that by ~2.5x; this row is the \
             freshness limit `crates/bridges/okx/src/feed/depth.rs` hands the depth driver",
        ),
        reseed_interval: Some(VenueTimingsValue::secs(
            300,
            "okx fixed its CRC32 book checksum to 0 on 2026-06-23 (869 of 869 frames sampled \
             2026-10-10), so this timed re-seed is the only backstop against silent book corruption; \
             this row is the cadence `crates/bridges/okx/src/feed/depth.rs` hands the depth \
             driver",
        )),
        backoff: VenueTimingsValue::secs(
            3,
            "the reconnect pause after a socket error or a sequence gap, the family's 3 s; this row \
             is the pause `crates/bridges/okx/src/feed/depth.rs` hands the depth driver",
        ),
    }),
};

/// Deribit (options + perp).
///
/// - `exec` — TWO sockets, one lane. The private user-data stream: public/auth and two private
///   subscribes (the stops' optional `user.orders`, then the fills'), all id-matched JSON-RPC acks
///   under ONE handshake deadline; the client's
///   `public/test` ping is the only dependable inbound traffic on an idle account (fills are the
///   sole other source), which is why the idle threshold exists at all. And the ORDER socket, the
///   only JSON-RPC-over-WebSocket order path on the roster, where every request waits up to
///   `response_timeout` for its id-matched reply. The 600 s token refresh rides every 30th ping tick
///   and is NOT modelled here. `dial_timeout` bounds BOTH sockets' dials: the TCP connect and, since
///   0.1.97, each read of the TLS + WebSocket handshake behind it (`connect_websocket`).
/// - `market` — one keyless public JSON-RPC socket per subscription on the shared market pump, a
///   `public/subscribe` frame the venue acknowledges, and the private side's ping and watchdog pair. `depth` — the
///   venue's idle watchdog is its market lane's; the other three knobs are deribit's own copies of the siblings'.
pub const DERIBIT: VenueTimings = VenueTimings {
    exec: Some(VenueTimingsLane {
        dial_timeout: Some(VenueTimingsValue::secs(
            10,
            "the dial bound of both sockets, the TCP connect and each read of the TLS + WebSocket \
             handshake behind it: a stalled dial must not hang the core thread (the order socket) \
             or the fill pump past one reply wait; this row is the bound \
             `crates/bridges/deribit/src/transport/client.rs`'s `connect` and \
             `crates/bridges/deribit/src/execution/user_data.rs`'s `open_deribit_user_data_websocket` hand \
             `connect_ws`",
        )),
        handshake_timeout: Some(VenueTimingsValue::secs(
            10,
            "one deadline covering public/auth and the two private subscribes (the stops' optional \
             `user.orders`, then the fills'), all id-matched; this row \
             is the bound `crates/bridges/deribit/src/execution/user_data.rs` gives its handshake",
        )),
        response_timeout: Some(VenueTimingsValue::secs(
            10,
            "how long the order socket waits for the id-matched reply to each JSON-RPC request, its \
             own auth reply included; after a send the read half is always ambiguous; this row is \
             the wait `crates/bridges/deribit/src/transport/client.rs` gives every request",
        )),
        keepalive_every: Some(VenueTimingsValue::secs(
            20,
            "a public/test ping every 20 s: its reply is the only dependable inbound traffic on an \
             idle account; this row is the cadence `crates/bridges/deribit/src/execution/user_data.rs` sends \
             it at",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            60,
            "3x the 20 s public/test ping, the same pair this row's market lane declares; this row is the silence \
             `crates/bridges/deribit/src/execution/user_data.rs` tolerates before it reconnects",
        )),
        reconnect: Some(user_data_backoff(
            "the user-data pump's doubling backoff; deribit passes the 30 s ceiling at both spawn \
             sites of `crates/bridges/deribit/src/execution/user_data.rs`",
        )),
        ..VenueTimingsLane::NONE
    }),
    market: Some(VenueTimingsLane {
        subscribe_acknowledgement_timeout: Some(VenueTimingsValue::secs(
            10,
            "deribit answers each `public/subscribe` by id, and an EMPTY `result` array (no channel matched) is \
             classified Fatal on the spot, so a silent refusal is caught in 10 s (read by \
             `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
        )),
        keepalive_every: Some(VenueTimingsValue::secs(
            20,
            "a `public/test` ping (`crates/bridges/deribit/src/feed/mod.rs`'s `KEEPALIVE_PING`) every 20 s on each \
             public socket: a quiet instrument's book or quote channel can be silent for minutes, so the reply is \
             the only dependable inbound a dead-socket watchdog can watch",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            60,
            "three of the market sockets' 20 s ping cycles, the pair deribit arms on its private side too; its depth \
             driver takes the same watchdog (read by \
             `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
        )),
        reconnect: Some(market_pump_redial(
            "the shared market pump's fixed 3 s redial of deribit's bar, quote, trade and book sessions (read by \
             `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
        )),
        ..VenueTimingsLane::NONE
    }),
    depth: Some(VenueTimingsDepth {
        idle_threshold: VenueTimingsValue::secs(
            60,
            "three 20 s public/test ping cycles, the same figure this row's market lane declares for deribit's \
             other public sockets; this row is the watchdog `crates/bridges/deribit/src/feed/mod.rs` \
             hands the depth driver",
        ),
        freshness_threshold: VenueTimingsValue::secs(
            120,
            "the siblings' 120 s, which clears a thin pair's worst gap; a far-dated option's book \
             can be silent longer and reads Stale, a disclosure and not a fault; this row is the \
             freshness limit `crates/bridges/deribit/src/feed/mod.rs` hands the depth driver",
        ),
        reseed_interval: Some(VenueTimingsValue::secs(
            300,
            "the siblings' 300 s: deribit's prev_change_id chain already turns a dropped frame into \
             a gap, so this is parity, and a refresh costs one snapshot (46 KB for BTC-PERPETUAL, \
             MEASURED 2026-10-04); this row is the cadence \
             `crates/bridges/deribit/src/feed/mod.rs` hands the depth driver",
        )),
        backoff: VenueTimingsValue::secs(
            3,
            "the siblings' 3 s, the figure deribit's pump row declares for its other lanes; this \
             row is the pause `crates/bridges/deribit/src/feed/mod.rs` hands the depth driver",
        ),
    }),
};

/// Polymarket CLOB.
///
/// - `exec` — the user channel: connect, send ONE auth-bearing subscribe, and it streams; the venue
///   sends no ack, so `handshake_timeout` is `None`. The dial takes
///   `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `CONNECT_10S` (`dial_timeout` is `None`), through
///   the SOCKS tunnel too (`crates/vike-transport/src/websocket_proxy.rs`'s `connect_websocket`). On this
///   venue EVERY post-acceptance terminal arrives only on this channel, so a stalled socket strands
///   orders outright.
/// - `market` — the book, quote and trade lanes on the shared market pump: a subscribe frame the CLOB does not
///   acknowledge (data freshness covers a dead subscribe), a text keepalive, and the one growing market redial.
///   `depth` — `None`: polymarket runs no depth driver.
pub const POLYMARKET: VenueTimings = VenueTimings {
    exec: Some(VenueTimingsLane {
        keepalive_every: Some(VenueTimingsValue::secs(
            10,
            "a text PING every 10 s, the CLOB WS idle keepalive; the user-data pump reads this row \
             at its call in `crates/bridges/polymarket/src/execution/user_data/mod.rs`",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            60,
            "6x the 10 s ping: the CLOB WS answers the idle keepalive, so a healthy socket delivers \
             a frame every ~10 s even on an idle account, and the user channel carries every \
             post-acceptance terminal, so a stalled socket strands orders; the pump reads this row \
             at its call in `crates/bridges/polymarket/src/execution/user_data/mod.rs`",
        )),
        reconnect: Some(user_data_backoff(
            "the user-data pump's doubling backoff; polymarket passes the pump's \
             30 s ceiling at its call in \
             `crates/bridges/polymarket/src/execution/user_data/mod.rs`",
        )),
        submit: Some(VenueTimingsSubmit {
            write_by: Duration::from_secs(25),
            calls: &[
                VenueTimingsSubmitCall::new("the order POST", 1, Duration::from_secs(30)),
                VenueTimingsSubmitCall::new(
                    "the order read after a POST of unknown outcome",
                    1,
                    Duration::from_secs(4),
                ),
            ],
            hold: None,
            why: "the exec thread serves its queue one order at a time, a batch included, and each order is refused \
                  unsigned once it has waited 25 s since it reached the client, asked at dequeue and again after its \
                  reads; then the one POST on the 30 s order agent and, when its answer is lost, one 4 s read of the \
                  order (`crates/bridges/polymarket/src/execution/mod.rs`'s `MAX_SUBMIT_AGE_MS`, pinned to this row)",
        }),
        ..VenueTimingsLane::NONE
    }),
    market: Some(VenueTimingsLane {
        keepalive_every: Some(VenueTimingsValue::secs(
            10,
            "the market channel's text `PING` (`crates/bridges/polymarket/src/feed/mod.rs`'s `PING_PAYLOAD`) every \
             10 s: the CLOB reaps a quiet market socket",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            30,
            "three of the market channel's 10 s pings, each answered, before a silent socket is cut (read by \
             `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
        )),
        reconnect: Some(VenueTimingsReconnect::new(
            Duration::from_millis(500),
            Duration::from_secs(30),
            "the one growing market redial on the roster, 500 ms doubling to a 30 s ceiling and reset by a \
             successful connect (read by `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
        )),
        ..VenueTimingsLane::NONE
    }),
    depth: None,
};

/// Hyperliquid.
///
/// - `exec` — `userFills` and `orderUpdates` on one address-scoped socket: connect, send the two
///   subscribe frames, and it streams, with no ack and no auth handshake (`handshake_timeout` is
///   `None`). The dial takes `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `CONNECT_10S` (`dial_timeout`
///   is `None`).
/// - `market` — the candle, bbo, trades and l2Book sessions on the shared market pump, a subscribe frame per
///   session with no acknowledgement watched, and this lane's 30 s ping constant. `depth` — `None`: no depth driver.
pub const HYPERLIQUID: VenueTimings = VenueTimings {
    exec: Some(VenueTimingsLane {
        keepalive_every: Some(VenueTimingsValue::secs(
            30,
            "an app-level {method:ping} at least every 30 s because the server drops a connection \
             idle for 60 s; this row's market lane states the same cadence, and a test \
             in the bridge holds it equal to \
             `crates/bridges/hyperliquid/src/transport/consts.rs`'s `WEBSOCKET_PING_SECS`",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            120,
            "4x the 30 s ping: hyperliquid answers each ping with a pong, so a healthy socket \
             delivers a frame every ~30 s even on an idle account; the user-data pump reads this \
             row at its call in `crates/bridges/hyperliquid/src/execution/user_data.rs`, and the live \
             soak `crates/bridges/hyperliquid/tests/hyperliquid_userdata_soak.rs` measures the \
             headroom under it",
        )),
        reconnect: Some(user_data_backoff(
            "the user-data pump's doubling backoff; hyperliquid's pump runs to its 30 s \
             ceiling (`vike_bridge_core::websocket_session::user`'s `spawn_user`, \
             called in `crates/bridges/hyperliquid/src/execution/user_data.rs`)",
        )),
        ..VenueTimingsLane::NONE
    }),
    market: Some(VenueTimingsLane {
        keepalive_every: Some(VenueTimingsValue::secs(
            30,
            "the market sessions' `{method:ping}` at the cadence the private stream pings at \
             (`crates/bridges/hyperliquid/src/transport/consts.rs`'s `WEBSOCKET_PING_SECS`): hyperliquid drops a \
             connection idle past 60 s",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            60,
            "two of the market sessions' 30 s pings, hyperliquid's own drop time for an idle connection (read \
             by `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
        )),
        reconnect: Some(market_pump_redial(
            "the shared market pump's fixed 3 s redial of hyperliquid's candle, bbo, trades and l2Book sessions (read \
             by `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
        )),
        ..VenueTimingsLane::NONE
    }),
    depth: None,
};

/// OANDA v20 (FX/CFD). Both lanes are CHUNKED HTTP streams, not WebSockets, on a hand-built `ureq`
/// agent (a stream cannot live under the shared agent's global request timeout), so the bounds are
/// the agent's: `timeout_connect` is the dial, `timeout_recv_response` the wait for the response
/// head, `timeout_per_read` the idle bound on each read wait. The server's ~5 s HEARTBEAT is its
/// own, so `keepalive_every` is `None`.
///
/// ⚠ `response_timeout` is NOT an alias of `dial_timeout`: both are 10 s today (the bridge once passed
/// one constant to both builder calls), and they are different waits, each read from its own field.
///
/// - `exec` — the transactions stream; a fixed 1 s between opens.
/// - `market` — the pricing stream; a fixed 3 s (the shared market pumps' classic). The lane rides the shared
///   market pump, whose row reads its dial, idle bound and redial from here
///   (`vike_bridge_core::websocket_session::spec::row`, as for every pump lane).
pub const OANDA: VenueTimings = VenueTimings {
    exec: Some(VenueTimingsLane {
        dial_timeout: Some(VenueTimingsValue::secs(
            10,
            "bounded so a black-holed route cannot hold the exec event lane for the OS's SYN ladder; \
             the transactions agent's `timeout_connect` (`crates/bridges/oanda/src/execution/user_data.rs`'s \
             `transactions_agent`)",
        )),
        response_timeout: Some(VenueTimingsValue::secs(
            10,
            "the wait for the transactions stream's response head, the same agent's \
             `timeout_recv_response`; a different wait from the dial though both read 10 s today \
             (`crates/bridges/oanda/src/execution/user_data.rs`'s `transactions_agent`)",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            20,
            "4 missed ~5 s HEARTBEATs on the transactions stream; the same agent's \
             `timeout_per_read` bounds each read wait \
             (`crates/bridges/oanda/src/execution/user_data.rs`'s `transactions_agent`)",
        )),
        reconnect: Some(VenueTimingsReconnect::fixed(
            Duration::from_secs(1),
            "a fixed stop-aware 1 s between opens, the shared pump's ladder from this row \
             (`crates/bridges/oanda/src/execution/user_data.rs`'s `transactions_backoff`); terminal opens are \
             bounded and a 401 past that bound slowed there (the ladder's refusal wait), neither a row",
        )),
        ..VenueTimingsLane::NONE
    }),
    market: Some(VenueTimingsLane {
        dial_timeout: Some(VenueTimingsValue::secs(
            10,
            "the same bounded dial on the pricing stream, so a black-holed route cannot pin a feed \
             thread past its stop flag (`crates/bridges/oanda/src/feed/tick.rs`'s \
             `streaming_agent`)",
        )),
        response_timeout: Some(VenueTimingsValue::secs(
            10,
            "the wait for the pricing stream's response head, the streaming agent's \
             `timeout_recv_response`; a different wait from the dial though both read 10 s today \
             (`crates/bridges/oanda/src/feed/tick.rs`'s `streaming_agent`)",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            20,
            "4 missed ~5 s HEARTBEATs on the pricing stream, the same value as the transactions \
             stream's: the agent's `timeout_per_read` (`crates/bridges/oanda/src/feed/tick.rs`'s \
             `streaming_agent`), whose first timed-out read trips the shared market pump's idle \
             watchdog at this same bound",
        )),
        reconnect: Some(VenueTimingsReconnect::fixed(
            Duration::from_secs(3),
            "the shared market pumps' classic fixed 3 s, the market pump's ladder after a failed \
             open or an ended stream, and waited again by the bar poll's retry after a failed candle \
             fetch (`crates/bridges/oanda/src/feed/bar/mod.rs`'s `POLL_RETRY`)",
        )),
        ..VenueTimingsLane::NONE
    }),
    depth: None,
};

/// IG (FX/CFD).
///
/// - `exec` — the Lightstreamer trade stream: chunked HTTP (`create_session`, then a control POST,
///   then a streaming body whose `PROBE` lines are the server's keepalive), not a WebSocket. The dial
///   takes the shared 10 s window (owner ruling B-13); no handshake bound is set (`handshake_timeout`
///   is `None`); the agent's `timeout_per_read` is the idle bound.
/// - `market` — the TLCP market feed on the shared market pump: the `create_session` -> `CONOK` budget inside the
///   connect closure (its handshake), then the `SUBOK` watchdog, the silence limit over the server's `PROBE` and the
///   redial.
pub const IG: VenueTimings = VenueTimings {
    exec: Some(VenueTimingsLane {
        dial_timeout: Some(VenueTimingsValue::secs(
            10,
            "IG states no connect window, so the trade stream's dial takes the one a venue that states \
             none takes (`crates/vike-bridge-core/src/websocket_session/spec.rs`'s `CONNECT_10S`; owner ruling B-13): \
             an unreachable host no longer holds the thread for the OS connect timeout \
             (`crates/bridges/ig/src/execution/user_data.rs`'s `trade_stream_agent`)",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            30,
            "`timeout_per_read` on the Lightstreamer trade-stream agent bounds each read wait of the \
             chunked stream, whose server PROBE lines are its keepalive \
             (`crates/bridges/ig/src/execution/user_data.rs`'s `trade_stream_agent`)",
        )),
        reconnect: Some(VenueTimingsReconnect::new(
            Duration::from_secs(1),
            Duration::from_secs(60),
            "step 0 of the retry plan is 1 s and each further failed dial doubles it to a 60 s \
             ceiling, because neither a rejected account nor an unreachable gateway heals in the \
             next second and both recover eventually (the shared `ReconnectBackoff` ladder, held by \
             `crates/bridges/ig/src/execution/user_data/retry.rs`'s `RetryState`)",
        )),
        submit: Some(VenueTimingsSubmit {
            write_by: Duration::from_secs(20),
            calls: &[
                VenueTimingsSubmitCall::new("the order POST", 1, Duration::from_secs(30)),
                VenueTimingsSubmitCall::new(
                    "a GET of the deal's confirm",
                    7,
                    Duration::from_secs(5),
                ),
                VenueTimingsSubmitCall::new(
                    "the pause between two confirms",
                    6,
                    Duration::from_millis(400),
                ),
            ],
            hold: Some(Duration::from_secs(35)),
            why: "the shared exec queue's 20 s, which the epic read in front of the POST counts against, then the \
                  order POST on the session's 30 s agent and the confirm ladder (seven GETs on a 5 s agent, 400 ms \
                  apart: IG answers a fresh reference 404 for a moment), asked only while the next confirm still \
                  ends inside 35 s from the POST; a 401's re-login inside a call is not counted \
                  (`crates/bridges/ig/src/execution/mod.rs`'s `SUBMIT_HOLD` pins this row to the code)",
        }),
        ..VenueTimingsLane::NONE
    }),
    market: Some(VenueTimingsLane {
        handshake_timeout: Some(VenueTimingsValue::secs(
            15,
            "wall-clock budget for create_session to CONOK inside the connect closure, which the \
             pump's replayed-subscribe-frame shape cannot express; the CONOK deadline of \
             `crates/bridges/ig/src/feed/mod.rs`'s `open_ls_session`",
        )),
        subscribe_acknowledgement_timeout: Some(VenueTimingsValue::secs(
            10,
            "IG answers a TLCP subscribe with SUBOK, so a silently refused subscription is caught in 10 s rather \
             than waited out; a wait after the CONOK handshake (read by \
             `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            60,
            "the load-bearing ig knob: a MERGE quote item on a CLOSED market sends nothing for hours while the \
             server's TLCP `PROBE` keeps arriving, so only a socket silent of EVERY frame kind trips it (the ig arm \
             of `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
        )),
        reconnect: Some(market_pump_redial(
            "the shared market pump's fixed 3 s redial of ig's TLCP market lanes (read by \
             `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
        )),
        ..VenueTimingsLane::NONE
    }),
    depth: None,
};

/// FXCM ForexConnect (FX/CFD). Execution rides a native C++ SDK loaded at runtime, which owns its own
/// transport, sockets and reconnects; vike sets no connection or liveness bound of its own.
///
/// - `exec` — only the `probation` of the gatewayhub's rebuild of a session the SDK gave up on (the owner's
///   ruling of 2026-11-22, "ok 30s"); the rebuild's ladder is the bridge's login ladder, not a row.
/// - `market` — `None`: the price lane rides the native session (it polls the Offers table on the running
///   reconcile session, `crate::venue_connection::FXCM`'s market row), so it waits nothing of its own.
///
/// No `idle_threshold`, on purpose (the idle-limit audit of 2026-11-22): the session is `Supervised`
/// (`crate::venue_connection::FXCM`), so its liveness is the SDK's and the gatewayhub's probe watches it, and
/// ForexConnect states no heartbeat cadence a bridge could time. MEASURED at the FX open of 2026-11-22 (the CI box,
/// 21:00-23:02 UTC, the `link gap summary` lines of `vike_bridge_core::link_gap`): 119 summaries and 0 events (the
/// bridge's events are the ORDER envelopes off the shim's queue, and nothing else reaches Rust), 2 SDK status
/// changes, both `none` to `Connected`, and no Reconnecting, SessionLost or Disconnected. No frame gap reaches Rust,
/// so there is nothing a bridge could time: it stays without one, on purpose.
pub const FXCM: VenueTimings = VenueTimings {
    exec: Some(VenueTimingsLane {
        probation: Some(VenueTimingsValue::secs(
            30,
            "the owner's ruling of 2026-11-22 (\"ok 30s\"): a rebuilt ForexConnect session is healthy, and the \
             gatewayhub's backoff starts over, only once it was seen Connected for this long without a break; a \
             server that holds each login for one probe and drops it climbs the ladder instead of a re-login \
             every few seconds; read by `crates/bridges/fxcm/src/account/venue_link.rs`'s `PROBATION`",
        )),
        ..VenueTimingsLane::NONE
    }),
    market: None,
    depth: None,
};

/// Dukascopy (FX).
///
/// - `exec` — the JForex Java sidecar over stdio, not a socket: how long the mount waits for the
///   sidecar to report ready, and how long the client waits before it starts another one after the
///   sidecar died. JForex's JNLP login downloads platform configuration on a cold cache (observed
///   over 60 s) and the sidecar polls its session for up to 240 s, so the handshake is bounded just
///   above that. A sidecar that dies after `ready` is RESTARTED by the client's supervisor
///   (`crates/bridges/dukascopy/src/execution/mod.rs`'s `supervise`); the first START that fails is still a
///   paper fallback, never a redial. The sidecar writes no heartbeat, so its liveness is the BRIDGE's own probe
///   (`keepalive_every`) and idle limit (`idle_threshold`), MEASURED at the FX open of 2026-11-22 (the CI box, 21:00-23:02
///   UTC, the `link gap summary` lines of `vike_bridge_core::link_gap`): 119 summaries, none without an event, 369
///   events, about 3 a minute in one burst about every 60 s, which are the sidecar's answers to the node's reconcile
///   pass (orders, fills and positions: three `recon` questions a pass, every `config.reconcile_interval_ms`, 60 s by
///   default); the longest gap 69.15 s, just after a login (live window 22:20-23:02: 60.875 s, per-minute p50 60.0 s,
///   p90 60.4 s); no sidecar restart. ⚠ A JForex session that ends ends the sidecar too (`fatal`, then exit), which the
///   supervisor already restarts, so the limit detects a WEDGED sidecar (its JVM, strategy thread or stdio), and only
///   for a jar that announces `recon`.
/// - `market` — `None`: the price lane rides the native session (the exec sidecar streams the ticks,
///   `crate::venue_connection::DUKASCOPY`'s market row), so it waits nothing of its own; history is keyless `.bi5`
///   files (T+1).
pub const DUKASCOPY: VenueTimings = VenueTimings {
    exec: Some(VenueTimingsLane {
        handshake_timeout: Some(VenueTimingsValue::secs(
            300,
            "JForex login downloads platform config on a cold cache (observed over 60 s) and the \
             sidecar polls its session for up to 240 s, so the handshake is bounded just above it \
             (`crates/bridges/dukascopy/src/execution/mod.rs`, READY_TIMEOUT)",
        )),
        keepalive_every: Some(VenueTimingsValue::secs(
            60,
            "the supervisor asks a living sidecar the `recon` question itself once it has written nothing for this \
             long, under an id no reconcile asker is given (`crates/bridges/dukascopy/src/reconcile/query.rs`'s \
             `PROBE_ID`): the node's own pass asks three times every 60 s (measured at the FX open of 2026-11-22), \
             but `flags.reconcile_off` stops it and `config.reconcile_interval_ms` stretches it, and a quiet \
             account's sidecar writes nothing unasked; read by `crates/bridges/dukascopy/src/execution/mod.rs`'s \
             `LIVENESS`",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            210,
            "3 x the longest healthy silence measured at the FX open of 2026-11-22 (69.15 s, just after a login; \
             the live window's 60.9 s gives 183 s), and over 2 x the 60 s probe: a sidecar that wrote no line for \
             this long, counted from its `ready`, is WEDGED (its JVM, strategy thread or stdio; a JForex session \
             that ends ends the sidecar through `fatal`) and is killed and started again on the reconnect ladder; \
             read by `crates/bridges/dukascopy/src/execution/mod.rs`'s `LIVENESS`",
        )),
        reconnect: Some(VenueTimingsReconnect::new(
            Duration::from_secs(5),
            Duration::from_secs(60),
            "the first restart of a dead sidecar waits 5 s and each failed one doubles the wait to a \
             60 s ceiling: JForex ends a session at a server shutdown and may refuse a login for a \
             while after it, a JVM start is heavy and a rejected login must not be hammered, yet a \
             dead order path must come back by itself within a minute of the venue allowing it; a \
             Dukascopy login is ONE JForex session, so the next sidecar starts only after the dead \
             one is reaped (`crates/bridges/dukascopy/src/execution/mod.rs`'s `supervise` and `RESTART`)",
        )),
        ..VenueTimingsLane::NONE
    }),
    market: None,
    depth: None,
};

/// Interactive Brokers on its default product, the socket backend (IB Gateway over `ibapi`, decision 0140): the
/// product `crate::venue_connection::IBKR` describes. The `ibapi` client owns the socket and its liveness and the
/// bridge never dials it again (`IbkrInbound::StreamDead`), so no keepalive, idle limit or dial bound is declared;
/// what this product waits is the bridge's link watchdog and the gatewayhub's rebuild of the adapter. The Client
/// Portal backend is [`IBKR_CPAPI`], read by name.
///
/// - `exec` — the link watchdog's `lost_after` and `probation`, and the rebuild's ladder (`reconnect`).
/// - `market` — `None`: the socket's quotes and bars ride the same `ibapi` session.
pub const IBKR: VenueTimings = VenueTimings {
    exec: Some(VenueTimingsLane {
        // The 94 s nightly window was measured on the CI box; the literal names the role, never the box.
        lost_after: Some(VenueTimingsValue::secs(
            300,
            "the IB Gateway link watchdog: a counted part (server link, socket, snapshot) broken this \
             long calls the link lost; 90 s was inside the normal range (a nightly 1100-to-1102 \
             window ran 94 s and healed by itself, the production box 2026-10-17); read by \
             `crates/bridges/vike-ibkr/src/transport/link_watch.rs`'s `LOST_AFTER`",
        )),
        probation: Some(VenueTimingsValue::secs(
            600,
            "the IB Gateway gate forgets an outage only after a session ran this long with nothing \
             counted broken: longer than lost_after, or a session would be forgiven just before it \
             could be judged; read by `crates/bridges/vike-ibkr/src/transport/link_watch.rs`'s \
             `PROBATION`",
        )),
        reconnect: Some(VenueTimingsReconnect::new(
            Duration::from_secs(2),
            Duration::from_secs(60),
            "the gatewayhub's rebuild of the socket adapter once its gateway is lost: the first failed \
             restore waits 2 s and each further one doubles the wait to a 60 s ceiling, the cadence the \
             bridge's probe hands the supervisor (`crates/bridges/vike-ibkr/src/gateway_watch.rs`'s \
             `IbkrGateway`, its `first_wait` and `MAX_WAIT`); the 2 s was read from the Client Portal \
             row until the two products split",
        )),
        ..VenueTimingsLane::NONE
    }),
    market: None,
    depth: None,
};

/// Interactive Brokers on the Client Portal Gateway backend (`crate::IbkrBackend::Cpapi`), not deployed: the
/// product `crate::venue_connection::IBKR_CPAPI` describes, read by name ([`timings_for`] answers [`IBKR`]).
///
/// - `exec` — the cpapi order-update socket: a `tic` frame and a REST `tickle` every 30 s keep the
///   gateway session alive and neither is answered, so the idle limit counts the gateway's own 10 s
///   heartbeat; a failed dial waits a fixed 2 s. (A drop mid-stream waits 1 s first, which is not
///   modelled.) The `tic` and the idle limit come from IBKR's documentation, not a capture.
/// - `market` — the cpapi market-data lane on the shared market pump: the same tickle, the same 60 s idle limit over
///   the same documented heartbeat (every connected socket hears it), and a fixed 2 s redial. Its pump row
///   (`vike_bridge_core::websocket_session::spec::IBKR_CPAPI`) reads the idle limit and the redial from here.
pub const IBKR_CPAPI: VenueTimings = VenueTimings {
    exec: Some(VenueTimingsLane {
        session_renew_every: Some(VenueTimingsValue::secs(
            30,
            "the cpapi backend's REST tickle keeps the gateway session alive and produces no inbound \
             frame (`pump_loop` in `crates/bridges/vike-ibkr/src/transport/cpapi/mod.rs`)",
        )),
        keepalive_every: Some(VenueTimingsValue::secs(
            30,
            "the `tic` frame on the Client Portal socket: IBKR's Web API documents it as the socket's \
             session ping, to be sent at least once per minute and answered with nothing \
             (https://www.interactivebrokers.com/docs/web-api/v1/ws/session/maintain-session-ping, read \
             2026-11-22); twice a minute, and the REST tickle rides the same hook \
             (`crates/bridges/vike-ibkr/src/transport/cpapi/mod.rs`'s `TIC_EVERY`)",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            60,
            "six of the `system` heartbeats IBKR's Web API documents the Client Portal socket sending every \
             10 s while it stays connected (https://www.interactivebrokers.com/docs/web-api/ws/system-messages, \
             read 2026-11-22): the `tic` is not answered, so on a quiet account they are the only frames; \
             from the documentation, as cpapi is not deployed and no production log holds a session of it \
             (`crates/bridges/vike-ibkr/src/transport/cpapi/mod.rs`'s `IDLE_LIMIT`)",
        )),
        reconnect: Some(VenueTimingsReconnect::fixed(
            Duration::from_secs(2),
            "the cpapi pump's wait after a failed websocket dial \
             (`crates/bridges/vike-ibkr/src/transport/cpapi/mod.rs`'s `redial`, `pump_loop`'s ladder)",
        )),
        session_lifetime: Some(VenueTimingsValue::secs(
            86_400,
            "the Client Portal Gateway's brokerage session lasts at most 24 hours before it must be \
             authenticated again (the connection survey of 2026-11-21); declared only: the cpapi \
             backend re-authenticates nothing today and is not deployed",
        )),
        submit: Some(VenueTimingsSubmit {
            write_by: Duration::from_secs(20),
            calls: &[
                VenueTimingsSubmitCall::new("the contract-id search", 1, Duration::from_secs(30)),
                VenueTimingsSubmitCall::new("the order POST", 1, Duration::from_secs(30)),
                VenueTimingsSubmitCall::new("a confirmation reply", 8, Duration::from_secs(30)),
            ],
            hold: Some(Duration::from_secs(35)),
            why: "the shared exec queue's 20 s, then the Client Portal place: a contract-id search for an unknown \
                  conId, the order POST and up to eight confirmation replies (the gateway asks its questions one at \
                  a time), each on the REST agent's 30 s, every call made only if it still ends inside 35 s from the \
                  top of the place, else the order is rejected unsent \
                  (`crates/bridges/vike-ibkr/src/transport/cpapi/mod.rs`'s `PLACE_HOLD` pins this row to the code)",
        }),
        ..VenueTimingsLane::NONE
    }),
    market: Some(VenueTimingsLane {
        session_renew_every: Some(VenueTimingsValue::secs(
            30,
            "the same tickle on the cpapi market-data lane, checked before each read of an open session \
             (`crates/bridges/vike-ibkr/src/feed/cpapi/lane.rs`'s `TICKLE_EVERY`)",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            60,
            "six of the `system` heartbeats IBKR's Web API documents a Client Portal socket receiving every 10 s \
             while it stays connected (https://www.interactivebrokers.com/docs/web-api/ws/system-messages, read \
             2026-11-22), the exec lane's value: this socket is the same gateway's `/v1/api/ws`, so a quiet book \
             hears the same heartbeat. The lane sends no `tic`, the session ping IBKR advises at least once a \
             minute, which that documentation says still needs the REST `/tickle` beside it \
             (https://www.interactivebrokers.com/docs/web-api/ws/ping, read 2026-11-22): the lane's tickle every \
             30 s keeps the session, as before. From the documentation, as cpapi is not deployed and no production \
             log holds a session of it (`crates/vike-bridge-core/src/websocket_session/spec.rs`'s `IBKR_CPAPI`)",
        )),
        reconnect: Some(VenueTimingsReconnect::fixed(
            Duration::from_secs(2),
            "the wait after every failed cpapi market-data dial and every ended session, as the lane's own loop \
             waited it (`crates/bridges/vike-ibkr/src/feed/cpapi/lane.rs`'s `run_on`, on the market pump's \
             ladder through `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `IBKR_CPAPI`)",
        )),
        ..VenueTimingsLane::NONE
    }),
    depth: None,
};

/// cTrader (FX/CFD). ONE protobuf-over-TCP session actor serves BOTH execution and market data, so
/// this row's `exec` lane is the whole connection and `market` is `None`.
///
/// - `response_timeout` — the handshake waits for EACH expected response in turn, 15 s apiece: a
///   per-reply wait and not one deadline over the whole handshake, so it is declared here and
///   `handshake_timeout` stays `None`.
/// - `idle_threshold` — the actor's stall watchdog: a socket the venue has ANSWERED nothing on for
///   three heartbeats is called dead and redialled. Only an answer counts, never a frame the venue
///   sends unasked (a blackholed way out still lets those in), so the actor asks a question at two
///   thirds of it to give a live socket something to answer.
/// - The dial of an ask with no ceiling (the actor's session and its reconnects) takes the shared 10 s
///   window per address (owner ruling B-13); the initial connect's own retry policy (`ConnectRetry`)
///   and the minute's account-session probe (`AUTH_PROBE_EVERY`, a cadence of a request about the
///   account, not the socket) are not modelled.
pub const CTRADER: VenueTimings = VenueTimings {
    exec: Some(VenueTimingsLane {
        dial_timeout: Some(VenueTimingsValue::secs(
            10,
            "cTrader states no connect window, so the actor's dial takes the one a venue that states \
             none takes (`crates/vike-bridge-core/src/websocket_session/spec.rs`'s `CONNECT_10S`; owner ruling \
             B-13), one window over the lookup and every address \
             (`crates/vike-transport/src/dial.rs`'s `dial_bounded`): it used to be the OS connect timeout, about two minutes \
             (`crates/bridges/ctrader/src/transport/connection/handshake.rs`'s `DIAL_WINDOW`)",
        )),
        response_timeout: Some(VenueTimingsValue::secs(
            15,
            "how long the handshake waits for EACH expected response before giving up, a per-reply \
             wait started afresh on every call; read by `read_until` in \
             `crates/bridges/ctrader/src/transport/connection/mod.rs`, which the recon client's requests reuse",
        )),
        keepalive_every: Some(VenueTimingsValue::secs(
            10,
            "a HEARTBEAT_EVENT every 10 s because cTrader disconnects idle sockets; the recon \
             client revives its own heartbeat-less connection at the same age; the actor loop \
             reads this row through `crates/bridges/ctrader/src/transport/connection/mod.rs`'s `HEARTBEAT_INTERVAL`",
        )),
        // The 45 s cut and the lapsed token below were both seen on the CI box; the literals name the role.
        idle_threshold: Some(VenueTimingsValue::secs(
            30,
            "three of the venue's heartbeats with no ANSWER on the socket: a blackholed socket \
             reads nothing while it accepts writes, and a cut of the outgoing packets alone kept \
             the exec socket RUNNING for 45 s on the production box (2026-11-09) while unasked \
             frames still came in; read by `crates/bridges/ctrader/src/transport/connection/mod.rs`'s `READ_IDLE_AFTER`",
        )),
        auth_probe_every: Some(VenueTimingsValue::secs(
            60,
            "a ProtoOATraderReq a minute: an access token lapsed with nothing logged and the socket \
             stayed up (the production box, 2026-11-04 to 11-06), found only by a far order two days later, and an \
             ended session refuses this request; a minute is the reconcile's cadence and 1/3000 of the \
             vendor's 50 requests a second; read by `crates/bridges/ctrader/src/transport/connection/mod.rs`'s \
             `AUTH_PROBE_EVERY`",
        )),
        reconnect: Some(VenueTimingsReconnect::new(
            Duration::from_secs(1),
            Duration::from_secs(5),
            "the shared user-data pump's 1 s first wait (owner ruling B-13: the 200 ms start served \
             an in-process test, not the venue) doubling to a 5 s ceiling, unbounded in attempts, long \
             enough not to hammer a briefly down venue (read by \
             `crates/bridges/ctrader/src/transport/connection/session.rs`'s `RECONNECT_WAIT`)",
        )),
        ..VenueTimingsLane::NONE
    }),
    market: None,
    depth: None,
};

/// Alpaca (US equities + crypto).
///
/// - `exec` — the SSE trade-events stream (`/v2/events/trades`) over `ureq`, on the shared private-stream pump: the
///   dial is bounded because `ureq` has no connect timeout by default and an unreachable host would otherwise pin the
///   thread (and the daemon's stop) for the OS connect timeout; the body bound is the read-idle bound, and the pump's
///   idle watchdog. Every wait is a fixed 2 s on the pump's reconnect ladder (owner ruling B-13): a failed open, a
///   refused (401) one and a drop mid-stream alike.
/// - `market` — the data WebSocket's lanes on the shared market pump, every lane of an asset class on that stream's
///   one socket, which writes the `subscribe` frames itself (so no acknowledgement is watched) and answers the
///   server's ping (so no client keepalive).
pub const ALPACA: VenueTimings = VenueTimings {
    exec: Some(VenueTimingsLane {
        dial_timeout: Some(VenueTimingsValue::secs(
            5,
            "about an order of magnitude over a healthy connect, short enough that a stop is seen \
             within seconds on an unreachable host, which `ExecutionClientActor::stop` joins with no timeout \
             (`crates/bridges/alpaca/src/execution/user_data.rs`'s `CONNECT_TIMEOUT`)",
        )),
        idle_threshold: Some(VenueTimingsValue::secs(
            30,
            "`timeout_per_read` on the SSE agent, the read-idle bound of a long-lived stream, \
             deliberately not a global timeout (`crates/bridges/alpaca/src/execution/user_data.rs`'s `sse_agent`), \
             and the shared pump's idle watchdog, which the first read that waits it out trips",
        )),
        reconnect: Some(VenueTimingsReconnect::fixed(
            Duration::from_secs(2),
            "the wait after a token mint failure, a refused (401) open, a non-2xx answer, a failed \
             connect or a stream that ended: the shared pump's `ReconnectBackoff` over this row \
             (`crates/bridges/alpaca/src/execution/user_data.rs`'s `spawn_trade_events`)",
        )),
        submit: Some(VenueTimingsSubmit {
            write_by: Duration::from_secs(20),
            calls: &[
                VenueTimingsSubmitCall::new("the order POST", 1, Duration::from_secs(30)),
                VenueTimingsSubmitCall::new(
                    "the re-mint after a 401 on it",
                    1,
                    Duration::from_secs(5),
                ),
            ],
            hold: None,
            why: "the shared exec queue's 20 s, which the token mint in front of the POST now counts against (a \
                  wait on the stream's mint of the same token included), then the order POST on the transport's 30 s \
                  agent and, on a 401, one re-mint on the 5 s mint agent, never a second POST \
                  (`crates/bridges/alpaca/src/transport/sign.rs`'s `MINT_TIMEOUT`, pinned to this row beside the \
                  submit)",
        }),
        ..VenueTimingsLane::NONE
    }),
    market: Some(VenueTimingsLane {
        idle_threshold: Some(VenueTimingsValue::secs(
            180,
            "alpaca-py's own dead-socket bound (`alpaca/data/live/websocket.py`'s `_websocket_params`: \
             `ping_timeout: 180`), judged on the data SOCKET's liveness, which the server's ping keeps audible, so a \
             quiet symbol on a live socket never trips it (read by \
             `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
        )),
        reconnect: Some(market_pump_redial(
            "the shared market pump's fixed 3 s redial of alpaca's data lanes, the market lane's classic, unchanged \
             (read by `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`)",
        )),
        ..VenueTimingsLane::NONE
    }),
    depth: None,
};

// vike:new-venue:row /// TODO(new-venue: {venue}): read the venue's CONNECTION and LIVENESS timings from its bridge.
// vike:new-venue:row /// `NOT_DECLARED` is the honest scaffolded value, and a real classification only because it is
// vike:new-venue:row /// NAMED (`undeclared_roster_venues_are_named_rows_not_fall_through`).
// vike:new-venue:row pub const {VENUE}: VenueTimings = VenueTimings::NOT_DECLARED;
// vike:new-venue:row

// ---------------------------------------------------------------------------------------------
// COMPILE-TIME invariants, not tests: a row with a zero duration, an inverted reconnect or an idle
// threshold under two keepalive periods fails the BUILD.
// ---------------------------------------------------------------------------------------------
const _: () = assert!(BINANCE_SPOT.is_sane());
const _: () = assert!(BINANCE_PERP.is_sane());
const _: () = assert!(ASTER_SPOT.is_sane());
const _: () = assert!(ASTER_PERP.is_sane());
const _: () = assert!(BYBIT.is_sane());
const _: () = assert!(OKX.is_sane());
const _: () = assert!(DERIBIT.is_sane());
const _: () = assert!(POLYMARKET.is_sane());
const _: () = assert!(HYPERLIQUID.is_sane());
const _: () = assert!(OANDA.is_sane());
const _: () = assert!(IG.is_sane());
const _: () = assert!(FXCM.is_sane());
const _: () = assert!(DUKASCOPY.is_sane());
const _: () = assert!(IBKR.is_sane());
const _: () = assert!(IBKR_CPAPI.is_sane());
const _: () = assert!(CTRADER.is_sane());
const _: () = assert!(ALPACA.is_sane());
const _: () = assert!(VenueTimings::NOT_DECLARED.is_sane());

/// The registry: the declared [`VenueTimings`] for a canonical `(venue, market)` pair — one arm per
/// [`vike_model::VENUES`] entry, mirroring [`crate::venue_rate_limits::rate_limits_for`].
///
/// **binance is the only market-SPLIT venue**: its spot socket is a WS-API subscribe/ack session and
/// its perp socket a listenKey stream, so the two rows differ. Aster has a row per market too (its
/// rate limits split), but both run ONE listenKey pump and the rows are equal; every other roster
/// venue is served the SAME row for both markets (asserted by
/// `market_split_venues_are_exactly_binance`, not assumed). IBKR answers its default product, the socket
/// backend ([`IBKR`]); the Client Portal backend is [`IBKR_CPAPI`], read by name, as
/// `crate::venue_connection::connection_for` answers them.
///
/// An unknown venue string returns [`VenueTimings::NOT_DECLARED`], fail-CLOSED in the literal sense:
/// every accessor on it panics, so a typo can never yield a timing.
#[must_use]
pub fn timings_for(venue: &str, market: VenueRateLimitsMarket) -> VenueTimings {
    match (venue, market) {
        ("binance", VenueRateLimitsMarket::Spot) => BINANCE_SPOT,
        ("binance", VenueRateLimitsMarket::Perp) => BINANCE_PERP,
        ("aster", VenueRateLimitsMarket::Spot) => ASTER_SPOT,
        ("aster", VenueRateLimitsMarket::Perp) => ASTER_PERP,
        ("bybit", _) => BYBIT,
        ("okx", _) => OKX,
        ("deribit", _) => DERIBIT,
        ("polymarket", _) => POLYMARKET,
        ("hyperliquid", _) => HYPERLIQUID,
        ("oanda", _) => OANDA,
        ("ig", _) => IG,
        ("fxcm", _) => FXCM,
        ("dukascopy", _) => DUKASCOPY,
        ("ibkr", _) => IBKR,
        ("ctrader", _) => CTRADER,
        ("alpaca", _) => ALPACA,
        // vike:new-venue:row ("{venue}", _) => {VENUE}, // TODO(new-venue: {venue}): split into (Spot, Perp) arms if the venue's markets run different sockets
        _ => VenueTimings::NOT_DECLARED,
    }
}

#[path = "venue_timings_tests.rs"]
#[cfg(test)]
mod venue_timings_tests;
